//! Session rollback / resume logic: rewinding a session to a saved branch
//! point, resuming an errored session from its snapshot, and trimming a
//! half-built trailing tool call before a resume.
//!
//! Split out of `lib.rs` so the `AgentLayer` dispatch entrypoint stays
//! readable; these methods operate on the same private fields (`db`,
//! `executor`) via `impl AgentLayer` blocks in this module.

use crate::AgentLayer;
use crate::lifecycle::{LifecycleOp, LifecycleWindow, decide};
use crate::rollback_support::truncate_at_user_message;
use crate::session::SessionStatus;
use crate::types::{BranchPoint, ReActSnapshot, TranscriptRecord};

impl AgentLayer {
    /// Roll back a session to a specific branch point. The session is rewound
    /// to the saved state at that step. When `pause` is true the session is
    /// set to Paused (user wants to edit the message before re-sending);
    /// otherwise it is set to Pending for immediate re-execution.
    /// `target_message_id` is the id of the exact message being rolled back;
    /// it lets the backend detect an orphan rollback (a user message that was
    /// never processed into the ReAct context). The id must resolve to a
    /// persisted session message when `pause` is true — an unresolvable id is
    /// an error, not a content-based guess.
    ///
    /// R6: when a dispatcher run slot is held (claim→spawn / stream / tools /
    /// pause unwind), cancel + join before restore so late writes cannot
    /// overwrite the restored snapshot. Ask/confirm gates are always cleared.
    pub async fn rollback_session(
        &self,
        session_id: &str,
        target_step: u32,
        pause: bool,
        target_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        let state = self.executor.get_session_state(session_id).await;
        let run_in_flight = self.executor.is_run_in_flight(session_id).await;
        let window = LifecycleWindow::classify(state.as_ref(), run_in_flight);
        match decide(window, LifecycleOp::BranchRollback, state.as_ref()) {
            crate::lifecycle::LifecycleDecision::CancelThenAllow => {
                // Cancel even when status already left Running (pause unwind /
                // claim→spawn / direct run): the loop observes the token at
                // wait points and exits without Error marking. Join via
                // `await_run_finished` (oneshot on slot release) so
                // exit_cancelled cannot overwrite the restored snapshot.
                let cancel = self.executor.cancellation_token(session_id).await;
                cancel.cancel();
                self.executor.await_run_finished(session_id).await;
            }
            crate::lifecycle::LifecycleDecision::AwaitThenAllow => {
                self.executor.await_run_finished(session_id).await;
            }
            crate::lifecycle::LifecycleDecision::Deny => {
                return Err(anyhow::anyhow!(
                    "rollback_session {}: denied in lifecycle window {:?}",
                    session_id,
                    window
                ));
            }
            crate::lifecycle::LifecycleDecision::Allow
            | crate::lifecycle::LifecycleDecision::NotApplicable => {}
        }

        // Background actions spawned before the rollback are stale relative to
        // the restored snapshot: kill them so their children cannot leak.
        self.executor.cancel_session_actions(session_id).await;

        // R6: drop every interaction before restore so ingress cannot route
        // input to a request that no longer exists.
        self.executor.clear_interactions(session_id, None).await;

        // Snapshot bytes are only a cache. Keep a read error around until the
        // event stream has been checked: a durable timeline can still provide
        // every rollback input when the cache is corrupt or unreadable.
        let (state_json, state_json_error) = match self.db.get_react_state(session_id) {
            Ok(state) => (state, None),
            Err(error) => (None, Some(error)),
        };
        let mut durable_state = self
            .react_engine
            .load_durable_event_state(session_id)
            .await?;
        if durable_state.is_none()
            && let Some(state_json) = state_json.as_deref()
        {
            let cached = ReActSnapshot::from_json(state_json)?;
            self.react_engine
                .seed_snapshot_events(session_id, &cached, 0)
                .await?;
            durable_state = self
                .react_engine
                .load_durable_event_state(session_id)
                .await?;
        }
        if durable_state.is_none()
            && let Some(error) = state_json_error
        {
            return Err(anyhow::anyhow!(
                "rollback_session {}: failed to read legacy snapshot: {}",
                session_id,
                error
            ));
        }
        let mut snapshot = match durable_state {
            Some(durable) => {
                // Snapshot metadata is a cache. If it is unavailable or
                // corrupt, the event stream still supplies the active
                // transcript and branch control plane.
                let mut snapshot = state_json
                    .as_deref()
                    .and_then(|json| ReActSnapshot::from_json(json).ok())
                    .unwrap_or_default();
                snapshot.events = durable.events;
                snapshot.branch_points = durable.branch_points;
                snapshot
            }
            None => match state_json {
                Some(state_json) => ReActSnapshot::from_json(&state_json)?,
                None => {
                    return Err(anyhow::anyhow!(
                        "rollback_session {}: no session event log; session is not resumable",
                        session_id
                    ));
                }
            },
        };

        // Compaction replaces the pre-compaction event prefix and clears its
        // rollback points. Refuse to pretend those old messages are still
        // restorable: rollback is an overwrite operation, not a branch tree,
        // and silently falling back to the compacted head would target the
        // wrong timeline.
        if target_step < snapshot.step_number
            && snapshot
                .events
                .first()
                .is_some_and(|event| matches!(event, TranscriptRecord::CompactSummary { .. }))
            && !snapshot.branch_points.contains_key(&target_step)
        {
            return Err(anyhow::anyhow!(
                "rollback_session {}: step {} is before the current compaction boundary and is no longer restorable",
                session_id,
                target_step
            ));
        }

        // If no branch_point exists at the target step, the step likely
        // failed before save_branch_point was called (e.g. LLM error
        // mid-stream). In that case the snapshot's current events ARE the
        // pre-step state — use them directly.
        let bp = if let Some(bp) = snapshot.branch_points.get(&target_step).cloned() {
            bp
        } else {
            tracing::warn!(
                "rollback_session {}: no branch_point at step {}, using snapshot state (step_number={})",
                session_id,
                target_step,
                snapshot.step_number
            );
            // Determine the cutoff timestamp from session messages: the last
            // user message for user-rollback (pause=true), or the last user
            // message for agent-rollback too (delete the partial output after
            // it).
            let cutoff_ts = self.db.last_user_message_ts(session_id)?;
            BranchPoint {
                event_cursor: snapshot.events.len(),
                step_number: target_step,
                last_msg_at: cutoff_ts,
            }
        };

        // Restore the append-only event log to the recorded cursor.
        snapshot.events.truncate(bp.event_cursor);
        snapshot.step_number = bp.step_number;

        // If the branch point was saved right after a ToolCall event but
        // before ToolResult(s) were appended, the projected canonical ends
        // with an assistant message carrying `tool_calls` but no matching
        // tool-result messages. Sending this to the LLM triggers a 400.
        // Trim the dangling ToolCall (and its Thought) so the loop
        // re-requests the tool call cleanly.
        crate::rollback_support::trim_dangling_tool_call(&mut snapshot.events);

        // Newest branch-point cutoff (computed BEFORE pruning): used below to
        // detect a user message persisted after every branch point.
        let max_bp_ts = snapshot
            .branch_points
            .values()
            .filter_map(|b| b.last_msg_at.clone())
            .max();

        // Prune branch points created after the target step, and any whose
        // cursor now sits past the truncated events.
        let event_len = snapshot.events.len();
        snapshot
            .branch_points
            .retain(|&k, b| k <= target_step && b.event_cursor <= event_len);

        // Truncate session messages persisted after the branch point so the
        // conversation context matches the restored snapshot.
        //
        // A user message may be persisted AFTER the newest branch point:
        // an interjection sent while the session was erroring (its supplement
        // was dropped as the session was terminal) or before the app closed
        // mid-generation (the steering queue is in-memory only and is lost).
        // Such a message was never added to the ReAct events, so rolling
        // back to it must discard ONLY that message — deleting from the
        // branch point's cutoff would wipe valid earlier history.
        let session_msgs = self.db.get_session_messages(session_id)?;
        // User-message rollback (pause=true) needs the EXACT clicked
        // message. The old fallbacks — matching by content when the id
        // missed, or guessing the newest user message — could delete the
        // wrong message, so an unresolvable id is now an error instead.
        // Step rollbacks (pause=false) need no message id at all.
        let target_msg = if pause {
            let id = target_message_id.ok_or_else(|| {
                anyhow::anyhow!(
                    "rollback_session {}: pause=true requires target_message_id",
                    session_id
                )
            })?;
            Some(session_msgs.iter().find(|m| m.id == id).ok_or_else(|| {
                anyhow::anyhow!(
                    "rollback_session {}: target message '{}' not found in session messages",
                    session_id,
                    id
                )
            })?)
        } else {
            None
        };
        let is_orphan_rollback = target_msg.is_some_and(|m| {
            m.role == "user"
                && max_bp_ts
                    .as_deref()
                    .is_some_and(|max| m.created_at.as_str() > max)
        });

        if let Some(ref ts) = bp.last_msg_at {
            if pause {
                // User-message rollback: delete the user message itself too
                // (inclusive), so the context is clean when the user re-sends
                // an edited version. `target_msg` is guaranteed to resolve
                // (validated above), so its own timestamp is authoritative —
                // the "newest user message at/before the branch point" guess
                // is gone.
                let user_ts = target_msg
                    .expect("pause target resolved above")
                    .created_at
                    .clone();
                // Rollback overwrites: remove the clicked user message and
                // every projection row after it in one transaction, including
                // the cumulative usage rebuild.
                self.db.truncate_session_after(session_id, &user_ts, true)?;
            } else {
                // Strict `>` for both: the branch-point cutoff is the last
                // message BEFORE the discarded step, so we keep the cutoff
                // itself intact (truncate_session_after is non-inclusive).
                // Also rebuilds session_usage from remaining llm_usage rows.
                self.db.truncate_session_after(session_id, ts, false)?;
            }
        }
        // Clear after join + truncation so unwind persists cannot leave a
        // stale-high cutoff in the mid-run branch-point cache. Invalidate
        // usage so in-memory counters re-seed from the rebuilt DB row and
        // late detached persists from discarded calls are ignored.
        self.react_engine.clear_last_msg_at(session_id);
        self.react_engine
            .invalidate_usage_after_truncate(session_id);

        // Drop any checkpointed partial stream text: the restored timeline
        // must not inherit a stale partial from the discarded run. Discard
        // goes through the executor's PartialStore so an in-flight stream
        // checkpoint cannot re-create the row afterwards.
        self.executor.partials.discard(session_id).await;

        // For user-message rollback, also remove the user message from the
        // restored events so the LLM doesn't see it when the session resumes.
        // Skipped for orphan rollback: the orphaned message was never in the
        // events, so truncating would drop a legitimately processed inject.
        //
        // Match the exact `UserInject.message_id` or compacted canonical
        // message id. Text is never used as an identity fallback.
        if pause
            && !is_orphan_rollback
            && let Some(target) = target_msg
            && !truncate_at_user_message(
                &mut snapshot.events,
                &mut snapshot.branch_points,
                target_step,
                &target.id,
            )
        {
            return Err(anyhow::anyhow!(
                "rollback_session {}: target user message not found in the restored events",
                session_id
            ));
        }

        // R6: branch restore must not resurrect interactions from the parent
        // snapshot (continue clears them; rollback previously did not).
        snapshot.interactions.clear();
        // Budget is per-run observability; a restored branch starts a new run.
        snapshot.run_budget = None;

        // Keep the database event log append-only. The marker changes the
        // active replay cursor; discarded rows remain available for audit and
        // can never leak into the resumed timeline.
        let event_store = self.react_engine.event_store.clone();
        let sid = session_id.to_string();
        let event_cursor = snapshot.events.len();
        let branch_point_sequence = self
            .db
            .run_blocking({
                let event_store = self.react_engine.event_store.clone();
                let sid = session_id.to_string();
                move |_| {
                    Ok(event_store
                        .branch_point_for_step(&sid, target_step)?
                        .filter(|(_, cursor, _)| *cursor == event_cursor)
                        .map(|(event, _, _)| event.sequence))
                }
            })
            .await?;
        let to_sequence = if let Some(sequence) = branch_point_sequence {
            sequence
        } else {
            self.db
                .run_blocking(move |_| {
                    event_store.sequence_for_transcript_cursor(&sid, event_cursor)
                })
                .await?
        };
        let event_store = self.react_engine.event_store.clone();
        let sid = session_id.to_string();
        self.db
            .run_blocking(move |_| {
                event_store.append_rollback(&sid, to_sequence, target_step, None)?;
                Ok(())
            })
            .await?;

        let json = serde_json::to_string(&snapshot)?;
        self.db.save_react_state(session_id, &json)?;

        // Rebuild per-session tool registrations from the restored rounds so
        // that tools loaded after the rollback point are dropped, and tools
        // loaded before it remain available.
        // Cursor-aware project (equivalent to project() after truncate).
        let (_, rounds) = snapshot.project_at(snapshot.events.len());
        self.restore_per_session_tools(session_id, &rounds).await;

        // Reload the session into executor memory (it may have been removed if we
        // marked a Running session as Error above, or was never loaded after restart).
        self.executor.ensure_session_loaded(session_id).await?;

        self.set_session_status(
            session_id,
            if pause {
                SessionStatus::Paused
            } else {
                SessionStatus::Pending
            },
        )
        .await?;
        if pause {
            tracing::info!(
                "rollback_session {} to step {}: session set to Paused (user-edit mode)",
                session_id,
                target_step
            );
        } else {
            tracing::info!(
                "rollback_session {} to step {}: session set to Pending",
                session_id,
                target_step
            );
        }
        Ok(())
    }

