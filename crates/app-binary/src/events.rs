use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable session event channels exposed by the Tauri boundary.
///
/// Keep the string literals here so command handlers and the Agent event
/// adapter cannot silently drift apart. The frontend's matching contract and
/// its snake_case-to-camelCase boundary conversion live in
/// `ui/src/lib/contracts/session.ts`.
pub(crate) const SESSION_CREATED_EVENT: &str = "session:created";
pub(crate) const SESSION_UPDATED_EVENT: &str = "session:updated";
pub(crate) const SESSION_COMPLETED_EVENT: &str = "session:completed";
pub(crate) const SESSION_ERROR_EVENT: &str = "session:error";
pub(crate) const SESSION_TITLE_UPDATED_EVENT: &str = "session:title-updated";
pub(crate) const SESSION_DELETED_EVENT: &str = "session:deleted";

/// Stable recording and transcription event channels exposed by the Tauri
/// boundary. Keep these names beside their DTOs so every producer shares one
/// public wire directory.
pub(crate) const RECORDING_STARTED_EVENT: &str = "recording:started";
pub(crate) const RECORDING_STOPPED_EVENT: &str = "recording:stopped";
pub(crate) const RECORDING_VAD_STATUS_EVENT: &str = "recording:vad_status";
pub(crate) const RECORDING_ERROR_EVENT: &str = "recording:error";
pub(crate) const TRANSCRIPTION_STARTED_EVENT: &str = "transcription:started";
pub(crate) const TRANSCRIPTION_RESULT_EVENT: &str = "transcription:result";
pub(crate) const TRANSCRIPTION_ERROR_EVENT: &str = "transcription:error";

/// Stable action event channels exposed by the Tauri boundary.
///
/// The tool crate deliberately owns execution, but it does not own the IPC
/// contract.  Keep the public channel names and their projected payload here,
/// alongside the session contract, so internal tool status JSON cannot become
/// an accidental frontend API.
pub(crate) const ACTION_CREATED_EVENT: &str = "action:created";
pub(crate) const ACTION_UPDATED_EVENT: &str = "action:updated";
pub(crate) const ACTION_OUTPUT_EVENT: &str = "action:output";
pub(crate) const ACTION_FINISHED_EVENT: &str = "action:finished";

/// Stable channels for the remaining app-shell events. Their producers may
/// live in different crates, but the Tauri wire names and DTOs belong here.
pub(crate) const APP_BOOTSTRAP_EVENT: &str = "app:bootstrap";
pub(crate) const TRAY_STATUS_CHANGED_EVENT: &str = "tray:status_changed";
pub(crate) const MUTE_CHANGED_EVENT: &str = "mute:changed";
pub(crate) const MCP_STATUS_CHANGED_EVENT: &str = "mcp:status_change";
pub(crate) const SKILLS_STATUS_CHANGED_EVENT: &str = "skills:status_change";
pub(crate) const CONFIRM_REQUESTED_EVENT: &str = "confirm:requested";
pub(crate) const HOTKEY_CONFLICT_EVENT: &str = "hotkey:conflict";
pub(crate) const HOTKEY_REBIND_EVENT: &str = "hotkey:rebind";
pub(crate) const LLM_CONFIG_CHANGED_EVENT: &str = "llm:config_changed";

/// Agent event channels that are not session lifecycle events.
pub(crate) const AGENT_THOUGHT_EVENT: &str = "agent:thought";
pub(crate) const AGENT_ACTION_EVENT: &str = "agent:action";
pub(crate) const AGENT_OBSERVATION_EVENT: &str = "agent:observation";
pub(crate) const AGENT_THOUGHT_CHUNK_EVENT: &str = "agent:thought_chunk";
pub(crate) const AGENT_REASONING_CHUNK_EVENT: &str = "agent:reasoning_chunk";
pub(crate) const AGENT_STREAM_RESET_EVENT: &str = "agent:stream_reset";
pub(crate) const AGENT_WEB_SEARCH_EVENT: &str = "agent:web_search";
pub(crate) const AGENT_STREAM_STALLED_EVENT: &str = "agent:stream_stalled";
pub(crate) const AGENT_SUPPLEMENT_EVENT: &str = "agent:supplement";
pub(crate) const AGENT_COMPACTION_EVENT: &str = "agent:compaction";
pub(crate) const AGENT_USAGE_EVENT: &str = "agent:usage";
pub(crate) const AGENT_TOOL_OUTPUT_EVENT: &str = "agent:tool_output";
pub(crate) const NOTIFICATION_SHOW_EVENT: &str = "notification:show";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Background,
    Scheduled,
}

