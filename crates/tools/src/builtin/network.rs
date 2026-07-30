use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

const MAX_RETRIES: u32 = 2;
const BASE_BACKOFF_SECS: u64 = 1;

pub struct NetworkTool;

#[async_trait]
impl Tool for NetworkTool {
    fn name(&self) -> String {
        "network".into()
    }
    fn description(&self) -> String {
        "Make basic HTTP requests to fetch web pages or API data. Supports GET and POST.".into()
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

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
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
        .map(|(k, v)| {
            serde_json::json!({"name": k.as_str(), "value": v.to_str().unwrap_or("")})
        })
        .collect();

    let response_bytes = response.bytes().await.map_err(map_reqwest_error)?;
    let response_body = haven_common::encoding::decode_lossy(&response_bytes);

    let max_chars = 100_000;
    let (body_truncated, truncated) = truncate_output(&response_body, max_chars);

    Ok(ToolResult::ok(serde_json::json!({
        "status": status,
        "headers": resp_headers,
        "body": body_truncated,
        "truncated": truncated,
    })))
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

fn truncate_output(text: &str, max_chars: usize) -> (String, bool) {
    if text.len() <= max_chars {
        (text.to_string(), false)
    } else {
        let truncated = format!(
            "{}[truncated ... {} chars omitted]",
            &text[..max_chars],
            text.len() - max_chars
        );
        (truncated, true)
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
}
