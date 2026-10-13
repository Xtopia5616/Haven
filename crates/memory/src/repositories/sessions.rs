use crate::db::Database;
use crate::repositories::kv_store::MAX_MEMORY_SESSION_ID_PAGE_SIZE;
use chrono::{Local, NaiveDate, TimeZone, Utc};
use haven_common::lifecycle::SessionStatus;
use haven_common::types::is_canonical_id;

/// WHERE clause shared by every session search query (list, count, paginated).
/// Kept as one constant so search semantics cannot drift between queries.
const SEARCH_WHERE: &str = "WHERE input_text LIKE ?1 OR title LIKE ?1
    OR EXISTS (SELECT 1 FROM messages
               WHERE messages.session_id = sessions.id AND messages.content LIKE ?1)";

fn validate_session_id(id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(id, "ses"),
        "invalid session_id: expected canonical ses- ID"
    );
    Ok(())
}

fn session_id_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<String> {
    let id: String = row.get(0)?;
    if is_canonical_id(&id, "ses") {
        Ok(id)
    } else {
        Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            "invalid persisted session_id".into(),
        ))
    }
}

/// Map a row produced by a session-record query, including provenance and run-end reason.
fn map_session_list_row(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let id = session_id_from_row(row)?;
    let status = row.get::<_, String>(3)?;
    let origin =
        SessionOrigin::from_storage(row.get::<_, String>(6)?, row.get::<_, Option<String>>(7)?)?;
    Ok(Session {
        session_id: id,
        input_text: row.get(1)?,
        title: row.get(2)?,
        status: SessionStatus::from_status_str(&status),
        run_end_reason: row.get(8)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        origin,
    })
}

/// Convert a local `YYYY-MM-DD` date into the UTC RFC3339 instant of local
/// midnight. Used to translate the UI's local-date filters into UTC bounds so
/// filtering by "today" matches what the user sees (the stored created_at is
/// UTC). Returns None when the date is malformed.
fn local_date_to_utc(date: &str) -> Option<String> {
    local_date_to_utc_bound(date, false)
}

/// Convert a local `YYYY-MM-DD` date into the UTC instant of the *next* local
/// midnight. Used as an exclusive upper bound so filtering by "through date X"
/// includes the whole of day X (the plain `YYYY-MM-DD <= created_at` string
/// comparison would exclude every session created during the end day itself).
fn local_date_to_utc_exclusive_end(date: &str) -> Option<String> {
    local_date_to_utc_bound(date, true)
}

fn local_date_to_utc_bound(date: &str, exclusive_end: bool) -> Option<String> {
    let day = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let day = if exclusive_end { day.succ_opt()? } else { day };
    let local_midnight = Local
        .from_local_datetime(&day.and_hms_opt(0, 0, 0)?)
        .earliest()?;
    Some(local_midnight.with_timezone(&Utc).to_rfc3339())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub session_id: String,
    pub input_text: String,
    pub title: Option<String>,
    pub status: SessionStatus,
    pub run_end_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub origin: SessionOrigin,
}

/// Durable provenance for a session. Peer sessions remain ordinary sessions;
/// this only records which creation path admitted the row and its parent id.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionOrigin {
    #[default]
    User,
    AgentSpawn {
        parent_session_id: String,
    },
}

impl SessionOrigin {
    pub(crate) const fn first_user_message_type(
        &self,
    ) -> haven_common::types::TranscriptMessageKind {
        match self {
            Self::User => haven_common::types::TranscriptMessageKind::Text,
            Self::AgentSpawn { .. } => haven_common::types::TranscriptMessageKind::PeerKickoff,
        }
    }

    fn storage_parts(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::User => ("user", None),
            Self::AgentSpawn { parent_session_id } => ("agent_spawn", Some(parent_session_id)),
        }
    }

    fn from_storage(origin: String, parent_session_id: Option<String>) -> rusqlite::Result<Self> {
        match (origin.as_str(), parent_session_id) {
            ("user", None) => Ok(Self::User),
            ("agent_spawn", Some(parent_session_id))
                if is_canonical_id(&parent_session_id, "ses") =>
            {
                Ok(Self::AgentSpawn { parent_session_id })
            }
            _ => {
                let invalid_column = if origin == "agent_spawn" { 7 } else { 6 };
                Err(rusqlite::Error::FromSqlConversionFailure(
                    invalid_column,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "invalid persisted session origin metadata",
                    )),
                ))
            }
        }
    }
}

impl Database {
    #[cfg(any(test, feature = "test-support"))]
    pub fn create_session(&self, input_text: &str) -> anyhow::Result<Session> {
        self.create_session_with_origin(input_text, SessionOrigin::User)
    }

