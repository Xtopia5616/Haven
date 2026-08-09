use serde::Serialize;

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
