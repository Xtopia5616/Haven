//! Snapshot / branch / pause persistence helpers for the ReAct loop.
//!
//! Owns checkpoint serialization and lifecycle exits for the shared
//! [`ReActState`]; the loop modules delegate here instead of carrying their
//! own snapshot argument lists.

use tracing::Instrument;

use super::{PauseReason, StepCtx, *};
use crate::types::{BranchPoint, TranscriptRecord};
use haven_memory::{RecoveryPersistenceStatus, SessionCursor, SessionEventInput};

/// The durable event-derived session state used by resume and rollback.
///
/// `ReActSnapshot` is intentionally not returned here: it also contains
/// checkpoint-only interaction and budget metadata. Keeping this value
/// separate makes it impossible for a stale snapshot transcript or branch map
/// to accidentally win over the event stream.
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
        recovery_snapshot: bool,
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

/// Mid-run DB snapshot throttle policy (Phase 7 / F3).
///
/// Tracks the last step at which each session wrote a snapshot so
/// [`ReActEngine::save_branch_point`] can decide whether a write is due
/// without embedding the interval math in the loop. Unit-testable without
/// starting the full ReAct loop.
#[derive(Debug, Default)]
pub(crate) struct CheckpointStore {
    last_written: HashMap<String, u32>,
}

impl CheckpointStore {
    /// Steps between mid-run DB snapshot writes on the happy path.
    pub const WRITE_INTERVAL: u32 = 3;

    /// Whether a DB snapshot write should happen at `step`.
    ///
    /// `force` always writes. Otherwise write if this session has never
    /// written, or `step - last >= WRITE_INTERVAL`.
    pub fn should_write(&self, session_id: &str, step: u32, force: bool) -> bool {
        force
            || self
                .last_written
                .get(session_id)
                .is_none_or(|last| step.saturating_sub(*last) >= Self::WRITE_INTERVAL)
    }

    /// Record that a snapshot was written at `step` for `session_id`.
    pub fn record_write(&mut self, session_id: &str, step: u32) {
        self.last_written.insert(session_id.to_string(), step);
    }

    /// Step-boundary hook: return whether a write is due and, if so, record it.
    #[cfg(test)]
    pub fn on_step_boundary(&mut self, session_id: &str, step: u32, force: bool) -> bool {
        let due = self.should_write(session_id, step, force);
        if due {
            self.record_write(session_id, step);
        }
        due
    }

    /// Drop throttle state for a finished session.
    pub fn clear_session(&mut self, session_id: &str) {
        self.last_written.remove(session_id);
    }
}

/// Inputs for a pause checkpoint. Keeping this boundary named prevents the
/// lifecycle writer from growing another positional-argument list.
pub(super) struct PauseTurnInput<'a> {
    pub(super) session_id: &'a str,
    pub(super) state: &'a mut ReActState,
    pub(super) snapshot_step: u32,
    pub(super) emitter: &'a Arc<dyn AgentEventEmitter>,
    pub(super) status: SessionStatus,
    pub(super) waiting_reason: Option<haven_common::SessionWaitingReason>,
    pub(super) final_text: &'a str,
    pub(super) branch_point_step: Option<u32>,
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
/// ordinary pauses derive from the interaction/action registry.
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
/// session is intentionally paused as a checkpoint —the session is NOT finished,
/// and the next user message resumes it with a fresh budget. System notices
/// like this must NOT land in the chat as an assistant bubble; they are
/// surfaced as a notification (in-app toast + Windows) instead.
const BUDGET_EXHAUSTED_TITLE: &str = "任务步骤上限已用尽";

const BUDGET_EXHAUSTED_BODY: &str = "本轮运行的步骤上限已用完，任务已暂停。发一条消息即可继续。";

