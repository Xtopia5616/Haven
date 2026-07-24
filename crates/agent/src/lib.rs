use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

mod compactor;
use compactor::ContextCompactor;

use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::{
    EndpointRole, FinishReason, LlmMessage, LlmResponse, LlmRole, LlmRouter, ToolDefinition,
    ToolFunction,
};
use haven_memory::Database;
use haven_task::{RunHandler, TaskExecutor, TaskInfo, TaskPriority, TaskStatus};
use serde::{Deserialize, Serialize};

/// Serializable snapshot of the ReAct loop state for pause/resume (§1.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReActSnapshot {
    canonical: Vec<CanonicalMessage>,
    history: Vec<ReActStep>,
    step_number: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Classification {
    NewTask {
        summary: String,
        priority: TaskPriority,
        suggested_tools: Vec<String>,
    },
    AppendToTask {
        task_id: String,
        additional_context: String,
    },
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
}

#[async_trait]
pub trait AgentEventEmitter: Send + Sync {
    async fn on_thought(&self, task_id: &str, thought: &str, step_number: u32, run_id: u64);
    async fn on_action(&self, _task_id: &str, _tool_name: &str, _input: &Value, _step_number: u32, _run_id: u64) {}
    async fn on_observation(
        &self,
        _task_id: &str,
        _observation: &str,
        _tool_name: &str,
        _step_number: u32,
        _run_id: u64,
        _silent: bool,
    ) {}
    async fn on_task_created(&self, task: &TaskInfo);
    async fn on_task_completed(&self, task_id: &str, title: &str);
    async fn on_task_error(&self, task_id: &str, error: &str);
    async fn on_fallback_activated(&self, task_id: &str, reason: &str);
    /// Incremental Reasoner text delta for the streaming Thought UI (design
    /// §4.4.3). Default no-op so existing emitters keep compiling.
    async fn on_thought_chunk(&self, _task_id: &str, _delta: &str, _step_number: u32, _run_id: u64) {}
    /// Incremental reasoning/chain-of-thought delta (e.g. DeepSeek-R1's
    /// reasoning_content). Default no-op for backward compat.
    async fn on_reasoning_chunk(&self, _task_id: &str, _delta: &str, _step_number: u32, _run_id: u64) {}
    /// User appended additional context to an in-flight task (design
    /// §4.5.3). Default no-op so existing emitters keep compiling.
    async fn on_supplement(&self, _task_id: &str, _additional_context: &str, _step_number: u32, _run_id: u64) {}
    /// Task status changed. Default no-op so existing emitters keep compiling.
    async fn on_task_updated(&self, _task_id: &str, _status: &str) {}
    /// Context compaction occurred (§3.1). Default no-op.
    async fn on_compaction(&self, _task_id: &str, _summary: &str, _tokens_before: u32, _tokens_after: u32) {}
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
    pub fn start_new_session(&self) -> anyhow::Result<String> {
        // Close old session if it exists and is active
        {
            let guard = self.session_id.lock().unwrap();
            if *guard != "default" {
                let _ = self.db.close_session(&guard);
            }
        }
        let session = self.db.create_session(None)?;
        let mut guard = self.session_id.lock().unwrap();
        *guard = session.id.clone();
        Ok(session.id)
    }

