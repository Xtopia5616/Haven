use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::Database;
use crate::repositories::facts::ContradictionCandidate;
pub use crate::repositories::facts::PredicateCount;

/// Typed persistence boundary for scheduled memory maintenance persistence.
///
/// Each method schedules exactly one existing repository operation on the
/// SQLite blocking pool. The caller owns maintenance order, LLM policy,
/// logging, best-effort continuation, and aggregate error policy.
#[derive(Clone)]
pub struct MemoryMaintenanceStore {
    db: Arc<Database>,
}

impl MemoryMaintenanceStore {
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

    async fn run<T, F>(
        &self,
        cancellation: Option<&CancellationToken>,
        operation: F,
    ) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Database) -> anyhow::Result<T> + Send + 'static,
    {
        match cancellation {
            Some(cancellation) => {
                anyhow::ensure!(
                    !cancellation.is_cancelled(),
                    "memory maintenance cancelled before database work started"
                );
                self.db
                    .run_blocking_cancellable(cancellation.clone(), operation)
                    .await
            }
            None => self.db.run_blocking(operation).await,
        }
    }

    /// Collapse duplicate fact triples, retaining the repository's selected
    /// keeper and merged tags.
    pub async fn dedup_facts(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, |db| db.dedup_facts()).await
    }

    /// Delete facts whose predicate or object contains credential-like data.
    pub async fn delete_sensitive_facts(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, |db| db.delete_sensitive_facts())
            .await
    }

    /// Apply the deterministic rule-based contradiction keeper before stale
    /// low-confidence facts are flushed.
    pub async fn resolve_contradictions(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, |db| db.resolve_contradictions())
            .await
    }

    /// Delete old facts whose effective confidence falls below the caller's
    /// maintenance threshold.
    pub async fn flush_low_confidence(
        &self,
        threshold: f64,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, move |db| db.flush_low_confidence(threshold))
            .await
    }

    /// Remove vector and LSH rows whose owning fact or episode has been
    /// deleted. Embedding generation and catch-up remain separate.
    pub async fn prune_orphaned_embeddings(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, |db| db.prune_orphaned_embeddings())
            .await
    }

    /// Remove fact/summary extraction cursors, throttle stamps, durable
    /// pending markers, and memory-event cursors belonging to deleted sessions.
    /// Live cursor reads and advances remain with their domain stores.
    pub async fn cleanup_orphan_extraction_cursors(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, |db| db.cleanup_orphan_extraction_cursors())
            .await
    }

    /// Normalize empty source record references while retaining opaque
    /// transcript IDs and FK-managed episode references.
    pub async fn cleanup_orphan_source_refs(
        &self,
        cancellation: Option<&CancellationToken>,
    ) -> anyhow::Result<u64> {
        self.run(cancellation, |db| db.cleanup_orphan_source_refs())
            .await
    }

    /// List residual contradiction groups that the optional LLM arbitrator
    /// may consider. Candidate visibility filtering and proposal policy stay
    /// with the caller.
    pub async fn list_ambiguous_contradictions(
        &self,
    ) -> anyhow::Result<Vec<ContradictionCandidate>> {
        self.run(None, |db| db.list_ambiguous_contradictions())
            .await
    }

    /// Demote the selected fact ids after the caller has applied its policy
    /// gate. An empty list retains the repository's zero-count behavior.
    pub async fn demote_fact_ids(&self, ids: Vec<String>) -> anyhow::Result<u64> {
        self.run(None, move |db| db.demote_fact_ids(ids)).await
    }

    /// Return exact predicate counts for maintenance policy decisions.
    pub async fn list_predicate_counts(&self) -> anyhow::Result<Vec<PredicateCount>> {
        self.run(None, |db| db.list_predicate_counts()).await
    }

    /// Rewrite one predicate and run the repository's existing duplicate
    /// collapse. Each proposal is an independent store call.
    pub async fn rewrite_predicate(&self, from: &str, to: &str) -> anyhow::Result<u64> {
        let from = from.to_string();
        let to = to.to_string();
        self.run(None, move |db| db.rewrite_predicate(&from, &to))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::MemoryMaintenanceStore;
    use crate::Database;
    use crate::repositories::facts::{ContradictionCandidate, ContradictionKind};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn fixture() -> (Arc<Database>, MemoryMaintenanceStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = MemoryMaintenanceStore::new(db.clone());
        (db, store)
    }

    #[tokio::test]
    async fn deterministic_fact_operations_return_repository_counts() {
        let (db, store) = fixture();
        db.insert_fact("user", "likes", "Rust", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "sk-test-secret", "inferred", 0.9, &[])
            .unwrap();
        let stale = db
            .insert_fact("user", "likes", "Stale", "inferred", 0.1, &[])
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET created_at = '2000-01-01T00:00:00Z',
                                  last_seen_at = '2000-01-01T00:00:00Z'
                 WHERE id = ?1",
                rusqlite::params![stale.id],
            )
            .unwrap();
        db.insert_fact("user", "works_at", "Acme", "inferred", 0.85, &[])
            .unwrap();
        db.insert_fact("user", "works_at", "BetaCorp", "user", 1.0, &[])
            .unwrap();
        let source_ref_fact = db
            .insert_fact("user", "likes", "Go", "inferred", 0.8, &[])
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET provenance_record_id = '  ' WHERE id = ?1",
                rusqlite::params![source_ref_fact.id],
            )
            .unwrap();

        assert_eq!(store.dedup_facts(None).await.unwrap(), 1);
        assert_eq!(store.delete_sensitive_facts(None).await.unwrap(), 1);
        assert_eq!(store.resolve_contradictions(None).await.unwrap(), 1);
        assert_eq!(store.flush_low_confidence(0.3, None).await.unwrap(), 1);
        assert_eq!(store.cleanup_orphan_source_refs(None).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn embedding_and_cursor_cleanup_return_deleted_counts() {
        let (db, store) = fixture();
        db.conn()
            .execute(
                "INSERT INTO memory_embeddings
                    (entity_type, entity_id, model, vector, text)
                 VALUES ('fact', 'fact-orphan', 'model', x'00000000', 'orphan')",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO embedding_lsh (entity_type, entity_id, model, bucket)
                 VALUES ('fact', 'fact-orphan', 'model', 1)",
                [],
            )
            .unwrap();
        db.set_kv("fact_extraction.ses-deadbeef", "msg-deadbeef")
            .unwrap();

        assert_eq!(store.prune_orphaned_embeddings(None).await.unwrap(), 1);
        assert_eq!(
            store.cleanup_orphan_extraction_cursors(None).await.unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn missing_tables_propagate_and_later_store_calls_still_run() {
        let (db, store) = fixture();
        db.set_kv("fact_extraction.ses-deadbeef", "msg-deadbeef")
            .unwrap();
        db.conn().execute_batch("DROP TABLE facts").unwrap();

        let error = store.dedup_facts(None).await.unwrap_err();
        assert!(error.to_string().contains("no such table: facts"));
        assert_eq!(
            store.cleanup_orphan_extraction_cursors(None).await.unwrap(),
            1,
            "an earlier repository failure must not prevent a later typed operation"
        );
    }

    #[tokio::test]
    async fn cancelled_operation_does_not_start_database_work() {
        let (db, store) = fixture();
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.8, &[])
            .unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = store.dedup_facts(Some(&cancellation)).await.unwrap_err();
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(db.list_facts_by_subject("user").unwrap().len(), 2);
    }

    #[tokio::test]
    async fn llm_maintenance_results_are_typed_dtos_with_serde_round_trip() {
        let (db, store) = fixture();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Rust", "inferred", 0.8, &[])
            .unwrap();

        let candidates = store.list_ambiguous_contradictions().await.unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].kind, ContradictionKind::Polarity);
        let encoded_candidates = serde_json::to_value(&candidates).unwrap();
        let decoded_candidates: Vec<ContradictionCandidate> =
            serde_json::from_value(encoded_candidates.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(decoded_candidates).unwrap(),
            encoded_candidates
        );

        let counts = store.list_predicate_counts().await.unwrap();
        assert_eq!(
            counts,
            vec![
                super::PredicateCount {
                    predicate: "dislikes".into(),
                    row_count: 1,
                },
                super::PredicateCount {
                    predicate: "likes".into(),
                    row_count: 1,
                },
            ]
        );
        let encoded_counts = serde_json::to_value(&counts).unwrap();
        let decoded_counts: Vec<super::PredicateCount> =
            serde_json::from_value(encoded_counts.clone()).unwrap();
        assert_eq!(decoded_counts, counts);
        assert_eq!(
            serde_json::to_value(decoded_counts).unwrap(),
            encoded_counts
        );
    }

    #[tokio::test]
    async fn llm_maintenance_store_propagates_database_errors_and_empty_demote_is_zero() {
        let (db, store) = fixture();
        assert_eq!(store.demote_fact_ids(Vec::new()).await.unwrap(), 0);

        db.conn().execute_batch("DROP TABLE facts").unwrap();
        for error in [
            store.list_ambiguous_contradictions().await.unwrap_err(),
            store.list_predicate_counts().await.unwrap_err(),
            store
                .demote_fact_ids(vec!["fact-00000000000000000000000000000000".into()])
                .await
                .unwrap_err(),
            store
                .rewrite_predicate("workspace", "project_path")
                .await
                .unwrap_err(),
        ] {
            assert!(error.to_string().contains("no such table: facts"));
        }
    }
}
