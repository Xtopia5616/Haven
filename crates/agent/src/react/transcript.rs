//! Append-only transcript events and a unified `apply` (Phase 6 / B1-2 + H1;
//! Phase 8 / B1-3: events are the snapshot authority; canonical is a
//! projection cache updated here).
//!
//! # X12 projection contract
//!
//! Events are the sole append-only authority. `messages` / `session_steps` are
//! materialized projections written from [`ReActEngine::apply_transcript`]
//! (or the shared [`ReActEngine::project_chat_message`] helper it owns).
//!
//! `apply` order is always: **project row → emit UI event → append
//! [`TranscriptRecord`] → project into `canonical`**.
//!
//! Exceptions (documented, not parallel authorities):
//! - **Ingress user seed**: `layer`/`ingress` may insert the user `messages`
//!   row before `UserInject` is applied (crash-safe queue). `UserInject` with
//!   `message_id` assumes that row already exists and only creates the
//!   shared-id thought step.
//! - **Error partials**: `persist_partial_on_error` writes recovery-only rows
//!   intentionally *outside* the event log so continue/rollback can truncate
//!   them via `last_msg_at` without replaying a failed step.
//! - **Terminal action-result**: no live loop left — history-only persist.

use super::*;
use crate::types::{Action, TranscriptRecord};
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
    pub action_index: u32,
    pub suppress_streamed_thought: bool,
}

/// Observation card emitted from [`TranscriptEvent::ToolResult`].
/// Display text is [`TranscriptEvent::ToolResult::history_observation`].
#[derive(Debug, Clone)]
pub(super) struct ObservationCard {
    pub tool_name: String,
    pub tool_call_id: Option<String>,
    pub step_id: String,
    pub action_index: u32,
    pub silent: bool,
    pub ask_options: Vec<String>,
}

/// Runtime transcript event (UI cards + serializable payload).
/// Converted to [`TranscriptRecord`] when appended to the event log.
#[derive(Debug, Clone)]
pub(super) enum TranscriptEvent {
    Thought {
        text: String,
        message_id: String,
    },
    /// Reasoning block → `messages` projection (type=`reasoning`) + event log.
    Reasoning {
        text: String,
        message_id: String,
    },
    ToolCall {
        text: String,
        tool_calls: Vec<CanonicalToolCall>,
        reasoning: Option<String>,
        web_search_calls: Vec<serde_json::Value>,
        thinking_blocks: Vec<serde_json::Value>,
        action_cards: Vec<ActionCard>,
        /// When `Some`, project `text` into `messages` under this id (final
        /// answer / synthetic text when Thought did not already project).
        persist_text_id: Option<String>,
    },
    ToolResult {
        canonical_observation: String,
        history_observation: String,
        tool_call_id: Option<String>,
        action: Action,
        action_index: u32,
        step_id: String,
        observation_card: Option<ObservationCard>,
    },
    UserInject {
        source: InjectSource,
        text: String,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    },
    CompactSummary {
        compacted: Vec<CanonicalMessage>,
        summary: String,
        tokens_before: u32,
        tokens_after: u32,
        episode_id: String,
        degraded: bool,
    },
}

impl TranscriptEvent {
    fn to_record(&self, step_number: u32) -> TranscriptRecord {
        match self {
            Self::Thought { text, message_id } => TranscriptRecord::Thought {
                step_number,
                text: text.clone(),
                message_id: message_id.clone(),
            },
            Self::Reasoning { text, message_id } => TranscriptRecord::Reasoning {
                step_number,
                text: text.clone(),
                message_id: message_id.clone(),
            },
            Self::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                ..
            } => TranscriptRecord::ToolCall {
                step_number,
                text: text.clone(),
                tool_calls: tool_calls.clone(),
                reasoning: reasoning.clone(),
                web_search_calls: web_search_calls.clone(),
                thinking_blocks: thinking_blocks.clone(),
            },
            Self::ToolResult {
                canonical_observation,
                history_observation,
                tool_call_id,
                action,
                action_index,
                step_id,
                ..
            } => TranscriptRecord::ToolResult {
                step_number,
                action_index: *action_index,
                step_id: step_id.clone(),
                canonical_observation: canonical_observation.clone(),
                history_observation: history_observation.clone(),
                tool_call_id: tool_call_id.clone(),
                action: action.clone(),
            },
            Self::UserInject {
                source,
                text,
                attachments,
                message_id,
            } => TranscriptRecord::UserInject {
                step_number,
                source: *source,
                text: text.clone(),
                attachments: attachments.clone(),
                message_id: message_id.clone(),
            },
            Self::CompactSummary {
                compacted,
                summary,
                tokens_before,
                tokens_after,
                episode_id,
                degraded,
            } => TranscriptRecord::CompactSummary {
                compacted: compacted.clone(),
                summary: summary.clone(),
                tokens_before: *tokens_before,
                tokens_after: *tokens_after,
                episode_id: episode_id.clone(),
                degraded: *degraded,
            },
        }
    }
}

