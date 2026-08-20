use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use base64::Engine;

mod compactor;
mod event;
mod inference;
mod partial;
mod prompt;
mod react;
mod rollback;
mod session;
mod title;
mod types;

pub use compactor::ContextCompactor;
pub use event::{AgentEvent, AgentEventEmitter, BufferedEmitter, EventBus, EventDispatcher};
pub use inference::InferenceEngine;
pub use prompt::SystemPromptBuilder;
pub use react::ReActEngine;
pub use session::{
    RunHandler, SessionExecutor, SessionInfo, SessionStatus, StepInfo, ToolExecution,
};
pub use types::{Action, BranchPoint, ProcessResult, ReActSnapshot, ReActStep};

use haven_common::config::ContextLimitsConfig;
use haven_common::types::MessageAttachment;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::LlmRouter;
use haven_llm::media::{AttachmentOutcome, GenerateKind, GenerateOutcome, MediaDecision};
use haven_memory::Database;
use haven_memory::repositories::messages::Message;
use haven_tools::ScheduleMode;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::title::TitleGenerator;

/// The single persistence entry point for chat messages: insert a message
/// into a session's message stream, dropping any checkpointed partial stream
/// text first (a real message supersedes it). Both user turns (AgentLayer)
/// and assistant turns (ReActEngine) go through this one implementation so
/// the two paths cannot drift apart. The partial discard goes through the
/// executor's `PartialStore` so an in-flight stream checkpoint can never
/// re-create the row after the real message landed.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn persist_session_message(
    executor: &crate::session::SessionExecutor,
    session_id: &str,
    role: &str,
    content: &str,
    message_type: Option<&str>,
    attachments: &[MessageAttachment],
    voice: bool,
    // When `Some`, insert the row under this pre-minted id instead of
    // minting a fresh one. Streaming message ids are minted when the
    // thought/reasoning block starts so the live bubble and the DB row
    // share one identity.
    message_id: Option<&str>,
    // `tool_call_id` for the row (sentinel markers like `__ask__` ride
    // along here); `None` for ordinary messages.
    tool_call_id: Option<&str>,
) -> anyhow::Result<Message> {
    executor.partials.discard(session_id).await;
    let db = executor.db().clone();
    let session_id = session_id.to_string();
    let role = role.to_string();
    let content = content.to_string();
    let message_type = message_type.map(String::from);
    let attachments = attachments.to_vec();
    let message_id = message_id.map(String::from);
    let tool_call_id = tool_call_id.map(String::from);
    db.run_blocking(move |db| {
        db.add_message_full(
            &session_id,
            &role,
            &content,
            message_type.as_deref(),
            tool_call_id.as_deref(),
            &attachments,
            voice,
            message_id.as_deref(),
        )
    })
    .await
}

/// Checkpoint throttle for streamed partial text lives in
/// `context_limits.partial_checkpoint_interval_secs` /
/// `partial_checkpoint_min_chars` (see `ReActEngine::stream_llm_response`).
/// Trim a long tool result to fit a notification body.
fn truncate_notification(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cutoff = text.floor_char_boundary(max_chars);
    format!(
        "{}[... {} chars omitted]",
        &text[..cutoff],
        text.chars().count() - cutoff
    )
}

/// Repair a canonical message array so it is acceptable to tool-calling LLM
/// APIs: every `tool` message must be the response to a preceding assistant
/// message that declared `tool_calls`, and a trailing assistant message that
/// declares `tool_calls` must be followed by its results. Both violations are
/// rejected with a 400 by providers.
///
/// True when a single `CanonicalMessage` is part of a dangling boundary
/// that must not start a suffix ??either a `Tool` result or an `Assistant`
/// message that declared `tool_calls`. Providers reject the former when its
/// declaration is missing above it and the latter when its results are
/// missing below it, so both forms need to slide past (in
/// `ContextCompactor::safe_end_idx`) or get dropped (in
/// `sanitize_canonical`).
pub(crate) fn is_dangling_boundary(msg: &CanonicalMessage) -> bool {
    msg.role == CanonicalRole::Tool
        || (msg.role == CanonicalRole::Assistant && msg.tool_calls.is_some())
}

/// The ReAct loop only ever builds valid arrays, but snapshots/compaction
/// output can be corrupted by an interruption: a compaction split between an
/// assistant tool_call message and its tool results (the assistant is
/// summarized away while the results survive), an app exit right after the
/// assistant message was appended, or a tool batch cancelled mid-flight with
/// only some of its results appended. This drops orphaned `tool` messages
/// (no preceding assistant tool_calls) and, for every tool_call an assistant
/// declared without a matching result, inserts a synthetic `Tool` result
/// marked "Interrupted". Inserting an interrupted result (instead of trimming
/// the dangling assistant) keeps the array valid for providers that reject a
/// tool_call with no following result as a 400 — including a partial batch
/// where one of two declared calls never returned — and lets the loop see that
/// the tool was cut off and retry it if needed.
///
/// Text used for the synthetic interrupted result.
const INTERRUPTED_RESULT: &str =
    "Interrupted: the tool call was cut off before it returned a result.";

/// Enrich the interrupted-result text with the tool name and the arguments
/// that were attempted, so the model can see exactly which call was cut off
/// and retry it with the same input instead of guessing. Used by both the
/// live cancel path and the snapshot sanitize/repair path.
pub(crate) fn interrupted_result_text(tool_name: &str, arguments: &Value) -> String {
    if tool_name.is_empty() {
        INTERRUPTED_RESULT.to_string()
    } else {
        format!(
            "{} (tool: {}, arguments: {})",
            INTERRUPTED_RESULT, tool_name, arguments
        )
    }
}

pub(crate) fn sanitize_canonical(canonical: &mut Vec<CanonicalMessage>) {
    let mut out: Vec<CanonicalMessage> = Vec::with_capacity(canonical.len());
    // Tool_calls declared by the most recent assistant that have not yet been
    // answered by a tool result. Orphaned tool messages (this is empty) are
    // dropped; every call left pending when a non-tool message (or the array
    // end) arrives is repaired with an "Interrupted" result carrying the call's
    // own fields (id, name, arguments).
    let mut pending_calls: Vec<CanonicalToolCall> = Vec::new();
    for m in canonical.drain(..) {
        match m.role {
            CanonicalRole::Tool => {
                if pending_calls.is_empty() {
                    tracing::warn!(
                        "dropping orphaned tool message (tool_call_id={:?}) with no preceding assistant tool_calls",
                        m.tool_call_id
                    );
                    continue;
                }
                if let Some(cid) = &m.tool_call_id {
                    if let Some(pos) = pending_calls.iter().position(|c| &c.id == cid) {
                        pending_calls.remove(pos);
                    } else {
                        // The id doesn't match any outstanding call (some
                        // providers/agents don't echo it): consume the next
                        // pending call in order to keep the pairing aligned.
                        pending_calls.pop();
                    }
                } else {
                    pending_calls.pop();
                }
                out.push(m);
            }
            CanonicalRole::Assistant => {
                // A new assistant supersedes the previous assistant's
                // tool_calls: any still-unanswered ones were interrupted.
                repair_interrupted_tool_calls(&mut out, &mut pending_calls);
                pending_calls = m.tool_calls.clone().unwrap_or_default();
                out.push(m);
            }
            _ => {
                // A user/system/other message breaks the tool-call chain.
                repair_interrupted_tool_calls(&mut out, &mut pending_calls);
                out.push(m);
            }
        }
    }
    repair_interrupted_tool_calls(&mut out, &mut pending_calls);
    *canonical = out;
}

/// Append a synthetic `Tool` result marked "Interrupted" for every tool_call
/// still pending (declared by an assistant but never answered). This keeps the
/// canonical array valid — providers reject an assistant tool_call with no
/// following result as a 400 — while preserving the fact that the tool was
/// attempted, so the model can retry it. The result text carries the call's
/// own name and arguments so the model sees exactly what was attempted.
fn repair_interrupted_tool_calls(
    out: &mut Vec<CanonicalMessage>,
    pending_calls: &mut Vec<CanonicalToolCall>,
) {
    while let Some(call) = pending_calls.pop() {
        tracing::info!(
            "repairing interrupted tool_call {} with an Interrupted result",
            call.id
        );
        let text = interrupted_result_text(&call.name, &call.arguments);
        out.push(CanonicalMessage::tool(
            vec![ContentPart::text(text)],
            Some(call.id),
        ));
    }
}

