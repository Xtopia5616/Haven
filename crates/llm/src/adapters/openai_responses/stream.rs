use super::*;

impl OpenAiResponsesAdapter {
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
        let mut body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            true,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let url = self.responses_url();
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

        let resp = self.send_request(&url, &mut body, true).await?;
        let cache_diagnostics = body.cache_diagnostics;

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseDataOnly);

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<Result<String, LlmError>>,
            done: bool,
            /// Function calls accumulated per item id; flushed in the final
            /// chunk. Tuple: (lookup key for argument deltas, resolved call
            /// id, name, raw argument JSON fragments parsed at flush time).
            tool_calls: Vec<(String, String, String, String)>,
            accumulated_text: String,
            last_model: Option<String>,
            finish_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_completed: bool,
            /// Raw `web_search_call` items seen while streaming; flushed in
            /// the final chunk for round-tripping into the next request.
            web_search_calls: Vec<Value>,
            /// Most recent `web_search_call` id from `output_item.added`,
            /// used when a status event omits `item_id`.
            active_web_search_id: Option<String>,
            cache_diagnostics: CacheDiagnostics,
        }

        let empty_chunk = empty_chunk;

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                tool_calls: Vec::new(),
                accumulated_text: String::new(),
                last_model: None,
                finish_reason: None,
                usage: None,
                saw_completed: false,
                web_search_calls: Vec::new(),
                active_web_search_id: None,
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
                        let unfinished_tools = state.tool_calls.iter().any(|(_, _, name, args)| {
                            CanonicalToolCall::stream_tool_args_unfinished(name, args)
                        });
                        let chunk = if !state.saw_completed
                            && (!state.accumulated_text.is_empty() || unfinished_tools)
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: state
                                    .tool_calls
                                    .drain(..)
                                    .map(|(_, id, name, args)| CanonicalToolCall {
                                        id,
                                        name,
                                        arguments: CanonicalToolCall::from_wire_args(&args),
                                    })
                                    .collect(),
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: std::mem::take(&mut state.web_search_calls),
                                thinking_blocks: Vec::new(),
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<ResponsesStreamEvent, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(ResponsesStreamEvent::OutputTextDelta { delta }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(d) = delta {
                            state.accumulated_text.push_str(&d);
                            chunk.text = Some(d);
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::ReasoningTextDelta { delta }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        // Forward each reasoning delta live so the UI can render
                        // thinking while it streams (matches the chat-completions
                        // adapter's per-delta `reasoning_content`); the router
                        // aggregates the deltas into the final response for the
                        // provider echo-back (`convert_input`).
                        if let Some(d) = delta {
                            chunk.reasoning = Some(d);
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::FunctionCallArgsDelta { item_id, delta }) => {
                        if let (Some(id), Some(d)) = (item_id, delta)
                            && let Some((_, _, _, args)) = state
                                .tool_calls
                                .iter_mut()
                                .find(|(tid, _, _, _)| tid == &id)
                        {
                            args.push_str(&d);
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::OutputItemAdded { item }) => {
                        if let Some(item) = item
                            && let Some(item_type) = item.item_type.as_deref()
                        {
                            match item_type {
                                "function_call" => {
                                    if let Some(id) = item.id.clone() {
                                        state.tool_calls.push((
                                            id,
                                            item.call_id.or(item.id).unwrap_or_default(),
                                            item.name.unwrap_or_default(),
                                            item.arguments.unwrap_or_default(),
                                        ));
                                    }
                                }
                                "web_search_call" => {
                                    let call_id = item.id.clone();
                                    let action = web_search_action_of(&item);
                                    if let Some(id) = call_id.clone() {
                                        state.active_web_search_id = Some(id);
                                    }
                                    upsert_web_search_call(
                                        &mut state.web_search_calls,
                                        normalize_web_search_call_item(
                                            serde_json::to_value(&item).unwrap_or_default(),
                                        ),
                                    );
                                    let mut chunk = empty_chunk();
                                    chunk.model = state.last_model.clone();
                                    // Announce only when keyed: a null-id card
                                    // would later collide with the real ws_* id.
                                    if call_id.is_some() {
                                        chunk.web_search = Some(
                                            WebSearchUpdate::new(WebSearchPhase::InProgress)
                                                .with_meta(call_id, action),
                                        );
                                    }
                                    return Some((Ok(chunk), state));
                                }
                                _ => {}
                            }
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::OutputItemDone { item }) => {
                        if let Some(item) = item
                            && let Some(item_type) = item.item_type.as_deref()
                        {
                            if item_type == "function_call" {
                                if let Some(item_id) = item.id.clone() {
                                    let call_id =
                                        item.call_id.clone().unwrap_or_else(|| item_id.clone());
                                    if let Some((_, resolved_id, name, args)) = state
                                        .tool_calls
                                        .iter_mut()
                                        .find(|(lookup_id, _, _, _)| lookup_id == &item_id)
                                    {
                                        *resolved_id = call_id;
                                        if let Some(item_name) = item.name {
                                            *name = item_name;
                                        }
                                        if let Some(arguments) = item.arguments {
                                            *args = arguments;
                                        }
                                    } else {
                                        state.tool_calls.push((
                                            item_id,
                                            call_id,
                                            item.name.unwrap_or_default(),
                                            item.arguments.unwrap_or_default(),
                                        ));
                                    }
                                }
                                let mut chunk = empty_chunk();
                                chunk.model = state.last_model.clone();
                                return Some((Ok(chunk), state));
                            }
                            // The authoritative `web_search_call` payload
                            // (`action`/`queries`): replace the in-progress
                            // skeleton captured from `output_item.added`, or
                            // record the item when no skeleton arrived.
                            if item_type == "web_search_call" {
                                let call_id = item.id.clone();
                                let action = web_search_action_of(&item);
                                let normalized = normalize_web_search_call_item(
                                    serde_json::to_value(&item).unwrap_or_default(),
                                );
                                let result = web_search_result_of(&normalized);
                                upsert_web_search_call(&mut state.web_search_calls, normalized);
                                // Keep `active_web_search_id` until the next
                                // `output_item.added` overwrites it: a late
                                // status event that omits `item_id` must still
                                // resolve to this call instead of emitting a
                                // null-id UI update (which would open a second
                                // card).
                                if call_id.is_some() {
                                    state.active_web_search_id = call_id.clone();
                                }
                                let mut chunk = empty_chunk();
                                chunk.model = state.last_model.clone();
                                // Re-emit completed with the action so the UI
                                // can switch "正在联网搜索…" → "已打开网页"
                                // once the discriminator arrives; the compact
                                // result payload rides along as the tool return.
                                chunk.web_search = Some(
                                    WebSearchUpdate::new(WebSearchPhase::Completed)
                                        .with_meta(call_id, action)
                                        .with_result(result),
                                );
                                return Some((Ok(chunk), state));
                            }
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::WebSearchInProgress { item_id }) => {
                        let call_id = item_id.or_else(|| state.active_web_search_id.clone());
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        // Skip unkeyed updates: a null call_id would create a
                        // placeholder card that later collides with the real id.
                        if let Some(call_id) = call_id {
                            let action = web_search_action_by_id(&state.web_search_calls, &call_id);
                            chunk.web_search = Some(
                                WebSearchUpdate::new(WebSearchPhase::InProgress)
                                    .with_meta(Some(call_id), action),
                            );
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::WebSearchSearching { item_id }) => {
                        let call_id = item_id.or_else(|| state.active_web_search_id.clone());
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(call_id) = call_id {
                            let action = web_search_action_by_id(&state.web_search_calls, &call_id);
                            chunk.web_search = Some(
                                WebSearchUpdate::new(WebSearchPhase::Searching)
                                    .with_meta(Some(call_id), action),
                            );
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::WebSearchCompleted { item_id, item }) => {
                        let mut action = None;
                        let mut call_id = item_id;
                        let mut result = None;
                        if let Some(item) = item
                            && item.item_type.as_deref() == Some("web_search_call")
                        {
                            if call_id.is_none() {
                                call_id = item.id.clone();
                            }
                            action = web_search_action_of(&item);
                            let normalized = normalize_web_search_call_item(
                                serde_json::to_value(&item).unwrap_or_default(),
                            );
                            result = web_search_result_of(&normalized);
                            upsert_web_search_call(&mut state.web_search_calls, normalized);
                        }
                        let call_id = call_id.or_else(|| state.active_web_search_id.clone());
                        if action.is_none()
                            && let Some(id) = call_id.as_deref()
                        {
                            action = web_search_action_by_id(&state.web_search_calls, id);
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if call_id.is_some() {
                            if let Some(id) = call_id.clone() {
                                state.active_web_search_id = Some(id);
                            }
                            chunk.web_search = Some(
                                WebSearchUpdate::new(WebSearchPhase::Completed)
                                    .with_meta(call_id, action)
                                    .with_result(result),
                            );
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(
                        event @ (ResponsesStreamEvent::Completed { .. }
                        | ResponsesStreamEvent::Incomplete { .. }),
                    ) => {
                        let incomplete = matches!(&event, ResponsesStreamEvent::Incomplete { .. });
                        let response = match event {
                            ResponsesStreamEvent::Completed { response }
                            | ResponsesStreamEvent::Incomplete { response } => response,
                            _ => unreachable!("guarded response terminal event"),
                        };
                        state.saw_completed = true;
                        let mut completed_text = None;
                        if let Some(resp) = response {
                            completed_text =
                                append_completed_output(&mut state.accumulated_text, &resp.output);
                            if let Some(m) = &resp.model {
                                state.last_model = Some(m.clone());
                            }
                            if let Some(u) = resp.usage {
                                let mut usage = u.to_usage(state.last_model.clone());
                                usage.cache_diagnostics = Some(
                                    state
                                        .cache_diagnostics
                                        .clone()
                                        .with_provider_usage(usage.cached_tokens),
                                );
                                state.usage = Some(usage);
                            }
                            if let Some(status) = resp.status.as_deref() {
                                state.finish_reason = Self::finish_reason_of(status);
                            } else if incomplete {
                                state.finish_reason = Some(FinishReason::Length);
                            }
                        } else if incomplete {
                            state.finish_reason = Some(FinishReason::Length);
                        }
                        state.done = true;
                        let final_chunk = StreamChunk {
                            text: completed_text,
                            tool_calls: state
                                .tool_calls
                                .drain(..)
                                .map(|(_, id, name, args)| CanonicalToolCall {
                                    id,
                                    name,
                                    arguments: CanonicalToolCall::from_wire_args(&args),
                                })
                                .collect(),
                            finish_reason: state.finish_reason,
                            usage: state.usage.take(),
                            model: state.last_model.clone(),
                            reasoning: None,
                            web_search: None,
                            web_search_calls: std::mem::take(&mut state.web_search_calls),
                            thinking_blocks: Vec::new(),
                        };
                        // Keep the output shape identical for the router: the
                        // completed payload contributes a final text delta,
                        // while tool calls and usage remain in this terminal
                        // chunk as before.
                        Some((Ok(final_chunk), state))
                    }
                    Ok(ResponsesStreamEvent::Failed { response }) => {
                        let msg = response
                            .and_then(|r| r.error)
                            .map(|e| serde_json::to_string(&e).unwrap_or_default())
                            .unwrap_or_else(|| "response failed".into());
                        state.done = true;
                        Some((Err(LlmError::RequestFailed(msg)), state))
                    }
                    Ok(ResponsesStreamEvent::Error { message, .. }) => {
                        state.done = true;
                        let msg = message.unwrap_or_else(|| "stream error".into());
                        Some((Err(LlmError::RequestFailed(msg)), state))
                    }
                    Ok(ResponsesStreamEvent::Other) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
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
pub(super) fn append_completed_output(
    accumulated: &mut String,
    output: &[ResponsesItem],
) -> Option<String> {
    let full = completed_output_text(output);
    if full.is_empty() || full == *accumulated {
        return None;
    }
    if let Some(suffix) = full.strip_prefix(accumulated.as_str()) {
        if suffix.is_empty() {
            return None;
        }
        accumulated.push_str(suffix);
        return Some(suffix.to_string());
    }
    // A shorter completion is stale relative to already streamed output. A
    // divergent completion cannot be represented as an append-only delta, so
    // leave the stream's accumulated text intact and make the discrepancy
    // observable for provider-specific follow-up.
    if accumulated.starts_with(full.as_str()) {
        return None;
    }
    tracing::warn!(
        streamed_chars = accumulated.len(),
        completed_chars = full.len(),
        "Responses completed output diverges from streamed text"
    );
    None
}

pub(super) fn completed_output_text(output: &[ResponsesItem]) -> String {
    let mut text = String::new();
    for item in output {
        if item.item_type.as_deref() != Some("message") {
            continue;
        }
        for part in &item.content {
            if let Some(t) = &part.text {
                text.push_str(t);
            }
        }
    }
    text
}
