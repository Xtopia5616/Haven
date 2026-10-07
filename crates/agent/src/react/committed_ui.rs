//! Publish durable UI events from committed `session_events`.
//!
//! `SessionStore` broadcasts each row only after `COMMIT`. This publisher is
//! the only live projection for transcript rows that have a UI card: Thought,
//! ToolCall, Observation, Supplement, MediaPlan and Compaction. Callers may
//! invoke [`CommittedUiPublisher::publish`] after their own write returns, and
//! the process bridge may invoke it from the store subscription. Both paths
//! share one gate, so a commit produces each sequence once even if projection
//! later fails or the two callers overlap.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use haven_common::config::RequestKind;
use haven_memory::{SessionEvent, TRANSCRIPT_EVENT_TYPE};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::event::{AgentEvent, AgentEventEmitter, EventDispatcher};
use crate::types::TranscriptRecord;

const PUBLISHED_SEQUENCE_LIMIT: usize = 8192;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredToolCallUi {
    pub tool_name: String,
    pub tool_input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub step_id: String,
    pub tool_index: u32,
    #[serde(default)]
    pub suppress_streamed_thought: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredObservationUi {
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    pub step_id: String,
    pub tool_index: u32,
    #[serde(default)]
    pub silent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ask_options: Vec<String>,
    pub idempotency: String,
    pub operation_scope: String,
    pub renderer: String,
    pub result: haven_tools::ToolResultEnvelope,
}

/// UI card data that is not part of the canonical transcript record.
/// Stored beside the record in the same JSON object so a committed row is
/// sufficient to rebuild the live event after a process restart of the
/// publisher.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum CommittedUi {
    ToolCalls { cards: Vec<StoredToolCallUi> },
    Observation { card: Box<StoredObservationUi> },
    Supplement { supplement_id: String },
}

pub(super) fn encode_transcript_payload(
    record: &TranscriptRecord,
    ui: Option<&CommittedUi>,
) -> anyhow::Result<String> {
    let mut value = serde_json::to_value(record)?;
    if let Some(ui) = ui {
        let object = value
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("transcript record must serialize to an object"))?;
        object.insert("ui".to_string(), serde_json::to_value(ui)?);
    }
    Ok(serde_json::to_string(&value)?)
}

struct PublishState {
    done: HashSet<(String, i64)>,
    order: VecDeque<(String, i64)>,
    deferred_tool_calls: HashMap<(String, String), AgentEvent>,
    deferred_tool_call_order: VecDeque<(String, String)>,
    published_tool_calls: HashSet<(String, String)>,
    published_tool_call_order: VecDeque<(String, String)>,
}

pub(crate) struct CommittedUiPublisher {
    started: AtomicBool,
    gate: tokio::sync::Mutex<PublishState>,
}

impl CommittedUiPublisher {
    pub(crate) fn new() -> Self {
        Self {
            started: AtomicBool::new(false),
            gate: tokio::sync::Mutex::new(PublishState {
                done: HashSet::new(),
                order: VecDeque::new(),
                deferred_tool_calls: HashMap::new(),
                deferred_tool_call_order: VecDeque::new(),
                published_tool_calls: HashSet::new(),
                published_tool_call_order: VecDeque::new(),
            }),
        }
    }