    /// Create a durable peer session and validate that its parent identity is
    /// canonical and present. The parent id is provenance, not a foreign key:
    /// deleting a parent must not rewrite a surviving child's origin.
    pub fn create_session_with_origin(
        &self,
        input_text: &str,
        origin: SessionOrigin,
    ) -> anyhow::Result<Session> {
        let (origin_kind, parent_session_id) = origin.storage_parts();
        if let Some(parent_session_id) = parent_session_id {
            anyhow::ensure!(
                is_canonical_id(parent_session_id, "ses"),
                "invalid parent session id"
            );
        }
        let session_id = haven_common::types::new_id("ses");
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let write_result = (|| -> anyhow::Result<()> {
            if let Some(parent_session_id) = parent_session_id {
                let parent_exists: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
                    rusqlite::params![parent_session_id],
                    |row| row.get(0),
                )?;
                anyhow::ensure!(
                    parent_exists,
                    "parent session '{}' not found",
                    parent_session_id
                );
            }
            conn.execute(
                "INSERT INTO sessions (
                    id, input_text, status, created_at, updated_at, origin, parent_session_id
                 ) VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    session_id,
                    input_text,
                    now,
                    now,
                    origin_kind,
                    parent_session_id
                ],
            )?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        if let Err(error) = conn.execute_batch("COMMIT") {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error.into());
        }
        drop(conn);
        self.cache_invalidate_session_history_page();
        Ok(Session {
            session_id,
            input_text: input_text.into(),
            title: None,
            status: SessionStatus::Pending,
            run_end_reason: None,
            created_at: now.clone(),
            updated_at: now,
            origin,
        })
    }

    pub fn get_session(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        validate_session_id(session_id)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at,
                    origin, parent_session_id, run_end_reason
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![session_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(map_session_list_row(row)?)),
            None => Ok(None),
        }
    }

    /// Return every persisted session id in stable order.
    ///
    /// Memory runtime startup uses this narrow view to identify sessions that
    /// existed before its live subscription began without loading session
    /// records or transcript projections into memory.
    pub fn list_session_ids(&self) -> anyhow::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT id FROM sessions ORDER BY id ASC")?;
        let rows = stmt.query_map([], session_id_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn session_id_high_water(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT MAX(id) FROM sessions", [], |row| {
                row.get::<_, Option<String>>(0)?
                    .map(|id| {
                        if is_canonical_id(&id, "ses") {
                            Ok(id)
                        } else {
                            Err(rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                "invalid persisted session_id high-water mark".into(),
                            ))
                        }
                    })
                    .transpose()
            })?)
    }

    pub fn list_session_ids_page(
        &self,
        after_session_id: Option<&str>,
        high_water_session_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<String>> {
        if let Some(after_session_id) = after_session_id {
            validate_session_id(after_session_id)?;
        }
        validate_session_id(high_water_session_id)?;
        anyhow::ensure!(
            (1..=MAX_MEMORY_SESSION_ID_PAGE_SIZE).contains(&limit),
            "session ID page limit must be between 1 and {MAX_MEMORY_SESSION_ID_PAGE_SIZE}"
        );
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id FROM sessions
             WHERE (?1 IS NULL OR id > ?1)
               AND id <= ?2
             ORDER BY id COLLATE BINARY ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![after_session_id, high_water_session_id, limit as i64],
            session_id_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn update_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2,
                run_end_reason = CASE WHEN ?1 IN ('pending', 'running') THEN NULL ELSE run_end_reason END
             WHERE id = ?3",
            rusqlite::params![status.as_str(), now, session_id],
        )?;
        self.cache_invalidate_session_history_page();
        Ok(())
    }

    pub fn set_session_run_end_reason(&self, session_id: &str, reason: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        let reason = haven_common::error::sanitize_error_text(reason)
            .trim()
            .to_owned();
        anyhow::ensure!(
            !reason.is_empty(),
            "session run-end reason must not be empty"
        );
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let changed = conn.execute(
            "UPDATE sessions SET run_end_reason = ?1, updated_at = ?2
             WHERE id = ?3 AND status IN ('paused', 'completed', 'error')",
            rusqlite::params![reason, now, session_id],
        )?;
        anyhow::ensure!(
            changed > 0,
            "session '{}' is missing or no longer in a run-end status",
            session_id
        );
        self.cache_invalidate_session_history_page();
        Ok(())
    }

    pub fn update_session_title(&self, session_id: &str, title: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let updated = conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, now, session_id],
        )?;
        anyhow::ensure!(updated == 1, "session '{session_id}' not found");
        self.cache_invalidate_session_history_page();
        Ok(())
    }

    pub fn list_persisted_sessions(&self, limit: i64, offset: i64) -> anyhow::Result<Vec<Session>> {
        if offset == 0
            && limit == 50
            && let Some(cached) = self.cache_get_session_history_page()
        {
            return Ok(cached);
        }
        let cache_gen =
            (offset == 0 && limit == 50).then(|| self.cache_generation("_session_history_page"));
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at,
                    origin, parent_session_id, run_end_reason
             FROM sessions ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], map_session_list_row)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        if let Some(cache_gen) = cache_gen {
            self.cache_put_session_history_page(sessions.clone(), 10, cache_gen);
        }
        Ok(sessions)
    }

    pub fn search_sessions(&self, query: &str) -> anyhow::Result<Vec<Session>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, input_text, title, status, created_at, updated_at,
                    origin, parent_session_id, run_end_reason
             FROM sessions {SEARCH_WHERE}
             ORDER BY created_at DESC LIMIT 50",
        ))?;
        let rows = stmt.query_map(rusqlite::params![pattern], map_session_list_row)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }
    pub fn count_sessions(&self) -> anyhow::Result<i64> {
        let conn = self.conn();
        conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .map_err(Into::into)
    }

    pub fn count_sessions_search(&self, query: &str) -> anyhow::Result<i64> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM sessions {SEARCH_WHERE}"),
            rusqlite::params![pattern],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }

    pub fn search_sessions_paginated(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Session>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, input_text, title, status, created_at, updated_at,
                    origin, parent_session_id, run_end_reason
             FROM sessions {SEARCH_WHERE}
             ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
        ))?;
        let rows = stmt.query_map(
            rusqlite::params![pattern, limit, offset],
            map_session_list_row,
        )?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Return direct child sessions in stable newest-first order.
    pub fn list_session_children(
        &self,
        parent_session_id: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Session>> {
        validate_session_id(parent_session_id)?;
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at,
                    origin, parent_session_id, run_end_reason
             FROM sessions
             WHERE origin = 'agent_spawn' AND parent_session_id = ?1
             ORDER BY created_at DESC, id ASC LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![parent_session_id, limit, offset],
            map_session_list_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        validate_session_id(session_id)?;
        let conn = self.conn();
        let episode_ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM memory_items WHERE session_id = ?1")?;
            let rows = stmt.query_map(rusqlite::params![session_id], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        // messages, session_steps and partial_messages cascade on session delete
        // (ON DELETE CASCADE). The memory-item trigger removes episode
        // embeddings in the same database operation.
        let affected = conn.execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )?;
        if affected == 0 {
            anyhow::bail!("session '{}' not found in database", session_id);
        }
        // Drop all session-scoped fact-extraction and event-consumer state for
        // this session; otherwise deletion leaves permanent kv_store rows.
        conn.execute(
            "DELETE FROM kv_store
             WHERE key = ?1 OR key = ?2 OR key = ?3 OR key = ?4
                OR key LIKE ?5 OR key LIKE ?6",
            rusqlite::params![
                format!("fact_extraction.{}", session_id),
                format!("fact_extraction_last_run.{}", session_id),
                format!("fact_extraction_pending.{}", session_id),
                format!("memory_event_cursor.{}", session_id),
                format!("fact_extraction_episode_pending.{}.%", session_id),
                format!("fact_extraction_episode_done.{}.%", session_id)
            ],
        )?;
        drop(conn);
        self.clear_pending_embedding_models_for_ids(
            crate::embeddings::entity_kind::EPISODE,
            &episode_ids,
        );
        self.cache_invalidate_session_history_page();
        self.cache_invalidate_messages(session_id);
        self.cache_invalidate_memory();
        Ok(())
    }

    pub fn delete_all_sessions(&self) -> anyhow::Result<usize> {
        let conn = self.conn();
        // Wrap both DELETEs in a transaction so readers don't see
        // orphaned messages between the two operations. A mid-transaction
        // failure rolls back explicitly — with a connection pool the
        // checked-out connection is reused afterwards, and re-pooling with
        // an open write transaction would poison it (later statements would
        // run inside the abandoned transaction and the held write lock would
        // block the other pooled connections).
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<usize> {
            // Delete all messages (session-level data) first.
            conn.execute("DELETE FROM messages", [])?;
            // CASCADE handles session_steps.
            let count = conn.execute("DELETE FROM sessions", [])?;
            // Extraction and event-consumer state are session-scoped even
            // though they live in the generic internal kv table.
            conn.execute(
                "DELETE FROM kv_store
                 WHERE key LIKE 'fact_extraction.%'
                    OR key LIKE 'fact_extraction_last_run.%'
                    OR key LIKE 'fact_extraction_episode_done.%'
                    OR key LIKE 'fact_extraction_pending.%'
                    OR key LIKE 'fact_extraction_episode_pending.%'
                    OR key GLOB 'memory_event_cursor.*'",
                [],
            )?;
            Ok(count)
        })();
        match result {
            Ok(count) => {
                conn.execute_batch("COMMIT")?;
                drop(conn);
                self.clear_pending_embedding_models();
                self.cache_invalidate_session_history_page();
                self.cache_invalidate_all_messages();
                if count > 0 {
                    self.cache_invalidate_memory();
                }
                Ok(count)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Mark every still-`running` session as `error`. Called once at app
    /// startup: since the previous process is gone, any `running` session can
    /// never resume and must be surfaced as errored so the user can retry it
    /// via the continue flow. `paused`/`pending` sessions are left untouched —
    /// they represent legitimately waiting work that should survive a
    /// restart. Checkpointed partial stream text is promoted into a real
    /// assistant message (same dedup as the ses-end promote: a message
    /// written after the last checkpoint is not duplicated) so the user
    /// keeps what was already streamed before the crash.
    pub fn finalize_orphaned_running_sessions(&self) -> anyhow::Result<usize> {
        let now = Utc::now().to_rfc3339();
        let ids: Vec<String> = {
            let conn = self.conn();
            conn.prepare("SELECT id FROM sessions WHERE status = 'running'")?
                .query_map([], session_id_from_row)?
                .collect::<Result<_, _>>()?
        };
        let count = self.set_running_status(SessionStatus::Error, &now, None)?;
        for id in ids {
            if let Err(e) = self.promote_partial_message(&id) {
                tracing::warn!(
                    session_id = %id,
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "failed to promote partial message while finalizing orphaned session"
                );
            }
        }
        Ok(count)
    }

    /// Mark every still-`running` session as `paused`. Called on graceful app
    /// exit so in-flight work survives a restart in a resumable state;
    /// `finalize_orphaned_running_sessions` at startup then only affects sessions
    /// left `running` by a crash (no graceful exit).
    pub fn pause_running_sessions(&self) -> anyhow::Result<usize> {
        let now = Utc::now().to_rfc3339();
        let count = self.set_running_status(SessionStatus::Paused, &now, None)?;
        Ok(count)
    }

    /// Shared helper for running-session status transitions:
    /// `UPDATE sessions SET status = ?status, updated_at = ?now WHERE status =
    /// 'running' [AND updated_at < ?cutoff]`. `cutoff` is `None` for the
    /// unconditional (orphan/pause) variants. Centralizing the UPDATE + cache
    /// invalidation keeps the callers from drifting apart.
    fn set_running_status(
        &self,
        status: SessionStatus,
        now: &str,
        cutoff: Option<&str>,
    ) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count = match cutoff {
            Some(threshold) => conn.execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2
                 WHERE status = 'running' AND updated_at < ?3",
                rusqlite::params![status.as_str(), now, threshold],
            )?,
            None => conn.execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2
                 WHERE status = 'running'",
                rusqlite::params![status.as_str(), now],
            )?,
        };
        if count > 0 {
            self.cache_invalidate_session_history_page();
        }
        Ok(count)
    }

    pub fn delete_old_sessions(&self, retention_days: u32) -> anyhow::Result<usize> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
        // Validate IDs before the retention delete can cascade through session-owned data.
        self.list_old_session_ids(retention_days)?;
        let conn = self.conn();
        let episode_ids: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT i.id FROM memory_items i
                 INNER JOIN sessions s ON s.id = i.session_id
                 WHERE s.created_at < ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![cutoff], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let count = conn.execute(
            "DELETE FROM sessions WHERE created_at < ?1",
            rusqlite::params![cutoff],
        )?;
        // Batch retention deletion bypasses `delete_session`; reclaim the
        // session-scoped state on the same connection using kv_store's
        // namespace ownership rule.
        super::kv_store::cleanup_orphan_session_scoped_state_on(&conn)?;
        drop(conn);
        if count > 0 {
            self.cache_invalidate_session_history_page();
            self.cache_invalidate_all_messages();
            self.clear_pending_embedding_models_for_ids(
                crate::embeddings::entity_kind::EPISODE,
                &episode_ids,
            );
            self.cache_invalidate_memory();
        }
        Ok(count)
    }

    /// Return session ids that would be removed by the retention pass. The
    /// supervisor uses this snapshot to quiesce live actors and release their
    /// process-local grants and asset leases before deleting each row.
    pub fn list_old_session_ids(&self, retention_days: u32) -> anyhow::Result<Vec<String>> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
        let conn = self.conn();
        let mut statement =
            conn.prepare("SELECT id FROM sessions WHERE created_at < ?1 ORDER BY created_at, id")?;
        let rows = statement.query_map(rusqlite::params![cutoff], session_id_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn search_sessions_filtered(
        &self,
        query: Option<&str>,
        status: Option<&str>,
        start_date: Option<&str>,
        end_date: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Session>> {
        let query = query.filter(|s| !s.is_empty());
        let status = status.filter(|s| !s.is_empty());
        let start_date = start_date.filter(|s| !s.is_empty());
        let end_date = end_date.filter(|s| !s.is_empty());

        // Unfiltered first page reuses the same short-TTL cache as list_persisted_sessions
        // so repeated visits to the history page skip the DB round-trip.
        let cacheable = query.is_none()
            && status.is_none()
            && start_date.is_none()
            && end_date.is_none()
            && offset == 0
            && limit == 50;
        if cacheable && let Some(cached) = self.cache_get_session_history_page() {
            return Ok(cached);
        }
        let cache_gen = cacheable.then(|| self.cache_generation("_session_history_page"));

        let mut wheres: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(q) = query {
            let p = format!("%{q}%");
            wheres.push(
                "(input_text LIKE ? OR title LIKE ?
                  OR EXISTS (SELECT 1 FROM messages
                             WHERE messages.session_id = sessions.id AND messages.content LIKE ?))"
                    .into(),
            );
            params.push(Box::new(p.clone()));
            params.push(Box::new(p.clone()));
            params.push(Box::new(p));
        }
        if let Some(s) = status {
            // InteractionRequest carries the reason for a paused session.
            if s == "paused" {
                wheres.push("status = 'paused'".into());
            } else {
                wheres.push("status = ?".into());
                params.push(Box::new(s.to_owned()));
            }
        }
        // The UI filters by local calendar days ("2026-08-01"), but created_at
        // is stored as UTC RFC3339. Convert the local day to its UTC midnight
        // boundary so the whole local day is included/excluded as expected.
        // The end date is inclusive: `created_at < next-day-UTC-midnight`.
        if let Some(d) = start_date
            && let Some(bound) = local_date_to_utc(d)
        {
            wheres.push("created_at >= ?".into());
            params.push(Box::new(bound));
        }
        if let Some(d) = end_date
            && let Some(bound) = local_date_to_utc_exclusive_end(d)
        {
            wheres.push("created_at < ?".into());
            params.push(Box::new(bound));
        }

        let where_clause = if wheres.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", wheres.join(" AND "))
        };

        let sql = format!(
            "SELECT id, input_text, title, status, created_at, updated_at, \
                    origin, parent_session_id, run_end_reason \
             FROM sessions {where_clause} ORDER BY created_at DESC LIMIT ? OFFSET ?"
        );

        let conn = self.conn();
        let mut stmt = conn.prepare(&sql)?;

        let mut param_refs: Vec<&dyn rusqlite::types::ToSql> = Vec::new();
        for p in &params {
            param_refs.push(p.as_ref());
        }
        let limit_param: i64 = limit;
        let offset_param: i64 = offset;
        param_refs.push(&limit_param);
        param_refs.push(&offset_param);

        let rows = stmt.query_map(param_refs.as_slice(), map_session_list_row)?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        if let Some(cache_gen) = cache_gen {
            self.cache_put_session_history_page(sessions.clone(), 10, cache_gen);
        }
        Ok(sessions)
    }
}

