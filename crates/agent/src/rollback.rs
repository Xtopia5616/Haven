//! Session rollback / resume logic: rewinding a session to a saved branch
//! point, resuming an errored session from its snapshot, and trimming a
//! half-built trailing tool call before a resume.
//!
//! Split out of `lib.rs` so the `AgentLayer` dispatch entrypoint stays
//! readable; these methods operate on the same private fields (`db`,
//! `executor`) via `impl AgentLayer` blocks in this module.

use crate::AgentLayer;
use crate::lifecycle::{LifecycleOp, LifecycleWindow, decide};
use crate::react::DurableEventState;
use crate::resume_support::infer_resume_step;
use crate::rollback_support::truncate_at_user_message;
use crate::session::SessionStatus;
use crate::types::{BranchPoint, TranscriptRecord, project_transcript_with_strategy};
use haven_memory::{RollbackProjectionBoundary, RollbackRequest};

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
        // Reject an end that already owns this session before cancelling a
        // run or touching action/interaction state. The commit path below
        // rechecks under the same lifecycle gate to cover an end that begins
        // while rollback is waiting for a running handler to exit.
        {
            let _lifecycle = self.executor.lifecycle_guard().await;
            self.executor.ensure_lifecycle_open()?;
            if self.executor.is_session_closing(session_id) {
                anyhow::bail!("session '{}' is closing; retry rollback later", session_id);
            }
        }

        // Validate the exact requested row before lifecycle handling can
        // cancel tool_runs, clear interactions, or otherwise change session
        // state. The store repeats this lookup in its rollback transaction.
        let target_msg = match target_message_id {
            Some(message_id) => Some(
                self.react_engine
                    .event_store
                    .load_rollback_target_message(session_id, message_id)
                    .await?,
            ),
            None if pause => {
                return Err(anyhow::anyhow!(
                    "rollback_session {}: pause=true requires target_message_id",
                    session_id
                ));
            }
            None => None,
        };

        let state = self.executor.get_active_session_status(session_id).await;
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
                self.executor.await_run_finished(session_id).await?;
            }
            crate::lifecycle::LifecycleDecision::AwaitThenAllow => {
                self.executor.await_run_finished(session_id).await?;
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

        let durable_state = self
            .react_engine
            .load_durable_event_state(session_id)
            .await?;
        if durable_state.is_none() {
            return Err(anyhow::anyhow!(
                "rollback_session {}: no session event log; reset is required",
                session_id
            ));
        }
        let mut replay = match durable_state {
            Some(durable) => DurableEventState {
                events: durable.events,
                branch_points: durable.branch_points,
                cursor: durable.cursor,
            },
            None => unreachable!("durable state absence handled above"),
        };

        // Compaction replaces the pre-compaction event prefix and clears its
        // rollback points. Refuse to pretend those old messages are still
        // restorable: rollback is an overwrite operation, not a branch tree,
        // and silently falling back to the compacted head would target the
        // wrong timeline.
        if target_step < infer_resume_step(&replay.events)
            && replay
                .events
                .first()
                .is_some_and(|event| matches!(event, TranscriptRecord::CompactSummary { .. }))
            && !replay.branch_points.contains_key(&target_step)
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
        let bp = if let Some(bp) = replay.branch_points.get(&target_step).cloned() {
            bp
        } else {
            tracing::warn!(
                "rollback_session {}: no branch_point at step {}, using snapshot state (step_number={})",
                session_id,
                target_step,
                infer_resume_step(&replay.events)
            );
            BranchPoint {
                event_cursor: replay.events.len(),
                step_number: target_step,
                last_msg_at: None,
            }
        };

        // Restore the append-only event log to the recorded cursor.
        replay.events.truncate(bp.event_cursor);

        // If the branch point was saved right after a ToolCall event but
        // before ToolResult(s) were appended, the projected canonical ends
        // with an assistant message carrying `tool_calls` but no matching
        // tool-result messages. Sending this to the LLM triggers a 400.
        // Trim the dangling ToolCall (and its Thought) so the loop
        // re-requests the tool call cleanly.
        crate::rollback_support::trim_dangling_tool_call(&mut replay.events);

        // Newest branch-point cutoff (computed BEFORE pruning): used below to
        // detect a user message persisted after every branch point.
        let max_bp_ts = replay
            .branch_points
            .values()
            .filter_map(|b| b.last_msg_at.clone())
            .max();

        // Prune branch points created after the target step, and any whose
        // cursor now sits past the truncated events.
        let event_len = replay.events.len();
        replay
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
        // User-message rollback (pause=true) needs the EXACT clicked
        // message. The old fallbacks — matching by content when the id
        // missed, or guessing the newest user message — could delete the
        // wrong message, so an unresolvable id is rejected before any
        // lifecycle mutation.
        // Step rollbacks (pause=false) need no message id at all.
        let is_orphan_rollback = target_msg.as_ref().is_some_and(|m| {
            m.role == "user"
                && max_bp_ts
                    .as_deref()
                    .is_some_and(|max| m.created_at.as_str() > max)
        });

        // For user-message rollback, also remove the user message from the
        // restored events so the LLM doesn't see it when the session resumes.
        // Skipped for orphan rollback: the orphaned message was never in the
        // events, so truncating would drop a legitimately processed inject.
        //
        // Match the exact `UserInject.message_id` or compacted canonical
        // message id. Text is never used as an identity fallback.
        let target_in_compacted_summary = pause
            && !is_orphan_rollback
            && target_msg.as_ref().is_some_and(|target| {
                replay.events.iter().any(|event| {
                    matches!(
                        event,
                        TranscriptRecord::CompactSummary { compacted, .. }
                            if compacted.iter().any(|message| {
                                message.id.as_deref() == Some(target.id.as_str())
                            })
                    )
                })
            });
        if pause
            && !is_orphan_rollback
            && let Some(target) = target_msg.as_ref()
            && !truncate_at_user_message(
                &mut replay.events,
                &mut replay.branch_points,
                target_step,
                &target.id,
            )
        {
            return Err(anyhow::anyhow!(
                "rollback_session {}: target user message not found in the restored events",
                session_id
            ));
        }

        // SessionStore::rollback_to applies this cutoff atomically with the
        // timeline marker below. User-message rollback is inclusive so the
        // clicked message itself is removed; step rollback keeps the branch
        // cutoff and removes only newer projection rows.
        let projection_boundary = if pause {
            RollbackProjectionBoundary::UserMessage {
                message_id: target_msg
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("rollback target message is missing"))?
                    .id
                    .clone(),
            }
        } else {
            RollbackProjectionBoundary::BranchPoint
        };

        // A compact summary is itself the active transcript root. Removing a
        // message from its canonical payload cannot be represented by a
        // cursor-only marker, so append the filtered root after the marker in
        // the same SessionStore transaction.
        let replacement_transcript = if target_in_compacted_summary {
            replay
                .events
                .iter()
                .map(|event| crate::react::ReActEngine::transcript_event_input(event, 0))
                .collect::<anyhow::Result<Vec<_>>>()?
        } else {
            Vec::new()
        };

        // Serialize the durable rollback commit and its paired interaction /
        // status projection against explicit end. If end acquired the marker
        // while this operation joined a run, no transcript mutation occurs.
        let _lifecycle = self.executor.lifecycle_guard().await;
        self.executor.ensure_lifecycle_open()?;
        if self.executor.is_session_closing(session_id) {
            anyhow::bail!("session '{}' is closing; retry rollback later", session_id);
        }

        // Background tool_runs spawned before the rollback are stale relative
        // to the restored snapshot: kill them so their children cannot leak.
        self.executor.cancel_session_tool_runs(session_id).await;

        // Drop any checkpointed partial stream text only after lifecycle
        // admission. The restored timeline must not inherit stale partials.
        self.executor.partials.discard(session_id).await;

        // Keep the database event log append-only. The marker changes the
        // active replay cursor; discarded rows remain available for audit and
        // can never leak into the resumed timeline.
        let event_cursor = replay.events.len();
        let rollback_request = RollbackRequest {
            expected_event_sequence: replay.cursor.event_sequence,
            transcript_cursor: event_cursor,
            target_step,
            projection_boundary,
        };
        self.react_engine
            .event_store
            .rollback_to_async(session_id, rollback_request, replacement_transcript, None)
            .await?;

        // Clear after the atomic rollback so ingress cannot route to requests
        // that no longer exist. Keep this paired with the durable marker under
        // the lifecycle gate so a concurrent end cannot leave a half-rollback.
        self.executor.clear_interactions(session_id, None).await?;

        // Clear after the atomic rollback so in-memory counters re-seed from
        // the rebuilt DB row and late detached persists are ignored.
        self.react_engine
            .invalidate_usage_after_truncate(session_id);

        // Rebuild per-session tool registrations from the restored rounds so
        // that tools loaded after the rollback point are dropped, and tools
        // loaded before it remain available.
        // Cursor-aware project (equivalent to project() after truncate).
        let (_, rounds) =
            project_transcript_with_strategy(&replay.events, self.react_engine.media_strategy());
        self.restore_per_session_tools(session_id, &rounds).await;

        // Reload the session into executor memory (it may have been removed if we
        // marked a Running session as Error above, or was never loaded after restart).
        self.executor
            .ensure_session_loaded_locked(session_id)
            .await?;

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
        loop {
            let state = self.executor.get_session_status(session_id).await;
            let run_in_flight = self.executor.is_run_in_flight(session_id).await;
            let window = LifecycleWindow::classify(state.as_ref(), run_in_flight);
            match decide(window, LifecycleOp::ErroredContinue, state.as_ref()) {
                crate::lifecycle::LifecycleDecision::AwaitThenAllow
                | crate::lifecycle::LifecycleDecision::CancelThenAllow => {
                    self.executor.await_run_finished(session_id).await?;
                    continue;
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

            // Serialize the durable transcript rewrite and status transition
            // with explicit end. A Continue selected after an end-cleanup
            // failure is an explicit choice to resume the session; any
            // remaining tool_runs keep their ordinary owner lifecycle.
            let _lifecycle = self.executor.lifecycle_guard().await;
            self.executor.ensure_lifecycle_open()?;
            if self.executor.is_session_closing(session_id) {
                anyhow::bail!("session '{}' is closing; retry continue later", session_id);
            }
            self.executor
                .ensure_session_loaded_locked(session_id)
                .await?;

            let state = self.executor.get_session_status(session_id).await;
            let run_in_flight = self.executor.is_run_in_flight(session_id).await;
            let window = LifecycleWindow::classify(state.as_ref(), run_in_flight);
            match decide(window, LifecycleOp::ErroredContinue, state.as_ref()) {
                crate::lifecycle::LifecycleDecision::AwaitThenAllow
                | crate::lifecycle::LifecycleDecision::CancelThenAllow => {
                    drop(_lifecycle);
                    self.executor.await_run_finished(session_id).await?;
                    continue;
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

            // The store owns the recovery marker decision and applies an
            // authorized projection cutoff in the same transaction.
            self.react_engine
                .event_store
                .truncate_projection_after_latest_committed_recovery_async(session_id)
                .await?;
            // Clear after join + truncation so unwind persists cannot leave a
            // stale-high cutoff in the mid-run branch-point cache. Invalidate
            // usage so retry re-seeds from rebuilt totals.
            self.react_engine
                .invalidate_usage_after_truncate(session_id);

            // Drop any checkpointed partial stream text: the retry re-streams
            // from scratch, so a crash during the retry must not promote the
            // pre-retry partial. Goes through PartialStore to prevent stale
            // checkpoint writes from resurrecting the row.
            self.executor.partials.discard(session_id).await;

            // Continuing explicitly cancels every pending interaction; status
            // alone flipping to Pending is not enough because the actor owns
            // the request lifecycle.
            if state.is_some_and(|status| status.is_paused()) {
                self.executor
                    .clear_interactions_persisted(session_id, None)
                    .await?;
            }

            // An explicit Continue is the user's decision to try the provider
            // again. Clear only the chat route's consecutive-failure gate so
            // the queued run can make one fresh attempt without cooldown.
            self.react_engine.prepare_manual_retry().await;

            // Set to Pending for the dispatcher to pick up.
            self.set_session_status(session_id, SessionStatus::Pending)
                .await?;

            tracing::info!(
                "continue_session: session {} set to Pending for retry",
                session_id
            );
            return Ok(());
        }
    }
}
