//! Snapshot / branch / pause persistence helpers for the ReAct loop.
//!
//! Split from `react.rs` (Phase 1 mechanical extract; behavior unchanged).

use super::{PauseReason, StepCtx, *};

/// Borrowed serialization view of a `ReActSnapshot`. Serializing this instead
/// of building an owned `ReActSnapshot` skips the per-step deep copies of
/// canonical/history/branch_points (which accumulate to O(n²) over a long
/// session). Field names/shape match `ReActSnapshot` exactly so the persisted
/// JSON stays wire-compatible.
#[derive(serde::Serialize)]
struct SnapshotView<'a> {
    canonical: &'a [CanonicalMessage],
    history: &'a [ReActStep],
    step_number: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    branch_points: &'a HashMap<u32, BranchPoint>,
    /// `saved_at` is written at serialization time: resume uses it to recover
    /// messages persisted after this snapshot by timestamp (see
    /// `ReActSnapshot::saved_at`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_at: Option<String>,
    /// Explicit ask-awaiting flag (Phase 4 / C5); see `ReActSnapshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    awaiting_answer: Option<&'a crate::types::AskPending>,
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

/// Mid-run React-state snapshot writes are throttled to once per this many
/// steps (`save_branch_point`). The in-memory canonical/history/branch-point
/// map is always current (branch points are inserted every step regardless),
/// and every pause/error/final path plus every cancellation exit writes the
/// snapshot unconditionally, so the DB row only lags behind by this many
/// steps in a hard-crash window — the resume then re-runs at most this many
/// tool batches, which is already the behavior for a crash mid-batch today.
const SNAPSHOT_WRITE_INTERVAL: u32 = 3;

