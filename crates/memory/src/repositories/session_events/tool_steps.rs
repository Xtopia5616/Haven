use super::*;

impl SessionStore {
    /// Ensure a pending action-step row exists with the supplied durable
    /// invocation identity and confirmation decision.
    pub async fn ensure_tool_step(
        &self,
        write: ToolStepWrite,
        confirmed: Option<bool>,
    ) -> anyhow::Result<()> {
        ensure_session_id(&write.session_id)?;
        ensure_step_id(&write.step_id, "step_id")?;
        self.db
            .run_blocking(move |db| {
                db.ensure_tool_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.tool_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )
            })
            .await
    }

    /// Ensure an action-step row and mark it running as one blocking-pool
    /// operation. Keeping both Database calls in this closure preserves the
    /// existing ordering and avoids an interleaving window between them.
    pub async fn ensure_and_start_tool_step(
        &self,
        write: ToolStepWrite,
        confirmed: Option<bool>,
    ) -> anyhow::Result<bool> {
        ensure_session_id(&write.session_id)?;
        ensure_step_id(&write.step_id, "step_id")?;
        self.db
            .run_blocking(move |db| {
                db.ensure_tool_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.tool_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )?;
                db.start_tool_step(&write.step_id)
            })
            .await
    }

    /// Ensure an action-step row and record its final observation/outcome as
    /// one blocking-pool operation, retaining the existing Database order.
    pub async fn ensure_and_finish_tool_step(
        &self,
        write: ToolStepWrite,
        confirmed: Option<bool>,
        observation: String,
        outcome: ToolStepOutcome,
    ) -> anyhow::Result<bool> {
        ensure_session_id(&write.session_id)?;
        ensure_step_id(&write.step_id, "step_id")?;
        self.db
            .run_blocking(move |db| {
                db.ensure_tool_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.tool_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )?;
                db.finish_tool_step(&write.step_id, &observation, outcome)
            })
            .await
    }

    /// Create the materialized thought step for a committed transcript row.
    /// The message row remains the sole content authority; this method only
    /// writes the execution-state projection.
    pub async fn create_thought_step(
        &self,
        session_id: &str,
        step_number: u32,
        message_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        ensure_step_id(message_id, "thought message_id")?;
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.create_thought_step(&session_id, step_number, &message_id)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
    }

    /// Read every pending session for dispatcher recovery, newest first, on
    /// SQLite's blocking pool. Cancellation interrupts a query already
    /// waiting on a SQLite connection.
    ///
    /// This is the semantic equivalent of the unbounded pending-session query:
    /// no text/date filters, `limit = -1`, and `offset = 0`. The generic search
    /// surface remains an implementation detail of the Database repository.
    pub async fn pending_session_records_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<Vec<Session>> {
        anyhow::ensure!(
            !cancel.is_cancelled(),
            "pending session read cancelled before start"
        );
        self.db
            .run_blocking_cancellable(cancel, |db| {
                db.search_sessions_filtered(
                    None,
                    Some(SessionStatus::Pending.as_str()),
                    None,
                    None,
                    -1,
                    0,
                )
            })
            .await
    }

    /// Mark pending/running action steps for a failed session as `unknown`.
    ///
    /// The existing session-steps repository owns the update semantics; this
    /// port only moves its SQLite work onto the blocking pool.
    pub async fn fail_pending_tool_run_steps(
        &self,
        session_id: &str,
        observation: &str,
    ) -> anyhow::Result<usize> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let observation = observation.to_owned();
        self.db
            .run_blocking(move |db| db.fail_pending_tool_run_steps(&session_id, &observation))
            .await
    }
}
