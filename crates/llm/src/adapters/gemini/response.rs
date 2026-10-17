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
            _ => FinishReason::parse_provider_value(&s.to_lowercase()),
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
                            tool_call_id: fc
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
                        .with_provider_usage(u.cached_tokens, u.cached_tokens.is_some()),
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
        let supports = meta
            .get("groundingSupports")
            .or_else(|| meta.get("grounding_supports"))
            .and_then(Value::as_array);
        let mut snippets_by_chunk: std::collections::HashMap<usize, Vec<String>> =
            std::collections::HashMap::new();
        if let Some(supports) = supports {
            for support in supports {
                let snippet = support
                    .get("segment")
                    .and_then(|segment| segment.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if snippet.is_empty() {
                    continue;
                }
                let indices = support
                    .get("groundingChunkIndices")
                    .or_else(|| support.get("grounding_chunk_indices"))
                    .and_then(Value::as_array);
                if let Some(indices) = indices {
                    for index in indices.iter().filter_map(Value::as_u64) {
                        snippets_by_chunk
                            .entry(index as usize)
                            .or_default()
                            .push(snippet.to_string());
                    }
                }
            }
        }
        let citations = meta
            .get("groundingChunks")
            .or_else(|| meta.get("grounding_chunks"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, chunk)| {
                let web = chunk
                    .get("web")
                    .or_else(|| chunk.get("image"))
                    .or_else(|| chunk.get("web_chunk"))
                    .or_else(|| chunk.get("image_chunk"))?;
                let url = web
                    .get("uri")
                    .or_else(|| web.get("sourceUri"))
                    .or_else(|| web.get("source_uri"))
                    .and_then(Value::as_str)?;
                if url.is_empty() {
                    return None;
                }
                let title = web.get("title").and_then(Value::as_str).unwrap_or(url);
                let snippet = snippets_by_chunk
                    .get(&index)
                    .map(|snippets| snippets.join(" "))
                    .unwrap_or_default();
                Some(json!({"title": title, "url": url, "snippet": snippet}))
            })
            .collect::<Vec<_>>();
        vec![normalize_web_search_call_item(json!({
            "type": "web_search_call",
            "id": "gemini_grounding",
            "status": "completed",
            "action": {"type": "search", "queries": queries, "citations": citations},
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
