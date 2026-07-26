use std::collections::HashMap;
use std::sync::Arc;

mod compactor;
mod event;
mod inference;
mod prompt;
mod react;
mod session;
mod types;

pub use types::{Action, BranchPoint, ProcessResult, ReActSnapshot, ReActStep};
pub use compactor::ContextCompactor;
pub use event::{AgentEvent, AgentEventEmitter, EventDispatcher};
pub use inference::InferenceEngine;
pub use prompt::SystemPromptBuilder;
pub use react::ReActEngine;
pub use session::SessionManager;
pub use haven_task::{RunHandler, TaskExecutor, TaskInfo, TaskPriority, TaskStatus};

use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::LlmRouter;
use haven_memory::Database;

pub struct AgentLayer {
    db: Arc<Database>,
    executor: Arc<TaskExecutor>,
    sessions: Arc<SessionManager>,
    events: Arc<EventDispatcher>,
    prompt_builder: Arc<SystemPromptBuilder>,
    react_engine: Arc<ReActEngine>,
    inference: Arc<InferenceEngine>,
}

impl AgentLayer {
    pub fn new(
        db: Arc<Database>,
        executor: Arc<TaskExecutor>,
        router: Arc<LlmRouter>,
        max_steps: u32,
        session_window_size: usize,
        max_observation_chars: usize,
    ) -> Self {
        let sessions = Arc::new(SessionManager::new(db.clone(), session_window_size));
        let events = Arc::new(EventDispatcher::new());
        let prompt_builder = Arc::new(SystemPromptBuilder::new(
            executor.get_tools(),
            db.clone(),
        ));
        let react_engine = Arc::new(ReActEngine::new(
            router,
            executor.clone(),
            db.clone(),
            max_steps,
            max_observation_chars,
        )        );
        let inference = Arc::new(InferenceEngine::new(db.clone(), sessions.clone()));
        let _ = db.set_preference("name", "Xtopia");
        let _ = db.insert_fact("user", "name", "Xtopia", "user", 1.0);

        Self {
            db,
            executor,
            sessions,
            events,
            prompt_builder,
            react_engine,
            inference,
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

    /// Get the branch points for a task (for frontend UI).
    pub async fn get_branch_points(&self, task_id: &str) -> Vec<u32> {
        if let Ok(Some(state_json)) = self.db.get_react_state(task_id)
            && let Ok(snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            let mut steps: Vec<u32> = snapshot.branch_points.keys().copied().collect();
            steps.sort();
            steps
        } else {
            Vec::new()
        }
    }

    /// Roll back a task to a specific branch point. The task state is
    /// replaced with the branch point snapshot and the task is set back
    /// to Pending so the dispatcher re-executes it.
    pub async fn rollback_task(&self, task_id: &str, target_step: u32) -> anyhow::Result<()> {
        let state_json = self.db.get_react_state(task_id)?
            .ok_or_else(|| anyhow::anyhow!("no saved state for task {}", task_id))?;
        let mut snapshot: ReActSnapshot = serde_json::from_str(&state_json)?;
        let bp = snapshot.branch_points.get(&target_step)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no branch point at step {}", target_step))?;

        snapshot.canonical = bp.canonical;
        snapshot.history = bp.history;
        snapshot.step_number = bp.step_number;
        let json = serde_json::to_string(&snapshot)?;
        self.db.save_react_state(task_id, &json)?;
        self.executor
            .update_task_status(task_id, TaskStatus::Pending)
            .await?;
        self.events.emit_task_updated(task_id, "pending").await;
        tracing::info!("rollback_task {} to step {}: task set to Pending", task_id, target_step);
        Ok(())
    }

    /// Fork a task into a new session. Creates a new task in a branched
    /// session and copies the current ReAct snapshot so the fork continues
    /// from the same point.
    pub async fn fork_task(&self, task_id: &str) -> anyhow::Result<String> {
        let task = self.executor.list_tasks().await
            .into_iter()
            .find(|t| t.id == task_id)
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found", task_id))?;

        let parent_session_id = self.sessions.current_session_id();
        let new_session_id = self.db.create_session(Some(&parent_session_id))?.id;

        let forked = self.executor.create_task_with_summary(
            &task.input,
            &task.classification,
            task.priority,
            &task.summary,
            Some(&new_session_id),
        ).await?;

        if let Ok(Some(state_json)) = self.db.get_react_state(task_id) {
            self.db.save_react_state(&forked.id, &state_json)?;
        }

        self.events.emit_task_created(&forked).await;
        tracing::info!("fork_task {} -> {} in session {}", task_id, forked.id, new_session_id);
        Ok(forked.id)
    }

    /// Dispatcher entrypoint. Looks up the task by id, fills in the
    /// classifier summary (description) and original transcript (context),
    /// loads conversation history, then runs the ReAct loop.
    pub async fn run_task_from_id(&self, task_id: &str) -> anyhow::Result<Vec<ReActStep>> {
        tracing::info!("run_task_from_id: task_id={}", task_id);
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
        let conv_history = self.load_conversation_history();
        let tools = self.prompt_builder.build_tool_definitions().await;

        if let Ok(Some(state_json)) = self.db.get_react_state(task_id)
            && let Ok(snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            tracing::info!("restoring ReAct state for task {} ({} steps)", task_id, snapshot.history.len());
            return self
                .run_task_resumed(task_id, snapshot, &conv_history, &tools)
                .await;
        }

        self.run_task(&task.id, &description, &context, &conv_history, &tools)
            .await
    }

    pub async fn emit_task_completed(&self, task_id: &str, title: &str) {
        self.events.emit_task_completed(task_id, title).await;
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
            let sys_end = canonical.iter().position(|m| m.role != CanonicalRole::System)
                .unwrap_or(1);
            for msg in conversation_history {
                canonical.insert(sys_end, CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("[conversation] {}", msg))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }
        }

        let emitter_arc = match self.events.emitter_arc() {
            Some(e) => e,
            None => return Ok(history),
        };
        let infer = || { self.inference.infer_all(); };
        self.react_engine.run_react_loop(
            task_id, &mut canonical, &mut history, start_step, &mut branch_points,
            emitter_arc, &self.sessions, tools, &infer,
        ).await?;
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
        tracing::info!("run_task start: task_id={:?} context={:?}", task_id, context);
        let mut history: Vec<ReActStep> = Vec::new();
        let system_prompt = self.prompt_builder
            .build(description, &[], conversation_history)
            .await;
        tracing::info!("run_task: system_prompt {} chars", system_prompt.len());

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
        let infer = || { self.inference.infer_all(); };
        self.react_engine.run_react_loop(
            task_id, &mut canonical, &mut history, 1, &mut branch_points,
            emitter_arc, &self.sessions, tools, &infer,
        ).await?;
        Ok(history)
    }

    pub async fn process_input(
        &self,
        transcript: &str,
        active_task_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        tracing::info!("process_input: text={:?} active_task_id={:?}", transcript, active_task_id);
        self.persist_message("user", transcript, Some("text"));

        if let Some(task_id) = active_task_id {
            let state = self.executor.get_task_state(&task_id).await;

            if state == TaskStatus::Running {
                self.executor
                    .add_steering(&task_id, transcript)
                    .await?;
            } else {
                let was_in_memory = self.executor.add_supplement(&task_id, transcript).await.is_ok();
                if !was_in_memory {
                    // Task may be stale/deleted — fall back to creating a new task
                    if self.executor.ensure_task_loaded(&task_id).await.is_err() {
                        let session_id = self.start_new_session().unwrap_or_else(|_| self.ensure_session());
                        let task = self
                            .executor
                            .create_task_with_summary(transcript, "NewTask", TaskPriority::Normal, transcript, Some(&session_id))
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
            let session_id = self.start_new_session().unwrap_or_else(|_| self.ensure_session());
            let task = self
                .executor
                .create_task_with_summary(transcript, "NewTask", TaskPriority::Normal, transcript, Some(&session_id))
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
    use haven_llm::{FinishReason, LlmClient, LlmError, LlmMessage, LlmResponse, StreamChunk, ToolCall, ToolDefinition};
    use haven_tools::ToolsManager;
    use std::pin::Pin;

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
                AgentEvent::Supplement { additional_context, .. } => {
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
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(db, executor.clone(), router, 30, 50, 8000));

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
                "do stuff summary", None,
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
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client,
        ));
        let agent = Arc::new(AgentLayer::new(db, executor.clone(), router, 30, 50, 8000));
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
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client,
        ));
        let agent = AgentLayer::new(db, executor, router, 10, 20, 4000);
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
        let router_a = Arc::new(LlmRouter::new_with_clients(
            client_a.clone(),
            client_a,
        ));
        let agent = Arc::new(AgentLayer::new(db, executor, router_a, 10, 20, 4000));
        // Create a new router via the same mock client factory
        let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_b = Arc::new(LlmRouter::new_with_clients(
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
        let found = msgs.iter().find(|m| m.role == "user" && m.content == "test message");
        assert!(found.is_some(), "persisted user message not found in db");
        let _ = (sid, msgs);
    }

    #[test]
    fn parse_reasoner_response_final_answer_from_text() {
        let resp = LlmResponse {
            text: "Task done.".into(),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, model_name: None, cost: None },
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
            usage: haven_llm::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, model_name: None, cost: None },
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
            usage: haven_llm::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, model_name: None, cost: None },
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
}
