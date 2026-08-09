use crate::db::Database;
use chrono::{Local, NaiveDate, TimeZone, Utc};

/// WHERE clause shared by every task search query (list, count, paginated).
/// Kept as one constant so search semantics cannot drift between queries.
const SEARCH_WHERE: &str = "WHERE input_text LIKE ?1 OR transcript LIKE ?1 OR title LIKE ?1
    OR EXISTS (SELECT 1 FROM messages
               WHERE messages.task_id = tasks.id AND messages.content LIKE ?1)";

/// Map a row produced by a history-list query (8 columns, no react_state).
fn map_task_list_row(row: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
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
/// comparison would exclude every task created during the end day itself).
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
pub struct Task {
    pub id: String,
    pub input_text: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub transcript: String,
    pub react_state: Option<String>,
}

impl Database {
    pub fn create_task(&self, input_text: &str, transcript: &str) -> anyhow::Result<Task> {
        let id = haven_common::types::new_id("task");
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO tasks (id, input_text, status, created_at, updated_at, transcript)
             VALUES (?1, ?2, 'pending', ?3, ?4, ?5)",
            rusqlite::params![id, input_text, now, now, transcript],
        )?;
        self.cache_invalidate_tasks();
        Ok(Task {
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

    pub fn get_task(&self, id: &str) -> anyhow::Result<Option<Task>> {
        let conn = self.conn();
        // react_state is excluded here too: it is a full ReAct snapshot that
        // can be tens of KB, and consumers of the Task row (review payload,
        // last-conversation restore) never read it. The agent reads it via
        // `get_react_state`, which selects only that column.
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
             FROM tasks WHERE id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(map_task_list_row(row)?)),
            None => Ok(None),
        }
    }

    pub fn update_task_status(&self, id: &str, status: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![status, now, id],
        )?;
        self.cache_invalidate_tasks();
        Ok(())
    }

    pub fn update_task_title(&self, id: &str, title: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, now, id],
        )?;
        self.cache_invalidate_tasks();
        Ok(())
    }

    pub fn list_tasks(&self, limit: i64, offset: i64) -> anyhow::Result<Vec<Task>> {
        if offset == 0
            && limit == 50
            && let Some(cached) = self.cache_get_tasks()
        {
            return Ok(cached);
        }
        let cache_gen = if offset == 0 && limit == 50 {
            self.cache_generation("_tasks")
        } else {
            0
        };
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
             FROM tasks ORDER BY created_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit, offset], map_task_list_row)?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        if offset == 0 && limit == 50 {
            self.cache_put_tasks(tasks.clone(), 10, cache_gen);
        }
        Ok(tasks)
    }

    pub fn search_tasks(&self, query: &str) -> anyhow::Result<Vec<Task>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
             FROM tasks {SEARCH_WHERE}
             ORDER BY created_at DESC LIMIT 50",
        ))?;
        let rows = stmt.query_map(rusqlite::params![pattern], map_task_list_row)?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }
    pub fn count_tasks(&self) -> anyhow::Result<i64> {
        let conn = self.conn();
        conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
            .map_err(Into::into)
    }

    pub fn count_tasks_search(&self, query: &str) -> anyhow::Result<i64> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        conn.query_row(
            &format!("SELECT COUNT(*) FROM tasks {SEARCH_WHERE}"),
            rusqlite::params![pattern],
            |r| r.get(0),
        )
        .map_err(Into::into)
    }

    pub fn search_tasks_paginated(
        &self,
        query: &str,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Task>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT id, input_text, title, status, created_at, updated_at, transcript 
             FROM tasks {SEARCH_WHERE}
             ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
        ))?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit, offset], map_task_list_row)?;
        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        Ok(tasks)
    }

    pub fn delete_task(&self, id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        // messages, task_steps and partial_messages cascade on task delete
        // (ON DELETE CASCADE), so a single statement keeps history consistent.
        let affected = conn.execute("DELETE FROM tasks WHERE id = ?1", rusqlite::params![id])?;
        if affected == 0 {
            anyhow::bail!("task '{}' not found in database", id);
        }
        // Drop the fact-extraction cursor for this task; otherwise every
        // deleted task leaves a permanent kv_store row behind.
        conn.execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![format!("fact_extraction.{}", id)],
        )?;
        drop(conn);
        self.cache_invalidate_tasks();
        self.cache_invalidate_messages(id);
        Ok(())
    }

    pub fn clear_tasks(&self) -> anyhow::Result<usize> {
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
            // CASCADE handles task_steps.
            let count = conn.execute("DELETE FROM tasks", [])?;
            Ok(count)
        })();
        match result {
            Ok(count) => {
                conn.execute_batch("COMMIT")?;
                drop(conn);
                self.cache_invalidate_tasks();
                Ok(count)
            }
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Mark every still-`running` task as `error`. Called once at app
    /// startup: since the previous process is gone, any `running` task can
    /// never resume and must be surfaced as errored so the user can retry it
    /// via the continue flow. `paused`/`pending` tasks are left untouched 鈥?
    /// they represent legitimately waiting work that should survive a
    /// restart. Checkpointed partial stream text is promoted into a real
    /// assistant message (same dedup as the task-end promote: a message
    /// written after the last checkpoint is not duplicated) so the user
    /// keeps what was already streamed before the crash.
    pub fn finalize_orphaned_running_tasks(&self) -> anyhow::Result<usize> {
        let now = Utc::now().to_rfc3339();
        let ids: Vec<String> = {
            let conn = self.conn();
            conn.prepare("SELECT id FROM tasks WHERE status = 'running'")?
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?
        };
        let count = self.set_running_status("error", &now, None)?;
        for id in ids {
            if let Err(e) = self.promote_partial_message(&id) {
                tracing::warn!(
                    "finalize_orphaned_running_tasks: failed to promote partial for task {}: {}",
                    id,
                    e
                );
            }
        }
        Ok(count)
    }

    /// Mark every still-`running` task as `paused`. Called on graceful app
    /// exit so in-flight work survives a restart in a resumable state;
    /// `finalize_orphaned_running_tasks` at startup then only affects tasks
    /// left `running` by a crash (no graceful exit).
    pub fn pause_running_tasks(&self) -> anyhow::Result<usize> {
        let now = Utc::now().to_rfc3339();
        let count = self.set_running_status("paused", &now, None)?;
        Ok(count)
    }

    /// Shared helper for running-task status transitions:
    /// `UPDATE tasks SET status = ?status, updated_at = ?now WHERE status =
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
                "UPDATE tasks SET status = ?1, updated_at = ?2
                 WHERE status = 'running' AND updated_at < ?3",
                rusqlite::params![status, now, threshold],
            )?,
            None => conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2
                 WHERE status = 'running'",
                rusqlite::params![status, now],
            )?,
        };
        if count > 0 {
            self.cache_invalidate_tasks();
        }
        Ok(count)
    }

    pub fn delete_old_tasks(&self, retention_days: u32) -> anyhow::Result<usize> {
        let cutoff = (Utc::now() - chrono::Duration::days(retention_days as i64)).to_rfc3339();
        let conn = self.conn();
        let count = conn.execute(
            "DELETE FROM tasks WHERE created_at < ?1",
            rusqlite::params![cutoff],
        )?;
        if count > 0 {
            self.cache_invalidate_tasks();
        }
        Ok(count)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn search_tasks_filtered(
        &self,
        query: Option<&str>,
        status: Option<&str>,
        start_date: Option<&str>,
        end_date: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Task>> {
        let query = query.and_then(|s| if s.is_empty() { None } else { Some(s) });
        let status = status.and_then(|s| if s.is_empty() { None } else { Some(s) });
        let start_date = start_date.and_then(|s| if s.is_empty() { None } else { Some(s) });
        let end_date = end_date.and_then(|s| if s.is_empty() { None } else { Some(s) });

        // Unfiltered first page reuses the same short-TTL cache as list_tasks
        // so repeated visits to the history page skip the DB round-trip.
        let cacheable = query.is_none()
            && status.is_none()
            && start_date.is_none()
            && end_date.is_none()
            && offset == 0
            && limit == 50;
        if cacheable && let Some(cached) = self.cache_get_tasks() {
            return Ok(cached);
        }
        let cache_gen = if cacheable {
            self.cache_generation("_tasks")
        } else {
            0
        };

        let mut wheres: Vec<String> = Vec::new();
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(q) = query {
            let p = format!("%{q}%");
            wheres.push(
                "(input_text LIKE ? OR transcript LIKE ? OR title LIKE ?
                  OR EXISTS (SELECT 1 FROM messages
                             WHERE messages.task_id = tasks.id AND messages.content LIKE ?))"
                    .into(),
            );
            params.push(Box::new(p.clone()));
            params.push(Box::new(p.clone()));
            params.push(Box::new(p.clone()));
            params.push(Box::new(p));
        }
        if let Some(s) = status {
            wheres.push("status = ?".into());
            params.push(Box::new(s.to_owned()));
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
             FROM tasks {where_clause} ORDER BY created_at DESC LIMIT ? OFFSET ?"
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

        let rows = stmt.query_map(param_refs.as_slice(), map_task_list_row)?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row?);
        }
        if cacheable {
            self.cache_put_tasks(tasks.clone(), 10, cache_gen);
        }
        Ok(tasks)
    }

    /// Save serialized ReAct state (canonical messages + history) for pause/resume.
    ///
    /// The snapshot is gzip-compressed before storage: every branch point
    /// carries a full canonical + history copy, so a long task's snapshot
    /// routinely reaches tens of MB of JSON (observed 53MB) and is rewritten
    /// on every step boundary. Compression shrinks it ~5x (the JSON is
    /// repetitive) and cuts both the DB size and per-step write cost.
    pub fn save_react_state(&self, task_id: &str, state_json: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let compressed = compress_react_state(state_json)?;
        let conn = self.conn();
        conn.execute(
            "UPDATE tasks SET react_state = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![compressed, now, task_id],
        )?;
        Ok(())
    }

    /// Load serialized ReAct state for a paused task. Transparently
    /// decompresses snapshots written by `save_react_state`; legacy
    /// uncompressed rows (older versions, stored as TEXT) are returned as-is.
    pub fn get_react_state(&self, task_id: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let value: Option<rusqlite::types::Value> = conn
            .query_row(
                "SELECT react_state FROM tasks WHERE id = ?1",
                rusqlite::params![task_id],
                |row| row.get(0),
            )
            .map_err(anyhow::Error::from)?;
        match value {
            Some(rusqlite::types::Value::Blob(b)) => decompress_react_state(&b).map(Some),
            // Legacy row written before compression (plain TEXT JSON).
            Some(rusqlite::types::Value::Text(t)) => Ok(Some(t)),
            _ => Ok(None),
        }
    }
}

