//! Embedding-backed memory indexing and vector recall.
//!
//! This module owns the boundary between the agent's memory workflow and the
//! embedding endpoint. Database methods remain the persistence authority;
//! this component only chooses bounded work, calls the shared LLM router, and
//! coordinates writes/rebuilds around that call. Fact extraction and rule
//! based maintenance stay in `inference.rs`.

use std::sync::Arc;

use haven_common::config::ModelEndpoint;
use haven_llm::adapters::api_style_for;
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::embeddings::entity_kind;
use haven_memory::recall::{MemoryHit, MemoryQuery, MemoryRetriever};
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

type PendingEmbedding = (String, String, String);

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
    db: Arc<Database>,
    router: Arc<LlmRouter>,
    embed_chunk_size: usize,
    /// Serializes maintenance passes so two schedulers cannot embed the same
    /// missing rows concurrently and race their derived-index updates.
    maintenance_gate: Mutex<()>,
}

impl MemoryEmbeddingIndex {
    pub(crate) fn new(db: Arc<Database>, router: Arc<LlmRouter>, embed_chunk_size: usize) -> Self {
        Self {
            db,
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
            .is_role_configured(EndpointRole::EmbeddingModel)
            .await
        {
            return None;
        }
        let endpoint = self.router.config().await.embedding_model.clone();
        (!endpoint.model_name.trim().is_empty()).then_some(EmbeddingIdentity {
            storage_model: embedding_index_model(&endpoint),
            provider_model: endpoint.model_name,
        })
    }

    /// True when persisted vectors belong to another model and cannot be
    /// compared safely with the configured endpoint.
    async fn model_changed(&self, current: &str) -> anyhow::Result<bool> {
        if current.is_empty() {
            return Ok(false);
        }
        let db = self.db.clone();
        let stored = db
            .run_blocking(move |db| db.list_embedding_models())
            .await?;
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
            Ok(true) => {
                let db = self.db.clone();
                match db.run_blocking(move |db| db.clear_embeddings()).await {
                    Ok(_) => tracing::info!(
                        "memory embedding model changed: cleared vector index for rebuild"
                    ),
                    Err(error) => tracing::error!(
                        "memory embedding model changed: failed to clear vector index: {}",
                        error
                    ),
                }
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(
                    "memory embedding model check failed; keeping existing index: {}",
                    error
                );
                return;
            }
        }

        let db = self.db.clone();
        let model_for_missing = identity.storage_model.clone();
        let pending = match db
            .run_blocking(move |db| collect_pending_from_db(db, &model_for_missing))
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
            let texts: Vec<String> = chunk.iter().map(|(_, _, text)| text.clone()).collect();
            let embedding = match self.router.embed(texts).await {
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
            let db_for_dimensions = self.db.clone();
            let model_for_dimensions = identity.storage_model.clone();
            let stored_dimensions = match db_for_dimensions
                .run_blocking(move |db| db.list_embedding_dimensions(&model_for_dimensions))
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
                let db = self.db.clone();
                match db.run_blocking(move |db| db.clear_embeddings()).await {
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
            let stored_model = identity.storage_model.clone();
            let rows: Vec<_> = chunk
                .iter()
                .zip(embedding.vectors)
                .filter(|(_, vector)| !vector.is_empty())
                .map(|((kind, id, text), vector)| (kind.clone(), id.clone(), text.clone(), vector))
                .collect();
            let row_count = rows.len();
            let db = self.db.clone();
            if let Err(error) = db
                .run_blocking(move |db| {
                    let mut failures = 0usize;
                    for (kind, id, text, vector) in rows {
                        if let Err(error) =
                            db.save_embedding(&kind, &id, &stored_model, &vector, &text)
                        {
                            failures += 1;
                            if failures <= 3 {
                                tracing::warn!(
                                    "save_embedding failed for {} {}: {}",
                                    kind,
                                    id,
                                    error
                                );
                            }
                        }
                    }
                    if failures > 0 {
                        tracing::warn!(
                            "memory embedding batch: {} of {} items failed to save",
                            failures,
                            row_count
                        );
                    }
                    Ok::<(), anyhow::Error>(())
                })
                .await
            {
                tracing::warn!("memory embedding batch persistence failed: {}", error);
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
        let db = self.db.clone();
        if let Err(error) = db
            .run_blocking(move |db| {
                if db.embedding_lsh_lagging(&identity.storage_model)? {
                    db.rebuild_embedding_lsh(&identity.storage_model)?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
        {
            tracing::warn!("memory embedding LSH rebuild failed: {}", error);
        }
    }

    /// Acquire a vector and resolve it through the shared memory read policy.
    /// The provider/index adapter never returns raw embedding rows: facts and
    /// episodes are filtered, scoped, and normalized by `MemoryRetriever`.
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
        let db_for_dimensions = self.db.clone();
        let model_for_dimensions = identity.storage_model.clone();
        let query_dimension = vector.len();
        let stored_dimensions = db_for_dimensions
            .run_blocking(move |db| db.list_embedding_dimensions(&model_for_dimensions))
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
        let db = self.db.clone();
        let query = query.clone();
        db.run_blocking(move |db| {
            MemoryRetriever::new(db).vector(&query, &vector, &identity.storage_model)
        })
        .await
        .map(Some)
    }
}

fn collect_pending_from_db(db: &Database, model: &str) -> anyhow::Result<Vec<PendingEmbedding>> {
    let mut pending = Vec::new();
    for entity_type in [entity_kind::FACT, entity_kind::EPISODE] {
        for id in db.missing_embedding_ids(entity_type, model)? {
            let text = match entity_type {
                entity_kind::FACT => db.fact_text_by_id(&id)?,
                entity_kind::EPISODE => db.episode_text(&id)?,
                _ => None,
            };
            if let Some(text) = text {
                if MemoryRetriever::visible_text(&text) {
                    pending.push((entity_type.to_string(), id, text));
                } else {
                    tracing::debug!(
                        entity_type,
                        entity_id = %id,
                        "skipping sensitive memory from embedding provider"
                    );
                }
            }
        }
    }
    Ok(pending)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn pending_collection_uses_bounded_repository_backlogs() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        let session = db.create_session("embedding-test", "").unwrap();
        for index in 0..(haven_memory::embeddings::FACT_EMBED_BACKLOG_LIMIT + 2) {
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
        db.add_episode(&session.id, "episode text").unwrap();
        let index = MemoryEmbeddingIndex::new(
            db,
            Arc::new(LlmRouter::new(haven_common::config::RouterConfig::default())),
            4,
        );
        let pending = collect_pending_from_db(&index.db, "test-model").unwrap();
        assert_eq!(
            pending
                .iter()
                .filter(|(kind, _, _)| kind == entity_kind::FACT)
                .count(),
            haven_memory::embeddings::FACT_EMBED_BACKLOG_LIMIT
        );
        assert!(
            pending
                .iter()
                .any(|(kind, _, text)| { kind == entity_kind::EPISODE && text == "episode text" })
        );
    }

    #[test]
    fn pending_collection_excludes_legacy_sensitive_memory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Database::open(&temp_dir.path().join("memory.db")).unwrap();
        let session = db.create_session("embedding-test", "").unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "sk-secret", "inferred", 1.0, &[])
            .unwrap();
        db.add_episode(&session.id, "password is hunter2").unwrap();

        let pending = collect_pending_from_db(&db, "test-model").unwrap();

        assert!(pending.iter().any(|(_, _, text)| text.contains("Rust")));
        assert!(
            !pending
                .iter()
                .any(|(_, _, text)| text.contains("sk-secret"))
        );
        assert!(!pending.iter().any(|(_, _, text)| text.contains("hunter2")));
    }

    #[tokio::test]
    async fn embedding_database_errors_are_not_treated_as_no_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        db.conn()
            .execute("DROP TABLE memory_embeddings", [])
            .unwrap();
        let router = Arc::new(LlmRouter::new(haven_common::config::RouterConfig::default()));
        let index = MemoryEmbeddingIndex::new(db, router, 1);

        let error = index.model_changed("test-model").await.unwrap_err();
        assert!(error.to_string().contains("memory_embeddings"));
    }
}
