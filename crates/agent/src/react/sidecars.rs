//! Independently constructible ReActEngine sidecars (Phase 8 / A2).
//!
//! Each type owns one mutex-backed concern previously inlined on
//! `ReActEngine`. The engine remains a facade that holds collaboration
//! deps plus these named sidecars (`CheckpointStore`, hooks, and process-wide
//! collaborators). Run-local stream identity is owned by `ReActState`.

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use haven_common::config::RequestKind;
use haven_llm::ToolDefinition;
use haven_messaging::MessagingService;

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

/// Provider definitions and their serialized schema cost, both derived from
/// one immutable catalog version.
#[derive(Clone)]
pub(crate) struct PreparedToolDefinitions {
    pub(crate) definitions: Arc<Vec<ToolDefinition>>,
    pub(crate) token_estimate: u32,
}

/// Per-session tool-definition cache keyed by the global catalog and the
/// session-local registration overlay version. Values share one schema vec
/// across steps, and the schema token estimate is computed only on a miss.
/// It is bounded so short-lived sessions cannot grow this sidecar indefinitely.
type ToolDefCacheEntry = ((u64, u64), PreparedToolDefinitions);
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
    ) -> Option<PreparedToolDefinitions> {
        let mut state = self.cache.lock().unwrap();
        let result = state
            .entries
            .get(session_id)
            .filter(|(v, _)| *v == version)
            .map(|(_, prepared)| prepared.clone());
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
        prepared: PreparedToolDefinitions,
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
            .insert(session_id.to_string(), (version, prepared));
        state.order.push_back(session_id.to_string());
    }
}

impl Default for ToolDefCache {
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
        let prepared = || PreparedToolDefinitions {
            definitions: Arc::new(Vec::new()),
            token_estimate: 7,
        };
        for index in 0..ToolDefCache::CAPACITY {
            cache.insert(&format!("ses-{index}"), (0, 0), prepared());
        }
        assert_eq!(
            cache
                .get_if_version("ses-0", (0, 0))
                .unwrap()
                .token_estimate,
            7
        );

        cache.insert("ses-overflow", (0, 0), prepared());

        assert!(cache.get_if_version("ses-0", (0, 0)).is_some());
        assert!(cache.get_if_version("ses-overflow", (0, 0)).is_some());
        assert!(cache.get_if_version("ses-1", (0, 0)).is_none());
    }

    #[test]
    fn tool_definition_cache_misses_after_catalog_version_changes() {
        let cache = ToolDefCache::new();
        cache.insert(
            "ses-a",
            (4, 9),
            PreparedToolDefinitions {
                definitions: Arc::new(Vec::new()),
                token_estimate: 17,
            },
        );

        assert!(cache.get_if_version("ses-a", (4, 9)).is_some());
        assert!(cache.get_if_version("ses-a", (5, 9)).is_none());
        assert!(cache.get_if_version("ses-a", (4, 10)).is_none());
    }
}
