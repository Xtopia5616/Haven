use std::collections::HashMap;
use std::sync::Arc;

mod compactor;
mod event;
mod inference;
mod prompt;
mod react;
mod session;
mod title;
mod types;

pub use compactor::ContextCompactor;
pub use event::{AgentEvent, AgentEventEmitter, EventBus, EventDispatcher};
pub use haven_task::{RunHandler, TaskExecutor, TaskInfo, TaskPriority, TaskStatus};
pub use inference::InferenceEngine;
pub use prompt::SystemPromptBuilder;
pub use react::ReActEngine;
pub use session::SessionManager;
pub use types::{Action, BranchPoint, ProcessResult, ReActSnapshot, ReActStep};

use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::LlmRouter;
use haven_memory::Database;

use crate::title::TitleGenerator;

pub struct AgentLayer {
    db: Arc<Database>,
    executor: Arc<TaskExecutor>,
    sessions: Arc<SessionManager>,
    events: Arc<EventDispatcher>,
    prompt_builder: Arc<SystemPromptBuilder>,
    react_engine: Arc<ReActEngine>,
    inference: Arc<InferenceEngine>,
    title: Option<TitleGenerator>,
}

impl AgentLayer {
    pub fn new(
        db: Arc<Database>,
        executor: Arc<TaskExecutor>,
        router: Arc<LlmRouter>,
        max_steps: u32,
        session_window_size: usize,
        max_observation_chars: usize,
        small_model_endpoint: Option<haven_common::config::ModelEndpoint>,
    ) -> Self {
        let sessions = Arc::new(SessionManager::new(db.clone(), session_window_size));
        let events = Arc::new(EventDispatcher::new());
        let prompt_builder = Arc::new(SystemPromptBuilder::new(executor.get_tools(), db.clone()));
        let react_engine = Arc::new(ReActEngine::new(
            router,
            executor.clone(),
            db.clone(),
            max_steps,
            max_observation_chars,
        ));
        let inference = Arc::new(InferenceEngine::new(db.clone(), sessions.clone()));
        let _ = db.set_preference("name", "Xtopia");
        let _ = db.insert_fact("user", "name", "Xtopia", "user", 1.0);
        let title = small_model_endpoint.map(TitleGenerator::new);

        Self {
            db,
            executor,
            sessions,
            events,
            prompt_builder,
            react_engine,
            inference,
            title,
        }
    }

    pub fn ensure_session(&self) -> String {
        self.sessions.ensure_session()
    }

    pub fn start_new_session(&self) -> anyhow::Result<String> {
        self.sessions.start_new_session()
    }

    fn persist_message(&self, role: &str, content: &str, message_type: Option<&str>) {
        self.sessions.persist_message(role, content, message_type);
    }

    /// Persist user input and inject it as a supplement into a running task,
    /// bypassing the classifier. If the task has already completed or failed,
    /// it is re-opened so the dispatcher picks it up again with the new context.
    pub async fn supplement_task(&self, task_id: &str, text: &str) -> anyhow::Result<()> {
        // Switch to the task's session so the supplement message and
        // subsequent conversation history load from the correct session.
        if let Ok(Some(task)) = self.db.get_task(task_id)
            && let Some(ref sid) = task.session_id
        {
            self.sessions.switch_to_session(sid);
        }
        self.persist_message("user", text, Some("text"));
        let was_in_memory = self.executor.add_supplement(task_id, text).await.is_ok();
        if !was_in_memory {
            // Task not in executor memory (e.g. after app restart). Load from
            // DB, re-add supplement, then let the dispatcher pick it up.
            self.executor.ensure_task_loaded(task_id).await?;
            self.executor.add_supplement(task_id, text).await?;
        }
        let state = self.executor.get_task_state(task_id).await;
        if state == TaskStatus::Paused {
            // The ReAct loop has already exited (status set to Paused and
            // returned).  Always hand off to the dispatcher by setting
            // Pending — the supplement_queue will cause take_next_pending to
            // pick it up within 100 ms regardless of dispatched_once.
            self.executor
                .update_task_status(task_id, TaskStatus::Pending)
                .await?;
            self.events.emit_task_updated(task_id, "pending").await;
        } else if state == TaskStatus::Completed
            || state == TaskStatus::Error
            || state == TaskStatus::Cancelled
        {
            self.executor
                .update_task_status(task_id, TaskStatus::Pending)
                .await?;
            self.events.emit_task_updated(task_id, "pending").await;
        }
        Ok(())
    }

