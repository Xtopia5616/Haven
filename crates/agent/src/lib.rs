use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

mod compactor;
use compactor::ContextCompactor;

pub use haven_task::{RunHandler, TaskExecutor, TaskInfo, TaskPriority, TaskStatus};

use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use haven_llm::{
    EndpointRole, FinishReason, LlmResponse, LlmRouter, ToolDefinition,
    ToolFunction,
};
use haven_memory::Database;
use serde::{Deserialize, Serialize};

/// Branch point saved before tool execution, used for rollback (§2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchPoint {
    pub canonical: Vec<CanonicalMessage>,
    pub history: Vec<ReActStep>,
    pub step_number: u32,
}

/// Serializable snapshot of the ReAct loop state for pause/resume (§1.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActSnapshot {
    pub canonical: Vec<CanonicalMessage>,
    pub history: Vec<ReActStep>,
    pub step_number: u32,
    /// Branch points keyed by step number for tree-structured rollback (§2).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub branch_points: HashMap<u32, BranchPoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReActStep {
    pub step_number: u32,
    pub thought: Option<String>,
    pub action: Option<Action>,
    pub observation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub tool_name: String,
    pub tool_input: Value,
    pub is_final: bool,
    pub tool_call_id: Option<String>,
}

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

pub struct AgentLayer {
    db: Arc<Database>,
    executor: Arc<TaskExecutor>,
    router: Arc<RwLock<Arc<LlmRouter>>>,
    max_steps: Mutex<u32>,
    emitter: Arc<Mutex<Option<Arc<dyn AgentEventEmitter>>>>,
    session_id: Mutex<String>,
    session_window_size: usize,
    max_observation_chars: usize,
    fallback_notified: Mutex<HashSet<String>>,
    compactor: ContextCompactor,
    run_counter: AtomicU64,
    current_run_id: AtomicU64,
}

type ChunkSender = tokio::sync::mpsc::Sender<(String, String, u32, u64)>;
type ConsumerHandle = Option<tokio::task::JoinHandle<()>>;

