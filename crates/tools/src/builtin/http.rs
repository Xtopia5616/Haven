use async_trait::async_trait;
use haven_common::network::{
    self, MAX_HTTP_REDIRECT_HOPS, MAX_HTTP_URL_BYTES, ResolvedHttpDestination,
};
use haven_common::tools::ToolCatalogGroup;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::http_content::extract_http_content;
#[cfg(test)]
use super::http_content::{html_to_text, looks_like_html};

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
    /// destination validation has rejected local/private hosts.
    pub allowed_domains: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct HttpDestinationPolicy {
    allowed_domains: Vec<String>,
    /// Only enabled by unit tests so the canned loopback HTTP servers can be
    /// exercised without weakening production policy.
    #[cfg(test)]
    allow_loopback: bool,
}

const MAX_HTTP_REQUEST_HEADERS: usize = 64;
const MAX_HTTP_REQUEST_HEADER_NAME_BYTES: usize = 256;
const MAX_HTTP_REQUEST_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_HTTP_REQUEST_HEADERS_BYTES: usize = 32 * 1024;
const MAX_HTTP_REQUEST_BODY_BYTES: usize = 1024 * 1024;

/// HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpRequestMethod {
    Get,
    Post,
}

impl HttpRequestMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpRequestMethod::Get => "GET",
            HttpRequestMethod::Post => "POST",
        }
    }
}

/// Typed parameters for `HttpTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `HttpTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct HttpRequestParams {
    /// HTTP method; defaults to GET.
    #[serde(default)]
    pub method: Option<HttpRequestMethod>,
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
        params: HttpRequestParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("HTTP request cancelled"));
        }

        validate_request_bounds(&params)?;

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

        tokio::select! {
            biased;
            _ = cancel.cancelled() => Ok(ToolResult::cancelled("HTTP request cancelled")),
            result = execute_once_mode(
                &url,
                &method,
                &headers,
                body.as_deref(),
                as_html,
                timeout_secs,
                self.max_body_bytes,
                &self.allowed_domains,
                true,
                cancel.clone(),
            ) => result,
        }
    }

    /// Fetch and extract a result page for the web-search workflow. This
    /// deliberately shares the public destination resolver, DNS pinning,
    /// redirect checks, body budget, timeout, and cancellation behavior with
    /// the model-callable HTTP operation.
    pub(crate) async fn fetch_for_research(
        &self,
        url: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<Value> {
        if cancel.is_cancelled() {
            anyhow::bail!("HTTP request cancelled");
        }
        if url.len() > MAX_HTTP_URL_BYTES {
            anyhow::bail!("HTTP URL exceeds the {MAX_HTTP_URL_BYTES} byte limit");
        }
        let result = execute_once_mode(
            url,
            "GET",
            &[],
            None,
            false,
            15,
            self.max_body_bytes,
            &self.allowed_domains,
            true,
            cancel,
        )
        .await?;
        Ok(result.output)
    }
}

