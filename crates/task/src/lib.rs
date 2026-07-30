use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_tools::{ToolResult, ToolsManager};
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

const DISPATCH_POLL_MS: u64 = 1000;
const DISPATCH_LOG_INTERVAL: u64 = 200; // log every ~20s instead of every 100ms

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Error,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Paused => "paused",
            TaskStatus::Completed => "completed",
            TaskStatus::Error => "error",
        }
    }

    pub fn from_status_str(s: &str) -> Self {
        match s {
            "pending" => TaskStatus::Pending,
            "running" => TaskStatus::Running,
            "paused" => TaskStatus::Paused,
            "completed" => TaskStatus::Completed,
            "error" => TaskStatus::Error,
            // Legacy: "cancelled" is treated as "error" since the variant was removed.
            "cancelled" => TaskStatus::Error,
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
    pub steps: Vec<StepInfo>,
    pub supplement_queue: Vec<String>,
    /// Steering queue: items that should interrupt the current tool sequence
    /// and be injected as context immediately (refine §1.2).
    pub steering_queue: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
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
    /// Notified when a task transitions to Pending, waking the dispatcher
    /// immediately instead of waiting for the next poll cycle.
    dispatch_notify: Arc<Notify>,
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
            dispatch_notify: Arc::new(Notify::new()),
            on_confirm_request: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn create_task(
        &self,
        input: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<TaskInfo> {
        self.create_task_with_summary(input, input, session_id)
            .await
    }

    pub async fn create_task_with_summary(
        &self,
        input: &str,
        summary: &str,
        session_id: Option<&str>,
    ) -> anyhow::Result<TaskInfo> {
        let now = chrono::Utc::now().to_rfc3339();
        let record = self.db.create_task(session_id, input, input)?;
        let task = TaskInfo {
            id: record.id,
            input: input.into(),
            summary: summary.into(),
            title: None,
            status: TaskStatus::Pending,
            steps: Vec::new(),
            supplement_queue: Vec::new(),
            steering_queue: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        let mut tasks = self.tasks.lock().await;
        tasks.push(task.clone());

        // Wake the dispatcher so it picks up this Pending task immediately.
        self.dispatch_notify.notify_one();
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
                    tracing::debug!("dispatcher heartbeat (iter {})", log_counter);
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
                    // Wait for a new Pending task to be signaled, but fall
                    // back to a periodic poll in case a notify is missed.
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(DISPATCH_POLL_MS),
                        exec.dispatch_notify.notified(),
                    )
                    .await;
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

    /// Pick the first `Pending` task ID. Caller will `mark_running`.
    async fn take_next_pending(&self) -> Option<String> {
        let mut tasks = self.tasks.lock().await;
        tracing::trace!("take_next_pending scanning {} tasks", tasks.len());
        for task in tasks.iter_mut() {
            if task.status == TaskStatus::Pending {
                tracing::debug!("take_next_pending found: {:?}", task.id);
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
            task.updated_at = chrono::Utc::now().to_rfc3339();
            let _ = self.db.update_task_status(task_id, "running");
            tracing::debug!("task {} → Running", task_id);
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
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            let status = tasks[pos].status.clone();
            if status == TaskStatus::Error
                || status == TaskStatus::Completed
            {
                tracing::debug!("task {} unmark_running: {:?}, removing from list", task_id, status);
                tasks.remove(pos);
            } else {
                tracing::debug!("task {} unmark_running: {:?}, keeping in list", task_id, status);
            }
        }
    }

    pub async fn running_count(&self) -> usize {
        self.running_tasks.lock().await.len()
    }

    /// Return a list of currently running task IDs. Used by rollback to wait
    /// until a stopped task's handler has fully released its slot.
    pub async fn running_tasks_list(&self) -> Vec<String> {
        self.running_tasks.lock().await.iter().cloned().collect()
    }

    pub async fn add_supplement(&self, task_id: &str, text: &str) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.supplement_queue.push(text.into());
            tracing::debug!("task {} supplement added ({} chars)", task_id, text.len());
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
            tracing::debug!("task {} steering added ({} chars)", task_id, text.len());
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

    /// End a task — if the task is still Running, it was forcibly
    /// interrupted, so mark it as Error. If it is Paused (the ReAct
    /// loop naturally finished), mark it as Completed. Cleans up
    /// resources either way. Called from the frontend "结束任务" button.
    pub async fn end_task(&self, task_id: &str) -> anyhow::Result<TaskStatus> {
        // Cancel the running token first to interrupt any active ReAct loop.
        // Ensure a real token exists even when the dispatcher hasn't created
        // one yet (race window between take_next_pending and token insertion);
        // otherwise cancel() would fire on a default token nobody observes.
        let cancel = {
            let mut cancels = self.task_cancellations.lock().await;
            cancels
                .entry(task_id.to_string())
                .or_insert_with(CancellationToken::new)
                .clone()
        };
        cancel.cancel();
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            // Running → Error (forced stop); Paused/other → Completed.
            let new_status = if tasks[pos].status == TaskStatus::Running {
                TaskStatus::Error
            } else {
                TaskStatus::Completed
            };
            tasks[pos].status = new_status.clone();
            tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
            self.db.update_task_status(task_id, new_status.as_str())?;
            tasks.remove(pos);
            drop(tasks);
            self.running_tasks.lock().await.remove(task_id);
            self.task_permits.lock().await.remove(task_id);
            if let Some(notify) = self.task_notify.lock().await.remove(task_id) {
                notify.notify_waiters();
            }
            self.task_cancellations.lock().await.remove(task_id);
            Ok(new_status)
        } else {
            // Task not in memory (e.g. after restart) — check DB status.
            let db_status = self.db.get_task(task_id)
                .ok()
                .flatten()
                .map(|t| TaskStatus::from_status_str(&t.status))
                .unwrap_or(TaskStatus::Error);
            let new_status = if db_status == TaskStatus::Running
                || db_status == TaskStatus::Pending
                || db_status == TaskStatus::Paused
            {
                TaskStatus::Error
            } else {
                TaskStatus::Completed
            };
            self.db.update_task_status(task_id, new_status.as_str())?;
            Ok(new_status)
        }
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
        let is_terminal = status_str == "completed" || status_str == "error";
        let is_pending = status_str == "pending";
        // Determine whether this is a terminal transition and capture the
        // task's index under the `tasks` lock. The lock is released before any
        // subordinate locks (`running_tasks`, `task_permits`, ...) are touched
        // so the acquisition order stays `tasks` -> (others), matching
        // `unmark_running`'s `running_tasks` -> `tasks` without nesting.
        let needs_terminal_cleanup: Option<usize>;
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
                let old_status = tasks[pos].status.as_str();
                tasks[pos].status = status;
                tracing::info!("update_task_status: task={} {} -> {}", task_id, old_status, status_str);
                tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
                self.db.update_task_status(task_id, &status_str)?;
                // Notify any waiter that status has changed. Lazily create the
                // Notify so a transition that happens before the ReAct loop
                // calls `status_notifier` is not lost (otherwise the later
                // `notified().await` on a fresh Notify would block forever).
                let notify = self
                    .task_notify
                    .lock()
                    .await
                    .entry(task_id.to_string())
                    .or_insert_with(|| Arc::new(Notify::new()))
                    .clone();
                notify.notify_waiters();
                // Wake the dispatcher when a task transitions to Pending.
                if is_pending {
                    self.dispatch_notify.notify_one();
                }
                needs_terminal_cleanup = if is_terminal { Some(pos) } else { None };
            } else {
                needs_terminal_cleanup = None;
            }
        }
        // Terminal cleanup performed without holding `tasks` to avoid lock
        // ordering inversion with `unmark_running` (which takes
        // `running_tasks` before `tasks`).
        if needs_terminal_cleanup.is_some() {
            self.running_tasks.lock().await.remove(task_id);
            self.task_permits.lock().await.remove(task_id);
            self.task_cancellations.lock().await.remove(task_id);
            if let Some(notify) = self.task_notify.lock().await.remove(task_id) {
                notify.notify_waiters();
            }
            self.tools.unregister_task(task_id).await;
            let mut tasks = self.tasks.lock().await;
            if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
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
    /// already there (e.g. after an app restart). Used by `process_input`
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
            steps: Vec::new(),
            supplement_queue: Vec::new(),
            steering_queue: Vec::new(),
            created_at: record.created_at,
            updated_at: record.updated_at,
        };
        let mut tasks = self.tasks.lock().await;
        // Re-check: another thread may have inserted this task between the
        // check above and the DB query.
        if !tasks.iter().any(|t| t.id == task_id) {
            tasks.push(task);
        }
        Ok(())
    }

    pub async fn get_task_state(&self, task_id: &str) -> TaskStatus {
        let tasks = self.tasks.lock().await;
        tasks
            .iter()
            .find(|t| t.id == task_id)
            .map(|t| t.status.clone())
            .unwrap_or(TaskStatus::Error)
    }

    pub fn get_tools(&self) -> Arc<ToolsManager> {
        self.tools.clone()
    }

    pub async fn execute_step(
        &self,
        task_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
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
            self.tools.register_skill_for_task(task_id, clean_name).await;
        }

        // Register MCP tool adapters per-task on successful load_mcp
        if result.success && tool_name == "load_mcp"
            && let Some(server_name) = result.output["server_name"].as_str()
        {
            self.tools.register_mcp_for_task(task_id, server_name).await;
        }

        let step_index = step_num as i32;
        // Guard against rollback/cancel: if the task has been removed from the
        // running set while the tool was executing (e.g. rollback_task marked
        // it Error and restored a snapshot), skip persisting step records that
        // would otherwise corrupt the restored state.
        if !self.running_tasks.lock().await.contains(task_id) {
            tracing::warn!(
                "execute_step: task {} left running set during tool execution; skipping step record",
                task_id
            );
            return Ok(result);
        }
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
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
            exec.create_task(&format!("task {}", i), None)
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
            .create_task("hello world", None)
            .await
            .unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.input, "hello world");
        assert!(!task.id.is_empty());
        assert!(!task.created_at.is_empty());
    }

    #[tokio::test]
    async fn create_task_with_summary_preserves_fields() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task_with_summary("raw input", "summary text", None)
            .await
            .unwrap();
        assert_eq!(task.input, "raw input");
        assert_eq!(task.summary, "summary text");
    }

    #[tokio::test]
    async fn end_task_running_marks_error_and_triggers_token() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec
            .create_task("test", None)
            .await
            .unwrap();
        // Set to Running so end_task marks it as Error.
        exec.update_task_status(&task.id, TaskStatus::Running)
            .await
            .unwrap();
        // Insert a token as the dispatcher would, so end_task can trigger it
        let real_token = CancellationToken::new();
        let clone = real_token.clone();
        exec.task_cancellations
            .lock()
            .await
            .insert(task.id.clone(), clone);
        assert!(!real_token.is_cancelled());
        let status = exec.end_task(&task.id).await.unwrap();
        assert_eq!(status, TaskStatus::Error);
        assert!(real_token.is_cancelled());
        let state = exec.get_task_state(&task.id).await;
        assert_eq!(state, TaskStatus::Error);
    }

    #[tokio::test]
    async fn end_task_nonexistent_succeeds() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        // end_task on a nonexistent task updates DB directly.
        let result = exec.end_task("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn end_task_paused_marks_completed() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", None)
            .await
            .unwrap();
        exec.update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();
        let status = exec.end_task(&task.id).await.unwrap();
        assert_eq!(status, TaskStatus::Completed);
    }

    #[tokio::test]
    async fn add_and_get_supplements() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", None)
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
            .create_task("test", None)
            .await
            .unwrap();
        exec.add_steering(&task.id, "steer 1").await.unwrap();
        let drained: Vec<String> = exec.get_steering(&task.id).await;
        assert_eq!(drained, vec!["steer 1"]);
    }

    #[tokio::test]
    async fn list_tasks_all_present() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);

        let _low = exec
            .create_task("low", None)
            .await
            .unwrap();
        let _normal = exec
            .create_task("normal", None)
            .await
            .unwrap();
        let _high = exec
            .create_task("high", None)
            .await
            .unwrap();

        let tasks = exec.list_tasks().await;
        assert_eq!(tasks.len(), 3);
    }

    #[tokio::test]
    async fn get_task_state_returns_correct_status() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", None)
            .await
            .unwrap();
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn get_task_state_nonexistent_returns_error() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        assert_eq!(
            exec.get_task_state("nonexistent").await,
            TaskStatus::Error
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
    async fn update_task_status_changes_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec
            .create_task("test", None)
            .await
            .unwrap();
        exec.update_task_status(&task.id, TaskStatus::Completed)
            .await
            .unwrap();
        // Terminal status removes the task from the in-memory list
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Error);
    }

    #[tokio::test]
    async fn update_task_status_completed_cleans_up() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec
            .create_task("test", None)
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
            .create_task("test", None)
            .await
            .unwrap();
        let result = exec.execute_step(&task.id, "nonexistent_tool", serde_json::json!({}), 1).await;
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
