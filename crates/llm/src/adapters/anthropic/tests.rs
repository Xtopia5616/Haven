#[path = "tests/golden.rs"]
mod golden;

use super::*;
use crate::ToolFunction;
use haven_common::prompts::{MEMORY_FENCE_START, SESSION_CONTEXT_FENCE_START};

#[test]
fn build_headers_uses_x_api_key_by_default() {
    let ep = ModelEndpoint {
        api_key: "sk-ant-test".into(),
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert_eq!(
        headers.get("x-api-key").unwrap().to_str().unwrap(),
        "sk-ant-test"
    );
    assert!(!headers.contains_key("authorization"));
    assert_eq!(
        headers.get("anthropic-version").unwrap().to_str().unwrap(),
        "2023-06-01"
    );
}

#[test]
fn build_headers_respects_custom_auth_scheme() {
    let ep = ModelEndpoint {
        api_key: "sk-ant-test".into(),
        auth_header_name: "Authorization".into(),
        auth_header_prefix: "Bearer".into(),
        ..Default::default()
    };
    // "Bearer" is the default prefix, so it must still use x-api-key.
    let client = AnthropicAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert!(headers.get("x-api-key").is_some());

    let ep = ModelEndpoint {
        api_key: "sk-ant-test".into(),
        auth_header_name: "X-Gateway-Key".into(),
        auth_header_prefix: String::new(),
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert!(headers.get("x-gateway-key").is_some());
    assert!(headers.get("x-api-key").is_none());
}

#[test]
fn build_headers_empty_api_key_skips_auth() {
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert!(headers.contains_key("content-type"));
    assert!(headers.get("x-api-key").is_none());
}

#[test]
fn messages_url_handles_v1_suffix() {
    let ep = ModelEndpoint {
        base_url: "https://api.anthropic.com".into(),
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    assert_eq!(
        client.messages_url(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(client.models_url(), "https://api.anthropic.com/v1/models");

    let ep = ModelEndpoint {
        base_url: "https://api.anthropic.com/v1".into(),
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    assert_eq!(
        client.messages_url(),
        "https://api.anthropic.com/v1/messages"
    );
}

#[test]
fn convert_messages_extracts_system_and_maps_roles() {
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("you are helpful")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hello")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (out, system) = AnthropicAdapter::convert_messages(msgs);
    assert_eq!(system.as_deref(), Some("you are helpful"));
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].role, "user");
    assert_eq!(out[0].content[0]["type"], "text");
    assert_eq!(out[0].content[0]["text"], "hello");
}

#[test]
fn validate_content_rejects_audio_instead_of_dropping_it() {
    let client = AnthropicAdapter::new(ModelEndpoint::default());
    let message = CanonicalMessage::user(vec![ContentPart::Audio {
        content_type: "input_audio".into(),
        media_type: "audio/wav".into(),
        data: "UklGRg==".into(),
    }]);
    let error = client.validate_content(&[message]).unwrap_err();
    assert!(error.is_unsupported());
    assert!(error.to_string().contains("audio input"));
}

#[test]
fn convert_messages_multiple_system_messages_joined() {
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("part one")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("part two")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hi")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (_, system) = AnthropicAdapter::convert_messages(msgs);
    assert_eq!(system.as_deref(), Some("part one\n\npart two"));
}

#[test]
fn convert_messages_tool_result_block() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Tool,
        content: vec![ContentPart::text("result body")],
        tool_call_id: Some("toolu_1".into()),
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].role, "user");
    assert_eq!(out[0].content[0]["type"], "tool_result");
    assert_eq!(out[0].content[0]["tool_use_id"], "toolu_1");
    assert_eq!(out[0].content[0]["content"], "result body");
}

#[test]
fn convert_messages_assistant_tool_use_blocks() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("let me check")],
        tool_call_id: None,
        tool_calls: Some(vec![CanonicalToolCall {
            id: "toolu_2".into(),
            name: "file".into(),
            arguments: serde_json::json!({"operation": "read"}),
        }]),
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    assert_eq!(out.len(), 1);
    let content = out[0].content.as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["id"], "toolu_2");
    assert_eq!(content[1]["name"], "file");
    assert_eq!(content[1]["input"]["operation"], "read");
}

