use crate::db::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;

/// Internal key-value store for agent bookkeeping that is not user memory.
///
/// User-facing preferences live in the `facts` table (tag `preference`);
/// this table holds only internal state such as the fact-extraction cursor
/// (`fact_extraction.<session_id>`), the durable extraction outbox
/// (`fact_extraction_pending.<session_id>`), and the committed-event cursor
/// (`memory_event_cursor.<session_id>`). Exposed as `kv_store` in the schema.
impl Database {
    pub fn set_kv(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO kv_store (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![key, value, now],
        )?;
        Ok(())
    }

    pub fn get_kv(&self, key: &str) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT value FROM kv_store WHERE key = ?1")?;
        let mut rows = stmt.query(rusqlite::params![key])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get(0)?)),
            None => Ok(None),
        }
    }

    /// Read the last committed session event consumed by the memory runtime.
    /// An absent key is the initial event sequence zero. This clock is separate
    /// from the fact-extraction message-id cursor.
    pub fn memory_event_cursor(&self, session_id: &str) -> anyhow::Result<i64> {
        let key = memory_event_cursor_key(session_id)?;
        let value = self.get_kv(&key)?;
        parse_memory_event_cursor(value.as_deref())
    }

    /// Persist a memory event checkpoint without allowing it to move
    /// backwards. An equal checkpoint is a no-op; writing zero to a missing
    /// key materializes the initial checkpoint before the first event.
    pub fn checkpoint_memory_event_cursor(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(sequence >= 0, "memory event cursor cannot be negative");
        let key = memory_event_cursor_key(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            let stored_value: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get(0),
                )
                .optional()?;
            let current = parse_memory_event_cursor(stored_value.as_deref())?;
            anyhow::ensure!(
                sequence >= current,
                "memory event cursor cannot move backwards from {current} to {sequence}"
            );
            if stored_value.is_none() || sequence > current {
                conn.execute(
                    "INSERT INTO kv_store (key, value, updated_at)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                         updated_at = excluded.updated_at",
                    rusqlite::params![key, sequence.to_string(), now],
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT").map_err(Into::into),
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Remove one session's memory event checkpoint.
    pub fn clear_memory_event_cursor(&self, session_id: &str) -> anyhow::Result<()> {
        let key = memory_event_cursor_key(session_id)?;
        self.conn().execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    /// Coalesce a fact-extraction job in durable internal state. `1` means the
    /// caller bypassed the normal throttle; once set it is never downgraded by
    /// a later normal enqueue for the same session.
    pub fn enqueue_fact_extraction(
        &self,
        session_id: &str,
        bypass_throttle: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        let key = format!("fact_extraction_pending.{session_id}");
        let value = if bypass_throttle { "1" } else { "0" };
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO kv_store (key, value, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                 value = CASE
                     WHEN kv_store.value = '1' OR excluded.value = '1' THEN '1'
                     ELSE '0'
                 END,
                 updated_at = excluded.updated_at",
            rusqlite::params![key, value, now],
        )?;
        Ok(())
    }

    /// Load durable extraction jobs that still belong to a live session.
    /// Orphaned markers are left for the shared cleanup pass, so this read
    /// never turns a deleted session into a new unit of work.
    pub fn pending_fact_extractions(&self) -> anyhow::Result<Vec<(String, bool)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT substr(k.key, 25), k.value
             FROM kv_store k
             INNER JOIN sessions s ON s.id = substr(k.key, 25)
             WHERE k.key LIKE 'fact_extraction_pending.%'
             ORDER BY k.updated_at, k.key",
        )?;
        let rows = stmt.query_map([], |row| {
            let session_id: String = row.get(0)?;
            let bypass: String = row.get(1)?;
            Ok((session_id, bypass == "1"))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Remove a durable extraction marker after the corresponding job has
    /// completed successfully. Keeping this separate from enqueue makes the
    /// crash window safe: a process dying before this call replays the job on
    /// the next startup, while the extraction cursor makes replay idempotent.
    pub fn clear_pending_fact_extraction(&self, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![format!("fact_extraction_pending.{session_id}")],
        )?;
        Ok(())
    }

    /// Remove session-scoped internal cursors whose session no longer exists
    /// (session rows are deleted without going through `delete_session`, e.g.
    /// history purge or older deletions before cursor cleanup was added). This
    /// also purges extraction throttle stamps, episode cursors, pending markers,
    /// and `memory_event_cursor.<session_id>` checkpoints of dead sessions.
    /// Called during memory maintenance so the kv table does not grow without
    /// bound.
    pub fn cleanup_orphan_extraction_cursors(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        let deleted = conn.execute(
            "DELETE FROM kv_store
             WHERE (key LIKE 'fact_extraction.%'
                    OR key LIKE 'fact_extraction_last_run.%'
                    OR key LIKE 'fact_extraction_episode.%'
                    OR key LIKE 'fact_extraction_pending.%'
                    OR key GLOB 'memory_event_cursor.*')
               AND NOT EXISTS (SELECT 1 FROM sessions
                               WHERE id = CASE
                                   WHEN key GLOB 'memory_event_cursor.*'
                                   THEN substr(key, 21)
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
        Ok(deleted as u64)
    }
}

fn memory_event_cursor_key(session_id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
    Ok(format!("memory_event_cursor.{session_id}"))
}

fn parse_memory_event_cursor(value: Option<&str>) -> anyhow::Result<i64> {
    let Some(value) = value else {
        return Ok(0);
    };
    let sequence = value
        .parse::<i64>()
        .map_err(|error| anyhow::anyhow!("invalid memory event cursor '{value}': {error}"))?;
    anyhow::ensure!(sequence >= 0, "memory event cursor cannot be negative");
    Ok(sequence)
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn set_and_get_kv() {
        let db = test_db();
        db.set_kv("fact_extraction.t1", "msg-1").unwrap();
        assert_eq!(
            db.get_kv("fact_extraction.t1").unwrap(),
            Some("msg-1".into())
        );
        assert_eq!(db.get_kv("nonexistent").unwrap(), None);
    }

    #[test]
    fn set_kv_updates_existing() {
        let db = test_db();
        db.set_kv("cursor", "a").unwrap();
        db.set_kv("cursor", "b").unwrap();
        assert_eq!(db.get_kv("cursor").unwrap(), Some("b".into()));
    }

    #[test]
    fn cleanup_orphan_extraction_cursors_removes_stale_keys() {
        let db = test_db();
        let session = db.create_session("").unwrap();
        // Cursor + throttle stamp for an existing session: kept.
        db.set_kv(&format!("fact_extraction.{}", session.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", session.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.checkpoint_memory_event_cursor(&session.id, 9).unwrap();
        // Orphan cursor / orphan throttle stamp (no session row) and
        // non-cursor keys: the orphans are removed, unrelated kv keys survive.
        db.set_kv("fact_extraction.gone", "msg-9").unwrap();
        db.set_kv("fact_extraction_last_run.gone", "2026-08-15T00:00:00Z")
            .unwrap();
        db.set_kv("fact_extraction_episode.gone", "msg-10").unwrap();
        db.set_kv("fact_extraction_pending.gone", "1").unwrap();
        db.set_kv("memory_event_cursor.gone", "8").unwrap();
        db.set_kv("memoryXeventYcursor.gone", "keep").unwrap();
        db.set_kv("other.state", "keep").unwrap();

        let removed = db.cleanup_orphan_extraction_cursors().unwrap();
        assert_eq!(removed, 5);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", session.id))
                .unwrap()
                .is_some()
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 9);
        assert!(db.get_kv("fact_extraction.gone").unwrap().is_none());
        assert!(
            db.get_kv("fact_extraction_last_run.gone")
                .unwrap()
                .is_none()
        );
        assert!(db.get_kv("fact_extraction_episode.gone").unwrap().is_none());
        assert!(db.get_kv("fact_extraction_pending.gone").unwrap().is_none());
        assert!(db.get_kv("memory_event_cursor.gone").unwrap().is_none());
        assert_eq!(
            db.get_kv("memoryXeventYcursor.gone").unwrap(),
            Some("keep".into())
        );
        assert_eq!(db.get_kv("other.state").unwrap(), Some("keep".into()));
    }

    #[test]
    fn memory_event_cursor_defaults_is_monotonic_and_session_scoped() {
        let db = test_db();
        let first = db.create_session("first").unwrap();
        let second = db.create_session("second").unwrap();
        db.set_kv(&format!("fact_extraction.{}", first.id), "msg-7")
            .unwrap();

        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 0);
        db.checkpoint_memory_event_cursor(&first.id, 0).unwrap();
        db.checkpoint_memory_event_cursor(&first.id, 4).unwrap();
        db.checkpoint_memory_event_cursor(&first.id, 4).unwrap();
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 4);
        assert_eq!(db.memory_event_cursor(&second.id).unwrap(), 0);
        assert!(db.checkpoint_memory_event_cursor(&first.id, 3).is_err());
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 4);

        db.checkpoint_memory_event_cursor(&second.id, 2).unwrap();
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 4);
        assert_eq!(db.memory_event_cursor(&second.id).unwrap(), 2);
        assert_eq!(
            db.get_kv(&format!("memory_event_cursor.{}", first.id))
                .unwrap(),
            Some("4".into())
        );
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", first.id)).unwrap(),
            Some("msg-7".into())
        );
        assert!(db.checkpoint_memory_event_cursor(&first.id, -1).is_err());

        db.clear_memory_event_cursor(&first.id).unwrap();
        assert_eq!(db.memory_event_cursor(&first.id).unwrap(), 0);
        assert_eq!(db.memory_event_cursor(&second.id).unwrap(), 2);
    }

    #[test]
    fn delete_session_removes_extraction_cursor() {
        let db = test_db();
        let session = db.create_session("t-cursor").unwrap();
        db.set_kv(&format!("fact_extraction.{}", session.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", session.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(&format!("fact_extraction_episode.{}", session.id), "msg-2")
            .unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", session.id), "1")
            .unwrap();
        db.checkpoint_memory_event_cursor(&session.id, 6).unwrap();
        db.delete_session(&session.id).unwrap();
        assert!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_episode.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 0);
    }

    #[test]
    fn pending_extraction_jobs_coalesce_and_restore_live_sessions() {
        let db = test_db();
        let session = db.create_session("t-pending").unwrap();
        db.enqueue_fact_extraction(&session.id, false).unwrap();
        db.enqueue_fact_extraction(&session.id, true).unwrap();
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true)]
        );

        db.clear_pending_fact_extraction(&session.id).unwrap();
        assert!(db.pending_fact_extractions().unwrap().is_empty());
    }
}
