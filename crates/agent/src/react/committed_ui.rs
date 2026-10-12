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
use haven_common::tools::{OperationIdempotency, ToolOperationScope, ToolResultEnvelope};
use haven_memory::{SessionEvent, TRANSCRIPT_EVENT_TYPE};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::event::{AgentEvent, AgentEventEmitter, EventDispatcher};
use crate::types::{TranscriptRecord, ensure_canonical_id};

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
    pub idempotency: OperationIdempotency,
    pub operation_scope: ToolOperationScope,
    pub renderer: String,
    pub result: ToolResultEnvelope,
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

impl CommittedUi {
    fn validate_for_record(&self, record: &TranscriptRecord) -> anyhow::Result<()> {
        match self {
            Self::ToolCalls { cards } => {
                anyhow::ensure!(
                    matches!(record, TranscriptRecord::ToolCall { .. }),
                    "tool-call UI annotation must accompany a tool_call record"
                );
                for card in cards {
                    ensure_canonical_id(&card.step_id, "step", "ui.tool_calls[].step_id")?;
                }
            }
            Self::Observation { card } => {
                let TranscriptRecord::ToolResult { step_id, .. } = record else {
                    anyhow::bail!("observation UI annotation must accompany a tool_result record");
                };
                ensure_canonical_id(&card.step_id, "step", "ui.observation.step_id")?;
                anyhow::ensure!(
                    &card.step_id == step_id,
                    "ui.observation.step_id must match tool_result.step_id"
                );
                for asset_id in &card.result.assets {
                    ensure_canonical_id(asset_id, "asset", "ui.observation.result.assets[]")?;
                }
            }
            Self::Supplement { supplement_id } => {
                let TranscriptRecord::UserInject { message_id, .. } = record else {
                    anyhow::bail!("supplement UI annotation must accompany a user_inject record");
                };
                ensure_canonical_id(supplement_id, "msg", "ui.supplement.supplement_id")?;
                if let Some(message_id) = message_id {
                    anyhow::ensure!(
                        supplement_id == message_id,
                        "ui.supplement.supplement_id must match user_inject.message_id"
                    );
                }
            }
        }
        Ok(())
    }
}

