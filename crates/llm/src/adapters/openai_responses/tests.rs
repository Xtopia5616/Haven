#[path = "tests/golden.rs"]
mod golden;

use super::*;
use crate::ToolFunction;

#[test]
fn stream_event_parses_reasoning_text_delta() {
    // DeepSeek streams thinking-mode reasoning via this event; it must be
    // parsed (not fall through to Other) so it can be echoed back.
    let ev: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.reasoning_text.delta","content_index":0,"delta":"We need","item_id":"rs_1","output_index":0,"sequence_number":4}"#,
        )
        .unwrap();
    match ev {
        ResponsesStreamEvent::ReasoningTextDelta { delta } => {
            assert_eq!(delta.as_deref(), Some("We need"));
        }
        other => panic!("unexpected variant: {:?}", other),
    }
}

#[test]
fn responses_url_handles_v1_suffix() {
    let ep = ModelEndpoint {
        base_url: "https://api.openai.com/v1".into(),
        ..Default::default()
    };
    let client = OpenAiResponsesAdapter::new(ep);
    assert_eq!(
        client.responses_url(),
        "https://api.openai.com/v1/responses"
    );

    let ep = ModelEndpoint {
        base_url: "https://api.openai.com".into(),
        ..Default::default()
    };
    let client = OpenAiResponsesAdapter::new(ep);
    assert_eq!(
        client.responses_url(),
        "https://api.openai.com/v1/responses"
    );
}

#[test]
fn embeddings_url_adds_v1_like_responses() {
    assert_eq!(
        super::super::openai_embeddings_url("https://api.deepseek.com", true),
        "https://api.deepseek.com/v1/embeddings"
    );
    assert_eq!(
        super::super::openai_embeddings_url("https://api.openai.com/v1", true),
        "https://api.openai.com/v1/embeddings"
    );
}

#[test]
fn convert_input_extracts_instructions() {
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
    let (items, instructions) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(instructions.as_deref(), Some("you are helpful"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["role"], "user");
    assert_eq!(items[0]["content"][0]["type"], "input_text");
    assert_eq!(items[0]["content"][0]["text"], "hello");
}

#[test]
fn convert_input_function_call_and_output() {
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("let me check")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_1".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("result body")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["role"], "assistant");
    assert_eq!(items[0]["content"][0]["type"], "output_text");
    assert_eq!(items[1]["type"], "function_call");
    assert_eq!(items[1]["call_id"], "call_1");
    assert_eq!(items[1]["name"], "file");
    assert_eq!(items[2]["type"], "function_call_output");
    assert_eq!(items[2]["call_id"], "call_1");
    assert_eq!(items[2]["output"], "result body");
}

#[test]
fn convert_input_echoes_reasoning_for_thinking_mode() {
    // DeepSeek's thinking-mode compat layer rejects tool-call history
    // without the assistant's reasoning_text passed back (400). The
    // reasoning item must be emitted before the function_call item, in
    // the same position the provider produced it (reasoning → message →
    // function_call).
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("let me check")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_1".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: Some("  I should read the file first.  ".into()),
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("result body")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs.clone(),
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        true,
    );
    assert_eq!(items.len(), 4);
    // The reasoning item leads the turn, carrying an OpenAI-style
    // content-part array (DeepSeek's compat layer deserializes the
    // `content` of a `reasoning` input item as a SEQUENCE of parts; a
    // plain string 400s with "invalid type: string, expected a sequence"),
    // then the message, then the tool call.
    assert_eq!(items[0]["type"], "reasoning");
    assert_eq!(items[0]["content"][0]["type"], "reasoning_text");
    assert_eq!(
        items[0]["content"][0]["text"],
        "I should read the file first."
    );
    assert_eq!(items[1]["role"], "assistant");
    assert_eq!(items[1]["content"][0]["type"], "output_text");
    assert_eq!(items[2]["type"], "function_call");
    assert_eq!(items[3]["type"], "function_call_output");
    // Providers without the echo requirement get NO reasoning item: the
    // plain-text form is DeepSeek-compat-specific, and other APIs neither
    // require nor accept it.
    let (no_echo, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(no_echo.len(), 3);
    assert_eq!(no_echo[0]["role"], "assistant");
    assert_eq!(no_echo[1]["type"], "function_call");
    assert_eq!(no_echo[2]["type"], "function_call_output");
}

#[test]
fn convert_input_synthesizes_empty_reasoning_for_tool_turns_when_echo_required() {
    // DeepSeek thinking mode validates PRESENCE of the reasoning item,
    // not content: a tool-call turn on which the model skipped thinking
    // (reasoning absent) must still echo an (empty) reasoning item, or
    // the next request 400s. Only the DeepSeek echo is synthesized.
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("let me check")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_1".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("result body")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (with_echo, _) = OpenAiResponsesAdapter::convert_input(
        msgs.clone(),
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        true,
    );
    assert_eq!(with_echo.len(), 4);
    assert_eq!(with_echo[0]["type"], "reasoning");
    assert_eq!(with_echo[0]["content"][0]["type"], "reasoning_text");
    assert_eq!(with_echo[0]["content"][0]["text"], "");
    assert_eq!(with_echo[1]["role"], "assistant");
    assert_eq!(with_echo[2]["type"], "function_call");
    assert_eq!(with_echo[3]["type"], "function_call_output");
    // Without the echo requirement the reasoning item must not appear.
    let (no_echo, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(no_echo.len(), 3);
    assert_eq!(no_echo[0]["role"], "assistant");
    assert_eq!(no_echo[1]["type"], "function_call");
}

#[test]
fn convert_input_truncates_oversized_reasoning_to_tail() {
    // Full reasoning echo (10k+ chars per turn) balloons the request body
    // and providers stall/truncate mid-inference. Oversized reasoning must
    // keep its TAIL (the conclusions), trimmed of whitespace.
    let long = format!(
        "{}END-MARKER",
        "thinking step. ".repeat(OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS + 500)
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
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        true,
    );
    assert_eq!(items.len(), 2);
    let echoed = items[0]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        echoed.len(),
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS
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
}

#[test]
fn convert_input_skips_blank_reasoning() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("hi")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: Some("   ".into()),
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["role"], "assistant");
}

