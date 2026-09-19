#[path = "tests/golden.rs"]
mod golden;

use super::*;
use crate::ToolFunction;

#[test]
fn build_headers_uses_goog_api_key_by_default() {
    let ep = ModelEndpoint {
        api_key: "AIza-test".into(),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    let headers = client.build_headers().unwrap();
    assert_eq!(
        headers.get("x-goog-api-key").unwrap().to_str().unwrap(),
        "AIza-test"
    );
    assert!(!headers.contains_key("authorization"));
}

#[test]
fn build_headers_respects_custom_auth_scheme() {
    let ep = ModelEndpoint {
        api_key: "key".into(),
        auth_header_name: "Authorization".into(),
        auth_header_prefix: "Bearer".into(),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    assert!(
        client
            .build_headers()
            .unwrap()
            .get("x-goog-api-key")
            .is_some()
    );

    let ep = ModelEndpoint {
        api_key: "key".into(),
        auth_header_name: "X-Gateway-Key".into(),
        auth_header_prefix: String::new(),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    assert!(
        client
            .build_headers()
            .unwrap()
            .get("x-gateway-key")
            .is_some()
    );
    assert!(
        client
            .build_headers()
            .unwrap()
            .get("x-goog-api-key")
            .is_none()
    );
}

#[test]
fn api_base_handles_v1beta_suffix() {
    let ep = ModelEndpoint {
        base_url: "https://generativelanguage.googleapis.com".into(),
        model_name: "gemini-2.5-flash".into(),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    assert_eq!(
        client.generate_url(),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
    );
    assert_eq!(
        client.stream_generate_url(),
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    );

    let ep = ModelEndpoint {
        base_url: "https://host/v1beta".into(),
        model_name: "gemini-2.5-flash".into(),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    assert_eq!(
        client.generate_url(),
        "https://host/v1beta/models/gemini-2.5-flash:generateContent"
    );
}

#[test]
fn embed_url_strips_models_prefix() {
    let ep = ModelEndpoint {
        base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
        model_name: "models/text-embedding-004".into(),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    assert_eq!(
        client.embed_url(),
        "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:batchEmbedContents"
    );
}

#[test]
fn parse_gemini_embed_response_batch_and_single() {
    let batch = r#"{"embeddings":[{"values":[0.1,0.2]},{"values":[0.3]}]}"#;
    let emb = parse_gemini_embed_response(batch, 2, "text-embedding-004").unwrap();
    assert_eq!(emb.vectors, vec![vec![0.1, 0.2], vec![0.3]]);

    let single = r#"{"embedding":{"values":[1.0,2.0]}}"#;
    let emb = parse_gemini_embed_response(single, 1, "m").unwrap();
    assert_eq!(emb.vectors, vec![vec![1.0, 2.0]]);
}

#[test]
fn convert_contents_extracts_system_instruction() {
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("be concise")],
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
    let (contents, system) = GeminiAdapter::convert_contents(msgs);
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].role, "user");
    assert_eq!(contents[0].parts[0].text.as_deref(), Some("hi"));
    let sys = system.unwrap();
    assert_eq!(sys["parts"][0]["text"], "be concise");
}

#[test]
fn convert_contents_separates_dynamic_system_context() {
    let system =
        format!("stable instructions{SESSION_CONTEXT_FENCE_START}Current session: inspect cache");
    let (_, system) =
        GeminiAdapter::convert_contents(vec![CanonicalMessage::system(vec![ContentPart::text(
            system,
        )])]);

    let system = system.unwrap();
    assert_eq!(system["parts"].as_array().unwrap().len(), 2);
    assert_eq!(system["parts"][0]["text"], "stable instructions");
    assert_eq!(
        system["parts"][1]["text"],
        format!("{SESSION_CONTEXT_FENCE_START}Current session: inspect cache")
    );
}

#[test]
fn convert_contents_tool_result_function_response() {
    // Without a preceding assistant declaration the call id is the only
    // name available; use it as the fallback.
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Tool,
        content: vec![ContentPart::text("result body")],
        tool_call_id: Some("call_1".into()),
        tool_calls: None,
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].role, "user");
    let fr = contents[0].parts[0].function_response.as_ref().unwrap();
    assert_eq!(fr["name"], "call_1");
    assert_eq!(fr["response"]["result"], "result body");
}

#[test]
fn convert_contents_tool_result_uses_function_name_of_matching_call() {
    // Gemini requires functionResponse.name to match the original
    // functionCall.name; the local call id (call_N) must never leak
    // into the response name. Build the assistant declaration first,
    // then the tool result referencing the same call id.
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("checking")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_0".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "/tmp"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("file contents")],
            tool_call_id: Some("call_0".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    assert_eq!(contents.len(), 2);
    let fr = contents[1].parts[0].function_response.as_ref().unwrap();
    assert_eq!(fr["name"], "read_file");
    assert_eq!(fr["response"]["result"], "file contents");
    // The assistant's functionCall keeps the function name (never the id).
    let fc = contents[0].parts[1].function_call.as_ref().unwrap();
    assert_eq!(fc["name"], "read_file");
}

#[test]
fn convert_contents_parallel_same_tool_results_follow_declaration_order() {
    // Two parallel calls to the SAME tool: Gemini pairs functionResponse
    // parts with functionCall parts by name + position, so results must
    // be emitted in DECLARATION order even when the canonical holds them
    // in completion order (c2 finished first).
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("doing")],
            tool_call_id: None,
            tool_calls: Some(vec![
                CanonicalToolCall {
                    id: "c1".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({"cmd": "echo one"}),
                },
                CanonicalToolCall {
                    id: "c2".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({"cmd": "echo two"}),
                },
            ]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("out-two")],
            tool_call_id: Some("c2".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("out-one")],
            tool_call_id: Some("c1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    assert_eq!(contents.len(), 3);
    // First result in the emitted stream belongs to c1 (the first
    // declared call), even though c2 completed first.
    let fr1 = contents[1].parts[0].function_response.as_ref().unwrap();
    assert_eq!(fr1["name"], "shell");
    assert_eq!(fr1["response"]["result"], "out-one");
    let fr2 = contents[2].parts[0].function_response.as_ref().unwrap();
    assert_eq!(fr2["response"]["result"], "out-two");
}

#[test]
fn convert_contents_multiple_tool_results_map_their_own_call_names() {
    let msgs = vec![
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("doing")],
            tool_call_id: None,
            tool_calls: Some(vec![
                CanonicalToolCall {
                    id: "c1".into(),
                    name: "shell".into(),
                    arguments: serde_json::json!({}),
                },
                CanonicalToolCall {
                    id: "c2".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({}),
                },
            ]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("out1")],
            tool_call_id: Some("c1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("out2")],
            tool_call_id: Some("c2".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        },
    ];
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    let fr1 = contents[1].parts[0].function_response.as_ref().unwrap();
    let fr2 = contents[2].parts[0].function_response.as_ref().unwrap();
    assert_eq!(fr1["name"], "shell");
    assert_eq!(fr2["name"], "ask");
}

#[test]
fn convert_contents_assistant_function_call() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("checking")],
        tool_call_id: None,
        tool_calls: Some(vec![CanonicalToolCall {
            id: "call_2".into(),
            name: "file".into(),
            arguments: serde_json::json!({"operation": "read"}),
        }]),
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
        source: None,
        id: None,
    }];
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].role, "model");
    let fc = contents[0].parts[1].function_call.as_ref().unwrap();
    assert_eq!(fc["name"], "file");
    assert_eq!(fc["args"]["operation"], "read");
}

