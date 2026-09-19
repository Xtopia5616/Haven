use super::*;

impl OpenAiAdapter {
    #[cfg(test)]
    pub(super) fn convert_messages(
        msgs: Vec<CanonicalMessage>,
        requires_reasoning_echo: bool,
        reasoning_echo_max_chars: usize,
    ) -> Vec<OpenAiMessage> {
        Self::convert_messages_ref(&msgs, requires_reasoning_echo, reasoning_echo_max_chars)
    }

    pub(super) fn convert_messages_ref(
        msgs: &[CanonicalMessage],
        requires_reasoning_echo: bool,
        reasoning_echo_max_chars: usize,
    ) -> Vec<OpenAiMessage> {
        msgs.iter()
            .map(|m| {
                let content_parts = if m.role == CanonicalRole::User {
                    crate::adapters::apply_wire_inject_prefix(m.source, m.content.clone())
                } else {
                    m.content.clone()
                };
                // When the assistant message carries tool_calls, the content
                // should be null (OpenAI API requirement).
                let has_tool_calls = m.tool_calls.is_some();
                let content = if content_parts.is_empty() || has_tool_calls {
                    None
                } else if content_parts.len() == 1 {
                    match &content_parts[0] {
                        ContentPart::Text(t) => Some(serde_json::Value::String(t.clone())),
                        ContentPart::Image {
                            media_type, data, ..
                        } => Some(serde_json::json!([{
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", media_type, data)
                            }
                        }])),
                        ContentPart::Audio {
                            media_type, data, ..
                        } => Some(serde_json::json!([{
                            "type": "input_audio",
                            "input_audio": {
                                "format": media_type.rsplit('/').next().unwrap_or("wav"),
                                "data": data
                            }
                        }])),
                        ContentPart::Video { .. } => Some(serde_json::json!("[Haven: video input is not supported by the configured OpenAI-compatible chat wire]")),
                    }
                } else {
                    let parts: Vec<serde_json::Value> = content_parts
                        .iter()
                        .map(|cp| match cp {
                            ContentPart::Text(t) => {
                                serde_json::json!({"type": "text", "text": t})
                            }
                            ContentPart::Image {
                                media_type, data, ..
                            } => serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data)
                                }
                            }),
                            ContentPart::Audio {
                                media_type, data, ..
                            } => serde_json::json!({
                                "type": "input_audio",
                                "input_audio": {
                                    "format": media_type.rsplit('/').next().unwrap_or("wav"),
                                    "data": data
                                }
                            }),
                            ContentPart::Video { .. } => serde_json::json!({
                                "type": "text",
                                "text": "[Haven: video input is not supported by the configured OpenAI-compatible chat wire]"
                            }),
                        })
                        .collect();
                    Some(serde_json::Value::Array(parts))
                };
                // Anthropic messages carry the thinking text only as raw
                // `thinking_blocks` (the agent drops the redundant `reasoning`
                // copy); reconstruct it so the reasoning echo still applies.
                let reasoning = m.reasoning.clone().or_else(|| {
                    let t = reasoning_text_from_thinking_blocks(&m.thinking_blocks);
                    (!t.is_empty()).then_some(t)
                });
                // Cap the echo to its tail (the conclusions), mirroring the
                // Responses adapter: unbounded reasoning (10k+ chars per turn)
                // balloons the request body and stalls/truncates the provider's
                // stream mid-inference. The provider validates presence, not
                // length, so the trimmed tail round-trips fine.
                let reasoning = reasoning.map(|r| reasoning_tail(r, reasoning_echo_max_chars));
                // DeepSeek thinking mode validates PRESENCE of
                // `reasoning_content`, not its content: a tool-call / web-search
                // turn on which the model skipped thinking must still carry the
                // field (empty is accepted) or the next request 400s.
                let requires_reasoning_pad = requires_reasoning_echo
                    && reasoning.as_ref().is_none_or(|r| r.trim().is_empty())
                    && (m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
                        || !m.web_search_calls.is_empty());
                let tool_calls = m.tool_calls.as_ref().map(|calls| {
                    calls
                        .iter()
                        .map(|tc| {
                            let args = tc.args_to_wire();
                            OpenAiMessageToolCall {
                                id: tc.id.clone(),
                                call_type: "function".into(),
                                function: OpenAiMessageToolFunction {
                                    name: tc.name.clone(),
                                    arguments: args,
                                },
                            }
                        })
                        .collect()
                });
                OpenAiMessage {
                    role: match m.role {
                        CanonicalRole::System => "system".to_string(),
                        CanonicalRole::User => "user".to_string(),
                        CanonicalRole::Assistant => "assistant".to_string(),
                        CanonicalRole::Tool => "tool".to_string(),
                    },
                    content,
                    tool_call_id: m.tool_call_id.clone(),
                    tool_calls,
                    reasoning_content: if requires_reasoning_pad {
                        Some(String::new())
                    } else {
                        reasoning
                    },
                    // `web_search_call` items are echoed back for the
                    // stateless chat API to restore the search context, with
                    // the `action` discriminator filled when the captured
                    // skeleton lacks it (DeepSeek 400s otherwise).
                    // Skip synthetic xAI citation markers — Live Search is
                    // driven by `search_parameters`, not call round-trip.
                    web_search_call: m
                        .web_search_calls
                        .iter()
                        .filter(|c| c.get("id").and_then(Value::as_str) != Some("xai_citations"))
                        .cloned()
                        .map(normalize_web_search_call_item)
                        .collect(),
                }
            })
            .collect()
    }

    pub(super) fn convert_messages_with_system_split(
        msgs: &[CanonicalMessage],
        requires_reasoning_echo: bool,
        reasoning_echo_max_chars: usize,
    ) -> (Vec<OpenAiMessage>, bool) {
        let Some(index) = msgs
            .iter()
            .position(|message| message.role == CanonicalRole::System)
        else {
            return (
                Self::convert_messages_ref(msgs, requires_reasoning_echo, reasoning_echo_max_chars),
                false,
            );
        };
        let Some(ContentPart::Text(text)) = msgs[index].content.first() else {
            return (
                Self::convert_messages_ref(msgs, requires_reasoning_echo, reasoning_echo_max_chars),
                false,
            );
        };
        let Some((stable, session, memory)) = split_system_prompt_cache_sections(text) else {
            return (
                Self::convert_messages_ref(msgs, requires_reasoning_echo, reasoning_echo_max_chars),
                false,
            );
        };
        if stable.trim().is_empty() || (session.is_empty() && memory.is_empty()) {
            return (
                Self::convert_messages_ref(msgs, requires_reasoning_echo, reasoning_echo_max_chars),
                false,
            );
        }

        let stable_message = CanonicalMessage::system(vec![ContentPart::text(stable)]);
        let session_message = (!session.is_empty())
            .then(|| CanonicalMessage::system(vec![ContentPart::text(session)]));
        let memory_message = (!memory.is_empty()).then(|| CanonicalMessage::user_text(memory));
        let mut out = Vec::with_capacity(msgs.len() + 2);
        for (message_index, message) in msgs.iter().enumerate() {
            if message_index == index {
                out.extend(Self::convert_messages_ref(
                    std::slice::from_ref(&stable_message),
                    requires_reasoning_echo,
                    reasoning_echo_max_chars,
                ));
                if let Some(session_message) = &session_message {
                    out.extend(Self::convert_messages_ref(
                        std::slice::from_ref(session_message),
                        requires_reasoning_echo,
                        reasoning_echo_max_chars,
                    ));
                }
            } else {
                out.extend(Self::convert_messages_ref(
                    std::slice::from_ref(message),
                    requires_reasoning_echo,
                    reasoning_echo_max_chars,
                ));
            }
        }
        if let Some(memory_message) = &memory_message {
            out.extend(Self::convert_messages_ref(
                std::slice::from_ref(memory_message),
                requires_reasoning_echo,
                reasoning_echo_max_chars,
            ));
        }
        (out, true)
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(super) fn split_system_memory(
        mut messages: Vec<CanonicalMessage>,
    ) -> (Vec<CanonicalMessage>, bool) {
        let Some(index) = messages
            .iter()
            .position(|message| message.role == CanonicalRole::System)
        else {
            return (messages, false);
        };
        let Some(ContentPart::Text(text)) = messages[index].content.first() else {
            return (messages, false);
        };
        let Some((stable, session, memory)) = split_system_prompt_cache_sections(text) else {
            return (messages, false);
        };
        if stable.trim().is_empty() || (session.is_empty() && memory.is_empty()) {
            return (messages, false);
        }
        let stable = stable.to_string();
        let session = session.to_string();
        let memory = memory.to_string();
        messages[index].content = vec![ContentPart::text(stable)];
        if !session.is_empty() {
            messages.insert(
                index + 1,
                CanonicalMessage::system(vec![ContentPart::text(session)]),
            );
        }
        if !memory.is_empty() {
            // OpenAI-compatible chat endpoints require system messages to
            // lead the conversation. A trailing user context item is the
            // compatible position for refreshable, explicitly quoted data.
            messages.push(CanonicalMessage::user_text(memory));
        }
        (messages, true)
    }

    pub(super) fn extract_tool_calls(choice: &OpenAiChoice) -> Vec<CanonicalToolCall> {
        let mut out = Vec::new();
        if let Some(msg) = choice.message.as_ref().or(choice.delta.as_ref())
            && let Some(calls) = &msg.tool_calls
        {
            for c in calls {
                let name = c.function.name.clone().unwrap_or_default();
                let args = c.function.arguments.clone().unwrap_or_default();
                let id = c.id.clone().unwrap_or_default();
                if !name.is_empty() {
                    out.push(CanonicalToolCall {
                        id,
                        name,
                        arguments: CanonicalToolCall::from_wire_args(&args),
                    });
                }
            }
        }
        out
    }

    #[cfg(test)]
    pub(super) fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<OpenAiTool> {
        Self::convert_tools_ref(&tools)
    }

    pub(super) fn convert_tools_ref(tools: &[ToolDefinition]) -> Vec<OpenAiTool> {
        tools
            .iter()
            .map(|t| {
                // Defense in depth: `ToolDefinition::from` already sanitizes,
                // but direct constructors / cache hits may still carry Null
                // or a non-object root.
                let parameters =
                    crate::types::sanitize_tool_parameters(t.function.parameters.clone());
                // OpenAI Chat is also the fallback wire format for unknown
                // gateways. Keep every Chat-compatible request on the same
                // object-root projection so a gateway cannot reject a root
                // union before sampling starts.
                let parameters = crate::types::project_tool_parameters_for_object_root(parameters);
                OpenAiTool {
                    tool_type: t.tool_type.clone(),
                    function: OpenAiToolFunction {
                        name: t.function.name.clone(),
                        description: t.function.description.clone(),
                        parameters: crate::types::canonicalize_json(parameters),
                    },
                }
            })
            .collect()
    }
}
