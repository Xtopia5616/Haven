//! Session context queues and the single interaction registry.

use super::*;

/// Context selected for the next model request.
#[derive(Debug, Default)]
pub(crate) struct ReactContextBatch {
    pub(crate) steering: Vec<FollowUp>,
    pub(crate) follow_ups: Vec<FollowUp>,
    pub(crate) action_results: Vec<String>,
}

impl SessionSupervisor {
    pub async fn add_follow_up(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_follow_up_with_attachments(session_id, text, &[], None)
            .await
    }

    pub async fn add_follow_up_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let actor = self
            .actor_for(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found", session_id))?;
        actor
            .queue_follow_up(text, attachments, false, message_id)
            .await
    }

    pub async fn add_answer_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let actor = self
            .actor_for(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found", session_id))?;
        actor
            .clear_interactions(Some(crate::interaction::InteractionKind::Ask))
            .await;
        actor
            .queue_follow_up(text, attachments, true, message_id)
            .await
    }

    pub async fn get_follow_ups(&self, session_id: &str) -> Vec<FollowUp> {
        match self.actor_for(session_id).await {
            Some(actor) => actor.drain_follow_ups().await,
            None => Vec::new(),
        }
    }

    pub async fn add_steering(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_steering_with_attachments(session_id, text, &[], None)
            .await
    }

    pub async fn add_steering_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let actor = self
            .actor_for(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found", session_id))?;
        actor.queue_steering(text, attachments, message_id).await
    }

    pub async fn get_steering(&self, session_id: &str) -> Vec<FollowUp> {
        match self.actor_for(session_id).await {
            Some(actor) => actor.drain_steering().await,
            None => Vec::new(),
        }
    }

    pub async fn add_action_completion(&self, session_id: &str, text: &str) {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.add_action_completion(text.to_string()).await;
        }
    }

    pub async fn drain_action_completions(&self, session_id: &str) -> Vec<String> {
        match self.actor_for(session_id).await {
            Some(actor) => actor.drain_action_completions().await,
            None => Vec::new(),
        }
    }

    pub(crate) async fn drain_react_context(&self, session_id: &str) -> ReactContextBatch {
        match self.actor_for(session_id).await {
            Some(actor) => {
                let (steering, follow_ups, action_results) = actor.drain_context().await;
                ReactContextBatch {
                    steering,
                    follow_ups,
                    action_results,
                }
            }
            None => ReactContextBatch::default(),
        }
    }

    pub async fn has_pending_context(&self, session_id: &str) -> bool {
        match self.actor_for(session_id).await {
            Some(actor) => actor.has_pending_context().await,
            None => false,
        }
    }

    pub async fn mark_user_queues_as_answer(&self, session_id: &str) {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.mark_queues_as_answer().await;
        }
    }

    pub async fn interaction_requests(
        &self,
        session_id: &str,
    ) -> Vec<crate::interaction::InteractionRequest> {
        match self.actor_for(session_id).await {
            Some(actor) => actor.interactions(None, false).await,
            None => Vec::new(),
        }
    }

    pub async fn pending_interactions(
        &self,
        session_id: &str,
        kind: crate::interaction::InteractionKind,
    ) -> Vec<crate::interaction::InteractionRequest> {
        match self.actor_for(session_id).await {
            Some(actor) => actor.interactions(Some(kind), true).await,
            None => Vec::new(),
        }
    }

    pub async fn has_pending_interaction(
        &self,
        session_id: &str,
        kind: crate::interaction::InteractionKind,
    ) -> bool {
        !self.pending_interactions(session_id, kind).await.is_empty()
    }

    pub async fn request_interaction(
        &self,
        request: crate::interaction::InteractionRequest,
    ) -> anyhow::Result<()> {
        let actor = self
            .actor_for(&request.session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found", request.session_id))?;
        actor.request_interaction(request.clone()).await?;
        if request.status == crate::interaction::InteractionStatus::Pending {
            self.emit_event(SessionEvent::InteractionRequested {
                request: Box::new(request),
            });
        }
        Ok(())
    }

    pub async fn clear_interactions(
        &self,
        session_id: &str,
        kind: Option<crate::interaction::InteractionKind>,
    ) {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.clear_interactions(kind).await;
        }
    }

    pub async fn clear_interactions_persisted(
        &self,
        session_id: &str,
        kind: Option<crate::interaction::InteractionKind>,
    ) -> anyhow::Result<()> {
        self.clear_interactions(session_id, kind).await;
        self.persist_interactions(session_id).await
    }

    pub(crate) async fn persist_interactions(&self, session_id: &str) -> anyhow::Result<()> {
        let interactions = self.interaction_requests(session_id).await;
        let sid = session_id.to_string();
        self.db
            .run_blocking(move |db| {
                let Some(json) = db.get_react_state(&sid)? else {
                    return Ok(());
                };
                let mut snapshot = crate::types::ReActSnapshot::from_json(&json)?;
                snapshot.interactions = interactions;
                db.save_react_state(&sid, &serde_json::to_string(&snapshot)?)?;
                Ok(())
            })
            .await
    }

    pub async fn resolve_interaction(
        &self,
        request_id: &str,
        response: serde_json::Value,
    ) -> anyhow::Result<Option<crate::interaction::InteractionRequest>> {
        let actors = self
            .actors
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for actor in actors {
            if let Some(decision) = actor
                .resolve_interaction(request_id.to_string(), response.clone())
                .await
            {
                let request = decision.request.clone();
                self.persist_interactions(&request.session_id).await?;
                if decision.wake_session {
                    self.update_session_status(&request.session_id, SessionStatus::Pending)
                        .await?;
                }
                return Ok(Some(request));
            }
        }
        Ok(None)
    }

    pub async fn is_confirm_gated_with(
        &self,
        session_id: &str,
        _state: Option<&SessionStatus>,
    ) -> bool {
        self.has_pending_interaction(session_id, crate::interaction::InteractionKind::Confirm)
            .await
    }

    pub async fn is_ask_gated_with(
        &self,
        session_id: &str,
        _state: Option<&SessionStatus>,
    ) -> bool {
        self.has_pending_interaction(session_id, crate::interaction::InteractionKind::Ask)
            .await
    }

    pub async fn blocks_auto_wake_with(
        &self,
        session_id: &str,
        _state: Option<&SessionStatus>,
    ) -> bool {
        self.has_pending_interaction(session_id, crate::interaction::InteractionKind::Ask)
            .await
            || self
                .has_pending_interaction(session_id, crate::interaction::InteractionKind::Confirm)
                .await
    }

    pub async fn is_confirm_gated(&self, session_id: &str) -> bool {
        self.has_pending_interaction(session_id, crate::interaction::InteractionKind::Confirm)
            .await
    }

    pub async fn is_ask_gated(&self, session_id: &str) -> bool {
        self.has_pending_interaction(session_id, crate::interaction::InteractionKind::Ask)
            .await
    }

    pub async fn blocks_auto_wake(&self, session_id: &str) -> bool {
        self.blocks_auto_wake_with(session_id, None).await
    }

    pub async fn request_confirm_batch(
        &self,
        session_id: &str,
        requests: Vec<crate::interaction::InteractionRequest>,
    ) -> anyhow::Result<()> {
        self.update_session_status(session_id, SessionStatus::Paused)
            .await?;
        for request in requests {
            self.request_interaction(request).await?;
        }
        Ok(())
    }
}
