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
        let annotations = choice
            .message
            .as_ref()
            .map(|m| m.annotations.as_slice())
            .unwrap_or_default();
        let usage = json.usage.as_ref();
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
        web_search_calls.extend(crate::adapters::chat_search_calls(
            &self.endpoint,
            self.style,
            annotations,
            usage.and_then(|usage| usage.x_tools.as_ref()),
            usage.and_then(|usage| usage.server_tool_use.as_ref()),
        ));
        // xAI returns citation URLs at the top level; fold them into the
        // canonical web_search_calls list so multi-turn echo / UI cards work.
        if let Some(call) = crate::adapters::xai::citation_search_call(json.citations) {
            web_search_calls.push(normalize_web_search_call_item(call));
        }

        let usage = json
            .usage
            .map(|u| {
                let mut usage = u.to_usage(model.clone());
                usage.cache_diagnostics = Some(
                    cache_diagnostics
                        .clone()
                        .with_provider_usage(u.cached_tokens_reported(), u.cache_usage_reported()),
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
                .and_then(|s| FinishReason::parse_provider_value(&s)),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning,
            web_search_calls,
            thinking_blocks: Vec::new(),
        };
        tracing::trace!(
            "parse_openai_response: text={} bytes, tool_calls={}, reasoning={}, usage p/c/t={}/{}/{}",
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