#[test]
fn convert_messages_echoes_thinking_blocks_verbatim() {
    let thinking = serde_json::json!({
        "type": "thinking",
        "thinking": "let me plan this out",
        "signature": "sig_abc123"
    });
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("checking")],
        tool_call_id: None,
        tool_calls: Some(vec![CanonicalToolCall {
            id: "toolu_3".into(),
            name: "file".into(),
            arguments: serde_json::json!({"operation": "read"}),
        }]),
        reasoning: Some("let me plan this out".into()),
        web_search_calls: Vec::new(),
        thinking_blocks: vec![thinking.clone()],
        source: None,
        id: None,
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    assert_eq!(out.len(), 1);
    let content = out[0].content.as_array().unwrap();
    assert_eq!(content.len(), 3);
    // The raw thinking block is echoed first, verbatim (type + text +
    // signature), ahead of the text and tool_use blocks.
    assert_eq!(content[0], thinking);
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[2]["type"], "tool_use");
    assert_eq!(content[2]["id"], "toolu_3");
}

fn resp_block(
    block_type: &str,
    text: Option<&str>,
    thinking: Option<&str>,
    id: Option<&str>,
    name: Option<&str>,
    input: Option<Value>,
    signature: Option<&str>,
) -> AnthropicResponseBlock {
    AnthropicResponseBlock {
        block_type: Some(block_type.into()),
        text: text.map(String::from),
        thinking: thinking.map(String::from),
        id: id.map(String::from),
        name: name.map(String::from),
        input,
        signature: signature.map(String::from),
        data: None,
    }
}

/// Parse a response and echo the resulting canonical message back,
/// returning the converted content blocks.
fn parse_and_echo(content: Vec<AnthropicResponseBlock>) -> Vec<Value> {
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let resp = client
        .parse_response(
            AnthropicResponse {
                content,
                stop_reason: None,
                usage: None,
                model: None,
            },
            None,
        )
        .unwrap();
    let msg = CanonicalMessage::assistant(
        vec![ContentPart::text(resp.text.clone())],
        Some(resp.tool_calls),
        resp.reasoning.clone(),
        Vec::new(),
        resp.thinking_blocks,
    );
    let (out, _) = AnthropicAdapter::convert_messages(vec![msg]);
    out[0].content.as_array().unwrap().clone()
}

#[test]
fn convert_messages_echo_restores_interleaved_thinking_text_tool_use() {
    // Original: [thinking, text, tool_use] — the echo must restore the
    // interleaving instead of front-loading the thinking block.
    let content = parse_and_echo(vec![
        resp_block(
            "thinking",
            None,
            Some("plan"),
            None,
            None,
            None,
            Some("sig_1"),
        ),
        resp_block("text", Some("Let me check"), None, None, None, None, None),
        resp_block(
            "tool_use",
            None,
            None,
            Some("toolu_1"),
            Some("file"),
            Some(json!({"operation": "read"})),
            None,
        ),
    ]);
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "plan");
    assert_eq!(content[0]["signature"], "sig_1");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "Let me check");
    assert_eq!(content[2]["type"], "tool_use");
    assert_eq!(content[2]["id"], "toolu_1");
}

#[test]
fn convert_messages_echo_restores_multi_tool_interleaving() {
    // Original: [thinking, tool_use, thinking, tool_use, text]. The final
    // text must stay after both tool calls, and each thinking block must
    // stay with its tool_use.
    let content = parse_and_echo(vec![
        resp_block(
            "thinking",
            None,
            Some("think one"),
            None,
            None,
            None,
            Some("sig_1"),
        ),
        resp_block(
            "tool_use",
            None,
            None,
            Some("toolu_1"),
            Some("file"),
            Some(json!({"op": 1})),
            None,
        ),
        resp_block(
            "thinking",
            None,
            Some("think two"),
            None,
            None,
            None,
            Some("sig_2"),
        ),
        resp_block(
            "tool_use",
            None,
            None,
            Some("toolu_2"),
            Some("file"),
            Some(json!({"op": 2})),
            None,
        ),
        resp_block("text", Some("all done"), None, None, None, None, None),
    ]);
    let types: Vec<&str> = content
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        ["thinking", "tool_use", "thinking", "tool_use", "text"]
    );
    assert_eq!(content[0]["thinking"], "think one");
    assert_eq!(content[1]["id"], "toolu_1");
    assert_eq!(content[2]["thinking"], "think two");
    assert_eq!(content[3]["id"], "toolu_2");
    assert_eq!(content[4]["text"], "all done");
}

