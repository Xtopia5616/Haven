use async_trait::async_trait;
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{
    OperationIdempotency, StructuredToolError, Tool, ToolConcurrency, ToolErrorMetadata,
    ToolExecutionOutcome, ToolResult,
};

pub struct HttpTool {
    /// Max retries for failed HTTP requests.
    pub max_retries: u32,
    /// Exponential backoff base (secs) between retries.
    pub backoff_base_secs: u64,
    /// Cap on how much of the response body is read (and thus buffered).
    pub max_body_bytes: usize,
    /// Optional host allowlist. Empty means public hosts are allowed after
    /// the SSRF network policy has rejected local/private destinations.
    pub allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct NetworkPolicy {
    allowed_domains: Vec<String>,
    /// Only enabled by unit tests so the canned loopback HTTP servers can be
    /// exercised without weakening production policy.
    allow_loopback: bool,
}

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NetworkMethod {
    Get,
    Post,
}

impl NetworkMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            NetworkMethod::Get => "GET",
            NetworkMethod::Post => "POST",
        }
    }
}

/// Typed parameters for `HttpTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `HttpTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct NetworkParams {
    /// HTTP method; defaults to GET.
    #[serde(default)]
    pub method: Option<NetworkMethod>,
    /// The URL to request.
    pub url: String,
    /// Optional HTTP headers as key-value pairs.
    #[serde(default)]
    pub headers: Option<HashMap<String, String>>,
    /// Request body for POST.
    #[serde(default)]
    pub body: Option<String>,
    /// Return the raw HTML instead of converting HTML pages to plain text
    /// (default false).
    #[serde(default)]
    pub as_html: Option<bool>,
    /// Request timeout in seconds (default 15).
    #[serde(default)]
    pub timeout_secs: Option<i64>,
}

impl HttpTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: NetworkParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("HTTP request cancelled"));
        }

        let url = params.url;
        let method = params
            .method
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "GET".to_string());
        // Keep direct/native callers safe too; JSON schema validation is only
        // applied at the model boundary and cannot protect internal callers.
        let timeout_secs = params.timeout_secs.unwrap_or(15).clamp(1, 120) as u64;

        let body = params.body;
        let as_html = params.as_html.unwrap_or(false);
        let headers: Vec<(String, String)> =
            params.headers.unwrap_or_default().into_iter().collect();

        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("HTTP request cancelled"));
        }

        execute_once(
            &url,
            &method,
            &headers,
            body.as_deref(),
            as_html,
            timeout_secs,
            self.max_body_bytes,
            &self.allowed_domains,
        )
        .await
    }
}

impl Default for HttpTool {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_base_secs: 1,
            max_body_bytes: 1024 * 1024,
            allowed_domains: Vec::new(),
        }
    }
}

#[async_trait]
impl Tool for HttpTool {
    fn name(&self) -> String {
        "http".into()
    }
    fn description(&self) -> String {
        crate::prompts::HTTP_DESCRIPTION.into()
    }

