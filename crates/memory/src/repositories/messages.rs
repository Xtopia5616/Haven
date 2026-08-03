use crate::db::Database;
use chrono::Utc;
use uuid::Uuid;

/// A binary attachment on a message (e.g. a user-provided image).
/// `data` holds base64-encoded bytes; `media_type` is the MIME type (e.g. "image/png").
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct MessageAttachment {
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: String,
    pub task_id: String,
    pub role: String,
    pub content: String,
    pub message_type: Option<String>,
    pub created_at: String,
    pub tool_call_id: Option<String>,
    pub is_compacted: bool,
    pub compaction_id: Option<String>,
    pub parent_message_id: Option<String>,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
}

impl Database {
    pub fn add_message(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
    ) -> anyhow::Result<Message> {
        self.add_message_with_window_full(
            task_id,
            role,
            content,
            message_type,
            tool_call_id,
            50,
            &[],
        )
    }

    pub fn add_message_with_attachments(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        attachments: &[MessageAttachment],
    ) -> anyhow::Result<Message> {
        self.add_message_with_window_full(
            task_id,
            role,
            content,
            message_type,
            tool_call_id,
            50,
            attachments,
        )
    }

    pub fn add_message_with_window(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        window_size: usize,
    ) -> anyhow::Result<Message> {
        self.add_message_with_window_full(
            task_id,
            role,
            content,
            message_type,
            tool_call_id,
            window_size,
            &[],
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_message_with_window_full(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        window_size: usize,
        attachments: &[MessageAttachment],
    ) -> anyhow::Result<Message> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        // Wrap INSERT + DELETE in an immediate transaction so concurrent
        // callers cannot interleave (the second DELETE could remove the
        // first caller's just-inserted message).
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            conn.execute(
                "INSERT INTO messages (id, task_id, role, content, message_type, created_at, tool_call_id, is_compacted, attachments)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
                rusqlite::params![
                    id,
                    task_id,
                    role,
                    content,
                    message_type,
                    now,
                    tool_call_id,
                    Self::serialize_attachments(attachments),
                ],
            )?;
            // Sliding window: keep only the last N messages per task.
            // Clamp to i64::MAX so callers can pass a huge value to disable
            // trimming (e.g. when bulk-copying messages during branching).
            let limit: i64 = window_size.min(i64::MAX as usize) as i64;
            conn.execute(
                "DELETE FROM messages WHERE task_id = ?1 AND id NOT IN (
                    SELECT id FROM messages WHERE task_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT ?2
                )",
                rusqlite::params![task_id, limit],
            )?;
            Ok::<(), anyhow::Error>(())
        })();
        if result.is_err() {
            let _ = conn.execute_batch("ROLLBACK");
        } else {
            conn.execute_batch("COMMIT")?;
        }
        result?;
        drop(conn);
        self.cache_invalidate_messages(task_id);
        Ok(Message {
            id,
            task_id: task_id.into(),
            role: role.into(),
            content: content.into(),
            message_type: message_type.map(String::from),
            created_at: now,
            tool_call_id: tool_call_id.map(String::from),
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
            attachments: attachments.to_vec(),
        })
    }

    fn serialize_attachments(attachments: &[MessageAttachment]) -> Option<String> {
        if attachments.is_empty() {
            None
        } else {
            serde_json::to_string(attachments).ok()
        }
    }

    fn parse_attachments(raw: Option<String>) -> Vec<MessageAttachment> {
        match raw {
            Some(s) if !s.is_empty() => serde_json::from_str(&s).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn get_task_messages(&self, task_id: &str) -> anyhow::Result<Vec<Message>> {
        if let Some(cached) = self.cache_get_messages(task_id) {
            return Ok(cached);
        }
        // Capture generation before querying DB so cache_put can detect a
        // concurrent invalidation and skip the stale-overwrite.
        let cache_gen = self.cache_generation(task_id);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, role, content, message_type, created_at, tool_call_id,
                    is_compacted, compaction_id, parent_message_id, attachments
             FROM messages WHERE task_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], |row| {
            Ok(Message {
                id: row.get(0)?,
                task_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                message_type: row.get(4)?,
                created_at: row.get(5)?,
                tool_call_id: row.get(6)?,
                is_compacted: row.get::<_, i32>(7)? != 0,
                compaction_id: row.get(8)?,
                parent_message_id: row.get(9)?,
                attachments: Self::parse_attachments(row.get(10)?),
            })
        })?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        self.cache_put_messages(task_id, msgs.clone(), 30, cache_gen);
        Ok(msgs)
    }

    pub fn get_task_messages_limit(
        &self,
        task_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, task_id, role, content, message_type, created_at, tool_call_id,
                    is_compacted, compaction_id, parent_message_id, attachments
             FROM messages WHERE task_id = ?1 AND (message_type IS NULL OR message_type = 'text')
             ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id, limit], |row| {
            Ok(Message {
                id: row.get(0)?,
                task_id: row.get(1)?,
                role: row.get(2)?,
                content: row.get(3)?,
                message_type: row.get(4)?,
                created_at: row.get(5)?,
                tool_call_id: row.get(6)?,
                is_compacted: row.get::<_, i32>(7)? != 0,
                compaction_id: row.get(8)?,
                parent_message_id: row.get(9)?,
                attachments: Self::parse_attachments(row.get(10)?),
            })
        })?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        msgs.reverse();
        Ok(msgs)
    }

    pub fn delete_task_messages(&self, task_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE task_id = ?1",
            rusqlite::params![task_id],
        )?;
        self.cache_invalidate_messages(task_id);
        Ok(())
    }

    /// Return the `created_at` of the most recent message in a task, or
    /// `None` if the task has no messages. Used by rollback to record the
    /// high-water mark at branch-point creation time.
    pub fn get_last_message_created_at(&self, task_id: &str) -> Option<String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT created_at FROM messages WHERE task_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![task_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Delete every message in a task whose `created_at` is strictly after
    /// the given timestamp. Used by rollback to discard messages persisted
    /// after the branch point.
    pub fn delete_messages_after(&self, task_id: &str, created_at: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE task_id = ?1 AND created_at > ?2",
            rusqlite::params![task_id, created_at],
        )?;
        self.cache_invalidate_messages(task_id);
        Ok(())
    }

    /// Delete every message in a task whose `created_at` is at or after
    /// the given timestamp (inclusive). Used by user-message rollback to also
    /// remove the rolled-back user message itself.
    pub fn delete_messages_from(&self, task_id: &str, created_at: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE task_id = ?1 AND created_at >= ?2",
            rusqlite::params![task_id, created_at],
        )?;
        self.cache_invalidate_messages(task_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn test_task(db: &Database) -> String {
        db.create_task("input", "transcript").unwrap().id
    }

    #[test]
    fn add_and_get_messages() {
        let db = test_db();
        let tid = test_task(&db);
        let msg = db.add_message(&tid, "user", "hello", None, None).unwrap();
        assert!(!msg.is_compacted);
        assert!(msg.compaction_id.is_none());
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn sliding_window_trims_old_messages() {
        let db = test_db();
        let tid = test_task(&db);
        for i in 0..5 {
            db.add_message_with_window(&tid, "user", &format!("msg {}", i), None, None, 3)
                .unwrap();
        }
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].content, "msg 2");
    }

    #[test]
    fn get_task_messages_limit_filters() {
        let db = test_db();
        let tid = test_task(&db);
        db.add_message(&tid, "user", "hello", Some("text"), None)
            .unwrap();
        db.add_message(&tid, "user", "world", None, None).unwrap();
        let msgs = db.get_task_messages_limit(&tid, 1).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "world");
    }

    #[test]
    fn delete_task_messages_clears_all() {
        let db = test_db();
        let tid = test_task(&db);
        db.add_message(&tid, "user", "msg1", None, None).unwrap();
        db.add_message(&tid, "user", "msg2", None, None).unwrap();
        db.delete_task_messages(&tid).unwrap();
        let msgs = db.get_task_messages(&tid).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn add_message_with_tool_call_id() {
        let db = test_db();
        let tid = test_task(&db);
        let msg = db
            .add_message(&tid, "tool", "result", Some("action"), Some("call-1"))
            .unwrap();
        assert_eq!(msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(msg.message_type.as_deref(), Some("action"));
    }

    #[test]
    fn add_message_with_attachments_roundtrip() {
        let db = test_db();
        let tid = test_task(&db);
        let att = MessageAttachment {
            media_type: "image/png".into(),
            data: "aGVsbG8=".into(),
        };
        db.add_message_with_attachments(
            &tid,
            "user",
            "看图",
            Some("text"),
            None,
            std::slice::from_ref(&att),
        )
        .unwrap();
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].attachments, vec![att]);
        assert_eq!(msgs[0].content, "看图");
    }

    #[test]
    fn message_without_attachments_reads_empty_vec() {
        let db = test_db();
        let tid = test_task(&db);
        db.add_message(&tid, "user", "plain", None, None).unwrap();
        let msgs = db.get_task_messages(&tid).unwrap();
        assert!(msgs[0].attachments.is_empty());
    }

    #[test]
    fn message_serde_roundtrip_with_attachments() {
        let msg = Message {
            id: "m1".into(),
            task_id: "t1".into(),
            role: "user".into(),
            content: "看图".into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
            attachments: vec![MessageAttachment {
                media_type: "image/jpeg".into(),
                data: "abc".into(),
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].media_type, "image/jpeg");
    }

    #[test]
    fn get_last_message_created_at_returns_latest() {
        let db = test_db();
        let tid = test_task(&db);
        assert!(db.get_last_message_created_at(&tid).is_none());
        db.add_message(&tid, "user", "first", None, None).unwrap();
        let m2 = db.add_message(&tid, "user", "second", None, None).unwrap();
        let last = db
            .get_last_message_created_at(&tid)
            .expect("some timestamp");
        assert_eq!(last, m2.created_at);
    }

    #[test]
    fn delete_messages_after_keeps_older() {
        let db = test_db();
        let tid = test_task(&db);
        let m1 = db.add_message(&tid, "user", "first", None, None).unwrap();
        let m2 = db
            .add_message(&tid, "assistant", "second", None, None)
            .unwrap();
        db.delete_messages_after(&tid, &m1.created_at).unwrap();
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "first");
        // m2 should be gone
        assert!(!msgs.iter().any(|m| m.id == m2.id));
    }

    #[test]
    fn delete_messages_from_inclusive() {
        let db = test_db();
        let tid = test_task(&db);
        let m1 = db.add_message(&tid, "user", "first", None, None).unwrap();
        let _m2 = db
            .add_message(&tid, "assistant", "second", None, None)
            .unwrap();
        // delete_messages_from deletes inclusively — m1 and m2 both gone
        db.delete_messages_from(&tid, &m1.created_at).unwrap();
        let msgs = db.get_task_messages(&tid).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn messages_cascade_on_task_delete() {
        let db = test_db();
        let tid = test_task(&db);
        db.add_message(&tid, "user", "msg1", None, None).unwrap();
        db.add_message(&tid, "user", "msg2", None, None).unwrap();
        db.delete_task(&tid).unwrap();
        let conn = db.conn();
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
