use super::*;

impl SessionStore {
    /// Append a recovery protocol marker on SQLite's blocking pool. Synthetic
    /// Agent tests may use a session id without a durable session row; retain
    /// their historical successful no-op behavior inside this boundary.
    pub async fn append_recovery_persistence_if_session_exists(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let phase = phase.to_owned();
        self.db
            .run_blocking(move |db| {
                if db.get_session(&session_id)?.is_none() {
                    return Ok(());
                }
                store.append_recovery_persistence_sync(
                    &session_id,
                    run_id,
                    step_number,
                    &phase,
                    status,
                )?;
                Ok(())
            })
            .await
    }

    /// Append a branch point using the projection clocks observed under its
    /// existing transaction. The durable-session check and append stay within
    /// one blocking-pool operation, preserving the caller's not-found error.
    pub async fn append_branch_point_from_projection_for_existing_session(
        &self,
        session_id: &str,
        step_number: u32,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                if db.get_session(&session_id)?.is_none() {
                    anyhow::bail!(
                        "session '{}' disappeared before branch-point append",
                        session_id
                    );
                }
                let (_, cursor) =
                    store.append_branch_point_from_projection(&session_id, step_number, run_id)?;
                Ok(cursor)
            })
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    fn append_branch_point_sync(
        &self,
        session_id: &str,
        event_cursor: usize,
        step_number: u32,
        last_msg_at: Option<&str>,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionEvent> {
        let payload = serde_json::json!({
            "event_cursor": event_cursor,
            "step_number": step_number,
            "last_msg_at": last_msg_at,
        });
        self.append_sync(
            session_id,
            BRANCH_POINT_EVENT_TYPE,
            &payload.to_string(),
            run_id,
            Some(step_number),
        )
    }

    /// Synchronous branch-point append for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_branch_point(
        &self,
        session_id: &str,
        event_cursor: usize,
        step_number: u32,
        last_msg_at: Option<&str>,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionEvent> {
        self.append_branch_point_sync(session_id, event_cursor, step_number, last_msg_at, run_id)
    }

    /// Append a branch point using the projection clocks observed under the
    /// same SQLite write transaction. ReAct callers receive the clocks as
    /// data; they never read `messages` or derive an event cursor locally.
    pub(super) fn append_branch_point_from_projection(
        &self,
        session_id: &str,
        step_number: u32,
        run_id: Option<u64>,
    ) -> anyhow::Result<(SessionEvent, SessionCursor)> {
        ensure_session_id(session_id)?;
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(SessionEvent, SessionCursor)> {
            let cursor = Self::cursor_in_connection(&conn, session_id)?;
            let payload = serde_json::json!({
                "event_cursor": cursor.event_cursor,
                "step_number": step_number,
                "last_msg_at": cursor.last_msg_at,
            });
            let input = SessionEventInput {
                event_type: BRANCH_POINT_EVENT_TYPE.into(),
                payload: payload.to_string(),
                run_id,
                step_number: Some(step_number),
            };
            let mut events = Self::append_batch_in_transaction(&conn, session_id, &[input])?;
            let event = events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("branch point append returned no event"))?;
            Ok((event, cursor))
        })();
        match result {
            Ok(result) => {
                commit_sqlite_transaction(&conn, "append branch point")?;
                let _ = self.live_tx.send(result.0.clone());
                Ok(result)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append branch point");
                Err(error)
            }
        }
    }

    /// Record one phase of the recovery-only persistence protocol. The
    /// marker is not part of transcript replay; it only makes a partially
    /// completed repair durable and auditable.
    fn append_recovery_persistence_sync(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<SessionEvent> {
        ensure_session_id(session_id)?;
        let payload = serde_json::json!({
            "phase": phase,
            "branch_point": status.branch_point,
            "partial_messages": status.partial_messages,
            "projection": status.projection,
            "event_boundary": status.event_boundary,
        });
        self.append_sync(
            session_id,
            RECOVERY_PERSISTENCE_EVENT_TYPE,
            &payload.to_string(),
            Some(run_id),
            Some(step_number),
        )
    }

    /// Synchronous recovery-marker append for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_recovery_persistence(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<SessionEvent> {
        self.append_recovery_persistence_sync(session_id, run_id, step_number, phase, status)
    }

    /// Return the latest recovery protocol marker, including failed markers
    /// outside the active timeline. Resume diagnostics must not lose this
    /// state merely because a later rollback moved the active cursor.
    #[cfg(any(test, feature = "test-support"))]
    fn latest_recovery_persistence_sync(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        let conn = self.db.try_conn()?;
        Self::latest_recovery_persistence_in_connection(&conn, session_id)
    }

    /// Synchronous recovery-marker lookup for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn latest_recovery_persistence(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        self.latest_recovery_persistence_sync(session_id)
    }

    pub(super) fn latest_recovery_persistence_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        ensure_session_id(session_id)?;
        conn.query_row(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND event_type = ?2
             ORDER BY sequence DESC LIMIT 1",
            rusqlite::params![session_id, RECOVERY_PERSISTENCE_EVENT_TYPE],
            session_event_from_row,
        )
        .optional()
        .map_err(Into::into)
    }
}
