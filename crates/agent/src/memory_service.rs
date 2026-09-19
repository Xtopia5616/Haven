//! Typed memory access used by prompt context and the memory worker.
//!
//! This module is the only agent-side owner of memory query plumbing.  Prompt
//! assembly receives a bounded candidate snapshot and never needs to know
//! about SQLite, embedding providers, or the memory cache layout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::Deref;
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::recall::{
    MAX_MEMORY_QUERY_CHARS, MAX_RECALL_LIMIT, MemoryKind, MemoryQuery, MemoryRecall,
    MemoryRetriever,
};

use crate::memory_index::{MemoryEmbeddingIndex, embedding_index_model};

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
    router: Option<Arc<LlmRouter>>,
    embedding_index: Option<MemoryEmbeddingIndex>,
    prompt_cache: Mutex<PromptMemoryCache>,
}

/// Narrow persistence capability handed to the worker.  Keeping this handle
/// private to the memory boundary avoids passing raw database ownership into
/// prompt and inference composition code.
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
        let embedding_index = router.as_ref().map(|router| {
            MemoryEmbeddingIndex::new(db.clone(), router.clone(), embed_chunk_size.max(1))
        });
        Self {
            db,
            router,
            embedding_index,
            prompt_cache: Mutex::new(PromptMemoryCache::new()),
        }
    }

    pub(crate) fn database_handle(&self) -> MemoryDatabase {
        MemoryDatabase(self.db.clone())
    }

    pub(crate) async fn context_window(&self, fallback: u32) -> u32 {
        match &self.router {
            Some(router) => router
                .context_window_for_role(EndpointRole::DefaultModel)
                .await
                .max(1),
            None => fallback.max(1),
        }
    }

    pub(crate) fn memory_revision(&self) -> u64 {
        self.db.memory_revision()
    }

    pub(crate) async fn current_embedding_model(&self) -> String {
        let Some(router) = &self.router else {
            return String::new();
        };
        if !router
            .is_role_configured(EndpointRole::EmbeddingModel)
            .await
        {
            return String::new();
        }
        let endpoint = router
            .config()
            .await
            .endpoint(EndpointRole::EmbeddingModel)
            .clone();
        if endpoint.model_name.trim().is_empty() {
            String::new()
        } else {
            embedding_index_model(&endpoint)
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

        let db = self.db.clone();
        let query_for_db = query_text.clone();
        let embedding_model_for_db = embedding_model.clone();
        let exclude_for_db = key.exclude_session_id.clone();
        let candidates = db
            .run_blocking(move |db| {
                collect_prompt_candidates(
                    db,
                    &query_for_db,
                    &embedding_model_for_db,
                    vector.as_deref(),
                    exclude_for_db.as_deref(),
                )
            })
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

    /// Execute a fully-scoped typed recall request.
    pub async fn recall(&self, query: MemoryQuery) -> anyhow::Result<MemoryRecall> {
        let vector_hits = match &self.embedding_index {
            Some(index) => index.search(&query).await?,
            None => None,
        };
        let query_for_db = query.clone();
        self.db
            .run_blocking(move |db| MemoryRetriever::new(db).retrieve(&query_for_db, vector_hits))
            .await
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

fn collect_prompt_candidates(
    db: &Database,
    query_text: &str,
    embedding_model: &str,
    vector: Option<&[f32]>,
    exclude_session_id: Option<&str>,
) -> anyhow::Result<PromptMemoryCandidates> {
    let retriever = MemoryRetriever::new(db);
    let keyword_fact_hits = if query_text.trim().is_empty() {
        Vec::new()
    } else {
        let query = MemoryQuery::new(query_text, MemoryKind::Fact, MAX_RECALL_LIMIT)?;
        retriever.keyword(&query)?
    };

    let (vector_fact_hits, vector_episode_hits) =
        if let Some(vector) = vector.filter(|_| !embedding_model.is_empty()) {
            let fact_query = MemoryQuery::new(query_text, MemoryKind::Fact, 8)?;
            let episode_query =
                MemoryQuery::new(query_text, MemoryKind::Episode, MAX_EPISODES_IN_PROMPT)?;
            (
                retriever.vector(
                    &fact_query.with_fact_subject(Some("user")),
                    vector,
                    embedding_model,
                )?,
                retriever.vector(
                    &episode_query.with_excluded_session(exclude_session_id),
                    vector,
                    embedding_model,
                )?,
            )
        } else {
            (Vec::new(), Vec::new())
        };

    let mut all_facts = MemoryRetriever::filter_visible_facts(db.get_facts_limited("user", 40)?);
    let mut seen_ids: HashSet<String> = all_facts.iter().map(|fact| fact.id.clone()).collect();
    let candidate_ids: Vec<String> = keyword_fact_hits
        .iter()
        .chain(vector_fact_hits.iter())
        .map(|hit| hit.entity_id.clone())
        .filter(|id| !id.is_empty() && !seen_ids.contains(id))
        .collect();
    if !candidate_ids.is_empty() {
        for fact in MemoryRetriever::filter_visible_facts(db.get_facts_by_ids(&candidate_ids)?) {
            if seen_ids.insert(fact.id.clone()) {
                all_facts.push(fact);
            }
        }
    }

    let keyword_episode_hits = if query_text.trim().is_empty() {
        Vec::new()
    } else {
        let query = MemoryQuery::new(query_text, MemoryKind::Episode, MAX_EPISODES_IN_PROMPT)?;
        retriever.keyword(&query.with_excluded_session(exclude_session_id))?
    };

    Ok(PromptMemoryCandidates {
        query_text: String::new(),
        vector_fact_hits,
        vector_episode_hits,
        keyword_episode_hits,
        all_facts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