#[test]
fn convert_messages_echo_restores_text_before_thinking() {
    // Original: [text, thinking, tool_use].
    let content = parse_and_echo(vec![
        resp_block("text", Some("preface"), None, None, None, None, None),
        resp_block(
            "thinking",
            None,
            Some("plan"),
            None,
            None,
            None,
            Some("sig_1"),
        ),
        resp_block(
            "tool_use",
            None,
            None,
            Some("toolu_1"),
            Some("file"),
            Some(json!({})),
            None,
        ),
    ]);
    let types: Vec<&str> = content
        .iter()
        .map(|b| b["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["text", "thinking", "tool_use"]);
    assert_eq!(content[0]["text"], "preface");
    // The layout marker is stripped before the echo; the thinking block
    // is emitted verbatim.
    assert_eq!(content[1]["signature"], "sig_1");
    assert!(content[1].get("__layout").is_none());
}

#[test]
fn convert_messages_echo_uses_front_loaded_order_for_inconsistent_layout() {
    // A hand-built message whose layout marker does not match the actual
    // thinking blocks uses the deterministic front-loaded order instead
    // of emitting a malformed echo.
    let thinking = serde_json::json!({
        "type": "thinking",
        "thinking": "plan",
        "signature": "sig_x",
    });
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("checking")],
        tool_call_id: None,
        tool_calls: Some(vec![CanonicalToolCall {
            id: "toolu_9".into(),
            name: "file".into(),
            arguments: serde_json::json!({"operation": "read"}),
        }]),
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: vec![
            thinking.clone(),
            // Layout claims two thinking blocks but only one exists.
            json!({"__layout": [[0, 0, 0], [0, 1, 0]]}),
        ],
        source: None,
        id: None,
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    let content = out[0].content.as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0], thinking);
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[2]["type"], "tool_use");
    assert_eq!(content[2]["id"], "toolu_9");
}

#[test]
fn convert_messages_echo_falls_back_when_layout_text_before_exceeds_text() {
    // Layout claims 10 chars of text before the thinking block, but the
    // message only carries 3 — the echo must fall back, not panic.
    let thinking = serde_json::json!({
        "type": "thinking",
        "thinking": "plan",
        "signature": "sig_x",
    });
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("abc")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: vec![thinking.clone(), json!({"__layout": [[0, 0, 10]]})],
        source: None,
        id: None,
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    let content = out[0].content.as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0], thinking);
    assert_eq!(content[1]["type"], "text");
    assert_eq!(content[1]["text"], "abc");
}