#[test]
fn convert_input_image_and_audio() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::User,
        content: vec![
            ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            },
            ContentPart::Audio {
                content_type: "input_audio".into(),
                media_type: "audio/wav".into(),
                data: "d3d3".into(),
            },
        ],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(items[0]["content"][0]["type"], "input_image");
    assert!(
        items[0]["content"][0]["image_url"]
            .as_str()
            .unwrap()
            .contains("data:image/png;base64,aGVsbG8=")
    );
    assert_eq!(items[0]["content"][1]["type"], "input_audio");
    assert_eq!(items[0]["content"][1]["input_audio"]["format"], "wav");
}

#[test]
fn capability_profile_image_reaches_responses_wire_payload() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint::default());
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
    assert_eq!(wire["input"][0]["content"][0]["type"], "input_image");
    assert!(
        wire["input"][0]["content"][0]["image_url"]
            .as_str()
            .unwrap()
            .contains("data:image/png;base64,aGVsbG8=")
    );
}

#[test]
fn build_request_body_fields() {
    let ep = ModelEndpoint {
        model_name: "gpt-5".into(),
        max_tokens: 2048,
        temperature: 0.4,
        ..Default::default()
    };
    let client = OpenAiResponsesAdapter::new(ep);
    let body = client.build_request_body_with_mode(
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
        WebSearchMode::Off,
    );
    assert_eq!(body.model, "gpt-5");
    assert_eq!(body.max_output_tokens, Some(2048));
    assert_eq!(body.temperature, Some(0.4));
    assert!(body.stream);
    assert!(body.tools.is_none());
    assert!(body.tool_choice.is_none());
}

#[test]
fn build_request_body_maps_top_p_and_nested_text_format() {
    let ep = ModelEndpoint {
        model_name: "gpt-5".into(),
        top_p: Some(0.8),
        response_format: Some(json!({"type": "json_object"})),
        ..Default::default()
    };
    let body = OpenAiResponsesAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(body.top_p, Some(0.8));
    assert_eq!(body.text, Some(json!({"format": {"type": "json_object"}})));
}

