use super::*;

impl OpenAiResponsesAdapter {
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

    pub(super) fn content_to_parts(parts: &[ContentPart]) -> Vec<Value> {
        parts
            .iter()
            .map(|p| match p {
                ContentPart::Text(t) => json!({"type": "input_text", "text": t}),
                ContentPart::Image {
                    media_type, data, ..
                } => json!({
                    "type": "input_image",
                    "image_url": format!("data:{};base64,{}", media_type, data)
                }),
                ContentPart::Audio {
                    media_type, data, ..
                } => json!({
                    "type": "input_audio",
                    "input_audio": {
                        "format": media_type.rsplit('/').next().unwrap_or("wav"),
                        "data": data
                    }
                }),
                ContentPart::Video { .. } => json!({
                    "type": "input_text",
                    "text": "[Haven: video input is not supported by the configured OpenAI Responses wire]"
                }),
            })
            .collect()
    }

    pub(super) fn convert_input(
        msgs: Vec<CanonicalMessage>,
        max_reasoning_echo_chars: usize,
        requires_reasoning_echo: bool,
    ) -> (Vec<Value>, Option<String>) {
        Self::convert_input_with_memory_split(
            msgs,
            max_reasoning_echo_chars,
            requires_reasoning_echo,
            true,
        )
    }

