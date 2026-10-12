//! Bounded process-local query-result cache.
//!
//! This module deliberately owns no SQL and knows nothing about repository
//! policies. It provides the shared mechanics used by `Database`: TTL,
//! bounded LRU eviction, and generation checks that prevent an in-flight
//! stale read from overwriting a newer result.
//!
//! Each operation holds one short-lived `std::sync::Mutex` guard; values are
//! cloned out before the guard is released, and no SQL, await, or provider
//! call happens while it is held. The store has no async work to cancel. If a
//! caller drops a blocking read after the database query has started, its
//! eventual write is still protected by the generation check.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::embeddings::EmbeddedText;
use crate::repositories::facts::Fact;
use crate::repositories::messages::Message;
use crate::repositories::sessions::Session;

/// Maximum number of logical query keys retained by the process-local cache.
/// A key holds one result slot, so this bounds cached result-set memory even
/// when a long-lived process visits many sessions or fact subjects.
pub(crate) const QUERY_CACHE_MAX_KEYS: usize = 256;

/// Maximum number of invalidated query keys whose generations are retained.
/// If this bound is reached, the cache is flushed and each query domain is
/// advanced so no in-flight read from before the flush can repopulate it.
pub(crate) const QUERY_CACHE_MAX_KEY_GENERATIONS: usize = QUERY_CACHE_MAX_KEYS * 4;

/// Internal key for the global `list_facts` result.
pub(crate) const ALL_FACTS_CACHE_KEY: &str = "facts:all";

/// Build a subject-scoped key without allowing a subject value to alias a
/// different query shape such as the global facts list.
pub(crate) fn fact_subject_cache_key(subject: &str) -> String {
    format!("facts:subject:{subject}")
}

#[derive(Clone)]
struct CacheEntry<T: Clone> {
    data: T,
    expiry: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CacheGeneration {
    domain: u64,
    key: u64,
}

#[derive(Clone, Copy)]
enum CacheDomain {
    Messages = 0,
    SessionHistory = 1,
    Facts = 2,
    Embeddings = 3,
}

impl CacheDomain {
    fn for_key(key: &str) -> Self {
        if key == "_session_history_page" {
            Self::SessionHistory
        } else if key == ALL_FACTS_CACHE_KEY || key.starts_with("facts:subject:") {
            Self::Facts
        } else if key.starts_with("_embeddings_") {
            Self::Embeddings
        } else {
            Self::Messages
        }
    }
}

#[derive(Clone)]
struct QueryCache {
    messages: Option<CacheEntry<Vec<Message>>>,
    session_history_page: Option<CacheEntry<Vec<Session>>>,
    facts: Option<CacheEntry<Vec<Fact>>>,
    embeddings: Option<CacheEntry<Vec<EmbeddedText>>>,
    last_used: u64,
}

impl QueryCache {
    fn empty(last_used: u64) -> Self {
        Self {
            messages: None,
            session_history_page: None,
            facts: None,
            embeddings: None,
            last_used,
        }
    }

    fn is_empty(&self) -> bool {
        self.messages.is_none()
            && self.session_history_page.is_none()
            && self.facts.is_none()
            && self.embeddings.is_none()
    }

