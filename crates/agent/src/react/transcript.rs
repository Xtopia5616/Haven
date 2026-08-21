//! Append-only transcript events and a unified `apply` (Phase 6 / B1-2 + H1).
//!
//! **Authority (B1-1):** `canonical` is the sole LLM transcript. `history`
//! (`Vec<ReActStep>`) is a derived debug / tool-restore projection updated
//! alongside `canonical` — business logic must not key LLM context off it.
//!
//! `apply` order is always: persist (row before card) → emit UI event →
//! project into `canonical` (+ derived `history` when applicable).
//!
//! Tool-batch Action/Observation cards may still be emitted by the caller
//! before `apply` (complex confirm/ask interleaving); those paths pass
//! `cards_already_emitted` / `already_emitted` so apply only projects.

use super::*;
use haven_common::types::InjectSource;
use haven_common::types::{CanonicalToolCall, MessageAttachment};

/// Append-only transcript event projected to canonical + UI.
#[derive(Debug, Clone)]
pub(super) enum TranscriptEvent {
    /// Assistant thought text for a step (content authority in `messages`).
    Thought {
        text: String,
        message_id: String,
    },
    /// Assistant message carrying tool calls (and optional thought text).
    ToolCall {
        text: String,
        tool_calls: Vec<CanonicalToolCall>,
        reasoning: Option<String>,
        web_search_calls: Vec<serde_json::Value>,
        thinking_blocks: Vec<serde_json::Value>,
    },
    /// Tool observation / result for one call — projects canonical + history.
    ToolResult {
        /// Text stored on the canonical tool message (raw step result).
        canonical_observation: String,
        /// Text recorded on the derived history step (may be display-shaped).
        history_observation: String,
        tool_call_id: Option<String>,
        action: Action,
    },
    /// User inject (steering / follow-up / answer / cross-session).
    UserInject {
        source: InjectSource,
        text: String,
        attachments: Vec<MessageAttachment>,
        message_id: Option<String>,
    },
    /// Compaction summary bubble (canonical projection only; episode write
    /// stays in the compactor path). Reserved for migrating compact off
    /// direct canonical splice (Phase 6.1).
    #[allow(dead_code)]
    CompactSummary {
        summary: String,
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
            } => {
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
            } => {
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
                let prefix = source.render_prefix();
                let mut content = vec![ContentPart::text(format!("{prefix}: {text}"))];
                content.extend(attachments.iter().map(attachment_to_content_part));
                canonical.push(CanonicalMessage::user_with_source(content, source));
            }
            TranscriptEvent::CompactSummary { summary } => {
                canonical.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(summary)],
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                ));
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

    fn step_ctx(session_id: &str) -> StepCtx {
        StepCtx {
            session_id: session_id.to_string(),
            step_num: 1,
            run_id: 1,
            emitter: Arc::new(NoopEmitter),
        }
    }

    #[tokio::test]
    async fn apply_user_inject_sets_source_and_prefix() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_inject_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let router = Arc::new(haven_llm::LlmRouter::new(
            haven_common::config::RouterConfig::default(),
        ));
        let executor = Arc::new(crate::session::SessionExecutor::new(
            db.clone(),
            Arc::new(haven_tools::ToolsManager::new()),
            2,
        ));
        let engine = ReActEngine::new(
            router,
            executor,
            db,
            10,
            haven_common::config::ContextLimitsConfig::default(),
        );
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
    async fn apply_thought_appends_derived_history() {
        let dir = std::env::temp_dir().join(format!(
            "haven_transcript_thought_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Arc::new(Database::open(&dir).unwrap());
        let session = db.create_session("t", "hi").unwrap();
        let router = Arc::new(haven_llm::LlmRouter::new(
            haven_common::config::RouterConfig::default(),
        ));
        let executor = Arc::new(crate::session::SessionExecutor::new(
            db.clone(),
            Arc::new(haven_tools::ToolsManager::new()),
            2,
        ));
        let engine = ReActEngine::new(
            router,
            executor,
            db,
            10,
            haven_common::config::ContextLimitsConfig::default(),
        );
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
}
