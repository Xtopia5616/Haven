use crate::db::Database;
use chrono::Utc;
use haven_common::types::MessageAttachment;

/// Map a `messages` row (9 columns: id, session_id, role, content, message_type,
/// created_at, tool_call_id, attachments, voice) into a `Message`. Shared by
/// every read query so column order cannot drift between them.
fn map_message_row(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
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
    pub session_id: String,
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
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
    ) -> anyhow::Result<Message> {
        self.add_message_full(
            session_id,
            role,
            content,
            message_type,
            tool_call_id,
            &[],
            false,
            None,
        )
    }

    pub fn add_message_with_attachments(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        attachments: &[MessageAttachment],
    ) -> anyhow::Result<Message> {
        self.add_message_full(
            session_id,
            role,
            content,
            message_type,
            tool_call_id,
            attachments,
            false,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_message_full(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        attachments: &[MessageAttachment],
        voice: bool,
        // When `Some`, insert the row under this pre-minted id (streamed
        // block ids minted at stream start); `None` mints a fresh `msg-*`.
        id: Option<&str>,
    ) -> anyhow::Result<Message> {
        let id = id
            .map(String::from)
            .unwrap_or_else(|| haven_common::types::new_id("msg"));
        let now = Utc::now().to_rfc3339();
        let conn = self.conn();
        conn.execute(
            "INSERT INTO messages (id, session_id, role, content, message_type, created_at, tool_call_id, attachments, voice)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                id,
                session_id,
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
        self.cache_invalidate_messages(session_id);
        Ok(Message {
            id,
            session_id: session_id.into(),
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

    pub fn get_session_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        if let Some(cached) = self.cache_get_messages(session_id) {
            return Ok(cached);
        }
        // Capture generation before querying DB so cache_put can detect a
        // concurrent invalidation and skip the stale-overwrite.
        let cache_gen = self.cache_generation(session_id);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    attachments, voice
             FROM messages WHERE session_id = ?1 ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], map_message_row)?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        self.cache_put_messages(session_id, msgs.clone(), 30, cache_gen);
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
                    attachments, voice
             FROM messages WHERE session_id = ?1 AND (message_type IS NULL OR message_type = 'text')
             ORDER BY created_at DESC, rowid DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, limit], map_message_row)?;
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

    /// Return the `created_at` of the most recent message in a session, or
    /// `None` if the session has no messages. Used by rollback to record the
    /// high-water mark at branch-point creation time.
    pub fn get_last_message_created_at(&self, session_id: &str) -> Option<String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT created_at FROM messages WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![session_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Delete a single message by its primary key. Used to remove a user
    /// message that was persisted before the backend discovered the session is
    /// terminal (no ghost rows in history).
    pub fn delete_message_by_id(&self, session_id: &str, message_id: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE id = ?1 AND session_id = ?2",
            rusqlite::params![message_id, session_id],
        )?;
        self.cache_invalidate_messages(session_id);
        Ok(())
    }

    /// Delete every message in a session whose `created_at` is strictly after
    /// the given timestamp. Used by rollback to discard messages persisted
    /// after the branch point.
    pub fn delete_messages_after(&self, session_id: &str, created_at: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND created_at > ?2",
            rusqlite::params![session_id, created_at],
        )?;
        self.cache_invalidate_messages(session_id);
        Ok(())
    }

    /// Delete every message in a session whose `created_at` is at or after
    /// the given timestamp (inclusive). Used by user-message rollback to also
    /// remove the rolled-back user message itself.
    pub fn delete_messages_from(&self, session_id: &str, created_at: &str) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1 AND created_at >= ?2",
            rusqlite::params![session_id, created_at],
        )?;
        self.cache_invalidate_messages(session_id);
        Ok(())
    }

    /// `created_at` of the most recent user-role message for a session, or
    /// `None` if the session has no user messages yet. Implemented in SQL so
    /// rollback does not have to load the entire message list just to find
    /// the trailing user-input timestamp.
    pub fn last_user_message_ts(&self, session_id: &str) -> Option<String> {
        let conn = self.conn();
        conn.query_row(
            "SELECT created_at FROM messages
             WHERE session_id = ?1 AND role = 'user'
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
            rusqlite::params![session_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    /// Drop every message **and** ses-step whose `created_at` is strictly
    /// after `ts`, or at-or-after `ts` when `inclusive`. Centralizes the
    /// `delete_messages_after/from + delete_session_steps_after` pair that
    /// rollback used to repeat at every branch-point cutoff, so a future
    /// step-row source (e.g. per-session tool tables) only needs one edit.
    /// `llm_usage` detail rows are cut on the same timeline (their
    /// `created_at` is RFC3339 like messages), so discarded steps leave no
    /// orphaned usage history behind.
    pub fn truncate_session_after(
        &self,
        session_id: &str,
        ts: &str,
        inclusive: bool,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        let op = if inclusive { ">=" } else { ">" };
        let msgs_sql = format!("DELETE FROM messages WHERE session_id = ?1 AND created_at {op} ?2");
        let steps_sql =
            format!("DELETE FROM session_steps WHERE session_id = ?1 AND created_at {op} ?2");
        let usage_sql =
            format!("DELETE FROM llm_usage WHERE session_id = ?1 AND created_at {op} ?2");
        conn.execute(&msgs_sql, rusqlite::params![session_id, ts])?;
        conn.execute(&steps_sql, rusqlite::params![session_id, ts])?;
        conn.execute(&usage_sql, rusqlite::params![session_id, ts])?;
        self.cache_invalidate_messages(session_id);
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

    fn test_session(db: &Database) -> String {
        db.create_session("input", "transcript").unwrap().id
    }

    #[test]
    fn add_and_get_messages() {
        let db = test_db();
        let tid = test_session(&db);
        let msg = db.add_message(&tid, "user", "hello", None, None).unwrap();
        assert_eq!(msg.content, "hello");
        let msgs = db.get_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[test]
    fn no_sliding_window_trim_keeps_full_history() {
        let db = test_db();
        let tid = test_session(&db);
        for i in 0..5 {
            db.add_message(&tid, "user", &format!("msg {}", i), None, None)
                .unwrap();
        }
        let msgs = db.get_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0].content, "msg 0");
        assert_eq!(msgs[4].content, "msg 4");
    }

    #[test]
    fn get_session_messages_limit_filters() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(&tid, "user", "hello", Some("text"), None)
            .unwrap();
        db.add_message(&tid, "user", "world", None, None).unwrap();
        let msgs = db.get_session_messages_limit(&tid, 1).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "world");
    }

    #[test]
    fn delete_session_messages_clears_all() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(&tid, "user", "msg1", None, None).unwrap();
        db.add_message(&tid, "user", "msg2", None, None).unwrap();
        db.delete_session_messages(&tid).unwrap();
        let msgs = db.get_session_messages(&tid).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn add_message_with_tool_call_id() {
        let db = test_db();
        let tid = test_session(&db);
        let msg = db
            .add_message(&tid, "tool", "result", Some("action"), Some("call-1"))
            .unwrap();
        assert_eq!(msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(msg.message_type.as_deref(), Some("action"));
    }

    #[test]
    fn add_message_with_attachments_roundtrip() {
        let db = test_db();
        let tid = test_session(&db);
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
        let msgs = db.get_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].attachments, vec![att]);
        assert_eq!(msgs[0].content, "看图");
    }

    #[test]
    fn message_without_attachments_reads_empty_vec() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(&tid, "user", "plain", None, None).unwrap();
        let msgs = db.get_session_messages(&tid).unwrap();
        assert!(msgs[0].attachments.is_empty());
    }

    #[test]
    fn message_serde_roundtrip_with_attachments() {
        let msg = Message {
            id: "m1".into(),
            session_id: "t1".into(),
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
        let tid = test_session(&db);
        db.add_message_full(
            &tid,
            "user",
            "voice hello",
            Some("text"),
            None,
            &[],
            true,
            None,
        )
        .unwrap();
        db.add_message(&tid, "user", "typed hello", Some("text"), None)
            .unwrap();
        let msgs = db.get_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].voice, "voice message must keep the flag");
        assert!(!msgs[1].voice, "typed message stays non-voice");
        // Serde default keeps old JSON payloads (pre-voice) decodable.
        let legacy = r#"{"id":"x","session_id":"t","role":"user","content":"c","message_type":"text","created_at":"2026-01-01T00:00:00Z","tool_call_id":null,"attachments":[]}"#;
        let decoded: Message = serde_json::from_str(legacy).unwrap();
        assert!(!decoded.voice);
    }

    #[test]
    fn get_last_message_created_at_returns_latest() {
        let db = test_db();
        let tid = test_session(&db);
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
        let tid = test_session(&db);
        let m1 = db.add_message(&tid, "user", "first", None, None).unwrap();
        let m2 = db
            .add_message(&tid, "assistant", "second", None, None)
            .unwrap();
        db.delete_messages_after(&tid, &m1.created_at).unwrap();
        let msgs = db.get_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "first");
        // m2 should be gone
        assert!(!msgs.iter().any(|m| m.id == m2.id));
    }

    #[test]
    fn delete_messages_from_inclusive() {
        let db = test_db();
        let tid = test_session(&db);
        let m1 = db.add_message(&tid, "user", "first", None, None).unwrap();
        let _m2 = db
            .add_message(&tid, "assistant", "second", None, None)
            .unwrap();
        // delete_messages_from deletes inclusively — m1 and m2 both gone
        db.delete_messages_from(&tid, &m1.created_at).unwrap();
        let msgs = db.get_session_messages(&tid).unwrap();
        assert!(msgs.is_empty());
    }

    #[test]
    fn messages_cascade_on_session_delete() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(&tid, "user", "msg1", None, None).unwrap();
        db.add_message(&tid, "user", "msg2", None, None).unwrap();
        db.delete_session(&tid).unwrap();
        let conn = db.conn();
        let count: i32 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn truncate_session_after_cleans_llm_usage_rows() {
        let db = test_db();
        let tid = test_session(&db);
        // Rows recorded in two batches separated by a cutoff; only the
        // second batch (after the cutoff) must be removed.
        db.record_llm_call_usage(
            &tid,
            Some(1),
            "default_model",
            None,
            10,
            5,
            15,
            0.0,
            false,
            None,
        )
        .unwrap();
        let cutoff = chrono::Utc::now().to_rfc3339();
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.record_llm_call_usage(
            &tid,
            Some(2),
            "default_model",
            None,
            20,
            10,
            30,
            0.0,
            false,
            None,
        )
        .unwrap();
        db.truncate_session_after(&tid, &cutoff, false).unwrap();
        let usage = db.get_session_llm_usage(&tid).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].step_number, Some(1));
    }
}
