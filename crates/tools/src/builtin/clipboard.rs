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
        "Clipboard read and write operations via system native API".into()
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

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("read");

        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        match op {
            "read" => {
                let text = tokio::task::spawn_blocking(|| -> anyhow::Result<String> {
                    let mut cb = arboard::Clipboard::new()?;
                    cb.get_text().map_err(|e| anyhow::anyhow!("clipboard read failed: {}", e))
                }).await??;

                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(serde_json::json!({"content": text})))
            }
            "write" => {
                let content = input["content"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("'content' is required for write operation")
                })?.to_string();

                tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                    let mut cb = arboard::Clipboard::new()?;
                    cb.set_text(content).map_err(|e| anyhow::anyhow!("clipboard write failed: {}", e))
                }).await??;

                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
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
        assert!(ClipboardTool.description().contains("Clipboard"));
    }

    #[test]
    fn test_clipboard_tool_risk_level() {
        assert_eq!(ClipboardTool.risk_level(&json!({"operation": "write"})), RiskLevel::Medium);
        assert_eq!(ClipboardTool.risk_level(&json!({"operation": "read"})), RiskLevel::Low);
    }

    #[test]
    fn test_clipboard_tool_input_schema() {
        let schema = ClipboardTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"].as_array().unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
    }
}
