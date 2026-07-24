use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub message_type: Option<String>,
    pub created_at: String,
    pub tool_call_id: Option<String>,
    pub is_compacted: bool,
    pub compaction_id: Option<String>,
}

impl Database {
    pub fn add_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
    ) -> anyhow::Result<Message> {
        self.add_message_with_window(session_id, role, content, message_type, tool_call_id, 50)
    }

    pub fn add_message_with_window(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        window_size: usize,
    ) -> anyhow::Result<Message> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, message_type, created_at, tool_call_id, is_compacted)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
            rusqlite::params![id, session_id, role, content, message_type, now, tool_call_id],
        )?;
        // Sliding window: keep only the last N messages per session
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND id NOT IN (
                SELECT id FROM messages WHERE session_id = ?1 ORDER BY created_at DESC LIMIT ?2
            )",
            rusqlite::params![session_id, window_size],
        )?;
        self.cache_invalidate_messages(session_id);
        Ok(Message {
            id,
            session_id: session_id.into(),
            role: role.into(),
            content: content.into(),
            message_type: message_type.map(String::from),
            created_at: now,
            tool_call_id: tool_call_id.map(String::from),
            is_compacted: false,
            compaction_id: None,
        })
    }

    pub fn get_session_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        if let Some(cached) = self.cache_get_messages(session_id) {
            return Ok(cached);
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    is_compacted, compaction_id
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(Message {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                message_type: row.get(4)?,
                created_at: row.get(5)?,
                tool_call_id: row.get(6)?,
                is_compacted: row.get::<_, i32>(7)? != 0,
                compaction_id: row.get(8)?,
            })
        })?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        self.cache_put_messages(session_id, msgs.clone(), 30);
        Ok(msgs)
    }

    pub fn get_session_messages_limit(
        &self,
        session_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    is_compacted, compaction_id
             FROM messages WHERE session_id = ?1 AND (message_type IS NULL OR message_type = 'text')
             ORDER BY created_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, limit], |row| {
            Ok(Message {
                id: row.get(0)?,
                session_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                message_type: row.get(4)?,
                created_at: row.get(5)?,
                tool_call_id: row.get(6)?,
                is_compacted: row.get::<_, i32>(7)? != 0,
                compaction_id: row.get(8)?,
            })
        })?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        msgs.reverse();
        Ok(msgs)
    }

    pub fn delete_session_messages(&self, session_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        self.cache_invalidate_messages(session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn test_session(db: &Database) -> String {
        db.create_session(None).unwrap().id
    }

    #[test]
    fn add_and_get_messages() {
        let db = test_db();
        let sid = test_session(&db);
        let msg = db.add_message(&sid, "user", "hello", None, None).unwrap();
        assert!(!msg.is_compacted);
        assert!(msg.compaction_id.is_none());
        let msgs = db.get_session_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn sliding_window_trims_old_messages() {
        let db = test_db();
        let sid = test_session(&db);
        for i in 0..5 {
            db.add_message_with_window(&sid, "user", &format!("msg {}", i), None, None, 3)
                .unwrap();
        }
        let msgs = db.get_session_messages(&sid).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg 2");
    }

    #[test]
    fn get_session_messages_limit_filters() {
        let db = test_db();
        let sid = test_session(&db);
        db.add_message(&sid, "user", "hello", Some("text"), None).unwrap();
        db.add_message(&sid, "user", "world", None, None).unwrap();
        let msgs = db.get_session_messages_limit(&sid, 1).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "world");
    }

    #[test]
    fn delete_session_messages_clears_all() {
        let db = test_db();
        let sid = test_session(&db);
        db.add_message(&sid, "user", "msg1", None, None).unwrap();
        db.add_message(&sid, "user", "msg2", None, None).unwrap();
        db.delete_session_messages(&sid).unwrap();
        let msgs = db.get_session_messages(&sid).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn add_message_with_tool_call_id() {
        let db = test_db();
        let sid = test_session(&db);
        let msg = db.add_message(&sid, "tool", "result", Some("action"), Some("call-1")).unwrap();
        assert_eq!(msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(msg.message_type.as_deref(), Some("action"));
    }
}

