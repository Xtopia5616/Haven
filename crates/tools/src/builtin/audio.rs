use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct AudioTool;

#[async_trait]
impl Tool for AudioTool {
    fn name(&self) -> String {
        "audio".into()
    }

    fn description(&self) -> String {
        "Play or record audio on the system".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("play") => RiskLevel::Low,
            Some("record") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["play", "record"]
                },
                "file_path": {
                    "type": "string",
                    "description": "Path to audio file for play operation"
                },
                "duration": {
                    "type": "number",
                    "description": "Recording duration in seconds"
                },
                "text": {
                    "type": "string",
                    "description": "Text to synthesize for TTS"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let _ = input;
        let _ = cancel;
        Err(anyhow::anyhow!("audio tool not yet implemented"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_audio_tool_name() {
        assert_eq!(AudioTool.name(), "audio");
    }

    #[test]
    fn test_audio_tool_description() {
        assert!(AudioTool.description().contains("audio"));
    }

    #[test]
    fn test_audio_tool_risk_level() {
        assert_eq!(
            AudioTool.risk_level(&json!({"operation": "play"})),
            RiskLevel::Low
        );
        assert_eq!(
            AudioTool.risk_level(&json!({"operation": "record"})),
            RiskLevel::Medium
        );
        assert_eq!(AudioTool.risk_level(&json!({})), RiskLevel::Low);
    }

    #[test]
    fn test_audio_tool_input_schema() {
        let schema = AudioTool.input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"play"));
        assert!(ops.contains(&"record"));
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"operation"));
        assert!(schema["properties"]["file_path"]["type"].as_str().is_some());
        assert!(schema["properties"]["duration"]["type"].as_str().is_some());
        assert!(schema["properties"]["text"]["type"].as_str().is_some());
    }

    #[tokio::test]
    async fn test_audio_execute_not_implemented() {
        let result = AudioTool
            .execute(
                json!({"operation": "play", "file_path": "x.wav"}),
                CancellationToken::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn test_audio_execute_cancelled_still_not_implemented() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = AudioTool
            .execute(json!({"operation": "record"}), cancel)
            .await;
        assert!(result.is_err());
    }
}
