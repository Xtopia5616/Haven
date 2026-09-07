use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};

use crate::session::SessionInfo;
use async_trait::async_trait;
use haven_memory::Database;
use serde::{Deserialize, Serialize};
use serde_json::Value;

fn lock_or_recover<'a, T>(lock: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::error!(lock = name, "agent event lock poisoned; recovering state");
        poisoned.into_inner()
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    Thought {
        session_id: String,
        thought: String,
        step_number: u32,
        run_id: u64,
        /// The `msg-*` id this thought is persisted under (minted when the
        /// step's thought stream started). The frontend uses it as the live
        /// bubble id, so live streaming, snap and the DB copy share one id
        /// and merges need no content-based dedup.
        message_id: String,
    },
    Action {
        session_id: String,
        tool_name: String,
        input: Value,
        step_number: u32,
        run_id: u64,
        tool_call_id: Option<String>,
        /// Stable zero-based position in the assistant tool-call list.
        action_index: u32,
        /// The `step-*` id of the step row this action is persisted under
        /// (minted before the action starts). The frontend uses it as the
        /// live tool-card id, matching the resume badge built from the DB.
        step_id: String,
        /// Remove provider text that was streamed before a tool call but was
        /// rejected as a non-meaningful fragment by the ReAct parser.
        suppress_streamed_thought: bool,
    },
    Observation {
        session_id: String,
        observation: String,
        tool_name: String,
        step_number: u32,
        run_id: u64,
        silent: bool,
        tool_call_id: Option<String>,
        /// Stable zero-based position in the assistant tool-call list.
        action_index: u32,
        /// Quick-reply options surfaced when the observation comes from the
        /// `ask` tool, so the UI can render clickable answer buttons.
        ask_options: Vec<String>,
        /// Same `step-*` id the matching `Action` event carried, so the live
        /// tool card keeps one id through placeholder → fill → DB badge.
        step_id: String,
        /// Durable terminal execution outcome for the tool card.
        outcome: String,
        /// Replay policy of the concrete operation, not just its tool name.
        idempotency: String,
        /// Whether the operation targets session-local or global state.
        operation_scope: String,
    },
    SessionCreated(SessionInfo),
    SessionCompleted {
        session_id: String,
        title: String,
    },
    SessionError {
        session_id: String,
        error: String,
    },
    ThoughtChunk {
        session_id: String,
        delta: String,
        step_number: u32,
        run_id: u64,
        /// `msg-*` id of the message this chunk accumulates into. Constant
        /// for the whole (step, run) block, matching the id the backend
        /// later persists the final text under.
        message_id: String,
    },
    ReasoningChunk {
        session_id: String,
        delta: String,
        step_number: u32,
        run_id: u64,
        /// Same semantics as `ThoughtChunk::message_id`.
        message_id: String,
    },
    /// Ordered boundary emitted before a replacement stream attempt. The
    /// frontend removes only the live thought/reasoning blocks for this step;
    /// durable transcript events remain authoritative.
    StreamReset {
        session_id: String,
        step_number: u32,
        run_id: u64,
        thought_message_id: String,
        reasoning_message_id: String,
    },
    /// Live status of the provider's built-in web search tool. Forwarded from
    /// the stream events (`in_progress` → `searching` → `completed`) so the
    /// UI can render one card per call. DeepSeek may emit several
    /// `web_search_call` items (`search` / `open_page` / `find_in_page`) in
    /// a single turn; `call_id` / `action` distinguish them. `result` carries
    /// the compact tool return (`{queries, results:[{title,url,snippet}]}`)
    /// on the completed phase so the card shows what the search returned.
    WebSearch {
        session_id: String,
        phase: String,
        step_number: u32,
        run_id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<serde_json::Value>,
    },
    /// The provider stream went silent (no chunk for several seconds) while
    /// the step is still in flight. Emitted by a per-call watchdog so the UI
    /// can show a factual waiting state instead of looking frozen; the stream
    /// itself is only aborted at the router's idle timeout. Re-emitted when
    /// the stream resumes and stalls again.
    StreamStalled {
        session_id: String,
    },
    Supplement {
        session_id: String,
        additional_context: String,
        step_number: u32,
        run_id: u64,
        /// User message id when the supplement came from persisted ingress.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// Stable identity for every live supplement, including action-result
        /// and cross-session context that has no user message row.
        supplement_id: String,
        /// Structured inject origin so the UI can render peer mail as an
        /// `agent` tool card, background-action auto-wake as an `actions`
        /// card, and still mark human steering as received.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inject_source: Option<haven_common::types::InjectSource>,
    },
    SessionUpdated {
        session_id: String,
        status: String,
    },
    Compaction {
        session_id: String,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
        /// Shared `msg-*` with the canonical summary bubble / episode row (L1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        episode_id: Option<String>,
        /// True when a deterministic older-context marker was used instead
        /// of an LLM-generated summary.
        #[serde(default)]
        degraded: bool,
    },
    TitleUpdated {
        session_id: String,
        title: String,
    },
    /// A user-facing notification requested by the agent (via the `notify`
    /// tool). Surfaced both in-app (toast) and as a Windows notification.
    Notification {
        session_id: String,
        title: String,
        body: String,
    },
    /// Token-usage statistics for one LLM call (or aggregate). Surfaces
    /// prompt/completion/total tokens and the USD cost when the active
    /// endpoint has pricing configured. Emitted after every ReAct step so
    /// the UI can display a running counter and remaining context budget.
    /// `step_number` / `duration_ms` / `role` / `has_cost` mirror the
    /// persisted `llm_usage` row so live tool cards can render the same
    /// per-step token chip that resume mode restores from the DB.
    Usage {
        session_id: String,
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        /// Prompt-cache hit / read tokens for this call.
        #[serde(default)]
        cached_tokens: u32,
        /// Prompt-cache write / creation tokens for this call.
        #[serde(default)]
        cache_creation_tokens: u32,
        /// Prompt tokens processed outside the cache read path.
        #[serde(default)]
        cache_miss_tokens: u32,
        /// Tokens occupying the model context window for this call
        /// (prompt, plus exclusive cache tokens when the provider reports
        /// cache outside `prompt_tokens`).
        #[serde(default)]
        context_tokens: u32,
        /// True when cache read/write tokens are counted outside `prompt_tokens`.
        #[serde(default)]
        cache_exclusive: bool,
        /// Explicit per-call cache token accounting contract (`inclusive`,
        /// `exclusive`, or `unknown` for legacy/unsupported providers).
        #[serde(default)]
        cache_accounting: String,
        cost_usd: Option<f64>,
        model: Option<String>,
        /// Cumulative totals across the entire session (incl. this step).
        cumulative_prompt_tokens: u32,
        cumulative_completion_tokens: u32,
        cumulative_total_tokens: u32,
        #[serde(default)]
        cumulative_cached_tokens: u32,
        #[serde(default)]
        cumulative_cache_creation_tokens: u32,
        #[serde(default)]
        cumulative_cache_miss_tokens: u32,
        /// Non-sensitive routing/outcome metadata; never includes cache key or prompt text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_diagnostics: Option<haven_llm::CacheDiagnostics>,
        cumulative_cost_usd: Option<f64>,
        /// Configured context window for the model (tokens). When `None`,
        /// the UI falls back to a generic budget indicator.
        context_window: Option<u32>,
        /// ReAct step this call served (`None` for non-step aggregates).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_number: Option<u32>,
        /// Wall-clock duration of the call in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        /// Endpoint role that produced the response (`default` / `small` / …).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        role: Option<String>,
        /// Whether `cost_usd` is a real priced value (vs absent pricing).
        #[serde(default)]
        has_cost: bool,
    },
}

