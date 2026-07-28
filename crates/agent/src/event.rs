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
    FallbackActivated {
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
}

#[async_trait]
pub trait AgentEventEmitter: Send + Sync {
    async fn emit(&self, event: AgentEvent);
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
        let mut buf = String::new();
        buf.push_str(&delta);
        let mut buf_bytes = delta.len();
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
            emitter.emit(AgentEvent::TaskCompleted {
                task_id: task_id.into(),
                title: title.into(),
            }).await;
        }
    }

    pub async fn emit_task_updated(&self, task_id: &str, status: &str) {
        tracing::info!("emit_task_updated event: task={} status={}", task_id, status);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TaskUpdated {
                task_id: task_id.into(),
                status: status.into(),
            }).await;
        }
    }

    pub async fn emit_title_updated(&self, task_id: &str, title: &str) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TitleUpdated {
                task_id: task_id.into(),
                title: title.into(),
            }).await;
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
        tracing::info!("emit_thought: task={} step={} run={} thought_len={}", task_id, step_number, run_id, thought.len());
        let _ = db.create_thought_step(task_id, step_number as i32, thought);
        emitter.emit(AgentEvent::Thought {
            task_id: task_id.into(),
            thought: thought.into(),
            step_number,
            run_id,
        }).await;
    }

    pub async fn emit_fallback_activated_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        reason: &str,
    ) {
        emitter.emit(AgentEvent::FallbackActivated {
            task_id: task_id.into(),
            reason: reason.into(),
        }).await;
    }

    pub async fn emit_compaction_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        summary: &str,
        tokens_before: u32,
        tokens_after: u32,
    ) {
        emitter.emit(AgentEvent::Compaction {
            task_id: task_id.into(),
            summary: summary.into(),
            tokens_before,
            tokens_after,
        }).await;
    }

    pub async fn emit_task_error_from(
        emitter: &Arc<dyn AgentEventEmitter>,
        task_id: &str,
        error: &str,
    ) {
        emitter.emit(AgentEvent::TaskError {
            task_id: task_id.into(),
            error: error.into(),
        }).await;
    }
}
