//! Independently constructible ReActEngine sidecars (Phase 8 / A2).
//!
//! Each type owns one mutex-backed concern previously inlined on
//! `ReActEngine`. The engine remains a facade that holds collaboration
//! deps plus these named sidecars (+ `IdentityMap`, `SnapshotStore`, hooks).

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

use haven_common::types::CanonicalMessage;
use haven_llm::{EndpointRole, ToolDefinition};
use haven_tools::MessagingService;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use crate::compactor::estimate_message_tokens;

/// State for the automatic cross-session inbox check, one per engine.
/// Notification cursors and fallback cadence are per session: the process-wide
/// inbox notifier is a shared wake-up signal, but consuming session A's signal
/// must never postpone session B's delivery.
pub(super) struct MessagingState {
    pub(super) service: Arc<MessagingService>,
    pub(super) receivers: HashMap<String, watch::Receiver<u64>>,
    pub(super) steps_since_poll: HashMap<String, u32>,
    pub(super) title_cache: HashMap<String, Option<String>>,
}

impl MessagingState {
    pub(super) fn new() -> Self {
        let service = Arc::new(MessagingService::default_root());
        Self {
            service,
            receivers: HashMap::new(),
            steps_since_poll: HashMap::new(),
            title_cache: HashMap::new(),
        }
    }
}

/// Sidecar wrapping [`MessagingState`] for cross-session inbox polling.
pub(crate) struct MessagingPoller {
    inner: Mutex<MessagingState>,
    /// Sessions with a heartbeat `spawn_blocking` already queued/running —
    /// coalesce so steps cannot unboundedly fill the blocking pool.
    heartbeat_inflight: Arc<Mutex<HashSet<String>>>,
}

impl MessagingPoller {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(MessagingState::new()),
            heartbeat_inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, MessagingState> {
        self.inner.lock().unwrap()
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

    /// Drop per-session title cache so finished sessions do not accumulate.
    pub(crate) fn clear_session(&self, session_id: &str) {
        let mut state = self.inner.lock().unwrap();
        state.title_cache.remove(session_id);
        state.receivers.remove(session_id);
        state.steps_since_poll.remove(session_id);
        self.heartbeat_inflight.lock().unwrap().remove(session_id);
    }
}

impl Default for MessagingPoller {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct CumulativeUsage {
    pub(super) prompt_tokens: u32,
    pub(super) completion_tokens: u32,
    pub(super) total_tokens: u32,
    pub(super) cached_tokens: u32,
    pub(super) cache_creation_tokens: u32,
    pub(super) cache_miss_tokens: u32,
    pub(super) cost_usd: f64,
    pub(super) has_cost: bool,
}

impl From<haven_memory::repositories::usage::SessionUsage> for CumulativeUsage {
    fn from(u: haven_memory::repositories::usage::SessionUsage) -> Self {
        Self {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cached_tokens: u.cached_tokens,
            cache_creation_tokens: u.cache_creation_tokens,
            cache_miss_tokens: u.cache_miss_tokens,
            cost_usd: u.cost_usd,
            has_cost: u.has_cost,
        }
    }
}

/// Cumulative totals after a usage accumulate pass.
#[derive(Debug, Clone, Copy)]
pub(super) struct CumulativeTotals {
    pub(super) prompt_tokens: u32,
    pub(super) completion_tokens: u32,
    pub(super) total_tokens: u32,
    pub(super) cached_tokens: u32,
    pub(super) cache_creation_tokens: u32,
    pub(super) cache_miss_tokens: u32,
    pub(super) cost_usd: Option<f64>,
}

/// Per-session cumulative token usage tracker.
pub(crate) struct UsageTracker {
    map: Mutex<HashMap<String, CumulativeUsage>>,
    /// Persist-epoch per session. Bumped on rollback/truncate so a detached
    /// fire-and-forget write from a discarded call cannot re-insert usage
    /// after the detail rows were cut.
    epochs: Arc<Mutex<HashMap<String, u64>>>,
}

impl UsageTracker {
    pub(crate) fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            epochs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Current persist epoch for `session_id` (0 when never invalidated).
    pub(crate) fn epoch(&self, session_id: &str) -> u64 {
        self.epochs
            .lock()
            .unwrap()
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }

    /// Shared epoch map so a detached persist task can observe invalidation.
    pub(crate) fn epochs_handle(&self) -> Arc<Mutex<HashMap<String, u64>>> {
        Arc::clone(&self.epochs)
    }