    /// Reopen a terminal task to Paused state.
    /// Used by the history review flow — shows the task as active on the chat
    /// page.  The dispatcher won't pick it up until the user sends a
    /// follow-up message (which calls supplement_task → Paused→Pending).
    pub async fn reopen_task(&self, task_id: &str) -> anyhow::Result<()> {
        // Ensure the task is in executor memory (load from DB if removed).
        let state = self.executor.get_task_state(task_id).await;
        if state == TaskStatus::Cancelled {
            self.executor.ensure_task_loaded(task_id).await?;
        }
        let state = self.executor.get_task_state(task_id).await;
        if state == TaskStatus::Completed
            || state == TaskStatus::Error
            || state == TaskStatus::Cancelled
        {
            self.executor
                .update_task_status(task_id, TaskStatus::Paused)
                .await?;
            self.events.emit_task_updated(task_id, "paused").await;
        }
        Ok(())
    }

    pub fn set_emitter(&self, emitter: Arc<dyn AgentEventEmitter>) {
        self.events.set_emitter(emitter);
    }

    /// Install an `EventBus` as the active emitter and return it so callers
    /// can register multiple subscribers via `subscribe`.
    pub fn install_event_bus(&self) -> Arc<EventBus> {
        self.events.install_bus()
    }

    pub fn replace_router(&self, new_router: Arc<LlmRouter>) {
        self.react_engine.replace_router(new_router);
    }

    pub fn set_max_steps(&self, max_steps: u32) {
        self.react_engine.set_max_steps(max_steps);
    }

    /// Spawn the TaskExecutor dispatcher with a runner wired to this
    /// AgentLayer. Must be called exactly once after construction.
    pub fn start(self: Arc<Self>) {
        let agent = self.clone();
        let executor = self.executor.clone();
        let handler: RunHandler = Arc::new(move |task_id: String| {
            let agent = agent.clone();
            Box::pin(async move { agent.run_task_from_id(&task_id).await.map(|_| ()) })
        });
        executor.start_dispatcher(handler);
    }

    fn load_conversation_history(&self) -> Vec<String> {
        self.sessions.load_conversation_history()
    }

    /// Roll back a task to a specific branch point. The task state is
    /// replaced with the branch point snapshot, session messages persisted
    /// after that point are deleted, branch points created after the target
    /// step are pruned, and the task is set back to Pending so the dispatcher
    /// re-executes it.
    pub async fn rollback_task(&self, task_id: &str, target_step: u32) -> anyhow::Result<()> {
        let state_json = self
            .db
            .get_react_state(task_id)?
            .ok_or_else(|| anyhow::anyhow!("no saved state for task {}", task_id))?;
        let mut snapshot: ReActSnapshot = serde_json::from_str(&state_json)?;
        let bp = snapshot
            .branch_points
            .get(&target_step)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no branch point at step {}", target_step))?;

        // Restore the canonical/history/step from the branch point.
        snapshot.canonical = bp.canonical;
        snapshot.history = bp.history;
        snapshot.step_number = bp.step_number;

        // If the branch point was saved right after an assistant tool_call
        // message but before the tool results were appended, the canonical
        // array ends with an assistant message carrying `tool_calls` but no
        // matching tool-result messages. Sending this to the LLM triggers a
        // 400 error (dangling tool_call). Trim such a trailing assistant
        // message so the loop re-requests the tool call cleanly.
        // Also drop the corresponding half-built history step (action: None).
        if snapshot
            .canonical
            .last()
            .is_some_and(|m| m.role == CanonicalRole::Assistant && m.tool_calls.is_some())
        {
            snapshot.canonical.pop();
            // The history step for this step has thought set but action=None.
            if snapshot.history.last().is_some_and(|s| s.action.is_none()) {
                snapshot.history.pop();
            }
        }

        // Prune branch points that were created after the target step so the
        // session tree does not accumulate stale forks from the discarded
        // timeline.
        snapshot.branch_points.retain(|&k, _| k <= target_step);

        // Truncate session messages persisted after the branch point so the
        // conversation context matches the restored snapshot. The session_id
        // is used directly — no need to mutate the global SessionManager.
        if let Ok(Some(task)) = self.db.get_task(task_id)
            && let Some(ref sid) = task.session_id
            && let Some(ref ts) = bp.last_msg_at
        {
            self.db.delete_messages_after(sid, ts)?;
        }

        let json = serde_json::to_string(&snapshot)?;
        self.db.save_react_state(task_id, &json)?;

        // Reset dispatched_once so the dispatcher picks the task up again.
        // The supplement/steering queues are empty after a pure rollback, so
        // without this take_next_pending would skip it indefinitely.
        self.executor.reset_dispatched_once(task_id).await;

        self.executor
            .update_task_status(task_id, TaskStatus::Pending)
            .await?;
        self.events.emit_task_updated(task_id, "pending").await;
        tracing::info!(
            "rollback_task {} to step {}: task set to Pending",
            task_id,
            target_step
        );
        Ok(())
    }

