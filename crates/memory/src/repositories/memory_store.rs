use std::sync::Arc;

use crate::Database;
use tokio_util::sync::CancellationToken;

/// Narrow durable persistence port for memory episodes and extraction outbox markers.
///
/// Agent decides which jobs to schedule. This store invokes the existing
/// database operations on SQLite's blocking pool without owning job policy.
#[derive(Clone)]
pub struct MemoryStore {
    db: Arc<Database>,
}

impl MemoryStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Persist one compaction episode and optionally enqueue its durable
    /// extraction marker in the same transaction. The underlying Database
    /// operation retains its idempotency and conflict behavior. Dropping this
    /// future cannot interrupt a write already running on Tokio's blocking
    /// pool.
    pub async fn persist_compaction_summary(
        &self,
        session_id: &str,
        summary: &str,
        episode_id: &str,
        enqueue_extraction: bool,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        let summary = summary.to_owned();
        let episode_id = episode_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.add_episode_with_pending_extraction(
                    &session_id,
                    &summary,
                    &episode_id,
                    enqueue_extraction,
                )
            })
            .await
    }

    /// Persist a fact-extraction outbox marker before publishing its live
    /// projection. This keeps marker writes on the cancellable SQLite
    /// blocking boundary while preserving the underlying Database contract.
    pub async fn enqueue_fact_extraction_cancellable(
        &self,
        session_id: &str,
        bypass_throttle: bool,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancellation.clone(), move |db| {
                db.enqueue_fact_extraction(&session_id, bypass_throttle)
            })
            .await
    }

    /// Synchronous compatibility path for construction-only callers that do
    /// not have a Tokio runtime in which the async port could schedule work.
    pub fn enqueue_fact_extraction_without_runtime(
        &self,
        session_id: &str,
        bypass_throttle: bool,
    ) -> anyhow::Result<()> {
        self.db.enqueue_fact_extraction(session_id, bypass_throttle)
    }

    /// Load durable fact-extraction jobs for live sessions.
    pub async fn pending_fact_extractions_cancellable(
        &self,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<Vec<(String, bool)>> {
        self.db
            .run_blocking_cancellable(cancellation.clone(), |db| db.pending_fact_extractions())
            .await
    }

    /// Load durable compaction-summary extraction jobs.
    pub async fn pending_summary_extractions_cancellable(
        &self,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<Vec<(String, String)>> {
        self.db
            .run_blocking_cancellable(cancellation.clone(), |db| db.pending_summary_extractions())
            .await
    }

    /// Persist a summary marker when its episode was already written by
    /// another Memory owner.
    pub async fn enqueue_summary_extraction_cancellable(
        &self,
        session_id: &str,
        episode_id: &str,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        let episode_id = episode_id.to_owned();
        self.db
            .run_blocking_cancellable(cancellation.clone(), move |db| {
                db.enqueue_summary_extraction(&session_id, &episode_id)
            })
            .await
    }

    /// Acknowledge a completed fact job without erasing a concurrent bypass
    /// upgrade. Ordinary jobs clear only ordinary markers.
    pub async fn clear_pending_fact_extraction_if_not_upgraded_cancellable(
        &self,
        session_id: &str,
        processed_bypass: bool,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancellation.clone(), move |db| {
                db.clear_pending_fact_extraction_if_not_upgraded(&session_id, processed_bypass)
            })
            .await
    }

    /// Read an episode's text while the summary outbox job is live.
    pub async fn episode_text_cancellable(
        &self,
        episode_id: &str,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<Option<String>> {
        let episode_id = episode_id.to_owned();
        self.db
            .run_blocking_cancellable(cancellation.clone(), move |db| db.episode_text(&episode_id))
            .await
    }

    /// Acknowledge one completed summary-extraction job.
    pub async fn clear_summary_extraction_cancellable(
        &self,
        session_id: &str,
        episode_id: &str,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        let episode_id = episode_id.to_owned();
        self.db
            .run_blocking_cancellable(cancellation.clone(), move |db| {
                db.clear_summary_extraction(&session_id, &episode_id)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persist_compaction_summary_commits_episode_and_pending_marker_idempotently() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("memory store summary").unwrap();
        let store = MemoryStore::new(db.clone());
        let episode_id = haven_common::types::new_id("msg");
        let summary = "A durable compaction summary that should be extracted.";

        store
            .persist_compaction_summary(&session.id, summary, &episode_id, true)
            .await
            .unwrap();
        store
            .persist_compaction_summary(&session.id, summary, &episode_id, true)
            .await
            .unwrap();

        let (count, content): (i64, String) = db
            .conn()
            .query_row(
                "SELECT COUNT(*), MIN(content) FROM memory_items WHERE id = ?1",
                rusqlite::params![episode_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(content, summary);
        assert_eq!(
            db.pending_summary_extractions().unwrap(),
            vec![(session.id.clone(), episode_id.clone())]
        );

        db.clear_summary_extraction(&session.id, &episode_id)
            .unwrap();
        store
            .persist_compaction_summary(&session.id, summary, &episode_id, true)
            .await
            .unwrap();
        assert_eq!(
            db.pending_summary_extractions().unwrap(),
            vec![(session.id, episode_id)]
        );
    }

    #[tokio::test]
    async fn persist_compaction_summary_without_enqueue_keeps_episode_and_rolls_back_conflict() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("memory store conflict").unwrap();
        let store = MemoryStore::new(db.clone());
        let episode_id = haven_common::types::new_id("msg");
        let original = "A persisted summary without extraction.";

        store
            .persist_compaction_summary(&session.id, original, &episode_id, false)
            .await
            .unwrap();
        assert!(db.pending_summary_extractions().unwrap().is_empty());

        let error = store
            .persist_compaction_summary(
                &session.id,
                "conflicting content must not replace the original",
                &episode_id,
                true,
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("already exists with different content")
        );

        let content: String = db
            .conn()
            .query_row(
                "SELECT content FROM memory_items WHERE id = ?1",
                rusqlite::params![episode_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(content, original);
        assert!(db.pending_summary_extractions().unwrap().is_empty());
    }

    #[tokio::test]
    async fn fact_outbox_ports_preserve_enqueue_upgrade_and_conditional_ack() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("memory store fact outbox").unwrap();
        let store = MemoryStore::new(db.clone());
        let cancellation = CancellationToken::new();

        store
            .enqueue_fact_extraction_cancellable(&session.id, false, &cancellation)
            .await
            .unwrap();
        store
            .enqueue_fact_extraction_cancellable(&session.id, true, &cancellation)
            .await
            .unwrap();
        assert_eq!(
            store
                .pending_fact_extractions_cancellable(&cancellation)
                .await
                .unwrap(),
            vec![(session.id.clone(), true)]
        );

        store
            .clear_pending_fact_extraction_if_not_upgraded_cancellable(
                &session.id,
                false,
                &cancellation,
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .pending_fact_extractions_cancellable(&cancellation)
                .await
                .unwrap(),
            vec![(session.id.clone(), true)]
        );
        store
            .clear_pending_fact_extraction_if_not_upgraded_cancellable(
                &session.id,
                true,
                &cancellation,
            )
            .await
            .unwrap();
        assert!(
            store
                .pending_fact_extractions_cancellable(&cancellation)
                .await
                .unwrap()
                .is_empty()
        );

        let error = store
            .clear_pending_fact_extraction_if_not_upgraded_cancellable("  ", false, &cancellation)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "session id is required");

        let error = store
            .enqueue_fact_extraction_cancellable("  ", false, &cancellation)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "session id is required");
    }

    #[tokio::test]
    async fn pending_outbox_ports_return_rows_and_propagate_database_errors() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("memory store pending rows").unwrap();
        let store = MemoryStore::new(db.clone());
        let cancellation = CancellationToken::new();
        db.enqueue_fact_extraction(&session.id, true).unwrap();
        store
            .enqueue_summary_extraction_cancellable(&session.id, "msg-episode", &cancellation)
            .await
            .unwrap();

        assert_eq!(
            store
                .pending_fact_extractions_cancellable(&cancellation)
                .await
                .unwrap(),
            vec![(session.id.clone(), true)]
        );
        assert_eq!(
            store
                .pending_summary_extractions_cancellable(&cancellation)
                .await
                .unwrap(),
            vec![(session.id.clone(), "msg-episode".to_owned())]
        );

        let error = store
            .enqueue_summary_extraction_cancellable(&session.id, "", &cancellation)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "episode id is required");

        db.conn().execute_batch("DROP TABLE kv_store").unwrap();
        assert!(
            store
                .pending_fact_extractions_cancellable(&cancellation)
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
        assert!(
            store
                .pending_summary_extractions_cancellable(&cancellation)
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
        assert!(
            store
                .enqueue_summary_extraction_cancellable("ses-valid", "msg-valid", &cancellation)
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: kv_store")
        );
    }

    #[tokio::test]
    async fn episode_read_and_summary_ack_keep_missing_and_error_semantics() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("memory store summary outbox").unwrap();
        let episode_id = "msg-summary-store-port".to_owned();
        db.add_episode_with_pending_extraction(
            &session.id,
            "A durable summary whose text is read through MemoryStore.",
            &episode_id,
            true,
        )
        .unwrap();
        let store = MemoryStore::new(db.clone());
        let cancellation = CancellationToken::new();

        assert_eq!(
            store
                .episode_text_cancellable(&episode_id, &cancellation)
                .await
                .unwrap()
                .as_deref(),
            Some("A durable summary whose text is read through MemoryStore.")
        );
        assert_eq!(
            store
                .episode_text_cancellable("msg-missing", &cancellation)
                .await
                .unwrap(),
            None
        );
        store
            .clear_summary_extraction_cancellable(&session.id, &episode_id, &cancellation)
            .await
            .unwrap();
        assert!(
            store
                .pending_summary_extractions_cancellable(&cancellation)
                .await
                .unwrap()
                .is_empty()
        );

        let error = store
            .clear_summary_extraction_cancellable(&session.id, "", &cancellation)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "episode id is required");

        db.conn().execute_batch("DROP TABLE memory_items").unwrap();
        assert!(
            store
                .episode_text_cancellable(&episode_id, &cancellation)
                .await
                .unwrap_err()
                .to_string()
                .contains("no such table: memory_items")
        );
    }

    #[tokio::test]
    async fn cancelled_fact_marker_ack_does_not_remove_a_locked_durable_marker() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "haven-memory-store-cancel-{}-{timestamp}.db",
            std::process::id()
        ));
        let db = Arc::new(Database::open(&path).unwrap());
        let session = db.create_session("memory store cancellation").unwrap();
        db.enqueue_fact_extraction(&session.id, false).unwrap();
        let store = MemoryStore::new(db.clone());
        let cancellation = CancellationToken::new();

        let lock = db.conn();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        let clear = tokio::spawn({
            let store = store.clone();
            let session_id = session.id.clone();
            let cancellation = cancellation.clone();
            async move {
                store
                    .clear_pending_fact_extraction_if_not_upgraded_cancellable(
                        &session_id,
                        false,
                        &cancellation,
                    )
                    .await
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), clear)
            .await
            .expect("cancelled SQLite marker acknowledgement should return promptly")
            .unwrap();
        assert!(result.is_err());
        lock.execute_batch("ROLLBACK").unwrap();
        drop(lock);

        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id, false)]
        );
        drop(store);
        drop(db);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }
}
