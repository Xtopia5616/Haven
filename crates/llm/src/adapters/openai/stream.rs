use super::*;

impl OpenAiAdapter {
    pub(super) async fn chat_stream_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens(messages, tools, None)
            .await
    }

    pub(super) async fn chat_stream_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens_shared(&messages, &tools, max_tokens)
            .await
    }

    pub(super) async fn chat_stream_inner_with_max_tokens_shared(
        &self,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        max_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens_shared_guidance(messages, tools, None, max_tokens)
            .await
    }

    pub(super) async fn chat_stream_inner_with_max_tokens_shared_guidance(
        &self,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        guidance: Option<&str>,
        max_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let mut body = self.build_request_body_with_mode_and_max_tokens_shared(
            messages,
            tools,
            true,
            self.web_search_mode,
            max_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        if let Some(guidance) = guidance {
            self.append_guidance_to_request(&mut body, guidance);
        }
        let url = format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        );
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            model = %self.endpoint.model_name,
            request_kind = "chat_stream",
            timeout_secs = self.endpoint.timeout_secs,
            timeout_streaming = ?self.endpoint.timeout_streaming_secs,
            "POST provider endpoint"
        );
        tracing::trace!(
            "provider request body: {} chars",
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );
        let resp = self
            .send_chat_request(&url, &mut body, true)
            .await
            .map_err(|e| {
                tracing::debug!("chat_stream_inner: send() error: {:?}", e);
                e
            })?;
        tracing::debug!("chat_stream_inner response status: {}", resp.status());
        let cache_diagnostics = body.cache_diagnostics;

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseOrRaw);

        // Merge streaming tool-call deltas by index. Arguments arrive as
        // incremental JSON fragments, so they accumulate as a raw string and
        // are parsed once at flush time.
        fn merge_tool_call(
            acc: &mut Vec<(String, String, String)>,
            index: usize,
            id: Option<&str>,
            name: Option<&str>,
            arguments: Option<&str>,
        ) {
            while acc.len() <= index {
                acc.push((String::new(), String::new(), String::new()));
            }
            if let Some(id) = id
                && !id.is_empty()
            {
                acc[index].0 = id.to_string();
            }
            if let Some(name) = name
                && !name.is_empty()
            {
                acc[index].1 = name.to_string();
            }
            if let Some(args) = arguments {
                acc[index].2.push_str(args);
            }
        }

        // Return the first delta/message available: providers differ on whether
        // they send `delta` (standard SSE) or `message` (non-standard) per chunk.
        fn choice_delta(choice: &OpenAiChoice) -> Option<&OpenAiMessageOut> {
            choice.delta.as_ref().or(choice.message.as_ref())
        }

        struct UnfoldState {
            rx: tokio::sync::mpsc::UnboundedReceiver<Result<String, LlmError>>,
            done: bool,
            accumulated_text: String,
            tool_calls_acc: Vec<(String, String, String)>,
            web_search_acc: Vec<serde_json::Value>,
            last_model: Option<String>,
            has_finish_reason: bool,
            usage: Option<Usage>,
            cache_diagnostics: CacheDiagnostics,
        }

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                accumulated_text: String::new(),
                tool_calls_acc: Vec::new(),
                web_search_acc: Vec::new(),
                last_model: None,
                has_finish_reason: false,
                usage: None,
                cache_diagnostics,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(Ok(d)) => d,
                    Some(Err(error)) => {
                        state.done = true;
                        return Some((Err(error), state));
                    }
                    None => {
                        // Interrupted mid-tool-call (no finish_reason): empty
                        // args after a name, structural-only repair, or
                        // mid-string JSON must not flush as executable calls.
                        let unfinished_tools =
                            state.tool_calls_acc.iter().any(|(_, name, args)| {
                                CanonicalToolCall::stream_tool_args_unfinished(name, args)
                            });
                        let chunk = if !state.has_finish_reason
                            && (!state.accumulated_text.is_empty() || unfinished_tools)
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: std::mem::take(&mut state.tool_calls_acc)
                                    .into_iter()
                                    .map(|(id, name, args)| CanonicalToolCall {
                                        id,
                                        name,
                                        arguments: CanonicalToolCall::from_wire_args(&args),
                                    })
                                    .collect(),
                                finish_reason: None,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: std::mem::take(&mut state.web_search_acc),
                                thinking_blocks: Vec::new(),
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<OpenAiStreamResponse, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(resp) => {
                        if let Some(model) = &resp.model {
                            state.last_model = Some(model.clone());
                        }
                        if let Some(u) = resp.usage {
                            let mut usage = u.to_usage(state.last_model.clone());
                            usage.cache_miss_tokens = usage.cache_miss_tokens();
                            usage.cache_diagnostics = Some(
                                state
                                    .cache_diagnostics
                                    .clone()
                                    .with_provider_usage(usage.cached_tokens),
                            );
                            state.usage = Some(usage);
                        }
                        if !resp.citations.is_empty() {
                            crate::adapters::upsert_web_search_call(
                                &mut state.web_search_acc,
                                normalize_web_search_call_item(serde_json::json!({
                                    "type": "web_search_call",
                                    "id": "xai_citations",
                                    "status": "completed",
                                    "action": {
                                        "type": "search",
                                        "queries": [],
                                        "citations": resp.citations,
                                    },
                                })),
                            );
                        }
                        if let Some(choice) = resp.choices.into_iter().next() {
                            let text = stream_content(&choice).and_then(|content| {
                                append_stream_text(&mut state.accumulated_text, &content)
                            });
                            if let Some(delta) = choice_delta(&choice)
                                && let Some(calls) = &delta.tool_calls
                            {
                                for c in calls {
                                    let idx = c.index.unwrap_or(0) as usize;
                                    merge_tool_call(
                                        &mut state.tool_calls_acc,
                                        idx,
                                        c.id.as_deref(),
                                        c.function.name.as_deref(),
                                        c.function.arguments.as_deref(),
                                    );
                                }
                            }
                            // DeepSeek's built-in web search: accumulate the
                            // `web_search_call` items so they can be echoed
                            // back verbatim on the next request.
                            if let Some(delta) = choice_delta(&choice)
                                && !delta.web_search_call.is_empty()
                            {
                                state
                                    .web_search_acc
                                    .extend(delta.web_search_call.iter().cloned());
                            }
                            if choice.finish_reason.is_some() {
                                state.has_finish_reason = true;
                            }
                            let finish_reason = choice
                                .finish_reason
                                .as_ref()
                                .and_then(|s| FinishReason::from_openai(s));
                            Some((
                                Ok(StreamChunk {
                                    text,
                                    reasoning: choice_delta(&choice)
                                        .and_then(|d| d.reasoning_content.clone()),
                                    tool_calls: Vec::new(),
                                    finish_reason,
                                    usage: None,
                                    model: state.last_model.clone(),
                                    web_search: None,
                                    web_search_calls: Vec::new(),
                                    thinking_blocks: Vec::new(),
                                }),
                                state,
                            ))
                        } else {
                            Some((
                                Ok(StreamChunk {
                                    text: None,
                                    reasoning: None,
                                    tool_calls: Vec::new(),
                                    finish_reason: None,
                                    usage: state.usage.take(),
                                    model: state.last_model.clone(),
                                    web_search: None,
                                    web_search_calls: Vec::new(),
                                    thinking_blocks: Vec::new(),
                                }),
                                state,
                            ))
                        }
                    }
                    Err(e) => Some((
                        Err(LlmError::InvalidResponse(format!("parse error: {}", e))),
                        state,
                    )),
                }
            },
        )
        .fuse();

        Ok(Box::pin(mapped))
    }
}
pub(super) fn append_stream_text(accumulated: &mut String, content: &str) -> Option<String> {
    let delta = if content.starts_with(accumulated.as_str()) {
        &content[accumulated.len()..]
    } else if accumulated.starts_with(content) {
        return None;
    } else {
        content
    };
    if delta.is_empty() {
        return None;
    }
    accumulated.push_str(delta);
    Some(delta.to_string())
}

pub(super) fn stream_content(choice: &OpenAiChoice) -> Option<String> {
    let delta = choice
        .delta
        .as_ref()
        .and_then(|message| message.content.as_deref());
    let message = choice
        .message
        .as_ref()
        .and_then(|message| message.content.as_deref());
    match (delta, message) {
        (Some(delta), Some(message)) if message.chars().count() > delta.chars().count() => {
            Some(message.to_string())
        }
        (Some(delta), _) => Some(delta.to_string()),
        (None, Some(message)) => Some(message.to_string()),
        (None, None) => None,
    }
}