    /// Seed (if missing) then add one call's tokens/cost; returns running totals.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_with_seed<F>(
        &self,
        session_id: &str,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
        cache_creation_tokens: u32,
        cache_miss_tokens: u32,
        step_cost: Option<f64>,
        seed: F,
    ) -> CumulativeTotals
    where
        F: FnOnce() -> CumulativeUsage,
    {
        let mut map = self.map.lock().unwrap();
        let entry = map.entry(session_id.to_string()).or_insert_with(seed);
        entry.prompt_tokens = entry.prompt_tokens.saturating_add(prompt_tokens);
        entry.completion_tokens = entry.completion_tokens.saturating_add(completion_tokens);
        entry.total_tokens = entry.total_tokens.saturating_add(total_tokens);
        entry.cached_tokens = entry.cached_tokens.saturating_add(cached_tokens);
        entry.cache_creation_tokens = entry
            .cache_creation_tokens
            .saturating_add(cache_creation_tokens);
        entry.cache_miss_tokens = entry.cache_miss_tokens.saturating_add(cache_miss_tokens);
        if let Some(c) = step_cost {
            entry.cost_usd += c;
            entry.has_cost = true;
        }
        let cost_usd = if entry.has_cost {
            Some(entry.cost_usd)
        } else {
            None
        };
        CumulativeTotals {
            prompt_tokens: entry.prompt_tokens,
            completion_tokens: entry.completion_tokens,
            total_tokens: entry.total_tokens,
            cached_tokens: entry.cached_tokens,
            cache_creation_tokens: entry.cache_creation_tokens,
            cache_miss_tokens: entry.cache_miss_tokens,
            cost_usd,
        }
    }

    pub(crate) fn reset(&self, session_id: &str) {
        self.map.lock().unwrap().remove(session_id);
    }

    /// Clear in-memory counters and bump the persist epoch so in-flight
    /// writes captured before a truncate are ignored.
    pub(crate) fn invalidate_after_truncate(&self, session_id: &str) {
        self.map.lock().unwrap().remove(session_id);
        let mut epochs = self.epochs.lock().unwrap();
        let next = epochs
            .get(session_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        epochs.insert(session_id.to_string(), next);
    }
}

impl Default for UsageTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-session tool-definition cache keyed by the global catalog and the
/// session-local registration overlay version.
/// Values are `Arc` so cache hits share one schema vec across steps. It is
/// bounded because ended sessions are normally removed eagerly, but a burst
/// of short-lived sessions must not grow this sidecar without limit.
type ToolDefCacheEntry = ((u64, u64), Arc<Vec<ToolDefinition>>);
type ToolDefCacheMap = HashMap<String, ToolDefCacheEntry>;

struct ToolDefCacheState {
    entries: ToolDefCacheMap,
    order: VecDeque<String>,
}

pub(crate) struct ToolDefCache {
    cache: Mutex<ToolDefCacheState>,
}

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

/// Cached `messages.created_at` of the newest row per session, used by
/// [`super::snapshot_io::ReActEngine::save_branch_point`] so mid-run branch
/// points do not hit SQLite on every step when the snapshot write is throttled.
pub(crate) struct LastMsgAtCache {
    map: Mutex<HashMap<String, Option<String>>>,
}

impl LastMsgAtCache {
    pub(crate) fn new() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn get(&self, session_id: &str) -> Option<Option<String>> {
        self.map.lock().unwrap().get(session_id).cloned()
    }

    pub(super) fn set(&self, session_id: &str, last_msg_at: Option<String>) {
        self.map
            .lock()
            .unwrap()
            .insert(session_id.to_string(), last_msg_at);
    }

    pub(crate) fn remove(&self, session_id: &str) {
        self.map.lock().unwrap().remove(session_id);
    }
}

impl Default for LastMsgAtCache {
    fn default() -> Self {
        Self::new()
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
    /// Fingerprint of the exact canonical prefix represented by `tokens`.
    /// Length alone is not a valid cache key: rollback, repair, or a caller
    /// can replace a message without changing the vector length.
    fingerprint: [u8; 32],
}

struct FingerprintWriter(Sha256);

impl io::Write for FingerprintWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn canonical_fingerprint(messages: &[CanonicalMessage]) -> [u8; 32] {
    // Stream JSON directly into the digest instead of allocating one large
    // Vec containing every message and every base64 attachment. A separator
    // keeps [a, bc] distinct from [ab, c] without retaining serialized data.
    let mut writer = FingerprintWriter(Sha256::new());
    for message in messages {
        if serde_json::to_writer(&mut writer, message).is_err() {
            // Canonical messages currently serialize infallibly. If a future
            // content part does not, the debug form remains deterministic and
            // still cannot create a false cache hit for valid JSON.
            writer.0.update(format!("{message:?}").as_bytes());
        }
        writer.0.update([0]);
    }
    writer.0.finalize().into()
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