    /// Dispatcher entrypoint. Looks up the task by id, fills in the
    /// classifier summary (description) and original transcript (context),
    /// loads conversation history, then runs the ReAct loop.
    pub async fn run_task_from_id(&self, task_id: &str) -> anyhow::Result<Vec<ReActStep>> {
        tracing::debug!("run_task_from_id: task_id={}", task_id);
        let task = self
            .executor
            .list_tasks()
            .await
            .into_iter()
            .find(|t| t.id == task_id)
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found by dispatcher", task_id))?;

        let run_id = self.react_engine.next_run_id();
        self.react_engine.set_current_run_id(run_id);

        let description = if task.summary.is_empty() {
            task.input.clone()
        } else {
            task.summary.clone()
        };
        let context = task.input.clone();

        // Switch to the task's session so conversation history and
        // subsequent message persistence target the correct session.
        // This matters for rollback/fork where the SessionManager may be
        // pointing at a different session than the dispatched task.
        if let Ok(Some(db_task)) = self.db.get_task(task_id)
            && let Some(ref sid) = db_task.session_id
        {
            self.sessions.switch_to_session(sid);
        }
        let conv_history = self.load_conversation_history();
        let tools = self.prompt_builder.build_tool_definitions().await;

        let result = if let Ok(Some(state_json)) = self.db.get_react_state(task_id)
            && let Ok(snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            tracing::info!(
                "restoring ReAct state for task {} ({} steps)",
                task_id,
                snapshot.history.len()
            );
            self.run_task_resumed(task_id, snapshot, &conv_history, &tools)
                .await
        } else {
            self.run_task(&task.id, &description, &context, &conv_history, &tools)
                .await
        };

        // Generate title after first ReAct loop if not already set
        if task.title.is_none() {
            let db = self.db.clone();
            let executor = self.executor.clone();
            let title = self.title.clone();
            let events = self.events.clone();
            let tid = task_id.to_string();
            tokio::spawn(async move {
                Self::try_generate_title(db, executor, title, events, tid).await;
            });
        }

        result
    }

    pub async fn emit_task_completed(&self, task_id: &str, title: &str) {
        self.events.emit_task_completed(task_id, title).await;
    }

    /// Generate a short title using small_model after the first ReAct loop
    /// completes. Spawned as a background task so it does not block the
    /// dispatcher. Only runs once per task (when title is None).
    async fn try_generate_title(
        db: Arc<Database>,
        executor: Arc<TaskExecutor>,
        title: Option<TitleGenerator>,
        events: Arc<EventDispatcher>,
        task_id: String,
    ) {
        let Some(generator) = title else { return };
        // Check if the task already has a title in the DB
        if let Ok(Some(task)) = db.get_task(&task_id)
            && task.title.is_some()
        {
            return;
        }
        // Build conversation context from session messages
        let messages = if let Ok(Some(task)) = db.get_task(&task_id)
            && let Some(ref sid) = task.session_id
        {
            db.get_session_messages_limit(sid, 10).unwrap_or_default()
        } else {
            return;
        };
        let conv_lines: Vec<String> = messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect();
        let title = match generator.generate(&conv_lines).await {
            Some(t) => t,
            None => return,
        };
        // Save to DB
        if let Err(e) = db.update_task_title(&task_id, &title) {
            tracing::warn!("failed to save generated title: {}", e);
            return;
        }
        // Update in-memory TaskInfo in executor
        executor.update_task_title(&task_id, &title).await;
        // Notify frontend
        events.emit_title_updated(&task_id, &title).await;
        tracing::info!("generated title for task {}: {}", task_id, title);
    }

