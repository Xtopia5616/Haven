use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct TaskEvent {
    pub task_id: String,
    pub status: String,
    pub title: String,
}

#[derive(Clone, Serialize)]
#[allow(dead_code)]
pub struct StepEvent {
    pub step_id: String,
    pub task_id: String,
    pub status: String,
    pub tool_name: String,
}

#[derive(Clone, Serialize)]
#[allow(dead_code)]
pub struct ConfirmRequestEvent {
    pub step_id: String,
    pub tool_name: String,
    pub task_id: String,
    pub risk_level: String,
}

#[derive(Clone, Serialize)]
pub struct RecordingEvent {
    pub is_recording: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
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
#[allow(dead_code)]
pub struct TranscriptEvent {
    pub text: String,
    pub is_final: bool,
}

#[derive(Clone, Serialize)]
pub struct TranscriptionResultEvent {
    pub session_id: String,
    pub text: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
}

#[derive(Clone, Serialize)]
pub struct TranscriptionErrorEvent {
    pub session_id: String,
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
    fn test_task_event_serde() {
        let ev = TaskEvent {
            task_id: "t1".into(),
            status: "completed".into(),
            title: "Test".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"task_id\":\"t1\""));
        assert!(json.contains("\"status\":\"completed\""));
    }

    #[test]
    fn test_step_event_serde() {
        let ev = StepEvent {
            step_id: "s1".into(),
            task_id: "t1".into(),
            status: "running".into(),
            tool_name: "file".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"step_id\":\"s1\""));
        assert!(json.contains("\"tool_name\":\"file\""));
    }

    #[test]
    fn test_confirm_request_event_serde() {
        let ev = ConfirmRequestEvent {
            step_id: "s1".into(),
            tool_name: "delete".into(),
            task_id: "t1".into(),
            risk_level: "High".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"risk_level\":\"High\""));
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
    fn test_transcript_event_serde() {
        let ev = TranscriptEvent {
            text: "hello world".into(),
            is_final: true,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"text\":\"hello world\""));
        assert!(json.contains("\"is_final\":true"));
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
