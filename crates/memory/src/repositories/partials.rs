use crate::db::Database;
use chrono::Utc;

/// Scratch storage for in-flight streamed text. While an LLM response is
/// streaming, `stream_llm_step` periodically checkpoints the accumulated
/// text here so a crash or user stop does not lose everything the user
/// already saw. The row is consumed (promoted to a real `messages` row or
/// deleted) when:
///
/// - any real message is persisted for the task (`persist_task_message`),
/// - the task is ended (`end_task` promotes it into history),
/// - the app crashed and the task is finalized to `error` at startup,
/// - the task itself is deleted (FK cascade).
///
/// Never read by the canonical conversation flows; the row lives outside
/// `messages`, so no compaction-style filtering is needed.
impl Database {
    /// Upsert the current partial stream text for a task.
    pub fn upsert_partial_message(&self, task_id: &str, content: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO partial_messages (task_id, content, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(task_id) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
            rusqlite::params![task_id, content, now],
        )?;
        Ok(())
    }

    /// Return `(content, updated_at)` of the partial row for a task, if any.
    pub fn get_partial_message(&self, task_id: &str) -> Option<(String, String)> {
        let conn = self.conn();
        conn.query_row(
            "SELECT content, updated_at FROM partial_messages WHERE task_id = ?1",
            rusqlite::params![task_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
    }

    /// Read and remove the partial row for a task. Atomic (single
    /// `DELETE ... RETURNING` statement), so a concurrent writer can never
    /// observe a row that was already taken.
    pub fn take_partial_message(&self, task_id: &str) -> Option<(String, String)> {
        let conn = self.conn();
        conn.query_row(
            "DELETE FROM partial_messages WHERE task_id = ?1 RETURNING content, updated_at",
            rusqlite::params![task_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .ok()
    }

    /// Drop the partial row for a task (superseded by a real message, or the
    /// stream was discarded). No-op when no row exists.
    pub fn delete_partial_message(&self, task_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM partial_messages WHERE task_id = ?1",
            rusqlite::params![task_id],
        )?;
        Ok(())
    }

    /// Atomically promote the partial row into a real assistant message.
    /// Skips (without inserting) when:
    /// - no partial row exists, or it holds only whitespace,
    /// - a real message was persisted AFTER the last checkpoint (the
    ///   stream's text already reached the message stream) — promoting
    ///   then would duplicate it.
    ///
    /// Returns `true` when a message was inserted. Single blocking round
    /// trip; used by task-end promotion and the startup orphan finalizer.
    pub fn promote_partial_message(&self, task_id: &str) -> anyhow::Result<bool> {
        let Some((content, updated_at)) = self.take_partial_message(task_id) else {
            return Ok(false);
        };
        if content.trim().is_empty() {
            return Ok(false);
        }
        if let Some(last) = self.get_last_message_created_at(task_id)
            && last >= updated_at
        {
            return Ok(false);
        }
        self.add_message_full(
            task_id,
            "assistant",
            content.trim(),
            Some("text"),
            None,
            &[],
            false,
        )?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn upsert_get_take_cycle() {
        let db = test_db();
        let task_id = db.create_task("input", "").unwrap().id;
        assert!(db.get_partial_message(&task_id).is_none());
        db.upsert_partial_message(&task_id, "partial one").unwrap();
        db.upsert_partial_message(&task_id, "partial two").unwrap();
        let (content, _) = db
            .get_partial_message(&task_id)
            .expect("partial exists after upsert");
        assert_eq!(content, "partial two");
        let (taken, _) = db.take_partial_message(&task_id).expect("taken");
        assert_eq!(taken, "partial two");
        assert!(db.get_partial_message(&task_id).is_none());
    }

    #[test]
    fn delete_partial_message_clears_row() {
        let db = test_db();
        let task_id = db.create_task("input", "").unwrap().id;
        db.upsert_partial_message(&task_id, "hello").unwrap();
        db.delete_partial_message(&task_id).unwrap();
        assert!(db.get_partial_message(&task_id).is_none());
    }

    #[test]
    fn promote_partial_creates_real_message() {
        let db = test_db();
        let task_id = db.create_task("input", "").unwrap().id;
        db.upsert_partial_message(&task_id, "streamed reply")
            .unwrap();
        assert!(db.promote_partial_message(&task_id).unwrap());
        let msgs = db.get_task_messages(&task_id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, "assistant");
        assert_eq!(msgs[0].content, "streamed reply");
        // The row is consumed: a second promote is a no-op.
        assert!(!db.promote_partial_message(&task_id).unwrap());
    }

    #[test]
    fn promote_partial_skips_empty_and_superseded() {
        let db = test_db();
        let task_id = db.create_task("input", "").unwrap().id;
        // Whitespace-only partial: nothing to promote.
        db.upsert_partial_message(&task_id, "   ").unwrap();
        assert!(!db.promote_partial_message(&task_id).unwrap());
        // A real message written after the last checkpoint supersedes it.
        db.upsert_partial_message(&task_id, "older stream text")
            .unwrap();
        db.add_message_full(
            &task_id,
            "assistant",
            "newer real message",
            Some("text"),
            None,
            &[],
            false,
        )
        .unwrap();
        assert!(!db.promote_partial_message(&task_id).unwrap());
        let msgs = db.get_task_messages(&task_id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "newer real message");
    }
}