#[test]
fn build_request_body_maps_deepseek_effort_to_output_config() {
    let ep = ModelEndpoint {
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com".into(),
        model_name: "deepseek-v4-pro".into(),
        top_p: Some(0.8),
        reasoning_effort: Some("medium".into()),
        ..Default::default()
    };
    let body = OpenAiResponsesAdapter::new(ep).build_request_body(vec![], vec![], false);
    assert_eq!(body.reasoning, Some(json!({"effort": "high"})));
    assert_eq!(body.output_config, Some(json!({"effort": "high"})));
    assert!(body.top_p.is_none());
    assert!(body.prompt_cache_key.is_none());
}

#[test]
fn build_request_body_skips_temperature_one() {
    let ep = ModelEndpoint {
        temperature: 1.0,
        ..Default::default()
    };
    let client = OpenAiResponsesAdapter::new(ep);
    let body = client.build_request_body(vec![], Vec::new(), false);
    assert_eq!(body.temperature, None);
}

#[test]
fn build_request_body_with_tools() {
    let ep = ModelEndpoint::default();
    let client = OpenAiResponsesAdapter::new(ep);
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "search".into(),
            description: "search the web".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    }];
    let body = client.build_request_body_with_mode(vec![], tools, false, WebSearchMode::Off);
    let tools = body.tools.unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "search");
    assert_eq!(tools[0]["strict"], false);
    assert_eq!(body.tool_choice, Some(serde_json::json!("auto")));
}

#[test]
fn responses_tools_project_root_union_to_object_schema() {
    let tools = OpenAiResponsesAdapter::convert_tools(vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "schedule".into(),
            description: "schedule an action".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["set", "list", "cancel"] }
                },
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
                            "action_id": { "type": "string" }
                        },
                        "required": ["operation", "action_id"]
                    },
                    {
                        "type": "object",
                        "properties": {
                            "operation": { "const": "set" },
                            "body": { "type": "string" }
                        },
                        "required": ["operation", "body"]
                    }
                ]
            }),
        },
    }]);

    assert_eq!(tools[0]["strict"], false);
    assert_eq!(tools[0]["parameters"]["type"], "object");
    assert!(tools[0]["parameters"].get("oneOf").is_none());
    assert_eq!(
        tools[0]["parameters"]["properties"]["operation"]["enum"],
        serde_json::json!(["set", "list", "cancel"])
    );
    assert!(tools[0]["parameters"]["properties"]["action_id"].is_object());
    assert!(tools[0]["parameters"]["properties"]["body"].is_object());
}

#[test]
fn web_search_mode_shapes_the_request() {
    let ep = ModelEndpoint::default();
    let client = OpenAiResponsesAdapter::new(ep);
    let defs = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "file".into(),
            description: "files".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    }];

    // auto: web_search tool appended, model decides via tool_choice auto.
    let body =
        client.build_request_body_with_mode(vec![], defs.clone(), false, WebSearchMode::Auto);
    let tools = body.tools.unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[1], serde_json::json!({"type": "web_search"}));
    assert_eq!(body.tool_choice, Some(serde_json::json!("auto")));

    // always: search forced via a specific tool choice.
    let body =
        client.build_request_body_with_mode(vec![], defs.clone(), false, WebSearchMode::Always);
    let tools = body.tools.unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(
        body.tool_choice,
        Some(serde_json::json!({"type": "web_search"}))
    );

    // auto with no function tools still exposes the search tool.
    let body = client.build_request_body_with_mode(vec![], Vec::new(), false, WebSearchMode::Auto);
    let tools = body.tools.unwrap();
    assert_eq!(tools, vec![serde_json::json!({"type": "web_search"})]);

    // off with no function tools: request identical to pre-feature shape.
    let body = client.build_request_body_with_mode(vec![], Vec::new(), false, WebSearchMode::Off);
    assert!(body.tools.is_none());
    assert!(body.tool_choice.is_none());
}

#[test]
fn convert_input_supplies_missing_web_search_call_action() {
    // DeepSeek rejects an `action`-less web_search_call input item with a
    // 400 ("missing field `action`"): the stream's output_item.added
    // skeleton only carries type/id/status, so the fallback fills the
    // internally-tagged-enum action object (search variant + queries).
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("let me search")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: vec![serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress"
        })],
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["role"], "assistant");
    assert_eq!(
        items[1],
        serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress",
            "action": {"type": "search", "queries": []}
        })
    );
}