#[test]
fn convert_contents_echoes_function_call_thought_signature() {
    let msgs = vec![CanonicalMessage {
        role: CanonicalRole::Assistant,
        content: vec![ContentPart::text("checking")],
        tool_call_id: None,
        tool_calls: Some(vec![CanonicalToolCall {
            id: "fc_1".into(),
            name: "file".into(),
            arguments: json!({"operation": "read"}),
        }]),
        reasoning: None,
        web_search_calls: Vec::new(),
        thinking_blocks: vec![json!({
            "type": "gemini_thought_signature",
            "part_type": "function_call",
            "name": "file",
            "signature": "sig_1"
        })],
        source: None,
        id: None,
    }];
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    let wire = serde_json::to_value(&contents[0]).unwrap();
    assert_eq!(wire["parts"][1]["functionCall"]["id"], "fc_1");
    assert_eq!(wire["parts"][1]["thoughtSignature"], "sig_1");
}

#[test]
fn convert_contents_image_and_audio_inline_data() {
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
    let (contents, _) = GeminiAdapter::convert_contents(msgs);
    let inline = contents[0].parts[0].inline_data.as_ref().unwrap();
    assert_eq!(inline["mimeType"], "image/png");
    assert_eq!(inline["data"], "aGVsbG8=");
    let inline = contents[0].parts[1].inline_data.as_ref().unwrap();
    assert_eq!(inline["mimeType"], "audio/wav");
    assert_eq!(inline["data"], "d3d3");
}

