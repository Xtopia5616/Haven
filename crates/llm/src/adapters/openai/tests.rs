#[path = "tests/golden.rs"]
mod golden;

use super::*;
use crate::ToolFunction;

#[tokio::test]
async fn error_classifies_correctly() {
    let ep = ModelEndpoint {
        base_url: "http://127.0.0.1:1".to_string(),
        timeout_secs: 1,
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let e = client
        .chat(vec![CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hi")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }])
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            LlmError::Timeout(_)
                | LlmError::Network(_)
                | LlmError::ServerError(_)
                | LlmError::RequestFailed(_)
                | LlmError::Unknown(_)
        ),
        "expected a recognized error variant, got: {e:?}"
    );
}

#[tokio::test]
async fn health_check_rejects_auth() {
    let ep = ModelEndpoint {
        api_key: "bad_key".to_string(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let _ = client.health_check().await;
}

#[test]
fn extract_tool_calls_parses_correctly() {
    let choice = OpenAiChoice {
        message: Some(OpenAiMessageOut {
            role: None,
            content: None,
            tool_calls: Some(vec![OpenAiToolCallOut {
                id: Some("tc_1".into()),
                index: None,
                function: OpenAiFunctionOut {
                    name: Some("file".into()),
                    arguments: Some("{\"path\":\".\"}".into()),
                },
            }]),
            reasoning_content: None,
            web_search_call: Vec::new(),
        }),
        delta: None,
        finish_reason: None,
    };
    let tc = OpenAiAdapter::extract_tool_calls(&choice);
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].name, "file");
}

#[test]
fn parse_openai_response_collects_web_search_calls() {
    // DeepSeek's built-in web search returns `web_search_call` items in
    // the assistant message. They must be collected so the next request
    // can echo them back verbatim (stateless chat API).
    let ws = serde_json::json!({
        "type": "web_search_call",
        "id": "ws_1",
        "status": "completed"
    });
    let choice = OpenAiChoice {
        message: Some(OpenAiMessageOut {
            role: Some("assistant".into()),
            content: Some("searched".into()),
            tool_calls: None,
            reasoning_content: None,
            web_search_call: vec![ws.clone()],
        }),
        delta: None,
        finish_reason: Some("stop".into()),
    };
    let json = OpenAiResponse {
        choices: vec![choice],
        usage: None,
        model: Some("deepseek".into()),
        citations: Vec::new(),
    };
    let ep = ModelEndpoint::default();
    let adapter = OpenAiAdapter::new(ep);
    let resp = adapter
        .parse_openai_response(json, None, CacheDiagnostics::default())
        .unwrap();
    assert_eq!(resp.web_search_calls.len(), 1);
    assert_eq!(resp.web_search_calls[0]["type"], "web_search_call");
    assert_eq!(
        resp.web_search_calls[0]["action"],
        serde_json::json!({"type": "search", "queries": []})
    );
}

#[test]
fn convert_messages_echoes_web_search_calls_verbatim() {
    // A complete item (with `action`) is echoed back untouched.
    let ws = serde_json::json!({
        "type": "web_search_call",
        "id": "ws_9",
        "status": "completed",
        "action": {"type": "search", "queries": ["capital of France"]},
        "query": "foo"
    });
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("searched")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: vec![ws.clone()],
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(msgs, Vec::new(), false);
    let out = body.messages[0].web_search_call.first().cloned().unwrap();
    assert_eq!(out, ws);
}

#[test]
fn convert_messages_derives_reasoning_from_thinking_blocks() {
    // Anthropic messages carry the thinking text only as raw
    // `thinking_blocks` (the agent drops the redundant `reasoning` copy);
    // the reasoning echo must still work when such a message is sent to an
    // OpenAI-compatible reasoning-echo provider.
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("checked")],
        tool_call_id: None,
        tool_calls: Some(vec![CanonicalToolCall {
            id: "call_1".into(),
            name: "file".into(),
            arguments: serde_json::json!({"operation": "read"}),
        }]),
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: vec![
            serde_json::json!({"type": "thinking", "thinking": "let me check", "signature": "s1"}),
            serde_json::json!({"type": "redacted_thinking", "data": "redacted"}),
        ],
        source: None,
        id: None,
    }];
    let ep = ModelEndpoint {
        provider: "deepseek".into(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(msgs, Vec::new(), false);
    assert_eq!(
        body.messages[0].reasoning_content.as_deref(),
        Some("let me check")
    );
}

#[test]
fn convert_messages_truncates_oversized_reasoning_to_tail() {
    // Unbounded reasoning echo (10k+ chars per turn) balloons the request
    // body and stalls/truncates the provider's stream mid-inference (the
    // same failure mode the Responses adapter documents). The echo must
    // keep the TAIL of the reasoning (the conclusions), bounded by the
    // cap — and the cap must come from the endpoint's
    // `reasoning_echo_max_chars` override.
    let long = format!(
        "{}END-MARKER",
        "thinking step. ".repeat(OpenAiAdapter::MAX_REASONING_ECHO_CHARS + 500)
    );
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("ok")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: Some(long.clone()),
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(msgs, Vec::new(), false);
    let echoed = body.messages[0].reasoning_content.as_deref().unwrap();
    assert_eq!(
        echoed.chars().count(),
        OpenAiAdapter::MAX_REASONING_ECHO_CHARS
    );
    assert!(
        echoed.ends_with("END-MARKER"),
        "the tail (conclusions) must be preserved, got: ...{}",
        &echoed[echoed.len().saturating_sub(40)..]
    );
    assert!(
        !echoed.starts_with("thinking step. "),
        "the head must be trimmed"
    );
    // A custom per-endpoint cap wins over the default.
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("ok")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: Some(long),
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let ep = ModelEndpoint {
        reasoning_echo_max_chars: Some(64),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(msgs, Vec::new(), false);
    let echoed = body.messages[0].reasoning_content.as_deref().unwrap();
    assert_eq!(echoed.chars().count(), 64);
    assert!(echoed.ends_with("END-MARKER"));
}

#[test]
fn convert_messages_supplies_missing_web_search_call_action() {
    // The in-progress skeleton captured from the stream lacks `action`;
    // echoing it back as-is 400s on DeepSeek, so the field is filled.
    let ws = serde_json::json!({
        "type": "web_search_call",
        "id": "ws_9",
        "status": "in_progress"
    });
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("searched")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: vec![ws.clone()],
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(msgs, Vec::new(), false);
    let out = body.messages[0].web_search_call.first().cloned().unwrap();
    assert_eq!(out["type"], "web_search_call");
    assert_eq!(out["id"], "ws_9");
    assert_eq!(out["status"], "in_progress");
    assert_eq!(
        out["action"],
        serde_json::json!({"type": "search", "queries": []})
    );
}

#[test]
fn build_headers_custom_auth_header_name() {
    let ep = ModelEndpoint {
        api_key: "sk-test".into(),
        auth_header_name: "X-API-Key".into(),
        auth_header_prefix: String::new(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert!(headers.contains_key("x-api-key"));
    // Empty prefix must send the raw key — never `" sk-test"`.
    assert_eq!(
        headers.get("x-api-key").unwrap().to_str().unwrap(),
        "sk-test"
    );
}

#[test]
fn is_whisper_model_covers_transcribe_ids() {
    assert!(is_whisper_model("whisper-1"));
    assert!(is_whisper_model("gpt-4o-transcribe"));
    assert!(is_whisper_model("gpt-4o-mini-transcribe"));
    assert!(is_whisper_model("FunAudioLLM/SenseVoiceSmall"));
    assert!(is_whisper_model("TeleAI/TeleSpeechASR"));
    assert!(!is_whisper_model("gpt-4o-audio-preview"));
    assert!(!is_whisper_model("gpt-4o"));
}

#[test]
fn build_headers_default_auth_header() {
    let ep = ModelEndpoint {
        api_key: "my-key".into(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    let val = headers.get("authorization").unwrap().to_str().unwrap();
    assert_eq!(val, "Bearer my-key");
}

#[test]
fn build_headers_custom_prefix() {
    let ep = ModelEndpoint {
        api_key: "token123".into(),
        auth_header_prefix: "Token".into(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    let val = headers.get("authorization").unwrap().to_str().unwrap();
    assert_eq!(val, "Token token123");
}

#[test]
fn build_headers_empty_api_key_skips_auth() {
    let ep = ModelEndpoint {
        api_key: String::new(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert!(headers.contains_key("content-type"));
    assert!(!headers.contains_key("authorization"));
}

#[test]
fn build_headers_content_type_is_json() {
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "application/json"
    );
}

#[test]
fn build_request_body_model_name() {
    let ep = ModelEndpoint {
        model_name: "gpt-4-turbo".into(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(vec![], vec![], false);
    assert_eq!(body.model, "gpt-4-turbo");
}

#[test]
fn build_request_body_stream_flag() {
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body_stream = client.build_request_body(vec![], vec![], true);
    assert!(body_stream.stream);
    let body_no_stream = client.build_request_body(vec![], vec![], false);
    assert!(!body_no_stream.stream);
}

#[test]
fn build_request_body_stream_options_requests_usage() {
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body_stream = client.build_request_body(vec![], vec![], true);
    let opts = body_stream
        .stream_options
        .expect("stream request must ask for usage");
    assert!(opts.include_usage);
    let body_no_stream = client.build_request_body(vec![], vec![], false);
    assert!(body_no_stream.stream_options.is_none());
}

#[test]
fn prompt_cache_key_is_stable_across_memory_refreshes() {
    let client = OpenAiAdapter::new(ModelEndpoint {
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let stable = "You are Haven.\nCurrent session: investigate cache\n";
    let first_system = CanonicalMessage::system(vec![ContentPart::text(format!(
        "{stable}{MEMORY_FENCE_START}first recalled fact"
    ))]);
    let refreshed_system = CanonicalMessage::system(vec![ContentPart::text(format!(
        "{stable}{MEMORY_FENCE_START}refreshed recalled fact"
    ))]);
    let user = CanonicalMessage::user_text("continue");
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "read".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({"type":"object"}),
        },
    }];

    let first = client
        .build_request_body(vec![first_system, user.clone()], tools.clone(), false)
        .prompt_cache_key;
    let refreshed = client
        .build_request_body(vec![refreshed_system, user], tools, false)
        .prompt_cache_key;

    assert!(first.is_some());
    assert_eq!(first, refreshed);
}

#[test]
fn prompt_cache_key_canonicalizes_nested_tool_schema_objects() {
    let client = OpenAiAdapter::new(ModelEndpoint {
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
    let user = CanonicalMessage::user_text("session anchor");
    let tool = |parameters| ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "read".into(),
            description: "read a file".into(),
            parameters,
        },
    };
    let first_schema = serde_json::json!({
        "type": "object",
        "properties": {"path": {"type": "string"}, "limit": {"type": "integer"}},
        "required": ["path", "limit"]
    });
    let reordered_schema = serde_json::json!({
        "required": ["path", "limit"],
        "properties": {"limit": {"type": "integer"}, "path": {"type": "string"}},
        "type": "object"
    });

    let first = client
        .build_request_body(
            vec![system.clone(), user.clone()],
            vec![tool(first_schema)],
            false,
        )
        .prompt_cache_key;
    let reordered = client
        .build_request_body(vec![system, user], vec![tool(reordered_schema)], false)
        .prompt_cache_key;

    assert_eq!(first, reordered);
}

#[test]
fn build_request_splits_memory_after_stable_system_prefix() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let body = client.build_request_body(
        vec![
            CanonicalMessage::system(vec![ContentPart::text(format!(
                "stable instructions\n{MEMORY_FENCE_START}volatile fact"
            ))]),
            CanonicalMessage::user_text("session anchor"),
        ],
        Vec::new(),
        false,
    );

    assert!(body.cache_diagnostics.system_split);
    assert_eq!(body.messages.len(), 3);
    assert_eq!(body.messages[0].role, "system");
    assert_eq!(
        body.messages[0].content,
        Some(Value::String("stable instructions\n".into()))
    );
    assert_eq!(body.messages[1].role, "user");
    assert_eq!(
        body.messages[1].content,
        Some(Value::String("session anchor".into()))
    );
    assert_eq!(body.messages[2].role, "user");
    assert_eq!(
        body.messages[2].content,
        Some(Value::String(format!("{MEMORY_FENCE_START}volatile fact")))
    );
}

#[test]
fn chat_memory_refresh_preserves_the_transcript_prefix() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let stable = "stable instructions\n";
    let session = format!("{SESSION_CONTEXT_FENCE_START}Current session: inspect cache\n");
    let history = vec![
        CanonicalMessage::user_text("session anchor"),
        CanonicalMessage::assistant(
            vec![ContentPart::text("checking")],
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
        CanonicalMessage::tool(vec![ContentPart::text("result")], Some("call_1".into())),
    ];
    let first = client.build_request_body(
        vec![CanonicalMessage::system(vec![ContentPart::text(format!(
            "{stable}{session}{MEMORY_FENCE_START}old fact"
        ))])]
        .into_iter()
        .chain(history.clone())
        .collect(),
        Vec::new(),
        false,
    );
    let refreshed = client.build_request_body(
        vec![CanonicalMessage::system(vec![ContentPart::text(format!(
            "{stable}{session}{MEMORY_FENCE_START}new fact"
        ))])]
        .into_iter()
        .chain(history)
        .collect(),
        Vec::new(),
        false,
    );

    assert_eq!(first.messages.len(), refreshed.messages.len());
    assert_eq!(
        serde_json::to_value(&first.messages[..first.messages.len() - 1]).unwrap(),
        serde_json::to_value(&refreshed.messages[..refreshed.messages.len() - 1]).unwrap()
    );
    assert_ne!(
        first.messages.last().unwrap().content,
        refreshed.messages.last().unwrap().content
    );
}

#[test]
fn prompt_cache_key_is_shared_across_dynamic_sessions() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let stable = "stable system";
    let first = client
        .build_request_body(
            vec![
                CanonicalMessage::system(vec![ContentPart::text(format!(
                    "{stable}{SESSION_CONTEXT_FENCE_START}Current session: first"
                ))]),
                CanonicalMessage::user_text("first session"),
            ],
            Vec::new(),
            false,
        )
        .prompt_cache_key
        .unwrap();
    let second = client
        .build_request_body(
            vec![
                CanonicalMessage::system(vec![ContentPart::text(format!(
                    "{stable}{SESSION_CONTEXT_FENCE_START}Current session: second"
                ))]),
                CanonicalMessage::user_text("second session"),
            ],
            Vec::new(),
            false,
        )
        .prompt_cache_key
        .unwrap();

    assert_eq!(first, second);
}

#[test]
fn prompt_cache_key_changes_when_tools_change_or_is_unsupported() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
    let user = CanonicalMessage::user_text("session anchor");
    let one_tool = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "read".into(),
            description: "read a file".into(),
            parameters: serde_json::json!({"type":"object"}),
        },
    }];
    let two_tools = vec![
        one_tool[0].clone(),
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "write".into(),
                description: "write a file".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
        },
    ];

    let first = client
        .build_request_body(vec![system.clone(), user.clone()], one_tool, false)
        .prompt_cache_key
        .unwrap();
    let changed = client
        .build_request_body(vec![system.clone(), user.clone()], two_tools, false)
        .prompt_cache_key
        .unwrap();
    assert_ne!(first, changed);

    client
        .prompt_cache_key_state
        .store(PROMPT_CACHE_KEY_UNSUPPORTED, Ordering::Relaxed);
    assert!(
        client
            .build_request_body(vec![system, user], Vec::new(), false)
            .prompt_cache_key
            .is_none()
    );
}

#[test]
fn prompt_cache_key_changes_with_openai_web_search_mode() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let messages = vec![CanonicalMessage::system(vec![ContentPart::text(
        "stable system",
    )])];
    let off = client
        .build_request_body_with_mode(messages.clone(), Vec::new(), false, WebSearchMode::Off)
        .prompt_cache_key;
    let auto = client
        .build_request_body_with_mode(messages, Vec::new(), false, WebSearchMode::Auto)
        .prompt_cache_key;

    assert_ne!(off, auto);
}

#[test]
fn prompt_cache_key_restarts_after_compaction_and_stays_stable_afterward() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
    let anchor = CanonicalMessage::user_text("session anchor");
    let before = client
        .build_request_body(vec![system.clone(), anchor.clone()], Vec::new(), false)
        .prompt_cache_key;

    let mut summary = CanonicalMessage::assistant(
        vec![ContentPart::text(format!(
            "{} summarized history",
            haven_common::prompts::COMPACTED_SUMMARY_PREFIX
        ))],
        None,
        None,
        Vec::new(),
        Vec::new(),
    );
    summary.id = Some("msg-compaction-1".into());
    let after = client
        .build_request_body(
            vec![system.clone(), anchor.clone(), summary.clone()],
            Vec::new(),
            false,
        )
        .prompt_cache_key;
    let after_next_turn = client
        .build_request_body(vec![system, anchor, summary], Vec::new(), false)
        .prompt_cache_key;

    assert_ne!(before, after);
    assert_eq!(after, after_next_turn);
}

#[test]
fn prompt_cache_key_tracks_raw_media_surface_without_hashing_media_bytes() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
    let text_only = client
        .build_request_body(
            vec![system.clone(), CanonicalMessage::user_text("attachment")],
            Vec::new(),
            false,
        )
        .prompt_cache_key;
    let image = CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![ContentPart::Image {
            content_type: "image_url".into(),
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }],
        tool_calls: None,
        tool_call_id: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let raw_image = client
        .build_request_body(vec![system.clone(), image.clone()], Vec::new(), false)
        .prompt_cache_key;
    let mut different_bytes = image;
    if let ContentPart::Image { data, .. } = &mut different_bytes.content[0] {
        *data = "d29ybGQ=".into();
    }
    let same_surface = client
        .build_request_body(vec![system, different_bytes], Vec::new(), false)
        .prompt_cache_key;

    assert_ne!(text_only, raw_image);
    assert_eq!(raw_image, same_surface);
}

