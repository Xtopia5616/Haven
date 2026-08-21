//! Append-only transcript events and a unified `apply` (Phase 6 / B1-2 + H1;
//! Phase 6.1 wires CompactSummary + Action/Observation cards through apply).
//!
//! **Authority (B1-1):** `canonical` is the sole LLM transcript. `history`
//! (`Vec<ReActStep>`) is a derived debug / tool-restore projection updated
//! alongside `canonical` — business logic must not key LLM context off it.
//!
//! `apply` order is always: persist (row before card) → emit UI event →
//! project into `canonical` (+ derived `history` when applicable).

use super::*;
use haven_common::types::InjectSource;
use haven_common::types::{CanonicalToolCall, MessageAttachment};
use serde_json::Value;

/// Pending Action card (+ step row) emitted from [`TranscriptEvent::ToolCall`].
#[derive(Debug, Clone)]
pub(super) struct ActionCard {
    pub tool_name: String,
    pub tool_input: Value,
    pub tool_call_id: Option<String>,
    pub step_id: String,
}

/// Observation card emitted from [`TranscriptEvent::ToolResult`].
/// Display text is [`TranscriptEvent::ToolResult::history_observation`].
#[derive(Debug, Clone)]
pub(super) struct ObservationCard {
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub step_id: String,
    pub silent: bool,
    pub ask_options: Vec<String>,
}

/// Append-only transcript event projected to canonical + UI.
#[derive(Debug, Clone)]
pub(super) enum TranscriptEvent {
    /// Assistant thought text for a step (content authority in `messages`).
    Thought {
        text: String,
        message_id: String,
    },
    /// Assistant message carrying tool calls (and optional thought text).
    /// When `action_cards` is non-empty, also persists pending step rows and
    /// emits Action cards (row before card) before projecting canonical.
    ToolCall {
        text: String,
        tool_calls: Vec<CanonicalToolCall>,
        reasoning: Option<String>,
        web_search_calls: Vec<serde_json::Value>,
        thinking_blocks: Vec<serde_json::Value>,
        action_cards: Vec<ActionCard>,
    },
    /// Tool observation / result for one call — projects canonical + history.
    /// When `observation_card` is set, emits the Observation UI card first.
    ToolResult {
        /// Text stored on the canonical tool message (raw step result).
        canonical_observation: String,
        /// Text recorded on the derived history step (may be display-shaped).
        history_observation: String,
        tool_call_id: Option<String>,
        action: Action,
        observation_card: Option<ObservationCard>,
    },
    /// User inject (steering / follow-up / answer / cross-session).
    UserInject {
        source: InjectSource,
        text: String,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    },
    /// Compaction: replace `canonical` with the compacted list, emit the
    /// Compaction UI event, and persist the summary episode. Does not touch
    /// derived `history` (existing behavior).
    CompactSummary {
        compacted: Vec<CanonicalMessage>,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
    },
}

