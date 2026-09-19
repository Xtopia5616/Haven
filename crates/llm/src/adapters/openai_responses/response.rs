use super::*;

impl OpenAiResponsesAdapter {
    pub(super) fn finish_reason_of(status: &str) -> Option<FinishReason> {
        match status {
            "completed" => Some(FinishReason::Stop),
            "incomplete" => Some(FinishReason::Length),
            "cancelled" => None,
            _ => None,
        }
    }

    #[cfg(test)]
    pub(super) fn parse_response(
        &self,
        json: ResponsesResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        self.parse_response_with_cache(json, model, CacheDiagnostics::default())
    }

    pub(super) fn parse_response_with_cache(
        &self,
        json: ResponsesResponse,
        model: Option<String>,
        cache_diagnostics: CacheDiagnostics,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut web_search_calls = Vec::new();
        for item in json.output {
            match item.item_type.as_deref() {
                Some("message") => {
                    for part in item.content {
                        if let Some(t) = part.text {
                            text.push_str(&t);
                        }
                    }
                }
                Some("function_call") => {
                    if let Some(name) = item.name {
                        tool_calls.push(CanonicalToolCall {
                            id: item.call_id.or(item.id).unwrap_or_default(),
                            name,
                            arguments: item
                                .arguments
                                .map(|a| CanonicalToolCall::from_wire_args(&a))
                                .unwrap_or(Value::Null),
                        });
                    }
                }
                Some("reasoning") => {
                    for part in item.content {
                        if let Some(t) = part.text {
                            reasoning.push_str(&t);
                        }
                    }
                }
                // Server-side web search (DeepSeek built-in): not a local
                // tool. Keep the raw item so it can be passed back verbatim
                // in the next request's input (with the `action`
                // discriminator normalized in).
                Some("web_search_call") => {
                    web_search_calls.push(normalize_web_search_call_item(
                        serde_json::to_value(&item).unwrap_or_default(),
                    ));
                }
                _ => {}
            }
        }
        if json.status.as_deref() == Some("failed") {
            let msg = json
                .error
                .map(|e| serde_json::to_string(&e).unwrap_or_default())
                .unwrap_or_else(|| "response failed".into());
            return Err(LlmError::RequestFailed(msg));
        }
        let usage = json
            .usage
            .map(|u| {
                let mut usage = u.to_usage(model.clone());
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
            finish_reason: json.status.as_deref().and_then(Self::finish_reason_of),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            web_search_calls,
            thinking_blocks: Vec::new(),
        })
    }
}