impl ReActEngine {
    /// Project → emit → append record → project into canonical cache.
    ///
    /// All durable assistant/thought/ask/reasoning chat rows for the ReAct
    /// loop are written here (X12). See module docs for the few documented
    /// exceptions (ingress seed, error partials, terminal action-result).
    pub(super) async fn apply_transcript(
        &self,
        ctx: &StepCtx,
        event: TranscriptEvent,
        state: &mut ReActState,
    ) {
        let record = event.to_record(ctx.step_num);
        match event {
            TranscriptEvent::Thought { text, message_id } => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    self.project_chat_message(
                        &ctx.session_id,
                        "assistant",
                        trimmed,
                        Some("text"),
                        None,
                        Some(&message_id),
                    )
                    .await;
                }
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
                state.events.push(record);
            }
            TranscriptEvent::Reasoning { text, message_id } => {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    self.project_chat_message(
                        &ctx.session_id,
                        "assistant",
                        trimmed,
                        Some("reasoning"),
                        None,
                        Some(&message_id),
                    )
                    .await;
                }
                state.events.push(record);
            }
            TranscriptEvent::ToolCall {
                text,
                tool_calls,
                reasoning,
                web_search_calls,
                thinking_blocks,
                action_cards,
                persist_text_id,
            } => {
                if let Some(ref mid) = persist_text_id {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        self.project_chat_message(
                            &ctx.session_id,
                            "assistant",
                            trimmed,
                            Some("text"),
                            None,
                            Some(mid),
                        )
                        .await;
                    }
                }
                for card in &action_cards {
                    self.executor
                        .begin_action_step_with_identity(
                            &ctx.session_id,
                            &card.tool_name,
                            &card.tool_input,
                            ctx.step_num,
                            card.action_index,
                            card.tool_call_id.as_deref(),
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
                            action_index: card.action_index,
                            suppress_streamed_thought: card.suppress_streamed_thought,
                        })
                        .await;
                }
                state.events.push(record);
                state.canonical.push(CanonicalMessage::assistant(
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
                action_index: _,
                step_id: _,
                observation_card,
            } => {
                if let Some(ref card) = observation_card {
                    // Ask question text is the messages projection under the
                    // shared step id (review card content authority).
                    if card.tool_name == "ask" {
                        let q = history_observation.trim();
                        if !q.is_empty() {
                            self.project_chat_message(
                                &ctx.session_id,
                                "assistant",
                                q,
                                Some("text"),
                                None,
                                Some(&card.step_id),
                            )
                            .await;
                        }
                    }
                    ctx.emitter
                        .emit(crate::event::AgentEvent::Observation {
                            session_id: ctx.session_id.clone(),
                            observation: history_observation.clone(),
                            tool_name: card.tool_name.clone(),
                            step_number: ctx.step_num,
                            run_id: ctx.run_id,
                            silent: card.silent,
                            tool_call_id: card.tool_call_id.clone(),
                            ask_options: card.ask_options.clone(),
                            step_id: card.step_id.clone(),
                            action_index: card.action_index,
                        })
                        .await;
                }
                state.events.push(record);
                let is_final = action.is_final || action.tool_name == "final_answer";
                if !is_final {
                    state.canonical.push(CanonicalMessage::tool(
                        vec![ContentPart::text(canonical_observation)],
                        tool_call_id,
                    ));
                }
            }
            TranscriptEvent::UserInject {
                source,
                text,
                attachments,
                message_id,
            } => {
                // Always notify the UI so auto-wake from a background action is
                // visible in-chat (not only a toast). ActionResult still skips
                // the thought-step DB write — it is producer-labelled context,
                // not a human steering/answer turn.
                ctx.emitter
                    .emit(crate::event::AgentEvent::Supplement {
                        session_id: ctx.session_id.clone(),
                        additional_context: text.clone(),
                        step_number: ctx.step_num,
                        run_id: ctx.run_id,
                        inject_source: Some(source),
                    })
                    .await;
                if source != InjectSource::ActionResult {
                    let step_id = message_id
                        .clone()
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
                state.events.push(record);
                let mut content = vec![ContentPart::text(text)];
                content.extend(attachments.iter().map(attachment_to_content_part));
                state
                    .canonical
                    .push(CanonicalMessage::user_with_source(content, source));
            }
            TranscriptEvent::CompactSummary {
                compacted,
                summary,
                tokens_before,
                tokens_after,
                episode_id,
                degraded,
            } => {
                // Replace the log with the CompactSummary root so pre-compaction
                // events (and embedded prior CompactSummaries) do not grow forever.
                state.replace_with_compaction(record, compacted);
                EventDispatcher::emit_compaction_from(
                    &ctx.emitter,
                    &ctx.session_id,
                    &summary,
                    tokens_before,
                    tokens_after,
                    &episode_id,
                    degraded,
                )
                .await;
                self.persist_compaction_summary(&ctx.session_id, &summary, &episode_id)
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::AgentEventEmitter;
    use crate::types::project_transcript;
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
    async fn apply_user_inject_sets_source_raw_text() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_inject_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::Steering,
                    text: "be brief".into(),
                    attachments: vec![],
                    message_id: None,
                },
                &mut state,
            )
            .await;
        assert_eq!(state.canonical.len(), 1);
        assert_eq!(state.canonical[0].source, Some(InjectSource::Steering));
        let text = match &state.canonical[0].content[0] {
            ContentPart::Text(t) => t.as_str(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, "be brief");
        assert_eq!(state.events.len(), 1);
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
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
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
                &mut state,
            )
            .await;
        assert_eq!(state.canonical[0].source, Some(InjectSource::ActionResult));
        let text = match &state.canonical[0].content[0] {
            ContentPart::Text(t) => t.as_str(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, body);
        assert!(!text.starts_with("Background action result: "));
    }

    #[tokio::test]
    async fn apply_action_result_emits_supplement_without_thought_step() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_action_supp_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db.clone());
        let mut ctx = step_ctx(&session.id);
        ctx.emitter = Arc::new(RecordingEmitter {
            events: recorded.clone(),
        });
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        let body = "[Background action result]\naction_id: act-9\nstatus: completed\n\nok";
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::UserInject {
                    source: InjectSource::ActionResult,
                    text: body.into(),
                    attachments: vec![],
                    message_id: None,
                },
                &mut state,
            )
            .await;
        let emitted = recorded.lock().unwrap().clone();
        assert!(
            emitted.iter().any(|e| matches!(
                e,
                crate::event::AgentEvent::Supplement {
                    inject_source: Some(InjectSource::ActionResult),
                    ..
                }
            )),
            "ActionResult must emit Supplement for in-chat wake visibility"
        );
        let steps = db.get_session_steps(&session.id).unwrap_or_default();
        assert!(
            steps.is_empty(),
            "ActionResult must not create a thought step"
        );
    }

    #[tokio::test]
    async fn apply_thought_appends_event_not_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_thought_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let mid = haven_common::types::new_id("step");
        let engine = test_engine(db.clone());
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Thought {
                    text: "thinking".into(),
                    message_id: mid.clone(),
                },
                &mut state,
            )
            .await;
        assert_eq!(state.events.len(), 1);
        let (_, rounds) = project_transcript(&state.events);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].thought.as_deref(), Some("thinking"));
        assert!(state.canonical.is_empty());
        // X12: Thought projects the messages row under the shared id.
        let msgs = db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.id == mid && m.content == "thinking" && m.role == "assistant"),
            "expected projected thought message, got {msgs:?}"
        );
    }

    #[tokio::test]
    async fn apply_reasoning_projects_message_not_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_reasoning_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let mid = haven_common::types::new_id("msg");
        let engine = test_engine(db.clone());
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Reasoning {
                    text: "why".into(),
                    message_id: mid.clone(),
                },
                &mut state,
            )
            .await;
        assert_eq!(state.events.len(), 1);
        assert!(matches!(
            &state.events[0],
            TranscriptRecord::Reasoning { message_id, .. } if message_id == &mid
        ));
        assert!(state.canonical.is_empty());
        let (canon, rounds) = project_transcript(&state.events);
        assert!(canon.is_empty());
        assert!(rounds.is_empty());
        let msgs = db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter().any(|m| {
                m.id == mid && m.content == "why" && m.message_type.as_deref() == Some("reasoning")
            }),
            "expected projected reasoning message, got {msgs:?}"
        );
    }

    #[tokio::test]
    async fn apply_compact_summary_replaces_canonical() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_compact_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let ui_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 2,
            run_id: 1,
            emitter: Arc::new(RecordingEmitter {
                events: ui_events.clone(),
            }),
        };
        let canonical = vec![
            CanonicalMessage::user_text("old1"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("old2")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ];
        let events = vec![TranscriptRecord::Thought {
            step_number: 1,
            text: "keep".into(),
            message_id: "step-keep".into(),
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
        let mut state = ReActState::new(events, canonical, std::collections::HashMap::new());
        state.branch_points.insert(
            1,
            BranchPoint {
                event_cursor: 1,
                step_number: 1,
                last_msg_at: None,
            },
        );
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::CompactSummary {
                    compacted: compacted.clone(),
                    summary: "prior turns".into(),
                    tokens_before: 100,
                    tokens_after: 40,
                    episode_id: haven_common::types::new_id("msg"),
                    degraded: false,
                },
                &mut state,
            )
            .await;
        assert_eq!(state.canonical.len(), 2);
        // CompactSummary replaces the event log (no pre-compaction growth).
        assert_eq!(state.events.len(), 1);
        assert!(matches!(
            &state.events[0],
            TranscriptRecord::CompactSummary { .. }
        ));
        assert!(state.branch_points.is_empty());
        let (_, rounds) = project_transcript(&state.events);
        assert!(rounds.is_empty());
        let ev = ui_events.lock().unwrap();
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
        let ui_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 3,
            run_id: 7,
            emitter: Arc::new(RecordingEmitter {
                events: ui_events.clone(),
            }),
        };
        let step_id = haven_common::types::new_id("step");
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
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
                        action_index: 0,
                        suppress_streamed_thought: false,
                    }],
                    persist_text_id: None,
                },
                &mut state,
            )
            .await;
        assert_eq!(state.canonical.len(), 1);
        assert_eq!(state.events.len(), 1);
        let ev = ui_events.lock().unwrap();
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
        let ui_events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let engine = test_engine(db);
        let ctx = StepCtx {
            session_id: session.id.clone(),
            step_num: 4,
            run_id: 2,
            emitter: Arc::new(RecordingEmitter {
                events: ui_events.clone(),
            }),
        };
        let step_id = haven_common::types::new_id("step");
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
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
                    action_index: 0,
                    step_id: step_id.clone(),
                    observation_card: Some(ObservationCard {
                        tool_name: "echo".into(),
                        tool_call_id: Some("call-2".into()),
                        step_id: step_id.clone(),
                        action_index: 0,
                        silent: false,
                        ask_options: vec![],
                    }),
                },
                &mut state,
            )
            .await;
        assert_eq!(state.canonical.len(), 1);
        let (_, rounds) = project_transcript(&state.events);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].tools[0].observation.as_deref(), Some("ok"));
        let ev = ui_events.lock().unwrap();
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

    #[tokio::test]
    async fn apply_parallel_tool_results_one_round() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_parallel_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let engine = test_engine(db);
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::Thought {
                    text: "both".into(),
                    message_id: haven_common::types::new_id("step"),
                },
                &mut state,
            )
            .await;
        for (id, name) in [("c1", "a"), ("c2", "b")] {
            engine
                .apply_transcript(
                    &ctx,
                    TranscriptEvent::ToolResult {
                        canonical_observation: format!("r{name}"),
                        history_observation: format!("r{name}"),
                        tool_call_id: Some(id.into()),
                        action: Action {
                            tool_name: name.into(),
                            tool_input: serde_json::json!({}),
                            is_final: false,
                            tool_call_id: Some(id.into()),
                        },
                        action_index: if id == "c1" { 0 } else { 1 },
                        step_id: format!("step-{id}"),
                        observation_card: None,
                    },
                    &mut state,
                )
                .await;
        }
        let (_, rounds) = project_transcript(&state.events);
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].tools.len(), 2);
        assert_eq!(state.canonical.len(), 2);
    }

    #[tokio::test]
    async fn apply_ask_tool_result_projects_question_under_step_id() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_ask_proj_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let step_id = haven_common::types::new_id("step");
        let engine = test_engine(db.clone());
        let ctx = step_ctx(&session.id);
        let mut state = ReActState::new(Vec::new(), Vec::new(), std::collections::HashMap::new());
        engine
            .apply_transcript(
                &ctx,
                TranscriptEvent::ToolResult {
                    canonical_observation: r#"{"question":"Pick one?"}"#.into(),
                    history_observation: "Pick one?".into(),
                    tool_call_id: Some("call-ask".into()),
                    action: Action {
                        tool_name: "ask".into(),
                        tool_input: serde_json::json!({"question":"Pick one?"}),
                        is_final: false,
                        tool_call_id: Some("call-ask".into()),
                    },
                    action_index: 0,
                    step_id: step_id.clone(),
                    observation_card: Some(ObservationCard {
                        tool_name: "ask".into(),
                        tool_call_id: Some("call-ask".into()),
                        step_id: step_id.clone(),
                        action_index: 0,
                        silent: false,
                        ask_options: vec!["A".into(), "B".into()],
                    }),
                },
                &mut state,
            )
            .await;
        let msgs = db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter()
                .any(|m| m.id == step_id && m.content == "Pick one?" && m.role == "assistant"),
            "ask question must project under shared step id, got {msgs:?}"
        );
    }
}