fn spawn_chunk_consumer(
    emitter: &Option<Arc<dyn AgentEventEmitter>>,
) -> (ChunkSender, ChunkSender, ConsumerHandle) {
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel(1024);
    let (reasoning_tx, mut reasoning_rx) = tokio::sync::mpsc::channel(1024);

    let consumer_handle = emitter.as_ref().map(|em| {
        let em_clone = em.clone();
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
    });

    (chunk_tx, reasoning_tx, consumer_handle)
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
        let session_id = db
            .get_or_create_active_session()
            .map(|s| s.id)
            .unwrap_or_else(|_| "default".to_string());
        // Ensure the user's name is set as a defined preference and user fact
        let _ = db.set_preference("name", "Xtopia");
        let _ = db.insert_fact("user", "name", "Xtopia", "user", 1.0);

        Self {
            db,
            executor,
            router: Arc::new(RwLock::new(router)),
            max_steps: Mutex::new(max_steps),
            emitter: Arc::new(Mutex::new(None)),
            session_id: Mutex::new(session_id),
            session_window_size,
            max_observation_chars,
            fallback_notified: Mutex::new(HashSet::new()),
            compactor: ContextCompactor::new(32_000, 4_096),
            run_counter: AtomicU64::new(0),
            current_run_id: AtomicU64::new(0),
        }
    }

    /// Return the current active session ID, creating a new session if needed.
    pub fn ensure_session(&self) -> String {
        let mut guard = self.session_id.lock().unwrap();
        if *guard == "default"
            && let Ok(s) = self.db.get_or_create_active_session()
        {
            *guard = s.id.clone();
            return s.id;
        }
        guard.clone()
    }

    /// Create a new session and switch the agent to it. Returns the new session
    /// ID. Each new task gets its own session so conversation history does not
    /// leak between tasks.
    /// Holds session_id lock across the entire operation to prevent a concurrent
    /// ensure_session or persist_message from seeing a stale session ID.
    pub fn start_new_session(&self) -> anyhow::Result<String> {
        let mut guard = self.session_id.lock().unwrap();
        if *guard != "default" {
            let _ = self.db.close_session(&guard);
        }
        let session = self.db.create_session(None)?;
        *guard = session.id.clone();
        Ok(session.id)
    }

    /// Persist a message to the active session with the configured window size.
    /// Holds the session_id lock across the DB write to prevent a TOCTOU race
    /// with concurrent start_new_session / supplement_task calls.
    fn persist_message(&self, role: &str, content: &str, message_type: Option<&str>) {
        let window_size = self.session_window_size;
        let guard = self.session_id.lock().unwrap();
        let _ = self.db.add_message_with_window(
            &guard,
            role,
            content,
            message_type,
            None,
            window_size,
        );
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
            *self.session_id.lock().unwrap() = sid.clone();
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
            self.emit_task_updated(task_id, "pending").await;
        } else if state == TaskStatus::Completed
            || state == TaskStatus::Error
            || state == TaskStatus::Cancelled
        {
            self.executor
                .update_task_status(task_id, TaskStatus::Pending)
                .await?;
            self.emit_task_updated(task_id, "pending").await;
        }
        Ok(())
    }

    pub fn set_emitter(&self, emitter: Arc<dyn AgentEventEmitter>) {
        *self.emitter.lock().unwrap() = Some(emitter);
    }

    /// Snapshot of the currently-active LlmRouter. Cloned cheaply (just an
    /// `Arc` refcount bump) so the read guard is released immediately and
    /// the call site is safe to `.await` against the returned handle.
    fn router(&self) -> Arc<LlmRouter> {
        self.router.read().unwrap().clone()
    }

    /// Hot-swap the LlmRouter for all subsequent Agent calls. Used by
    /// `update_settings` to apply new model endpoints at runtime without an
    /// app restart (design §4.4.1).
    pub fn replace_router(&self, new_router: Arc<LlmRouter>) {
        *self.router.write().unwrap() = new_router;
    }

    /// Hot-update the ReAct loop max_steps (design §4.4.3, applied at
    /// next dispatched task — does not retroactively affect running tasks).
    pub fn set_max_steps(&self, max_steps: u32) {
        *self.max_steps.lock().unwrap() = max_steps;
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

    /// Load the most recent conversation messages from DB as text lines that
    /// get fed into the ReAct system prompt as Conversation History.
    fn load_conversation_history(&self) -> Vec<String> {
        let guard = self.session_id.lock().unwrap();
        self.db
            .get_session_messages_limit(&guard, self.session_window_size)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect()
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
        self.emit_task_updated(task_id, "pending").await;
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

        let parent_session_id = self.session_id.lock().unwrap().clone();
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

        self.emit_task_created(&forked).await;
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

        let run_id = self.run_counter.fetch_add(1, Ordering::SeqCst);
        self.current_run_id.store(run_id, Ordering::SeqCst);

        let description = if task.summary.is_empty() {
            task.input.clone()
        } else {
            task.summary.clone()
        };
        let context = task.input.clone();
        let conv_history = self.load_conversation_history();

        // Check if this is a resumed task with saved ReAct state (§1.3)
        if let Ok(Some(state_json)) = self.db.get_react_state(task_id)
            && let Ok(snapshot) = serde_json::from_str::<ReActSnapshot>(&state_json)
        {
            tracing::info!("restoring ReAct state for task {} ({} steps)", task_id, snapshot.history.len());
            return self
                .run_task_resumed(task_id, &description, snapshot, &conv_history)
                .await;
        }

        self.run_task(&task.id, &description, &context, &conv_history)
            .await
    }

    async fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        let schemas = self.executor.get_tools().registry.list_schemas().await;
        schemas
            .into_iter()
            .map(|s| ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: s["name"].as_str().unwrap_or("").into(),
                    description: s["description"].as_str().unwrap_or("").into(),
                    parameters: s["input_schema"].clone(),
                },
            })
            .collect()
    }

    async fn build_system_prompt(
        &self,
        task_description: &str,
        history: &[ReActStep],
        conversation_history: &[String],
    ) -> String {
        let mut prompt = String::from(
            "You are Haven, a PC voice assistant. You help users accomplish tasks using available tools.\n\n\
             Available tools:\n",
        );

        // Built-in tools: full schema injected into prompt
        prompt.push_str(
            "You have access to the following built-in tools:\n\n",
        );
        for tool in self.build_tool_definitions().await {
            let name = &tool.function.name;
            if name.starts_with("mcp::") || name.starts_with("skill::") { continue; }
            let desc = &tool.function.description;
            let params =
                serde_json::to_string_pretty(&tool.function.parameters).unwrap_or_default();
            prompt.push_str(&format!("- {}: {}\n  {}\n", name, desc, params));
        }

        // MCP tools: concise listing
        let schemas = self.executor.get_tools().registry.list_schemas().await;
        let mcp_tools: Vec<_> = schemas.iter().filter(|s| {
            s["name"].as_str().is_some_and(|n| n.starts_with("mcp::"))
        }).collect();
        if !mcp_tools.is_empty() {
            prompt.push_str("\nMCP tools (external, prefixed with `mcp::<server>::`):\n");
            for tool in &mcp_tools {
                let name = tool["name"].as_str().unwrap_or("");
                let desc = tool["description"].as_str().unwrap_or("");
                prompt.push_str(&format!("  - {}: {}\n", name, desc));
            }
        }

        // Skill index: concise listing (Pi-style)
        let skill_index = self.executor.get_tools().build_skill_index().await;
        if !skill_index.is_empty() {
            prompt.push_str("\nInstallable skills (use `load_skill` to activate):\n");
            for entry in &skill_index {
                prompt.push_str(&format!(
                    "  - {}: {}\n",
                    entry["name"].as_str().unwrap_or(""),
                    entry["description"].as_str().unwrap_or("")
                ));
            }
        }

        // MCP server index: concise listing for progressive loading
        let mcp_index = self.executor.get_tools().build_mcp_index().await;
        if !mcp_index.is_empty() {
            prompt.push_str("\nAvailable MCP servers (use `load_mcp` to activate):\n");
            for entry in &mcp_index {
                prompt.push_str(&format!(
                    "  - {}: {}\n",
                    entry["name"].as_str().unwrap_or(""),
                    entry["description"].as_str().unwrap_or(""),
                ));
            }
        }

        // User facts (concise, subject = "user")
        if let Ok(facts) = self.db.get_facts("user")
            && !facts.is_empty()
        {
            prompt.push_str("\nAbout the user:");
            for fact in facts.iter().take(10) {
                prompt.push_str(&format!(
                    " [{}] {} {}",
                    if fact.source == "user" { "defined" } else { "inferred" },
                    fact.predicate,
                    fact.object,
                ));
            }
            prompt.push('\n');
        }

        // Preferences (concise)
        if let Ok(summary) = self.db.get_preference_summary()
            && !summary.is_empty()
        {
            prompt.push_str("Preferences:");
            for (key, value) in &summary {
                prompt.push_str(&format!(" {}={}", key, value));
            }
            prompt.push('\n');
        }

        prompt.push_str(
            "\nGuidelines:\n\
             1. Think step by step. Decide what to do, then call the right tool.\n\
             2. After each tool call you will receive the result. Use it to decide next.\n\
             3. When the task is complete, respond with a summary of what was done.\n\
             4. If no tool is needed, answer directly.\n\
             5. Never call the same tool with identical parameters twice in a row.\n\n",
        );

        prompt.push_str(&format!("Current task: {}\n\n", task_description));

        // Conversation history accumulated during pause
        if !conversation_history.is_empty() {
            prompt.push_str("Additional context:\n");
            for msg in conversation_history {
                prompt.push_str(&format!("  {}\n", msg));
            }
            prompt.push('\n');
        }

        // Previous steps
        if !history.is_empty() {
            prompt.push_str("Steps so far:\n");
            for step in history {
                if let Some(ref thought) = step.thought {
                    prompt.push_str(&format!("  Thought {}: {}\n", step.step_number, thought));
                }
                if let Some(ref action) = step.action {
                    if action.is_final {
                        prompt.push_str(&format!("  Action {}: done\n", step.step_number));
                    } else {
                        prompt.push_str(&format!(
                            "  Action {}: {} {}\n",
                            step.step_number,
                            action.tool_name,
                            serde_json::to_string(&action.tool_input).unwrap_or_default()
                        ));
                    }
                }
                if let Some(ref obs) = step.observation {
                    prompt.push_str(&format!("  Result {}: {}\n", step.step_number, obs));
                }
            }
        }

        prompt.push_str("\nWhat is your next step?\n");
        prompt
    }

    async fn emit_thought(&self, task_id: &str, thought: &str, step_number: u32, run_id: u64) {
        tracing::info!("emit_thought: task={} step={} run={} thought_len={}", task_id, step_number, run_id, thought.len());
        let _ = self
            .db
            .create_thought_step(task_id, step_number as i32, thought);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::Thought {
                task_id: task_id.into(),
                thought: thought.into(),
                step_number,
                run_id,
            }).await;
        }
    }

    async fn emit_supplement(&self, task_id: &str, additional_context: &str, step_number: u32, run_id: u64) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::Supplement {
                task_id: task_id.into(),
                additional_context: additional_context.into(),
                step_number,
                run_id,
            }).await;
        }
    }

    async fn emit_action(&self, task_id: &str, tool_name: &str, input: &Value, step_number: u32, run_id: u64, tool_call_id: Option<String>) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::Action {
                task_id: task_id.into(),
                tool_name: tool_name.into(),
                input: input.clone(),
                step_number,
                run_id,
                tool_call_id,
            }).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn emit_observation(
        &self,
        task_id: &str,
        observation: &str,
        tool_name: &str,
        step_number: u32,
        run_id: u64,
        silent: bool,
        tool_call_id: Option<String>,
    ) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::Observation {
                task_id: task_id.into(),
                observation: observation.into(),
                tool_name: tool_name.into(),
                step_number,
                run_id,
                silent,
                tool_call_id,
            }).await;
        }
    }

    async fn emit_task_created(&self, task: &TaskInfo) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TaskCreated(task.clone())).await;
        }
    }

    pub async fn emit_task_completed(&self, task_id: &str, title: &str) {
        self.fallback_notified.lock().unwrap().remove(task_id);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TaskCompleted {
                task_id: task_id.into(),
                title: title.into(),
            }).await;
        }
    }

    async fn emit_task_updated(&self, task_id: &str, status: &str) {
        tracing::info!("emit_task_updated agent: task={} status={}", task_id, status);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TaskUpdated {
                task_id: task_id.into(),
                status: status.into(),
            }).await;
        }
    }

    async fn emit_task_error(&self, task_id: &str, error: &str) {
        self.fallback_notified.lock().unwrap().remove(task_id);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::TaskError {
                task_id: task_id.into(),
                error: error.into(),
            }).await;
        }
    }

    async fn emit_fallback_activated(&self, task_id: &str, reason: &str) {
        {
            let mut notified = self.fallback_notified.lock().unwrap();
            if !notified.insert(task_id.to_string()) {
                return;
            }
        }
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::FallbackActivated {
                task_id: task_id.into(),
                reason: reason.into(),
            }).await;
        }
    }

    async fn emit_compaction(&self, task_id: &str, summary: &str, tokens_before: u32, tokens_after: u32) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.emit(AgentEvent::Compaction {
                task_id: task_id.into(),
                summary: summary.into(),
                tokens_before,
                tokens_after,
            }).await;
        }
    }

    /// Check if context compaction is needed before the next LLM call.
    /// If so, run compaction via the DefaultModel and replace `canonical` in place.
    async fn maybe_compact(&self, task_id: &str, canonical: &mut Vec<CanonicalMessage>) {
        if canonical.len() < 4 {
            return;
        }
        if !self.compactor.needs_compaction(canonical) {
            return;
        }
        let router = self.router();
        if let Some(result) = self.compactor.compact(canonical, &router).await {
            tracing::info!(
                "compaction for task {}: {} tokens → {} tokens ({} msgs summarized)",
                task_id, result.tokens_before, result.tokens_after, result.summarized_count
            );
            *canonical = result.compacted;
            self.emit_compaction(task_id, &result.summary, result.tokens_before, result.tokens_after).await;
        }
    }

    fn parse_reasoner_response(
        &self,
        response: &LlmResponse,
        step_number: u32,
    ) -> (Option<String>, Vec<Action>) {
        let text = response.text.trim().to_string();

        let thought = if text.is_empty() {
            None
        } else {
            Some(text.clone())
        };

        let actions: Vec<Action> = if !response.tool_calls.is_empty() {
            response
                .tool_calls
                .iter()
                .map(|tc| {
                    let args: Value =
                        serde_json::from_str(&tc.arguments).unwrap_or(Value::Null);
                    let is_final =
                        tc.name == "final_answer" || tc.name == "answer" || tc.name == "done";
                    Action {
                        tool_name: tc.name.clone(),
                        tool_input: args,
                        is_final,
                        tool_call_id: Some(
                            if tc.id.is_empty() {
                                uuid::Uuid::new_v4().to_string()
                            } else {
                                tc.id.clone()
                            },
                        ),
                    }
                })
                .collect()
        } else if !text.is_empty()
            && response.finish_reason == Some(FinishReason::Stop)
            && step_number > 0
        {
            vec![Action {
                tool_name: "final_answer".into(),
                tool_input: Value::Null,
                is_final: true,
                tool_call_id: None,
            }]
        } else {
            Vec::new()
        };

        (thought, actions)
    }



    /// Save snapshot including branch points for tree-structured rollback (§2).
    fn save_snapshot_with_branches(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &HashMap<u32, BranchPoint>,
    ) {
        let snapshot = ReActSnapshot {
            canonical: canonical.to_vec(),
            history: history.to_vec(),
            step_number,
            branch_points: branch_points.clone(),
        };
        if let Ok(json) = serde_json::to_string(&snapshot) {
            let _ = self.db.save_react_state(task_id, &json);
        }
    }

    /// Save a branch point at the current step before tool execution (§2).
    fn save_branch_point(
        &self,
        task_id: &str,
        canonical: &[CanonicalMessage],
        history: &[ReActStep],
        step_number: u32,
        branch_points: &mut HashMap<u32, BranchPoint>,
    ) {
        branch_points.insert(step_number, BranchPoint {
            canonical: canonical.to_vec(),
            history: history.to_vec(),
            step_number,
        });
        self.save_snapshot_with_branches(task_id, canonical, history, step_number, branch_points);
    }



    /// Shared ReAct loop body. Runs from `start_step` through `max_steps`,
    /// then checks for follow-up items before looping through more steps.
    /// Called by both `run_task` (fresh) and `run_task_resumed` (resumed from
    /// snapshot) so the ~530-line loop body exists only once.
