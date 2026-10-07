use crate::db::Database;
use base64::Engine as _;
use chrono::{SecondsFormat, Utc};
use haven_common::media::{MediaInput, message_attachment_to_media_input};
use haven_common::types::{CanonicalRole, MessageAttachment};
use rusqlite::OptionalExtension;
use std::collections::HashSet;
use std::path::PathBuf;

/// Milliseconds-precision RFC3339: rows written within the same second must
/// remain distinguishable for the resume timeline rebuild (the messages and
/// session_steps tables are interleaved by created_at on read).
pub(crate) fn now_rfc3339_millis() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Map a `messages` row (11 columns: id, session_id, role, content, message_type,
/// created_at, tool_call_id, ui_metadata, voice, ingress_seq, media_inputs) into a `Message`. Shared by
/// every read query so column order cannot drift between them.
fn map_message_row(row: &rusqlite::Row) -> rusqlite::Result<Message> {
    let role_text: String = row.get(2)?;
    let role = CanonicalRole::parse(&role_text).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            format!("invalid CanonicalRole in messages.role: {role_text}").into(),
        )
    })?;
    Ok(Message {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role,
        content: row.get(3)?,
        message_type: row.get(4)?,
        created_at: row.get(5)?,
        tool_call_id: row.get(6)?,
        attachments: Database::parse_ui_metadata(row.get(7)?),
        voice: row.get::<_, i32>(8)? != 0,
        ingress_seq: row.get(9)?,
        media_inputs: Database::parse_media_inputs(row.get(10)?),
    })
}

/// The database keeps this small projection only for UI rendering and host
/// asset retention. It intentionally excludes inline bytes and provider
/// representations; those belong exclusively to `messages.media_inputs`.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct MessageUiMetadata {
    #[serde(default)]
    attachment_previews: Vec<UiAttachmentMetadata>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct UiAttachmentMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asset_id: Option<String>,
    media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<String>,
}

impl From<&MessageAttachment> for UiAttachmentMetadata {
    fn from(attachment: &MessageAttachment) -> Self {
        Self {
            asset_id: attachment.asset_id.clone(),
            media_type: attachment.media_type.clone(),
            filename: attachment.filename.clone(),
            path: attachment.path.clone(),
            sha256: attachment.sha256.clone(),
            size_bytes: attachment.size_bytes,
            expires_at: attachment.expires_at.clone(),
        }
    }
}