#[test]
fn convert_input_round_trips_complete_web_search_call_verbatim() {
    // An item that already carries a well-formed object `action`
    // (e.g. a completed payload) is passed back untouched.
    let ws = serde_json::json!({
        "type": "web_search_call",
        "id": "ws_1",
        "status": "completed",
        "action": {"type": "open_page", "url": "https://example.com"},
        "query": "foo"
    });
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("let me search")],
        tool_call_id: None,
        tool_calls: None,
        reasoning: None,
        web_search_calls: vec![ws.clone()],
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (items, _) = OpenAiResponsesAdapter::convert_input(
        msgs,
        OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
        false,
    );
    assert_eq!(items.len(), 2);
    assert_eq!(items[1], ws);
}

#[test]
fn parse_response_collects_web_search_calls() {
    let json = ResponsesResponse {
        status: Some("completed".into()),
        output: vec![
            ResponsesItem {
                item_type: Some("web_search_call".into()),
                content: vec![],
                call_id: None,
                id: Some("ws_1".into()),
                name: None,
                arguments: None,
                extra: serde_json::json!({"status": "completed"})
                    .as_object()
                    .unwrap()
                    .clone(),
            },
            ResponsesItem {
                item_type: Some("message".into()),
                content: vec![ResponsesContentPart {
                    text: Some("found it".into()),
                }],
                call_id: None,
                id: Some("msg_1".into()),
                name: None,
                arguments: None,
                extra: Default::default(),
            },
        ],
        usage: None,
        model: Some("deepseek-v4-flash".into()),
        error: None,
    };
    let ep = ModelEndpoint::default();
    let client = OpenAiResponsesAdapter::new(ep);
    let resp = client
        .parse_response(json, Some("deepseek-v4-flash".into()))
        .unwrap();
    assert_eq!(resp.text, "found it");
    assert!(resp.tool_calls.is_empty());
    assert_eq!(resp.web_search_calls.len(), 1);
    let item = &resp.web_search_calls[0];
    assert_eq!(item["type"], "web_search_call");
    assert_eq!(item["id"], "ws_1");
    assert_eq!(item["status"], "completed");
    assert_eq!(
        item["action"],
        serde_json::json!({"type": "search", "queries": []})
    );
}

#[test]
fn parse_response_text_tool_call_usage() {
    let json = ResponsesResponse {
        status: Some("completed".into()),
        output: vec![
            ResponsesItem {
                item_type: Some("message".into()),
                content: vec![ResponsesContentPart {
                    text: Some("checking".into()),
                }],
                call_id: None,
                id: Some("msg_1".into()),
                name: None,
                arguments: None,
                extra: Default::default(),
            },
            ResponsesItem {
                item_type: Some("function_call".into()),
                content: vec![],
                call_id: Some("call_1".into()),
                id: Some("fc_1".into()),
                name: Some("file".into()),
                arguments: Some(r#"{"operation":"read"}"#.into()),
                extra: Default::default(),
            },
        ],
        usage: Some(ResponsesUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        }),
        model: Some("gpt-5".into()),
        error: None,
    };
    let ep = ModelEndpoint::default();
    let client = OpenAiResponsesAdapter::new(ep);
    let resp = client.parse_response(json, Some("gpt-5".into())).unwrap();
    assert_eq!(resp.text, "checking");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_1");
    assert_eq!(resp.tool_calls[0].name, "file");
    assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
    assert_eq!(resp.usage.total_tokens, 15);
    assert_eq!(resp.model.as_deref(), Some("gpt-5"));
}

#[test]
fn parse_response_failed_status_errors() {
    let json = ResponsesResponse {
        status: Some("failed".into()),
        output: vec![],
        usage: None,
        model: None,
        error: Some(json!({"code": "server_error", "message": "boom"})),
    };
    let ep = ModelEndpoint::default();
    let client = OpenAiResponsesAdapter::new(ep);
    let err = client.parse_response(json, None).unwrap_err();
    assert!(matches!(err, LlmError::RequestFailed(_)));
}