#[test]
fn prompt_cache_key_rejection_detection_is_specific() {
    assert!(OpenAiAdapter::prompt_cache_key_rejected(
        &LlmError::RequestFailed("400: Unknown parameter: prompt_cache_key".into())
    ));
    assert!(OpenAiAdapter::prompt_cache_key_rejected(
        &LlmError::RequestFailed("invalid parameter 'prompt_cache_key'".into())
    ));
    assert!(OpenAiAdapter::prompt_cache_key_rejected(
        &LlmError::RequestFailed(
            "Additional properties are not allowed ('prompt_cache_key' was unexpected)".into()
        )
    ));
    assert!(!OpenAiAdapter::prompt_cache_key_rejected(
        &LlmError::RequestFailed("400: maximum context length exceeded".into())
    ));
}

#[tokio::test]
async fn rejected_prompt_cache_key_retries_without_key_and_disables_it() {
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen_keys = Arc::new(Mutex::new(Vec::new()));
    let seen_keys_server = Arc::clone(&seen_keys);
    let server = tokio::spawn(async move {
        for request_number in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let n = socket.read(&mut chunk).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if buf.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let body = String::from_utf8_lossy(&buf[header_end + 4..]);
            seen_keys_server
                .lock()
                .unwrap()
                .push(body.contains("prompt_cache_key"));
            let (status, response) = if request_number == 0 {
                (
                    "400 Bad Request",
                    r#"{"error":{"message":"Unknown parameter: prompt_cache_key"}}"#,
                )
            } else {
                (
                    "200 OK",
                    r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1,"total_tokens":11},"model":"gpt-test"}"#,
                )
            };
            let wire = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            socket.write_all(wire.as_bytes()).await.unwrap();
        }
    });

    let client = OpenAiAdapter::new(ModelEndpoint {
        base_url: format!("http://{addr}"),
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let messages = vec![
        CanonicalMessage::system(vec![ContentPart::text("stable system")]),
        CanonicalMessage::user_text("session anchor"),
    ];
    let response = client.chat(messages.clone()).await.unwrap();
    assert_eq!(response.text, "ok");
    assert!(
        client
            .build_request_body(messages, Vec::new(), false)
            .prompt_cache_key
            .is_none()
    );
    server.await.unwrap();
    assert_eq!(*seen_keys.lock().unwrap(), vec![true, false]);
}

#[test]
fn rejected_prompt_cache_key_is_reprobed_after_cooldown() {
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    client
        .prompt_cache_key_state
        .store(PROMPT_CACHE_KEY_UNSUPPORTED, Ordering::Relaxed);
    client.prompt_cache_key_retry_at.store(1, Ordering::Relaxed);

    let key = client.prompt_cache_key(
        &[CanonicalMessage::system(vec![ContentPart::text(
            "stable system",
        )])],
        &[],
        WebSearchMode::Off,
    );
    assert!(key.is_some());
}

#[test]
fn build_request_body_with_tools() {
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "search".into(),
            description: "search the web".into(),
            parameters: serde_json::json!({}),
        },
    }];
    let body = client.build_request_body(vec![], tools, false);
    assert!(body.tools.is_some());
    assert_eq!(body.tools.as_ref().unwrap().len(), 1);
    assert_eq!(body.tools.unwrap()[0].tool_type, "function");
}

