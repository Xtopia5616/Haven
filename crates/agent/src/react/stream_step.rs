//! LLM streaming step: StreamForwarder, StreamSession, call_step_llm.
//!
//! Phase 1 mechanical extract; Phase 5 / E1 wraps the stream behind
//! [`StreamSession`] so the thin loop only consumes [`StepCallOutcome`]
//! and never constructs [`StreamForwarder`].

use super::snapshot_io::RecoveryPersistenceResult;
use super::*;
use crate::types::media_inputs_from_events;
use haven_llm::{EndpointRole, LlmResponse, LlmRouter, StreamAttemptHooks, ToolDefinition};

struct CheckpointRequest {
    session_id: String,
    generation: u64,
    content: String,
}

/// A single bounded, latest-wins checkpoint writer for one stream.
///
/// Streaming callbacks are synchronous, so they cannot await a database
/// write. Keeping the pending request in a one-slot mailbox gives the stream
/// a bounded hand-off while ensuring that a slow write does not make flush
/// await a chain of independent checkpoint tasks. Every request still carries
/// the generation captured before it was published; `PartialStore` performs
/// the authoritative stale-generation check while holding the session lock.
#[derive(Clone)]
struct CheckpointWriter {
    pending: Arc<std::sync::Mutex<Option<CheckpointRequest>>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<tokio::sync::Notify>,
    task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<anyhow::Result<()>>>>>,
    metrics: Arc<ReActMetrics>,
}

impl CheckpointWriter {
    fn new(store: Arc<crate::partial::PartialStore>, metrics: Arc<ReActMetrics>) -> Self {
        let pending = Arc::new(std::sync::Mutex::new(None));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let notify = Arc::new(tokio::sync::Notify::new());
        let pending_task: Arc<std::sync::Mutex<Option<CheckpointRequest>>> = pending.clone();
        let closed_task = closed.clone();
        let notify_task = notify.clone();
        let metrics_task = metrics.clone();
        let task = tokio::spawn(async move {
            loop {
                let request = pending_task.lock().unwrap().take();
                if let Some(request) = request {
                    metrics_task.decrement(MetricsCounter::CheckpointPending);
                    store
                        .checkpoint(&request.session_id, request.generation, &request.content)
                        .await?;
                    continue;
                }
                if closed_task.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(());
                }
                notify_task.notified().await;
            }
        });
        Self {
            pending,
            closed,
            notify,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            metrics,
        }
    }

    fn submit(&self, request: CheckpointRequest) {
        // A later snapshot contains all earlier text, so replacing the one
        // pending request is safe and prevents an unbounded stream backlog.
        let was_empty = {
            let mut pending = self.pending.lock().unwrap();
            let was_empty = pending.is_none();
            *pending = Some(request);
            was_empty
        };
        if was_empty {
            self.metrics.increment(MetricsCounter::CheckpointPending);
        }
        self.notify.notify_one();
    }

    async fn finish(&self) -> anyhow::Result<()> {
        self.closed
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_one();
        let task = self.task.lock().await.take();
        match task.expect("checkpoint writer task must be present").await {
            Ok(result) => result,
            Err(error) => Err(anyhow::anyhow!("stream checkpoint task failed: {error}")),
        }
    }
}