fn validate_request_bounds(params: &HttpRequestParams) -> anyhow::Result<()> {
    if params.url.len() > MAX_HTTP_URL_BYTES {
        anyhow::bail!("HTTP URL exceeds the {MAX_HTTP_URL_BYTES} byte limit");
    }
    if matches!(
        params.method.unwrap_or(HttpRequestMethod::Get),
        HttpRequestMethod::Get
    ) && params.body.is_some()
    {
        anyhow::bail!("GET requests must not include a request body");
    }
    if let Some(body) = &params.body
        && body.len() > MAX_HTTP_REQUEST_BODY_BYTES
    {
        anyhow::bail!("HTTP request body exceeds the {MAX_HTTP_REQUEST_BODY_BYTES} byte limit");
    }
    if let Some(headers) = &params.headers {
        if headers.len() > MAX_HTTP_REQUEST_HEADERS {
            anyhow::bail!("HTTP request has more than {MAX_HTTP_REQUEST_HEADERS} headers");
        }
        let mut total_bytes = 0usize;
        for (name, value) in headers {
            if name.len() > MAX_HTTP_REQUEST_HEADER_NAME_BYTES {
                anyhow::bail!(
                    "HTTP request header name exceeds the {MAX_HTTP_REQUEST_HEADER_NAME_BYTES} byte limit"
                );
            }
            if value.len() > MAX_HTTP_REQUEST_HEADER_VALUE_BYTES {
                anyhow::bail!(
                    "HTTP request header value exceeds the {MAX_HTTP_REQUEST_HEADER_VALUE_BYTES} byte limit"
                );
            }
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|error| anyhow::anyhow!("invalid HTTP request header name: {error}"))?;
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|error| anyhow::anyhow!("invalid HTTP request header value: {error}"))?;
            total_bytes = total_bytes
                .saturating_add(name.len())
                .saturating_add(value.len())
                .saturating_add(4);
            if total_bytes > MAX_HTTP_REQUEST_HEADERS_BYTES {
                anyhow::bail!(
                    "HTTP request headers exceed the {MAX_HTTP_REQUEST_HEADERS_BYTES} byte limit"
                );
            }
        }
    }
    Ok(())
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
        ToolCatalogGroup::Network
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn operation_spec(&self) -> Option<crate::operation_view::OperationSpec> {
        Some(crate::operation_view::root_tool_spec(
            "http",
            crate::prompts::HTTP_DESCRIPTION,
            self.input_schema(),
            RiskLevel::Medium,
            OperationIdempotency::Idempotent,
            crate::ToolOperationScope::Session,
            ToolConcurrency::SharedResource("http".into()),
            ToolCatalogGroup::Network,
            Some(crate::operation_view::OperationPolicyRule::HttpVerb),
        ))
    }

    fn timeout_outcome(&self) -> ToolExecutionOutcome {
        // Dropping a reqwest request future closes the request body/response
        // stream; unlike a shell or remote MCP server there is no child
        // process that can continue after the future is dropped.
        ToolExecutionOutcome::TimedOutAndTerminated
    }

    fn default_timeout_secs(&self) -> u64 {
        // `HttpRequestParams::timeout_secs` is the provider/request timeout. The
        // outer tool execution timer gets a small handoff margin below.
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
                "method": { "type": "string", "enum": ["GET", "POST"], "default": "GET", "description": "GET reads a known URL; POST sends body to a known endpoint. Defaults to GET" },
                "url": { "type": "string", "minLength": 1, "maxLength": MAX_HTTP_URL_BYTES, "description": "Known HTTP or HTTPS URL to request; this tool does not search for URLs" },
                "headers": { "type": "object", "maxProperties": MAX_HTTP_REQUEST_HEADERS, "propertyNames": { "maxLength": MAX_HTTP_REQUEST_HEADER_NAME_BYTES }, "additionalProperties": { "type": "string", "maxLength": MAX_HTTP_REQUEST_HEADER_VALUE_BYTES }, "description": "Optional request headers as string name/value pairs" },
                "body": { "type": "string", "maxLength": MAX_HTTP_REQUEST_BODY_BYTES, "description": "Request body for POST" },
                "as_html": { "type": "boolean", "default": false, "description": "Return raw HTML instead of extracted readable text; defaults to false" },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120, "default": 15, "description": "Request timeout in seconds; defaults to 15, maximum 120" }
            },
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "method": { "const": "GET", "default": "GET", "description": "Read the URL without sending a request body" },
                        "url": { "type": "string", "minLength": 1, "maxLength": MAX_HTTP_URL_BYTES, "description": "Known HTTP or HTTPS URL to request" },
                        "headers": { "type": "object", "maxProperties": MAX_HTTP_REQUEST_HEADERS, "propertyNames": { "maxLength": MAX_HTTP_REQUEST_HEADER_NAME_BYTES }, "additionalProperties": { "type": "string", "maxLength": MAX_HTTP_REQUEST_HEADER_VALUE_BYTES }, "description": "Optional HTTP headers" },
                        "as_html": { "type": "boolean", "default": false, "description": "Return raw HTML instead of extracted readable text; defaults to false" },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120, "default": 15, "description": "Request timeout in seconds; defaults to 15" }
                    },
                    "required": ["url"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "method": { "const": "POST", "description": "Send a request body to this URL" },
                        "url": { "type": "string", "minLength": 1, "maxLength": MAX_HTTP_URL_BYTES, "description": "Known HTTP or HTTPS URL to request" },
                        "headers": { "type": "object", "maxProperties": MAX_HTTP_REQUEST_HEADERS, "propertyNames": { "maxLength": MAX_HTTP_REQUEST_HEADER_NAME_BYTES }, "additionalProperties": { "type": "string", "maxLength": MAX_HTTP_REQUEST_HEADER_VALUE_BYTES }, "description": "Optional HTTP headers" },
                        "body": { "type": "string", "maxLength": MAX_HTTP_REQUEST_BODY_BYTES, "description": "Request body" },
                        "as_html": { "type": "boolean", "default": false, "description": "Return raw HTML instead of extracted readable text; defaults to false" },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 120, "default": 15, "description": "Request timeout in seconds; defaults to 15" }
                    },
                    "required": ["method", "url"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `HttpRequestParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<HttpRequestParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_once_mode(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    timeout_secs: u64,
    max_body_bytes: usize,
    allowed_domains: &[String],
    extract_known_content: bool,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    let destination_policy = HttpDestinationPolicy {
        allowed_domains: allowed_domains.to_vec(),
        #[cfg(test)]
        allow_loopback: true,
    };
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        // Requests are pinned to the address validated below. A proxy would
        // resolve the target independently and bypass that pin.
        .no_proxy()
        .user_agent("Haven/1.0");
    let resolved = resolve_http_destination(url, &destination_policy).await?;
    if let Some(address) = resolved.pinned_address
        && let Some(host) = resolved.url.host_str()
    {
        builder = builder.resolve(host, address);
    }
    let client = builder.build()?;

    execute_once_with_mode(
        &client,
        url,
        method,
        headers,
        body,
        as_html,
        max_body_bytes,
        &destination_policy,
        extract_known_content,
        cancel,
    )
    .await
}

/// Send one request with a caller-supplied client. Split out so tests can
/// exercise the connection-error path with a proxy-free client: a system or
/// environment proxy can answer loopback requests with its own error page
/// (e.g. 502) instead of relaying the peer's reset, masking the failure.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn execute_once_with(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    max_body_bytes: usize,
    destination_policy: &HttpDestinationPolicy,
) -> anyhow::Result<ToolResult> {
    execute_once_with_mode(
        client,
        url,
        method,
        headers,
        body,
        as_html,
        max_body_bytes,
        destination_policy,
        false,
        CancellationToken::new(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_once_with_mode(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    max_body_bytes: usize,
    destination_policy: &HttpDestinationPolicy,
    extract_known_content: bool,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    let mut current_url = validate_http_destination(url, destination_policy).await?;
    let mut current_method = method.to_string();
    let mut current_body = body.map(str::to_owned);
    let current_headers = headers.to_vec();
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
        if redirect_hops > MAX_HTTP_REDIRECT_HOPS {
            anyhow::bail!("HTTP redirect limit exceeded");
        }
        let next_url = current_url.join(&location)?;
        let next_url = validate_http_destination(next_url.as_str(), destination_policy).await?;
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
            anyhow::bail!("cross-origin HTTP redirects are not allowed");
        }
        current_url = next_url;
    };

    let status = response.status().as_u16();
    let final_url = current_url.to_string();
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
    let byte_cap = if html_by_header || extract_known_content {
        max_body_bytes
    } else {
        max_chars * 4
    };
    let (response_bytes, byte_cap_truncated) =
        read_body_capped(response, byte_cap, max_body_bytes).await?;
    let extraction_content_type = content_type.unwrap_or_default().to_string();
    let extraction_cancel = cancel.clone();
    let (body_truncated, body_truncated_by_text, format, title) =
        tokio::task::spawn_blocking(move || {
            extract_http_content(
                &response_bytes,
                &extraction_content_type,
                as_html,
                extract_known_content,
                max_chars,
                byte_cap_truncated,
                &extraction_cancel,
            )
        })
        .await??;

    let truncated = byte_cap_truncated || body_truncated_by_text;
    let mut output = serde_json::json!({
        "operation": "request",
        "method": method,
        "url": final_url,
        "status": status,
        "headers": resp_headers,
        "content_type": content_type,
        "body": body_truncated,
        "truncated": truncated,
        "format": format,
    });
    if let Some(title) = title
        && let Some(object) = output.as_object_mut()
    {
        object.insert("title".into(), Value::String(title));
    }
    Ok(ToolResult::from_output(output, truncated))
}

async fn validate_http_destination(
    raw_url: &str,
    destination_policy: &HttpDestinationPolicy,
) -> anyhow::Result<reqwest::Url> {
    Ok(resolve_http_destination(raw_url, destination_policy)
        .await?
        .url)
}

async fn resolve_http_destination(
    raw_url: &str,
    destination_policy: &HttpDestinationPolicy,
) -> anyhow::Result<ResolvedHttpDestination> {
    #[cfg(test)]
    if destination_policy.allow_loopback {
        return network::resolve_http_destination_for_test(
            raw_url,
            &destination_policy.allowed_domains,
        )
        .await;
    }
    network::resolve_http_destination(raw_url, &destination_policy.allowed_domains).await
}

fn same_origin(left: &reqwest::Url, right: &reqwest::Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().map(network::normalize_http_host)
            == right.host_str().map(network::normalize_http_host)
        && left.port_or_known_default() == right.port_or_known_default()
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
        assert_eq!(schema["properties"]["url"]["maxLength"], MAX_HTTP_URL_BYTES);
        assert_eq!(
            schema["properties"]["headers"]["maxProperties"],
            MAX_HTTP_REQUEST_HEADERS
        );
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

    async fn stall_once() -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let Ok((_sock, _)) = listener.accept().await else {
                return;
            };
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });
        (format!("http://{addr}/"), accepted_rx, task)
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
    async fn test_network_execute_json_body_is_formatted() {
        let url = serve_once("200 OK", "application/json", "{\"ok\":true}").await;
        let result = HttpTool::default()
            .execute(
                json!({"method": "GET", "url": url, "timeout_secs": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["format"], "json");
        assert_eq!(result.output["body"], "{\n  \"ok\": true\n}");
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
            &HttpDestinationPolicy {
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
    async fn native_http_request_bounds_reject_oversized_url_body_and_headers() {
        let tool = HttpTool::default();
        let cases = [
            HttpRequestParams {
                method: None,
                url: format!("https://example.com/{}", "a".repeat(MAX_HTTP_URL_BYTES)),
                headers: None,
                body: None,
                as_html: None,
                timeout_secs: None,
            },
            HttpRequestParams {
                method: None,
                url: "https://example.com/".into(),
                headers: None,
                body: Some("x".repeat(MAX_HTTP_REQUEST_BODY_BYTES + 1)),
                as_html: None,
                timeout_secs: None,
            },
            HttpRequestParams {
                method: Some(HttpRequestMethod::Get),
                url: "https://example.com/".into(),
                headers: None,
                body: Some("ignored by GET".into()),
                as_html: None,
                timeout_secs: None,
            },
            HttpRequestParams {
                method: None,
                url: "https://example.com/".into(),
                headers: Some(
                    (0..=MAX_HTTP_REQUEST_HEADERS)
                        .map(|index| (format!("x-test-{index}"), "v".into()))
                        .collect(),
                ),
                body: None,
                as_html: None,
                timeout_secs: None,
            },
        ];
        for params in cases {
            assert!(tool.run(params, CancellationToken::new()).await.is_err());
        }
    }

    #[tokio::test]
    async fn cancellation_drops_an_in_flight_http_request() {
        let (url, accepted, server) = stall_once().await;
        let cancel = CancellationToken::new();
        let request_cancel = cancel.clone();
        let request = tokio::spawn(async move {
            HttpTool::default()
                .run(
                    HttpRequestParams {
                        method: Some(HttpRequestMethod::Get),
                        url,
                        headers: None,
                        body: None,
                        as_html: None,
                        timeout_secs: Some(30),
                    },
                    request_cancel,
                )
                .await
        });
        accepted.await.expect("server accepted request");
        cancel.cancel();
        let result = request.await.unwrap().unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::Cancelled);
        server.abort();
    }

    #[tokio::test]
    async fn http_destination_policy_blocks_local_private_and_metadata_destinations() {
        let destination_policy = HttpDestinationPolicy::default();
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
                validate_http_destination(url, &destination_policy)
                    .await
                    .is_err(),
                "destination must be blocked: {url}"
            );
        }
    }

    #[test]
    fn http_destination_policy_allowlist_supports_exact_and_subdomain_entries() {
        assert!(network::http_domain_allowed("example.com", &[]));
        assert!(network::http_domain_allowed(
            "api.example.com",
            &["*.example.com".to_string()]
        ));
        assert!(!network::http_domain_allowed(
            "example.com",
            &["*.example.com".to_string()]
        ));
        assert!(network::http_domain_allowed(
            "example.com",
            &["EXAMPLE.COM.".to_string()]
        ));
        assert!(!network::http_domain_allowed(
            "other.example.net",
            &["example.com".to_string()]
        ));
    }

    #[tokio::test]
    async fn http_destination_policy_allows_loopback_only_for_test_configuration() {
        let destination_policy = HttpDestinationPolicy {
            allowed_domains: Vec::new(),
            allow_loopback: true,
        };
        assert!(
            validate_http_destination("http://127.0.0.1:1/", &destination_policy)
                .await
                .is_ok()
        );
        assert!(
            validate_http_destination("http://localhost:1/", &destination_policy)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn redirect_target_is_rechecked_before_following() {
        let initial = reqwest::Url::parse("https://public.example/start").unwrap();
        let target = initial.join("http://127.0.0.1:8080/").unwrap();
        assert!(
            validate_http_destination(target.as_str(), &HttpDestinationPolicy::default())
                .await
                .is_err()
        );

        let allowlist = HttpDestinationPolicy {
            allowed_domains: vec!["example.com".into()],
            allow_loopback: false,
        };
        assert!(
            validate_http_destination("https://other.example.com/", &allowlist)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_network_native_entry_lands_in_run() {
        let url = serve_once("200 OK", "text/plain", "hello native").await;
        let result = HttpTool::default()
            .run(
                HttpRequestParams {
                    method: Some(HttpRequestMethod::Get),
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
