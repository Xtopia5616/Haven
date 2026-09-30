use std::sync::Arc;

use crate::Database;
use crate::repositories::messages::Message;
use crate::repositories::session_steps::SessionStep;

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
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Read the shared wall-clock throttle timestamp used by ordinary and
    /// summary extraction. Callers decide whether it is still inside policy.
    pub async fn shared_extraction_last_attempt_timestamp(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let key = format!("fact_extraction_last_run.{session_id}");
        self.db.run_blocking(move |db| db.get_kv(&key)).await
    }

    /// Load message and execution-step projections used to build an extraction
    /// window, in the same blocking operation as the previous implementation.
    pub async fn load_ordinary_transcript(
        &self,
        session_id: &str,
    ) -> anyhow::Result<FactExtractionTranscript> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let messages = db.get_session_messages(&session_id)?;
                let steps = db.get_session_steps(&session_id)?;
                Ok(FactExtractionTranscript { messages, steps })
            })
            .await
    }

    /// Read the last processed user-message id for ordinary extraction.
    pub async fn ordinary_extraction_cursor(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<String>> {
        let key = format!("fact_extraction.{session_id}");
        self.db.run_blocking(move |db| db.get_kv(&key)).await
    }

    /// Record an extraction attempt before the caller calls the model. The
    /// ordinary and summary paths intentionally share this timestamp/key.
    pub async fn stamp_shared_extraction_attempt(
        &self,
        session_id: &str,
        timestamp: &str,
    ) -> anyhow::Result<()> {
        let key = format!("fact_extraction_last_run.{session_id}");
        let timestamp = timestamp.to_owned();
        self.db
            .run_blocking(move |db| db.set_kv(&key, &timestamp))
            .await
    }
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
                "user",
                "Use the checked path",
                Some("text"),
                None,
            )
            .unwrap();
        let step = db
            .create_action_step(
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
        db.complete_action_step(&step.id, "C:/Workspace/Haven", true)
            .unwrap();

        let transcript = MemoryFactExtractionStore::new(db)
            .load_ordinary_transcript(&session.id)
            .await
            .unwrap();

        assert_eq!(transcript.messages.len(), 1);
        assert_eq!(transcript.messages[0].id, user.id);
        assert_eq!(transcript.messages[0].content, "Use the checked path");
        assert_eq!(transcript.steps.len(), 1);
        assert_eq!(transcript.steps[0].id, step.id);
        assert_eq!(
            transcript.steps[0].observation.as_deref(),
            Some("C:/Workspace/Haven")
        );
    }

    #[tokio::test]
    async fn transcript_port_propagates_missing_projection_tables() {
        let messages_db = Arc::new(Database::open_in_memory().unwrap());
        messages_db
            .conn()
            .execute_batch("DROP TABLE messages")
            .unwrap();
        let error = MemoryFactExtractionStore::new(messages_db)
            .load_ordinary_transcript("ses-missing-messages")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no such table: messages"));

        let steps_db = Arc::new(Database::open_in_memory().unwrap());
        steps_db
            .conn()
            .execute_batch("DROP TABLE session_steps")
            .unwrap();
        let error = MemoryFactExtractionStore::new(steps_db)
            .load_ordinary_transcript("ses-missing-steps")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no such table: session_steps"));
    }

    #[tokio::test]
    async fn ordinary_cursor_read_and_shared_attempt_timestamp_report_kv_errors() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = MemoryFactExtractionStore::new(db.clone());

        assert_eq!(
            store.ordinary_extraction_cursor("ses-state").await.unwrap(),
            None
        );
        assert_eq!(
            store
                .shared_extraction_last_attempt_timestamp("ses-state")
                .await
                .unwrap(),
            None
        );
        store
            .stamp_shared_extraction_attempt("ses-state", "2026-09-25T10:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            store
                .shared_extraction_last_attempt_timestamp("ses-state")
                .await
                .unwrap(),
            Some("2026-09-25T10:00:00Z".into())
        );
        assert_eq!(
            store.ordinary_extraction_cursor("ses-state").await.unwrap(),
            None
        );

        db.conn().execute_batch("DROP TABLE kv_store").unwrap();
        assert!(
            store
                .shared_extraction_last_attempt_timestamp("ses-state")
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
        assert!(
            store
                .ordinary_extraction_cursor("ses-state")
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
        assert!(
            store
                .stamp_shared_extraction_attempt("ses-state", "2026-09-25T10:01:00Z")
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
    }
}
