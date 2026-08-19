use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_input::InputPipeline;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Default capture window when the LLM omits `duration`.
const DEFAULT_RECORD_SECS: f64 = 10.0;
/// Hard cap: the microphone belongs to the user first, and a runaway tool
/// call must not monopolize it (the pipeline's own `max_duration_secs`
/// config still applies as a final bound).
const MAX_RECORD_SECS: f64 = 60.0;

/// Play or record audio. `record` captures through the shared input pipeline
/// (same engine/STT as user voice input) and returns the transcription;
/// `play` is not yet implemented (no playback engine exists).
/// Play or record audio. `record` captures through the shared input pipeline
/// (same engine/STT as user voice input) and returns the transcription;
/// `play` is not yet implemented (no playback engine exists).
pub struct AudioTool {
    /// Shared capture/STT pipeline. `None` in headless/test contexts where
    /// recording is unavailable; the `record` operation then fails cleanly.
    pipeline: Option<Arc<InputPipeline>>,
}

/// Audio operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioOperation {
    Record,
    Play,
}

/// Typed parameters for `AudioTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `AudioTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AudioParams {
    /// What to do with audio.
    pub operation: AudioOperation,
    /// Path to audio file for play operation.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Recording duration in seconds (default 10, max 60).
    #[serde(default)]
    pub duration: Option<f64>,
    /// Text to synthesize for TTS.
    #[serde(default)]
    pub text: Option<String>,
}

impl AudioTool {
    pub fn new(pipeline: Option<Arc<InputPipeline>>) -> Self {
        Self { pipeline }
    }

    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: AudioParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        match params.operation {
            AudioOperation::Record => self.record(&params, cancel).await,
            AudioOperation::Play => Err(anyhow::anyhow!("audio tool: play is not yet implemented")),
        }
    }
}

#[async_trait]
impl Tool for AudioTool {
    fn name(&self) -> String {
        "audio".into()
    }

    fn description(&self) -> String {
        "Play or record audio on the system: `record` captures a clip through \
         the microphone, transcribes it with the configured STT provider and \
         returns the text"
            .into()
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
                    "description": "Recording duration in seconds (default 10, max 60)"
                },
                "text": {
                    "type": "string",
                    "description": "Text to synthesize for TTS"
                }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `AudioParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<AudioParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

impl AudioTool {
    /// Capture for `duration` seconds via the shared input pipeline, run STT
    /// and return the transcript. Never disturbs the user-facing recording
    /// UI: the pipeline's timed mode skips VAD auto-stop and handler
    /// notifications, and a user recording in flight is reported as an error.
    async fn record(
        &self,
        params: &AudioParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let Some(pipeline) = &self.pipeline else {
            return Err(anyhow::anyhow!(
                "audio tool: recording is unavailable in this context"
            ));
        };
        if !pipeline.recording_configured().await {
            return Err(anyhow::anyhow!(
                "audio tool: STT is not configured; enable an STT provider to record audio"
            ));
        }
        let duration = params
            .duration
            .unwrap_or(DEFAULT_RECORD_SECS)
            .clamp(1.0, MAX_RECORD_SECS);
        match pipeline.get_state().await {
            haven_input::RecordingState::Recording => {
                return Err(anyhow::anyhow!(
                    "audio tool: a recording is already in progress, try again later"
                ));
            }
            haven_input::RecordingState::Processing => {
                return Err(anyhow::anyhow!(
                    "audio tool: the previous recording is still processing, try again shortly"
                ));
            }
            haven_input::RecordingState::Pending => {}
        }

        let mut result = tokio::select! {
            r = pipeline.record_for(Duration::from_secs_f64(duration)) => r.map_err(|e| {
                anyhow::anyhow!("audio tool: recording failed: {e}")
            })?,
            _ = cancel.cancelled() => {
                // Release the microphone promptly; the partial capture is
                // discarded (nothing was delivered to the agent).
                let _ = pipeline.stop_capture().await;
                return Err(anyhow::anyhow!("audio tool: recording cancelled"));
            }
        };
        pipeline.transcribe(&mut result).await;

        if let Some(text) = result.transcript.filter(|t| !t.trim().is_empty()) {
            return Ok(ToolResult::ok(serde_json::json!({
                "transcript": text,
                "duration_ms": result.duration_ms,
            })));
        }
        let detail = result
            .transcript_error
            .unwrap_or_else(|| "no speech detected in the recording".into());
        Err(anyhow::anyhow!("audio tool: {detail}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_audio_tool_name() {
        assert_eq!(AudioTool::new(None).name(), "audio");
    }

    #[test]
    fn test_audio_tool_description() {
        assert!(AudioTool::new(None).description().contains("audio"));
    }

    #[test]
    fn test_audio_tool_risk_level() {
        assert_eq!(
            AudioTool::new(None).risk_level(&json!({"operation": "play"})),
            RiskLevel::Low
        );
        assert_eq!(
            AudioTool::new(None).risk_level(&json!({"operation": "record"})),
            RiskLevel::Medium
        );
        assert_eq!(AudioTool::new(None).risk_level(&json!({})), RiskLevel::Low);
    }

    #[test]
    fn test_audio_tool_input_schema() {
        let schema = AudioTool::new(None).input_schema();
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
    async fn test_audio_execute_play_not_implemented() {
        let result = AudioTool::new(None)
            .execute(
                json!({"operation": "play", "file_path": "x.wav"}),
                CancellationToken::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn test_audio_execute_record_without_pipeline() {
        let result = AudioTool::new(None)
            .execute(json!({"operation": "record"}), CancellationToken::new())
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn test_audio_execute_cancelled_record_without_pipeline() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = AudioTool::new(None)
            .execute(json!({"operation": "record"}), cancel)
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn test_audio_execute_unknown_operation() {
        let result = AudioTool::new(None)
            .execute(json!({"operation": "record_x"}), CancellationToken::new())
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unknown variant `record_x`"));
    }

    #[tokio::test]
    async fn test_audio_native_entry_lands_in_run() {
        let result = AudioTool::new(None)
            .run(
                AudioParams {
                    operation: AudioOperation::Record,
                    file_path: None,
                    duration: None,
                    text: None,
                },
                CancellationToken::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }
}