#[test]
fn xai_search_parameters_follow_web_search_mode() {
    let ep = ModelEndpoint {
        api_style: Some("xai".into()),
        provider: "xai".into(),
        base_url: "https://api.x.ai/v1".into(),
        model_name: "grok-3".into(),
        web_search: Some("auto".into()),
        ..Default::default()
    };
    let client = OpenAiAdapter::new_with_style(ep, "xai");
    assert_eq!(client.style(), "xai");
    let auto = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Auto);
    assert_eq!(
        auto.search_parameters,
        Some(serde_json::json!({"mode": "auto", "return_citations": true}))
    );
    let always = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Always);
    assert_eq!(
        always.search_parameters,
        Some(serde_json::json!({"mode": "on", "return_citations": true}))
    );
    let off = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Off);
    assert!(off.search_parameters.is_none());
    // Non-xAI style never injects search_parameters.
    let chat = OpenAiAdapter::new(ModelEndpoint::default());
    let body = chat.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Always);
    assert!(body.search_parameters.is_none());
}

#[test]
fn build_request_body_deepseek_thinking_enabled_maps_medium() {
    let ep = ModelEndpoint {
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com".into(),
        model_name: "deepseek-v4-pro".into(),
        temperature: 0.7,
        reasoning_effort: Some("medium".into()),
        ..Default::default()
    };
    let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(body.thinking, Some(serde_json::json!({"type": "enabled"})));
    assert_eq!(body.reasoning_effort.as_deref(), Some("high"));
    assert!(body.temperature.is_none());
}