    fn catalog_group(&self) -> ToolCatalogGroup {
        ToolCatalogGroup::System
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input.get("method").and_then(Value::as_str) {
            None | Some("GET") => OperationIdempotency::Idempotent,
            Some("POST") => OperationIdempotency::NonIdempotent,
            _ => OperationIdempotency::Unknown,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        match input.get("method").and_then(Value::as_str) {
            None | Some("GET") => ToolConcurrency::SharedResource("http".into()),
            _ => ToolConcurrency::Resource("http".into()),
        }
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        // Dropping a reqwest request future closes the request body/response
        // stream; unlike a shell or remote MCP server there is no child
        // process that can continue after the future is dropped.
        ToolExecutionOutcome::TimedOutAndTerminated
    }

    fn default_timeout_secs(&self) -> u64 {
        // `NetworkParams::timeout_secs` is the provider/request timeout. The
        // manager's outer timer gets a small handoff margin below.
        20
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        input
            .get("timeout_secs")
            .and_then(Value::as_i64)
            .map(|value| value.clamp(1, 120) as u64)
            .unwrap_or(15)
            .saturating_add(5)
    }

    fn default_max_retries(&self) -> u32 {
        self.max_retries
    }

    fn default_retry_backoff_secs(&self) -> u64 {
        self.backoff_base_secs
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "method": { "type": "string", "enum": ["GET", "POST"], "default": "GET" },
                "url": { "type": "string", "minLength": 1 },
                "headers": { "type": "object" },
                "body": { "type": "string" },
                "as_html": { "type": "boolean" },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120 }
            },
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "method": { "const": "GET", "default": "GET" },
                        "url": { "type": "string", "minLength": 1, "description": "The URL to request" },
                        "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Optional HTTP headers" },
                        "as_html": { "type": "boolean", "description": "Return raw HTML instead of extracted text" },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120, "default": 15 }
                    },
                    "required": ["url"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "method": { "const": "POST" },
                        "url": { "type": "string", "minLength": 1, "description": "The URL to request" },
                        "headers": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Optional HTTP headers" },
                        "body": { "type": "string", "description": "Request body" },
                        "as_html": { "type": "boolean", "description": "Return raw HTML instead of extracted text" },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120, "default": 15 }
                    },
                    "required": ["method", "url"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `NetworkParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<NetworkParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_once(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    timeout_secs: u64,
    max_body_bytes: usize,
    allowed_domains: &[String],
) -> anyhow::Result<ToolResult> {
    let policy = NetworkPolicy {
        allowed_domains: allowed_domains.to_vec(),
        allow_loopback: cfg!(test),
    };
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Haven/1.0");
    // Route through a locally detected proxy so international requests work
    // when the user runs one (e.g. 127.0.0.1:10808) — same detection as the
    // shell tool's spawned commands. User-set env vars already short-circuit
    // the probe (reqwest also honors them natively).
    for (key, val) in crate::proxy_env_vars() {
        if key == "HTTP_PROXY" || key == "http_proxy" {
            builder = builder.proxy(reqwest::Proxy::http(&val)?);
        } else if key == "HTTPS_PROXY" || key == "https_proxy" {
            builder = builder.proxy(reqwest::Proxy::https(&val)?);
        }
    }
    let client = builder.build()?;

    execute_once_with(
        &client,
        url,
        method,
        headers,
        body,
        as_html,
        max_body_bytes,
        &policy,
    )
    .await
}

/// Send one request with a caller-supplied client. Split out so tests can
/// exercise the connection-error path with a proxy-free client: a system or
/// environment proxy can answer loopback requests with its own error page
/// (e.g. 502) instead of relaying the peer's reset, masking the failure.
#[allow(clippy::too_many_arguments)]
async fn execute_once_with(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    max_body_bytes: usize,
    policy: &NetworkPolicy,
) -> anyhow::Result<ToolResult> {
    let mut current_url = validate_network_url(url, policy).await?;
    let mut current_method = method.to_string();
    let mut current_body = body.map(str::to_owned);
    let mut current_headers = headers.to_vec();
    let mut redirect_hops = 0usize;
    let response = loop {
        let mut req = match current_method.as_str() {
            "GET" => client.get(current_url.clone()),
            "POST" => client
                .post(current_url.clone())
                .body(current_body.clone().unwrap_or_default()),
            _ => anyhow::bail!("unsupported method: {}", current_method),
        };

        for (key, val) in &current_headers {
            req = req.header(key.as_str(), val.as_str());
        }

        let response = req.send().await.map_err(map_reqwest_error)?;
        let Some(location) = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .filter(|_| response.status().is_redirection())
        else {
            break response;
        };

        // Validate every redirect target before issuing the next request.
        // reqwest's automatic redirect policy is disabled so a redirect can
        // never bypass the SSRF check or the host allowlist.
        redirect_hops += 1;
        if redirect_hops > MAX_REDIRECT_HOPS {
            anyhow::bail!("HTTP redirect limit exceeded");
        }
        let next_url = current_url.join(&location)?;
        let next_url = validate_network_url(next_url.as_str(), policy).await?;
        if response.status() == reqwest::StatusCode::SEE_OTHER
            || (matches!(
                response.status(),
                reqwest::StatusCode::MOVED_PERMANENTLY | reqwest::StatusCode::FOUND
            ) && current_method == "POST")
        {
            current_method = "GET".to_string();
            current_body = None;
        }
        if !same_origin(&current_url, &next_url) {
            current_headers.retain(|(key, _)| !is_sensitive_request_header(key));
        }
        current_url = next_url;
    };

    let status = response.status().as_u16();
    let resp_headers: Vec<Value> = response
        .headers()
        .iter()
        .filter(|(name, _)| !is_sensitive_response_header(name.as_str()))
        .map(|(k, v)| serde_json::json!({"name": k.as_str(), "value": v.to_str().unwrap_or("")}))
        .collect();

    let max_chars = 20_000;
    let content_type = resp_headers
        .iter()
        .find(|h| {
            h["name"]
                .as_str()
                .unwrap_or("")
                .eq_ignore_ascii_case("content-type")
        })
        .and_then(|h| h["value"].as_str());
    let html_by_header = content_type.is_some_and(|ct| ct.to_ascii_lowercase().contains("html"));
    // HTML loses bulk when extracted to text, so read more raw bytes for it;
    // the final body is still truncated to `max_chars`.
    let byte_cap = if html_by_header {
        max_body_bytes
    } else {
        max_chars * 4
    };
    let (response_bytes, byte_cap_truncated) =
        read_body_capped(response, byte_cap, max_body_bytes).await?;
    let response_body = haven_common::encoding::decode_lossy(&response_bytes);

    let is_html = html_by_header || looks_like_html(&response_body);

    let (body_truncated, body_truncated_by_text, format) = if is_html && !as_html {
        let (t, tr) =
            haven_common::encoding::truncate_output(&html_to_text(&response_body), max_chars);
        (t, tr, "text")
    } else {
        let (t, tr) = haven_common::encoding::truncate_output(&response_body, max_chars);
        (t, tr, if is_html { "html" } else { "raw" })
    };

    let truncated = byte_cap_truncated || body_truncated_by_text;
    Ok(ToolResult::from_output(
        serde_json::json!({
            "operation": "request",
            "method": method,
            "status": status,
            "headers": resp_headers,
            "body": body_truncated,
            "truncated": truncated,
            "format": format,
        }),
        truncated,
    ))
}

const MAX_REDIRECT_HOPS: usize = 10;

async fn validate_network_url(
    raw_url: &str,
    policy: &NetworkPolicy,
) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw_url)
        .map_err(|error| anyhow::anyhow!("invalid HTTP URL: {}", error))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("HTTP tool only supports http and https URLs");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("HTTP URL userinfo is not allowed");
    }
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("HTTP URL must include a host"))?;
    let normalized_host = normalize_host(host);
    let port = url
        .port_or_known_default()
        .ok_or_else(|| anyhow::anyhow!("HTTP URL has no supported port"))?;

    if is_blocked_metadata_host(&normalized_host)
        && !(policy.allow_loopback && normalized_host == "localhost")
    {
        anyhow::bail!("HTTP destination is a blocked metadata host");
    }
    if !domain_allowed(&normalized_host, &policy.allowed_domains) {
        anyhow::bail!("HTTP destination is not in the configured domain allowlist");
    }

    if let Ok(ip) = normalized_host.parse::<IpAddr>() {
        if is_blocked_ip(ip, policy.allow_loopback) {
            anyhow::bail!("HTTP destination resolves to a blocked local or private address");
        }
        return Ok(url);
    }

    let addresses = tokio::net::lookup_host((normalized_host.as_str(), port))
        .await
        .map_err(|error| anyhow::anyhow!("failed to resolve HTTP host: {}", error))?;
    let mut saw_address = false;
    for address in addresses {
        saw_address = true;
        if is_blocked_ip(address.ip(), policy.allow_loopback) {
            anyhow::bail!("HTTP host resolves to a blocked local or private address");
        }
    }
    if !saw_address {
        anyhow::bail!("HTTP host did not resolve to an address");
    }
    Ok(url)
}

fn normalize_host(host: &str) -> String {
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn domain_allowed(host: &str, allowed_domains: &[String]) -> bool {
    if allowed_domains.is_empty() {
        return true;
    }
    allowed_domains.iter().any(|entry| {
        let entry = normalize_host(entry.trim());
        if let Some(suffix) = entry.strip_prefix("*.") {
            host.ends_with(&format!(".{suffix}")) && host != suffix
        } else {
            host == entry
        }
    })
}

fn is_blocked_metadata_host(host: &str) -> bool {
    matches!(
        host,
        "localhost"
            | "metadata"
            | "metadata.google.internal"
            | "metadata.azure.internal"
            | "metadata.internal"
            | "instance-data"
            | "instance-data.ec2.internal"
    )
}

fn is_blocked_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    if allow_loopback && ip.is_loopback() {
        return false;
    }
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_link_local()
                || ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
                || ip.octets() == [169, 254, 169, 254]
                || ip.octets() == [100, 100, 100, 200]
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped), allow_loopback);
            }
            let first = ip.octets()[0];
            let second = ip.octets()[1];
            ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_multicast()
                || (first & 0xfe) == 0xfc
                || (first == 0xfe && (second & 0xc0) == 0x80)
        }
    }
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(normalize_host) == right.host_str().map(normalize_host)
        && left.port_or_known_default() == right.port_or_known_default()
}

fn is_sensitive_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization" | "cookie" | "proxy-authorization"
    )
}

/// Read at most `byte_cap` bytes (bounded by `max_body_bytes`) of the response
/// body, streaming, so huge responses never get fully buffered and we never
/// read far more than what will be shown.
async fn read_body_capped(
    response: reqwest::Response,
    byte_cap: usize,
    max_body_bytes: usize,
) -> anyhow::Result<(Vec<u8>, bool)> {
    use futures_util::StreamExt;
    let cap = byte_cap.min(max_body_bytes);
    let mut stream = response.bytes_stream();
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_reqwest_error)?;
        let room = cap.saturating_sub(out.len());
        if room == 0 {
            truncated = true;
            break;
        }
        let take = chunk.len().min(room);
        out.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            truncated = true;
            break;
        }
    }
    Ok((out, truncated))
}

/// Response headers can contain bearer tokens or browser session material
/// even when the response body is public. Keep those values out of the model
/// observation while preserving useful transport metadata such as
/// `content-type` and `etag`.
fn is_sensitive_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "authorization"
            | "cookie"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "set-cookie"
            | "set-cookie2"
            | "www-authenticate"
            | "x-api-key"
    )
}

/// Cheap sniff for an HTML document when the server omitted (or mislabeled)
/// the Content-Type. Only matches the very start of the body.
fn looks_like_html(body: &str) -> bool {
    let head = &body[..body.floor_char_boundary(body.len().min(512))];
    let head = head.trim_start().to_ascii_lowercase();
    head.starts_with("<!doctype html")
        || head.starts_with("<html")
        || head.starts_with("<head")
        || head.starts_with("<body")
}

/// Extract readable plain text from an HTML document. Script/style/noscript
/// content and whitespace noise are dropped, and block-level elements start a
/// new line so the output reads like a document instead of a run-on blob.
fn html_to_text(html: &str) -> String {
    use scraper::node::Node;
    use scraper::{Html, Selector};

    const BLOCK_TAGS: &[&str] = &[
        "article",
        "aside",
        "blockquote",
        "body",
        "br",
        "div",
        "footer",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "header",
        "hr",
        "li",
        "main",
        "nav",
        "ol",
        "p",
        "pre",
        "section",
        "table",
        "td",
        "th",
        "tr",
        "ul",
    ];
    const HIDDEN_TAGS: &[&str] = &["head", "script", "style", "noscript", "template"];

    let doc = Html::parse_document(html);
    let root = Selector::parse("body")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .unwrap_or_else(|| doc.root_element());

    let mut out = String::new();
    // DFS over the tree, dropping hidden subtrees. Children are pushed in
    // reverse so they are visited in document order.
    let mut stack: Vec<_> = Vec::new();
    for child in root.children().rev() {
        stack.push(child);
    }
    while let Some(node) = stack.pop() {
        match node.value() {
            Node::Element(el) => {
                let name = el.name();
                if HIDDEN_TAGS.contains(&name) {
                    continue;
                }
                if BLOCK_TAGS.contains(&name) {
                    out.push('\n');
                }
                for child in node.children().rev() {
                    stack.push(child);
                }
            }
            Node::Text(text) => out.push_str(&text.text),
            _ => {}
        }
    }

    out.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn map_reqwest_error(e: reqwest::Error) -> anyhow::Error {
    let detail = haven_common::error::sanitize_error_text(&e.to_string());
    let (message, metadata) = if e.is_timeout() {
        (
            format!("request timed out: {detail}"),
            ToolErrorMetadata::transient(),
        )
    } else if e.is_connect() {
        (
            format!("connection failed: {detail}"),
            ToolErrorMetadata::transient(),
        )
    } else if e.is_status() {
        let transient = e.status().is_some_and(|status| {
            status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
        });
        (
            format!("HTTP error: {detail}"),
            if transient {
                ToolErrorMetadata::transient()
            } else {
                ToolErrorMetadata::other()
            },
        )
    } else {
        (
            format!("request failed: {detail}"),
            ToolErrorMetadata::other(),
        )
    };
    anyhow::Error::new(StructuredToolError::new(message, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_network_tool_name() {
        assert_eq!(HttpTool::default().name(), "http");
    }

    #[test]
    fn test_network_tool_risk_level() {
        assert_eq!(
            HttpTool::default().risk_level(&json!({})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_network_tool_input_schema() {
        let schema = HttpTool::default().input_schema();
        assert!(schema["properties"]["url"].is_object());
    }

    #[test]
    fn retry_policy_only_allows_get_replay() {
        let tool = HttpTool::default();
        assert_eq!(
            tool.idempotency(&json!({"method": "GET"})),
            OperationIdempotency::Idempotent
        );
        assert_eq!(
            tool.idempotency(&json!({"method": "POST"})),
            OperationIdempotency::NonIdempotent
        );
        assert_eq!(
            tool.idempotency(&json!({"method": "PUT"})),
            OperationIdempotency::Unknown
        );
    }

    /// Serve a single canned HTTP/1.1 response on a local listener and return
    /// the URL to request. The connection is closed after one exchange.
    /// The complete request (headers plus any Content-Length body) is read
    /// before responding: reqwest can split a POST's headers and body across
    /// separate TCP segments on loopback, and closing the socket while the
    /// client is still writing would surface as a reset instead of a response.
    async fn serve_once(status_line: &str, content_type: &str, body: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        let status = status_line.to_string();
        let content_type = content_type.to_string();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            loop {
                let n = sock.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&buf[..header_end]);
                    let content_length = head
                        .lines()
                        .find_map(|l| {
                            let lower = l.to_lowercase();
                            lower
                                .strip_prefix("content-length:")
                                .and_then(|v| v.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
            }
            let resp = format!(
                "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
                content_type,
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });
        format!("http://{}/", addr)
    }

    /// Accept one connection and drop it immediately so the client's request
    /// fails with a connection error. The listener stays bound until the
    /// connection arrives, so the outcome is deterministic — unlike binding a
    /// port and closing it first, which races the OS (and other tests running
    /// in parallel) reusing the freed port.
    async fn connection_drop_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            drop(sock);
        });
        format!("http://{}/", addr)
    }

    #[tokio::test]
    async fn test_network_execute_get_success() {
        let url = serve_once("200 OK", "text/plain", "hello from mock server").await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": url, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], 200);
        assert_eq!(result.output["body"], "hello from mock server");
        assert_eq!(result.output["truncated"], false);
        assert!(!result.truncated);
        let headers = result.output["headers"].as_array().unwrap();
        assert!(headers.iter().any(|h| h["name"] == "content-type"));
    }

    #[tokio::test]
    async fn test_network_body_cap_reports_truncation_at_both_layers() {
        let url = serve_once("200 OK", "text/plain", "hello from mock server").await;
        let tool = HttpTool {
            max_retries: 0,
            backoff_base_secs: 0,
            max_body_bytes: 5,
            allowed_domains: Vec::new(),
        };
        let result = tool
            .execute(
                json!({"method": "GET", "url": url, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["body"], "hello");
        assert_eq!(result.output["truncated"], true);
        assert!(result.truncated);
    }

    #[test]
    fn sensitive_response_headers_are_not_exposed() {
        assert!(is_sensitive_response_header("Set-Cookie"));
        assert!(is_sensitive_response_header("www-authenticate"));
        assert!(!is_sensitive_response_header("Content-Type"));
        assert!(!is_sensitive_response_header("ETag"));
    }

    #[test]
    fn test_html_to_text_strips_tags_and_scripts() {
        let html = concat!(
            "<html><head><title>ignored</title>",
            "<style>a{color:red}</style>",
            "</head><body><h1>  Title  </h1>",
            "<p>Hello <b>Haven</b>!</p>",
            "<script>evil()</script>",
            "<ul><li>one</li><li>two</li></ul></body></html>",
        );
        let text = html_to_text(html);
        assert!(text.contains("Title"), "got: {}", text);
        assert!(text.contains("Hello Haven!"), "got: {}", text);
        assert!(text.contains("one"), "got: {}", text);
        assert!(text.contains("two"), "got: {}", text);
        assert!(!text.contains("ignored"), "got: {}", text);
        assert!(!text.contains("evil()"), "got: {}", text);
        assert!(!text.contains("color:red"), "got: {}", text);
        assert!(!text.contains('<'), "got: {}", text);
    }

    #[test]
    fn test_looks_like_html_detects_doctype_and_tag() {
        assert!(looks_like_html("<!DOCTYPE html>\n<html>..."));
        assert!(looks_like_html("  <html lang=\"en\">..."));
        assert!(looks_like_html("<body>x</body>"));
        assert!(!looks_like_html("{\"ok\": true}"));
        assert!(!looks_like_html("hello world"));
    }

    #[tokio::test]
    async fn test_network_execute_html_converted_to_text() {
        let html = "<html><head><title>x</title></head><body><h1>Welcome</h1><p>Hello Haven</p><script>bad()</script></body></html>";
        let url = serve_once("200 OK", "text/html; charset=utf-8", html).await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": url, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["format"], "text");
        let body = result.output["body"].as_str().unwrap();
        assert!(body.contains("Welcome"), "got: {}", body);
        assert!(body.contains("Hello Haven"), "got: {}", body);
        assert!(!body.contains("bad()"), "got: {}", body);
        assert!(!body.contains("<h1>"), "got: {}", body);
    }

    #[tokio::test]
    async fn test_network_execute_as_html_returns_raw() {
        let html = "<html><body><p>hi</p></body></html>";
        let url = serve_once("200 OK", "text/html", html).await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": url, "as_html": true, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["format"], "html");
        assert_eq!(result.output["body"], html);
    }

    #[tokio::test]
    async fn test_network_execute_plain_body_format_raw() {
        let url = serve_once("200 OK", "application/json", "{\"ok\":true}").await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": url, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["format"], "raw");
        assert_eq!(result.output["body"], "{\"ok\":true}");
    }

    #[tokio::test]
    async fn test_network_execute_get_not_found_no_retry() {
        let url = serve_once("404 Not Found", "text/plain", "nope").await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": url, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], 404);
        assert_eq!(result.output["body"], "nope");
    }

    #[tokio::test]
    async fn test_network_execute_post_with_body() {
        let url = serve_once("201 Created", "text/plain", "created").await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "POST", "url": url, "body": "payload", "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["status"], 201);
        assert_eq!(result.output["body"], "created");
    }

    #[tokio::test]
    async fn test_network_execute_connection_dropped_returns_error() {
        // The peer accepts and immediately drops the connection. Use a
        // proxy-free client: the system proxy answers loopback requests with
        // its own 502 error page when the upstream resets, which would turn
        // this failure into a "successful" HTTP response.
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let url = connection_drop_url().await;
        let result = execute_once_with(
            &client,
            &url,
            "POST",
            &[],
            Some("payload"),
            false,
            1024 * 1024,
            &NetworkPolicy {
                allowed_domains: Vec::new(),
                allow_loopback: true,
            },
        )
        .await;
        assert!(
            result.is_err(),
            "connection failure must surface as an error"
        );
    }

    #[tokio::test]
    async fn test_network_execute_unsupported_method() {
        let result = HttpTool::default()
            .execute(
                json!({"method": "PUT", "url": "http://127.0.0.1:1/"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown variant `PUT`")
        );
    }

    #[tokio::test]
    async fn test_network_execute_requires_url() {
        let result = HttpTool::default()
            .execute(json!({"method": "GET"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing field `url`")
        );
    }

    #[tokio::test]
    async fn test_network_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": "http://127.0.0.1:1/"}),
                cancel,
            )
            .await;
        assert_eq!(result.unwrap().outcome, ToolExecutionOutcome::Cancelled);
    }

    #[tokio::test]
    async fn network_policy_blocks_local_private_and_metadata_destinations() {
        let policy = NetworkPolicy::default();
        for url in [
            "http://localhost/",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://metadata.google.internal/",
            "http://[::1]/",
            "http://[fd00::1]/",
        ] {
            assert!(
                validate_network_url(url, &policy).await.is_err(),
                "destination must be blocked: {url}"
            );
        }
    }

    #[test]
    fn network_policy_domain_allowlist_supports_exact_and_subdomain_entries() {
        assert!(domain_allowed("example.com", &[]));
        assert!(domain_allowed(
            "api.example.com",
            &["*.example.com".to_string()]
        ));
        assert!(!domain_allowed(
            "example.com",
            &["*.example.com".to_string()]
        ));
        assert!(domain_allowed("example.com", &["EXAMPLE.COM.".to_string()]));
        assert!(!domain_allowed(
            "other.example.net",
            &["example.com".to_string()]
        ));
    }

    #[tokio::test]
    async fn network_policy_allows_loopback_only_for_explicit_test_policy() {
        let policy = NetworkPolicy {
            allowed_domains: Vec::new(),
            allow_loopback: true,
        };
        assert!(
            validate_network_url("http://127.0.0.1:1/", &policy)
                .await
                .is_ok()
        );
        assert!(
            validate_network_url("http://localhost:1/", &policy)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn redirect_target_is_rechecked_before_following() {
        let initial = reqwest::Url::parse("https://public.example/start").unwrap();
        let target = initial.join("http://127.0.0.1:8080/").unwrap();
        assert!(
            validate_network_url(target.as_str(), &NetworkPolicy::default())
                .await
                .is_err()
        );

        let allowlist = NetworkPolicy {
            allowed_domains: vec!["example.com".into()],
            allow_loopback: false,
        };
        assert!(
            validate_network_url("https://other.example.com/", &allowlist)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_network_native_entry_lands_in_run() {
        let url = serve_once("200 OK", "text/plain", "hello native").await;
        let result = HttpTool::default()
            .run(
                NetworkParams {
                    method: Some(NetworkMethod::Get),
                    url: url.clone(),
                    headers: None,
                    body: None,
                    as_html: None,
                    timeout_secs: Some(5),
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["status"], 200);
        assert_eq!(result.output["body"], "hello native");
    }
}
