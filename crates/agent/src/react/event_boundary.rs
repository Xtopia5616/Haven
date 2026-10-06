//! Event-boundary / branch / pause persistence helpers for the ReAct loop.
//!
//! Owns durable event-boundary checks and lifecycle exits for the shared
//! [`ReActState`]; the loop modules delegate here instead of carrying their
//! own persistence argument lists.

use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use super::identity::StreamBlockIdentity;
use super::{PauseReason, StepCtx, *};
use crate::types::{BranchPoint, TranscriptRecord};
use haven_memory::{RecoveryPersistenceStatus, SessionCursor, SessionEventInput};

/// The durable event-derived session state used by resume and rollback.
///
/// This value contains only event replay data and projection clocks. It is
/// never serialized; the event stream is the recovery authority.
#[derive(Debug)]
pub(crate) struct DurableEventState {
    pub(crate) events: Vec<TranscriptRecord>,
    pub(crate) branch_points: HashMap<u32, BranchPoint>,
    pub(crate) cursor: SessionCursor,
}

/// Outcome of the recovery-only persistence repair after a failed provider
/// turn.  The scratch partial is safe to discard only when every durable
/// representation needed by Continue has been committed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryPersistenceResult {
    Persisted,
    Failed {
        branch_point: bool,
        partial_messages: bool,
        event_boundary: bool,
        projection: bool,
        /// Whether the terminal `failed` marker itself was committed to the
        /// durable event stream. If false, the database was unavailable even
        /// for the protocol marker and the scratch must remain authoritative.
        failure_marker: bool,
    },
}

impl RecoveryPersistenceResult {
    pub(super) fn should_discard(&self) -> bool {
        matches!(self, Self::Persisted)
    }
}

/// Inputs for a pause boundary. Keeping this boundary named prevents the
/// lifecycle writer from growing another positional-argument list.
pub(super) struct PauseTurnInput<'a> {
    pub(super) session_id: &'a str,
    pub(super) state: &'a mut ReActState,
    pub(super) boundary_step: u32,
    pub(super) emitter: &'a Arc<dyn AgentEventEmitter>,
    pub(super) status: SessionStatus,
    pub(super) waiting_reason: Option<haven_common::SessionWaitingReason>,
    pub(super) branch_point_step: Option<u32>,
    pub(super) run_id: u64,
    pub(super) reason: PauseReason,
}

/// Update a session's status and emit the `SessionUpdated` event, in that order.
/// Shared by every status-transition path (pause, budget pause, agent layer)
/// so the pair cannot drift.
pub(crate) async fn set_status_and_emit(
    executor: &SessionSupervisor,
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: &str,
    status: SessionStatus,
) -> anyhow::Result<()> {
    set_status_and_emit_with_waiting_reason(executor, emitter, session_id, status, None).await
}

/// Update a session and publish the derived paused reason in the same event.
/// The explicit value is used for boundaries such as step-budget exhaustion;
/// ordinary pauses derive from the interaction/ToolRun registry.
pub(crate) async fn set_status_and_emit_with_waiting_reason(
    executor: &SessionSupervisor,
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: &str,
    status: SessionStatus,
    explicit_reason: Option<haven_common::SessionWaitingReason>,
) -> anyhow::Result<()> {
    tracing::debug!("session {} status -> {}", session_id, status.as_str());
    if executor.update_session_status(session_id, status).await? {
        let waiting_reason = if status == SessionStatus::Paused {
            let reason = explicit_reason.or(executor.waiting_reason(session_id).await);
            executor.set_waiting_reason(session_id, reason).await?;
            reason
        } else {
            None
        };
        emitter
            .emit(crate::event::AgentEvent::SessionUpdated {
                session_id: session_id.into(),
                status,
                waiting_reason,
                reason: None,
            })
            .await;
    }
    Ok(())
}