#[test]
fn convert_messages_image_part_becomes_base64_source() {
    let msgs = vec![CanonicalMessage {
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
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    let content = out[0].content.as_array().unwrap();
    assert_eq!(content[0]["type"], "image");
    assert_eq!(content[0]["source"]["type"], "base64");
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert_eq!(content[0]["source"]["data"], "aGVsbG8=");
}

#[test]
fn capability_profile_image_reaches_anthropic_wire_payload() {
    let client = AnthropicAdapter::new(ModelEndpoint::default());
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
    let messages = vec![CanonicalMessage {
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
    }];
    let wire =
        serde_json::to_value(client.build_request_body(messages, Vec::new(), false)).unwrap();
    assert_eq!(wire["messages"][0]["content"][0]["type"], "image");
    assert_eq!(
        wire["messages"][0]["content"][0]["source"]["type"],
        "base64"
    );
    assert_eq!(
        wire["messages"][0]["content"][0]["source"]["media_type"],
        "image/png"
    );
}

#[test]
fn convert_messages_empty_user_content_skipped() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (out, _) = AnthropicAdapter::convert_messages(msgs);
    assert!(out.is_empty());
}

#[test]
fn build_request_body_fields() {
    let ep = ModelEndpoint {
        model_name: "claude-sonnet-4-20250514".into(),
        max_tokens: 4096,
        temperature: 0.3,
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    let body = client.build_request_body(
        vec![CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hi")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }],
        Vec::new(),
        true,
    );
    assert_eq!(body.model, "claude-sonnet-4-20250514");
    assert_eq!(body.max_tokens, 4096);
    assert_eq!(body.temperature, Some(0.3));
    assert!(body.stream);
    assert!(body.tools.is_none());
    assert!(body.system.is_none());
}

#[test]
fn build_request_body_uses_adaptive_thinking_for_claude_46() {
    let ep = ModelEndpoint {
        model_name: "claude-sonnet-4-6".into(),
        max_tokens: 4096,
        reasoning_effort: Some("medium".into()),
        temperature: 0.4,
        ..Default::default()
    };
    let body = AnthropicAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(body.thinking, Some(json!({"type": "adaptive"})));
    assert_eq!(body.output_config, Some(json!({"effort": "medium"})));
    assert!(body.temperature.is_none());
}

#[test]
fn build_request_body_uses_manual_budget_for_claude_37() {
    let ep = ModelEndpoint {
        model_name: "claude-3-7-sonnet-20250219".into(),
        max_tokens: 4096,
        reasoning_effort: Some("low".into()),
        ..Default::default()
    };
    let body = AnthropicAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(
        body.thinking,
        Some(json!({"type": "enabled", "budget_tokens": 1024}))
    );
    assert!(body.output_config.is_none());
}

#[test]
fn build_request_body_with_tools() {
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "search".into(),
            description: "search the web".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    }];
    let body = client.build_request_body(vec![], tools, false);
    let tools = body.tools.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "search");
    assert_eq!(tools[0]["input_schema"]["type"], "object");
    assert_eq!(
        tools[0]["cache_control"],
        json!({"type": "ephemeral"}),
        "last tool should carry a prompt-cache breakpoint"
    );
    assert_eq!(body.tool_choice, Some(json!({"type": "auto"})));
}

#[test]
fn convert_tools_sanitizes_non_object_schema_without_flattening_unions() {
    let tools = AnthropicAdapter::convert_tools(vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "schedule".into(),
            description: "schedule an action".into(),
            parameters: json!({
                "type": "object",
                "oneOf": [
                    { "type": "object", "properties": { "operation": { "const": "list" } } },
                    { "type": "object", "properties": { "operation": { "const": "set" } } }
                ]
            }),
        },
    }]);

    assert!(tools[0]["input_schema"].get("oneOf").is_some());

    let sanitized = AnthropicAdapter::convert_tools(vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "broken".into(),
            description: "broken schema".into(),
            parameters: Value::Null,
        },
    }]);
    assert_eq!(
        sanitized[0]["input_schema"],
        json!({"type": "object", "properties": {}})
    );
}

