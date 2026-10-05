use crate::db::Database;
use chrono::Utc;
use rusqlite::OptionalExtension;

fn encode_fact_extraction_marker(event_sequence: i64, bypass_throttle: bool) -> String {
    if event_sequence == 0 {
        return if bypass_throttle { "1" } else { "0" }.to_owned();
    }
    format!("{event_sequence}:{}", u8::from(bypass_throttle))
}

fn decode_fact_extraction_marker(value: &str) -> anyhow::Result<(i64, bool)> {
    match value {
        "0" => return Ok((0, false)),
        "1" => return Ok((0, true)),
        _ => {}
    }
    let (event_sequence, bypass) = value
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid fact extraction marker"))?;
    let event_sequence = event_sequence.parse::<i64>()?;
    anyhow::ensure!(
        event_sequence > 0,
        "invalid fact extraction marker sequence"
    );
    let bypass = match bypass {
        "0" => false,
        "1" => true,
        _ => anyhow::bail!("invalid fact extraction marker bypass flag"),
    };
    Ok((event_sequence, bypass))
}

/// Internal key-value store for agent bookkeeping that is not user memory.
///
/// User-facing preferences live in the `facts` table (tag `preference`);
/// this table holds only internal state such as the fact-extraction cursor
/// (`fact_extraction.<session_id>`), the durable extraction outbox
/// (`fact_extraction_pending.<session_id>` and
/// `fact_extraction_episode_pending.<session_id>.<episode_id>`), per-episode
/// completion markers (`fact_extraction_episode_done.<session_id>.<episode_id>`),
/// the committed-event cursor (`memory_event_cursor.<session_id>`), and the
/// runtime's scheduled-confirmation execution claim
/// (`scheduled_execution_claim.<action_id>`). Exposed as `kv_store` in the
/// schema.
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
        Ok(self.memory_event_cursor_optional(session_id)?.unwrap_or(0))
    }

    /// Read the memory event cursor while preserving whether its key exists.
    /// An explicit zero is meaningful during startup baseline selection.
    pub fn memory_event_cursor_optional(&self, session_id: &str) -> anyhow::Result<Option<i64>> {
        let key = memory_event_cursor_key(session_id)?;
        let value = self.get_kv(&key)?;
        value
            .as_deref()
            .map(|value| parse_memory_event_cursor(Some(value)))
            .transpose()
    }

    /// Initialize the memory event cursor only when no durable key exists.
    /// Returns true when this call installed `sequence`; an existing zero is
    /// preserved just like any other checkpoint.
    pub fn initialize_memory_event_cursor_if_absent(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(sequence >= 0, "memory event cursor cannot be negative");
        let key = memory_event_cursor_key(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO kv_store (key, value, updated_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![key, sequence.to_string(), now],
        )?;
        if inserted == 0 {
            // Validate an existing value instead of treating a malformed key
            // as a successful no-op.
            let stored: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get(0),
                )
                .optional()?;
            parse_memory_event_cursor(stored.as_deref())?;
        }
        Ok(inserted != 0)
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

    /// Coalesce one committed memory trigger into durable internal state.
    /// The event sequence identifies the generation: while a marker is pending,
    /// replaying the same event preserves its generation and a newer event
    /// cannot be cleared by an older job. If a replay follows an ack but comes
    /// before the event cursor checkpoint, it may recreate that generation;
    /// the message cursor makes the resulting empty extraction safe to ack.
    /// `bypass_throttle` may only be upgraded while the marker is pending.
    pub fn enqueue_fact_extraction(
        &self,
        session_id: &str,
        bypass_throttle: bool,
        event_sequence: i64,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(event_sequence > 0, "event sequence must be positive");
        let key = format!("fact_extraction_pending.{session_id}");
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            let previous_value: Option<String> = conn
                .query_row(
                    "SELECT value FROM kv_store WHERE key = ?1",
                    rusqlite::params![&key],
                    |row| row.get(0),
                )
                .optional()?;
            let (previous_sequence, previous_bypass) = previous_value
                .as_deref()
                .map(decode_fact_extraction_marker)
                .transpose()?
                .unwrap_or((0, false));
            let value = encode_fact_extraction_marker(
                previous_sequence.max(event_sequence),
                previous_bypass || bypass_throttle,
            );
            let now = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO kv_store (key, value, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                     value = excluded.value,
                     updated_at = excluded.updated_at",
                rusqlite::params![key, value, now],
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
        conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Load durable extraction jobs that still belong to a live session.
    /// Orphaned markers are left for the shared cleanup pass, so this read
    /// never turns a deleted session into a new unit of work.
    pub fn pending_fact_extractions(&self) -> anyhow::Result<Vec<(String, bool, i64)>> {
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
            let marker: String = row.get(1)?;
            Ok((session_id, marker))
        })?;
        rows.map(|row| {
            let (session_id, marker) = row?;
            let (event_sequence, bypass) = decode_fact_extraction_marker(&marker)?;
            Ok((session_id, bypass, event_sequence))
        })
        .collect()
    }

    /// Acknowledge only the durable marker generation captured by this job.
    /// A newer committed trigger remains pending even when its bypass flag is
    /// identical to the in-flight job's flag.
    pub fn clear_pending_fact_extraction_if_current(
        &self,
        session_id: &str,
        event_sequence: i64,
        bypass_throttle: bool,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(event_sequence >= 0, "event sequence must not be negative");
        let expected_value = encode_fact_extraction_marker(event_sequence, bypass_throttle);
        let changed = self.conn().execute(
            "DELETE FROM kv_store
             WHERE key = ?1 AND value = ?2",
            rusqlite::params![
                format!("fact_extraction_pending.{session_id}"),
                expected_value
            ],
        )?;
        Ok(changed > 0)
    }

    /// Load durable compaction-summary extraction jobs. The episode id is
    /// encoded in the key while the value keeps session cleanup inexpensive.
    pub fn pending_summary_extractions(&self) -> anyhow::Result<Vec<(String, String)>> {
        let prefix = "fact_extraction_episode_pending.";
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT key, value FROM kv_store
             WHERE key LIKE 'fact_extraction_episode_pending.%'
             ORDER BY updated_at, key",
        )?;
        let rows = stmt.query_map([], |row| {
            let key: String = row.get(0)?;
            let session_id: String = row.get(1)?;
            Ok((key, session_id))
        })?;
        let mut pending = Vec::new();
        for row in rows {
            let (key, session_id) = row?;
            let Some(suffix) = key.strip_prefix(prefix) else {
                continue;
            };
            let Some((key_session_id, episode_id)) = suffix.split_once('.') else {
                continue;
            };
            if key_session_id == session_id && !episode_id.is_empty() {
                pending.push((session_id, episode_id.to_owned()));
            }
        }
        Ok(pending)
    }

    /// Acknowledge one completed compaction-summary extraction.
    pub fn clear_summary_extraction(
        &self,
        session_id: &str,
        episode_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(!episode_id.trim().is_empty(), "episode id is required");
        self.conn().execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![format!(
                "fact_extraction_episode_pending.{session_id}.{episode_id}"
            )],
        )?;
        Ok(())
    }

    /// Remove session-scoped internal cursors whose session no longer exists
    /// (session rows are deleted without going through `delete_session`, e.g.
    /// history purge or older deletions before cursor cleanup was added). This
    /// also purges extraction throttle stamps, per-episode completion
    /// markers, pending markers, and
    /// `memory_event_cursor.<session_id>` checkpoints of dead sessions.
    /// Called during memory maintenance so the kv table does not grow without
    /// bound. Session retention uses the same connection-level cleanup rule.
    pub fn cleanup_orphan_extraction_cursors(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        cleanup_orphan_session_scoped_state_on(&conn)
    }
}

