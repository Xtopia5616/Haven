use super::*;

impl AnthropicAdapter {
    /// Layout entry kind: the block lives in `tool_calls`.
    pub(super) const LAYOUT_KIND_TOOL_USE: u8 = 1;

    /// Layout entry kind: the block lives in `thinking_blocks`.
    pub(super) const LAYOUT_KIND_THINKING: u8 = 0;

    /// by position, so the echo is free to restore the exact interleaving.
    pub(super) const LAYOUT_KEY: &str = "__layout";

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

    pub(super) fn split_layout(blocks: &mut Vec<Value>) -> Option<Vec<(u8, usize, usize)>> {
        let last = blocks.last_mut()?;
        if !last.is_object() {
            return None;
        }
        let entry = last.get(Self::LAYOUT_KEY)?.clone();
        let layout: Vec<(u8, usize, usize)> = serde_json::from_value(entry).ok()?;
        blocks.pop();
        Some(layout)
    }

    pub(super) fn rebuild_ordered_blocks(
        content: &[ContentPart],
        thinking_blocks: &[Value],
        tool_calls: Option<&[CanonicalToolCall]>,
        layout: &[(u8, usize, usize)],
    ) -> Option<Vec<Value>> {
        // Non-text parts (e.g. images) have no position in the layout; use
        // the deterministic front-loaded order for such messages.
        if content.iter().any(|p| !matches!(p, ContentPart::Text(_))) {
            return None;
        }
        let text: String = content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let calls = tool_calls.unwrap_or_default();
        let text_len = text.chars().count();

        let (think_count, call_count) =
            layout
                .iter()
                .fold((0usize, 0usize), |(t, c), (kind, _, _)| {
                    if *kind == Self::LAYOUT_KIND_THINKING {
                        (t + 1, c)
                    } else {
                        (t, c + 1)
                    }
                });
        if think_count != thinking_blocks.len() || call_count != calls.len() {
            return None;
        }

        let mut out = Vec::new();
        let mut prev_pos: Option<usize> = None;
        let mut prev_tb = 0usize;
        let mut think_idx = 0usize;
        let mut call_idx = 0usize;
        for (kind, pos, tb) in layout {
            if let Some(p) = prev_pos
                && *pos <= p
            {
                return None;
            }
            if *tb < prev_tb || *tb > text_len {
                return None;
            }
            prev_pos = Some(*pos);
            if *tb > prev_tb {
                out.push(json!({
                    "type": "text",
                    "text": text
                        .chars()
                        .skip(prev_tb)
                        .take(*tb - prev_tb)
                        .collect::<String>()
                }));
            }
            match *kind {
                Self::LAYOUT_KIND_THINKING => {
                    out.push(thinking_blocks[think_idx].clone());
                    think_idx += 1;
                }
                _ => {
                    let tc = &calls[call_idx];
                    out.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments
                    }));
                    call_idx += 1;
                }
            }
            prev_tb = *tb;
        }
        if text_len > prev_tb {
            out.push(json!({
                "type": "text",
                "text": text.chars().skip(prev_tb).collect::<String>()
            }));
        }
        Some(out)
    }

    pub(super) fn front_loaded_assistant_blocks(
        captured: Vec<Value>,
        content: &[ContentPart],
        tool_calls: Option<Vec<CanonicalToolCall>>,
    ) -> Vec<Value> {
        // `thinking_blocks` is a provider-opaque field on the canonical
        // message. Gemini stores its `thoughtSignature` echo markers there;
        // Anthropic must only receive its own signed content blocks.
        let mut blocks: Vec<Value> = captured
            .into_iter()
            .filter(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking") | Some("redacted_thinking")
                )
            })
            .collect();
        blocks.extend(Self::content_to_blocks(content));
        if let Some(calls) = tool_calls {
            for tc in calls {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.arguments
                }));
            }
        }
        blocks
    }

    pub(super) fn content_to_blocks(parts: &[ContentPart]) -> Vec<Value> {
        let mut blocks = Vec::new();
        for p in parts {
            match p {
                ContentPart::Text(t) => {
                    blocks.push(json!({"type": "text", "text": t}));
                }
                ContentPart::Image {
                    media_type, data, ..
                } => {
                    blocks.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data
                        }
                    }));
                }
                ContentPart::Audio { .. } => {
                    tracing::warn!(
                        "Anthropic Messages API does not support audio input; rendering an explicit unsupported marker"
                    );
                    blocks.push(json!({
                        "type": "text",
                        "text": "[Haven: audio input is not supported by the configured Anthropic model]"
                    }));
                }
                ContentPart::Video { .. } => {
                    tracing::warn!(
                        "Anthropic Messages API does not support video input; rendering an explicit unsupported marker"
                    );
                    blocks.push(json!({
                        "type": "text",
                        "text": "[Haven: video input is not supported by the configured Anthropic model]"
                    }));
                }
            }
        }
        blocks
    }

    pub(super) fn convert_messages(
        msgs: impl AsRef<[CanonicalMessage]>,
    ) -> (Vec<AnthropicMessage>, Option<String>) {
        let msgs = msgs.as_ref();
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<AnthropicMessage> = Vec::new();
        for m in msgs {
            match m.role {
                CanonicalRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            system_parts.push(t.clone());
                        }
                    }
                }
                CanonicalRole::User => {
                    if m.tool_call_id.is_some() {
                        out.push(AnthropicMessage {
                            role: "user".into(),
                            content: json!([{
                                "type": "tool_result",
                                "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                                "content": Self::text_content(&m.content)
                            }]),
                        });
                    } else {
                        let content =
                            crate::adapters::apply_wire_inject_prefix(m.source, m.content.clone());
                        let blocks = Self::content_to_blocks(&content);
                        if blocks.is_empty() {
                            continue;
                        }
                        out.push(AnthropicMessage {
                            role: "user".into(),
                            content: Value::Array(blocks),
                        });
                    }
                }
                CanonicalRole::Assistant => {
                    // Echo the raw thinking blocks verbatim (text + signature):
                    // Anthropic 400s a tool-use turn that omits or rewrites
                    // them. `thinking_blocks` holds the exact
                    // `{"type":"thinking",…}` JSON captured upstream, plus an
                    // internal `__layout` marker that records each block's
                    // original position so the echo restores the exact
                    // interleaved order instead of front-loading the thinking
                    // blocks (position does not affect signature validation).
                    let calls = m.tool_calls.clone();
                    let mut captured = m.thinking_blocks.clone();
                    let layout = Self::split_layout(&mut captured);
                    captured.retain(|block| {
                        matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("thinking") | Some("redacted_thinking")
                        )
                    });
                    let blocks = match layout {
                        Some(layout) => {
                            match Self::rebuild_ordered_blocks(
                                &m.content,
                                &captured,
                                calls.as_deref(),
                                &layout,
                            ) {
                                Some(ordered) => ordered,
                                None => {
                                    Self::front_loaded_assistant_blocks(captured, &m.content, calls)
                                }
                            }
                        }
                        None => Self::front_loaded_assistant_blocks(captured, &m.content, calls),
                    };
                    if blocks.is_empty() {
                        continue;
                    }
                    out.push(AnthropicMessage {
                        role: "assistant".into(),
                        content: Value::Array(blocks),
                    });
                }
                CanonicalRole::Tool => {
                    out.push(AnthropicMessage {
                        role: "user".into(),
                        content: json!([{
                            "type": "tool_result",
                            "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                            "content": Self::text_content(&m.content)
                        }]),
                    });
                }
            }
        }
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        (out, system)
    }

    pub(super) fn convert_tools(tools: impl AsRef<[ToolDefinition]>) -> Vec<Value> {
        tools
            .as_ref()
            .iter()
            .map(|t| {
                // Defense in depth: the local ToolDefinition constructor
                // sanitizes schemas, but cached/direct definitions can still
                // contain a null or non-object root.
                let parameters = crate::types::canonicalize_json(
                    crate::types::sanitize_tool_parameters(t.function.parameters.clone()),
                );
                json!({
                    "name": t.function.name,
                    "description": t.function.description,
                    "input_schema": parameters,
                })
            })
            .collect()
    }
}