#[async_trait]
pub trait AgentEventEmitter: Send + Sync {
    async fn emit(&self, event: AgentEvent);
}

/// Bounded-queue emitter wrapper: `emit` becomes an enqueue into an in-memory
/// queue drained by a dedicated consumer session, so producers (the ReAct loop,
/// chunk batchers, action/scheduled-action consumers) never await the subscriber chain
/// (Tauri IPC, toast logic, log writers). The queue is sized far above the
/// per-step event volume and chunk deltas are already micro-batched upstream,
/// so eviction is a logged last resort, not a normal path.
///
/// Overflow policy: the OLDEST queued chunk event (`ThoughtChunk` /
/// `ReasoningChunk`) is evicted to make room for the new event — never the
/// newest one. Chunk deltas are self-healing: the step's final
/// `agent:thought` snap and the full-text reasoning reconcile replace the
/// accumulated streamed text, so losing an old intermediate chunk is
/// invisible in the end state. Stream-reset markers are authoritative ordering
/// events and are kept in preference to ordinary status events. Dropping the
/// newest event (or a non-chunk event like the snap, session status, completion,
/// error) would lose authoritative state permanently — the very events that
/// repair the stream.
///
/// Ordering within one producer is preserved (FIFO); concurrent producers
/// interleave, exactly as with direct awaited emits.
pub struct BufferedEmitter {
    queue: std::sync::Mutex<std::collections::VecDeque<AgentEvent>>,
    notify: tokio::sync::Notify,
    capacity: usize,
}

impl BufferedEmitter {
    pub fn new(capacity: usize, inner: Arc<dyn AgentEventEmitter>) -> Arc<Self> {
        let this = Arc::new(Self {
            queue: std::sync::Mutex::new(std::collections::VecDeque::new()),
            notify: tokio::sync::Notify::new(),
            capacity,
        });
        let worker = this.clone();
        tokio::spawn(async move {
            loop {
                let ev = lock_or_recover(&worker.queue, "buffered_event_queue").pop_front();
                match ev {
                    Some(ev) => inner.emit(ev).await,
                    None => {
                        // Register the waiter BEFORE re-checking the queue so
                        // a push between the initial pop and the registration
                        // cannot be missed (a notify_one with no registered
                        // waiter would be lost, wedging the drain forever).
                        let notified = worker.notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        if lock_or_recover(&worker.queue, "buffered_event_queue").is_empty() {
                            notified.await;
                        }
                    }
                }
            }
        });
        this
    }
}

