//! User-turn ingress for [`AgentLayer`]: `process_input` routing (D1),
//! attachment persistence, and media-gateway enrich helpers.
//!
//! Split out of `layer.rs` so the facade stays focused on wiring; these
//! methods operate on the same fields via `impl AgentLayer` blocks.

use crate::AgentLayer;
use crate::session::SessionStatus;
use crate::types::ProcessResult;
use base64::Engine;
use haven_common::types::MessageAttachment;
use haven_llm::media::{AttachmentOutcome, GenerateOutcome, MediaDecision};

/// Human-readable label for a gateway extraction decision, shown in the
/// message content so the user (and the model) see where the text came from.
fn extraction_label(decision: &MediaDecision) -> &'static str {
    match decision.routed_to.as_str() {
        "ocr" => "已通过 OCR 识别文字",
        "stt" => "已通过语音识别转写",
        "llm:image" => "已通过主模型提取图片文字",
        "llm:audio" => "已通过主模型转写音频",
        _ => "已自动提取内容",
    }
}

/// Build a message attachment from a gateway-generated media file so the
/// generated images show up in the chat like a user attachment.
fn attachment_from_generated_file(path: &std::path::Path) -> anyhow::Result<MessageAttachment> {
    let bytes = std::fs::read(path)?;
    let media_type = haven_llm::media::detect_media_type(&bytes).to_string();
    Ok(MessageAttachment {
        media_type,
        data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        filename: path.file_name().map(|n| n.to_string_lossy().into_owned()),
        path: Some(path.to_string_lossy().into_owned()),
    })
}

impl AgentLayer {
    pub async fn process_input(
        &self,
        transcript: &str,
        active_session_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        self.process_input_with_attachments(transcript, active_session_id, &[], false)
            .await
    }

