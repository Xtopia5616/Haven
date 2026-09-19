use super::*;

impl GeminiAdapter {
    pub(super) const THOUGHT_SIGNATURE_TYPE: &'static str = "gemini_thought_signature";

    pub(super) fn thought_signature_marker(
        part_type: &str,
        name: Option<&str>,
        signature: &str,
    ) -> Value {
        let mut marker = json!({
            "type": Self::THOUGHT_SIGNATURE_TYPE,
            "part_type": part_type,
            "signature": signature,
        });
        if let Some(name) = name {
            marker["name"] = json!(name);
        }
        marker
    }

    pub(super) fn take_thought_signature(
        blocks: &[Value],
        used: &mut Vec<usize>,
        part_type: &str,
        name: Option<&str>,
    ) -> Option<String> {
        let index = blocks.iter().enumerate().find_map(|(idx, block)| {
            if used.contains(&idx)
                || block.get("type").and_then(Value::as_str) != Some(Self::THOUGHT_SIGNATURE_TYPE)
                || block.get("part_type").and_then(Value::as_str) != Some(part_type)
            {
                return None;
            }
            let marker_name = block.get("name").and_then(Value::as_str);
            if name.is_some() && marker_name != name {
                return None;
            }
            block.get("signature").and_then(Value::as_str).map(|_| idx)
        })?;
        used.push(index);
        blocks[index]
            .get("signature")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    pub(super) fn capture_thought_signature(
        blocks: &mut Vec<Value>,
        part: &GeminiResponsePart,
        function_name: Option<&str>,
    ) {
        if let Some(signature) = part.thought_signature.as_deref() {
            let part_type = if function_name.is_some() {
                "function_call"
            } else {
                "content"
            };
            let marker = Self::thought_signature_marker(part_type, function_name, signature);
            if !blocks.iter().any(|existing| existing == &marker) {
                blocks.push(marker);
            }
        }
    }

    pub(super) fn convert_contents(
        msgs: Vec<CanonicalMessage>,
    ) -> (Vec<GeminiContent>, Option<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<GeminiContent> = Vec::new();
        // Gemini's `functionResponse.name` must match the `functionCall.name`
        // of the original call (call ids are generated locally and never sent
        // to the API). Track the id -> function name mapping from assistant
        // tool calls so tool results reference the function name.
        let mut call_id_to_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Call ids in the order the latest assistant DECLARED them. Gemini
        // pairs each `functionResponse` with the `functionCall` of the same
        // name by position, so parallel calls to the SAME tool must have
        // their results emitted in declaration order — the canonical holds
        // them in completion order, which could swap them.
        let mut declared_order: Vec<String> = Vec::new();
        // Consecutive tool results buffered until the next non-tool message
        // (or end of input), then flushed in declaration order.
        let mut pending_tool_results: Vec<(String, String)> = Vec::new();
        for mut m in msgs {
            match m.role {
                CanonicalRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            system_parts.push(t.clone());
                        }
                    }
                }
                CanonicalRole::User | CanonicalRole::Tool => {
                    let is_tool_result =
                        matches!(m.role, CanonicalRole::Tool) || m.tool_call_id.is_some();
                    if is_tool_result {
                        let call_id = m.tool_call_id.unwrap_or_default();
                        let text = Self::text_content(&m.content);
                        pending_tool_results.push((call_id, text));
                    } else {
                        Self::flush_pending_tool_results(
                            &mut out,
                            &mut pending_tool_results,
                            &declared_order,
                            &call_id_to_name,
                        );
                        if m.role == CanonicalRole::User {
                            m.content =
                                crate::adapters::apply_wire_inject_prefix(m.source, m.content);
                        }
                        let parts = Self::content_to_parts(&m.content);
                        if parts.is_empty() {
                            continue;
                        }
                        out.push(GeminiContent {
                            role: "user".into(),
                            parts,
                        });
                    }
                }
                CanonicalRole::Assistant => {
                    Self::flush_pending_tool_results(
                        &mut out,
                        &mut pending_tool_results,
                        &declared_order,
                        &call_id_to_name,
                    );
                    let mut parts = Self::content_to_parts(&m.content);
                    let mut used_signatures = Vec::new();
                    if let Some(signature) = Self::take_thought_signature(
                        &m.thinking_blocks,
                        &mut used_signatures,
                        "content",
                        None,
                    ) && let Some(part) = parts.last_mut()
                    {
                        part.thought_signature = Some(signature);
                    }
                    if let Some(calls) = &m.tool_calls {
                        declared_order.clear();
                        for tc in calls {
                            call_id_to_name.insert(tc.id.clone(), tc.name.clone());
                            declared_order.push(tc.id.clone());
                            let thought_signature = Self::take_thought_signature(
                                &m.thinking_blocks,
                                &mut used_signatures,
                                "function_call",
                                Some(&tc.name),
                            );
                            parts.push(GeminiPart {
                                text: None,
                                inline_data: None,
                                function_call: Some(json!({
                                    "id": tc.id,
                                    "name": tc.name,
                                    "args": tc.arguments
                                })),
                                function_response: None,
                                thought_signature,
                            });
                        }
                    }
                    if parts.is_empty() {
                        continue;
                    }
                    out.push(GeminiContent {
                        role: "model".into(),
                        parts,
                    });
                }
            }
        }
        Self::flush_pending_tool_results(
            &mut out,
            &mut pending_tool_results,
            &declared_order,
            &call_id_to_name,
        );
        let system = if system_parts.is_empty() {
            None
        } else {
            let text = system_parts.join("\n\n");
            let parts = if let Some((stable, session, memory)) =
                split_system_prompt_cache_sections(&text)
            {
                let dynamic = format!("{session}{memory}");
                vec![json!({"text": stable}), json!({"text": dynamic})]
            } else {
                vec![json!({"text": text})]
            };
            Some(json!({"parts": parts}))
        };
        (out, system)
    }

    pub(super) fn flush_pending_tool_results(
        out: &mut Vec<GeminiContent>,
        pending: &mut Vec<(String, String)>,
        declared_order: &[String],
        call_id_to_name: &std::collections::HashMap<String, String>,
    ) {
        if pending.is_empty() {
            return;
        }
        let mut items: Vec<(usize, (String, String))> = pending
            .drain(..)
            .map(|(call_id, text)| {
                let pos = declared_order
                    .iter()
                    .position(|id| *id == call_id)
                    .unwrap_or(usize::MAX);
                (pos, (call_id, text))
            })
            .collect();
        // Stable sort: unknown call ids keep their arrival order at the end.
        items.sort_by_key(|(pos, _)| *pos);
        for (_, (call_id, text)) in items {
            let name = call_id_to_name
                .get(&call_id)
                .cloned()
                .unwrap_or_else(|| call_id.clone());
            out.push(GeminiContent {
                role: "user".into(),
                parts: vec![GeminiPart {
                    text: None,
                    inline_data: None,
                    function_call: None,
                    function_response: Some({
                        let mut response = json!({
                            "name": name,
                            "response": {"result": text}
                        });
                        if !call_id.is_empty() {
                            response["id"] = json!(call_id);
                        }
                        response
                    }),
                    thought_signature: None,
                }],
            });
        }
    }

    pub(super) fn text_content(parts: &[ContentPart]) -> String {
        parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn content_to_parts(parts: &[ContentPart]) -> Vec<GeminiPart> {
        parts
            .iter()
            .map(|p| match p {
                ContentPart::Text(t) => GeminiPart {
                    text: Some(t.clone()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                },
                ContentPart::Image {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mimeType": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                },
                ContentPart::Audio {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mimeType": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                },
                ContentPart::Video {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mimeType": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                },
            })
            .collect()
    }

    pub(super) fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<GeminiTool> {
        tools
            .into_iter()
            .map(|t| GeminiTool::Functions {
                function_declarations: vec![GeminiFunctionDeclaration {
                    name: t.function.name,
                    description: t.function.description,
                    parameters: crate::types::project_tool_parameters_for_gemini(
                        t.function.parameters,
                    ),
                }],
            })
            .collect()
    }
}