/// The minimal action record displayed by the task panel and history.
///
/// This intentionally excludes internal dynamic tool parameters, continuation
/// prompts, and local output-log paths.  Those are execution details rather
/// than a stable UI contract and may contain sensitive values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActionEvent {
    pub id: String,
    pub kind: ActionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

impl ActionEvent {
    pub(crate) fn background_from_value(payload: &Value) -> Result<Self, String> {
        Ok(Self {
            id: required_string(payload, "action_id")?,
            kind: ActionKind::Background,
            status: optional_string(payload, "status")?,
            session_id: optional_string(payload, "session_id")?,
            started_at: optional_string(payload, "started_at")?,
            finished_at: optional_string(payload, "finished_at")?,
            due_at: None,
            title: None,
            body: None,
            mode: None,
            command: optional_string(payload, "command")?,
            output: optional_string(payload, "output")?,
            error: optional_string(payload, "error")?,
            error_reason: optional_string(payload, "error_reason")?,
            exit_code: optional_i32(payload, "exit_code")?,
            preview: optional_string(payload, "preview")?,
        })
    }

    pub(crate) fn scheduled_from_value(payload: &Value, cancelled: bool) -> Result<Self, String> {
        Ok(Self {
            id: required_string(payload, "id")?,
            kind: ActionKind::Scheduled,
            status: cancelled.then(|| "cancelled".to_string()),
            session_id: optional_string(payload, "session_id")?,
            started_at: None,
            finished_at: None,
            due_at: optional_string(payload, "due_at")?,
            title: optional_string(payload, "title")?,
            body: optional_string(payload, "body")?,
            mode: optional_string(payload, "mode")?,
            command: None,
            output: None,
            error: None,
            error_reason: None,
            exit_code: None,
            preview: None,
        })
    }
}

impl ActionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Scheduled => "scheduled",
        }
    }
}

fn required_string(payload: &Value, field: &str) -> Result<String, String> {
    let value = optional_string(payload, field)?
        .ok_or_else(|| format!("action payload missing string '{field}'"))?;
    if value.is_empty() {
        return Err(format!("action payload string '{field}' cannot be empty"));
    }
    Ok(value)
}

fn optional_string(payload: &Value, field: &str) -> Result<Option<String>, String> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(value) => Err(format!(
            "action payload field '{field}' must be a string or null, got {}",
            value_type(value)
        )),
    }
}

