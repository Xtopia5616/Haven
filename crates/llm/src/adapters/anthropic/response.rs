use super::*;

impl AnthropicAdapter {
    #[cfg(test)]
    pub(super) fn parse_response(
        &self,
        json: AnthropicResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        self.parse_response_with_cache(json, model, CacheDiagnostics::default())
    }

    pub(super) fn parse_response_with_cache(
        &self,
        json: AnthropicResponse,
        model: Option<String>,
        cache_diagnostics: CacheDiagnostics,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut web_search_calls = Vec::new();
        let mut thinking_blocks = Vec::new();
        // Original position of each captured block (index in the content
        // array + visible-text char count before it) so the next tool-use
        // request can restore the exact interleaved order on echo.
        let mut layout: Vec<(u8, usize, usize)> = Vec::new();
        for (i, block) in json.content.into_iter().enumerate() {
            match block.block_type.as_deref() {
                Some("text") => {
                    if let Some(t) = block.text {
                        text.push_str(&t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.thinking {
                        reasoning.push_str(&t);
                        // Keep the raw thinking block (text + signature) so the
                        // next tool-use request can echo it back verbatim.
                        let mut block_json = json!({
                            "type": "thinking",
                            "thinking": t,
                        });
                        if let Some(sig) = block.signature {
                            block_json["signature"] = Value::String(sig);
                        }
                        thinking_blocks.push(block_json);
                        layout.push((Self::LAYOUT_KIND_THINKING, i, text.chars().count()));
                    }
                }
                Some("redacted_thinking") => {
                    // Redacted thinking (extended-thinking safety redaction)
                    // must be echoed back verbatim too; the data is not real
                    // thinking text, so it never feeds `reasoning`.
                    if let Some(data) = block.data {
                        thinking_blocks.push(json!({
                            "type": "redacted_thinking",
                            "data": data,
                        }));
                        layout.push((Self::LAYOUT_KIND_THINKING, i, text.chars().count()));
                    }
                }
                Some("tool_use") => {
                    if let Some(name) = block.name {
                        tool_calls.push(CanonicalToolCall {
                            id: block.id.unwrap_or_default(),
                            name,
                            arguments: block.input.unwrap_or_default(),
                        });
                        layout.push((Self::LAYOUT_KIND_TOOL_USE, i, text.chars().count()));
                    }
                }
                Some("server_tool_use") if block.name.as_deref() == Some("web_search") => {
                    let id = block.id.clone().unwrap_or_else(|| format!("ws_{i}"));
                    let queries = block
                        .input
                        .as_ref()
                        .and_then(|v| v.get("query"))
                        .cloned()
                        .map(|q| json!([q]))
                        .unwrap_or_else(|| json!([]));
                    web_search_calls.push(normalize_web_search_call_item(json!({
                        "type": "web_search_call",
                        "id": id,
                        "status": "completed",
                        "action": {"type": "search", "queries": queries},
                    })));
                }
                Some("web_search_tool_result") => {
                    let id = block.id.clone().unwrap_or_else(|| format!("ws_result_{i}"));
                    web_search_calls.push(normalize_web_search_call_item(json!({
                        "type": "web_search_call",
                        "id": id,
                        "status": "completed",
                        "action": {
                            "type": "search",
                            "queries": [],
                            "result": block.input.clone().unwrap_or(Value::Null),
                        },
                    })));
                }
                _ => {}
            }
        }
        if !layout.is_empty() {
            thinking_blocks.push(json!({Self::LAYOUT_KEY: layout}));
        }
        let usage = json
            .usage
            .map(|u| {
                let mut usage = Usage::from_counts_with_accounting(
                    u.input_tokens,
                    u.output_tokens,
                    u.input_tokens
                        .saturating_add(u.cache_read_input_tokens)
                        .saturating_add(u.cache_creation_input_tokens)
                        .saturating_add(u.output_tokens),
                    u.cache_read_input_tokens,
                    u.cache_creation_input_tokens,
                    CacheAccounting::Exclusive,
                    model.clone(),
                );
                usage.cache_diagnostics = Some(
                    cache_diagnostics
                        .clone()
                        .with_provider_usage(usage.cached_tokens),
                );
                usage
            })
            .unwrap_or_else(|| Usage {
                cache_diagnostics: Some(cache_diagnostics),
                ..Default::default()
            });
        Ok(LlmResponse {
            text,
            tool_calls,
            finish_reason: json
                .stop_reason
                .as_deref()
                .and_then(FinishReason::from_openai),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            web_search_calls,
            thinking_blocks,
        })
    }
}
