use super::*;

impl SessionStore {
    pub(super) fn append_sync(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
        run_id: Option<u64>,
        step_number: Option<u32>,
    ) -> anyhow::Result<SessionEvent> {
        ensure_session_id(session_id)?;
        Self::validate_single_event(event_type, payload, run_id, step_number)?;
        let input = SessionEventInput {
            event_type: event_type.into(),
            payload: payload.into(),
            run_id,
            step_number,
        };
        self.append_batch(session_id, std::slice::from_ref(&input))
            .map(|mut events| events.remove(0))
    }

    /// Synchronous raw event append for fixtures and unit tests. Production
    /// callers use the typed async event APIs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
        run_id: Option<u64>,
        step_number: Option<u32>,
    ) -> anyhow::Result<SessionEvent> {
        self.append_sync(session_id, event_type, payload, run_id, step_number)
    }

    /// Append a non-transcript domain event through the session persistence
    /// boundary. The caller owns the domain payload and policy; this port only
    /// schedules the existing append operation on SQLite's blocking pool.
    /// Domain events have no run or step association. The existing append
    /// implementation continues to broadcast the committed event to live
    /// subscribers.
    pub async fn append_domain_event(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
    ) -> anyhow::Result<SessionEvent> {
        ensure_session_id(session_id)?;
        ensure_domain_event_type(event_type)?;
        Self::validate_single_event(event_type, payload, None, None)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let event_type = event_type.to_owned();
        let payload = payload.to_owned();
        self.db
            .run_blocking(move |_| {
                store.append_sync(&session_id, &event_type, &payload, None, None)
            })
            .await
    }

    /// Append non-transcript domain events and move the owning session to a new
    /// status in one SQLite transaction. The status compare-and-set and every
    /// event commit together, and live subscribers only see committed events.
    pub async fn append_domain_event_batch_with_session_status(
        &self,
        session_id: &str,
        expected_status: SessionStatus,
        next_status: SessionStatus,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        anyhow::ensure!(!events.is_empty(), "domain event batch cannot be empty");
        Self::validate_inputs(events)?;
        ensure_domain_event_types(events)?;
        anyhow::ensure!(
            events
                .iter()
                .all(|event| event.run_id.is_none() && event.step_number.is_none()),
            "domain event batches cannot include transcript coordinates"
        );
        let store = self.clone();
        let session_id = session_id.to_owned();
        let events = events.to_vec();
        self.db
            .run_blocking(move |_| {
                store.append_domain_event_batch_with_session_status_sync(
                    &session_id,
                    expected_status,
                    next_status,
                    &events,
                )
            })
            .await
    }

    fn append_domain_event_batch_with_session_status_sync(
        &self,
        session_id: &str,
        expected_status: SessionStatus,
        next_status: SessionStatus,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        ensure_domain_event_types(events)?;
        Self::validate_inputs(events)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let changed = conn.execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2,
                    run_end_reason = CASE WHEN ?1 IN ('pending', 'running') THEN NULL ELSE run_end_reason END,
                    run_end_kind = NULL
                 WHERE id = ?3 AND status = ?4",
                rusqlite::params![
                    next_status.as_str(),
                    now,
                    session_id,
                    expected_status.as_str()
                ],
            )?;
            anyhow::ensure!(
                changed == 1,
                "session '{}' status changed before confirmation batch commit",
                session_id
            );
            let stored = Self::append_batch_in_transaction(&conn, session_id, events)?;
            commit_sqlite_transaction(&conn, "append confirmation batch and pause session")?;
            Ok(stored)
        })();

        match result {
            Ok(stored) => {
                self.db.cache_invalidate_session_history_page();
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append confirmation batch and pause session");
                Err(error)
            }
        }
    }

    /// Append a batch in one SQLite transaction.  Sequence allocation happens
    /// under `BEGIN IMMEDIATE`, so concurrent sessions and concurrent writers
    /// cannot produce duplicate per-session cursors.
    pub(in crate::repositories) fn append_batch(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        Self::validate_inputs(events)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = Self::append_batch_in_transaction(&conn, session_id, events);
        match result {
            Ok(stored) => {
                commit_sqlite_transaction(&conn, "append batch")?;
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append batch");
                Err(error)
            }
        }
    }

    fn append_transcript_sync(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<SessionEvent> {
        self.append_sync(
            session_id,
            TRANSCRIPT_EVENT_TYPE,
            payload,
            Some(run_id),
            Some(step_number),
        )
    }

    /// Synchronous append access for fixture and unit tests. Production
    /// callers use `append_transcript_async`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_transcript(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<SessionEvent> {
        self.append_transcript_sync(session_id, payload, run_id, step_number)
    }

    /// Append one transcript event on SQLite's blocking pool and return its
    /// durable sequence. The session foreign key rejects writes for a missing
    /// session instead of silently returning sequence zero.
    pub async fn append_transcript_async(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let payload = payload.to_owned();
        self.db
            .run_blocking(move |_| {
                Ok(store
                    .append_transcript_sync(&session_id, &payload, run_id, step_number)?
                    .sequence)
            })
            .await
    }

    /// Seed an event stream exactly once from the initial transcript. Stable
    /// session configuration events do not make the transcript non-empty; any
    /// transcript or control event still prevents a stale seed from rewriting
    /// an established timeline.
    pub(super) fn seed_if_empty(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        Self::validate_inputs(events)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Option<Vec<SessionEvent>>> {
            let has_timeline_events: bool = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM session_events
                    WHERE session_id = ?1 AND event_type != ?2
                )",
                rusqlite::params![session_id, super::SESSION_EXECUTION_WORKSPACE_EVENT_TYPE],
                |row| row.get(0),
            )?;
            if has_timeline_events {
                return Ok(None);
            }
            Ok(Some(Self::append_batch_in_transaction(
                &conn, session_id, events,
            )?))
        })();
        match result {
            Ok(Some(stored)) => {
                commit_sqlite_transaction(&conn, "seed session events")?;
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Ok(None) => {
                commit_sqlite_transaction(&conn, "seed session events")?;
                drop(conn);
                self.read_active(session_id)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "seed session events");
                Err(error)
            }
        }
    }

    /// Seed an event stream once on SQLite's blocking pool, reusing the
    /// synchronous transaction, validation, and post-commit broadcast path.
    pub async fn seed_if_empty_async(
        &self,
        session_id: &str,
        events: Vec<SessionEventInput>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.seed_if_empty(&session_id, &events))
            .await
    }
}
