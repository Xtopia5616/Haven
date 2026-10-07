//! User-turn ingress for [`AgentLayer`]: `process_input` routing (D1),
//! attachment persistence and user-turn routing.
//!
//! Split out of `layer.rs` so the facade stays focused on wiring; these
//! methods operate on the same fields via `impl AgentLayer` blocks.

use crate::AgentLayer;
use crate::session::SessionStatus;
use crate::types::ProcessResult;
use haven_common::types::MessageAttachment;

impl AgentLayer {
    pub async fn process_input(
        &self,
        transcript: &str,
        active_session_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        self.process_input_with_attachments(transcript, active_session_id, &[], false)
            .await
    }

    /// Like `process_input`, but attaches binary payloads (images and
    /// user-uploaded files) to the user message. Attachments are persisted
    /// with the message; images are injected into the ReAct context as image
    /// parts, while file attachments (which carry a `path`) surface as a text
    /// reference the agent resolves with the file tool. `voice` marks messages
    /// transcribed from audio so the UI can keep the mic style across reloads.
    pub async fn process_input_with_attachments(
        &self,
        transcript: &str,
        active_session_id: Option<String>,
        attachments: &[MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<ProcessResult> {
        tracing::debug!(
            "process_input: text={:?} active_session_id={:?} attachments={} voice={}",
            transcript,
            active_session_id,
            attachments.len(),
            voice
        );

        // For an existing session, keep the lifecycle gate for the whole
        // ingress decision. Delete/clear may quiesce an actor concurrently,
        // but must not remove its durable row between reading the lifecycle /
        // interaction gates, persisting the input and route, and mailbox
        // enqueue; otherwise a successful UI send can become a ghost message
        // or be routed to an orphan actor.
        let lifecycle = if let Some(session_id) = active_session_id.as_deref() {
            let lifecycle = self.executor.lifecycle_guard().await;
            self.executor.ensure_lifecycle_open()?;
            if self.executor.is_session_closing(session_id) {
                anyhow::bail!("session is closing; retry after deletion");
            }
            Some(lifecycle)
        } else {
            None
        };

        // Freeze the answer/follow-up decision before persisting the message.
        // Read the two gates through one actor mailbox operation so Confirm
        // has consistent precedence over a stashed Ask.
        let state = if let Some(session_id) = active_session_id.as_ref() {
            self.executor.get_active_session_status(session_id).await
        } else {
            None
        };
        let interaction_gates = if let Some(session_id) = active_session_id.as_ref() {
            self.executor.pending_interaction_gates(session_id).await
        } else {
            crate::session::PendingInteractionGates::default()
        };
        let requested_disposition = if state != Some(SessionStatus::Running)
            && interaction_gates.ask_pending
            && !interaction_gates.confirmation_pending
        {
            haven_memory::PendingInputDisposition::Answer
        } else {
            haven_memory::PendingInputDisposition::FollowUp
        };

        let mut persisted_input = if let Some(session_id) = active_session_id.as_ref() {
            let input = match crate::persist_pending_user_input(
                &self.executor,
                session_id,
                transcript,
                Some(haven_common::types::TranscriptMessageKind::Text),
                attachments,
                voice,
                requested_disposition,
            )
            .await
            {
                Ok(input) => input,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "failed to persist user message for session {session_id}: {e}"
                    ));
                }
            };
            Some(input)
        } else {
            None
        };
        let is_answer = persisted_input.as_ref().is_some_and(|input| {
            input.disposition == haven_memory::PendingInputDisposition::Answer
        });
        let mut persisted_msg = persisted_input.take().map(|input| input.message);
        if let Some(session_id) = active_session_id.as_ref() {
            // Phase 4 / D1 routing (+ Phase 5 / E3 confirm gate):
            //   Running                 → steering
            //   Paused + available Ask  → reserved answer follow-up
            //   Pending Answer / Confirm → ordinary follow-up
            //   Paused / other          → follow_up
            // If the steering queue is unavailable (session vanished from
            // memory between the state read and the enqueue), fall through
            // to the follow-up path instead of failing: the user message is
            // already persisted, and that path reloads / wakes the session.
            let steering_delivered = state == Some(SessionStatus::Running)
                && match self
                    .executor
                    .add_steering_with_attachments(
                        session_id,
                        transcript,
                        attachments,
                        persisted_msg.as_ref().map(|m| m.id.clone()),
                    )
                    .await
                {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!(
                            "process_input: steering failed for session {} ({}); falling back to supplement path",
                            session_id,
                            e
                        );
                        false
                    }
                };