    async fn run_task_resumed(
        &self,
        task_id: &str,
        snapshot: ReActSnapshot,
        conversation_history: &[String],
        tools: &[haven_llm::ToolDefinition],
    ) -> anyhow::Result<Vec<ReActStep>> {
        let mut history = snapshot.history;
        let mut canonical = snapshot.canonical;
        let start_step = snapshot.step_number;
        let mut branch_points = snapshot.branch_points;

        if !conversation_history.is_empty() {
            let sys_end = canonical
                .iter()
                .position(|m| m.role != CanonicalRole::System)
                .unwrap_or(1);
            for msg in conversation_history {
                canonical.insert(
                    sys_end,
                    CanonicalMessage {
                        role: CanonicalRole::User,
                        content: vec![ContentPart::text(format!("[conversation] {}", msg))],
                        tool_calls: None,
                        tool_call_id: None,
                        parent_message_id: None,
                    },
                );
            }
        }

        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let infer = || {
            self.inference.infer_all();
        };
        self.react_engine
            .run_react_loop(
                task_id,
                &mut canonical,
                &mut history,
                start_step,
                &mut branch_points,
                emitter_arc,
                &self.sessions,
                tools,
                &infer,
            )
            .await?;
        Ok(history)
    }

    pub async fn run_task(
        &self,
        task_id: &str,
        description: &str,
        context: &str,
        conversation_history: &[String],
        tools: &[haven_llm::ToolDefinition],
    ) -> anyhow::Result<Vec<ReActStep>> {
        tracing::debug!(
            "run_task start: task_id={:?} context={:?}",
            task_id,
            context
        );
        let mut history: Vec<ReActStep> = Vec::new();
        let system_prompt = self
            .prompt_builder
            .build(description, &[], conversation_history)
            .await;
        tracing::debug!("run_task: system_prompt {} chars", system_prompt.len());

        let mut canonical: Vec<CanonicalMessage> = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text(system_prompt)],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text(context.to_string())],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
            },
        ];

        let mut branch_points: HashMap<u32, BranchPoint> = HashMap::new();
        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let infer = || {
            self.inference.infer_all();
        };
        self.react_engine
            .run_react_loop(
                task_id,
                &mut canonical,
                &mut history,
                1,
                &mut branch_points,
                emitter_arc,
                &self.sessions,
                tools,
                &infer,
            )
            .await?;
        Ok(history)
    }

    pub async fn process_input(
        &self,
        transcript: &str,
        active_task_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        tracing::debug!(
            "process_input: text={:?} active_task_id={:?}",
            transcript,
            active_task_id
        );

        if let Some(task_id) = active_task_id {
            // Switch to the task's session so persisted messages are
            // associated with the correct session for review.
            if let Ok(Some(task)) = self.db.get_task(&task_id)
                && let Some(ref sid) = task.session_id
            {
                self.sessions.switch_to_session(sid);
            }
            self.persist_message("user", transcript, Some("text"));

            let state = self.executor.get_task_state(&task_id).await;

            if state == TaskStatus::Running {
                self.executor.add_steering(&task_id, transcript).await?;
            } else {
                let was_in_memory = self
                    .executor
                    .add_supplement(&task_id, transcript)
                    .await
                    .is_ok();
                if !was_in_memory {
                    // Task may be stale/deleted — fall back to creating a new task
                    if self.executor.ensure_task_loaded(&task_id).await.is_err() {
                        let session_id = self
                            .start_new_session()
                            .unwrap_or_else(|_| self.ensure_session());
                        let task = self
                            .executor
                            .create_task_with_summary(
                                transcript,
                                "NewTask",
                                TaskPriority::Normal,
                                transcript,
                                Some(&session_id),
                            )
                            .await?;
                        self.events.emit_task_created(&task).await;
                        return Ok(ProcessResult::TaskCreated(task.id));
                    }
                    self.executor.add_supplement(&task_id, transcript).await?;
                }
                if state == TaskStatus::Completed
                    || state == TaskStatus::Error
                    || state == TaskStatus::Cancelled
                    || state == TaskStatus::Paused
                {
                    self.executor
                        .update_task_status(&task_id, TaskStatus::Pending)
                        .await?;
                    self.events.emit_task_updated(&task_id, "pending").await;
                }
            }
            Ok(ProcessResult::Supplemented)
        } else {
            let session_id = self
                .start_new_session()
                .unwrap_or_else(|_| self.ensure_session());
            self.persist_message("user", transcript, Some("text"));
            let task = self
                .executor
                .create_task_with_summary(
                    transcript,
                    "NewTask",
                    TaskPriority::Normal,
                    transcript,
                    Some(&session_id),
                )
                .await?;
            tracing::info!("process_input created task: id={:?}", task.id);
            self.events.emit_task_created(&task).await;
            Ok(ProcessResult::TaskCreated(task.id))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::stream;
    use haven_common::types::RiskLevel;
    use haven_llm::{
        FinishReason, LlmClient, LlmError, LlmMessage, LlmResponse, StreamChunk, ToolCall,
        ToolDefinition, Usage,
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
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: chat not implemented".into()))
        }
        async fn chat_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown(
                "mock: chat_with_tools not implemented".into(),
            ))
        }
        async fn chat_stream(
            &self,
            _: Vec<LlmMessage>,
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
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let chunk = StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
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
        completed: std::sync::Mutex<bool>,
    }

    #[async_trait]
    impl AgentEventEmitter for RecordingEmitter {
        async fn emit(&self, event: AgentEvent) {
            match event {
                AgentEvent::Thought { thought, .. } => {
                    self.thoughts.lock().unwrap().push(thought);
                }
                AgentEvent::TaskCompleted { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::TaskUpdated { .. } => {
                    *self.completed.lock().unwrap() = true;
                }
                AgentEvent::Supplement {
                    additional_context, ..
                } => {
                    self.supplements.lock().unwrap().push(additional_context);
                }
                _ => {}
            }
        }
    }

    #[tokio::test]
    async fn run_task_emits_supplement_when_additional_context_queued() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(client.clone(), client));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            8000,
            None,
        ));

        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let task = executor
            .create_task_with_summary(
                "do stuff",
                "NewTask",
                TaskPriority::Normal,
                "do stuff summary",
                None,
            )
            .await
            .unwrap();
        executor
            .add_supplement(&task.id, "extra: remember path X")
            .await
            .unwrap();

        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());

        let sups = recorder.supplements.lock().unwrap().clone();
        assert_eq!(sups.len(), 1, "exactly one supplement event expected");
        assert_eq!(sups[0], "extra: remember path X");
        // With supplements, task pauses instead of completing (conversation mode)
        let state = executor.get_task_state(&task.id).await;
        assert_eq!(
            state,
            TaskStatus::Paused,
            "task should be paused (not completed) when supplements were processed"
        );
    }

    // ─── Pure-logic and data-layer tests (no LLM required) ───

    fn make_test_agent() -> (Arc<AgentLayer>, Arc<TaskExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(client.clone(), client));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            8000,
            None,
        ));
        (agent, executor)
    }

    #[test]
    fn agent_new_constructor_works() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_new_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(client.clone(), client));
        let agent = AgentLayer::new(db, executor, router, 10, 20, 4000, None);
        // Verify construction succeeded; session_id is set
        let sid = agent.ensure_session();
        assert!(!sid.is_empty());
    }

    #[test]
    fn ensure_session_returns_non_empty() {
        let (agent, _) = make_test_agent();
        let sid = agent.ensure_session();
        assert!(!sid.is_empty());
        // Calling again returns the same session
        let sid2 = agent.ensure_session();
        assert_eq!(sid, sid2);
    }

    #[test]
    fn set_emitter_stores_reference() {
        let (agent, _) = make_test_agent();
        let recorder = Arc::new(RecordingEmitter {
            thoughts: std::sync::Mutex::new(Vec::new()),
            supplements: std::sync::Mutex::new(Vec::new()),
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());
        // Verify emitter is stored without panic (set_emitter succeeds)
    }

    #[test]
    fn replace_router_and_router_work() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_router_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let client_a = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_a = Arc::new(LlmRouter::new_with_clients(client_a.clone(), client_a));
        let agent = Arc::new(AgentLayer::new(db, executor, router_a, 10, 20, 4000, None));
        // Create a new router via the same mock client factory
        let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_b = Arc::new(LlmRouter::new_with_clients(client_b.clone(), client_b));
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
            completed: std::sync::Mutex::new(false),
        });
        agent.set_emitter(recorder.clone());

        let task = executor
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
    }

    #[tokio::test]
    async fn build_tool_definitions_returns_list() {
        let (agent, _) = make_test_agent();
        let defs = agent.prompt_builder.build_tool_definitions().await;
        // ToolsManager starts empty; definitions should be an empty list
        assert!(defs.is_empty());
    }

    #[tokio::test]
    async fn persist_message_adds_to_db() {
        let (agent, _) = make_test_agent();
        let sid = agent.ensure_session();
        agent.persist_message("user", "test message", Some("text"));
        // Read back via db
        let agent_ref = agent.clone();
        let read_sid = sid.clone();
        let db = agent_ref.db.clone();
        let msgs = db.get_session_messages_limit(&read_sid, 50).unwrap();
        // Messages may or may not be immediately flushed depending on cache
        // — verify at minimum the message is retrievable
        let found = msgs
            .iter()
            .find(|m| m.role == "user" && m.content == "test message");
        assert!(found.is_some(), "persisted user message not found in db");
        let _ = (sid, msgs);
    }

    #[test]
    fn parse_reasoner_response_final_answer_from_text() {
        let resp = LlmResponse {
            text: "Task done.".into(),
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
        };
        let (thought, actions) = ReActEngine::parse_reasoner_response(&resp, 1);
        assert_eq!(thought, Some("Task done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
        assert_eq!(actions[0].tool_name, "final_answer");
    }

    #[test]
    fn parse_reasoner_response_with_tool_calls() {
        let resp = LlmResponse {
            text: "Opening file.".into(),
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "open_file".into(),
                arguments: r#"{"path":"/tmp/test"}"#.into(),
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
        };
        let (thought, actions) = ReActEngine::parse_reasoner_response(&resp, 2);
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
    fn parse_reasoner_response_final_answer_tool_call() {
        let resp = LlmResponse {
            text: "All done.".into(),
            tool_calls: vec![ToolCall {
                id: "final".into(),
                name: "final_answer".into(),
                arguments: "{}".into(),
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
        };
        let (thought, actions) = ReActEngine::parse_reasoner_response(&resp, 1);
        assert_eq!(thought, Some("All done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
    }

    #[tokio::test]
    async fn supplement_task_reactivates_completed_task() {
        let (agent, executor) = make_test_agent();
        let task = executor
            .create_task("original", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        executor.end_task(&task.id).await.unwrap();
        assert_eq!(
            executor.get_task_state(&task.id).await,
            TaskStatus::Cancelled
        );

        agent
            .supplement_task(&task.id, "more context")
            .await
            .unwrap();
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Pending);
        let supps: Vec<String> = executor.get_supplements(&task.id).await;
        assert_eq!(supps, vec!["more context"]);
    }

    #[tokio::test]
    async fn process_input_creates_new_task() {
        let (agent, executor) = make_test_agent();
        let result = agent.process_input("open notepad", None).await.unwrap();
        match result {
            ProcessResult::TaskCreated(task_id) => {
                assert!(!task_id.is_empty());
                let state = executor.get_task_state(&task_id).await;
                assert_eq!(state, TaskStatus::Pending);
            }
            ProcessResult::Supplemented => panic!("expected TaskCreated"),
        }
    }

    #[tokio::test]
    async fn run_fact_inference_does_not_panic() {
        let (agent, executor) = make_test_agent();
        let task = executor
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        executor.end_task(&task.id).await.unwrap();
        agent.inference.infer_facts();
        agent.inference.infer_preferences();
    }

    // ─── Integration tests for the ReAct core loop (refine §11) ───

    fn make_test_agent_with(
        client: Arc<dyn LlmClient>,
        tools: Arc<ToolsManager>,
    ) -> (Arc<AgentLayer>, Arc<TaskExecutor>) {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let router = Arc::new(LlmRouter::new_with_clients(client.clone(), client));
        let agent = Arc::new(AgentLayer::new(
            db,
            executor.clone(),
            router,
            30,
            50,
            8000,
            None,
        ));
        (agent, executor)
    }

    /// Scripted LlmClient that returns a pre-programmed sequence of responses
    /// from `chat_stream_with_tools`, enabling full ReAct-loop integration
    /// tests without a live LLM. Mirrors Pi's `MockLlmClient` pattern.
    struct ScriptedMock {
        stream_responses: std::sync::Mutex<VecDeque<ScriptedResponse>>,
        chat_text: std::sync::Mutex<String>,
    }

    enum ScriptedResponse {
        Err(LlmError),
        Chunk(StreamChunk),
    }

    impl ScriptedMock {
        fn new(responses: Vec<ScriptedResponse>) -> Self {
            Self {
                stream_responses: std::sync::Mutex::new(VecDeque::from(responses)),
                chat_text: std::sync::Mutex::new("Compacted summary.".into()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ScriptedMock {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
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
            })
        }
        async fn chat_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
        }
        async fn chat_stream(
            &self,
            _: Vec<LlmMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown("mock: use chat_stream_with_tools".into()))
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
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
        fn has_compaction(&self) -> bool {
            self.events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, AgentEvent::Compaction { .. }))
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
    async fn run_task_executes_tool_then_final_answer() {
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("I'll echo that.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"text":"hello"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor
            .create_task("echo hello", "NewTask", TaskPriority::Normal, None)
            .await
            .unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(history.len() >= 2, "should have at least 2 steps");
        assert!(collector.has_action("echo"));
        assert!(collector.has_observation("echo"));
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_parallel_tool_execution() {
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
                    ToolCall {
                        id: "tc1".into(),
                        name: "delay_a".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "tc2".into(),
                        name: "delay_b".into(),
                        arguments: "{}".into(),
                    },
                ],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
            }),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor
            .create_task("run parallel", "NewTask", TaskPriority::Normal, None)
            .await
            .unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(collector.has_action("delay_a"));
        assert!(collector.has_action("delay_b"));
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
    async fn run_task_compaction_retry_on_context_exceeded() {
        let tools = Arc::new(ToolsManager::new());
        tools.registry.register(Arc::new(EchoTool) as ToolBox).await;
        let mock = Arc::new(ScriptedMock::new(vec![
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Calling echo.".into()),
                tool_calls: vec![ToolCall {
                    id: "tc1".into(),
                    name: "echo".into(),
                    arguments: r#"{"text":"data"}"#.into(),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                usage: None,
                model: None,
                reasoning: None,
            }),
            ScriptedResponse::Err(LlmError::ContextLengthExceeded),
            ScriptedResponse::Chunk(StreamChunk {
                text: Some("Done after compaction.".into()),
                tool_calls: vec![ToolCall {
                    id: "final".into(),
                    name: "final_answer".into(),
                    arguments: "{}".into(),
                }],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
            }),
        ]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor
            .create_task("test compaction", "NewTask", TaskPriority::Normal, None)
            .await
            .unwrap();
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
        assert!(
            collector.has_compaction(),
            "Compaction event should be emitted"
        );
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn run_task_context_exceeded_compaction_fails() {
        let tools = Arc::new(ToolsManager::new());
        let mock = Arc::new(ScriptedMock::new(vec![ScriptedResponse::Err(
            LlmError::ContextLengthExceeded,
        )]));
        let (agent, executor) = make_test_agent_with(mock, tools);
        let collector = Arc::new(EventCollector::new());
        agent.set_emitter(collector.clone());
        let task = executor
            .create_task("compaction fail", "NewTask", TaskPriority::Normal, None)
            .await
            .unwrap();
        let result = agent.run_task_from_id(&task.id).await;
        assert!(result.is_err(), "should error when compaction fails");
        assert_eq!(executor.get_task_state(&task.id).await, TaskStatus::Error);
    }
}