#[test]
fn stream_events_parse() {
    let delta: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"Hello"}"#,
        )
        .unwrap();
    assert!(matches!(
        delta,
        ResponsesStreamEvent::OutputTextDelta { delta: Some(d) } if d == "Hello"
    ));

    let item: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"file","arguments":""}}"#,
        )
        .unwrap();
    if let ResponsesStreamEvent::OutputItemAdded { item: Some(item) } = item {
        assert_eq!(item.name.as_deref(), Some("file"));
    } else {
        panic!("expected output_item.added");
    }

    let args: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"op"}"#,
        )
        .unwrap();
    assert!(matches!(
        args,
        ResponsesStreamEvent::FunctionCallArgsDelta { delta: Some(d), .. } if d == "{\"op"
    ));

    let completed: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.completed","response":{"status":"completed","model":"gpt-5","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
        )
        .unwrap();
    assert!(matches!(completed, ResponsesStreamEvent::Completed { .. }));

    let incomplete: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.incomplete","response":{"status":"incomplete","model":"deepseek-v4-pro"}}"#,
        )
        .unwrap();
    assert!(matches!(
        incomplete,
        ResponsesStreamEvent::Incomplete { .. }
    ));

    let other: ResponsesStreamEvent =
        serde_json::from_str(r#"{"type":"response.in_progress"}"#).unwrap();
    assert!(matches!(other, ResponsesStreamEvent::Other));
}

#[test]
fn stream_web_search_events_parse() {
    let in_progress: ResponsesStreamEvent =
        serde_json::from_str(r#"{"type":"response.web_search_call.in_progress","item_id":"ws_1"}"#)
            .unwrap();
    match in_progress {
        ResponsesStreamEvent::WebSearchInProgress { item_id: Some(id) } => {
            assert_eq!(id, "ws_1")
        }
        other => panic!("expected WebSearchInProgress with item_id, got {other:?}"),
    }

    let searching: ResponsesStreamEvent =
        serde_json::from_str(r#"{"type":"response.web_search_call.searching","item_id":"ws_1"}"#)
            .unwrap();
    match searching {
        ResponsesStreamEvent::WebSearchSearching { item_id: Some(id) } => {
            assert_eq!(id, "ws_1")
        }
        other => panic!("expected WebSearchSearching with item_id, got {other:?}"),
    }

    let completed: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.web_search_call.completed","item_id":"ws_1","item":{"type":"web_search_call","id":"ws_1","status":"completed"}}"#,
        )
        .unwrap();
    if let ResponsesStreamEvent::WebSearchCompleted {
        item_id: Some(item_id),
        item: Some(item),
    } = completed
    {
        assert_eq!(item_id, "ws_1");
        assert_eq!(item.item_type.as_deref(), Some("web_search_call"));
        assert_eq!(item.id.as_deref(), Some("ws_1"));
        assert_eq!(
            item.extra.get("status").and_then(|v| v.as_str()),
            Some("completed")
        );
    } else {
        panic!("expected web_search_call.completed with item");
    }

    // The raw item serializes back verbatim (round-trip input fidelity).
    let raw = serde_json::to_value(item_of(&completed_parse())).unwrap();
    assert_eq!(
        raw,
        serde_json::json!({"type": "web_search_call", "id": "ws_1", "status": "completed"})
    );

    // `output_item.done` carries the FULL web_search_call payload
    // (action + queries) that the skeleton lacks; this is the event the
    // round-trip must keep.
    let done: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","queries":["capital of France","ws_call_id=ws_1"]}}}"#,
        )
        .unwrap();
    if let ResponsesStreamEvent::OutputItemDone { item: Some(item) } = done {
        assert_eq!(item.item_type.as_deref(), Some("web_search_call"));
        assert_eq!(item.id.as_deref(), Some("ws_1"));
        let action = item.extra.get("action").unwrap();
        assert_eq!(action["type"], "search");
        assert_eq!(action["queries"][0], "capital of France");
    } else {
        panic!("expected output_item.done with web_search_call item");
    }
}

fn completed_parse() -> ResponsesStreamEvent {
    serde_json::from_str(
            r#"{"type":"response.web_search_call.completed","item":{"type":"web_search_call","id":"ws_1","status":"completed"}}"#,
        )
        .unwrap()
}

fn item_of(event: &ResponsesStreamEvent) -> &ResponsesItem {
    match event {
        ResponsesStreamEvent::WebSearchCompleted { item, .. } => item.as_ref().unwrap(),
        _ => panic!("expected WebSearchCompleted"),
    }
}

