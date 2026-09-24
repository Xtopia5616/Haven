//! Embedding-backed memory indexing and vector recall.
//!
//! This module owns the boundary between the agent's memory workflow and the
//! embedding endpoint. Database methods remain the persistence authority;
//! this component only chooses bounded work, calls the shared LLM router, and
//! coordinates writes/rebuilds around that call. Fact extraction and rule
//! based maintenance stay in `memory_worker.rs`.

use std::sync::Arc;

use haven_common::config::{ModelEndpoint, RequestKind};
use haven_llm::LlmRouter;
use haven_llm::adapters::api_style_for;
use haven_llm::types::EmbeddingRequest;
use haven_memory::recall::{MemoryHit, MemoryQuery};
use haven_memory::{MemoryEmbeddingStore, MemoryEmbeddingVector, MemoryRecallStore};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

/// Maximum inputs accepted by one embedding request, regardless of the
/// configured batch size. The provider contract is intentionally enforced at
/// this boundary so callers cannot accidentally create an unbounded request.
const MAX_EMBEDDING_BATCH_SIZE: usize = 10;

/// Clamp a configured embedding batch to the provider-safe range.
pub(crate) fn embedding_batch_size(configured_size: usize) -> usize {
    configured_size.clamp(1, MAX_EMBEDDING_BATCH_SIZE)
}

/// The persisted model column is a vector-space identity, not merely a
/// provider model label. The same label served by two gateways can produce
/// incompatible vectors, so include the effective wire style and endpoint in
/// an opaque digest. Keeping the endpoint out of the stored value avoids
/// leaking private gateway URLs through memory recall results.
pub(crate) fn embedding_index_model(endpoint: &ModelEndpoint) -> String {
    let canonical = format!(
        "provider={}\nstyle={}\nbase_url={}\nmodel={}",
        endpoint.provider.trim().to_ascii_lowercase(),
        api_style_for(endpoint),
        endpoint.base_url.trim().trim_end_matches('/'),
        endpoint.model_name.trim(),
    );
    let digest = Sha256::digest(canonical.as_bytes());
    format!("embedding-v2:{digest:x}")
}

#[derive(Debug, Clone)]
struct EmbeddingIdentity {
    storage_model: String,
    provider_model: String,
}

/// Agent-side owner of embedding lifecycle operations.
pub(crate) struct MemoryEmbeddingIndex {
    store: MemoryEmbeddingStore,
    recall_store: MemoryRecallStore,
    router: Arc<LlmRouter>,
    embed_chunk_size: usize,
    /// Serializes maintenance passes so two schedulers cannot embed the same
    /// missing rows concurrently and race their derived-index updates.
    maintenance_gate: Mutex<()>,
}

impl MemoryEmbeddingIndex {
    pub(crate) fn new(
        store: MemoryEmbeddingStore,
        recall_store: MemoryRecallStore,
        router: Arc<LlmRouter>,
        embed_chunk_size: usize,
    ) -> Self {
        Self {
            store,
            recall_store,
            router,
            embed_chunk_size: embedding_batch_size(embed_chunk_size),
            maintenance_gate: Mutex::new(()),
        }
    }

    /// Resolve the configured vector-space identity once per operation rather
    /// than inferring it from persisted rows.
    async fn configured_identity(&self) -> Option<EmbeddingIdentity> {
        if !self
            .router
            .is_request_configured(RequestKind::Embedding)
            .await
        {
            return None;
        }
        let endpoint = self
            .router
            .config()
            .await
            .route(RequestKind::Embedding)
            .map(|model| &model.endpoint)
            .cloned()
            .unwrap_or_default();
        (!endpoint.model_name.trim().is_empty()).then_some(EmbeddingIdentity {
            storage_model: embedding_index_model(&endpoint),
            provider_model: endpoint.model_name,
        })
    }