    pub(crate) fn try_mark_started(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Emit the UI projections for these committed rows, in order. ToolCall
    /// ToolCall cards are retained until execution asks to publish each one.
    ///
    /// The gate is held across emit so a store subscriber and the committing
    /// task cannot reorder or duplicate a sequence. `AgentEventEmitter`
    /// implementations must not call back into this publisher.
    pub(crate) async fn publish(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        events: &[SessionEvent],
    ) {
        let mut state = self.gate.lock().await;
        for event in events {
            if event.sequence <= 0 || event.event_type != TRANSCRIPT_EVENT_TYPE {
                continue;
            }
            let key = (event.session_id.clone(), event.sequence);
            if state.done.contains(&key) {
                continue;
            }
            let ui_events = agent_events_from_committed(event);
            if ui_events.is_empty() {
                continue;
            }
            for ui_event in ui_events {
                if let AgentEvent::ToolCall {
                    session_id,
                    step_id,
                    ..
                } = &ui_event
                {
                    let tool_call_key = (session_id.clone(), step_id.clone());
                    if !state.published_tool_calls.contains(&tool_call_key)
                        && !state.deferred_tool_calls.contains_key(&tool_call_key)
                    {
                        remember_deferred_tool_call(&mut state, tool_call_key, ui_event);
                    }
                } else {
                    emitter.emit(ui_event).await;
                }
            }
            remember_published(&mut state, key);
        }
    }

    /// Publish one ToolCall card after its tool is admitted to execution (or
    /// immediately before an admission/cancellation result is committed).
    /// The card retains the sequence of the committed ToolCall row.
    pub(crate) async fn publish_tool_call(
        &self,
        emitter: &Arc<dyn AgentEventEmitter>,
        session_id: &str,
        step_id: &str,
    ) {
        let mut state = self.gate.lock().await;
        let key = (session_id.to_string(), step_id.to_string());
        if state.published_tool_calls.contains(&key) {
            return;
        }
        let Some(event) = state.deferred_tool_calls.remove(&key) else {
            return;
        };
        emitter.emit(event).await;
        remember_published_tool_call(&mut state, key);
    }

    pub(crate) async fn run(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<SessionEvent>,
        events: Arc<EventDispatcher>,
    ) {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let Some(emitter) = events.emitter_arc() else {
                        tracing::warn!(
                            session_id = %event.session_id,
                            sequence = event.sequence,
                            "committed UI bridge has no emitter; the committing task must publish"
                        );
                        continue;
                    };
                    self.publish(&emitter, std::slice::from_ref(&event)).await;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "committed UI bridge lagged; the committing task still publishes its batch"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
}

fn remember_deferred_tool_call(state: &mut PublishState, key: (String, String), event: AgentEvent) {
    state.deferred_tool_call_order.push_back(key.clone());
    state.deferred_tool_calls.insert(key, event);
    while state.deferred_tool_call_order.len() > PUBLISHED_SEQUENCE_LIMIT {
        if let Some(old) = state.deferred_tool_call_order.pop_front() {
            state.deferred_tool_calls.remove(&old);
        }
    }
}

fn remember_published_tool_call(state: &mut PublishState, key: (String, String)) {
    state.published_tool_call_order.push_back(key.clone());
    state.published_tool_calls.insert(key);
    while state.published_tool_call_order.len() > PUBLISHED_SEQUENCE_LIMIT {
        if let Some(old) = state.published_tool_call_order.pop_front() {
            state.published_tool_calls.remove(&old);
        }
    }
}

fn remember_published(state: &mut PublishState, key: (String, i64)) {
    state.order.push_back(key.clone());
    state.done.insert(key);
    while state.order.len() > PUBLISHED_SEQUENCE_LIMIT {
        if let Some(old) = state.order.pop_front() {
            state.done.remove(&old);
        }
    }
}

pub(super) fn agent_events_from_committed(event: &SessionEvent) -> Vec<AgentEvent> {
    if event.event_type != TRANSCRIPT_EVENT_TYPE || event.sequence <= 0 {
        return Vec::new();
    }
    let value: Value = match serde_json::from_str(&event.payload) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(
                session_id = %event.session_id,
                sequence = event.sequence,
                %error,
                "committed transcript payload is not JSON; skipping UI publish"
            );
            return Vec::new();
        }
    };
    let record: TranscriptRecord = match serde_json::from_value(value.clone()) {
        Ok(record) => record,
        Err(error) => {
            tracing::warn!(
                session_id = %event.session_id,
                sequence = event.sequence,
                %error,
                "committed transcript payload does not decode; skipping UI publish"
            );
            return Vec::new();
        }
    };
    let ui =
        value
            .get("ui")
            .cloned()
            .and_then(|ui| match serde_json::from_value::<CommittedUi>(ui) {
                Ok(ui) => Some(ui),
                Err(error) => {
                    tracing::warn!(
                        session_id = %event.session_id,
                        sequence = event.sequence,
                        %error,
                        "committed transcript UI annotation does not decode"
                    );
                    None
                }
            });
    let event_seq = u64::try_from(event.sequence).ok();
    let run_id = event.run_id.unwrap_or(0);
    let session_id = event.session_id.clone();
    match record {
        TranscriptRecord::Thought {
            step_number,
            text,
            message_id,
        } => vec![AgentEvent::Thought {
            session_id,
            thought: text,
            step_number,
            run_id,
            message_id,
            event_seq,
        }],
        TranscriptRecord::Reasoning { .. } => Vec::new(),
        TranscriptRecord::ToolCall { step_number, .. } => match ui {
            Some(CommittedUi::ToolCalls { cards }) => cards
                .into_iter()
                .map(|card| AgentEvent::ToolCall {
                    session_id: session_id.clone(),
                    tool_name: card.tool_name,
                    input: card.tool_input,
                    step_number,
                    run_id,
                    tool_call_id: card.tool_call_id,
                    tool_index: card.tool_index,
                    step_id: card.step_id,
                    suppress_streamed_thought: card.suppress_streamed_thought,
                    event_seq,
                })
                .collect(),
            _ => Vec::new(),
        },
        TranscriptRecord::ToolResult {
            step_number,
            history_observation,
            ..
        } => match ui {
            Some(CommittedUi::Observation { card }) => vec![AgentEvent::Observation {
                session_id,
                observation: history_observation,
                tool_name: card.tool_name,
                step_number,
                run_id,
                silent: card.silent,
                tool_call_id: card.tool_call_id,
                tool_index: card.tool_index,
                ask_options: card.ask_options,
                step_id: card.step_id,
                idempotency: card.idempotency,
                operation_scope: card.operation_scope,
                renderer: card.renderer,
                result: card.result,
                event_seq,
            }],
            _ => Vec::new(),
        },
        TranscriptRecord::UserInject {
            step_number,
            source,
            text,
            message_id,
            ..
        } => {
            let supplement_id = match ui {
                Some(CommittedUi::Supplement { supplement_id }) => supplement_id,
                _ => message_id
                    .clone()
                    .unwrap_or_else(|| format!("msg-seq-{}", event.sequence)),
            };
            vec![AgentEvent::Supplement {
                session_id,
                additional_context: text,
                step_number,
                run_id,
                message_id,
                supplement_id,
                inject_source: Some(source),
                event_seq,
            }]
        }
        TranscriptRecord::MediaPlan {
            step_number,
            strategy,
            projections,
            notices,
            ..
        } => vec![AgentEvent::MediaPlan {
            session_id,
            step_number,
            run_id,
            role: RequestKind::Chat,
            strategy,
            projections,
            notices,
            event_seq,
        }],
        TranscriptRecord::CompactSummary {
            summary,
            tokens_before,
            tokens_after,
            episode_id,
            degraded,
            ..
        } => vec![AgentEvent::Compaction {
            session_id,
            summary,
            tokens_before,
            tokens_after,
            episode_id: Some(episode_id),
            degraded,
            event_seq,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEventEmitter;
    use async_trait::async_trait;
    use haven_memory::{Database, SessionStore};
    use std::sync::Mutex;

    struct RecordingEmitter {
        events: Arc<Mutex<Vec<AgentEvent>>>,
    }

    #[async_trait]
    impl AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn thought_event(sequence: i64) -> SessionEvent {
        let record = TranscriptRecord::Thought {
            step_number: 2,
            text: "keep".into(),
            message_id: "step-keep".into(),
        };
        SessionEvent {
            session_id: "ses-1".into(),
            sequence,
            event_type: TRANSCRIPT_EVENT_TYPE.into(),
            event_version: 1,
            payload: encode_transcript_payload(&record, None).unwrap(),
            created_at: "2026-01-01T00:00:00Z".into(),
            run_id: Some(4),
            step_number: Some(2),
        }
    }

    fn tool_call_event(sequence: i64) -> SessionEvent {
        let record = TranscriptRecord::ToolCall {
            step_number: 2,
            text: "run both".into(),
            tool_calls: vec![
                haven_common::types::CanonicalToolCall {
                    id: "call-a".into(),
                    name: "tool_a".into(),
                    arguments: serde_json::json!({}),
                },
                haven_common::types::CanonicalToolCall {
                    id: "call-b".into(),
                    name: "tool_b".into(),
                    arguments: serde_json::json!({}),
                },
            ],
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let ui = CommittedUi::ToolCalls {
            cards: vec![
                StoredToolCallUi {
                    tool_name: "tool_a".into(),
                    tool_input: serde_json::json!({}),
                    tool_call_id: Some("call-a".into()),
                    step_id: "step-a".into(),
                    tool_index: 0,
                    suppress_streamed_thought: false,
                },
                StoredToolCallUi {
                    tool_name: "tool_b".into(),
                    tool_input: serde_json::json!({}),
                    tool_call_id: Some("call-b".into()),
                    step_id: "step-b".into(),
                    tool_index: 1,
                    suppress_streamed_thought: false,
                },
            ],
        };
        SessionEvent {
            session_id: "ses-1".into(),
            sequence,
            event_type: TRANSCRIPT_EVENT_TYPE.into(),
            event_version: 1,
            payload: encode_transcript_payload(&record, Some(&ui)).unwrap(),
            created_at: "2026-01-01T00:00:00Z".into(),
            run_id: Some(4),
            step_number: Some(2),
        }
    }

    #[tokio::test]
    async fn publish_is_once_per_sequence_and_keeps_the_durable_seq() {
        let publisher = CommittedUiPublisher::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(RecordingEmitter {
            events: seen.clone(),
        });
        let event = thought_event(9);
        publisher
            .publish(&emitter, std::slice::from_ref(&event))
            .await;
        publisher
            .publish(&emitter, std::slice::from_ref(&event))
            .await;
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AgentEvent::Thought {
                thought,
                event_seq: Some(9),
                message_id,
                ..
            } if thought == "keep" && message_id == "step-keep"
        ));
    }

    #[tokio::test]
    async fn tool_call_cards_publish_individually_when_requested() {
        let publisher = CommittedUiPublisher::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let emitter: Arc<dyn AgentEventEmitter> = Arc::new(RecordingEmitter {
            events: seen.clone(),
        });
        let event = tool_call_event(10);

        publisher
            .publish(&emitter, std::slice::from_ref(&event))
            .await;
        assert!(seen.lock().unwrap().is_empty());

        publisher
            .publish_tool_call(&emitter, "ses-1", "step-a")
            .await;
        publisher
            .publish_tool_call(&emitter, "ses-1", "step-a")
            .await;
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            AgentEvent::ToolCall {
                tool_name,
                step_id,
                event_seq: Some(10),
                ..
            } if tool_name == "tool_a" && step_id == "step-a"
        ));

        publisher
            .publish_tool_call(&emitter, "ses-1", "step-b")
            .await;
        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[1],
            AgentEvent::ToolCall {
                tool_name,
                step_id,
                event_seq: Some(10),
                ..
            } if tool_name == "tool_b" && step_id == "step-b"
        ));
    }

    #[test]
    fn ui_annotation_does_not_break_transcript_decode() {
        let record = TranscriptRecord::Thought {
            step_number: 1,
            text: "x".into(),
            message_id: "step-x".into(),
        };
        let ui = CommittedUi::Supplement {
            supplement_id: "msg-1".into(),
        };
        let payload = encode_transcript_payload(&record, Some(&ui)).unwrap();
        let decoded: TranscriptRecord = serde_json::from_str(&payload).unwrap();
        assert!(matches!(decoded, TranscriptRecord::Thought { text, .. } if text == "x"));
    }

    #[tokio::test]
    async fn bridge_publishes_a_store_commit_once() {
        let dir =
            std::env::temp_dir().join(format!("haven_committed_ui_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t").unwrap();
        let store = SessionStore::new(db);
        let publisher = Arc::new(CommittedUiPublisher::new());
        let dispatcher = Arc::new(EventDispatcher::new());
        let seen = Arc::new(Mutex::new(Vec::new()));
        dispatcher.set_emitter(Arc::new(RecordingEmitter {
            events: seen.clone(),
        }));
        let rx = store.subscribe();
        let bridge = Arc::clone(&publisher);
        let bridge_dispatcher = Arc::clone(&dispatcher);
        tokio::spawn(async move {
            bridge.run(rx, bridge_dispatcher).await;
        });
        let record = TranscriptRecord::Thought {
            step_number: 1,
            text: "from-store".into(),
            message_id: "step-store".into(),
        };
        let payload = encode_transcript_payload(&record, None).unwrap();
        let committed = store
            .append_transcript(&session.id, &payload, 3, 1)
            .unwrap();
        let emitter = dispatcher.emitter_arc().expect("test emitter");
        publisher
            .publish(&emitter, std::slice::from_ref(&committed))
            .await;
        for _ in 0..50 {
            if seen.lock().unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let events = seen.lock().unwrap().clone();
        assert_eq!(
            events.len(),
            1,
            "store bridge and direct publish must dedup"
        );
        assert!(matches!(
            &events[0],
            AgentEvent::Thought {
                thought,
                event_seq: Some(sequence),
                ..
            } if thought == "from-store" && *sequence == committed.sequence as u64
        ));
    }
}
