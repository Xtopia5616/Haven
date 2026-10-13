use crate::db::Database;
use chrono::Utc;
use haven_common::types::is_canonical_id;
use rusqlite::{Connection, OptionalExtension};

impl Database {
    /// Upsert a memory node by `(kind, label)`. Returns the stable `node-*` id.
    pub fn ensure_node(&self, kind: &str, label: &str) -> anyhow::Result<String> {
        let conn = self.conn();
        ensure_node_on_connection(&conn, kind, label)
    }
}

pub(crate) fn ensure_node_on_connection(
    conn: &Connection,
    kind: &str,
    label: &str,
) -> anyhow::Result<String> {
    let kind = kind.trim();
    let label = label.trim();
    anyhow::ensure!(!kind.is_empty(), "node kind must not be empty");
    anyhow::ensure!(!label.is_empty(), "node label must not be empty");
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM memory_nodes WHERE kind = ?1 AND label = ?2",
            rusqlite::params![kind, label],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        return validate_node_id(existing);
    }
    let node_id = haven_common::types::new_id("node");
    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO memory_nodes (id, kind, label, aliases, created_at, updated_at)
         VALUES (?1, ?2, ?3, '[]', ?4, ?4)
         ON CONFLICT(kind, label) DO NOTHING",
        rusqlite::params![node_id, kind, label, now],
    )?;
    let node_id = conn.query_row(
        "SELECT id FROM memory_nodes WHERE kind = ?1 AND label = ?2",
        rusqlite::params![kind, label],
        |r| r.get::<_, String>(0),
    )?;
    validate_node_id(node_id)
}

fn validate_node_id(node_id: String) -> anyhow::Result<String> {
    anyhow::ensure!(
        is_canonical_id(&node_id, "node"),
        "invalid persisted memory_nodes.id: expected canonical node-* ID"
    );
    Ok(node_id)
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

    #[test]
    fn ensure_node_rejects_noncanonical_persisted_identity() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO memory_nodes (id, kind, label, aliases, created_at, updated_at)
                 VALUES ('node-short', 'concept', 'corrupt', '[]', 'now', 'now')",
                [],
            )
            .unwrap();

        assert!(db.ensure_node("concept", "corrupt").is_err());
    }
}
