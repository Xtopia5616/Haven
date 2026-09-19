use super::*;

impl GeminiAdapter {
    pub(super) fn finish_reason_of(s: &str) -> Option<FinishReason> {
        match s {
            "STOP" => Some(FinishReason::Stop),
            "MAX_TOKENS" => Some(FinishReason::Length),
            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => {
                Some(FinishReason::ContentFilter)
            }
            "MALFORMED_FUNCTION_CALL" => Some(FinishReason::ToolCalls),
            _ => FinishReason::from_openai(&s.to_lowercase()),
        }
    }

    #[cfg(test)]
    pub(super) fn parse_response(
        &self,
        json: GeminiResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        self.parse_response_with_cache(json, model, CacheDiagnostics::default())
    }

    pub(super) fn parse_response_with_cache(
        &self,
        json: GeminiResponse,
        model: Option<String>,
        cache_diagnostics: CacheDiagnostics,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut thinking_blocks = Vec::new();
        let mut finish_reason = None;
        if let Some(candidate) = json.candidates.and_then(|c| c.into_iter().next()) {
            finish_reason = candidate
                .finish_reason
                .as_deref()
                .and_then(Self::finish_reason_of);
            if let Some(content) = candidate.content {
                for part in content.parts {
                    let function_name = part
                        .function_call
                        .as_ref()
                        .and_then(|fc| fc.name.as_deref())
                        .map(str::to_string);
                    Self::capture_thought_signature(
                        &mut thinking_blocks,
                        &part,
                        function_name.as_deref(),
                    );
                    if part.thought == Some(true) {
                        // Thinking-mode parts are internal reasoning, not
                        // assistant output: route to `reasoning` (displayed as
                        // a thought bubble), never into the visible answer.
                        if let Some(t) = part.text {
                            reasoning.push_str(&t);
                        }
                    } else if let Some(t) = part.text {
                        text.push_str(&t);
                    }
                    if let Some(fc) = part.function_call
                        && let Some(name) = fc.name
                    {
                        tool_calls.push(CanonicalToolCall {
                            id: fc
                                .id
                                .unwrap_or_else(|| format!("call_{}", tool_calls.len())),
                            name,
                            arguments: fc.args.unwrap_or_default(),
                        });
                    }
                }
            }
        }
        let usage = json
            .usage_metadata
            .as_ref()
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
            finish_reason,
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            web_search_calls: Vec::new(),
            thinking_blocks,
        })
    }

    pub(super) fn web_search_calls_from_metadata(meta: &Value) -> Vec<Value> {
        let queries = meta
            .get("webSearchQueries")
            .or_else(|| meta.get("web_search_queries"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        vec![normalize_web_search_call_item(json!({
            "type": "web_search_call",
            "id": "gemini_grounding",
            "status": "completed",
            "action": {"type": "search", "queries": queries},
        }))]
    }

    pub(super) fn web_search_calls_from_grounding(raw: &Value) -> Vec<Value> {
        let Some(meta) = raw
            .pointer("/candidates/0/groundingMetadata")
            .or_else(|| raw.pointer("/candidates/0/grounding_metadata"))
        else {
            return Vec::new();
        };
        Self::web_search_calls_from_metadata(meta)
    }
}
pub(super) fn parse_gemini_embed_response(
    body: &str,
    requested: usize,
    requested_model: &str,
) -> Result<Embedding, LlmError> {
    let json: GeminiEmbedResponse =
        serde_json::from_str(body).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let mut vectors: Vec<Vec<f32>> = json
        .embeddings
        .into_iter()
        .map(|item| item.values)
        .collect();
    if vectors.is_empty()
        && let Some(single) = json.embedding
    {
        vectors.push(single.values);
    }
    if vectors.is_empty() {
        return Err(LlmError::InvalidResponse(
            "embeddings response missing data".into(),
        ));
    }
    if vectors.len() != requested {
        return Err(LlmError::InvalidResponse(format!(
            "embeddings count mismatch: requested {requested}, got {}",
            vectors.len()
        )));
    }
    Ok(Embedding {
        vectors,
        model: Some(requested_model.to_string()),
        usage: Usage::default(),
    })
}
