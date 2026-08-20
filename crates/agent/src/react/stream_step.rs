//! LLM streaming step: StreamForwarder, StreamSession, call_step_llm.
//!
//! Phase 1 mechanical extract; Phase 5 / E1 wraps the stream behind
//! [`StreamSession`] so the thin loop only consumes [`StepResponse`] /
//! [`StepCallOutcome`] and never constructs [`StreamForwarder`].

use super::*;
use haven_llm::{EndpointRole, LlmResponse, LlmRouter, ToolDefinition};

/// Aggregated result of one streamed LLM call (Phase 5 / E1).
/// Currently surfaced via [`StepCallOutcome::Response`]'s `LlmResponse`; the
/// duration is recorded inside `call_step_llm` / usage emit. Kept as the
/// explicit E1 type so future hooks can take it without reshaping the loop.
#[derive(Debug)]
#[allow(dead_code)]
pub(super) struct StepResponse {
    pub response: LlmResponse,
    /// Wall-clock duration of the primary (or compaction-retry) API call.
    pub duration_ms: Option<u64>,
}

/// Streaming session for one step: primary call + empty/cut-off retries.
/// Owns partial buffers and msg-id reuse; the loop only matches outcomes.
pub(super) struct StreamSession<'a> {
    engine: &'a ReActEngine,
    ctx: &'a StepCtx,
    router: Arc<LlmRouter>,
    role: EndpointRole,
    tools: &'a [ToolDefinition],
    cancel: tokio_util::sync::CancellationToken,
    partial_thought: &'a Arc<std::sync::Mutex<String>>,
    partial_reasoning: &'a Arc<std::sync::Mutex<String>>,
}

impl<'a> StreamSession<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        engine: &'a ReActEngine,
        ctx: &'a StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        tools: &'a [ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        partial_thought: &'a Arc<std::sync::Mutex<String>>,
        partial_reasoning: &'a Arc<std::sync::Mutex<String>>,
    ) -> Self {
        Self {
            engine,
            ctx,
            router,
            role,
            tools,
            cancel,
            partial_thought,
            partial_reasoning,
        }
    }

    /// Primary step call including compaction retry / fatal paths.
    pub(super) async fn run(
        &self,
        llm_messages: &mut Vec<CanonicalMessage>,
        canonical: &mut Vec<CanonicalMessage>,
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
    ) -> StepCallOutcome {
        match self
            .engine
            .call_step_llm(
                self.ctx,
                self.router.clone(),
                self.role,
                llm_messages,
                self.tools,
                self.cancel.clone(),
                canonical,
                history,
                branch_points,
                self.partial_thought,
                self.partial_reasoning,
            )
            .await
        {
            StepCallOutcome::Response(resp) => StepCallOutcome::Response(resp),
            other => other,
        }
    }

    /// Empty / cut-off retry: reuses the primary call's minted msg-ids.
    pub(super) async fn retry(
        &self,
        messages: &[CanonicalMessage],
    ) -> Result<LlmResponse, haven_llm::LlmError> {
        self.engine
            .stream_retry_step(
                self.ctx,
                self.router.clone(),
                self.role,
                messages,
                self.tools,
                self.cancel.clone(),
                self.partial_thought,
                self.partial_reasoning,
            )
            .await
    }
}

/// A provider stream that delivers no chunk for this long is announced to the
/// UI as `StreamStalled` — long before the router's idle timeout aborts the
/// stream, so the status chip can show a factual waiting state instead of a
/// frozen conversation. Covers the first-chunk wait too (the anchor starts at
/// the call's creation). Configurable via `context_limits.stream_stall_warn_delay_ms`
/// (was a `STALL_WARN_DELAY_MS` constant before it was unified into settings).
///
/// Current wall-clock time in milliseconds since the Unix epoch. Used by the
/// stall watchdog anchors (the chunk timestamps it compares against).
fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One LLM call's live-chunk forwarding bundle: micro-batched
/// thought/reasoning channels (see `spawn_chunk_consumer_raw`), the
/// web-search event session, and a stall watchdog that emits `StreamStalled`
/// when the provider goes silent mid-call — the router only aborts at its
/// idle timeout, so without the watchdog the UI would sit frozen with
/// zero feedback during the whole stall window. `flush` drains the
/// batchers and stops the watchdog.
///
/// The `on_chunk` callback accumulates into the partial buffers
/// (checkpointed into `partial_messages` for crash recovery) and forwards
/// text chunks to the frontend. Reasoning chunks are always accumulated
/// but only forwarded when `forward_reasoning` is set: the UI reasoning
/// block may already hold the primary generation's reconciled text, and a
/// fresh reasoning pass from a retry would concatenate onto it.
struct StreamForwarder {
    chunk_tx: crate::event::ChunkSender,
    reasoning_tx: crate::event::ChunkSender,
    ws_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    consumer: crate::event::ConsumerHandle,
    ws_session: tokio::task::JoinHandle<()>,
    watchdog: tokio::task::JoinHandle<()>,
}

