use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct NetworkTool {
    /// Max retries for failed HTTP requests.
    pub max_retries: u32,
    /// Exponential backoff base (secs) between retries.
    pub backoff_base_secs: u64,
    /// Cap on how much of the response body is read (and thus buffered).
    pub max_body_bytes: usize,
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

/// Typed parameters for `NetworkTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `NetworkTool::run`.
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

impl NetworkTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: NetworkParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let url = params.url;
        let method = params
            .method
            .map(|m| m.as_str().to_string())
            .unwrap_or_else(|| "GET".to_string());
        let timeout_secs = params.timeout_secs.unwrap_or(15) as u64;

        let body = params.body;
        let as_html = params.as_html.unwrap_or(false);
        let headers: Vec<(String, String)> =
            params.headers.unwrap_or_default().into_iter().collect();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // GET is idempotent → retry on transient errors. POST is not retried.
        let max_attempts = if method == "GET" {
            1 + self.max_retries
        } else {
            1
        };

        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = Duration::from_secs(self.backoff_base_secs * 2u64.pow(attempt - 1));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                }
            }
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }

            match execute_once(
                &url,
                &method,
                &headers,
                body.as_deref(),
                as_html,
                timeout_secs,
                self.max_body_bytes,
            )
            .await
            {
                Ok(result) => return Ok(result),
                Err(e) if attempt + 1 < max_attempts && is_retryable_error(&e) => {
                    tracing::debug!(
                        "network tool attempt {} failed, retrying: {}",
                        attempt + 1,
                        e
                    );
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        anyhow::bail!("unreachable: network tool retry loop exhausted")
    }
}

impl Default for NetworkTool {
    fn default() -> Self {
        Self {
            max_retries: 2,
            backoff_base_secs: 1,
            max_body_bytes: 1024 * 1024,
        }
    }
}

