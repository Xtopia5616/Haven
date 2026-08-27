//! Follow-up / steering / action-completion queues and ask/confirm awaiting flags.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract; behavior unchanged).
//!
//! ## RAM vs durability (Phase 7 / D2)
//!
//! These queues are an **in-memory cache** for the current process only.
//! Durability for user injects is:
//! 1. the persisted `messages` row (written at submit time),
//! 2. snapshot `saved_at`, and
//! 3. the undelivered (anchor-less) scan in `run_session_resumed`.
//!
//! Resume re-queues recovered rows by `message_id`; enqueue helpers skip a
//! duplicate `message_id` already present so replay is idempotent.

use super::*;

impl SessionExecutor {
    /// Queue routing (Phase 4 / D1):
    /// ```text
    /// Running            → steering
    /// Paused             → follow_up
    /// PausedAwaitingAnswer → follow_up with is_answer (reply_to)
    /// action completion  → action_completions (system inject)
    /// ```
    pub async fn add_follow_up(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_follow_up_with_attachments(session_id, text, &[], None)
            .await
    }

    /// `message_id` is the id of the persisted user message row this
    /// follow-up's words were stored under (persisted at submit time by
    /// `process_input`). The ReAct loop's `push_user_context` creates the
    /// anchoring thought-step row under that same id, so resume/rollback
    /// resolve the step by id. `None` when no row was persisted.
    pub async fn add_follow_up_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.push_follow_up(session_id, text, attachments, false, message_id)
            .await
    }

    /// Alias for [`Self::add_follow_up`] (pre-Phase-4 name).
    pub async fn add_supplement(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_follow_up(session_id, text).await
    }

    /// Alias for [`Self::add_follow_up_with_attachments`] (pre-Phase-4 name).
    pub async fn add_supplement_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.add_follow_up_with_attachments(session_id, text, attachments, message_id)
            .await
    }

    /// Queue a follow-up that is the user's reply to a pending `ask`
    /// question (`reply_to`). Injected as a paired answer on resume so the
    /// model no longer sees the old question as open.
    pub async fn add_answer_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.push_follow_up(session_id, text, attachments, true, message_id)
            .await
    }

    /// Push onto the follow-up queue. When `message_id` is `Some`, skip if
    /// that id is already queued (Phase 7 / D2 resume idempotency).
    async fn push_follow_up(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        is_answer: bool,
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            anyhow::bail!("session '{}' not found", session_id)
        };
        let mut session = entry.lock().await;
        if let Some(ref mid) = message_id
            && session
                .follow_up_queue
                .iter()
                .any(|f| f.message_id.as_deref() == Some(mid.as_str()))
        {
            tracing::debug!(
                "session {} follow_up skipped (message_id {} already queued)",
                session_id,
                mid
            );
            return Ok(());
        }
        let follow_up = if is_answer {
            FollowUp::answer_with_message_id(text, attachments.to_vec(), message_id)
        } else {
            FollowUp::new_with_message_id(text, attachments.to_vec(), message_id)
        };
        session.follow_up_queue.push(follow_up);
        tracing::debug!(
            "session {} {} added ({} chars, {} attachments)",
            session_id,
            if is_answer { "answer" } else { "follow_up" },
            text.len(),
            attachments.len()
        );
        Ok(())
    }

    pub async fn get_follow_ups(&self, session_id: &str) -> Vec<FollowUp> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return Vec::new();
        };
        let mut session = entry.lock().await;
        std::mem::take(&mut session.follow_up_queue)
    }

    /// Alias for [`Self::get_follow_ups`] (pre-Phase-4 name).
    pub async fn get_supplements(&self, session_id: &str) -> Vec<FollowUp> {
        self.get_follow_ups(session_id).await
    }

    /// Add a steering item for the next step boundary (Phase 7 / D3).
    ///
    /// Steering does **not** interrupt in-flight tools (R1 product default):
    /// the current tool batch finishes (unless the session is cancelled /
    /// rolled back), then the drained text is injected before the next LLM
    /// call. `CancelToolsOnSteer` remains an optional product knob and is
    /// **not** implemented — document UI honestly: steer waits for the batch.
    pub async fn add_steering(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_steering_with_attachments(session_id, text, &[], None)
            .await
    }

    /// Queue steering. When `message_id` is `Some`, skip if that id is
    /// already in the steering queue (Phase 7 / D2 resume idempotency).
    pub async fn add_steering_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            anyhow::bail!("session '{}' not found", session_id)
        };
        let mut session = entry.lock().await;
        if let Some(ref mid) = message_id
            && session
                .steering_queue
                .iter()
                .any(|s| s.message_id.as_deref() == Some(mid.as_str()))
        {
            tracing::debug!(
                "session {} steering skipped (message_id {} already queued)",
                session_id,
                mid
            );
            return Ok(());
        }
        session.steering_queue.push(Supplement::with_message_id(
            text,
            attachments.to_vec(),
            message_id,
        ));
        tracing::debug!(
            "session {} steering added ({} chars, {} attachments)",
            session_id,
            text.len(),
            attachments.len()
        );
        Ok(())
    }

    /// Drain the steering queue for a session.
    pub async fn get_steering(&self, session_id: &str) -> Vec<Supplement> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return Vec::new();
        };
        let mut session = entry.lock().await;
        std::mem::take(&mut session.steering_queue)
    }

    /// Buffer a completed background-action result for a session. It is delivered
    /// to the ReAct loop as context at the next step start (drained by
    /// `drain_action_completions`), separate from the user-driven steering queue.
    pub async fn add_action_completion(&self, session_id: &str, text: &str) {
        let mut actions = self.action_completions.lock().await;
        actions
            .entry(session_id.to_string())
            .or_default()
            .push(text.to_string());
    }

    /// Drain buffered background-action completions for a session.
    pub async fn drain_action_completions(&self, session_id: &str) -> Vec<String> {
        self.action_completions
            .lock()
            .await
            .remove(session_id)
            .unwrap_or_default()
    }

    /// Drain all pending context for a session in one lock pass: follow-ups
    /// (paused-session replies / `ask` answers), steering (mid-run user
    /// interjections) and buffered background-action results (system inject).
    ///
    /// Phase 7 / D2: this only clears the **RAM cache**. Durability lives in
    /// DB messages + snapshot `saved_at` + undelivered scan; resume may
    /// re-queue the same `message_id` after a restart, and enqueue is
    /// idempotent so a duplicate id does not double-inject.
    pub async fn drain_pending_context(
        &self,
        session_id: &str,
    ) -> (Vec<FollowUp>, Vec<FollowUp>, Vec<String>) {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let (follow_ups, steering) = match entry {
            Some(entry) => {
                let mut session = entry.lock().await;
                (
                    std::mem::take(&mut session.follow_up_queue),
                    std::mem::take(&mut session.steering_queue),
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let action_results = self
            .action_completions
            .lock()
            .await
            .remove(session_id)
            .unwrap_or_default();
        (follow_ups, steering, action_results)
    }

    /// Non-draining check for pending user-facing context (follow-ups or
    /// steering). Resume uses it to decide whether post-snapshot inputs must
    /// be recovered from the DB: when the queues still hold the inputs (the
    /// pause → answer flow in the same process), the ReAct loop injects them
    /// and the DB copy must NOT be re-queued; after a restart the queues are
    /// empty and the DB is the only source.
    pub async fn has_pending_context(&self, session_id: &str) -> bool {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        match entry {
            Some(entry) => {
                let session = entry.lock().await;
                !session.follow_up_queue.is_empty() || !session.steering_queue.is_empty()
            }
            None => false,
        }
    }

    /// Mark every queued user inject as an ask answer in place (Phase 4 / C3).
    /// Mid-run steering that arrived while still Running is injected with the
    /// Answer prefix on the next run — without transferring between queues.
    pub async fn mark_user_queues_as_answer(&self, session_id: &str) {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return;
        };
        let mut session = entry.lock().await;
        for item in &mut session.follow_up_queue {
            item.is_answer = true;
        }
        for item in &mut session.steering_queue {
            item.is_answer = true;
        }
    }

    pub async fn set_awaiting_answer(
        &self,
        session_id: &str,
        pending: Option<crate::types::AskPending>,
    ) {
        let mut map = self.awaiting_answer.lock().await;
        match pending {
            Some(p) => {
                map.insert(session_id.to_string(), p);
            }
            None => {
                map.remove(session_id);
            }
        }
    }

    pub async fn get_awaiting_answer(&self, session_id: &str) -> Option<crate::types::AskPending> {
        self.awaiting_answer.lock().await.get(session_id).cloned()
    }

    /// Dual-track confirm gate: status flavor **or** in-memory/snapshot flag.
    /// Prefer this over duplicating the OR at every ingress/wake call site —
    /// status may already be `Pending` while the flag is still live (ask +
    /// pre-queued answer / confirm race).
    pub async fn is_confirm_gated_with(
        &self,
        session_id: &str,
        state: Option<&SessionStatus>,
    ) -> bool {
        matches!(state, Some(s) if s.is_awaiting_confirm())
            || self.get_awaiting_confirm(session_id).await.is_some()
    }

    /// Dual-track ask gate: status flavor **or** in-memory/snapshot flag.
    pub async fn is_ask_gated_with(&self, session_id: &str, state: Option<&SessionStatus>) -> bool {
        matches!(state, Some(s) if s.is_awaiting_answer())
            || self.get_awaiting_answer(session_id).await.is_some()
    }

    /// Background auto-wake must not interrupt ask/confirm pauses.
    pub async fn blocks_auto_wake_with(
        &self,
        session_id: &str,
        state: Option<&SessionStatus>,
    ) -> bool {
        matches!(state, Some(s) if s.blocks_auto_wake())
            || self.get_awaiting_answer(session_id).await.is_some()
            || self.get_awaiting_confirm(session_id).await.is_some()
    }

    pub async fn is_confirm_gated(&self, session_id: &str) -> bool {
        let state = self.get_session_state(session_id).await;
        self.is_confirm_gated_with(session_id, state.as_ref()).await
    }

    pub async fn is_ask_gated(&self, session_id: &str) -> bool {
        let state = self.get_session_state(session_id).await;
        self.is_ask_gated_with(session_id, state.as_ref()).await
    }

    pub async fn blocks_auto_wake(&self, session_id: &str) -> bool {
        let state = self.get_session_state(session_id).await;
        self.blocks_auto_wake_with(session_id, state.as_ref()).await
    }

    pub async fn clear_awaiting_answer(&self, session_id: &str) {
        self.awaiting_answer.lock().await.remove(session_id);
    }

    /// Clear the in-memory ask gate and rewrite `react_state` so a crash
    /// after inject cannot resurrect `awaiting_answer` from a stale snapshot.
    pub async fn clear_awaiting_answer_persisted(&self, session_id: &str) {
        self.clear_awaiting_answer(session_id).await;
        let sid = session_id.to_string();
        let _ = self
            .db
            .run_blocking(move |db| {
                let Some(json) = db.get_react_state(&sid)? else {
                    return Ok(());
                };
                let Ok(mut snapshot) = crate::types::ReActSnapshot::from_json(&json) else {
                    return Ok(());
                };
                if snapshot.awaiting_answer.take().is_none() {
                    return Ok(());
                }
                let rewritten = serde_json::to_string(&snapshot)?;
                db.save_react_state(&sid, &rewritten)?;
                Ok(())
            })
            .await;
    }

    pub async fn set_awaiting_confirm(
        &self,
        session_id: &str,
        pending: Option<crate::types::ConfirmPending>,
    ) {
        let mut map = self.awaiting_confirm.lock().await;
        match pending {
            Some(p) => {
                map.insert(session_id.to_string(), p);
            }
            None => {
                map.remove(session_id);
            }
        }
    }

    pub async fn get_awaiting_confirm(
        &self,
        session_id: &str,
    ) -> Option<crate::types::ConfirmPending> {
        self.awaiting_confirm.lock().await.get(session_id).cloned()
    }

    pub async fn clear_awaiting_confirm(&self, session_id: &str) {
        self.awaiting_confirm.lock().await.remove(session_id);
    }

    /// Clear the in-memory confirm gate and rewrite `react_state` so a crash
    /// after continuation cannot resurrect `awaiting_confirm` from a stale
    /// snapshot (Phase 5 / E3).
    pub async fn clear_awaiting_confirm_persisted(&self, session_id: &str) {
        self.clear_awaiting_confirm(session_id).await;
        let sid = session_id.to_string();
        let _ = self
            .db
            .run_blocking(move |db| {
                let Some(json) = db.get_react_state(&sid)? else {
                    return Ok(());
                };
                let Ok(mut snapshot) = crate::types::ReActSnapshot::from_json(&json) else {
                    return Ok(());
                };
                if snapshot.awaiting_confirm.take().is_none() {
                    return Ok(());
                }
                let rewritten = serde_json::to_string(&snapshot)?;
                db.save_react_state(&sid, &rewritten)?;
                Ok(())
            })
            .await;
    }

    /// Record a pause-based confirm decision. When every tool in the batch
    /// has a decision, transition the session to Pending so the dispatcher
    /// re-enters the loop and re-executes approved tools (Phase 5 / E3).
    pub(super) async fn resolve_confirm_pause(
        &self,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> Option<crate::session::ConfirmResolution> {
        let confirm_key = step_id.to_string();
        let mut map = self.awaiting_confirm.lock().await;
        let mut found: Option<crate::session::ConfirmResolution> = None;
        let mut persist: Option<(String, crate::types::ConfirmPending)> = None;
        let mut wake_sid: Option<String> = None;
        for (session_id, pending) in map.iter_mut() {
            if let Some(tool) = pending
                .tools
                .iter_mut()
                .find(|t| t.confirm_id == confirm_key)
            {
                // One-shot: ignore late/duplicate resolves so reject→approve
                // cannot flip a denied gated tool back to runnable.
                if tool.decision.is_some() {
                    return None;
                }
                tool.decision = Some(confirmed);
                found = Some(crate::session::ConfirmResolution {
                    session_id: Some(session_id.clone()),
                    tool_name: tool.tool_name.clone(),
                    tool_input: tool.tool_input.clone(),
                });
                persist = Some((session_id.clone(), pending.clone()));
                if pending.all_decided() {
                    wake_sid = Some(session_id.clone());
                }
                break;
            }
        }
        drop(map);
        if let Some((sid, pending)) = persist {
            self.persist_awaiting_confirm(&sid, &pending).await;
        }
        if let Some(sid) = wake_sid {
            // Wake the dispatcher: continuation runs approved tools.
            if let Err(e) = self
                .update_session_status(&sid, SessionStatus::Pending)
                .await
            {
                tracing::warn!(
                    "resolve_confirm_pause: failed to wake session {}: {}",
                    sid,
                    e
                );
            }
        }
        found
    }

    /// Rewrite `react_state.awaiting_confirm` so confirm decisions survive
    /// restart (Phase 5 / E3).
    async fn persist_awaiting_confirm(
        &self,
        session_id: &str,
        pending: &crate::types::ConfirmPending,
    ) {
        let sid = session_id.to_string();
        let pending = pending.clone();
        let _ = self
            .db
            .run_blocking(move |db| {
                let Some(json) = db.get_react_state(&sid)? else {
                    return Ok(());
                };
                let Ok(mut snapshot) = crate::types::ReActSnapshot::from_json(&json) else {
                    return Ok(());
                };
                snapshot.awaiting_confirm = Some(pending);
                let rewritten = serde_json::to_string(&snapshot)?;
                db.save_react_state(&sid, &rewritten)?;
                Ok(())
            })
            .await;
    }

    /// Request confirmations for a gated batch without blocking (Phase 5 / E3).
    /// Emits `confirm:requested` for each tool; the loop pauses and exits.
    pub async fn request_confirm_batch(
        &self,
        session_id: &str,
        pending: crate::types::ConfirmPending,
    ) {
        for tool in &pending.tools {
            let confirm_id: haven_common::types::ConfirmId = tool.confirm_id.clone().into();
            if let Some(cb) = self.on_confirm_request.snap() {
                cb(
                    confirm_id,
                    session_id.to_string(),
                    tool.tool_name.clone(),
                    tool.risk_level,
                    tool.tool_input.clone(),
                );
            }
        }
        self.set_awaiting_confirm(session_id, Some(pending)).await;
    }
}