impl ReActEngine {
    /// Load the active transcript and branch metadata from the durable event
    /// stream in one blocking read. `None` means that no durable event log
    /// exists; once any control or transcript event exists, the snapshot
    /// cache is never consulted for transcript state.
    pub(crate) async fn load_durable_event_state(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<DurableEventState>> {
        let store = self.event_store.clone();
        let session_id = session_id.to_string();
        self.db
            .run_blocking(move |_| {
                let latest_sequence = store.latest_sequence(&session_id)?;
                if latest_sequence == 0 {
                    return Ok(None);
                }
                let cursor = store.cursor(&session_id)?;
                let events = store
                    .read_active_transcript(&session_id)?
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
                let branch_points = store
                    .read_active_branch_points(&session_id)?
                    .into_iter()
                    .map(|(_, event_cursor, step_number, last_msg_at)| {
                        (
                            step_number,
                            BranchPoint {
                                event_cursor,
                                step_number,
                                last_msg_at,
                            },
                        )
                    })
                    .collect();
                Ok(Some(DurableEventState {
                    events,
                    branch_points,
                    cursor,
                }))
            })
            .await
    }

    fn transcript_event_input(
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
            TranscriptRecord::CompactSummary { .. } => 0,
        };
        Ok(SessionEventInput::transcript(
            serde_json::to_string(event)?,
            run_id,
            step_number,
        ))
    }