    fn expire(&mut self, now: Instant) {
        if self
            .messages
            .as_ref()
            .is_some_and(|entry| entry.expiry <= now)
        {
            self.messages = None;
        }
        if self
            .session_history_page
            .as_ref()
            .is_some_and(|entry| entry.expiry <= now)
        {
            self.session_history_page = None;
        }
        if self.facts.as_ref().is_some_and(|entry| entry.expiry <= now) {
            self.facts = None;
        }
        if self
            .embeddings
            .as_ref()
            .is_some_and(|entry| entry.expiry <= now)
        {
            self.embeddings = None;
        }
    }
}

struct CacheState {
    entries: HashMap<String, QueryCache>,
    /// Per-key generations remain even when an entry is absent. This is what
    /// closes the race where a query starts before its first cache write and
    /// the owning row is invalidated before that write completes.
    key_generations: HashMap<String, u64>,
    /// Bulk generations are bounded and avoid making unrelated domains miss
    /// together. For example, a fact maintenance pass does not invalidate
    /// session-message cache writes.
    domain_generations: [u64; 4],
    clock: u64,
}

impl CacheState {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            key_generations: HashMap::new(),
            domain_generations: [0; 4],
            clock: 0,
        }
    }

    fn generation_for(&self, key: &str) -> CacheGeneration {
        let domain = CacheDomain::for_key(key) as usize;
        CacheGeneration {
            domain: self.domain_generations[domain],
            key: self.key_generations.get(key).copied().unwrap_or(0),
        }
    }

    fn touch(&mut self, key: &str) {
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.last_used = self.clock;
        }
    }

    fn bump_key(&mut self, key: &str) {
        let next = self
            .key_generations
            .get(key)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.key_generations.insert(key.to_string(), next);
    }

    fn ensure_key_generation_capacity(&mut self, keys: &[&str]) {
        let mut additional = 0;
        for (index, key) in keys.iter().enumerate() {
            if keys[..index].contains(key) || self.key_generations.contains_key(*key) {
                continue;
            }
            additional += 1;
        }
        if self.key_generations.len().saturating_add(additional) <= QUERY_CACHE_MAX_KEY_GENERATIONS
        {
            return;
        }

        // Dropping an individual tombstone could let a query that started
        // before its invalidation write stale data back. A bounded reset
        // instead invalidates every in-flight key while keeping domain
        // generations small and the tombstone map finite.
        for generation in &mut self.domain_generations {
            *generation = generation.saturating_add(1);
        }
        self.entries.clear();
        self.key_generations.clear();
    }

    fn bump_domain(&mut self, domain: CacheDomain) {
        let slot = domain as usize;
        self.domain_generations[slot] = self.domain_generations[slot].saturating_add(1);
    }

    fn remove_if_empty(&mut self, key: &str) {
        if self.entries.get(key).is_some_and(QueryCache::is_empty) {
            self.entries.remove(key);
        }
    }

    fn purge_expired(&mut self, now: Instant) {
        for entry in self.entries.values_mut() {
            entry.expire(now);
        }
        self.entries.retain(|_, entry| !entry.is_empty());
    }

    fn evict_if_full(&mut self, key: &str) {
        if self.entries.contains_key(key) || self.entries.len() < QUERY_CACHE_MAX_KEYS {
            return;
        }
        if let Some(oldest) = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        {
            self.entries.remove(&oldest);
        }
    }
}

/// Thread-safe cache mechanics used by the database facade.
pub(crate) struct QueryResultCache {
    state: Mutex<CacheState>,
}

