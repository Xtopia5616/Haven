use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct ClipboardTool;

#[async_trait]
impl Tool for ClipboardTool {
    fn name(&self) -> String {
        "clipboard".into()
    }
    fn description(&self) -> String {
        "Read or write the system clipboard".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("write") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write"] },
                "content": { "type": "string" }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("read");

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        match op {
            "read" => {
                let text = tokio::task::spawn_blocking(|| -> anyhow::Result<String> {
                    let mut cb = arboard::Clipboard::new()?;
                    cb.get_text()
                        .map_err(|e| anyhow::anyhow!("clipboard read failed: {}", e))
                })
                .await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let max_chars = self.max_output_chars();
                let (text, truncated) = haven_common::encoding::truncate_output(&text, max_chars);
                let mut result = serde_json::json!({"content": text});
                if truncated {
                    result["truncated"] = serde_json::Value::Bool(true);
                }
                Ok(ToolResult::ok(result))
            }
            "write" => {
                let content = input["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'content' is required for write operation"))?
                    .to_string();

                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let mut cb = arboard::Clipboard::new()?;
                    cb.set_text(content)
                        .map_err(|e| anyhow::anyhow!("clipboard write failed: {}", e))
                })
                .await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                Ok(ToolResult::ok(serde_json::json!({"written": true})))
            }
            _ => anyhow::bail!("unknown clipboard operation: {}", op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_clipboard_tool_name() {
        assert_eq!(ClipboardTool.name(), "clipboard");
    }

    #[test]
    fn test_clipboard_tool_description() {
        assert!(ClipboardTool.description().contains("clipboard"));
    }

    #[test]
    fn test_clipboard_tool_risk_level() {
        assert_eq!(
            ClipboardTool.risk_level(&json!({"operation": "write"})),
            RiskLevel::Medium
        );
        assert_eq!(
            ClipboardTool.risk_level(&json!({"operation": "read"})),
            RiskLevel::Low
        );
    }

    #[test]
    fn test_clipboard_tool_input_schema() {
        let schema = ClipboardTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
    }

    #[tokio::test]
    async fn test_clipboard_write_read_roundtrip() {
        let content = format!("haven-clipboard-test-{}", std::process::id());
        let write = ClipboardTool
            .execute(
                json!({"operation": "write", "content": content.clone()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(write.success);
        assert_eq!(write.output["written"], true);

        let read = ClipboardTool
            .execute(json!({"operation": "read"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(read.success);
        assert_eq!(read.output["content"], content);
    }

    #[tokio::test]
    async fn test_clipboard_write_requires_content() {
        let result = ClipboardTool
            .execute(json!({"operation": "write"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clipboard_unknown_operation() {
        let result = ClipboardTool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clipboard_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = ClipboardTool
            .execute(json!({"operation": "read"}), cancel)
            .await;
        assert!(result.is_err());
    }
}