#[test]
fn build_request_body_deepseek_thinking_omits_unsupported_sampling_fields() {
    let ep = ModelEndpoint {
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com".into(),
        model_name: "deepseek-v4-pro".into(),
        top_p: Some(0.8),
        frequency_penalty: Some(0.2),
        presence_penalty: Some(0.1),
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert!(body.top_p.is_none());
    assert!(body.frequency_penalty.is_none());
    assert!(body.presence_penalty.is_none());
}

#[test]
fn build_request_body_deepseek_thinking_disabled() {
    let ep = ModelEndpoint {
        provider: "openai".into(),
        base_url: "https://gateway.example/v1".into(),
        model_name: "deepseek-chat".into(),
        reasoning_effort: Some("disabled".into()),
        ..Default::default()
    };
    let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(body.thinking, Some(serde_json::json!({"type": "disabled"})));
    assert!(body.reasoning_effort.is_none());
}

#[test]
fn build_request_body_kimi_thinking_keep_all() {
    let ep = ModelEndpoint {
        provider: "moonshot".into(),
        base_url: "https://api.moonshot.ai/v1".into(),
        model_name: "kimi-k2.6".into(),
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(
        body.thinking,
        Some(serde_json::json!({"type": "enabled", "keep": "all"}))
    );
    assert!(body.reasoning_effort.is_none());
}

#[test]
fn build_request_body_openai_passes_reasoning_effort_without_thinking() {
    let ep = ModelEndpoint {
        provider: "openai".into(),
        base_url: "https://api.openai.com/v1".into(),
        model_name: "o3".into(),
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert!(body.thinking.is_none());
    assert_eq!(body.reasoning_effort.as_deref(), Some("high"));
}

#[test]
fn build_request_body_omits_temperature_for_reasoning_effort_models() {
    // o1/o3-family models reject a non-default temperature; when a
    // reasoning_effort is pinned the temperature field must be omitted
    // (provider default 1.0 applies).
    let ep = ModelEndpoint {
        temperature: 0.7,
        reasoning_effort: Some("high".into()),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(vec![], vec![], false);
    assert!(body.temperature.is_none());
    assert_eq!(body.reasoning_effort.as_deref(), Some("high"));
    // Without reasoning_effort the configured temperature is sent.
    let ep = ModelEndpoint {
        temperature: 0.7,
        reasoning_effort: None,
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(vec![], vec![], false);
    assert_eq!(body.temperature, Some(0.7));
}

#[test]
fn build_request_body_without_tools_has_none() {
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(vec![], vec![], false);
    assert!(body.tools.is_none());
}

#[test]
fn build_request_body_extra_params_all_present() {
    let ep = ModelEndpoint {
        top_p: Some(0.9),
        top_k: Some(40),
        frequency_penalty: Some(0.5),
        presence_penalty: Some(0.3),
        stop: Some(vec!["END".into()]),
        seed: Some(42),
        response_format: Some(serde_json::json!({"type": "json_object"})),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(vec![], vec![], false);
    assert_eq!(body.top_p, Some(0.9));
    assert_eq!(body.top_k, Some(40));
    assert_eq!(body.frequency_penalty, Some(0.5));
    assert_eq!(body.presence_penalty, Some(0.3));
    assert_eq!(body.stop, Some(vec!["END".into()]));
    assert_eq!(body.seed, Some(42));
    assert!(body.response_format.is_some());
}

#[test]
fn build_request_body_extra_params_none_by_default() {
    let ep = ModelEndpoint::default();
    let client = OpenAiAdapter::new(ep);
    let body = client.build_request_body(vec![], vec![], false);
    assert_eq!(body.top_p, None);
    assert_eq!(body.top_k, None);
    assert_eq!(body.frequency_penalty, None);
    assert_eq!(body.presence_penalty, None);
    assert_eq!(body.stop, None);
    assert_eq!(body.seed, None);
    assert_eq!(body.response_format, None);
}

#[test]
fn convert_messages_image_content_part() {
    let msg = CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![
            ContentPart::Text("describe this".into()),
            ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
        ],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let client = OpenAiAdapter::new(ModelEndpoint::default());
    let asset = haven_common::media::MediaAsset::new(
        "image/png",
        5,
        None,
        haven_common::media::MediaAssetSource::UserAttachment,
        haven_common::media::MediaAssetLifecycle::Request,
    );
    let representation = haven_common::media::MediaRepresentation::available(
        haven_common::media::MediaRepresentationKind::RawImage,
        haven_common::media::MediaProvenance::Original,
        haven_common::media::MediaRepresentationPayload::InlineData {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        },
    );
    assert_eq!(
        client
            .capability_profile()
            .supports(&representation, &asset),
        CapabilitySupport::Supported
    );
    let wire =
        serde_json::to_value(client.build_request_body(vec![msg.clone()], Vec::new(), false))
            .unwrap();
    assert_eq!(wire["messages"][0]["content"][1]["type"], "image_url");
    assert!(
        wire["messages"][0]["content"][1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .contains("data:image/png;base64,aGVsbG8=")
    );
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    assert_eq!(openai_msgs.len(), 1);
    let content = openai_msgs[0].content.as_ref().unwrap();
    assert!(content.is_array());
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "describe this");
    assert_eq!(arr[1]["type"], "image_url");
    let url = arr[1]["image_url"]["url"].as_str().unwrap();
    assert!(url.contains("data:image/png;base64,aGVsbG8="));
}

#[test]
fn convert_messages_empty_content() {
    let msg = CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    assert_eq!(openai_msgs.len(), 1);
    assert!(openai_msgs[0].content.is_none());
}

#[test]
fn convert_messages_system_role_maps_to_system_string() {
    let msg = CanonicalMessage {
        role: CanonicalRole::System,
        content: vec![ContentPart::text("you are helpful")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    assert_eq!(openai_msgs[0].role, "system");
}

#[test]
fn convert_messages_assistant_role_maps_to_assistant_string() {
    let msg = CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("hello")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    assert_eq!(openai_msgs[0].role, "assistant");
}

#[test]
fn convert_messages_tool_role_maps_to_tool_string() {
    let msg = CanonicalMessage {
        role: CanonicalRole::Tool,
        content: vec![ContentPart::text("result")],
        tool_call_id: Some("call_1".into()),
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    assert_eq!(openai_msgs[0].role, "tool");
    assert_eq!(openai_msgs[0].tool_call_id.as_deref(), Some("call_1"));
}

#[test]
fn convert_messages_single_text_part_becomes_json_string() {
    let msg = CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![ContentPart::text("hello")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    let content = openai_msgs[0].content.as_ref().unwrap();
    assert!(content.is_string());
    assert_eq!(content.as_str().unwrap(), "hello");
}

#[test]
fn convert_messages_single_audio_part_nested_input_audio() {
    let msg = CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![ContentPart::Audio {
            content_type: "input_audio".into(),
            media_type: "audio/wav".into(),
            data: "aGVsbG8=".into(),
        }],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    let content = openai_msgs[0].content.as_ref().unwrap();
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "input_audio");
    assert_eq!(arr[0]["input_audio"]["format"], "wav");
    assert_eq!(arr[0]["input_audio"]["data"], "aGVsbG8=");
}

#[test]
fn convert_messages_single_image_part_nested_image_url() {
    let msg = CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![ContentPart::Image {
            content_type: "image_url".into(),
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        }],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    };
    let openai_msgs =
        OpenAiAdapter::convert_messages(vec![msg], false, OpenAiAdapter::MAX_REASONING_ECHO_CHARS);
    let content = openai_msgs[0].content.as_ref().unwrap();
    let arr = content.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["type"], "image_url");
    let url = arr[0]["image_url"]["url"].as_str().unwrap();
    assert!(url.contains("data:image/png;base64,aGVsbG8="));
}

#[test]
fn extract_tool_calls_no_message_no_delta() {
    let choice = OpenAiChoice {
        message: None,
        delta: None,
        finish_reason: None,
    };
    let tc = OpenAiAdapter::extract_tool_calls(&choice);
    assert!(tc.is_empty());
}

#[test]
fn stream_content_recovers_long_message_next_to_short_delta() {
    let choice = OpenAiChoice {
        message: Some(OpenAiMessageOut {
            role: Some("assistant".into()),
            content: Some("我先读取文件".into()),
            tool_calls: None,
            reasoning_content: None,
            web_search_call: Vec::new(),
        }),
        delta: Some(OpenAiMessageOut {
            role: None,
            content: Some("我先".into()),
            tool_calls: None,
            reasoning_content: None,
            web_search_call: Vec::new(),
        }),
        finish_reason: Some("tool_calls".into()),
    };
    let mut accumulated = String::new();
    let emitted =
        stream_content(&choice).and_then(|content| append_stream_text(&mut accumulated, &content));
    assert_eq!(emitted.as_deref(), Some("我先读取文件"));
    assert_eq!(accumulated, "我先读取文件");
}

#[test]
fn append_stream_text_converts_cumulative_message_to_delta() {
    let mut accumulated = "我先".to_string();
    let emitted = append_stream_text(&mut accumulated, "我先读取文件");
    assert_eq!(emitted.as_deref(), Some("读取文件"));
    assert_eq!(accumulated, "我先读取文件");

    let duplicate = append_stream_text(&mut accumulated, "我先读取");
    assert_eq!(duplicate, None);
    assert_eq!(accumulated, "我先读取文件");
}

#[test]
fn extract_tool_calls_message_without_tool_calls_field() {
    let choice = OpenAiChoice {
        message: Some(OpenAiMessageOut {
            role: Some("assistant".into()),
            content: Some("plain text response".into()),
            tool_calls: None,
            reasoning_content: None,
            web_search_call: Vec::new(),
        }),
        delta: None,
        finish_reason: Some("stop".into()),
    };
    let tc = OpenAiAdapter::extract_tool_calls(&choice);
    assert!(tc.is_empty());
}

#[test]
fn extract_tool_calls_empty_name_skipped() {
    let choice = OpenAiChoice {
        message: None,
        delta: Some(OpenAiMessageOut {
            role: None,
            content: None,
            tool_calls: Some(vec![OpenAiToolCallOut {
                id: Some("tc1".into()),
                index: None,
                function: OpenAiFunctionOut {
                    name: Some(String::new()),
                    arguments: Some("{}".into()),
                },
            }]),
            reasoning_content: None,
            web_search_call: Vec::new(),
        }),
        finish_reason: None,
    };
    let tc = OpenAiAdapter::extract_tool_calls(&choice);
    assert!(tc.is_empty());
}

#[test]
fn extract_tool_calls_missing_id_defaults_to_empty() {
    let choice = OpenAiChoice {
        message: Some(OpenAiMessageOut {
            role: None,
            content: None,
            tool_calls: Some(vec![OpenAiToolCallOut {
                id: None,
                index: None,
                function: OpenAiFunctionOut {
                    name: Some("run".into()),
                    arguments: Some("{}".into()),
                },
            }]),
            reasoning_content: None,
            web_search_call: Vec::new(),
        }),
        delta: None,
        finish_reason: None,
    };
    let tc = OpenAiAdapter::extract_tool_calls(&choice);
    assert_eq!(tc.len(), 1);
    assert_eq!(tc[0].name, "run");
    assert_eq!(tc[0].id, "");
}

#[test]
fn convert_tools_single_tool() {
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "read".into(),
            description: "read file contents".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    }];
    let result = OpenAiAdapter::convert_tools(tools);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].tool_type, "function");
    assert_eq!(result[0].function.name, "read");
    assert_eq!(result[0].function.description, "read file contents");
}

#[test]
fn convert_tools_multiple_tools() {
    let tools = vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "a".into(),
                description: "d1".into(),
                parameters: serde_json::json!({}),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "b".into(),
                description: "d2".into(),
                parameters: serde_json::json!({}),
            },
        },
    ];
    let result = OpenAiAdapter::convert_tools(tools);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].function.name, "a");
    assert_eq!(result[1].function.name, "b");
}

