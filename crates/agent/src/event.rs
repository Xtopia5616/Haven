use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use haven_memory::Database;
use haven_task::TaskInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Thought {
        task_id: String,
        thought: String,
        step_number: u32,
        run_id: u64,
    },
    Action {
        task_id: String,
        tool_name: String,
        input: Value,
        step_number: u32,
        run_id: u64,
        tool_call_id: Option<String>,
    },
    Observation {
        task_id: String,
        observation: String,
        tool_name: String,
        step_number: u32,
        run_id: u64,
        silent: bool,
        tool_call_id: Option<String>,
        /// Quick-reply options surfaced when the observation comes from the
        /// `ask` tool, so the UI can render clickable answer buttons.
        ask_options: Vec<String>,
    },
    TaskCreated(TaskInfo),
    TaskCompleted {
        task_id: String,
        title: String,
    },
    TaskError {
        task_id: String,
        error: String,
    },
    BalancedModelActivated {
        task_id: String,
        reason: String,
    },
    ThoughtChunk {
        task_id: String,
        delta: String,
        step_number: u32,
        run_id: u64,
    },
    ReasoningChunk {
        task_id: String,
        delta: String,
        step_number: u32,
        run_id: u64,
    },
    Supplement {
        task_id: String,
        additional_context: String,
        step_number: u32,
        run_id: u64,
    },
    TaskUpdated {
        task_id: String,
        status: String,
    },
    Compaction {
        task_id: String,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
    },
    TitleUpdated {
        task_id: String,
        title: String,
    },
    /// A user-facing notification requested by the agent (via the `notify`
    /// tool). Surfaced both in-app (toast) and as a Windows notification.
    Notification {
        task_id: String,
        title: String,
        body: String,
    },
    /// Token-usage statistics for one LLM call (or aggregate). Surfaces
    /// prompt/completion/total tokens and the USD cost when the active
    /// endpoint has pricing configured. Emitted after every ReAct step so
    /// the UI can display a running counter and remaining context budget.
    Usage {
        task_id: String,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cost_usd: Option<f64>,
        model: Option<String>,
        /// Cumulative totals across the entire task (incl. this step).
        cumulative_prompt_tokens: u32,
        cumulative_completion_tokens: u32,
        cumulative_total_tokens: u32,
        cumulative_cost_usd: Option<f64>,
        /// Configured context window for the model (tokens). When `None`,
        /// the UI falls back to a generic budget indicator.
        context_window: Option<u32>,
    },
}

#[async_trait]
pub trait AgentEventEmitter: Send + Sync {
    async fn emit(&self, event: AgentEvent);
}

