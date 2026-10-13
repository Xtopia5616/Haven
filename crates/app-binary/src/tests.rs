//! Unit tests for the app-binary composition boundary.

use crate::bootstrap::to_tauri_shortcut;
use crate::desktop::TrayStatus;
use crate::event_bridge::{TauriEmitter, project_tool_run_event};
use crate::events::*;
use crate::handlers::{TRAY_ICON_SIZE, make_tray_icon};
use haven_agent::{AgentEvent, SessionInfo};
use haven_common::lifecycle::SessionStatus;
use serde_json::json;

/// Parse a binding through the unified `haven-input` hotkey parser and
/// convert to the Tauri shortcut type (the production startup path).
fn parse_shortcut(binding: &str) -> Option<tauri_plugin_global_shortcut::Shortcut> {
    haven_input::KeyCombo::parse(binding).and_then(|combo| to_tauri_shortcut(&combo))
}

fn test_session_info() -> SessionInfo {
    SessionInfo {
        session_id: "ses-7ae290dbf0b4468c9d2c401be649850b".into(),
        input: "my input".into(),
        summary: "my summary".into(),
        title: Some("My Title".into()),
        status: SessionStatus::Running,
        waiting_reason: None,
        steps: vec![],
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    }
}

#[test]
fn channel_maps_every_variant_to_expected_channel() {
    let cases: Vec<(AgentEvent, &str)> = vec![
        (
            AgentEvent::Thought {
                session_id: "ses-00000000000000000000000000000001".into(),
                thought: "x".into(),
                step_number: 1,
                run_id: 1,
                message_id: "step-e11ea518ef030561fe32765a0ab84b18".into(),
                event_sequence: None,
            },
            "agent:thought",
        ),
        (
            AgentEvent::ToolCall {
                session_id: "ses-00000000000000000000000000000001".into(),
                tool_name: "read_file".into(),
                input: json!({}),
                step_number: 1,
                run_id: 1,
                tool_call_id: None,
                step_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
                tool_index: 0,
                suppress_streamed_thought: false,
                event_sequence: None,
            },
            "agent:tool_call",
        ),
        (
            AgentEvent::Observation {
                session_id: "ses-00000000000000000000000000000001".into(),
                observation: "o".into(),
                tool_name: "read_file".into(),
                step_number: 1,
                run_id: 1,
                silent: false,
                tool_call_id: None,
                ask_options: vec![],
                step_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
                tool_index: 0,
                idempotency: haven_common::tools::OperationIdempotency::Idempotent,
                operation_scope: haven_common::tools::ToolOperationScope::Session,
                renderer: "generic".into(),
                result: haven_common::tools::ToolResultEnvelope::default(),
                event_sequence: None,
            },
            "agent:observation",
        ),
        (
            AgentEvent::SessionCreated(test_session_info()),
            "session:lifecycle",
        ),
        (
            AgentEvent::SessionCompleted {
                session_id: "ses-00000000000000000000000000000001".into(),
                title: "x".into(),
                reason: "用户主动结束会话".into(),
            },
            "session:lifecycle",
        ),
        (
            AgentEvent::SessionUpdated {
                session_id: "ses-00000000000000000000000000000001".into(),
                status: haven_common::lifecycle::SessionStatus::Paused,
                waiting_reason: None,
                reason: None,
            },
            "session:lifecycle",
        ),
        (
            AgentEvent::SessionError {
                session_id: "ses-00000000000000000000000000000001".into(),
                error: "e".into(),
            },
            "session:lifecycle",
        ),
        (
            AgentEvent::Notification {
                session_id: Some("ses-00000000000000000000000000000001".into()),
                title: "x".into(),
                body: "y".into(),
            },
            "notification:show",
        ),
        (
            AgentEvent::TitleUpdated {
                session_id: "ses-00000000000000000000000000000001".into(),
                title: "x".into(),
            },
            "session:lifecycle",
        ),
        (
            AgentEvent::ThoughtChunk {
                session_id: "ses-00000000000000000000000000000001".into(),
                delta: "d".into(),
                step_number: 1,
                run_id: 1,
                message_id: "step-e11ea518ef030561fe32765a0ab84b18".into(),
            },
            "agent:thought_chunk",
        ),
        (
            AgentEvent::ReasoningChunk {
                session_id: "ses-00000000000000000000000000000001".into(),
                delta: "d".into(),
                step_number: 1,
                run_id: 1,
                message_id: "msg-c5be24b3c5f5ee71083c02117bb756ed".into(),
            },
            "agent:reasoning_chunk",
        ),
        (
            AgentEvent::StreamReset {
                session_id: "ses-00000000000000000000000000000001".into(),
                step_number: 1,
                run_id: 1,
                thought_message_id: "step-e11ea518ef030561fe32765a0ab84b18".into(),
                reasoning_message_id: "msg-c5be24b3c5f5ee71083c02117bb756ed".into(),
            },
            "agent:stream_reset",
        ),
        (
            AgentEvent::MediaPlan {
                session_id: "ses-00000000000000000000000000000001".into(),
                step_number: 1,
                run_id: 1,
                request_kind: haven_common::config::RequestKind::Vision,
                strategy: haven_common::media::MediaInputStrategy::Auto,
                projections: vec![],
                notices: vec![haven_common::media::MediaPlanNotice {
                    asset_id: "asset-2c740c2a13b7cd0c9ba244dca6894e99".into(),
                    code: haven_common::media::MediaPlanNoticeCode::RawCapabilityUnknown,
                }],
                event_sequence: None,
            },
            "agent:media_plan",
        ),
        (
            AgentEvent::WebSearch {
                session_id: "ses-00000000000000000000000000000001".into(),
                phase: haven_llm::WebSearchPhase::Searching,
                step_number: 1,
                run_id: 1,
                call_id: Some("ws_1".into()),
                call_index: None,
                action: Some("search".into()),
                result: None,
            },
            "agent:web_search",
        ),
        (
            AgentEvent::StreamStalled {
                session_id: "ses-00000000000000000000000000000001".into(),
            },
            "agent:stream_stalled",
        ),
        (
            AgentEvent::Supplement {
                session_id: "ses-00000000000000000000000000000001".into(),
                additional_context: "c".into(),
                step_number: 1,
                run_id: 1,
                message_id: None,
                supplement_id: "msg-15ec69c7ac12649c1a367602af3327c3".into(),
                inject_source: None,
                event_sequence: None,
            },
            "agent:supplement",
        ),
        (
            AgentEvent::Compaction {
                session_id: "ses-00000000000000000000000000000001".into(),
                summary: "s".into(),
                tokens_before: 1,
                tokens_after: 2,
                episode_id: None,
                degraded: false,
                event_sequence: None,
            },
            "agent:compaction",
        ),
        (
            AgentEvent::Usage {
                session_id: "ses-00000000000000000000000000000001".into(),
                prompt_tokens: 1,
                completion_tokens: 2,
                total_tokens: 3,
                cached_tokens: 0,
                cache_creation_tokens: 0,
                cache_miss_tokens: 0,
                context_tokens: 1,
                cache_exclusive: false,
                cache_accounting: haven_common::usage::CacheAccounting::Unknown,
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
                request_kind: Some(haven_common::config::RequestKind::Chat),
                call_kind: haven_common::usage::LlmCallKind::Agent,
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
fn usage_event_keeps_call_kind_typed_and_serializes_the_canonical_value() {
    let event = AgentEvent::Usage {
        session_id: "ses-eb008455936524547e8585c022c9f7d8".into(),
        prompt_tokens: 1,
        completion_tokens: 2,
        total_tokens: 3,
        cached_tokens: 0,
        cache_creation_tokens: 0,
        cache_miss_tokens: 1,
        context_tokens: 1,
        cache_exclusive: false,
        cache_accounting: haven_common::usage::CacheAccounting::Unknown,
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
        duration_ms: None,
        request_kind: Some(haven_common::config::RequestKind::Chat),
        call_kind: haven_common::usage::LlmCallKind::Tool,
        has_cost: false,
    };

    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(payload["call_kind"], "tool");
    assert_eq!(payload["request_kind"], "chat");
    assert!(payload.get("role").is_none());
}

#[test]
fn media_plan_payload_contains_strategy_projection_and_reason() {
    let event = AgentEvent::MediaPlan {
        session_id: "ses-7ae290dbf0b4468c9d2c401be649850b".into(),
        step_number: 4,
        run_id: 8,
        request_kind: haven_common::config::RequestKind::AudioChat,
        strategy: haven_common::media::MediaInputStrategy::Auto,
        projections: vec![haven_common::media::MediaProjection {
            asset_id: "asset-2c740c2a13b7cd0c9ba244dca6894e99".into(),
            representation: haven_common::media::MediaRepresentationKind::Transcript,
            mode: haven_common::media::MediaProjectionMode::Derived,
            provenance: haven_common::media::MediaProvenance::Derived {
                operation: haven_common::media::MediaDerivation::Stt,
                provider: None,
                source_kind: Some(haven_common::media::MediaRepresentationKind::RawAudio),
            },
        }],
        event_sequence: None,
        notices: vec![haven_common::media::MediaPlanNotice {
            asset_id: "asset-2c740c2a13b7cd0c9ba244dca6894e99".into(),
            code: haven_common::media::MediaPlanNoticeCode::RawCapabilityUnknown,
        }],
    };

    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(payload["request_kind"], "audio_chat");
    assert!(payload.get("role").is_none());
    assert_eq!(payload["strategy"], "auto");
    assert_eq!(
        payload["projections"][0]["asset_id"],
        "asset-2c740c2a13b7cd0c9ba244dca6894e99"
    );
    assert_eq!(payload["projections"][0]["representation"], "transcript");
    assert_eq!(payload["projections"][0]["mode"], "derived");
    assert_eq!(payload["notices"][0]["code"], "raw_capability_unknown");
}

#[test]
fn thought_payload_uses_the_explicit_wire_dto() {
    let event = AgentEvent::Thought {
        session_id: "ses-00000000000000000000000000000002".into(),
        thought: "hello".into(),
        step_number: 2,
        run_id: 7,
        message_id: "step-e11ea518ef030561fe32765a0ab84b18".into(),
        event_sequence: Some(11),
    };
    assert_eq!(
        TauriEmitter::payload(&event, None),
        json!({
            "session_id": "ses-00000000000000000000000000000002",
            "thought": "hello",
            "step_number": 2,
            "run_id": 7,
            "message_id": "step-e11ea518ef030561fe32765a0ab84b18",
            "event_sequence": 11,
        })
    );
}

#[test]
fn payload_adds_silent_to_tool_call() {
    let event = AgentEvent::ToolCall {
        session_id: "ses-00000000000000000000000000000001".into(),
        tool_name: "read_file".into(),
        input: json!({"silent": true, "path": "/tmp/x"}),
        step_number: 1,
        run_id: 1,
        tool_call_id: Some("call-1".into()),
        step_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
        tool_index: 0,
        suppress_streamed_thought: false,
        event_sequence: Some(17),
    };
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(payload["silent"], json!(true));
    assert_eq!(payload["tool_name"], json!("read_file"));
    assert_eq!(payload["tool_call_id"], json!("call-1"));
    assert_eq!(
        payload["step_id"],
        json!("step-f9430084054c5002eb0d7715a0ec20cd")
    );
    assert_eq!(payload["event_sequence"], json!(17));
}

#[test]
fn payload_never_silences_ask() {
    let event = AgentEvent::ToolCall {
        session_id: "ses-00000000000000000000000000000001".into(),
        tool_name: "ask".into(),
        input: json!({"silent": true}),
        step_number: 1,
        run_id: 1,
        tool_call_id: None,
        step_id: "step-f9430084054c5002eb0d7715a0ec20cd".into(),
        tool_index: 0,
        suppress_streamed_thought: false,
        event_sequence: None,
    };
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(payload["silent"], json!(false));
}

#[test]
fn payload_injects_seq_for_chunk_variants() {
    let thought = AgentEvent::ThoughtChunk {
        session_id: "ses-00000000000000000000000000000001".into(),
        delta: "d".into(),
        step_number: 1,
        run_id: 1,
        message_id: "step-e11ea518ef030561fe32765a0ab84b18".into(),
    };
    let payload = TauriEmitter::payload(&thought, Some(42));
    assert_eq!(payload["seq"], json!(42));
    assert_eq!(payload["delta"], json!("d"));
    assert_eq!(
        payload["message_id"],
        json!("step-e11ea518ef030561fe32765a0ab84b18")
    );

    let reasoning = AgentEvent::ReasoningChunk {
        session_id: "ses-00000000000000000000000000000001".into(),
        delta: "d".into(),
        step_number: 1,
        run_id: 1,
        message_id: "msg-c5be24b3c5f5ee71083c02117bb756ed".into(),
    };
    let payload = TauriEmitter::payload(&reasoning, Some(43));
    assert_eq!(payload["seq"], json!(43));
    assert_eq!(
        payload["message_id"],
        json!("msg-c5be24b3c5f5ee71083c02117bb756ed")
    );
}

#[test]
fn payload_projects_stream_reset_boundary() {
    let event = AgentEvent::StreamReset {
        session_id: "ses-00000000000000000000000000000001".into(),
        step_number: 3,
        run_id: 7,
        thought_message_id: "step-d1cddeb2513e8070a41670497d336ee0".into(),
        reasoning_message_id: "msg-ed03e30c61a91f6ed2ae4ebbba109918".into(),
    };
    let payload = TauriEmitter::payload(&event, None);
    assert_eq!(
        payload,
        json!({
            "session_id": "ses-00000000000000000000000000000001",
            "step_number": 3,
            "run_id": 7,
            "thought_message_id": "step-d1cddeb2513e8070a41670497d336ee0",
            "reasoning_message_id": "msg-ed03e30c61a91f6ed2ae4ebbba109918",
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
            "type": "created",
            "session_id": "ses-7ae290dbf0b4468c9d2c401be649850b",
            "status": "running",
            "title": "My Title",
        })
    );
    assert!(
        payload.get("id").is_none(),
        "must not leak SessionInfo.session_id"
    );
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
        session_id: "ses-00000000000000000000000000000001".into(),
        title: "X".into(),
        reason: "用户主动结束会话".into(),
    };
    let payload = TauriEmitter::payload(&completed, None);
    assert_eq!(
        payload,
        json!({
            "type": "completed",
            "session_id": "ses-00000000000000000000000000000001",
            "title": "X",
            "reason": "用户主动结束会话"
        })
    );

    let updated = AgentEvent::SessionUpdated {
        session_id: "ses-00000000000000000000000000000001".into(),
        status: haven_common::lifecycle::SessionStatus::Paused,
        waiting_reason: None,
        reason: Some("用户主动打断输出".into()),
    };
    let payload = TauriEmitter::payload(&updated, None);
    assert_eq!(
        payload,
        json!({
            "type": "updated",
            "session_id": "ses-00000000000000000000000000000001",
            "status": "paused",
            "title": "ses-00000000000000000000000000000001",
            "reason": "用户主动打断输出"
        })
    );

    let errored = AgentEvent::SessionError {
        session_id: "ses-00000000000000000000000000000001".into(),
        error: "sanitized failure".into(),
    };
    assert_eq!(
        TauriEmitter::payload(&errored, None),
        json!({
            "type": "error",
            "session_id": "ses-00000000000000000000000000000001",
            "title": "ses-00000000000000000000000000000001",
            "error": "sanitized failure"
        })
    );
}

#[test]
fn tool_run_projection_maps_scheduled_progress_to_the_public_contract() {
    let mut payload = haven_tools::ToolRunLifecyclePayload::new_for_test(
        haven_tools::ToolRunKind::Scheduled,
        "toolrun-11111111111111111111111111111111",
        haven_tools::ToolRunLifecycleState::Running {
            started_at: "2026-10-06T12:00:00Z".into(),
        },
    );
    payload.title = Some("Reminder".into());
    let projection = project_tool_run_event(haven_tools::ToolRunLifecycleEvent::Updated(
        haven_tools::ToolRunLifecycleUpdate::StateChanged(Box::new(payload)),
    ));
    assert_eq!(projection.channel, TOOL_RUN_UPDATED_EVENT);
    assert_eq!(
        projection.payload.status,
        Some(haven_common::lifecycle::ToolRunStatus::Running)
    );
    assert_eq!(
        projection.payload.started_at.as_deref(),
        Some("2026-10-06T12:00:00Z")
    );
    assert_eq!(projection.payload.title.as_deref(), Some("Reminder"));
}

#[test]
fn tool_run_projection_maps_session_attachment_as_a_metadata_update() {
    let event = haven_tools::ToolRunLifecycleEvent::Updated(
        haven_tools::ToolRunLifecycleUpdate::SessionAttached(
            haven_tools::ToolRunSessionAttachedPayload::new_for_test(
                "toolrun-22222222222222222222222222222222",
                "ses-22222222222222222222222222222222",
                Some("step-22222222222222222222222222222222".into()),
            ),
        ),
    );
    let projection = project_tool_run_event(event);

    assert_eq!(projection.channel, TOOL_RUN_UPDATED_EVENT);
    assert_eq!(
        serde_json::to_value(projection.payload).unwrap(),
        serde_json::json!({
            "tool_run_id": "toolrun-22222222222222222222222222222222",
            "kind": "background",
            "session_id": "ses-22222222222222222222222222222222",
            "source_step_id": "step-22222222222222222222222222222222",
        })
    );
}

#[test]
fn tool_run_output_projection_uses_only_typed_preview_fields() {
    let event = haven_tools::ToolRunLifecycleEvent::Output(
        haven_tools::ToolRunOutputPayload::new_for_test(
            "toolrun-33333333333333333333333333333333",
            Some("step-33333333333333333333333333333333".into()),
            "preview",
        ),
    );
    let projection = project_tool_run_event(event);
    assert_eq!(projection.channel, TOOL_RUN_OUTPUT_EVENT);
    assert_eq!(
        projection.payload.tool_run_id,
        "toolrun-33333333333333333333333333333333"
    );
    assert_eq!(
        projection.payload.source_step_id.as_deref(),
        Some("step-33333333333333333333333333333333")
    );
    assert_eq!(projection.payload.output.as_deref(), Some("preview"));
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
    assert_eq!(img.width(), TRAY_ICON_SIZE);
    assert_eq!(img.height(), TRAY_ICON_SIZE);
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
        assert_eq!(img.width(), TRAY_ICON_SIZE, "failed for {:?}", status);
        assert_eq!(img.height(), TRAY_ICON_SIZE, "failed for {:?}", status);
    }
}

#[test]
fn test_make_tray_icon_uses_opaque_background_and_status_colors() {
    let statuses = [
        TrayStatus::Normal,
        TrayStatus::Recording,
        TrayStatus::Muted,
        TrayStatus::Busy,
    ];
    let images = statuses.map(make_tray_icon);

    assert_eq!(
        images[0].rgba().len(),
        (TRAY_ICON_SIZE * TRAY_ICON_SIZE * 4) as usize
    );
    assert_eq!(
        images[0].rgba()[3],
        255,
        "desktop tray icon should use an opaque brand tile"
    );
    assert!(
        images[0]
            .rgba()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] > 0)
    );
    let expected_border_red = [44, 240, 169, 255];
    let border_offset = ((TRAY_ICON_SIZE / 16) * TRAY_ICON_SIZE + TRAY_ICON_SIZE / 2) * 4;
    for (index, image) in images.iter().enumerate() {
        let border_pixel = &image.rgba()[border_offset as usize..border_offset as usize + 4];
        assert_eq!(
            border_pixel[0], expected_border_red[index],
            "status {index} should keep its status-coloured perimeter"
        );
        for previous in &images[..index] {
            assert_ne!(
                image.rgba(),
                previous.rgba(),
                "status icons must remain distinct"
            );
        }
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
        let appdata = std::env::var_os("APPDATA").expect("APPDATA is set on Windows");
        let expected = std::path::PathBuf::from(appdata).join("haven");
        let dir = haven_common::config::ConfigLoader::data_dir();
        assert_eq!(dir, expected);
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
