use crate::db::Database;
use chrono::Utc;

/// A binary attachment on a message (e.g. a user-provided image or file).
/// `data` holds base64-encoded bytes; `media_type` is the MIME type (e.g. "image/png").
/// Non-image attachments (user-uploaded files) additionally carry `filename`
/// (the original name) and `path` (absolute path on disk, set after the
/// backend persists the bytes so the agent can read them with the file tool).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct MessageAttachment {
    pub media_type: String,
    pub data: String,
    /// Original file name for non-image attachments (e.g. "report.pdf").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Absolute path where a non-image attachment was persisted on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl MessageAttachment {
    /// Create a binary attachment without disk metadata (used for images and
    /// tests). `filename`/`path` are left empty and skipped in serialization.
    pub fn new(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            media_type: media_type.into(),
            data: data.into(),
            filename: None,
            path: None,
        }
    }

    /// True for vision-capable attachments (images), which are injected into
    /// the model context as image content parts. Everything else is a file
    /// attachment the agent reads from `path` via the file tool.
    pub fn is_image(&self) -> bool {
        self.media_type.starts_with("image/")
    }
}

/// Map a `messages` row (9 columns: id, task_id, role, content, message_type,
/// created_at, tool_call_id, attachments, voice) into a `Message`. Shared by
/// every read query so column order cannot drift between them.
fn map_message_row(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        task_id: row.get(1)?,
        role: row.get(2)?,
        content: row.get(3)?,
        message_type: row.get(4)?,
        created_at: row.get(5)?,
        tool_call_id: row.get(6)?,
        attachments: Database::parse_attachments(row.get(7)?),
        voice: row.get::<_, i32>(8)? != 0,
    })
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
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    /// True for user messages that came from voice transcription (mic style
    /// in the UI survives reloads). Assistant/tool messages are always false.
    #[serde(default)]
    pub voice: bool,
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
        self.add_message_full(
            task_id,
            role,
            content,
            message_type,
            tool_call_id,
            &[],
            false,
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
        self.add_message_full(
            task_id,
            role,
            content,
            message_type,
            tool_call_id,
            attachments,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_message_full(
        &self,
        task_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        attachments: &[MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<Message> {
        let id = haven_common::types::new_id("msg");
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (id, task_id, role, content, message_type, created_at, tool_call_id, attachments, voice)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id,
                task_id,
                role,
                content,
                message_type,
                now,
                tool_call_id,
                Self::serialize_attachments(attachments),
                voice,
            ],
        )?;
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
            attachments: attachments.to_vec(),
            voice,
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
                    attachments, voice
             FROM messages WHERE task_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], map_message_row)?;
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
                    attachments, voice
             FROM messages WHERE task_id = ?1 AND (message_type IS NULL OR message_type = 'text')
             ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id, limit], map_message_row)?;
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

    /// Delete a single message by its primary key. Used to remove a user
    /// message that was persisted before the backend discovered the task is
    /// terminal (no ghost rows in history).
    pub fn delete_message_by_id(&self, task_id: &str, message_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE id = ?1 AND task_id = ?2",
            rusqlite::params![message_id, task_id],
        )?;
        self.cache_invalidate_messages(task_id);
        Ok(())
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

    /// `created_at` of the most recent user-role message for a task, or
    /// `None` if the task has no user messages yet. Implemented in SQL so
    /// rollback does not have to load the entire message list just to find
    /// the trailing user-input timestamp.
    pub fn last_user_message_ts(&self, task_id: &str) -> Option<String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT created_at FROM messages
             WHERE task_id = ?1 AND role = 'user'
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![task_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Drop every message **and** task-step whose `created_at` is strictly
    /// after `ts`, or at-or-after `ts` when `inclusive`. Centralizes the
    /// `delete_messages_after/from + delete_task_steps_after` pair that
    /// rollback used to repeat at every branch-point cutoff, so a future
    /// step-row source (e.g. per-task tool tables) only needs one edit.
    pub fn truncate_task_after(
        &self,
        task_id: &str,
        ts: &str,
        inclusive: bool,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        let op = if inclusive { ">=" } else { ">" };
        let msgs_sql = format!("DELETE FROM messages WHERE task_id = ?1 AND created_at {op} ?2");
        let steps_sql = format!("DELETE FROM task_steps WHERE task_id = ?1 AND created_at {op} ?2");
        conn.execute(&msgs_sql, rusqlite::params![task_id, ts])?;
        conn.execute(&steps_sql, rusqlite::params![task_id, ts])?;
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
        assert_eq!(msg.content, "hello");
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn no_sliding_window_trim_keeps_full_history() {
        let db = test_db();
        let tid = test_task(&db);
        for i in 0..5 {
            db.add_message(&tid, "user", &format!("msg {}", i), None, None)
                .unwrap();
        }
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0].content, "msg 0");
        assert_eq!(msgs[4].content, "msg 4");
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
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
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
            attachments: vec![MessageAttachment::new("image/jpeg", "abc")],
            voice: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.attachments.len(), 1);
        assert_eq!(decoded.attachments[0].media_type, "image/jpeg");
        assert!(decoded.voice);
    }

    #[test]
    fn voice_flag_persists_and_roundtrips() {
        let db = test_db();
        let tid = test_task(&db);
        db.add_message_full(&tid, "user", "voice hello", Some("text"), None, &[], true)
            .unwrap();
        db.add_message(&tid, "user", "typed hello", Some("text"), None)
            .unwrap();
        let msgs = db.get_task_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].voice, "voice message must keep the flag");
        assert!(!msgs[1].voice, "typed message stays non-voice");
        // Serde default keeps old JSON payloads (pre-voice) decodable.
        let legacy = r#"{"id":"x","task_id":"t","role":"user","content":"c","message_type":"text","created_at":"2026-01-01T00:00:00Z","tool_call_id":null,"attachments":[]}"#;
        let decoded: Message = serde_json::from_str(legacy).unwrap();
        assert!(!decoded.voice);
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