#[cfg(test)]
mod tests {
    use super::SessionOrigin;
    use crate::Database;
    use chrono::{Local, Utc};
    use haven_common::lifecycle::SessionStatus;

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_session() {
        let db = create_db();
        let session = db.create_session("input text").unwrap();
        assert!(!session.session_id.is_empty());
        assert_eq!(session.input_text, "input text");
        assert_eq!(session.title, None);
        assert_eq!(session.status, SessionStatus::Pending);
        assert_eq!(session.origin, SessionOrigin::User);
        assert!(!session.created_at.is_empty());
        assert!(!session.updated_at.is_empty());

        let has_transcript: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('sessions') WHERE name = 'transcript'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(has_transcript, 0);
    }

    #[test]
    fn peer_session_origin_is_queryable_and_survives_parent_deletion() {
        let db = create_db();
        let parent = db.create_session("parent").unwrap();
        let unrelated = db.create_session("unrelated").unwrap();
        let child = db
            .create_session_with_origin(
                "delegated kickoff",
                SessionOrigin::AgentSpawn {
                    parent_session_id: parent.session_id.clone(),
                },
            )
            .unwrap();

        assert_eq!(
            child.origin,
            SessionOrigin::AgentSpawn {
                parent_session_id: parent.session_id.clone(),
            }
        );
        assert_eq!(
            db.list_session_children(&parent.session_id, 10, 0)
                .unwrap()
                .into_iter()
                .map(|session| session.session_id)
                .collect::<Vec<_>>(),
            vec![child.session_id.clone()]
        );
        assert!(
            db.list_session_children(&unrelated.session_id, 10, 0)
                .unwrap()
                .is_empty()
        );

        db.delete_session(&parent.session_id).unwrap();
        let retained = db.get_session(&child.session_id).unwrap().unwrap();
        assert_eq!(
            retained.origin,
            SessionOrigin::AgentSpawn {
                parent_session_id: parent.session_id,
            }
        );
    }

