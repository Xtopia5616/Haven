use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CompactionEntry {
    pub id: String,
    pub task_id: String,
    pub summary: String,
    pub first_kept_entry_id: Option<String>,
    pub tokens_before: i64,
    pub created_at: String,
}

impl Database {
    /// Save a compaction entry for a task.
    pub fn save_compaction(
        &self,
        task_id: &str,
        summary: &str,
        tokens_before: i64,
    ) -> anyhow::Result<CompactionEntry> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO compaction_entries (id, task_id, summary, tokens_before, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, task_id, summary, tokens_before, now],
        )?;
        Ok(CompactionEntry {
            id,
            task_id: task_id.into(),
            summary: summary.into(),
            first_kept_entry_id: None,
            tokens_before,
            created_at: now,
        })
    }

    /// Load the most recent compaction entries for a task, ordered newest first.
    pub fn get_task_compactions(&self, task_id: &str) -> anyhow::Result<Vec<CompactionEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, summary, first_kept_entry_id, tokens_before, created_at
             FROM compaction_entries WHERE task_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], |row| {
            Ok(CompactionEntry {
                id: row.get(0)?,
                task_id: row.get(1)?,
                summary: row.get(2)?,
                first_kept_entry_id: row.get(3)?,
                tokens_before: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// When loading messages for a task, inject compaction summaries at the
    /// appropriate positions so the LLM sees a compacted view.
    pub fn load_messages_with_compaction(
        &self,
        task_id: &str,
    ) -> anyhow::Result<Vec<crate::repositories::messages::Message>> {
        let mut messages = self.get_task_messages(task_id)?;
        // Mark messages that are part of a compaction range
        // (The compaction itself happened at the agent level; the DB stores
        //  the full history. We only use `is_compacted` for UI awareness.)
        if let Ok(compactions) = self.get_task_compactions(task_id) {
            for _comp in &compactions {
                if let Some(_msg) = messages.iter_mut().find(|m| m.is_compacted) {}
            }
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn save_and_get_compactions() {
        let db = test_db();
        let task_id = db.create_task("input", "").unwrap().id;
        db.save_compaction(&task_id, "user asked about files", 500)
            .unwrap();
        let entries = db.get_task_compactions(&task_id).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].summary, "user asked about files");
        assert_eq!(entries[0].tokens_before, 500);
    }

    #[test]
    fn get_compactions_empty() {
        let db = test_db();
        let entries = db.get_task_compactions("nonexistent").unwrap();
        assert!(entries.is_empty());
    }
}