            if !steering_delivered {
                // When the persisted disposition reserves an `ask` answer,
                // queue it as an answer so the loop injects a paired "Answer
                // to your previous question" instead of generic context. The
                // disposition is frozen in the durable pending marker before
                // this queue operation, so recovery preserves the route if
                // gates change before UserInject commits.
                let message_id = persisted_msg.as_ref().map(|m| m.id.clone());
                let was_in_memory = if is_answer {
                    self.executor
                        .add_answer_with_attachments(
                            session_id,
                            transcript,
                            attachments,
                            message_id.clone(),
                        )
                        .await
                        .is_ok()
                } else {
                    self.executor
                        .add_follow_up_with_attachments(
                            session_id,
                            transcript,
                            attachments,
                            message_id.clone(),
                        )
                        .await
                        .is_ok()
                };
                if !was_in_memory {
                    // Session may be stale/deleted — fall back to creating a new session
                    if self
                        .executor
                        .ensure_session_loaded_locked(session_id)
                        .await
                        .is_err()
                    {
                        drop(lifecycle);
                        let created_session = self
                            .create_session_with_first_message(transcript, attachments, voice)
                            .await?;
                        self.events
                            .emit_session_created(&created_session.session)
                            .await;
                        return Ok(ProcessResult::session_created(
                            created_session.session.id,
                            Some(created_session.first_user_message_id),
                        ));
                    }
                    // Re-read state after ensure_session_loaded may have reloaded
                    // the session from DB (M3/H10 TOCTOU: end_session may have ended
                    // it between the get_active_session_status read above and the failed
                    // add_follow_up). Only non-terminal sessions may be
                    // reactivated by a follow-up message; Completed/Error sessions
                    // were ended on purpose and must be reopened explicitly via
                    // the resume flow — auto-converting them would resurrect a
                    // ghost session.
                    let fresh_state = self.executor.get_session_status(session_id).await;
                    if fresh_state
                        .as_ref()
                        .is_some_and(|status| status.is_terminal())
                    {
                        tracing::warn!(
                            "process_input: session {} is terminal ({:?}) despite active_session_id; dropping supplement to avoid resurrection",
                            session_id,
                            fresh_state
                        );
                        // Remove the just-persisted user message so history
                        // does not show an unanswered ghost bubble (the
                        // frontend is told to drop its copy below).
                        if let Some(msg) = persisted_msg.take() {
                            let tid = session_id.clone();
                            let msg_id = msg.id.clone();
                            if let Err(e) = self
                                .executor
                                .session_store()
                                .delete_message_by_id(&tid, &msg_id)
                                .await
                            {
                                tracing::warn!(
                                    "process_input: failed to remove ghost user message {} for session {}: {}",
                                    msg_id,
                                    tid,
                                    e
                                );
                                if let Err(marker_error) = self
                                    .executor
                                    .session_store()
                                    .discard_pending_user_input(&tid, &msg_id)
                                    .await
                                {
                                    tracing::warn!(
                                        "process_input: failed to clear recovery marker for rejected user message {} in session {}: {}",
                                        msg_id,
                                        tid,
                                        marker_error
                                    );
                                }
                            }
                        }
                        // Notify the frontend so it can drop the stale
                        // activeToolCallId and reset the model indicator instead of
                        // showing an orphaned bubble with no response.
                        let fresh_status = fresh_state.unwrap_or(SessionStatus::Error);
                        self.events
                            .emit_session_updated(session_id, fresh_status)
                            .await;
                        // Do not keep the reloaded terminal session in the working
                        // set — it was ended and should not be dispatchable.
                        drop(lifecycle);
                        self.executor.remove_session(session_id).await?;
                        return Ok(ProcessResult::supplemented(None));
                    } else {
                        if is_answer {
                            self.executor
                                .add_answer_with_attachments(
                                    session_id,
                                    transcript,
                                    attachments,
                                    message_id.clone(),
                                )
                                .await?;
                        } else {
                            self.executor
                                .add_follow_up_with_attachments(
                                    session_id,
                                    transcript,
                                    attachments,
                                    message_id.clone(),
                                )
                                .await?;
                        }
                        // Confirm-awaiting: free-text must not wake — only
                        // resolve_confirmation finishes the gated batch.
                        let confirm_blocked = self
                            .executor
                            .is_confirm_gated_with(session_id, fresh_state.as_ref())
                            .await;
                        if matches!(fresh_state, Some(s) if s.is_paused()) && !confirm_blocked {
                            self.set_session_status_if(
                                session_id,
                                SessionStatus::Paused,
                                SessionStatus::Pending,
                            )
                            .await?;
                        }
                    }
                    return Ok(ProcessResult::supplemented(message_id));
                }
                let confirm_blocked = self
                    .executor
                    .is_confirm_gated_with(session_id, state.as_ref())
                    .await;
                if matches!(state.as_ref(), Some(s) if s.is_paused()) && !confirm_blocked {
                    self.set_session_status_if(
                        session_id,
                        SessionStatus::Paused,
                        SessionStatus::Pending,
                    )
                    .await?;
                }
            }
            Ok(ProcessResult::supplemented(
                persisted_msg.as_ref().map(|m| m.id.clone()),
            ))
        } else {
            let created_session = self
                .create_session_with_first_message(transcript, attachments, voice)
                .await?;
            tracing::info!(
                "process_input created session: id={:?}",
                created_session.session.id
            );
            self.events
                .emit_session_created(&created_session.session)
                .await;
            Ok(ProcessResult::session_created(
                created_session.session.id,
                Some(created_session.first_user_message_id),
            ))
        }
    }
}