#[async_trait]
impl Tool for NetworkTool {
    fn name(&self) -> String {
        "network".into()
    }
    fn description(&self) -> String {
        "Fetch web pages or API data via HTTP GET/POST. HTML pages are converted to plain text by default; pass as_html to get the raw HTML instead.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "method": { "type": "string", "enum": ["GET", "POST"], "default": "GET" },
                "url": { "type": "string", "description": "The URL to request" },
                "headers": { "type": "object", "description": "Optional HTTP headers as key-value pairs" },
                "body": { "type": "string", "description": "Request body for POST" },
                "as_html": { "type": "boolean", "description": "Return the raw HTML instead of converting HTML pages to plain text (default false)" },
                "timeout_secs": { "type": "integer", "description": "Request timeout in seconds", "default": 15 }
            },
            "required": ["url"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `NetworkParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<NetworkParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

async fn execute_once(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    timeout_secs: u64,
    max_body_bytes: usize,
) -> anyhow::Result<ToolResult> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("Haven/1.0");
    // Route through a locally detected proxy so international requests work
    // when the user runs one (e.g. 127.0.0.1:10808) — same detection as the
    // shell tool's spawned commands. User-set env vars already short-circuit
    // the probe (reqwest also honors them natively).
    for (key, val) in crate::bg::proxy_env_vars() {
        if key == "HTTP_PROXY" || key == "http_proxy" {
            builder = builder.proxy(reqwest::Proxy::http(&val)?);
        } else if key == "HTTPS_PROXY" || key == "https_proxy" {
            builder = builder.proxy(reqwest::Proxy::https(&val)?);
        }
    }
    let client = builder.build()?;

    execute_once_with(&client, url, method, headers, body, as_html, max_body_bytes).await
}

/// Send one request with a caller-supplied client. Split out so tests can
/// exercise the connection-error path with a proxy-free client: a system or
/// environment proxy can answer loopback requests with its own error page
/// (e.g. 502) instead of relaying the peer's reset, masking the failure.
async fn execute_once_with(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    as_html: bool,
    max_body_bytes: usize,
) -> anyhow::Result<ToolResult> {
    let mut req = match method {
        "GET" => client.get(url),
        "POST" => client.post(url).body(body.unwrap_or("").to_string()),
        _ => anyhow::bail!("unsupported method: {}", method),
    };

    for (key, val) in headers {
        req = req.header(key.as_str(), val.as_str());
    }

    let response = req.send().await.map_err(map_reqwest_error)?;

    let status = response.status().as_u16();
    let resp_headers: Vec<Value> = response
        .headers()
        .iter()
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
    let response_bytes = read_body_capped(response, byte_cap, max_body_bytes).await?;
    let response_body = haven_common::encoding::decode_lossy(&response_bytes);

    let is_html = html_by_header || looks_like_html(&response_body);

    let (body_truncated, truncated, format) = if is_html && !as_html {
        let (t, tr) =
            haven_common::encoding::truncate_output(&html_to_text(&response_body), max_chars);
        (t, tr, "text")
    } else {
        let (t, tr) = haven_common::encoding::truncate_output(&response_body, max_chars);
        (t, tr, if is_html { "html" } else { "raw" })
    };

    Ok(ToolResult::ok(serde_json::json!({
        "status": status,
        "headers": resp_headers,
        "body": body_truncated,
        "truncated": truncated,
        "format": format,
    })))
}

/// Read at most `byte_cap` bytes (bounded by `max_body_bytes`) of the response
/// body, streaming, so huge responses never get fully buffered and we never
/// read far more than what will be shown.
async fn read_body_capped(
    response: reqwest::Response,
    byte_cap: usize,
    max_body_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    use futures_util::StreamExt;
    let cap = byte_cap.min(max_body_bytes);
    let mut stream = response.bytes_stream();
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(map_reqwest_error)?;
        let room = cap.saturating_sub(out.len());
        if room == 0 {
            break;
        }
        let take = chunk.len().min(room);
        out.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            break;
        }
    }
    Ok(out)
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

fn is_retryable_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    // Timeout: tokio timeout or reqwest timeout
    if msg.contains("timed out") || msg.contains("timeout") || msg.contains("timedout") {
        return true;
    }
    // Connection / DNS / IO errors
    if msg.contains("connection refused")
        || msg.contains("connection reset")
        || msg.contains("dns")
        || msg.contains("no route to host")
        || msg.contains("eof")
    {
        return true;
    }
    false
}

fn map_reqwest_error(e: reqwest::Error) -> anyhow::Error {
    if e.is_timeout() {
        anyhow::anyhow!("request timed out: {}", e)
    } else if e.is_connect() {
        anyhow::anyhow!("connection failed: {}", e)
    } else if e.is_status() {
        anyhow::anyhow!("HTTP error: {}", e)
    } else {
        anyhow::anyhow!("request failed: {}", e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_network_tool_name() {
        assert_eq!(NetworkTool::default().name(), "network");
    }

    #[test]
    fn test_network_tool_risk_level() {
        assert_eq!(
            NetworkTool::default().risk_level(&json!({})),
            RiskLevel::Medium
        );
    }

    #[test]
    fn test_network_tool_input_schema() {
        let schema = NetworkTool::default().input_schema();
        assert!(schema["properties"]["url"].is_object());
    }

    #[test]
    fn test_is_retryable_error() {
        let e1 = anyhow::anyhow!("request timed out");
        assert!(is_retryable_error(&e1));

        let e2 = anyhow::anyhow!("connection refused: 127.0.0.1:8080");
        assert!(is_retryable_error(&e2));

        let e3 = anyhow::anyhow!("HTTP error: 404 Not Found");
        assert!(!is_retryable_error(&e3));

        let e4 = anyhow::anyhow!("invalid URL: bad format");
        assert!(!is_retryable_error(&e4));
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
        let result = NetworkTool::default()
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
        let headers = result.output["headers"].as_array().unwrap();
        assert!(headers.iter().any(|h| h["name"] == "content-type"));
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
        let result = NetworkTool::default()
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
        let result = NetworkTool::default()
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
        let result = NetworkTool::default()
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
        let result = NetworkTool::default()
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
        let result = NetworkTool::default()
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
        )
        .await;
        assert!(
            result.is_err(),
            "connection failure must surface as an error"
        );
    }

    #[tokio::test]
    async fn test_network_execute_unsupported_method() {
        let result = NetworkTool::default()
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
        let result = NetworkTool::default()
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
        let result = NetworkTool::default()
            .execute(
                json!({"method": "GET", "url": "http://127.0.0.1:1/"}),
                cancel,
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_network_native_entry_lands_in_run() {
        let url = serve_once("200 OK", "text/plain", "hello native").await;
        let result = NetworkTool::default()
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