/// gzip-compress a JSON snapshot. Writes a leading gzip magic, which
/// `decompress_react_state` uses to distinguish compressed from legacy rows.
fn compress_react_state(json: &str) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(json.as_bytes())?;
    Ok(enc.finish()?)
}

/// Decompress a stored snapshot; returns the input unchanged when it is not
/// gzip (legacy uncompressed row written by an older build).
fn decompress_react_state(blob: &[u8]) -> anyhow::Result<String> {
    if blob.len() >= 2 && blob[0] == 0x1f && blob[1] == 0x8b {
        use std::io::Read;
        let mut dec = flate2::read::GzDecoder::new(blob);
        let mut out = String::new();
        dec.read_to_string(&mut out)?;
        Ok(out)
    } else {
        String::from_utf8(blob.to_vec()).map_err(Into::into)
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
    fn test_create_task() {
        let db = create_db();
        let task = db.create_task("input text", "transcript").unwrap();
        assert!(!task.id.is_empty());
        assert_eq!(task.input_text, "input text");
        assert_eq!(task.title, None);
        assert_eq!(task.status, "pending");
        assert!(!task.created_at.is_empty());
        assert!(!task.updated_at.is_empty());
        assert_eq!(task.transcript, "transcript");
        assert!(task.react_state.is_none());
    }

    #[test]
    fn test_create_task_without_session_works() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();
        assert!(!task.id.is_empty());
    }

    #[test]
    fn test_list_tasks_returns_most_recent_first() {
        let db = create_db();
        let first = db.create_task("first", "").unwrap();
        let second = db.create_task("second", "").unwrap();
        let tasks = db.list_tasks(1, 0).unwrap();
        assert_eq!(tasks.len(), 1);
        // The most recent task must come first 鈥?the app start
        // conversation restore relies on this ordering.
        assert_eq!(tasks[0].id, second.id);
        assert_ne!(tasks[0].id, first.id);
    }

    #[test]
    fn test_get_task_found() {
        let db = create_db();
        let created = db.create_task("input", "").unwrap();
        let found = db.get_task(&created.id).unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, created.id);
        assert_eq!(found.input_text, "input");
    }

    #[test]
    fn test_get_task_not_found() {
        let db = create_db();
        let result = db.get_task("non-existent-id").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_task_status() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();
        db.update_task_status(&task.id, "running").unwrap();
        let updated = db.get_task(&task.id).unwrap().unwrap();
        assert_eq!(updated.status, "running");
    }

    #[test]
    fn test_list_tasks_default() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();
        db.create_task("c", "").unwrap();

        let tasks = db.list_tasks(50, 0).unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[test]
    fn test_list_tasks_limit_offset() {
        let db = create_db();
        for i in 0..5 {
            db.create_task(&format!("task-{}", i), "").unwrap();
        }
        let tasks = db.list_tasks(2, 0).unwrap();
        assert_eq!(tasks.len(), 2);

        let tasks = db.list_tasks(2, 2).unwrap();
        assert_eq!(tasks.len(), 2);

        let tasks = db.list_tasks(10, 5).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_list_tasks_caching() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        let first = db.list_tasks(50, 0).unwrap();
        assert_eq!(first.len(), 1);

        db.create_task("b", "").unwrap();
        let second = db.list_tasks(50, 0).unwrap();
        assert_eq!(second.len(), 2);
    }

    #[test]
    fn test_search_tasks() {
        let db = create_db();
        db.create_task("rust compiler", "").unwrap();
        db.create_task("python script", "").unwrap();
        db.create_task("rust debugging", "").unwrap();

        let results = db.search_tasks("rust").unwrap();
        assert_eq!(results.len(), 2);

        let results = db.search_tasks("python").unwrap();
        assert_eq!(results.len(), 1);

        let results = db.search_tasks("nonexistent").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_tasks_in_transcript() {
        let db = create_db();
        db.create_task("task", "transcript about rust").unwrap();
        let results = db.search_tasks("rust").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_tasks_paginated() {
        let db = create_db();
        for i in 0..5 {
            db.create_task(&format!("rust task {}", i), "").unwrap();
        }

        let page1 = db.search_tasks_paginated("rust", 2, 0).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = db.search_tasks_paginated("rust", 2, 2).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = db.search_tasks_paginated("rust", 2, 4).unwrap();
        assert_eq!(page3.len(), 1);

        let empty = db.search_tasks_paginated("rust", 2, 10).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_count_tasks() {
        let db = create_db();
        assert_eq!(db.count_tasks().unwrap(), 0);
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();
        assert_eq!(db.count_tasks().unwrap(), 2);
    }

    #[test]
    fn test_count_tasks_search() {
        let db = create_db();
        db.create_task("hello world", "").unwrap();
        db.create_task("goodbye", "").unwrap();
        assert_eq!(db.count_tasks_search("hello").unwrap(), 1);
        assert_eq!(db.count_tasks_search("good").unwrap(), 1);
        assert_eq!(db.count_tasks_search("xyz").unwrap(), 0);
    }

    #[test]
    fn test_delete_task() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();
        db.delete_task(&task.id).unwrap();
        assert!(db.get_task(&task.id).unwrap().is_none());
        assert_eq!(db.count_tasks().unwrap(), 0);
    }

    #[test]
    fn test_delete_task_nonexistent() {
        let db = create_db();
        let result = db.delete_task("non-existent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_clear_tasks() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();
        db.create_task("c", "").unwrap();

        let count = db.clear_tasks().unwrap();
        assert_eq!(count, 3);
        assert_eq!(db.count_tasks().unwrap(), 0);
    }

    #[test]
    fn test_clear_tasks_empty() {
        let db = create_db();
        let count = db.clear_tasks().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_finalize_orphaned_running_tasks() {
        let db = create_db();
        let running = db.create_task("running", "").unwrap();
        db.update_task_status(&running.id, "running").unwrap();
        let paused = db.create_task("paused", "").unwrap();
        db.update_task_status(&paused.id, "paused").unwrap();
        let pending = db.create_task("pending", "").unwrap();
        let done = db.create_task("done", "").unwrap();
        db.update_task_status(&done.id, "completed").unwrap();

        let count = db.finalize_orphaned_running_tasks().unwrap();
        assert_eq!(count, 1);

        assert_eq!(db.get_task(&running.id).unwrap().unwrap().status, "error");
        // paused/pending are left alone 鈥?they are legitimate waiting work.
        assert_eq!(db.get_task(&paused.id).unwrap().unwrap().status, "paused");
        assert_eq!(db.get_task(&pending.id).unwrap().unwrap().status, "pending");
        assert_eq!(db.get_task(&done.id).unwrap().unwrap().status, "completed");
    }

    #[test]
    fn test_delete_old_tasks() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();

        let count = db.delete_old_tasks(0).unwrap();
        assert_eq!(count, 2);
        assert_eq!(db.count_tasks().unwrap(), 0);
    }

    #[test]
    fn test_delete_old_tasks_keeps_recent() {
        let db = create_db();
        db.create_task("a", "").unwrap();

        let count = db.delete_old_tasks(365).unwrap();
        assert_eq!(count, 0);
        assert_eq!(db.count_tasks().unwrap(), 1);
    }

    #[test]
    fn test_search_tasks_filtered_query_only() {
        let db = create_db();
        db.create_task("rust compile", "").unwrap();
        db.create_task("python run", "").unwrap();

        let results = db
            .search_tasks_filtered(Some("rust"), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_tasks_filtered_status() {
        let db = create_db();
        let t1 = db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();
        db.update_task_status(&t1.id, "completed").unwrap();

        let results = db
            .search_tasks_filtered(None, Some("completed"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, "completed");
    }

    #[test]
    fn test_search_tasks_filtered_date_range() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();

        let results = db
            .search_tasks_filtered(None, None, Some("2000-01-01"), Some("2099-12-31"), 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);

        let results = db
            .search_tasks_filtered(None, None, Some("2099-01-01"), None, 50, 0)
            .unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_tasks_filtered_combined() {
        let db = create_db();
        let t1 = db.create_task("rust compiler bug", "").unwrap();
        db.create_task("python script", "").unwrap();
        db.update_task_status(&t1.id, "completed").unwrap();

        let results = db
            .search_tasks_filtered(Some("rust"), Some("completed"), None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, t1.id);
    }

    #[test]
    fn test_search_tasks_filtered_no_filters() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();

        let results = db
            .search_tasks_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_search_tasks_filtered_empty_query_ignored() {
        let db = create_db();
        db.create_task("a", "").unwrap();

        let results = db
            .search_tasks_filtered(Some(""), None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_search_tasks_filtered_end_date_includes_end_day() {
        let db = create_db();
        let task = db.create_task("a", "").unwrap();

        // created_at is stored as UTC RFC3339; its local calendar date must
        // be included when filtering with that same date as the end bound.
        let local_today = Utc::now()
            .with_timezone(&Local)
            .format("%Y-%m-%d")
            .to_string();

        let results = db
            .search_tasks_filtered(None, None, Some(&local_today), Some(&local_today), 50, 0)
            .unwrap();
        assert!(
            results.iter().any(|t| t.id == task.id),
            "task created today must match a same-day end-date filter"
        );
    }

    #[test]
    fn test_search_tasks_filtered_list_rows_have_no_react_state() {
        let db = create_db();
        let task = db.create_task("a", "").unwrap();
        db.save_react_state(&task.id, r#"{"v":1}"#).unwrap();

        let results = db
            .search_tasks_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].react_state.is_none());

        let listed = db.list_tasks(50, 0).unwrap();
        assert!(listed[0].react_state.is_none());
    }

    #[test]
    fn test_search_tasks_filtered_no_filters_uses_cache() {
        let db = create_db();
        db.create_task("a", "").unwrap();
        db.create_task("b", "").unwrap();

        let first = db
            .search_tasks_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(first.len(), 2);

        // The unfiltered first page is cached; a second call must return
        // immediately from the cache (and stay consistent after invalidation).
        let second = db
            .search_tasks_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(second.len(), 2);

        db.create_task("c", "").unwrap();
        let third = db
            .search_tasks_filtered(None, None, None, None, 50, 0)
            .unwrap();
        assert_eq!(third.len(), 3);
    }

    #[test]
    fn test_get_task_excludes_react_state() {
        let db = create_db();
        let task = db.create_task("a", "").unwrap();
        db.save_react_state(&task.id, r#"{"v":1}"#).unwrap();

        let loaded = db.get_task(&task.id).unwrap().unwrap();
        assert!(loaded.react_state.is_none());
        // The full state is still retrievable through the dedicated accessor.
        assert_eq!(db.get_react_state(&task.id).unwrap().unwrap(), r#"{"v":1}"#);
    }

    #[test]
    fn test_save_and_get_react_state() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();

        let result = db.get_react_state(&task.id).unwrap();
        assert!(result.is_none());

        let state = r#"{"step":0,"messages":[]}"#;
        db.save_react_state(&task.id, state).unwrap();

        let loaded = db.get_react_state(&task.id).unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap(), state);
    }

    #[test]
    fn test_save_react_state_overwrites() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();

        db.save_react_state(&task.id, r#"{"v":1}"#).unwrap();
        db.save_react_state(&task.id, r#"{"v":2}"#).unwrap();

        let loaded = db.get_react_state(&task.id).unwrap().unwrap();
        assert_eq!(loaded, r#"{"v":2}"#);
    }

    #[test]
    fn test_react_state_roundtrip_compresses() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();
        let big = format!(
            r#"{{"canonical":[{}]}}"#,
            (0..500)
                .map(|i| format!(r#"{{"role":"user","content":[{{"type":"text","text":"message {} 中文内容"}}]}}"#, i))
                .collect::<Vec<_>>()
                .join(",")
        );
        db.save_react_state(&task.id, &big).unwrap();
        let loaded = db.get_react_state(&task.id).unwrap().unwrap();
        assert_eq!(loaded, big);
        // The stored column must actually be compressed (not the raw JSON).
        let raw_len: i64 = db
            .conn()
            .query_row(
                "SELECT length(react_state) FROM tasks WHERE id = ?1",
                [&task.id],
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
    fn test_react_state_legacy_uncompressed_still_reads() {
        let db = create_db();
        let task = db.create_task("input", "").unwrap();
        // Simulate a row written by an older build (plain TEXT, no gzip magic).
        db.conn()
            .execute(
                "UPDATE tasks SET react_state = ?1 WHERE id = ?2",
                rusqlite::params![r#"{"legacy":true}"#, task.id],
            )
            .unwrap();
        let loaded = db.get_react_state(&task.id).unwrap().unwrap();
        assert_eq!(loaded, r#"{"legacy":true}"#);
    }
}
