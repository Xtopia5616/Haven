use std::sync::Arc;

use crate::Database;

/// Narrow durable-write port for memory-owned episode summaries.
///
/// Agent decides when a compaction summary is eligible for extraction. This
/// store only schedules the existing atomic episode and optional outbox-marker
/// transaction on SQLite's blocking pool.
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
}
