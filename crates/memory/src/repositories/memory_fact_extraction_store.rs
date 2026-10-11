use std::sync::Arc;

use crate::Database;
use crate::repositories::messages::Message;
use crate::repositories::session_steps::SessionStep;
use haven_common::types::is_canonical_id;

/// The persisted transcript projections used as input to ordinary session
/// fact extraction. Window selection remains an Agent policy.
#[derive(Debug, Clone)]
pub struct FactExtractionTranscript {
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
}

/// Persistence port for fact-extraction inputs and shared throttle state.
///
/// It owns transcript reads and extraction-attempt timestamps. Fact writes
/// and their ordinary cursor or summary completion marker are committed
/// through `MemoryFactStore` in one transaction.
#[derive(Clone)]
pub struct MemoryFactExtractionStore {
    db: Arc<Database>,
}

impl MemoryFactExtractionStore {
    #[cfg(feature = "test-support")]
    pub fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    fn from_database(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Read the shared wall-clock throttle timestamp used by ordinary and
    /// summary extraction. Callers decide whether it is still inside policy.
    pub async fn shared_extraction_last_attempt_timestamp(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let key = extraction_last_attempt_key(session_id)?;
        self.db.run_blocking(move |db| db.get_kv(&key)).await
    }

    /// Load message and execution-step projections used to build an extraction
    /// window, in the same blocking operation as the previous implementation.
    pub async fn load_ordinary_transcript(
        &self,
        session_id: &str,
    ) -> anyhow::Result<FactExtractionTranscript> {
        ensure_extraction_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let messages = db.list_session_messages(&session_id)?;
                let steps = db.list_session_steps(&session_id)?;
                Ok(FactExtractionTranscript { messages, steps })
            })
            .await
    }

    /// Read the last processed user-message id for ordinary extraction.
    pub async fn ordinary_extraction_cursor(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let key = ordinary_extraction_cursor_key(session_id)?;
        self.db
            .run_blocking(move |db| {
                let cursor = db.get_kv(&key)?;
                if let Some(message_id) = cursor.as_deref() {
                    anyhow::ensure!(
                        is_canonical_id(message_id, "msg"),
                        "invalid message id in ordinary extraction cursor"
                    );
                }
                Ok(cursor)
            })
            .await
    }

    /// Record an extraction attempt before the caller calls the model. The
    /// ordinary and summary paths intentionally share this timestamp/key.
    pub async fn stamp_shared_extraction_attempt(
        &self,
        session_id: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        let key = extraction_last_attempt_key(session_id)?;
        let timestamp = timestamp.to_owned();
        self.db
            .run_blocking(move |db| db.set_kv(&key, &timestamp))
            .await
    }
}

fn ensure_extraction_session_id(session_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(session_id, "ses"),
        "invalid session id for fact extraction"
    );
    Ok(())
}

fn extraction_last_attempt_key(session_id: &str) -> anyhow::Result<String> {
    ensure_extraction_session_id(session_id)?;
    Ok(format!("fact_extraction_last_run.{session_id}"))
}

fn ordinary_extraction_cursor_key(session_id: &str) -> anyhow::Result<String> {
    ensure_extraction_session_id(session_id)?;
    Ok(format!("fact_extraction.{session_id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transcript_port_reads_messages_and_step_observations() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("fact extraction transcript").unwrap();
        let user = db
            .add_message(
                &session.id,
                haven_common::types::CanonicalRole::User,
                "Use the checked path",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        let step = db
            .create_tool_step(
                &session.id,
                1,
                "shell",
                "{\"command\":\"pwd\"}",
                false,
                false,
                None,
                None,
            )
            .unwrap();
        db.complete_tool_step(&step.step_id, "C:/Workspace/Haven", true)
            .unwrap();

        let transcript = MemoryFactExtractionStore::new(db)
            .load_ordinary_transcript(&session.id)
            .await
            .unwrap();

        assert_eq!(transcript.messages.len(), 1);
        assert_eq!(transcript.messages[0].id, user.id);
        assert_eq!(transcript.messages[0].content, "Use the checked path");
        assert_eq!(transcript.steps.len(), 1);
        assert_eq!(transcript.steps[0].step_id, step.step_id);
        assert_eq!(
            transcript.steps[0].observation.as_deref(),
            Some("C:/Workspace/Haven")
        );
    }

    #[tokio::test]
    async fn transcript_port_propagates_missing_projection_tables() {
        let messages_db = Arc::new(Database::open_in_memory().unwrap());
        let messages_session = messages_db.create_session("missing messages").unwrap();
        messages_db
            .conn()
            .execute_batch("DROP TABLE messages")
            .unwrap();
        let error = MemoryFactExtractionStore::new(messages_db)
            .load_ordinary_transcript(&messages_session.id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no such table: messages"));

        let steps_db = Arc::new(Database::open_in_memory().unwrap());
        let steps_session = steps_db.create_session("missing steps").unwrap();
        steps_db
            .conn()
            .execute_batch("DROP TABLE session_steps")
            .unwrap();
        let error = MemoryFactExtractionStore::new(steps_db)
            .load_ordinary_transcript(&steps_session.id)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no such table: session_steps"));
    }

    #[tokio::test]
    async fn extraction_store_rejects_noncanonical_session_and_message_ids() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("extraction identity").unwrap();
        let store = MemoryFactExtractionStore::new(db.clone());
        let invalid_session_id = haven_common::types::new_id("msg");
        let invalid_message_id = haven_common::types::new_id("ses");

        assert!(
            store
                .shared_extraction_last_attempt_timestamp(&invalid_session_id)
                .await
                .is_err()
        );
        assert!(
            store
                .load_ordinary_transcript(&invalid_session_id)
                .await
                .is_err()
        );
        assert!(
            store
                .ordinary_extraction_cursor(&invalid_session_id)
                .await
                .is_err()
        );
        assert!(
            store
                .stamp_shared_extraction_attempt(&invalid_session_id, "2026-09-25T10:00:00Z")
                .await
                .is_err()
        );

        db.set_kv(
            &format!("fact_extraction.{}", session.id),
            &invalid_message_id,
        )
        .unwrap();
        assert!(store.ordinary_extraction_cursor(&session.id).await.is_err());
        assert!(
            db.get_kv(&format!("fact_extraction_last_run.{invalid_session_id}"))
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn ordinary_cursor_read_and_shared_attempt_timestamp_report_kv_errors() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("fact extraction kv errors").unwrap();
        let session_id = session.id.as_str();
        let store = MemoryFactExtractionStore::new(db.clone());

        assert_eq!(
            store.ordinary_extraction_cursor(session_id).await.unwrap(),
            None
        );
        assert_eq!(
            store
                .shared_extraction_last_attempt_timestamp(session_id)
                .await
                .unwrap(),
            None
        );
        store
            .stamp_shared_extraction_attempt(session_id, "2026-09-25T10:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            store
                .shared_extraction_last_attempt_timestamp(session_id)
                .await
                .unwrap(),
            Some("2026-09-25T10:00:00Z".into())
        );
        assert_eq!(
            store.ordinary_extraction_cursor(session_id).await.unwrap(),
            None
        );

        db.conn().execute_batch("DROP TABLE kv_store").unwrap();
        assert!(
            store
                .shared_extraction_last_attempt_timestamp(session_id)
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
        assert!(
            store
                .ordinary_extraction_cursor(session_id)
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
        assert!(
            store
                .stamp_shared_extraction_attempt(session_id, "2026-09-25T10:01:00Z")
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
    }
}
