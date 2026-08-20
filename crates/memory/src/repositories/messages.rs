use crate::db::Database;
use chrono::{SecondsFormat, Utc};
use haven_common::types::MessageAttachment;

/// Milliseconds-precision RFC3339: rows written within the same second must
/// remain distinguishable for the review timeline rebuild (the messages and
/// session_steps tables are interleaved by created_at on read).
pub(crate) fn now_rfc3339_millis() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Lower bound for undelivered-input recovery scans. Older unanchored rows are
/// treated as historical noise (legacy id formats / failed thought-step writes)
/// and must not re-enter the ReAct loop on history review or crash resume.
pub const UNDELIVERED_RECOVERY_MAX_AGE: chrono::Duration = chrono::Duration::days(2);

/// RFC3339 timestamp `now - UNDELIVERED_RECOVERY_MAX_AGE` for recovery scans.
pub fn undelivered_recovery_since() -> String {
    (Utc::now() - UNDELIVERED_RECOVERY_MAX_AGE).to_rfc3339_opts(SecondsFormat::Millis, true)
}

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
        // Strictly monotonic per session: two writes landing within the same
        // millisecond (or a clock step backwards) must stay distinguishable —
        // rollback's `delete_messages_after` deletes with `created_at > ?`
        // and would otherwise fail to discard the later message.
        let now = now_rfc3339_millis();
        let created_at = match self.get_last_message_created_at(session_id) {
            Some(last) if last >= now => Self::bump_millis(&last),
            _ => now,
        };
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
                created_at,
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
            created_at,
            tool_call_id: tool_call_id.map(String::from),
            attachments: attachments.to_vec(),
            voice,
        })
    }

    /// Step a stored `created_at` forward by one millisecond, keeping strict
    /// ordering when a new write collides with the session's latest row.
    /// Falls back to the current time when the stored value cannot be parsed.
    fn bump_millis(last: &str) -> String {
        match chrono::DateTime::parse_from_rfc3339(last) {
            Ok(ts) => (ts + chrono::Duration::milliseconds(1))
                .to_rfc3339_opts(SecondsFormat::Millis, true),
            Err(_) => now_rfc3339_millis(),
        }
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

    /// Return every message persisted strictly after the given timestamp
    /// (ascending). Resume uses this to recover inputs that landed after the
    /// restored snapshot (supplements/steering/answers persisted to the DB
    /// while paused or after a crash): anything newer than `saved_at` cannot
    /// be in the snapshot's canonical, so no content comparison is needed.
    pub fn get_session_messages_since(
        &self,
        session_id: &str,
        since: &str,
    ) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    attachments, voice
             FROM messages WHERE session_id = ?1 AND created_at > ?2
             ORDER BY created_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, since], map_message_row)?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        Ok(msgs)
    }

    /// Return every user message that was persisted but never injected into the
    /// agent's context. A submitted input is "delivered" when the ReAct loop
    /// injects it via `push_user_context`, which anchors it with a
    /// `session_steps` row under the message's own id (see
    /// `create_thought_step`). A `msg-*` user row without that anchor was
    /// queued as steering/supplement and then lost — the session errored,
    /// completed, or was cancelled mid-batch before the loop drained the queue.
    /// Reopen/resume re-delivers these so history review never leaves a user
    /// message stranded in a "pending / not delivered" state.
    ///
    /// The session's FIRST user message is the session input seeded into the
    /// canonical directly (it never carries an anchor), so it is excluded here.
    pub fn get_undelivered_user_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        self.get_undelivered_user_messages_since(session_id, None)
    }

    /// Like [`Self::get_undelivered_user_messages`], but when `since_created_at`
    /// is set only rows with `created_at > since` are returned. Callers use this
    /// to bound crash/review recovery (e.g. last 2 days) so ancient false
    /// positives from missing anchors never re-enter the ReAct loop.
    pub fn get_undelivered_user_messages_since(
        &self,
        session_id: &str,
        since_created_at: Option<&str>,
    ) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.message_type, m.created_at,
                    m.tool_call_id, m.attachments, m.voice
             FROM messages m
             WHERE m.session_id = ?1
               AND m.role = 'user'
               AND m.id LIKE 'msg-%'
               AND (?2 IS NULL OR m.created_at > ?2)
               AND m.id <> (
                   SELECT id FROM messages
                   WHERE session_id = ?1 AND role = 'user'
                   ORDER BY created_at ASC, rowid ASC LIMIT 1
               )
               AND NOT EXISTS (
                   SELECT 1 FROM session_steps st
                   WHERE st.session_id = m.session_id AND st.id = m.id
               )
             ORDER BY m.created_at ASC, m.rowid ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, since_created_at],
            map_message_row,
        )?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        Ok(msgs)
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

    /// Forward-date a message row's `created_at`. Mid-turn interjections
    /// (steering) are persisted at SUBMIT time — before the interrupted
    /// step's thought row lands — so the review rebuild would order them
    /// before the text they interrupted. The ReAct loop calls this when it
    /// actually injects the message, moving the row to its logical position
    /// (after the interrupted thought, before the answer to it).
    pub fn update_message_created_at(
        &self,
        session_id: &str,
        message_id: &str,
        created_at: &str,
    ) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2 AND session_id = ?3",
            rusqlite::params![created_at, message_id, session_id],
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
    fn get_session_messages_since_returns_only_newer_rows() {
        let db = test_db();
        let tid = test_session(&db);
        let first = db.add_message(&tid, "user", "before", None, None).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let cutoff = now_rfc3339_millis();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = db.add_message(&tid, "user", "after", None, None).unwrap();

        let since = db.get_session_messages_since(&tid, &cutoff).unwrap();
        assert_eq!(since.len(), 1, "only rows after the cutoff");
        assert_eq!(since[0].id, second.id);
        assert_ne!(since[0].id, first.id, "the earlier row must be excluded");
    }

    #[test]
    fn get_undelivered_user_messages_excludes_first_and_anchored() {
        let db = test_db();
        let tid = test_session(&db);
        // The session input (first user message) is seeded into the canonical
        // directly and never carries a step anchor — must be excluded.
        let first = db.add_message(&tid, "user", "开场", None, None).unwrap();
        // A steering input delivered via push_user_context gets a step anchor.
        let delivered = db.add_message(&tid, "user", "继续", None, None).unwrap();
        db.create_thought_step(&tid, 2, &delivered.id).unwrap();
        // A queued steering lost before injection has no anchor.
        let lost = db
            .add_message(&tid, "user", "C:\\照片目录", None, None)
            .unwrap();

        let undelivered = db.get_undelivered_user_messages(&tid).unwrap();
        assert_eq!(
            undelivered.len(),
            1,
            "only the never-injected input is pending"
        );
        assert_eq!(undelivered[0].id, lost.id);
        assert_ne!(undelivered[0].id, first.id);
        assert_ne!(undelivered[0].id, delivered.id);
        // Attachment payloads survive the scan (images travel with the input).
        assert!(undelivered[0].attachments.is_empty());
    }

    #[test]
    fn get_undelivered_user_messages_skips_legacy_and_empty_sessions() {
        let db = test_db();
        let tid = test_session(&db);
        // No messages at all: nothing to recover.
        assert!(db.get_undelivered_user_messages(&tid).unwrap().is_empty());
        // Assistant rows are never user inputs.
        db.add_message(&tid, "assistant", "hi", Some("text"), None)
            .unwrap();
        assert!(db.get_undelivered_user_messages(&tid).unwrap().is_empty());
    }

    #[test]
    fn get_undelivered_user_messages_since_filters_by_created_at() {
        let db = test_db();
        let tid = test_session(&db);
        let _first = db.add_message(&tid, "user", "开场", None, None).unwrap();
        let lost = db.add_message(&tid, "user", "丢失输入", None, None).unwrap();
        // A cutoff newer than the lost row excludes it.
        let after = (Utc::now() + chrono::Duration::seconds(1))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        assert!(
            db.get_undelivered_user_messages_since(&tid, Some(after.as_str()))
                .unwrap()
                .is_empty()
        );
        // A cutoff older than the lost row still returns it.
        let before = (Utc::now() - chrono::Duration::days(1))
            .to_rfc3339_opts(SecondsFormat::Millis, true);
        let undelivered = db
            .get_undelivered_user_messages_since(&tid, Some(before.as_str()))
            .unwrap();
        assert_eq!(undelivered.len(), 1);
        assert_eq!(undelivered[0].id, lost.id);
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
    fn add_message_full_with_attachments_roundtrip() {
        let db = test_db();
        let tid = test_session(&db);
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
        db.add_message_full(
            &tid,
            "user",
            "看图",
            Some("text"),
            None,
            std::slice::from_ref(&att),
            false,
            None,
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
    fn update_message_created_at_reorders_row_after_other_messages() {
        let db = test_db();
        let tid = test_session(&db);
        // The steering row is persisted at submit; the interrupted step's
        // thought row lands later. Forward-dating the steering row must
        // move it AFTER the thought row in read order.
        let steering = db
            .add_message(&tid, "user", "steering", None, None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let thought = db
            .add_message(&tid, "assistant", "被打断的思考", None, None)
            .unwrap();
        let order_before: Vec<String> = db
            .get_session_messages(&tid)
            .unwrap()
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(order_before, vec![steering.id.clone(), thought.id.clone()]);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let now = now_rfc3339_millis();
        db.update_message_created_at(&tid, &steering.id, &now)
            .unwrap();
        let order_after: Vec<String> = db
            .get_session_messages(&tid)
            .unwrap()
            .iter()
            .map(|m| m.id.clone())
            .collect();
        assert_eq!(order_after, vec![thought.id.clone(), steering.id.clone()]);
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