    /// Persist a message to the active session with the configured window size.
    fn persist_message(&self, role: &str, content: &str, message_type: Option<&str>) {
        let session_id = self.session_id.lock().unwrap().clone();
        let window_size = self.session_window_size;
        let _ = self.db.add_message_with_window(
            &session_id,
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
        if state == haven_task::TaskStatus::Paused {
            // The ReAct loop has already exited (status set to Paused and
            // returned).  Always hand off to the dispatcher by setting
            // Pending — the supplement_queue will cause take_next_pending to
            // pick it up within 100 ms regardless of dispatched_once.
            self.executor
                .update_task_status(task_id, haven_task::TaskStatus::Pending)
                .await?;
        } else if state == haven_task::TaskStatus::Completed
            || state == haven_task::TaskStatus::Error
            || state == haven_task::TaskStatus::Cancelled
        {
            self.executor
                .update_task_status(task_id, haven_task::TaskStatus::Pending)
                .await?;
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
        let session_id = self.session_id.lock().unwrap().clone();
        self.db
            .get_session_messages_limit(&session_id, self.session_window_size)
            .ok()
            .unwrap_or_default()
            .into_iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect()
    }

    /// Dispatcher entrypoint. Looks up the task by id, fills in the
    /// classifier summary (description) and original transcript (context),
    /// loads conversation history, then runs the ReAct loop.
    pub async fn run_task_from_id(&self, task_id: &str) -> anyhow::Result<Vec<ReActStep>> {
        let task = self
            .executor
            .list_tasks()
            .await
            .into_iter()
            .find(|t| t.id == task_id)
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found by dispatcher", task_id))?;

        let run_id = self.run_counter.fetch_add(1, Ordering::Relaxed);
        self.current_run_id.store(run_id, Ordering::Relaxed);

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
            "You are Haven, a PC voice assistant. Your goal is to help users accomplish tasks by using available tools.\n\n\
             Available Tools:\n",
        );
        for tool in self.build_tool_definitions().await {
            let name = &tool.function.name;
            let desc = &tool.function.description;
            let params =
                serde_json::to_string_pretty(&tool.function.parameters).unwrap_or_default();
            prompt.push_str(&format!("- {}: {}\n  Parameters: {}\n", name, desc, params));
        }

        // Skill Index — progressive skill loading (refine §4.7)
        let skill_index = self.executor.get_tools().build_skill_index().await;
        if !skill_index.is_empty() {
            prompt.push_str("\nSkill Index (name + description only; use `load_skill` to get full schema):\n");
            for entry in &skill_index {
                prompt.push_str(&format!(
                    "  - {}: {}\n",
                    entry["name"].as_str().unwrap_or(""),
                    entry["description"].as_str().unwrap_or("")
                ));
            }
            prompt.push_str("\nTo use a skill, call `load_skill` with its name to retrieve the full schema, then call the skill tool with the appropriate parameters.\n");
        }

// Inject known facts about the user (subject = "user") as background
        // knowledge so the agent can reason about preferences, paths, etc.
        // (design §4.7.5). Distinguish user-defined vs inferred facts (M6-04).
        if let Ok(facts) = self.db.get_facts("user")
            && !facts.is_empty()
        {
            prompt.push_str("\nUser Facts (background knowledge):\n");
            for fact in facts.iter().take(20) {
                let prefix = if fact.source == "user" { "[user-defined]" } else { "[inferred]" };
                prompt.push_str(&format!(
                    "  {} {} {} {}\n",
                    prefix, fact.predicate, fact.object, fact.confidence
                ));
            }
        }

        // Surface auto-learned preferences such as most frequently used tools,
        // preferred language, working directory, etc. (design §4.7.3, M6-02).
        if let Ok(summary) = self.db.get_preference_summary()
            && !summary.is_empty()
        {
            prompt.push_str("\nUser Preferences:\n");
            for (key, value) in &summary {
                prompt.push_str(&format!("  {} = {}\n", key, value));
            }
            prompt.push('\n');
        }

        prompt.push_str("\nInstructions:\n");
        prompt.push_str("1. ANALYZE the user's request and decide the next step.\n");
        prompt.push_str("2. If you need to use a tool, call it with the correct parameters.\n");
        prompt.push_str("3. After each tool call, you will receive the output (observation).\n");
        prompt.push_str("4. When the task is complete, respond with a summary - do NOT call a tool named 'final_answer'.\n");
        prompt.push_str(
            "5. If no tool fits the request, respond with a natural language answer directly.\n",
        );
        prompt.push_str("6. NEVER call the same tool with the same parameters twice in a row.\n\n");

        prompt.push_str(&format!("Current Task: {}\n\n", task_description));

        if !conversation_history.is_empty() {
            prompt.push_str("Conversation History:\n");
            for msg in conversation_history {
                prompt.push_str(&format!("  {}\n", msg));
            }
            prompt.push('\n');
        }

        if !history.is_empty() {
            prompt.push_str("Previous Steps:\n");
            for step in history {
                if let Some(ref thought) = step.thought {
                    prompt.push_str(&format!("Thought {}: {}\n", step.step_number, thought));
                }
                if let Some(ref action) = step.action {
                    if action.is_final {
                        prompt.push_str(&format!("Action {}: [Final Answer]\n", step.step_number));
                    } else {
                        prompt.push_str(&format!(
                            "Action {}: call {} with {}\n",
                            step.step_number,
                            action.tool_name,
                            serde_json::to_string(&action.tool_input).unwrap_or_default()
                        ));
                    }
                }
                if let Some(ref obs) = step.observation {
                    prompt.push_str(&format!("Observation {}: {}\n", step.step_number, obs));
                }
            }
        }

        prompt.push_str("\nWhat is your next step?\n");
        prompt
    }

    fn build_classifier_prompt(text: &str, has_active_task: bool) -> String {
        if has_active_task {
            format!(
                "You are classifying user input for a PC voice assistant.\n\
                 The user currently has an active task in progress.\n\n\
                 User input: \"{}\"\n\n\
                 Classify as:\n\
                 - NEW_TASK: This is a completely new, independent task (does not relate to the active task)\n\
                 - APPEND_TO_TASK: This supplements or adds context to the currently active task\n\n\
                 Respond with JSON only: {{\"classification\":\"NEW_TASK\"}} or {{\"classification\":\"APPEND_TO_TASK\",\"additional_context\":\"...\"}}",
                text
            )
        } else {
            format!(
                "You are classifying user input for a PC voice assistant.\n\
                 No active task exists.\n\n\
                 User input: \"{}\"\n\n\
                 Summarize the task in one sentence.\n\n\
                 Respond with JSON only: {{\"classification\":\"NEW_TASK\",\"summary\":\"brief summary\",\"suggested_tools\":[]}}",
                text
            )
        }
    }

    fn parse_classification_response(text: &str, active_task_id: Option<&str>) -> Classification {
        if let Ok(v) = serde_json::from_str::<Value>(text)
            && let Some(class) = v["classification"].as_str()
        {
            match class {
                "APPEND_TO_TASK" => {
                    let ctx = v["additional_context"].as_str().unwrap_or("").to_string();
                    let tid = active_task_id.unwrap_or("active").to_string();
                    return Classification::AppendToTask {
                        task_id: tid,
                        additional_context: ctx,
                    };
                }
                "NEW_TASK" => {
                    let summary = v["summary"].as_str().unwrap_or(text).to_string();
                    let tools: Vec<String> = v["suggested_tools"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let priority = match v["priority"].as_str() {
                        Some("critical") => TaskPriority::Critical,
                        Some("high") => TaskPriority::High,
                        Some("low") => TaskPriority::Low,
                        _ => TaskPriority::Normal,
                    };
                    return Classification::NewTask {
                        summary,
                        priority,
                        suggested_tools: tools,
                    };
                }
                _ => {}
            }
        }
        Classification::NewTask {
            summary: text.to_string(),
            priority: TaskPriority::Normal,
            suggested_tools: Vec::new(),
        }
    }

    pub async fn classify_intent(
        &self,
        text: &str,
        active_task_id: Option<&str>,
    ) -> Classification {
        let prompt = Self::build_classifier_prompt(text, active_task_id.is_some());
        let messages = vec![LlmMessage {
            role: LlmRole::User,
            content: vec![ContentPart::text(prompt)],
        }];
        let router = self.router();
        match router.chat(EndpointRole::SmallModel, messages).await {
            Ok(resp) => {
                let content = resp.text.trim().to_string();
                let start = content.find('{').unwrap_or(0);
                let end = content.rfind('}').map(|i| i + 1).unwrap_or(content.len());
                let json = &content[start..end];
                Self::parse_classification_response(json, active_task_id)
            }
            Err(e) => {
                tracing::warn!(
                    "Classifier LLM call failed: {}, falling back to heuristic",
                    e
                );
                Self::parse_classification_response(text, active_task_id)
            }
        }
    }

    async fn emit_thought(&self, task_id: &str, thought: &str, step_number: u32) {
        let run_id = self.current_run_id.load(Ordering::Relaxed);
        let _ = self
            .db
            .create_thought_step(task_id, step_number as i32, thought);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.on_thought(task_id, thought, step_number, run_id).await;
        }
    }

    async fn emit_supplement(&self, task_id: &str, additional_context: &str, step_number: u32) {
        let run_id = self.current_run_id.load(Ordering::Relaxed);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .on_supplement(task_id, additional_context, step_number, run_id)
                .await;
        }
    }

