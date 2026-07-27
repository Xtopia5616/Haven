use haven_common::types::RiskLevel;
pub use haven_common::types::TaskPriority;
use haven_memory::Database;
use haven_tools::{ConfirmationResult, McpToolAdapter, SkillToolAdapter, ToolResult, ToolsManager};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// Runner invoked by the dispatcher for each picked task. The closure must
/// perform the ReAct loop for `task_id` and return `Ok(())` on completion.
/// It is responsible for acquiring no permits (dispatcher already does) but
/// is expected to update the task status on completion/error.
pub type RunHandler =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

const DISPATCH_POLL_MS: u64 = 5000;
const DISPATCH_LOG_INTERVAL: u64 = 200; // log every ~20s instead of every 100ms

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Error,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Paused => "paused",
            TaskStatus::Completed => "completed",
            TaskStatus::Error => "error",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_status_str(s: &str) -> Self {
        match s {
            "pending" => TaskStatus::Pending,
            "running" => TaskStatus::Running,
            "paused" => TaskStatus::Paused,
            "completed" => TaskStatus::Completed,
            "error" => TaskStatus::Error,
            "cancelled" => TaskStatus::Cancelled,
            _ => TaskStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskInfo {
    pub id: String,
    pub input: String,
    /// LLM-produced one-line summary used as the ReAct task description
    /// when the dispatcher runs the task. Defaults to `input` when no
    /// classifier summary is available.
    pub summary: String,
    /// LLM-generated short title for display. Set automatically after the
    /// first ReAct loop completes, or manually by the user.
    pub title: Option<String>,
    pub status: TaskStatus,
    pub classification: String,
    pub priority: TaskPriority,
    pub steps: Vec<StepInfo>,
    pub supplement_queue: Vec<String>,
    /// Steering queue: items that should interrupt the current tool sequence
    /// and be injected as context immediately (refine §1.2).
    pub steering_queue: Vec<String>,
    /// Follow-up queue: items to process after the current task completes
    /// (refine §1.2).
    pub followup_queue: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
    /// True after the dispatcher picks this task at least once.
    /// Used to distinguish fresh tasks from pending ones in `take_next_pending`.
    pub dispatched_once: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepInfo {
    pub id: String,
    pub step_index: i32,
    pub tool_name: String,
    pub input: Value,
    pub output: Option<Value>,
    pub status: String,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

type ConfirmRequestCallback = Arc<Mutex<Option<Box<dyn Fn(String, String, RiskLevel) + Send>>>>;

pub struct TaskExecutor {
    db: Arc<Database>,
    tools: Arc<ToolsManager>,
    tasks: Arc<Mutex<Vec<TaskInfo>>>,
    running_tasks: Arc<Mutex<HashSet<String>>>,
    semaphore: Arc<Semaphore>,
    /// Tracks the semaphore permit held by each running task's handler.
    /// When a task is paused, its permit is dropped so the dispatcher slot
    /// is freed. On resume the dispatcher re-acquires a permit.
    task_permits: Arc<Mutex<HashMap<String, OwnedSemaphorePermit>>>,
    /// Cancellation tokens for each task, used to abort in-flight LLM calls.
    task_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Per-task Notify signals so the ReAct loop can block on status changes
    /// instead of polling every 200ms.
    task_notify: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    pub on_confirm_request: ConfirmRequestCallback,
}

impl TaskExecutor {
    pub fn new(db: Arc<Database>, tools: Arc<ToolsManager>, max_concurrent: usize) -> Self {
        Self {
            db,
            tools,
            tasks: Arc::new(Mutex::new(Vec::new())),
            running_tasks: Arc::new(Mutex::new(HashSet::new())),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            task_permits: Arc::new(Mutex::new(HashMap::new())),
            task_cancellations: Arc::new(Mutex::new(HashMap::new())),
            task_notify: Arc::new(Mutex::new(HashMap::new())),
            on_confirm_request: Arc::new(Mutex::new(None)),
        }
    }

    fn insert_by_priority(tasks: &mut Vec<TaskInfo>, task: TaskInfo) {
        fn order(p: TaskPriority) -> u8 {
            match p {
                TaskPriority::Critical => 0,
                TaskPriority::High => 1,
                TaskPriority::Normal => 2,
                TaskPriority::Low => 3,
            }
        }
        let o = order(task.priority);
        let pos = tasks.iter().position(|t| order(t.priority) > o);
        match pos {
            Some(i) => tasks.insert(i, task),
            None => tasks.push(task),
        }
    }

    pub async fn create_task(
        &self,
        input: &str,
        classification: &str,
        priority: TaskPriority,
        session_id: Option<&str>,
    ) -> anyhow::Result<TaskInfo> {
        self.create_task_with_summary(input, classification, priority, input, session_id)
            .await
    }

    pub async fn create_task_with_summary(
        &self,
        input: &str,
        classification: &str,
        priority: TaskPriority,
        summary: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<TaskInfo> {
        let now = chrono::Utc::now().to_rfc3339();
        let record = self.db.create_task(session_id, input, classification, input)?;
        let task = TaskInfo {
            id: record.id,
            input: input.into(),
            summary: summary.into(),
            title: None,
            status: TaskStatus::Pending,
            classification: classification.into(),
            priority,
            steps: Vec::new(),
            supplement_queue: Vec::new(),
            steering_queue: Vec::new(),
            followup_queue: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
            dispatched_once: false,
        };
        let mut tasks = self.tasks.lock().await;
        Self::insert_by_priority(&mut tasks, task.clone());

        // Critical priority: preempt every currently running task to Paused so
        // its ReAct loop pauses at the next step boundary and the Critical job
        // wins the next dispatcher slot.
        if priority == TaskPriority::Critical {
            let running: Vec<String> = self.running_tasks.lock().await.iter().cloned().collect();
            for running_id in running {
                if let Some(running_task) = tasks.iter_mut().find(|t| t.id == running_id)
                    && running_task.status == TaskStatus::Running
                {
                    running_task.status = TaskStatus::Paused;
                    let _ = self.db.update_task_status(&running_id, "paused");
                }
            }
        }

        Ok(task)
    }

    /// Spawn the background dispatcher. Whenever a semaphore permit is free
    /// and a `Pending` task exists, the dispatcher calls `handler(task_id)`.
    /// The handler must perform the ReAct loop and finalize the task status.
    pub fn start_dispatcher(self: Arc<Self>, handler: RunHandler) {
        let exec = self.clone();
        tokio::spawn(async move {
            let mut log_counter: u64 = 0;
            loop {
                log_counter += 1;
                if log_counter.is_multiple_of(DISPATCH_LOG_INTERVAL) {
                    tracing::info!("dispatcher heartbeat (iter {})", log_counter);
                }
                let permit = match exec.semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::error!("task semaphore closed");
                        return;
                    }
                };

                let task_id = exec.take_next_pending().await;
                let Some(task_id) = task_id else {
                    drop(permit);
                    tokio::time::sleep(std::time::Duration::from_millis(DISPATCH_POLL_MS)).await;
                    continue;
                };

                exec.mark_running(&task_id).await;

                // Register the permit so pause/cancel can release it.
                {
                    let mut permits = exec.task_permits.lock().await;
                    permits.insert(task_id.clone(), permit);
                }
                // Create a cancellation token for this task.
                {
                    let mut cancels = exec.task_cancellations.lock().await;
                    cancels.insert(task_id.clone(), CancellationToken::new());
                }

                let exec_inner = exec.clone();
                let handler_inner = handler.clone();
                tracing::info!("dispatcher spawning handler for task: {:?}", task_id);
                tokio::spawn(async move {
                    if let Err(e) = handler_inner(task_id.clone()).await {
                        tracing::error!("dispatcher task {} failed: {}", task_id, e);
                        let _ = exec_inner
                            .update_task_status(&task_id, TaskStatus::Error)
                            .await;
                    }
                    exec_inner.unmark_running(&task_id).await;
                });
            }
        });
    }

    /// Pick the highest-priority `Pending` task ID. Atomically (under the
    /// `tasks` lock) marks the task as dispatched by moving it to `Running`'
    /// s "queued-to-run" stage via status — actually we keep it `Pending`
    /// until mark_running flips it. Here we just reserve it by removing from
    /// the pending set conceptually: returns the id; caller will mark_running.
    async fn take_next_pending(&self) -> Option<String> {
        let mut tasks = self.tasks.lock().await;
        tracing::info!("take_next_pending scanning {} tasks", tasks.len());
        for task in tasks.iter_mut() {
            if task.status == TaskStatus::Pending {
                // A task that has already been dispatched but
                // has no new supplements or steering is pending — skip it
                // until the user provides more context.
                if task.dispatched_once
                    && task.supplement_queue.is_empty()
                    && task.steering_queue.is_empty()
                {
                    continue;
                }
                tracing::info!("take_next_pending found: {:?}", task.id);
                return Some(task.id.clone());
            }
        }
        None
    }

    /// Mark a task as running: flip status + insert into running set + persist.
    async fn mark_running(&self, task_id: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.status = TaskStatus::Running;
            task.dispatched_once = true;
            task.updated_at = chrono::Utc::now().to_rfc3339();
            let _ = self.db.update_task_status(task_id, "running");
        }
        drop(tasks);
        self.running_tasks.lock().await.insert(task_id.to_string());
    }

    /// Remove a task from the running set. Terminal status updates are
    /// performed by the handler / agent loop. Also removes terminal-status
    /// tasks from the in-memory list so `take_next_pending` only counts
    /// active (Pending / Running) tasks.
    async fn unmark_running(&self, task_id: &str) {
        self.running_tasks.lock().await.remove(task_id);
        self.task_permits.lock().await.remove(task_id);
        self.task_cancellations.lock().await.remove(task_id);
        // Remove terminal tasks from the in-memory list so they don't clutter
        // the pending scan or the frontend task list.
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id)
            && (tasks[pos].status == TaskStatus::Error
                || tasks[pos].status == TaskStatus::Completed
                || tasks[pos].status == TaskStatus::Cancelled)
        {
            tasks.remove(pos);
        }
    }

    pub async fn running_count(&self) -> usize {
        self.running_tasks.lock().await.len()
    }

    pub async fn add_supplement(&self, task_id: &str, text: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.supplement_queue.push(text.into());
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    pub async fn get_supplements(&self, task_id: &str) -> Vec<String> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.supplement_queue.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    /// Add a steering item: interrupts the current tool sequence and is
    /// injected as context immediately (refine §1.2).
    pub async fn add_steering(&self, task_id: &str, text: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.steering_queue.push(text.into());
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    /// Drain the steering queue for a task.
    pub async fn get_steering(&self, task_id: &str) -> Vec<String> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.steering_queue.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    /// Add a follow-up item: processed after the current task completes
    /// (refine §1.2).
    pub async fn add_followup(&self, task_id: &str, text: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.followup_queue.push(text.into());
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    /// Drain the follow-up queue for a task.
    pub async fn get_followup(&self, task_id: &str) -> Vec<String> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.followup_queue.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    pub async fn cancel_task(&self, task_id: &str) -> anyhow::Result<()> {
        // Cancel the token first, before mutating any task list. This prevents
        // a TOCTOU race where ensure_task_loaded re-inserts the task between
        // the list removal and token cancellation (the re-inserted task would
        // have no active token, so cancelling the old one is harmless).
        if let Some(token) = self.task_cancellations.lock().await.remove(task_id) {
            token.cancel();
        }
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            tasks[pos].status = TaskStatus::Cancelled;
            tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
            self.running_tasks.lock().await.remove(task_id);
            self.db.update_task_status(task_id, "cancelled")?;
            self.task_permits.lock().await.remove(task_id);
            tasks.remove(pos);
        } else {
            // Task not in memory (e.g. after restart) — update DB directly.
            self.db.update_task_status(task_id, "cancelled")?;
        }
        Ok(())
    }

    /// End a task — marks as Completed and cleans up resources.
    /// Called from the frontend when the user explicitly taps "结束任务".
    /// Works for tasks both in memory and DB-only (e.g. after app restart).
    pub async fn end_task(&self, task_id: &str) -> anyhow::Result<()> {
        // Cancel the running token first to interrupt any active ReAct loop.
        let cancel = self.cancellation_token(task_id).await;
        cancel.cancel();
        // Acquire tasks lock BEFORE task_notify to prevent ABBA deadlock with
        // update_task_status (which locks tasks → task_notify). Notifying and
        // status update happen under the tasks lock scope.
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            tasks[pos].status = TaskStatus::Completed;
            tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
            self.db.update_task_status(task_id, "completed")?;
            tasks.remove(pos);
            drop(tasks);
            self.running_tasks.lock().await.remove(task_id);
            self.task_permits.lock().await.remove(task_id);
            if let Some(notify) = self.task_notify.lock().await.remove(task_id) {
                notify.notify_waiters();
            }
            self.task_cancellations.lock().await.remove(task_id);
        } else {
            // Task not in memory (e.g. after restart) — update DB directly.
            self.db.update_task_status(task_id, "completed")?;
        }
        Ok(())
    }

    /// Remove a task entirely from the in-memory state.
    /// This does NOT delete from DB — the caller handles that.
    /// Succeeds even if the task is not in memory (e.g. after restart).
    pub async fn remove_task(&self, task_id: &str) {
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|t| t.id != task_id);
        drop(tasks);
        self.running_tasks.lock().await.remove(task_id);
        self.task_permits.lock().await.remove(task_id);
        self.task_cancellations.lock().await.remove(task_id);
    }

    pub async fn update_task_title(&self, task_id: &str, title: &str) {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.title = Some(title.into());
        }
    }

    pub async fn pause_task(&self, task_id: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            if task.status == TaskStatus::Running {
                task.status = TaskStatus::Paused;
                self.db.update_task_status(task_id, "paused")?;
                drop(tasks);
                // Release the semaphore permit so the slot can be used by
                // another pending task. The dispatcher will re-acquire a
                // permit when the task is resumed and picked up again.
                self.task_permits.lock().await.remove(task_id);
            }
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    pub async fn resume_task(&self, task_id: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            if task.status == TaskStatus::Paused {
                task.status = TaskStatus::Pending;
                self.db.update_task_status(task_id, "pending")?;
            }
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    pub async fn list_tasks(&self) -> Vec<TaskInfo> {
        self.tasks.lock().await.clone()
    }

    /// Remove all tasks from memory and clean up running state.
    /// Used when the user clears history — the DB is already wiped.
    pub async fn clear_all_tasks(&self) {
        let mut tasks = self.tasks.lock().await;
        tasks.clear();
        drop(tasks);
        self.running_tasks.lock().await.clear();
        self.task_permits.lock().await.clear();
        self.task_cancellations.lock().await.clear();
    }

    /// Get or create a Notify for a given task. The agent loop awaits this
    /// to block on status changes instead of polling.
    pub async fn status_notifier(&self, task_id: &str) -> Arc<Notify> {
        let mut map = self.task_notify.lock().await;
        map.entry(task_id.to_string())
            .or_insert_with(|| Arc::new(Notify::new()))
            .clone()
    }

    pub async fn update_task_status(
        &self,
        task_id: &str,
        status: TaskStatus,
    ) -> anyhow::Result<()> {
        let status_str = status.as_str().to_string();
        let is_terminal = status_str == "cancelled" || status_str == "completed" || status_str == "error";
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            let old_status = tasks[pos].status.as_str();
            tasks[pos].status = status;
            tracing::info!("update_task_status: task={} {} -> {}", task_id, old_status, status_str);
            tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
            self.db.update_task_status(task_id, &status_str)?;
            // Notify any waiter that status has changed.
            if let Some(notify) = self.task_notify.lock().await.get(task_id) {
                notify.notify_waiters();
            }
            if is_terminal {
                self.running_tasks.lock().await.remove(task_id);
                self.task_permits.lock().await.remove(task_id);
                self.task_cancellations.lock().await.remove(task_id);
                self.task_notify.lock().await.remove(task_id);
                self.tools.unregister_task(task_id).await;
                tasks.remove(pos);
            }
        }
        Ok(())
    }

    pub async fn cancellation_token(&self, task_id: &str) -> CancellationToken {
        self.task_cancellations
            .lock()
            .await
            .get(task_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Load a task from the database into the in-memory list if it is not
    /// already there (e.g. after an app restart). Used by `supplement_task`
    /// so that follow-up messages can reach tasks that were paused before
    /// the restart and never re-entered the executor's working set.
    pub async fn ensure_task_loaded(&self, task_id: &str) -> anyhow::Result<()> {
        {
            let tasks = self.tasks.lock().await;
            if tasks.iter().any(|t| t.id == task_id) {
                return Ok(());
            }
        }
        let record = self
            .db
            .get_task(task_id)?
            .ok_or_else(|| anyhow::anyhow!("task '{}' not found in database", task_id))?;
        let task = TaskInfo {
            id: record.id,
            input: record.input_text,
            summary: record.transcript,
            title: record.title,
            status: TaskStatus::from_status_str(&record.status),
            classification: record.classification,
            priority: TaskPriority::Normal,
            steps: Vec::new(),
            supplement_queue: Vec::new(),
            steering_queue: Vec::new(),
            followup_queue: Vec::new(),
            created_at: record.created_at,
            updated_at: record.updated_at,
            dispatched_once: true,
        };
        let mut tasks = self.tasks.lock().await;
        // Re-check: another thread may have inserted this task between the
        // check above and the DB query.
        if !tasks.iter().any(|t| t.id == task_id) {
            Self::insert_by_priority(&mut tasks, task);
        }
        Ok(())
    }

    pub async fn get_task_state(&self, task_id: &str) -> TaskStatus {
        let tasks = self.tasks.lock().await;
        tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.status.clone())
            .unwrap_or(TaskStatus::Cancelled)
    }

    pub fn get_tools(&self) -> Arc<ToolsManager> {
        self.tools.clone()
    }

    pub async fn execute_step(
        &self,
        task_id: &str,
        tool_name: &str,
        input: Value,
    ) -> anyhow::Result<ToolResult> {
        tracing::info!("execute_step: task={} tool={} input={:?}", task_id, tool_name, input);
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                let prev = task.status.as_str();
                task.status = TaskStatus::Running;
                if prev != "running" {
                    tracing::warn!("execute_step: task {} was {} before tool call, forcing Running", task_id, prev);
                }
            }
        }

        let cancel = self.cancellation_token(task_id).await;
        let result = self
            .tools
            .execute_tool(Some(task_id), tool_name, input.clone(), cancel)
            .await?;
        tracing::info!("execute_step result: tool={} success={}", tool_name, result.success);

        let risk_level = self.tools.get_risk_level(Some(task_id), tool_name, &input).await;

        // Register skill adapter per-task on successful load_skill
        // instead of polluting the global registry (refine §6).
        if result.success && tool_name == "load_skill"
            && let Some(skill_name) = result.output["skill"]["name"].as_str()
        {
            let clean_name = skill_name.strip_prefix("skill::").unwrap_or(skill_name);
            if let Some(skill) = self.tools.skills_engine.get_skill(clean_name).await {
                let runner = self.tools.skill_runner.read().await.clone();
                let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
                self.tools.register_for_task(task_id, Arc::new(adapter)).await;
            }
        }

        // Register MCP tool adapters per-task on successful load_mcp
        if result.success && tool_name == "load_mcp"
            && let Some(server_name) = result.output["server_name"].as_str()
            && let Some(client) = self.tools.mcp_manager.get_client(server_name).await
        {
            let tools = client.tools_cache().await;
            for info in tools {
                let adapter = McpToolAdapter::new(client.clone(), server_name, info);
                self.tools.register_for_task(task_id, Arc::new(adapter)).await;
            }
        }

        let step_index: i32;
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                step_index = task.steps.len() as i32;
                task.steps.push(StepInfo {
                    id: uuid::Uuid::new_v4().to_string(),
                    step_index,
                    tool_name: tool_name.into(),
                    input: input.clone(),
                    output: Some(result.output.clone()),
                    status: if result.success {
                        "completed".into()
                    } else {
                        "failed".into()
                    },
                    risk_level,
                    confirmed: None,
                });
                task.updated_at = chrono::Utc::now().to_rfc3339();
            } else {
                step_index = 0;
            }
        }

        let step_record = self.db.create_action_step(
            task_id,
            step_index,
            tool_name,
            &input.to_string(),
            risk_level != RiskLevel::Safe,
        )?;
        let obs = if result.success {
            serde_json::to_string(&result.output).unwrap_or_else(|_| "success".into())
        } else {
            result.error.clone().unwrap_or_else(|| "unknown failure".into())
        };
        self.db.complete_action_step(&step_record.id, &obs, result.success)?;
        Ok(result)
    }

    async fn add_step(
        &self,
        task_id: &str,
        tool_name: &str,
        input: Value,
        risk_level: RiskLevel,
    ) -> anyhow::Result<StepInfo> {
        let step_index = {
            let tasks = self.tasks.lock().await;
            tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.steps.len() as i32)
                .unwrap_or(0)
        };
        let is_high_risk = risk_level != RiskLevel::Safe;
        let record = self.db.create_action_step(
            task_id,
            step_index,
            tool_name,
            &input.to_string(),
            is_high_risk,
        )?;
        let step = StepInfo {
            id: record.id,
            step_index,
            tool_name: tool_name.into(),
            input,
            output: None,
            status: "pending".into(),
            risk_level,
            confirmed: None,
        };
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.steps.push(step.clone());
        }
        Ok(step)
    }

    pub async fn execute_task(
        &self,
        task_id: &str,
        steps: Vec<(String, Value, RiskLevel)>,
    ) -> anyhow::Result<()> {
        let _permit = self.semaphore.acquire().await?;
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                task.status = TaskStatus::Running;
            }
            self.db.update_task_status(task_id, "running")?;
            self.running_tasks.lock().await.insert(task_id.to_string());
        }

        for (tool_name, input, risk_level) in &steps {
            // Check if cancelled or paused
            {
                let tasks = self.tasks.lock().await;
                let task = tasks.iter().find(|t| t.id == task_id).unwrap();
                if task.status == TaskStatus::Cancelled {
                    return Ok(());
                }
                if task.status == TaskStatus::Paused {
                    drop(tasks);
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        let tasks = self.tasks.lock().await;
                        let task = tasks.iter().find(|t| t.id == task_id).unwrap();
                        if task.status == TaskStatus::Running
                            || task.status == TaskStatus::Cancelled
                        {
                            break;
                        }
                    }
                }
            }

            let step = self
                .add_step(task_id, tool_name, input.clone(), *risk_level)
                .await?;
            self.db.update_step_status(&step.id, "running", None)?;

            // Handle confirmation via SafetyGateway
            let confirmation = self
                .tools
                .safety_gateway
                .check(tool_name, input, *risk_level)
                .await;
            match confirmation {
                ConfirmationResult::RequiresConfirmation {
                    tool_name: _,
                    params: _,
                    risk_level: rl,
                } => {
                    let mut tasks = self.tasks.lock().await;
                    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id)
                        && let Some(step_mut) = task.steps.iter_mut().find(|s| s.id == step.id)
                    {
                        step_mut.status = "waiting_confirmation".into();
                    }
                    drop(tasks);

                    let cb = self.on_confirm_request.lock().await;
                    if let Some(ref callback) = *cb {
                        callback(step.id.clone(), tool_name.clone(), rl);
                    }
                    drop(cb);

                    let mut confirmed = false;
                    for _ in 0..30 {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        let tasks = self.tasks.lock().await;
                        if let Some(task) = tasks.iter().find(|t| t.id == task_id)
                            && let Some(s) = task.steps.iter().find(|s| s.id == step.id)
                            && s.confirmed.is_some()
                        {
                            confirmed = s.confirmed.unwrap();
                            break;
                        }
                        let tasks = self.tasks.lock().await;
                        if let Some(task) = tasks.iter().find(|t| t.id == task_id)
                            && task.status == TaskStatus::Cancelled
                        {
                            break;
                        }
                    }

                    if !confirmed {
                        self.db.update_step_status(&step.id, "cancelled", None)?;
                        let mut tasks = self.tasks.lock().await;
                        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id)
                            && let Some(s) = task.steps.iter_mut().find(|s| s.id == step.id)
                        {
                            s.status = "cancelled".into();
                        }
                        continue;
                    }
                }
                ConfirmationResult::Blocked => {
                    self.db.update_step_status(&step.id, "cancelled", None)?;
                    continue;
                }
                ConfirmationResult::AutoApproved => {}
            }

            // Execute
            let cancel = CancellationToken::new();
            let result = self
                .tools
                .execute_tool(Some(task_id), tool_name, input.clone(), cancel)
                .await;

            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id)
                && let Some(s) = task.steps.iter_mut().find(|s| s.id == step.id)
            {
                match result {
                    Ok(r) => {
                        s.status = if r.success {
                            "completed".into()
                        } else {
                            "failed".into()
                        };
                        s.output = Some(r.output);
                        self.db.update_step_status(
                            &step.id,
                            &s.status,
                            s.output.as_ref().map(|v| v.to_string()).as_deref(),
                        )?;
                    }
                    Err(e) => {
                        s.status = "failed".into();
                        s.output = Some(serde_json::json!({"error": e.to_string()}));
                        self.db.update_step_status(
                            &step.id,
                            "failed",
                            s.output.as_ref().map(|v| v.to_string()).as_deref(),
                        )?;
                    }
                }
            }
        }

        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id)
            && task.status != TaskStatus::Cancelled
        {
            task.status = TaskStatus::Completed;
            self.db.update_task_status(task_id, "completed")?;
        }
        self.running_tasks.lock().await.remove(task_id);

        Ok(())
    }

    pub fn confirm_step(&self, step_id: &str, confirmed: bool) -> anyhow::Result<()> {
        self.db.confirm_step(step_id, confirmed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_task_test_{}.db", uuid::Uuid::new_v4()));
        p
    }

    fn make_executor(max_concurrent: usize) -> Arc<TaskExecutor> {
        let path = temp_db_path();
        let db = Arc::new(Database::open(&path).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, max_concurrent));
        // Best-effort cleanup; failures are ignored since the OS will purge
        // temp files eventually.
        let _ = path;
        exec
    }

    /// Dispatcher honors `max_concurrent` and drains all Pending tasks.
    #[tokio::test]
    async fn dispatcher_respects_max_concurrent() {
        let exec = make_executor(2);

        let current = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let completed = Arc::new(AtomicU32::new(0));

        let cur = current.clone();
        let pk = peak.clone();
        let done = completed.clone();
        let exec_ref = exec.clone();
        let handler: RunHandler = Arc::new(move |id: String| {
            let cur = cur.clone();
            let pk = pk.clone();
            let done = done.clone();
            let exec_ref = exec_ref.clone();
            Box::pin(async move {
                let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                pk.fetch_max(n, Ordering::SeqCst);
                assert!(n <= 2, "concurrency exceeded max_concurrent=2: {}", n);
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                cur.fetch_sub(1, Ordering::SeqCst);
                let _ = exec_ref
                    .update_task_status(&id, TaskStatus::Completed)
                    .await;
                done.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });

        for i in 0..5 {
            exec.create_task(&format!("task {}", i), "NewTask", TaskPriority::Normal, None)
                .await
                .unwrap();
        }

        exec.clone().start_dispatcher(handler);

        for _ in 0..200 {
            if completed.load(Ordering::SeqCst) == 5 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert_eq!(completed.load(Ordering::SeqCst), 5);
        assert!(
            peak.load(Ordering::SeqCst) >= 1 && peak.load(Ordering::SeqCst) <= 2,
            "peak concurrent out of expected range: {}",
            peak.load(Ordering::SeqCst)
        );
    }

    /// A High-priority task must be picked before a later Normal task.
    #[tokio::test]
    async fn dispatcher_prefers_high_priority() {
        let exec = make_executor(1);

        // Use a release gate so the handler doesn't start before both tasks
        // are queued; max_concurrent=1 means only the first-picked task runs
        // first. The runner records the order it sees.
        let order = Arc::new(Mutex::new(Vec::<String>::new()));
        let gate = Arc::new(tokio::sync::Notify::new());

        let order_ref = order.clone();
        let exec_ref = exec.clone();
        let gate_ref = gate.clone();
        let handler: RunHandler = Arc::new(move |id: String| {
            let order = order_ref.clone();
            let exec = exec_ref.clone();
            let gate = gate_ref.clone();
            Box::pin(async move {
                order.lock().await.push(id.clone());
                gate.notify_waiters();
                // Let the dispatcher pick up the next pending task.
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                let _ = exec.update_task_status(&id, TaskStatus::Completed).await;
                Ok(())
            })
        });

        let normal = exec
            .create_task("low-input", "NewTask", TaskPriority::Normal, None)
            .await
            .unwrap();
        let high = exec
            .create_task("high-input", "NewTask", TaskPriority::High, None)
            .await
            .unwrap();

        exec.clone().start_dispatcher(handler);

        // Wait for both entries to appear in the order vec.
        for _ in 0..200 {
            if order.lock().await.len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let seq = order.lock().await.clone();
        assert_eq!(seq.len(), 2);
        assert_eq!(seq[0], high.id, "high-priority task must run first");
        assert_eq!(seq[1], normal.id, "normal-priority task must run second");
        let _ = gate;
    }

    /// Critical-priority submission preempts a running Normal task.
    #[tokio::test]
    async fn dispatcher_critical_preempts_running() {
        let exec = make_executor(1);

        let started = Arc::new(AtomicU32::new(0));
        let completed = Arc::new(AtomicU32::new(0));

        let started_ref = started.clone();
        let completed_ref = completed.clone();
        let exec_ref = exec.clone();
        let handler: RunHandler = Arc::new(move |id: String| {
            let s = started_ref.clone();
            let d = completed_ref.clone();
            let exec = exec_ref.clone();
            Box::pin(async move {
                s.fetch_add(1, Ordering::SeqCst);
                // Give the test time to submit the Critical task while this
                // handler is in its first (dummy) step.
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                let _ = exec.update_task_status(&id, TaskStatus::Completed).await;
                d.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });

        let normal = exec
            .create_task("normal", "NewTask", TaskPriority::Normal, None)
            .await
            .unwrap();

        exec.clone().start_dispatcher(handler);

        // Wait for the normal task to actually start.
        for _ in 0..100 {
            if started.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        assert_eq!(started.load(Ordering::SeqCst), 1);
        // Before Critical submission, the normal task is Running.
        let statuses = exec.list_tasks().await;
        let normal_status = statuses
            .iter()
            .find(|t| t.id == normal.id)
            .map(|t| t.status.clone())
            .unwrap();
        assert_eq!(normal_status, TaskStatus::Running);

        // Submit Critical; create_task flips the Running normal task to Paused.
        let critical = exec
            .create_task("critical", "NewTask", TaskPriority::Critical, None)
            .await
            .unwrap();

        // The normal task must now be Paused.
        let statuses = exec.list_tasks().await;
        let normal_status = statuses
            .iter()
            .find(|t| t.id == normal.id)
            .map(|t| t.status.clone())
            .unwrap();
        assert_eq!(normal_status, TaskStatus::Paused);

        // Wait for both tasks to eventually complete.
        for _ in 0..200 {
            if completed.load(Ordering::SeqCst) == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(completed.load(Ordering::SeqCst), 2);
        // Critical must finish (we're waiting for completions, run_handler is the
        // dummy which doesn't touch run_task; completion via update_task_status
        // is enough for this dispatcher test).
        let _ = critical.id;
    }

    // ─── Data-layer tests (no dispatcher required) ───

    fn temp_db() -> Arc<Database> {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_task_test_{}.db", uuid::Uuid::new_v4()));
        Arc::new(Database::open(&p).unwrap())
    }

    #[tokio::test]
    async fn constructor_creates_executor() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db.clone(), tools.clone(), 3);
        assert_eq!(exec.running_count().await, 0);
        assert!(exec.list_tasks().await.is_empty());
    }

    #[tokio::test]
    async fn create_task_returns_pending_task() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("hello world", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.input, "hello world");
        assert_eq!(task.classification, "NEW_TASK");
        assert!(!task.id.is_empty());
        assert!(!task.created_at.is_empty());
    }

    #[tokio::test]
    async fn create_task_with_summary_preserves_fields() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task_with_summary("raw input", "NEW_TASK", TaskPriority::High, "summary text", None)
            .await
            .unwrap();
        assert_eq!(task.input, "raw input");
        assert_eq!(task.summary, "summary text");
        assert_eq!(task.priority, TaskPriority::High);
    }

    #[tokio::test]
    async fn cancel_task_changes_status_and_triggers_token() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        // Insert a token as the dispatcher would, so cancel_task can trigger it
        let real_token = CancellationToken::new();
        let clone = real_token.clone();
        exec.task_cancellations
            .lock()
            .await
            .insert(task.id.clone(), clone);
        assert!(!real_token.is_cancelled());
        exec.cancel_task(&task.id).await.unwrap();
        assert!(real_token.is_cancelled());
        let state = exec.get_task_state(&task.id).await;
        assert_eq!(state, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancel_nonexistent_task_succeeds() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        // cancel_task no longer errors for tasks not in memory
        // (it falls back to DB-only update).
        let result = exec.cancel_task("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn end_task_marks_completed() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        exec.end_task(&task.id).await.unwrap();
        // After end_task the task is removed from the in-memory list
        // (no longer polluting dispatcher scans).
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn pause_and_resume_task() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();

        // Manually move to Running since no dispatcher is running
        {
            let mut tasks = exec.tasks.lock().await;
            if let Some(t) = tasks.iter_mut().find(|t| t.id == task.id) {
                t.status = TaskStatus::Running;
            }
            exec.running_tasks.lock().await.insert(task.id.clone());
        }

        exec.pause_task(&task.id).await.unwrap();
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Paused);

        exec.resume_task(&task.id).await.unwrap();
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn pause_task_on_non_running_is_noop() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        // Task is Pending, not Running — pause should be a no-op
        let result = exec.pause_task(&task.id).await;
        assert!(result.is_ok());
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn resume_task_on_non_paused_is_noop() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        // Task is Pending, not Paused — resume should be a no-op
        let result = exec.resume_task(&task.id).await;
        assert!(result.is_ok());
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn add_and_get_supplements() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        exec.add_supplement(&task.id, "extra context 1").await.unwrap();
        exec.add_supplement(&task.id, "extra context 2").await.unwrap();
        let drained: Vec<String> = exec.get_supplements(&task.id).await;
        assert_eq!(drained, vec!["extra context 1", "extra context 2"]);
        assert!(exec.get_supplements(&task.id).await.is_empty());
    }

    #[tokio::test]
    async fn add_supplement_nonexistent_task_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let result = exec.add_supplement("nonexistent", "ctx").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn add_and_get_steering() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        exec.add_steering(&task.id, "steer 1").await.unwrap();
        let drained: Vec<String> = exec.get_steering(&task.id).await;
        assert_eq!(drained, vec!["steer 1"]);
    }

    #[tokio::test]
    async fn add_and_get_followup() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        exec.add_followup(&task.id, "followup 1").await.unwrap();
        let drained: Vec<String> = exec.get_followup(&task.id).await;
        assert_eq!(drained, vec!["followup 1"]);
    }

    #[tokio::test]
    async fn list_tasks_respects_priority_order() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);

        let _low = exec
            .create_task("low", "NEW_TASK", TaskPriority::Low, None)
            .await
            .unwrap();
        let _normal = exec
            .create_task("normal", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        let _high = exec
            .create_task("high", "NEW_TASK", TaskPriority::High, None)
            .await
            .unwrap();
        let _critical = exec
            .create_task("critical", "NEW_TASK", TaskPriority::Critical, None)
            .await
            .unwrap();

        let tasks = exec.list_tasks().await;
        assert_eq!(tasks.len(), 4);
        // Sorted by priority: Critical > High > Normal > Low
        assert_eq!(tasks[0].input, "critical");
        assert_eq!(tasks[1].input, "high");
        assert_eq!(tasks[2].input, "normal");
        assert_eq!(tasks[3].input, "low");
    }

    #[tokio::test]
    async fn get_task_state_returns_correct_status() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn get_task_state_nonexistent_returns_cancelled() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        assert_eq!(
            exec.get_task_state("nonexistent").await,
            TaskStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn cancellation_token_returns_default_for_unknown_task() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let token = exec.cancellation_token("nonexistent").await;
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn critical_priority_preempts_running_tasks_in_memory() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let normal = exec
            .create_task("normal", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        let high = exec
            .create_task("high", "NEW_TASK", TaskPriority::High, None)
            .await
            .unwrap();

        // Manually move normal to Running
        {
            let mut tasks = exec.tasks.lock().await;
            if let Some(t) = tasks.iter_mut().find(|t| t.id == normal.id) {
                t.status = TaskStatus::Running;
            }
            exec.running_tasks.lock().await.insert(normal.id.clone());
        }

        // Create a Critical task; only the Running Normal task is preempted
        let _critical = exec
            .create_task("critical", "NEW_TASK", TaskPriority::Critical, None)
            .await
            .unwrap();

        let tasks = exec.list_tasks().await;
        let normal_status = tasks
            .iter()
            .find(|t| t.id == normal.id)
            .map(|t| t.status.clone())
            .unwrap();
        assert_eq!(normal_status, TaskStatus::Paused);
        // High was Pending (not Running), so it stays Pending
        let high_status = tasks
            .iter()
            .find(|t| t.id == high.id)
            .map(|t| t.status.clone())
            .unwrap();
        assert_eq!(high_status, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn update_task_status_changes_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        exec.update_task_status(&task.id, TaskStatus::Completed)
            .await
            .unwrap();
        // Terminal status removes the task from the in-memory list
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn create_task_with_summary_critical_preempts() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let normal = exec
            .create_task("normal", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        {
            let mut tasks = exec.tasks.lock().await;
            if let Some(t) = tasks.iter_mut().find(|t| t.id == normal.id) {
                t.status = TaskStatus::Running;
            }
            exec.running_tasks.lock().await.insert(normal.id.clone());
        }
        let _critical = exec
            .create_task_with_summary("critical", "NEW_TASK", TaskPriority::Critical, "urgent", None)
            .await
            .unwrap();
        let tasks = exec.list_tasks().await;
        let normal_status = tasks.iter().find(|t| t.id == normal.id).map(|t| t.status.clone()).unwrap();
        assert_eq!(normal_status, TaskStatus::Paused);
    }

    #[tokio::test]
    async fn update_task_status_completed_cleans_up() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        exec.running_tasks.lock().await.insert(task.id.clone());
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = sem.clone().acquire_owned().await.unwrap();
        exec.task_permits.lock().await.insert(task.id.clone(), permit);
        exec.task_cancellations.lock().await.insert(task.id.clone(), CancellationToken::new());

        exec.update_task_status(&task.id, TaskStatus::Completed).await.unwrap();
        assert!(!exec.running_tasks.lock().await.contains(&task.id));
        assert!(exec.task_permits.lock().await.get(&task.id).is_none());
    }

    #[tokio::test]
    async fn execute_step_unknown_tool_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec
            .create_task("test", "NEW_TASK", TaskPriority::Normal, None)
            .await
            .unwrap();
        let result = exec.execute_step(&task.id, "nonexistent_tool", serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[test]
    fn confirm_step_nonexistent_does_not_panic() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let result = exec.confirm_step("nonexistent-step", true);
        // lenient: DB UPDATE on missing step is a no-op
        assert!(result.is_ok());
    }
}
