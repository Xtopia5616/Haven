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
}

#[async_trait]
pub trait AgentEventEmitter: Send + Sync {
    async fn emit(&self, event: AgentEvent);
}

type ChunkSender = tokio::sync::mpsc::Sender<(String, String, u32, u64)>;
type ConsumerHandle = Option<tokio::task::JoinHandle<()>>;

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
        let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel(1024);
        let (reasoning_tx, mut reasoning_rx) = tokio::sync::mpsc::channel(1024);

        let consumer_handle = {
            let em_clone = emitter.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        val = chunk_rx.recv() => {
                            match val {
                                Some((tid, delta, sn, rid)) => {
                                    em_clone.emit(AgentEvent::ThoughtChunk {
                                        task_id: tid, delta, step_number: sn, run_id: rid,
                                    }).await;
                                }
                                None => break,
                            }
                        }
                        val = reasoning_rx.recv() => {
                            match val {
                                Some((tid, delta, sn, rid)) => {
                                    em_clone.emit(AgentEvent::ReasoningChunk {
                                        task_id: tid, delta, step_number: sn, run_id: rid,
                                    }).await;
                                }
                                None => break,
                            }
                        }
                    }
                }
                while let Some((tid, delta, sn, rid)) = chunk_rx.recv().await {
                    em_clone.emit(AgentEvent::ThoughtChunk {
                        task_id: tid, delta, step_number: sn, run_id: rid,
                    }).await;
                }
                while let Some((tid, delta, sn, rid)) = reasoning_rx.recv().await {
                    em_clone.emit(AgentEvent::ReasoningChunk {
                        task_id: tid, delta, step_number: sn, run_id: rid,
                    }).await;
                }
            })
        };

        (chunk_tx, reasoning_tx, Some(consumer_handle))
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
