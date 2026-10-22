//! Typed memory access used by prompt context and the memory worker.
//!
//! This module is the only agent-side owner of memory query plumbing.  Prompt
//! assembly receives a bounded candidate snapshot and never needs to know
//! about SQLite, embedding providers, or the memory cache layout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
#[cfg(test)]
use haven_common::config::RequestKind;
use haven_llm::LlmRouter;
#[cfg(test)]
use haven_memory::Database;
use haven_memory::{
    MAX_MEMORY_QUERY_CHARS, MemoryEntityKind, MemoryQuery, MemoryRecall, MemoryRecallKind,
};
use haven_memory::{
    MemoryFactExtractionStore, MemoryFactProposalStore, MemoryFactStore, MemoryMaintenanceStore,
    MemoryOutboxStore, MemoryRecallStore, MemoryStores,
};

use crate::memory_index::MemoryEmbeddingIndex;

const MAX_EPISODES_IN_PROMPT: usize = 5;
const PROMPT_FACT_RECALL_ROUTES: [(MemoryRecallKind, usize); 3] = [
    (MemoryRecallKind::Profile, 8),
    (MemoryRecallKind::Semantic, 8),
    (MemoryRecallKind::Procedural, 4),
];

/// Raw, bounded candidates for prompt memory rendering.
///
/// The service owns retrieval and cache invalidation.  The renderer owns
/// ranking, sanitization, and the prompt-specific character/token budget.
#[derive(Clone, Default)]
pub(crate) struct PromptMemoryCandidates {
    pub(crate) query_text: String,
    pub(crate) vector_fact_hits: Vec<haven_memory::MemoryHit>,
    pub(crate) vector_episode_hits: Vec<haven_memory::MemoryHit>,
    pub(crate) keyword_episode_hits: Vec<haven_memory::MemoryHit>,
    pub(crate) all_facts: Vec<haven_memory::Fact>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct PromptMemoryCacheKey {
    query: String,
    embedding_model: String,
    memory_revision: u64,
    exclude_session_id: Option<String>,
}

struct PromptMemoryCache {
    entries: HashMap<PromptMemoryCacheKey, PromptMemoryCandidates>,
    order: VecDeque<PromptMemoryCacheKey>,
}

impl PromptMemoryCache {
    const CAPACITY: usize = 32;

    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &PromptMemoryCacheKey) -> Option<PromptMemoryCandidates> {
        let value = self.entries.get(key).cloned();
        if value.is_some() {
            self.order.retain(|cached| cached != key);
            self.order.push_back(key.clone());
        }
        value
    }

