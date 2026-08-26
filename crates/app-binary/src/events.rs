use serde::Serialize;
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