impl ReActEngine {
    /// Persist → emit → project. New transcript mutations go through here.
    pub(super) async fn apply_transcript(
        &self,
        ctx: &StepCtx,
        event: TranscriptEvent,
        canonical: &mut Vec<CanonicalMessage>,
        history: &mut Vec<ReActStep>,
    ) {
        match event {
            TranscriptEvent::Thought { text, message_id } => {
                EventDispatcher::emit_thought_from(
                    &ctx.emitter,
                    &ctx.session_id,
                    &text,
                    ctx.step_num,
                    ctx.run_id,
                    &message_id,
                    &self.db,
                )
                .await;
                history.push(ReActStep {
                    step_number: ctx.step_num,
                    thought: Some(text),
                    action: None,
                    observation: None,
                });
            }
            TranscriptEvent::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                action_cards,
            } => {
                for card in &action_cards {
                    self.executor
                        .begin_action_step(
                            &ctx.session_id,
                            &card.tool_name,
                            &card.tool_input,
                            ctx.step_num,
                            &card.step_id,
                        )
                        .await;
                    ctx.emitter
                        .emit(crate::event::AgentEvent::Action {
                            session_id: ctx.session_id.clone(),
                            tool_name: card.tool_name.clone(),
                            input: card.tool_input.clone(),
                            step_number: ctx.step_num,
                            run_id: ctx.run_id,
                            tool_call_id: card.tool_call_id.clone(),
                            step_id: card.step_id.clone(),
                        })
                        .await;
                }
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(text)],
                    if tool_calls.is_empty() {
                        None
                    } else {
                        Some(tool_calls)
                    },
                    reasoning,
                    web_search_calls,
                    thinking_blocks,
                ));
            }
            TranscriptEvent::ToolResult {
                canonical_observation,
                history_observation,
                tool_call_id,
                action,
                observation_card,
            } => {
                if let Some(card) = observation_card {
                    ctx.emitter
                        .emit(crate::event::AgentEvent::Observation {
                            session_id: ctx.session_id.clone(),
                            observation: history_observation.clone(),
                            tool_name: card.tool_name,
                            step_number: ctx.step_num,
                            run_id: ctx.run_id,
                            silent: card.silent,
                            tool_call_id: card.tool_call_id,
                            ask_options: card.ask_options,
                            step_id: card.step_id,
                        })
                        .await;
                }
                if let Some(last) = history
                    .last_mut()
                    .filter(|s| s.step_number == ctx.step_num && s.action.is_none())
                {
                    last.action = Some(action.clone());
                    last.observation = Some(history_observation);
                } else {
                    history.push(ReActStep {
                        step_number: ctx.step_num,
                        thought: None,
                        action: Some(action),
                        observation: Some(history_observation),
                    });
                }
                canonical.push(CanonicalMessage::tool(
                    vec![ContentPart::text(canonical_observation)],
                    tool_call_id,
                ));
            }
            TranscriptEvent::UserInject {
                source,
                text,
                attachments,
                message_id,
            } => {
                // ActionResult is runtime context only: keep InjectSource on
                // canonical, but do not emit Supplement UI or mint thought
                // steps (those are for user/steering/answer/cross-session).
                if source != InjectSource::ActionResult {
                    ctx.emitter
                        .emit(crate::event::AgentEvent::Supplement {
                            session_id: ctx.session_id.clone(),
                            additional_context: text.clone(),
                            step_number: ctx.step_num,
                            run_id: ctx.run_id,
                        })
                        .await;
                    let step_id = message_id
                        .map(String::from)
                        .unwrap_or_else(|| haven_common::types::new_id("step"));
                    let _ = self
                        .db
                        .run_blocking({
                            let session_id = ctx.session_id.clone();
                            let step_id = step_id.clone();
                            let step_num = ctx.step_num;
                            move |db| {
                                if let Err(e) =
                                    db.create_thought_step(&session_id, step_num as i32, &step_id)
                                {
                                    tracing::warn!(
                                        "create_thought_step failed (session={} step={}): {}",
                                        session_id,
                                        step_num,
                                        e
                                    );
                                }
                                Ok::<(), anyhow::Error>(())
                            }
                        })
                        .await;
                }
                // Action-result payloads are already self-labelled by the
                // producer (`[Background action result]…`); do not bake a
                // second prefix. Other injects keep `{prefix}: {text}`.
                let body = if source == InjectSource::ActionResult {
                    text
                } else {
                    format!("{}: {text}", source.render_prefix())
                };
                let mut content = vec![ContentPart::text(body)];
                content.extend(attachments.iter().map(attachment_to_content_part));
                canonical.push(CanonicalMessage::user_with_source(content, source));
            }
            TranscriptEvent::CompactSummary {
                compacted,
                summary,
                tokens_before,
                tokens_after,
            } => {
                *canonical = compacted;
                EventDispatcher::emit_compaction_from(
                    &ctx.emitter,
                    &ctx.session_id,
                    &summary,
                    tokens_before,
                    tokens_after,
                )
                .await;
                self.persist_compaction_summary(&ctx.session_id, &summary)
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEventEmitter;
    use async_trait::async_trait;
    use haven_memory::Database;
    use std::sync::Arc;

    struct NoopEmitter;
    #[async_trait]
    impl AgentEventEmitter for NoopEmitter {
        async fn emit(&self, _event: crate::event::AgentEvent) {}
    }

    struct RecordingEmitter {
        events: Arc<std::sync::Mutex<Vec<crate::event::AgentEvent>>>,
    }
    #[async_trait]
    impl AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: crate::event::AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    fn step_ctx(session_id: &str) -> StepCtx {
        StepCtx {
            session_id: session_id.to_string(),
            step_num: 1,
            run_id: 1,
            emitter: Arc::new(NoopEmitter),
        }
    }

    fn test_engine(db: Arc<Database>) -> ReActEngine {
        let router = Arc::new(haven_llm::LlmRouter::new(
            haven_common::config::RouterConfig::default(),
        ));
        let executor = Arc::new(crate::session::SessionExecutor::new(
            db.clone(),
            Arc::new(haven_tools::ToolsManager::new()),
            2,
        ));
        ReActEngine::new(
            router,
            executor,
            db,
            10,
            haven_common::config::ContextLimitsConfig::default(),
        )
    }

    #[tokio::test]
    async fn apply_user_inject_sets_source_and_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_inject_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut canonical = Vec::new();
        let mut history = Vec::new();
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::Steering,
                    text: "be brief".into(),
                    attachments: vec![],
                    message_id: None,
                },
                &mut canonical,
                &mut history,
            )
            .await;
        assert_eq!(canonical.len(), 1);
        assert_eq!(canonical[0].source, Some(InjectSource::Steering));
        let text = match &canonical[0].content[0] {
            ContentPart::Text(t) => t.as_str(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, "Steering: be brief");
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn apply_action_result_keeps_self_labelled_body() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_action_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut canonical = Vec::new();
        let mut history = Vec::new();
        let body = "[Background action result]\naction_id=act-1\nok";
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::ActionResult,
                    text: body.into(),
                    attachments: vec![],
                    message_id: None,
                },
                &mut canonical,
                &mut history,
            )
            .await;
        assert_eq!(canonical[0].source, Some(InjectSource::ActionResult));
        let text = match &canonical[0].content[0] {
            ContentPart::Text(t) => t.as_str(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, body);
        assert!(!text.starts_with("Background action result: "));
    }

    #[tokio::test]
    async fn apply_thought_appends_derived_history() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_thought_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut canonical = Vec::new();
        let mut history = Vec::new();
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Thought {
                    text: "thinking".into(),
                    message_id: haven_common::types::new_id("step"),
                },
                &mut canonical,
                &mut history,
            )
            .await;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].thought.as_deref(), Some("thinking"));
        assert!(canonical.is_empty());
    }

    #[tokio::test]
    async fn apply_compact_summary_replaces_canonical_and_emits() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_compact_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 2,
            run_id: 1,
            emitter: Arc::new(RecordingEmitter {
                events: events.clone(),
            }),
        };
        let mut canonical = vec![
            CanonicalMessage::user_text("old1"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("old2")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ];
        let mut history = vec![ReActStep {
            step_number: 1,
            thought: Some("keep".into()),
            action: None,
            observation: None,
        }];
        let compacted = vec![
            CanonicalMessage::assistant(
                vec![ContentPart::text("SUMMARY: prior turns")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::user_text("recent"),
        ];
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::CompactSummary {
                    compacted: compacted.clone(),
                    summary: "prior turns".into(),
                    tokens_before: 100,
                    tokens_after: 40,
                },
                &mut canonical,
                &mut history,
            )
            .await;
        assert_eq!(canonical.len(), 2);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].thought.as_deref(), Some("keep"));
        let ev = events.lock().unwrap();
        assert!(
            ev.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Compaction {
                    tokens_before: 100,
                    tokens_after: 40,
                    ..
                }
            )),
            "expected Compaction event, got {ev:?}"
        );
    }

    #[tokio::test]
    async fn apply_tool_call_emits_action_cards_then_projects() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_toolcall_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 3,
            run_id: 7,
            emitter: Arc::new(RecordingEmitter {
                events: events.clone(),
            }),
        };
        let step_id = haven_common::types::new_id("step");
        let mut canonical = Vec::new();
        let mut history = Vec::new();
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolCall {
                    text: "calling".into(),
                    tool_calls: vec![CanonicalToolCall {
                        id: "call-1".into(),
                        name: "echo".into(),
                        arguments: serde_json::json!({"x": 1}),
                    }],
                    reasoning: None,
                    web_search_calls: vec![],
                    thinking_blocks: vec![],
                    action_cards: vec![ActionCard {
                        tool_name: "echo".into(),
                        tool_input: serde_json::json!({"x": 1}),
                        tool_call_id: Some("call-1".into()),
                        step_id: step_id.clone(),
                    }],
                },
                &mut canonical,
                &mut history,
            )
            .await;
        assert_eq!(canonical.len(), 1);
        assert!(history.is_empty());
        let ev = events.lock().unwrap();
        assert!(
            ev.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Action {
                    tool_name,
                    step_id: sid,
                    ..
                } if tool_name == "echo" && sid == &step_id
            )),
            "expected Action card, got {ev:?}"
        );
    }

    #[tokio::test]
    async fn apply_tool_result_emits_observation_then_projects() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_toolresult_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 4,
            run_id: 2,
            emitter: Arc::new(RecordingEmitter {
                events: events.clone(),
            }),
        };
        let step_id = haven_common::types::new_id("step");
        let mut canonical = Vec::new();
        let mut history = Vec::new();
        let action = Action {
            tool_name: "echo".into(),
            tool_input: serde_json::json!({}),
            is_final: false,
            tool_call_id: Some("call-2".into()),
        };
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolResult {
                    canonical_observation: r#"{"ok":true}"#.into(),
                    history_observation: "ok".into(),
                    tool_call_id: Some("call-2".into()),
                    action,
                    observation_card: Some(ObservationCard {
                        tool_name: "echo".into(),
                        tool_call_id: Some("call-2".into()),
                        step_id: step_id.clone(),
                        silent: false,
                        ask_options: vec![],
                    }),
                },
                &mut canonical,
                &mut history,
            )
            .await;
        assert_eq!(canonical.len(), 1);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].observation.as_deref(), Some("ok"));
        let ev = events.lock().unwrap();
        assert!(
            ev.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Observation {
                    observation,
                    step_id: sid,
                    ..
                } if observation == "ok" && sid == &step_id
            )),
            "expected Observation card, got {ev:?}"
        );
    }
}