    /// Resume a session that errored mid-step. Removes any partial assistant
    /// output that was persisted on error (so the retry produces a clean
    /// message), then sets the session to Pending so the dispatcher picks it up
    /// and `run_session_from_id` restores from the saved snapshot.
    pub async fn continue_session(&self, session_id: &str) -> anyhow::Result<()> {
        // Ensure the session is loaded in executor memory.
        self.executor.ensure_session_loaded(session_id).await?;

        let state = self.executor.get_session_status(session_id).await;
        // R6: lifecycle matrix owns continue allow/deny (Error|Paused* only).
        // If pause already flipped status but the handler is still unwinding,
        // join before truncating messages / flipping to Pending.
        let run_in_flight = self.executor.is_run_in_flight(session_id).await;
        let window = LifecycleWindow::classify(state.as_ref(), run_in_flight);
        match decide(window, LifecycleOp::ErroredContinue, state.as_ref()) {
            crate::lifecycle::LifecycleDecision::AwaitThenAllow
            | crate::lifecycle::LifecycleDecision::CancelThenAllow => {
                self.executor.await_run_finished(session_id).await;
            }
            crate::lifecycle::LifecycleDecision::Deny => {
                return Err(anyhow::anyhow!(
                    "session is not in a retryable state (current: {:?})",
                    state
                ));
            }
            crate::lifecycle::LifecycleDecision::Allow
            | crate::lifecycle::LifecycleDecision::NotApplicable => {}
        }

        // A normal branch point is a periodic checkpoint, not necessarily a
        // boundary for this error (after an app restart it can be several
        // completed steps old). Only an explicit failed-stream marker makes
        // this attempt's branch point safe to truncate.
        match self.db.get_react_state(session_id) {
            Ok(Some(state_json)) => match ReActSnapshot::from_json(&state_json) {
                Ok(snapshot) => {
                    if let Some(error_partial_message_ids) = snapshot.error_partial_message_ids {
                        if let Some(cutoff) = snapshot
                            .branch_points
                            .get(&snapshot.step_number)
                            .and_then(|bp| bp.last_msg_at.as_deref())
                        {
                            // The marker is saved immediately after
                            // save_branch_point in persist_partial_on_error,
                            // so this range belongs to the known failed
                            // attempt, including its step projection.
                            self.db.truncate_session_after(session_id, cutoff, false)?;
                        } else {
                            // A partially persisted error snapshot may lack a
                            // branch point. Its explicit recovery IDs are
                            // still safe.
                            self.db
                                .delete_messages_by_ids(session_id, &error_partial_message_ids)?;
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    session_id,
                    error = %error,
                    "ignoring corrupt snapshot while continuing from durable session events"
                ),
            },
            Ok(None) => {}
            Err(error) => tracing::warn!(
                session_id,
                error = %error,
                "ignoring unreadable snapshot while continuing from durable session events"
            ),
        }
        // Clear after join + truncation so unwind persists cannot leave a
        // stale-high cutoff in the mid-run branch-point cache. Invalidate
        // usage so retry re-seeds from rebuilt totals.
        self.react_engine.clear_last_msg_at(session_id);
        self.react_engine
            .invalidate_usage_after_truncate(session_id);

        // Drop any checkpointed partial stream text: the retry re-streams
        // from scratch, so a crash during the retry must not promote the
        // pre-retry partial. Goes through the PartialStore so no in-flight
        // checkpoint can resurrect the row.
        self.executor.partials.discard(session_id).await;

        // Continuing explicitly cancels every pending interaction; status
        // alone flipping to Pending is not enough because the actor owns the
        // request lifecycle.
        if state.is_some_and(|status| status.is_paused()) {
            self.executor
                .clear_interactions_persisted(session_id, None)
                .await?;
        }

        // Set to Pending for the dispatcher to pick up.
        self.set_session_status(session_id, SessionStatus::Pending)
            .await?;

        tracing::info!(
            "continue_session: session {} set to Pending for retry",
            session_id
        );
        Ok(())
    }
}
