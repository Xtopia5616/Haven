use crate::db::Database;
use chrono::Utc;

/// Internal key-value store for agent bookkeeping that is not user memory.
///
/// User-facing preferences live in the `facts` table (tag `preference`);
/// this table holds only internal state such as the fact-extraction cursor
/// (`fact_extraction.<task_id>`). Exposed as `kv_store` in the schema.
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

    pub fn delete_kv(&self, key: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM kv_store WHERE key = ?1",
            rusqlite::params![key],
        )?;
        Ok(())
    }

    /// Remove fact-extraction cursors whose task no longer exists (task rows
    /// are deleted without going through `delete_task`, e.g. history purge or
    /// older deletions before cursor cleanup was added). Called during memory
    /// maintenance so the kv table does not grow without bound.
    pub fn cleanup_orphan_extraction_cursors(&self) -> anyhow::Result<u64> {
        let conn = self.conn();
        let deleted = conn.execute(
            "DELETE FROM kv_store
             WHERE key LIKE 'fact_extraction.%'
               AND NOT EXISTS (SELECT 1 FROM tasks WHERE id = substr(key, 17))",
            [],
        )?;
        Ok(deleted as u64)
    }
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
    fn delete_kv() {
        let db = test_db();
        db.set_kv("key1", "val1").unwrap();
        db.delete_kv("key1").unwrap();
        assert_eq!(db.get_kv("key1").unwrap(), None);
    }

    #[test]
    fn cleanup_orphan_extraction_cursors_removes_stale_keys() {
        let db = test_db();
        let task = db.create_task("", "").unwrap();
        // Cursor for an existing task: kept.
        db.set_kv(&format!("fact_extraction.{}", task.id), "msg-1")
            .unwrap();
        // Orphan cursor (no task row) and non-cursor keys: the orphan is
        // removed, unrelated kv keys survive.
        db.set_kv("fact_extraction.gone", "msg-9").unwrap();
        db.set_kv("other.state", "keep").unwrap();

        let removed = db.cleanup_orphan_extraction_cursors().unwrap();
        assert_eq!(removed, 1);
        assert!(
            db.get_kv(&format!("fact_extraction.{}", task.id))
                .unwrap()
                .is_some()
        );
        assert!(db.get_kv("fact_extraction.gone").unwrap().is_none());
        assert_eq!(db.get_kv("other.state").unwrap(), Some("keep".into()));
    }

    #[test]
    fn delete_task_removes_extraction_cursor() {
        let db = test_db();
        let task = db.create_task("t-cursor", "").unwrap();
        db.set_kv(&format!("fact_extraction.{}", task.id), "msg-1")
            .unwrap();
        db.delete_task(&task.id).unwrap();
        assert!(
            db.get_kv(&format!("fact_extraction.{}", task.id))
                .unwrap()
                .is_none()
        );
    }
}
