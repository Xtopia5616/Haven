use super::*;

impl AnthropicAdapter {
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
        let body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            true,
            self.web_search_mode,
            max_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let cache_diagnostics = body.cache_diagnostics.clone();
        let url = self.messages_url();
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
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseDataOnly);

        struct BlockState {
            kind: BlockKind,
            tool_id: String,
            tool_name: String,
            tool_input: String,
            thinking: String,
            thinking_signature: String,
            /// Original content-block index (for the echo layout marker).
            pos: usize,
            /// Char count of accumulated visible text when this block started
            /// (for the echo layout marker).
            text_before: usize,
        }

        #[derive(PartialEq)]
        enum BlockKind {
            Text,
            Thinking,
            RedactedThinking,
            ToolUse,
        }

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<Result<String, LlmError>>,
            done: bool,
            /// Per-content-block streaming state, indexed by Anthropic block index.
            blocks: Vec<BlockState>,
            accumulated_text: String,
            /// Capture-time layout: `(kind, pos, text_before)` per content
            /// block, in order. Emitted as the trailing `__layout` marker on
            /// the final chunk so the echo can restore the interleaving.
            layout: Vec<(u8, usize, usize)>,
            last_model: Option<String>,
            stop_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_message_stop: bool,
            web_search_calls: Vec<Value>,
            cache_diagnostics: CacheDiagnostics,
        }

        let empty_chunk = empty_chunk;

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                blocks: Vec::new(),
                accumulated_text: String::new(),
                layout: Vec::new(),
                last_model: None,
                stop_reason: None,
                usage: None,
                saw_message_stop: false,
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
                        // Open / unfinished tool_use blocks mean the stream
                        // died mid-arguments — treat as truncated.
                        let unfinished_tools = state.blocks.iter().any(|b| {
                            matches!(b.kind, BlockKind::ToolUse)
                                && (CanonicalToolCall::stream_tool_args_unfinished(
                                    &b.tool_name,
                                    &b.tool_input,
                                ) || !state.layout.iter().any(|(k, pos, _)| {
                                    *k == Self::LAYOUT_KIND_TOOL_USE && *pos == b.pos
                                }))
                        });
                        let chunk = if !state.saw_message_stop
                            && (!state.accumulated_text.is_empty() || unfinished_tools)
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            let mut final_chunk = StreamChunk {
                                text: None,
                                tool_calls: Vec::new(),
                                finish_reason: state.stop_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: std::mem::take(&mut state.web_search_calls),
                                thinking_blocks: Vec::new(),
                            };
                            if !state.layout.is_empty() {
                                final_chunk.thinking_blocks.push(json!({
                                    Self::LAYOUT_KEY: state.layout
                                }));
                            }
                            Ok(final_chunk)
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<AnthropicStreamEvent, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(AnthropicStreamEvent::MessageStart { message }) => {
                        if let Some(m) = &message.model {
                            state.last_model = Some(m.clone());
                        }
                        if let Some(u) = message.usage {
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
                                state.last_model.clone(),
                            );
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
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::ContentBlockStart {
                        index,
                        content_block,
                    }) => {
                        while state.blocks.len() <= index {
                            state.blocks.push(BlockState {
                                kind: BlockKind::Text,
                                tool_id: String::new(),
                                tool_name: String::new(),
                                tool_input: String::new(),
                                thinking: String::new(),
                                thinking_signature: String::new(),
                                pos: 0,
                                text_before: 0,
                            });
                        }
                        {
                            let block = &mut state.blocks[index];
                            block.pos = index;
                            block.text_before = state.accumulated_text.chars().count();
                            match content_block.block_type.as_deref() {
                                Some("tool_use") => {
                                    block.kind = BlockKind::ToolUse;
                                    block.tool_id = content_block.id.clone().unwrap_or_default();
                                    block.tool_name = content_block.name.clone().unwrap_or_default();
                                    block.tool_input = String::new();
                                    // Some gateways send the full input in the
                                    // start event instead of `{}` + deltas.
                                    if let Some(ref input) = content_block.input {
                                        let s = serde_json::to_string(input).unwrap_or_default();
                                        if s != "{}" {
                                            block.tool_input = s;
                                        }
                                    }
                                }
                                Some("thinking") => {
                                    block.kind = BlockKind::Thinking;
                                    // The signature arrives on the start event;
                                    // without it the echo of this block would be
                                    // rejected with a 400 on the next turn.
                                    block.thinking_signature =
                                        content_block.signature.unwrap_or_default();
                                }
                                Some("redacted_thinking") => {
                                    // Redacted thinking deltas accumulate into
                                    // `block.thinking` like plain thinking; the
                                    // stop handler re-emits them as a
                                    // `redacted_thinking` block for the echo.
                                    block.kind = BlockKind::RedactedThinking;
                                }
                                _ => block.kind = BlockKind::Text,
                            }
                        }
                        match content_block.block_type.as_deref() {
                            Some("server_tool_use")
                                if content_block.name.as_deref() == Some("web_search") =>
                            {
                                let id = content_block
                                    .id
                                    .clone()
                                    .unwrap_or_else(|| format!("ws_{index}"));
                                let queries = content_block
                                    .input
                                    .as_ref()
                                    .and_then(|v| v.get("query"))
                                    .cloned()
                                    .map(|q| json!([q]))
                                    .unwrap_or_else(|| json!([]));
                                state.web_search_calls.push(normalize_web_search_call_item(
                                    json!({
                                        "type": "web_search_call",
                                        "id": id,
                                        "status": "completed",
                                        "action": {"type": "search", "queries": queries},
                                    }),
                                ));
                            }
                            Some("web_search_tool_result") => {
                                let id = content_block
                                    .id
                                    .clone()
                                    .unwrap_or_else(|| format!("ws_result_{index}"));
                                state.web_search_calls.push(normalize_web_search_call_item(
                                    json!({
                                        "type": "web_search_call",
                                        "id": id,
                                        "status": "completed",
                                        "action": {
                                            "type": "search",
                                            "queries": [],
                                            "result": content_block.input.clone().unwrap_or(Value::Null),
                                        },
                                    }),
                                ));
                            }
                            _ => {}
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::ContentBlockDelta { index, delta }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        match delta {
                            AnthropicStreamDelta::TextDelta { text } => {
                                state.accumulated_text.push_str(&text);
                                chunk.text = Some(text);
                            }
                            AnthropicStreamDelta::ThinkingDelta { thinking } => {
                                chunk.reasoning = Some(thinking.clone());
                                if let Some(block) = state.blocks.get_mut(index) {
                                    block.thinking.push_str(&thinking);
                                }
                            }
                            AnthropicStreamDelta::InputJsonDelta { partial_json } => {
                                if let Some(block) = state.blocks.get_mut(index) {
                                    block.tool_input.push_str(&partial_json);
                                }
                            }
                            AnthropicStreamDelta::Other => {}
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::ContentBlockStop { index }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(block) = state.blocks.get(index) {
                            match block.kind {
                                BlockKind::ToolUse => {
                                    chunk.tool_calls.push(CanonicalToolCall {
                                        id: block.tool_id.clone(),
                                        name: block.tool_name.clone(),
                                        arguments: CanonicalToolCall::from_wire_args(
                                            &block.tool_input,
                                        ),
                                    });
                                    state.layout.push((
                                        Self::LAYOUT_KIND_TOOL_USE,
                                        block.pos,
                                        block.text_before,
                                    ));
                                }
                                BlockKind::Thinking => {
                                    // Emit the completed thinking block (text +
                                    // signature) so the aggregation keeps it
                                    // verbatim for the next request's echo.
                                    if !block.thinking.is_empty() {
                                        let mut block_json = json!({
                                            "type": "thinking",
                                            "thinking": block.thinking.clone(),
                                        });
                                        if !block.thinking_signature.is_empty() {
                                            block_json["signature"] =
                                                Value::String(block.thinking_signature.clone());
                                        }
                                        chunk.thinking_blocks.push(block_json);
                                        state.layout.push((
                                            Self::LAYOUT_KIND_THINKING,
                                            block.pos,
                                            block.text_before,
                                        ));
                                    }
                                }
                                BlockKind::RedactedThinking => {
                                    // Redacted thinking must also round-trip
                                    // verbatim; the deltas held `data` chunks.
                                    if !block.thinking.is_empty() {
                                        chunk.thinking_blocks.push(json!({
                                            "type": "redacted_thinking",
                                            "data": block.thinking.clone(),
                                        }));
                                        state.layout.push((
                                            Self::LAYOUT_KIND_THINKING,
                                            block.pos,
                                            block.text_before,
                                        ));
                                    }
                                }
                                BlockKind::Text => {}
                            }
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::MessageDelta { delta, usage }) => {
                        if let Some(sr) = delta.stop_reason.as_deref() {
                            state.stop_reason = FinishReason::from_openai(sr);
                        }
                        if let (Some(u), Some(existing)) = (usage, state.usage.as_mut()) {
                            existing.completion_tokens = u.output_tokens;
                            if u.input_tokens > 0 {
                                existing.prompt_tokens = u.input_tokens;
                            }
                            if u.cache_read_input_tokens > 0 {
                                existing.cached_tokens = u.cache_read_input_tokens;
                            }
                            if u.cache_creation_input_tokens > 0 {
                                existing.cache_creation_tokens = u.cache_creation_input_tokens;
                            }
                            existing.total_tokens = existing.prompt_tokens
                                + existing.cached_tokens
                                + existing.cache_creation_tokens
                                + existing.completion_tokens;
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::MessageStop) => {
                        state.saw_message_stop = true;
                        state.done = true;
                        let mut final_chunk = StreamChunk {
                            text: None,
                            tool_calls: Vec::new(),
                            finish_reason: state.stop_reason,
                            usage: state.usage.take(),
                            model: state.last_model.clone(),
                            reasoning: None,
                            web_search: None,
                            web_search_calls: Vec::new(),
                            thinking_blocks: Vec::new(),
                        };
                        if !state.layout.is_empty() {
                            final_chunk.thinking_blocks.push(json!({
                                Self::LAYOUT_KEY: state.layout
                            }));
                        }
                        Some((Ok(final_chunk), state))
                    }
                    Ok(AnthropicStreamEvent::Ping) | Ok(AnthropicStreamEvent::Other) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::Error { error }) => {
                        let msg = error.message.unwrap_or_default();
                        state.done = true;
                        let err = match error.error_type.as_deref() {
                            Some("overloaded_error") | Some("api_error") => {
                                LlmError::ServerError(msg)
                            }
                            Some("rate_limit_error") => LlmError::RateLimit { retry_after: None },
                            _ => LlmError::RequestFailed(msg),
                        };
                        Some((Err(err), state))
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