/// Delete session-scoped extraction and event-consumer markers whose owning
/// session no longer exists. Callers that already hold a connection can reuse
/// this predicate without checking out another pooled connection.
pub(super) fn cleanup_orphan_session_scoped_state_on(
    conn: &rusqlite::Connection,
) -> anyhow::Result<u64> {
    let deleted = conn.execute(
        "DELETE FROM kv_store
         WHERE (key LIKE 'fact_extraction.%'
                OR key LIKE 'fact_extraction_last_run.%'
                OR key LIKE 'fact_extraction_episode_done.%'
                OR key LIKE 'fact_extraction_pending.%'
                OR key LIKE 'fact_extraction_episode_pending.%'
                OR key GLOB 'memory_event_cursor.*')
           AND NOT EXISTS (SELECT 1 FROM sessions
                           WHERE id = CASE
                               WHEN key GLOB 'memory_event_cursor.*'
                               THEN substr(key, 21)
                               WHEN key LIKE 'fact_extraction_last_run.%'
                               THEN substr(key, 26)
                               WHEN key LIKE 'fact_extraction_pending.%'
                               THEN substr(key, 25)
                               WHEN key LIKE 'fact_extraction_episode_pending.%'
                               THEN value
                               WHEN key LIKE 'fact_extraction_episode_done.%'
                               THEN value
                               ELSE substr(key, 17)
                           END)",
        [],
    )?;
    Ok(deleted as u64)
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
    use haven_common::types::new_id;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn memory_event_cursor_optional_distinguishes_missing_from_zero() {
        let db = test_db();
        let session = db.create_session("cursor").unwrap();

        assert_eq!(db.memory_event_cursor_optional(&session.id).unwrap(), None);
        assert!(
            db.initialize_memory_event_cursor_if_absent(&session.id, 0)
                .unwrap()
        );
        assert_eq!(
            db.memory_event_cursor_optional(&session.id).unwrap(),
            Some(0)
        );
        assert!(
            !db.initialize_memory_event_cursor_if_absent(&session.id, 8)
                .unwrap()
        );
        assert_eq!(
            db.memory_event_cursor_optional(&session.id).unwrap(),
            Some(0)
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 0);
    }

    #[test]
    fn memory_event_cursor_optional_rejects_invalid_stored_values() {
        let db = test_db();
        let session = db.create_session("cursor").unwrap();
        db.set_kv(&format!("memory_event_cursor.{}", session.id), "bad")
            .unwrap();

        assert!(db.memory_event_cursor_optional(&session.id).is_err());
        assert!(
            db.initialize_memory_event_cursor_if_absent(&session.id, 1)
                .is_err()
        );
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
    fn summary_extraction_jobs_keep_each_episode_and_ack_independently() {
        let db = test_db();
        let session = db.create_session("summary jobs").unwrap();
        let first_episode_id = new_id("msg");
        let second_episode_id = new_id("msg");
        db.add_episode_with_pending_extraction(
            &session.id,
            "The first durable summary contains enough extraction context.",
            &first_episode_id,
            true,
        )
        .unwrap();
        db.add_episode_with_pending_extraction(
            &session.id,
            "The second durable summary contains enough extraction context.",
            &second_episode_id,
            true,
        )
        .unwrap();

        let mut pending = db.pending_summary_extractions().unwrap();
        pending.sort();
        let mut expected = vec![
            (session.id.clone(), first_episode_id.clone()),
            (session.id.clone(), second_episode_id.clone()),
        ];
        expected.sort();
        assert_eq!(pending, expected);
        db.clear_summary_extraction(&session.id, &first_episode_id)
            .unwrap();
        assert_eq!(
            db.pending_summary_extractions().unwrap(),
            vec![(session.id, second_episode_id)]
        );
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
        db.set_kv("fact_extraction_episode_done.gone.msg-11", "gone")
            .unwrap();
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
        assert!(
            db.get_kv("fact_extraction_episode_done.gone.msg-11")
                .unwrap()
                .is_none()
        );
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
        let episode_id = new_id("msg");
        db.set_kv(&format!("fact_extraction.{}", session.id), "msg-1")
            .unwrap();
        db.set_kv(
            &format!("fact_extraction_last_run.{}", session.id),
            "2026-08-15T00:00:00Z",
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_episode_done.{}.{}", session.id, episode_id),
            &session.id,
        )
        .unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", session.id), "1")
            .unwrap();
        db.add_episode_with_pending_extraction(
            &session.id,
            "A durable summary with an extraction marker for session cleanup.",
            &episode_id,
            true,
        )
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
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .is_none()
        );
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", session.id))
                .unwrap()
                .is_none()
        );
        assert!(
            db.get_kv(&format!(
                "fact_extraction_episode_pending.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .is_none()
        );
        assert_eq!(db.memory_event_cursor(&session.id).unwrap(), 0);
    }

    #[test]
    fn pending_extraction_markers_keep_event_generation_and_bypass() {
        let db = test_db();
        let session = db.create_session("t-pending").unwrap();
        db.enqueue_fact_extraction(&session.id, false, 1).unwrap();
        db.enqueue_fact_extraction(&session.id, true, 2).unwrap();
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 2)]
        );

        assert!(
            !db.clear_pending_fact_extraction_if_current(&session.id, 1, false)
                .unwrap()
        );
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 2)]
        );
        assert!(
            db.clear_pending_fact_extraction_if_current(&session.id, 2, true)
                .unwrap()
        );
        assert!(db.pending_fact_extractions().unwrap().is_empty());
    }

    #[test]
    fn same_value_retrigger_survives_an_older_job_ack() {
        let db = test_db();
        let session = db.create_session("t-pending-retrigger").unwrap();
        db.enqueue_fact_extraction(&session.id, true, 4).unwrap();
        db.enqueue_fact_extraction(&session.id, true, 5).unwrap();

        assert!(
            !db.clear_pending_fact_extraction_if_current(&session.id, 4, true)
                .unwrap()
        );
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 5)]
        );

        assert!(
            db.clear_pending_fact_extraction_if_current(&session.id, 5, true)
                .unwrap()
        );
        assert!(db.pending_fact_extractions().unwrap().is_empty());
    }

    #[test]
    fn legacy_boolean_fact_marker_is_readable_and_upgraded_on_enqueue() {
        let db = test_db();
        let session = db.create_session("t-legacy-pending").unwrap();
        db.set_kv(&format!("fact_extraction_pending.{}", session.id), "1")
            .unwrap();

        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 0)]
        );
        db.enqueue_fact_extraction(&session.id, false, 8).unwrap();
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 8)]
        );
        assert!(
            !db.clear_pending_fact_extraction_if_current(&session.id, 0, true)
                .unwrap()
        );
        assert!(
            db.clear_pending_fact_extraction_if_current(&session.id, 8, true)
                .unwrap()
        );
    }
}
