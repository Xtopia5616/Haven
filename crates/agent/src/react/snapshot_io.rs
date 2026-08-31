//! Snapshot / branch / pause persistence helpers for the ReAct loop.
//!
//! Owns checkpoint serialization and lifecycle exits for the shared
//! [`ReActState`]; the loop modules delegate here instead of carrying their
//! own snapshot argument lists.

use tracing::Instrument;

use super::{PauseReason, StepCtx, *};
use crate::types::TranscriptRecord;

/// Mid-run DB snapshot throttle policy (Phase 7 / F3).
///
/// Tracks the last step at which each session wrote a snapshot so
/// [`ReActEngine::save_branch_point`] can decide whether a write is due
/// without embedding the interval math in the loop. Unit-testable without
/// starting the full ReAct loop.
#[derive(Debug, Default)]
pub(crate) struct SnapshotStore {
    last_written: HashMap<String, u32>,
}

impl SnapshotStore {
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

/// Borrowed serialization view of a `ReActSnapshot`. Serializing this instead
/// of building an owned `ReActSnapshot` skips the per-step deep copies of
/// events/branch_points (which accumulate to O(n²) over a long session).
/// Field names/shape match `ReActSnapshot` exactly so the persisted JSON
/// stays wire-compatible. `events` is the sole transcript authority (Phase 8).
#[derive(serde::Serialize)]
struct SnapshotView<'a> {
    events: &'a [TranscriptRecord],
    step_number: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    branch_points: &'a HashMap<u32, BranchPoint>,
    /// `saved_at` is written at serialization time: resume uses it to recover
    /// messages persisted after this snapshot by timestamp (see
    /// `ReActSnapshot::saved_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error_partial_message_ids: Option<&'a [String]>,
    /// Explicit ask-awaiting flag (Phase 4 / C5); see `ReActSnapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    awaiting_answer: Option<&'a crate::types::AskPending>,
    /// Explicit confirm-awaiting batch (Phase 5 / E3); see `ReActSnapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    awaiting_confirm: Option<&'a crate::types::ConfirmPending>,
    /// Per-run step budget for observability (R4); see `ReActSnapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_budget: Option<&'a crate::types::RunBudget>,
}

/// Inputs for a pause checkpoint. Keeping this boundary named prevents the
/// lifecycle writer from growing another positional-argument list.
pub(super) struct PauseTurnInput<'a> {
    pub(super) session_id: &'a str,
    pub(super) state: &'a mut ReActState,
    pub(super) snapshot_step: u32,
    pub(super) emitter: &'a Arc<dyn AgentEventEmitter>,
    pub(super) status: SessionStatus,
    pub(super) final_text: &'a str,
    pub(super) branch_point_step: Option<u32>,
}

/// Update a session's status and emit the `SessionUpdated` event, in that order.
/// Shared by every status-transition path (pause, budget pause, agent layer)
/// so the pair cannot drift.
pub(crate) async fn set_status_and_emit(
    executor: &SessionExecutor,
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: &str,
    status: SessionStatus,
) -> anyhow::Result<()> {
    let status_str = status.as_str().to_string();
    tracing::debug!("session {} status -> {}", session_id, status_str);
    executor.update_session_status(session_id, status).await?;
    emitter
        .emit(crate::event::AgentEvent::SessionUpdated {
            session_id: session_id.into(),
            status: status_str,
        })
        .await;
    Ok(())
}

/// Interval (in ReAct steps) at which long-running sessions re-run fact
/// inference mid-session, so memory is refreshed before the session
/// ever pauses or completes.
/// Message persisted when a run exhausts its step budget (`max_steps`). The
/// session is intentionally paused as a checkpoint —the session is NOT finished,
/// and the next user message resumes it with a fresh budget. System notices
/// like this must NOT land in the chat as an assistant bubble; they are
/// surfaced as a notification (in-app toast + Windows) instead.
const BUDGET_EXHAUSTED_TITLE: &str = "任务步骤上限已用尽";

const BUDGET_EXHAUSTED_BODY: &str = "本轮运行的步骤上限已用完，任务已暂停。发一条消息即可继续。";

