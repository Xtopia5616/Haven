use serde_json::Value;
use std::sync::{Arc, Mutex, MutexGuard};

/// Shared sink used by both kinds of long-running work. Background processes
/// and scheduled timers deliberately keep separate state machines and
/// persistence semantics, while lifecycle event delivery has one owner.
pub type EventSink = Arc<dyn Fn(String, Value) + Send + Sync>;

fn lock_or_recover<'a, T>(lock: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            lock = name,
            "action lifecycle lock poisoned; recovering state"
        );
        poisoned.into_inner()
    })
}

/// Common lifecycle infrastructure for long-running actions. It owns only
/// cross-cutting event delivery; each action family remains responsible for
/// its own state, cancellation mechanics, and durable representation.
#[derive(Default)]
pub(crate) struct ActionLifecycle {
    sink: Mutex<Option<EventSink>>,
}

impl ActionLifecycle {
    pub(crate) fn set(&self, sink: EventSink) {
        self.set_event_sink(sink);
    }

    pub(crate) fn set_event_sink(&self, sink: EventSink) {
        *lock_or_recover(&self.sink, "action_event_sink") = Some(sink);
    }

    pub(crate) fn emit(&self, event: &str, payload: Value) {
        if let Some(sink) = lock_or_recover(&self.sink, "action_event_sink").as_ref() {
            sink(event.to_string(), payload);
        }
    }
}

/// Compatibility-free lower-level view used by the two action registries when
/// they need to forward the same sink without sharing their business state.
pub(crate) type EventSinkState = ActionLifecycle;