    pub(super) fn estimate(&self, session_id: &str, canonical: &[CanonicalMessage]) -> u32 {
        let fingerprint = canonical_fingerprint(canonical);
        // Snapshot the prior entry under the lock; run tiktoken outside so
        // concurrent sessions are not serialized behind one mutex for the
        // whole estimate.
        let prior = {
            let state = self.cache.lock().unwrap();
            state.entries.get(session_id).cloned()
        };
        if let Some(entry) = &prior
            && entry.fingerprint == fingerprint
        {
            let mut state = self.cache.lock().unwrap();
            if state.entries.contains_key(session_id) {
                state.order.retain(|cached| cached != session_id);
                state.order.push_back(session_id.to_string());
            }
            return entry.tokens;
        }

        let (new_msgs_len, new_tokens) = if let Some(entry) = prior
            && entry.msgs_len < canonical.len()
            && entry.fingerprint == canonical_fingerprint(&canonical[..entry.msgs_len])
        {
            (
                canonical.len(),
                entry
                    .tokens
                    .saturating_add(estimate_message_tokens(&canonical[entry.msgs_len..])),
            )
        } else {
            (canonical.len(), estimate_message_tokens(canonical))
        };
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
                fingerprint,
            },
        );
        state.order.push_back(session_key);
        new_tokens
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

/// Reusable per-session snapshot serialization buffers.
/// Mid-run write throttle lives on [`super::snapshot_io::SnapshotStore`].
pub(crate) struct SnapshotBufs {
    bufs: Mutex<HashMap<String, Vec<u8>>>,
}

impl SnapshotBufs {
    pub(crate) fn new() -> Self {
        Self {
            bufs: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, HashMap<String, Vec<u8>>> {
        self.bufs.lock().unwrap()
    }

    pub(super) fn try_lock(&self) -> Result<MutexGuard<'_, HashMap<String, Vec<u8>>>, ()> {
        self.bufs.try_lock().map_err(|_| ())
    }

    pub(crate) fn remove(&self, session_id: &str) {
        self.bufs.lock().unwrap().remove(session_id);
    }
}

impl Default for SnapshotBufs {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-role context-window cache keyed by the router instance pointer.
pub(crate) struct ContextWindowCache {
    cache: Mutex<(usize, HashMap<EndpointRole, u32>)>,
}

impl ContextWindowCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: Mutex::new((0, HashMap::new())),
        }
    }

    pub(super) fn get(&self, router_ptr: usize, role: EndpointRole) -> Option<u32> {
        let cache = self.cache.lock().unwrap();
        if cache.0 == router_ptr {
            cache.1.get(&role).copied()
        } else {
            None
        }
    }

    pub(super) fn insert(&self, router_ptr: usize, role: EndpointRole, window: u32) {
        let mut cache = self.cache.lock().unwrap();
        if cache.0 != router_ptr {
            cache.0 = router_ptr;
            cache.1.clear();
        }
        cache.1.insert(role, window);
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
    fn snapshot_bufs_try_lock_is_non_blocking() {
        let bufs = SnapshotBufs::new();
        let _guard = bufs.lock();
        assert!(
            bufs.try_lock().is_err(),
            "try_lock must fail while another guard holds the mutex"
        );
    }

    #[test]
    fn messaging_clear_session_drops_title_cache() {
        let poller = MessagingPoller::new();
        {
            let mut st = poller.lock();
            st.title_cache.insert("ses-a".into(), Some("hello".into()));
        }
        poller.clear_session("ses-a");
        assert!(!poller.lock().title_cache.contains_key("ses-a"));
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
            cache.estimate(&format!("ses-{index}"), &[]);
        }
        cache.estimate("ses-0", &[]);

        cache.estimate("ses-overflow", &[]);

        let state = cache.cache.lock().unwrap();
        assert_eq!(state.entries.len(), TokenEstimateCache::CAPACITY);
        assert!(state.entries.contains_key("ses-0"));
        assert!(state.entries.contains_key("ses-overflow"));
        assert!(!state.entries.contains_key("ses-1"));
    }

    #[test]
    fn usage_tracker_invalidate_bumps_epoch_and_clears_map() {
        let tracker = UsageTracker::new();
        assert_eq!(tracker.epoch("ses-a"), 0);
        let _ =
            tracker.record_with_seed("ses-a", 10, 5, 15, 0, 0, 10, None, CumulativeUsage::default);
        tracker.invalidate_after_truncate("ses-a");
        assert_eq!(tracker.epoch("ses-a"), 1);
        // Next record must re-seed (map was cleared), not keep the old 15.
        let totals =
            tracker.record_with_seed("ses-a", 1, 1, 2, 0, 0, 1, None, CumulativeUsage::default);
        assert_eq!(totals.total_tokens, 2);
    }

    #[test]
    fn token_estimate_cache_detects_same_length_content_changes() {
        let cache = TokenEstimateCache::new();
        let mut messages = vec![CanonicalMessage::user_text("short")];
        assert_eq!(
            cache.estimate("ses-a", &messages),
            estimate_message_tokens(&messages)
        );

        messages[0] = CanonicalMessage::user_text(
            "a substantially longer replacement message with different content",
        );
        assert_eq!(
            cache.estimate("ses-a", &messages),
            estimate_message_tokens(&messages)
        );
    }

    #[test]
    fn token_estimate_cache_reuses_valid_prefix_for_appends() {
        let cache = TokenEstimateCache::new();
        let mut messages = vec![CanonicalMessage::user_text("first")];
        let first = cache.estimate("ses-a", &messages);

        messages.push(CanonicalMessage::user_text("second"));
        let appended = cache.estimate("ses-a", &messages);

        assert_eq!(first, estimate_message_tokens(&messages[..1]));
        assert_eq!(appended, estimate_message_tokens(&messages));
    }
}
