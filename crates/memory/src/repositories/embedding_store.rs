use std::sync::Arc;

use crate::Database;
use crate::embeddings::entity_kind;
use crate::recall::MemoryRetriever;

/// Closed set of entities that can own persisted memory embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEmbeddingEntity {
    Fact,
    Episode,
}

impl MemoryEmbeddingEntity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fact => entity_kind::FACT,
            Self::Episode => entity_kind::EPISODE,
        }
    }
}

/// One visible row that is missing an embedding for the requested model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingMemoryEmbedding {
    pub entity: MemoryEmbeddingEntity,
    pub entity_id: String,
    pub text: String,
}

/// One provider vector ready for persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryEmbeddingVector {
    pub entity: MemoryEmbeddingEntity,
    pub entity_id: String,
    pub text: String,
    pub vector: Vec<f32>,
}

/// A failed individual vector save. Other rows in the same batch are still
/// attempted, matching the existing best-effort persistence behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEmbeddingSaveFailure {
    pub entity: MemoryEmbeddingEntity,
    pub entity_id: String,
    pub error: String,
}

/// Result of attempting all vectors in one provider batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEmbeddingSaveReport {
    pub attempted: usize,
    pub failures: Vec<MemoryEmbeddingSaveFailure>,
}

/// Async persistence port for Agent-owned embedding lifecycle orchestration.
///
/// This store owns SQLite blocking-pool scheduling and the typed read/write
/// operations needed by the embedding index. It does not choose a provider,
/// batch provider requests, or decide when maintenance runs.
#[derive(Clone)]
pub struct MemoryEmbeddingStore {
    db: Arc<Database>,
}

impl MemoryEmbeddingStore {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// List vector-space identities currently represented by live rows.
    pub async fn list_models(&self) -> anyhow::Result<Vec<String>> {
        self.db.run_blocking(|db| db.list_embedding_models()).await
    }

    /// Clear vectors and their LSH side table after the caller detects a
    /// model or dimension transition.
    pub async fn clear_embeddings(&self) -> anyhow::Result<u64> {
        self.db.run_blocking(|db| db.clear_embeddings()).await
    }

    /// Read the bounded missing-item backlogs and their source text, dropping
    /// sensitive content before it can cross into the Agent/provider layer.
    /// Fact and episode ordering and per-domain backlog limits come directly
    /// from the existing Database queries.
    pub async fn pending_embeddings(
        &self,
        model: String,
    ) -> anyhow::Result<Vec<PendingMemoryEmbedding>> {
        self.db
            .run_blocking(move |db| {
                let mut pending = Vec::new();
                for entity in [MemoryEmbeddingEntity::Fact, MemoryEmbeddingEntity::Episode] {
                    let entity_type = entity.as_str();
                    for entity_id in db.missing_embedding_ids(entity_type, &model)? {
                        let text = match entity {
                            MemoryEmbeddingEntity::Fact => db.fact_text_by_id(&entity_id)?,
                            MemoryEmbeddingEntity::Episode => db.episode_text(&entity_id)?,
                        };
                        if let Some(text) = text {
                            if MemoryRetriever::visible_text(&text) {
                                pending.push(PendingMemoryEmbedding {
                                    entity,
                                    entity_id,
                                    text,
                                });
                            } else {
                                tracing::debug!(
                                    entity_type,
                                    entity_id = %entity_id,
                                    "skipping sensitive memory from embedding provider"
                                );
                            }
                        }
                    }
                }
                Ok(pending)
            })
            .await
    }

    /// Read all persisted vector dimensions for a vector-space identity.
    pub async fn list_dimensions(&self, model: String) -> anyhow::Result<Vec<usize>> {
        self.db
            .run_blocking(move |db| db.list_embedding_dimensions(&model))
            .await
    }

    /// Save a provider batch. A stale or invalid row does not prevent other
    /// rows from being saved; callers retain the existing per-row warning
    /// behavior through the returned typed report.
    pub async fn save_batch(
        &self,
        model: String,
        vectors: Vec<MemoryEmbeddingVector>,
    ) -> anyhow::Result<MemoryEmbeddingSaveReport> {
        self.db
            .run_blocking(move |db| {
                let attempted = vectors.len();
                let mut failures = Vec::new();
                for item in vectors {
                    if let Err(error) = db.save_embedding(
                        item.entity.as_str(),
                        &item.entity_id,
                        &model,
                        &item.vector,
                        &item.text,
                    ) {
                        failures.push(MemoryEmbeddingSaveFailure {
                            entity: item.entity,
                            entity_id: item.entity_id,
                            error: error.to_string(),
                        });
                    }
                }
                Ok(MemoryEmbeddingSaveReport {
                    attempted,
                    failures,
                })
            })
            .await
    }