async fn run_react_loop(
    &self,
    task_id: &str,
    canonical: &mut Vec<CanonicalMessage>,
    history: &mut Vec<ReActStep>,
    start_step: u32,
    branch_points: &mut HashMap<u32, BranchPoint>,
) -> anyhow::Result<()> {
        let tools = self.build_tool_definitions().await;
        let max_steps = *self.max_steps.lock().unwrap();
        let mut last_step = start_step.saturating_sub(1);

        for step_num in start_step..=max_steps {
            last_step = step_num;
            loop {
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    TaskStatus::Cancelled => {
                        return Ok(());
                    }
                    TaskStatus::Error | TaskStatus::Completed => {
                        if state != TaskStatus::Completed {
                            self.emit_task_error(task_id, "task interrupted").await;
                        }
                        return Ok(());
                    }
                    TaskStatus::Paused => {
                        self.save_snapshot_with_branches(task_id, canonical, history, step_num, branch_points);
                    }
                    _ => break,
                }
                let cancel = self.executor.cancellation_token(task_id).await;
                if cancel.is_cancelled() {
                    return Ok(());
                }
                if self.executor.get_task_state(task_id).await == TaskStatus::Paused {
                    self.executor.status_notifier(task_id).await.notified().await;
                }
            }

            let run_id = self.current_run_id.load(Ordering::SeqCst);
            let supplements = self.executor.get_supplements(task_id).await;
            for supplement in &supplements {
                self.emit_supplement(task_id, supplement, step_num, run_id).await;
                let _ = self.db.create_thought_step(
                    task_id,
                    history.len() as i32,
                    supplement,
                );
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!(
                        "Additional context from user: {}",
                        supplement
                    ))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            let steering = self.executor.get_steering(task_id).await;
            for s in &steering {
                self.emit_supplement(task_id, s, step_num, run_id).await;
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Steering: {}", s))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            self.maybe_compact(task_id, canonical).await;

            let mut thought_chunks: Vec<String> = Vec::new();
            let mut reasoning_chunks: Vec<String> = Vec::new();
            let emitter = self.emitter.lock().unwrap().clone();
            let (chunk_tx, reasoning_tx, consumer_handle) = spawn_chunk_consumer(&emitter);
            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
            tracing::info!("ReAct step {} calling LLM, messages count: {}", step_num, llm_messages.len());
            let response = match router
                .chat_stream_with_tools_aggregated_cancellable(
                    EndpointRole::DefaultModel,
                    llm_messages,
                    tools.clone(),
                    |c: &haven_llm::StreamChunk| {
                        if let Some(t) = &c.text {
                            thought_chunks.push(t.clone());
                            if let Err(e) = chunk_tx.try_send((task_id.to_string(), t.clone(), step_num, run_id)) {
                                tracing::warn!("thought chunk channel full, dropping: {}", e);
                            }
                        }
                        if let Some(r) = &c.reasoning {
                            reasoning_chunks.push(r.clone());
                            if let Err(e) = reasoning_tx.try_send((task_id.to_string(), r.clone(), step_num, run_id)) {
                                tracing::warn!("reasoning chunk channel full, dropping: {}", e);
                            }
                        }
                    },
                    cancel_res.clone(),
                )
                .await
            {
                Ok(resp) => {
                    if router.fallback_active() {
                        self.emit_fallback_activated(task_id, "switching to fallback model").await;
                    }
                    resp
                }
                Err(haven_llm::LlmError::ContextLengthExceeded) => {
                    tracing::warn!("context length exceeded for task {}, forcing compaction", task_id);
                    if let Some(result) = self.compactor.compact(canonical, &self.router()).await {
                        tracing::info!("compacted {} → {} tokens", result.tokens_before, result.tokens_after);
                        *canonical = result.compacted;
                        self.emit_compaction(task_id, &result.summary, result.tokens_before, result.tokens_after).await;
                        let (chunk_tx2, reasoning_tx2, consumer_handle2) = spawn_chunk_consumer(&emitter);
                        // Don't replay old chunks — they were already emitted
                        // during the first (failed) LLM call. Re-sending would
                        // duplicate content on the frontend.
                        let llm_messages2 = haven_llm::types::convert_to_llm(canonical.clone());
                        match router
                            .chat_stream_with_tools_aggregated_cancellable(
                                EndpointRole::DefaultModel,
                                llm_messages2,
                                tools.clone(),
                                |c: &haven_llm::StreamChunk| {
                                    if let Some(t) = &c.text {
                                        thought_chunks.push(t.clone());
                                        if let Err(e) = chunk_tx2.try_send((task_id.to_string(), t.clone(), step_num, run_id)) {
                                            tracing::warn!("retry thought chunk channel full, dropping: {}", e);
                                        }
                                    }
                                    if let Some(r) = &c.reasoning {
                                        reasoning_chunks.push(r.clone());
                                        if let Err(e) = reasoning_tx2.try_send((task_id.to_string(), r.clone(), step_num, run_id)) {
                                            tracing::warn!("retry reasoning chunk channel full, dropping: {}", e);
                                        }
                                    }
                                },
                                cancel_res.clone(),
                            )
                            .await
                        {
                            Ok(retry_resp) => {
                                drop(chunk_tx2);
                                drop(reasoning_tx2);
                                if let Some(handle) = consumer_handle2 {
                                    let _ = handle.await;
                                }
                                retry_resp
                            }
                            Err(haven_llm::LlmError::Cancelled) => {
                                return Ok(());
                            }
                            Err(e2) => {
                                let err_msg = format!("Compaction retry also failed: {}", e2);
                                self.emit_task_error(task_id, &err_msg).await;
                                self.executor.update_task_status(task_id, TaskStatus::Error).await?;
                                return Err(anyhow::anyhow!("{}", err_msg));
                            }
                        }
                    } else {
                        let err_msg = "context length exceeded but compaction failed".to_string();
                        self.emit_task_error(task_id, &err_msg).await;
                        self.executor.update_task_status(task_id, TaskStatus::Error).await?;
                        return Err(anyhow::anyhow!("{}", err_msg));
                    }
                }
                Err(haven_llm::LlmError::Cancelled) => {
                    return Ok(());
                }
                Err(e) => {
                    let err_msg = format!("Both reasoner and fallback failed: {}", e);
                    self.emit_task_error(task_id, &err_msg).await;
                    self.executor.update_task_status(task_id, TaskStatus::Error).await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
            };

            drop(chunk_tx);
            drop(reasoning_tx);
            if let Some(handle) = consumer_handle {
                let _ = handle.await;
            }

            if !reasoning_chunks.is_empty() {
                let text: String = reasoning_chunks.concat();
                self.persist_message("assistant", &text, Some("reasoning"));
            }

            let (thought, actions) = self.parse_reasoner_response(&response, step_num);

            if let Some(ref t) = thought {
                self.emit_thought(task_id, t, step_num, run_id).await;
                history.push(ReActStep {
                    step_number: step_num,
                    thought: Some(t.clone()),
                    action: None,
                    observation: None,
                });
            }

            if actions.is_empty() {
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                if let Some(last) = history.last_mut() {
                    last.action = Some(Action {
                        tool_name: "final_answer".into(),
                        tool_input: Value::Null,
                        is_final: true,
                        tool_call_id: None,
                    });
                    if last.observation.is_none() {
                        last.observation = Some(msg.clone());
                    }
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: Some(msg.clone()),
                        action: Some(Action {
                            tool_name: "final_answer".into(),
                            tool_input: Value::Null,
                            is_final: true,
                            tool_call_id: None,
                        }),
                        observation: Some(msg.clone()),
                    });
                }
                self.executor.update_task_status(task_id, TaskStatus::Paused).await?;
                self.emit_task_updated(task_id, "paused").await;
                self.persist_message("assistant", &msg, Some("text"));
                self.run_fact_inference();
                self.run_preference_inference();
                self.save_branch_point(task_id, canonical, history, step_num, branch_points);
                self.save_snapshot_with_branches(task_id, canonical, history, last_step + 1, branch_points);
                return Ok(());
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Task completed.".into());
                if let Some(s) = history.last_mut() {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                self.executor.update_task_status(task_id, TaskStatus::Paused).await?;
                self.emit_task_updated(task_id, "paused").await;
                self.persist_message("assistant", &final_text, Some("text"));
                self.run_fact_inference();
                self.run_preference_inference();
                self.save_branch_point(task_id, canonical, history, step_num, branch_points);
                self.save_snapshot_with_branches(task_id, canonical, history, step_num + 1, branch_points);
                return Ok(());
            }

            if let Some(ref t) = thought {
                let text = t.trim();
                if !text.is_empty() {
                    self.persist_message("assistant", text, Some("text"));
                }
            }

            let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
            for action in &non_final {
                self.emit_action(task_id, &action.tool_name, &action.tool_input, step_num, run_id, action.tool_call_id.clone()).await;
            }

            if !non_final.is_empty() {
                let tool_calls: Option<Vec<CanonicalToolCall>> = if response.tool_calls.is_empty() {
                    None
                } else {
                    Some(response.tool_calls.iter().map(|tc| CanonicalToolCall {
                        id: tc.id.clone(),
                        name: tc.name.clone(),
                        arguments: serde_json::from_str(&tc.arguments).unwrap_or(Value::Null),
                    }).collect())
                };
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::Assistant,
                    content: vec![ContentPart::text(response.text.clone())],
                    tool_calls,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            self.save_branch_point(task_id, canonical, history, step_num, branch_points);

            use futures_util::StreamExt;

            let mut tool_futures = futures_util::stream::FuturesUnordered::new();
            for action in &non_final {
                let task_id = task_id.to_string();
                let tool_name = action.tool_name.clone();
                let tool_input = action.tool_input.clone();
                let action = (*action).clone();
                let max_obs = self.max_observation_chars;
                let db = self.db.clone();
                let executor = self.executor.clone();
                tool_futures.push(async move {
                    let result = executor
                        .execute_step(&task_id, &tool_name, tool_input.clone())
                        .await;
                    let (text, is_error) = match result {
                        Ok(r) => {
                            let _ = db.record_tool_usage(&tool_name, &tool_input, r.success);
                            let text = if r.success {
                                serde_json::to_string(&r.output)
                                    .unwrap_or_else(|_| "success".into())
                            } else {
                                r.error.unwrap_or_else(|| "unknown failure".into())
                            };
                            let text = if text.len() > max_obs {
                                format!("{}[... truncated {} chars omitted]", &text[..max_obs], text.len() - max_obs)
                            } else {
                                text
                            };
                            (text, !r.success)
                        }
                        Err(e) => (e.to_string(), true),
                    };
                    (action, tool_name, text, is_error)
                });
            }

            let mut any_tool_failure = false;
            while let Some((action, tool_name, step_result, is_error)) = tool_futures.next().await {
                if is_error {
                    any_tool_failure = true;
                }
                let silent = action.tool_input.get("silent").and_then(|v| v.as_bool()).unwrap_or(false);
                self.emit_observation(task_id, &step_result, &tool_name, step_num, run_id, silent, action.tool_call_id.clone()).await;

                if let Some(last) = history.last_mut() {
                    last.action = Some(action.clone());
                    last.observation = Some(step_result.clone());
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: None,
                        action: Some(action.clone()),
                        observation: Some(step_result.clone()),
                    });
                }

                canonical.push(CanonicalMessage {
                    role: CanonicalRole::Tool,
                    content: vec![ContentPart::text(step_result)],
                    tool_calls: None,
                    tool_call_id: action.tool_call_id.clone(),
                    parent_message_id: None,
                });
            }

            if any_tool_failure && step_num < max_steps - 1 {
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(
                        "The previous approach encountered errors. Please try a completely different approach this time."
                    )],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
            }

            let state = self.executor.get_task_state(task_id).await;
            if state == TaskStatus::Paused {
                self.save_snapshot_with_branches(task_id, canonical, history, step_num, branch_points);
                return Ok(());
            }
            if state == TaskStatus::Cancelled || state == TaskStatus::Error || state == TaskStatus::Completed {
                return Ok(());
            }
            }

        self.executor.update_task_status(task_id, TaskStatus::Paused).await?;
        self.emit_task_updated(task_id, "paused").await;
        self.persist_message("assistant", "Task completed.", Some("text"));
        self.run_fact_inference();
        self.run_preference_inference();
        self.save_snapshot_with_branches(task_id, canonical, history, last_step + 1, branch_points);
        Ok(())
    }

    /// Resume a previously saved ReAct loop from a snapshot.
    async fn run_task_resumed(
        &self,
        task_id: &str,
        _description: &str,
        snapshot: ReActSnapshot,
        conversation_history: &[String],
    ) -> anyhow::Result<Vec<ReActStep>> {
        let mut history = snapshot.history;
        let mut canonical = snapshot.canonical;
        let start_step = snapshot.step_number;
        let mut branch_points = snapshot.branch_points;

        // Inject conversation history messages that accumulated during the
        // pause period into the canonical message stream (design §4.7.4).
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

        self.run_react_loop(task_id, &mut canonical, &mut history, start_step, &mut branch_points).await?;
        Ok(history)
    }

    pub async fn run_task(
        &self,
        task_id: &str,
        description: &str,
        context: &str,
        conversation_history: &[String],
    ) -> anyhow::Result<Vec<ReActStep>> {
        tracing::info!("run_task start: task_id={:?} context={:?}", task_id, context);
        let mut history: Vec<ReActStep> = Vec::new();
        let system_prompt = self
            .build_system_prompt(description, &[], conversation_history)
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
        self.run_react_loop(task_id, &mut canonical, &mut history, 1, &mut branch_points).await?;
        Ok(history)
    }

    /// Run fact inference from the current session messages (M6-04).
    /// Uses rule-based extraction and optionally LLM-assisted inference.
    fn run_fact_inference(&self) {
        let guard = self.session_id.lock().unwrap();
        if let Ok(messages) = self.db.get_session_messages(&guard) {
            // Only infer facts from user messages — assistant messages
            // (e.g. "I am Haven") should not become facts about the user.
            let user_messages: Vec<_> = messages.into_iter().filter(|m| m.role == "user").collect();
            let inferred = self.db.infer_facts_from_messages(&user_messages);
            for (subject, predicate, object, confidence) in inferred {
                let _ = self.db.insert_fact(&subject, &predicate, &object, "inferred", confidence);
            }
            let _ = self.db.dedup_facts();
            let _ = self.db.flush_low_confidence(0.3);
        }
    }

    /// Run cross-session preference inference after a task completes (M6-02).
    /// Extracts patterns such as preferred language, working directory, editor,
    /// and verbosity from the conversation messages and persists them as
    /// `inferred.*` preference keys. User-set keys always take precedence.
    fn run_preference_inference(&self) {
        let guard = self.session_id.lock().unwrap();
        if let Ok(messages) = self.db.get_session_messages(&guard) {
            let inferred = self.db.infer_preferences_from_messages(&messages);
            let _ = self.db.save_inferred_preferences(&inferred);
        }
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
                        self.emit_task_created(&task).await;
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
                    self.emit_task_updated(&task_id, "pending").await;
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
            self.emit_task_created(&task).await;
            Ok(ProcessResult::TaskCreated(task.id))
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ProcessResult {
    TaskCreated(String),
    Supplemented,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::stream;
    use haven_llm::{LlmClient, LlmError, LlmMessage, StreamChunk, ToolCall};
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
        // Verify emitter is stored without panic
        let emitter = agent.emitter.lock().unwrap();
        assert!(emitter.is_some());
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
        let orig = agent.router();
        // Create a new router via the same mock client factory
        let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_b = Arc::new(LlmRouter::new_with_clients(
            client_b.clone(),
            client_b,
        ));
        agent.replace_router(router_b);
        let swapped = agent.router();
        assert!(!Arc::ptr_eq(&orig, &swapped));
    }

    #[tokio::test]
    async fn set_max_steps_updates_field() {
        let (agent, executor) = make_test_agent();
        agent.set_max_steps(5);

        let task = executor
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        // max_steps is read inside run_task; calling it verifies the setter doesn't panic
        // The mock will terminate in one step regardless
        let history = agent.run_task_from_id(&task.id).await.unwrap();
        assert!(!history.is_empty());
    }

    #[tokio::test]
    async fn build_tool_definitions_returns_list() {
        let (agent, _) = make_test_agent();
        let defs = agent.build_tool_definitions().await;
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
        let agent = {
            // We need an AgentLayer instance to call the method, but it doesn't
            // need a live executor/router for this parsing test.
            let mut p = std::env::temp_dir();
            p.push(format!("haven_agent_parse_{}.db", uuid::Uuid::new_v4()));
            let db = Arc::new(Database::open(&p).unwrap());
            let tools = Arc::new(ToolsManager::new());
            let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
            let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
            let router = Arc::new(LlmRouter::new_with_clients(
                client.clone(),
                client,
            ));
            AgentLayer::new(db, executor, router, 10, 20, 4000)
        };

        let resp = LlmResponse {
            text: "Task done.".into(),
            tool_calls: vec![],
            finish_reason: Some(FinishReason::Stop),
            usage: haven_llm::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, model_name: None, cost: None },
            model: None,
            reasoning: None,
        };
        let (thought, actions) = agent.parse_reasoner_response(&resp, 1);
        assert_eq!(thought, Some("Task done.".into()));
        assert_eq!(actions.len(), 1);
        assert!(actions[0].is_final);
        assert_eq!(actions[0].tool_name, "final_answer");
    }

    #[test]
    fn parse_reasoner_response_with_tool_calls() {
        let agent = {
            let mut p = std::env::temp_dir();
            p.push(format!("haven_agent_parse2_{}.db", uuid::Uuid::new_v4()));
            let db = Arc::new(Database::open(&p).unwrap());
            let tools = Arc::new(ToolsManager::new());
            let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
            let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
            let router = Arc::new(LlmRouter::new_with_clients(
                client.clone(),
                client,
            ));
            AgentLayer::new(db, executor, router, 10, 20, 4000)
        };

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
        let (thought, actions) = agent.parse_reasoner_response(&resp, 2);
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
        let agent = {
            let mut p = std::env::temp_dir();
            p.push(format!("haven_agent_parse3_{}.db", uuid::Uuid::new_v4()));
            let db = Arc::new(Database::open(&p).unwrap());
            let tools = Arc::new(ToolsManager::new());
            let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
            let client = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
            let router = Arc::new(LlmRouter::new_with_clients(
                client.clone(),
                client,
            ));
            AgentLayer::new(db, executor, router, 10, 20, 4000)
        };

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
        let (thought, actions) = agent.parse_reasoner_response(&resp, 1);
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
        agent.run_fact_inference();
        agent.run_preference_inference();
    }
}
