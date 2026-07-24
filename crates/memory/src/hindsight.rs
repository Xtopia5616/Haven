use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HindsightEntry {
    pub id: String,
    /// Unique key for de-duplication and look-up
    pub key: String,
    /// The factual content stored by the Agent
    pub content: String,
    /// JSON array of tag strings for categorization
    pub tags: Vec<String>,
    /// Session that created this entry
    pub session_id: String,
    pub created_at: String,
}

impl Database {
    /// Store (or overwrite) a hindsight memory entry.
    /// Uses `key` as the unique identifier — inserting a second time with the
    /// same key replaces the existing entry.
    pub fn retain_hindsight(
        &self,
        key: &str,
        content: &str,
        tags: &[&str],
        session_id: &str,
    ) -> anyhow::Result<HindsightEntry> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".into());
        let conn = self.conn();

        // Delete existing entry with same key (upsert-like behavior)
        conn.execute(
            "DELETE FROM hindsight_store WHERE key = ?1",
            rusqlite::params![key],
        )?;

        conn.execute(
            "INSERT INTO hindsight_store (id, key, content, tags, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, key, content, tags_json, session_id, now],
        )?;

        Ok(HindsightEntry {
            id,
            key: key.into(),
            content: content.into(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            session_id: session_id.into(),
            created_at: now,
        })
    }

    /// Search hindsight memories by keyword (matches key, content, tags).
    pub fn recall_hindsight(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<HindsightEntry>> {
        let pattern = format!("%{}%", query);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, key, content, tags, session_id, created_at
             FROM hindsight_store
             WHERE key LIKE ?1 OR content LIKE ?1 OR tags LIKE ?1
             ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern, limit as i64], |row| {
            let tags_str: String = row.get(3)?;
            let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
            Ok(HindsightEntry {
                id: row.get(0)?,
                key: row.get(1)?,
                content: row.get(2)?,
                tags,
                session_id: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    }

    /// Recall entries by exact key match.
    pub fn recall_by_key(&self, key: &str) -> anyhow::Result<Option<HindsightEntry>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, key, content, tags, session_id, created_at
             FROM hindsight_store WHERE key = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![key])?;
        match rows.next()? {
            Some(row) => {
                let tags_str: String = row.get(3)?;
                let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                Ok(Some(HindsightEntry {
                    id: row.get(0)?,
                    key: row.get(1)?,
                    content: row.get(2)?,
                    tags,
                    session_id: row.get(4)?,
                    created_at: row.get(5)?,
                }))
            }
            None => Ok(None),
        }
    }

    /// Delete a hindsight entry by key.
    pub fn forget_hindsight(&self, key: &str) -> anyhow::Result<bool> {
        let conn = self.conn();
        let count = conn.execute(
            "DELETE FROM hindsight_store WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(count > 0)
    }

    /// Build a human-readable summary of all hindsight memories,
    /// grouped by tag, for injection into the system prompt.
    pub fn hindsight_summary(&self, limit: usize) -> anyhow::Result<String> {
        let entries = self.recall_hindsight("", limit)?;
        if entries.is_empty() {
            return Ok(String::new());
        }
        let mut summary = String::from("Known facts about the user:\n");
        for entry in &entries {
            let tags = entry.tags.join(", ");
            summary.push_str(&format!(
                "  - [{}] {}: {}\n",
                tags, entry.key, entry.content
            ));
        }
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn seed_session(db: &Database, session_id: &str) {
        let conn = db.conn();
        let _ = conn.execute(
            "INSERT INTO sessions (id, started_at, status) VALUES (?1, datetime('now'), 'active')",
            rusqlite::params![session_id],
        );
    }

    #[test]
    fn retain_and_recall() {
        let db = test_db();
        seed_session(&db, "session-1");
        db.retain_hindsight("user.name", "Alice", &["identity"], "session-1")
            .unwrap();
        let entries = db.recall_hindsight("Alice", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, "user.name");
        assert_eq!(entries[0].content, "Alice");
    }

    #[test]
    fn retain_overwrites_same_key() {
        let db = test_db();
        seed_session(&db, "s1");
        db.retain_hindsight("pref.lang", "Chinese", &["preference"], "s1")
            .unwrap();
        db.retain_hindsight("pref.lang", "English", &["preference"], "s1")
            .unwrap();
        let entries = db.recall_hindsight("pref.lang", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "English");
    }

    #[test]
    fn recall_by_exact_key() {
        let db = test_db();
        seed_session(&db, "s1");
        db.retain_hindsight("project.path", "/home/user/project", &["workspace"], "s1")
            .unwrap();
        let entry = db.recall_by_key("project.path").unwrap();
        assert!(entry.is_some());
        assert_eq!(entry.unwrap().content, "/home/user/project");
    }

    #[test]
    fn forget_removes_entry() {
        let db = test_db();
        seed_session(&db, "s1");
        db.retain_hindsight("temp", "temporary", &[], "s1").unwrap();
        assert!(db.forget_hindsight("temp").unwrap());
        assert!(db.recall_by_key("temp").unwrap().is_none());
    }

    #[test]
    fn hindsight_summary_builds_text() {
        let db = test_db();
        seed_session(&db, "s1");
        db.retain_hindsight("user.name", "Bob", &["identity"], "s1")
            .unwrap();
        let summary = db.hindsight_summary(10).unwrap();
        assert!(summary.contains("Bob"));
        assert!(summary.contains("user.name"));
    }
}
