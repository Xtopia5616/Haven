use serde::Serialize;

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