    fn insert(&mut self, key: PromptMemoryCacheKey, value: PromptMemoryCandidates) {
        self.order.retain(|cached| cached != &key);
        while self.entries.len() >= Self::CAPACITY && !self.entries.contains_key(&key) {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        self.entries.insert(key.clone(), value);
        self.order.push_back(key);
    }
}

/// Memory capability boundary shared by prompt context, tools, and the
/// background memory worker. The service accepts the Memory-owned typed store
/// bundle and never receives or retains a raw Database in production.
pub struct MemoryService {
    #[cfg(test)]
    test_database: Option<Arc<Database>>,
    fact_store: MemoryFactStore,
    fact_extraction_store: MemoryFactExtractionStore,
    fact_proposal_store: MemoryFactProposalStore,
    maintenance_store: MemoryMaintenanceStore,
    outbox_store: MemoryOutboxStore,
    recall_store: MemoryRecallStore,
    router: Option<Arc<LlmRouter>>,
    embedding_index: Option<MemoryEmbeddingIndex>,
    prompt_cache: Mutex<PromptMemoryCache>,
}

impl MemoryService {
    pub fn new(
        stores: impl Into<MemoryStores>,
        router: Option<Arc<LlmRouter>>,
        embed_chunk_size: usize,
    ) -> Self {
        let stores = stores.into();
        #[cfg(test)]
        let test_database = stores.database_handle_for_test();
        let outbox_store = stores.outbox_store();
        let fact_store = stores.fact_store();
        let fact_extraction_store = stores.fact_extraction_store();
        let fact_proposal_store = stores.fact_proposal_store();
        let maintenance_store = stores.maintenance_store();
        let recall_store = stores.recall_store();
        let embeddings = stores.embedding_store();
        let embedding_index = router.as_ref().map(|router| {
            MemoryEmbeddingIndex::new(
                embeddings,
                recall_store.clone(),
                router.clone(),
                embed_chunk_size.max(1),
            )
        });
        Self {
            #[cfg(test)]
            test_database,
            fact_store,
            fact_extraction_store,
            fact_proposal_store,
            maintenance_store,
            outbox_store,
            recall_store,
            router,
            embedding_index,
            prompt_cache: Mutex::new(PromptMemoryCache::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn database_handle_for_test(&self) -> Arc<Database> {
        self.test_database
            .as_ref()
            .expect("Database-backed test construction preserves its fixture handle")
            .clone()
    }

    /// Return the shared store used for memory-owned durable episode and
    /// outbox persistence, backed by the service's shared database.
    pub(crate) fn outbox_store(&self) -> MemoryOutboxStore {
        self.outbox_store.clone()
    }

    /// Return the shared typed fact capability used by the memory worker.
    pub(crate) fn memory_fact_store(&self) -> MemoryFactStore {
        self.fact_store.clone()
    }

    /// Return the shared persistence port for ordinary/summary extraction
    /// cursors, their shared throttle timestamp, and ordinary transcript
    /// projections.
    pub(crate) fn memory_fact_extraction_store(&self) -> MemoryFactExtractionStore {
        self.fact_extraction_store.clone()
    }

    pub(crate) fn memory_fact_proposal_store(&self) -> MemoryFactProposalStore {
        self.fact_proposal_store.clone()
    }

    /// Return the shared persistence port for deterministic and LLM
    /// maintenance database operations.
    pub(crate) fn memory_maintenance_store(&self) -> MemoryMaintenanceStore {
        self.maintenance_store.clone()
    }

    pub(crate) fn memory_revision(&self) -> u64 {
        self.recall_store.memory_revision()
    }

    pub(crate) async fn current_embedding_model(&self) -> String {
        match &self.embedding_index {
            Some(index) => index.current_vector_space_identity().await,
            None => String::new(),
        }
    }

    /// Return one bounded retrieval snapshot for a prompt turn.
    pub(crate) async fn prompt_candidates(
        &self,
        session_description: &str,
        exclude_session_id: Option<&str>,
    ) -> anyhow::Result<PromptMemoryCandidates> {
        let query_text = session_description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let query_text: String = query_text.chars().take(MAX_MEMORY_QUERY_CHARS).collect();
        let embedding_model = self.current_embedding_model().await;
        let key = PromptMemoryCacheKey {
            query: query_text.clone(),
            embedding_model: embedding_model.clone(),
            memory_revision: self.memory_revision(),
            exclude_session_id: exclude_session_id
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        };
        if let Ok(mut cache) = self.prompt_cache.lock()
            && let Some(candidates) = cache.get(&key)
        {
            return Ok(candidates);
        }

        let (vector, cacheable) =
            if embedding_model.is_empty() || haven_memory::is_sensitive_text(&query_text) {
                (None, true)
            } else if let Some(router) = &self.router {
                match router.embed_text(&query_text).await {
                    Ok(vector) if !vector.is_empty() => (Some(vector), true),
                    Ok(_) => (None, true),
                    Err(error) => {
                        tracing::debug!(
                            failure_reason = prompt_memory_embedding_failure_reason(&error),
                            "prompt memory embedding failed; skipping memory cache for this pass"
                        );
                        (None, false)
                    }
                }
            } else {
                (None, true)
            };

        let candidates = async {
            let mut routed_fact_hits = Vec::new();
            let mut vector_fact_hits = Vec::new();
            let mut vector_episode_hits = Vec::new();
            if !query_text.trim().is_empty() {
                let query_vector = vector.as_deref().filter(|_| !embedding_model.is_empty());
                for (kind, limit) in PROMPT_FACT_RECALL_ROUTES {
                    let query = MemoryQuery::new(&query_text, kind, limit)?;
                    let category_vector_hits = if let Some(query_vector) = query_vector {
                        let hits = self
                            .recall_store
                            .vector_recall(
                                query.clone(),
                                query_vector.to_vec(),
                                embedding_model.clone(),
                            )
                            .await?;
                        vector_fact_hits.extend(hits.iter().cloned());
                        Some(hits)
                    } else {
                        None
                    };
                    let recall = self
                        .recall_store
                        .retrieve(query, category_vector_hits)
                        .await?;
                    routed_fact_hits.extend(recall.hits);
                }

                if let Some(query_vector) = query_vector {
                    let episode_query = MemoryQuery::new(
                        &query_text,
                        MemoryEntityKind::Episode,
                        MAX_EPISODES_IN_PROMPT,
                    )?
                    .with_excluded_session(key.exclude_session_id.as_deref())?;
                    vector_episode_hits = self
                        .recall_store
                        .vector_recall(
                            episode_query,
                            query_vector.to_vec(),
                            embedding_model.clone(),
                        )
                        .await?;
                }
            }

            let mut all_facts = self.recall_store.visible_user_facts(40).await?;
            let mut seen_ids: HashSet<String> =
                all_facts.iter().map(|fact| fact.fact_id.clone()).collect();
            let candidate_ids: Vec<String> = routed_fact_hits
                .iter()
                .chain(vector_fact_hits.iter())
                .map(|hit| hit.entity_id.clone())
                .filter(|id| !id.is_empty() && !seen_ids.contains(id))
                .collect();
            if !candidate_ids.is_empty() {
                for fact in self
                    .recall_store
                    .visible_facts_by_ids(candidate_ids)
                    .await?
                {
                    if seen_ids.insert(fact.fact_id.clone()) {
                        all_facts.push(fact);
                    }
                }
            }

            let keyword_episode_hits = if query_text.trim().is_empty() {
                Vec::new()
            } else {
                let query = MemoryQuery::new(
                    &query_text,
                    MemoryEntityKind::Episode,
                    MAX_EPISODES_IN_PROMPT,
                )?
                .with_excluded_session(key.exclude_session_id.as_deref())?;
                self.recall_store.keyword_recall(query).await?
            };

            Ok::<_, anyhow::Error>(PromptMemoryCandidates {
                query_text: String::new(),
                vector_fact_hits,
                vector_episode_hits,
                keyword_episode_hits,
                all_facts,
            })
        }
        .await
        .context("collect prompt memory candidates")?;
        let candidates = PromptMemoryCandidates {
            query_text,
            ..candidates
        };
        if cacheable && let Ok(mut cache) = self.prompt_cache.lock() {
            cache.insert(key, candidates.clone());
        }
        Ok(candidates)
    }

    /// Check whether a prompt-memory snapshot is already available without
    /// issuing another embedding request. This is used by the background
    /// prefetch path before it marks the MEMORY fence dirty.
    pub(crate) async fn has_cached_prompt_candidates(
        &self,
        session_description: &str,
        exclude_session_id: Option<&str>,
    ) -> bool {
        let query_text: String = session_description
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_MEMORY_QUERY_CHARS)
            .collect();
        let embedding_model = self.current_embedding_model().await;
        let key = PromptMemoryCacheKey {
            query: query_text,
            embedding_model,
            memory_revision: self.memory_revision(),
            exclude_session_id: exclude_session_id
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string),
        };
        self.prompt_cache
            .lock()
            .map(|mut cache| cache.get(&key).is_some())
            .unwrap_or(false)
    }

    /// Execute a fully-scoped typed recall request.
    pub async fn recall(&self, query: MemoryQuery) -> anyhow::Result<MemoryRecall> {
        let vector_hits = match &self.embedding_index {
            Some(index) => index.search(&query).await?,
            None => None,
        };
        self.recall_store.retrieve(query, vector_hits).await
    }

    pub(crate) async fn embed_new_memory(&self) {
        if let Some(index) = &self.embedding_index {
            index.embed_new_memory().await;
        }
    }

    pub(crate) async fn rebuild_lsh_if_lagging(&self) {
        if let Some(index) = &self.embedding_index {
            index.rebuild_lsh_if_lagging().await;
        }
    }
}

fn prompt_memory_embedding_failure_reason(error: &haven_llm::LlmError) -> &'static str {
    error.connection_failure_reason().as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_embedding_failure_diagnostic_uses_stable_category() {
        let error = haven_llm::LlmError::Network(
            "provider returned secret response body and https://user:secret@example.com".into(),
        );

        let reason = prompt_memory_embedding_failure_reason(&error);

        assert_eq!(reason, "network");
        assert!(!reason.contains("secret"));
        assert!(!reason.contains("example.com"));
    }
    use haven_common::config::{
        Capability, ModelEndpoint, RequestPolicy, RoutedModel, RouterConfig,
    };

