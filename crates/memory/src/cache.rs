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

#[derive(Clone)]
struct CacheEntry<T: Clone> {
    data: T,
    expiry: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheGeneration {
    domain: u64,
    key: u64,
}

#[derive(Clone, Copy)]
enum CacheDomain {
    Messages = 0,
    Sessions = 1,
    Facts = 2,
    Embeddings = 3,
}

impl CacheDomain {
    fn for_key(key: &str) -> Self {
        if key == "_sessions" {
            Self::Sessions
        } else if key == "_facts_all" || key.starts_with("_facts_") {
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
    sessions: Option<CacheEntry<Vec<Session>>>,
    facts: Option<CacheEntry<Vec<Fact>>>,
    embeddings: Option<CacheEntry<Vec<EmbeddedText>>>,
    last_used: u64,
}

impl QueryCache {
    fn empty(last_used: u64) -> Self {
        Self {
            messages: None,
            sessions: None,
            facts: None,
            embeddings: None,
            last_used,
        }
    }

    fn is_empty(&self) -> bool {
        self.messages.is_none()
            && self.sessions.is_none()
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
            .sessions
            .as_ref()
            .is_some_and(|entry| entry.expiry <= now)
        {
            self.sessions = None;
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
pub(crate) struct QueryCacheStore {
    state: Mutex<CacheState>,
}

impl QueryCacheStore {
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

    pub(crate) fn get_sessions(&self) -> Option<Vec<Session>> {
        let mut state = self.state.lock().ok()?;
        let key = "_sessions";
        let result = state
            .entries
            .get(key)
            .and_then(|cache| cache.sessions.as_ref())
            .and_then(|entry| (entry.expiry > Instant::now()).then(|| entry.data.clone()));
        if result.is_some() {
            state.touch(key);
        } else {
            if let Some(cache) = state.entries.get_mut(key) {
                cache.sessions = None;
            }
            state.remove_if_empty(key);
        }
        result
    }

    pub(crate) fn put_sessions(
        &self,
        data: Vec<Session>,
        ttl_secs: u64,
        generation: CacheGeneration,
    ) {
        self.put("_sessions", data, ttl_secs, generation, |cache, entry| {
            cache.sessions = Some(entry)
        });
    }

    pub(crate) fn get_facts(&self, subject: &str) -> Option<Vec<Fact>> {
        let key = format!("_facts_{subject}");
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
            &format!("_facts_{subject}"),
            data,
            ttl_secs,
            generation,
            |cache, entry| cache.facts = Some(entry),
        );
    }

    pub(crate) fn get_facts_all(&self) -> Option<Vec<Fact>> {
        let mut state = self.state.lock().ok()?;
        let key = "_facts_all";
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
        self.put("_facts_all", data, ttl_secs, generation, |cache, entry| {
            cache.facts = Some(entry)
        });
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

    pub(crate) fn invalidate_sessions(&self) {
        self.invalidate_key("_sessions", |cache| cache.sessions = None);
    }

    pub(crate) fn invalidate_facts(&self, subject: &str) {
        let key = format!("_facts_{subject}");
        self.invalidate_key(&key, |cache| cache.facts = None);
        self.invalidate_key("_facts_all", |cache| cache.facts = None);
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
        if let Ok(mut state) = self.state.lock() {
            state.bump_key(key);
            if let Some(cache) = state.entries.get_mut(key) {
                clear(cache);
            }
            state.remove_if_empty(key);
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

impl Default for QueryCacheStore {
    fn default() -> Self {
        Self::new()
    }
}
