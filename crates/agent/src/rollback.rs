//! Session rollback / resume logic: rewinding a session to a saved branch
//! point, resuming an errored session from its snapshot, and trimming a
//! half-built trailing tool call before a resume.
//!
//! Split out of `lib.rs` so the `AgentLayer` dispatch entrypoint stays
//! readable; these methods operate on the same private fields (`db`,
//! `executor`) via `impl AgentLayer` blocks in this module.

use crate::AgentLayer;
use crate::sanitize_canonical;
use crate::session::SessionStatus;
use crate::types::{BranchPoint, ReActSnapshot, ReActStep};
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};

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
    pub async fn rollback_session(
        &self,
        session_id: &str,
        target_step: u32,
        pause: bool,
        target_message_id: Option<&str>,
    ) -> anyhow::Result<()> {
        // If the session is currently Running, cancel it first so the ReAct loop
        // exits cleanly. Otherwise the loop's in-memory canonical/history would
        // diverge from the restored snapshot and overwrite it on the next save.
        // The loop observes the token at every wait point (step top, LLM call,
        // tool batch drain) and exits without touching status, so no Error
        // marking is needed — setting Error here would only emit a spurious
        // "session interrupted" error event and trigger terminal cleanup.
        let state = self.executor.get_session_state(session_id).await;
        if state == Some(SessionStatus::Running) {
            let cancel = self.executor.cancellation_token(session_id).await;
            cancel.cancel();
            // Wait until the loop handler releases the running slot.
            let mut waited = false;
            for _ in 0..50 {
                if !self
                    .executor
                    .running_actions_list()
                    .await
                    .contains(&session_id.to_string())
                {
                    break;
                }
                waited = true;
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            if waited
                && self
                    .executor
                    .running_actions_list()
                    .await
                    .contains(&session_id.to_string())
            {
                tracing::warn!(
                    "rollback_session {}: handler did not exit within 5s; proceeding with restore (late step writes are guarded by execute_step)",
                    session_id
                );
            }
        }

        // Background actions spawned before the rollback are stale relative to
        // the restored snapshot: kill them so their children cannot leak.
        self.executor.cancel_session_actions(session_id).await;

        let state_json = match self.db.get_react_state(session_id)? {
            Some(s) => s,
            None => {
                // No saved state at all — this happens when a session errored
                // before any snapshot was saved (e.g. first LLM call failed
                // in an older version without Fix 1). We can't restore
                // canonical, but we can still truncate session messages so
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
                } else if let Some(ts) = self.db.last_user_message_ts(session_id) {
                    let _ = self.db.delete_messages_after(session_id, &ts);
                }
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
        let mut snapshot: ReActSnapshot = serde_json::from_str(&state_json)?;

        // If no branch_point exists at the target step, the step likely
        // failed before save_branch_point was called (e.g. LLM error
        // mid-stream). In that case the snapshot's current canonical/history
        // IS the pre-step state — use it directly.
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
                canonical: snapshot.canonical.clone(),
                history: snapshot.history.clone(),
                step_number: target_step,
                last_msg_at: cutoff_ts,
            }
        };

        // Restore the canonical/history/step from the branch point.
        snapshot.canonical = bp.canonical;
        snapshot.history = bp.history;
        snapshot.step_number = bp.step_number;

        // If the branch point was saved right after an assistant tool_call
        // message but before the tool results were appended, the canonical
        // array ends with an assistant message carrying `tool_calls` but no
        // matching tool-result messages. Sending this to the LLM triggers a
        // 400 error (dangling tool_call). Trim such a trailing assistant
        // message so the loop re-requests the tool call cleanly.
        Self::trim_dangling_tool_call(&mut snapshot.canonical, &mut snapshot.history);

        // Newest branch-point cutoff (computed BEFORE pruning): used below to
        // detect a user message persisted after every branch point.
        let max_bp_ts = snapshot
            .branch_points
            .values()
            .filter_map(|b| b.last_msg_at.clone())
            .max();

        // Prune branch points that were created after the target step so the
        // session tree does not accumulate stale entries from the discarded
        // timeline.
        snapshot.branch_points.retain(|&k, _| k <= target_step);

        // Truncate session messages persisted after the branch point so the
        // conversation context matches the restored snapshot.
        //
        // A user message may be persisted AFTER the newest branch point:
        // an interjection sent while the session was erroring (its supplement
        // was dropped as the session was terminal) or before the app closed
        // mid-generation (the steering queue is in-memory only and is lost).
        // Such a message was never added to the ReAct canonical, so rolling
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
            } else {
                // Strict `>` for both: the branch-point cutoff is the last
                // message BEFORE the discarded step, so we keep the cutoff
                // itself intact (truncate_session_after is non-inclusive).
                self.db.truncate_session_after(session_id, ts, false)?;
            }
        }

        // Drop any checkpointed partial stream text: the restored timeline
        // must not inherit a stale partial from the discarded run. Discard
        // goes through the executor's PartialStore so an in-flight stream
        // checkpoint cannot re-create the row afterwards.
        self.executor.partials.discard(session_id).await;

        // For user-message rollback, also remove the user message from the
        // restored canonical so the LLM doesn't see it when the session resumes.
        // Skipped for orphan rollback: the orphaned message was never in the
        // canonical, so the last User entry there is a legitimately processed
        // message that must stay in the restored context.
        //
        // The target user message is NOT necessarily the last User entry:
        // steering/supplement inputs pushed after it also carry role User
        // (with their "Steering: — / "Additional context from user: —
        // prefixes). Trimming at the last User would leave the rolled-back
        // message in the canonical. Match the target by content instead
        // (canonical stores the prefixed form, the DB the raw text). If the
        // target cannot be located even with the known prefixes, that is a
        // genuine inconsistency — error instead of guessing the last User
        // entry (which could truncate a different message).
        if pause
            && !is_orphan_rollback
            && let Some(target) = target_msg
        {
            let prefixes = [
                "Additional context from user: ",
                "Answer to your previous question: ",
                "Steering: ",
            ];
            let matches_target = |t: &str| {
                t == target.content
                    || prefixes
                        .iter()
                        .any(|p| t.strip_prefix(p).is_some_and(|rest| rest == target.content))
            };
            let pos = snapshot
                .canonical
                .iter()
                .rposition(|m| {
                    m.role == CanonicalRole::User
                        && m.content
                            .iter()
                            .any(|p| matches!(p, ContentPart::Text(t) if matches_target(t)))
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "rollback_session {}: target user message not found in the restored canonical",
                        session_id
                    )
                })?;
            // Keep everything before the target user message. Drop the
            // message and any assistant messages that followed it.
            snapshot.canonical.truncate(pos);
        }

        let json = serde_json::to_string(&snapshot)?;
        self.db.save_react_state(session_id, &json)?;

        // Rebuild per-session tool registrations from the restored history so
        // that tools loaded after the rollback point are dropped, and tools
        // loaded before it remain available.
        self.restore_per_session_tools(session_id, &snapshot.history)
            .await;

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
        // Accept Error (directly after interruption), Paused (after
        // reopen_session during review) and PausedAwaitingAnswer (blocked on an
        // `ask` whose answer the user chose to skip). All indicate the session
        // can be retried from its saved snapshot.
        if !matches!(
            state,
            Some(SessionStatus::Error)
                | Some(SessionStatus::Paused)
                | Some(SessionStatus::PausedAwaitingAnswer)
        ) {
            return Err(anyhow::anyhow!(
                "session is not in a retryable state (current: {:?})",
                state
            ));
        }

        // Load the snapshot saved on error to find the branch point's
        // last_msg_at — the timestamp of the last message BEFORE the partial
        // output. We delete everything after it so the retry starts clean.
        if let Ok(Some(state_json)) = self.db.get_react_state(session_id)
            && let Ok(snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            // The snapshot's step_number is the step that failed. Try to
            // find a branch_point at that step; if none (the error
            // happened before save_branch_point was called), fall back to
            // the last user message's timestamp.
            let cutoff = snapshot
                .branch_points
                .get(&snapshot.step_number)
                .and_then(|bp| bp.last_msg_at.clone())
                .or_else(|| self.db.last_user_message_ts(session_id));
            if let Some(ts) = cutoff {
                // Retry OVERWRITES the previous attempt: drop both messages
                // and step rows (tool badges, thought entries) after the
                // branch point so the review history stays linear. Only
                // branching creates separate timelines.
                self.db.truncate_session_after(session_id, &ts, false)?;
            }
        }

        // Drop any checkpointed partial stream text: the retry re-streams
        // from scratch, so a crash during the retry must not promote the
        // pre-retry partial. Goes through the PartialStore so no in-flight
        // checkpoint can resurrect the row.
        self.executor.partials.discard(session_id).await;

        // Skipping an ask via continue must drop the C5 gate (status alone
        // flipping to Pending is not enough — the loop still reads the flag).
        if matches!(state, Some(SessionStatus::PausedAwaitingAnswer)) {
            self.executor
                .clear_awaiting_answer_persisted(session_id)
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

    /// If the canonical array ends with an assistant message carrying
    /// `tool_calls` but no matching tool-result messages, sending it to the
    /// LLM triggers a 400 error ("assistant message with tool calls must be
    /// followed by tool messages responding to each tool call"). This happens
    /// when a snapshot/branch point was saved right after the assistant
    /// message but before the tool results were appended (save_branch_point
    /// runs before tool execution; the app may die or be cancelled mid-batch).
    /// Trim such a trailing assistant message and the matching half-built
    /// history step so the loop re-requests the tool call cleanly.
    pub(crate) fn trim_dangling_tool_call(
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
    ) {
        sanitize_canonical(canonical);
        // A snapshot can also end with a half-built step: the assistant
        // message declared tool calls but the app died before the results
        // were appended (sanitize_canonical now repairs the dangling call
        // with an Interrupted result instead of popping it). Drop its
        // half-built history step (thought set, action=None) so the loop
        // re-requests the tool call cleanly on top of the repaired canonical.
        if history.last().is_some_and(|s| s.action.is_none()) {
            history.pop();
        }
    }
}