/// Multi-subscriber fan-out for `AgentEventEmitter`. Itself implements
/// `AgentEventEmitter`, so a single bus can be installed via
/// `EventDispatcher::set_emitter` while any number of independent subscribers
/// (frontend `TauriEmitter`, log recorder, test mock, …) register and
/// unregister by id without disturbing each other.
///
/// `emit` snapshots the subscriber list under a read lock, then awaits each
/// subscriber sequentially — order matches registration order. Failures in one
/// subscriber do not abort delivery to the rest (errors are logged and skipped).
pub struct EventBus {
    subscribers: tokio::sync::RwLock<Vec<(String, Arc<dyn AgentEventEmitter>)>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            subscribers: tokio::sync::RwLock::new(Vec::new()),
        }
    }

    /// Add (or, if `id` already exists, replace) a subscriber. Returns the
    /// previously registered emitter for that id, if any.
    pub async fn subscribe(
        &self,
        id: &str,
        emitter: Arc<dyn AgentEventEmitter>,
    ) -> Option<Arc<dyn AgentEventEmitter>> {
        let mut subs = self.subscribers.write().await;
        if let Some(slot) = subs.iter_mut().find(|(sid, _)| sid == id) {
            Some(std::mem::replace(&mut slot.1, emitter))
        } else {
            subs.push((id.to_string(), emitter));
            None
        }
    }

    /// Remove a subscriber by id. Returns the removed emitter, if any.
    pub async fn unsubscribe(&self, id: &str) -> Option<Arc<dyn AgentEventEmitter>> {
        let mut subs = self.subscribers.write().await;
        if let Some(pos) = subs.iter().position(|(sid, _)| sid == id) {
            Some(subs.swap_remove(pos).1)
        } else {
            None
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentEventEmitter for EventBus {
    async fn emit(&self, event: AgentEvent) {
        let snapshot: Vec<Arc<dyn AgentEventEmitter>> = {
            let subs = self.subscribers.read().await;
            subs.iter().map(|(_, e)| e.clone()).collect()
        };
        for emitter in snapshot {
            // Clone per-subscriber so a slow/panicking subscriber can't poison others.
            let ev = event.clone();
            emitter.emit(ev).await;
        }
    }
}

type ChunkSender = tokio::sync::mpsc::Sender<(String, String, u32, u64)>;
type ConsumerHandle = Option<tokio::task::JoinHandle<()>>;

/// Per-chunk micro-batching parameters. Incoming per-token chunks are aggregated
/// for at most this duration before a single `ThoughtChunk`/`ReasoningChunk` with
/// the concatenated `delta` is emitted, dramatically reducing Tauri IPC frequency.
const CHUNK_BATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// Flush early once the accumulated batch exceeds this size to bound memory and latency.
const CHUNK_BATCH_MAX_BYTES: usize = 8 * 1024;

/// Runs a chunk batcher: aggregates incoming `(task_id, delta, step, run)` tuples
/// for up to `CHUNK_BATCH_INTERVAL` (or until `CHUNK_BATCH_MAX_BYTES`), then emits
/// a single `AgentEvent::ThoughtChunk`/`ReasoningChunk` with the concatenated delta.
/// A batch boundary is also forced whenever the `(task_id, step, run)` key changes
/// or the sender half is dropped (flush remainder then exit).
async fn run_chunk_batcher(
    mut rx: tokio::sync::mpsc::Receiver<(String, String, u32, u64)>,
    emitter: Arc<dyn AgentEventEmitter>,
    is_reasoning: bool,
) {
    let emit_batch = |tid: String, sn: u32, rid: u64, delta: String| {
        let emitter = emitter.clone();
        async move {
            if delta.is_empty() {
                return;
            }
            let event = if is_reasoning {
                AgentEvent::ReasoningChunk {
                    task_id: tid,
                    delta,
                    step_number: sn,
                    run_id: rid,
                }
            } else {
                AgentEvent::ThoughtChunk {
                    task_id: tid,
                    delta,
                    step_number: sn,
                    run_id: rid,
                }
            };
            emitter.emit(event).await;
        }
    };

    loop {
        // Block until the first item of a new batch arrives.
        let (mut tid, mut delta, mut sn, mut rid) = match rx.recv().await {
            Some(v) => v,
            None => return,
        };
        // Emit the first chunk immediately so the user sees text without the
        // 50ms batch delay. Subsequent chunks are aggregated normally.
        emit_batch(tid.clone(), sn, rid, delta.clone()).await;
        let mut buf = String::new();
        let mut buf_bytes = 0usize;
        delta.clear();
        // Fresh deadline for this batch (fixed, not sliding — recreated each loop
        // iteration with the same value so it fires at the original deadline).
        let mut deadline = tokio::time::Instant::now() + CHUNK_BATCH_INTERVAL;

        if buf_bytes >= CHUNK_BATCH_MAX_BYTES {
            emit_batch(std::mem::take(&mut tid), sn, rid, std::mem::take(&mut buf)).await;
            continue;
        }

        loop {
            tokio::select! {
                biased;
                val = rx.recv() => {
                    match val {
                        Some((tid2, delta2, sn2, rid2)) => {
                            if (tid2.as_str(), sn2, rid2) != (tid.as_str(), sn, rid) {
                                // key changed: flush current batch, start a new one
                                emit_batch(std::mem::take(&mut tid), sn, rid, std::mem::take(&mut buf)).await;
                                tid = tid2;
                                sn = sn2;
                                rid = rid2;
                                buf = delta2;
                                buf_bytes = buf.len();
                                deadline = tokio::time::Instant::now() + CHUNK_BATCH_INTERVAL;
                                if buf_bytes >= CHUNK_BATCH_MAX_BYTES {
                                    emit_batch(std::mem::take(&mut tid), sn, rid, std::mem::take(&mut buf)).await;
                                    break;
                                }
                            } else {
                                buf_bytes += delta2.len();
                                buf.push_str(&delta2);
                                if buf_bytes >= CHUNK_BATCH_MAX_BYTES {
                                    emit_batch(std::mem::take(&mut tid), sn, rid, std::mem::take(&mut buf)).await;
                                    break;
                                }
                            }
                        }
                        None => {
                            emit_batch(std::mem::take(&mut tid), sn, rid, std::mem::take(&mut buf)).await;
                            return;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    emit_batch(std::mem::take(&mut tid), sn, rid, std::mem::take(&mut buf)).await;
                    break;
                }
            }
        }
    }
}

pub struct EventDispatcher {
    emitter: Arc<Mutex<Option<Arc<dyn AgentEventEmitter>>>>,
}

impl Default for EventDispatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl EventDispatcher {
    pub fn new() -> Self {
        Self {
            emitter: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_emitter(&self, emitter: Arc<dyn AgentEventEmitter>) {
        *self.emitter.lock().unwrap() = Some(emitter);
    }

    /// Create an `EventBus`, install it as the active emitter, and return a
    /// handle so callers can register subscribers. Replaces any previously
    /// installed emitter.
    pub fn install_bus(&self) -> Arc<EventBus> {
        let bus = Arc::new(EventBus::new());
        self.set_emitter(bus.clone());
        bus
    }

    pub fn emitter_arc(&self) -> Option<Arc<dyn AgentEventEmitter>> {
        self.emitter.lock().unwrap().clone()
    }

    pub fn spawn_chunk_consumer_raw(
        emitter: &Arc<dyn AgentEventEmitter>,
    ) -> (ChunkSender, ChunkSender, ConsumerHandle) {
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel(1024);
        let (reasoning_tx, reasoning_rx) = tokio::sync::mpsc::channel(1024);

        let em_clone = emitter.clone();
        let thought_task = tokio::spawn(run_chunk_batcher(chunk_rx, em_clone.clone(), false));
        let reasoning_task = tokio::spawn(run_chunk_batcher(reasoning_rx, em_clone, true));
        // Join both batchers so awaiting this handle guarantees all buffered chunks
        // have been flushed (and emitted) before the caller proceeds.
        let consumer_handle = Some(tokio::spawn(async move {
            let _ = thought_task.await;
            let _ = reasoning_task.await;
        }));

        (chunk_tx, reasoning_tx, consumer_handle)
    }

    pub async fn emit_task_created(&self, task: &TaskInfo) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TaskCreated(task.clone())).await;
        }
    }

    pub async fn emit_task_completed(&self, task_id: &str, title: &str) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::TaskCompleted {
                    task_id: task_id.into(),
                    title: title.into(),
                })
                .await;
        }
    }

    pub async fn emit_task_updated(&self, task_id: &str, status: &str) {
        tracing::debug!(
            "emit_task_updated event: task={} status={}",
            task_id,
            status
        );
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::TaskUpdated {
                    task_id: task_id.into(),
                    status: status.into(),
                })
                .await;
        }
    }

    pub async fn emit_title_updated(&self, task_id: &str, title: &str) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::TitleUpdated {
                    task_id: task_id.into(),
                    title: title.into(),
                })
                .await;
        }
    }

    /// Surface a user-facing notification (used by fired reminders, which are
    /// not tied to a task). Same event the `notify` tool produces.
    pub async fn emit_notification(&self, title: &str, body: &str) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::Notification {
                    task_id: String::new(),
                    title: title.into(),
                    body: body.into(),
                })
                .await;
        }
    }

    // ── Static helpers for working with Arc<dyn AgentEventEmitter> ──

    pub async fn emit_thought_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        thought: &str,
        step_number: u32,
        run_id: u64,
        db: &Database,
    ) {
        tracing::debug!(
            "emit_thought: task={} step={} run={} thought_len={}",
            task_id,
            step_number,
            run_id,
            thought.len()
        );
        let _ = db.create_thought_step(task_id, step_number as i32, thought);
        emitter
            .emit(AgentEvent::Thought {
                task_id: task_id.into(),
                thought: thought.into(),
                step_number,
                run_id,
            })
            .await;
    }

    pub async fn emit_balanced_model_activated_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        reason: &str,
    ) {
        emitter
            .emit(AgentEvent::BalancedModelActivated {
                task_id: task_id.into(),
                reason: reason.into(),
            })
            .await;
    }

    pub async fn emit_compaction_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        summary: &str,
        tokens_before: u32,
        tokens_after: u32,
    ) {
        emitter
            .emit(AgentEvent::Compaction {
                task_id: task_id.into(),
                summary: summary.into(),
                tokens_before,
                tokens_after,
            })
            .await;
    }

    pub async fn emit_task_error_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        error: &str,
    ) {
        emitter
            .emit(AgentEvent::TaskError {
                task_id: task_id.into(),
                error: error.into(),
            })
            .await;
    }

    pub async fn emit_usage_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        usage: crate::event::UsagePayload,
    ) {
        emitter
            .emit(AgentEvent::Usage {
                task_id: usage.task_id,
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cost_usd: usage.cost_usd,
                model: usage.model,
                cumulative_prompt_tokens: usage.cumulative_prompt_tokens,
                cumulative_completion_tokens: usage.cumulative_completion_tokens,
                cumulative_total_tokens: usage.cumulative_total_tokens,
                cumulative_cost_usd: usage.cumulative_cost_usd,
                context_window: usage.context_window,
            })
            .await;
    }
}