mod layer;
pub use layer::AgentLayer;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::stream;
    use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, RiskLevel};
    use haven_llm::{
        FinishReason, LlmClient, LlmError, LlmResponse, StreamChunk, ToolDefinition, Usage,
    };
    use haven_tools::{Tool, ToolBox, ToolResult, ToolsManager};
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;

    fn temp_db() -> Arc<Database> {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        Arc::new(Database::open(&p).unwrap())
    }

    /// Mock LlmClient whose `chat_stream_with_tools` returns a single chunk
    /// containing the `final_answer` tool call so the ReAct loop terminates
    /// in one step.
    struct FinalAnswerMock;

    #[async_trait]
    impl LlmClient for FinalAnswerMock {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: chat not implemented".into()))
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown(
                "mock: chat_with_tools not implemented".into(),
            ))
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown(
                "mock: chat_stream not implemented".into(),
            ))
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let chunk = StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            };
            Ok(Box::pin(stream::iter(vec![Ok(chunk)])))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    struct RecordingEmitter {
        thoughts: std::sync::Mutex<Vec<String>>,
        supplements: std::sync::Mutex<Vec<String>>,
        notifications: std::sync::Mutex<Vec<(String, String)>>,
        completed: std::sync::Mutex<bool>,
    }

    #[async_trait]
    impl AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: AgentEvent) {
            match event {
                AgentEvent::Thought { thought, .. } => {
                    self.thoughts.lock().unwrap().push(thought);
                }
                AgentEvent::SessionCompleted { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::SessionUpdated { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::Supplement {
                    additional_context, ..
                } => {
                    self.supplements.lock().unwrap().push(additional_context);
                }
                AgentEvent::Notification { title, body, .. } => {
                    self.notifications.lock().unwrap().push((title, body));
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn run_session_emits_supplement_when_additional_context_queued() {
        let tools = Arc::new(ToolsManager::new());
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);

        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let session = executor
            .create_session_with_summary("do stuff", "do stuff summary")
            .await
            .unwrap();
        executor
            .add_supplement(&session.id, "extra: remember path X")
            .await
            .unwrap();

        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());

        let sups = recorder.supplements.lock().unwrap().clone();
        assert_eq!(sups.len(), 1, "exactly one supplement event expected");
        assert_eq!(sups[0], "extra: remember path X");
        // With supplements, session pauses instead of completing (conversation mode)
        let state = executor.get_session_state(&session.id).await;
        assert_eq!(
            state,
            Some(SessionStatus::Paused),
            "session should be paused (not completed) when supplements were processed"
        );
    }

    // ─── Pure-logic and data-layer tests (no LLM required) ───

    fn make_test_agent() -> (Arc<AgentLayer>, Arc<SessionExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));
        (agent, executor)
    }

    #[test]
    fn agent_new_constructor_works() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_new_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = AgentLayer::new(db, executor, router, 10, 20, ContextLimitsConfig::default());
        // Verify construction succeeded; no per-session indirection remains.
        let session = agent.db.create_session("input", "transcript").unwrap();
        assert!(!session.id.is_empty());
    }

    #[test]
    fn agent_constructs_without_session_machinery() {
        let (agent, _) = make_test_agent();
        let session = agent.db.create_session("input", "").unwrap();
        assert!(!session.id.is_empty());
        // Two sessions never share message keys ??each owns its own stream.
        let other = agent.db.create_session("input2", "").unwrap();
        assert_ne!(session.id, other.id);
    }

    #[test]
    fn set_emitter_stores_reference() {
        let (agent, _) = make_test_agent();
        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());
        // Verify emitter is stored without panic (set_emitter succeeds)
    }

    #[tokio::test]
    async fn replace_router_and_router_work() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_router_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let client_a = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_a = Arc::new(LlmRouter::new_with_clients(
            client_a.clone(),
            client_a.clone(),
            client_a.clone(),
            client_a.clone(),
            client_a,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor,
            router_a,
            10,
            20,
            ContextLimitsConfig::default(),
        ));
        // Create a new router via the same mock client factory
        let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_b = Arc::new(LlmRouter::new_with_clients(
            client_b.clone(),
            client_b.clone(),
            client_b.clone(),
            client_b.clone(),
            client_b,
        ));
        agent.replace_router(router_b);
        // No panic == success
    }

    #[tokio::test]
    async fn set_max_steps_updates_field() {
        let (agent, executor) = make_test_agent();
        agent.set_max_steps(5);

        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let session = executor.create_session("test").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());
    }

    #[tokio::test]
    async fn build_system_prompt_succeeds() {
        let (agent, _) = make_test_agent();
        let prompt = agent.prompt_builder.build("test session", &[], &[]).await;
        assert!(prompt.contains("You have access to the following built-in tools"));
    }

    #[tokio::test]
    async fn build_system_prompt_excludes_sensitive_and_duplicate_facts() {
        let dir =
            std::env::temp_dir().join(format!("haven_prompt_facts_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&dir).unwrap());
        // Duplicate triple (same tags, same everything).
        db.insert_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        db.insert_fact("user", "name", "Xtopia", "user", 1.0, &["identity"])
            .unwrap();
        // A legitimate preference.
        db.insert_fact("user", "likes", "Rust", "user", 0.9, &["preference"])
            .unwrap();
        // Secrets that must never reach the prompt.
        db.insert_fact(
            "user",
            "tavily_api_key",
            "tvly-dev-secret",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();
        db.insert_fact(
            "user",
            "secret_token",
            "ghp_abc",
            "inferred",
            1.0,
            &["workspace"],
        )
        .unwrap();

        let tools = Arc::new(ToolsManager::new());
        let builder = SystemPromptBuilder::new(tools, db);
        let prompt = builder.build("test session", &[], &[]).await;

        assert!(prompt.contains("name=Xtopia"));
        assert!(prompt.contains("likes=Rust"));
        assert!(!prompt.contains("tavily_api_key"));
        assert!(!prompt.contains("tvly-dev-secret"));
        assert!(!prompt.contains("secret_token"));
        assert!(!prompt.contains("ghp_abc"));
        // Duplicates are collapsed: the name fact is rendered exactly once.
        assert_eq!(prompt.matches("name=Xtopia").count(), 1);
    }

    #[tokio::test]
    async fn restore_per_session_tools_rebuilds_from_history() {
        // Create a skill on disk so SkillsEngine can discover it.
        let dir = std::env::temp_dir().join(format!("haven_restore_test_{}", uuid::Uuid::new_v4()));
        let skill_dir = dir.join("echo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
        )
        .unwrap();

        let db = Arc::new(
            Database::open(
                &std::env::temp_dir().join(format!("haven_restore_db_{}.db", uuid::Uuid::new_v4())),
            )
            .unwrap(),
        );
        let tools = Arc::new(ToolsManager::new());
        tools
            .skills_engine
            .set_config(Some(dir.clone()), None)
            .await
            .unwrap();
        tools.rebuild_catalog().await;
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools.clone(), 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor,
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));

        // Simulate a history where load_skill was called.
        let history = vec![ReActStep {
            step_number: 1,
            thought: Some("I need the echo skill".into()),
            action: Some(Action {
                tool_name: "load_skill".into(),
                tool_input: serde_json::json!({"skill_name": "echo"}),
                is_final: false,
                tool_call_id: Some("tc1".into()),
            }),
            observation: Some(r#"{"skill":{"name":"skill__echo"}}"#.into()),
        }];

        // Before restore, no per-session tools.
        let before = tools.list_schemas_for_session("ses-x").await;
        assert!(!before.iter().any(|s| s["name"] == "skill__echo"));

        agent.restore_per_session_tools("ses-x", &history).await;

        // After restore, the skill tool should be visible per-session.
        let after = tools.list_schemas_for_session("ses-x").await;
        assert!(
            after.iter().any(|s| s["name"] == "skill__echo"),
            "restored skill should appear in per-session schemas"
        );

        // Other sessions should NOT see it.
        let other = tools.list_schemas_for_session("ses-y").await;
        assert!(!other.iter().any(|s| s["name"] == "skill__echo"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn persist_message_adds_to_db() {
        let (agent, _) = make_test_agent();
        let session = agent.db.create_session("input", "").unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "test message",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Read back via db
        let agent_ref = agent.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_session_messages_limit(&session.id, 50).unwrap();
        // Messages may or may not be immediately flushed depending on cache
        // ??verify at minimum the message is retrievable
        let found = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "test message");
        assert!(found.is_some(), "persisted user message not found in db");
    }

    #[tokio::test]
    async fn persist_message_with_attachments_roundtrips() {
        let (agent, _) = make_test_agent();
        let session = agent.db.create_session("input", "").unwrap();
        let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "看图",
                Some("text"),
                std::slice::from_ref(&att),
                false,
            )
            .await
            .unwrap();
        let agent_ref = agent.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_session_messages_limit(&session.id, 50).unwrap();
        let found = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "看图");
        assert!(found.is_some(), "persisted message not found in db");
        let msg = found.unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(msg.attachments[0].media_type, "image/png");
        assert_eq!(msg.attachments[0].data, "aGVsbG8=");
    }

    #[test]
    fn parse_default_model_response_final_answer_from_text() {
        let resp = LlmResponse {
            text: "Session done.".into(),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                model_name: None,
                cost: None,
            },
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 1);
        assert_eq!(thought, Some("Session done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
        assert_eq!(actions[0].tool_name, "final_answer");
    }

    #[test]
    fn parse_default_model_response_with_tool_calls() {
        let resp = LlmResponse {
            text: "Opening file.".into(),
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "open_file".into(),
                arguments: serde_json::json!({"path": "/tmp/test"}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: haven_llm::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                model_name: None,
                cost: None,
            },
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 2);
        assert_eq!(thought, Some("Opening file.".into()));
        assert_eq!(actions.len(), 1);
        assert!(!actions[0].is_final);
        assert_eq!(actions[0].tool_name, "open_file");
        assert_eq!(
            actions[0].tool_input,
            serde_json::json!({"path": "/tmp/test"})
        );
    }

    #[test]
    fn parse_default_model_response_final_answer_tool_call() {
        let resp = LlmResponse {
            text: "All done.".into(),
            tool_calls: vec![CanonicalToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                model_name: None,
                cost: None,
            },
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let (thought, actions) = ReActEngine::parse_default_model_response(&resp, 1);
        assert_eq!(thought, Some("All done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
    }

    /// M3/H10: a follow-up message must NOT resurrect a session that was ended.
    /// Terminal sessions are only reactivated explicitly via `reopen_session`
    /// (Completed/Error ??Paused) in the review flow.
    #[tokio::test]
    async fn process_input_does_not_resurrect_ended_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor.end_session(&session.id).await.unwrap();
        // end_session removes the session from the working set entirely.
        assert_eq!(executor.get_session_state(&session.id).await, None);

        let result = agent
            .process_input("more context", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        // Session is not reloaded into the working set and never becomes Pending.
        assert_eq!(executor.get_session_state(&session.id).await, None);
        assert!(executor.get_supplements(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn process_input_reactivates_paused_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let result = agent
            .process_input("more context", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let supps: Vec<String> = executor
            .get_supplements(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(supps, vec!["more context"]);
    }

    #[tokio::test]
    async fn process_input_marks_reply_as_answer_when_awaiting() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::PausedAwaitingAnswer)
            .await
            .unwrap();

        let result = agent
            .process_input("the answer", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let supps = executor.get_supplements(&session.id).await;
        assert_eq!(supps.len(), 1);
        assert!(
            supps[0].is_answer,
            "reply to an ask must be marked as answer"
        );
        assert_eq!(supps[0].text, "the answer");
        assert!(
            !executor
                .get_session_state(&session.id)
                .await
                .is_some_and(|s| s.is_awaiting_answer()),
            "reactivation must clear the awaiting-answer gate"
        );
    }

    #[tokio::test]
    async fn reopen_session_requeues_undelivered_inputs_stays_paused() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("input text").await.unwrap();
        // The session input (first user message) is seeded into the canonical
        // directly and never carries a step anchor.
        agent
            .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
            .await
            .unwrap();
        // A steering input that WAS delivered carries a step anchor under its
        // own id (created by `push_user_context`).
        let delivered = agent
            .persist_message_parts(
                &session.id,
                "user",
                "steering delivered",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        agent
            .db
            .create_thought_step(&session.id, 2, &delivered.id)
            .unwrap();
        // A steering input lost before injection has no anchor.
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "steering lost",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Terminal state: the session leaves the working set (an error/cancel
        // dropped the in-memory queues along with the lost steering).
        executor
            .update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        assert_eq!(executor.get_session_state(&session.id).await, None);

        agent.reopen_session(&session.id).await.unwrap();

        // Re-queued for a later Continue / follow-up, but review stays Paused
        // so opening history never auto-runs ReAct on old chats.
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let supps = executor.get_supplements(&session.id).await;
        assert_eq!(supps.len(), 1, "only the never-injected input is re-queued");
        assert_eq!(supps[0].text, "steering lost");
    }

    #[tokio::test]
    async fn reopen_session_without_pending_inputs_stays_paused() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("input text").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "input text", Some("text"), &[], false)
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();

        agent.reopen_session(&session.id).await.unwrap();

        // No lost inputs: the session reopens as Paused (review-only),
        // matching the historical behavior.
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(executor.get_supplements(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn process_input_paused_without_ask_is_plain_supplement() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("original").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let result = agent
            .process_input("follow up", Some(session.id.clone()))
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        let supps = executor.get_supplements(&session.id).await;
        assert_eq!(supps.len(), 1);
        assert!(
            !supps[0].is_answer,
            "a follow-up to a normal pause is not an ask reply"
        );
    }

    #[tokio::test]
    async fn resume_dedups_conversation_prefix_against_canonical() {
        // A legacy snapshot may carry `[conversation]`-wrapped lines from an
        // older resume implementation. They must be stripped, and nothing may
        // be re-injected on top of the canonical's full transcript — the
        // snapshot is the single authority for everything it contains.
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "assistant",
                "hi there",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Snapshot whose canonical already carries the full transcript PLUS
        // a stale `[conversation]` prefix left by a previous resume ??the
        // exact duplication that made the model re-answer old questions.
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("hello"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("hi there")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::user_text("[conversation] [user] hello"),
            CanonicalMessage::user_text("[conversation] [assistant] hi there"),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let user_texts: Vec<String> = saved
            .canonical
            .iter()
            .filter(|m| m.role == CanonicalRole::User)
            .filter_map(|m| {
                m.content.iter().find_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert!(
            user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
            "stale [conversation] lines must be stripped: {:?}",
            user_texts
        );
        assert_eq!(
            user_texts.iter().filter(|t| t.as_str() == "hello").count(),
            1,
            "already-present messages must not be duplicated"
        );
    }

    #[tokio::test]
    async fn resume_dedups_supplement_inputs_against_prefixed_canonical() {
        // Supplement/steering inputs are pushed into the canonical with a
        // text prefix ("Additional context from user: —, "Steering: —)
        // while the DB stores the raw text. A legacy snapshot (no saved_at)
        // is trusted as complete: nothing is recovered, so the already
        // prefixed inputs are never re-injected as fresh user turns.
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        // DB stores the RAW user text (this is what process_input persists).
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "please be brief",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Canonical carries the prefixed form (as push_user_context emits it).
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("hello"),
            CanonicalMessage::user_text("Additional context from user: please be brief"),
            CanonicalMessage::user_text("Steering: please be brief"),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let user_texts: Vec<String> = saved
            .canonical
            .iter()
            .filter(|m| m.role == CanonicalRole::User)
            .filter_map(|m| {
                m.content.iter().find_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            user_texts
                .iter()
                .filter(|t| t.as_str() == "[conversation] [user] please be brief")
                .count(),
            0,
            "supplement text already present (prefixed) must not be re-seeded: {:?}",
            user_texts
        );
    }

    #[tokio::test]
    async fn resume_keeps_repeated_same_text_turns() {
        // Two distinct turns with identical text (user said "好的" twice) are
        // both legitimate history. The snapshot is the single authority for
        // everything it contains; a message persisted AFTER the snapshot's
        // saved_at is recovered by timestamp — identical text is recovered
        // too (timestamp recovery never drops a repeated turn).
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "好的", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(&session.id, "assistant", "好的", Some("text"), &[], false)
            .await
            .unwrap();
        // Snapshot saved right after the first pair: its saved_at sits
        // between the persisted rows and the second user "好的" below.
        let msgs_before = agent.db.get_session_messages(&session.id).unwrap();
        let saved_at = msgs_before[1].created_at.clone();
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("好的"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("好的")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: Some(saved_at),
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        // The second identical user turn lands after the snapshot.
        agent
            .persist_message_parts(&session.id, "user", "好的", Some("text"), &[], false)
            .await
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let user_texts: Vec<String> = saved
            .canonical
            .iter()
            .filter(|m| m.role == CanonicalRole::User)
            .filter_map(|m| {
                m.content.iter().find_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            user_texts.iter().filter(|t| t.as_str() == "好的").count(),
            1,
            "the first user turn must not be duplicated: {:?}",
            user_texts
        );
        assert_eq!(
            user_texts
                .iter()
                .filter(|t| t.starts_with("Additional context from user: 好的"))
                .count(),
            1,
            "the second identical user turn must be recovered by timestamp: {:?}",
            user_texts
        );
        assert!(
            user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
            "no [conversation]-wrapped lines may exist: {:?}",
            user_texts
        );
    }

    #[tokio::test]
    async fn resume_does_not_recover_messages_before_saved_at() {
        // Timestamp recovery is bounded by the snapshot's saved_at: rows
        // persisted before it are already represented in the canonical and
        // must NOT be re-queued, even when the canonical never carried them
        // as user turns (e.g. an ask question persisted under the step id).
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
            .await
            .unwrap();
        let saved_at = agent.db.get_session_messages(&session.id).unwrap()[0]
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("hello"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("hi there")],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: Some(saved_at),
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        // Assistant rows older than saved_at are not recovered either.
        agent
            .persist_message_parts(
                &session.id,
                "assistant",
                "hi there",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let user_texts: Vec<String> = saved
            .canonical
            .iter()
            .filter(|m| m.role == CanonicalRole::User)
            .filter_map(|m| {
                m.content.iter().find_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            user_texts.iter().filter(|t| t.as_str() == "hello").count(),
            1,
            "nothing older than saved_at may be recovered: {:?}",
            user_texts
        );
        assert!(
            user_texts
                .iter()
                .all(|t| !t.starts_with("Additional context from user:")),
            "no post-snapshot supplement may appear: {:?}",
            user_texts
        );
    }

    #[tokio::test]
    async fn resume_skips_conversation_reseed_when_canonical_is_compacted() {
        // Compaction replaces the old turns with a summary inside the
        // canonical but leaves the DB message stream untouched. Recovery is
        // timestamp-bounded (only rows newer than the snapshot's saved_at are
        // re-queued), so the summarized-away turns — all older than the
        // snapshot — are never resurrected; a compacted canonical stays
        // compacted across resume.
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("hello").await.unwrap();
        agent
            .persist_message_parts(&session.id, "user", "hello", Some("text"), &[], false)
            .await
            .unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "assistant",
                "long ago answer",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::assistant(
                vec![ContentPart::text(
                    "[Compacted summary of previous messages]: hello / long ago answer",
                )],
                None,
                None,
                Vec::new(),
                Vec::new(),
            ),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let user_texts: Vec<String> = saved
            .canonical
            .iter()
            .filter(|m| m.role == CanonicalRole::User)
            .filter_map(|m| {
                m.content.iter().find_map(|p| match p {
                    ContentPart::Text(t) => Some(t.clone()),
                    _ => None,
                })
            })
            .collect();
        assert!(
            user_texts.iter().all(|t| !t.starts_with("[conversation] ")),
            "compacted canonical must not be re-seeded from the DB window: {:?}",
            user_texts
        );
    }

    #[tokio::test]
    async fn loop_pauses_on_pending_ask_instead_of_heuristic_final() {
        // The model responds with text + Stop and no tool calls while an
        // unanswered `ask` is pending: the turn must not end on the
        // synthesized heuristic final ??the loop must pause and wait for
        // the user's answer instead.
        let client = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("I'll stop here.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("session").await.unwrap();
        let canonical = vec![
            CanonicalMessage::system(vec![ContentPart::text("sys")]),
            CanonicalMessage::user_text("help me"),
            CanonicalMessage::assistant(
                vec![ContentPart::text("let me ask")],
                Some(vec![CanonicalToolCall {
                    id: "call_ask".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "which file?"}),
                }]),
                None,
                Vec::new(),
                Vec::new(),
            ),
            CanonicalMessage::tool(
                vec![ContentPart::text(
                    r#"{"ask":true,"question":"which file?","awaiting_answer":true,"options":[]}"#,
                )],
                Some("call_ask".into()),
            ),
        ];
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();

        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "session must pause for the pending question instead of completing"
        );
        assert!(
            executor
                .get_session_state(&session.id)
                .await
                .is_some_and(|s| s.is_awaiting_answer()),
            "pause must be flagged as awaiting the user's answer"
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(
            msgs.last().unwrap().content,
            "which file?",
            "the pending question must be surfaced as the pause message"
        );
    }

    #[tokio::test]
    async fn budget_exhaustion_pauses_with_notification_and_no_chat_message() {
        // The scripted LLM always returns a non-final tool call, so the run
        // consumes its 1-step budget without ever producing a final answer.
        let client = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("keep working".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "x"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
        agent.set_max_steps(1);
        let recorder = make_recording_emitter();
        agent.set_emitter(recorder.clone());
        let session = executor.create_session("session").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "budget exhaustion must pause the session as a checkpoint"
        );
        // The notice must NOT be persisted as an assistant chat message.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.iter()
                .all(|m| !m.content.contains("任务步骤上限已用尽")),
            "budget notice must not appear as a chat message: {:?}",
            msgs.iter().map(|m| m.content.as_str()).collect::<Vec<_>>()
        );
        // It must be surfaced as a Notification event instead.
        let notifications = recorder.notifications.lock().unwrap().clone();
        assert!(
            notifications
                .iter()
                .any(|(title, _)| title == "任务步骤上限已用尽"),
            "budget notice must be emitted as a notification: {:?}",
            notifications
        );
    }

    #[tokio::test]
    async fn truncated_text_only_response_retried_before_final() {
        // First response: text with a Length finish (generation cut off) ??        // must NOT end the turn as if it were the final answer. Second
        // response: a complete Stop answer, which ends the turn.
        let client = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Here is the partial answer".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Length),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Here is the complete answer.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(client, Arc::new(ToolsManager::new()));
        agent.set_emitter(make_recording_emitter());
        let session = executor.create_session("session").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "turn must end paused after the retried final"
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(
            msgs.last().unwrap().content,
            "Here is the complete answer.",
            "the retried (complete) response must be the final message, not the truncated one"
        );
    }

    fn make_recording_emitter() -> Arc<RecordingEmitter> {
        Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            notifications: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        })
    }

    #[tokio::test]
    async fn run_session_from_id_attaches_first_user_message_images() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor
            .create_session_with_summary("看图", "看图")
            .await
            .unwrap();
        let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .persist_message_parts(&session.id, "user", "看图", Some("text"), &[att], false)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        let snapshot: crate::types::ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let user_msg = snapshot
            .canonical
            .iter()
            .find(|m| m.role == CanonicalRole::User)
            .expect("initial user message exists");
        assert!(
            user_msg
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. })),
            "initial user message should carry the image part"
        );
    }

    #[tokio::test]
    async fn run_session_from_id_ignores_later_image_supplement() {
        let (agent, executor) = make_test_agent();
        agent.set_emitter(make_recording_emitter());
        let session = executor
            .create_session_with_summary("plain session", "plain session")
            .await
            .unwrap();
        agent
            .persist_message_parts(
                &session.id,
                "user",
                "plain session",
                Some("text"),
                &[],
                false,
            )
            .await
            .unwrap();
        // Image arrives AFTER the session input (a supplement) ??it must not be
        // attached to the initial user turn.
        let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        agent
            .process_input_with_attachments("补充看图", Some(session.id.clone()), &[att], false)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        let snapshot: crate::types::ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let first_user = snapshot
            .canonical
            .iter()
            .find(|m| m.role == CanonicalRole::User)
            .expect("initial user message exists");
        assert!(
            !first_user
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. })),
            "image supplement must not be attached to the initial user turn"
        );
        // The supplement itself is still injected (with its image) later.
        assert!(
            snapshot.canonical.iter().any(|m| m
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::Image { .. }))),
            "supplement image should be injected into the conversation"
        );
    }

    #[tokio::test]
    async fn run_session_from_id_trims_dangling_tool_call_before_resume() {
        // Simulate a snapshot saved by save_branch_point right after the
        // assistant tool_call message but before tool results were appended
        // (e.g. the app was closed mid-tool-execution). Resuming must trim
        // the dangling assistant message instead of sending it to the LLM,
        // which would reject it with a 400 error.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("resume me").await.unwrap();

        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("system prompt")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("resume me")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("calling echo")],
                tool_calls: Some(vec![CanonicalToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "hi"}),
                }]),
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let history = vec![ReActStep {
            step_number: 1,
            thought: Some("calling echo".into()),
            action: None,
            observation: None,
        }];
        let snapshot = ReActSnapshot {
            canonical,
            history,
            step_number: 2,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        let result = agent.run_session_from_id(&session.id).await.unwrap();

        // No batch sent to the LLM may end with a dangling assistant tool_call.
        {
            let seen = mock.seen.lock().unwrap();
            assert!(!seen.is_empty(), "LLM should have been called after resume");
            for batch in seen.iter() {
                let last = batch.last().expect("batch has messages");
                assert!(
                    !(matches!(last.role, CanonicalRole::Assistant) && last.tool_calls.is_some()),
                    "batch must not end with a dangling assistant tool_call: {:?}",
                    batch
                );
            }
        }
        assert!(!result.is_empty(), "resumed loop should produce history");
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "final_answer should complete the resumed session"
        );
    }

    #[tokio::test]
    async fn run_session_rebuilds_tool_chain_from_steps_without_snapshot() {
        // When react_state is missing (corrupt or schema-drifted), resume
        // falls back to a fresh run. The DB message stream holds only text,
        // so the rebuilt canonical must recover the tool-call/result pairs
        // from session_steps —otherwise the model forgets every tool it ran
        // and re-executes them.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("resume me").await.unwrap();
        // Persisted text turns (what the DB message stream holds)—
        agent
            .persist_message_parts(&session.id, "user", "resume me", Some("text"), &[], false)
            .await
            .unwrap();
        // …plus the action chain in session_steps (what a snapshot-less resume
        // must reconstruct). Use raw repo calls to avoid going through the
        // ReAct loop.
        agent
            .db
            .run_blocking({
                let session_id = session.id.clone();
                move |db| {
                    db.create_thought_step(&session_id, 1, "step-echo-thought")?;
                    let step = db.create_action_step(
                        &session_id,
                        2,
                        "echo",
                        r#"{"text":"hi"}"#,
                        false,
                        false,
                        None,
                        None,
                    )?;
                    db.complete_action_step(&step.id, "hi", true)?;
                    Ok::<(), anyhow::Error>(())
                }
            })
            .await
            .unwrap();
        // NO react_state row: fallback path.
        assert!(agent.db.get_react_state(&session.id).unwrap().is_none());

        agent.run_session_from_id(&session.id).await.unwrap();

        {
            let seen = mock.seen.lock().unwrap();
            assert_eq!(seen.len(), 1, "fresh run after snapshot-less resume");
            let first = &seen[0];
            let roles: Vec<String> = first.iter().map(|m| m.role.to_string()).collect();
            // The rebuilt chain must appear: assistant with tool_calls,
            // followed by its tool result (sanitize may keep them intact).
            assert!(
                roles.iter().any(|r| r == "assistant"),
                "expected an assistant tool-call message: {:?}",
                roles
            );
            let rebuilt_tool = first.iter().any(|m| {
                matches!(m.role, CanonicalRole::Assistant)
                    && m.tool_calls.as_ref().is_some_and(|c| {
                        c.iter()
                            .any(|tc| tc.name == "echo" && tc.id.starts_with("resumed_"))
                    })
            });
            assert!(
                rebuilt_tool,
                "snapshot-less resume must rebuild the echo call from session_steps"
            );
            let rebuilt_result = first.iter().any(|m| {
                matches!(m.role, CanonicalRole::Tool)
                    && m.tool_call_id
                        .as_deref()
                        .is_some_and(|id| id.starts_with("resumed_"))
            });
            assert!(
                rebuilt_result,
                "snapshot-less resume must rebuild the echo result from session_steps"
            );
        }
    }

    fn make_canonical(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text.to_string())],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }
    }

    fn make_assistant_with_calls(ids: &[&str]) -> CanonicalMessage {
        let mut m = make_canonical(CanonicalRole::Assistant, "");
        m.tool_calls = Some(
            ids.iter()
                .map(|id| CanonicalToolCall {
                    id: id.to_string(),
                    name: "tool".into(),
                    arguments: serde_json::Value::Null,
                })
                .collect(),
        );
        m
    }

    fn make_tool_result(call_id: &str, text: &str) -> CanonicalMessage {
        let mut m = make_canonical(CanonicalRole::Tool, text);
        m.tool_call_id = Some(call_id.to_string());
        m
    }

    #[test]
    fn sanitize_canonical_drops_orphaned_tool_messages_and_dangling_calls() {
        // Mirrors the corruption found in a real interrupted session: a
        // compaction split the assistant(tool_calls)/tool-results pair, so
        // the summary assistant (no tool_calls) is followed by orphaned tool
        // messages. A valid pair and a dangling trailing assistant follow —
        // the dangling call is repaired with an Interrupted result instead of
        // being dropped.
        let mut canonical = vec![
            make_canonical(CanonicalRole::System, "sys"),
            make_canonical(CanonicalRole::User, "hello"),
            make_canonical(CanonicalRole::Assistant, "[Compacted summary]"),
            make_tool_result("call_00_a", "result a"),
            make_tool_result("call_01_b", "result b"),
            make_assistant_with_calls(&["call_00_c", "call_01_d"]),
            make_tool_result("call_00_c", "result c"),
            make_tool_result("call_01_d", "result d"),
            make_assistant_with_calls(&["call_00_e"]),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::System,
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::Tool,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
            ],
            "orphaned tools must be dropped and the dangling tool_call repaired with an Interrupted result"
        );
        // The surviving pair's tool results are intact.
        assert_eq!(canonical[4].tool_call_id.as_deref(), Some("call_00_c"));
        assert_eq!(canonical[5].tool_call_id.as_deref(), Some("call_01_d"));
        // The dangling trailing call was answered with an Interrupted result.
        assert_eq!(canonical[7].tool_call_id.as_deref(), Some("call_00_e"));
        assert!(canonical[7].content.iter().any(|p| matches!(
            p,
            ContentPart::Text(t) if t.contains("Interrupted")
        )));
    }

    #[test]
    fn sanitize_canonical_repairs_partial_tool_batch() {
        // The real interrupted-batch failure: an assistant declared TWO tool
        // calls but only one result came back (the other tool was cut off
        // mid-execution). Providers reject an incomplete batch with a 400, so
        // the missing result must be repaired with an Interrupted one.
        let mut canonical = vec![
            make_canonical(CanonicalRole::User, "hi"),
            make_assistant_with_calls(&["call_a", "call_b"]),
            make_tool_result("call_a", "result a"),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::Tool,
            ],
            "the missing tool_call result must be repaired with an Interrupted result"
        );
        assert_eq!(canonical[2].tool_call_id.as_deref(), Some("call_a"));
        assert_eq!(canonical[3].tool_call_id.as_deref(), Some("call_b"));
        assert!(canonical[3].content.iter().any(|p| matches!(
            p,
            ContentPart::Text(t)
                if t.contains("Interrupted") && t.contains("tool:") && t.contains("arguments")
        )));
    }

    #[test]
    fn sanitize_canonical_repairs_interrupted_call_before_user_message() {
        // A dangling tool_call followed by a new user message: the interrupted
        // call must get an Interrupted result inserted before the user message
        // (it can no longer be trimmed as trailing), keeping the array valid.
        let mut canonical = vec![
            make_canonical(CanonicalRole::User, "a"),
            make_assistant_with_calls(&["call_1"]),
            make_canonical(CanonicalRole::User, "next"),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::User,
            ],
            "an Interrupted result must be inserted between the dangling call and the next user message"
        );
        assert_eq!(canonical[2].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn sanitize_canonical_keeps_user_reset_and_trailing_tool() {
        // A tool message following a user message is orphaned; a trailing
        // tool message after its assistant-with-calls is valid.
        let mut canonical = vec![
            make_canonical(CanonicalRole::User, "a"),
            make_assistant_with_calls(&["call_1"]),
            make_tool_result("call_1", "r"),
            make_canonical(CanonicalRole::User, "b"),
            make_tool_result("call_1", "orphan after user"),
        ];
        sanitize_canonical(&mut canonical);

        let roles: Vec<CanonicalRole> = canonical.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            vec![
                CanonicalRole::User,
                CanonicalRole::Assistant,
                CanonicalRole::Tool,
                CanonicalRole::User,
            ],
            "only the orphaned trailing tool must be removed"
        );
    }

    #[tokio::test]
    async fn process_input_with_attachments_queues_and_persists_attachments() {
        let (agent, executor) = make_test_agent();
        let session = executor
            .create_session_with_summary("original", "original")
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let att = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        let result = agent
            .process_input_with_attachments(
                "看图",
                Some(session.id.clone()),
                std::slice::from_ref(&att),
                false,
            )
            .await
            .unwrap();
        assert_eq!(result, ProcessResult::Supplemented);
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let supps = executor.get_supplements(&session.id).await;
        assert_eq!(supps.len(), 1);
        assert_eq!(supps[0].text, "看图");
        assert_eq!(supps[0].attachments, vec![att]);

        // Persisted with attachments in the session's message stream.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let user_msg = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "看图")
            .expect("user message persisted");
        assert_eq!(user_msg.attachments.len(), 1);
        assert_eq!(user_msg.attachments[0].media_type, "image/png");
    }

    #[tokio::test]
    async fn process_input_creates_new_session() {
        let (agent, executor) = make_test_agent();
        let result = agent.process_input("open notepad", None).await.unwrap();
        match result {
            ProcessResult::SessionCreated(session_id) => {
                assert!(!session_id.is_empty());
                let state = executor.get_session_state(&session_id).await;
                assert_eq!(state, Some(SessionStatus::Pending));
            }
            ProcessResult::Supplemented => panic!("expected SessionCreated"),
        }
    }

    #[tokio::test]
    async fn run_fact_inference_does_not_panic() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("test").await.unwrap();
        executor.end_session(&session.id).await.unwrap();
        agent.inference.infer_facts(&session.id).await;
    }

    // ─── Integration tests for the ReAct core loop (refine loop) ───

    fn make_test_agent_with(
        client: Arc<dyn LlmClient>,
        tools: Arc<ToolsManager>,
    ) -> (Arc<AgentLayer>, Arc<SessionExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));
        (agent, executor)
    }

    /// A mock tool whose schema requires an `action` field, mirroring the
    /// production failure where a call's arguments are missing a required
    /// discriminator field and the provider rejects the request body.
    struct ActionRequiredTool;
    #[async_trait]
    impl Tool for ActionRequiredTool {
        fn name(&self) -> String {
            "action_required".into()
        }
        fn description(&self) -> String {
            "requires an action".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["go", "stop"],
                        "default": "go"
                    },
                    "query": { "type": "string" }
                },
                "required": ["action", "query"]
            })
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_fills_schema_defaults() {
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        // A call missing both required fields (`action` and `query`).
        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        let input = &actions[0].tool_input;
        // `action` has a schema default ("go"); `query` has none, so it gets
        // a type-appropriate placeholder (empty string).
        assert_eq!(input["action"], "go");
        assert_eq!(input["query"], "");
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_skips_fully_populated_and_final() {
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        // Complete call: nothing to supplement.
        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": "stop", "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 0);

        // Final actions are never repaired.
        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({}),
            is_final: true,
            tool_call_id: None,
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 0);
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_repairs_null_input() {
        // Interrupted/truncated generation yields unparseable arguments,
        // which parse_default_model_response converts to Null. The repair
        // must still fill the required fields instead of shipping the bare
        // Null to the tool (which fails validate_input for every field).
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::Value::Null,
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert!(actions[0].tool_input.is_object());
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "");
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_fills_null_valued_fields() {
        // A required field explicitly set to null is as unusable as a
        // missing one: the validator rejects null for typed fields.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": null, "query": null}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "");
    }
    /// A mock tool whose required field is enum-constrained with NO schema
    /// default, mirroring the `input` tool's `operation` discriminator.
    /// The type placeholder (`""`) would violate the enum, so the repair
    /// must fall back to the first declared enum value.
    struct EnumRequiredTool;
    #[async_trait]
    impl Tool for EnumRequiredTool {
        fn name(&self) -> String {
            "enum_required".into()
        }
        fn description(&self) -> String {
            "requires an enum value".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["type", "key", "click"]
                    }
                },
                "required": ["operation"]
            })
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    /// A mock tool with an optional enum-constrained field: the value is
    /// validated when present, but the field itself is not required.
    struct EnumWithOptionalTool;
    #[async_trait]
    impl Tool for EnumWithOptionalTool {
        fn name(&self) -> String {
            "enum_with_optional".into()
        }
        fn description(&self) -> String {
            "optional enum field".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["type", "key", "click"]
                    },
                    "optional": {
                        "type": "string",
                        "enum": ["a", "b", "c"]
                    }
                },
                "required": ["operation"]
            })
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn supplement_missing_required_fields_enum_field_gets_first_value() {
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(EnumRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "enum_required".into(),
            tool_input: serde_json::json!({}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["operation"], "type");
    }

    #[tokio::test]
    async fn supplement_repairs_present_value_not_in_enum() {
        // The `action` field is PRESENT but its value is not in the schema
        // enum. Strict providers validate tool_use input against the declared
        // schema and reject the request with a 400 ("Failed to deserialize
        // the JSON body into the target type: input.action: ...") — the value
        // must be replaced with the schema default before it reaches the
        // provider, not just when the field is missing.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": "bogus", "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        // `action` falls back to the schema default "go"; the valid `query`
        // is left untouched.
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "hi");
    }

    #[tokio::test]
    async fn supplement_repairs_present_value_of_wrong_type() {
        // Same provider 400 when a field's value type contradicts the schema
        // (e.g. a number where the schema declares a string enum).
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": 42, "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["action"], "go");
        assert_eq!(actions[0].tool_input["query"], "hi");
    }

    #[tokio::test]
    async fn supplement_keeps_valid_enum_values_untouched() {
        // A value that conforms to the schema (in the enum, correct type)
        // must NOT be repaired.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(ActionRequiredTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "action_required".into(),
            tool_input: serde_json::json!({"action": "stop", "query": "hi"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 0);
        assert_eq!(actions[0].tool_input["action"], "stop");
    }

    #[tokio::test]
    async fn supplement_repairs_invalid_optional_field() {
        // Even a non-required property with an invalid value can trip the
        // provider's deserialization (the input object is validated as a
        // whole), so it is repaired too.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(EnumWithOptionalTool) as ToolBox)
            .await;
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let (agent, executor) = make_test_agent_with(client, tools);
        let session = executor.create_session("do it").await.unwrap();

        let mut actions = vec![Action {
            tool_name: "enum_with_optional".into(),
            tool_input: serde_json::json!({"operation": "type", "optional": "nope"}),
            is_final: false,
            tool_call_id: Some("call_1".into()),
        }];
        let repaired = agent
            .react_engine
            .supplement_missing_required_fields(&session.id, &mut actions)
            .await;
        assert_eq!(repaired, 1);
        assert_eq!(actions[0].tool_input["operation"], "type");
        // Optional invalid enum field falls back to the first enum value.
        assert_eq!(actions[0].tool_input["optional"], "a");
    }

    /// Scripted LlmClient that returns a pre-programmed sequence of responses
    /// from `chat_stream_with_tools`, enabling full ReAct-loop integration
    /// tests without a live LLM. Mirrors Pi's `MockLlmClient` pattern.
    struct ScriptedMock {
        stream_responses: std::sync::Mutex<VecDeque<ScriptedResponse>>,
        chat_text: std::sync::Mutex<String>,
        /// Every message batch sent to `chat_stream_with_tools`, for
        /// assertions (e.g. that no dangling tool_call is sent).
        seen: std::sync::Mutex<Vec<Vec<CanonicalMessage>>>,
    }

    enum ScriptedResponse {
        Err(LlmError),
        Chunk(StreamChunk),
        ChunkThenErr(StreamChunk, LlmError),
        /// Yield the chunk only after `delay_ms`, so a test can deliver
        /// steering/supplements while the LLM call is in flight.
        ChunkDelayed(StreamChunk, u64),
    }

    impl ScriptedMock {
        fn new(responses: Vec<ScriptedResponse>) -> Self {
            Self {
                stream_responses: std::sync::Mutex::new(VecDeque::from(responses)),
                chat_text: std::sync::Mutex::new("Compacted summary.".into()),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedMock {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            let text = self.chat_text.lock().unwrap().clone();
            Ok(LlmResponse {
                text,
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    model_name: None,
                    cost: None,
                },
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
        }
        async fn chat_stream_with_tools(
            &self,
            messages: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.seen.lock().unwrap().push(messages);
            let resp =
                self.stream_responses
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(ScriptedResponse::Err(LlmError::Unknown(
                        "scripted responses exhausted".into(),
                    )));
            match resp {
                ScriptedResponse::Err(e) => Err(e),
                ScriptedResponse::Chunk(chunk) => Ok(Box::pin(stream::iter(vec![Ok(chunk)]))),
                ScriptedResponse::ChunkThenErr(chunk, e) => {
                    Ok(Box::pin(stream::iter(vec![Ok(chunk), Err(e)])))
                }
                ScriptedResponse::ChunkDelayed(chunk, delay_ms) => {
                    Ok(Box::pin(stream::once(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        Ok(chunk)
                    })))
                }
            }
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    struct EventCollector {
        events: std::sync::Mutex<Vec<AgentEvent>>,
    }
    impl EventCollector {
        fn new() -> Self {
            Self {
                events: std::sync::Mutex::new(Vec::new()),
            }
        }
        fn has_action(&self, tool_name: &str) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::Action { tool_name: tn, .. } if tn == tool_name))
        }
        fn has_observation(&self, tool_name: &str) -> bool {
            self.events.lock().unwrap().iter().any(
                |e| matches!(e, AgentEvent::Observation { tool_name: tn, .. } if tn == tool_name),
            )
        }
        fn interrupted_observations(&self) -> Vec<String> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| {
                    if let AgentEvent::Observation { observation, .. } = e {
                        if observation.contains("Interrupted") {
                            Some(observation.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect()
        }
        fn has_compaction(&self) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::Compaction { .. }))
        }
        fn has_notification(&self) -> Option<(String, String)> {
            self.events.lock().unwrap().iter().find_map(|e| {
                if let AgentEvent::Notification { title, body, .. } = e {
                    Some((title.clone(), body.clone()))
                } else {
                    None
                }
            })
        }
    }
    #[async_trait]
    impl AgentEventEmitter for EventCollector {
        async fn emit(&self, event: AgentEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    struct EchoTool;
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> String {
            "echo".into()
        }
        fn description(&self) -> String {
            "Echo back the input text".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}})
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(
                serde_json::json!({"echoed": input["text"].as_str().unwrap_or("")}),
            ))
        }
    }

    struct TimingState {
        intervals: std::sync::Mutex<Vec<(Instant, Instant)>>,
    }
    impl TimingState {
        fn new() -> Self {
            Self {
                intervals: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    struct TimingTool {
        tool_name: String,
        state: Arc<TimingState>,
    }
    impl TimingTool {
        fn new(name: &str, state: Arc<TimingState>) -> Self {
            Self {
                tool_name: name.into(),
                state,
            }
        }
    }
    #[async_trait]
    impl Tool for TimingTool {
        fn name(&self) -> String {
            self.tool_name.clone()
        }
        fn description(&self) -> String {
            "Delayed tool for parallel testing".into()
        }
        fn risk_level(&self, _: &serde_json::Value) -> RiskLevel {
            RiskLevel::Safe
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _: serde_json::Value,
            _: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            let start = Instant::now();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            self.state
                .intervals
                .lock()
                .unwrap()
                .push((start, Instant::now()));
            Ok(ToolResult::ok(serde_json::json!({"ok": true})))
        }
    }

    #[tokio::test]
    async fn run_session_executes_tool_then_final_answer() {
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "hello"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("echo hello").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");
        assert!(collector.has_action("echo"));
        assert!(collector.has_observation("echo"));
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_empty_tool_call_id_stays_consistent_in_canonical() {
        // Some providers return an empty tool_call_id. The Action side
        // synthesizes a UUID; the canonical assistant declaration must echo
        // the SAME id (not the raw empty string), otherwise the tool result
        // references an id the assistant never declared and the next request
        // is rejected with a 400.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: String::new(), // provider sends empty id
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "hello"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("echo hello").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        // Inspect the saved snapshot's canonical: the assistant declaration
        // and the tool result must share the same (non-empty) id.
        let saved: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        let mut declared: Option<String> = None;
        for m in &saved.canonical {
            if let Some(calls) = &m.tool_calls {
                for tc in calls {
                    assert!(!tc.id.is_empty(), "declared id must not be empty");
                    declared = Some(tc.id.clone());
                }
            }
            if let Some(tid) = &m.tool_call_id {
                assert_eq!(
                    Some(tid),
                    declared.as_ref(),
                    "tool result id must match the assistant's declared call id"
                );
            }
        }
        assert!(
            declared.is_some(),
            "an echo tool call must have been declared"
        );
    }

    #[tokio::test]
    async fn run_session_injects_mid_turn_steering_before_final_content() {
        // A user message sent while the agent is generating its final answer
        // must be injected before the turn ends (between the tool calls and
        // the final content) instead of being deferred until after
        // completion. The final LLM response is delayed so the steering
        // arrives while that call is still in flight; the agent must then
        // re-run with the message in context.
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "hello"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            // Delayed final answer: the steering is added while this call is
            // in flight, so the final-content branch must pick it up.
            ScriptedResponse::ChunkDelayed(
                StreamChunk {
                    text: Some("Done.".into()),
                    tool_calls: vec![CanonicalToolCall {
                        id: "final".into(),
                        name: "final_answer".into(),
                        arguments: serde_json::json!({}),
                    }],
                    finish_reason: Some(FinishReason::Stop),
                    usage: None,
                    model: None,
                    reasoning: None,
                    web_search: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                },
                300,
            ),
            // The re-run after the steering was injected also answers finally.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Understood, continuing in French.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final2".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("echo hello").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let session_id = session.id.clone();
            async move { agent.run_session_from_id(&session_id).await }
        });
        // The second LLM call is `ChunkDelayed` (300 ms) specifically so the
        // steering can land while it streams. Wait until that call has actually
        // STARTED (its `seen` entry is pushed before the delay) instead of
        // racing a fixed wall-clock sleep: under parallel test load the sleep
        // can overshoot past the delay and the steering would land after the
        // turn already completed, flaking the assertion below.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if mock.seen.lock().unwrap().len() >= 2 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second (delayed) LLM call never started"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        executor
            .add_steering(&session.id, "stop and use French")
            .await
            .unwrap();
        let history = run.await.unwrap().unwrap();

        {
            let seen = mock.seen.lock().unwrap();
            assert_eq!(seen.len(), 3, "agent must re-run after mid-turn steering");
            let last_call = seen.last().unwrap();
            assert!(
                last_call
                    .iter()
                    .any(|m| matches!(m.role, CanonicalRole::User)
                        && m.content.iter().any(|c| matches!(
                            c,
                            ContentPart::Text(t) if t.contains("Steering: stop and use French")
                        ))),
                "steering must be injected into the re-run LLM call"
            );
        }
        assert!(
            history.len() >= 3,
            "should have re-run after steering injection"
        );
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_injects_steering_between_tool_calls() {
        // A user message sent while the agent is executing tools is drained
        // at the next step boundary ??between tool calls ??so the final
        // answer is generated with the new context.
        let tools = Arc::new(ToolsManager::new());
        let timing = Arc::new(TimingState::new());
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Running the tool.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "delay_a".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("run tool").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let session_id = session.id.clone();
            async move { agent.run_session_from_id(&session_id).await }
        });
        // Deliver the message while delay_a (200ms) is still executing.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        executor
            .add_steering(&session.id, "add more detail")
            .await
            .unwrap();
        let _ = run.await.unwrap().unwrap();

        {
            let seen = mock.seen.lock().unwrap();
            assert_eq!(
                seen.len(),
                2,
                "no re-run needed: steering is drained at step boundary"
            );
            assert!(
                seen[1].iter().any(|m| matches!(m.role, CanonicalRole::User)
                    && m.content.iter().any(|c| matches!(
                        c,
                        ContentPart::Text(t) if t.contains("Steering: add more detail")
                    ))),
                "steering must be injected into the next LLM call after the tool step"
            );
        }
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_ask_tool_pauses_and_surfaces_question() {
        // The `ask` tool signals the ReAct loop to pause and wait for the
        // user's reply (delivered as a supplement on resume). Verify the session
        // ends Paused and the question is persisted as an assistant message.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I need to clarify before proceeding.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "Which path should I take: A or B?"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            // The loop pauses after `ask`, so a second response is never
            // consumed; include a final_answer anyway to catch regressions
            // where the loop incorrectly continues.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("decide a path").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        // Session must be paused, awaiting the user's answer.
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "ask should pause the session"
        );
        assert!(collector.has_action("ask"));
        assert!(collector.has_observation("ask"));

        // The question must be persisted so the user can see and answer it.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let found = msgs
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Which path should I take"));
        assert!(found, "question should be persisted as assistant message");
    }

    #[tokio::test]
    async fn run_session_ask_resumes_after_user_answer() {
        // After `ask` pauses the session, the user's reply arrives as a
        // supplement; the loop resumes and should reach final_answer.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            // Step 1: agent asks.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Clarifying.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "A or B?"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            // Step 2 (after resume): final answer.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Going with A.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("pick a path").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "ask should pause"
        );

        // User answers; the supplement flips the session back to Pending.
        executor
            .add_supplement(&session.id, "Use option A.")
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused),
            "session should pause again after final answer"
        );
        // The final answer text should be persisted, proving the loop resumed
        // past the `ask` step and reached final_answer.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let answered = msgs
            .iter()
            .any(|m| m.role == "assistant" && m.content.contains("Going with A"));
        assert!(answered, "final answer should be persisted after resume");
    }

    #[tokio::test]
    async fn retry_after_ask_answer_error_keeps_single_history() {
        // Reproduce the reported issue: the agent asks a question, the user
        // answers, the resumed step fails, and the user retries. Every retry
        // must OVERWRITE the previous attempt's persisted output ??the review
        // history should show exactly one question, one answer, one response.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            // Step 1: ask the question.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Asking.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "ask".into(),
                    arguments: serde_json::json!({"question": "Proceed?"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            // Step 2 (after the answer): streams a partial thought, then fails.
            ScriptedResponse::ChunkThenErr(
                StreamChunk {
                    text: Some("Let me think...".into()),
                    tool_calls: vec![],
                    finish_reason: None,
                    usage: None,
                    model: None,
                    reasoning: None,
                    web_search: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                },
                LlmError::Unknown("mock mid-stream failure".into()),
            ),
            // Step 2 retry: final answer.
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Answer accepted.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        agent.set_emitter(Arc::new(EventCollector::new()));
        let session = executor.create_session("ask retry").await.unwrap();

        // Turn 1: the ask pauses the session.
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer)
        );

        // Turn 2: the user answers; the resumed step fails mid-stream.
        executor.add_supplement(&session.id, "Yes").await.unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        let _ = agent.run_session_from_id(&session.id).await;
        // The failed run ended in Error; terminal cleanup removed the session
        // from the working set.
        assert_eq!(executor.get_session_state(&session.id).await, None);

        // Turn 3: retry via continue_session ??Pending ??re-run.
        agent.continue_session(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        agent.run_session_from_id(&session.id).await.unwrap();

        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let steps = agent.db.get_session_steps(&session.id).unwrap();

        // The failed attempt's partial text must be gone (overwritten).
        let partials: Vec<&str> = msgs
            .iter()
            .filter(|m| m.role == "assistant" && m.content.contains("Let me think"))
            .map(|m| m.content.as_str())
            .collect();
        assert!(
            partials.is_empty(),
            "partial output from the failed attempt should be deleted, got {:?}",
            partials
        );
        // Exactly one question and one final answer.
        let questions = msgs
            .iter()
            .filter(|m| m.role == "assistant" && m.content.contains("Proceed?"))
            .count();
        let finals = msgs
            .iter()
            .filter(|m| m.role == "assistant" && m.content.contains("Answer accepted."))
            .count();
        assert_eq!(questions, 1, "ask question must appear exactly once");
        assert_eq!(finals, 1, "final answer must appear exactly once");

        // Step rows from the failed attempt must be overwritten too — the
        // review history stays linear (only branching splits timelines).
        // Thought rows carry no text anymore (the text lives in messages),
        // so count by step number: the failed attempt's step-2 rows must be
        // gone, leaving only the retried step.
        let step2_rows = steps.iter().filter(|s| s.step_number == 2).count();
        assert_eq!(
            step2_rows, 1,
            "step rows from the failed attempt should be deleted, got {:?}",
            steps
        );
    }

    #[tokio::test]
    async fn run_session_notify_tool_emits_notification_without_pausing() {
        // The `notify` tool signals the ReAct loop to emit a Notification
        // event (in-app toast + Windows). Unlike `ask`, it must NOT pause the
        // session: the loop continues to the final answer.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::notify::NotifyTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Notifying the user.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "notify".into(),
                    arguments: serde_json::json!({"title": "Build", "body": "Compilation finished"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("build and notify").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");

        // The Notification event must carry the tool's title/body.
        let (title, body) = collector
            .has_notification()
            .expect("notify should emit a Notification event");
        assert_eq!(title, "Build");
        assert_eq!(body, "Compilation finished");

        // The chat/review observation must be readable, not raw JSON.
        assert!(collector.has_observation("notify"));

        // Unlike `ask`, notify must not pause the session mid-loop: the loop
        // continued past the notify step (history has 2 steps) and reached the
        // normal end state (Paused = conversation mode, waiting for follow-up).
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_multiple_asks_surface_all_questions() {
        // Two `ask` calls in one batch must both be surfaced (joined into one
        // assistant message), not just the first.
        let tools = Arc::new(ToolsManager::new());
        tools
            .registry
            .register(Arc::new(haven_tools::builtin::ask::AskTool) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("Two questions.".into()),
                tool_calls: vec![
                    CanonicalToolCall {
                        id: "tc1".into(),
                        name: "ask".into(),
                        arguments: serde_json::json!({"question": "First?"}),
                    },
                    CanonicalToolCall {
                        id: "tc2".into(),
                        name: "ask".into(),
                        arguments: serde_json::json!({"question": "Second?"}),
                    },
                ],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("two questions").await.unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer),
            "ask should pause"
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let persisted: String = msgs
            .iter()
            .filter(|m| m.role == "assistant")
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            persisted.contains("First?"),
            "first question missing: {}",
            persisted
        );
        assert!(
            persisted.contains("Second?"),
            "second question missing: {}",
            persisted
        );
    }

    #[tokio::test]
    async fn run_session_parallel_tool_execution() {
        let tools = Arc::new(ToolsManager::new());
        let timing = Arc::new(TimingState::new());
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
            .await;
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Running both in parallel.".into()),
                tool_calls: vec![
                    CanonicalToolCall {
                        id: "tc1".into(),
                        name: "delay_a".into(),
                        arguments: serde_json::json!({}),
                    },
                    CanonicalToolCall {
                        id: "tc2".into(),
                        name: "delay_b".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("run parallel").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(collector.has_action("delay_a"));
        assert!(collector.has_action("delay_b"));
        let step1_tool_entries = history
            .iter()
            .filter(|s| s.step_number == 1 && s.action.is_some())
            .count();
        assert_eq!(
            step1_tool_entries, 2,
            "each parallel tool must have its own history entry (the old code kept only the last one)"
        );
        let mut intervals = timing.intervals.lock().unwrap().clone();
        assert_eq!(intervals.len(), 2, "both tools should have executed");
        intervals.sort_by_key(|(start, _)| *start);
        let (_, a_end) = intervals[0];
        let (b_start, _) = intervals[1];
        assert!(
            b_start < a_end,
            "tools should execute in parallel (overlap)"
        );
    }

    #[tokio::test]
    async fn run_session_cancelled_mid_batch_surfaces_interrupted_tools() {
        // A tool batch cancelled mid-flight must NOT silently drop the
        // in-flight calls: each one is repaired with an "Interrupted"
        // observation (so the UI shows it and the model can retry) and the
        // snapshot canonical stays a valid assistant/tool chain.
        let tools = Arc::new(ToolsManager::new());
        let timing = Arc::new(TimingState::new());
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_a", timing.clone())) as ToolBox)
            .await;
        tools
            .registry
            .register(Arc::new(TimingTool::new("delay_b", timing.clone())) as ToolBox)
            .await;
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Chunk(
            StreamChunk {
                text: Some("Running both in parallel.".into()),
                tool_calls: vec![
                    CanonicalToolCall {
                        id: "tc1".into(),
                        name: "delay_a".into(),
                        arguments: serde_json::json!({}),
                    },
                    CanonicalToolCall {
                        id: "tc2".into(),
                        name: "delay_b".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        )]));
        let (agent, executor) = make_test_agent_with(mock.clone(), tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("run parallel").await.unwrap();

        let run = tokio::spawn({
            let agent = agent.clone();
            let session_id = session.id.clone();
            async move { agent.run_session_from_id(&session_id).await }
        });
        // Wait until both action events were emitted (the assistant message
        // with tool_calls is in canonical and the drain loop is running), then
        // cancel while both tools (200ms sleeps) are still in flight.
        for _ in 0..50 {
            if collector.has_action("delay_a") && collector.has_action("delay_b") {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            collector.has_action("delay_a") && collector.has_action("delay_b"),
            "batch must have started before the cancel"
        );
        // end_session registers a real cancellation token (entry().or_insert) and
        // cancels it — the same path the frontend's "end session" button uses —
        // so the in-flight tool batch observes the cancellation mid-drain.
        executor.end_session(&session.id).await.unwrap();
        let history = run.await.unwrap().unwrap();

        // Every in-flight tool got an "Interrupted" observation emitted to
        // the UI — the cancelled tools are not silently skipped.
        let interrupted = collector.interrupted_observations();
        assert_eq!(
            interrupted.len(),
            2,
            "both in-flight tools must emit an Interrupted observation"
        );
        // The observation must carry the tool name and the attempted arguments
        // (field supplementation), not a bare "Interrupted" marker, so the UI
        // and the model can see exactly which call was cut off.
        let all_text = interrupted.join("\n");
        for tool in ["delay_a", "delay_b"] {
            assert!(
                all_text.contains(tool),
                "interrupted observation must name tool '{}' (got: {})",
                tool,
                all_text
            );
        }
        assert!(
            all_text.contains("arguments"),
            "interrupted observation must carry the attempted arguments (got: {})",
            all_text
        );
        // The history recorded the interrupted steps so a resume keeps them.
        let interrupted_steps = history
            .iter()
            .filter(|s| {
                s.observation
                    .as_deref()
                    .is_some_and(|o| o.contains("Interrupted"))
            })
            .count();
        assert_eq!(
            interrupted_steps, 2,
            "interrupted tool calls must be recorded in history"
        );
        // The history entries must also carry the enriched fields.
        assert!(
            history
                .iter()
                .all(|s| s.observation.as_deref().is_none_or(|o| {
                    !o.contains("Interrupted") || (o.contains("tool:") && o.contains("arguments"))
                })),
            "interrupted history observations must carry tool name and arguments"
        );
        // The saved snapshot canonical stays a valid assistant/tool chain: no
        // dangling assistant tool_calls without a following result (which
        // providers would reject as a 400 on resume).
        let state_json = agent
            .db
            .get_react_state(&session.id)
            .unwrap()
            .expect("exit snapshot must be saved after mid-batch cancel");
        let snapshot: ReActSnapshot = serde_json::from_str(&state_json).unwrap();
        let mut pending: Vec<String> = Vec::new();
        let mut interrupted_with_fields = 0;
        for m in &snapshot.canonical {
            match m.role {
                CanonicalRole::Tool => {
                    if let Some(cid) = &m.tool_call_id {
                        if let Some(pos) = pending.iter().position(|p| p == cid) {
                            pending.remove(pos);
                        }
                    } else if let Some(cid) = pending.pop() {
                        let _ = cid;
                    }
                    // The repaired Interrupted tool results must carry the
                    // tool name and arguments so a resume sees what happened.
                    if m.content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Text(t) if t.contains("Interrupted")))
                    {
                        interrupted_with_fields += 1;
                        assert!(
                            m.content.iter().any(|p| matches!(
                                p,
                                ContentPart::Text(t) if t.contains("tool:") && t.contains("arguments")
                            )),
                            "repaired Interrupted result must include tool name and arguments: {:?}",
                            m.content
                        );
                    }
                }
                CanonicalRole::Assistant => {
                    pending = m
                        .tool_calls
                        .as_ref()
                        .map(|tc| tc.iter().map(|t| t.id.clone()).collect())
                        .unwrap_or_default();
                }
                _ => {}
            }
        }
        assert_eq!(
            interrupted_with_fields, 2,
            "both interrupted tool results in the snapshot must carry fields"
        );
        assert!(
            pending.is_empty(),
            "snapshot canonical must not end with unanswered tool_calls (got {:?})",
            pending
        );
        // Pending step rows created at Action emit must be completed with the
        // Interrupted observation so review/resume rebuilds the tool cards
        // from session_steps (not live-only UI state).
        let db_steps = agent.db.get_session_steps(&session.id).unwrap();
        let interrupted_db = db_steps
            .iter()
            .filter(|s| {
                s.action_tool.is_some()
                    && s.observation
                        .as_deref()
                        .is_some_and(|o| o.contains("Interrupted"))
            })
            .count();
        assert_eq!(
            interrupted_db, 2,
            "interrupted tools must be persisted in session_steps for UI rebuild (got {:?})",
            db_steps
                .iter()
                .map(|s| (s.action_tool.clone(), s.status.clone(), s.observation.clone()))
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn pause_snapshot_and_resume_keep_own_final_answer_in_canonical() {
        // The pause snapshot must end with the agent's own final answer (not
        // right after the tool results), so a resume sees the completed
        // answer BEFORE the follow-up instead of having the re-seed re-insert
        // it at the transcript head, out of order.
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(SessionExecutor::new(db.clone(), tools, 1));
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("First answer.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Second answer.".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let client: Arc<dyn LlmClient> = mock.clone();
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(
            db.clone(),
            executor.clone(),
            router,
            30,
            50,
            ContextLimitsConfig::default(),
        ));
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("question one").await.unwrap();

        agent.run_session_from_id(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );

        let state_json = db
            .get_react_state(&session.id)
            .unwrap()
            .expect("snapshot must exist after the pause");
        let snapshot: ReActSnapshot = serde_json::from_str(&state_json).unwrap();
        let last = snapshot.canonical.last().expect("canonical not empty");
        assert_eq!(
            last.role,
            CanonicalRole::Assistant,
            "pause snapshot canonical must end with the agent's own answer"
        );
        let snapshot_text: String = last
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            snapshot_text.contains("First answer."),
            "snapshot canonical must carry the final answer, got: {snapshot_text:?}"
        );

        // Resume with a follow-up: the next LLM request must show the agent's
        // own completed answer BEFORE the injected follow-up, so the model
        // answers with knowledge of what it already said.
        executor
            .add_supplement(&session.id, "next question")
            .await
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        agent.run_session_from_id(&session.id).await.unwrap();

        let seen = mock.seen.lock().unwrap();
        assert!(
            seen.len() >= 2,
            "expected a resumed request, got {:?}",
            seen.len()
        );
        let last_req = seen.last().unwrap();
        let idx_answer = last_req.iter().position(|m| {
            matches!(m.role, CanonicalRole::Assistant)
                && m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("First answer.")))
        });
        let idx_followup = last_req.iter().position(|m| {
            matches!(m.role, CanonicalRole::User)
                && m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Text(t) if t.contains("next question")))
        });
        let roles: Vec<String> = last_req.iter().map(|m| m.role.to_string()).collect();
        assert!(
            idx_answer.is_some(),
            "resumed request must contain the agent's own answer, roles: {roles:?}"
        );
        assert!(
            idx_followup.is_some(),
            "resumed request must contain the follow-up, roles: {roles:?}"
        );
        assert!(
            idx_answer.unwrap() < idx_followup.unwrap(),
            "the agent's own answer must precede the follow-up message"
        );
    }

    #[tokio::test]
    async fn run_session_compaction_retry_on_context_exceeded() {
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Calling echo.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: serde_json::json!({"text": "data"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            ScriptedResponse::Err(LlmError::ContextLengthExceeded),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done after compaction.".into()),
                tool_calls: vec![CanonicalToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: serde_json::json!({}),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("test compaction").await.unwrap();
        let history = agent.run_session_from_id(&session.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(
            collector.has_compaction(),
            "Compaction event should be emitted"
        );
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn run_session_context_exceeded_compaction_fails() {
        let tools = Arc::new(ToolsManager::new());
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Err(
            LlmError::ContextLengthExceeded,
        )]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let session = executor.create_session("compaction fail").await.unwrap();
        let result = agent.run_session_from_id(&session.id).await;
        assert!(result.is_err(), "should error when compaction fails");
        // Terminal cleanup removed the session from the working set.
        assert_eq!(executor.get_session_state(&session.id).await, None);
    }

    #[tokio::test]
    async fn continue_session_resumes_errored_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("test continue").await.unwrap();
        // Simulate an errored session with a saved snapshot.
        agent
            .db
            .update_session_status(&session.id, "error")
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();
        let snapshot = ReActSnapshot {
            canonical: vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }],
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        // Add a partial assistant message that should be cleaned up.
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        // The partial lands strictly AFTER the user row (the continue
        // truncation deletes rows created_at > the last user message).
        std::thread::sleep(std::time::Duration::from_millis(5));
        agent
            .db
            .add_message(
                &session.id,
                "assistant",
                "partial output",
                Some("text"),
                None,
            )
            .unwrap();

        agent.continue_session(&session.id).await.unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        // The partial output should have been deleted (only the user message remains).
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[tokio::test]
    async fn continue_session_non_error_fails() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("not error").await.unwrap();
        // Session is Pending, not Error ??should refuse.
        let result = agent.continue_session(&session.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rollback_without_react_state_truncates_messages() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("no state").await.unwrap();
        // No react_state saved ??simulate an old session that errored before
        // snapshots were persisted.
        agent
            .db
            .update_session_status(&session.id, "error")
            .unwrap();
        executor
            .update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "partial", Some("text"), None)
            .unwrap();
        let hello_id = agent
            .db
            .get_session_messages(&session.id)
            .unwrap()
            .into_iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id;

        // User-message rollback (pause=true) should truncate from the user msg.
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(msgs.is_empty(), "messages should be empty after rollback");
    }

    #[tokio::test]
    async fn rollback_with_snapshot_no_branch_point_uses_snapshot() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("no bp").await.unwrap();
        // Save a snapshot with no branch_points at the target step.
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let snapshot = ReActSnapshot {
            canonical: canonical.clone(),
            history: vec![],
            step_number: 1,
            branch_points: HashMap::new(),
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        // The partial must land strictly after the user row (rollback
        // truncates rows created_at > the snapshot's last message ts).
        std::thread::sleep(std::time::Duration::from_millis(5));
        agent
            .db
            .add_message(&session.id, "assistant", "partial", Some("text"), None)
            .unwrap();

        // Rollback to step 1 with pause=false (agent rollback).
        agent
            .rollback_session(&session.id, 1, false, None)
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        // The partial assistant message should be deleted, user message kept.
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].content, "hello");
    }

    #[tokio::test]
    async fn rollback_pause_true_removes_user_message_from_session() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("user rollback").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        // Branch point at step 1: canonical ends at the user message, but
        // last_msg_at points at the thought that was persisted AFTER it (the
        // realistic shape saved by save_branch_point).
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
        let thought_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(thought_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points,
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // User-message rollback: the user message itself must be removed from
        // the session (its text returns to the composer for editing) ??not
        // left behind to reappear on the next review rebuild.
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.is_empty(),
            "user message should be deleted from the session, got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn rollback_fallback_no_branch_point_pause_true_deletes_from_last_user_message() {
        // Regression: rollback to a step that has NO branch point (e.g. the
        // step failed before save_branch_point ran) falls back to a cutoff
        // derived from session messages. With pause=true the user message
        // itself must be removed too — and because the clicked message's
        // live-view id never matches a DB id, the backend can only guess the
        // target from the newest user message at/before the cutoff.
        let (agent, executor) = make_test_agent();
        let session = executor
            .create_session("fallback user rollback")
            .await
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "first", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply1", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "second", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply2", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let reply1_ts = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.content == "reply1")
            .unwrap()
            .created_at
            .clone();
        let second_id = msgs
            .iter()
            .find(|m| m.content == "second")
            .unwrap()
            .id
            .clone();
        // Snapshot with a branch point ONLY at step 1; the target step 2 has
        // no branch point (realistic: step 2's save_branch_point never ran).
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(reply1_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 2,
            branch_points,
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // The user clicked "second" (the newest user message, whose id
        // resolves to a persisted row).
        agent
            .rollback_session(&session.id, 2, true, Some(&second_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["first", "reply1"],
            "rollback must delete the clicked user message and everything after it: {:?}",
            contents
        );
    }

    #[tokio::test]
    async fn rollback_errors_when_target_message_id_does_not_match() {
        // Regression: user-message rollback used to fall back to matching by
        // content when the clicked message's id missed, and to guessing the
        // newest user message when even that failed. Both guesses could
        // delete the wrong message; an unresolvable id is now a direct error
        // and the session is left untouched.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("strict rollback").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "first question", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply A", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "user", "second question", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "reply B", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let reply_a_ts = msgs
            .iter()
            .find(|m| m.role == "assistant" && m.content == "reply A")
            .unwrap()
            .created_at
            .clone();
        // Branch point at step 1 only; target step 2 has none (fallback).
        let canonical = vec![CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("sys")],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(reply_a_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 2,
            branch_points,
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // The live-view id never matches a DB id and no content fallback
        // exists anymore: rollback must error and delete nothing.
        let err = agent
            .rollback_session(&session.id, 1, true, Some("live-view-local-id"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not found in session messages"),
            "unexpected error: {}",
            err
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(msgs.len(), 4, "no message may be deleted on error");
    }

    /// Seed the common rollback-test fixture: messages `hello` / `thinking` /
    /// `interrupt` plus a saved ReAct snapshot at step 1 (canonical =
    /// [System "sys", User "hello"], branch point after the thinking turn).
    /// Returns the persisted messages so tests can resolve specific ids.
    fn seed_hello_snapshot(
        agent: &AgentLayer,
        session_id: &str,
    ) -> Vec<haven_memory::repositories::messages::Message> {
        agent
            .db
            .add_message(session_id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(session_id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(session_id, "user", "interrupt", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(session_id).unwrap();
        let thinking_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(thinking_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points,
            saved_at: None,
        };
        agent
            .db
            .save_react_state(session_id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();
        msgs
    }

    #[tokio::test]
    async fn rollback_orphan_after_processed_turn_preserves_earlier_history() {
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("orphan rollback").await.unwrap();
        let msgs = seed_hello_snapshot(&agent, &session.id);

        // Roll back the interrupted message: only it must be discarded; the
        // earlier exchange ("hello" / "thinking") survives.
        let interrupt_id = msgs
            .iter()
            .find(|m| m.content == "interrupt")
            .unwrap()
            .id
            .clone();
        agent
            .rollback_session(&session.id, 1, true, Some(&interrupt_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let contents: Vec<&str> = msgs.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            contents,
            vec!["hello", "thinking"],
            "orphan rollback must not wipe earlier history, got {:?}",
            contents
        );
        // The canonical must NOT be truncated: "hello" is a legitimately
        // processed message and stays in the restored context.
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "orphan rollback must not truncate the processed user message from canonical"
        );
    }

    #[tokio::test]
    async fn rollback_processed_user_message_with_later_orphan_wipes_target_timeline() {
        // Same layout as the orphan test, but the user rolls back the
        // PROCESSED message ("hello") rather than the orphan. The orphan's
        // existence must not hijack the rollback: deleting from the target's
        // own timestamp also discards the later orphan (it belongs to the
        // discarded timeline), and the canonical IS truncated.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("processed rollback").await.unwrap();
        let msgs = seed_hello_snapshot(&agent, &session.id);

        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.is_empty(),
            "rollback of the processed message must wipe the orphan too, got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "rollback of a processed message must truncate the canonical"
        );
    }

    #[tokio::test]
    async fn rollback_pause_uses_target_message_ts_not_latest_user() {
        // A steering interjection persisted between the rolled-back user
        // message and the branch point must NOT hijack the delete range: the
        // target message's own timestamp wins, so the target is removed too.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("target ts").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "hello", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        // A steering interjection persisted after "hello" but BEFORE the
        // branch-point thought timestamp (the user typed while the agent was
        // working on the first step).
        agent
            .db
            .add_message(
                &session.id,
                "user",
                "also check the time",
                Some("text"),
                None,
            )
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let hello_id = msgs
            .iter()
            .find(|m| m.content == "hello")
            .unwrap()
            .id
            .clone();
        let thinking_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 1,
                last_msg_at: Some(thinking_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 1,
            branch_points,
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        // Roll back "hello" specifically —the steering interjection must
        // NOT keep "hello" alive.
        agent
            .rollback_session(&session.id, 1, true, Some(&hello_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert!(
            msgs.is_empty(),
            "rolling back 'hello' must delete it (and the interjection), got {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User),
            "canonical must not keep the rolled-back user message"
        );
    }

    #[tokio::test]
    async fn rollback_pause_matches_prefixed_supplement_in_canonical() {
        // The canonical stores supplement/steering inputs with a prefix
        // ("Steering: —, "Additional context from user: —) while the DB
        // stores the raw text. Rolling back such a message must find the
        // prefixed canonical entry (not merely the last User), so the
        // message is removed from the restored context.
        let (agent, executor) = make_test_agent();
        let session = executor.create_session("prefixed rollback").await.unwrap();
        agent
            .db
            .add_message(&session.id, "user", "do it", Some("text"), None)
            .unwrap();
        // The steering is injected BEFORE the step's LLM call, so it is
        // persisted before the branch-point thought timestamp.
        agent
            .db
            .add_message(&session.id, "user", "use French", Some("text"), None)
            .unwrap();
        agent
            .db
            .add_message(&session.id, "assistant", "thinking", Some("text"), None)
            .unwrap();
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        let steering_id = msgs
            .iter()
            .find(|m| m.content == "use French")
            .unwrap()
            .id
            .clone();
        let thinking_ts = msgs
            .iter()
            .find(|m| m.role == "assistant")
            .unwrap()
            .created_at
            .clone();
        let canonical = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("sys")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("do it")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            // The steering was pushed into the canonical with its prefix.
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("Steering: use French")],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            2,
            BranchPoint {
                canonical: canonical.clone(),
                history: vec![],
                step_number: 2,
                last_msg_at: Some(thinking_ts),
            },
        );
        let snapshot = ReActSnapshot {
            canonical,
            history: vec![],
            step_number: 2,
            branch_points,
            saved_at: None,
        };
        agent
            .db
            .save_react_state(&session.id, &serde_json::to_string(&snapshot).unwrap())
            .unwrap();

        agent
            .rollback_session(&session.id, 2, true, Some(&steering_id))
            .await
            .unwrap();
        assert_eq!(
            executor.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let msgs = agent.db.get_session_messages(&session.id).unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "the steering message itself must be deleted, 'do it' stays: {:?}",
            msgs.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
        assert_eq!(msgs[0].content, "do it");
        let restored: ReActSnapshot =
            serde_json::from_str(&agent.db.get_react_state(&session.id).unwrap().unwrap()).unwrap();
        assert!(
            !restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User
                    && m.content.iter().any(|p| matches!(
                        p,
                        ContentPart::Text(t) if t.contains("use French")
                    ))),
            "the prefixed steering entry must be trimmed from the canonical"
        );
        assert!(
            restored
                .canonical
                .iter()
                .any(|m| m.role == CanonicalRole::User
                    && m.content
                        .iter()
                        .any(|p| matches!(p, ContentPart::Text(t) if t == "do it"))),
            "'do it' must stay in the canonical"
        );
    }
}