#[test]
fn serialized_request_preserves_all_cache_breakpoints() {
    let client = AnthropicAdapter::new(ModelEndpoint::default());
    let body = client.build_request_body(
        vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("stable instructions")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("old question")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
            CanonicalMessage::assistant(
                vec![ContentPart::text("old answer")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("latest question")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
        ],
        vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "lookup".into(),
                description: "look up a value".into(),
                parameters: json!({"type": "object"}),
            },
        }],
        false,
    );
    let wire = serde_json::to_value(body).unwrap();

    assert_eq!(
        wire["system"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        wire["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert_eq!(
        wire["messages"][1]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(
        wire["messages"][2]["content"][0]
            .get("cache_control")
            .is_none()
    );
    assert!(
        wire["messages"][3]["content"][0]
            .get("cache_control")
            .is_none()
    );
}

#[test]
fn messages_cache_breakpoint_marks_conversation_prefix() {
    let mut messages = vec![
        AnthropicMessage {
            role: "user".into(),
            content: json!([{"type": "text", "text": "first"}]),
        },
        AnthropicMessage {
            role: "assistant".into(),
            content: json!([{"type": "text", "text": "reply"}]),
        },
        AnthropicMessage {
            role: "user".into(),
            content: json!([{"type": "text", "text": "latest"}]),
        },
    ];

    AnthropicAdapter::apply_messages_cache_breakpoint(&mut messages);

    assert_eq!(
        messages[1].content[0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(messages[2].content[0].get("cache_control").is_none());
}

#[test]
fn messages_cache_breakpoint_keeps_tool_result_turn_uncached() {
    let mut messages = vec![
        AnthropicMessage {
            role: "user".into(),
            content: json!([{"type": "text", "text": "request"}]),
        },
        AnthropicMessage {
            role: "assistant".into(),
            content: json!([{"type": "tool_use", "id": "toolu_1"}]),
        },
        AnthropicMessage {
            role: "user".into(),
            content: json!([{"type": "tool_result", "tool_use_id": "toolu_1"}]),
        },
    ];

    AnthropicAdapter::apply_messages_cache_breakpoint(&mut messages);

    assert!(messages[0].content[0].get("cache_control").is_none());
    assert_eq!(
        messages[1].content[0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(messages[2].content[0].get("cache_control").is_none());
}

#[test]
fn messages_cache_breakpoint_marks_short_prefix_without_assuming_a_hit() {
    let mut messages = vec![
        AnthropicMessage {
            role: "assistant".into(),
            content: json!([{"type": "text", "text": "short"}]),
        },
        AnthropicMessage {
            role: "user".into(),
            content: json!([{"type": "text", "text": "latest"}]),
        },
    ];

    AnthropicAdapter::apply_messages_cache_breakpoint(&mut messages);

    assert_eq!(
        messages[0].content[0]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[test]
fn system_with_cache_control_splits_memory_fence() {
    let fence = haven_common::prompts::MEMORY_FENCE_START;
    let full = format!("stable guidelines{fence}volatile facts");
    let blocks = AnthropicAdapter::system_with_cache_control(Some(full)).unwrap();
    let arr = blocks.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["text"], "stable guidelines");
    assert_eq!(arr[0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(arr[1]["text"], format!("{fence}volatile facts"));
    assert!(arr[1].get("cache_control").is_none());

    let no_fence = AnthropicAdapter::system_with_cache_control(Some("all stable".into())).unwrap();
    let one = no_fence.as_array().unwrap();
    assert_eq!(one.len(), 1);
    assert_eq!(one[0]["cache_control"], json!({"type": "ephemeral"}));
    assert!(AnthropicAdapter::system_with_cache_control(None).is_none());
}

#[test]
fn system_with_cache_control_caches_only_stable_prompt_prefix() {
    let full = format!(
        "stable guidelines{SESSION_CONTEXT_FENCE_START}runtime snapshot{MEMORY_FENCE_START}volatile facts"
    );
    let blocks = AnthropicAdapter::system_with_cache_control(Some(full)).unwrap();
    let arr = blocks.as_array().unwrap();

    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0]["text"], "stable guidelines");
    assert_eq!(arr[0]["cache_control"], json!({"type": "ephemeral"}));
    assert_eq!(
        arr[1]["text"],
        format!("{SESSION_CONTEXT_FENCE_START}runtime snapshot")
    );
    assert!(arr[1].get("cache_control").is_none());
    assert_eq!(
        arr[2]["text"],
        format!("{MEMORY_FENCE_START}volatile facts")
    );
    assert!(arr[2].get("cache_control").is_none());
}

#[test]
fn web_search_mode_injects_server_tool() {
    let client = AnthropicAdapter::new(ModelEndpoint::default());
    let client_tool = ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "search".into(),
            description: "search".into(),
            parameters: json!({"type": "object"}),
        },
    };
    let auto =
        client.build_request_body_with_mode(vec![], vec![client_tool], false, WebSearchMode::Auto);
    let tools = auto.tools.expect("web_search tool present");
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[1]["type"], ANTHROPIC_WEB_SEARCH_TOOL_TYPE);
    assert_eq!(tools[1]["name"], "web_search");
    assert_eq!(
        tools[0]["cache_control"],
        json!({"type": "ephemeral"}),
        "client tool index remains the stable tools-cache boundary"
    );
    assert!(tools[1].get("cache_control").is_none());
    assert_eq!(auto.tool_choice, Some(json!({"type": "auto"})));

    let always = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Always);
    assert_eq!(
        always.tool_choice,
        Some(json!({"type": "tool", "name": "web_search"}))
    );

    let with_fn = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "shell".into(),
            description: "run".into(),
            parameters: json!({"type": "object"}),
        },
    }];
    let always_with_tools =
        client.build_request_body_with_mode(vec![], with_fn, false, WebSearchMode::Always);
    assert_eq!(
        always_with_tools.tool_choice,
        Some(json!({"type": "auto"})),
        "Always must not force web_search when ReAct function tools are present"
    );

    let off = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Off);
    assert!(off.tools.is_none());

    let server_only = client
        .build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Auto)
        .tools
        .unwrap();
    assert_eq!(
        server_only[0]["cache_control"],
        json!({"type": "ephemeral"})
    );
}

