//! Independently constructible ReActEngine sidecars (Phase 8 / A2).
//!
//! Each type owns one mutex-backed concern previously inlined on
//! `ReActEngine`. The engine remains a facade that holds collaboration
//! deps plus these named sidecars (+ `IdentityMap`, `SnapshotStore`, hooks).

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;

use haven_common::types::CanonicalMessage;
use haven_llm::{EndpointRole, ToolDefinition};
use haven_tools::inbox::InboxBus;
use tokio::sync::watch;

use crate::compactor::estimate_message_tokens;

/// State for the automatic cross-session inbox check, one per engine
/// (shared across sessions — each session's mailbox is keyed by its own id).
pub(super) struct MessagingState {
    pub(super) bus: InboxBus,
    pub(super) rx: watch::Receiver<u64>,
    pub(super) steps_since_poll: u32,
    pub(super) title_cache: HashMap<String, Option<String>>,
}

impl MessagingState {
    pub(super) fn new() -> Self {
        let bus = InboxBus::default_root();
        let rx = bus.subscribe();
        Self {
            bus,
            rx,
            steps_since_poll: 0,
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
        self.inner.lock().unwrap().title_cache.remove(session_id);
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

/// Per-session tool-definition cache keyed by ToolsManager catalog version.
/// Values are `Arc` so cache hits share one schema vec across steps.
pub(crate) struct ToolDefCache {
    cache: Mutex<HashMap<String, (u64, Arc<Vec<ToolDefinition>>)>>,
}

impl ToolDefCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn get_if_version(
        &self,
        session_id: &str,
        version: u64,
    ) -> Option<Arc<Vec<ToolDefinition>>> {
        self.cache
            .lock()
            .unwrap()
            .get(session_id)
            .filter(|(v, _)| *v == version)
            .map(|(_, defs)| Arc::clone(defs))
    }

    pub(super) fn insert(&self, session_id: &str, version: u64, defs: Arc<Vec<ToolDefinition>>) {
        self.cache
            .lock()
            .unwrap()
            .insert(session_id.to_string(), (version, defs));
    }

    pub(crate) fn remove(&self, session_id: &str) {
        self.cache.lock().unwrap().remove(session_id);
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
    passes: u32,
}

/// Per-session incremental token-estimate cache.
pub(crate) struct TokenEstimateCache {
    cache: Mutex<HashMap<String, TokenEstimate>>,
}

impl TokenEstimateCache {
    pub(crate) fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn estimate(&self, session_id: &str, canonical: &[CanonicalMessage]) -> u32 {
        const FULL_ESTIMATE_PASS_INTERVAL: u32 = 8;
        // Snapshot decision under the lock; run tiktoken outside so concurrent
        // sessions are not serialized behind one mutex for the whole estimate.
        let (full_pass, msgs_len, tokens, passes) = {
            let cache = self.cache.lock().unwrap();
            match cache.get(session_id) {
                Some(entry) => {
                    let full_pass = entry.tokens == 0
                        || entry.msgs_len > canonical.len()
                        || entry.passes.is_multiple_of(FULL_ESTIMATE_PASS_INTERVAL);
                    (full_pass, entry.msgs_len, entry.tokens, entry.passes)
                }
                None => (true, 0, 0, 0),
            }
        };
        let (new_msgs_len, new_tokens) = if full_pass {
            (canonical.len(), estimate_message_tokens(canonical))
        } else if msgs_len < canonical.len() {
            (
                canonical.len(),
                tokens.saturating_add(estimate_message_tokens(&canonical[msgs_len..])),
            )
        } else {
            (msgs_len, tokens)
        };
        let new_passes = passes.saturating_add(1);
        let mut cache = self.cache.lock().unwrap();
        cache.insert(
            session_id.to_string(),
            TokenEstimate {
                msgs_len: new_msgs_len,
                tokens: new_tokens,
                passes: new_passes,
            },
        );
        new_tokens
    }

    pub(crate) fn remove(&self, session_id: &str) {
        self.cache.lock().unwrap().remove(session_id);
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

/// Per-session dedup for balanced-model-activated notifications.
pub(crate) struct BalancedModelNotifier {
    notified: Mutex<HashSet<String>>,
}

impl BalancedModelNotifier {
    pub(crate) fn new() -> Self {
        Self {
            notified: Mutex::new(HashSet::new()),
        }
    }

    /// Mark `session_id` as notified. Returns `true` when this is the first
    /// mark (caller should emit).
    pub(super) fn try_mark(&self, session_id: &str) -> bool {
        self.notified.lock().unwrap().insert(session_id.to_string())
    }

    pub(super) fn clear(&self, session_id: &str) {
        self.notified.lock().unwrap().remove(session_id);
    }
}

impl Default for BalancedModelNotifier {
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
}
