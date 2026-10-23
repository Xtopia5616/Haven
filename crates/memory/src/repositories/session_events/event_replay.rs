use super::*;

impl SessionStore {
    /// Load the existing read models used by App session resume on SQLite's
    /// blocking pool. Queries run in the established order and retain their
    /// independent read semantics; this port does not promise one snapshot.
    /// Dropping this future cannot interrupt a query already running on
    /// Tokio's blocking pool.
    pub async fn session_resume_projection(
        &self,
        session_id: &str,
    ) -> anyhow::Result<SessionResumeProjection> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let store = self.clone();
        self.db
            .run_blocking(move |db| {
                let messages = db.list_session_messages(&session_id)?;
                let steps = db.list_session_steps(&session_id)?;
                let usage = db.get_session_usage(&session_id)?;
                let llm_usage = db.list_session_llm_usage(&session_id)?;
                let (active_events, event_sequence_watermark) =
                    store.read_active_with_watermark(&session_id)?;
                let active_domain_events = active_events
                    .iter()
                    .filter(|event| event.event_type != TRANSCRIPT_EVENT_TYPE)
                    .cloned()
                    .collect();
                Ok(SessionResumeProjection {
                    messages,
                    steps,
                    usage,
                    llm_usage,
                    active_events,
                    active_domain_events,
                    event_sequence_watermark,
                })
            })
            .await
    }

    /// Read the active timeline and append-only high-water mark from one
    /// SQLite snapshot. The high-water mark lets clients reject a committed
    /// event delivered late after a rollback has already rewound the active
    /// projection.
    fn read_active_with_watermark(
        &self,
        session_id: &str,
    ) -> anyhow::Result<(Vec<SessionEvent>, i64)> {
        ensure_session_id(session_id)?;
        let connection = self.db.try_conn()?;
        connection.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            let active_events = Self::read_active_in_connection(&connection, session_id)?;
            let watermark = connection.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get::<_, i64>(0),
            )?;
            Ok((active_events, watermark))
        })();
        match result {
            Ok(snapshot) => {
                connection.execute_batch("COMMIT")?;
                Ok(snapshot)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub(super) fn load_replay_state(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionReplayState>> {
        ensure_session_id(session_id)?;
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN")?;
        let result = (|| -> anyhow::Result<Option<SessionReplayState>> {
            let has_timeline_events: bool = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM session_events
                    WHERE session_id = ?1 AND event_type NOT IN (?2, ?3)
                )",
                rusqlite::params![
                    session_id,
                    super::SESSION_EXECUTION_WORKSPACE_EVENT_TYPE,
                    super::SESSION_EXECUTION_MODE_EVENT_TYPE
                ],
                |row| row.get(0),
            )?;
            if !has_timeline_events {
                return Ok(None);
            }
            let active_events = Self::read_active_in_connection(&conn, session_id)?;
            let transcript = active_events
                .iter()
                .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
                .cloned()
                .collect::<Vec<_>>();
            let branch_points = Self::active_branch_points_from_events(&active_events)?;
            Ok(Some(SessionReplayState {
                cursor: Self::cursor_in_connection_with_event_cursor(
                    &conn,
                    session_id,
                    transcript.len(),
                )?,
                transcript,
                branch_points,
            }))
        })();
        match result {
            Ok(state) => {
                conn.execute_batch("COMMIT")?;
                Ok(state)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Load the durable replay state on SQLite's blocking pool. Agent keeps
    /// transcript decoding and ReAct projection policy outside Memory.
    pub async fn load_replay_state_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionReplayState>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.load_replay_state(&session_id))
            .await
    }

    /// Read the durable replay cursor used to verify an Agent event boundary.
    /// A session without an event log keeps the same default cursor as an
    /// absent replay state. This reads cursor fields and lightweight timeline
    /// metadata without hydrating transcript events.
    pub async fn event_boundary_cursor(
        &self,
        session_id: &str,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let read = move |_db: &Database| store.event_boundary_cursor_sync(&session_id);

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, read).await,
            None => self.db.run_blocking(read).await,
        }
    }

    pub(super) fn cursor_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let event_cursor = Self::active_transcript_cursor_in_connection(conn, session_id)?;
        Self::cursor_in_connection_with_event_cursor(conn, session_id, event_cursor)
    }

    fn cursor_in_connection_with_event_cursor(
        conn: &rusqlite::Connection,
        session_id: &str,
        event_cursor: usize,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let event_sequence = conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )?;
        let message_ingress_seq = conn
            .query_row(
                "SELECT COALESCE(last_ingress_seq, 0)
                 FROM message_ingress_cursors WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let step_seq = conn
            .query_row(
                "SELECT COALESCE(last_step_seq, 0)
                 FROM session_step_cursors WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let last_msg_at = last_projection_created_at(conn, session_id)?;
        Ok(SessionCursor {
            event_sequence,
            event_cursor,
            message_ingress_seq,
            step_seq,
            last_msg_at,
        })
    }

    fn event_boundary_cursor_sync(&self, session_id: &str) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN DEFERRED")?;
        let result = Self::cursor_in_connection(&conn, session_id);
        match result {
            Ok(cursor) => {
                conn.execute_batch("COMMIT")?;
                Ok(cursor)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Count active transcript entries without constructing SessionEvent
    /// values. The common no-rollback path is a SQL COUNT; if rollbacks exist,
    /// replay only sequence/type metadata and rollback payloads.
    fn active_transcript_cursor_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<usize> {
        ensure_session_id(session_id)?;
        let (after_sequence, scan_compaction_markers) =
            Self::active_replay_boundary_details_in_connection(conn, session_id)?;

        if !scan_compaction_markers {
            let has_rollback: bool = conn.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM session_events
                    WHERE session_id = ?1 AND sequence > ?2 AND event_type = ?3
                )",
                rusqlite::params![session_id, after_sequence, TIMELINE_ROLLBACK_EVENT_TYPE],
                |row| row.get(0),
            )?;
            if !has_rollback {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM session_events
                     WHERE session_id = ?1 AND sequence > ?2 AND event_type = ?3",
                    rusqlite::params![session_id, after_sequence, TRANSCRIPT_EVENT_TYPE],
                    |row| row.get(0),
                )?;
                return usize::try_from(count)
                    .map_err(|_| anyhow::anyhow!("active transcript cursor is out of range"));
            }
        }

        let mut statement = conn.prepare(
            "SELECT sequence, event_type,
                    CASE WHEN event_type = ?3 THEN payload END,
                    CASE WHEN ?4 AND event_type = ?5
                         THEN CAST(json_extract(payload, '$.type') AS TEXT) END
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
               AND event_type IN (?3, ?5)
             ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                session_id,
                after_sequence,
                TIMELINE_ROLLBACK_EVENT_TYPE,
                scan_compaction_markers,
                TRANSCRIPT_EVENT_TYPE,
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )?;

        let mut active_transcripts = Vec::<i64>::new();
        for row in rows {
            let (sequence, event_type, rollback_payload, transcript_type) = row?;
            if event_type == TIMELINE_ROLLBACK_EVENT_TYPE {
                let payload = rollback_payload
                    .ok_or_else(|| anyhow::anyhow!("rollback event payload is missing"))?;
                let target = serde_json::from_str::<serde_json::Value>(&payload)?
                    .get("to_sequence")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| anyhow::anyhow!("rollback event has no to_sequence"))?;
                let retained = active_transcripts.partition_point(|active| *active <= target);
                active_transcripts.truncate(retained);
            } else {
                if transcript_type.as_deref() == Some("compact_summary") {
                    active_transcripts.clear();
                }
                active_transcripts.push(sequence);
            }
        }
        Ok(active_transcripts.len())
    }

    fn latest_sequence_sync(&self, session_id: &str) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        Ok(self.db.try_conn()?.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )?)
    }

    /// Synchronous sequence inspection for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn latest_sequence(&self, session_id: &str) -> anyhow::Result<i64> {
        self.latest_sequence_sync(session_id)
    }

    /// Read the latest durable event sequence on SQLite's blocking pool.
    pub async fn latest_sequence_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| store.latest_sequence_sync(&session_id))
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    fn read_all_sync(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_from_sync(session_id, 0)
    }

    /// Read the complete append-only stream synchronously in fixtures/tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read_all(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_all_sync(session_id)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn read_from_sync(
        &self,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let conn = self.db.try_conn()?;
        Self::read_from_in_connection(&conn, session_id, after_sequence)
    }

    /// Read one bounded page of durable events strictly after
    /// `after_sequence`. Rows are ordered by ascending sequence. The store
    /// owns the fixed page capacity.
    pub(super) fn replay_page_sync(
        &self,
        session_id: &str,
        after_sequence: i64,
        limit: usize,
    ) -> anyhow::Result<SessionEventPage> {
        ensure_session_id(session_id)?;
        anyhow::ensure!(
            after_sequence >= 0,
            "event replay cursor cannot be negative"
        );
        anyhow::ensure!(
            (1..=MAX_SESSION_EVENT_REPLAY_PAGE_SIZE).contains(&limit),
            "event replay page size must be between 1 and {MAX_SESSION_EVENT_REPLAY_PAGE_SIZE}"
        );

        let conn = self.db.try_conn()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, after_sequence, (limit + 1) as i64],
            session_event_from_row,
        )?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = events.len() > limit;
        if has_more {
            events.pop();
        }
        let next_cursor = events
            .last()
            .map(|event| event.sequence)
            .unwrap_or(after_sequence);
        Ok(SessionEventPage {
            events,
            next_cursor,
            has_more,
        })
    }

    /// Read one bounded event page on SQLite's blocking pool.
    pub async fn replay_page_cancellable(
        &self,
        session_id: &str,
        after_sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<SessionEventPage> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| {
                store.replay_page_sync(
                    &session_id,
                    after_sequence,
                    MAX_SESSION_EVENT_REPLAY_PAGE_SIZE,
                )
            })
            .await
    }

    fn read_from_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let mut stmt = conn.prepare(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, after_sequence],
            session_event_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Replay the current timeline. Rollback markers only move the active
    /// cursor; the underlying append-only rows remain available for audit and
    /// future branch tooling.
    pub(super) fn read_active(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        let conn = self.db.try_conn()?;
        Self::read_active_in_connection(&conn, session_id)
    }

    /// Read the complete active event stream on SQLite's blocking pool. Agent
    /// recovery reducers use this when a control projection must be reconciled
    /// with canonical transcript events after an interrupted commit sequence.
    pub async fn read_active_events_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.read_active(&session_id))
            .await
    }

    pub(super) fn read_active_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let started = Instant::now();
        // A compact_summary is a durable active-root marker.  The audit log
        // before it remains readable through read_all, but normal recovery
        // only needs the root and its suffix.  Fall back to a full replay if
        // an unusual late rollback targets before that root; correctness wins
        // over the optimization for that branch.
        let after_sequence = Self::active_replay_boundary_in_connection(conn, session_id)?;
        let mut active = Vec::new();
        for event in Self::read_from_in_connection(conn, session_id, after_sequence)? {
            if event.event_type == TIMELINE_ROLLBACK_EVENT_TYPE {
                let target = serde_json::from_str::<serde_json::Value>(&event.payload)?
                    .get("to_sequence")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| anyhow::anyhow!("rollback event has no to_sequence"))?;
                active.retain(|candidate: &SessionEvent| candidate.sequence <= target);
            } else if event.event_type == TRANSCRIPT_EVENT_TYPE
                && serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|payload| payload.get("type").cloned())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref()
                    == Some("compact_summary")
            {
                // Compaction intentionally replaces the active transcript
                // root. Older rows remain append-only history, but they no
                // longer belong to the replayable timeline.
                active.clear();
                active.push(event);
            } else {
                active.push(event);
            }
        }
        // A session's bound execution workspace is session configuration, not
        // branch history. Preserve its latest value across rollback and
        // compaction even when the original event falls before the active root.
        let retained_context =
            Self::read_latest_session_execution_context_in_connection(conn, session_id)?;
        for event in retained_context {
            if !active
                .iter()
                .any(|active_event| active_event.sequence == event.sequence)
            {
                active.push(event);
            }
        }
        active.sort_by_key(|event| event.sequence);
        tracing::debug!(
            session_id,
            after_sequence,
            active_events = active.len(),
            scan_ms = started.elapsed().as_millis() as u64,
            "active session event replay scanned durable suffix"
        );
        Ok(active)
    }

    fn read_latest_session_execution_context_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let mut statement = conn.prepare(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1
               AND event_type IN (?2, ?3)
               AND sequence IN (
                    SELECT MAX(sequence) FROM session_events
                    WHERE session_id = ?1 AND event_type IN (?2, ?3)
                    GROUP BY event_type
               )
             ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map(
            rusqlite::params![
                session_id,
                super::SESSION_EXECUTION_WORKSPACE_EVENT_TYPE,
                super::SESSION_EXECUTION_MODE_EVENT_TYPE
            ],
            session_event_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn active_replay_boundary_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<i64> {
        Self::active_replay_boundary_details_in_connection(conn, session_id)
            .map(|(boundary, _)| boundary)
    }

    fn active_replay_boundary_details_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<(i64, bool)> {
        let root = conn.query_row(
            "SELECT MAX(sequence) FROM session_events
                 WHERE session_id = ?1 AND event_type = ?2
                   AND json_extract(payload, '$.type') = 'compact_summary'",
            rusqlite::params![session_id, TRANSCRIPT_EVENT_TYPE],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let Some(root) = root else {
            return Ok((0, false));
        };

        let mut statement = conn.prepare(
            "SELECT payload FROM session_events
             WHERE session_id = ?1 AND sequence > ?2 AND event_type = ?3
             ORDER BY sequence ASC",
        )?;
        let rollback_payloads = statement.query_map(
            rusqlite::params![session_id, root, TIMELINE_ROLLBACK_EVENT_TYPE],
            |row| row.get::<_, String>(0),
        )?;
        for payload in rollback_payloads {
            let payload = payload?;
            let target = serde_json::from_str::<serde_json::Value>(&payload)?
                .get("to_sequence")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("rollback event has no to_sequence"))?;
            if target < root {
                return Ok((0, true));
            }
        }
        Ok((root.saturating_sub(1), false))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn read_active_transcript(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .read_active(session_id)?
            .into_iter()
            .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
            .collect())
    }

    /// Return active non-transcript domain events for the session.  ReAct
    /// recovery uses this for interaction state; messages and steps are never
    /// consulted to reconstruct the actor.
    fn read_active_domain_events_inner(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .read_active(session_id)?
            .into_iter()
            .filter(|event| event.event_type != TRANSCRIPT_EVENT_TYPE)
            .collect())
    }

    /// Synchronous active-event projection for fixture and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read_active_domain_events(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_active_domain_events_inner(session_id)
    }

    /// Read active non-transcript domain events through the session
    /// persistence boundary on SQLite's blocking pool.
    pub async fn read_active_domain_events_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.read_active_domain_events_inner(&session_id))
            .await
    }

    /// Return the latest active branch point for each step. Branch points are
    /// control events and are intentionally kept separate from transcript
    /// replay, but they share the same rollback cursor and audit log.
    #[cfg(test)]
    pub(super) fn read_active_branch_points(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<ActiveBranchPoint>> {
        let conn = self.db.try_conn()?;
        Self::read_active_branch_points_in_connection(&conn, session_id)
    }

    pub(super) fn read_active_branch_points_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<ActiveBranchPoint>> {
        Self::active_branch_points_from_events(&Self::read_active_in_connection(conn, session_id)?)
    }

    fn active_branch_points_from_events(
        events: &[SessionEvent],
    ) -> anyhow::Result<Vec<ActiveBranchPoint>> {
        let mut points = Vec::new();
        for event in events {
            if event.event_type != BRANCH_POINT_EVENT_TYPE {
                continue;
            }
            let payload: serde_json::Value = serde_json::from_str(&event.payload)?;
            let event_cursor = payload
                .get("event_cursor")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| anyhow::anyhow!("branch point event has no event_cursor"))?;
            let step_number = payload
                .get("step_number")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| anyhow::anyhow!("branch point event has no step_number"))?;
            let last_msg_at = payload
                .get("last_msg_at")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            let point = ActiveBranchPoint {
                event_sequence: event.sequence,
                event_cursor,
                step_number,
                last_msg_at,
            };
            if let Some(index) = points
                .iter()
                .position(|point: &ActiveBranchPoint| point.step_number == step_number)
            {
                points[index] = point;
            } else {
                points.push(point);
            }
        }
        Ok(points)
    }

    pub(super) fn sequence_for_transcript_cursor_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
        transcript_cursor: usize,
    ) -> anyhow::Result<i64> {
        let events = Self::read_active_in_connection(conn, session_id)?;
        if transcript_cursor == 0 {
            return Ok(0);
        }
        events
            .iter()
            .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
            .nth(transcript_cursor - 1)
            .map(|event| event.sequence)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "transcript cursor {} is outside the active session timeline",
                    transcript_cursor
                )
            })
    }
}
