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

    /// Read and remove the partial row for a task. Used by `end_task` and the
    /// startup orphan finalizer to promote streamed text into real messages.
    pub fn take_partial_message(&self, task_id: &str) -> Option<String> {
        let conn = self.conn();
        let taken = conn
            .query_row(
                "SELECT content FROM partial_messages WHERE task_id = ?1",
                rusqlite::params![task_id],
                |row| row.get::<_, String>(0),
            )
            .ok();
        if taken.is_some() {
            let _ = conn.execute(
                "DELETE FROM partial_messages WHERE task_id = ?1",
                rusqlite::params![task_id],
            );
        }
        taken
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
        let taken = db.take_partial_message(&task_id).expect("taken");
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
}
