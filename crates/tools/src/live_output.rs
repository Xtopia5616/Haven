//! Live tool-output preview for foreground tools (esp. shell).
//!
//! Mirrors the background `action:output` channel: while a tool runs, a
//! bounded stdout/stderr tail is pushed periodically as `agent:tool_output`
//! so the chat tool card can expand and show progress. Final observation
//! remains the LLM/canonical authority; these events are UI-only.

use crate::bg::EventSink;
use crate::bg::EventSinkState;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;

/// Shared hub that foreground tools (currently shell) use to push live
/// output previews to the UI. Wired once by the desktop shell via
/// [`LiveOutputHub::set_event_sink`].
pub struct LiveOutputHub {
    event_sink: EventSinkState,
    /// Bounded live-output tail (chars). Defaults match background actions.
    tail_max_chars: RwLock<usize>,
    /// Cadence of `agent:tool_output` events while a tool produces output.
    /// Slightly snappier than background actions because the user is watching
    /// the active tool card.
    emit_interval: RwLock<Duration>,
}

impl Default for LiveOutputHub {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveOutputHub {
    pub fn new() -> Self {
        Self {
            event_sink: EventSinkState::default(),
            tail_max_chars: RwLock::new(2000),
            emit_interval: RwLock::new(Duration::from_millis(500)),
        }
    }

    pub fn set_event_sink(&self, sink: EventSink) {
        self.event_sink.set(sink);
    }

    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.tail_max_chars.write().await = limits.background_job_tail_max_chars;
        // Foreground cards use half the background interval (min 250ms) so
        // the active tool feels live without flooding Tauri IPC.
        let bg_ms = limits.background_job_output_emit_interval_ms.max(250);
        *self.emit_interval.write().await = Duration::from_millis((bg_ms / 2).max(250));
    }

    pub async fn tail_max_chars(&self) -> usize {
        *self.tail_max_chars.read().await
    }

    pub async fn emit_interval(&self) -> Duration {
        *self.emit_interval.read().await
    }

    /// Push a live-output snapshot for the tool card keyed by `step_id`.
    pub fn emit_output(&self, session_id: &str, step_id: &str, output: &str) {
        if step_id.is_empty() {
            return;
        }
        self.event_sink.emit(
            "agent:tool_output",
            json!({
                "session_id": session_id,
                "step_id": step_id,
                "output": output,
            }),
        );
    }

    /// Spawn a periodic emitter that pushes the shared `tail` while
    /// `running` stays true. Stops when `running` is cleared (tool finished
    /// or cancelled). No-op when `step_id` is empty.
    pub fn spawn_tail_emitter(
        self: &Arc<Self>,
        session_id: String,
        step_id: String,
        tail: Arc<Mutex<String>>,
        running: Arc<std::sync::atomic::AtomicBool>,
        emit_interval: Duration,
    ) {
        if step_id.is_empty() {
            return;
        }
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            // Compare by value: a capped sliding window can change content
            // without changing length (same freeze as background actions).
            let mut last_output = String::new();
            loop {
                tokio::time::sleep(emit_interval).await;
                if !running.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                if crate::bg::take_tail_if_changed(&tail, &mut last_output) {
                    hub.emit_output(&session_id, &step_id, &last_output);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[tokio::test]
    async fn emit_output_forwards_to_sink() {
        let hub = LiveOutputHub::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(String::new()));
        let hits2 = hits.clone();
        let last2 = last.clone();
        hub.set_event_sink(Arc::new(move |event, payload| {
            assert_eq!(event, "agent:tool_output");
            assert_eq!(payload["session_id"], "ses-1");
            assert_eq!(payload["step_id"], "step-1");
            *last2.lock().unwrap() = payload["output"].as_str().unwrap().into();
            hits2.fetch_add(1, Ordering::SeqCst);
        }));
        hub.emit_output("ses-1", "step-1", "hello");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(last.lock().unwrap().as_str(), "hello");
        // Empty step id is a no-op.
        hub.emit_output("ses-1", "", "ignored");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn spawn_tail_emitter_pushes_when_tail_grows() {
        let hub = Arc::new(LiveOutputHub::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        hub.set_event_sink(Arc::new(move |event, payload| {
            assert_eq!(event, "agent:tool_output");
            assert_eq!(payload["step_id"], "step-live");
            hits2.fetch_add(1, Ordering::SeqCst);
        }));
        let tail = Arc::new(Mutex::new(String::new()));
        let running = Arc::new(AtomicBool::new(true));
        hub.spawn_tail_emitter(
            "ses-1".into(),
            "step-live".into(),
            tail.clone(),
            running.clone(),
            Duration::from_millis(30),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        *tail.lock().unwrap() = "line1\n".into();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            hits.load(Ordering::SeqCst) >= 1,
            "expected at least one emit after tail growth"
        );
        running.store(false, Ordering::SeqCst);
        let before = hits.load(Ordering::SeqCst);
        *tail.lock().unwrap() = "line1\nline2\n".into();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            before,
            "emitter must stop once running is cleared"
        );
    }

    #[tokio::test]
    async fn spawn_tail_emitter_pushes_when_content_slides_at_same_len() {
        let hub = Arc::new(LiveOutputHub::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(String::new()));
        let hits2 = hits.clone();
        let last2 = last.clone();
        hub.set_event_sink(Arc::new(move |event, payload| {
            assert_eq!(event, "agent:tool_output");
            *last2.lock().unwrap() = payload["output"].as_str().unwrap().into();
            hits2.fetch_add(1, Ordering::SeqCst);
        }));
        let tail = Arc::new(Mutex::new(String::new()));
        let running = Arc::new(AtomicBool::new(true));
        hub.spawn_tail_emitter(
            "ses-1".into(),
            "step-slide".into(),
            tail.clone(),
            running.clone(),
            Duration::from_millis(30),
        );
        *tail.lock().unwrap() = "a".repeat(64);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(hits.load(Ordering::SeqCst) >= 1);
        let after_first = hits.load(Ordering::SeqCst);
        // Same length, different bytes — old len-only emitters would freeze here.
        *tail.lock().unwrap() = "b".repeat(64);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            hits.load(Ordering::SeqCst) > after_first,
            "capped-window slide must still emit"
        );
        assert_eq!(last.lock().unwrap().as_str(), "b".repeat(64));
        running.store(false, Ordering::SeqCst);
    }
}
