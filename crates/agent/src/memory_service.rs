//! Typed memory access used by prompt context and the memory worker.
//!
//! This module is the only agent-side owner of memory query plumbing.  Prompt
//! assembly receives a bounded candidate snapshot and never needs to know
//! about SQLite, embedding providers, or the memory cache layout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use haven_common::config::RequestKind;
use haven_llm::LlmRouter;
use haven_memory::recall::{
    MAX_MEMORY_QUERY_CHARS, MAX_RECALL_LIMIT, MemoryKind, MemoryQuery, MemoryRecall,
    MemoryRetriever,
};
use haven_memory::{
    Database, MemoryEmbeddingStore, MemoryFactExtractionStore, MemoryFactStore,
    MemoryMaintenanceStore, MemoryRecallStore, MemoryStore,
};

use crate::memory_index::MemoryEmbeddingIndex;

const MAX_EPISODES_IN_PROMPT: usize = 5;

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
    pub(crate) all_facts: Vec<haven_memory::repositories::facts::Fact>,
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
/// background memory worker.
pub struct MemoryService {
    db: Arc<Database>,
    fact_store: MemoryFactStore,
    fact_extraction_store: MemoryFactExtractionStore,
    maintenance_store: MemoryMaintenanceStore,
    memory_store: MemoryStore,
    recall_store: MemoryRecallStore,
    router: Option<Arc<LlmRouter>>,
    embedding_index: Option<MemoryEmbeddingIndex>,
    prompt_cache: Mutex<PromptMemoryCache>,
}

/// Narrow persistence capability handed to the worker.  Keeping this handle
/// private to the memory boundary avoids passing raw database ownership into
/// prompt and memory-worker composition code.
#[derive(Clone)]
pub(crate) struct MemoryDatabase(Arc<Database>);