impl From<UiAttachmentMetadata> for MessageAttachment {
    fn from(metadata: UiAttachmentMetadata) -> Self {
        Self {
            asset_id: metadata.asset_id,
            media_type: metadata.media_type,
            data: String::new(),
            filename: metadata.filename,
            path: metadata.path,
            sha256: metadata.sha256,
            size_bytes: metadata.size_bytes,
            expires_at: metadata.expires_at,
            representations: Vec::new(),
            preferred_representation: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub role: CanonicalRole,
    pub content: String,
    pub message_type: Option<String>,
    pub created_at: String,
    pub tool_call_id: Option<String>,
    pub attachments: Vec<MessageAttachment>,
    /// Durable provider-neutral media representations. This is the only
    /// persistence projection used for media planning and recovery. The
    /// `attachments` field is an ingress/UI DTO and is reconstructed from the
    /// message's UI metadata when a row is read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_inputs: Vec<MediaInput>,
    /// True for user messages that came from voice transcription (mic style
    /// in the UI survives reloads). Assistant/tool messages are always false.
    pub voice: bool,
    /// Durable per-session ingress order. This is intentionally omitted from
    /// the IPC JSON surface; it is a recovery cursor, not user-visible data.
    #[serde(skip)]
    pub ingress_seq: i64,
}

/// How an accepted input should be queued if the process exits before its
/// `UserInject` event commits. This value is authoritative only while the
/// matching durable pending marker exists; after acknowledgement the event
/// stream owns transcript recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingInputDisposition {
    Answer,
    FollowUp,
}

impl PendingInputDisposition {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Answer => "answer",
            Self::FollowUp => "follow_up",
        }
    }

    fn from_db_value(value: &str) -> rusqlite::Result<Self> {
        match value {
            "answer" => Ok(Self::Answer),
            "follow_up" => Ok(Self::FollowUp),
            other => Err(rusqlite::Error::FromSqlConversionFailure(
                11,
                rusqlite::types::Type::Text,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid pending input disposition: {other}"),
                )
                .into(),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingSessionInput {
    pub message: Message,
    pub disposition: PendingInputDisposition,
}

impl Database {
    pub fn add_message(
        &self,
        session_id: &str,
        role: CanonicalRole,
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
        role: CanonicalRole,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        attachments: &[MessageAttachment],
        voice: bool,
        // When `Some`, insert the row under this pre-minted id (streamed
        // block ids minted at stream start); `None` mints a fresh `msg-*`.
        id: Option<&str>,
    ) -> anyhow::Result<Message> {
        let media_inputs: Vec<MediaInput> = attachments
            .iter()
            .map(message_attachment_to_media_input)
            .map(|input| input.for_snapshot())
            .collect();
        self.add_message_full_with_media(
            session_id,
            role,
            content,
            message_type,
            tool_call_id,
            attachments,
            voice,
            id,
            &media_inputs,
            None,
        )
        .map(|(message, _)| message)
    }

    /// Persist a newly submitted user input and its recovery marker atomically.
    /// The marker remains until the owning `UserInject` transcript commit
    /// acknowledges delivery; both rows are in the same SQLite transaction.
    #[allow(clippy::too_many_arguments)]
    pub fn add_pending_user_input(
        &self,
        session_id: &str,
        content: &str,
        message_type: Option<&str>,
        attachments: &[MessageAttachment],
        voice: bool,
        id: Option<&str>,
        disposition: PendingInputDisposition,
    ) -> anyhow::Result<PendingSessionInput> {
        let media_inputs: Vec<MediaInput> = attachments
            .iter()
            .map(message_attachment_to_media_input)
            .map(|input| input.for_snapshot())
            .collect();
        let (message, disposition) = self.add_message_full_with_media(
            session_id,
            CanonicalRole::User,
            content,
            message_type,
            None,
            attachments,
            voice,
            id,
            &media_inputs,
            Some(disposition),
        )?;
        Ok(PendingSessionInput {
            message,
            disposition: disposition
                .ok_or_else(|| anyhow::anyhow!("pending input write omitted its disposition"))?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn add_message_full_with_media(
        &self,
        session_id: &str,
        role: CanonicalRole,
        content: &str,
        message_type: Option<&str>,
        tool_call_id: Option<&str>,
        attachments: &[MessageAttachment],
        voice: bool,
        id: Option<&str>,
        media_inputs: &[MediaInput],
        pending_disposition: Option<PendingInputDisposition>,
    ) -> anyhow::Result<(Message, Option<PendingInputDisposition>)> {
        let id = id
            .map(String::from)
            .unwrap_or_else(|| haven_common::types::new_id("msg"));
        // Strictly monotonic per session: two writes landing within the same
        // millisecond (or a clock step backwards) must stay distinguishable —
        // rollback's `delete_messages_after` deletes with `created_at > ?`
        // and would otherwise fail to discard the later message.
        let now = now_rfc3339_millis();
        let created_at = match self.get_last_message_created_at_best_effort(session_id) {
            Some(last) if last >= now => Self::bump_millis(&last),
            _ => now,
        };
        let conn = self.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(i64, Option<PendingInputDisposition>)> {
            conn.execute(
                "INSERT INTO message_ingress_cursors (session_id, last_ingress_seq)
                 VALUES (?1, 1)
                 ON CONFLICT(session_id) DO UPDATE SET
                     last_ingress_seq = message_ingress_cursors.last_ingress_seq + 1",
                rusqlite::params![session_id],
            )?;
            let ingress_seq = conn.query_row(
                "SELECT last_ingress_seq FROM message_ingress_cursors WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )?;
            conn.execute(
                "INSERT INTO messages (id, session_id, role, content, message_type, created_at, tool_call_id, ui_metadata, voice, ingress_seq, media_inputs)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                rusqlite::params![
                    id,
                    session_id,
            role.as_str(),
                    content,
                    message_type,
                    created_at,
                    tool_call_id,
                    Self::serialize_ui_metadata(attachments),
                    voice,
                    ingress_seq,
                    Self::serialize_media_inputs(media_inputs),
                ],
            )?;
            let mut disposition = pending_disposition;
            if let Some(requested) = disposition {
                if requested == PendingInputDisposition::Answer {
                    let answer_reserved: bool = conn.query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM pending_session_inputs
                            WHERE session_id = ?1 AND disposition = 'answer'
                        )",
                        rusqlite::params![session_id],
                        |row| row.get(0),
                    )?;
                    if answer_reserved {
                        disposition = Some(PendingInputDisposition::FollowUp);
                    }
                }
                conn.execute(
                    "INSERT INTO pending_session_inputs (session_id, message_id, disposition)
                     VALUES (?1, ?2, ?3)",
                    rusqlite::params![session_id, id, disposition.unwrap().as_db_value()],
                )?;
            }
            Ok((ingress_seq, disposition))
        })();
        let (ingress_seq, disposition) = match result {
            Ok(result) => {
                conn.execute_batch("COMMIT")?;
                result
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        };
        drop(conn);
        self.cache_invalidate_messages(session_id);
        Ok((
            Message {
                id,
                session_id: session_id.into(),
                role,
                content: content.into(),
                message_type: message_type.map(String::from),
                created_at,
                tool_call_id: tool_call_id.map(String::from),
                attachments: attachments.to_vec(),
                media_inputs: media_inputs.to_vec(),
                voice,
                ingress_seq,
            },
            disposition,
        ))
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

    fn serialize_ui_metadata(attachments: &[MessageAttachment]) -> Option<String> {
        if attachments.is_empty() {
            None
        } else {
            let metadata = MessageUiMetadata {
                attachment_previews: attachments.iter().map(UiAttachmentMetadata::from).collect(),
            };
            serde_json::to_string(&metadata).ok()
        }
    }

    fn parse_ui_metadata(raw: Option<String>) -> Vec<MessageAttachment> {
        let mut attachments: Vec<MessageAttachment> = match raw {
            Some(s) if !s.is_empty() => serde_json::from_str::<MessageUiMetadata>(&s)
                .map(|metadata| {
                    metadata
                        .attachment_previews
                        .into_iter()
                        .map(Into::into)
                        .collect()
                })
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        // `messages.ui_metadata` is deliberately limited to a UI/retention
        // projection. Rehydrate a preview only from the two host-owned media
        // roots; the durable provider-neutral representation remains
        // `media_inputs`.
        for attachment in &mut attachments {
            if attachment.data.is_empty()
                && let Some(path) = attachment.path.as_deref()
                && let Some(bytes) = read_host_media(path)
            {
                attachment.data = base64::engine::general_purpose::STANDARD.encode(bytes);
            }
        }
        attachments
    }

    fn serialize_media_inputs(media_inputs: &[MediaInput]) -> Option<String> {
        if media_inputs.is_empty() {
            None
        } else {
            serde_json::to_string(media_inputs).ok()
        }
    }

    fn parse_media_inputs(raw: Option<String>) -> Vec<MediaInput> {
        match raw {
            Some(value) if !value.is_empty() => serde_json::from_str(&value).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Read one message by its durable id. ToolRun result delivery uses this
    /// as the idempotency check when a completion is retried after the
    /// projection write succeeded but the acknowledgement was lost.
    pub fn get_message_by_id(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<Message>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    ui_metadata, voice, ingress_seq, media_inputs
             FROM messages WHERE session_id = ?1 AND id = ?2",
            rusqlite::params![session_id, message_id],
            map_message_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn list_session_messages(&self, session_id: &str) -> anyhow::Result<Vec<Message>> {
        if let Some(cached) = self.cache_get_messages(session_id) {
            return Ok(cached);
        }
        // Capture generation before querying DB so cache_put can detect a
        // concurrent invalidation and skip the stale-overwrite.
        let cache_gen = self.cache_generation(session_id);
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    ui_metadata, voice, ingress_seq, media_inputs
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

    /// Return host-managed attachment paths still referenced by persisted
    /// messages. Retention cleanup uses this reference set so paused or
    /// long-running sessions keep their files, while assets belonging to
    /// deleted history can be removed from the process-local registry.
    pub fn list_managed_attachment_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        let conn = self.conn();
        let mut statement = conn.prepare(
            "SELECT ui_metadata FROM messages
             WHERE ui_metadata IS NOT NULL AND ui_metadata != ''",
        )?;
        let mut rows = statement.query([])?;
        let mut paths = HashSet::new();
        while let Some(row) = rows.next()? {
            let raw: String = row.get(0)?;
            let metadata: MessageUiMetadata = serde_json::from_str(&raw).map_err(|error| {
                anyhow::anyhow!("invalid persisted message UI metadata: {error}")
            })?;
            for attachment in metadata.attachment_previews {
                if let Some(path) = attachment.path
                    && !path.trim().is_empty()
                {
                    paths.insert(PathBuf::from(path));
                }
            }
        }
        Ok(paths.into_iter().collect())
    }

    pub fn list_recent_session_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<Message>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, session_id, role, content, message_type, created_at, tool_call_id,
                    ui_metadata, voice, ingress_seq, media_inputs
             FROM messages WHERE session_id = ?1 AND (message_type IS NULL OR message_type = 'text' OR message_type = 'peer_kickoff')
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

    /// Return all user inputs whose durable delivery marker has not been
    /// acknowledged by a committed `UserInject` event. Recovery state is
    /// explicit and has no age cutoff.
    pub fn list_pending_session_inputs(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<PendingSessionInput>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT m.id, m.session_id, m.role, m.content, m.message_type, m.created_at,
                    m.tool_call_id, m.ui_metadata, m.voice, m.ingress_seq, m.media_inputs,
                    p.disposition
             FROM pending_session_inputs p
             JOIN messages m ON m.session_id = p.session_id AND m.id = p.message_id
             WHERE p.session_id = ?1
             ORDER BY m.ingress_seq ASC, m.rowid ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok(PendingSessionInput {
                message: map_message_row(row)?,
                disposition: PendingInputDisposition::from_db_value(&row.get::<_, String>(11)?)?,
            })
        })?;
        let mut msgs = Vec::new();
        for row in rows {
            msgs.push(row?);
        }
        Ok(msgs)
    }

    pub fn get_pending_session_input(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Option<PendingSessionInput>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT m.id, m.session_id, m.role, m.content, m.message_type, m.created_at,
                    m.tool_call_id, m.ui_metadata, m.voice, m.ingress_seq, m.media_inputs,
                    p.disposition
             FROM pending_session_inputs p
             JOIN messages m ON m.session_id = p.session_id AND m.id = p.message_id
             WHERE p.session_id = ?1 AND p.message_id = ?2",
            rusqlite::params![session_id, message_id],
            |row| {
                Ok(PendingSessionInput {
                    message: map_message_row(row)?,
                    disposition: PendingInputDisposition::from_db_value(
                        &row.get::<_, String>(11)?,
                    )?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Acknowledge an input only after its `UserInject` event commits.
    pub(crate) fn acknowledge_pending_session_input(
        conn: &rusqlite::Connection,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        conn.execute(
            "DELETE FROM pending_session_inputs WHERE session_id = ?1 AND message_id = ?2",
            rusqlite::params![session_id, message_id],
        )?;
        Ok(())
    }

    /// Return the most recent message timestamp when the read succeeds. A
    /// database read failure is treated as unavailable; use the strict
    /// variant when a missing cutoff could affect recovery correctness.
    pub fn get_last_message_created_at_best_effort(&self, session_id: &str) -> Option<String> {
        self.try_get_last_message_created_at(session_id)
            .ok()
            .flatten()
    }

    /// Return the most recent message timestamp and preserve SQLite read
    /// errors so recovery callers never turn an unknown cutoff into `NULL`.
    pub fn try_get_last_message_created_at(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let conn = self.conn();
        Ok(conn
            .query_row(
                "SELECT created_at FROM messages WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                rusqlite::params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    }

    /// Return the highest durable ingress cursor for a session.
    pub fn get_last_message_ingress_seq(&self, session_id: &str) -> i64 {
        let conn = self.conn();
        conn.query_row(
            "SELECT COALESCE(last_ingress_seq, 0)
             FROM message_ingress_cursors WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap_or(0)
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
    /// step's thought row lands — so the resume rebuild would order them
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

    /// Drop every message **and** ses-step whose `created_at` is strictly
    /// after `ts`, or at-or-after `ts` when `inclusive`. Centralizes the
    /// `delete_messages_after/from + delete_session_steps_after` pair that
    /// rollback used to repeat at every branch-point cutoff, so a future
    /// step-row source (e.g. per-session tool tables) only needs one edit.
    /// `llm_usage` detail rows are cut on the same timeline (their
    /// `created_at` is RFC3339 like messages), so discarded steps leave no
    /// orphaned usage history behind. `session_usage` is then rebuilt from
    /// the remaining detail rows so cumulative token stats stay accurate.
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
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            conn.execute(&msgs_sql, rusqlite::params![session_id, ts])?;
            conn.execute(&steps_sql, rusqlite::params![session_id, ts])?;
            conn.execute(&usage_sql, rusqlite::params![session_id, ts])?;
            // Keep the cumulative usage projection in the same transaction as
            // the detail-row deletion. A crash between separate commits would
            // otherwise leave the UI showing tokens for a rolled-back branch.
            Self::rebuild_session_usage_from_calls_conn(&conn, session_id)?;
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(error);
            }
        }
        self.cache_invalidate_messages(session_id);
        Ok(())
    }
}

fn read_host_media(path: &str) -> Option<Vec<u8>> {
    let path = std::path::Path::new(path);
    let roots = [
        haven_common::default_work_dir().join("uploads"),
        haven_common::config::default_generated_media_dir(),
    ];
    let canonical_path = std::fs::canonicalize(path).ok()?;
    if !roots.iter().any(|root| {
        std::fs::canonicalize(root)
            .ok()
            .is_some_and(|root| canonical_path.starts_with(root))
    }) {
        return None;
    }
    std::fs::read(canonical_path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use haven_common::config::RequestKind;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    fn test_session(db: &Database) -> String {
        db.create_session("input").unwrap().id
    }

    #[test]
    fn add_and_get_messages() {
        let db = test_db();
        let tid = test_session(&db);
        let msg = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "hello",
                None,
                None,
            )
            .unwrap();
        assert_eq!(msg.content, "hello");
        let msgs = db.list_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
        assert_eq!(msgs[0].ingress_seq, 1);
    }

    #[test]
    fn list_session_messages_rejects_unknown_role_from_storage() {
        let db = test_db();
        let session_id = test_session(&db);
        db.add_message(&session_id, CanonicalRole::User, "hello", None, None)
            .unwrap();

        {
            let conn = db.conn();
            conn.execute_batch("PRAGMA ignore_check_constraints = ON")
                .unwrap();
            conn.execute(
                "UPDATE messages SET role = 'developer' WHERE session_id = ?1",
                [&session_id],
            )
            .unwrap();
            conn.execute_batch("PRAGMA ignore_check_constraints = OFF")
                .unwrap();
        }

        let error = db.list_session_messages(&session_id).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid CanonicalRole in messages.role")
        );
    }

    #[test]
    fn ingress_cursor_does_not_reuse_sequence_after_message_delete() {
        let db = test_db();
        let tid = test_session(&db);
        let first = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "first",
                None,
                None,
            )
            .unwrap();
        let second = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "second",
                None,
                None,
            )
            .unwrap();
        db.delete_message_by_id(&tid, &second.id).unwrap();

        let replacement = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "replacement",
                None,
                None,
            )
            .unwrap();
        assert_eq!(first.ingress_seq, 1);
        assert_eq!(replacement.ingress_seq, 3);
        assert_eq!(db.get_last_message_ingress_seq(&tid), 3);
    }

    #[test]
    fn no_sliding_window_trim_keeps_full_history() {
        let db = test_db();
        let tid = test_session(&db);
        for i in 0..5 {
            db.add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                &format!("msg {}", i),
                None,
                None,
            )
            .unwrap();
        }
        let msgs = db.list_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0].content, "msg 0");
        assert_eq!(msgs[4].content, "msg 4");
    }

    #[test]
    fn list_recent_session_messages_filters() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "hello",
            Some("text"),
            None,
        )
        .unwrap();
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "world",
            None,
            None,
        )
        .unwrap();
        let msgs = db.list_recent_session_messages(&tid, 1).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "world");
    }

    #[test]
    fn pending_session_inputs_only_include_explicitly_tracked_rows() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "开场",
            None,
            None,
        )
        .unwrap();
        let delivered = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "继续",
                None,
                None,
            )
            .unwrap();
        db.create_thought_step(&tid, 2, &delivered.id).unwrap();
        let pending = db
            .add_pending_user_input(
                &tid,
                "C:\\照片目录",
                Some("text"),
                &[],
                false,
                None,
                PendingInputDisposition::FollowUp,
            )
            .unwrap();

        let pending_rows = db.list_pending_session_inputs(&tid).unwrap();
        assert_eq!(
            pending_rows.len(),
            1,
            "only an explicit pending marker is returned"
        );
        assert_eq!(pending_rows[0].message.id, pending.message.id);
        assert_eq!(
            pending_rows[0].disposition,
            PendingInputDisposition::FollowUp
        );
        // Attachment payloads survive the scan (images travel with the input).
        assert!(pending_rows[0].message.attachments.is_empty());
    }

    #[test]
    fn pending_session_inputs_skips_untracked_rows_and_empty_sessions() {
        let db = test_db();
        let tid = test_session(&db);
        // No messages at all: nothing to recover.
        assert!(db.list_pending_session_inputs(&tid).unwrap().is_empty());
        // Ordinary historical user and assistant rows are never pending.
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "history",
            Some("text"),
            None,
        )
        .unwrap();
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::Assistant,
            "hi",
            Some("text"),
            None,
        )
        .unwrap();
        assert!(db.list_pending_session_inputs(&tid).unwrap().is_empty());
    }

    #[test]
    fn pending_session_inputs_survive_arbitrary_message_age() {
        let db = test_db();
        let tid = test_session(&db);
        let pending = db
            .add_pending_user_input(
                &tid,
                "丢失输入",
                Some("text"),
                &[],
                false,
                None,
                PendingInputDisposition::FollowUp,
            )
            .unwrap();
        let old =
            (Utc::now() - chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Millis, true);
        let conn = db.conn();
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![old, pending.message.id],
        )
        .unwrap();
        drop(conn);

        let recovered = db.list_pending_session_inputs(&tid).unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].message.id, pending.message.id);
        assert_eq!(recovered[0].disposition, PendingInputDisposition::FollowUp);
    }

    #[test]
    fn pending_answer_reservation_downgrades_later_answers_to_follow_ups() {
        let db = test_db();
        let tid = test_session(&db);
        let first = db
            .add_pending_user_input(
                &tid,
                "answer",
                Some("text"),
                &[],
                false,
                None,
                PendingInputDisposition::Answer,
            )
            .unwrap();
        let second = db
            .add_pending_user_input(
                &tid,
                "later input",
                Some("text"),
                &[],
                false,
                None,
                PendingInputDisposition::Answer,
            )
            .unwrap();

        assert_eq!(first.disposition, PendingInputDisposition::Answer);
        assert_eq!(second.disposition, PendingInputDisposition::FollowUp);
        let pending = db.list_pending_session_inputs(&tid).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].disposition, PendingInputDisposition::Answer);
        assert_eq!(pending[1].disposition, PendingInputDisposition::FollowUp);
    }

    #[test]
    fn pending_input_and_recovery_marker_persist_atomically() {
        let db = test_db();
        let tid = test_session(&db);
        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_pending_marker
                 BEFORE INSERT ON pending_session_inputs
                 BEGIN SELECT RAISE(ABORT, 'forced marker failure'); END;",
            )
            .unwrap();

        assert!(
            db.add_pending_user_input(
                &tid,
                "input",
                Some("text"),
                &[],
                false,
                None,
                PendingInputDisposition::FollowUp,
            )
            .is_err()
        );
        assert!(db.list_session_messages(&tid).unwrap().is_empty());
        assert_eq!(db.get_last_message_ingress_seq(&tid), 0);
    }

    #[test]
    fn add_message_with_tool_call_id() {
        let db = test_db();
        let tid = test_session(&db);
        let msg = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::Tool,
                "result",
                Some("tool_call"),
                Some("call-1"),
            )
            .unwrap();
        assert_eq!(msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(msg.message_type.as_deref(), Some("tool_call"));
    }

    #[test]
    fn add_message_full_with_attachments_roundtrip() {
        let db = test_db();
        let tid = test_session(&db);
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
        db.add_message_full(
            &tid,
            haven_common::types::CanonicalRole::User,
            "看图",
            Some("text"),
            None,
            std::slice::from_ref(&att),
            false,
            None,
        )
        .unwrap();
        let msgs = db.list_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].attachments[0].media_type, att.media_type);
        assert!(msgs[0].attachments[0].data.is_empty());
        assert_eq!(msgs[0].media_inputs.len(), 1);
        assert!(matches!(
            msgs[0].media_inputs[0].representations[0].payload,
            haven_common::media::MediaRepresentationPayload::ManagedFileRef { .. }
        ));
        assert_eq!(msgs[0].content, "看图");
    }

    #[test]
    fn managed_attachment_persists_metadata_and_rehydrates_only_host_preview() -> anyhow::Result<()>
    {
        let db = test_db();
        let tid = test_session(&db);
        let dir = haven_common::default_work_dir()
            .join("uploads")
            .join(format!("message-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("photo.png");
        std::fs::write(&path, b"hello").unwrap();

        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some(haven_common::types::new_id("asset"));
        attachment.filename = Some("photo.png".into());
        attachment.path = Some(path.to_string_lossy().into_owned());
        attachment.size_bytes = Some(5);
        db.add_message_full(
            &tid,
            haven_common::types::CanonicalRole::User,
            "看图",
            Some("text"),
            None,
            std::slice::from_ref(&attachment),
            false,
            None,
        )
        .unwrap();

        let raw = db.conn().query_row(
            "SELECT ui_metadata, media_inputs FROM messages WHERE session_id = ?1",
            rusqlite::params![tid],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;
        let ui_metadata = raw.0.unwrap_or_default();
        assert!(!ui_metadata.contains("aGVsbG8="));
        assert!(!ui_metadata.contains("representations"));
        assert!(!ui_metadata.contains("preferred_representation"));
        assert!(!raw.1.unwrap_or_default().contains("aGVsbG8="));

        let message = db.list_session_messages(&tid).unwrap().remove(0);
        assert_eq!(message.attachments[0].data, "aGVsbG8=");
        assert_eq!(message.media_inputs.len(), 1);
        assert!(matches!(
            message.media_inputs[0].representations[0].payload,
            haven_common::media::MediaRepresentationPayload::ManagedFileRef { .. }
        ));
        let _ = std::fs::remove_dir_all(dir);
        Ok(())
    }

    #[test]
    fn message_without_attachments_reads_empty_vec() {
        let db = test_db();
        let tid = test_session(&db);
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "plain",
            None,
            None,
        )
        .unwrap();
        let msgs = db.list_session_messages(&tid).unwrap();
        assert!(msgs[0].attachments.is_empty());
    }

    #[test]
    fn list_managed_attachment_paths_returns_only_non_empty_paths() {
        let db = test_db();
        let tid = test_session(&db);
        let mut file = MessageAttachment::new("application/pdf", "");
        file.path = Some(r"C:\uploads\file-a\report.pdf".into());
        db.add_message_full(
            &tid,
            haven_common::types::CanonicalRole::User,
            "read this",
            Some("text"),
            None,
            std::slice::from_ref(&file),
            false,
            None,
        )
        .unwrap();
        db.add_message_full(
            &tid,
            haven_common::types::CanonicalRole::User,
            "look at this",
            Some("text"),
            None,
            &[MessageAttachment::new("image/png", "aGVsbG8=")],
            false,
            None,
        )
        .unwrap();

        let paths = db.list_managed_attachment_paths().unwrap();
        assert_eq!(paths, vec![PathBuf::from(r"C:\uploads\file-a\report.pdf")]);
    }

    #[test]
    fn message_serde_roundtrip_with_attachments() {
        let msg = Message {
            id: "m1".into(),
            session_id: "t1".into(),
            role: CanonicalRole::User,
            content: "看图".into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            attachments: vec![MessageAttachment::new("image/jpeg", "abc")],
            media_inputs: vec![],
            voice: true,
            ingress_seq: 0,
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
            haven_common::types::CanonicalRole::User,
            "voice hello",
            Some("text"),
            None,
            &[],
            true,
            None,
        )
        .unwrap();
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "typed hello",
            Some("text"),
            None,
        )
        .unwrap();
        let msgs = db.list_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].voice, "voice message must keep the flag");
        assert!(!msgs[1].voice, "typed message stays non-voice");
    }

    #[test]
    fn get_last_message_created_at_best_effort_returns_latest() {
        let db = test_db();
        let tid = test_session(&db);
        assert!(db.get_last_message_created_at_best_effort(&tid).is_none());
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "first",
            None,
            None,
        )
        .unwrap();
        let m2 = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "second",
                None,
                None,
            )
            .unwrap();
        let last = db
            .get_last_message_created_at_best_effort(&tid)
            .expect("some timestamp");
        assert_eq!(last, m2.created_at);
    }

    #[test]
    fn strict_last_message_timestamp_surfaces_database_failure() {
        let db = test_db();
        let tid = test_session(&db);
        db.conn().execute_batch("DROP TABLE messages").unwrap();

        assert!(db.try_get_last_message_created_at(&tid).is_err());
    }

    #[test]
    fn delete_messages_after_keeps_older() {
        let db = test_db();
        let tid = test_session(&db);
        let m1 = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "first",
                None,
                None,
            )
            .unwrap();
        let m2 = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::Assistant,
                "second",
                None,
                None,
            )
            .unwrap();
        db.delete_messages_after(&tid, &m1.created_at).unwrap();
        let msgs = db.list_session_messages(&tid).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "first");
        // m2 should be gone
        assert!(!msgs.iter().any(|m| m.id == m2.id));
    }

    #[test]
    fn delete_messages_from_inclusive() {
        let db = test_db();
        let tid = test_session(&db);
        let m1 = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "first",
                None,
                None,
            )
            .unwrap();
        let _m2 = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::Assistant,
                "second",
                None,
                None,
            )
            .unwrap();
        // delete_messages_from deletes inclusively — m1 and m2 both gone
        db.delete_messages_from(&tid, &m1.created_at).unwrap();
        let msgs = db.list_session_messages(&tid).unwrap();
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
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::User,
                "steering",
                None,
                None,
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let thought = db
            .add_message(
                &tid,
                haven_common::types::CanonicalRole::Assistant,
                "被打断的思考",
                None,
                None,
            )
            .unwrap();
        let order_before: Vec<String> = db
            .list_session_messages(&tid)
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
            .list_session_messages(&tid)
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
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "msg1",
            None,
            None,
        )
        .unwrap();
        db.add_message(
            &tid,
            haven_common::types::CanonicalRole::User,
            "msg2",
            None,
            None,
        )
        .unwrap();
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
        db.persist_llm_call_and_refresh_session_usage(
            &tid,
            Some(1),
            RequestKind::Chat,
            None,
            10,
            5,
            15,
            0,
            0,
            0.0,
            false,
            None,
        )
        .unwrap();
        // Usage rows use fixed-width millisecond RFC3339 timestamps. Use the
        // same representation for lexicographic SQL cutoff comparisons; a
        // whole-second timestamp sorts after fractional timestamps in that
        // second and would incorrectly delete the first row too.
        let cutoff = now_rfc3339_millis();
        std::thread::sleep(std::time::Duration::from_millis(5));
        db.persist_llm_call_and_refresh_session_usage(
            &tid,
            Some(2),
            RequestKind::Chat,
            None,
            20,
            10,
            30,
            0,
            0,
            0.0,
            false,
            None,
        )
        .unwrap();
        let before = db.get_session_usage(&tid).unwrap().unwrap();
        assert_eq!(before.total_tokens, 45);
        db.truncate_session_after(&tid, &cutoff, false).unwrap();
        let usage = db.list_session_llm_usage(&tid).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].step_number, Some(1));
        // Cumulative counters must shrink with the detail rows, not stay at 45.
        let after = db.get_session_usage(&tid).unwrap().unwrap();
        assert_eq!(after.prompt_tokens, 10);
        assert_eq!(after.completion_tokens, 5);
        assert_eq!(after.total_tokens, 15);
    }
}
