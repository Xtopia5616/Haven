use crate::db::Database;
use crate::repositories::session_events::ensure_session_id;
use chrono::Utc;
use rusqlite::OptionalExtension;

/// In-flight text checkpoint persisted outside the canonical transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialMessageCheckpoint {
    pub content: String,
    pub updated_at: String,
    /// Highest durable message ingress sequence visible when this checkpoint
    /// was stored. This is scratch ordering metadata, not a rollback watermark.
    pub ingress_seq: i64,
}

/// Scratch storage for in-flight streamed text. While an LLM response is
/// streaming, `stream_llm_step` periodically checkpoints the accumulated
/// text here so a crash or user stop does not lose everything the user
/// already saw. The row is consumed (promoted to a real `messages` row or
/// deleted) when:
///
/// - a real transcript, ingress seed, recovery partial, or terminal ToolRun-result message
///   supersedes it,
/// - the session is ended (`end_session` promotes it into history),
/// - the app crashed and the session is finalized to `error` at startup,
/// - the session itself is deleted (FK cascade).
///
/// Never read by the canonical transcript flows; the row lives outside
/// `messages`, so no compaction-style filtering is needed.
impl Database {
    /// Upsert the current partial stream text for a session.
    pub fn upsert_partial_message(&self, session_id: &str, content: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.try_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<()> {
            let ingress_seq = last_message_ingress_seq(&conn, session_id)?;
            conn.execute(
                "INSERT INTO partial_messages (session_id, content, updated_at, ingress_seq)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(session_id) DO UPDATE SET
                     content = excluded.content,
                     updated_at = excluded.updated_at,
                     ingress_seq = excluded.ingress_seq",
                rusqlite::params![session_id, content, now, ingress_seq],
            )?;
            Ok(())
        })();
        finish_write_transaction(&conn, result)?;
        Ok(())
    }

    /// Return the in-flight text checkpoint for a session, if any.
    #[cfg(any(test, feature = "test-support"))]
    pub fn get_partial_message(&self, session_id: &str) -> Option<PartialMessageCheckpoint> {
        let conn = self
            .conn()
            .expect("test database connection checkout failed");
        conn.query_row(
            "SELECT content, updated_at, ingress_seq FROM partial_messages WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| {
                Ok(PartialMessageCheckpoint {
                    content: row.get(0)?,
                    updated_at: row.get(1)?,
                    ingress_seq: row.get(2)?,
                })
            },
        )
        .ok()
    }

    /// Drop the partial row for a session (superseded by a real message, or the
    /// stream was discarded). No-op when no row exists.
    pub fn delete_partial_message(&self, session_id: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let conn = self.try_conn()?;
        conn.execute(
            "DELETE FROM partial_messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }

    /// Atomically promote the partial row into a real assistant message.
    /// Skips (without inserting) when:
    /// - no partial row exists, or it holds only whitespace,
    /// - a real message was persisted AFTER the last checkpoint (the
    ///   stream's text already reached the message stream) — promoting
    ///   then would duplicate it.
    ///
    /// Returns `true` when a message was inserted. Single blocking round
    /// trip; used by ses-end promotion and the startup orphan finalizer.
    pub fn promote_partial_message(&self, session_id: &str) -> anyhow::Result<bool> {
        ensure_session_id(session_id)?;
        let conn = self.try_conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<bool> {
            let checkpoint = conn
                .query_row(
                    "SELECT content, updated_at, ingress_seq
                     FROM partial_messages WHERE session_id = ?1",
                    rusqlite::params![session_id],
                    |row| {
                        Ok(PartialMessageCheckpoint {
                            content: row.get(0)?,
                            updated_at: row.get(1)?,
                            ingress_seq: row.get(2)?,
                        })
                    },
                )
                .optional()?;
            let Some(checkpoint) = checkpoint else {
                return Ok(false);
            };
            let superseded = last_message_ingress_seq(&conn, session_id)? > checkpoint.ingress_seq;
            if checkpoint.content.trim().is_empty() || superseded {
                conn.execute(
                    "DELETE FROM partial_messages WHERE session_id = ?1",
                    rusqlite::params![session_id],
                )?;
                return Ok(false);
            }
            let message_id = haven_common::types::new_id("msg");
            Database::insert_message_row_in_transaction(
                &conn,
                session_id,
                haven_common::types::CanonicalRole::Assistant,
                checkpoint.content.trim(),
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
                &[],
                false,
                &message_id,
                &[],
            )?;
            let deleted = conn.execute(
                "DELETE FROM partial_messages WHERE session_id = ?1",
                rusqlite::params![session_id],
            )?;
            anyhow::ensure!(
                deleted == 1,
                "partial message checkpoint disappeared during promotion"
            );
            Ok(true)
        })();
        let promoted = finish_write_transaction(&conn, result)?;
        drop(conn);
        if promoted {
            self.cache_invalidate_messages(session_id);
        }
        Ok(promoted)
    }
}

fn last_message_ingress_seq(conn: &rusqlite::Connection, session_id: &str) -> anyhow::Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(ingress_seq), 0) FROM messages WHERE session_id = ?1",
        rusqlite::params![session_id],
        |row| row.get(0),
    )?)
}