impl Deref for MemoryDatabase {
    type Target = Arc<Database>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl MemoryService {
    pub fn new(db: Arc<Database>, router: Option<Arc<LlmRouter>>, embed_chunk_size: usize) -> Self {
        let fact_store = MemoryFactStore::new(db.clone());
        let fact_extraction_store = MemoryFactExtractionStore::new(db.clone());
        let maintenance_store = MemoryMaintenanceStore::new(db.clone());
        let recall_store = MemoryRecallStore::new(db.clone());
        let embedding_index = router.as_ref().map(|router| {
            MemoryEmbeddingIndex::new(
                MemoryEmbeddingStore::new(db.clone()),
                recall_store.clone(),
                router.clone(),
                embed_chunk_size.max(1),
            )
        });
        Self {
            fact_store,
            fact_extraction_store,
            maintenance_store,
            memory_store: MemoryStore::new(db.clone()),
            recall_store,
            db,
            router,
            embedding_index,
            prompt_cache: Mutex::new(PromptMemoryCache::new()),
        }
    }

    pub(crate) fn database_handle(&self) -> MemoryDatabase {
        MemoryDatabase(self.db.clone())
    }

    /// Return the shared store used for memory-owned durable episode and
    /// outbox persistence. The service and worker keep the same Database Arc.
    pub(crate) fn memory_store(&self) -> MemoryStore {
        self.memory_store.clone()
    }

    /// Return the shared typed fact capability used by the memory worker.
    pub(crate) fn memory_fact_store(&self) -> MemoryFactStore {
        self.fact_store.clone()
    }

    /// Return the shared persistence port for ordinary session fact
    /// extraction state and transcript projections.
    pub(crate) fn memory_fact_extraction_store(&self) -> MemoryFactExtractionStore {
        self.fact_extraction_store.clone()
    }

    /// Return the shared persistence port for deterministic maintenance SQL.
    pub(crate) fn memory_maintenance_store(&self) -> MemoryMaintenanceStore {
        self.maintenance_store.clone()
    }

    pub(crate) async fn context_window(&self, fallback: u32) -> u32 {
        match &self.router {
            Some(router) => router
                .context_window_for_request(RequestKind::Chat)
                .await
                .max(1),
            None => fallback.max(1),
        }
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

        let (vector, cacheable) = if embedding_model.is_empty()
            || !MemoryRetriever::visible_text(&query_text)
        {
            (None, true)
        } else if let Some(router) = &self.router {
            match router.embed_text(&query_text).await {
                Ok(vector) if !vector.is_empty() => (Some(vector), true),
                Ok(_) => (None, true),
                Err(error) => {
                    tracing::debug!(
                        "prompt memory embedding failed; skipping memory cache for this pass: {error}"
                    );
                    (None, false)
                }
            }
        } else {
            (None, true)
        };

        let candidates = async {
            let keyword_fact_hits = if query_text.trim().is_empty() {
                Vec::new()
            } else {
                let query = MemoryQuery::new(&query_text, MemoryKind::Fact, MAX_RECALL_LIMIT)?;
                self.recall_store.keyword_recall(query).await?
            };

            let (vector_fact_hits, vector_episode_hits) =
                if let Some(vector) = vector.as_deref().filter(|_| !embedding_model.is_empty()) {
                    let fact_query = MemoryQuery::new(&query_text, MemoryKind::Fact, 8)?
                        .with_fact_subject(Some("user"));
                    let episode_query =
                        MemoryQuery::new(&query_text, MemoryKind::Episode, MAX_EPISODES_IN_PROMPT)?
                            .with_excluded_session(key.exclude_session_id.as_deref());
                    let fact_hits = self
                        .recall_store
                        .vector_recall(fact_query, vector.to_vec(), embedding_model.clone())
                        .await?;
                    let episode_hits = self
                        .recall_store
                        .vector_recall(episode_query, vector.to_vec(), embedding_model.clone())
                        .await?;
                    (fact_hits, episode_hits)
                } else {
                    (Vec::new(), Vec::new())
                };

            let mut all_facts = self.recall_store.visible_user_facts(40).await?;
            let mut seen_ids: HashSet<String> =
                all_facts.iter().map(|fact| fact.id.clone()).collect();
            let candidate_ids: Vec<String> = keyword_fact_hits
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
                    if seen_ids.insert(fact.id.clone()) {
                        all_facts.push(fact);
                    }
                }
            }

            let keyword_episode_hits = if query_text.trim().is_empty() {
                Vec::new()
            } else {
                let query =
                    MemoryQuery::new(&query_text, MemoryKind::Episode, MAX_EPISODES_IN_PROMPT)?
                        .with_excluded_session(key.exclude_session_id.as_deref());
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

#[cfg(test)]
mod tests {
    use super::*;
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
        db.add_episode(&current.id, "The user researches Rust testing.")
            .unwrap();
        db.add_episode(&earlier.id, "The user researches Rust testing.")
            .unwrap();
        let service = MemoryService::new(db.clone(), None, 1);

        assert!(
            !service
                .has_cached_prompt_candidates(" Rust\n testing ", Some(&current.id))
                .await
        );
        let candidates = service
            .prompt_candidates(" Rust\n testing ", Some(&current.id))
            .await
            .unwrap();

        assert_eq!(candidates.query_text, "Rust testing");
        assert!(candidates.vector_fact_hits.is_empty());
        assert!(candidates.vector_episode_hits.is_empty());
        assert_eq!(candidates.all_facts.len(), 1);
        assert_eq!(candidates.all_facts[0].id, safe_fact.id);
        assert_eq!(candidates.keyword_episode_hits.len(), 1);
        assert_eq!(
            candidates.keyword_episode_hits[0].text,
            "The user researches Rust testing."
        );
        assert!(
            service
                .has_cached_prompt_candidates("Rust testing", Some(&current.id))
                .await
        );

        db.insert_fact("user", "uses", "Cargo", "user", 0.8, &[])
            .unwrap();
        assert!(
            !service
                .has_cached_prompt_candidates("Rust testing", Some(&current.id))
                .await
        );
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
            .recall(MemoryQuery::new("SQLite", MemoryKind::Fact, 5).unwrap())
            .await
            .unwrap();

        assert_eq!(recall.mode, haven_memory::MemoryRecallMode::Keyword);
        assert_eq!(recall.hits.len(), 1);
        assert_eq!(recall.hits[0].entity_id, safe.id);
        assert!(recall.hits[0].model.is_empty());
    }

    #[tokio::test]
    async fn prompt_candidate_store_errors_keep_the_existing_context() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.conn().execute_batch("DROP TABLE facts").unwrap();
        let service = MemoryService::new(db, None, 1);

        let error = match service.prompt_candidates("Rust", None).await {
            Ok(_) => panic!("expected prompt memory query to fail"),
            Err(error) => error,
        };

        assert_eq!(error.to_string(), "collect prompt memory candidates");
    }
}