#[test]
fn client_tool_cache_boundary_stays_before_server_tools() {
    let mut tools = AnthropicAdapter::convert_tools(vec![
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "one".into(),
                description: "one".into(),
                parameters: json!({"type": "object"}),
            },
        },
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "two".into(),
                description: "two".into(),
                parameters: json!({"type": "object"}),
            },
        },
    ]);
    tools.push(json!({
        "type": ANTHROPIC_WEB_SEARCH_TOOL_TYPE,
        "name": "web_search"
    }));

    AnthropicAdapter::apply_tools_cache_breakpoint(&mut tools);

    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(tools[1]["cache_control"], json!({"type": "ephemeral"}));
    assert!(tools[2].get("cache_control").is_none());
}

#[test]
fn parse_response_text_and_usage() {
    let json = AnthropicResponse {
        content: vec![AnthropicResponseBlock {
            block_type: Some("text".into()),
            text: Some("hello there".into()),
            thinking: None,
            id: None,
            name: None,
            input: None,
            signature: None,
            data: None,
        }],
        stop_reason: Some("end_turn".into()),
        usage: Some(AnthropicUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        }),
        model: Some("claude-3".into()),
    };
    let ep = ModelEndpoint {
        model_name: "claude-3".into(),
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    let resp = client
        .parse_response(json, Some("claude-3".into()))
        .unwrap();
    assert_eq!(resp.text, "hello there");
    assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
    assert_eq!(resp.usage.prompt_tokens, 10);
    assert_eq!(resp.usage.completion_tokens, 5);
    assert_eq!(resp.usage.total_tokens, 15);
    assert_eq!(resp.model.as_deref(), Some("claude-3"));
}

#[test]
fn parse_response_usage_includes_cache_tokens_in_total() {
    let json = AnthropicResponse {
        content: vec![AnthropicResponseBlock {
            block_type: Some("text".into()),
            text: Some("cached".into()),
            thinking: None,
            id: None,
            name: None,
            input: None,
            signature: None,
            data: None,
        }],
        stop_reason: Some("end_turn".into()),
        usage: Some(AnthropicUsage {
            input_tokens: 100,
            output_tokens: 5,
            cache_read_input_tokens: 400,
            cache_creation_input_tokens: 50,
        }),
        model: Some("claude-3".into()),
    };
    let ep = ModelEndpoint {
        model_name: "claude-3".into(),
        ..Default::default()
    };
    let client = AnthropicAdapter::new(ep);
    let resp = client
        .parse_response(json, Some("claude-3".into()))
        .unwrap();
    assert_eq!(resp.usage.prompt_tokens, 100);
    assert_eq!(resp.usage.cached_tokens, 400);
    assert_eq!(resp.usage.cache_creation_tokens, 50);
    assert_eq!(resp.usage.total_tokens, 555);
    assert_eq!(resp.usage.context_tokens(), 550);
}