impl ReActEngine {
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
    ) {
        let result = crate::persist_session_message(
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
        .await;
        match result {
            Ok(msg) => {
                self.note_last_msg_at(session_id, Some(msg.created_at));
            }
            Err(e) => {
                tracing::warn!(
                    "ReAct: failed to project {} message for session {} (type={:?}): {}",
                    role,
                    session_id,
                    message_type,
                    e
                );
            }
        }
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
    ) {
        self.project_chat_message(
            session_id,
            role,
            content,
            message_type,
            tool_call_id,
            message_id,
        )
        .await;
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
        if let Some(ref inference) = self.inference {
            inference.enqueue_summary_extract(&session_id, &episode_id, &summary);
        }
    }

    /// Finalize a turn: save the branch point (when requested), snapshot the
    /// ReAct state, then mark the session with the given status and notify the
    /// frontend + inference.
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
                self.save_branch_point(session_id, state, step, false).await;
            }
            self.save_snapshot_with_branches(session_id, state, snapshot_step)
                .await;
            // The status itself carries the awaiting-answer flavor
            // (`PausedAwaitingAnswer`), so the transition is atomic: a
            // background-action completion landing concurrently reads the final
            // state and cannot auto-wake an answer-blocked session.
            let reason = if status.is_awaiting_answer() {
                PauseReason::Ask
            } else if status.is_awaiting_confirm() {
                PauseReason::Confirm
            } else {
                PauseReason::TurnEnd
            };
            set_status_and_emit(&self.executor, emitter, session_id, status).await?;
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
            self.save_snapshot_with_branches(session_id, state, snapshot_step)
                .await;
            set_status_and_emit(&self.executor, emitter, session_id, SessionStatus::Paused).await?;
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
    ) {
        self.save_snapshot_with_branches(session_id, state, step_number)
            .await;
    }

    /// Phase 7 / C4: single cancel-exit path — write the exit snapshot then
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

    /// Write the exit snapshot then return `exit`. Used by Completed / Error /
    /// Cancelled so step-head and mid-batch paths cannot drift on whether
    /// `react_state` is flushed (review fix).
    pub(super) async fn exit_with_snapshot(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        exit: LoopExit,
    ) -> LoopExit {
        self.save_exit_snapshot(session_id, state, step_number)
            .await;
        exit
    }

    /// Shared External-pause exit (step-head and mid-batch): snapshot →
    /// `on_pause(External)` → `LoopExit::Paused`.
    pub(super) async fn exit_external_pause(
        &self,
        session_id: &str,
        state: &ReActState,
        step_number: u32,
        emitter: &Arc<dyn AgentEventEmitter>,
        run_id: u64,
    ) -> LoopExit {
        self.save_snapshot_with_branches(session_id, state, step_number)
            .await;
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

    /// Save snapshot including branch points for tree-structured rollback (§2).
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
        self.save_snapshot_with_error_partials(session_id, state, step_number, None)
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
    ) -> bool {
        let awaiting = self.executor.get_awaiting_answer(session_id).await;
        let awaiting_confirm = self.executor.get_awaiting_confirm(session_id).await;
        let run_budget = self.current_run_budget(session_id);
        let view = SnapshotView {
            events: &state.events,
            step_number,
            branch_points: &state.branch_points,
            saved_at: Some(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            error_partial_message_ids,
            awaiting_answer: awaiting.as_ref(),
            awaiting_confirm: awaiting_confirm.as_ref(),
            run_budget: run_budget.as_ref(),
        };
        // Serialize into the session's own buffer inside a scoped block so the
        // mutex guard is dropped before the await below (the guard is not
        // Send, so it must not be live across the spawn_blocking boundary).
        let bytes = {
            let mut bufs = self.snapshot_bufs.lock();
            let buf = bufs.entry(session_id.to_string()).or_default();
            buf.clear();
            if serde_json::to_writer(&mut *buf, &view).is_err() {
                return false;
            }
            std::mem::take(buf)
        };
        let json = match String::from_utf8(bytes) {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!("snapshot serialization produced invalid UTF-8: {error}");
                return false;
            }
        };
        let db = self.db.clone();
        let tid_owned = session_id.to_string();
        // Return ownership of the serialized bytes so the allocation is
        // handed back to the session's buffer for reuse on the next snapshot.
        let back: String = match db
            .run_blocking(move |db| {
                db.save_react_state(&tid_owned, &json)?;
                Ok::<String, anyhow::Error>(json)
            })
            .await
        {
            Ok(json) => json,
            Err(error) => {
                tracing::warn!(
                    "save_react_state failed for session {}: {}",
                    session_id,
                    error
                );
                return false;
            }
        };
        if let Ok(mut bufs) = self.snapshot_bufs.try_lock() {
            *bufs.entry(session_id.to_string()).or_default() = back.into_bytes();
        }
        true
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
    ) {
        // Save a branch point BEFORE persisting the partial output, so
        // last_msg_at captures the timestamp of the last message BEFORE the
        // partial. This lets continue_session / rollback_session precisely delete
        // only the partial output via delete_messages_after(last_msg_at).
        // The events here represent the state BEFORE the failed LLM call
        // (the response was never appended), so resuming will retry cleanly.
        // FORCED write: continue_session / rollback_session locate this branch
        // point in the DB snapshot; a throttled (stale) row would silently
        // skip their message truncation.
        self.save_branch_point(&ctx.session_id, state, ctx.step_num, true)
            .await;

        let thought_text = partial_thought.lock().unwrap().clone();
        let reasoning_text = partial_reasoning.lock().unwrap().clone();
        let mut error_partial_message_ids = Vec::with_capacity(2);
        if !reasoning_text.trim().is_empty() {
            let message_id =
                self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "reasoning");
            self.persist_session_message(
                &ctx.session_id,
                "assistant",
                reasoning_text.trim(),
                Some("reasoning"),
                None,
                Some(&message_id),
            )
            .await;
            error_partial_message_ids.push(message_id);
        }
        if !thought_text.trim().is_empty() {
            let text = thought_text.trim();
            let message_id =
                self.block_msg_id(&ctx.session_id, ctx.step_num, ctx.run_id, "thought");
            self.persist_session_message(
                &ctx.session_id,
                "assistant",
                text,
                Some("text"),
                None,
                Some(&message_id),
            )
            .await;
            error_partial_message_ids.push(message_id.clone());
            EventDispatcher::emit_thought_from(
                &ctx.emitter,
                &ctx.session_id,
                text,
                ctx.step_num,
                ctx.run_id,
                &message_id,
                &self.db,
            )
            .await;
        }
        // The branch-point snapshot above is intentionally written before the
        // recovery-only rows. Mark this follow-up write even when no visible
        // text arrived: only this marker authorizes Continue to replace the
        // failed step, never an ordinary periodic pre-crash snapshot.
        self.save_snapshot_with_error_partials(
            &ctx.session_id,
            state,
            ctx.step_num,
            Some(&error_partial_message_ids),
        )
        .await;
        // The stream text now lives in the message stream (persisted above),
        // so any checkpointed partial row for this session is obsolete — and an
        // in-flight checkpoint write must not re-create it. Discard goes
        // through the PartialStore, whose generation bump invalidates stale
        // writes.
        self.executor.partials.discard(&ctx.session_id).await;
    }

    /// Save a branch point at the current step before tool execution (§2).
    ///
    /// The DB snapshot write is throttled via [`SnapshotStore`] on the happy
    /// path (`force = false`): the in-memory branch-point map is always
    /// current, and every pause/error/final path plus every cancellation exit
    /// writes unconditionally. Error paths MUST pass `force = true` (e.g.
    /// `persist_partial_on_error`): `continue_session` / `rollback_session`
    /// locate the failed step's branch point in the DB snapshot, and a stale
    /// row would silently skip their message truncation.
    pub(super) async fn save_branch_point(
        &self,
        session_id: &str,
        state: &mut ReActState,
        step_number: u32,
        force: bool,
    ) {
        // Mid-run (`force=false`): prefer the in-process cache filled by
        // persist paths so throttled steps skip SQLite. Force paths
        // (pause/error/cancel) always re-read so the snapshot cutoff matches
        // the DB after concurrent truncations (rollback / continue).
        let last_msg_at = if !force {
            if let Some(cached) = self.last_msg_at.get(session_id) {
                cached
            } else {
                self.refresh_last_msg_at(session_id).await
            }
        } else {
            self.refresh_last_msg_at(session_id).await
        };
        // Phase 8 / F4: store only an index into the parent events vec — no
        // Arc copies of transcript state.
        state.branch_points.insert(
            step_number,
            BranchPoint {
                event_cursor: state.events.len(),
                step_number,
                last_msg_at,
            },
        );
        // The throttle marker guard is confined to this block so it is always
        // dropped before the write's await.
        let due = {
            let mut store = self.snapshot_store.lock().unwrap();
            store.on_step_boundary(session_id, step_number, force)
        };
        if due {
            self.save_snapshot_with_branches(session_id, state, step_number)
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::SnapshotStore;

    #[test]
    fn should_write_first_step_always() {
        let store = SnapshotStore::default();
        assert!(store.should_write("s", 1, false));
        assert!(store.should_write("s", 100, false));
    }

    #[test]
    fn throttle_skips_until_interval() {
        let mut store = SnapshotStore::default();
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
        let mut store = SnapshotStore::default();
        assert!(store.on_step_boundary("s", 1, false));
        assert!(!store.should_write("s", 2, false));
        assert!(store.should_write("s", 2, true));
        assert!(store.on_step_boundary("s", 2, true));
        assert_eq!(store.last_written.get("s"), Some(&2));
    }

    #[test]
    fn clear_session_resets_throttle() {
        let mut store = SnapshotStore::default();
        assert!(store.on_step_boundary("s", 1, false));
        store.clear_session("s");
        assert!(store.should_write("s", 2, false));
    }

    #[test]
    fn sessions_throttled_independently() {
        let mut store = SnapshotStore::default();
        assert!(store.on_step_boundary("a", 1, false));
        assert!(store.on_step_boundary("b", 1, false));
        assert!(!store.should_write("a", 2, false));
        assert!(!store.should_write("b", 2, false));
        assert!(store.on_step_boundary("a", 4, false));
        assert!(!store.should_write("b", 3, false));
    }
}