impl StreamForwarder {
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    pub(super) fn new(
        ctx: &StepCtx,
        max_batch_bytes: usize,
        stall_warn_delay_ms: u64,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
        partial_store: Arc<crate::partial::PartialStore>,
        checkpoint_min_chars: usize,
        checkpoint_interval: std::time::Duration,
        cancel: tokio_util::sync::CancellationToken,
        forward_reasoning: bool,
        // Minted ids shared with the chunk events, the snap and the final
        // persistence, so the live bubble and the DB row match.
        thought_msg_id: String,
        reasoning_msg_id: String,
    ) -> (Self, impl FnMut(&haven_llm::StreamChunk) + Send + 'static) {
        let (chunk_tx, reasoning_tx, consumer_handle) =
            EventDispatcher::spawn_chunk_consumer_raw(&ctx.emitter, max_batch_bytes);
        let chunk_tx_c = chunk_tx.clone();
        let reasoning_tx_c = reasoning_tx.clone();
        let session_id_c = Arc::<str>::from(ctx.session_id.as_str());
        let pt = partial_thought.clone();
        let pr = partial_reasoning.clone();
        let checkpoint_session = ctx.session_id.clone();
        let checkpoint_inflight = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // Crash/stop recovery: the accumulated thought text is checkpointed
        // into the `partial_messages` scratch table while streaming so a
        // crash, user stop, or app exit does not lose the whole reply. The
        // first chunk checkpoints immediately; afterwards at most every
        // `checkpoint_interval` or every `checkpoint_min_chars` new chars,
        // and never while a write is in flight. All writes go through the
        // executor's `PartialStore`, which serializes them against
        // promote/discard and drops writes that land after the session was
        // ended/rolled back.
        let mut checkpoint_at = std::time::Instant::now() - checkpoint_interval;
        let mut checkpoint_len = 0usize;
        let (ws_tx, mut ws_rx) = tokio::sync::mpsc::unbounded_channel();
        let ws_tx_c = ws_tx.clone();
        let em_ws = ctx.emitter.clone();
        let ws_session = tokio::spawn(async move {
            while let Some(event) = ws_rx.recv().await {
                em_ws.emit(event).await;
            }
        });
        let step_num = ctx.step_num;
        let run_id = ctx.run_id;
        let last_chunk_ms = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_chunk_c = last_chunk_ms.clone();
        let thought_mid = Arc::<str>::from(thought_msg_id.as_str());
        let reasoning_mid = Arc::<str>::from(reasoning_msg_id.as_str());
        let on_chunk = move |c: &haven_llm::StreamChunk| {
            if let Some(t) = c.text.as_deref() {
                // Single lock scope per chunk: push, read the new length
                // and clone the checkpoint snapshot (when due) under one
                // guard instead of locking up to three times per token.
                let checkpoint_snapshot = {
                    let mut guard = pt.lock().unwrap();
                    guard.push_str(t);
                    let len = guard.len();
                    let now = std::time::Instant::now();
                    if !checkpoint_inflight.load(std::sync::atomic::Ordering::Relaxed)
                        && (now.duration_since(checkpoint_at) >= checkpoint_interval
                            || len.saturating_sub(checkpoint_len) >= checkpoint_min_chars)
                    {
                        checkpoint_at = now;
                        checkpoint_len = len;
                        Some(guard.clone())
                    } else {
                        None
                    }
                };
                if let Err(e) = chunk_tx_c.try_send((
                    session_id_c.clone(),
                    thought_mid.clone(),
                    t.to_string(),
                    step_num,
                    run_id,
                )) {
                    tracing::warn!("thought chunk channel full, dropping: {}", e);
                }
                if let Some(snapshot) = checkpoint_snapshot {
                    // Generation captured BEFORE the write is spawned: if a
                    // promote/discard bumps it while the write is queued, the
                    // PartialStore drops the stale snapshot.
                    let gen_id = partial_store.generation(&checkpoint_session);
                    let store = partial_store.clone();
                    let tid = checkpoint_session.clone();
                    let flag = checkpoint_inflight.clone();
                    tokio::spawn(async move {
                        store.checkpoint(&tid, gen_id, &snapshot).await;
                        flag.store(false, std::sync::atomic::Ordering::Relaxed);
                    });
                }
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(r) = &c.reasoning {
                pr.lock().unwrap().push_str(r);
                if forward_reasoning
                    && let Err(e) = reasoning_tx_c.try_send((
                        session_id_c.clone(),
                        reasoning_mid.clone(),
                        r.clone(),
                        step_num,
                        run_id,
                    ))
                {
                    tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                }
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(ws) = &c.web_search {
                let _ = ws_tx_c.send(AgentEvent::WebSearch {
                    session_id: session_id_c.to_string(),
                    phase: ws.phase.as_str().to_string(),
                    step_number: step_num,
                    run_id,
                    call_id: ws.call_id.clone(),
                    action: ws.action.clone(),
                });
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
        };
        // Stall watchdog: announce `StreamStalled` once per silent episode
        // (a chunk anchor that produced no traffic for `stall_warn_delay_ms`).
        // The anchor starts at creation so a slow first chunk is covered
        // too; the emitted-anchor sentinel starts at MAX so the no-chunk
        // case (anchor 0) announces exactly once. Aborted by `flush` and
        // by session cancellation.
        let watchdog = {
            let em = ctx.emitter.clone();
            let tid = ctx.session_id.clone();
            let last = last_chunk_ms.clone();
            let created_ms = now_millis();
            let mut emitted_anchor: u64 = u64::MAX;
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => return,
                        _ = tokio::time::sleep(STALL_WATCHDOG_POLL) => {
                            let last_ms = last.load(std::sync::atomic::Ordering::Relaxed);
                            let base = if last_ms == 0 { created_ms } else { last_ms };
                            if now_millis().saturating_sub(base) >= stall_warn_delay_ms
                                && last_ms != emitted_anchor
                            {
                                emitted_anchor = last_ms;
                                em.emit(AgentEvent::StreamStalled {
                                    session_id: tid.clone(),
                                })
                                .await;
                            }
                        }
                    }
                }
            })
        };
        (
            Self {
                chunk_tx,
                reasoning_tx,
                ws_tx,
                consumer: consumer_handle,
                ws_session,
                watchdog,
            },
            on_chunk,
        )
    }

    /// Drain every buffered chunk to the frontend (batchers flush on
    /// channel close) and stop the watchdog. Must run once the router
    /// call has returned so no straggler events survive the step.
    pub(super) async fn flush(self) {
        self.watchdog.abort();
        drop(self.chunk_tx);
        drop(self.reasoning_tx);
        drop(self.ws_tx);
        if let Some(handle) = self.consumer {
            let _ = handle.await;
        }
        let _ = self.ws_session.await;
    }
}

