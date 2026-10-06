use crate::ToolRunLifecycleEvent;
use serde_json::Value;
use std::sync::{Arc, Mutex, MutexGuard};

/// Typed sink for ToolRun lifecycle events.
pub type EventSink = Arc<dyn Fn(ToolRunLifecycleEvent) + Send + Sync>;

/// The foreground tool-output channel is separate from ToolRun lifecycle
/// events and retains its existing internal JSON envelope.
pub(crate) type LiveOutputEventSink = Arc<dyn Fn(String, Value) + Send + Sync>;

fn lock_or_recover<'a, T>(lock: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            lock = name,
            "ToolRun lifecycle lock poisoned; recovering state"
        );
        poisoned.into_inner()
    })
}

/// Common lifecycle infrastructure for long-running tool_runs. It owns only
/// cross-cutting event delivery; each ToolRun family remains responsible for
/// its own state, cancellation mechanics, and durable representation.
#[derive(Default)]
pub(crate) struct ToolRunLifecycle {
    sink: Mutex<Option<EventSink>>,
}

impl ToolRunLifecycle {
    pub(crate) fn set_event_sink(&self, sink: EventSink) {
        *lock_or_recover(&self.sink, "tool_run_event_sink") = Some(sink);
    }

    pub(crate) fn emit(&self, event: ToolRunLifecycleEvent) {
        if let Some(sink) = lock_or_recover(&self.sink, "tool_run_event_sink").as_ref() {
            sink(event);
        }
    }
}

/// Separate sink state for foreground tool output, which is not a ToolRun
/// lifecycle event.
#[derive(Default)]
pub(crate) struct LiveOutputEventSinkState {
    sink: Mutex<Option<LiveOutputEventSink>>,
}

impl LiveOutputEventSinkState {
    pub(crate) fn set(&self, sink: LiveOutputEventSink) {
        *lock_or_recover(&self.sink, "live_output_event_sink") = Some(sink);
    }

    pub(crate) fn emit(&self, event: &str, payload: Value) {
        if let Some(sink) = lock_or_recover(&self.sink, "live_output_event_sink").as_ref() {
            sink(event.to_string(), payload);
        }
    }
}