/// Interval (in ReAct steps) at which long-running sessions re-run fact
/// fact memory mid-session, so memory is refreshed before the session
/// ever pauses or completes.
/// Message persisted when a run exhausts its step budget (`max_steps`). The
/// session is intentionally paused at an event boundary —the session is NOT finished,
/// and the next user message resumes it with a fresh budget. System notices
/// like this must NOT land in the chat as an assistant bubble; they are
/// surfaced as a notification (in-app toast + Windows) instead.
const BUDGET_EXHAUSTED_TITLE: &str = "任务步骤上限已用尽";

const BUDGET_EXHAUSTED_BODY: &str = "本轮运行的步骤上限已用完，任务已暂停。发一条消息即可继续。";

impl ReActEngine {
    /// Load the active transcript and branch metadata from the durable event
    /// stream in one SessionStore read. `None` means that no durable event log
    /// exists; once any control or transcript event exists, no cache is
    /// consulted for transcript state.
    pub(crate) async fn load_durable_event_state(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<DurableEventState>> {
        let Some(replay) = self.event_store.load_replay_state_async(session_id).await? else {
            return Ok(None);
        };
        let events = replay
            .transcript
            .into_iter()
            .map(|event| {
                serde_json::from_str::<TranscriptRecord>(&event.payload).map_err(|error| {
                    anyhow::anyhow!(
                        "invalid transcript event {} for session {}: {}",
                        event.sequence,
                        session_id,
                        error
                    )
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let branch_points = replay
            .branch_points
            .into_iter()
            .map(|branch_point| {
                (
                    branch_point.step_number,
                    BranchPoint {
                        event_cursor: branch_point.event_cursor,
                        step_number: branch_point.step_number,
                        last_msg_at: branch_point.last_msg_at,
                    },
                )
            })
            .collect();
        Ok(Some(DurableEventState {
            events,
            branch_points,
            cursor: replay.cursor,
        }))
    }

    pub(crate) fn transcript_event_input(
        event: &TranscriptRecord,
        run_id: u64,
    ) -> anyhow::Result<SessionEventInput> {
        let step_number = match event {
            TranscriptRecord::Thought { step_number, .. }
            | TranscriptRecord::Reasoning { step_number, .. }
            | TranscriptRecord::ToolCall { step_number, .. }
            | TranscriptRecord::ToolResult { step_number, .. }
            | TranscriptRecord::UserInject { step_number, .. }
            | TranscriptRecord::MediaPlan { step_number, .. } => *step_number,
            TranscriptRecord::CompactSummary { step_number, .. } => *step_number,
        };
        Ok(SessionEventInput::transcript(
            serde_json::to_string(event)?,
            run_id,
            step_number,
        ))
    }

    /// Seed the durable event log for a fresh session before the first model
    /// request. Resume never calls this helper: an existing event stream must
    /// already have a durable event log or it requires a reset.
    pub(crate) async fn seed_transcript_events(
        &self,
        session_id: &str,
        events: &[TranscriptRecord],
        run_id: u64,
    ) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let inputs = events
            .iter()
            .map(|event| Self::transcript_event_input(event, run_id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.event_store
            .seed_if_empty_async(session_id, inputs)
            .await?;
        Ok(())
    }

    /// Append one transcript record to the durable event authority. The
    /// returned sequence is useful for boundary metadata, while the hot
    /// ReAct projection continues to use the record itself.
    pub(super) async fn append_transcript_record(
        &self,
        session_id: &str,
        record: &TranscriptRecord,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<i64> {
        let _timer = self
            .metrics
            .start(MetricsPhase::EventAppend, session_id, run_id, step_number);
        let payload = serde_json::to_string(record)?;
        self.event_store
            .append_transcript_async(session_id, &payload, run_id, step_number)
            .await
    }

    /// Recovery-only write used by `persist_partial_on_error` (intentionally
    /// outside the event log — see transcript module docs).
    pub(super) async fn persist_recovery_partial(
        &self,
        session_id: &str,
        content: &str,
        kind: haven_memory::repositories::session_events::RecoveryPartialKind,
        message_id: &str,
    ) -> anyhow::Result<()> {
        self.executor
            .session_store()
            .persist_recovery_partial(session_id, content, kind, message_id)
            .await?;
        Ok(())
    }

    /// Persist a compaction summary into episodic long-term memory
    /// (`memory_items`) so context that compaction summarized away stays
    /// retrievable across sessions (embedding + keyword recall). The episode
    /// and its optional summary-extraction marker are committed together so a
    /// process exit cannot leave a durable episode without its job marker.
    pub(super) async fn persist_compaction_summary(
        &self,
        session_id: &str,
        summary: &str,
        episode_id: &str,
    ) {
        let summary = summary.trim();
        if summary.is_empty() {
            return;
        }
        let session_id = session_id.to_string();
        let episode_id = episode_id.to_string();
        let enqueue_extraction = summary.len() >= 24;
        if let Err(e) = self
            .memory_store
            .persist_compaction_summary(&session_id, summary, &episode_id, enqueue_extraction)
            .await
        {
            tracing::warn!(
                "ReAct: failed to persist compaction summary for session {}: {}",
                session_id,
                e
            );
            return;
        }
        // M3: light fact extraction from the compaction summary (throttled,
        // separate episode cursor — does not advance the user-message cursor).
        if enqueue_extraction && let Some(ref memory_worker) = self.memory_worker {
            memory_worker.wake_summary_extract(&session_id, &episode_id);
        }
    }

    /// Finalize a turn: save the branch point (when requested), verify the
    /// durable event boundary, then mark the session with the given status and notify the
    /// frontend + memory worker.
    ///
    /// X12: chat content must already be projected via `apply_transcript`
    /// before this call. The pause path only verifies the durable event
    /// boundary and changes the
    /// lifecycle; it never writes a second assistant message.
    pub(super) async fn pause_turn(&self, input: PauseTurnInput<'_>) -> anyhow::Result<()> {
        let PauseTurnInput {
            session_id,
            state,
            boundary_step,
            emitter,
            status,
            waiting_reason,
            branch_point_step,
            run_id,
            reason,
        } = input;
        let status_label = status.as_str();
        async {
            tracing::info!(
                "ReAct turn finished: session={} step={} status={}",
                session_id,
                boundary_step,
                status_label
            );
            if let Some(step) = branch_point_step {
                self.save_branch_point(session_id, state, step, false)
                    .await?;
            }
            if !self
                .ensure_event_boundary(session_id, state, boundary_step)
                .await
            {
                anyhow::bail!(
                    "failed to durably record event boundary for session '{}' at step {}",
                    session_id,
                    boundary_step
                );
            }
            // A synchronous resolve may have already woken a confirm batch.
            // Do not overwrite that Pending transition with a stale pause.
            if self.executor.get_active_session_status(session_id).await
                != Some(SessionStatus::Pending)
            {
                set_status_and_emit_with_waiting_reason(
                    &self.executor,
                    emitter,
                    session_id,
                    status,
                    waiting_reason,
                )
                .await?;
            }
            let ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num: boundary_step,
                run_id,
                emitter: emitter.clone(),
            };
            if let Some(memory_trigger) = self.hooks.on_pause(self, &ctx, reason).await {
                crate::memory_trigger::append_memory_trigger_nonfatal(
                    self.event_store.clone(),
                    session_id,
                    memory_trigger,
                    CancellationToken::new(),
                )
                .await;
            }
            Ok(())
        }
        .instrument(tracing::info_span!(
            "pause",
            session_id,
            step = boundary_step,
            status = status_label
        ))
        .await
    }

    /// Pause the session because the run exhausted its step budget. Mirrors
    /// `pause_turn`'s event-boundary side effects (Paused status,
    /// infer) but does NOT persist an assistant chat message: system notices
    /// of this kind must not pollute the conversation stream as fake agent
    /// replies —they are surfaced as a notification (in-app toast +
    /// Windows) instead, so the user sees them without the chat pretending
    /// the turn produced an answer.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn pause_turn_budget(
        &self,
        session_id: &str,
        state: &ReActState,
        boundary_step: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> anyhow::Result<()> {
        // Phase 7 / I2: pause span for budget exhaustion (no assistant persist).
        async {
            tracing::info!(
                "ReAct step budget exhausted: session={} next_step={}",
                session_id,
                boundary_step
            );
            if !self
                .ensure_event_boundary(session_id, state, boundary_step)
                .await
            {
                anyhow::bail!(
                    "failed to durably record event boundary for session '{}' after step-budget exhaustion",
                    session_id
                );
            }
            set_status_and_emit_with_waiting_reason(
                &self.executor,
                emitter,
                session_id,
                SessionStatus::Paused,
                Some(haven_common::SessionWaitingReason::StepBudget),
            )
            .await?;
            emitter
                .emit(crate::event::AgentEvent::Notification {
                    session_id: Some(session_id.into()),
                    title: BUDGET_EXHAUSTED_TITLE.into(),
                    body: BUDGET_EXHAUSTED_BODY.into(),
                })
                .await;
            let ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num: boundary_step,
                run_id,
                emitter: emitter.clone(),
            };
            if let Some(memory_trigger) = self
                .hooks
                .on_pause(self, &ctx, PauseReason::Budget)
                .await
            {
                crate::memory_trigger::append_memory_trigger_nonfatal(
                    self.event_store.clone(),
                    session_id,
                    memory_trigger,
                    CancellationToken::new(),
                )
                .await;
            }
            Ok(())
        }
        .instrument(tracing::info_span!(
            "pause",
            session_id,
            step = boundary_step,
            reason = "budget"
        ))
        .await
    }

    /// Verify one final event boundary before leaving the loop on a cancellation,
    /// so the DB row is never stale when `rollback_session` / `continue_session`
    /// read it after the handler exits. The mid-run throttle in
    /// `save_branch_point` may have skipped the last write, and the state at
    /// this point is always a clean step boundary (the cancelled response or
    /// partial tool results are discarded by the exit).
    pub(super) async fn check_event_boundary(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.ensure_event_boundary(session_id, state, step_number)
            .await
    }

    /// Phase 7 / C4: single cancel-exit path — verify the event boundary then
    /// return [`LoopExit::Cancelled`]. All cancel sites in the thin loop /
    /// tool batch must go through this helper.
    pub(super) async fn exit_cancelled(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> LoopExit {
        self.exit_at_boundary(session_id, state, step_number, LoopExit::Cancelled)
            .await
    }

    /// Verify the event boundary then return `exit`. Used by Completed / Error /
    /// Cancelled so every lifecycle exit advances the durable clocks.
    pub(super) async fn exit_at_boundary(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        exit: LoopExit,
    ) -> LoopExit {
        if self
            .check_event_boundary(session_id, state, step_number)
            .await
        {
            exit
        } else {
            LoopExit::Error(format!(
                "failed to durably record event boundary for session '{}' before exit",
                session_id
            ))
        }
    }

    /// Shared External-pause exit (step-head and mid-batch): boundary check →
    /// typed pause trigger → `LoopExit::Paused`.
    pub(super) async fn exit_external_pause(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> LoopExit {
        if !self
            .ensure_event_boundary(session_id, state, step_number)
            .await
        {
            return LoopExit::Error(format!(
                "failed to durably record event boundary for session '{}' before pause",
                session_id
            ));
        }
        let ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num: step_number,
            run_id,
            emitter: emitter.clone(),
        };
        if let Some(memory_trigger) = self.hooks.on_pause(self, &ctx, PauseReason::External).await {
            crate::memory_trigger::append_memory_trigger_nonfatal(
                self.event_store.clone(),
                session_id,
                memory_trigger,
                CancellationToken::new(),
            )
            .await;
        }
        LoopExit::Paused {
            reason: PauseReason::External,
        }
    }

    /// Verify the event stream and projection clocks, including rollback points.
    ///
    /// Serializes a borrowed view of the ReAct state (no per-step deep copies
    /// of events/branch_points — those clones were O(n²) over a long session)
    /// into a reusable buffer, then writes to SQLite on the blocking thread
    /// pool so the WAL fsync never stalls the async runtime.
    pub(super) async fn ensure_event_boundary(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.ensure_event_boundary_with_error_partials(session_id, state, step_number, None, false)
            .await
    }

    /// Verify a completed tool batch before the loop makes another LLM
    /// request. This closes the crash window where `session_steps` and chat
    /// rows already contain tool results but the durable event boundary still ends
    /// at the assistant's unanswered tool call.
    pub(super) async fn ensure_event_boundary_after_tool_results(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.ensure_event_boundary_with_error_partials(session_id, state, step_number, None, false)
            .await
    }

    /// Same boundary check as [`Self::ensure_event_boundary_after_tool_results`], but the
    /// completed confirmation batch must not remain resumable. Writing the
    /// result events and clearing confirm interactions in one boundary prevents
    /// a crash between result projection and the in-memory gate cleanup from
    /// replaying an already executed side effect.
    pub(super) async fn ensure_event_boundary_after_confirm_results(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.ensure_event_boundary_with_error_partials(session_id, state, step_number, None, true)
            .await
    }

    /// Persist recovery-only rows carrying the marker for a failed LLM
    /// response. Normal boundaries pass `None`, which clears any marker
    /// consumed by a prior Continue. `Some(&[])` still marks an error whose
    /// stream produced no visible partial text.
    async fn ensure_event_boundary_with_error_partials(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        error_partial_message_ids: Option<&[String]>,
        clear_confirm_interactions: bool,
    ) -> bool {
        let result = self
            .read_event_boundary_with_error_partials(
                session_id,
                state,
                step_number,
                error_partial_message_ids,
                clear_confirm_interactions,
            )
            .await;
        if !result {
            self.metrics.increment(MetricsCounter::SnapshotFailures);
        }
        result
    }

    async fn read_event_boundary_with_error_partials(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        _error_partial_message_ids: Option<&[String]>,
        _clear_confirm_interactions: bool,
    ) -> bool {
        // Some lifecycle callers do not carry a provider run id (for example
        // a pause boundary). There is no serialized ReAct state to write:
        // the event stream and atomic projections are already durable. Read
        // the store cursor only as a final integrity check for the boundary.
        let _timer = self
            .metrics
            .start(MetricsPhase::Snapshot, session_id, 0, step_number);
        let saved = self
            .event_store
            .event_boundary_cursor(session_id, state.turn_cancel.clone())
            .await;
        match saved {
            Ok(cursor) => {
                let _ = (
                    cursor,
                    state,
                    _error_partial_message_ids,
                    _clear_confirm_interactions,
                );
                true
            }
            Err(error) => {
                tracing::warn!("checkpoint failed for session {}: {}", session_id, error);
                false
            }
        }
    }

    /// and record recovery-only rows so the session can be resumed via "continue" or
    /// rolled back. Without this, any text streamed before the error is lost
    /// on page refresh because it was only in the frontend's memory.
    pub(super) async fn persist_partial_on_error(
        &self,
        ctx: &StepCtx,
        state: &mut ReActState,
        partial_thought: &std::sync::Arc<std::sync::Mutex<String>>,
        partial_reasoning: &std::sync::Arc<std::sync::Mutex<String>>,
    ) -> RecoveryPersistenceResult {
        // Start a durable two-phase marker before any of the independent
        // branch/message/projection/boundary writes. A later resume can see
        // that this repair was in flight even if the process-local projection is stale.
        let protocol_started = self
            .append_recovery_marker(
                &ctx.session_id,
                ctx.run_id,
                ctx.step_num,
                "started",
                RecoveryPersistenceStatus {
                    branch_point: false,
                    partial_messages: false,
                    projection: false,
                    event_boundary: false,
                },
            )
            .await;
        if !protocol_started {
            let failure_marker = self
                .append_recovery_marker(
                    &ctx.session_id,
                    ctx.run_id,
                    ctx.step_num,
                    "failed",
                    RecoveryPersistenceStatus {
                        branch_point: false,
                        partial_messages: false,
                        projection: false,
                        event_boundary: false,
                    },
                )
                .await;
            let result = RecoveryPersistenceResult::Failed {
                branch_point: false,
                partial_messages: false,
                event_boundary: false,
                projection: false,
                failure_marker,
            };
            tracing::error!(
                session_id = %ctx.session_id,
                step = ctx.step_num,
                ?result,
                "recovery persistence protocol could not start; retaining scratch partial"
            );
            return result;
        }

        // Save a branch point BEFORE persisting the partial output, so
        // last_msg_at captures the timestamp of the last message BEFORE the
        // partial. This lets continue_session / rollback_session precisely delete
        // only the partial output via delete_messages_after(last_msg_at).
        // The events here represent the state BEFORE the failed LLM call
        // (the response was never appended), so resuming will retry cleanly.
        // FORCED write: continue_session / rollback_session locate this branch
        // point in the durable event timeline; a throttled (stale) row would silently
        // skip their message truncation.
        let branch_point = self
            .save_branch_point(&ctx.session_id, state, ctx.step_num, true)
            .await
            .is_ok();

        let thought_text = partial_thought.lock().unwrap().clone();
        let reasoning_text = partial_reasoning.lock().unwrap().clone();
        let mut error_partial_message_ids = Vec::with_capacity(2);
        let mut partial_messages = true;
        let mut projection = true;
        if !reasoning_text.trim().is_empty() {
            let message_id = state.stream_block_message_id_or_new(StreamBlockIdentity::reasoning(
                ctx.step_num,
                ctx.run_id,
            ));
            if let Err(error) = self
                .persist_recovery_partial(
                    &ctx.session_id,
                    reasoning_text.trim(),
                    haven_memory::repositories::session_events::RecoveryPartialKind::Reasoning,
                    &message_id,
                )
                .await
            {
                partial_messages = false;
                tracing::error!(
                    session_id = %ctx.session_id,
                    step = ctx.step_num,
                    error = %error,
                    "failed to persist recovery reasoning partial; retaining scratch partial"
                );
            } else {
                error_partial_message_ids.push(message_id);
            }
        }
        if !thought_text.trim().is_empty() {
            let text = thought_text.trim();
            let message_id = state.stream_block_message_id_or_new(StreamBlockIdentity::thought(
                ctx.step_num,
                ctx.run_id,
            ));
            if let Err(error) = self
                .persist_recovery_partial(
                    &ctx.session_id,
                    text,
                    haven_memory::repositories::session_events::RecoveryPartialKind::Thought,
                    &message_id,
                )
                .await
            {
                partial_messages = false;
                tracing::error!(
                    session_id = %ctx.session_id,
                    step = ctx.step_num,
                    error = %error,
                    "failed to persist recovery thought partial; retaining scratch partial"
                );
            } else {
                error_partial_message_ids.push(message_id.clone());
            }
            if let Err(error) = EventDispatcher::emit_thought_from(
                &ctx.emitter,
                &ctx.session_id,
                text,
                ctx.step_num,
                ctx.run_id,
                &message_id,
                &self.event_store,
            )
            .await
            {
                projection = false;
                tracing::error!(
                    session_id = %ctx.session_id,
                    step = ctx.step_num,
                    error = %error,
                    "failed to project recovery thought step"
                );
            }
        }
        // The branch-point marker above is intentionally written before the
        // recovery-only rows. Mark this follow-up write even when no visible
        // text arrived: only this marker authorizes Continue to replace the
        // failed step, never an ordinary periodic boundary check.
        let event_boundary = self
            .ensure_event_boundary_with_error_partials(
                &ctx.session_id,
                state,
                ctx.step_num,
                Some(&error_partial_message_ids),
                false,
            )
            .await;
        // The stream text now lives in the message stream (persisted above),
        // so any checkpointed partial row for this session is obsolete — and an
        // in-flight checkpoint write must not re-create it. Discard goes
        // through the PartialStore, whose generation bump invalidates stale
        // writes.
        let all_phases_succeeded = branch_point && partial_messages && event_boundary && projection;
        let marker_phase = if all_phases_succeeded {
            "committed"
        } else {
            "failed"
        };
        let terminal_marker = self
            .append_recovery_marker(
                &ctx.session_id,
                ctx.run_id,
                ctx.step_num,
                marker_phase,
                RecoveryPersistenceStatus {
                    branch_point,
                    partial_messages,
                    projection,
                    event_boundary,
                },
            )
            .await;
        let result = if all_phases_succeeded && terminal_marker {
            RecoveryPersistenceResult::Persisted
        } else {
            RecoveryPersistenceResult::Failed {
                branch_point,
                partial_messages,
                event_boundary,
                projection,
                failure_marker: if all_phases_succeeded {
                    // A failed commit marker is itself a failed protocol. Try
                    // to leave the explicit failure state if the transient
                    // write failure has cleared.
                    self.append_recovery_marker(
                        &ctx.session_id,
                        ctx.run_id,
                        ctx.step_num,
                        "failed",
                        RecoveryPersistenceStatus {
                            branch_point,
                            partial_messages,
                            projection,
                            event_boundary,
                        },
                    )
                    .await
                } else {
                    terminal_marker
                },
            }
        };
        if result.should_discard() {
            self.executor.partials.discard(&ctx.session_id).await;
        } else {
            tracing::error!(
                session_id = %ctx.session_id,
                step = ctx.step_num,
                ?result,
                "recovery persistence failed; retaining scratch partial for retry"
            );
        }
        result
    }

    async fn append_recovery_marker(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> bool {
        let phase = phase.to_string();
        match self
            .event_store
            .append_recovery_persistence_if_session_exists(
                session_id,
                run_id,
                step_number,
                &phase,
                status,
            )
            .await
        {
            Ok(()) => true,
            Err(error) => {
                tracing::error!(
                    session_id,
                    step = step_number,
                    phase,
                    error = %error,
                    "failed to persist recovery protocol marker"
                );
                false
            }
        }
    }

    /// Save a branch point at the current step before tool execution (§2).
    ///
    /// Append the durable branch marker before updating the actor's cache.
    /// `force` remains part of the call contract for terminal/error paths, but
    /// persistence is no longer throttled or stored as a session snapshot.
    pub(super) async fn save_branch_point(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_number: u32,
        force: bool,
    ) -> anyhow::Result<()> {
        let _ = force;
        let cursor = self
            .event_store
            .append_branch_point_from_projection_for_existing_session(session_id, step_number, None)
            .await
            .map_err(|error| {
                tracing::warn!(
                    session_id,
                    step = step_number,
                    error = %error,
                    "failed to append durable branch point"
                );
                self.metrics.increment(MetricsCounter::BranchPointFailures);
                error
            })?;

        let branch_point = BranchPoint {
            event_cursor: cursor.event_cursor,
            step_number,
            last_msg_at: cursor.last_msg_at,
        };

        // The in-memory index is a cache of the durable marker.  Publishing it
        // only after the append succeeds keeps rollback fail-closed when SQLite
        // is unavailable.
        state.branch_points.insert(step_number, branch_point);
        Ok(())
    }
}

#[cfg(test)]
#[path = "event_boundary_tests.rs"]
mod tests;