    async fn emit_action(&self, task_id: &str, tool_name: &str, input: &Value, step_number: u32, _silent: bool) {
        let run_id = self.current_run_id.load(Ordering::Relaxed);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .on_action(task_id, tool_name, input, step_number, run_id)
                .await;
        }
    }

    async fn emit_observation(
        &self,
        task_id: &str,
        observation: &str,
        tool_name: &str,
        step_number: u32,
        silent: bool,
    ) {
        let run_id = self.current_run_id.load(Ordering::Relaxed);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter
                .on_observation(task_id, observation, tool_name, step_number, run_id, silent)
                .await;
        }
    }

    async fn emit_task_created(&self, task: &TaskInfo) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.on_task_created(task).await;
        }
    }

    pub async fn emit_task_completed(&self, task_id: &str, title: &str) {
        self.fallback_notified.lock().unwrap().remove(task_id);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.on_task_completed(task_id, title).await;
        }
    }

    async fn emit_task_updated(&self, task_id: &str, status: &str) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.on_task_updated(task_id, status).await;
        }
    }

    async fn emit_task_error(&self, task_id: &str, error: &str) {
        self.fallback_notified.lock().unwrap().remove(task_id);
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.on_task_error(task_id, error).await;
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
            emitter.on_fallback_activated(task_id, reason).await;
        }
    }

    async fn emit_compaction(&self, task_id: &str, summary: &str, tokens_before: u32, tokens_after: u32) {
        let emitter = self.emitter.lock().unwrap().clone();
        if let Some(emitter) = emitter {
            emitter.on_compaction(task_id, summary, tokens_before, tokens_after).await;
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
            }]
        } else {
            Vec::new()
        };

        (thought, actions)
    }

    /// Save the current ReAct loop state to DB for pause/resume.
    fn save_snapshot(&self, task_id: &str, canonical: &[CanonicalMessage], history: &[ReActStep], step_number: u32) {
        let snapshot = ReActSnapshot {
            canonical: canonical.to_vec(),
            history: history.to_vec(),
            step_number,
        };
        if let Ok(json) = serde_json::to_string(&snapshot) {
            let _ = self.db.save_react_state(task_id, &json);
        }
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
                });
            }
        }

        let tools = self.build_tool_definitions().await;
        let max_steps = *self.max_steps.lock().unwrap();

        // Outer loop: after the inner ReAct loop finishes, check for
        // follow-up items before exiting (refine §1.2).
        loop {
            for step_num in start_step..=max_steps {
                // Pause/cancel/error check with state save
            loop {
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    TaskStatus::Cancelled | TaskStatus::Error | TaskStatus::Completed => {
                        if state != TaskStatus::Completed {
                            self.emit_task_error(task_id, "task interrupted").await;
                        }
                        return Ok(history);
                    }
                    TaskStatus::Paused => {
                        self.save_snapshot(task_id, &canonical, &history, step_num);
                    }
                    _ => break,
                }
                let cancel = self.executor.cancellation_token(task_id).await;
                if cancel.is_cancelled() {
                    return Ok(history);
                }
                // Double-check to avoid race: notify may have fired between
                // get_task_state and notified(). Only block if still paused.
                if self.executor.get_task_state(task_id).await == TaskStatus::Paused {
                    self.executor.status_notifier(task_id).await.notified().await;
                }
            }

            let supplements = self.executor.get_supplements(task_id).await;
            for supplement in &supplements {
                self.emit_supplement(task_id, supplement, step_num).await;
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
                });
            }

            // Check context pressure and auto-compact if needed (§3.1)
            self.maybe_compact(task_id, &mut canonical).await;

            let mut thought_chunks: Vec<String> = Vec::new();
            let emitter = self.emitter.lock().unwrap().clone();
            let run_id = self.current_run_id.load(Ordering::Relaxed);
            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
            let (reasoning_tx, mut reasoning_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
            let consumer_handle = emitter.as_ref().map(|em| {
                let em_clone = em.clone();
                tokio::spawn(async move {
                    let mut thought_done = false;
                    let mut reasoning_done = false;
                    while !(thought_done && reasoning_done) {
                        if thought_done {
                            while let Some((tid, delta, sn, rid)) = reasoning_rx.recv().await {
                                em_clone.on_reasoning_chunk(&tid, &delta, sn, rid).await;
                            }
                            reasoning_done = true;
                        } else if reasoning_done {
                            while let Some((tid, delta, sn, rid)) = chunk_rx.recv().await {
                                em_clone.on_thought_chunk(&tid, &delta, sn, rid).await;
                            }
                            thought_done = true;
                        } else {
                            tokio::select! {
                                biased;
                                val = chunk_rx.recv() => {
                                    match val {
                                        Some((tid, delta, sn, rid)) => em_clone.on_thought_chunk(&tid, &delta, sn, rid).await,
                                        None => thought_done = true,
                                    }
                                }
                                val = reasoning_rx.recv() => {
                                    match val {
                                        Some((tid, delta, sn, rid)) => em_clone.on_reasoning_chunk(&tid, &delta, sn, rid).await,
                                        None => reasoning_done = true,
                                    }
                                }
                            }
                        }
                    }
                })
            });
            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
            let response = match router
                .chat_stream_with_tools_aggregated_cancellable(
                    EndpointRole::DefaultModel,
                    llm_messages,
                    tools.clone(),
                    |c: &haven_llm::StreamChunk| {
                        if let Some(t) = &c.text {
                            thought_chunks.push(t.clone());
                            let _ = chunk_tx.send((task_id.to_string(), t.clone(), step_num, run_id));
                        }
                        if let Some(r) = &c.reasoning {
                            let _ = reasoning_tx.send((task_id.to_string(), r.clone(), step_num, run_id));
                        }
                    },
                    cancel_res.clone(),
                )
                .await
            {
                Ok(resp) => {
                    if router.fallback_active() {
                        self.emit_fallback_activated(task_id, "switching to fallback model")
                            .await;
                    }
                    resp
                }
                Err(haven_llm::LlmError::ContextLengthExceeded) => {
                    tracing::warn!("context length exceeded for task {}, forcing compaction", task_id);
                    if let Some(result) = self.compactor.compact(&canonical, &self.router()).await {
                        tracing::info!("compacted {} → {} tokens", result.tokens_before, result.tokens_after);
                        canonical = result.compacted;
                        self.emit_compaction(task_id, &result.summary, result.tokens_before, result.tokens_after).await;
                        let (chunk_tx2, mut chunk_rx2) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
                        let (reasoning_tx2, mut reasoning_rx2) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
                        let consumer_handle2 = emitter.as_ref().map(|em| {
                            let em_clone = em.clone();
                            tokio::spawn(async move {
                                let mut thought_done = false;
                                let mut reasoning_done = false;
                                while !(thought_done && reasoning_done) {
                                    tokio::select! {
                                        biased;
                                        val = chunk_rx2.recv() => {
                                            match val {
                                                Some((tid, delta, sn, rid)) => em_clone.on_thought_chunk(&tid, &delta, sn, rid).await,
                                                None => thought_done = true,
                                            }
                                        }
                                        val = reasoning_rx2.recv() => {
                                            match val {
                                                Some((tid, delta, sn, rid)) => em_clone.on_reasoning_chunk(&tid, &delta, sn, rid).await,
                                                None => reasoning_done = true,
                                            }
                                        }
                                    }
                                }
                            })
                        });
                        let llm_messages2 = haven_llm::types::convert_to_llm(canonical.clone());
                        match router
                            .chat_stream_with_tools_aggregated_cancellable(
                                EndpointRole::DefaultModel,
                                llm_messages2,
                                tools.clone(),
                                |c: &haven_llm::StreamChunk| {
                                    if let Some(t) = &c.text {
                                        thought_chunks.push(t.clone());
                                        let _ = chunk_tx2.send((task_id.to_string(), t.clone(), step_num, run_id));
                                    }
                                    if let Some(r) = &c.reasoning {
                                        let _ = reasoning_tx2.send((task_id.to_string(), r.clone(), step_num, run_id));
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
                                self.emit_task_error(task_id, "task cancelled by user").await;
                                return Ok(history);
                            }
                            Err(e2) => {
                                let err_msg = format!("Compaction retry also failed: {}", e2);
                                self.emit_task_error(task_id, &err_msg).await;
                                self.executor
                                    .update_task_status(task_id, TaskStatus::Error)
                                    .await?;
                                return Err(anyhow::anyhow!("{}", err_msg));
                            }
                        }
                    } else {
                        let err_msg = "context length exceeded but compaction failed".to_string();
                        self.emit_task_error(task_id, &err_msg).await;
                        self.executor
                            .update_task_status(task_id, TaskStatus::Error)
                            .await?;
                        return Err(anyhow::anyhow!("{}", err_msg));
                    }
                }
                Err(haven_llm::LlmError::Cancelled) => {
                    self.emit_task_error(task_id, "task cancelled by user").await;
                    return Ok(history);
                }
                Err(e) => {
                    let err_msg = format!("Both reasoner and fallback failed: {}", e);
                    self.emit_task_error(task_id, &err_msg).await;
                    self.executor
                        .update_task_status(task_id, TaskStatus::Error)
                        .await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
            };

            drop(chunk_tx);
            drop(reasoning_tx);
            if let Some(handle) = consumer_handle {
                let _ = handle.await;
            }

            let (thought, actions) = self.parse_reasoner_response(&response, step_num);

            if let Some(ref t) = thought {
                let thought_text = t
                    .lines()
                    .filter(|l| !l.starts_with("Action:") && !l.starts_with("Final Answer:"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !thought_text.trim().is_empty() {
                    self.emit_thought(task_id, &thought_text, step_num).await;

                    history.push(ReActStep {
                        step_number: step_num,
                        thought: Some(thought_text),
                        action: None,
                        observation: None,
                    });
                }
            }

            if actions.is_empty() {
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                self.emit_thought(task_id, &msg, step_num).await;
                history.push(ReActStep {
                    step_number: step_num,
                    thought: Some(msg.clone()),
                    action: None,
                    observation: None,
                });
                if let Some(last) = history.last_mut() {
                    last.action = Some(Action {
                        tool_name: "final_answer".into(),
                        tool_input: Value::Null,
                        is_final: true,
                    });
                    if last.observation.is_none() {
                        last.observation = Some(msg.clone());
                    }
                }
                self.executor
                    .update_task_status(task_id, TaskStatus::Paused)
                    .await?;
                self.emit_task_updated(task_id, "paused").await;
                self.persist_message("assistant", &msg, Some("text"));
                self.run_fact_inference();
                self.run_preference_inference();
                let _ = self.db.save_react_state(task_id, "");
                return Ok(history);
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Task completed.".into());
                if let Some(s) = history.last_mut() {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                self.executor
                    .update_task_status(task_id, TaskStatus::Paused)
                    .await?;
                self.emit_task_updated(task_id, "paused").await;
                self.persist_message("assistant", &final_text, Some("text"));
                self.run_fact_inference();
                self.run_preference_inference();
                return Ok(history);
            }

            let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
            // Emit "Calling X" for each tool immediately, before execution.
            for action in &non_final {
                let silent = action.tool_input.get("silent").and_then(|v| v.as_bool()).unwrap_or(false);
                self.emit_action(task_id, &action.tool_name, &action.tool_input, step_num, silent).await;
            }

            let results: Vec<(String, String)> = futures_util::future::join_all(
                non_final.iter().map(|action| {
                    let task_id = task_id.to_string();
                    let tool_name = action.tool_name.clone();
                    let tool_input = action.tool_input.clone();
                    let max_obs = self.max_observation_chars;
                    let db = self.db.clone();
                    let executor = self.executor.clone();
                    async move {
                        let tool_name_for_err = tool_name.clone();
                        executor
                            .execute_step(&task_id, &tool_name, tool_input.clone())
                            .await
                            .map(|result| {
                                let _ = db.record_tool_usage(&tool_name, &tool_input, result.success);
                                let mut text = if result.success {
                                    serde_json::to_string(&result.output)
                                        .unwrap_or_else(|_| "success".into())
                                } else {
                                    result.error.unwrap_or_else(|| "unknown failure".into())
                                };
                                if text.len() > max_obs {
                                    text = format!(
                                        "{}[... truncated {} chars omitted]",
                                        &text[..max_obs],
                                        text.len() - max_obs
                                    );
                                }
                                (tool_name, text)
                            })
                            .unwrap_or_else(|e| (tool_name_for_err, e.to_string()))
                    }
                }),
            )
            .await;

            for (idx, action) in non_final.iter().enumerate() {
                let (ref tool_name, ref step_result) = results[idx];
                let silent = action.tool_input.get("silent").and_then(|v| v.as_bool()).unwrap_or(false);
                self.emit_observation(task_id, step_result, tool_name, step_num, silent)
                    .await;

                if let Some(last) = history.last_mut() {
                    last.action = Some((*action).clone());
                    last.observation = Some(step_result.clone());
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: None,
                        action: Some((*action).clone()),
                        observation: Some(step_result.clone()),
                    });
                }

                let obs_msg = format!("Tool '{}' result: {}", tool_name, step_result);
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::Assistant,
                    content: vec![ContentPart::text(response.text.clone())],
                    tool_calls: None,
                    tool_call_id: None,
                });
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(obs_msg)],
                    tool_calls: None,
                    tool_call_id: None,
                });
            }

            let state = self.executor.get_task_state(task_id).await;
            if state == TaskStatus::Cancelled || state == TaskStatus::Error || state == TaskStatus::Completed {
                return Ok(history);
            }
            }

            // Inner loop exhausted (max steps) — check for follow-up items
            // before finalizing (refine §1.2 outer loop).
            let followups = self.executor.get_followup(task_id).await;
            if followups.is_empty() {
                break;
            }
            for f in &followups {
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Follow-up: {}", f))],
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        self.executor
            .update_task_status(task_id, TaskStatus::Paused)
            .await?;
        self.emit_task_updated(task_id, "paused").await;
        self.persist_message("assistant", "Task completed.", Some("text"));
        self.run_fact_inference();
        self.run_preference_inference();
        let _ = self.db.save_react_state(task_id, "");
        Ok(history)
    }

    pub async fn run_task(
        &self,
        task_id: &str,
        description: &str,
        context: &str,
        conversation_history: &[String],
    ) -> anyhow::Result<Vec<ReActStep>> {
        let mut history: Vec<ReActStep> = Vec::new();
        let system_prompt = self
            .build_system_prompt(description, &[], conversation_history)
            .await;

        let mut canonical: Vec<CanonicalMessage> = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text(system_prompt)],
                tool_calls: None,
                tool_call_id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text(context.to_string())],
                tool_calls: None,
                tool_call_id: None,
            },
        ];

        let tools = self.build_tool_definitions().await;
        let max_steps = *self.max_steps.lock().unwrap();

        // Outer loop: after the inner ReAct loop finishes, check for
        // follow-up items before exiting (refine §1.2).
        loop {
            // --- Inner: ReAct tool-call loop ---
            for step_num in 1..=max_steps {
            // Between steps: honor pauses, cancellations, and any user
            // supplements that have been queued since the last step.
            loop {
                let state = self.executor.get_task_state(task_id).await;
                match state {
                    TaskStatus::Cancelled | TaskStatus::Error | TaskStatus::Completed => {
                        if state != TaskStatus::Completed {
                            self.emit_task_error(task_id, "task interrupted").await;
                        }
                        return Ok(history);
                    }
                    TaskStatus::Paused => {
                        self.save_snapshot(task_id, &canonical, &history, step_num);
                    }
                    _ => break,
                }
                // Also check cancellation token for early abort while paused.
                let cancel = self.executor.cancellation_token(task_id).await;
                if cancel.is_cancelled() {
                    return Ok(history);
                }
                // Double-check to avoid race: notify may have fired between
                // get_task_state and notified(). Only block if still paused.
                if self.executor.get_task_state(task_id).await == TaskStatus::Paused {
                    self.executor.status_notifier(task_id).await.notified().await;
                }
            }

            let supplements = self.executor.get_supplements(task_id).await;
            for supplement in &supplements {
                // Surface the user's added context to the UI as a dedicated
                // supplement event before re-injecting it into the ReAct
                // message stream (design §4.5.3).
                self.emit_supplement(task_id, supplement, step_num).await;
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
                });
            }

            // Check for steering items before invoking the LLM (refine §1.2).
            // Steering interrupts the current tool sequence immediately.
            let steering = self.executor.get_steering(task_id).await;
            for s in &steering {
                self.emit_supplement(task_id, s, step_num).await;
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Steering: {}", s))],
                    tool_calls: None,
                    tool_call_id: None,
                });
            }

            // Check context pressure and auto-compact if needed (§3.1)
            self.maybe_compact(task_id, &mut canonical).await;

            // Stream the Reasoner's response so UI can render Thought deltas in
            // real time (design §4.4.3 "Thought 流式推送到 UI"). Tool-call
            // arguments are accumulated server-side by the router so the
            // ReAct loop sees the final `LlmResponse` once the stream ends.
            let mut thought_chunks: Vec<String> = Vec::new();
            let emitter = self.emitter.lock().unwrap().clone();
            let run_id = self.current_run_id.load(Ordering::Relaxed);
            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
            let (reasoning_tx, mut reasoning_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
            let consumer_handle = emitter.as_ref().map(|em| {
                let em_clone = em.clone();
                tokio::spawn(async move {
                    let mut thought_done = false;
                    let mut reasoning_done = false;
                    while !(thought_done && reasoning_done) {
                        if thought_done {
                            while let Some((tid, delta, sn, rid)) = reasoning_rx.recv().await {
                                em_clone.on_reasoning_chunk(&tid, &delta, sn, rid).await;
                            }
                            reasoning_done = true;
                        } else if reasoning_done {
                            while let Some((tid, delta, sn, rid)) = chunk_rx.recv().await {
                                em_clone.on_thought_chunk(&tid, &delta, sn, rid).await;
                            }
                            thought_done = true;
                        } else {
                            tokio::select! {
                                biased;
                                val = chunk_rx.recv() => {
                                    match val {
                                        Some((tid, delta, sn, rid)) => em_clone.on_thought_chunk(&tid, &delta, sn, rid).await,
                                        None => thought_done = true,
                                    }
                                }
                                val = reasoning_rx.recv() => {
                                    match val {
                                        Some((tid, delta, sn, rid)) => em_clone.on_reasoning_chunk(&tid, &delta, sn, rid).await,
                                        None => reasoning_done = true,
                                    }
                                }
                            }
                        }
                    }
                })
            });
            let router = self.router();
            let cancel_res = self.executor.cancellation_token(task_id).await;
            let llm_messages = haven_llm::types::convert_to_llm(canonical.clone());
            let response = match router
                .chat_stream_with_tools_aggregated_cancellable(
                    EndpointRole::DefaultModel,
                    llm_messages,
                    tools.clone(),
                    |c: &haven_llm::StreamChunk| {
                        if let Some(t) = &c.text {
                            thought_chunks.push(t.clone());
                            let _ = chunk_tx.send((task_id.to_string(), t.clone(), step_num, run_id));
                        }
                        if let Some(r) = &c.reasoning {
                            let _ = reasoning_tx.send((task_id.to_string(), r.clone(), step_num, run_id));
                        }
                    },
                    cancel_res.clone(),
                )
                .await
            {
                Ok(resp) => {
                    if router.fallback_active() {
                        self.emit_fallback_activated(task_id, "switching to fallback model")
                            .await;
                    }
                    resp
                }
                Err(haven_llm::LlmError::ContextLengthExceeded) => {
                    tracing::warn!("context length exceeded for task {}, forcing compaction", task_id);
                    if let Some(result) = self.compactor.compact(&canonical, &self.router()).await {
                        tracing::info!("compacted {} → {} tokens", result.tokens_before, result.tokens_after);
                        canonical = result.compacted;
                        self.emit_compaction(task_id, &result.summary, result.tokens_before, result.tokens_after).await;
                        let (chunk_tx2, mut chunk_rx2) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
                        let (reasoning_tx2, mut reasoning_rx2) = tokio::sync::mpsc::unbounded_channel::<(String, String, u32, u64)>();
                        let consumer_handle2 = emitter.as_ref().map(|em| {
                            let em_clone = em.clone();
                            tokio::spawn(async move {
                                let mut thought_done = false;
                                let mut reasoning_done = false;
                                while !(thought_done && reasoning_done) {
                                    tokio::select! {
                                        biased;
                                        val = chunk_rx2.recv() => {
                                            match val {
                                                Some((tid, delta, sn, rid)) => em_clone.on_thought_chunk(&tid, &delta, sn, rid).await,
                                                None => thought_done = true,
                                            }
                                        }
                                        val = reasoning_rx2.recv() => {
                                            match val {
                                                Some((tid, delta, sn, rid)) => em_clone.on_reasoning_chunk(&tid, &delta, sn, rid).await,
                                                None => reasoning_done = true,
                                            }
                                        }
                                    }
                                }
                            })
                        });
                        let llm_messages2 = haven_llm::types::convert_to_llm(canonical.clone());
                        match router
                            .chat_stream_with_tools_aggregated_cancellable(
                                EndpointRole::DefaultModel,
                                llm_messages2,
                                tools.clone(),
                                |c: &haven_llm::StreamChunk| {
                                    if let Some(t) = &c.text {
                                        thought_chunks.push(t.clone());
                                        let _ = chunk_tx2.send((task_id.to_string(), t.clone(), step_num, run_id));
                                    }
                                    if let Some(r) = &c.reasoning {
                                        let _ = reasoning_tx2.send((task_id.to_string(), r.clone(), step_num, run_id));
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
                                self.emit_task_error(task_id, "task cancelled by user").await;
                                return Ok(history);
                            }
                            Err(e2) => {
                                let err_msg = format!("Compaction retry also failed: {}", e2);
                                self.emit_task_error(task_id, &err_msg).await;
                                self.executor
                                    .update_task_status(task_id, TaskStatus::Error)
                                    .await?;
                                return Err(anyhow::anyhow!("{}", err_msg));
                            }
                        }
                    } else {
                        let err_msg = "context length exceeded but compaction failed".to_string();
                        self.emit_task_error(task_id, &err_msg).await;
                        self.executor
                            .update_task_status(task_id, TaskStatus::Error)
                            .await?;
                        return Err(anyhow::anyhow!("{}", err_msg));
                    }
                }
                Err(haven_llm::LlmError::Cancelled) => {
                    self.emit_task_error(task_id, "task cancelled by user").await;
                    return Ok(history);
                }
                Err(e) => {
                    let err_msg = format!("Both reasoner and fallback failed: {}", e);
                    self.emit_task_error(task_id, &err_msg).await;
                    self.executor
                        .update_task_status(task_id, TaskStatus::Error)
                        .await?;
                    return Err(anyhow::anyhow!("{}", err_msg));
                }
            };

            drop(chunk_tx);
            drop(reasoning_tx);
            if let Some(handle) = consumer_handle {
                let _ = handle.await;
            }

            let (thought, actions) = self.parse_reasoner_response(&response, step_num);

            if let Some(ref t) = thought {
                let thought_text = t
                    .lines()
                    .filter(|l| !l.starts_with("Action:") && !l.starts_with("Final Answer:"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !thought_text.trim().is_empty() {
                    self.emit_thought(task_id, &thought_text, step_num).await;

                    history.push(ReActStep {
                        step_number: step_num,
                        thought: Some(thought_text),
                        action: None,
                        observation: None,
                    });
                }
            }

            if actions.is_empty() {
                let msg = thought.unwrap_or_else(|| "No action decided.".into());
                self.emit_thought(task_id, &msg, step_num).await;
                history.push(ReActStep {
                    step_number: step_num,
                    thought: Some(msg.clone()),
                    action: None,
                    observation: None,
                });
                if let Some(last) = history.last_mut() {
                    last.action = Some(Action {
                        tool_name: "final_answer".into(),
                        tool_input: Value::Null,
                        is_final: true,
                    });
                    if last.observation.is_none() {
                        last.observation = Some(msg.clone());
                    }
                }
                self.executor
                    .update_task_status(task_id, TaskStatus::Paused)
                    .await?;
                self.emit_task_updated(task_id, "paused").await;
                self.persist_message("assistant", &msg, Some("text"));
                self.run_fact_inference();
                self.run_preference_inference();
                let _ = self.db.save_react_state(task_id, "");
                return Ok(history);
            }

            if let Some(final_action) = actions.iter().find(|a| a.is_final) {
                let final_text = thought.unwrap_or_else(|| "Task completed.".into());
                if let Some(s) = history.last_mut() {
                    s.action = Some(final_action.clone());
                    if s.observation.is_none() {
                        s.observation = Some(final_text.clone());
                    }
                }
                self.executor
                    .update_task_status(task_id, TaskStatus::Paused)
                    .await?;
                self.emit_task_updated(task_id, "paused").await;
                self.persist_message("assistant", &final_text, Some("text"));
                self.run_fact_inference();
                self.run_preference_inference();
                return Ok(history);
            }

            let non_final: Vec<&Action> = actions.iter().filter(|a| !a.is_final).collect();
            // Emit "Calling X" for each tool immediately, before execution.
            for action in &non_final {
                let silent = action.tool_input.get("silent").and_then(|v| v.as_bool()).unwrap_or(false);
                self.emit_action(task_id, &action.tool_name, &action.tool_input, step_num, silent).await;
            }

            let results: Vec<(String, String)> = futures_util::future::join_all(
                non_final.iter().map(|action| {
                    let task_id = task_id.to_string();
                    let tool_name = action.tool_name.clone();
                    let tool_input = action.tool_input.clone();
                    let max_obs = self.max_observation_chars;
                    let db = self.db.clone();
                    let executor = self.executor.clone();
                    async move {
                        let tool_name_for_err = tool_name.clone();
                        executor
                            .execute_step(&task_id, &tool_name, tool_input.clone())
                            .await
                            .map(|result| {
                                let _ = db.record_tool_usage(&tool_name, &tool_input, result.success);
                                let mut text = if result.success {
                                    serde_json::to_string(&result.output)
                                        .unwrap_or_else(|_| "success".into())
                                } else {
                                    result.error.unwrap_or_else(|| "unknown failure".into())
                                };
                                if text.len() > max_obs {
                                    text = format!(
                                        "{}[... truncated {} chars omitted]",
                                        &text[..max_obs],
                                        text.len() - max_obs
                                    );
                                }
                                (tool_name, text)
                            })
                            .unwrap_or_else(|e| (tool_name_for_err, e.to_string()))
                    }
                }),
            )
            .await;

            for (idx, action) in non_final.iter().enumerate() {
                let (ref tool_name, ref step_result) = results[idx];
                let silent = action.tool_input.get("silent").and_then(|v| v.as_bool()).unwrap_or(false);
                self.emit_observation(task_id, step_result, tool_name, step_num, silent)
                    .await;

                if let Some(last) = history.last_mut() {
                    last.action = Some((*action).clone());
                    last.observation = Some(step_result.clone());
                } else {
                    history.push(ReActStep {
                        step_number: step_num,
                        thought: None,
                        action: Some((*action).clone()),
                        observation: Some(step_result.clone()),
                    });
                }

                let obs_msg = format!("Tool '{}' result: {}", tool_name, step_result);
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::Assistant,
                    content: vec![ContentPart::text(response.text.clone())],
                    tool_calls: None,
                    tool_call_id: None,
                });
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(obs_msg)],
                    tool_calls: None,
                    tool_call_id: None,
                });
            }

            let state = self.executor.get_task_state(task_id).await;
            if state == TaskStatus::Cancelled || state == TaskStatus::Error || state == TaskStatus::Completed {
                return Ok(history);
            }
            } // end inner for loop

            // Inner loop exhausted (max steps) — check for follow-up items
            // before finalizing (refine §1.2 outer loop).
            let followups = self.executor.get_followup(task_id).await;
            if followups.is_empty() {
                break;
            }
            // Rebuild system prompt with follow-up context and continue
            for f in &followups {
                canonical.push(CanonicalMessage {
                    role: CanonicalRole::User,
                    content: vec![ContentPart::text(format!("Follow-up: {}", f))],
                    tool_calls: None,
                    tool_call_id: None,
                });
            }
        }

        self.executor
            .update_task_status(task_id, TaskStatus::Paused)
            .await?;
        self.persist_message("assistant", "Task completed.", Some("text"));
        self.run_fact_inference();
        self.run_preference_inference();
        Ok(history)
    }

    /// Run fact inference from the current session messages (M6-04).
    /// Uses rule-based extraction and optionally LLM-assisted inference.
    fn run_fact_inference(&self) {
        let session_id = self.session_id.lock().unwrap().clone();
        if let Ok(messages) = self.db.get_session_messages(&session_id) {
            let inferred = self.db.infer_facts_from_messages(&messages);
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
        let session_id = self.session_id.lock().unwrap().clone();
        if let Ok(messages) = self.db.get_session_messages(&session_id) {
            let inferred = self.db.infer_preferences_from_messages(&messages);
            let _ = self.db.save_inferred_preferences(&inferred);
        }
    }

    pub async fn process_input(
        &self,
        transcript: &str,
        active_task_id: Option<String>,
    ) -> anyhow::Result<ProcessResult> {
        // No active task → always NewTask, skip the classifier round-trip.
        let classification = if active_task_id.is_none() {
            Classification::NewTask {
                summary: transcript.to_string(),
                priority: TaskPriority::Normal,
                suggested_tools: Vec::new(),
            }
        } else {
            self.classify_intent(transcript, active_task_id.as_deref())
                .await
        };

        match classification {
            Classification::AppendToTask {
                task_id,
                additional_context,
            } => {
                self.persist_message("user", transcript, Some("text"));
                let was_in_memory = self.executor.add_supplement(&task_id, &additional_context).await.is_ok();
                if !was_in_memory {
                    self.executor.ensure_task_loaded(&task_id).await?;
                    self.executor.add_supplement(&task_id, &additional_context).await?;
                }
                let state = self.executor.get_task_state(&task_id).await;
                if state == haven_task::TaskStatus::Completed
                    || state == haven_task::TaskStatus::Error
                    || state == haven_task::TaskStatus::Cancelled
                {
                    self.executor
                        .update_task_status(&task_id, haven_task::TaskStatus::Pending)
                        .await?;
                } else if state == haven_task::TaskStatus::Running
                {
                    self.executor
                        .add_steering(&task_id, &additional_context)
                        .await?;
                } else if state == haven_task::TaskStatus::Paused {
                    if was_in_memory {
                        // Loop is alive (blocked on Notify), wake it directly.
                        self.executor
                            .update_task_status(&task_id, haven_task::TaskStatus::Running)
                            .await?;
                    } else {
                        // No running loop (app restart), hand off to dispatcher.
                        self.executor
                            .update_task_status(&task_id, haven_task::TaskStatus::Pending)
                            .await?;
                    }
                }
                Ok(ProcessResult::Supplemented)
            }
            Classification::NewTask {
                summary,
                priority,
                suggested_tools: _,
            } => {
                let session_id = self.start_new_session().unwrap_or_else(|_| self.ensure_session());
                self.persist_message("user", transcript, Some("text"));
                let task = self
                    .executor
                    .create_task_with_summary(transcript, "NewTask", priority, &summary, Some(&session_id))
                    .await?;
                self.emit_task_created(&task).await;
                // The background dispatcher will pick the task up respecting
                // priority/FIFO and the semaphore-bounded `max_concurrent`.
                Ok(ProcessResult::TaskCreated(task.id))
            }
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
    use haven_llm::{LlmClient, LlmError, StreamChunk, ToolCall};
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
        async fn on_thought(&self, _: &str, thought: &str, _: u32, _: u64) {
            self.thoughts.lock().unwrap().push(thought.into());
        }
        async fn on_action(&self, _: &str, _: &str, _: &Value, _: u32, _: u64) {}
        async fn on_observation(&self, _: &str, _: &str, _: &str, _: u32, _: u64, _: bool) {}
        async fn on_task_created(&self, _: &TaskInfo) {}
        async fn on_task_completed(&self, _: &str, _: &str) {
            *self.completed.lock().unwrap() = true;
        }
        async fn on_task_updated(&self, _: &str, _: &str) {
            *self.completed.lock().unwrap() = true;
        }
        async fn on_task_error(&self, _: &str, _: &str) {}
        async fn on_fallback_activated(&self, _: &str, _: &str) {}
        async fn on_supplement(&self, _: &str, ctx: &str, _: u32, _: u64) {
            self.supplements.lock().unwrap().push(ctx.into());
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
            haven_task::TaskStatus::Paused,
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
            client_a.clone(),
            client_a,
        ));
        let agent = Arc::new(AgentLayer::new(db, executor, router_a, 10, 20, 4000));
        let orig = agent.router();
        // Create a new router via the same mock client factory
        let client_b = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router_b = Arc::new(LlmRouter::new_with_clients(
            client_b.clone(),
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

    #[test]
    fn parse_classification_response_new_task() {
        let json = r#"{"classification":"NEW_TASK","summary":"open calculator","suggested_tools":["shell"]}"#;
        let result = AgentLayer::parse_classification_response(json, None);
        match result {
            Classification::NewTask { summary, priority, suggested_tools } => {
                assert_eq!(summary, "open calculator");
                assert_eq!(priority, TaskPriority::Normal);
                assert_eq!(suggested_tools, vec!["shell"]);
            }
            _ => panic!("expected NewTask"),
        }
    }

    #[test]
    fn parse_classification_response_append_to_task() {
        let json = r#"{"classification":"APPEND_TO_TASK","additional_context":"also check disk"}"#;
        let result = AgentLayer::parse_classification_response(json, Some("task-123"));
        match result {
            Classification::AppendToTask { task_id, additional_context } => {
                assert_eq!(task_id, "task-123");
                assert_eq!(additional_context, "also check disk");
            }
            _ => panic!("expected AppendToTask"),
        }
    }

    #[test]
    fn parse_classification_response_fallback_on_invalid_json() {
        let result = AgentLayer::parse_classification_response("not json at all", None);
        match result {
            Classification::NewTask { summary, priority, suggested_tools } => {
                assert_eq!(summary, "not json at all");
                assert_eq!(priority, TaskPriority::Normal);
                assert!(suggested_tools.is_empty());
            }
            _ => panic!("expected fallback NewTask"),
        }
    }

    #[test]
    fn parse_classification_response_priority_field() {
        let json = r#"{"classification":"NEW_TASK","summary":"urgent","priority":"critical"}"#;
        let result = AgentLayer::parse_classification_response(json, None);
        match result {
            Classification::NewTask { priority, .. } => {
                assert_eq!(priority, TaskPriority::Critical);
            }
            _ => panic!("expected NewTask with critical priority"),
        }
    }

    #[test]
    fn build_classifier_prompt_with_active_task() {
        let prompt = AgentLayer::build_classifier_prompt("remind me", true);
        assert!(prompt.contains("active task"));
        assert!(prompt.contains("remind me"));
        assert!(prompt.contains("NEW_TASK"));
        assert!(prompt.contains("APPEND_TO_TASK"));
    }

    #[test]
    fn build_classifier_prompt_without_active_task() {
        let prompt = AgentLayer::build_classifier_prompt("open notepad", false);
        assert!(prompt.contains("No active task exists"));
        assert!(prompt.contains("open notepad"));
        assert!(prompt.contains("NEW_TASK"));
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
            TaskStatus::Completed
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

    /// Mock classifier that returns a valid classification JSON.
    struct ClassifierOkMock;

    #[async_trait]
    impl LlmClient for ClassifierOkMock {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: r#"{"classification":"NEW_TASK","summary":"test summary","priority":"normal"}"#.into(),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: haven_llm::Usage { prompt_tokens: 0, completion_tokens: 0, total_tokens: 0, model_name: None, cost: None },
                model: None,
                reasoning: None,
            })
        }
        async fn chat_with_tools(&self, _: Vec<LlmMessage>, _: Vec<ToolDefinition>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("not implemented".into()))
        }
        async fn chat_stream(&self, _: Vec<LlmMessage>) -> Result<Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
            Err(LlmError::Unknown("not implemented".into()))
        }
        async fn chat_stream_with_tools(&self, _: Vec<LlmMessage>, _: Vec<ToolDefinition>) -> Result<Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
            Err(LlmError::Unknown("not implemented".into()))
        }
        async fn health_check(&self) -> Result<(), LlmError> { Ok(()) }
    }

    #[tokio::test]
    async fn classify_intent_returns_new_task_on_success() {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_classify_{}.db", uuid::Uuid::new_v4()));
        let db = Arc::new(Database::open(&p).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools, 1));
        let classifier = Arc::new(ClassifierOkMock) as Arc<dyn LlmClient>;
        let reasoner = Arc::new(FinalAnswerMock) as Arc<dyn LlmClient>;
        let router = Arc::new(LlmRouter::new_with_clients(
            reasoner.clone(), reasoner, classifier,
        ));
        let agent = AgentLayer::new(db, executor, router, 10, 20, 4000);

        let result = agent.classify_intent("open calculator", None).await;
        match result {
            Classification::NewTask { summary, priority, .. } => {
                assert_eq!(summary, "test summary");
                assert_eq!(priority, TaskPriority::Normal);
            }
            _ => panic!("expected NewTask"),
        }
    }

    #[tokio::test]
    async fn classify_intent_fallback_on_llm_error() {
        let (agent, _) = make_test_agent();
        let result = agent.classify_intent("some input", Some("task-1")).await;
        match result {
            Classification::NewTask { summary, .. } => {
                assert_eq!(summary, "some input");
            }
            _ => panic!("expected NewTask fallback on LLM error"),
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
