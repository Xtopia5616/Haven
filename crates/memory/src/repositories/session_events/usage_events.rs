use super::*;

impl SessionStore {
    /// Append usage domain events and project them into the usage tables in
    /// the same transaction. This is the only live usage write boundary for
    /// Agent-owned, tool-owned and media-owned model calls. The synchronous
    /// form exists only for fixtures; production uses
    /// `append_usage_batch_cancellable`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_usage(
        &self,
        session_id: &str,
        input: &LlmUsageRecordInput,
    ) -> anyhow::Result<LlmUsageRecord> {
        let mut records = self.append_usage_batch(session_id, std::slice::from_ref(input))?;
        records
            .pop()
            .ok_or_else(|| anyhow::anyhow!("usage append returned no record"))
    }

    fn append_usage_batch(
        &self,
        session_id: &str,
        inputs: &[LlmUsageRecordInput],
    ) -> anyhow::Result<Vec<LlmUsageRecord>> {
        ensure_session_id(session_id)?;
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let mut records = inputs
            .iter()
            .map(|input| {
                LlmUsageRecord::from_input(
                    haven_common::types::new_id("usage"),
                    session_id,
                    input,
                    String::new(),
                )
            })
            .collect::<Vec<_>>();
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(Vec<LlmUsageRecord>, Vec<SessionEvent>)> {
            let mut previous_created_at: Option<String> = None;
            for record in &mut records {
                let created_at =
                    next_projection_created_at(&conn, session_id, previous_created_at.as_deref())?;
                record.created_at = created_at.clone();
                previous_created_at = Some(created_at);
            }
            let events = records
                .iter()
                .map(|record| {
                    Ok(SessionEventInput {
                        event_type: USAGE_RECORDED_EVENT_TYPE.into(),
                        payload: serde_json::to_string(record)?,
                        run_id: None,
                        step_number: record.step_number,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let stored_events = Self::append_batch_in_transaction(&conn, session_id, &events)?;
            for record in &records {
                Database::project_llm_call_usage_conn(&conn, record)?;
            }
            Ok((std::mem::take(&mut records), stored_events))
        })();
        match result {
            Ok((records, stored_events)) => {
                commit_sqlite_transaction(&conn, "append usage batch")?;
                for event in stored_events {
                    let _ = self.live_tx.send(event);
                }
                Ok(records)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append usage batch");
                Err(error)
            }
        }
    }

    /// Append usage records on the blocking pool while keeping cancellation
    /// and SQLite interrupt handling inside the memory boundary.
    pub async fn append_usage_batch_cancellable(
        &self,
        session_id: &str,
        inputs: Vec<LlmUsageRecordInput>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Vec<LlmUsageRecord>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let store = self.clone();
        let persist = move |_db: &Database| store.append_usage_batch(&session_id, &inputs);
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
    }

    /// Record a compensating domain event when an in-flight usage write loses
    /// a rollback epoch race. Deleting only the projection would leave the
    /// usage event active and allow a later event replay to resurrect it.
    fn discard_usage(&self, session_id: &str, usage_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "session_id must be canonical"
        );
        anyhow::ensure!(
            is_canonical_id(usage_id, "usage"),
            "usage_id must be canonical"
        );
        let payload = serde_json::json!({ "usage_id": usage_id }).to_string();
        let input = SessionEventInput::new(USAGE_DISCARDED_EVENT_TYPE, payload);
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<SessionEvent> {
            let mut events = Self::append_batch_in_transaction(&conn, session_id, &[input])?;
            let event = events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("usage discard append returned no event"))?;
            conn.execute(
                "DELETE FROM llm_usage WHERE session_id = ?1 AND id = ?2",
                rusqlite::params![session_id, usage_id],
            )?;
            Database::rebuild_session_usage_from_calls_conn(&conn, session_id)?;
            Ok(event)
        })();
        match result {
            Ok(event) => {
                commit_sqlite_transaction(&conn, "discard usage")?;
                let _ = self.live_tx.send(event.clone());
                Ok(())
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "discard usage");
                Err(error)
            }
        }
    }

    /// Append a compensating usage-discard event on the blocking pool after
    /// a rollback epoch invalidates an already-written usage record.
    pub async fn discard_usage_cancellable(
        &self,
        session_id: &str,
        usage_id: &str,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        anyhow::ensure!(
            is_canonical_id(usage_id, "usage"),
            "usage_id must be a canonical usage-* ID"
        );
        let session_id = session_id.to_owned();
        let usage_id = usage_id.to_owned();
        let store = self.clone();
        let discard = move |_db: &Database| store.discard_usage(&session_id, &usage_id);
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, discard).await,
            None => self.db.run_blocking(discard).await,
        }
    }
}