/// Whether an event is a streamed chunk delta (self-healing via the step's
/// final snap / full-text reconcile, so the safest overflow eviction target).
fn is_chunk_event(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::ThoughtChunk { .. } | AgentEvent::ReasoningChunk { .. }
    )
}

#[async_trait]
impl AgentEventEmitter for BufferedEmitter {
    async fn emit(&self, event: AgentEvent) {
        let mut queue = lock_or_recover(&self.queue, "buffered_event_queue");
        if queue.len() >= self.capacity {
            if let Some(pos) = queue.iter().position(is_chunk_event) {
                queue.remove(pos);
                tracing::warn!(
                    "event buffer full (capacity {}), evicting oldest queued chunk event",
                    self.capacity
                );
            } else if let Some(pos) = queue.iter().position(|queued| !is_stream_reset(queued)) {
                queue.remove(pos);
                tracing::warn!(
                    "event buffer full (capacity {}), evicting ordinary event to preserve stream reset",
                    self.capacity
                );
            } else if is_stream_reset(&event) {
                queue.pop_front();
                tracing::warn!(
                    "event buffer full (capacity {}), evicting oldest stream reset",
                    self.capacity
                );
            } else {
                tracing::warn!(
                    "event buffer full (capacity {}), dropping incoming ordinary event",
                    self.capacity
                );
                return;
            }
        }
        queue.push_back(event);
        drop(queue);
        self.notify.notify_one();
    }
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

/// One ordered item in the live-stream event pipeline.
///
/// Thought and reasoning used to have independent queues, which meant a
/// retry could overtake the other kind of chunk. A single queue makes output
/// order explicit and lets a retry insert a reset marker before its first
/// delta. Both ids are `Arc<str>` so the producer's per-token hot loop shares
/// allocations instead of cloning the session/message ids each time.
pub(crate) enum ChunkItem {
    Delta {
        session_id: Arc<str>,
        message_id: Arc<str>,
        delta: String,
        step_number: u32,
        run_id: u64,
        reasoning: bool,
    },
    Reset {
        session_id: Arc<str>,
        thought_message_id: Arc<str>,
        reasoning_message_id: Arc<str>,
        step_number: u32,
        run_id: u64,
    },
}

fn is_stream_reset(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::StreamReset { .. })
}
pub(crate) type ChunkSender = tokio::sync::mpsc::Sender<ChunkItem>;
pub(crate) type ConsumerHandle = Option<tokio::task::JoinHandle<()>>;

/// Per-chunk micro-batching parameters. Incoming per-token chunks are aggregated
/// for at most this duration before a single `ThoughtChunk`/`ReasoningChunk` with
/// the concatenated `delta` is emitted, dramatically reducing Tauri IPC frequency.
const CHUNK_BATCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

async fn emit_chunk_delta(
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: Arc<str>,
    message_id: Arc<str>,
    delta: String,
    step_number: u32,
    run_id: u64,
    reasoning: bool,
) {
    if delta.is_empty() {
        return;
    }
    let event = if reasoning {
        AgentEvent::ReasoningChunk {
            session_id: session_id.to_string(),
            delta,
            step_number,
            run_id,
            message_id: message_id.to_string(),
        }
    } else {
        AgentEvent::ThoughtChunk {
            session_id: session_id.to_string(),
            delta,
            step_number,
            run_id,
            message_id: message_id.to_string(),
        }
    };
    emitter.emit(event).await;
}

async fn emit_stream_reset(
    emitter: &Arc<dyn AgentEventEmitter>,
    session_id: Arc<str>,
    thought_message_id: Arc<str>,
    reasoning_message_id: Arc<str>,
    step_number: u32,
    run_id: u64,
) {
    emitter
        .emit(AgentEvent::StreamReset {
            session_id: session_id.to_string(),
            thought_message_id: thought_message_id.to_string(),
            reasoning_message_id: reasoning_message_id.to_string(),
            step_number,
            run_id,
        })
        .await;
}