fn finish_write_transaction<T>(
    conn: &rusqlite::Connection,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::db::Database;
    use crate::repositories::session_events::SessionStore;
    use std::sync::Arc;

    fn test_db() -> Database {
        Database::open_in_memory().expect("create in-memory db")
    }

    #[test]
    fn upsert_get_cycle() {
        let db = test_db();
        let session_id = db.create_session("input").unwrap().session_id;
        assert!(db.get_partial_message(&session_id).is_none());
        db.upsert_partial_message(&session_id, "partial one")
            .unwrap();
        db.upsert_partial_message(&session_id, "partial two")
            .unwrap();
        let checkpoint = db
            .get_partial_message(&session_id)
            .expect("partial exists after upsert");
        assert_eq!(checkpoint.content, "partial two");
        assert_eq!(checkpoint.ingress_seq, 0);
    }

    #[test]
    fn delete_partial_message_clears_row() {
        let db = test_db();
        let session_id = db.create_session("input").unwrap().session_id;
        db.upsert_partial_message(&session_id, "hello").unwrap();
        db.delete_partial_message(&session_id).unwrap();
        assert!(db.get_partial_message(&session_id).is_none());
    }

    #[test]
    fn partial_message_database_apis_reject_noncanonical_ids_without_mutation() {
        let db = test_db();
        let invalid_session_id = "ses-short";
        {
            let conn = db.conn().expect("test database connection checkout failed");
            conn.execute(
                "INSERT INTO sessions (id, input_text) VALUES (?1, 'invalid legacy row')",
                [invalid_session_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO partial_messages (session_id, content, updated_at)
                 VALUES (?1, 'preserve this checkpoint', '2026-10-13T00:00:00Z')",
                [invalid_session_id],
            )
            .unwrap();
        }

        assert!(
            db.upsert_partial_message(invalid_session_id, "must not overwrite")
                .is_err()
        );
        assert!(db.delete_partial_message(invalid_session_id).is_err());
        assert!(db.promote_partial_message(invalid_session_id).is_err());

        assert_eq!(
            db.get_partial_message(invalid_session_id)
                .expect("invalid-id checkpoint remains untouched")
                .content,
            "preserve this checkpoint"
        );
        let message_count: i64 = db
            .conn()
            .expect("test database connection checkout failed")
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                [invalid_session_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(message_count, 0);
    }

    #[test]
    fn promote_partial_creates_real_message() {
        let db = test_db();
        let session_id = db.create_session("input").unwrap().session_id;
        db.upsert_partial_message(&session_id, "streamed reply")
            .unwrap();
        assert!(db.promote_partial_message(&session_id).unwrap());
        let msgs = db.list_session_messages(&session_id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, haven_common::types::CanonicalRole::Assistant);
        assert_eq!(msgs[0].content, "streamed reply");
        // The row is consumed: a second promote is a no-op.
        assert!(!db.promote_partial_message(&session_id).unwrap());
    }

    #[test]
    fn promote_partial_skips_empty_and_superseded() {
        let db = test_db();
        let session_id = db.create_session("input").unwrap().session_id;
        // Whitespace-only partial: nothing to promote.
        db.upsert_partial_message(&session_id, "   ").unwrap();
        assert!(!db.promote_partial_message(&session_id).unwrap());
        // A real message written after the last checkpoint supersedes it.
        db.upsert_partial_message(&session_id, "older stream text")
            .unwrap();
        db.add_message_full(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "newer real message",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
            &[],
            false,
            None,
        )
        .unwrap();
        assert!(!db.promote_partial_message(&session_id).unwrap());
        let msgs = db.list_session_messages(&session_id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "newer real message");
    }

    #[test]
    fn failed_partial_promotion_keeps_checkpoint_and_retries_atomically() {
        let db = test_db();
        let session_id = db.create_session("partial retry").unwrap().session_id;
        db.upsert_partial_message(&session_id, "preserve this reply")
            .unwrap();
        {
            let conn = db.conn().expect("test database connection checkout failed");
            conn.execute_batch(
                "CREATE TRIGGER fail_partial_promotion
                 BEFORE INSERT ON messages
                 WHEN NEW.role = 'assistant'
                 BEGIN SELECT RAISE(ABORT, 'forced partial insert failure'); END;",
            )
            .unwrap();
        }

        assert!(db.promote_partial_message(&session_id).is_err());
        assert_eq!(
            db.get_partial_message(&session_id)
                .expect("failed insert rolls checkpoint back")
                .content,
            "preserve this reply"
        );
        assert!(db.list_session_messages(&session_id).unwrap().is_empty());
        assert_eq!(db.get_last_message_ingress_seq(&session_id), 0);

        db.conn()
            .expect("test database connection checkout failed")
            .execute_batch("DROP TRIGGER fail_partial_promotion")
            .unwrap();
        assert!(db.promote_partial_message(&session_id).unwrap());
        assert!(db.get_partial_message(&session_id).is_none());
        assert_eq!(
            db.list_session_messages(&session_id).unwrap()[0].content,
            "preserve this reply"
        );
    }

    #[tokio::test]
    async fn session_store_partial_stream_port_preserves_checkpoint_and_promote_semantics() {
        let db = Arc::new(test_db());
        let session_id = db.create_session("input").unwrap().session_id;
        let store = SessionStore::new(db.clone());
        let initial_last_msg_at = store.cursor(&session_id).unwrap().last_msg_at;

        assert!(!store.promote_partial_stream(&session_id).await.unwrap());
        store.discard_partial_stream(&session_id).await.unwrap();

        store
            .upsert_partial_stream(&session_id, "  \t ")
            .await
            .unwrap();
        assert_eq!(
            store.cursor(&session_id).unwrap().last_msg_at,
            initial_last_msg_at,
            "a scratch checkpoint must not advance the message projection clock"
        );
        assert!(!store.promote_partial_stream(&session_id).await.unwrap());

        store
            .upsert_partial_stream(&session_id, "  streamed reply  ")
            .await
            .unwrap();
        assert!(store.promote_partial_stream(&session_id).await.unwrap());
        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content, "streamed reply");
        assert!(store.cursor(&session_id).unwrap().last_msg_at.is_some());
        assert!(!store.promote_partial_stream(&session_id).await.unwrap());
    }
}
