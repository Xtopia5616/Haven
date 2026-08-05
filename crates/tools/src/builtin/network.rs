use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

const MAX_RETRIES: u32 = 2;
const BASE_BACKOFF_SECS: u64 = 1;
/// Cap on how much of the response body is read (and thus buffered).
const MAX_BODY_BYTES: usize = 1024 * 1024;

pub struct NetworkTool;

#[async_trait]
impl Tool for NetworkTool {
    fn name(&self) -> String {
        "network".into()
    }
    fn description(&self) -> String {
        "Fetch web pages or API data via HTTP GET/POST".into()
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
                "timeout_secs": { "type": "integer", "description": "Request timeout in seconds", "default": 15 }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let url = input["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("url is required"))?
            .to_string();
        let method = input["method"].as_str().unwrap_or("GET").to_string();
        let timeout_secs = input["timeout_secs"].as_i64().unwrap_or(15) as u64;

        let body = input["body"].as_str().map(|s| s.to_string());
        let headers = input["headers"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        // GET is idempotent → retry on transient errors. POST is not retried.
        let max_attempts = if method == "GET" { 1 + MAX_RETRIES } else { 1 };

        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = Duration::from_secs(BASE_BACKOFF_SECS * 2u64.pow(attempt - 1));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                }
            }
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }

            match execute_once(&url, &method, &headers, body.as_deref(), timeout_secs).await {
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

async fn execute_once(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout_secs: u64,
) -> anyhow::Result<ToolResult> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .user_agent("Haven/1.0")
        .build()?;

    execute_once_with(&client, url, method, headers, body).await
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
    let response_bytes = read_body_capped(response, max_chars).await?;
    let response_body = haven_common::encoding::decode_lossy(&response_bytes);

    let (body_truncated, truncated) =
        haven_common::encoding::truncate_output(&response_body, max_chars);

    Ok(ToolResult::ok(serde_json::json!({
        "status": status,
        "headers": resp_headers,
        "body": body_truncated,
        "truncated": truncated,
    })))
}

/// Read at most `display_chars * 4` bytes (bounded by `MAX_BODY_BYTES`) of the
/// response body, streaming, so huge responses never get fully buffered and
/// we never read far more than what will be shown.
async fn read_body_capped(
    response: reqwest::Response,
    display_chars: usize,
) -> anyhow::Result<Vec<u8>> {
    use futures_util::StreamExt;
    let cap = (display_chars * 4).min(MAX_BODY_BYTES);
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
        assert_eq!(NetworkTool.name(), "network");
    }

    #[test]
    fn test_network_tool_risk_level() {
        assert_eq!(NetworkTool.risk_level(&json!({})), RiskLevel::Medium);
    }

    #[test]
    fn test_network_tool_input_schema() {
        let schema = NetworkTool.input_schema();
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
    async fn serve_once(status_line: &str, body: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();
        let status = status_line.to_string();
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
                "HTTP/1.1 {}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status,
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
        let url = serve_once("200 OK", "hello from mock server").await;
        let result = NetworkTool
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

    #[tokio::test]
    async fn test_network_execute_get_not_found_no_retry() {
        let url = serve_once("404 Not Found", "nope").await;
        let result = NetworkTool
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
        let url = serve_once("201 Created", "created").await;
        let result = NetworkTool
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
        let result = execute_once_with(&client, &url, "POST", &[], Some("payload")).await;
        assert!(
            result.is_err(),
            "connection failure must surface as an error"
        );
    }

    #[tokio::test]
    async fn test_network_execute_unsupported_method() {
        let result = NetworkTool
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
                .contains("unsupported method")
        );
    }

    #[tokio::test]
    async fn test_network_execute_requires_url() {
        let result = NetworkTool
            .execute(json!({"method": "GET"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("url is required"));
    }

    #[tokio::test]
    async fn test_network_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = NetworkTool
            .execute(
                json!({"method": "GET", "url": "http://127.0.0.1:1/"}),
                cancel,
            )
            .await;
        assert!(result.is_err());
    }
}
