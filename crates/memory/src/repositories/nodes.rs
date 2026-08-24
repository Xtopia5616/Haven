use crate::db::Database;
use chrono::Utc;

impl Database {
    /// Upsert a memory node by `(kind, label)`. Returns the stable `node-*` id.
    pub fn ensure_node(&self, kind: &str, label: &str) -> anyhow::Result<String> {
        let kind = kind.trim();
        let label = label.trim();
        anyhow::ensure!(!kind.is_empty(), "node kind must not be empty");
        anyhow::ensure!(!label.is_empty(), "node label must not be empty");
        let conn = self.conn();
        if let Ok(existing) = conn.query_row(
            "SELECT id FROM memory_nodes WHERE kind = ?1 AND label = ?2",
            rusqlite::params![kind, label],
            |r| r.get::<_, String>(0),
        ) {
            return Ok(existing);
        }
        let id = haven_common::types::new_id("node");
        let now = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO memory_nodes (id, kind, label, aliases, created_at, updated_at)
             VALUES (?1, ?2, ?3, '[]', ?4, ?4)
             ON CONFLICT(kind, label) DO NOTHING",
            rusqlite::params![id, kind, label, now],
        )?;
        let id = conn.query_row(
            "SELECT id FROM memory_nodes WHERE kind = ?1 AND label = ?2",
            rusqlite::params![kind, label],
            |r| r.get::<_, String>(0),
        )?;
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use crate::Database;

    #[test]
    fn ensure_node_upserts_by_kind_label() {
        let db = Database::open_in_memory().unwrap();
        let a = db.ensure_node("user", "user").unwrap();
        let b = db.ensure_node("user", "user").unwrap();
        assert_eq!(a, b);
        assert!(a.starts_with("node-"));
        let c = db.ensure_node("concept", "Rust").unwrap();
        assert_ne!(a, c);
    }
}