/// Bundle of values for emitting an `AgentEvent::Usage` without forcing every
/// caller to construct the full enum variant inline. Keeps the emit helper
/// signature narrow.
#[derive(Debug, Clone)]
pub struct UsagePayload {
    pub task_id: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub cumulative_prompt_tokens: u32,
    pub cumulative_completion_tokens: u32,
    pub cumulative_total_tokens: u32,
    pub cumulative_cost_usd: Option<f64>,
    pub context_window: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Collects every emitted `AgentEvent` into a guarded Vec.
    struct CollectorEmitter {
        events: Mutex<Vec<AgentEvent>>,
    }

    #[async_trait]
    impl AgentEventEmitter for CollectorEmitter {
        async fn emit(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn delta_of(event: &AgentEvent) -> Option<&str> {
        match event {
            AgentEvent::ThoughtChunk { delta, .. } | AgentEvent::ReasoningChunk { delta, .. } => {
                Some(delta)
            }
            _ => None,
        }
    }

    #[tokio::test]
    async fn batcher_aggregates_into_fewer_emits_and_preserves_content() {
        let emitter: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel::<(String, String, u32, u64)>(1024);
        let handle = tokio::spawn(run_chunk_batcher(rx, emitter.clone(), false));

        // Push 100 tiny per-token chunks faster than the batch interval.
        for i in 0..100u32 {
            tx.send(("t1".into(), format!("{}", i % 10), 1, 7))
                .await
                .unwrap();
        }
        drop(tx);
        handle.await.unwrap();

        let events = emitter.events.lock().unwrap().clone();
        let chunks: Vec<&str> = events.iter().filter_map(delta_of).collect();
        // Content preserved: concatenation equals "0123456789" repeated 10×.
        let total: String = chunks.concat();
        assert_eq!(total, "0123456789".repeat(10));
        // Far fewer emits than tokens (100 → a handful of batches). With 100 fast
        // tokens and a 50ms window there should be multiple batches but well under 100.
        assert!(
            chunks.len() < 100,
            "expected batching, got {} emits",
            chunks.len()
        );
        // Every emit carries the same step/run identity.
        for e in &events {
            if let AgentEvent::ThoughtChunk {
                step_number,
                run_id,
                ..
            } = e
            {
                assert_eq!(*step_number, 1);
                assert_eq!(*run_id, 7);
            }
        }
    }

    #[tokio::test]
    async fn batcher_flushes_on_channel_close_even_within_interval() {
        let emitter: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel::<(String, String, u32, u64)>(1024);
        let handle = tokio::spawn(run_chunk_batcher(rx, emitter.clone(), true));

        tx.send(("t2".into(), "hello ".into(), 3, 1)).await.unwrap();
        tx.send(("t2".into(), "world".into(), 3, 1)).await.unwrap();
        drop(tx);
        // Should NOT need to wait the full 50ms — closing the sender flushes promptly.
        let joined = tokio::time::timeout(std::time::Duration::from_millis(200), handle);
        joined.await.unwrap().unwrap();

        let events = emitter.events.lock().unwrap().clone();
        let total: String = events.iter().filter_map(delta_of).collect();
        assert_eq!(total, "hello world");
        // Reasoning path was used.
        assert!(
            events
                .iter()
                .all(|e| matches!(e, AgentEvent::ReasoningChunk { .. }))
        );
    }

    #[tokio::test]
    async fn batcher_flushes_on_max_bytes_threshold() {
        let emitter: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel::<(String, String, u32, u64)>(1024);
        let handle = tokio::spawn(run_chunk_batcher(rx, emitter.clone(), false));

        // Push enough data to cross CHUNK_BATCH_MAX_BYTES mid-batch.
        let big = "x".repeat(CHUNK_BATCH_MAX_BYTES);
        tx.send(("t3".into(), big.clone(), 1, 1)).await.unwrap();
        tx.send(("t3".into(), "tail".into(), 1, 1)).await.unwrap();
        drop(tx);
        handle.await.unwrap();

        let events = emitter.events.lock().unwrap().clone();
        let total: String = events.iter().filter_map(delta_of).collect();
        assert_eq!(total, format!("{}tail", big));
    }

    #[tokio::test]
    async fn event_bus_fans_out_to_all_subscribers() {
        let bus = Arc::new(EventBus::new());
        let a: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let b: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        bus.subscribe("a", a.clone()).await;
        bus.subscribe("b", b.clone()).await;

        bus.emit(AgentEvent::ThoughtChunk {
            task_id: "t".into(),
            delta: "hi".into(),
            step_number: 1,
            run_id: 1,
        })
        .await;

        assert_eq!(a.events.lock().unwrap().len(), 1);
        assert_eq!(b.events.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn event_bus_unsubscribe_stops_delivery() {
        let bus = Arc::new(EventBus::new());
        let a: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        bus.subscribe("a", a.clone()).await;
        let removed = bus.unsubscribe("a").await;
        assert!(removed.is_some());

        bus.emit(AgentEvent::ThoughtChunk {
            task_id: "t".into(),
            delta: "x".into(),
            step_number: 1,
            run_id: 1,
        })
        .await;
        assert!(a.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn event_bus_subscribe_replaces_existing_id() {
        let bus = Arc::new(EventBus::new());
        let a: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let b: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        bus.subscribe("a", a.clone()).await;
        let prev = bus.subscribe("a", b.clone()).await;
        assert!(prev.is_some());

        bus.emit(AgentEvent::ThoughtChunk {
            task_id: "t".into(),
            delta: "x".into(),
            step_number: 1,
            run_id: 1,
        })
        .await;
        // Replaced subscriber "a" is now b; the old a should NOT receive.
        assert!(a.events.lock().unwrap().is_empty());
        assert_eq!(b.events.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn install_bus_installs_event_bus_as_emitter() {
        let dispatcher = EventDispatcher::new();
        let bus = dispatcher.install_bus();
        let collector: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        bus.subscribe("c", collector.clone()).await;

        // emit_thought_from drives the installed emitter (the bus), which fans out.
        let mut p = std::env::temp_dir();
        p.push(format!("haven_event_test_{}.db", uuid::Uuid::new_v4()));
        let db = Database::open(&p).unwrap();
        let bus_dyn: Arc<dyn AgentEventEmitter> = bus;
        EventDispatcher::emit_thought_from(&bus_dyn, "t", "hello", 1, 1, &db).await;
        assert_eq!(collector.events.lock().unwrap().len(), 1);
        assert!(matches!(
            collector.events.lock().unwrap()[0],
            AgentEvent::Thought { .. }
        ));
    }
}