    fn embedding_router() -> Arc<LlmRouter> {
        let endpoint = ModelEndpoint {
            api_key: "test-key".into(),
            base_url: "https://embedding.example/v1".into(),
            model_name: "text-embedding-3-small".into(),
            ..Default::default()
        };
        Arc::new(LlmRouter::new(RouterConfig {
            models: vec![RoutedModel {
                model_config_id: "embedding".into(),
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
    fn prompt_cache_is_bounded() {
        let mut cache = PromptMemoryCache::new();
        for index in 0..PromptMemoryCache::CAPACITY {
            let key = PromptMemoryCacheKey {
                query: index.to_string(),
                embedding_model: String::new(),
                memory_revision: 0,
                exclude_session_id: None,
            };
            cache.insert(key, PromptMemoryCandidates::default());
        }
        let first = PromptMemoryCacheKey {
            query: "0".into(),
            embedding_model: String::new(),
            memory_revision: 0,
            exclude_session_id: None,
        };
        assert!(cache.get(&first).is_some());
        cache.insert(
            PromptMemoryCacheKey {
                query: "overflow".into(),
                embedding_model: String::new(),
                memory_revision: 0,
                exclude_session_id: None,
            },
            PromptMemoryCandidates::default(),
        );
        let second = PromptMemoryCacheKey {
            query: "1".into(),
            embedding_model: String::new(),
            memory_revision: 0,
            exclude_session_id: None,
        };
        assert!(cache.get(&first).is_some());
        assert!(cache.get(&second).is_none());
        assert_eq!(cache.entries.len(), PromptMemoryCache::CAPACITY);
    }

    #[tokio::test]
    async fn current_embedding_model_matches_index_identity() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        let router = embedding_router();
        let service = MemoryService::new(db, Some(router.clone()), 1);

        let service_identity = service.current_embedding_model().await;
        let index_identity = service
            .embedding_index
            .as_ref()
            .unwrap()
            .current_vector_space_identity()
            .await;

        assert!(!service_identity.is_empty());
        assert_eq!(service_identity, index_identity);
    }

    #[tokio::test]
    async fn current_embedding_model_is_empty_without_an_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open(&temp_dir.path().join("memory.db")).unwrap());
        let service = MemoryService::new(db, None, 1);

        assert_eq!(service.current_embedding_model().await, "");
    }

    #[tokio::test]
    async fn prompt_candidates_keep_keyword_fallback_visibility_exclusion_and_revision_cache() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let current = db.create_session("current recall session").unwrap();
        let earlier = db.create_session("earlier recall session").unwrap();
        let safe_fact = db
            .insert_fact("user", "likes", "Rust", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "sk-hidden", "user", 1.0, &[])
            .unwrap();
        db.add_episode(&current.session_id, "The user researches Rust testing.")
            .unwrap();
        db.add_episode(&earlier.session_id, "The user researches Rust testing.")
            .unwrap();
        let service = MemoryService::new(db.clone(), None, 1);

        assert!(
            !service
                .has_cached_prompt_candidates(" Rust\n testing ", Some(&current.session_id))
                .await
        );
        let candidates = service
            .prompt_candidates(" Rust\n testing ", Some(&current.session_id))
            .await
            .unwrap();

        assert_eq!(candidates.query_text, "Rust testing");
        assert!(candidates.vector_fact_hits.is_empty());
        assert!(candidates.vector_episode_hits.is_empty());
        assert_eq!(candidates.all_facts.len(), 1);
        assert_eq!(candidates.all_facts[0].fact_id, safe_fact.fact_id);
        assert_eq!(candidates.keyword_episode_hits.len(), 1);
        assert_eq!(
            candidates.keyword_episode_hits[0].text,
            "The user researches Rust testing."
        );
        assert!(
            service
                .has_cached_prompt_candidates("Rust testing", Some(&current.session_id))
                .await
        );

        db.insert_fact("user", "uses", "Cargo", "user", 0.8, &[])
            .unwrap();
        assert!(
            !service
                .has_cached_prompt_candidates("Rust testing", Some(&current.session_id))
                .await
        );
    }

    #[tokio::test]
    async fn prompt_candidates_route_each_fact_kind_and_expand_graph_neighbors() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        for index in 0..60 {
            db.insert_fact(
                "user",
                "prefers_release_note",
                &format!("release preference {index}"),
                "user",
                1.0,
                &["release"],
            )
            .unwrap();
        }
        let semantic_seed = db
            .insert_fact("user", "maintains", "Acme", "inferred", 0.9, &["release"])
            .unwrap();
        let semantic_neighbor = db
            .insert_fact("Acme", "uses", "Rust", "inferred", 0.9, &[])
            .unwrap();
        let memory = MemoryService::new(db, None, 1);
        let procedural = memory
            .memory_fact_store()
            .set_user_fact_with_kind(
                "user".into(),
                "release_steps".into(),
                "run checks before tagging".into(),
                vec!["release".into()],
                haven_memory::MemoryFactKind::Procedural,
            )
            .await
            .unwrap();

        let candidates = memory
            .prompt_candidates("release deployment", None)
            .await
            .unwrap();
        let ids: HashSet<String> = candidates
            .all_facts
            .iter()
            .map(|fact| fact.fact_id.clone())
            .collect();

        assert!(ids.contains(&semantic_seed.fact_id));
        assert!(ids.contains(&semantic_neighbor.fact_id));
        assert!(ids.contains(&procedural.fact_id));
        assert!(candidates.all_facts.iter().any(|fact| {
            fact.fact_id == semantic_neighbor.fact_id
                && fact.memory_kind == haven_memory::MemoryFactKind::Semantic
        }));
    }

    #[tokio::test]
    async fn recall_uses_keyword_fallback_and_filters_sensitive_facts() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let safe = db
            .insert_fact("user", "uses", "SQLite", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "SQLite sk-hidden", "user", 1.0, &[])
            .unwrap();
        let service = MemoryService::new(db, None, 1);

        let recall = service
            .recall(MemoryQuery::new("SQLite", MemoryEntityKind::Fact, 5).unwrap())
            .await
            .unwrap();

        assert_eq!(recall.mode, haven_memory::MemoryRecallMode::Keyword);
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, safe.fact_id);
        assert!(recall.hits[0].model.is_empty());
    }

    #[tokio::test]
    async fn prompt_candidate_store_errors_keep_the_existing_context() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.conn()
            .expect("test database connection checkout failed")
            .execute_batch("DROP TABLE facts")
            .unwrap();
        let service = MemoryService::new(db, None, 1);

        let error = match service.prompt_candidates("Rust", None).await {
            Ok(_) => panic!("expected prompt memory query to fail"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "collect prompt memory candidates");
    }
}