#[test]
fn convert_tools_empty_vec() {
    let result = OpenAiAdapter::convert_tools(vec![]);
    assert!(result.is_empty());
}

#[test]
fn convert_tools_sanitizes_non_object_schema() {
    let result = OpenAiAdapter::convert_tools(vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "broken".into(),
            description: "broken schema".into(),
            parameters: serde_json::Value::Null,
        },
    }]);

    assert_eq!(
        result[0].function.parameters,
        serde_json::json!({"type": "object", "properties": {}})
    );
}

#[test]
fn openai_convert_tools_projects_root_union_schema() {
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "schedule".into(),
            description: "schedule an action".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["set", "list"] }
                },
                "oneOf": [
                    {
                        "type": "object",
                        "properties": {
                            "operation": { "const": "set" },
                            "body": { "type": "string" }
                        },
                        "required": ["operation", "body"]
                    },
                    {
                        "type": "object",
                        "properties": { "operation": { "const": "list" } },
                        "required": ["operation"]
                    }
                ]
            }),
        },
    }];

    let result = OpenAiAdapter::convert_tools(tools);
    let parameters = &result[0].function.parameters;
    assert_eq!(parameters["type"], "object");
    assert!(parameters.get("oneOf").is_none());
    assert_eq!(parameters["properties"]["operation"]["type"], "string");
    assert_eq!(
        parameters["properties"]["operation"]["enum"],
        serde_json::json!(["set", "list"])
    );
    assert_eq!(
        parameters["dependentSchemas"]["operation"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn openai_chat_projects_schedule_schema_for_xai_provider() {
    let endpoint = ModelEndpoint {
        provider: "xai".into(),
        api_style: Some("openai-chat".into()),
        base_url: "https://api.x.ai/v1".into(),
        ..Default::default()
    };
    let client = OpenAiAdapter::new(endpoint);
    let schedule = ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "schedule".into(),
            description: "schedule an action".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["set", "list", "cancel"] },
                    "delay_secs": { "type": "integer", "minimum": 1 },
                    "due_at": { "type": "string", "minLength": 1 },
                    "body": { "type": "string", "minLength": 1 },
                    "action_id": { "type": "string", "minLength": 1 }
                },
                "required": ["operation"],
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "operation": { "const": "list" } },
                        "required": ["operation"]
                    },
                    {
                        "type": "object",
                        "properties": {
                            "operation": { "const": "cancel" },
                            "action_id": { "type": "string", "minLength": 1 }
                        },
                        "required": ["operation", "action_id"]
                    },
                    {
                        "type": "object",
                        "properties": {
                            "operation": { "const": "set" },
                            "delay_secs": { "type": "integer", "minimum": 1 },
                            "due_at": { "type": "string", "minLength": 1 },
                            "body": { "type": "string", "minLength": 1 }
                        },
                        "required": ["operation", "body"],
                        "oneOf": [
                            { "required": ["delay_secs"] },
                            { "required": ["due_at"] }
                        ]
                    }
                ]
            }),
        },
    };

    let body = client.build_request_body(vec![], vec![schedule], false);
    let parameters = &body.tools.unwrap()[0].function.parameters;
    assert_eq!(parameters["type"], "object");
    assert!(parameters.get("oneOf").is_none());
    assert_eq!(
        parameters["properties"]["operation"]["enum"],
        serde_json::json!(["set", "list", "cancel"])
    );
    assert_eq!(parameters["properties"]["action_id"]["type"], "string");
    assert!(parameters["dependentSchemas"]["operation"]["oneOf"].is_array());
    assert!(parameters["dependentSchemas"]["operation"]["oneOf"][2]["oneOf"].is_array());
}

