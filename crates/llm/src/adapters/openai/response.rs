use super::*;

impl OpenAiAdapter {
    pub(super) fn parse_openai_response(
        &self,
        json: OpenAiResponse,
        model: Option<String>,
        cache_diagnostics: CacheDiagnostics,
    ) -> Result<LlmResponse, LlmError> {
        let choice = json
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::InvalidResponse("no choices".into()))?;
        let text = choice
            .message
            .as_ref()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let reasoning = choice
            .message
            .as_ref()
            .and_then(|m| m.reasoning_content.clone());
        let tool_calls = Self::extract_tool_calls(&choice);
        let mut web_search_calls: Vec<Value> = choice
            .message
            .as_ref()
            .map(|m| {
                m.web_search_call
                    .iter()
                    .cloned()
                    .map(normalize_web_search_call_item)
                    .collect()
            })
            .unwrap_or_default();
        // xAI returns citation URLs at the top level; fold them into the
        // canonical web_search_calls list so multi-turn echo / UI cards work.
        if !json.citations.is_empty() {
            web_search_calls.push(normalize_web_search_call_item(serde_json::json!({
                "type": "web_search_call",
                "id": "xai_citations",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": [],
                    "citations": json.citations,
                },
            })));
        }

        let usage = json
            .usage
            .map(|u| {
                let mut usage = u.to_usage(model.clone());
                usage.cache_miss_tokens = usage.cache_miss_tokens();
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

        let response = LlmResponse {
            text,
            tool_calls,
            finish_reason: choice
                .finish_reason
                .and_then(|s| FinishReason::from_openai(&s)),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning,
            web_search_calls,
            thinking_blocks: Vec::new(),
        };
        tracing::trace!(
            "parse_openai_response: text={} chars, tool_calls={}, reasoning={}, usage p/c/t={}/{}/{}",
            response.text.len(),
            response.tool_calls.len(),
            response.reasoning.is_some(),
            response.usage.prompt_tokens,
            response.usage.completion_tokens,
            response.usage.total_tokens,
        );
        Ok(response)
    }
}