pub(super) fn encode_transcript_payload(
    record: &TranscriptRecord,
    ui: Option<&CommittedUi>,
) -> anyhow::Result<String> {
    record.validate_identity()?;
    if let Some(ui) = ui {
        ui.validate_for_record(record)?;
    }
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
        Err(_) => {
            tracing::warn!(
                session_id = %event.session_id,
                sequence = event.sequence,
                failure_kind = "invalid_transcript_json",
                "committed transcript payload is not JSON; skipping UI publish"
            );
            return Vec::new();
        }
    };
    let record: TranscriptRecord = match serde_json::from_value(value.clone()) {
        Ok(record) => record,
        Err(_) => {
            tracing::warn!(
                session_id = %event.session_id,
                sequence = event.sequence,
                failure_kind = "invalid_transcript_record",
                "committed transcript payload does not decode; skipping UI publish"
            );
            return Vec::new();
        }
    };
    if record.validate_identity().is_err() {
        tracing::warn!(
            session_id = %event.session_id,
            sequence = event.sequence,
            failure_kind = "invalid_transcript_identity",
            "committed transcript identity is invalid; skipping UI publish"
        );
        return Vec::new();
    }
    let ui = match value.get("ui").cloned() {
        Some(ui_value) => match serde_json::from_value::<CommittedUi>(ui_value) {
            Ok(ui) => {
                if ui.validate_for_record(&record).is_err() {
                    tracing::warn!(
                        session_id = %event.session_id,
                        sequence = event.sequence,
                        failure_kind = "invalid_transcript_ui_identity",
                        "committed transcript UI identity is invalid; skipping UI publish"
                    );
                    return Vec::new();
                }
                Some(ui)
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %event.session_id,
                    sequence = event.sequence,
                    failure_kind = "invalid_transcript_ui_annotation",
                    "committed transcript UI annotation does not decode"
                );
                None
            }
        },
        None => None,
    };
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
                    .unwrap_or_else(|| haven_common::types::new_id("msg")),
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

    const STEP_ID_1: &str = "step-00000000000000000000000000000001";
    const STEP_ID_2: &str = "step-00000000000000000000000000000002";
    const MESSAGE_ID_1: &str = "msg-00000000000000000000000000000001";

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
            message_id: STEP_ID_1.into(),
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
                    step_id: STEP_ID_1.into(),
                    tool_index: 0,
                    suppress_streamed_thought: false,
                },
                StoredToolCallUi {
                    tool_name: "tool_b".into(),
                    tool_input: serde_json::json!({}),
                    tool_call_id: Some("call-b".into()),
                    step_id: STEP_ID_2.into(),
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
            } if thought == "keep" && message_id == STEP_ID_1
        ));
    }

    #[test]
    fn supplement_without_a_message_id_gets_a_canonical_fallback_identity() {
        let record = TranscriptRecord::UserInject {
            step_number: 3,
            source: haven_common::types::InjectSource::Steering,
            text: "steer the current turn".into(),
            media_inputs: Vec::new(),
            message_id: None,
        };
        let event = SessionEvent {
            session_id: "ses-00000000000000000000000000000001".into(),
            sequence: 17,
            event_type: TRANSCRIPT_EVENT_TYPE.into(),
            event_version: 1,
            payload: encode_transcript_payload(&record, None).unwrap(),
            created_at: "2026-01-01T00:00:00Z".into(),
            run_id: Some(4),
            step_number: Some(3),
        };

        let events = agent_events_from_committed(&event);
        assert!(matches!(
            events.as_slice(),
            [AgentEvent::Supplement {
                message_id: None,
                supplement_id,
                ..
            }] if haven_common::types::is_canonical_id(supplement_id, "msg")
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
            .publish_tool_call(&emitter, "ses-1", STEP_ID_1)
            .await;
        publisher
            .publish_tool_call(&emitter, "ses-1", STEP_ID_1)
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
            } if tool_name == "tool_a" && step_id == STEP_ID_1
        ));

        publisher
            .publish_tool_call(&emitter, "ses-1", STEP_ID_2)
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
            } if tool_name == "tool_b" && step_id == STEP_ID_2
        ));
    }

    #[test]
    fn ui_annotation_does_not_break_transcript_decode() {
        let record = TranscriptRecord::UserInject {
            step_number: 1,
            source: haven_common::types::InjectSource::FollowUp,
            text: "x".into(),
            media_inputs: Vec::new(),
            message_id: Some(MESSAGE_ID_1.into()),
        };
        let ui = CommittedUi::Supplement {
            supplement_id: MESSAGE_ID_1.into(),
        };
        let payload = encode_transcript_payload(&record, Some(&ui)).unwrap();
        let decoded: TranscriptRecord = serde_json::from_str(&payload).unwrap();
        assert!(matches!(decoded, TranscriptRecord::UserInject { text, .. } if text == "x"));
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
            message_id: STEP_ID_1.into(),
        };
        let payload = encode_transcript_payload(&record, None).unwrap();
        let committed = store
            .append_transcript(&session.session_id, &payload, 3, 1)
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

    #[test]
    fn invalid_transcript_identity_is_not_published() {
        let mut event = thought_event(20);
        event.payload =
            r#"{"type":"thought","step_number":2,"text":"bad","message_id":"step-invalid"}"#.into();
        assert!(agent_events_from_committed(&event).is_empty());
    }

    #[test]
    fn invalid_committed_ui_identity_is_not_published() {
        let mut event = tool_call_event(21);
        let mut payload: Value = serde_json::from_str(&event.payload).unwrap();
        payload["ui"]["cards"][0]["step_id"] = Value::String("step-invalid".into());
        event.payload = serde_json::to_string(&payload).unwrap();
        assert!(agent_events_from_committed(&event).is_empty());
    }

    #[test]
    fn transcript_writer_rejects_noncanonical_ids() {
        let record = TranscriptRecord::Thought {
            step_number: 1,
            text: "bad".into(),
            message_id: "msg-00000000000000000000000000000001".into(),
        };
        assert!(record.to_payload().is_err());
    }

    #[test]
    fn observation_ui_reuses_tool_result_step_and_validates_assets() {
        let record = TranscriptRecord::ToolResult {
            step_number: 1,
            tool_index: 0,
            step_id: STEP_ID_1.into(),
            canonical_observation: "done".into(),
            history_observation: "done".into(),
            tool_call_id: Some("provider/call-id".into()),
            tool_call: crate::types::ToolCall {
                tool_name: "media".into(),
                tool_input: serde_json::json!({}),
                is_final: false,
                tool_call_id: Some("provider/call-id".into()),
            },
        };
        let mut result = ToolResultEnvelope::default();
        result
            .assets
            .push("asset-00000000000000000000000000000001".into());
        let ui = CommittedUi::Observation {
            card: Box::new(StoredObservationUi {
                tool_name: "media".into(),
                tool_call_id: Some("provider/call-id".into()),
                step_id: STEP_ID_1.into(),
                tool_index: 0,
                silent: false,
                ask_options: Vec::new(),
                idempotency: OperationIdempotency::Unknown,
                operation_scope: ToolOperationScope::Session,
                renderer: "media".into(),
                result,
            }),
        };
        ui.validate_for_record(&record).unwrap();

        let mut bad_step = ui.clone();
        let CommittedUi::Observation { card } = &mut bad_step else {
            unreachable!()
        };
        card.step_id = STEP_ID_2.into();
        assert!(bad_step.validate_for_record(&record).is_err());

        let mut bad_asset = ui;
        let CommittedUi::Observation { card } = &mut bad_asset else {
            unreachable!()
        };
        card.result.assets[0] = "asset-invalid".into();
        assert!(bad_asset.validate_for_record(&record).is_err());
    }
}
