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
        let mut response_citations = Vec::new();
        for item in json.output {
            match item.item_type.as_deref() {
                Some("message") => {
                    for part in item.content {
                        if let Some(t) = part.text {
                            text.push_str(&t);
                        }
                        response_citations
                            .extend(Self::citations_from_annotations(&part.annotations));
                    }
                }
                Some("function_call") => {
                    if let Some(name) = item.name {
                        tool_calls.push(CanonicalToolCall {
                            tool_call_id: item.call_id.or(item.id).unwrap_or_default(),
                            name,
                            arguments: item
                                .arguments
                                .map(|a| parse_completed_tool_arguments(&a))
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
                Some("web_search_call") | Some("browser_search_call") => {
                    web_search_calls.push(normalize_web_search_call_item(
                        serde_json::to_value(&item).unwrap_or_default(),
                    ));
                }
                _ => {}
            }
        }
        Self::merge_citations(&mut web_search_calls, response_citations);
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
                        .with_provider_usage(u.cached_tokens_reported(), u.cache_usage_reported()),
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

    pub(super) fn citations_from_annotations(annotations: &[Value]) -> Vec<Value> {
        crate::adapters::web_search::citations_from_annotations(annotations)
    }

    pub(super) fn merge_citations(calls: &mut Vec<Value>, citations: Vec<Value>) {
        if citations.is_empty() {
            return;
        }
        let call = calls.iter_mut().find(|call| {
            call.get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "web_search_call" | "browser_search_call"))
        });
        let call = if let Some(call) = call {
            call
        } else {
            calls.push(serde_json::json!({
                "type": "web_search_call",
                "id": "responses_citations",
                "status": "completed",
                "action": {"type": "search", "queries": [], "citations": []},
                "_responses_citation_only": true
            }));
            calls.last_mut().expect("citation item was appended")
        };
        if !call.get("action").is_some_and(Value::is_object) {
            call["action"] = serde_json::json!({"type": "search", "queries": []});
        }
        let action = call
            .get_mut("action")
            .and_then(Value::as_object_mut)
            .expect("search action was normalized to an object");
        let existing = action
            .entry("citations")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(existing) = existing.as_array_mut() {
            for citation in citations {
                if !existing.contains(&citation) {
                    existing.push(citation);
                }
            }
        }
    }
}