    #[test]
    fn peer_session_origin_rejects_missing_parent() {
        let db = create_db();
        let error = db
            .create_session_with_origin(
                "orphan kickoff",
                SessionOrigin::AgentSpawn {
                    parent_session_id: "ses-00000000000000000000000000000000".into(),
                },
            )
            .unwrap_err();

        assert!(error.to_string().contains("parent session"));
        assert_eq!(db.count_sessions().unwrap(), 0);
    }

    #[test]
    fn peer_session_origin_rejects_noncanonical_parent_before_write() {
        let db = create_db();
        let error = db
            .create_session_with_origin(
                "invalid parent kickoff",
                SessionOrigin::AgentSpawn {
                    parent_session_id: "session-invalid".into(),
                },
            )
            .unwrap_err();

        assert!(error.to_string().contains("invalid parent session id"));
        assert_eq!(db.count_sessions().unwrap(), 0);
    }

    #[test]
    fn persisted_peer_session_origin_rejects_noncanonical_parent_identity() {
        let db = create_db();
        let parent = db.create_session("parent").unwrap();
        let child = db
            .create_session_with_origin(
                "delegated kickoff",
                SessionOrigin::AgentSpawn {
                    parent_session_id: parent.session_id,
                },
            )
            .unwrap();

        db.conn()
            .execute(
                "UPDATE sessions SET parent_session_id = 'session-invalid' WHERE id = ?1",
                [&child.session_id],
            )
            .unwrap();

        let error = db.get_session(&child.session_id).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid persisted session origin metadata")
        );
    }

    #[test]
    fn test_create_session_without_session_works() {
        let db = create_db();
        let session = db.create_session("input").unwrap();
        assert!(!session.session_id.is_empty());
    }

    #[test]
    fn list_session_ids_returns_every_id_in_stable_order() {
        let db = create_db();
        let first = db.create_session("first").unwrap();
        let second = db.create_session("second").unwrap();

        let mut expected = vec![first.session_id, second.session_id];
        expected.sort();
        assert_eq!(db.list_session_ids().unwrap(), expected);
    }

    #[test]
    fn session_id_keyset_pages_are_bounded_and_honor_captured_high_water() {
        let db = create_db();
        let mut expected = (0..130)
            .map(|index| {
                db.create_session(&format!("paged-{index}"))
                    .unwrap()
                    .session_id
            })
            .collect::<Vec<_>>();
        expected.sort();
        let high_water_session_id = db.session_id_high_water().unwrap().unwrap();
        let later_id = format!("ses-{}", "f".repeat(32));
        db.conn()
            .execute("INSERT INTO sessions (id) VALUES (?1)", [&later_id])
            .unwrap();

        let mut after_session_id = None;
        let mut actual = Vec::new();
        loop {
            let page = db
                .list_session_ids_page(after_session_id.as_deref(), &high_water_session_id, 64)
                .unwrap();
            assert!(page.len() <= 64);
            if page.is_empty() {
                break;
            }
            after_session_id = page.last().cloned();
            actual.extend(page);
        }
        assert_eq!(actual, expected);
        assert!(
            db.list_session_ids_page(None, &high_water_session_id, 0)
                .is_err()
        );
        assert!(
            db.list_session_ids_page(None, &high_water_session_id, 65)
                .is_err()
        );
    }

    #[test]
    fn test_list_tool_runs_returns_most_recent_first() {
        let db = create_db();
        let first = db.create_session("first").unwrap();
        let second = db.create_session("second").unwrap();
        let sessions = db.list_persisted_sessions(1, 0).unwrap();
        assert_eq!(sessions.len(), 1);
        // The most recent session must come first —the app start
        // session restore relies on this ordering.
        assert_eq!(sessions[0].session_id, second.session_id);
        assert_ne!(sessions[0].session_id, first.session_id);
    }

    #[test]
    fn test_get_session_found() {
        let db = create_db();
        let created = db.create_session("input").unwrap();
        let found = db.get_session(&created.session_id).unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.session_id, created.session_id);
        assert_eq!(found.input_text, "input");
    }

    #[test]
    fn test_get_session_not_found() {
        let db = create_db();
        let missing_id = haven_common::types::new_id("ses");
        let result = db.get_session(&missing_id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_session_status() {
        let db = create_db();
        let session = db.create_session("input").unwrap();
        db.update_session_status(&session.session_id, SessionStatus::Running)
            .unwrap();
        let updated = db.get_session(&session.session_id).unwrap().unwrap();
        assert_eq!(updated.status, SessionStatus::Running);
    }

    #[test]
    fn test_list_tool_runs_default() {
        let db = create_db();
        db.create_session("a").unwrap();
        db.create_session("b").unwrap();
        db.create_session("c").unwrap();

        let sessions = db.list_persisted_sessions(50, 0).unwrap();
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn test_list_tool_runs_limit_offset() {
        let db = create_db();
        for i in 0..5 {
            db.create_session(&format!("session {i}")).unwrap();
        }
        let sessions = db.list_persisted_sessions(2, 0).unwrap();
        assert_eq!(sessions.len(), 2);

        let sessions = db.list_persisted_sessions(2, 2).unwrap();
        assert_eq!(sessions.len(), 2);

        let sessions = db.list_persisted_sessions(10, 5).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_persisted_sessions_caching() {
        let db = create_db();
        db.create_session("a").unwrap();
        let first = db.list_persisted_sessions(50, 0).unwrap();
        assert_eq!(first.len(), 1);

        db.create_session("b").unwrap();
        let second = db.list_persisted_sessions(50, 0).unwrap();
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn test_search_sessions() {
        let db = create_db();
        db.create_session("rust compiler").unwrap();
        db.create_session("python script").unwrap();
        db.create_session("rust debugging").unwrap();

        let results = db.search_sessions("rust").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.search_sessions("python").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_sessions("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_sessions_in_messages() {
        let db = create_db();
        let session = db.create_session("session").unwrap();
        db.add_message(
            &session.session_id,
            haven_common::types::CanonicalRole::User,
            "message about rust",
            None,
            None,
        )
        .unwrap();
        let results = db.search_sessions("rust").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_sessions_paginated() {
        let db = create_db();
        for i in 0..5 {
            db.create_session(&format!("rust session {}", i)).unwrap();
        }

        let page1 = db.search_sessions_paginated("rust", 2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = db.search_sessions_paginated("rust", 2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = db.search_sessions_paginated("rust", 2, 4).unwrap();
        assert_eq!(page3.len(), 1);

        let empty = db.search_sessions_paginated("rust", 2, 10).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_count_sessions() {
        let db = create_db();
        assert_eq!(db.count_sessions().unwrap(), 0);
        db.create_session("a").unwrap();
        db.create_session("b").unwrap();
        assert_eq!(db.count_sessions().unwrap(), 2);
    }

    #[test]
    fn test_count_sessions_search() {
        let db = create_db();
        db.create_session("hello world").unwrap();
        db.create_session("goodbye").unwrap();
        assert_eq!(db.count_sessions_search("hello").unwrap(), 1);
        assert_eq!(db.count_sessions_search("good").unwrap(), 1);
        assert_eq!(db.count_sessions_search("xyz").unwrap(), 0);
    }

    #[test]
    fn test_delete_session() {
        let db = create_db();
        let session = db.create_session("input").unwrap();
        let other = db.create_session("other").unwrap();
        db.checkpoint_memory_event_cursor(&session.session_id, 5)
            .unwrap();
        db.checkpoint_memory_event_cursor(&other.session_id, 8)
            .unwrap();
        db.delete_session(&session.session_id).unwrap();
        assert!(db.get_session(&session.session_id).unwrap().is_none());
        assert_eq!(db.count_sessions().unwrap(), 1);
        assert_eq!(db.memory_event_cursor(&session.session_id).unwrap(), 0);
        assert_eq!(db.memory_event_cursor(&other.session_id).unwrap(), 8);
    }

    #[test]
    fn test_delete_session_nonexistent() {
        let db = create_db();
        let missing_id = haven_common::types::new_id("ses");
        let result = db.delete_session(&missing_id);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn update_session_title_nonexistent_returns_not_found() {
        let db = create_db();
        let missing_id = haven_common::types::new_id("ses");

        let error = db
            .update_session_title(&missing_id, "late title")
            .unwrap_err();

        assert!(error.to_string().contains("not found"));
    }

    #[test]
    fn test_delete_all_sessions() {
        let db = create_db();
        let first = db.create_session("a").unwrap();
        db.create_session("b").unwrap();
        db.create_session("c").unwrap();
        db.set_kv(
            &format!("fact_extraction.{}", first.session_id),
            "msg-e11ea518ef030561fe32765a0ab84b18",
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", first.session_id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(
            &format!(
                "fact_extraction_episode_done.{}.msg-1c54a19f14b80e82ef5996e9ca905fd1",
                first.session_id
            ),
            &first.session_id,
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_pending.{}", first.session_id),
            "0:1:0:0",
        )
        .unwrap();
        db.checkpoint_memory_event_cursor(&first.session_id, 12)
            .unwrap();

        let count = db.delete_all_sessions().unwrap();
        assert_eq!(count, 3);
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", first.session_id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", first.session_id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.msg-1c54a19f14b80e82ef5996e9ca905fd1",
                first.session_id
            ))
            .unwrap()
            .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", first.session_id))
                .unwrap()
                .is_none()
        );
        assert_eq!(db.memory_event_cursor(&first.session_id).unwrap(), 0);
    }

    #[test]
    fn test_delete_all_sessions_empty() {
        let db = create_db();
        let count = db.delete_all_sessions().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_finalize_orphaned_running_sessions() {
        let db = create_db();
        let running = db.create_session("running").unwrap();
        db.update_session_status(&running.session_id, SessionStatus::Running)
            .unwrap();
        let paused = db.create_session("paused").unwrap();
        db.update_session_status(&paused.session_id, SessionStatus::Paused)
            .unwrap();
        let pending = db.create_session("pending").unwrap();
        let done = db.create_session("done").unwrap();
        db.update_session_status(&done.session_id, SessionStatus::Completed)
            .unwrap();

        let count = db.finalize_orphaned_running_sessions().unwrap();
        assert_eq!(count, 1);

        assert_eq!(
            db.get_session(&running.session_id).unwrap().unwrap().status,
            SessionStatus::Error
        );
        // paused/pending are left alone —they are legitimate waiting work.
        assert_eq!(
            db.get_session(&paused.session_id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&pending.session_id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        assert_eq!(
            db.get_session(&done.session_id).unwrap().unwrap().status,
            SessionStatus::Completed
        );
    }

    #[test]
    fn test_delete_old_sessions() {
        let db = create_db();
        let first = db.create_session("a").unwrap();
        db.create_session("b").unwrap();
        db.set_kv(
            &format!("fact_extraction.{}", first.session_id),
            "msg-e11ea518ef030561fe32765a0ab84b18",
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", first.session_id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(
            &format!(
                "fact_extraction_episode_done.{}.msg-1c54a19f14b80e82ef5996e9ca905fd1",
                first.session_id
            ),
            &first.session_id,
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_pending.{}", first.session_id),
            "0:1:0:0",
        )
        .unwrap();
        db.checkpoint_memory_event_cursor(&first.session_id, 15)
            .unwrap();

        let count = db.delete_old_sessions(0).unwrap();
        assert_eq!(count, 2);
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", first.session_id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", first.session_id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.msg-1c54a19f14b80e82ef5996e9ca905fd1",
                first.session_id
            ))
            .unwrap()
            .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", first.session_id))
                .unwrap()
                .is_none()
        );
        assert_eq!(db.memory_event_cursor(&first.session_id).unwrap(), 0);
    }

    #[test]
    fn test_delete_old_tool_runs_keeps_recent() {
        let db = create_db();
        db.create_session("a").unwrap();

        let count = db.delete_old_sessions(365).unwrap();
        assert_eq!(count, 0);
        assert_eq!(db.count_sessions().unwrap(), 1);
    }

    #[test]
    fn test_search_sessions_filtered_query_only() {
        let db = create_db();
        db.create_session("rust compile").unwrap();
        db.create_session("python run").unwrap();

        let results = db
            .search_sessions_filtered(Some("rust"), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_sessions_filtered_status() {
        let db = create_db();
        let t1 = db.create_session("a").unwrap();
        db.create_session("b").unwrap();
        db.update_session_status(&t1.session_id, SessionStatus::Completed)
            .unwrap();

        let results = db
            .search_sessions_filtered(None, Some("completed"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, SessionStatus::Completed);
    }

    #[test]
    fn test_search_sessions_filtered_date_range() {
        let db = create_db();
        db.create_session("a").unwrap();
        db.create_session("b").unwrap();

        let results = db
            .search_sessions_filtered(None, None, Some("2000-01-01"), Some("2099-12-31"), 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = db
            .search_sessions_filtered(None, None, Some("2099-01-01"), None, 50, 0)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_sessions_filtered_combined() {
        let db = create_db();
        let t1 = db.create_session("rust compiler bug").unwrap();
        db.create_session("python script").unwrap();
        db.update_session_status(&t1.session_id, SessionStatus::Completed)
            .unwrap();

        let results = db
            .search_sessions_filtered(Some("rust"), Some("completed"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, t1.session_id);
    }

    #[test]
    fn test_search_sessions_filtered_no_filters() {
        let db = create_db();
        db.create_session("a").unwrap();
        db.create_session("b").unwrap();

        let results = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_sessions_filtered_empty_query_ignored() {
        let db = create_db();
        db.create_session("a").unwrap();

        let results = db
            .search_sessions_filtered(Some(""), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_sessions_filtered_end_date_includes_end_day() {
        let db = create_db();
        let session = db.create_session("a").unwrap();

        // created_at is stored as UTC RFC3339; its local calendar date must
        // be included when filtering with that same date as the end bound.
        let local_today = Utc::now()
            .with_timezone(&Local)
            .format("%Y-%m-%d")
            .to_string();

        let results = db
            .search_sessions_filtered(None, None, Some(&local_today), Some(&local_today), 50, 0)
            .unwrap();
        assert!(
            results.iter().any(|t| t.session_id == session.session_id),
            "session created today must match a same-day end-date filter"
        );
    }

    #[test]
    fn test_search_sessions_filtered_no_filters_uses_cache() {
        let db = create_db();
        db.create_session("a").unwrap();
        db.create_session("b").unwrap();

        let first = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(first.len(), 2);

        // The unfiltered first page is cached; a second call must return
        // immediately from the cache (and stay consistent after invalidation).
        let second = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(second.len(), 2);

        db.create_session("c").unwrap();
        let third = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(third.len(), 3);
    }

    #[test]
    fn session_identity_inputs_and_typed_reads_require_canonical_ids() {
        let db = create_db();
        let session = db.create_session("valid session").unwrap();
        assert!(db.get_session("ses-short").is_err());
        assert!(
            db.update_session_status("ses-short", SessionStatus::Paused)
                .is_err()
        );
        assert!(
            db.set_session_run_end_reason("ses-short", "reason")
                .is_err()
        );
        assert!(db.update_session_title("ses-short", "title").is_err());
        assert!(db.delete_session("ses-short").is_err());

        db.conn()
            .execute(
                "UPDATE sessions SET id = 'ses-short', created_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                [&session.session_id],
            )
            .unwrap();
        assert!(db.list_session_ids().is_err());
        assert!(db.session_id_high_water().is_err());
        assert!(db.list_persisted_sessions(50, 0).is_err());
        assert!(db.list_old_session_ids(1).is_err());
        assert!(db.delete_old_sessions(1).is_err());
    }
}