impl QueryResultCache {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(CacheState::new()),
        }
    }

    pub(crate) fn generation(&self, key: &str) -> CacheGeneration {
        self.state
            .lock()
            .map(|state| state.generation_for(key))
            .unwrap_or(CacheGeneration { domain: 0, key: 0 })
    }

    pub(crate) fn get_messages(&self, key: &str) -> Option<Vec<Message>> {
        let mut state = self.state.lock().ok()?;
        let result = state
            .entries
            .get(key)
            .and_then(|cache| cache.messages.as_ref())
            .and_then(|entry| (entry.expiry > Instant::now()).then(|| entry.data.clone()));
        if result.is_some() {
            state.touch(key);
        } else {
            if let Some(cache) = state.entries.get_mut(key) {
                cache.messages = None;
            }
            state.remove_if_empty(key);
        }
        result
    }

    pub(crate) fn put_messages(
        &self,
        key: &str,
        data: Vec<Message>,
        ttl_secs: u64,
        generation: CacheGeneration,
    ) {
        self.put(key, data, ttl_secs, generation, |cache, entry| {
            cache.messages = Some(entry)
        });
    }

    pub(crate) fn get_session_history_page(&self) -> Option<Vec<Session>> {
        let mut state = self.state.lock().ok()?;
        let key = "_session_history_page";
        let result = state
            .entries
            .get(key)
            .and_then(|cache| cache.session_history_page.as_ref())
            .and_then(|entry| (entry.expiry > Instant::now()).then(|| entry.data.clone()));
        if result.is_some() {
            state.touch(key);
        } else {
            if let Some(cache) = state.entries.get_mut(key) {
                cache.session_history_page = None;
            }
            state.remove_if_empty(key);
        }
        result
    }

    pub(crate) fn put_session_history_page(
        &self,
        data: Vec<Session>,
        ttl_secs: u64,
        generation: CacheGeneration,
    ) {
        self.put(
            "_session_history_page",
            data,
            ttl_secs,
            generation,
            |cache, entry| cache.session_history_page = Some(entry),
        );
    }

    pub(crate) fn get_facts(&self, subject: &str) -> Option<Vec<Fact>> {
        let key = fact_subject_cache_key(subject);
        let mut state = self.state.lock().ok()?;
        let result = state
            .entries
            .get(&key)
            .and_then(|cache| cache.facts.as_ref())
            .and_then(|entry| (entry.expiry > Instant::now()).then(|| entry.data.clone()));
        if result.is_some() {
            state.touch(&key);
        } else {
            if let Some(cache) = state.entries.get_mut(&key) {
                cache.facts = None;
            }
            state.remove_if_empty(&key);
        }
        result
    }

    pub(crate) fn put_facts(
        &self,
        subject: &str,
        data: Vec<Fact>,
        ttl_secs: u64,
        generation: CacheGeneration,
    ) {
        self.put(
            &fact_subject_cache_key(subject),
            data,
            ttl_secs,
            generation,
            |cache, entry| cache.facts = Some(entry),
        );
    }

    pub(crate) fn get_facts_all(&self) -> Option<Vec<Fact>> {
        let mut state = self.state.lock().ok()?;
        let key = ALL_FACTS_CACHE_KEY;
        let result = state
            .entries
            .get(key)
            .and_then(|cache| cache.facts.as_ref())
            .and_then(|entry| (entry.expiry > Instant::now()).then(|| entry.data.clone()));
        if result.is_some() {
            state.touch(key);
        } else {
            if let Some(cache) = state.entries.get_mut(key) {
                cache.facts = None;
            }
            state.remove_if_empty(key);
        }
        result
    }

    pub(crate) fn put_facts_all(
        &self,
        data: Vec<Fact>,
        ttl_secs: u64,
        generation: CacheGeneration,
    ) {
        self.put(
            ALL_FACTS_CACHE_KEY,
            data,
            ttl_secs,
            generation,
            |cache, entry| cache.facts = Some(entry),
        );
    }

    pub(crate) fn get_embeddings(&self, entity_type: &str) -> Option<Vec<EmbeddedText>> {
        let key = format!("_embeddings_{entity_type}");
        let mut state = self.state.lock().ok()?;
        let result = state
            .entries
            .get(&key)
            .and_then(|cache| cache.embeddings.as_ref())
            .and_then(|entry| (entry.expiry > Instant::now()).then(|| entry.data.clone()));
        if result.is_some() {
            state.touch(&key);
        } else {
            if let Some(cache) = state.entries.get_mut(&key) {
                cache.embeddings = None;
            }
            state.remove_if_empty(&key);
        }
        result
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn put_embeddings(
        &self,
        entity_type: &str,
        data: Vec<EmbeddedText>,
        ttl_secs: u64,
        generation: CacheGeneration,
    ) {
        self.put(
            &format!("_embeddings_{entity_type}"),
            data,
            ttl_secs,
            generation,
            |cache, entry| cache.embeddings = Some(entry),
        );
    }

    fn put<T: Clone + Send>(
        &self,
        key: &str,
        data: T,
        ttl_secs: u64,
        generation: CacheGeneration,
        set: impl FnOnce(&mut QueryCache, CacheEntry<T>),
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.generation_for(key) != generation {
            return;
        }
        state.purge_expired(Instant::now());
        state.evict_if_full(key);
        state.clock = state.clock.saturating_add(1);
        let last_used = state.clock;
        let cache = state
            .entries
            .entry(key.to_string())
            .or_insert_with(|| QueryCache::empty(last_used));
        cache.last_used = last_used;
        set(
            cache,
            CacheEntry {
                data,
                expiry: Instant::now() + Duration::from_secs(ttl_secs),
            },
        );
    }

    pub(crate) fn invalidate_messages(&self, session_id: &str) {
        self.invalidate_key(session_id, |cache| cache.messages = None);
    }

    pub(crate) fn invalidate_all_messages(&self) {
        self.invalidate_domain(CacheDomain::Messages, |cache| cache.messages = None);
    }

    pub(crate) fn invalidate_session_history_page(&self) {
        self.invalidate_key("_session_history_page", |cache| {
            cache.session_history_page = None
        });
    }

    pub(crate) fn invalidate_facts(&self, subject: &str) {
        let subject_key = fact_subject_cache_key(subject);
        self.invalidate_keys(&[&subject_key, ALL_FACTS_CACHE_KEY], |cache| {
            cache.facts = None
        });
    }

    pub(crate) fn invalidate_all_facts(&self) {
        self.invalidate_domain(CacheDomain::Facts, |cache| cache.facts = None);
    }

    pub(crate) fn invalidate_embeddings(&self, entity_type: &str) {
        self.invalidate_key(&format!("_embeddings_{entity_type}"), |cache| {
            cache.embeddings = None
        });
    }

    pub(crate) fn invalidate_memory(&self) {
        self.invalidate_domain(CacheDomain::Embeddings, |cache| cache.embeddings = None);
    }

    fn invalidate_key(&self, key: &str, clear: impl Fn(&mut QueryCache)) {
        self.invalidate_keys(&[key], clear);
    }

    fn invalidate_keys(&self, keys: &[&str], clear: impl Fn(&mut QueryCache)) {
        if let Ok(mut state) = self.state.lock() {
            state.ensure_key_generation_capacity(keys);
            for (index, key) in keys.iter().enumerate() {
                if keys[..index].contains(key) {
                    continue;
                }
                let key = *key;
                state.bump_key(key);
                if let Some(cache) = state.entries.get_mut(key) {
                    clear(cache);
                }
                state.remove_if_empty(key);
            }
        }
    }

    fn invalidate_domain(&self, domain: CacheDomain, clear: impl Fn(&mut QueryCache)) {
        if let Ok(mut state) = self.state.lock() {
            state.bump_domain(domain);
            for cache in state.entries.values_mut() {
                clear(cache);
            }
            state.entries.retain(|_, cache| !cache.is_empty());
        }
    }
}