impl ReActEngine {
    /// Persist an assistant message into the session's message stream.
    /// Delegates to the shared `crate::persist_session_message` so this path
    /// cannot drift from the user-turn persistence path (same trim, same
    /// error policy). Persistence failures are logged here instead of being
    /// silently swallowed: a dropped write would make the streamed content
    /// disappear after a reload while the UI keeps showing it.
    pub(super) async fn persist_session_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        message_id: Option<&str>,
    ) {
        if let Err(e) = crate::persist_session_message(
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
        .await
        {
            tracing::warn!(
                "ReAct: failed to persist {} message for session {} (type={:?}): {}",
                role,
                session_id,
                message_type,
                e
            );
        }
    }

    /// Persist a compaction summary into episodic long-term memory
    /// (`memory_episodes`) so context that compaction summarized away stays
    /// retrievable across sessions (embedding + keyword recall). Fire-and-forget:
    /// a dropped write only loses the summary episode, never the session itself.
    pub(super) async fn persist_compaction_summary(&self, session_id: &str, summary: &str) {
        let summary = summary.trim();
        if summary.is_empty() {
            return;
        }
        let db = self.db.clone();
        let session_id = session_id.to_string();
        let summary = summary.to_string();
        let session_id_owned = session_id.clone();
        if let Err(e) = db
            .run_blocking(move |db| {
                db.add_episode(&session_id_owned, &summary)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
        {
            tracing::warn!(
                "ReAct: failed to persist compaction summary for session {}: {}",
                session_id,
                e
            );
        }
    }

    /// Finalize a turn: persist the assistant text, save the branch point
    /// (when requested), snapshot the ReAct state, then mark the session with
    /// the given status and notify the frontend + inference. Shared by all
    /// pause/complete paths so the persist → branch-point → snapshot →
    /// status → event ordering cannot drift between them. The snapshot is
    /// taken after the branch point so it includes the newly added entry.
    /// Callers pause with `SessionStatus::Paused` (scheduling) or
    /// `SessionStatus::PausedAwaitingAnswer` (the `ask` tool is blocked on a
    /// human reply — that flavor also blocks background-action auto-wake).
    /// The step-budget checkpoint uses `pause_turn_budget` instead, which
    /// skips the assistant-message persist (the notice is a notification).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn pause_turn(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        snapshot_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        status: SessionStatus,
        final_text: &str,
        branch_point_step: Option<u32>,
        infer: &(dyn Fn() + Send + Sync),
        // Pre-minted id of the streamed thought bubble this final text is the
        // authoritative copy of (`None` mints a fresh id).
        persist_message_id: Option<&str>,
        // True when this pause follows an `ask` batch: the question message
        // rows were already persisted by the caller (one per ask step, under
        // the step ids), so the persist below is skipped.
        is_ask: bool,
    ) -> anyhow::Result<()> {
        tracing::info!(
            "ReAct turn finished: session={} step={} status={} final={} chars",
            session_id,
            snapshot_step,
            status.as_str(),
            final_text.chars().count()
        );
        if std::env::var("HAVEN_DEBUG_PAUSE").is_ok() {
            eprintln!(
                "DEBUG pause_turn persist ask={} id={:?} final={}",
                is_ask, persist_message_id, final_text
            );
        }
        if !is_ask {
            self.persist_session_message(
                session_id,
                "assistant",
                final_text,
                Some("text"),
                None,
                persist_message_id,
            )
            .await;
        }
        if let Some(step) = branch_point_step {
            self.save_branch_point(session_id, canonical, history, step, branch_points, false)
                .await;
        }
        self.save_snapshot_with_branches(
            session_id,
            canonical,
            history,
            snapshot_step,
            branch_points,
        )
        .await;
        // The status itself carries the awaiting-answer flavor
        // (`PausedAwaitingAnswer`), so the transition is atomic: a
        // background-action completion landing concurrently reads the final
        // state and cannot auto-wake an answer-blocked session.
        let reason = if status.is_awaiting_answer() {
            PauseReason::Ask
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
        self.hooks.on_pause(self, &ctx, reason, infer).await;
        Ok(())
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
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        snapshot_step: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        emitter: &Arc<dyn AgentEventEmitter>,
        infer: &(dyn Fn() + Send + Sync),
    ) -> anyhow::Result<()> {
        tracing::info!(
            "ReAct step budget exhausted: session={} next_step={}",
            session_id,
            snapshot_step
        );
        self.save_snapshot_with_branches(
            session_id,
            canonical,
            history,
            snapshot_step,
            branch_points,
        )
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
        self.hooks
            .on_pause(self, &ctx, PauseReason::Budget, infer)
            .await;
        Ok(())
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
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        self.save_snapshot_with_branches(
            session_id,
            canonical,
            history,
            step_number,
            branch_points,
        )
        .await;
    }

    /// Save snapshot including branch points for tree-structured rollback (鎼?).
    ///
    /// Serializes a borrowed view of the ReAct state (no per-step deep copies
    /// of canonical/history/branch_points —those clones were O(n²) over a
    /// long session) into a reusable buffer, then writes to SQLite on the
    /// blocking thread pool so the WAL fsync never stalls the async runtime.
    pub(super) async fn save_snapshot_with_branches(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        let awaiting = self.executor.get_awaiting_answer(session_id).await;
        let view = SnapshotView {
            canonical,
            history,
            step_number,
            branch_points,
            saved_at: Some(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
            awaiting_answer: awaiting.as_ref(),
        };
        // Serialize into the session's own buffer inside a scoped block so the
        // mutex guard is dropped before the await below (the guard is not
        // Send, so it must not be live across the spawn_blocking boundary).
        let bytes = {
            let mut bufs = self.snapshot_bufs.lock().unwrap();
            let buf = bufs.entry(session_id.to_string()).or_default();
            buf.clear();
            if serde_json::to_writer(&mut *buf, &view).is_err() {
                return;
            }
            std::mem::take(buf)
        };
        let json = String::from_utf8(bytes).unwrap_or_default();
        let db = self.db.clone();
        let tid_owned = session_id.to_string();
        // Return ownership of the serialized bytes so the allocation is
        // handed back to the session's buffer for reuse on the next snapshot.
        let back: String = db
            .run_blocking(move |db| {
                if let Err(e) = db.save_react_state(&tid_owned, &json) {
                    tracing::warn!("save_react_state failed for session {}: {}", tid_owned, e);
                }
                Ok(json)
            })
            .await
            .unwrap_or_default();
        if let Ok(mut bufs) = self.snapshot_bufs.lock() {
            *bufs.entry(session_id.to_string()).or_default() = back.into_bytes();
        }
    }

    /// and save a snapshot so the session can be resumed via "continue" or
    /// rolled back. Without this, any text streamed before the error is lost
    /// on page refresh because it was only in the frontend's memory.
    pub(super) async fn persist_partial_on_error(
        &self,
        ctx: &StepCtx,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        branch_points: &mut HashMap<u32, BranchPoint>,
        partial_thought: &Arc<std::sync::Mutex<String>>,
        partial_reasoning: &Arc<std::sync::Mutex<String>>,
    ) {
        // Save a branch point BEFORE persisting the partial output, so
        // last_msg_at captures the timestamp of the last message BEFORE the
        // partial. This lets continue_session / rollback_session precisely delete
        // only the partial output via delete_messages_after(last_msg_at).
        // The canonical/history here represent the state BEFORE the failed
        // LLM call (the response was never pushed to canonical), so resuming
        // will retry the step cleanly.
        // FORCED write: continue_session / rollback_session locate this branch
        // point in the DB snapshot; a throttled (stale) row would silently
        // skip their message truncation.
        self.save_branch_point(
            &ctx.session_id,
            canonical,
            history,
            ctx.step_num,
            branch_points,
            true,
        )
        .await;

        let thought_text = partial_thought.lock().unwrap().clone();
        let reasoning_text = partial_reasoning.lock().unwrap().clone();
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
        // The stream text now lives in the message stream (persisted above),
        // so any checkpointed partial row for this session is obsolete — and an
        // in-flight checkpoint write must not re-create it. Discard goes
        // through the PartialStore, whose generation bump invalidates stale
        // writes.
        self.executor.partials.discard(&ctx.session_id).await;
    }

    /// Save a branch point at the current step before tool execution (—).
    ///
    /// The DB snapshot write is throttled to every `SNAPSHOT_WRITE_INTERVAL`
    /// steps on the happy path (`force = false`): the in-memory branch-point
    /// map is always current, and every pause/error/final path plus every
    /// cancellation exit writes unconditionally. Error paths MUST pass
    /// `force = true` (e.g. `persist_partial_on_error`): `continue_session` /
    /// `rollback_session` locate the failed step's branch point in the DB
    /// snapshot, and a stale row would silently skip their message truncation.
    pub(super) async fn save_branch_point(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
        force: bool,
    ) {
        // `get_last_message_created_at` is a blocking SQLite read; run it on
        // the blocking thread pool instead of the async runtime.
        let db = self.db.clone();
        let session_id_owned = session_id.to_string();
        let last_msg_at = db
            .run_blocking(move |db| Ok(db.get_last_message_created_at(&session_id_owned)))
            .await
            .ok()
            .flatten();
        branch_points.insert(
            step_number,
            BranchPoint {
                canonical: canonical.to_vec(),
                history: history.to_vec(),
                step_number,
                last_msg_at,
            },
        );
        // The throttle marker guard is confined to this block so it is always
        // dropped before the write's await.
        let due = {
            let mut last_written = self.last_snapshot_step.lock().unwrap();
            let due = force
                || last_written.get(session_id).is_none_or(|last| {
                    step_number.saturating_sub(*last) >= SNAPSHOT_WRITE_INTERVAL
                });
            if due {
                last_written.insert(session_id.to_string(), step_number);
            }
            due
        };
        if due {
            self.save_snapshot_with_branches(
                session_id,
                canonical,
                history,
                step_number,
                branch_points,
            )
            .await;
        }
    }

}