/// Runs one ordered chunk batcher. Deltas for the same stream block are
/// aggregated for at most `CHUNK_BATCH_INTERVAL` (or until
/// `max_batch_bytes`). Reset markers are hard boundaries: pending deltas are
/// flushed first, then the reset is emitted, so the frontend never observes a
/// new attempt before the old attempt has drained.
async fn run_chunk_batcher(
    mut rx: tokio::sync::mpsc::Receiver<ChunkItem>,
    emitter: Arc<dyn AgentEventEmitter>,
    max_batch_bytes: usize,
) {
    loop {
        let first = match rx.recv().await {
            Some(item) => item,
            None => return,
        };

        let ChunkItem::Delta {
            mut session_id,
            mut message_id,
            mut delta,
            mut step_number,
            mut run_id,
            mut reasoning,
        } = first
        else {
            if let ChunkItem::Reset {
                session_id,
                thought_message_id,
                reasoning_message_id,
                step_number,
                run_id,
            } = first
            {
                emit_stream_reset(
                    &emitter,
                    session_id,
                    thought_message_id,
                    reasoning_message_id,
                    step_number,
                    run_id,
                )
                .await;
            }
            continue;
        };

        // Emit the first chunk immediately so the user sees text without the
        // 50ms batch delay. Subsequent chunks are aggregated normally.
        emit_chunk_delta(
            &emitter,
            session_id.clone(),
            message_id.clone(),
            std::mem::take(&mut delta),
            step_number,
            run_id,
            reasoning,
        )
        .await;
        let mut buf = String::new();
        let mut buf_bytes = 0usize;
        let mut deadline = tokio::time::Instant::now() + CHUNK_BATCH_INTERVAL;

        loop {
            tokio::select! {
                biased;
                val = rx.recv() => {
                    match val {
                        Some(ChunkItem::Delta {
                            session_id: session_id_2,
                            message_id: message_id_2,
                            delta: delta_2,
                            step_number: step_number_2,
                            run_id: run_id_2,
                            reasoning: reasoning_2,
                        }) => {
                            if (&*session_id_2, &*message_id_2, step_number_2, run_id_2, reasoning_2)
                                != (&*session_id, &*message_id, step_number, run_id, reasoning)
                            {
                                // key changed: flush current batch, start a new one
                                emit_chunk_delta(
                                    &emitter,
                                    session_id.clone(),
                                    message_id.clone(),
                                    std::mem::take(&mut buf),
                                    step_number,
                                    run_id,
                                    reasoning,
                                ).await;
                                session_id = session_id_2;
                                message_id = message_id_2;
                                step_number = step_number_2;
                                run_id = run_id_2;
                                reasoning = reasoning_2;
                                emit_chunk_delta(
                                    &emitter,
                                    session_id.clone(),
                                    message_id.clone(),
                                    delta_2,
                                    step_number,
                                    run_id,
                                    reasoning,
                                ).await;
                                buf.clear();
                                buf_bytes = 0;
                                deadline = tokio::time::Instant::now() + CHUNK_BATCH_INTERVAL;
                            } else {
                                buf_bytes += delta_2.len();
                                buf.push_str(&delta_2);
                                if buf_bytes >= max_batch_bytes {
                                    emit_chunk_delta(
                                        &emitter,
                                        session_id.clone(),
                                        message_id.clone(),
                                        std::mem::take(&mut buf),
                                        step_number,
                                        run_id,
                                        reasoning,
                                    ).await;
                                    break;
                                }
                            }
                        }
                        Some(ChunkItem::Reset {
                            session_id: reset_session_id,
                            thought_message_id,
                            reasoning_message_id,
                            step_number: reset_step_number,
                            run_id: reset_run_id,
                        }) => {
                            emit_chunk_delta(
                                &emitter,
                                session_id.clone(),
                                message_id.clone(),
                                std::mem::take(&mut buf),
                                step_number,
                                run_id,
                                reasoning,
                            ).await;
                            emit_stream_reset(
                                &emitter,
                                reset_session_id,
                                thought_message_id,
                                reasoning_message_id,
                                reset_step_number,
                                reset_run_id,
                            ).await;
                            break;
                        }
                        None => {
                            emit_chunk_delta(
                                &emitter,
                                session_id.clone(),
                                message_id.clone(),
                                std::mem::take(&mut buf),
                                step_number,
                                run_id,
                                reasoning,
                            ).await;
                            return;
                        }
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    emit_chunk_delta(
                        &emitter,
                        session_id.clone(),
                        message_id.clone(),
                        std::mem::take(&mut buf),
                        step_number,
                        run_id,
                        reasoning,
                    ).await;
                    break;
                }
            }
            // The `biased` select prefers the recv branch whenever a chunk is
            // already queued, so under a continuous fast stream the timer above
            // would never be polled as ready — batches would only flush when
            // `max_batch_bytes` was crossed (a big silent pause followed by an
            // 8KB dump). Check the fixed deadline after every received chunk:
            // once it has passed the batch flushes on time no matter how hot
            // the producer is, so the UI always receives smooth ~50ms updates.
            if tokio::time::Instant::now() >= deadline {
                emit_chunk_delta(
                    &emitter,
                    session_id.clone(),
                    message_id.clone(),
                    std::mem::take(&mut buf),
                    step_number,
                    run_id,
                    reasoning,
                )
                .await;
                break;
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
        *lock_or_recover(&self.emitter, "event_emitter") = Some(emitter);
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
        lock_or_recover(&self.emitter, "event_emitter").clone()
    }

    pub(crate) fn spawn_chunk_consumer_raw(
        emitter: &Arc<dyn AgentEventEmitter>,
        max_batch_bytes: usize,
    ) -> (ChunkSender, ConsumerHandle) {
        let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel(1024);

        let em_clone = emitter.clone();
        let thought_session = tokio::spawn(run_chunk_batcher(
            chunk_rx,
            em_clone.clone(),
            max_batch_bytes,
        ));
        // Awaiting this handle guarantees all buffered chunks and reset
        // markers have been flushed before the caller proceeds.
        let consumer_handle = Some(tokio::spawn(async move {
            let _ = thought_session.await;
        }));

        (chunk_tx, consumer_handle)
    }

    pub async fn emit_session_created(&self, session: &SessionInfo) {
        let emitter = lock_or_recover(&self.emitter, "event_emitter").clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::SessionCreated(session.clone()))
                .await;
        }
    }

    pub async fn emit_session_completed(&self, session_id: &str, title: &str) {
        let emitter = lock_or_recover(&self.emitter, "event_emitter").clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::SessionCompleted {
                    session_id: session_id.into(),
                    title: title.into(),
                })
                .await;
        }
    }

    pub async fn emit_session_updated(&self, session_id: &str, status: &str) {
        tracing::debug!(
            "emit_session_updated event: session={} status={}",
            session_id,
            status
        );
        let emitter = lock_or_recover(&self.emitter, "event_emitter").clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::SessionUpdated {
                    session_id: session_id.into(),
                    status: status.into(),
                })
                .await;
        }
    }

    pub async fn emit_title_updated(&self, session_id: &str, title: &str) {
        let emitter = lock_or_recover(&self.emitter, "event_emitter").clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::TitleUpdated {
                    session_id: session_id.into(),
                    title: title.into(),
                })
                .await;
        }
    }

    /// Surface a user-facing notification (used by fired scheduled_actions, which are
    /// not tied to a session). Same event the `notify` tool produces.
    pub async fn emit_notification(&self, title: &str, body: &str) {
        let emitter = lock_or_recover(&self.emitter, "event_emitter").clone();
        if let Some(emitter) = emitter {
            emitter
                .emit(AgentEvent::Notification {
                    session_id: String::new(),
                    title: title.into(),
                    body: body.into(),
                })
                .await;
        }
    }

    // ── Static helpers for working with Arc<dyn AgentEventEmitter> ──

    pub async fn emit_thought_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        thought: &str,
        step_number: u32,
        run_id: u64,
        message_id: &str,
        db: &Arc<Database>,
    ) -> anyhow::Result<()> {
        tracing::debug!(
            "emit_thought: session={} step={} run={} msg={} thought_len={}",
            session_id,
            step_number,
            run_id,
            message_id,
            thought.len()
        );
        // The step row shares the streamed bubble's id (the message row is
        // persisted under the same id) and stores no text: the thought text
        // lives exclusively in the `messages` table. Run on the blocking pool
        // so WAL fsync cannot stall the async runtime (same contract as
        // UserInject thought-step writes).
        let sid = session_id.to_string();
        let mid = message_id.to_string();
        let step = step_number;
        db.run_blocking(move |db| {
            db.create_thought_step(&sid, step as i32, &mid)?;
            Ok::<(), anyhow::Error>(())
        })
        .await?;
        emitter
            .emit(AgentEvent::Thought {
                session_id: session_id.into(),
                thought: thought.into(),
                step_number,
                run_id,
                message_id: message_id.into(),
            })
            .await;
        Ok(())
    }

    pub async fn emit_compaction_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        summary: &str,
        tokens_before: u32,
        tokens_after: u32,
        episode_id: &str,
        degraded: bool,
    ) {
        emitter
            .emit(AgentEvent::Compaction {
                session_id: session_id.into(),
                summary: summary.into(),
                tokens_before,
                tokens_after,
                episode_id: Some(episode_id.into()),
                degraded,
            })
            .await;
    }

    pub async fn emit_session_error_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        error: &str,
    ) {
        emitter
            .emit(AgentEvent::SessionError {
                session_id: session_id.into(),
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
                session_id: usage.session_id,
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                cached_tokens: usage.cached_tokens,
                cache_creation_tokens: usage.cache_creation_tokens,
                cache_miss_tokens: usage.cache_miss_tokens,
                context_tokens: usage.context_tokens,
                cache_exclusive: usage.cache_exclusive,
                cache_accounting: usage.cache_accounting,
                cost_usd: usage.cost_usd,
                model: usage.model,
                cumulative_prompt_tokens: usage.cumulative_prompt_tokens,
                cumulative_completion_tokens: usage.cumulative_completion_tokens,
                cumulative_total_tokens: usage.cumulative_total_tokens,
                cumulative_cached_tokens: usage.cumulative_cached_tokens,
                cumulative_cache_creation_tokens: usage.cumulative_cache_creation_tokens,
                cumulative_cache_miss_tokens: usage.cumulative_cache_miss_tokens,
                cache_diagnostics: usage.cache_diagnostics,
                cumulative_cost_usd: usage.cumulative_cost_usd,
                context_window: usage.context_window,
                step_number: usage.step_number,
                duration_ms: usage.duration_ms,
                role: usage.role,
                has_cost: usage.has_cost,
            })
            .await;
    }
}