#[test]
fn openai_convert_tools_projects_undiscriminated_root_union_schema() {
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "example".into(),
            description: "example".into(),
            parameters: serde_json::json!({
                "type": "object",
                "oneOf": [
                    { "type": "object", "required": ["a"] },
                    { "type": "object", "required": ["b"] }
                ]
            }),
        },
    }];

    let result = OpenAiAdapter::convert_tools(tools);
    assert!(result[0].function.parameters.get("oneOf").is_none());
    assert!(result[0].function.parameters["properties"].is_object());
}

#[test]
fn stream_response_parses_usage_from_final_chunk() {
    let json = r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15},"model":"gpt-5"}"#;
    let resp: OpenAiStreamResponse = serde_json::from_str(json).unwrap();
    let usage = resp.usage.expect("final chunk must carry usage");
    assert_eq!(usage.prompt(), 10);
    assert_eq!(usage.completion(), 5);
    assert_eq!(usage.total_tokens, 15);
    assert_eq!(usage.cached(), 0);
}

#[test]
fn usage_parses_prompt_tokens_details_cached_tokens() {
    let json = r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":80}}"#;
    let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
    assert_eq!(usage.cached(), 80);
}

#[test]
fn usage_parses_cache_write_and_miss_tokens() {
    let json = r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":70,"cache_write_tokens":10},"prompt_cache_miss_tokens":20}"#;
    let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
    let normalized = usage.to_usage(None);
    assert_eq!(normalized.cached_tokens, 70);
    assert_eq!(normalized.cache_creation_tokens, 10);
    assert_eq!(normalized.cache_miss_tokens(), 20);
}

