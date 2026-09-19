use super::*;

impl GeminiAdapter {
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
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens_shared(&messages, &tools, max_output_tokens)
            .await
    }

    pub(super) async fn chat_stream_inner_with_max_tokens_shared(
        &self,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let cache_diagnostics = body.cache_diagnostics.clone();
        let url = self.stream_generate_url();
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            model = %self.endpoint.model_name,
            request_kind = "chat_stream",
            "POST provider endpoint"
        );
        tracing::trace!(
            "provider request body: {} chars",
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );

        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body);
        // For streaming, only apply an HTTP-level timeout when explicitly configured.
        // When timeout_streaming_secs is None, `stream_header_timeout` bounds the
        // response-header wait (a provider that accepts the connection but never
        // responds would otherwise stall silently until the router-level
        // max_total_duration_secs) while leaving the body stream to the router's
        // per-chunk idle timeouts.
        if let Some(timeout) = self.endpoint.timeout_streaming_secs {
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = send_request(
            req,
            stream_header_timeout(self.endpoint.timeout_streaming_secs),
        )
        .await?;

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        // `:streamGenerateContent?alt=sse` returns SSE frames; gateways that
        // ignore `alt=sse` fall back to raw JSON lines — both are handled.
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseOrRaw);

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<Result<String, LlmError>>,
            done: bool,
            /// Accumulated text per part index (deltas are emitted as suffixes).
            /// Tracks EVERY part (including `thought: true` reasoning parts) so
            /// prefix-stripping stays aligned on the part index.
            part_texts: Vec<String>,
            /// Accumulated reasoning per thinking part index (emitted as
            /// reasoning deltas, mirroring the text delta logic).
            reasoning_parts: Vec<String>,
            /// Accumulated tool calls per functionCall part index.
            tool_calls_acc: Vec<CanonicalToolCall>,
            /// Gemini thought signatures captured from response Parts. These
            /// are opaque provider state and are echoed on the next turn.
            thinking_blocks: Vec<Value>,
            accumulated_text: String,
            last_model: Option<String>,
            finish_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_finish: bool,
            web_search_calls: Vec<Value>,
            cache_diagnostics: CacheDiagnostics,
        }

        let empty_chunk = empty_chunk;

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                part_texts: Vec::new(),
                reasoning_parts: Vec::new(),
                tool_calls_acc: Vec::new(),
                thinking_blocks: Vec::new(),
                accumulated_text: String::new(),
                last_model: None,
                finish_reason: None,
                usage: None,
                saw_finish: false,
                web_search_calls: Vec::new(),
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
                        // Gemini delivers args as already-parsed JSON; a name
                        // with Null args and no finish means the stream died
                        // before arguments arrived. On a clean finish, omitted
                        // args mean `{}` (empty-parameter tools) — not Null.
                        let unfinished_tools = state
                            .tool_calls_acc
                            .iter()
                            .any(|tc| !tc.name.is_empty() && tc.arguments.is_null());
                        let chunk = if !state.saw_finish
                            && (!state.accumulated_text.is_empty() || unfinished_tools)
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            let tool_calls = std::mem::take(&mut state.tool_calls_acc)
                                .into_iter()
                                .filter(|tc| !tc.name.is_empty())
                                .map(|mut tc| {
                                    if tc.arguments.is_null() {
                                        tc.arguments = serde_json::json!({});
                                    }
                                    tc
                                })
                                .collect();
                            Ok(StreamChunk {
                                text: None,
                                // Flush accumulated tool calls like the OpenAI
                                // adapter: per-delta chunks carry none, the
                                // final chunk carries all merged calls.
                                tool_calls,
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: std::mem::take(&mut state.web_search_calls),
                                thinking_blocks: std::mem::take(&mut state.thinking_blocks),
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<GeminiResponse, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(resp) => {
                        if let Some(m) = &resp.model_version {
                            state.last_model = Some(m.clone());
                        }
                        if let Some(u) = resp.usage_metadata {
                            let mut usage = u.to_usage(state.last_model.clone());
                            usage.cache_diagnostics = Some(
                                state
                                    .cache_diagnostics
                                    .clone()
                                    .with_provider_usage(usage.cached_tokens),
                            );
                            state.usage = Some(usage);
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(candidate) = resp.candidates.and_then(|c| c.into_iter().next())
                        {
                            if let Some(sr) = candidate.finish_reason.as_deref() {
                                state.saw_finish = true;
                                state.finish_reason = Self::finish_reason_of(sr);
                            }
                            if let Some(meta) = candidate.grounding_metadata.as_ref() {
                                let calls = Self::web_search_calls_from_metadata(meta);
                                if !calls.is_empty() {
                                    state.web_search_calls = calls;
                                }
                            }
                            if let Some(content) = candidate.content {
                                for (idx, part) in content.parts.into_iter().enumerate() {
                                    let function_name = part
                                        .function_call
                                        .as_ref()
                                        .and_then(|fc| fc.name.as_deref())
                                        .map(str::to_string);
                                    Self::capture_thought_signature(
                                        &mut state.thinking_blocks,
                                        &part,
                                        function_name.as_deref(),
                                    );
                                    if let Some(t) = part.text {
                                        // Previous text is always a prefix of
                                        // the new text; emit only the delta.
                                        let delta = match state.part_texts.get(idx) {
                                            Some(prev) => {
                                                t.strip_prefix(prev).unwrap_or(&t).to_string()
                                            }
                                            None => t.clone(),
                                        };
                                        if state.part_texts.len() <= idx {
                                            state.part_texts.push(t);
                                        } else {
                                            state.part_texts[idx] = t;
                                        }
                                        if !delta.is_empty() && part.thought == Some(true) {
                                            // Thinking-mode part: reasoning
                                            // delta, never visible assistant
                                            // text. Mirror the text-delta
                                            // suffix logic per part index.
                                            let prev = state.reasoning_parts.get(idx);
                                            let rdelta = match prev {
                                                Some(prev) => {
                                                    let full = state.part_texts[idx].clone();
                                                    full.strip_prefix(prev)
                                                        .unwrap_or(&delta)
                                                        .to_string()
                                                }
                                                None => delta.clone(),
                                            };
                                            if !rdelta.is_empty() {
                                                if state.reasoning_parts.len() <= idx {
                                                    state
                                                        .reasoning_parts
                                                        .push(state.part_texts[idx].clone());
                                                } else {
                                                    state.reasoning_parts[idx] =
                                                        state.part_texts[idx].clone();
                                                }
                                                let r =
                                                    chunk.reasoning.get_or_insert_with(String::new);
                                                r.push_str(&rdelta);
                                            }
                                        } else if !delta.is_empty() {
                                            state.accumulated_text.push_str(&delta);
                                            let c = chunk.text.get_or_insert_with(String::new);
                                            c.push_str(&delta);
                                        }
                                    }
                                    if let Some(fc) = part.function_call
                                        && let Some(name) = fc.name
                                    {
                                        while state.tool_calls_acc.len() <= idx {
                                            state.tool_calls_acc.push(CanonicalToolCall {
                                                id: format!("call_{}", state.tool_calls_acc.len()),
                                                name: String::new(),
                                                arguments: Value::Null,
                                            });
                                        }
                                        if let Some(id) = fc.id {
                                            state.tool_calls_acc[idx].id = id;
                                        }
                                        state.tool_calls_acc[idx].name = name;
                                        if let Some(args) = fc.args {
                                            state.tool_calls_acc[idx].arguments = args;
                                        }
                                    }
                                }
                            }
                        }
                        Some((Ok(chunk), state))
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
