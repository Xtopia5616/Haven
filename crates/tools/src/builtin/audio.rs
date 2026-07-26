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

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let _ = input;
        let _ = cancel;
        Err(anyhow::anyhow!("audio tool not yet implemented"))
    }
}