/// Streaming session for one step: primary call + empty/cut-off retries.
/// Owns the effective endpoint role, partial buffers and msg-id reuse; the
/// loop only matches outcomes.
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
    /// Streams from the turn-owned provider request buffer. The durable
    /// canonical projection remains in `ReActState`; only compaction retries
    /// replace that projection and rebuild the provider request.
    pub(super) async fn run(
        &mut self,
        state: &mut ReActState,
        request_context: &RequestContext,
        retry_nudge: Option<&RetryNudge>,
    ) -> StepCallOutcome {
        self.engine
            .call_step_llm(
                self.ctx,
                self.router.clone(),
                &mut self.role,
                self.tools,
                self.cancel.clone(),
                state,
                request_context,
                retry_nudge,
                self.partial_thought,
                self.partial_reasoning,
            )
            .await
    }

    /// Empty / cut-off retry: reuses the primary call's minted msg-ids.
    pub(super) async fn retry(
        &self,
        request_context: &RequestContext,
    ) -> Result<(LlmResponse, u64), haven_llm::LlmError> {
        self.engine
            .stream_llm_call(
                self.ctx,
                self.router.clone(),
                self.role,
                request_context,
                true,
                self.tools,
                self.cancel.clone(),
                self.partial_thought,
                self.partial_reasoning,
            )
            .await
    }

    pub(super) fn role(&self) -> EndpointRole {
        self.role
    }

    /// Promote the current stream scratch into the explicit recovery-only
    /// partial path. Response-policy retries can fail after the first provider
    /// response but before a durable transcript event exists.
    pub(super) async fn persist_partial_on_error(
        &self,
        state: &mut ReActState,
    ) -> RecoveryPersistenceResult {
        self.engine
            .persist_partial_on_error(
                self.ctx,
                state,
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
/// one ordered thought/reasoning queue (see `spawn_chunk_consumer_raw`), the
/// web-search event session, and a stall watchdog that emits `StreamStalled`
/// when the provider goes silent mid-call — the router only aborts at its
/// idle timeout, so without the watchdog the UI would sit frozen with
/// zero feedback during the whole stall window. `flush` drains the
/// batchers and stops the watchdog.
///
/// The `on_chunk` callback accumulates into the partial buffers
/// (checkpointed into `partial_messages` for crash recovery) and forwards
/// text chunks to the frontend. Thought and reasoning chunks share the same
/// queue, so interleaved provider output keeps its arrival order.
struct StreamForwarder {
    chunk_tx: crate::event::ChunkSender,
    ws_tx: tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    consumer: crate::event::ConsumerHandle,
    checkpoint_writer: CheckpointWriter,
    ws_session: tokio::task::JoinHandle<()>,
    watchdog: tokio::task::JoinHandle<()>,
}

impl StreamForwarder {
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    pub(super) fn new(
        metrics: Arc<ReActMetrics>,
        ctx: &StepCtx,
        max_batch_bytes: usize,
        stall_warn_delay_ms: u64,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
        partial_store: Arc<crate::partial::PartialStore>,
        checkpoint_min_chars: usize,
        checkpoint_interval: std::time::Duration,
        cancel: tokio_util::sync::CancellationToken,
        // Minted ids shared with the chunk events, the snap and the final
        // persistence, so the live bubble and the DB row match.
        thought_msg_id: String,
        reasoning_msg_id: String,
    ) -> (
        Self,
        impl FnMut(&haven_llm::StreamChunk) + Send + 'static,
        impl FnMut(bool) + Send + 'static,
    ) {
        let (chunk_tx, consumer_handle) =
            EventDispatcher::spawn_chunk_consumer_raw(&ctx.emitter, max_batch_bytes);
        let chunk_tx_c = chunk_tx.clone();
        let session_id_c = Arc::<str>::from(ctx.session_id.as_str());
        let pt = partial_thought.clone();
        let pr = partial_reasoning.clone();
        let checkpoint_session = ctx.session_id.clone();
        let checkpoint_state = Arc::new(std::sync::Mutex::new((
            std::time::Instant::now() - checkpoint_interval,
            0usize,
        )));
        let checkpoint_writer = CheckpointWriter::new(partial_store.clone(), metrics.clone());
        // Crash/stop recovery: the accumulated thought text is checkpointed
        // into the `partial_messages` scratch table while streaming so a
        // crash, user stop, or app exit does not lose the whole reply. The
        // first chunk checkpoints immediately; afterwards at most every
        // `checkpoint_interval` or every `checkpoint_min_chars` new chars.
        // The writer keeps one pending latest snapshot while a write is in
        // flight. All writes go through the executor's `PartialStore`, which
        // serializes them against promote/discard and drops writes that land
        // after the session was ended/rolled back.
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
        let attempt_generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(
            partial_store.generation(&checkpoint_session),
        ));
        let thought_mid = Arc::<str>::from(thought_msg_id.as_str());
        let reasoning_mid = Arc::<str>::from(reasoning_msg_id.as_str());
        let reset_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reset_pt = pt.clone();
        let reset_pr = pr.clone();
        let reset_checkpoint_state = checkpoint_state.clone();
        let reset_pending_c = reset_pending.clone();
        let reset_last_chunk = last_chunk_ms.clone();
        let reset_attempt_generation = attempt_generation.clone();
        let attempt_session = checkpoint_session.clone();
        let attempt_store = partial_store.clone();
        let on_attempt_start = move |replace_output: bool| {
            if !replace_output {
                return;
            }
            let generation = attempt_store.begin_attempt(&attempt_session);
            reset_attempt_generation.store(generation, std::sync::atomic::Ordering::Release);
            reset_pt.lock().unwrap().clear();
            reset_pr.lock().unwrap().clear();
            let mut state = reset_checkpoint_state.lock().unwrap();
            state.0 = std::time::Instant::now() - checkpoint_interval;
            state.1 = 0;
            drop(state);
            // A replacement attempt is a fresh stall episode. Do not carry
            // the previous attempt's last-delta timestamp into its watchdog.
            reset_last_chunk.store(0, std::sync::atomic::Ordering::Release);
            // Do not enqueue the marker here: a full bounded channel could
            // drop it. The first new delta enqueues Reset before itself and
            // retries until it succeeds, preserving the ordering guarantee.
            reset_pending_c.store(true, std::sync::atomic::Ordering::Release);
        };
        let checkpoint_state_c = checkpoint_state.clone();
        let checkpoint_writer_c = checkpoint_writer.clone();
        let reset_pending_c = reset_pending.clone();
        let attempt_generation_c = attempt_generation;
        let reset_session_id_c = session_id_c.clone();
        let reset_thought_mid_c = thought_mid.clone();
        let reset_reasoning_mid_c = reasoning_mid.clone();
        let first_content_seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let first_content_seen_c = first_content_seen.clone();
        let stream_started = std::time::Instant::now();
        let metrics_c = metrics.clone();
        let on_chunk = move |c: &haven_llm::StreamChunk| {
            metrics_c.increment(MetricsCounter::StreamChunks);
            if (c.text.as_ref().is_some_and(|text| !text.is_empty())
                || c.reasoning.as_ref().is_some_and(|text| !text.is_empty()))
                && !first_content_seen_c.swap(true, std::sync::atomic::Ordering::AcqRel)
            {
                metrics_c.increment(MetricsCounter::FirstTokens);
                metrics_c.observe(MetricsPhase::FirstToken, stream_started.elapsed());
            }
            let stream_ready = if reset_pending_c.load(std::sync::atomic::Ordering::Acquire) {
                let marker = crate::event::ChunkItem::Reset {
                    session_id: reset_session_id_c.clone(),
                    thought_message_id: reset_thought_mid_c.clone(),
                    reasoning_message_id: reset_reasoning_mid_c.clone(),
                    step_number: step_num,
                    run_id,
                };
                if let Err(e) = chunk_tx_c.try_send(marker) {
                    tracing::debug!("stream reset waiting for chunk queue capacity: {}", e);
                    false
                } else {
                    reset_pending_c.store(false, std::sync::atomic::Ordering::Release);
                    true
                }
            } else {
                true
            };
            if let Some(t) = c.text.as_deref() {
                // Single lock scope per chunk: push, read the new length
                // and clone the checkpoint snapshot (when due) under one
                // guard instead of locking up to three times per token.
                let checkpoint_snapshot = {
                    let mut guard = pt.lock().unwrap();
                    guard.push_str(t);
                    let len = guard.len();
                    let now = std::time::Instant::now();
                    let mut checkpoint = checkpoint_state_c.lock().unwrap();
                    let due = now.duration_since(checkpoint.0) >= checkpoint_interval
                        || len.saturating_sub(checkpoint.1) >= checkpoint_min_chars;
                    if due {
                        checkpoint.0 = now;
                        checkpoint.1 = len;
                        Some(guard.clone())
                    } else {
                        None
                    }
                };
                if stream_ready
                    && let Err(e) = chunk_tx_c.try_send(crate::event::ChunkItem::Delta {
                        session_id: session_id_c.clone(),
                        message_id: thought_mid.clone(),
                        delta: t.to_string(),
                        step_number: step_num,
                        run_id,
                        reasoning: false,
                    })
                {
                    metrics_c.increment(MetricsCounter::ChunkDrops);
                    tracing::warn!("thought chunk channel full, dropping: {}", e);
                }
                if let Some(snapshot) = checkpoint_snapshot {
                    // Generation captured BEFORE the write is spawned: if a
                    // promote/discard bumps it while the write is queued, the
                    // PartialStore drops the stale snapshot.
                    let gen_id = attempt_generation_c.load(std::sync::atomic::Ordering::Acquire);
                    checkpoint_writer_c.submit(CheckpointRequest {
                        session_id: checkpoint_session.clone(),
                        generation: gen_id,
                        content: snapshot,
                    });
                }
                last_chunk_c.store(now_millis(), std::sync::atomic::Ordering::Relaxed);
            }
            if let Some(r) = &c.reasoning {
                pr.lock().unwrap().push_str(r);
                if stream_ready
                    && let Err(e) = chunk_tx_c.try_send(crate::event::ChunkItem::Delta {
                        session_id: session_id_c.clone(),
                        message_id: reasoning_mid.clone(),
                        delta: r.clone(),
                        step_number: step_num,
                        run_id,
                        reasoning: true,
                    })
                {
                    metrics_c.increment(MetricsCounter::ChunkDrops);
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
                    result: ws.result.clone(),
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
                ws_tx,
                consumer: consumer_handle,
                checkpoint_writer,
                ws_session,
                watchdog,
            },
            on_chunk,
            on_attempt_start,
        )
    }

    /// Drain every buffered chunk to the frontend (batchers flush on
    /// channel close) and stop the watchdog. Must run once the router
    /// call has returned so no straggler events survive the step.
    pub(super) async fn flush(self) -> anyhow::Result<()> {
        self.watchdog.abort();
        let mut join_error = match self.watchdog.await {
            Ok(()) => None,
            Err(error) if error.is_cancelled() => None,
            Err(error) => Some(anyhow::anyhow!("stream watchdog task failed: {error}")),
        };
        drop(self.chunk_tx);
        drop(self.ws_tx);
        if let Some(handle) = self.consumer
            && let Err(error) = handle.await
        {
            join_error = Some(anyhow::anyhow!(
                "stream chunk consumer task failed: {error}"
            ));
        }
        if let Err(error) = self.ws_session.await {
            join_error.get_or_insert_with(|| {
                anyhow::anyhow!("stream websocket forwarder task failed: {error}")
            });
        }
        // The stream consumer has stopped before this point, so no callback
        // can enqueue another checkpoint. Close the one-slot writer barrier
        // and wait for its current/latest snapshot before the caller projects
        // the final assistant message; otherwise a late checkpoint timestamp
        // could make end-session promotion duplicate a response already
        // persisted as a real message.
        if let Err(error) = self.checkpoint_writer.finish().await {
            tracing::error!(error = %error, "stream checkpoint task failed");
            join_error.get_or_insert(error);
        }
        join_error.map_or(Ok(()), Err)
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
    /// A primary call and every replacement retry use the same lifecycle;
    /// `replace_output_on_start` only controls whether the previous partial
    /// output is discarded before the provider attempt begins.
    #[allow(clippy::too_many_arguments)] // consolidated stream setup; params are read-only
    pub(super) async fn stream_llm_call(
        &self,
        ctx: &StepCtx,
        router: Arc<LlmRouter>,
        role: EndpointRole,
        request_context: &RequestContext,
        replace_output_on_start: bool,
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> Result<(LlmResponse, u64), haven_llm::LlmError> {
        if replace_output_on_start {
            // A replacement stream owns the partial scratch row from this
            // step. Remove it before the new attempt starts so an empty retry
            // cannot leave the previous attempt eligible for end-session
            // promotion.
            self.executor.partials.discard(&ctx.session_id).await;
        }
        // Mint the block ids this call's chunks accumulate into. Reused by
        // the chunk events, the snap and the final persistence of this step.
        let thought_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
        let reasoning_msg_id =
            self.ensure_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
        let limits = self.limits();
        let (forwarder, on_chunk, on_attempt_start) = StreamForwarder::new(
            self.metrics.clone(),
            ctx,
            limits.event_chunk_batch_max_bytes,
            limits.stream_stall_warn_delay_ms,
            partial_thought,
            partial_reasoning,
            self.executor.partials.clone(),
            limits.partial_checkpoint_min_chars,
            std::time::Duration::from_secs(limits.partial_checkpoint_interval_secs),
            cancel.clone(),
            thought_msg_id,
            reasoning_msg_id,
        );
        let started = std::time::Instant::now();
        // RequestContext carries the exact token estimate for its immutable
        // provider-visible copy. Do not fingerprint the cloned request again
        // during stream setup; retries create a new context with its own
        // precise estimate.
        let estimated_input_tokens =
            crate::compactor::estimate_provider_request_tokens_with_message_estimate(
                request_context.messages(),
                tools,
                request_context.message_tokens(),
            );
        let max_output_tokens = router
            .effective_output_tokens(role, estimated_input_tokens)
            .await;
        let result = router
            .chat_stream_with_tools_aggregated_cancellable_with_attempts(
                role,
                request_context.messages(),
                tools,
                StreamAttemptHooks::new(on_chunk, on_attempt_start, replace_output_on_start),
                cancel,
                Some(max_output_tokens),
            )
            .await;
        let duration_ms = started.elapsed().as_millis() as u64;
        if let Err(error) = forwarder.flush().await {
            tracing::error!(
                session_id = %ctx.session_id,
                step = ctx.step_num,
                error = %error,
                "stream forwarding task failed while draining provider output"
            );
            return Err(haven_llm::LlmError::Unknown(error.to_string()));
        }
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

    pub(super) async fn record_step_usage(
        &self,
        ctx: &StepCtx,
        role: EndpointRole,
        response: &LlmResponse,
        duration_ms: u64,
    ) {
        self.record_usage_and_emit(
            &ctx.session_id,
            role,
            response,
            ctx.step_num as i32,
            Some(duration_ms),
            &ctx.emitter,
        )
        .await;
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
        role: &mut EndpointRole,
        tools: &[ToolDefinition],
        cancel: tokio_util::sync::CancellationToken,
        state: &mut ReActState,
        request_context: &RequestContext,
        retry_nudge: Option<&RetryNudge>,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) -> StepCallOutcome {
        match self
            .stream_llm_call(
                ctx,
                router.clone(),
                *role,
                request_context,
                false,
                tools,
                cancel.clone(),
                partial_thought,
                partial_reasoning,
            )
            .await
        {
            Ok((resp, duration_ms)) => {
                self.record_step_usage(ctx, *role, &resp, duration_ms).await;
                StepCallOutcome::Response(Box::new(resp))
            }
            Err(haven_llm::LlmError::ContextLengthExceeded) => {
                tracing::warn!(
                    "context length exceeded for session {}, forcing compaction",
                    ctx.session_id
                );
                let compaction = {
                    let compactor = self.context_compactor(*role).await;
                    compactor
                        .compact(&state.canonical, tools, &self.router(), cancel.clone())
                        .await
                };
                match compaction {
                    Ok(Some(result)) => {
                        tracing::debug!(
                            "compacted {} -> {} tokens",
                            result.tokens_before,
                            result.tokens_after
                        );
                        // Phase 6.1: CompactSummary via apply (emit + persist + replace).
                        self.reset_token_estimate(&ctx.session_id);
                        if let Err(error) = self
                            .apply_transcript(
                                ctx,
                                TranscriptEvent::CompactSummary {
                                    compacted: result.compacted,
                                    media_inputs: media_inputs_from_events(&state.events),
                                    summary: result.summary,
                                    tokens_before: result.tokens_before,
                                    tokens_after: result.tokens_after,
                                    episode_id: result.episode_id,
                                    degraded: result.degraded,
                                },
                                state,
                            )
                            .await
                        {
                            let err_msg = format!("Failed to persist context compaction: {error}");
                            tracing::error!(
                                session_id = %ctx.session_id,
                                step = ctx.step_num,
                                error = %error,
                                "ReAct step cannot continue after compaction projection failure"
                            );
                            let recovery = self
                                .persist_partial_on_error(
                                    ctx,
                                    state,
                                    partial_thought,
                                    partial_reasoning,
                                )
                                .await;
                            if !recovery.should_discard() {
                                tracing::error!(
                                    session_id = %ctx.session_id,
                                    step = ctx.step_num,
                                    ?recovery,
                                    "compaction projection failure also failed recovery persistence"
                                );
                            }
                            EventDispatcher::emit_session_error_from(
                                &ctx.emitter,
                                &ctx.session_id,
                                &err_msg,
                            )
                            .await;
                            self.mark_session_error(&ctx.session_id).await;
                            return StepCallOutcome::Fatal(err_msg);
                        }
                        // Retry streams the *compacted* canonical in place; the
                        // role must be re-resolved: summarizing away the last
                        // image-bearing turn changes routing for the retry.
                        let raw_retry_context = RequestContext::from_state(state, retry_nudge);
                        let retry_role =
                            super::choose_agent_role(&router, &raw_retry_context).await;
                        *role = retry_role;
                        let (retry_context, media_plan) = raw_retry_context.with_capabilities(
                            &router.capability_profile(retry_role),
                            self.media_strategy(),
                        );
                        super::emit_media_plan(
                            &ctx.emitter,
                            &ctx.session_id,
                            ctx.step_num,
                            ctx.run_id,
                            retry_role,
                            media_plan,
                        )
                        .await;
                        match self
                            .stream_llm_call(
                                ctx,
                                router.clone(),
                                retry_role,
                                &retry_context,
                                true,
                                tools,
                                cancel,
                                partial_thought,
                                partial_reasoning,
                            )
                            .await
                        {
                            Ok((retry_resp, retry_duration_ms)) => {
                                self.record_step_usage(
                                    ctx,
                                    retry_role,
                                    &retry_resp,
                                    retry_duration_ms,
                                )
                                .await;
                                StepCallOutcome::Response(Box::new(retry_resp))
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
                                let recovery = self
                                    .persist_partial_on_error(
                                        ctx,
                                        state,
                                        partial_thought,
                                        partial_reasoning,
                                    )
                                    .await;
                                if !recovery.should_discard() {
                                    tracing::error!(
                                        session_id = %ctx.session_id,
                                        step = ctx.step_num,
                                        ?recovery,
                                        "compaction retry failure also failed recovery persistence"
                                    );
                                }
                                self.emit_error(&ctx.emitter, &ctx.session_id, &err_msg)
                                    .await;
                                self.mark_session_error(&ctx.session_id).await;
                                StepCallOutcome::Fatal(err_msg)
                            }
                        }
                    }
                    Ok(None) | Err(haven_llm::LlmError::RequestFailed(_)) => {
                        let err_msg = "context length exceeded but compaction failed".to_string();
                        tracing::error!(
                            "ReAct step {} session {} fatal: {}",
                            ctx.step_num,
                            ctx.session_id,
                            err_msg
                        );
                        let recovery = self
                            .persist_partial_on_error(
                                ctx,
                                state,
                                partial_thought,
                                partial_reasoning,
                            )
                            .await;
                        if !recovery.should_discard() {
                            tracing::error!(
                                session_id = %ctx.session_id,
                                step = ctx.step_num,
                                ?recovery,
                                "compaction failure also failed recovery persistence"
                            );
                        }
                        EventDispatcher::emit_session_error_from(
                            &ctx.emitter,
                            &ctx.session_id,
                            &err_msg,
                        )
                        .await;
                        self.mark_session_error(&ctx.session_id).await;
                        StepCallOutcome::Fatal(err_msg)
                    }
                    Err(haven_llm::LlmError::Cancelled) => StepCallOutcome::Cancelled,
                    Err(error) => {
                        let err_msg =
                            format!("context length exceeded and compaction failed: {error}");
                        tracing::error!(
                            "ReAct step {} session {} fatal: {}",
                            ctx.step_num,
                            ctx.session_id,
                            err_msg
                        );
                        let recovery = self
                            .persist_partial_on_error(
                                ctx,
                                state,
                                partial_thought,
                                partial_reasoning,
                            )
                            .await;
                        if !recovery.should_discard() {
                            tracing::error!(
                                session_id = %ctx.session_id,
                                step = ctx.step_num,
                                ?recovery,
                                "compaction failure also failed recovery persistence"
                            );
                        }
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
            }
            Err(haven_llm::LlmError::Cancelled) => StepCallOutcome::Cancelled,
            Err(e) => {
                let err_msg = format!("Model request failed: {}", e);
                tracing::error!(
                    "ReAct step {} session {} fatal: {}",
                    ctx.step_num,
                    ctx.session_id,
                    err_msg
                );
                let recovery = self
                    .persist_partial_on_error(ctx, state, partial_thought, partial_reasoning)
                    .await;
                if !recovery.should_discard() {
                    tracing::error!(
                        session_id = %ctx.session_id,
                        step = ctx.step_num,
                        ?recovery,
                        "model request failure also failed recovery persistence"
                    );
                }
                EventDispatcher::emit_session_error_from(&ctx.emitter, &ctx.session_id, &err_msg)
                    .await;
                self.mark_session_error(&ctx.session_id).await;
                StepCallOutcome::Fatal(err_msg)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn checkpoint_writer_keeps_latest_pending_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).unwrap());
        let session = db.create_session("input", "").unwrap();
        let store = Arc::new(crate::partial::PartialStore::new(db.clone()));
        let generation = store.generation(&session.id);
        let writer = CheckpointWriter::new(store, Arc::new(ReActMetrics::new()));
        writer.submit(CheckpointRequest {
            session_id: session.id.clone(),
            generation,
            content: "first".to_string(),
        });
        writer.submit(CheckpointRequest {
            session_id: session.id.clone(),
            generation,
            content: "latest".to_string(),
        });
        writer.finish().await.unwrap();

        let session_id = session.id;
        let partial = db
            .run_blocking(move |db| Ok(db.get_partial_message(&session_id)))
            .await
            .unwrap();
        assert_eq!(partial.map(|row| row.0).as_deref(), Some("latest"));
    }
    use async_trait::async_trait;
    use futures_util::stream;
    use haven_common::config::RouterConfig;
    use haven_llm::client::LlmClient;
    use haven_llm::{FinishReason, LlmError, StreamChunk, Usage};
    use haven_memory::Database;
    use haven_tools::ToolsManager;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    struct NoopEmitter;

    #[async_trait]
    impl AgentEventEmitter for NoopEmitter {
        async fn emit(&self, _event: AgentEvent) {}
    }

    enum ProbeResponse {
        Error(LlmError),
        Chunk(StreamChunk),
    }

    struct ProbeClient {
        stream_responses: Mutex<VecDeque<ProbeResponse>>,
        stream_calls: AtomicUsize,
    }

    impl ProbeClient {
        fn new(responses: Vec<ProbeResponse>) -> Self {
            Self {
                stream_responses: Mutex::new(responses.into()),
                stream_calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ProbeClient {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: "Compacted summary.".into(),
                ..Default::default()
            })
        }

        async fn chat_with_tools(
            &self,
            _messages: Vec<CanonicalMessage>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(Vec::new()).await
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "probe: chat_stream not implemented".into(),
            ))
        }

        async fn chat_stream_with_tools(
            &self,
            _messages: Vec<CanonicalMessage>,
            _tools: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.stream_calls.fetch_add(1, Ordering::Relaxed);
            match self.stream_responses.lock().unwrap().pop_front() {
                Some(ProbeResponse::Error(error)) => Err(error),
                Some(ProbeResponse::Chunk(chunk)) => Ok(Box::pin(stream::iter(vec![Ok(chunk)]))),
                None => Err(LlmError::Unknown("probe responses exhausted".into())),
            }
        }

        async fn chat_stream_with_tools_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            tools: Vec<ToolDefinition>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.chat_stream_with_tools(messages, tools).await
        }

        async fn chat_stream_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.chat_stream_with_tools(messages, Vec::new()).await
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn text_message(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    fn image_message() -> CanonicalMessage {
        CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    fn chunk(text: &str, finish_reason: FinishReason) -> StreamChunk {
        StreamChunk {
            text: Some(text.into()),
            finish_reason: Some(finish_reason),
            usage: Some(Usage::default()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn compaction_retry_updates_role_for_followup_retries() {
        let db_path =
            std::env::temp_dir().join(format!("haven_stream_step_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&db_path).unwrap());
        let executor = Arc::new(SessionSupervisor::new(
            db.clone(),
            Arc::new(ToolsManager::new()),
            1,
        ));
        let session = db.create_session("role probe", "role probe").unwrap();
        let default_client = Arc::new(ProbeClient::new(vec![
            ProbeResponse::Chunk(chunk("I will finish", FinishReason::Length)),
            ProbeResponse::Chunk(chunk("Finished.", FinishReason::Stop)),
        ]));
        let image_client = Arc::new(ProbeClient::new(vec![ProbeResponse::Error(
            LlmError::ContextLengthExceeded,
        )]));
        let auxiliary_client = Arc::new(ProbeClient::new(Vec::new()));
        let router = Arc::new(LlmRouter::new_with_clients(
            auxiliary_client.clone(),
            default_client.clone(),
            image_client.clone(),
            auxiliary_client,
        ));
        let engine = ReActEngine::new(
            router.clone(),
            executor,
            db,
            8,
            ContextLimitsConfig::default(),
        );
        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(NoopEmitter);
        let ctx = StepCtx {
            session_id: session.id,
            step_num: 2,
            run_id: 1,
            emitter,
        };
        let partial_thought = Arc::new(Mutex::new(String::new()));
        let partial_reasoning = Arc::new(Mutex::new(String::new()));
        let canonical = vec![
            text_message(CanonicalRole::System, "system"),
            text_message(CanonicalRole::User, "anchor"),
            image_message(),
            text_message(CanonicalRole::User, "recent"),
        ];
        let mut state = ReActState::new(Vec::new(), canonical, HashMap::new());
        let request_context = RequestContext::from_state(&state, None);
        let mut stream = StreamSession::new(
            &engine,
            &ctx,
            router,
            EndpointRole::ImageModel,
            &[],
            CancellationToken::new(),
            &partial_thought,
            &partial_reasoning,
        );

        let outcome = stream.run(&mut state, &request_context, None).await;
        assert!(matches!(outcome, StepCallOutcome::Response(_)));

        let retry_context = RequestContext::from_state(&state, None);
        let (retry, _duration_ms) = stream.retry(&retry_context).await.unwrap();
        assert_eq!(retry.text, "Finished.");
        assert_eq!(image_client.stream_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            default_client.stream_calls.load(Ordering::Relaxed),
            2,
            "both the compaction retry and the follow-up response retry must use the new role"
        );
    }

    #[tokio::test]
    async fn recovery_projection_failure_keeps_scratch_and_writes_failed_marker() {
        let db_path =
            std::env::temp_dir().join(format!("haven_recovery_fault_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&db_path).unwrap());
        let session = db.create_session("input", "input").unwrap();
        let fault_trigger = format!("recovery_fault_{}", uuid::Uuid::new_v4().simple());
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER {fault_trigger}
                 BEFORE INSERT ON messages
                 BEGIN SELECT RAISE(ABORT, 'injected recovery message failure'); END;"
            ))
            .unwrap();

        let executor = Arc::new(SessionSupervisor::new(
            db.clone(),
            Arc::new(ToolsManager::new()),
            1,
        ));
        let router = Arc::new(LlmRouter::new(RouterConfig::default()));
        let engine = ReActEngine::new(
            router,
            executor,
            db.clone(),
            8,
            ContextLimitsConfig::default(),
        );
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 2,
            run_id: 1,
            emitter: Arc::new(NoopEmitter),
        };
        let mut state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());
        let partial_thought = Arc::new(Mutex::new("partial reply".to_string()));
        let partial_reasoning = Arc::new(Mutex::new(String::new()));

        let result = engine
            .persist_partial_on_error(&ctx, &mut state, &partial_thought, &partial_reasoning)
            .await;

        assert!(matches!(
            result,
            RecoveryPersistenceResult::Failed {
                partial_messages: false,
                failure_marker: true,
                ..
            }
        ));
        let marker = engine
            .event_store
            .latest_recovery_persistence(&session.id)
            .unwrap()
            .expect("failed recovery marker must be durable");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&marker.payload).unwrap()["phase"],
            "failed"
        );
        assert!(db.get_session_messages(&session.id).unwrap().is_empty());
        let _ = std::fs::remove_file(db_path);
    }
}

/// Phase 7 / G4: outcome of preparing provider server-side search context
/// before tool / turn-end handling. The thin loop never branches on
/// `web_search_*` fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchContextOutcome {
    /// Search round with no answer yet — context pushed, thought persisted,
    /// branch saved. Loop must `continue`.
    ContinueWithoutTools,
    /// Proceed to tool batch or turn-end. `assistant_already_pushed` is true
    /// when a synthesized final arrived in the same response as the search
    /// (canonical already carries the search context).
    Proceed { assistant_already_pushed: bool },
}

impl ReActEngine {
    /// Phase 7 / G4: push provider server-side search context into the
    /// canonical when the response carries search items and there are no
    /// real tool calls (empty actions or synthesized final only). Mixed
    /// tool+search responses are left for `execute_tool_batch`, which
    /// round-trips the items alongside function tool results.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn prepare_search_context(
        &self,
        ctx: &StepCtx,
        response: &LlmResponse,
        thought: &Option<String>,
        actions: &[Action],
        state: &mut ReActState,
    ) -> anyhow::Result<SearchContextOutcome> {
        if response.web_search_calls.is_empty() {
            return Ok(SearchContextOutcome::Proceed {
                assistant_already_pushed: false,
            });
        }
        let synthesized_final = !actions.is_empty()
            && actions
                .iter()
                .all(|a| a.is_final && a.tool_call_id.is_none());
        if !(actions.is_empty() || synthesized_final) {
            // Mixed real tools + search: tool_batch pushes the search items.
            return Ok(SearchContextOutcome::Proceed {
                assistant_already_pushed: false,
            });
        }

        // Text matches Thought projection (trimmed). X12: apply ToolCall so
        // events + canonical stay on the single writer path.
        let push_text = thought.as_deref().unwrap_or(&response.text);
        let reasoning = if response.thinking_blocks.is_empty() {
            response.reasoning.clone()
        } else {
            None
        };
        // Thought already projected the messages row when present.
        self.apply_transcript(
            ctx,
            TranscriptEvent::ToolCall {
                text: push_text.to_string(),
                tool_calls: Vec::new(),
                reasoning,
                web_search_calls: response.web_search_calls.clone(),
                thinking_blocks: response.thinking_blocks.clone(),
                action_cards: Vec::new(),
                persist_text_id: None,
            },
            state,
        )
        .await?;

        if actions.is_empty() {
            // Search round: no answer yet — keep the turn open and re-request
            // with the search context in the next input.
            self.save_branch_point(&ctx.session_id, state, ctx.step_num, false)
                .await;
            tracing::debug!(
                "ReAct step {} session {} server-side search round ({} item(s)); continuing",
                ctx.step_num,
                ctx.session_id,
                response.web_search_calls.len()
            );
            return Ok(SearchContextOutcome::ContinueWithoutTools);
        }

        // synthesized_final: answer arrived with the search call — fall
        // through to turn-end; the push above keeps search context alive.
        Ok(SearchContextOutcome::Proceed {
            assistant_already_pushed: true,
        })
    }
}