/// Bundle of values for emitting an `AgentEvent::Usage` without forcing every
/// caller to construct the full enum variant inline. Keeps the emit helper
/// signature narrow.
#[derive(Debug, Clone)]
pub struct UsagePayload {
    pub session_id: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub cached_tokens: u32,
    pub cache_creation_tokens: u32,
    pub cache_miss_tokens: u32,
    pub context_tokens: u32,
    pub cache_exclusive: bool,
    pub cache_accounting: String,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
    pub cumulative_prompt_tokens: u32,
    pub cumulative_completion_tokens: u32,
    pub cumulative_total_tokens: u32,
    pub cumulative_cached_tokens: u32,
    pub cumulative_cache_creation_tokens: u32,
    pub cumulative_cache_miss_tokens: u32,
    pub cache_diagnostics: Option<haven_llm::CacheDiagnostics>,
    pub cumulative_cost_usd: Option<f64>,
    pub context_window: Option<u32>,
    pub step_number: Option<u32>,
    pub duration_ms: Option<u64>,
    pub role: Option<String>,
    pub has_cost: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Default max-batch threshold used by the batcher tests (the production
    /// default now lives in `context_limits.event_chunk_batch_max_bytes`).
    const DEFAULT_CHUNK_BATCH_MAX_BYTES: usize = 8 * 1024;

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

    fn collector_emitter() -> Arc<CollectorEmitter> {
        Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        })
    }

    fn collector_emitters() -> (Arc<CollectorEmitter>, Arc<CollectorEmitter>) {
        (collector_emitter(), collector_emitter())
    }

    fn delta(
        session_id: &str,
        message_id: &str,
        text: impl Into<String>,
        step_number: u32,
        run_id: u64,
        reasoning: bool,
    ) -> ChunkItem {
        ChunkItem::Delta {
            session_id: Arc::from(session_id),
            message_id: Arc::from(message_id),
            delta: text.into(),
            step_number,
            run_id,
            reasoning,
        }
    }

    #[tokio::test]
    async fn batcher_aggregates_into_fewer_emits_and_preserves_content() {
        let emitter = collector_emitter();
        let (tx, rx) = tokio::sync::mpsc::channel::<ChunkItem>(1024);
        let handle = tokio::spawn(run_chunk_batcher(
            rx,
            emitter.clone(),
            DEFAULT_CHUNK_BATCH_MAX_BYTES,
        ));

        // Push 100 tiny per-token chunks faster than the batch interval.
        for i in 0..100u32 {
            tx.send(delta(
                "t1",
                "msg-thought-1",
                format!("{}", i % 10),
                1,
                7,
                false,
            ))
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
        // Every emit carries the same step/run identity and message id.
        for e in &events {
            if let AgentEvent::ThoughtChunk {
                step_number,
                run_id,
                message_id,
                ..
            } = e
            {
                assert_eq!(*step_number, 1);
                assert_eq!(*run_id, 7);
                assert_eq!(message_id, "msg-thought-1");
            }
        }
    }

    #[tokio::test]
    async fn batcher_flushes_on_channel_close_even_within_interval() {
        let emitter: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let (tx, rx) = tokio::sync::mpsc::channel::<ChunkItem>(1024);
        let handle = tokio::spawn(run_chunk_batcher(
            rx,
            emitter.clone(),
            DEFAULT_CHUNK_BATCH_MAX_BYTES,
        ));

        tx.send(delta("t2", "msg-1", "hello ", 3, 1, true))
            .await
            .unwrap();
        tx.send(delta("t2", "msg-1", "world", 3, 1, true))
            .await
            .unwrap();
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
        let emitter = collector_emitter();
        let (tx, rx) = tokio::sync::mpsc::channel::<ChunkItem>(1024);
        let handle = tokio::spawn(run_chunk_batcher(
            rx,
            emitter.clone(),
            DEFAULT_CHUNK_BATCH_MAX_BYTES,
        ));

        // Push enough data to cross the default max-batch threshold mid-batch.
        let big = "x".repeat(DEFAULT_CHUNK_BATCH_MAX_BYTES);
        tx.send(delta("t3", "msg-3", big.clone(), 1, 1, false))
            .await
            .unwrap();
        tx.send(delta("t3", "msg-3", "tail", 1, 1, false))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        let events = emitter.events.lock().unwrap().clone();
        let total: String = events.iter().filter_map(delta_of).collect();
        assert_eq!(total, format!("{}tail", big));
    }

    #[tokio::test]
    async fn batcher_keeps_reset_between_old_and_new_attempts() {
        let emitter = collector_emitter();
        let (tx, rx) = tokio::sync::mpsc::channel::<ChunkItem>(16);
        let handle = tokio::spawn(run_chunk_batcher(
            rx,
            emitter.clone(),
            DEFAULT_CHUNK_BATCH_MAX_BYTES,
        ));

        tx.send(delta("t4", "msg-4", "old", 2, 9, false))
            .await
            .unwrap();
        tx.send(ChunkItem::Reset {
            session_id: Arc::from("t4"),
            thought_message_id: Arc::from("msg-4"),
            reasoning_message_id: Arc::from("msg-r-4"),
            step_number: 2,
            run_id: 9,
        })
        .await
        .unwrap();
        tx.send(delta("t4", "msg-4", "new", 2, 9, false))
            .await
            .unwrap();
        drop(tx);
        handle.await.unwrap();

        let events = emitter.events.lock().unwrap().clone();
        assert!(matches!(
            events.as_slice(),
            [
                AgentEvent::ThoughtChunk { delta, .. },
                AgentEvent::StreamReset { .. },
                AgentEvent::ThoughtChunk { delta: next, .. },
            ] if delta == "old" && next == "new"
        ));
    }

    #[tokio::test]
    async fn event_bus_fans_out_to_all_subscribers() {
        let bus = Arc::new(EventBus::new());
        let (a, b) = collector_emitters();
        bus.subscribe("a", a.clone()).await;
        bus.subscribe("b", b.clone()).await;

        bus.emit(AgentEvent::ThoughtChunk {
            session_id: "t".into(),
            message_id: "msg-t-1".into(),
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
            session_id: "t".into(),
            message_id: "msg-t-1".into(),
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
        let (a, b) = collector_emitters();
        bus.subscribe("a", a.clone()).await;
        let prev = bus.subscribe("a", b.clone()).await;
        assert!(prev.is_some());

        bus.emit(AgentEvent::ThoughtChunk {
            session_id: "t".into(),
            message_id: "msg-t-1".into(),
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
        let db = Arc::new(Database::open(&p).unwrap());
        let session = db.create_session("t", "").unwrap();
        let bus_dyn: Arc<dyn AgentEventEmitter> = bus;
        EventDispatcher::emit_thought_from(&bus_dyn, &session.id, "hello", 1, 1, "msg-t-1", &db)
            .await
            .unwrap();
        assert_eq!(collector.events.lock().unwrap().len(), 1);
        assert!(matches!(
            collector.events.lock().unwrap()[0],
            AgentEvent::Thought { .. }
        ));
    }

    #[tokio::test]
    async fn buffered_emitter_delivers_asynchronously_in_order() {
        let collector: Arc<CollectorEmitter> = Arc::new(CollectorEmitter {
            events: Mutex::new(Vec::new()),
        });
        let buffered = BufferedEmitter::new(16, collector.clone() as Arc<dyn AgentEventEmitter>);

        // emit() returns immediately (try_send); the consumer session drains.
        for i in 0..5u32 {
            buffered
                .emit(AgentEvent::SessionUpdated {
                    session_id: format!("ses-{i}"),
                    status: "running".into(),
                })
                .await;
        }
        // The consumer delivers asynchronously; wait briefly for the drain.
        for _ in 0..100 {
            if collector.events.lock().unwrap().len() == 5 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let events = collector.events.lock().unwrap().clone();
        assert_eq!(events.len(), 5, "all events must eventually arrive");
        // FIFO order preserved within the single producer.
        for (i, e) in events.iter().enumerate() {
            match e {
                AgentEvent::SessionUpdated { session_id, .. } => {
                    assert_eq!(session_id, &format!("ses-{i}"))
                }
                other => panic!("unexpected event: {:?}", other),
            }
        }
    }

    /// Slow subscriber that ALSO collects delivered events (the plain
    /// `SlowEmitter` below never touches a collector — events are delivered
    /// to it, not to the test's collector).
    struct SlowCollector {
        events: Mutex<Vec<AgentEvent>>,
    }
    #[async_trait]
    impl AgentEventEmitter for SlowCollector {
        async fn emit(&self, event: AgentEvent) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.events.lock().unwrap().push(event);
        }
    }

    #[tokio::test]
    async fn buffered_emitter_overflow_keeps_newest_chunk() {
        let collector = Arc::new(SlowCollector {
            events: Mutex::new(Vec::new()),
        });
        let slow: Arc<dyn AgentEventEmitter> = collector.clone();
        let buffered = BufferedEmitter::new(1, slow);
        buffered
            .emit(AgentEvent::ThoughtChunk {
                session_id: "t".into(),
                message_id: "msg-t-1".into(),
                delta: "old-chunk".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;
        // Capacity 1 and a slow subscriber: the second emit must evict the
        // oldest queued chunk instead of dropping the newest event or
        // blocking the producer.
        let t0 = std::time::Instant::now();
        buffered
            .emit(AgentEvent::ThoughtChunk {
                session_id: "t".into(),
                message_id: "msg-t-1".into(),
                delta: "new-chunk".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(30),
            "emit must not block when the buffer is full"
        );
        // Whatever interleaving with the drain session occurred, the newest
        // event always survives and is delivered.
        for _ in 0..200 {
            let has_new = collector.events.lock().unwrap().iter().any(
                |e| matches!(e, AgentEvent::ThoughtChunk { delta, .. } if delta == "new-chunk"),
            );
            if has_new {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("newest chunk event was never delivered");
    }

    #[tokio::test]
    async fn buffered_emitter_overflow_never_drops_authoritative_state() {
        let collector = Arc::new(SlowCollector {
            events: Mutex::new(Vec::new()),
        });
        let slow: Arc<dyn AgentEventEmitter> = collector.clone();
        let buffered = BufferedEmitter::new(2, slow);
        // Fill the queue with a chunk + the step's authoritative snap, then
        // overflow it with another chunk: whatever the drain interleaving,
        // the snap and the newest chunk must be delivered — only the oldest
        // chunk is ever evicted.
        buffered
            .emit(AgentEvent::ThoughtChunk {
                session_id: "t".into(),
                message_id: "msg-t-1".into(),
                delta: "streamed-partial".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;
        buffered
            .emit(AgentEvent::Thought {
                session_id: "t".into(),
                message_id: "msg-t-1".into(),
                thought: "full authoritative text".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;
        buffered
            .emit(AgentEvent::ThoughtChunk {
                session_id: "t".into(),
                message_id: "msg-t-1".into(),
                delta: "straggler".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;
        for _ in 0..200 {
            let events = collector.events.lock().unwrap().clone();
            let has_snap = events
                .iter()
                .any(|e| matches!(e, AgentEvent::Thought { .. }));
            let has_new = events.iter().any(
                |e| matches!(e, AgentEvent::ThoughtChunk { delta, .. } if delta == "straggler"),
            );
            if has_snap && has_new {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("authoritative snap or newest chunk was never delivered");
    }

    /// Phase 8 / H2: mid-stream ThoughtChunks may be dropped under overflow,
    /// but the final Thought snap still reconciles the UI to full text.
    #[tokio::test]
    async fn buffered_emitter_drop_chunks_snap_still_reconciles() {
        let collector = Arc::new(SlowCollector {
            events: Mutex::new(Vec::new()),
        });
        let slow: Arc<dyn AgentEventEmitter> = collector.clone();
        let buffered = BufferedEmitter::new(1, slow);

        for i in 0..20u32 {
            buffered
                .emit(AgentEvent::ThoughtChunk {
                    session_id: "t".into(),
                    message_id: "msg-t-1".into(),
                    delta: format!("chunk-{i}"),
                    step_number: 1,
                    run_id: 1,
                })
                .await;
        }
        buffered
            .emit(AgentEvent::Thought {
                session_id: "t".into(),
                message_id: "msg-t-1".into(),
                thought: "full authoritative text after stream".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;

        for _ in 0..300 {
            let events = collector.events.lock().unwrap().clone();
            if let Some(AgentEvent::Thought { thought, .. }) = events
                .iter()
                .rev()
                .find(|e| matches!(e, AgentEvent::Thought { .. }))
            {
                assert_eq!(thought, "full authoritative text after stream");
                // Chunks may have been evicted; snap is the authority.
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("Thought snap never delivered after chunk overflow");
    }

    /// Same reconcile contract for ReasoningChunk → full-text ReasoningChunk
    /// (loop emits a final full-text reasoning chunk after stream).
    #[tokio::test]
    async fn buffered_emitter_drop_reasoning_chunks_full_text_reconciles() {
        let collector = Arc::new(SlowCollector {
            events: Mutex::new(Vec::new()),
        });
        let slow: Arc<dyn AgentEventEmitter> = collector.clone();
        let buffered = BufferedEmitter::new(1, slow);

        for i in 0..15u32 {
            buffered
                .emit(AgentEvent::ReasoningChunk {
                    session_id: "t".into(),
                    message_id: "msg-r-1".into(),
                    delta: format!("r-{i}"),
                    step_number: 1,
                    run_id: 1,
                })
                .await;
        }
        buffered
            .emit(AgentEvent::ReasoningChunk {
                session_id: "t".into(),
                message_id: "msg-r-1".into(),
                delta: "complete reasoning body".into(),
                step_number: 1,
                run_id: 1,
            })
            .await;

        for _ in 0..300 {
            let events = collector.events.lock().unwrap().clone();
            let has_full = events.iter().any(
                |e| matches!(e, AgentEvent::ReasoningChunk { delta, .. } if delta == "complete reasoning body"),
            );
            if has_full {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("full-text ReasoningChunk never delivered after overflow");
    }
}