#[test]
fn build_headers_default_auth_header() {
    let ep = ModelEndpoint {
        api_key: "sk-test".into(),
        ..Default::default()
    };
    let client = OpenAiResponsesAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    let val = headers.get("authorization").unwrap().to_str().unwrap();
    assert_eq!(val, "Bearer sk-test");
}

#[test]
fn build_request_body_forwards_reasoning_effort() {
    let ep = ModelEndpoint {
        model_name: "gpt-5".into(),
        temperature: 0.4,
        reasoning_effort: Some("medium".into()),
        ..Default::default()
    };
    let body = OpenAiResponsesAdapter::new(ep).build_request_body_with_mode(
        vec![],
        Vec::new(),
        false,
        WebSearchMode::Off,
    );
    assert_eq!(
        body.reasoning,
        Some(serde_json::json!({"effort": "medium"}))
    );
    assert!(body.temperature.is_none());
}

#[test]
fn build_request_body_deepseek_reasoning_maps_medium_and_none() {
    let ep = ModelEndpoint {
        provider: "deepseek".into(),
        base_url: "https://api.deepseek.com".into(),
        model_name: "deepseek-v4-flash".into(),
        api_style: Some("openai-responses".into()),
        reasoning_effort: Some("medium".into()),
        temperature: 0.7,
        ..Default::default()
    };
    let body = OpenAiResponsesAdapter::new(ep.clone()).build_request_body_with_mode(
        vec![],
        Vec::new(),
        false,
        WebSearchMode::Off,
    );
    assert_eq!(body.reasoning, Some(serde_json::json!({"effort": "high"})));
    assert!(body.temperature.is_none());

    let off = ModelEndpoint {
        reasoning_effort: Some("none".into()),
        ..ep
    };
    let body = OpenAiResponsesAdapter::new(off).build_request_body_with_mode(
        vec![],
        Vec::new(),
        false,
        WebSearchMode::Off,
    );
    assert_eq!(body.reasoning, Some(serde_json::json!({"effort": "none"})));
}

#[test]
fn build_request_body_omits_reasoning_when_effort_unset() {
    let ep = ModelEndpoint {
        model_name: "gpt-5".into(),
        temperature: 0.4,
        ..Default::default()
    };
    let body = OpenAiResponsesAdapter::new(ep).build_request_body_with_mode(
        vec![],
        Vec::new(),
        false,
        WebSearchMode::Off,
    );
    assert!(body.reasoning.is_none());
    assert_eq!(body.temperature, Some(0.4));
}

#[test]
fn prompt_cache_key_is_stable_across_memory_refreshes() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint {
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let stable = "You are Haven.\nCurrent session: inspect cache\n";
    let first_system = CanonicalMessage::system(vec![ContentPart::text(format!(
        "{stable}{MEMORY_FENCE_START}first recalled fact"
    ))]);
    let refreshed_system = CanonicalMessage::system(vec![ContentPart::text(format!(
        "{stable}{MEMORY_FENCE_START}refreshed recalled fact"
    ))]);
    let user = CanonicalMessage::user_text("continue");

    let first = client
        .build_request_body(vec![first_system, user.clone()], Vec::new(), false)
        .prompt_cache_key;
    let refreshed = client
        .build_request_body(vec![refreshed_system, user], Vec::new(), false)
        .prompt_cache_key;

    assert!(first.is_some());
    assert_eq!(first, refreshed);
}

#[test]
fn prompt_cache_key_matches_the_sanitized_responses_tool_wire() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint {
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
    let user = CanonicalMessage::user_text("session anchor");
    let tool = |parameters| ToolDefinition {
        tool_type: "function".into(),
        function: crate::types::ToolFunction {
            name: "read".into(),
            description: "read a file".into(),
            parameters,
        },
    };

    let null_root = client
        .build_request_body(
            vec![system.clone(), user.clone()],
            vec![tool(Value::Null)],
            false,
        )
        .prompt_cache_key;
    let sanitized_root = client
        .build_request_body(
            vec![system, user],
            vec![tool(json!({"type": "object", "properties": {}}))],
            false,
        )
        .prompt_cache_key;

    assert_eq!(null_root, sanitized_root);
}