    /// Seed the durable event log for a fresh session before the first model
    /// request. Resume never calls this helper: an existing checkpoint must
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
        let store = self.event_store.clone();
        let session_id = session_id.to_string();
        self.db
            .run_blocking(move |_| {
                store.seed_if_empty(&session_id, &inputs)?;
                Ok(())
            })
            .await
    }

    /// Append one transcript record to the durable event authority. The
    /// returned sequence is useful for checkpoint metadata, while the hot
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
        let store = self.event_store.clone();
        let session_id = session_id.to_string();
        self.db
            .run_blocking(move |db| {
                // A few provider/stream unit tests exercise the ReAct engine
                // with a synthetic session id and no database session row.
                // Production ingress always creates the row first; keep the
                // isolated engine test path side-effect free.
                if db.get_session(&session_id)?.is_none() {
                    return Ok(0);
                }
                Ok(store
                    .append_transcript(&session_id, &payload, run_id, step_number)?
                    .sequence)
            })
            .await
    }

    /// Project a chat row into `messages` and refresh `last_msg_at`.
    ///
    /// X12: the ReAct loop must call this only from [`Self::apply_transcript`]
    /// (or documented recovery exceptions). Ingress user seeds go through
    /// `crate::persist_session_message` directly so the queue has a durable id
    /// before `UserInject` lands.
    pub(super) async fn project_chat_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::persist_session_message(
            &self.executor,
            session_id,
            role,
            content,
            message_type,
            &[],
            false,
            message_id,
            tool_call_id,
        )
        .instrument(tracing::info_span!("project", session_id, role))
        .await?;
        Ok(())
    }

    /// Recovery-only alias used by `persist_partial_on_error` (intentionally
    /// outside the event log — see transcript module docs).
    pub(super) async fn persist_session_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        crate::persist_session_message_preserving_partial(
            &self.executor,
            session_id,
            role,
            content,
            message_type,
            &[],
            false,
            message_id,
            tool_call_id,
        )
        .await?;
        Ok(())
    }

    /// Persist a compaction summary into episodic long-term memory
    /// (`memory_items`) so context that compaction summarized away stays
    /// retrievable across sessions (embedding + keyword recall). Fire-and-forget:
    /// a dropped write only loses the summary episode, never the session itself.
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
        let db = self.db.clone();
        let session_id = session_id.to_string();
        let summary = summary.to_string();
        let episode_id = episode_id.to_string();
        let session_id_owned = session_id.clone();
        let summary_for_db = summary.clone();
        let episode_id_for_db = episode_id.clone();
        if let Err(e) = db
            .run_blocking(move |db| {
                db.add_episode_with_id(&session_id_owned, &summary_for_db, &episode_id_for_db)?;
                Ok::<(), anyhow::Error>(())
            })
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
        if let Some(ref memory_worker) = self.memory_worker {
            memory_worker.enqueue_summary_extract(&session_id, &episode_id, &summary);
        }
    }

    /// Finalize a turn: save the branch point (when requested), snapshot the
    /// ReAct state, then mark the session with the given status and notify the
    /// frontend + memory worker.
    ///
    /// X12: chat content must already be projected via `apply_transcript`
    /// before this call. The pause path only checkpoints state and changes the
    /// lifecycle; it never writes a second assistant message.
    pub(super) async fn pause_turn(&self, input: PauseTurnInput<'_>) -> anyhow::Result<()> {
        let PauseTurnInput {
            session_id,
            state,
            snapshot_step,
            emitter,
            status,
            waiting_reason,
            final_text,
            branch_point_step,
        } = input;
        let status_label = status.as_str();
        async {
            tracing::info!(
                "ReAct turn finished: session={} step={} status={} final={} chars",
                session_id,
                snapshot_step,
                status_label,
                final_text.chars().count()
            );
            if let Some(step) = branch_point_step {
                self.save_branch_point(session_id, state, step, false)
                    .await?;
            }
            if !self
                .save_snapshot_with_branches(session_id, state, snapshot_step)
                .await
            {
                anyhow::bail!(
                    "failed to durably checkpoint session '{}' at step {}",
                    session_id,
                    snapshot_step
                );
            }
            let reason = if self
                .executor
                .has_pending_interaction(session_id, crate::interaction::InteractionKind::Ask)
                .await
            {
                PauseReason::Ask
            } else if self
                .executor
                .has_pending_interaction(session_id, crate::interaction::InteractionKind::Confirm)
                .await
            {
                PauseReason::Confirm
            } else {
                PauseReason::TurnEnd
            };
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
                step_num: snapshot_step,
                run_id: 0,
                emitter: emitter.clone(),
            };
            self.hooks.on_pause(self, &ctx, reason).await;
            Ok(())
        }
        .instrument(tracing::info_span!(
            "pause",
            session_id,
            step = snapshot_step,
            status = status_label
        ))
        .await
    }

    /// Pause the session because the run exhausted its step budget. Mirrors
    /// `pause_turn`'s checkpoint side effects (snapshot, Paused status,
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
        snapshot_step: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
    ) -> anyhow::Result<()> {
        // Phase 7 / I2: pause span for budget exhaustion (no assistant persist).
        async {
            tracing::info!(
                "ReAct step budget exhausted: session={} next_step={}",
                session_id,
                snapshot_step
            );
            if !self
                .save_snapshot_with_branches(session_id, state, snapshot_step)
                .await
            {
                anyhow::bail!(
                    "failed to durably checkpoint session '{}' after step-budget exhaustion",
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
                    session_id: session_id.into(),
                    title: BUDGET_EXHAUSTED_TITLE.into(),
                    body: BUDGET_EXHAUSTED_BODY.into(),
                })
                .await;
            let ctx = StepCtx {
                session_id: session_id.to_string(),
                step_num: snapshot_step,
                run_id: 0,
                emitter: emitter.clone(),
            };
            self.hooks.on_pause(self, &ctx, PauseReason::Budget).await;
            Ok(())
        }
        .instrument(tracing::info_span!(
            "pause",
            session_id,
            step = snapshot_step,
            reason = "budget"
        ))
        .await
    }

    /// Persist one final snapshot before leaving the loop on a cancellation,
    /// so the DB row is never stale when `rollback_session` / `continue_session`
    /// read it after the handler exits. The mid-run throttle in
    /// `save_branch_point` may have skipped the last write, and the state at
    /// this point is always a clean step boundary (the cancelled response or
    /// partial tool results are discarded by the exit).
    pub(super) async fn save_exit_snapshot(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.save_snapshot_with_branches(session_id, state, step_number)
            .await
    }

    /// Phase 7 / C4: single cancel-exit path — write the exit checkpoint then
    /// return [`LoopExit::Cancelled`]. All cancel sites in the thin loop /
    /// tool batch must go through this helper.
    pub(super) async fn exit_cancelled(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> LoopExit {
        self.exit_with_snapshot(session_id, state, step_number, LoopExit::Cancelled)
            .await
    }

    /// Write the exit checkpoint then return `exit`. Used by Completed / Error /
    /// Cancelled so every lifecycle exit advances the durable clocks.
    pub(super) async fn exit_with_snapshot(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        exit: LoopExit,
    ) -> LoopExit {
        if self
            .save_exit_snapshot(session_id, state, step_number)
            .await
        {
            exit
        } else {
            LoopExit::Error(format!(
                "failed to durably checkpoint session '{}' before exit",
                session_id
            ))
        }
    }

    /// Shared External-pause exit (step-head and mid-batch): checkpoint →
    /// `on_pause(External)` → `LoopExit::Paused`.
    pub(super) async fn exit_external_pause(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> LoopExit {
        if !self
            .save_snapshot_with_branches(session_id, state, step_number)
            .await
        {
            return LoopExit::Error(format!(
                "failed to durably checkpoint session '{}' before pause",
                session_id
            ));
        }
        let ctx = StepCtx {
            session_id: session_id.to_string(),
            step_num: step_number,
            run_id,
            emitter: emitter.clone(),
        };
        self.hooks.on_pause(self, &ctx, PauseReason::External).await;
        LoopExit::Paused {
            reason: PauseReason::External,
        }
    }

    /// Save snapshot including rollback points for overwrite rollback (§2).
    ///
    /// Serializes a borrowed view of the ReAct state (no per-step deep copies
    /// of events/branch_points — those clones were O(n²) over a long session)
    /// into a reusable buffer, then writes to SQLite on the blocking thread
    /// pool so the WAL fsync never stalls the async runtime.
    pub(super) async fn save_snapshot_with_branches(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.save_snapshot_with_error_partials(session_id, state, step_number, None, false)
            .await
    }

    /// Checkpoint a completed tool batch before the loop makes another LLM
    /// request. This closes the crash window where `session_steps` and chat
    /// rows already contain tool results but the periodic snapshot still ends
    /// at the assistant's unanswered tool call.
    pub(super) async fn save_snapshot_after_tool_results(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.save_snapshot_with_error_partials(session_id, state, step_number, None, false)
            .await
    }

    /// Same checkpoint as [`Self::save_snapshot_after_tool_results`], but the
    /// completed confirmation batch must not remain resumable. Writing the
    /// result events and clearing confirm interactions in one snapshot prevents
    /// a crash between result projection and the in-memory gate cleanup from
    /// replaying an already executed side effect.
    pub(super) async fn save_snapshot_after_confirm_results(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
    ) -> bool {
        self.save_snapshot_with_error_partials(session_id, state, step_number, None, true)
            .await
    }

    /// Persist a snapshot carrying the recovery marker for a failed LLM
    /// response. Normal snapshots pass `None`, which clears any marker
    /// consumed by a prior Continue. `Some(&[])` still marks an error whose
    /// stream produced no visible partial text.
    async fn save_snapshot_with_error_partials(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        error_partial_message_ids: Option<&[String]>,
        clear_confirm_interactions: bool,
    ) -> bool {
        let result = self
            .write_snapshot_with_error_partials(
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

    async fn write_snapshot_with_error_partials(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        _error_partial_message_ids: Option<&[String]>,
        _clear_confirm_interactions: bool,
    ) -> bool {
        // Some lifecycle callers do not carry a provider run id (for example
        // a pause checkpoint). The checkpoint is deliberately independent of
        // ReAct runtime state; it records only the clocks observed after the
        // durable event/projection writes.
        let _timer = self
            .metrics
            .start(MetricsPhase::Snapshot, session_id, 0, step_number);
        let store = self.event_store.clone();
        let sid = session_id.to_string();
        let write = move |db: &Database| {
            if db.get_session(&sid)?.is_none() {
                return store.cursor(&sid);
            }
            store.checkpoint(&sid)
        };
        let saved = match state.turn_cancel.clone() {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, write).await,
            None => self.db.run_blocking(write).await,
        };
        match saved {
            Ok(cursor) => {
                #[cfg(test)]
                if let Err(error) = self
                    .write_test_compat_snapshot(
                        session_id,
                        state,
                        step_number,
                        _error_partial_message_ids,
                        _clear_confirm_interactions,
                        cursor,
                    )
                    .await
                {
                    tracing::warn!(
                        session_id,
                        error = %error,
                        "test compatibility snapshot write failed"
                    );
                    return false;
                }
                #[cfg(not(test))]
                let _ = cursor;
                true
            }
            Err(error) => {
                tracing::warn!("checkpoint failed for session {}: {}", session_id, error);
                false
            }
        }
    }

    #[cfg(test)]
    async fn write_test_compat_snapshot(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        error_partial_message_ids: Option<&[String]>,
        clear_confirm_interactions: bool,
        cursor: SessionCursor,
    ) -> anyhow::Result<()> {
        let mut interactions = self.executor.interaction_requests(session_id).await;
        if clear_confirm_interactions {
            interactions
                .retain(|request| request.kind != crate::interaction::InteractionKind::Confirm);
        }
        let snapshot = crate::types::ReActSnapshot {
            events: state.events.clone(),
            event_cursor: state.events.len(),
            step_number,
            branch_points: state.branch_points.clone(),
            last_ingress_seq: cursor.message_ingress_seq,
            error_partial_message_ids: error_partial_message_ids.map(ToOwned::to_owned),
            interactions,
            run_budget: self.current_run_budget(session_id).await,
        };
        let json = serde_json::to_string(&snapshot)?;
        let db = self.db.clone();
        let sid = session_id.to_string();
        db.run_blocking(move |db| db.save_react_state(&sid, &json))
            .await
    }

    /// and save a snapshot so the session can be resumed via "continue" or
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
        // branch/message/projection/snapshot writes. A later resume can see
        // that this repair was in flight even if the snapshot cache is stale.
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
                    recovery_snapshot: false,
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
                        recovery_snapshot: false,
                    },
                )
                .await;
            let result = RecoveryPersistenceResult::Failed {
                branch_point: false,
                partial_messages: false,
                recovery_snapshot: false,
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
        // point in the DB snapshot; a throttled (stale) row would silently
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
            let message_id = self
                .block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning")
                .await;
            if let Err(error) = self
                .persist_session_message(
                    &ctx.session_id,
                    "assistant",
                    reasoning_text.trim(),
                    Some("reasoning"),
                    None,
                    Some(&message_id),
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
            let message_id = self
                .block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought")
                .await;
            if let Err(error) = self
                .persist_session_message(
                    &ctx.session_id,
                    "assistant",
                    text,
                    Some("text"),
                    None,
                    Some(&message_id),
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
                &self.db,
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
        // The branch-point snapshot above is intentionally written before the
        // recovery-only rows. Mark this follow-up write even when no visible
        // text arrived: only this marker authorizes Continue to replace the
        // failed step, never an ordinary periodic pre-crash snapshot.
        let recovery_snapshot = self
            .save_snapshot_with_error_partials(
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
        let all_phases_succeeded =
            branch_point && partial_messages && recovery_snapshot && projection;
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
                    recovery_snapshot,
                },
            )
            .await;
        let result = if all_phases_succeeded && terminal_marker {
            RecoveryPersistenceResult::Persisted
        } else {
            RecoveryPersistenceResult::Failed {
                branch_point,
                partial_messages,
                recovery_snapshot,
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
                            recovery_snapshot,
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
        let store = self.event_store.clone();
        let sid = session_id.to_string();
        let phase = phase.to_string();
        let phase_for_write = phase.clone();
        match self
            .db
            .run_blocking(move |db| {
                // Synthetic ReAct unit tests do not create a session row. As
                // with the existing event writer, keep those tests side
                // effect free while production sessions get the durable mark.
                if db.get_session(&sid)?.is_none() {
                    return Ok(true);
                }
                store.append_recovery_persistence(
                    &sid,
                    run_id,
                    step_number,
                    &phase_for_write,
                    status,
                )?;
                Ok(true)
            })
            .await
        {
            Ok(value) => value,
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
    /// The DB checkpoint write is throttled via [`CheckpointStore`] on the happy
    /// path (`force = false`): every pause/error/final path plus every
    /// cancellation exit writes unconditionally. Error paths MUST pass
    /// `force = true` (e.g.
    /// `persist_partial_on_error`): `continue_session` / `rollback_session`
    /// locate the failed step's branch point in the DB snapshot, and a stale
    /// row would silently skip their message truncation. The durable append
    /// happens before the in-memory cache is updated; all failures propagate.
    pub(super) async fn save_branch_point(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_number: u32,
        force: bool,
    ) -> anyhow::Result<()> {
        // The projection cutoff is read through SessionStore.  In particular,
        // do not reuse a ReAct-side timestamp cache here: rollback and an
        // ingress write may commit between two turns.
        let _ = force;
        let last_msg_at = self.refresh_last_msg_at(session_id).await;
        let last_msg_at = match last_msg_at {
            Ok(value) => value,
            Err(error) => {
                tracing::error!(
                    session_id,
                    step = step_number,
                    error = %error,
                    "refusing to write branch point without a durable message cutoff"
                );
                return Err(error);
            }
        };
        let last_msg_at_for_event = last_msg_at.clone();
        // Phase 8 / F4: store only an index into the parent events vec — no
        // Arc copies of transcript state.
        let branch_point = BranchPoint {
            event_cursor: state.events.len(),
            step_number,
            last_msg_at,
        };
        // Branch metadata is part of the durable timeline as well. This lets
        // rollback recover its target without requiring the snapshot cache;
        // the cache still stores the same map for cheap hot-path access.
        let store = self.event_store.clone();
        let sid = session_id.to_string();
        let event_cursor = state.events.len();
        if let Err(error) = self
            .db
            .run_blocking(move |db| {
                if db.get_session(&sid)?.is_none() {
                    anyhow::bail!("session '{}' disappeared before branch-point append", sid);
                }
                store.append_branch_point(
                    &sid,
                    event_cursor,
                    step_number,
                    last_msg_at_for_event.as_deref(),
                    None,
                )?;
                Ok(())
            })
            .await
        {
            tracing::warn!(
                session_id,
                step = step_number,
                error = %error,
                "failed to append durable branch point"
            );
            self.metrics.increment(MetricsCounter::BranchPointFailures);
            return Err(error);
        }

        // The in-memory index is a cache of the durable marker.  Publishing it
        // only after the append succeeds keeps rollback fail-closed when SQLite
        // is unavailable.
        state.branch_points.insert(step_number, branch_point);
        // The throttle marker guard is confined to this block so it is always
        // dropped before the write's await.
        let due = {
            let store = self.checkpoint_store.lock().unwrap();
            store.should_write(session_id, step_number, force)
        };
        if due {
            if !self
                .save_snapshot_with_branches(session_id, state, step_number)
                .await
            {
                self.metrics.increment(MetricsCounter::SnapshotFailures);
                anyhow::bail!(
                    "failed to durably checkpoint branch point for session '{}' at step {}",
                    session_id,
                    step_number
                );
            }
            self.checkpoint_store
                .lock()
                .unwrap()
                .record_write(session_id, step_number);
        } else {
            // Nothing else to persist for this checkpoint.
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{CheckpointStore, ReActEngine, ReActState, RecoveryPersistenceResult};
    use crate::session::SessionSupervisor;
    use haven_common::config::{ContextLimitsConfig, RouterConfig};
    use haven_llm::LlmRouter;
    use haven_memory::Database;
    use haven_tools::ToolsManager;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn recovery_discards_scratch_only_after_every_projection_succeeds() {
        assert!(RecoveryPersistenceResult::Persisted.should_discard());
        assert!(
            !RecoveryPersistenceResult::Failed {
                branch_point: false,
                partial_messages: true,
                recovery_snapshot: true,
                projection: true,
                failure_marker: false,
            }
            .should_discard()
        );
    }

    #[test]
    fn should_write_first_step_always() {
        let store = CheckpointStore::default();
        assert!(store.should_write("s", 1, false));
        assert!(store.should_write("s", 100, false));
    }

    #[test]
    fn throttle_skips_until_interval() {
        let mut store = CheckpointStore::default();
        assert!(store.on_step_boundary("s", 1, false));
        assert!(!store.should_write("s", 2, false));
        assert!(!store.should_write("s", 3, false));
        assert!(store.should_write("s", 4, false));
        assert!(store.on_step_boundary("s", 4, false));
        assert!(!store.should_write("s", 5, false));
        assert!(!store.should_write("s", 6, false));
        assert!(store.should_write("s", 7, false));
    }

    #[test]
    fn force_bypasses_throttle() {
        let mut store = CheckpointStore::default();
        assert!(store.on_step_boundary("s", 1, false));
        assert!(!store.should_write("s", 2, false));
        assert!(store.should_write("s", 2, true));
        assert!(store.on_step_boundary("s", 2, true));
        assert_eq!(store.last_written.get("s"), Some(&2));
    }

    #[test]
    fn clear_session_resets_throttle() {
        let mut store = CheckpointStore::default();
        assert!(store.on_step_boundary("s", 1, false));
        store.clear_session("s");
        assert!(store.should_write("s", 2, false));
    }

    #[test]
    fn sessions_throttled_independently() {
        let mut store = CheckpointStore::default();
        assert!(store.on_step_boundary("a", 1, false));
        assert!(store.on_step_boundary("b", 1, false));
        assert!(!store.should_write("a", 2, false));
        assert!(!store.should_write("b", 2, false));
        assert!(store.on_step_boundary("a", 4, false));
        assert!(!store.should_write("b", 3, false));
    }

    #[test]
    fn tool_result_checkpoint_is_not_throttled_by_mid_run_interval() {
        let mut store = CheckpointStore::default();
        assert!(store.on_step_boundary("s", 1, false));
        assert!(!store.should_write("s", 2, false));
        assert!(store.should_write("s", 2, true));
    }

    #[tokio::test]
    async fn branch_point_fault_does_not_publish_in_memory_cache() {
        let path =
            std::env::temp_dir().join(format!("haven_branch_fault_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&path).unwrap());
        let session = db.create_session("input").unwrap();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER branch_point_fault
                 BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'branch_point'
                 BEGIN SELECT RAISE(ABORT, 'injected branch-point failure'); END;",
            )
            .unwrap();
        let executor = Arc::new(SessionSupervisor::new(
            db.clone(),
            Arc::new(ToolsManager::new()),
            1,
        ));
        let engine = ReActEngine::new(
            Arc::new(LlmRouter::new(RouterConfig::default())),
            executor,
            db,
            8,
            ContextLimitsConfig::default(),
        );
        let mut state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());

        let error = engine
            .save_branch_point(&session.id, &mut state, 3, true)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("injected branch-point failure"));
        assert!(state.branch_points.is_empty());
        assert!(
            engine
                .event_store
                .read_active_branch_points(&session.id)
                .unwrap()
                .is_empty()
        );
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn snapshot_fault_after_branch_append_preserves_durable_cutoff() {
        let path = std::env::temp_dir().join(format!(
            "haven_snapshot_before_crash_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&path).unwrap());
        let session = db.create_session("input").unwrap();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER snapshot_fault
                 BEFORE UPDATE OF react_state ON sessions
                 BEGIN SELECT RAISE(ABORT, 'injected snapshot failure'); END;",
            )
            .unwrap();
        let executor = Arc::new(SessionSupervisor::new(
            db.clone(),
            Arc::new(ToolsManager::new()),
            1,
        ));
        let engine = ReActEngine::new(
            Arc::new(LlmRouter::new(RouterConfig::default())),
            executor,
            db.clone(),
            8,
            ContextLimitsConfig::default(),
        );
        let mut state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());

        let error = engine
            .save_branch_point(&session.id, &mut state, 3, true)
            .await
            .unwrap_err();

        assert!(
            error.to_string().contains("failed to durably checkpoint"),
            "unexpected snapshot failure: {error}"
        );
        assert!(state.branch_points.contains_key(&3));
        assert_eq!(
            engine
                .event_store
                .read_active_branch_points(&session.id)
                .unwrap()
                .len(),
            1,
            "the branch cutoff must survive a crash before the cache snapshot"
        );
        assert!(
            db.get_react_state(&session.id).unwrap().is_none(),
            "the failed snapshot transaction must not publish a partial cache"
        );
        assert!(
            !engine
                .checkpoint_store
                .lock()
                .unwrap()
                .last_written
                .contains_key(&session.id),
            "a failed snapshot must not advance the write throttle"
        );
        drop(engine);
        let _ = std::fs::remove_file(path);
    }
}
