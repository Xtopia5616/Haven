use crate::db::Database;
use chrono::{Local, NaiveDate, TimeZone, Utc};
use rusqlite::OptionalExtension;

/// WHERE clause shared by every session search query (list, count, paginated).
/// Kept as one constant so search semantics cannot drift between queries.
const SEARCH_WHERE: &str = "WHERE input_text LIKE ?1 OR transcript LIKE ?1 OR title LIKE ?1
    OR EXISTS (SELECT 1 FROM messages
               WHERE messages.session_id = sessions.id AND messages.content LIKE ?1)";

/// Map a row produced by a history-list query (8 columns, no react_state).
fn map_session_list_row(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        input_text: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        transcript: row.get(6)?,
        react_state: None,
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
    pub id: String,
    pub input_text: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub transcript: String,
    pub react_state: Option<String>,
}

/// Metadata committed alongside `sessions.react_state`.
///
/// The snapshot JSON is the event authority; these cursors only describe the
/// point at which its materialized projections were observed. They let resume
/// distinguish a projection that is ahead of a snapshot (reconcile) from one
/// that is behind it (fail closed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReactCheckpoint {
    pub revision: i64,
    pub event_cursor: i64,
    pub message_ingress_seq: i64,
    pub step_seq: i64,
}

impl Database {
    pub fn create_session(&self, input_text: &str, transcript: &str) -> anyhow::Result<Session> {
        let id = haven_common::types::new_id("ses");
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sessions (id, input_text, status, created_at, updated_at, transcript)
             VALUES (?1, ?2, 'pending', ?3, ?4, ?5)",
            rusqlite::params![id, input_text, now, now, transcript],
        )?;
        self.cache_invalidate_sessions();
        Ok(Session {
            id,
            input_text: input_text.into(),
            title: None,
            status: "pending".into(),
            created_at: now.clone(),
            updated_at: now,
            transcript: transcript.into(),
            react_state: None,
        })
    }

    pub fn get_session(&self, id: &str) -> anyhow::Result<Option<Session>> {
        let conn = self.conn();
        // react_state is excluded here too: it is a full ReAct snapshot that
        // can be tens of KB, and consumers of the Session row (resume payload,
        // last-conversation restore) never read it. The agent reads it via
        // `get_react_state`, which selects only that column.
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
             FROM sessions WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(map_session_list_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn update_session_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET status = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status, now, id],
        )?;
        self.cache_invalidate_sessions();
        Ok(())
    }

    pub fn update_session_title(&self, id: &str, title: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, now, id],
        )?;
        self.cache_invalidate_sessions();
        Ok(())
    }

    pub fn list_sessions(&self, limit: i64, offset: i64) -> anyhow::Result<Vec<Session>> {
        if offset == 0
            && limit == 50
            && let Some(cached) = self.cache_get_sessions()
        {
            return Ok(cached);
        }
        let cache_gen = (offset == 0 && limit == 50).then(|| self.cache_generation("_sessions"));
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
             FROM sessions ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], map_session_list_row)?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        if let Some(cache_gen) = cache_gen {
            self.cache_put_sessions(sessions.clone(), 10, cache_gen);
        }
        Ok(sessions)
    }

    pub fn search_sessions(&self, query: &str) -> anyhow::Result<Vec<Session>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
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
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
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

    pub fn delete_session(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        let episode_ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT id FROM memory_items WHERE session_id = ?1")?;
            let rows = stmt.query_map(rusqlite::params![id], |row| row.get(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        // messages, session_steps and partial_messages cascade on session delete
        // (ON DELETE CASCADE). The memory-item trigger removes episode
        // embeddings in the same database operation.
        let affected = conn.execute("DELETE FROM sessions WHERE id = ?1", rusqlite::params![id])?;
        if affected == 0 {
            anyhow::bail!("session '{}' not found in database", id);
        }
        // Drop all session-scoped fact-extraction state for this session;
        // otherwise every deleted session leaves permanent kv_store rows behind.
        conn.execute(
            "DELETE FROM kv_store WHERE key = ?1 OR key = ?2 OR key = ?3 OR key = ?4",
            rusqlite::params![
                format!("fact_extraction.{}", id),
                format!("fact_extraction_last_run.{}", id),
                format!("fact_extraction_episode.{}", id),
                format!("fact_extraction_pending.{}", id)
            ],
        )?;
        drop(conn);
        self.clear_pending_embedding_models_for_ids(
            crate::embeddings::entity_kind::EPISODE,
            &episode_ids,
        );
        self.cache_invalidate_sessions();
        self.cache_invalidate_messages(id);
        self.cache_invalidate_memory();
        Ok(())
    }

    pub fn clear_sessions(&self) -> anyhow::Result<usize> {
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
            // Extraction state is session-scoped even though it lives in the
            // generic internal kv table. Clear all four namespaces together
            // with the session rows so a history reset is complete.
            conn.execute(
                "DELETE FROM kv_store
                 WHERE key LIKE 'fact_extraction.%'
                    OR key LIKE 'fact_extraction_last_run.%'
                    OR key LIKE 'fact_extraction_episode.%'
                    OR key LIKE 'fact_extraction_pending.%'",
                [],
            )?;
            Ok(count)
        })();
        match result {
            Ok(count) => {
                conn.execute_batch("COMMIT")?;
                drop(conn);
                self.clear_pending_embedding_models();
                self.cache_invalidate_sessions();
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
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?
        };
        let count = self.set_running_status("error", &now, None)?;
        for id in ids {
            if let Err(e) = self.promote_partial_message(&id) {
                tracing::warn!(
                    "finalize_orphaned_running_sessions: failed to promote partial for session {}: {}",
                    id,
                    e
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
        let count = self.set_running_status("paused", &now, None)?;
        Ok(count)
    }

    /// Shared helper for running-session status transitions:
    /// `UPDATE sessions SET status = ?status, updated_at = ?now WHERE status =
    /// 'running' [AND updated_at < ?cutoff]`. `cutoff` is `None` for the
    /// unconditional (orphan/pause) variants. Centralizing the UPDATE + cache
    /// invalidation keeps the callers from drifting apart.
    fn set_running_status(
        &self,
        status: &str,
        now: &str,
        cutoff: Option<&str>,
    ) -> anyhow::Result<usize> {
        let conn = self.conn();
        let count = match cutoff {
            Some(threshold) => conn.execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2
                 WHERE status = 'running' AND updated_at < ?3",
                rusqlite::params![status, now, threshold],
            )?,
            None => conn.execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2
                 WHERE status = 'running'",
                rusqlite::params![status, now],
            )?,
        };
        if count > 0 {
            self.cache_invalidate_sessions();
        }
        Ok(count)
    }

    pub fn delete_old_sessions(&self, retention_days: u32) -> anyhow::Result<usize> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
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
        // session-scoped extraction state in the same connection while the
        // deleted session ids are still the source of truth for this pass.
        conn.execute(
            "DELETE FROM kv_store
             WHERE (key LIKE 'fact_extraction.%'
                    OR key LIKE 'fact_extraction_last_run.%'
                    OR key LIKE 'fact_extraction_episode.%'
                    OR key LIKE 'fact_extraction_pending.%')
               AND NOT EXISTS (SELECT 1 FROM sessions
                               WHERE id = CASE
                                   WHEN key LIKE 'fact_extraction_last_run.%'
                                   THEN substr(key, 26)
                                   WHEN key LIKE 'fact_extraction_episode.%'
                                   THEN substr(key, 25)
                                   WHEN key LIKE 'fact_extraction_pending.%'
                                   THEN substr(key, 25)
                                   ELSE substr(key, 17)
                               END)",
            [],
        )?;
        drop(conn);
        if count > 0 {
            self.cache_invalidate_sessions();
            self.cache_invalidate_all_messages();
            self.clear_pending_embedding_models_for_ids(
                crate::embeddings::entity_kind::EPISODE,
                &episode_ids,
            );
            self.cache_invalidate_memory();
        }
        Ok(count)
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

        // Unfiltered first page reuses the same short-TTL cache as list_sessions
        // so repeated visits to the history page skip the DB round-trip.
        let cacheable = query.is_none()
            && status.is_none()
            && start_date.is_none()
            && end_date.is_none()
            && offset == 0
            && limit == 50;
        if cacheable && let Some(cached) = self.cache_get_sessions() {
            return Ok(cached);
        }
        let cache_gen = cacheable.then(|| self.cache_generation("_sessions"));

        let mut wheres: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(q) = query {
            let p = format!("%{q}%");
            wheres.push(
                "(input_text LIKE ? OR transcript LIKE ? OR title LIKE ?
                  OR EXISTS (SELECT 1 FROM messages
                             WHERE messages.session_id = sessions.id AND messages.content LIKE ?))"
                    .into(),
            );
            params.push(Box::new(p.clone()));
            params.push(Box::new(p.clone()));
            params.push(Box::new(p.clone()));
            params.push(Box::new(p));
        }
        if let Some(s) = status {
            // History "Paused" covers both scheduling pause and ask-awaiting
            // (Phase 4 / F2 distinct wire status).
            if s == "paused" {
                wheres.push(
                    "status IN ('paused','paused_awaiting_answer','paused_awaiting_confirm')"
                        .into(),
                );
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
            "SELECT id, input_text, title, status, created_at, updated_at, transcript \
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
            self.cache_put_sessions(sessions.clone(), 10, cache_gen);
        }
        Ok(sessions)
    }

    /// Save serialized ReAct state (canonical messages + history) for pause/resume.
    ///
    /// The snapshot is gzip-compressed before storage: every branch point
    /// carries a full canonical + history copy, so a long session's snapshot
    /// routinely reaches tens of MB of JSON (observed 53MB) and is rewritten
    /// on every step boundary. Compression shrinks it ~5x (the JSON is
    /// repetitive) and cuts both the DB size and per-step write cost.
    pub fn save_react_state(&self, session_id: &str, state_json: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let compressed = compress_react_state(state_json)?;
        let event_cursor = serde_json::from_str::<serde_json::Value>(state_json)
            .ok()
            .and_then(|value| {
                value
                    .get("events")
                    .and_then(|events| events.as_array())
                    .cloned()
            })
            .map(|events| events.len() as i64)
            .unwrap_or(0);
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            let previous_revision: Option<i64> = conn
                .query_row(
                    "SELECT COALESCE(revision, 0) + 1
                 FROM react_checkpoints WHERE session_id = ?1",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )
                .optional()?;
            let revision = previous_revision.unwrap_or(1);
            let message_ingress_seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(last_ingress_seq, 0)
                     FROM message_ingress_cursors WHERE session_id = ?1",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            let step_seq: i64 = conn
                .query_row(
                    "SELECT COALESCE(last_step_seq, 0)
                     FROM session_step_cursors WHERE session_id = ?1",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or(0);
            conn.execute(
                "UPDATE sessions SET react_state = ?1, updated_at = ?2 WHERE id = ?3",
                rusqlite::params![compressed, now, session_id],
            )?;
            conn.execute(
                "INSERT INTO react_checkpoints
                    (session_id, revision, event_cursor, message_ingress_seq, step_seq, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(session_id) DO UPDATE SET
                    revision = excluded.revision,
                    event_cursor = excluded.event_cursor,
                    message_ingress_seq = excluded.message_ingress_seq,
                    step_seq = excluded.step_seq,
                    updated_at = excluded.updated_at",
                rusqlite::params![
                    session_id,
                    revision,
                    event_cursor,
                    message_ingress_seq,
                    step_seq,
                    now
                ],
            )?;
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        Ok(())
    }

    /// Read the checkpoint metadata written with the latest snapshot.
    pub fn get_react_checkpoint(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<ReactCheckpoint>> {
        let conn = self.conn();
        let value = conn
            .query_row(
                "SELECT revision, event_cursor, message_ingress_seq, step_seq
                 FROM react_checkpoints WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| {
                    Ok(ReactCheckpoint {
                        revision: row.get(0)?,
                        event_cursor: row.get(1)?,
                        message_ingress_seq: row.get(2)?,
                        step_seq: row.get(3)?,
                    })
                },
            )
            .optional()?;
        Ok(value)
    }

    /// Read the current high-water marks of the materialized projections.
    pub fn get_react_projection_cursor(&self, session_id: &str) -> anyhow::Result<(i64, i64)> {
        let conn = self.conn();
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
        Ok((message_ingress_seq, step_seq))
    }

    /// Load serialized ReAct state for a paused session. Snapshots must be
    /// gzip-compressed rows written by `save_react_state`; older uncompressed
    /// rows are incompatible and require a data reset.
    pub fn get_react_state(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let value: Option<rusqlite::types::Value> = conn
            .query_row(
                "SELECT react_state FROM sessions WHERE id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .map_err(anyhow::Error::from)?;
        match value {
            Some(rusqlite::types::Value::Blob(b)) => decompress_react_state(&b).map(Some),
            Some(rusqlite::types::Value::Text(_)) => {
                anyhow::bail!(
                    "incompatible react_state: legacy uncompressed snapshot requires reset"
                )
            }
            _ => Ok(None),
        }
    }
}

/// Gzip-compress a JSON snapshot.
fn compress_react_state(json: &str) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(json.as_bytes())?;
    Ok(enc.finish()?)
}

/// Decompress a stored snapshot. Non-gzip blobs are incompatible with the
/// current snapshot contract and require a data reset.
fn decompress_react_state(blob: &[u8]) -> anyhow::Result<String> {
    if blob.len() >= 2 && blob[0] == 0x1f && blob[1] == 0x8b {
        use std::io::Read;
        let mut dec = flate2::read::GzDecoder::new(blob);
        let mut out = String::new();
        dec.read_to_string(&mut out)?;
        Ok(out)
    } else {
        anyhow::bail!("incompatible react_state: legacy uncompressed snapshot requires reset")
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;
    use chrono::{Local, Utc};

    fn create_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_session() {
        let db = create_db();
        let session = db.create_session("input text", "transcript").unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.input_text, "input text");
        assert_eq!(session.title, None);
        assert_eq!(session.status, "pending");
        assert!(!session.created_at.is_empty());
        assert!(!session.updated_at.is_empty());
        assert_eq!(session.transcript, "transcript");
        assert!(session.react_state.is_none());
    }

    #[test]
    fn test_create_session_without_session_works() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();
        assert!(!session.id.is_empty());
    }

    #[test]
    fn test_list_actions_returns_most_recent_first() {
        let db = create_db();
        let first = db.create_session("first", "").unwrap();
        let second = db.create_session("second", "").unwrap();
        let sessions = db.list_sessions(1, 0).unwrap();
        assert_eq!(sessions.len(), 1);
        // The most recent session must come first —the app start
        // conversation restore relies on this ordering.
        assert_eq!(sessions[0].id, second.id);
        assert_ne!(sessions[0].id, first.id);
    }

    #[test]
    fn test_get_session_found() {
        let db = create_db();
        let created = db.create_session("input", "").unwrap();
        let found = db.get_session(&created.id).unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.input_text, "input");
    }

    #[test]
    fn test_get_session_not_found() {
        let db = create_db();
        let result = db.get_session("non-existent-id").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_session_status() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();
        db.update_session_status(&session.id, "running").unwrap();
        let updated = db.get_session(&session.id).unwrap().unwrap();
        assert_eq!(updated.status, "running");
    }

    #[test]
    fn test_list_actions_default() {
        let db = create_db();
        db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();
        db.create_session("c", "").unwrap();

        let sessions = db.list_sessions(50, 0).unwrap();
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn test_list_actions_limit_offset() {
        let db = create_db();
        for i in 0..5 {
            db.create_session(&format!("ses-{}", i), "").unwrap();
        }
        let sessions = db.list_sessions(2, 0).unwrap();
        assert_eq!(sessions.len(), 2);

        let sessions = db.list_sessions(2, 2).unwrap();
        assert_eq!(sessions.len(), 2);

        let sessions = db.list_sessions(10, 5).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_list_actions_caching() {
        let db = create_db();
        db.create_session("a", "").unwrap();
        let first = db.list_sessions(50, 0).unwrap();
        assert_eq!(first.len(), 1);

        db.create_session("b", "").unwrap();
        let second = db.list_sessions(50, 0).unwrap();
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn test_search_sessions() {
        let db = create_db();
        db.create_session("rust compiler", "").unwrap();
        db.create_session("python script", "").unwrap();
        db.create_session("rust debugging", "").unwrap();

        let results = db.search_sessions("rust").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.search_sessions("python").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_sessions("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_actions_in_transcript() {
        let db = create_db();
        db.create_session("session", "transcript about rust")
            .unwrap();
        let results = db.search_sessions("rust").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_sessions_paginated() {
        let db = create_db();
        for i in 0..5 {
            db.create_session(&format!("rust session {}", i), "")
                .unwrap();
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
        db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();
        assert_eq!(db.count_sessions().unwrap(), 2);
    }

    #[test]
    fn test_count_sessions_search() {
        let db = create_db();
        db.create_session("hello world", "").unwrap();
        db.create_session("goodbye", "").unwrap();
        assert_eq!(db.count_sessions_search("hello").unwrap(), 1);
        assert_eq!(db.count_sessions_search("good").unwrap(), 1);
        assert_eq!(db.count_sessions_search("xyz").unwrap(), 0);
    }

    #[test]
    fn test_delete_session() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();
        db.delete_session(&session.id).unwrap();
        assert!(db.get_session(&session.id).unwrap().is_none());
        assert_eq!(db.count_sessions().unwrap(), 0);
    }

    #[test]
    fn test_delete_session_nonexistent() {
        let db = create_db();
        let result = db.delete_session("non-existent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_clear_sessions() {
        let db = create_db();
        let first = db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();
        db.create_session("c", "").unwrap();
        db.set_kv(&format!("fact_extraction.{}", first.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", first.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(&format!("fact_extraction_episode.{}", first.id), "msg-2")
            .unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", first.id), "1")
            .unwrap();

        let count = db.clear_sessions().unwrap();
        assert_eq!(count, 3);
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", first.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", first.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_episode.{}", first.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", first.id))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_clear_actions_empty() {
        let db = create_db();
        let count = db.clear_sessions().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_finalize_orphaned_running_sessions() {
        let db = create_db();
        let running = db.create_session("running", "").unwrap();
        db.update_session_status(&running.id, "running").unwrap();
        let paused = db.create_session("paused", "").unwrap();
        db.update_session_status(&paused.id, "paused").unwrap();
        let pending = db.create_session("pending", "").unwrap();
        let done = db.create_session("done", "").unwrap();
        db.update_session_status(&done.id, "completed").unwrap();

        let count = db.finalize_orphaned_running_sessions().unwrap();
        assert_eq!(count, 1);

        assert_eq!(
            db.get_session(&running.id).unwrap().unwrap().status,
            "error"
        );
        // paused/pending are left alone —they are legitimate waiting work.
        assert_eq!(
            db.get_session(&paused.id).unwrap().unwrap().status,
            "paused"
        );
        assert_eq!(
            db.get_session(&pending.id).unwrap().unwrap().status,
            "pending"
        );
        assert_eq!(
            db.get_session(&done.id).unwrap().unwrap().status,
            "completed"
        );
    }

    #[test]
    fn test_delete_old_sessions() {
        let db = create_db();
        let first = db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();
        db.set_kv(&format!("fact_extraction.{}", first.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", first.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(&format!("fact_extraction_episode.{}", first.id), "msg-2")
            .unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", first.id), "1")
            .unwrap();

        let count = db.delete_old_sessions(0).unwrap();
        assert_eq!(count, 2);
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", first.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", first.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_episode.{}", first.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", first.id))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_delete_old_actions_keeps_recent() {
        let db = create_db();
        db.create_session("a", "").unwrap();

        let count = db.delete_old_sessions(365).unwrap();
        assert_eq!(count, 0);
        assert_eq!(db.count_sessions().unwrap(), 1);
    }

    #[test]
    fn test_search_sessions_filtered_query_only() {
        let db = create_db();
        db.create_session("rust compile", "").unwrap();
        db.create_session("python run", "").unwrap();

        let results = db
            .search_sessions_filtered(Some("rust"), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_sessions_filtered_status() {
        let db = create_db();
        let t1 = db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();
        db.update_session_status(&t1.id, "completed").unwrap();

        let results = db
            .search_sessions_filtered(None, Some("completed"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "completed");
    }

    #[test]
    fn test_search_sessions_filtered_date_range() {
        let db = create_db();
        db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();

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
        let t1 = db.create_session("rust compiler bug", "").unwrap();
        db.create_session("python script", "").unwrap();
        db.update_session_status(&t1.id, "completed").unwrap();

        let results = db
            .search_sessions_filtered(Some("rust"), Some("completed"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, t1.id);
    }

    #[test]
    fn test_search_sessions_filtered_no_filters() {
        let db = create_db();
        db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();

        let results = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_sessions_filtered_empty_query_ignored() {
        let db = create_db();
        db.create_session("a", "").unwrap();

        let results = db
            .search_sessions_filtered(Some(""), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_sessions_filtered_end_date_includes_end_day() {
        let db = create_db();
        let session = db.create_session("a", "").unwrap();

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
            results.iter().any(|t| t.id == session.id),
            "session created today must match a same-day end-date filter"
        );
    }

    #[test]
    fn test_search_sessions_filtered_list_rows_have_no_react_state() {
        let db = create_db();
        let session = db.create_session("a", "").unwrap();
        db.save_react_state(&session.id, r#"{"v":1}"#).unwrap();

        let results = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].react_state.is_none());

        let listed = db.list_sessions(50, 0).unwrap();
        assert!(listed[0].react_state.is_none());
    }

    #[test]
    fn test_search_sessions_filtered_no_filters_uses_cache() {
        let db = create_db();
        db.create_session("a", "").unwrap();
        db.create_session("b", "").unwrap();

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

        db.create_session("c", "").unwrap();
        let third = db
            .search_sessions_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(third.len(), 3);
    }

    #[test]
    fn test_get_session_excludes_react_state() {
        let db = create_db();
        let session = db.create_session("a", "").unwrap();
        db.save_react_state(&session.id, r#"{"v":1}"#).unwrap();

        let loaded = db.get_session(&session.id).unwrap().unwrap();
        assert!(loaded.react_state.is_none());
        // The full state is still retrievable through the dedicated accessor.
        assert_eq!(
            db.get_react_state(&session.id).unwrap().unwrap(),
            r#"{"v":1}"#
        );
    }

    #[test]
    fn test_save_and_get_react_state() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();

        let result = db.get_react_state(&session.id).unwrap();
        assert!(result.is_none());

        let state = r#"{"step":0,"messages":[]}"#;
        db.save_react_state(&session.id, state).unwrap();

        let loaded = db.get_react_state(&session.id).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap(), state);
    }

    #[test]
    fn test_save_react_state_overwrites() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();

        db.save_react_state(&session.id, r#"{"v":1}"#).unwrap();
        db.save_react_state(&session.id, r#"{"v":2}"#).unwrap();

        let loaded = db.get_react_state(&session.id).unwrap().unwrap();
        assert_eq!(loaded, r#"{"v":2}"#);
    }

    #[test]
    fn test_react_checkpoint_tracks_revision_event_and_projection_cursors() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();

        db.save_react_state(&session.id, r#"{"events":[{},{}]}"#)
            .unwrap();
        let first = db
            .get_react_checkpoint(&session.id)
            .unwrap()
            .expect("checkpoint after snapshot");
        assert_eq!(first.revision, 1);
        assert_eq!(first.event_cursor, 2);
        assert_eq!(first.message_ingress_seq, 0);
        assert_eq!(first.step_seq, 0);

        db.add_message(&session.id, "user", "later", None, None)
            .unwrap();
        db.create_thought_step(&session.id, 1, "step-checkpoint")
            .unwrap();
        db.save_react_state(&session.id, r#"{"events":[{}, {}, {}]}"#)
            .unwrap();

        let second = db
            .get_react_checkpoint(&session.id)
            .unwrap()
            .expect("updated checkpoint");
        assert_eq!(second.revision, 2);
        assert_eq!(second.event_cursor, 3);
        assert_eq!(second.message_ingress_seq, 1);
        assert_eq!(second.step_seq, 1);
        assert!(second.message_ingress_seq > first.message_ingress_seq);
        assert!(second.step_seq > first.step_seq);
    }

    #[test]
    fn test_react_state_roundtrip_compresses() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();
        let big = format!(
            r#"{{"canonical":[{}]}}"#,
            (0..500)
                .map(|i| format!(r#"{{"role":"user","content":[{{"type":"text","text":"message {} 中文内容"}}]}}"#, i))
                .collect::<Vec<_>>()
                .join(",")
        );
        db.save_react_state(&session.id, &big).unwrap();
        let loaded = db.get_react_state(&session.id).unwrap().unwrap();
        assert_eq!(loaded, big);
        // The stored column must actually be compressed (not the raw JSON).
        let raw_len: i64 = db
            .conn()
            .query_row(
                "SELECT length(react_state) FROM sessions WHERE id = ?1",
                [&session.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            (raw_len as usize) < (big.len() / 2),
            "snapshot should be compressed, raw {} vs stored {}",
            big.len(),
            raw_len
        );
    }

    #[test]
    fn test_react_state_legacy_uncompressed_requires_reset() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();
        // Simulate a row written by an older build (plain TEXT, no gzip magic).
        db.conn()
            .execute(
                "UPDATE sessions SET react_state = ?1 WHERE id = ?2",
                rusqlite::params![r#"{"legacy":true}"#, session.id],
            )
            .unwrap();
        let err = db.get_react_state(&session.id).unwrap_err();
        assert!(err.to_string().contains("requires reset"));
    }

    #[test]
    fn test_react_state_non_gzip_blob_requires_reset() {
        let db = create_db();
        let session = db.create_session("input", "").unwrap();
        // A non-gzip BLOB is also outside the current snapshot contract.
        db.conn()
            .execute(
                "UPDATE sessions SET react_state = ?1 WHERE id = ?2",
                rusqlite::params![
                    rusqlite::types::Value::Blob(br#"{"legacy":true}"#.to_vec()),
                    session.id
                ],
            )
            .unwrap();
        let err = db.get_react_state(&session.id).unwrap_err();
        assert!(err.to_string().contains("requires reset"));
    }
}