#[test]
fn parse_real_messages_usage_fixture() {
    // Captured from the Anthropic Messages API response shape. Keep this
    // as JSON so serde coverage includes the wire field names and nesting.
    let fixture = r#"
        {
          "id": "msg_01fixture",
          "type": "message",
          "role": "assistant",
          "model": "claude-sonnet-4-20250514",
          "content": [{"type": "text", "text": "cached response"}],
          "stop_reason": "end_turn",
          "usage": {
            "input_tokens": 128,
            "output_tokens": 12,
            "cache_creation_input_tokens": 64,
            "cache_read_input_tokens": 512
          }
        }
        "#;
    let response: AnthropicResponse = serde_json::from_str(fixture).unwrap();
    let client = AnthropicAdapter::new(ModelEndpoint::default());
    let parsed = client
        .parse_response(response, Some("claude-sonnet-4-20250514".into()))
        .unwrap();

    assert_eq!(parsed.usage.prompt_tokens, 128);
    assert_eq!(parsed.usage.cached_tokens, 512);
    assert_eq!(parsed.usage.cache_creation_tokens, 64);
    assert_eq!(parsed.usage.total_tokens, 716);
}

#[test]
fn parse_response_tool_use_blocks() {
    let json = AnthropicResponse {
        content: vec![
            AnthropicResponseBlock {
                block_type: Some("text".into()),
                text: Some("calling tool".into()),
                thinking: None,
                id: None,
                name: None,
                input: None,
                signature: None,
                data: None,
            },
            AnthropicResponseBlock {
                block_type: Some("tool_use".into()),
                text: None,
                thinking: None,
                id: Some("toolu_9".into()),
                name: Some("file".into()),
                input: Some(json!({"operation": "read", "path": "."})),
                signature: None,
                data: None,
            },
        ],
        stop_reason: Some("tool_use".into()),
        usage: None,
        model: None,
    };
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let resp = client.parse_response(json, None).unwrap();
    assert_eq!(resp.text, "calling tool");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "toolu_9");
    assert_eq!(resp.tool_calls[0].name, "file");
    assert_eq!(resp.tool_calls[0].arguments["operation"], "read");
    assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
}

#[test]
fn parse_response_thinking_becomes_reasoning() {
    let json = AnthropicResponse {
        content: vec![
            AnthropicResponseBlock {
                block_type: Some("thinking".into()),
                text: None,
                thinking: Some("inner monologue".into()),
                id: None,
                name: None,
                input: None,
                signature: Some("sig_1".into()),
                data: None,
            },
            AnthropicResponseBlock {
                block_type: Some("text".into()),
                text: Some("final answer".into()),
                thinking: None,
                id: None,
                name: None,
                input: None,
                signature: None,
                data: None,
            },
        ],
        stop_reason: None,
        usage: None,
        model: None,
    };
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let resp = client.parse_response(json, None).unwrap();
    assert_eq!(resp.text, "final answer");
    assert_eq!(resp.reasoning.as_deref(), Some("inner monologue"));
    // The raw thinking block (text + signature) is preserved verbatim for
    // the next request's echo, followed by the internal layout marker that
    // records its original position (block at index 0, no text before).
    assert_eq!(resp.thinking_blocks.len(), 2);
    assert_eq!(resp.thinking_blocks[0]["type"], "thinking");
    assert_eq!(resp.thinking_blocks[0]["thinking"], "inner monologue");
    assert_eq!(resp.thinking_blocks[0]["signature"], "sig_1");
    assert_eq!(resp.thinking_blocks[1], json!({"__layout": [[0, 0, 0]]}));
}

#[test]
fn parse_response_thinking_block_without_signature_omits_field() {
    let json = AnthropicResponse {
        content: vec![AnthropicResponseBlock {
            block_type: Some("thinking".into()),
            text: None,
            thinking: Some("no sig".into()),
            id: None,
            name: None,
            input: None,
            signature: None,
            data: None,
        }],
        stop_reason: None,
        usage: None,
        model: None,
    };
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let resp = client.parse_response(json, None).unwrap();
    assert_eq!(resp.thinking_blocks.len(), 2);
    assert!(resp.thinking_blocks[0].get("signature").is_none());
    assert_eq!(resp.reasoning.as_deref(), Some("no sig"));
}