#[test]
fn prompt_cache_key_changes_with_builtin_web_search_mode() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint::default());
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
fn memory_refresh_keeps_responses_instructions_prefix_stable() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint {
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let stable = "You are Haven.\nCurrent session: inspect cache\n";
    let first_system = CanonicalMessage::system(vec![ContentPart::text(format!(
        "{stable}{MEMORY_FENCE_START}first recalled fact"
    ))]);
    let refreshed_system = CanonicalMessage::system(vec![ContentPart::text(format!(
        "{stable}{MEMORY_FENCE_START}refreshed recalled fact"
    ))]);
    let user = CanonicalMessage::user_text("continue");

    let first = client.build_request_body(vec![first_system, user.clone()], Vec::new(), false);
    let refreshed = client.build_request_body(vec![refreshed_system, user], Vec::new(), false);

    assert_eq!(first.instructions, Some(stable.into()));
    assert_eq!(first.instructions, refreshed.instructions);
    assert_eq!(first.input[0]["role"], "user");
    assert_eq!(first.input[0]["content"][0]["text"], "continue");
    assert_eq!(refreshed.input[1]["role"], "developer");
    assert_eq!(
        refreshed.input[1]["content"][0]["text"],
        format!("{MEMORY_FENCE_START}refreshed recalled fact")
    );
    assert_eq!(first.input[1]["role"], "developer");
    assert_eq!(
        first.input[1]["content"][0]["text"],
        format!("{MEMORY_FENCE_START}first recalled fact")
    );
}

#[test]
fn responses_memory_refresh_preserves_the_conversation_prefix() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint::default());
    let stable = "stable instructions\n";
    let session = format!("{SESSION_CONTEXT_FENCE_START}Current session: inspect cache\n");
    let canonical = |memory: &str| {
        vec![
            CanonicalMessage::system(vec![ContentPart::text(format!(
                "{stable}{session}{MEMORY_FENCE_START}{memory}"
            ))]),
            CanonicalMessage::user_text("session anchor"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("checking")],
                Some(vec![CanonicalToolCall {
                    id: "call_1".into(),
                    name: "read".into(),
                    arguments: json!({"path": "notes.txt"}),
                }]),
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::tool(vec![ContentPart::text("result")], Some("call_1".into())),
        ]
    };
    let first = client.build_request_body(canonical("old fact"), Vec::new(), false);
    let refreshed = client.build_request_body(canonical("new fact"), Vec::new(), false);

    assert_eq!(first.instructions, refreshed.instructions);
    assert_eq!(first.input.len(), refreshed.input.len());
    assert_eq!(
        first.input[..first.input.len() - 1],
        refreshed.input[..refreshed.input.len() - 1]
    );
    assert_eq!(first.input.last().unwrap()["role"], "developer");
    assert_eq!(refreshed.input.last().unwrap()["role"], "developer");
    assert_ne!(
        first.input.last().unwrap()["content"][0]["text"],
        refreshed.input.last().unwrap()["content"][0]["text"]
    );
}

