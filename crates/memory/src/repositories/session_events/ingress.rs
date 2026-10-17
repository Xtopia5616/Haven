use super::*;

impl SessionStore {
    /// Persist a new session's crash-safe user seed before it is exposed to
    /// the dispatcher. The seed type is derived from the durable session
    /// origin; later input must use the pending-input and `UserInject` path.
    pub async fn persist_ingress_user_seed(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<Message> {
        ensure_session_id(session_id)?;
        let origin = self
            .load_session_record(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {session_id} not found"))?
            .origin;
        self.persist_message_row(
            session_id,
            haven_common::types::CanonicalRole::User,
            content,
            Some(origin.first_user_message_type()),
            attachments,
            voice,
            None,
            None,
            None,
        )
        .await
    }

    /// Persist a recovery-only error partial. It intentionally stays outside
    /// `session_events` and remains bounded by the existing `last_msg_at`
    /// rollback clock.
    pub async fn persist_recovery_partial(
        &self,
        session_id: &str,
        content: &str,
        kind: RecoveryPartialKind,
        message_id: &str,
    ) -> anyhow::Result<Message> {
        ensure_message_id(message_id, "recovery partial message_id")?;
        self.persist_message_row(
            session_id,
            haven_common::types::CanonicalRole::Assistant,
            content,
            Some(kind.as_message_type()),
            &[],
            false,
            Some(message_id),
            None,
            None,
        )
        .await
    }

    /// Persist a terminal ToolRun result when there is no live Agent loop to
    /// commit it. The stable message id keeps delivery retries idempotent.
    pub async fn persist_terminal_tool_run_result(
        &self,
        session_id: &str,
        content: &str,
        message_id: &str,
    ) -> anyhow::Result<Message> {
        ensure_message_id(message_id, "terminal ToolRun result message_id")?;
        self.persist_message_row(
            session_id,
            haven_common::types::CanonicalRole::User,
            content,
            Some(TranscriptMessageKind::Text),
            &[],
            false,
            Some(message_id),
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn persist_message_row(
        &self,
        session_id: &str,
        role: haven_common::types::CanonicalRole,
        content: &str,
        message_type: Option<TranscriptMessageKind>,
        attachments: &[MessageAttachment],
        voice: bool,
        message_id: Option<&str>,
        tool_call_id: Option<&str>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Message> {
        ensure_session_id(session_id)?;
        if let Some(message_id) = message_id {
            ensure_message_or_step_id(message_id, "message_id")?;
        }
        let session_id = session_id.to_owned();
        let content = content.to_owned();
        let attachments = attachments.to_vec();
        let message_id = message_id.map(str::to_owned);
        let tool_call_id = tool_call_id.map(str::to_owned);
        let persist = move |db: &Database| {
            if let Some(message_id) = message_id.as_deref()
                && let Some(existing) = db.get_message_by_id(&session_id, message_id)?
            {
                if existing.role != role
                    || existing.content != content
                    || existing.message_type != message_type
                    || existing.tool_call_id.as_deref() != tool_call_id.as_deref()
                {
                    anyhow::bail!(
                        "message idempotency conflict for session {} message {}",
                        session_id,
                        message_id
                    );
                }
                return Ok(existing);
            }
            db.add_message_full(
                &session_id,
                role,
                &content,
                message_type,
                tool_call_id.as_deref(),
                &attachments,
                voice,
                message_id.as_deref(),
            )
        };

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
    }
}