    /// Rebuild LSH only when the side table is behind live vector rows.
    pub async fn rebuild_lsh_if_lagging(&self, model: String) -> anyhow::Result<()> {
        self.db
            .run_blocking(move |db| {
                if db.embedding_lsh_lagging(&model)? {
                    db.rebuild_embedding_lsh(&model)?;
                }
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Arc<Database>, MemoryEmbeddingStore) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = MemoryEmbeddingStore::new(db.clone());
        (db, store)
    }

    #[tokio::test]
    async fn pending_embeddings_keep_backlog_order_and_filter_sensitive_text() {
        let (db, store) = store();
        let session = db.create_session("embedding store pending").unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "sk-secret", "inferred", 1.0, &[])
            .unwrap();
        db.add_episode(&session.id, "safe episode").unwrap();
        db.add_episode(&session.id, "password is hunter2").unwrap();

        let pending = store.pending_embeddings("model-a".into()).await.unwrap();

        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].entity, MemoryEmbeddingEntity::Fact);
        assert_eq!(pending[0].text, "user likes Rust");
        assert_eq!(pending[1].entity, MemoryEmbeddingEntity::Episode);
        assert_eq!(pending[1].text, "safe episode");
        assert!(pending.iter().all(|item| !item.text.contains("secret")));
        assert!(pending.iter().all(|item| !item.text.contains("hunter2")));
    }

    #[tokio::test]
    async fn pending_embeddings_keep_repository_backlog_limits() {
        let (db, store) = store();
        for index in 0..(crate::embeddings::FACT_EMBED_BACKLOG_LIMIT + 2) {
            db.insert_fact(
                "user",
                "likes",
                &format!("item-{index}"),
                "inferred",
                0.8,
                &[],
            )
            .unwrap();
        }

        let pending = store.pending_embeddings("model-a".into()).await.unwrap();

        assert_eq!(pending.len(), crate::embeddings::FACT_EMBED_BACKLOG_LIMIT);
        assert!(
            pending
                .iter()
                .all(|item| item.entity == MemoryEmbeddingEntity::Fact)
        );
    }

    #[tokio::test]
    async fn embedding_lifecycle_store_preserves_dimensions_and_lsh() {
        let (db, store) = store();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        let mut pending = store.pending_embeddings("model-a".into()).await.unwrap();
        pending.sort_by(|a, b| a.entity_id.cmp(&b.entity_id));
        let vectors = pending
            .into_iter()
            .map(|item| MemoryEmbeddingVector {
                entity: item.entity,
                entity_id: item.entity_id,
                text: item.text,
                vector: vec![1.0, 0.0],
            })
            .collect();

        let report = store.save_batch("model-a".into(), vectors).await.unwrap();
        assert_eq!(report.attempted, 1);
        assert!(report.failures.is_empty());
        assert_eq!(store.list_models().await.unwrap(), ["model-a"]);
        assert_eq!(store.list_dimensions("model-a".into()).await.unwrap(), [2]);

        db.conn()
            .execute("DELETE FROM embedding_lsh WHERE model = 'model-a'", [])
            .unwrap();
        store
            .rebuild_lsh_if_lagging("model-a".into())
            .await
            .unwrap();
        assert!(!db.embedding_lsh_lagging("model-a").unwrap());

        store.clear_embeddings().await.unwrap();
        assert!(store.list_models().await.unwrap().is_empty());
        assert!(
            store
                .list_dimensions("model-a".into())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn save_batch_reports_row_failures_and_continues() {
        let (db, store) = store();
        let first = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        let second = db
            .insert_fact("user", "likes", "Go", "inferred", 0.8, &[])
            .unwrap();
        let pending = store.pending_embeddings("model-a".into()).await.unwrap();
        assert_eq!(pending.len(), 2);

        let report = store
            .save_batch(
                "model-a".into(),
                vec![
                    MemoryEmbeddingVector {
                        entity: MemoryEmbeddingEntity::Fact,
                        entity_id: first.id.clone(),
                        text: "user likes Rust".into(),
                        vector: vec![1.0, 0.0],
                    },
                    MemoryEmbeddingVector {
                        entity: MemoryEmbeddingEntity::Fact,
                        entity_id: second.id.clone(),
                        text: "user likes Go".into(),
                        vector: Vec::new(),
                    },
                ],
            )
            .await
            .unwrap();

        assert_eq!(report.attempted, 2);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].entity_id, second.id);
        assert!(report.failures[0].error.contains("must not be empty"));
        assert!(
            db.get_embedding_for_model(entity_kind::FACT, &first.id, Some("model-a"))
                .unwrap()
                .is_some()
        );
        assert!(
            db.get_embedding_for_model(entity_kind::FACT, &second.id, Some("model-a"))
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn embedding_store_propagates_database_errors() {
        let (db, store) = store();
        db.conn()
            .execute("DROP TABLE memory_embeddings", [])
            .unwrap();

        let error = store.list_models().await.unwrap_err();

        assert!(error.to_string().contains("memory_embeddings"));
    }
}