#[test]
fn capability_profile_image_reaches_gemini_wire_payload() {
    let client = GeminiAdapter::new(ModelEndpoint::default());
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
    assert_eq!(
        wire["contents"][0]["parts"][0]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(
        wire["contents"][0]["parts"][0]["inlineData"]["data"],
        "aGVsbG8="
    );
}

#[test]
fn parse_response_thought_parts_route_to_reasoning_not_text() {
    // Gemini 2.5 thinking mode returns `"thought": true` parts. They must
    // never leak into the visible assistant text; they surface as reasoning.
    let json = GeminiResponse {
        candidates: Some(vec![GeminiCandidate {
            content: Some(GeminiResponseContent {
                parts: vec![
                    GeminiResponsePart {
                        text: Some("I should read the file first.".into()),
                        function_call: None,
                        thought: Some(true),
                        thought_signature: None,
                    },
                    GeminiResponsePart {
                        text: Some("Final answer.".into()),
                        function_call: None,
                        thought: Some(false),
                        thought_signature: None,
                    },
                ],
            }),
            finish_reason: Some("STOP".into()),
            grounding_metadata: None,
        }]),
        usage_metadata: None,
        model_version: None,
    };
    let client = GeminiAdapter::new(ModelEndpoint::default());
    let resp = client.parse_response(json, None).unwrap();
    assert_eq!(resp.text, "Final answer.");
    assert_eq!(
        resp.reasoning.as_deref(),
        Some("I should read the file first.")
    );
}

#[test]
fn build_request_body_with_tools_and_config() {
    let ep = ModelEndpoint {
        model_name: "gemini-2.5-flash".into(),
        max_tokens: 2048,
        temperature: 0.2,
        top_p: Some(0.9),
        top_k: Some(40),
        ..Default::default()
    };
    let client = GeminiAdapter::new(ep);
    let tools = vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "search".into(),
            description: "search the web".into(),
            parameters: serde_json::json!({"type": "object"}),
        },
    }];
    let body = client.build_request_body(vec![], tools, false);
    let gtools = body.tools.unwrap();
    match &gtools[0] {
        GeminiTool::Functions {
            function_declarations,
        } => assert_eq!(function_declarations[0].name, "search"),
        GeminiTool::GoogleSearch { .. } => panic!("expected function tool"),
    }
    let cfg = body.generation_config.unwrap();
    assert_eq!(cfg.max_output_tokens, 2048);
    assert_eq!(cfg.temperature, 0.2);
    assert_eq!(cfg.top_p, Some(0.9));
    assert_eq!(cfg.top_k, Some(40));
}

#[test]
fn convert_tools_projects_gemini_schema_subset() {
    let tools = GeminiAdapter::convert_tools(vec![ToolDefinition {
        tool_type: "function".into(),
        function: ToolFunction {
            name: "schedule".into(),
            description: "schedule an action".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": { "type": "string", "enum": ["list", "set"] },
                    "delay_secs": { "type": "integer", "minimum": 1 }
                },
                "oneOf": [
                    {
                        "type": "object",
                        "properties": { "operation": { "const": "list" } }
                    },
                    {
                        "type": "object",
                        "properties": { "operation": { "const": "set" } }
                    }
                ]
            }),
        },
    }]);

    let GeminiTool::Functions {
        function_declarations,
    } = &tools[0]
    else {
        panic!("expected function declaration");
    };
    let parameters = &function_declarations[0].parameters;
    assert_eq!(parameters["type"], "object");
    assert!(parameters.get("oneOf").is_none());
    assert_eq!(
        parameters["properties"]["operation"]["enum"],
        json!(["list", "set"])
    );
    assert!(
        parameters["properties"]["delay_secs"]
            .get("minimum")
            .is_none()
    );
}

#[test]
fn google_search_tool_injected_when_web_search_on() {
    let client = GeminiAdapter::new(ModelEndpoint::default());
    let body = client.build_request_body_with_mode(vec![], vec![], WebSearchMode::Auto);
    let tools = body.tools.expect("google_search present");
    assert!(matches!(tools[0], GeminiTool::GoogleSearch { .. }));
    let off = client.build_request_body_with_mode(vec![], vec![], WebSearchMode::Off);
    assert!(off.tools.is_none());
}

#[test]
fn grounding_metadata_keeps_queries_only() {
    let raw = json!({
        "candidates": [{
            "groundingMetadata": {
                "webSearchQueries": ["haven voice"],
                "groundingChunks": [{"huge": "blob".repeat(100)}],
            }
        }]
    });
    let calls = GeminiAdapter::web_search_calls_from_grounding(&raw);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["action"]["queries"], json!(["haven voice"]));
    assert!(calls[0]["action"].get("grounding_metadata").is_none());
}