fn optional_i32(payload: &Value, field: &str) -> Result<Option<i32>, String> {
    match payload.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_i64()
            .and_then(|value| i32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("action payload field '{field}' must be a 32-bit integer")),
        Some(value) => Err(format!(
            "action payload field '{field}' must be an integer or null, got {}",
            value_type(value)
        )),
    }
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Shared wire payload for session lifecycle transitions.
///
/// `status` intentionally remains a string: the Agent owns the state-machine
/// vocabulary and can add a persisted state without coupling that enum to the
/// Tauri adapter. This DTO fixes the public field set instead.
#[derive(Clone, Serialize)]
pub(crate) struct SessionLifecycleEvent {
    pub session_id: String,
    pub status: String,
    /// A newly-created session may not have a generated title yet.
    pub title: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct SessionErrorEvent {
    pub session_id: String,
    pub error: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct SessionTitleUpdatedEvent {
    pub session_id: String,
    pub title: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct SessionDeletedEvent {
    /// `None` means every session was removed by `clear_history`.
    pub session_id: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct RecordingEvent {
    pub is_recording: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<haven_common::types::SessionId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Serialize)]
pub struct VadStatusEvent {
    pub signal: String,
    pub state: String,
}

#[derive(Clone, Serialize)]
pub struct TranscriptionResultEvent {
    pub session_id: haven_common::types::SessionId,
    pub text: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

/// Emitted right before STT runs, so the UI can show a "transcribing" hint
/// covering the gap between `recording:stopped` and `transcription:result`.
#[derive(Clone, Serialize)]
pub struct TranscriptionStartedEvent {
    pub session_id: haven_common::types::SessionId,
}

#[derive(Clone, Serialize)]
pub struct TranscriptionErrorEvent {
    pub session_id: haven_common::types::SessionId,
    pub error: String,
}

/// User-facing recording failure. This is deliberately separate from a
/// transcription failure because it can occur before capture begins.
#[derive(Clone, Serialize)]
pub struct RecordingErrorEvent {
    pub session_id: haven_common::types::SessionId,
    pub error: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AppBootstrapEvent {
    pub status: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct TrayStatusChangedEvent {
    pub status: String,
    pub tooltip: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct MuteChangedEvent {
    pub muted: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct McpStatusChangedEvent {
    pub name: String,
    pub status: haven_tools::McpClientStatus,
}

#[derive(Clone, Serialize)]
pub(crate) struct SkillsStatusChangedEvent {
    pub op: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct ConfirmationRequestedEvent {
    /// Confirmation request id used by `session:resolve_confirmation`.
    pub step_id: haven_common::types::ConfirmId,
    /// ReAct invocation identity. `None` for scheduled/background actions.
    pub invocation_step_id: Option<String>,
    pub action_index: u32,
    pub tool_call_id: Option<String>,
    pub tool_name: String,
    pub risk_level: haven_common::types::RiskLevel,
    pub session_id: String,
    /// Renderer-safe explanation. Raw tool parameters remain backend-only.
    pub summary: String,
    pub permission_key: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct HotkeyConflictEvent {
    pub binding: String,
    pub error: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct HotkeyRebindEvent {
    pub old_binding: String,
    pub new_binding: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentThoughtEvent {
    pub session_id: String,
    pub thought: String,
    pub step_number: u32,
    pub run_id: u64,
    pub message_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentActionEvent {
    pub session_id: String,
    pub tool_name: String,
    pub input: Value,
    pub step_number: u32,
    pub run_id: u64,
    pub tool_call_id: Option<String>,
    pub action_index: u32,
    pub step_id: String,
    pub suppress_streamed_thought: bool,
    pub silent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentObservationEvent {
    pub session_id: String,
    pub observation: String,
    pub tool_name: String,
    pub step_number: u32,
    pub run_id: u64,
    pub silent: bool,
    pub tool_call_id: Option<String>,
    pub action_index: u32,
    pub ask_options: Vec<String>,
    pub step_id: String,
    pub outcome: String,
    pub idempotency: String,
    pub operation_scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentThoughtChunkEvent {
    pub session_id: String,
    pub delta: String,
    pub step_number: u32,
    pub run_id: u64,
    pub message_id: String,
    pub seq: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentReasoningChunkEvent {
    pub session_id: String,
    pub delta: String,
    pub step_number: u32,
    pub run_id: u64,
    pub message_id: String,
    pub seq: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentStreamResetEvent {
    pub session_id: String,
    pub step_number: u32,
    pub run_id: u64,
    pub thought_message_id: String,
    pub reasoning_message_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentWebSearchEvent {
    pub session_id: String,
    pub phase: String,
    pub step_number: u32,
    pub run_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentStreamStalledEvent {
    pub session_id: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentSupplementEvent {
    pub session_id: String,
    pub additional_context: String,
    pub step_number: u32,
    pub run_id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub supplement_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inject_source: Option<haven_common::types::InjectSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<u64>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentCompactionEvent {
    pub session_id: String,
    pub summary: String,
    pub tokens_before: u32,
    pub tokens_after: u32,
    pub degraded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_id: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentNotificationEvent {
    pub session_id: String,
    pub title: String,
    pub body: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct AgentUsageEvent {
    pub session_id: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_miss_tokens: u32,
    pub context_tokens: u32,
    pub cache_exclusive: bool,
    pub cache_accounting: String,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub cumulative_prompt_tokens: u32,
    pub cumulative_completion_tokens: u32,
    pub cumulative_total_tokens: u32,
    pub cumulative_cached_tokens: u32,
    pub cumulative_cache_creation_tokens: u32,
    pub cumulative_cache_miss_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_diagnostics: Option<haven_llm::CacheDiagnostics>,
    pub cumulative_cost_usd: Option<f64>,
    pub context_window: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub has_cost: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct AgentToolOutputEvent {
    pub session_id: String,
    pub step_id: String,
    pub output: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recording_event_serde() {
        let ev = RecordingEvent {
            is_recording: true,
            session_id: Some("s1".into()),
            reason: Some("manual".into()),
            duration_ms: Some(1000),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"is_recording\":true"));
        assert!(json.contains("\"session_id\":\"s1\""));
    }

    #[test]
    fn session_lifecycle_event_has_the_stable_wire_shape() {
        let event = SessionLifecycleEvent {
            session_id: "ses-1".into(),
            status: "paused".into(),
            title: Some("Plan migration".into()),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "session_id": "ses-1",
                "status": "paused",
                "title": "Plan migration",
            })
        );
    }

    #[test]
    fn session_deleted_event_can_signal_global_clear() {
        let event = SessionDeletedEvent { session_id: None };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({ "session_id": null })
        );
    }

    #[test]
    fn action_event_projects_background_status_to_the_stable_wire_shape() {
        let event = ActionEvent::background_from_value(&serde_json::json!({
            "action_id": "act-1",
            "status": "completed",
            "session_id": "ses-1",
            "output": "done",
            "exit_code": 0,
            "log_path": "C:/private/action.log",
        }))
        .unwrap();

        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({
                "id": "act-1",
                "kind": "background",
                "status": "completed",
                "session_id": "ses-1",
                "output": "done",
                "exit_code": 0,
            })
        );
    }

    #[test]
    fn action_event_hides_scheduled_execution_details() {
        let event = ActionEvent::scheduled_from_value(
            &serde_json::json!({
                "id": "act-2",
                "title": "Reminder",
                "body": "Take a break",
                "mode": "tool",
                "tool_name": "notify",
                "tool_args": { "token": "secret" },
                "prompt": "private continuation",
            }),
            false,
        )
        .unwrap();
        let wire = serde_json::to_value(event).unwrap();

        assert_eq!(wire["id"], "act-2");
        assert_eq!(wire["kind"], "scheduled");
        assert!(wire.get("tool_name").is_none());
        assert!(wire.get("tool_args").is_none());
        assert!(wire.get("prompt").is_none());
    }

    #[test]
    fn action_projection_rejects_wrong_optional_field_types() {
        let result = ActionEvent::background_from_value(&serde_json::json!({
            "action_id": "act-1",
            "status": 42,
        }));
        assert_eq!(
            result.unwrap_err(),
            "action payload field 'status' must be a string or null, got number"
        );
    }

    #[test]
    fn action_projection_rejects_out_of_range_exit_codes() {
        let result = ActionEvent::background_from_value(&serde_json::json!({
            "action_id": "act-1",
            "exit_code": 2_147_483_648_i64,
        }));
        assert_eq!(
            result.unwrap_err(),
            "action payload field 'exit_code' must be a 32-bit integer"
        );
    }

    #[test]
    fn action_projection_rejects_empty_ids() {
        let result = ActionEvent::scheduled_from_value(&serde_json::json!({"id": ""}), false);
        assert_eq!(
            result.unwrap_err(),
            "action payload string 'id' cannot be empty"
        );
    }

    #[test]
    fn test_recording_event_skips_optional_none() {
        let ev = RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: None,
            duration_ms: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("session_id"));
        assert!(!json.contains("reason"));
    }

    #[test]
    fn test_vad_status_event_serde() {
        let ev = VadStatusEvent {
            signal: "speech".into(),
            state: "speaking".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"signal\":\"speech\""));
    }

    #[test]
    fn test_transcription_result_event_serde() {
        let ev = TranscriptionResultEvent {
            session_id: "s1".into(),
            text: "hello".into(),
            duration_ms: 500,
            confidence: Some(0.95),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"session_id\":\"s1\""));
        assert!(json.contains("\"confidence\":0.95"));
    }

    #[test]
    fn test_transcription_started_event_serde() {
        let ev = TranscriptionStartedEvent {
            session_id: "rec-abc".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"session_id\":\"rec-abc\""));
    }

    #[test]
    fn test_transcription_error_event_serde() {
        let ev = TranscriptionErrorEvent {
            session_id: "s1".into(),
            error: "timeout".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"error\":\"timeout\""));
    }

    #[test]
    fn recording_error_event_has_the_stable_wire_shape() {
        let event = RecordingErrorEvent {
            session_id: "rec-1".into(),
            error: "microphone unavailable".into(),
        };
        assert_eq!(
            serde_json::to_value(event).unwrap(),
            serde_json::json!({ "session_id": "rec-1", "error": "microphone unavailable" })
        );
    }

    #[test]
    fn test_recording_event_all_fields_serialized() {
        let ev = RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: None,
            duration_ms: None,
        };
        let json = serde_json::to_string(&ev).unwrap();
        // Should only contain is_recording
        assert_eq!(json, r#"{"is_recording":false}"#);
    }
}
