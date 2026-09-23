//! Independently constructible ReActEngine sidecars (Phase 8 / A2).
//!
//! Each type owns one mutex-backed concern previously inlined on
//! `ReActEngine`. The engine remains a facade that holds collaboration
//! deps plus these named sidecars (+ `IdentityMap`, `CheckpointStore`, hooks).

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use crate::compactor::estimate_message_tokens;
use haven_common::config::RequestKind;
use haven_common::types::CanonicalMessage;
use haven_llm::ToolDefinition;
use haven_tools::MessagingService;

/// Process-wide inbox transport for cross-session polling.
///
/// Per-session notification cursors, poll cadence and title cache live on
/// `SessionState`. This sidecar only shares the messaging service and coalesces
/// heartbeat tasks so one session cannot fill the blocking pool.
pub(crate) struct MessagingPoller {
    service: Arc<MessagingService>,
    /// Sessions with a heartbeat `spawn_blocking` already queued/running —
    /// coalesce so steps cannot unboundedly fill the blocking pool.
    heartbeat_inflight: Arc<Mutex<HashSet<String>>>,
}

impl MessagingPoller {
    pub(crate) fn new() -> Self {
        Self::with_service(Arc::new(MessagingService::default_root()))
    }

    pub(crate) fn with_service(service: Arc<MessagingService>) -> Self {
        Self {
            service,
            heartbeat_inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub(crate) fn service(&self) -> Arc<MessagingService> {
        Arc::clone(&self.service)
    }

    /// Claim a heartbeat slot for `session_id`. Returns `None` when one is
    /// already in flight; otherwise returns a clone of the inflight set so
    /// the blocking task can release the slot when done.
    pub(crate) fn try_begin_heartbeat(
        &self,
        session_id: &str,
    ) -> Option<Arc<Mutex<HashSet<String>>>> {
        let mut set = self.heartbeat_inflight.lock().unwrap();
        if !set.insert(session_id.to_string()) {
            return None;
        }
        Some(Arc::clone(&self.heartbeat_inflight))
    }

    /// Release a coalesced heartbeat slot. Session poll state is owned by the
    /// actor and cleared through `SessionActorHandle::clear_messaging_now`.
    pub(crate) fn clear_session(&self, session_id: &str) {
        self.heartbeat_inflight.lock().unwrap().remove(session_id);
    }
}

impl Default for MessagingPoller {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-session tool-definition cache keyed by the global catalog and the
/// session-local registration overlay version.
/// Values are `Arc` so cache hits share one schema vec across steps. It is
/// bounded because ended sessions are normally removed eagerly, but a burst
/// of short-lived sessions must not grow this sidecar without limit.
#[allow(dead_code)]
type ToolDefCacheEntry = ((u64, u64), Arc<Vec<ToolDefinition>>);
#[allow(dead_code)]
type ToolDefCacheMap = HashMap<String, ToolDefCacheEntry>;

#[allow(dead_code)]
struct ToolDefCacheState {
    entries: ToolDefCacheMap,
    order: VecDeque<String>,
}

#[allow(dead_code)]
pub(crate) struct ToolDefCache {
    cache: Mutex<ToolDefCacheState>,
}

#[allow(dead_code)]
impl ToolDefCache {
    pub(crate) const CAPACITY: usize = 128;

    pub(crate) fn new() -> Self {
        Self {
            cache: Mutex::new(ToolDefCacheState {
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    pub(super) fn get_if_version(
        &self,
        session_id: &str,
        version: (u64, u64),
    ) -> Option<Arc<Vec<ToolDefinition>>> {
        let mut state = self.cache.lock().unwrap();
        let result = state
            .entries
            .get(session_id)
            .filter(|(v, _)| *v == version)
            .map(|(_, defs)| Arc::clone(defs));
        if result.is_some() {
            state.order.retain(|cached| cached != session_id);
            state.order.push_back(session_id.to_string());
        }
        result
    }

    pub(super) fn insert(
        &self,
        session_id: &str,
        version: (u64, u64),
        defs: Arc<Vec<ToolDefinition>>,
    ) {
        let mut state = self.cache.lock().unwrap();
        state.order.retain(|cached| cached != session_id);
        while state.entries.len() >= Self::CAPACITY && !state.entries.contains_key(session_id) {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            state.entries.remove(&oldest);
        }
        state
            .entries
            .insert(session_id.to_string(), (version, defs));
        state.order.push_back(session_id.to_string());
    }

    pub(crate) fn remove(&self, session_id: &str) {
        let mut state = self.cache.lock().unwrap();
        state.entries.remove(session_id);
        state.order.retain(|cached| cached != session_id);
    }
}

impl Default for ToolDefCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Incremental token estimate for a session's canonical message list.
#[derive(Debug, Clone, Default)]
pub(super) struct TokenEstimate {
    msgs_len: usize,
    tokens: u32,
    /// Identity of the in-memory canonical projection represented by `tokens`.
    /// Revision alone is insufficient because resume/rollback rebuilds start
    /// at revision zero for a new state object.
    generation: u64,
    /// Revision of the in-memory canonical projection represented by
    /// `tokens`. The owner increments this whenever the projection changes;
    /// no full JSON fingerprint is needed on the hot path.
    revision: u64,
}

/// Per-session incremental token-estimate cache.
/// Entries are bounded for the same reason as [`ToolDefCache`]; eviction only
/// costs a fresh estimate and cannot affect durable canonical state.
pub(crate) struct TokenEstimateCache {
    cache: Mutex<TokenEstimateCacheState>,
}

struct TokenEstimateCacheState {
    entries: HashMap<String, TokenEstimate>,
    order: VecDeque<String>,
}

impl TokenEstimateCache {
    pub(crate) const CAPACITY: usize = 256;

    pub(crate) fn new() -> Self {
        Self {
            cache: Mutex::new(TokenEstimateCacheState {
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
        }
    }

    pub(crate) fn estimate(
        &self,
        session_id: &str,
        canonical: &[CanonicalMessage],
        generation: u64,
        revision: u64,
    ) -> u32 {
        // Snapshot the prior entry under the lock; run tiktoken outside so
        // concurrent sessions are not serialized behind one mutex for the
        // whole estimate.
        let prior = {
            let state = self.cache.lock().unwrap();
            state.entries.get(session_id).cloned()
        };
        if let Some(entry) = &prior
            && entry.generation == generation
            && entry.revision == revision
            && entry.msgs_len == canonical.len()
        {
            let mut state = self.cache.lock().unwrap();
            if state.entries.contains_key(session_id) {
                state.order.retain(|cached| cached != session_id);
                state.order.push_back(session_id.to_string());
            }
            return entry.tokens;
        }

        // A revision change without an explicit append notification means the
        // projection may have been replaced or edited in place. Re-tokenize
        // once rather than hashing every message on every turn. Normal
        // transcript appends update this entry through `append_message` below.
        let new_msgs_len = canonical.len();
        let new_tokens = estimate_message_tokens(canonical);
        let mut state = self.cache.lock().unwrap();
        state.order.retain(|cached| cached != session_id);
        while state.entries.len() >= Self::CAPACITY && !state.entries.contains_key(session_id) {
            let Some(oldest) = state.order.pop_front() else {
                break;
            };
            state.entries.remove(&oldest);
        }
        let session_key = session_id.to_string();
        state.entries.insert(
            session_key.clone(),
            TokenEstimate {
                msgs_len: new_msgs_len,
                tokens: new_tokens,
                generation,
                revision,
            },
        );
        state.order.push_back(session_key);
        new_tokens
    }

    /// Extend a cached estimate after the canonical projection appended one
    /// message. This is called at the single transcript projection boundary,
    /// so subsequent turn-start checks remain O(1) with respect to history
    /// length. If the cache is cold or the revision does not line up, leave it
    /// untouched and let the next `estimate` rebuild it safely.
    pub(crate) fn append_message(
        &self,
        session_id: &str,
        message: &CanonicalMessage,
        canonical_len: usize,
        generation: u64,
        revision: u64,
    ) {
        let can_append = {
            let state = self.cache.lock().unwrap();
            state.entries.get(session_id).is_some_and(|entry| {
                entry.generation == generation
                    && entry.revision.saturating_add(1) == revision
                    && entry.msgs_len.saturating_add(1) == canonical_len
            })
        };
        if !can_append {
            return;
        }

        // Keep the cache lock free while running the tokenizer. A concurrent
        // replacement can invalidate this append; the second validation below
        // then safely leaves the cache for the next full rebuild.
        let message_tokens = estimate_message_tokens(std::slice::from_ref(message));
        let mut state = self.cache.lock().unwrap();
        let Some(entry) = state.entries.get_mut(session_id) else {
            return;
        };
        if entry.generation != generation
            || entry.revision.saturating_add(1) != revision
            || entry.msgs_len.saturating_add(1) != canonical_len
        {
            return;
        }
        entry.tokens = entry.tokens.saturating_add(message_tokens);
        entry.msgs_len = canonical_len;
        entry.revision = revision;
        state.order.retain(|cached| cached != session_id);
        state.order.push_back(session_id.to_string());
    }

    pub(crate) fn remove(&self, session_id: &str) {
        let mut state = self.cache.lock().unwrap();
        state.entries.remove(session_id);
        state.order.retain(|cached| cached != session_id);
    }
}

impl Default for TokenEstimateCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-request context-window cache keyed by the router instance pointer.
pub(crate) struct ContextWindowCache {
    cache: Mutex<(usize, HashMap<RequestKind, u32>)>,
}

impl ContextWindowCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: Mutex::new((0, HashMap::new())),
        }
    }

    pub(super) fn get(&self, router_ptr: usize, request: RequestKind) -> Option<u32> {
        let cache = self.cache.lock().unwrap();
        if cache.0 == router_ptr {
            cache.1.get(&request).copied()
        } else {
            None
        }
    }

    pub(super) fn insert(&self, router_ptr: usize, request: RequestKind, window: u32) {
        let mut cache = self.cache.lock().unwrap();
        if cache.0 != router_ptr {
            cache.0 = router_ptr;
            cache.1.clear();
        }
        cache.1.insert(request, window);
    }

    /// Drop all cached windows (e.g. after `context_limits` hot-reload).
    pub(super) fn clear(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.0 = 0;
        cache.1.clear();
    }
}

impl Default for ContextWindowCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_session_releases_heartbeat_slot() {
        let poller = MessagingPoller::new();
        assert!(poller.try_begin_heartbeat("ses-a").is_some());
        assert!(poller.try_begin_heartbeat("ses-a").is_none());
        poller.clear_session("ses-a");
        assert!(poller.try_begin_heartbeat("ses-a").is_some());
    }

    #[test]
    fn tool_definition_cache_is_bounded_and_evicts_least_recently_used() {
        let cache = ToolDefCache::new();
        for index in 0..ToolDefCache::CAPACITY {
            cache.insert(&format!("ses-{index}"), (0, 0), Arc::new(Vec::new()));
        }
        assert!(cache.get_if_version("ses-0", (0, 0)).is_some());

        cache.insert("ses-overflow", (0, 0), Arc::new(Vec::new()));

        assert!(cache.get_if_version("ses-0", (0, 0)).is_some());
        assert!(cache.get_if_version("ses-overflow", (0, 0)).is_some());
        assert!(cache.get_if_version("ses-1", (0, 0)).is_none());
    }

    #[test]
    fn token_estimate_cache_is_bounded_and_evicts_least_recently_used() {
        let cache = TokenEstimateCache::new();
        for index in 0..TokenEstimateCache::CAPACITY {
            cache.estimate(&format!("ses-{index}"), &[], 1, 0);
        }
        cache.estimate("ses-0", &[], 1, 0);

        cache.estimate("ses-overflow", &[], 1, 0);

        let state = cache.cache.lock().unwrap();
        assert_eq!(state.entries.len(), TokenEstimateCache::CAPACITY);
        assert!(state.entries.contains_key("ses-0"));
        assert!(state.entries.contains_key("ses-overflow"));
        assert!(!state.entries.contains_key("ses-1"));
    }

    #[test]
    fn token_estimate_cache_detects_same_length_content_changes() {
        let cache = TokenEstimateCache::new();
        let mut messages = vec![CanonicalMessage::user_text("short")];
        assert_eq!(
            cache.estimate("ses-a", &messages, 1, 0),
            estimate_message_tokens(&messages)
        );

        messages[0] = CanonicalMessage::user_text(
            "a substantially longer replacement message with different content",
        );
        assert_eq!(
            cache.estimate("ses-a", &messages, 1, 1),
            estimate_message_tokens(&messages)
        );
    }

    #[test]
    fn token_estimate_cache_reuses_valid_prefix_for_appends() {
        let cache = TokenEstimateCache::new();
        let mut messages = vec![CanonicalMessage::user_text("first")];
        let first = cache.estimate("ses-a", &messages, 1, 0);

        messages.push(CanonicalMessage::user_text("second"));
        cache.append_message("ses-a", &messages[1], messages.len(), 1, 1);
        let appended = cache.estimate("ses-a", &messages, 1, 1);

        assert_eq!(first, estimate_message_tokens(&messages[..1]));
        assert_eq!(appended, estimate_message_tokens(&messages));
    }

    #[test]
    fn token_estimate_cache_rebuilds_after_non_append_revision() {
        let cache = TokenEstimateCache::new();
        let messages = vec![CanonicalMessage::user_text("first")];
        let first = cache.estimate("ses-a", &messages, 1, 0);

        let replacement = vec![CanonicalMessage::user_text(
            "a replacement with a different token cost",
        )];
        cache.append_message("ses-a", &replacement[0], replacement.len(), 1, 1);
        assert_eq!(
            cache.estimate("ses-a", &replacement, 1, 1),
            estimate_message_tokens(&replacement),
            "a mismatched append notification must not reuse stale tokens"
        );
        assert_ne!(first, estimate_message_tokens(&replacement));
    }

    #[test]
    fn token_estimate_cache_does_not_reuse_revision_zero_across_state_generations() {
        let cache = TokenEstimateCache::new();
        let first = vec![CanonicalMessage::user_text("short")];
        let second = vec![CanonicalMessage::user_text(
            "a substantially longer replacement with a different token cost",
        )];

        let first_tokens = cache.estimate("ses-a", &first, 10, 0);
        let second_tokens = cache.estimate("ses-a", &second, 11, 0);

        assert_eq!(first_tokens, estimate_message_tokens(&first));
        assert_eq!(second_tokens, estimate_message_tokens(&second));
        assert_ne!(first_tokens, second_tokens);
    }
}