#[test]
fn response_without_usage_keeps_cache_outcome_unknown() {
    let adapter = OpenAiAdapter::new(ModelEndpoint::default());
    let response = adapter
        .parse_openai_response(
            OpenAiResponse {
                choices: vec![OpenAiChoice {
                    message: Some(OpenAiMessageOut {
                        role: Some("assistant".into()),
                        content: Some("ok".into()),
                        tool_calls: None,
                        reasoning_content: None,
                        web_search_call: Vec::new(),
                    }),
                    delta: None,
                    finish_reason: Some("stop".into()),
                }],
                usage: None,
                model: None,
                citations: Vec::new(),
            },
            None,
            CacheDiagnostics::for_request(true, true),
        )
        .unwrap();
    assert_eq!(response.usage.cache_diagnostics.unwrap().outcome, "unknown");
}

#[test]
fn usage_parses_deepseek_prompt_cache_hit_tokens() {
    let json = r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_cache_hit_tokens":70}"#;
    let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
    assert_eq!(usage.cached(), 70);
}

#[test]
fn usage_parses_kimi_top_level_cached_tokens() {
    let json =
        r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"cached_tokens":60}"#;
    let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
    assert_eq!(usage.cached(), 60);
}

#[test]
fn usage_parses_input_output_token_aliases() {
    let json = r#"{"input_tokens":40,"output_tokens":8}"#;
    let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
    let canon = usage.to_usage(None);
    assert_eq!(canon.prompt_tokens, 40);
    assert_eq!(canon.completion_tokens, 8);
    assert_eq!(canon.total_tokens, 48);
}

#[test]
fn stream_response_usage_absent_parses_fine() {
    let json = r#"{"id":"c1","choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#;
    let resp: OpenAiStreamResponse = serde_json::from_str(json).unwrap();
    assert!(resp.usage.is_none());
    assert!(!resp.choices.is_empty());
}