impl Default for QueryResultCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fact(fact_id: &str, subject: &str) -> Fact {
        Fact {
            fact_id: fact_id.to_string(),
            subject: subject.to_string(),
            predicate: "likes".to_string(),
            object: fact_id.to_string(),
            source: "user".to_string(),
            confidence: 1.0,
            tags: Vec::new(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            mention_count: 1,
            last_seen_at: None,
            source_ref: None,
            durability: 1.0,
        }
    }

    #[test]
    fn fact_subject_all_has_a_distinct_cache_key_from_the_global_list() {
        let cache = QueryResultCache::new();
        let subject_key = fact_subject_cache_key("all");
        assert_ne!(subject_key, ALL_FACTS_CACHE_KEY);

        let subject_generation = cache.generation(&subject_key);
        let all_generation = cache.generation(ALL_FACTS_CACHE_KEY);
        cache.put_facts(
            "all",
            vec![fact("subject-fact", "all")],
            60,
            subject_generation,
        );
        cache.put_facts_all(vec![fact("global-fact", "someone")], 60, all_generation);

        assert_eq!(cache.get_facts("all").unwrap()[0].fact_id, "subject-fact");
        assert_eq!(cache.get_facts_all().unwrap()[0].fact_id, "global-fact");
    }

    #[test]
    fn fact_invalidation_clears_subject_and_global_entries_and_rejects_old_reads() {
        let cache = QueryResultCache::new();
        let subject = "person";
        let subject_key = fact_subject_cache_key(subject);
        let subject_generation = cache.generation(&subject_key);
        let all_generation = cache.generation(ALL_FACTS_CACHE_KEY);
        cache.put_facts(
            subject,
            vec![fact("subject-fact", subject)],
            60,
            subject_generation,
        );
        cache.put_facts_all(vec![fact("global-fact", subject)], 60, all_generation);

        cache.invalidate_facts(subject);

        assert!(cache.get_facts(subject).is_none());
        assert!(cache.get_facts_all().is_none());
        cache.put_facts(
            subject,
            vec![fact("stale-subject", subject)],
            60,
            subject_generation,
        );
        cache.put_facts_all(vec![fact("stale-global", subject)], 60, all_generation);
        assert!(cache.get_facts(subject).is_none());
        assert!(cache.get_facts_all().is_none());
    }

    #[test]
    fn key_generation_tombstones_stay_bounded_without_revalidating_old_reads() {
        let cache = QueryResultCache::new();
        let stale_key = "session-before-generation-reset";
        let stale_generation = cache.generation(stale_key);
        cache.put_messages(stale_key, Vec::new(), 60, stale_generation);

        for index in 0..QUERY_CACHE_MAX_KEY_GENERATIONS {
            cache.invalidate_messages(&format!("session-{index}"));
        }
        cache.invalidate_messages("session-that-triggers-generation-reset");

        let state = cache.state.lock().unwrap();
        assert!(state.key_generations.len() <= QUERY_CACHE_MAX_KEY_GENERATIONS);
        assert!(state.entries.is_empty());
        drop(state);

        cache.put_messages(stale_key, Vec::new(), 60, stale_generation);
        assert!(
            cache.get_messages(stale_key).is_none(),
            "the domain epoch reset must reject reads started before the reset"
        );
    }
}