    /// Return the identity used to scope persisted vectors and prompt-memory
    /// cache entries. An empty identity means vector recall is unavailable and
    /// callers should retain the keyword path.
    pub(crate) async fn current_vector_space_identity(&self) -> String {
        self.configured_identity()
            .await
            .map(|identity| identity.storage_model)
            .unwrap_or_default()
    }

    /// True when persisted vectors belong to another model and cannot be
    /// compared safely with the configured endpoint.
    async fn model_changed(&self, current: &str) -> anyhow::Result<bool> {
        if current.is_empty() {
            return Ok(false);
        }
        let stored = self.store.list_models().await?;
        Ok(!stored.is_empty() && stored.iter().any(|model| model != current))
    }

    /// Embed facts and episode summaries that are not indexed by the current
    /// model. Work is bounded by the memory repository backlog limits and by
    /// the provider-safe request chunk size.
    pub(crate) async fn embed_new_memory(&self) {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let Some(identity) = self.configured_identity().await else {
            tracing::debug!("embedding_model unconfigured; skipping vector indexing");
            return;
        };

        match self.model_changed(&identity.storage_model).await {
            Ok(true) => match self.store.clear_embeddings().await {
                Ok(_) => tracing::info!(
                    "memory embedding model changed: cleared vector index for rebuild"
                ),
                Err(error) => tracing::error!(
                    "memory embedding model changed: failed to clear vector index: {}",
                    error
                ),
            },
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    "memory embedding model check failed; keeping existing index: {}",
                    error
                );
                return;
            }
        }

        let pending = match self
            .store
            .pending_embeddings(identity.storage_model.clone())
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(
                    "memory embedding: failed to collect pending items: {}",
                    error
                );
                return;
            }
        };

        if pending.is_empty() {
            return;
        }
        tracing::info!("embedding {} memory items", pending.len());

        for chunk in pending.chunks(self.embed_chunk_size) {
            let texts: Vec<String> = chunk.iter().map(|item| item.text.clone()).collect();
            let embedding = match self.router.embed(EmbeddingRequest { input: texts }).await {
                Ok(embedding) => embedding,
                Err(error) => {
                    tracing::warn!("memory embedding batch failed: {}", error);
                    return;
                }
            };
            if embedding.vectors.len() != chunk.len() {
                tracing::warn!(
                    expected = chunk.len(),
                    actual = embedding.vectors.len(),
                    "memory embedding provider returned a vector count different from the request"
                );
                return;
            }
            if let Some(provider_model) = embedding.model.as_deref()
                && provider_model != identity.provider_model
            {
                tracing::warn!(
                    configured_model = %identity.provider_model,
                    provider_model,
                    "memory embedding provider returned a different model; refusing to persist vectors"
                );
                return;
            }
            let dimensions: Vec<usize> = embedding
                .vectors
                .iter()
                .filter(|vector| !vector.is_empty())
                .map(Vec::len)
                .collect();
            let Some(&dimension) = dimensions.first() else {
                tracing::warn!("memory embedding provider returned no usable vectors");
                return;
            };
            if dimensions.iter().any(|candidate| *candidate != dimension) {
                tracing::warn!(
                    configured_model = %identity.provider_model,
                    "memory embedding provider returned mixed vector dimensions; refusing to persist batch"
                );
                return;
            }
            let stored_dimensions = match self
                .store
                .list_dimensions(identity.storage_model.clone())
                .await
            {
                Ok(dimensions) => dimensions,
                Err(error) => {
                    tracing::warn!(
                        "memory embedding: failed to inspect stored dimensions: {}",
                        error
                    );
                    return;
                }
            };
            if stored_dimensions
                .iter()
                .any(|stored_dimension| *stored_dimension != dimension)
            {
                match self.store.clear_embeddings().await {
                    Ok(_) => tracing::info!(
                        configured_model = %identity.provider_model,
                        dimension,
                        "memory embedding dimension changed: cleared vector index for rebuild"
                    ),
                    Err(error) => {
                        tracing::error!(
                            "memory embedding dimension changed: failed to clear vector index: {}",
                            error
                        );
                        return;
                    }
                }
            }
            let vectors: Vec<_> = chunk
                .iter()
                .zip(embedding.vectors)
                .filter(|(_, vector)| !vector.is_empty())
                .map(|(item, vector)| MemoryEmbeddingVector {
                    entity: item.entity,
                    entity_id: item.entity_id.clone(),
                    text: item.text.clone(),
                    vector,
                })
                .collect();
            let row_count = vectors.len();
            match self
                .store
                .save_batch(identity.storage_model.clone(), vectors)
                .await
            {
                Ok(report) => {
                    for failure in report.failures.iter().take(3) {
                        tracing::warn!(
                            "save_embedding failed for {} {}: {}",
                            failure.entity.as_str(),
                            failure.entity_id,
                            failure.error
                        );
                    }
                    if !report.failures.is_empty() {
                        tracing::warn!(
                            "memory embedding batch: {} of {} items failed to save",
                            report.failures.len(),
                            row_count
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!("memory embedding batch persistence failed: {}", error);
                }
            }
        }
    }

    /// Rebuild the derived LSH side table when it falls behind the vector
    /// rows. This is a maintenance operation and never runs on the recall
    /// hot path.
    pub(crate) async fn rebuild_lsh_if_lagging(&self) {
        let _maintenance_guard = self.maintenance_gate.lock().await;
        let Some(identity) = self.configured_identity().await else {
            return;
        };
        if let Err(error) = self
            .store
            .rebuild_lsh_if_lagging(identity.storage_model)
            .await
        {
            tracing::warn!("memory embedding LSH rebuild failed: {}", error);
        }
    }

    /// Acquire a vector and resolve it through the shared memory read policy.
    /// The provider/index adapter never returns raw embedding rows: facts and
    /// episodes are filtered, scoped, and normalized by the shared memory
    /// retriever inside `MemoryRecallStore`.
    /// `Ok(None)` means the caller should use its keyword fallback because the
    /// embedding provider is unavailable or not configured. Database and
    /// retriever errors remain errors so callers cannot confuse an outage with
    /// an empty memory result.
    pub(crate) async fn search(
        &self,
        query: &MemoryQuery,
    ) -> anyhow::Result<Option<Vec<MemoryHit>>> {
        let Some(identity) = self.configured_identity().await else {
            return Ok(None);
        };
        if self.model_changed(&identity.storage_model).await? {
            return Ok(None);
        }
        let vector = match self.router.embed_text(&query.text).await {
            Ok(vector) => vector,
            Err(error) => {
                tracing::warn!(
                    "memory vector recall unavailable; using keyword fallback: {}",
                    error
                );
                return Ok(None);
            }
        };
        if vector.is_empty() {
            return Ok(None);
        }
        let query_dimension = vector.len();
        let stored_dimensions = self
            .store
            .list_dimensions(identity.storage_model.clone())
            .await?;
        if stored_dimensions
            .iter()
            .any(|stored_dimension| *stored_dimension != query_dimension)
        {
            tracing::warn!(
                configured_model = %identity.provider_model,
                query_dimension,
                ?stored_dimensions,
                "memory vector index dimension mismatch; using keyword fallback"
            );
            return Ok(None);
        }
        self.recall_store
            .vector_recall(query.clone(), vector, identity.storage_model)
            .await
            .map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{Capability, RequestPolicy, RoutedModel, RouterConfig};
    use haven_memory::{Database, MemoryRecallStore};

    fn recall_store(db: Arc<Database>) -> MemoryRecallStore {
        MemoryRecallStore::new(db)
    }

    fn embedding_router(model_name: &str, base_url: &str) -> Arc<LlmRouter> {
        let endpoint = ModelEndpoint {
            api_key: "test-key".into(),
            base_url: base_url.into(),
            model_name: model_name.into(),
            ..Default::default()
        };
        Arc::new(LlmRouter::new(RouterConfig {
            models: vec![RoutedModel {
                id: "embedding".into(),
                endpoint,
                capabilities: vec![Capability::Embedding],
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Embedding,
                primary: "embedding".into(),
            }],
            ..Default::default()
        }))
    }

    #[test]
    fn embedding_batch_size_is_provider_bounded() {
        assert_eq!(embedding_batch_size(0), 1);
        assert_eq!(embedding_batch_size(4), 4);
        assert_eq!(embedding_batch_size(64), MAX_EMBEDDING_BATCH_SIZE);
    }

    #[test]
    fn embedding_index_model_partitions_same_label_across_endpoints() {
        let mut first = ModelEndpoint::default();
        first.provider = "openai".into();
        first.model_name = "text-embedding-3-small".into();
        first.base_url = "https://gateway-a.example/v1/".into();
        let mut second = first.clone();
        second.base_url = "https://gateway-b.example/v1".into();

        assert_ne!(
            embedding_index_model(&first),
            embedding_index_model(&second)
        );
        assert!(embedding_index_model(&first).starts_with("embedding-v2:"));
        assert!(!embedding_index_model(&first).contains("gateway-a"));
    }

    #[tokio::test]
    async fn current_vector_space_identity_is_empty_without_embedding_configuration() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        let unconfigured = MemoryEmbeddingIndex::new(
            MemoryEmbeddingStore::new(db.clone()),
            recall_store(db.clone()),
            Arc::new(LlmRouter::new(RouterConfig::default())),
            1,
        );
        let empty_model = MemoryEmbeddingIndex::new(
            MemoryEmbeddingStore::new(db.clone()),
            recall_store(db),
            embedding_router("  ", "https://gateway.example/v1"),
            1,
        );

        assert_eq!(unconfigured.current_vector_space_identity().await, "");
        assert_eq!(empty_model.current_vector_space_identity().await, "");
    }

    #[tokio::test]
    async fn current_vector_space_identity_changes_when_endpoint_changes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        let gateway_a = MemoryEmbeddingIndex::new(
            MemoryEmbeddingStore::new(db.clone()),
            recall_store(db.clone()),
            embedding_router("text-embedding-3-small", "https://gateway-a.example/v1"),
            1,
        );
        let gateway_b = MemoryEmbeddingIndex::new(
            MemoryEmbeddingStore::new(db.clone()),
            recall_store(db),
            embedding_router("text-embedding-3-small", "https://gateway-b.example/v1"),
            1,
        );

        let identity_a = gateway_a.current_vector_space_identity().await;
        let identity_b = gateway_b.current_vector_space_identity().await;

        assert_ne!(identity_a, identity_b);
        assert!(identity_a.starts_with("embedding-v2:"));
        assert!(identity_b.starts_with("embedding-v2:"));
    }

    #[tokio::test]
    async fn model_change_detection_uses_the_configured_vector_space() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        let fact = db
            .insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        db.save_embedding(
            haven_memory::embeddings::entity_kind::FACT,
            &fact.id,
            "previous-vector-space",
            &[1.0, 0.0],
            "user likes Rust",
        )
        .unwrap();
        let index = MemoryEmbeddingIndex::new(
            MemoryEmbeddingStore::new(db.clone()),
            recall_store(db),
            embedding_router("text-embedding-3-small", "https://gateway.example/v1"),
            1,
        );

        let current = index.current_vector_space_identity().await;

        assert!(index.model_changed(&current).await.unwrap());
    }

    #[tokio::test]
    async fn embedding_database_errors_are_not_treated_as_no_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        db.conn()
            .execute("DROP TABLE memory_embeddings", [])
            .unwrap();
        let router = Arc::new(LlmRouter::new(haven_common::config::RouterConfig::default()));
        let index = MemoryEmbeddingIndex::new(
            MemoryEmbeddingStore::new(db.clone()),
            recall_store(db),
            router,
            1,
        );

        let error = index.model_changed("test-model").await.unwrap_err();
        assert!(error.to_string().contains("memory_embeddings"));
    }
}
