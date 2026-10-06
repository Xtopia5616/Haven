use std::sync::Arc;

use crate::db::Database;
use crate::recall::{MemoryHit, MemoryQuery, MemoryRecall, MemoryRetriever};
use crate::repositories::facts::Fact;

/// Async read port for typed memory recall.
///
/// Agent owns query preparation, provider calls, caching, candidate merging,
/// and prompt budgets. This store owns SQLite blocking-pool scheduling and
/// the shared visibility, scope, ranking, and recall policies.
#[derive(Clone)]
pub struct MemoryRecallStore {
    db: Arc<Database>,
}

impl MemoryRecallStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// Current process-local revision for memory-derived cache keys.
    ///
    /// This value is an atomic invalidation counter, not a SQLite read, so it
    /// remains synchronous like the underlying `Database` accessor.
    pub fn memory_revision(&self) -> u64 {
        self.db.memory_revision()
    }

    /// Read keyword candidates through the shared recall policy.
    pub async fn keyword_recall(&self, query: MemoryQuery) -> anyhow::Result<Vec<MemoryHit>> {
        self.db
            .run_blocking(move |db| MemoryRetriever::new(db).keyword(&query))
            .await
    }

    /// Read model-scoped vector candidates through the shared visibility,
    /// scope, and deterministic ordering policy.
    pub async fn vector_recall(
        &self,
        query: MemoryQuery,
        vector: Vec<f32>,
        model: String,
    ) -> anyhow::Result<Vec<MemoryHit>> {
        self.db
            .run_blocking(move |db| MemoryRetriever::new(db).vector(&query, &vector, &model))
            .await
    }

    /// Read the existing confidence-limited seed set for user facts, applying
    /// the common sensitive-row and provenance-snippet visibility policy.
    pub async fn visible_user_facts(&self, limit: usize) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(move |db| {
                let facts = db.get_facts_limited("user", limit)?;
                Ok(MemoryRetriever::filter_visible_facts(facts))
            })
            .await
    }

    /// Hydrate candidate facts by id and filter/redact them before they leave
    /// the Memory boundary. Database row order is preserved.
    pub async fn visible_facts_by_ids(&self, ids: Vec<String>) -> anyhow::Result<Vec<Fact>> {
        self.db
            .run_blocking(move |db| {
                let facts = db.get_facts_by_ids(&ids)?;
                Ok(MemoryRetriever::filter_visible_facts(facts))
            })
            .await
    }

    /// Execute a fully-scoped typed recall request with optional vector hits.
    /// Vector acquisition remains an Agent/provider concern; this method is
    /// the stable boundary for keyword fallback and hybrid result projection.
    pub async fn retrieve(
        &self,
        query: MemoryQuery,
        vector_hits: Option<Vec<MemoryHit>>,
    ) -> anyhow::Result<MemoryRecall> {
        self.db
            .run_blocking(move |db| MemoryRetriever::new(db).retrieve(&query, vector_hits))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embeddings::entity_kind;
    use crate::recall::{
        MemoryEntityKind, MemoryRecallEmptyReason, MemoryRecallMode, MemoryRecallSourceStatus,
    };

    fn store() -> (Arc<Database>, MemoryRecallStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = MemoryRecallStore::new(db.clone());
        (db, store)
    }

    #[tokio::test]
    async fn typed_recall_ports_preserve_visibility_scope_and_vector_ordering() {
        let (db, store) = store();
        let source_ref = crate::repositories::facts::FactSourceRef {
            message_id: "msg-recall-source".into(),
            snippet: "password=hidden".into(),
        };
        let safe = db
            .insert_fact_with_source_ref(
                "user",
                "likes",
                "Rust",
                "inferred",
                0.8,
                &[],
                Some(&source_ref),
                1.0,
            )
            .unwrap();
        let hidden = db
            .insert_fact("user", "api_key", "sk-hidden", "inferred", 1.0, &[])
            .unwrap();
        db.save_embedding(
            entity_kind::FACT,
            &safe.id,
            "model-a",
            &[1.0, 0.0],
            "user likes Rust",
        )
        .unwrap();
        // Simulate a legacy vector row predating the embedding write filter.
        db.save_embedding(
            entity_kind::FACT,
            &hidden.id,
            "model-a",
            &[1.0, 0.0],
            "user api_key sk-hidden",
        )
        .unwrap();

        let keyword = store
            .keyword_recall(MemoryQuery::new("Rust", MemoryEntityKind::Fact, 5).unwrap())
            .await
            .unwrap();
        assert_eq!(keyword.len(), 1);
        assert_eq!(keyword[0].entity_id, safe.id);

        let vector = store
            .vector_recall(
                MemoryQuery::new("programming", MemoryEntityKind::Fact, 5)
                    .unwrap()
                    .with_fact_subject(Some("user")),
                vec![1.0, 0.0],
                "model-a".into(),
            )
            .await
            .unwrap();
        assert_eq!(vector.len(), 1);
        assert_eq!(vector[0].entity_id, safe.id);
        assert_eq!(vector[0].text, "likes=Rust");

        let visible = store.visible_user_facts(40).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, safe.id);
        assert_eq!(
            visible[0].source_ref.as_ref().unwrap().snippet,
            "[redacted]"
        );
        let by_id = store
            .visible_facts_by_ids(vec![hidden.id.clone(), safe.id.clone()])
            .await
            .unwrap();
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].id, safe.id);
        assert_eq!(by_id[0].source_ref.as_ref().unwrap().snippet, "[redacted]");
    }

    #[tokio::test]
    async fn retrieve_preserves_keyword_fallback_and_empty_diagnostics() {
        let (db, store) = store();
        let fact = db
            .insert_fact("user", "uses", "SQLite", "user", 1.0, &[])
            .unwrap();
        let result = store
            .retrieve(
                MemoryQuery::new("SQLite", MemoryEntityKind::Fact, 5).unwrap(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.mode, MemoryRecallMode::Keyword);
        assert_eq!(result.hits[0].entity_id, fact.id);

        let empty = store
            .retrieve(
                MemoryQuery::new("missing", MemoryEntityKind::Fact, 5).unwrap(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(empty.empty_reason, Some(MemoryRecallEmptyReason::NoHits));
        assert_eq!(
            empty.diagnostics.unwrap().vector,
            MemoryRecallSourceStatus::NotConfigured
        );
    }

    #[test]
    fn memory_revision_tracks_memory_mutations() {
        let (db, store) = store();
        let before = store.memory_revision();

        db.insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();

        assert!(store.memory_revision() > before);
    }
}