    /// Run the media gateway over an incoming user message: extract
    /// attachments through dedicated providers (OCR / ASR, with main-model
    /// confidence/error fallback), and handle pure-text generation requests
    /// (text-to-image). TTS is a model-facing `audio.speak` tool action and is
    /// intentionally not performed during ingress. Returns the enriched
    /// message content and any generated-media attachments.
    /// Fail-open: a gateway error leaves the message untouched.
    async fn enrich_with_gateway(
        &self,
        transcript: &str,
        attachments: &[MessageAttachment],
    ) -> (String, Vec<MessageAttachment>) {
        let Some(gateway) = self.gateway.read().await.clone() else {
            return (transcript.to_string(), attachments.to_vec());
        };
        let mut notes: Vec<String> = Vec::new();
        let mut out_attachments = attachments.to_vec();

        if !attachments.is_empty() {
            for att in attachments {
                let bytes = match base64::engine::general_purpose::STANDARD.decode(&att.data) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!("gateway: attachment base64 decode failed: {e}");
                        continue;
                    }
                };
                let filename = att.filename.clone().unwrap_or_default();
                match gateway
                    .process_attachment(&bytes, &filename, transcript, None)
                    .await
                {
                    Ok(AttachmentOutcome::Extracted { text, decision }) => {
                        notes.push(format!("【{}】\n{}", extraction_label(&decision), text));
                    }
                    Ok(AttachmentOutcome::PassThrough { .. }) => {}
                    Err(e) => tracing::warn!("gateway: attachment processing failed: {e}"),
                }
            }
        } else if !transcript.trim().is_empty() {
            match gateway.process_generate(transcript, None).await {
                Ok(GenerateOutcome::Generated { file_path, .. }) => {
                    match attachment_from_generated_file(&file_path) {
                        Ok(att) => {
                            let name = file_path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "media".into());
                            out_attachments.push(att);
                            notes.push(format!("（已生成图片：{name}）"));
                        }
                        Err(e) => tracing::warn!("gateway: attaching generated file failed: {e}"),
                    }
                }
                Ok(GenerateOutcome::NotGenerate) | Ok(GenerateOutcome::Unsupported { .. }) => {}
                Err(e) => tracing::warn!("gateway: generate request failed: {e}"),
            }
        }

        if notes.is_empty() {
            (transcript.to_string(), out_attachments)
        } else {
            (
                format!("{}\n\n{}", transcript, notes.join("\n\n")),
                out_attachments,
            )
        }
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
        // Media gateway pre-processing: extraction actions (OCR / ASR) and
        // Image-generation requests are handled here, before persistence, so
        // every downstream path (steering, supplements, new sessions) sees
        // the enriched message. TTS remains an explicit tool side effect.
        let (enriched, enriched_attachments) =
            self.enrich_with_gateway(transcript, attachments).await;
        let transcript: &str = &enriched;
        let attachments = enriched_attachments.as_slice();
        tracing::debug!(
            "process_input: text={:?} active_session_id={:?} attachments={} voice={}",
            transcript,
            active_session_id,
            attachments.len(),
            voice
        );

        // The message is persisted BEFORE the state check on purpose: the
        // steering/supplement fallback paths below rely on it being on disk.
        // If the session turns out to be terminal, the persisted row is removed
        // again below so history never shows a ghost user message.
        let mut persisted_msg = if let Some(session_id) = active_session_id.as_ref() {
            let msg = match self
                .persist_message_parts(
                    session_id,
                    "user",
                    transcript,
                    Some("text"),
                    attachments,
                    voice,
                )
                .await
            {
                Ok(msg) => msg,
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "failed to persist user message for session {session_id}: {e}"
                    ));
                }
            };
            Some(msg)
        } else {
            None
        };
        if let Some(session_id) = active_session_id.as_ref() {
            let state = self.executor.get_session_state(session_id).await;

            // Phase 4 / D1 routing (+ Phase 5 / E3 confirm gate):
            //   Running                 → steering
            //   PausedAwaitingAnswer    → follow_up (is_answer / reply_to)
            //   PausedAwaitingConfirm   → follow_up, but do NOT wake (confirm
            //                             dialog is the only resume path)
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
                // A Paused session that is awaiting an `ask` answer: this
                // message IS the reply to the pending question. Queue it as
                // an answer so the loop injects a paired "Answer to your
                // previous question" instead of generic context —otherwise
                // the model sees the old question as still open and answers
                // questions from long ago. The awaiting-answer flavor is
                // carried by the status itself (`PausedAwaitingAnswer`), so
                // it is read BEFORE set_session_status(Pending) below clears it.
                // Prefer status, but also honor the C5 flag while status has
                // already flipped to Pending (ask + pre-queued answer).
                // While a confirm gate is active (including confirm+ask
                // batches that stash awaiting_answer early), do NOT treat
                // free-text as the ask reply — confirm must finish first.
                let confirm_pending = self
                    .executor
                    .is_confirm_gated_with(session_id, state.as_ref())
                    .await;
                let is_answer = !confirm_pending
                    && self
                        .executor
                        .is_ask_gated_with(session_id, state.as_ref())
                        .await;
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
                        .ensure_session_loaded(session_id)
                        .await
                        .is_err()
                    {
                        let (session, first_msg_id) = self
                            .create_session_with_first_message(transcript, attachments, voice)
                            .await?;
                        self.events.emit_session_created(&session).await;
                        return Ok(ProcessResult::session_created(
                            session.id,
                            Some(first_msg_id),
                        ));
                    }
                    // Re-read state after ensure_session_loaded may have reloaded
                    // the session from DB (M3/H10 TOCTOU: end_session may have ended
                    // it between the get_session_state read above and the failed
                    // add_follow_up). Only non-terminal sessions may be
                    // reactivated by a follow-up message; Completed/Error sessions
                    // were ended on purpose and must be reopened explicitly via
                    // the resume flow — auto-converting them would resurrect a
                    // ghost session.
                    let fresh_state = self.executor.get_session_state(session_id).await;
                    if fresh_state == Some(SessionStatus::Completed)
                        || fresh_state == Some(SessionStatus::Error)
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
                            let db = self.db.clone();
                            let tid = session_id.clone();
                            let msg_id = msg.id.clone();
                            let tid_c = tid.clone();
                            let msg_id_c = msg_id.clone();
                            if let Err(e) = db
                                .run_blocking(move |db| db.delete_message_by_id(&tid_c, &msg_id_c))
                                .await
                            {
                                tracing::warn!(
                                    "process_input: failed to remove ghost user message {} for session {}: {}",
                                    msg_id,
                                    tid,
                                    e
                                );
                            }
                        }
                        // Notify the frontend so it can drop the stale
                        // activeActionId and reset the model indicator instead of
                        // showing an orphaned bubble with no response.
                        let fresh_status =
                            fresh_state.as_ref().map(|s| s.as_str()).unwrap_or("error");
                        self.events
                            .emit_session_updated(session_id, fresh_status)
                            .await;
                        // Do not keep the reloaded terminal session in the working
                        // set — it was ended and should not be dispatchable.
                        self.executor.remove_session(session_id).await;
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
                            self.set_session_status(session_id, SessionStatus::Pending)
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
                    self.set_session_status(session_id, SessionStatus::Pending)
                        .await?;
                }
            }
            Ok(ProcessResult::supplemented(
                persisted_msg.as_ref().map(|m| m.id.clone()),
            ))
        } else {
            let (session, first_msg_id) = self
                .create_session_with_first_message(transcript, attachments, voice)
                .await?;
            tracing::info!("process_input created session: id={:?}", session.id);
            self.events.emit_session_created(&session).await;
            Ok(ProcessResult::session_created(
                session.id,
                Some(first_msg_id),
            ))
        }
    }
}
