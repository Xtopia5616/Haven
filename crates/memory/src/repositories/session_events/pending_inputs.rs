use super::*;

impl SessionStore {
    /// Read every persisted input that has not yet been acknowledged by a
    /// committed `UserInject` event. The durable marker, not message age or
    /// step-row shape, determines whether it is recoverable.
    pub async fn pending_session_inputs(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<PendingSessionInput>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.list_pending_session_inputs(&session_id))
            .await
    }

    /// Drop recovery state for an input explicitly rejected by ingress, such
    /// as a message routed to a terminal session whose history row cannot be
    /// deleted. Normal delivery uses the atomic transcript acknowledgement.
    pub async fn discard_pending_user_input(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        ensure_message_id(message_id, "pending message_id")?;
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let conn = db.try_conn()?;
                Database::acknowledge_pending_session_input(&conn, &session_id, &message_id)
            })
            .await
    }

    /// Persist a newly accepted input with its recovery disposition and marker.
    /// Repeating a supplied message id is idempotent and does not recreate a
    /// marker already acknowledged by a transcript event.
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_pending_user_input(
        &self,
        session_id: &str,
        content: &str,
        message_type: Option<TranscriptMessageKind>,
        attachments: &[MessageAttachment],
        voice: bool,
        message_id: Option<&str>,
        disposition: PendingInputDisposition,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<PendingSessionInput> {
        ensure_session_id(session_id)?;
        if let Some(message_id) = message_id {
            ensure_message_id(message_id, "pending message_id")?;
        }
        let session_id = session_id.to_owned();
        let content = content.to_owned();
        let attachments = attachments.to_vec();
        let message_id = message_id.map(str::to_owned);
        let persist = move |db: &Database| {
            if let Some(message_id) = message_id.as_deref()
                && let Some(existing) = db.get_message_by_id(&session_id, message_id)?
            {
                if existing.role != haven_common::types::CanonicalRole::User
                    || existing.content != content
                    || existing.message_type != message_type
                {
                    anyhow::bail!(
                        "message idempotency conflict for session {} message {}",
                        session_id,
                        message_id
                    );
                }
                return Ok(db
                    .get_pending_session_input(&session_id, message_id)?
                    .unwrap_or(PendingSessionInput {
                        message: existing,
                        disposition: PendingInputDisposition::FollowUp,
                    }));
            }
            db.add_pending_user_input(
                &session_id,
                &content,
                message_type,
                &attachments,
                voice,
                message_id.as_deref(),
                disposition,
            )
        };

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
    }
}
