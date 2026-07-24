use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    /// Parent session ID for branching / session tree (§3.5)
    pub parent_id: Option<String>,
}

impl Database {
    pub fn get_or_create_active_session(&self) -> anyhow::Result<Session> {
        let existing = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT id, started_at, ended_at, status, parent_id FROM sessions WHERE status = 'active' LIMIT 1",
            )?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                Some(Session {
                    id: row.get(0)?,
                    started_at: row.get(1)?,
                    ended_at: row.get(2)?,
                    status: row.get(3)?,
                    parent_id: row.get(4)?,
                })
            } else {
                None
            }
        };
        match existing {
            Some(session) => Ok(session),
            None => self.create_session(None),
        }
    }

    pub fn close_active_session(&self) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET ended_at = ?1, status = 'closed' WHERE status = 'active'",
            rusqlite::params![now],
        )?;
        Ok(())
    }

    /// Create a new session, optionally with a parent_id for branching.
    pub fn create_session(&self, parent_id: Option<&str>) -> anyhow::Result<Session> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO sessions (id, started_at, status, parent_id) VALUES (?1, ?2, 'active', ?3)",
            rusqlite::params![id, now, parent_id],
        )?;
        Ok(Session {
            id,
            started_at: now,
            ended_at: None,
            status: "active".into(),
            parent_id: parent_id.map(String::from),
        })
    }

    /// Build the ancestor chain from a session up to the root (session tree traversal).
    /// Returns sessions in leaf→root order.
    pub fn build_session_context(&self, session_id: &str) -> anyhow::Result<Vec<Session>> {
        let mut chain = Vec::new();
        let mut current_id = Some(session_id.to_string());
        while let Some(ref cid) = current_id {
            if let Some(session) = self.get_session(cid)? {
                current_id = session.parent_id.clone();
                chain.push(session);
            } else {
                break;
            }
        }
        Ok(chain)
    }

    /// Branch: close the current active session and create a new child session.
    pub fn branch_session(&self, parent_id: &str) -> anyhow::Result<Session> {
        let _ = self.close_active_session();
        self.create_session(Some(parent_id))
    }

    pub fn get_session(&self, id: &str) -> anyhow::Result<Option<Session>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, started_at, ended_at, status, parent_id FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Session {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                status: row.get(3)?,
                parent_id: row.get(4)?,
            })),
            None => Ok(None),
        }
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<Session>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, started_at, ended_at, status, parent_id FROM sessions ORDER BY started_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Session {
                id: row.get(0)?,
                started_at: row.get(1)?,
                ended_at: row.get(2)?,
                status: row.get(3)?,
                parent_id: row.get(4)?,
            })
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    pub fn close_session(&self, id: &str) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "UPDATE sessions SET ended_at = ?1, status = 'closed' WHERE id = ?2",
            rusqlite::params![now, id],
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
    fn create_branch_and_traverse() {
        let db = test_db();
        let root = db.create_session(None).unwrap();
        assert!(root.parent_id.is_none());
        let child = db.branch_session(&root.id).unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(root.id.as_str()));
        let chain = db.build_session_context(&child.id).unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].id, child.id);
        assert_eq!(chain[1].id, root.id);
    }

    #[test]
    fn get_or_create_creates_when_missing() {
        let db = test_db();
        let session = db.get_or_create_active_session().unwrap();
        assert_eq!(session.status, "active");
        assert!(session.parent_id.is_none());
    }

    #[test]
    fn get_or_create_returns_existing() {
        let db = test_db();
        let first = db.get_or_create_active_session().unwrap();
        let second = db.get_or_create_active_session().unwrap();
        assert_eq!(first.id, second.id);
    }

    #[test]
    fn close_active_session_updates_status() {
        let db = test_db();
        let session = db.get_or_create_active_session().unwrap();
        db.close_active_session().unwrap();
        let closed = db.get_session(&session.id).unwrap().unwrap();
        assert_eq!(closed.status, "closed");
        assert!(closed.ended_at.is_some());
    }

    #[test]
    fn create_session_with_parent() {
        let db = test_db();
        let parent = db.create_session(None).unwrap();
        let child = db.create_session(Some(&parent.id)).unwrap();
        assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
    }

    #[test]
    fn get_session_not_found_returns_none() {
        let db = test_db();
        let result = db.get_session("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_sessions_returns_all() {
        let db = test_db();
        let s1 = db.create_session(None).unwrap();
        let s2 = db.create_session(None).unwrap();
        let sessions = db.list_sessions().unwrap();
        assert!(sessions.len() >= 2);
        assert!(sessions.iter().any(|s| s.id == s1.id));
        assert!(sessions.iter().any(|s| s.id == s2.id));
    }

    #[test]
    fn close_session_by_id() {
        let db = test_db();
        let session = db.create_session(None).unwrap();
        db.close_session(&session.id).unwrap();
        let closed = db.get_session(&session.id).unwrap().unwrap();
        assert_eq!(closed.status, "closed");
    }
}
