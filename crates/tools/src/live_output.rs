//! Live tool-output preview for foreground tools (esp. shell).
//!
//! Mirrors the background `tool_run:output` channel: while a tool runs, a
//! bounded stdout/stderr tail is pushed periodically as `agent:tool_output`
//! so the chat tool card can expand and show progress. Final observation
//! remains the LLM/canonical authority; these events are UI-only.

use crate::tool_run_lifecycle::{LiveOutputEventSink, LiveOutputEventSinkState};
use crate::tool_run_output::{
    ToolRunOutputPort, ToolRunOutputTail, ToolRunTailFactory, ToolRunTailSnapshot,
};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Shared hub that foreground tools (currently shell) use to push live
/// output previews to the UI. Wired once by the desktop shell via
/// [`LiveOutputHub::set_event_sink`].
pub struct LiveOutputHub {
    event_sink: LiveOutputEventSinkState,
    /// Shared policy owned by ToolRunService; this hub only emits the
    /// foreground tool-card projection.
    tail_factory: ToolRunTailFactory,
    /// Cadence of `agent:tool_output` events while a tool produces output.
    /// Slightly snappier than background ToolRuns because the user is watching
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
        Self::with_tail_factory(ToolRunOutputPort::new().tail_factory())
    }

    pub(crate) fn with_tail_factory(tail_factory: ToolRunTailFactory) -> Self {
        Self {
            event_sink: LiveOutputEventSinkState::default(),
            tail_factory,
            emit_interval: RwLock::new(Duration::from_millis(500)),
        }
    }

    pub fn set_event_sink(&self, sink: LiveOutputEventSink) {
        self.event_sink.set(sink);
    }

    pub(crate) async fn set_emit_interval(
        &self,
        limits: &haven_common::config::ContextLimitsConfig,
    ) {
        // Foreground cards use a bounded, faster cadence than background
        // ToolRuns. The setting remains the source of truth, but a large
        // background interval must not make an active card look frozen.
        let bg_ms = limits.background_tool_run_output_emit_interval_ms.max(100);
        *self.emit_interval.write().await = Duration::from_millis((bg_ms / 4).clamp(100, 250));
    }

    pub(crate) async fn new_tail(&self) -> ToolRunOutputTail {
        self.tail_factory.new_tail().await
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
    pub(crate) fn spawn_tail_emitter(
        self: &Arc<Self>,
        session_id: String,
        step_id: String,
        tail: ToolRunOutputTail,
        running: Arc<std::sync::atomic::AtomicBool>,
        emit_interval: Duration,
    ) {
        if step_id.is_empty() {
            return;
        }
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            // Compare by value: a capped sliding window can change content
            // without changing length (same freeze as background ToolRuns).
            let mut last_output = ToolRunTailSnapshot::default();
            loop {
                if !running.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                if tail.snapshot_if_changed(&mut last_output) {
                    hub.emit_output(&session_id, &step_id, last_output.as_str());
                }
                tokio::time::sleep(emit_interval).await;
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
            assert_eq!(
                payload,
                json!({
                    "session_id": "ses-1",
                    "step_id": "step-1",
                    "output": "hello",
                })
            );
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
        let running = Arc::new(AtomicBool::new(true));
        let tail = hub.new_tail().await;
        hub.spawn_tail_emitter(
            "ses-1".into(),
            "step-live".into(),
            tail.clone(),
            running.clone(),
            Duration::from_millis(30),
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
        tail.append_text("line1\n");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            hits.load(Ordering::SeqCst) >= 1,
            "expected at least one emit after tail growth"
        );
        running.store(false, Ordering::SeqCst);
        let before = hits.load(Ordering::SeqCst);
        tail.append_text("line2\n");
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            before,
            "emitter must stop once running is cleared"
        );
    }

    #[tokio::test]
    async fn spawn_tail_emitter_paints_prefilled_tail_without_interval_delay() {
        let hub = Arc::new(LiveOutputHub::new());
        let hits = Arc::new(AtomicUsize::new(0));
        let hits2 = hits.clone();
        hub.set_event_sink(Arc::new(move |event, payload| {
            assert_eq!(event, "agent:tool_output");
            assert_eq!(payload["output"], "already available");
            hits2.fetch_add(1, Ordering::SeqCst);
        }));
        let tail = hub.new_tail().await;
        tail.append_text("already available");
        let running = Arc::new(AtomicBool::new(true));
        hub.spawn_tail_emitter(
            "ses-1".into(),
            "step-immediate".into(),
            tail,
            running.clone(),
            Duration::from_secs(1),
        );

        for _ in 0..20 {
            if hits.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        running.store(false, Ordering::SeqCst);
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn foreground_and_background_tails_share_tool_run_service_policy() {
        let tool_runs = crate::ToolRunService::new();
        let limits = haven_common::config::ContextLimitsConfig {
            background_tool_run_tail_max_chars: 3,
            ..Default::default()
        };
        tool_runs.set_limits(&limits).await;
        let tail_factory = tool_runs.output_tail_factory();
        let hub = LiveOutputHub::with_tail_factory(tail_factory.clone());

        let foreground_tail = hub.new_tail().await;
        foreground_tail.append_text("secret");
        let background_tail = tail_factory.new_tail().await;
        background_tail.append_text("secret");

        assert_eq!(foreground_tail.snapshot().as_str(), "ret");
        assert_eq!(background_tail.snapshot().as_str(), "ret");
    }

    #[tokio::test]
    async fn spawn_tail_emitter_pushes_when_content_slides_at_same_len() {
        let tool_runs = crate::ToolRunService::new();
        let limits = haven_common::config::ContextLimitsConfig {
            background_tool_run_tail_max_chars: 64,
            ..Default::default()
        };
        tool_runs.set_limits(&limits).await;
        let hub = Arc::new(LiveOutputHub::with_tail_factory(
            tool_runs.output_tail_factory(),
        ));
        let hits = Arc::new(AtomicUsize::new(0));
        let last = Arc::new(Mutex::new(String::new()));
        let hits2 = hits.clone();
        let last2 = last.clone();
        hub.set_event_sink(Arc::new(move |event, payload| {
            assert_eq!(event, "agent:tool_output");
            *last2.lock().unwrap() = payload["output"].as_str().unwrap().into();
            hits2.fetch_add(1, Ordering::SeqCst);
        }));
        let tail = hub.new_tail().await;
        let running = Arc::new(AtomicBool::new(true));
        hub.spawn_tail_emitter(
            "ses-1".into(),
            "step-slide".into(),
            tail.clone(),
            running.clone(),
            Duration::from_millis(30),
        );
        tail.append_text(&"a".repeat(64));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(hits.load(Ordering::SeqCst) >= 1);
        let after_first = hits.load(Ordering::SeqCst);
        // Same length, different bytes — old len-only emitters would freeze here.
        tail.append_text(&"b".repeat(64));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            hits.load(Ordering::SeqCst) > after_first,
            "capped-window slide must still emit"
        );
        assert_eq!(last.lock().unwrap().as_str(), "b".repeat(64));
        running.store(false, Ordering::SeqCst);
    }
}