#[test]
fn finish_reason_mapping() {
    assert_eq!(
        GeminiAdapter::finish_reason_of("STOP"),
        Some(FinishReason::Stop)
    );
    assert_eq!(
        GeminiAdapter::finish_reason_of("MAX_TOKENS"),
        Some(FinishReason::Length)
    );
    assert_eq!(
        GeminiAdapter::finish_reason_of("SAFETY"),
        Some(FinishReason::ContentFilter)
    );
    assert_eq!(
        GeminiAdapter::finish_reason_of("RECITATION"),
        Some(FinishReason::ContentFilter)
    );
    assert_eq!(
        GeminiAdapter::finish_reason_of("MALFORMED_FUNCTION_CALL"),
        Some(FinishReason::ToolCalls)
    );
}

#[test]
fn parse_response_text_tool_call_usage() {
    let json = GeminiResponse {
        candidates: Some(vec![GeminiCandidate {
            content: Some(GeminiResponseContent {
                parts: vec![
                    GeminiResponsePart {
                        text: Some("checking".into()),
                        function_call: None,
                        thought: None,
                        thought_signature: None,
                    },
                    GeminiResponsePart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            id: None,
                            name: Some("file".into()),
                            args: Some(json!({"operation": "read"})),
                        }),
                        thought: None,
                        thought_signature: None,
                    },
                ],
            }),
            finish_reason: Some("STOP".into()),
            grounding_metadata: None,
        }]),
        usage_metadata: Some(GeminiUsage {
            prompt_tokens: 10,
            candidates_tokens: 5,
            total_tokens: 15,
            ..Default::default()
        }),
        model_version: Some("gemini-2.5-flash".into()),
    };
    let ep = ModelEndpoint::default();
    let client = GeminiAdapter::new(ep);
    let resp = client
        .parse_response(json, Some("gemini-2.5-flash".into()))
        .unwrap();
    assert_eq!(resp.text, "checking");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "file");
    assert_eq!(resp.tool_calls[0].arguments["operation"], "read");
    assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
    assert_eq!(resp.usage.total_tokens, 15);
    assert_eq!(resp.model.as_deref(), Some("gemini-2.5-flash"));
}

#[test]
fn parse_response_captures_official_function_call_signature_and_id() {
    let raw = json!({
        "candidates": [{
            "content": {"parts": [{
                "functionCall": {
                    "id": "fc_1",
                    "name": "file",
                    "args": {"operation": "read"}
                },
                "thoughtSignature": "sig_1"
            }]},
            "finishReason": "STOP"
        }],
        "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5},
        "modelVersion": "gemini-3.7-flash"
    });
    let response: GeminiResponse = serde_json::from_value(raw).unwrap();
    let client = GeminiAdapter::new(ModelEndpoint::default());
    let parsed = client
        .parse_response(response, Some("gemini-3.7-flash".into()))
        .unwrap();
    assert_eq!(parsed.tool_calls[0].id, "fc_1");
    assert_eq!(parsed.thinking_blocks[0]["signature"], "sig_1");
    assert_eq!(parsed.model.as_deref(), Some("gemini-3.7-flash"));
}

#[test]
fn serialized_request_uses_gemini_rest_wire_names() {
    let client = GeminiAdapter::new(ModelEndpoint {
        model_name: "gemini-3.7-flash".into(),
        ..Default::default()
    });
    let body = client.build_request_body(
        vec![CanonicalMessage::user_text("hello")],
        vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "file".into(),
                description: "read a file".into(),
                parameters: json!({"type": "object"}),
            },
        }],
        false,
    );
    let wire = serde_json::to_value(body).unwrap();
    assert!(wire["generationConfig"]["maxOutputTokens"].is_number());
    assert!(wire["generationConfig"].get("max_output_tokens").is_none());
    assert!(wire["tools"][0]["functionDeclarations"].is_array());
    assert!(wire["tools"][0].get("function_declarations").is_none());
    assert!(wire["systemInstruction"].is_null());
}

#[test]
fn generate_url_strips_models_prefix() {
    let client = GeminiAdapter::new(ModelEndpoint {
        model_name: "models/gemini-3.7-flash".into(),
        ..Default::default()
    });
    assert!(
        client
            .generate_url()
            .ends_with("/models/gemini-3.7-flash:generateContent")
    );
}

#[test]
fn usage_folds_thoughts_and_tool_use_into_counts() {
    let u = GeminiUsage {
        prompt_tokens: 100,
        candidates_tokens: 20,
        thoughts_tokens: 80,
        tool_use_prompt_tokens: 15,
        total_tokens: 215,
        cached_tokens: 40,
    };
    let usage = u.to_usage(None);
    assert_eq!(usage.prompt_tokens, 115);
    assert_eq!(usage.completion_tokens, 100);
    assert_eq!(usage.total_tokens, 215);
    assert_eq!(usage.cached_tokens, 40);
    assert!(!usage.cache_exclusive_of_prompt());
    assert_eq!(usage.context_tokens(), 115);
}
