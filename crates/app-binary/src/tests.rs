//! Unit tests for the app-binary composition boundary.

use crate::bootstrap::to_tauri_shortcut;
use crate::desktop::TrayStatus;
use crate::event_bridge::{TauriEmitter, project_action_event};
use crate::events::*;
use crate::handlers::make_tray_icon;
use haven_agent::{AgentEvent, SessionInfo, SessionStatus};
use serde_json::json;

/// Parse a binding through the unified `haven-input` hotkey parser and
/// convert to the Tauri shortcut type (the production startup path).
fn parse_shortcut(binding: &str) -> Option<tauri_plugin_global_shortcut::Shortcut> {
    haven_input::hotkey::KeyCombo::parse(binding).and_then(|combo| to_tauri_shortcut(&combo))
}

fn test_session_info() -> SessionInfo {
    SessionInfo {
        id: "ses-1".into(),
        input: "my input".into(),
        summary: "my summary".into(),
        title: Some("My Title".into()),
        status: SessionStatus::Running,
        steps: vec![],
        follow_up_queue: vec![],
        steering_queue: vec![],
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

#[test]
fn channel_maps_every_variant_to_expected_channel() {
    let cases: Vec<(AgentEvent, &str)> = vec![
        (
            AgentEvent::Thought {
                session_id: "t".into(),
                thought: "x".into(),
                step_number: 1,
                run_id: 1,
                message_id: "msg-1".into(),
            },
            "agent:thought",
        ),
        (
            AgentEvent::Action {
                session_id: "t".into(),
                tool_name: "read_file".into(),
                input: json!({}),
                step_number: 1,
                run_id: 1,
                tool_call_id: None,
                step_id: "step-1".into(),
                action_index: 0,
                suppress_streamed_thought: false,
            },
            "agent:action",
        ),
        (
            AgentEvent::Observation {
                session_id: "t".into(),
                observation: "o".into(),
                tool_name: "read_file".into(),
                step_number: 1,
                run_id: 1,
                silent: false,
                tool_call_id: None,
                ask_options: vec![],
                step_id: "step-1".into(),
                action_index: 0,
            },
            "agent:observation",
        ),
        (
            AgentEvent::SessionCreated(test_session_info()),
            "session:created",
        ),
        (
            AgentEvent::SessionCompleted {
                session_id: "t".into(),
                title: "x".into(),
            },
            "session:completed",
        ),
        (
            AgentEvent::SessionUpdated {
                session_id: "t".into(),
                status: "paused".into(),
            },
            "session:updated",
        ),
        (
            AgentEvent::SessionError {
                session_id: "t".into(),
                error: "e".into(),
            },
            "session:error",
        ),
        (
            AgentEvent::Notification {
                session_id: "t".into(),
                title: "x".into(),
                body: "y".into(),
            },
            "notification:show",
        ),
        (
            AgentEvent::TitleUpdated {
                session_id: "t".into(),
                title: "x".into(),
            },
            "session:title-updated",
        ),
        (
            AgentEvent::BalancedModelActivated {
                session_id: "t".into(),
                reason: "r".into(),
            },
            "agent:balanced_model",
        ),
        (
            AgentEvent::ThoughtChunk {
                session_id: "t".into(),
                delta: "d".into(),
                step_number: 1,
                run_id: 1,
                message_id: "msg-1".into(),
            },
            "agent:thought_chunk",
        ),
        (
            AgentEvent::ReasoningChunk {
                session_id: "t".into(),
                delta: "d".into(),
                step_number: 1,
                run_id: 1,
                message_id: "msg-2".into(),
            },
            "agent:reasoning_chunk",
        ),
        (
            AgentEvent::StreamReset {
                session_id: "t".into(),
                step_number: 1,
                run_id: 1,
                thought_message_id: "msg-1".into(),
                reasoning_message_id: "msg-2".into(),
            },
            "agent:stream_reset",
        ),
        (
            AgentEvent::WebSearch {
                session_id: "t".into(),
                phase: "searching".into(),
                step_number: 1,
                run_id: 1,
                call_id: Some("ws_1".into()),
                action: Some("search".into()),
                result: None,
            },
            "agent:web_search",
        ),
        (
            AgentEvent::StreamStalled {
                session_id: "t".into(),
            },
            "agent:stream_stalled",
        ),
        (
            AgentEvent::Supplement {
                session_id: "t".into(),
                additional_context: "c".into(),
                step_number: 1,
                run_id: 1,
                inject_source: None,
            },
            "agent:supplement",
        ),
        (
            AgentEvent::Compaction {
                session_id: "t".into(),
                summary: "s".into(),
                tokens_before: 1,
                tokens_after: 2,
                episode_id: None,
            },
            "agent:compaction",
        ),
        (
            AgentEvent::Usage {
                session_id: "t".into(),
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cache_miss_tokens: 0,
                context_tokens: 1,
                cache_exclusive: false,
                cache_accounting: "unknown".into(),
                cost_usd: None,
                model: None,
                cumulative_prompt_tokens: 1,
                cumulative_completion_tokens: 2,
                cumulative_total_tokens: 3,
                cumulative_cached_tokens: 0,
                cumulative_cache_creation_tokens: 0,
                cumulative_cache_miss_tokens: 0,
                cache_diagnostics: None,
                cumulative_cost_usd: None,
                context_window: None,
                step_number: Some(1),
                duration_ms: Some(42),
                role: Some("default".into()),
                has_cost: false,
            },
            "agent:usage",
        ),
    ];
    for (event, expected) in cases {
        assert_eq!(
            TauriEmitter::channel(&event),
            expected,
            "channel mismatch for {:?}",
            event
        );
    }
}

#[test]
fn thought_payload_uses_the_explicit_wire_dto() {
    let event = AgentEvent::Thought {
        session_id: "t1".into(),
        thought: "hello".into(),
        step_number: 2,
        run_id: 7,
        message_id: "msg-1".into(),
    };
    assert_eq!(
        TauriEmitter::payload(&event, None),
        json!({
            "session_id": "t1",
            "thought": "hello",
            "step_number": 2,
            "run_id": 7,
            "message_id": "msg-1",
        })
    );
}

#[test]
fn payload_adds_silent_to_action() {
    let event = AgentEvent::Action {
        session_id: "t".into(),
        tool_name: "read_file".into(),
        input: json!({"silent": true, "path": "/tmp/x"}),
        step_number: 1,
        run_id: 1,
        tool_call_id: Some("call-1".into()),
        step_id: "step-1".into(),
        action_index: 0,
        suppress_streamed_thought: false,
    };
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(payload["silent"], json!(true));
    assert_eq!(payload["tool_name"], json!("read_file"));
    assert_eq!(payload["tool_call_id"], json!("call-1"));
    assert_eq!(payload["step_id"], json!("step-1"));
}

#[test]
fn payload_never_silences_ask() {
    let event = AgentEvent::Action {
        session_id: "t".into(),
        tool_name: "ask".into(),
        input: json!({"silent": true}),
        step_number: 1,
        run_id: 1,
        tool_call_id: None,
        step_id: "step-1".into(),
        action_index: 0,
        suppress_streamed_thought: false,
    };
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(payload["silent"], json!(false));
}

#[test]
fn payload_injects_seq_for_chunk_variants() {
    let thought = AgentEvent::ThoughtChunk {
        session_id: "t".into(),
        delta: "d".into(),
        step_number: 1,
        run_id: 1,
        message_id: "msg-1".into(),
    };
    let payload = TauriEmitter::payload(&thought, Some(42));
    assert_eq!(payload["seq"], json!(42));
    assert_eq!(payload["delta"], json!("d"));
    assert_eq!(payload["message_id"], json!("msg-1"));

    let reasoning = AgentEvent::ReasoningChunk {
        session_id: "t".into(),
        delta: "d".into(),
        step_number: 1,
        run_id: 1,
        message_id: "msg-2".into(),
    };
    let payload = TauriEmitter::payload(&reasoning, Some(43));
    assert_eq!(payload["seq"], json!(43));
    assert_eq!(payload["message_id"], json!("msg-2"));
}

#[test]
fn payload_projects_stream_reset_boundary() {
    let event = AgentEvent::StreamReset {
        session_id: "t".into(),
        step_number: 3,
        run_id: 7,
        thought_message_id: "msg-thought".into(),
        reasoning_message_id: "msg-reasoning".into(),
    };
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(
        payload,
        json!({
            "session_id": "t",
            "step_number": 3,
            "run_id": 7,
            "thought_message_id": "msg-thought",
            "reasoning_message_id": "msg-reasoning",
        })
    );
}

#[test]
fn payload_projects_session_created_without_leaking_internal_fields() {
    let event = AgentEvent::SessionCreated(test_session_info());
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(
        payload,
        json!({
            "session_id": "ses-1",
            "status": "running",
            "title": "My Title",
        })
    );
    assert!(payload.get("id").is_none(), "must not leak SessionInfo.id");
    assert!(
        payload.get("input").is_none(),
        "must not leak SessionInfo.input"
    );
    assert!(
        payload.get("summary").is_none(),
        "must not leak SessionInfo.summary"
    );
}

#[test]
fn payload_preserves_session_lifecycle_and_error_wire_shapes() {
    let completed = AgentEvent::SessionCompleted {
        session_id: "t".into(),
        title: "X".into(),
    };
    let payload = TauriEmitter::payload(&completed, None);
    assert_eq!(
        payload,
        json!({"session_id": "t", "status": "completed", "title": "X"})
    );

    let updated = AgentEvent::SessionUpdated {
        session_id: "t".into(),
        status: "paused".into(),
    };
    let payload = TauriEmitter::payload(&updated, None);
    assert_eq!(
        payload,
        json!({"session_id": "t", "status": "paused", "title": ""})
    );

    let errored = AgentEvent::SessionError {
        session_id: "t".into(),
        error: "sanitized failure".into(),
    };
    assert_eq!(
        TauriEmitter::payload(&errored, None),
        json!({"session_id": "t", "error": "sanitized failure"})
    );
}

#[test]
fn action_projection_maps_scheduled_cancellation_to_the_public_contract() {
    let (channel, action) = project_action_event(
        ActionKind::Scheduled,
        "action:updated",
        &json!({
            "id": "act-1",
            "tool_name": "notify",
            "tool_args": {"secret": "hidden"},
        }),
    )
    .expect("known action event");

    assert_eq!(channel, ACTION_UPDATED_EVENT);
    assert_eq!(
        serde_json::to_value(action.expect("valid action payload")).unwrap(),
        json!({"id": "act-1", "kind": "scheduled", "status": "cancelled"})
    );
}

#[test]
fn action_projection_drops_unknown_events_and_malformed_payloads() {
    assert!(project_action_event(ActionKind::Background, "action:unknown", &json!({})).is_none());

    let (_, result) = project_action_event(
        ActionKind::Background,
        "action:finished",
        &json!({"status": "completed"}),
    )
    .expect("known action event");
    assert!(result.is_err());
}

#[test]
fn test_parse_shortcut_ctrl_shift_space() {
    let s = parse_shortcut("Ctrl+Shift+Space");
    assert!(s.is_some());
}

#[test]
fn test_parse_shortcut_single_key() {
    let s = parse_shortcut("a");
    assert!(s.is_some());
}

#[test]
fn test_parse_shortcut_alt_tab() {
    let s = parse_shortcut("Alt+Tab");
    assert!(s.is_some());
}

#[test]
fn test_parse_shortcut_control_alias() {
    let s = parse_shortcut("Control+C");
    assert!(s.is_some());
}

#[test]
fn test_parse_shortcut_super_modifier() {
    let s = parse_shortcut("Super+Space");
    assert!(s.is_some());
    let s = parse_shortcut("Win+E");
    assert!(s.is_some());
}

#[test]
fn test_parse_shortcut_invalid_key() {
    let s = parse_shortcut("Ctrl+InvalidKey");
    assert!(s.is_none());
}

#[test]
fn test_parse_shortcut_empty() {
    let s = parse_shortcut("");
    assert!(s.is_none());
}

#[test]
fn test_parse_shortcut_numeric_keys() {
    let s = parse_shortcut("Ctrl+F1");
    assert!(s.is_some());
    let s = parse_shortcut("Shift+F12");
    assert!(s.is_some());
}

#[test]
fn test_parse_shortcut_letter_keys() {
    for ch in 'a'..='z' {
        let binding = format!("Ctrl+{}", ch);
        assert!(parse_shortcut(&binding).is_some(), "failed for {}", binding);
    }
}

#[test]
fn test_make_tray_icon_creates_valid_image() {
    let img = make_tray_icon(TrayStatus::Normal);
    assert_eq!(img.width(), 32);
    assert_eq!(img.height(), 32);
}

#[test]
fn test_make_tray_icon_all_statuses_have_correct_size() {
    for status in &[
        TrayStatus::Normal,
        TrayStatus::Recording,
        TrayStatus::Muted,
        TrayStatus::Busy,
    ] {
        let img = make_tray_icon(*status);
        assert_eq!(img.width(), 32, "failed for {:?}", status);
        assert_eq!(img.height(), 32, "failed for {:?}", status);
    }
}

#[test]
fn test_app_data_dir_contains_haven() {
    let dir = haven_common::config::ConfigLoader::data_dir();
    let name = dir.file_name().unwrap().to_string_lossy().to_string();
    assert!(
        name.eq_ignore_ascii_case("haven"),
        "expected 'haven' got '{}'",
        name
    );
}

#[test]
fn test_app_data_dir_on_windows_uses_appdata() {
    #[cfg(target_os = "windows")]
    {
        let dir = haven_common::config::ConfigLoader::data_dir();
        let s = dir.to_string_lossy();
        assert!(
            s.contains("AppData\\Roaming") || s.contains("APPDATA"),
            "expected AppData path, got: {}",
            s
        );
    }
}

#[test]
fn tauri_csp_is_explicit_and_keeps_only_required_capabilities() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid Tauri config");
    let security = &config["app"]["security"];
    let csp = security["csp"]
        .as_str()
        .expect("production CSP must be an explicit string");
    let dev_csp = security["devCsp"]
        .as_str()
        .expect("development CSP must be an explicit string");

    for policy in [csp, dev_csp] {
        assert!(policy.contains("default-src 'self'"));
        assert!(policy.contains("connect-src ipc: http://ipc.localhost"));
        assert!(policy.contains("object-src 'none'"));
        assert!(policy.contains("frame-src 'none'"));
        assert!(policy.contains("base-uri 'self'"));
        assert!(!policy.contains("'unsafe-eval'"));
        assert!(!policy.contains(" *"));
    }

    assert!(
        !csp.contains("script-src 'self' 'unsafe-inline'"),
        "release scripts stay hash/nonce-bound"
    );
    assert!(!csp.contains("localhost:4721"));
    assert!(dev_csp.contains("ws://localhost:4721"));
    assert!(dev_csp.contains("script-src 'self' 'unsafe-inline' http://localhost:4721"));
}
