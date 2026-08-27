//! Session rollback / resume logic: rewinding a session to a saved branch
//! point, resuming an errored session from its snapshot, and trimming a
//! half-built trailing tool call before a resume.
//!
//! Split out of `lib.rs` so the `AgentLayer` dispatch entrypoint stays
//! readable; these methods operate on the same private fields (`db`,
//! `executor`) via `impl AgentLayer` blocks in this module.

use haven_common::types::{CanonicalRole, ContentPart};

use crate::AgentLayer;
use crate::lifecycle::{LifecycleOp, LifecycleWindow, decide};
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

        // R6: drop ask/confirm gates before restore so ingress cannot mis-route
        // the next user input as an answer to a gate that no longer exists.
        // The restored snapshot also clears `awaiting_*` before save below.
        self.executor.clear_awaiting_answer(session_id).await;
        self.executor.clear_awaiting_confirm(session_id).await;

        let state_json = match self.db.get_react_state(session_id)? {
            Some(s) => s,
            None => {
                // No saved state at all — this happens when a session errored
                // before any snapshot was saved (e.g. first LLM call failed
                // in an older version without Fix 1). We can't restore
                // events, but we can still truncate session messages so
                // the user can edit and re-send their input.
                tracing::warn!(
                    "rollback_session {}: no react_state — falling back to message-only truncation",
                    session_id
                );
                if pause {
                    // User-message rollback needs the exact clicked message;
                    // the "newest user message" guess is gone — an
                    // unresolvable id is an error.
                    let id = target_message_id.ok_or_else(|| {
                        anyhow::anyhow!(
                            "rollback_session {}: pause=true requires target_message_id",
                            session_id
                        )
                    })?;
                    let target = self
                        .db
                        .get_session_messages(session_id)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|m| m.id == id)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "rollback_session {}: target message '{}' not found in session messages",
                                session_id,
                                id
                            )
                        })?;
                    let _ = self.db.delete_messages_from(session_id, &target.created_at);
                    let _ = self
                        .db
                        .delete_llm_usage_from(session_id, &target.created_at);
                    let _ = self.db.rebuild_session_usage_from_calls(session_id);
                } else if let Some(ts) = self.db.last_user_message_ts(session_id) {
                    let _ = self.db.truncate_session_after(session_id, &ts, false);
                }
                // After truncation (and after any in-flight run join above):
                // drop the cutoff cache so a late persist cannot repopulate
                // timestamps that no longer exist. Also invalidate in-memory
                // usage so the next run re-seeds from the rebuilt DB totals.
                self.react_engine.clear_last_msg_at(session_id);
                self.react_engine
                    .invalidate_usage_after_truncate(session_id);
                // Reload into memory and set status.
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
                return Ok(());
            }
        };
        let mut snapshot = ReActSnapshot::from_json(&state_json)?;

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
            let cutoff_ts = self.db.last_user_message_ts(session_id);
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
        Self::trim_dangling_tool_call(&mut snapshot.events);

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
                self.db.delete_messages_from(session_id, &user_ts)?;
                // Rollback overwrites: drop step rows recorded after
                // the user message too (they belong to the discarded
                // timeline).
                self.db.delete_session_steps_after(session_id, &user_ts)?;
                // Usage for the discarded assistant turns is at-or-after the
                // user message; cut it and rebuild cumulative counters so
                // token stats do not stay inflated after edit-resend.
                self.db.delete_llm_usage_from(session_id, &user_ts)?;
                self.db.rebuild_session_usage_from_calls(session_id)?;
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
        // Match `UserInject.text` (raw; adapters add wire prefixes) or a User
        // row inside a CompactSummary seed. Also accept historically prefixed
        // text via `InjectSource::match_prefixes`.
        if pause
            && !is_orphan_rollback
            && let Some(target) = target_msg
        {
            let target_content = target.content.as_str();
            let prefixes = haven_common::types::InjectSource::match_prefixes();
            let matches_target = |t: &str| {
                t == target_content
                    || prefixes.iter().any(|p| {
                        t.strip_prefix(p.as_str())
                            .is_some_and(|rest| rest == target_content)
                    })
            };
            let mut found = false;
            if let Some(pos) = snapshot.events.iter().rposition(|ev| {
                matches!(
                    ev,
                    TranscriptRecord::UserInject { text, .. } if matches_target(text)
                )
            }) {
                // Keep everything before the target inject. Drop it and any
                // events that followed it.
                snapshot.events.truncate(pos);
                let event_len = snapshot.events.len();
                for bp in snapshot.branch_points.values_mut() {
                    if bp.event_cursor > event_len {
                        bp.event_cursor = event_len;
                    }
                }
                snapshot
                    .branch_points
                    .retain(|&k, b| k <= target_step && b.event_cursor <= event_len);
                found = true;
            } else {
                // CompactSummary seed: trim the compacted user row in place,
                // then drop every event after that CompactSummary so
                // post-summary transcript cannot linger.
                for idx in (0..snapshot.events.len()).rev() {
                    if let TranscriptRecord::CompactSummary { compacted, .. } =
                        &mut snapshot.events[idx]
                        && let Some(pos) = compacted.iter().rposition(|m| {
                            m.role == CanonicalRole::User
                                && m.content
                                    .iter()
                                    .any(|p| matches!(p, ContentPart::Text(t) if matches_target(t)))
                        })
                    {
                        compacted.truncate(pos);
                        snapshot.events.truncate(idx + 1);
                        let event_len = snapshot.events.len();
                        for bp in snapshot.branch_points.values_mut() {
                            if bp.event_cursor > event_len {
                                bp.event_cursor = event_len;
                            }
                        }
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                return Err(anyhow::anyhow!(
                    "rollback_session {}: target user message not found in the restored events",
                    session_id
                ));
            }
        }

        // R6: branch restore must not resurrect ask/confirm gates from the
        // parent snapshot (continue clears them; rollback previously did not).
        snapshot.awaiting_answer = None;
        snapshot.awaiting_confirm = None;
        // Budget is per-run observability; a restored branch starts a new run.
        snapshot.run_budget = None;

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

        let state = self.executor.get_session_state(session_id).await;
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
        if let Ok(Some(state_json)) = self.db.get_react_state(session_id)
            && let Ok(snapshot) = ReActSnapshot::from_json(&state_json)
            && let Some(error_partial_message_ids) = snapshot.error_partial_message_ids
        {
            if let Some(cutoff) = snapshot
                .branch_points
                .get(&snapshot.step_number)
                .and_then(|bp| bp.last_msg_at.as_deref())
            {
                // The marker is saved immediately after save_branch_point
                // in persist_partial_on_error, so this range belongs to
                // the known failed attempt, including its step projection.
                self.db.truncate_session_after(session_id, cutoff, false)?;
            } else {
                // A partially persisted error snapshot may lack a branch
                // point. Its explicit recovery IDs are still safe.
                self.db
                    .delete_messages_by_ids(session_id, &error_partial_message_ids)?;
            }
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

        // Skipping an ask / confirm via continue must drop the gate (status
        // alone flipping to Pending is not enough — the loop still reads the
        // flag).
        if matches!(state, Some(SessionStatus::PausedAwaitingAnswer)) {
            self.executor
                .clear_awaiting_answer_persisted(session_id)
                .await;
        }
        if matches!(state, Some(SessionStatus::PausedAwaitingConfirm)) {
            self.executor
                .clear_awaiting_confirm_persisted(session_id)
                .await;
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

    /// If the event log ends with a [`TranscriptRecord::ToolCall`] that has
    /// non-empty `tool_calls` and no following [`TranscriptRecord::ToolResult`],
    /// the projected canonical ends with an assistant message carrying
    /// `tool_calls` but no matching tool results — providers reject that with
    /// a 400. This happens when a snapshot/branch point was saved right after
    /// the assistant message but before tool results were appended
    /// (`save_branch_point` runs before tool execution; the app may die or be
    /// cancelled mid-batch).
    ///
    /// Empty-`tool_calls` ToolCalls are final-answer / search assistant turns
    /// and must be kept.
    ///
    /// Pop the dangling `ToolCall` and, when present, the preceding
    /// same-step `Thought` so the loop re-requests the tool call cleanly.
    pub(crate) fn trim_dangling_tool_call(events: &mut Vec<TranscriptRecord>) {
        let Some(TranscriptRecord::ToolCall {
            step_number,
            tool_calls,
            ..
        }) = events.last()
        else {
            return;
        };
        if tool_calls.is_empty() {
            return;
        }
        let step = *step_number;
        events.pop();
        if let Some(TranscriptRecord::Thought { step_number, .. }) = events.last()
            && *step_number == step
        {
            events.pop();
        }
    }
}