    pub(super) fn convert_input_with_memory_split(
        msgs: Vec<CanonicalMessage>,
        max_reasoning_echo_chars: usize,
        requires_reasoning_echo: bool,
        split_memory: bool,
    ) -> (Vec<Value>, Option<String>) {
        let mut instructions: Vec<String> = Vec::new();
        let mut session_context: Vec<String> = Vec::new();
        let mut volatile_system: Vec<String> = Vec::new();
        let mut items: Vec<Value> = Vec::new();
        for m in msgs {
            match m.role {
                CanonicalRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            if split_memory
                                && let Some((stable, session, memory)) =
                                    split_system_prompt_cache_sections(t)
                            {
                                if !stable.is_empty() {
                                    instructions.push(stable.to_string());
                                }
                                if !session.is_empty() {
                                    session_context.push(session.to_string());
                                }
                                if !memory.is_empty() {
                                    volatile_system.push(memory.to_string());
                                }
                            } else {
                                instructions.push(t.clone());
                            }
                        }
                    }
                }
                CanonicalRole::User => {
                    let content = Self::content_to_parts(
                        &crate::adapters::apply_wire_inject_prefix(m.source, m.content),
                    );
                    if !content.is_empty() {
                        items.push(json!({"role": "user", "content": content}));
                    }
                }
                CanonicalRole::Assistant => {
                    let text = Self::text_content(&m.content);
                    // DeepSeek's thinking-mode Responses compat layer REQUIRES
                    // the reasoning_text of previous assistant turns to be
                    // passed back whenever the input carries tool-call history;
                    // omitting it returns 400 ("The `reasoning_text` in the
                    // thinking mode must be passed back to the API.") or — on
                    // the streaming path — a silent empty/truncated stream.
                    // `reasoning` was persisted on the assistant message for
                    // exactly this purpose. Anthropic messages carry the text
                    // only as raw `thinking_blocks` (the agent drops the
                    // redundant `reasoning` copy), so it is reconstructed
                    // before the echo.
                    let reasoning = m
                        .reasoning
                        .as_deref()
                        .map(str::trim)
                        .filter(|r| !r.is_empty())
                        .map(str::to_string)
                        .or_else(|| {
                            let t = reasoning_text_from_thinking_blocks(&m.thinking_blocks);
                            let t = t.trim();
                            (!t.is_empty()).then(|| t.to_string())
                        });
                    //
                    // The echo is capped: full reasoning (10k+ chars per turn
                    // is routine) makes the request body balloon to 150-200KB,
                    // and providers then stall/truncate the stream mid-inference
                    // (observed as repeated empty responses on large contexts).
                    // Keeping the TAIL of each turn's reasoning preserves the
                    // conclusions while bounding the request; the provider
                    // validates presence, not length.
                    //
                    // The reasoning item comes FIRST in the turn, before the
                    // assistant message, matching the order DeepSeek's own
                    // response output emits a turn (reasoning → message →
                    // function_call). The text goes into an OpenAI-style
                    // content-part array (`[{"type":"reasoning_text","text":…}]`):
                    // DeepSeek's compat layer deserializes the `content` of a
                    // `reasoning` input item as a SEQUENCE of parts — a plain
                    // string 400s with "invalid type: string, expected a
                    // sequence" (verified against the live API).
                    //
                    // The echo is emitted only for endpoints that demand it
                    // (DeepSeek/Kimi/MiMo): the array form is
                    // compat-layer-specific, and other providers (official
                    // OpenAI Responses) neither require a reasoning input item
                    // nor accept this shape.
                    if requires_reasoning_echo {
                        if let Some(r) = reasoning {
                            items.push(json!({
                                "type": "reasoning",
                                "content": [{
                                    "type": "reasoning_text",
                                    "text": reasoning_tail(r, max_reasoning_echo_chars)
                                }]
                            }));
                        } else if m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
                            || !m.web_search_calls.is_empty()
                        {
                            // Best-effort presence echo for a tool-call /
                            // web-search turn on which the model produced no
                            // reasoning_text (it may skip thinking for a turn).
                            // NOTE (live-API verified): an empty reasoning item
                            // does NOT satisfy DeepSeek — it still 400s with
                            // "The `reasoning_text` in the thinking mode must be
                            // passed back" — so this injection only avoids a
                            // missing-item shape; a truly reasoning-less tool
                            // turn cannot be round-tripped and is the provider's
                            // constraint, not ours. Shape stays the array form
                            // (`reasoning_text` parts), never a plain string.
                            items.push(json!({
                                "type": "reasoning",
                                "content": [{"type": "reasoning_text", "text": ""}]
                            }));
                        }
                    }
                    if !text.is_empty() {
                        items.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        }));
                    }
                    // `web_search_call` items are passed back verbatim: the
                    // server restores the search context from them. Never
                    // parsed or rewritten (deepseek docs: 原样回传) — except
                    // that the `action` discriminator is filled when the
                    // captured skeleton lacks it (DeepSeek rejects an
                    // `action`-less item with a 400).
                    for ws in &m.web_search_calls {
                        items.push(normalize_web_search_call_item(ws.clone()));
                    }
                    if let Some(calls) = m.tool_calls {
                        for tc in calls {
                            items.push(json!({
                                "type": "function_call",
                                "call_id": tc.id,
                                "name": tc.name,
                                "arguments": tc.args_to_wire()
                            }));
                        }
                    }
                }
                CanonicalRole::Tool => {
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": m.tool_call_id.unwrap_or_default(),
                        "output": Self::text_content(&m.content)
                    }));
                }
            }
        }
        if !session_context.is_empty() {
            // Session context is stable for a ReAct run, so keep it before the
            // transcript and preserve its developer-level priority.
            items.insert(
                0,
                json!({
                    "role": "developer",
                    "content": [{
                        "type": "input_text",
                        "text": session_context.join("\n\n")
                    }]
                }),
            );
        }
        if !volatile_system.is_empty() {
            // Refreshed MEMORY belongs after the reusable conversation prefix.
            // Responses accepts developer input items in the input sequence,
            // preserving system-level priority without moving instructions.
            items.push(json!({
                "role": "developer",
                "content": [{
                    "type": "input_text",
                    "text": volatile_system.join("\n\n")
                }]
            }));
        }
        let instructions = if instructions.is_empty() {
            None
        } else {
            Some(instructions.join("\n\n"))
        };
        (items, instructions)
    }

    pub(super) fn merge_developer_memory_into_instructions(body: &mut ResponsesRequest) -> bool {
        let mut developer_text = Vec::new();
        body.input.retain(|item| {
            if item.get("role").and_then(Value::as_str) != Some("developer") {
                return true;
            }
            if let Some(text) = item.pointer("/content/0/text").and_then(Value::as_str) {
                developer_text.push(text.to_string());
            }
            false
        });
        if developer_text.is_empty() {
            return false;
        }
        body.instructions
            .get_or_insert_with(String::new)
            .push_str(&developer_text.join("\n\n"));
        true
    }

    pub(super) fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<Value> {
        tools
            .into_iter()
            .map(|t| {
                // Defense in depth: `ToolDefinition::from` already sanitizes,
                // but direct constructors / cache hits may still carry Null.
                let parameters = crate::types::canonicalize_json(
                    crate::types::project_tool_parameters_for_object_root(
                        crate::types::sanitize_tool_parameters(t.function.parameters),
                    ),
                );
                serde_json::to_value(ResponsesTool {
                    tool_type: t.tool_type,
                    name: Some(t.function.name),
                    description: Some(t.function.description),
                    parameters: Some(parameters),
                    strict: Some(false),
                })
                .unwrap_or_default()
            })
            .collect()
    }
}
pub(super) fn web_search_action_by_id(calls: &[Value], call_id: &str) -> Option<String> {
    calls.iter().find_map(|c| {
        if c.get("id").and_then(Value::as_str) == Some(call_id) {
            c.get("action")
                .and_then(|a| a.get("type"))
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

pub(super) fn web_search_action_of(item: &ResponsesItem) -> Option<String> {
    item.extra
        .get("action")
        .and_then(|a| a.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string)
}
