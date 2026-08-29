//! Embedding-backed memory indexing and vector recall.
//!
//! This module owns the boundary between the agent's memory workflow and the
//! embedding endpoint. Database methods remain the persistence authority;
//! this component only chooses bounded work, calls the shared LLM router, and
//! coordinates writes/rebuilds around that call. Fact extraction and rule
//! based maintenance stay in `inference.rs`.

use std::sync::Arc;

use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::embeddings::{EmbeddedText, entity_kind};

/// Maximum inputs accepted by one embedding request, regardless of the
/// configured batch size. The provider contract is intentionally enforced at
/// this boundary so callers cannot accidentally create an unbounded request.
const MAX_EMBEDDING_BATCH_SIZE: usize = 10;

/// Clamp a configured embedding batch to the provider-safe range.
pub(crate) fn embedding_batch_size(configured_size: usize) -> usize {
    configured_size.clamp(1, MAX_EMBEDDING_BATCH_SIZE)
}

type PendingEmbedding = (String, String, String);

/// Agent-side owner of embedding lifecycle operations.
pub(crate) struct MemoryEmbeddingIndex {
    db: Arc<Database>,
    router: Arc<LlmRouter>,
    embed_chunk_size: usize,
}

impl MemoryEmbeddingIndex {
    pub(crate) fn new(db: Arc<Database>, router: Arc<LlmRouter>, embed_chunk_size: usize) -> Self {
        Self {
            db,
            router,
            embed_chunk_size: embedding_batch_size(embed_chunk_size),
        }
    }

    /// Return the configured model name, or `None` when embedding is not
    /// configured. The model name is the identity of the vector space and is
    /// therefore resolved once per operation rather than inferred from rows.
    async fn configured_model(&self) -> Option<String> {
        if !self
            .router
            .is_role_configured(EndpointRole::EmbeddingModel)
            .await
        {
            return None;
        }
        let model = self
            .router
            .config()
            .await
            .embedding_model
            .model_name
            .clone();
        (!model.is_empty()).then_some(model)
    }

    /// True when persisted vectors belong to another model and cannot be
    /// compared safely with the configured endpoint.
    async fn model_changed(&self, current: &str) -> bool {
        if current.is_empty() {
            return false;
        }
        let db = self.db.clone();
        let stored = match db.run_blocking(move |db| db.list_embedding_models()).await {
            Ok(models) => models,
            Err(error) => {
                tracing::warn!(
                    "memory embedding model check failed; keeping existing index: {}",
                    error
                );
                return false;
            }
        };
        !stored.is_empty() && stored.iter().any(|model| model != current)
    }

    /// Embed facts and episode summaries that are not indexed by the current
    /// model. Work is bounded by the memory repository backlog limits and by
    /// the provider-safe request chunk size.
    pub(crate) async fn embed_new_memory(&self) {
        let Some(model) = self.configured_model().await else {
            tracing::debug!("embedding_model unconfigured; skipping vector indexing");
            return;
        };

        if self.model_changed(&model).await {
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

        let db = self.db.clone();
        let model_for_missing = model.clone();
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
            let stored_model = embedding.model.unwrap_or_else(|| model.clone());
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
        let Some(model) = self.configured_model().await else {
            return;
        };
        let db = self.db.clone();
        if let Err(error) = db
            .run_blocking(move |db| {
                if db.embedding_lsh_lagging(&model)? {
                    db.rebuild_embedding_lsh(&model)?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await
        {
            tracing::warn!("memory embedding LSH rebuild failed: {}", error);
        }
    }

    /// Try vector recall. `None` means the caller should use its keyword
    /// fallback; an empty vector result is still a successful vector query.
    pub(crate) async fn search(
        &self,
        entity_type: &str,
        query: &str,
        limit: usize,
    ) -> Option<Vec<(EmbeddedText, f64)>> {
        let model = self.configured_model().await?;
        if self.model_changed(&model).await {
            return None;
        }
        let vector = self.router.embed_text(query).await.ok()?;
        if vector.is_empty() {
            return None;
        }
        let db = self.db.clone();
        let entity_type = entity_type.to_string();
        db.run_blocking(move |db| db.search_embeddings(&entity_type, &vector, limit, &model))
            .await
            .ok()
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
                pending.push((entity_type.to_string(), id, text));
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
}
