use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct NetworkTool;

#[async_trait]
impl Tool for NetworkTool {
    fn name(&self) -> String {
        "network".into()
    }
    fn description(&self) -> String {
        "Make HTTP requests to fetch web pages or API data. Supports GET and POST.".into()
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
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        let url = input["url"].as_str().ok_or_else(|| anyhow::anyhow!("url is required"))?;
        let method = input["method"].as_str().unwrap_or("GET");
        let timeout_secs = input["timeout_secs"].as_i64().unwrap_or(15) as u64;

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs))
            .user_agent("Haven/1.0")
            .build()?;

        let mut req = match method {
            "GET" => client.get(url),
            "POST" => {
                let body = input["body"].as_str().unwrap_or("");
                client.post(url).body(body.to_string())
            }
            _ => anyhow::bail!("unsupported method: {}", method),
        };

        // Apply optional headers
        if let Some(headers) = input["headers"].as_object() {
            for (key, val) in headers {
                if let Some(val_str) = val.as_str() {
                    req = req.header(key.as_str(), val_str);
                }
            }
        }

        let response = req.send().await?;
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        let status = response.status().as_u16();
        let headers: Vec<Value> = response
            .headers()
            .iter()
            .map(|(k, v)| serde_json::json!({"name": k.as_str(), "value": v.to_str().unwrap_or("")}))
            .collect();

        let body = response.text().await?;
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        let max_chars = self.max_output_chars();
        let (body_truncated, truncated) = truncate_output(&body, max_chars);

        Ok(ToolResult::ok(serde_json::json!({
            "status": status,
            "headers": headers,
            "body": body_truncated,
            "truncated": truncated,
        })))
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
}