#[test]
fn parse_response_redacted_thinking_captured_for_echo() {
    let json = AnthropicResponse {
        content: vec![
            AnthropicResponseBlock {
                block_type: Some("redacted_thinking".into()),
                text: None,
                thinking: None,
                id: None,
                name: None,
                input: None,
                signature: None,
                data: Some("base64-redacted".into()),
            },
            AnthropicResponseBlock {
                block_type: Some("text".into()),
                text: Some("answer".into()),
                thinking: None,
                id: None,
                name: None,
                input: None,
                signature: None,
                data: None,
            },
        ],
        stop_reason: None,
        usage: None,
        model: None,
    };
    let ep = ModelEndpoint::default();
    let client = AnthropicAdapter::new(ep);
    let resp = client.parse_response(json, None).unwrap();
    assert_eq!(resp.text, "answer");
    // Redacted data never leaks into visible reasoning, but the block is
    // kept verbatim so the next tool-use request can echo it back.
    assert_eq!(resp.reasoning, None);
    assert_eq!(resp.thinking_blocks.len(), 2);
    assert_eq!(resp.thinking_blocks[0]["type"], "redacted_thinking");
    assert_eq!(resp.thinking_blocks[0]["data"], "base64-redacted");
    assert_eq!(resp.thinking_blocks[1], json!({"__layout": [[0, 0, 0]]}));
}

#[test]
fn stream_events_parse() {
    let start: AnthropicStreamEvent =
            serde_json::from_str(r#"{"type":"message_start","message":{"model":"claude-3","usage":{"input_tokens":10,"output_tokens":0}}}"#)
                .unwrap();
    assert!(matches!(start, AnthropicStreamEvent::MessageStart { .. }));

    let block_start: AnthropicStreamEvent = serde_json::from_str(
        r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
    )
    .unwrap();
    assert!(matches!(
        block_start,
        AnthropicStreamEvent::ContentBlockStart { index: 0, .. }
    ));

    let tool_start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"file","input":{}}}"#,
        )
        .unwrap();
    if let AnthropicStreamEvent::ContentBlockStart { content_block, .. } = tool_start {
        assert_eq!(content_block.name.as_deref(), Some("file"));
    } else {
        panic!("expected tool_use block start");
    }

    // The thinking block's signature arrives on `content_block_start` and
    // must be captured for the verbatim echo.
    let thinking_start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"thinking","signature":"sig_stream_1"}}"#,
        )
        .unwrap();
    if let AnthropicStreamEvent::ContentBlockStart { content_block, .. } = thinking_start {
        assert_eq!(content_block.signature.as_deref(), Some("sig_stream_1"));
    } else {
        panic!("expected thinking block start");
    }

    // `redacted_thinking` blocks stream the same `thinking_delta` data
    // chunks; the start event only marks the block type.
    let redacted_start: AnthropicStreamEvent = serde_json::from_str(
        r#"{"type":"content_block_start","index":3,"content_block":{"type":"redacted_thinking"}}"#,
    )
    .unwrap();
    if let AnthropicStreamEvent::ContentBlockStart { content_block, .. } = redacted_start {
        assert_eq!(
            content_block.block_type.as_deref(),
            Some("redacted_thinking")
        );
    } else {
        panic!("expected redacted_thinking block start");
    }

    let delta: AnthropicStreamEvent = serde_json::from_str(
        r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
    )
    .unwrap();
    assert!(matches!(
        delta,
        AnthropicStreamEvent::ContentBlockDelta {
            delta: AnthropicStreamDelta::TextDelta { text },
            ..
        } if text == "Hi"
    ));

    let msg_delta: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
        )
        .unwrap();
    assert!(matches!(
        msg_delta,
        AnthropicStreamEvent::MessageDelta { .. }
    ));

    let stop: AnthropicStreamEvent = serde_json::from_str(r#"{"type":"message_stop"}"#).unwrap();
    assert!(matches!(stop, AnthropicStreamEvent::MessageStop));

    let unknown: AnthropicStreamEvent =
        serde_json::from_str(r#"{"type":"future_event","x":1}"#).unwrap();
    assert!(matches!(unknown, AnthropicStreamEvent::Other));
}