#[test]
fn prompt_cache_key_changes_when_tools_change_or_is_unsupported() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint::default());
    let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
    let user = CanonicalMessage::user_text("session input");
    let one_tool = vec![ToolDefinition {
        tool_type: "function".into(),
        function: crate::types::ToolFunction {
            name: "read".into(),
            description: "read a file".into(),
            parameters: json!({"type":"object"}),
        },
    }];
    let two_tools = vec![
        one_tool[0].clone(),
        ToolDefinition {
            tool_type: "function".into(),
            function: crate::types::ToolFunction {
                name: "write".into(),
                description: "write a file".into(),
                parameters: json!({"type":"object"}),
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
fn prompt_cache_key_rejection_detection_is_specific() {
    assert!(OpenAiResponsesAdapter::prompt_cache_key_rejected(
        &LlmError::RequestFailed("400: Unknown parameter: prompt_cache_key".into())
    ));
    assert!(!OpenAiResponsesAdapter::prompt_cache_key_rejected(
        &LlmError::RequestFailed("400: maximum context length exceeded".into())
    ));
}

#[test]
fn developer_input_rejection_detection_is_specific() {
    assert!(OpenAiResponsesAdapter::developer_input_rejected(
        &LlmError::RequestFailed("400: developer role is not supported".into())
    ));
    assert!(!OpenAiResponsesAdapter::developer_input_rejected(
        &LlmError::RequestFailed("400: invalid developer instruction content".into())
    ));
}

#[test]
fn developer_input_downgrade_preserves_instructions_prefix() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint::default());
    let mut body = client.build_request_body(
        vec![CanonicalMessage::system(vec![ContentPart::text(format!(
            "stable{SESSION_CONTEXT_FENCE_START}session{MEMORY_FENCE_START}volatile"
        ))])],
        Vec::new(),
        false,
    );

    assert_eq!(body.instructions.as_deref(), Some("stable"));
    assert_eq!(body.input[0]["role"], "developer");
    assert_eq!(body.input[1]["role"], "developer");

    assert!(OpenAiResponsesAdapter::downgrade_developer_input(&mut body));
    assert_eq!(body.instructions.as_deref(), Some("stable"));
    assert_eq!(body.input[0]["role"], "user");
    assert_eq!(body.input[1]["role"], "user");
    assert!(!OpenAiResponsesAdapter::downgrade_developer_input(
        &mut body
    ));
}

#[test]
fn unsupported_developer_input_keeps_dynamic_sections_out_of_instructions() {
    let client = OpenAiResponsesAdapter::new(ModelEndpoint::default());
    client
        .developer_input_state
        .store(DEVELOPER_INPUT_UNSUPPORTED, Ordering::Relaxed);
    let body = client.build_request_body(
        vec![CanonicalMessage::system(vec![ContentPart::text(format!(
            "stable{SESSION_CONTEXT_FENCE_START}session{MEMORY_FENCE_START}volatile"
        ))])],
        Vec::new(),
        false,
    );

    assert_eq!(body.instructions.as_deref(), Some("stable"));
    assert_eq!(body.input.len(), 2);
    assert!(body.input.iter().all(|item| item["role"] == "user"));
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
                    r#"{"status":"completed","output":[{"type":"message","content":[{"text":"ok"}]}],"usage":{"input_tokens":10,"output_tokens":1,"total_tokens":11},"model":"gpt-test"}"#,
                )
            };
            let wire = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            socket.write_all(wire.as_bytes()).await.unwrap();
        }
    });

    let client = OpenAiResponsesAdapter::new(ModelEndpoint {
        base_url: format!("http://{addr}"),
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let messages = vec![
        CanonicalMessage::system(vec![ContentPart::text("stable system")]),
        CanonicalMessage::user_text("session input"),
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

#[tokio::test]
async fn stream_completed_output_recovers_text_before_tool_call() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
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

        let body = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"我\"}\n\n",
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"file\",\"arguments\":\"\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"fc_1\",\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"text\":\"我先读取文件\"}]},{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"file\",\"arguments\":\"{}\"}]}}\n\n",
        );
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });

    let client = OpenAiResponsesAdapter::new(ModelEndpoint {
        base_url: format!("http://{addr}"),
        model_name: "gpt-test".into(),
        ..Default::default()
    });
    let mut stream = client
        .chat_stream_with_tools(Vec::new(), Vec::new())
        .await
        .unwrap();
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.unwrap();
        if let Some(delta) = chunk.text {
            text.push_str(&delta);
        }
        tool_calls.extend(chunk.tool_calls);
    }

    assert_eq!(text, "我先读取文件");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].name, "file");
    server.await.unwrap();
}

#[test]
fn usage_parses_input_tokens_details_cached_tokens() {
    let json = r#"{"input_tokens":100,"output_tokens":5,"total_tokens":105,"input_tokens_details":{"cached_tokens":80}}"#;
    let usage: ResponsesUsage = serde_json::from_str(json).unwrap();
    assert_eq!(usage.cached(), 80);
}

#[test]
fn usage_parses_deepseek_prompt_cache_hit_tokens() {
    let json =
        r#"{"input_tokens":100,"output_tokens":5,"total_tokens":105,"prompt_cache_hit_tokens":70}"#;
    let usage: ResponsesUsage = serde_json::from_str(json).unwrap();
    assert_eq!(usage.cached(), 70);
}

#[test]
fn usage_fills_omitted_total_from_input_output() {
    let json = r#"{"input_tokens":40,"output_tokens":8}"#;
    let usage: ResponsesUsage = serde_json::from_str(json).unwrap();
    let canon = usage.to_usage(None);
    assert_eq!(canon.prompt_tokens, 40);
    assert_eq!(canon.completion_tokens, 8);
    assert_eq!(canon.total_tokens, 48);
}