/// Poll interval of the per-call stall watchdog (see `StreamForwarder`).
const STALL_WATCHDOG_POLL: std::time::Duration = std::time::Duration::from_secs(1);

impl ReActEngine {
    /// Run one streamed LLM call for an agent step: spawn the chunk consumer,
    /// forward text/reasoning chunks to the frontend while accumulating them
    /// into the partial buffers (persisted if the step fails mid-stream), then
    /// drain the consumer and return the aggregated response. Shared by the
    /// primary step call and the post-compaction retry so the two cannot
    /// drift. Error handling stays at the call site.
    /// One step's primary LLM call: streamed with live chunk forwarding,
    /// cancellation, the stall watchdog and partial-text recovery. Returns
    /// the aggregated response plus the wall-clock duration of the API call
    /// in milliseconds (persisted with the per-call usage detail).
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    pub(super) async fn stream_llm_step(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        llm_messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<(LlmResponse, u64), haven_llm::LlmError> {
        // Mint the block ids this call's chunks accumulate into. Reused by
        // the chunk events, the snap and the final persistence of this step.
        let thought_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        let reasoning_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
        let (forwarder, on_chunk) = StreamForwarder::new(
            ctx,
            self.context_limits.event_chunk_batch_max_bytes,
            self.context_limits.stream_stall_warn_delay_ms,
            partial_thought,
            partial_reasoning,
            self.executor.partials.clone(),
            self.context_limits.partial_checkpoint_min_chars,
            std::time::Duration::from_secs(self.context_limits.partial_checkpoint_interval_secs),
            cancel.clone(),
            true,
            thought_msg_id,
            reasoning_msg_id,
        );
        let started = std::time::Instant::now();
        let result = router
            .chat_stream_with_tools_aggregated_cancellable(
                role,
                llm_messages,
                tools,
                on_chunk,
                cancel,
            )
            .await;
        let duration_ms = started.elapsed().as_millis() as u64;
        forwarder.flush().await;
        match result {
            Ok(resp) => {
                tracing::debug!(
                    "ReAct step {} session {} LLM stream took {} ms ({} text chars, {} tool_calls)",
                    ctx.step_num,
                    ctx.session_id,
                    duration_ms,
                    resp.text.len(),
                    resp.tool_calls.len()
                );
                Ok((resp, duration_ms))
            }
            Err(e) => Err(e),
        }
    }

    /// One empty-response / cut-off retry with live chunk forwarding: streamed
    /// text is accumulated into the partial buffers (crash recovery) and
    /// forwarded to the frontend, so a recovering provider never leaves the
    /// UI frozen for the retry's whole budget. Reasoning is accumulated but
    /// NOT forwarded (the UI block may already hold the primary generation's
    /// reconciled text; a fresh reasoning pass would concatenate onto it).
    /// The stall watchdog runs exactly like the primary call.
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    pub(super) async fn stream_retry_step(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<LlmResponse, haven_llm::LlmError> {
        // Retry chunks reuse the primary call's minted ids (same step/run),
        // so the frontend continues the same bubble instead of splitting it.
        let thought_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        let reasoning_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
        let (forwarder, on_chunk) = StreamForwarder::new(
            ctx,
            self.context_limits.event_chunk_batch_max_bytes,
            self.context_limits.stream_stall_warn_delay_ms,
            partial_thought,
            partial_reasoning,
            self.executor.partials.clone(),
            self.context_limits.partial_checkpoint_min_chars,
            std::time::Duration::from_secs(self.context_limits.partial_checkpoint_interval_secs),
            cancel.clone(),
            false,
            thought_msg_id,
            reasoning_msg_id,
        );
        let result = router
            .chat_stream_with_tools_aggregated_cancellable(role, messages, tools, on_chunk, cancel)
            .await;
        forwarder.flush().await;
        result
    }

    /// One step's full LLM call, including the context-length compaction
    /// retry and all failure paths. The loop dispatches on the returned
    /// [`StepCallOutcome`] instead of inlining the error handling: a
    /// `Fatal` outcome has already persisted partial text, emitted the error
    /// event and marked the session Error.
    #[allow(clippy::too_many_arguments)] // consolidates ~130 lines of inline error handling
    pub(super) async fn call_step_llm(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        llm_messages: &mut Vec<CanonicalMessage>,
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        canonical: &mut Vec<CanonicalMessage>,
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> StepCallOutcome {
        match self
            .stream_llm_step(
                ctx,
                router.clone(),
                role,
                llm_messages,
                tools,
                cancel.clone(),
                partial_thought,
                partial_reasoning,
            )
            .await
        {
            Ok((resp, duration_ms)) => {
                if router.balanced_model_active() {
                    self.emit_balanced_model(
                        &ctx.emitter,
                        &ctx.session_id,
                        "switching to balanced model",
                    )
                    .await;
                }
                self.record_usage_and_emit(
                    &ctx.session_id,
                    role,
                    &resp,
                    ctx.step_num as i32,
                    Some(duration_ms),
                    &ctx.emitter,
                )
                .await;
                StepCallOutcome::Response(resp)
            }
            Err(haven_llm::LlmError::ContextLengthExceeded) => {
                tracing::warn!(
                    "context length exceeded for session {}, forcing compaction",
                    ctx.session_id
                );
                if let Some(result) = {
                    let compactor = self.context_compactor(role).await;
                    compactor.compact(canonical, &self.router()).await
                } {
                    tracing::debug!(
                        "compacted {} -> {} tokens",
                        result.tokens_before,
                        result.tokens_after
                    );
                    *canonical = result.compacted;
                    // The retry must convert the *compacted* canonical
                    // (the old messages are stale), and the role must be
                    // re-resolved: summarizing away the last image-bearing
                    // turn changes the routing for the retry.
                    *llm_messages = canonical.clone();
                    let retry_role = if canonical_has_image(canonical) {
                        router.vision_role().await
                    } else {
                        EndpointRole::DefaultModel
                    };
                    EventDispatcher::emit_compaction_from(
                        &ctx.emitter,
                        &ctx.session_id,
                        &result.summary,
                        result.tokens_before,
                        result.tokens_after,
                    )
                    .await;
                    self.persist_compaction_summary(&ctx.session_id, &result.summary)
                        .await;
                    // Reset the accumulators: the first attempt's partial
                    // text was based on pre-compaction context and should
                    // not be mixed with the retry's output.
                    partial_thought.lock().unwrap().clear();
                    partial_reasoning.lock().unwrap().clear();
                    match self
                        .stream_llm_step(
                            ctx,
                            router.clone(),
                            retry_role,
                            llm_messages,
                            tools,
                            cancel,
                            partial_thought,
                            partial_reasoning,
                        )
                        .await
                    {
                        Ok((retry_resp, retry_duration_ms)) => {
                            self.record_usage_and_emit(
                                &ctx.session_id,
                                retry_role,
                                &retry_resp,
                                ctx.step_num as i32,
                                Some(retry_duration_ms),
                                &ctx.emitter,
                            )
                            .await;
                            StepCallOutcome::Response(retry_resp)
                        }
                        Err(haven_llm::LlmError::Cancelled) => StepCallOutcome::Cancelled,
                        Err(e2) => {
                            let err_msg = format!("Compaction retry also failed: {}", e2);
                            tracing::error!(
                                "ReAct step {} session {} fatal: {}",
                                ctx.step_num,
                                ctx.session_id,
                                err_msg
                            );
                            self.persist_partial_on_error(
                                ctx,
                                canonical,
                                history,
                                branch_points,
                                partial_thought,
                                partial_reasoning,
                            )
                            .await;
                            self.emit_error(&ctx.emitter, &ctx.session_id, &err_msg)
                                .await;
                            self.mark_session_error(&ctx.session_id).await;
                            StepCallOutcome::Fatal(err_msg)
                        }
                    }
                } else {
                    let err_msg = "context length exceeded but compaction failed".to_string();
                    tracing::error!(
                        "ReAct step {} session {} fatal: {}",
                        ctx.step_num,
                        ctx.session_id,
                        err_msg
                    );
                    self.persist_partial_on_error(
                        ctx,
                        canonical,
                        history,
                        branch_points,
                        partial_thought,
                        partial_reasoning,
                    )
                    .await;
                    EventDispatcher::emit_session_error_from(
                        &ctx.emitter,
                        &ctx.session_id,
                        &err_msg,
                    )
                    .await;
                    self.mark_session_error(&ctx.session_id).await;
                    StepCallOutcome::Fatal(err_msg)
                }
            }
            Err(haven_llm::LlmError::Cancelled) => StepCallOutcome::Cancelled,
            Err(e) => {
                let err_msg = format!("Both default model and balanced model failed: {}", e);
                tracing::error!(
                    "ReAct step {} session {} fatal: {}",
                    ctx.step_num,
                    ctx.session_id,
                    err_msg
                );
                self.persist_partial_on_error(
                    ctx,
                    canonical,
                    history,
                    branch_points,
                    partial_thought,
                    partial_reasoning,
                )
                .await;
                EventDispatcher::emit_session_error_from(&ctx.emitter, &ctx.session_id, &err_msg)
                    .await;
                self.mark_session_error(&ctx.session_id).await;
                StepCallOutcome::Fatal(err_msg)
            }
        }
    }

}
