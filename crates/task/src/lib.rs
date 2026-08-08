use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::messages::MessageAttachment;
use haven_memory::repositories::tasks::Task as DbTask;
use haven_tools::{ToolResult, ToolsManager, is_silent_action};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// A user message queued for injection into the ReAct loop (supplement or
/// steering). `text` is the plain-text content; `attachments` hold binary
/// payloads (e.g. images) for multimodal requests.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Default)]
pub struct Supplement {
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    /// True when this message is the user's reply to a pending `ask`
    /// question. The ReAct loop injects it as a paired answer ("Answer to
    /// your previous question") instead of generic additional context, so
    /// the model does not treat the old question as still open and answer
    /// stale questions again.
    #[serde(default)]
    pub is_answer: bool,
}

impl Supplement {
    pub fn new(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
            is_answer: false,
        }
    }

    pub fn answer(text: impl Into<String>, attachments: Vec<MessageAttachment>) -> Self {
        Self {
            text: text.into(),
            attachments,
            is_answer: true,
        }
    }
}

impl From<String> for Supplement {
    fn from(text: String) -> Self {
        Supplement::new(text, vec![])
    }
}

impl From<&str> for Supplement {
    fn from(text: &str) -> Self {
        Supplement::new(text, vec![])
    }
}

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
    pub supplement_queue: Vec<Supplement>,
    /// Steering queue: items that should interrupt the current tool sequence
    /// and be injected as context immediately (refine 搂1.2).
    pub steering_queue: Vec<Supplement>,
    pub created_at: String,
    pub updated_at: String,
}

impl TaskInfo {
    /// Build an in-memory `TaskInfo` from a freshly-loaded DB record. Centralizes
    /// the 10-field literal that used to be duplicated at every `load_*` site;
    /// `status` is taken from the record so callers that need a forced override
    /// (e.g. `load_pending_tasks`) can mutate it after construction.
    pub fn from_db_record(record: &DbTask) -> Self {
        Self {
            id: record.id.clone(),
            input: record.input_text.clone(),
            summary: record.transcript.clone(),
            title: record.title.clone(),
            status: TaskStatus::from_status_str(&record.status),
            steps: Vec::new(),
            supplement_queue: Vec::new(),
            steering_queue: Vec::new(),
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
        }
    }
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
    /// Tasks currently paused because the `ask` tool is awaiting a human
    /// reply. While set, background-job completions must NOT auto-wake the
    /// task: the model is blocked on a user answer, not on job results, and
    /// resuming it would let the agent continue (and run tools) without the
    /// user's consent. Cleared centrally in `update_task_status` whenever the
    /// task is (re)activated to Pending/Running.
    awaiting_answer: Arc<Mutex<HashSet<String>>>,
    /// Per-task buffer of completed background-job results, delivered to the
    /// ReAct loop as context at the next step start. Kept separate from the
    /// steering queue so job output is never mistaken for a user reply (the
    /// `ask` pause path keys resume off the steering queue, which now holds
    /// only genuine user interjections).
    job_completions: Arc<Mutex<HashMap<String, Vec<String>>>>,
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
            awaiting_answer: Arc::new(Mutex::new(HashSet::new())),
            job_completions: Arc::new(Mutex::new(HashMap::new())),
            on_confirm_request: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn create_task(&self, input: &str) -> anyhow::Result<TaskInfo> {
        self.create_task_with_summary(input, input).await
    }

    pub async fn create_task_with_summary(
        &self,
        input: &str,
        summary: &str,
    ) -> anyhow::Result<TaskInfo> {
        let record = self.db.create_task(input, input)?;
        let mut task = TaskInfo::from_db_record(&record);
        // The DB record was created with `input` as its transcript, but the
        // caller may have a distinct classifier-generated summary — overlay
        // it after construction so we keep the constructor single-purpose.
        task.summary = summary.into();
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
            // Pick up tasks that were still Pending when the app stopped so
            // queued work survives a restart instead of being stranded in
            // the DB (the in-memory working set is empty on a fresh start).
            let reloaded = exec.load_pending_tasks().await;
            if reloaded > 0 {
                tracing::info!(
                    "dispatcher reloaded {} pending task(s) from previous run",
                    reloaded
                );
            }
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

                let task_id = exec.try_claim_pending().await;
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

                // Register the permit so pause/cancel can release it.
                {
                    let mut permits = exec.task_permits.lock().await;
                    permits.insert(task_id.clone(), permit);
                }
                // Create a cancellation token for this task. Use entry() so a
                // token already created (and possibly cancelled) by end_task
                // during the claim window is never clobbered with a fresh one.
                {
                    let mut cancels = exec.task_cancellations.lock().await;
                    cancels
                        .entry(task_id.clone())
                        .or_insert_with(CancellationToken::new);
                }

                let exec_inner = exec.clone();
                let handler_inner = handler.clone();
                tracing::info!("dispatcher spawning handler for task: {:?}", task_id);
                // Run the handler on a nested task so a panic in the ReAct
                // loop is contained: the JoinHandle turns it into an Err and
                // the cleanup below still runs. Without this, a panicked
                // handler would skip the Error marking and unmark_running,
                // leaving the task stuck in Running (memory + DB) forever.
                tokio::spawn(async move {
                    let result = tokio::spawn(handler_inner(task_id.clone())).await;
                    let failed = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(format!("handler failed: {}", e)),
                        Err(join_err) if join_err.is_panic() => {
                            Some(format!("handler panicked: {}", join_err))
                        }
                        Err(join_err) => Some(format!("handler aborted: {}", join_err)),
                    };
                    if let Some(reason) = failed {
                        tracing::error!("dispatcher task {} {}", task_id, reason);
                        let _ = exec_inner
                            .update_task_status(&task_id, TaskStatus::Error)
                            .await;
                        // The ReAct loop errored out: kill any background jobs
                        // the task spawned so their children cannot leak.
                        exec_inner.cancel_task_jobs(&task_id).await;
                    }
                    exec_inner.unmark_running(&task_id).await;
                });
            }
        });
    }

    /// Atomically claim the first `Pending` task that is not already being
    /// handled, flip it to `Running` (memory + DB) and insert it into the
    /// running set. Returns the claimed task id, or `None` if nothing is
    /// dispatchable.
    ///
    /// Claiming under the `tasks` lock closes the TOCTOU window of the former
    /// "find Pending" + "mark Running" pair, where `end_task`/rollback could
    /// terminate a task in between and the dispatcher would resurrect it
    /// (ghost execution) or leave DB and memory diverged.
    ///
    /// The `running_tasks` check prevents double-dispatch: a task whose
    /// handler is still alive (e.g. blocked in a pause-wait after a
    /// supplement flipped it Paused 鈫?Pending) must not be claimed again 鈥?
    /// its own loop picks up the supplement via the status notifier.
    async fn try_claim_pending(&self) -> Option<String> {
        let mut tasks = self.tasks.lock().await;
        let mut running = self.running_tasks.lock().await;
        for task in tasks.iter_mut() {
            if task.status == TaskStatus::Pending && !running.contains(&task.id) {
                let task_id = task.id.clone();
                task.status = TaskStatus::Running;
                task.updated_at = chrono::Utc::now().to_rfc3339();
                let db = self.db.clone();
                let tid = task_id.clone();
                let _ = db
                    .run_blocking(move |db| db.update_task_status(&tid, "running"))
                    .await;
                running.insert(task_id.clone());
                tracing::debug!("try_claim_pending: claimed task {}", task_id);
                return Some(task_id);
            }
        }
        None
    }

    /// Remove a task from the running set. Terminal status updates are
    /// performed by the handler / agent loop. Also removes terminal-status
    /// tasks from the in-memory list so `try_claim_pending` only counts
    /// active (Pending / Running) tasks.
    async fn unmark_running(&self, task_id: &str) {
        self.cleanup_task_maps(task_id).await;
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            let status = tasks[pos].status.clone();
            if status == TaskStatus::Error || status == TaskStatus::Completed {
                tracing::debug!(
                    "task {} unmark_running: {:?}, removing from list",
                    task_id,
                    status
                );
                tasks.remove(pos);
            } else {
                tracing::debug!(
                    "task {} unmark_running: {:?}, keeping in list",
                    task_id,
                    status
                );
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
        self.add_supplement_with_attachments(task_id, text, &[])
            .await
    }

    pub async fn add_supplement_with_attachments(
        &self,
        task_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
    ) -> anyhow::Result<()> {
        self.push_supplement(task_id, text, attachments, false)
            .await
    }

    /// Queue a supplement that is the user's reply to a pending `ask`
    /// question. Injected as a paired answer on resume so the model no
    /// longer sees the old question as open.
    pub async fn add_answer_with_attachments(
        &self,
        task_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
    ) -> anyhow::Result<()> {
        self.push_supplement(task_id, text, attachments, true).await
    }

    async fn push_supplement(
        &self,
        task_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        is_answer: bool,
    ) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            let supplement = if is_answer {
                Supplement::answer(text, attachments.to_vec())
            } else {
                Supplement::new(text, attachments.to_vec())
            };
            task.supplement_queue.push(supplement);
            tracing::debug!(
                "task {} {} added ({} chars, {} attachments)",
                task_id,
                if is_answer { "answer" } else { "supplement" },
                text.len(),
                attachments.len()
            );
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    pub async fn get_supplements(&self, task_id: &str) -> Vec<Supplement> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.supplement_queue.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    /// Add a steering item: interrupts the current tool sequence and is
    /// injected as context immediately (refine 搂1.2).
    pub async fn add_steering(&self, task_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_steering_with_attachments(task_id, text, &[]).await
    }

    pub async fn add_steering_with_attachments(
        &self,
        task_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
    ) -> anyhow::Result<()> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.steering_queue
                .push(Supplement::new(text, attachments.to_vec()));
            tracing::debug!(
                "task {} steering added ({} chars, {} attachments)",
                task_id,
                text.len(),
                attachments.len()
            );
            Ok(())
        } else {
            anyhow::bail!("task '{}' not found", task_id)
        }
    }

    /// Drain the steering queue for a task.
    pub async fn get_steering(&self, task_id: &str) -> Vec<Supplement> {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
            task.steering_queue.drain(..).collect()
        } else {
            Vec::new()
        }
    }

    /// Mark/unmark a task as paused awaiting a human answer to an `ask`
    /// question. The background-job completion consumer consults this to avoid
    /// auto-resuming a task that is blocked on the user.
    pub async fn set_awaiting_answer(&self, task_id: &str, awaiting: bool) {
        let mut set = self.awaiting_answer.lock().await;
        if awaiting {
            set.insert(task_id.to_string());
        } else {
            set.remove(task_id);
        }
    }

    pub async fn is_awaiting_answer(&self, task_id: &str) -> bool {
        self.awaiting_answer.lock().await.contains(task_id)
    }

    /// Buffer a completed background-job result for a task. It is delivered
    /// to the ReAct loop as context at the next step start (drained by
    /// `drain_job_completions`), separate from the user-driven steering queue.
    pub async fn add_job_completion(&self, task_id: &str, text: &str) {
        let mut jobs = self.job_completions.lock().await;
        jobs.entry(task_id.to_string())
            .or_default()
            .push(text.to_string());
    }

    /// Drain buffered background-job completions for a task.
    pub async fn drain_job_completions(&self, task_id: &str) -> Vec<String> {
        self.job_completions
            .lock()
            .await
            .remove(task_id)
            .unwrap_or_default()
    }

    /// Promote a checkpointed partial stream reply into a real assistant
    /// message when the user stops the task mid-generation, so history keeps
    /// the text that was already streamed to the screen. Skips when a real
    /// message was persisted after the last checkpoint (the loop finished
    /// writing before the cancel landed) — promoting then would duplicate it.
    async fn promote_partial_message(&self, task_id: &str) {
        let db = self.db.clone();
        let tid = task_id.to_string();
        let partial = db
            .run_blocking(move |db| Ok(db.get_partial_message(&tid)))
            .await
            .ok()
            .flatten();
        let Some((content, updated_at)) = partial else {
            return;
        };
        if content.trim().is_empty() {
            return;
        }
        let db = self.db.clone();
        let tid = task_id.to_string();
        let last_ts = db
            .run_blocking(move |db| Ok(db.get_last_message_created_at(&tid)))
            .await
            .ok()
            .flatten();
        if let Some(last) = last_ts
            && last >= updated_at
        {
            return;
        }
        let db = self.db.clone();
        let tid = task_id.to_string();
        let res = db
            .run_blocking(move |db| {
                let taken = db.take_partial_message(&tid);
                if let Some(text) = taken {
                    db.add_message_full(&tid, "assistant", &text, Some("text"), None, &[], false)?;
                }
                Ok::<(), anyhow::Error>(())
            })
            .await;
        if let Err(e) = res {
            tracing::warn!(
                "promote_partial_message: failed to promote partial reply for task {}: {}",
                task_id,
                e
            );
        }
    }

    /// End a task. Since the user explicitly asked to end it, the task is
    /// always marked as Completed 鈥?regardless of whether it was still
    /// Running (forced stop) or Paused (naturally finished). Clean up
    /// resources either way. Called from the frontend "缁撴潫浠诲姟" button.
    pub async fn end_task(&self, task_id: &str) -> anyhow::Result<TaskStatus> {
        // Cancel the running token first to interrupt any active ReAct loop.
        // Ensure a real token exists even when the dispatcher hasn't created
        // one yet (race window between try_claim_pending and token insertion);
        // otherwise cancel() would fire on a default token nobody observes.
        let cancel = {
            let mut cancels = self.task_cancellations.lock().await;
            cancels
                .entry(task_id.to_string())
                .or_insert_with(CancellationToken::new)
                .clone()
        };
        cancel.cancel();
        // Kill any background jobs the task spawned; they would otherwise
        // keep running (and leak child processes) after the task is gone.
        self.cancel_task_jobs(task_id).await;
        let mut tasks = self.tasks.lock().await;
        if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
            let new_status = TaskStatus::Completed;
            tasks[pos].status = new_status.clone();
            tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
            let db = self.db.clone();
            let tid = task_id.to_string();
            db.run_blocking(move |db| db.update_task_status(&tid, "completed"))
                .await?;
            tasks.remove(pos);
            drop(tasks);
            self.promote_partial_message(task_id).await;
            // `task_notify` sits between the maps here — wake any ReAct-loop
            // waiters before tearing down the rest of the per-task state.
            self.running_tasks.lock().await.remove(task_id);
            self.task_permits.lock().await.remove(task_id);
            if let Some(notify) = self.task_notify.lock().await.remove(task_id) {
                notify.notify_waiters();
            }
            self.task_cancellations.lock().await.remove(task_id);
            Ok(new_status)
        } else {
            // Task not in memory (e.g. after restart) 鈥?end it regardless of
            // its DB state; the user asked to finish it.
            let db = self.db.clone();
            let tid = task_id.to_string();
            db.run_blocking(move |db| db.update_task_status(&tid, "completed"))
                .await?;
            self.promote_partial_message(task_id).await;
            Ok(TaskStatus::Completed)
        }
    }

    /// Remove a task entirely from the in-memory state.
    /// This does NOT delete from DB 鈥?the caller handles that.
    /// Succeeds even if the task is not in memory (e.g. after restart).
    pub async fn remove_task(&self, task_id: &str) {
        self.cancel_task_jobs(task_id).await;
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|t| t.id != task_id);
        drop(tasks);
        self.cleanup_task_maps(task_id).await;
        self.awaiting_answer.lock().await.remove(task_id);
        self.job_completions.lock().await.remove(task_id);
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
    /// Used when the user clears history 鈥?the DB is already wiped.
    pub async fn clear_all_tasks(&self) {
        let mut tasks = self.tasks.lock().await;
        tasks.clear();
        drop(tasks);
        self.running_tasks.lock().await.clear();
        self.task_permits.lock().await.clear();
        self.task_cancellations.lock().await.clear();
        self.awaiting_answer.lock().await.clear();
        self.job_completions.lock().await.clear();
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
                tracing::info!(
                    "update_task_status: task={} {} -> {}",
                    task_id,
                    old_status,
                    status_str
                );
                tasks[pos].updated_at = chrono::Utc::now().to_rfc3339();
                let db = self.db.clone();
                let tid = task_id.to_string();
                let st = status_str.clone();
                db.run_blocking(move |db| db.update_task_status(&tid, &st))
                    .await?;
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
                // Reactivation clears the awaiting-answer gate: the task is
                // being (re)dispatched (user answered, supplement flipped it,
                // continue flow, ...), so background-job completions may once
                // again auto-wake it if it pauses for scheduling reasons.
                if is_pending || status_str == "running" {
                    self.awaiting_answer.lock().await.remove(task_id);
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
            self.cleanup_task_maps(task_id).await;
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

    /// Remove `task_id` from the three per-task maps (`running_tasks`,
    /// `task_permits`, `task_cancellations`). Centralizes the three-line
    /// triplet that used to be copy-pasted at every cleanup site.
    /// Does NOT touch `tasks` (working set) or `task_notify` — those have
    /// ordering-sensitive callers (`update_task_status`, `unmark_running`)
    /// that need to remain in the lock-order path.
    pub async fn cleanup_task_maps(&self, task_id: &str) {
        self.running_tasks.lock().await.remove(task_id);
        self.task_permits.lock().await.remove(task_id);
        self.task_cancellations.lock().await.remove(task_id);
    }

    /// Look up an in-memory `TaskInfo` by id. Equivalent to the
    /// `list_tasks().await.into_iter().find(|t| t.id == task_id)` pattern
    /// that was repeated at three call sites — using a method keeps the
    /// scan colocated with the rest of the working-set code so any future
    /// secondary index (e.g. HashMap-backed lookup) only needs one edit.
    pub async fn get_task(&self, task_id: &str) -> Option<TaskInfo> {
        self.tasks
            .lock()
            .await
            .iter()
            .find(|t| t.id == task_id)
            .cloned()
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
        let task = TaskInfo::from_db_record(&record);
        let mut tasks = self.tasks.lock().await;
        // Re-check: another thread may have inserted this task between the
        // check above and the DB query.
        if !tasks.iter().any(|t| t.id == task_id) {
            tasks.push(task);
        }
        Ok(())
    }

    /// Reload tasks that are still `Pending` in the database into the
    /// in-memory working set and wake the dispatcher. Called at dispatcher
    /// startup so queued work from a previous run is picked up after an app
    /// restart. Returns the number of tasks reloaded.
    pub async fn load_pending_tasks(&self) -> usize {
        let pending = self
            .db
            .search_tasks_filtered(None, Some("pending"), None, None, -1, 0)
            .unwrap_or_default();
        let mut loaded = 0;
        {
            let mut tasks = self.tasks.lock().await;
            for record in pending {
                if tasks.iter().any(|t| t.id == record.id) {
                    continue;
                }
                // Force Pending: this loader only ever rehydrates tasks
                // whose DB status is already "pending" (the SQL filter
                // guarantees that), so the override is a no-op but keeps
                // the invariant explicit at the call site.
                let mut info = TaskInfo::from_db_record(&record);
                info.status = TaskStatus::Pending;
                tasks.push(info);
                loaded += 1;
            }
        }
        if loaded > 0 {
            self.dispatch_notify.notify_one();
        }
        loaded
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

    /// Cancel and drop all background jobs owned by a task. Called when the
    /// task ends, is removed, or is rolled back so child processes cannot
    /// leak past their task.
    pub async fn cancel_task_jobs(&self, task_id: &str) {
        self.tools.background_jobs.cancel_for_task(task_id).await;
    }

    pub async fn execute_step(
        &self,
        task_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
    ) -> anyhow::Result<ToolResult> {
        tracing::info!(
            "execute_step: task={} tool={} input={:?}",
            task_id,
            tool_name,
            input
        );
        {
            let mut tasks = self.tasks.lock().await;
            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                let prev = task.status.as_str();
                task.status = TaskStatus::Running;
                if prev != "running" {
                    tracing::warn!(
                        "execute_step: task {} was {} before tool call, forcing Running",
                        task_id,
                        prev
                    );
                }
            }
        }

        let cancel = self.cancellation_token(task_id).await;
        let result = self
            .tools
            .execute_tool(Some(task_id), tool_name, input.clone(), cancel)
            .await?;
        tracing::info!(
            "execute_step result: tool={} success={}",
            tool_name,
            result.success
        );

        let risk_level = self
            .tools
            .get_risk_level(Some(task_id), tool_name, &input)
            .await;

        // Register skill adapter per-task on successful load_skill
        // instead of polluting the global registry (refine 搂6).
        if result.success
            && tool_name == "load_skill"
            && let Some(skill_name) = result.output["skill"]["name"].as_str()
        {
            let clean_name = skill_name.strip_prefix("skill__").unwrap_or(skill_name);
            self.tools
                .register_skill_for_task(task_id, clean_name)
                .await;
        }

        // Register MCP tool adapters per-task on successful load_mcp
        if result.success
            && tool_name == "load_mcp"
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
        // Tie a background job to its task so end/rollback can clean it up.
        // After the running-set guard: a job spawned in a step that was
        // concurrently rolled back must not attach past the cleanup sweep.
        // Gated on the shell tool 鈥?any other tool whose output happens to
        // carry "background"+job_id must not re-bind a foreign job.
        if result.success
            && tool_name == "shell"
            && result.output.get("background").and_then(|v| v.as_bool()) == Some(true)
            && let Some(job_id) = result.output["job_id"].as_str()
        {
            self.tools
                .background_jobs
                .attach_task(job_id, task_id)
                .await;
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

        let step_record = self
            .db
            .run_blocking({
                let task_id = task_id.to_string();
                let tool_name = tool_name.to_string();
                let tool_input = input.to_string();
                let silent = is_silent_action(&tool_name, &input);
                move |db| {
                    db.create_action_step(
                        &task_id,
                        step_index,
                        &tool_name,
                        &tool_input,
                        risk_level != RiskLevel::Safe,
                        silent,
                    )
                }
            })
            .await?;
        let obs = result.summary_text();
        let step_id = step_record.id.clone();
        let success = result.success;
        self.db
            .run_blocking(move |db| db.complete_action_step(&step_id, &obs, success))
            .await?;
        Ok(result)
    }

    pub fn confirm_step(&self, step_id: &str, confirmed: bool) -> anyhow::Result<()> {
        self.db.confirm_step(step_id, confirmed)?;
        Ok(())
    }

    /// Atomically resolve a confirmation step and, when the caller wants to
    /// trust the risk level for the session, return the step's risk level.
    /// The risk level is captured under the `tasks` lock so a concurrent
    /// `end_task`/rollback cannot leave the caller trusting a step that was
    /// already removed (the old `resolve_confirmation` command read the step
    /// from a separate `list_tasks()` snapshot, racing with removal).
    pub async fn resolve_confirmation(
        &self,
        step_id: &str,
        confirmed: bool,
    ) -> anyhow::Result<Option<RiskLevel>> {
        let db = self.db.clone();
        let step_id_owned = step_id.to_string();
        db.run_blocking(move |db| db.confirm_step(&step_id_owned, confirmed))
            .await?;
        let risk = self
            .tasks
            .lock()
            .await
            .iter()
            .find_map(|t| t.steps.iter().find(|s| s.id == step_id))
            .map(|s| s.risk_level);
        Ok(risk)
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

    /// A handler that panics must still release the running slot and mark the
    /// task Error 鈥?otherwise the task is stuck in Running forever.
    #[tokio::test]
    async fn dispatcher_panicked_handler_marks_error() {
        let exec = make_executor(1);
        let task = exec.create_task("t1").await.unwrap();

        let handler: RunHandler = Arc::new(move |_id: String| {
            Box::pin(async move {
                panic!("simulated handler panic");
                #[allow(unreachable_code)]
                Ok(())
            })
        });
        exec.clone().start_dispatcher(handler);

        // Wait for the dispatcher to claim the task, run the panicking
        // handler, and mark it Error in the DB (pending 鈫?running 鈫?error).
        let mut db_status = String::new();
        for _ in 0..100 {
            db_status = exec
                .db
                .get_task(&task.id)
                .unwrap()
                .map(|t| t.status)
                .unwrap_or_default();
            if db_status == "error" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(db_status, "error");
        // Terminal status removed the task from the working set and released
        // the running slot (get_task_state falls back to the Error sentinel
        // for absent tasks).
        assert!(!exec.running_tasks.lock().await.contains(&task.id));
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Error);
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
            exec.create_task(&format!("task {}", i)).await.unwrap();
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

    /// Claim is atomic: it flips the task to Running in memory + DB and
    /// inserts it into the running set, so a second claim returns nothing.
    #[tokio::test]
    async fn try_claim_pending_claims_once_and_persists() {
        let exec = make_executor(2);
        let task = exec.create_task("t1").await.unwrap();

        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(task.id.as_str()));

        let state = exec.get_task_state(&task.id).await;
        assert_eq!(state, TaskStatus::Running);
        assert!(exec.running_tasks.lock().await.contains(&task.id));
        let db_status = exec
            .db
            .get_task(&task.id)
            .unwrap()
            .map(|t| t.status)
            .unwrap_or_default();
        assert_eq!(db_status, "running");

        // No second claim while the first handler holds the slot.
        assert!(exec.try_claim_pending().await.is_none());
    }

    /// A Pending task whose handler is still alive (present in the running
    /// set, e.g. blocked in a pause-wait after Paused 鈫?Pending) must not be
    /// claimed again 鈥?otherwise the dispatcher spawns a duplicate ReAct loop.
    #[tokio::test]
    async fn try_claim_pending_skips_task_already_in_running_set() {
        let exec = make_executor(2);
        let task = exec.create_task("t1").await.unwrap();
        exec.running_tasks.lock().await.insert(task.id.clone());

        assert!(exec.try_claim_pending().await.is_none());

        // Once the handler releases the slot, the task becomes claimable.
        exec.running_tasks.lock().await.remove(&task.id);
        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(task.id.as_str()));
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Running);
    }

    /// A task terminated by end_task between the old find/mark window must
    /// not be resurrected by a late claim (no ghost execution).
    #[tokio::test]
    async fn try_claim_pending_respects_end_task() {
        let exec = make_executor(2);
        let task = exec.create_task("t1").await.unwrap();

        let status = exec.end_task(&task.id).await.unwrap();
        assert_eq!(status, TaskStatus::Completed);

        assert!(exec.try_claim_pending().await.is_none());
        assert!(!exec.running_tasks.lock().await.contains(&task.id));
    }

    // 鈹€鈹€鈹€ Data-layer tests (no dispatcher required) 鈹€鈹€鈹€

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
        let task = exec.create_task("hello world").await.unwrap();
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
            .create_task_with_summary("raw input", "summary text")
            .await
            .unwrap();
        assert_eq!(task.input, "raw input");
        assert_eq!(task.summary, "summary text");
    }

    #[tokio::test]
    async fn end_task_running_marks_completed_and_triggers_token() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec.create_task("test").await.unwrap();
        // Set to Running so end_task also cancels the loop token.
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
        assert_eq!(status, TaskStatus::Completed);
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
        let task = exec.create_task("test").await.unwrap();
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
        let task = exec.create_task("test").await.unwrap();
        exec.add_supplement(&task.id, "extra context 1")
            .await
            .unwrap();
        exec.add_supplement(&task.id, "extra context 2")
            .await
            .unwrap();
        let drained: Vec<String> = exec
            .get_supplements(&task.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["extra context 1", "extra context 2"]);
        assert!(exec.get_supplements(&task.id).await.is_empty());
    }

    #[tokio::test]
    async fn answer_supplement_carries_is_answer_flag() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("test").await.unwrap();
        exec.add_answer_with_attachments(&task.id, "the answer", &[])
            .await
            .unwrap();
        exec.add_supplement(&task.id, "plain context")
            .await
            .unwrap();
        let drained = exec.get_supplements(&task.id).await;
        assert_eq!(drained.len(), 2);
        assert!(drained[0].is_answer, "first message is an ask reply");
        assert_eq!(drained[0].text, "the answer");
        assert!(!drained[1].is_answer, "plain supplement is not an answer");
    }

    #[tokio::test]
    async fn add_and_get_supplements_with_attachments() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("test").await.unwrap();
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
        exec.add_supplement_with_attachments(&task.id, "鐪嬪浘", std::slice::from_ref(&att))
            .await
            .unwrap();
        let drained = exec.get_supplements(&task.id).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text, "鐪嬪浘");
        assert_eq!(drained[0].attachments, vec![att]);
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
        let task = exec.create_task("test").await.unwrap();
        exec.add_steering(&task.id, "steer 1").await.unwrap();
        let drained: Vec<String> = exec
            .get_steering(&task.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["steer 1"]);
    }

    #[tokio::test]
    async fn list_tasks_all_present() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);

        let _low = exec.create_task("low").await.unwrap();
        let _normal = exec.create_task("normal").await.unwrap();
        let _high = exec.create_task("high").await.unwrap();

        let tasks = exec.list_tasks().await;
        assert_eq!(tasks.len(), 3);
    }

    #[tokio::test]
    async fn get_task_state_returns_correct_status() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("test").await.unwrap();
        assert_eq!(exec.get_task_state(&task.id).await, TaskStatus::Pending);
    }

    #[tokio::test]
    async fn get_task_state_nonexistent_returns_error() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        assert_eq!(exec.get_task_state("nonexistent").await, TaskStatus::Error);
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
    async fn load_pending_tasks_reloads_after_restart() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db.clone(), tools.clone(), 3);
        let task = exec.create_task("queued before restart").await.unwrap();

        // Simulate a restart: fresh executor over the same DB with an empty
        // working set. The pending task must be reloaded and dispatchable.
        let exec2 = TaskExecutor::new(db.clone(), tools, 3);
        assert!(exec2.list_tasks().await.is_empty());
        let loaded = exec2.load_pending_tasks().await;
        assert_eq!(loaded, 1);

        let claimed = exec2.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(task.id.as_str()));
        assert_eq!(exec2.get_task_state(&task.id).await, TaskStatus::Running);
    }

    #[tokio::test]
    async fn load_pending_tasks_skips_non_pending() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db.clone(), tools.clone(), 3);
        let done = exec.create_task("done").await.unwrap();
        exec.end_task(&done.id).await.unwrap();
        let paused = exec.create_task("paused").await.unwrap();
        exec.update_task_status(&paused.id, TaskStatus::Paused)
            .await
            .unwrap();

        // Restart: only the still-pending task is reloaded.
        let exec2 = TaskExecutor::new(db, tools, 3);
        let loaded = exec2.load_pending_tasks().await;
        assert_eq!(loaded, 0);
        assert!(exec2.list_tasks().await.is_empty());
    }

    #[tokio::test]
    async fn update_task_status_changes_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("test").await.unwrap();
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
        let task = exec.create_task("test").await.unwrap();
        exec.running_tasks.lock().await.insert(task.id.clone());
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = sem.clone().acquire_owned().await.unwrap();
        exec.task_permits
            .lock()
            .await
            .insert(task.id.clone(), permit);
        exec.task_cancellations
            .lock()
            .await
            .insert(task.id.clone(), CancellationToken::new());

        exec.update_task_status(&task.id, TaskStatus::Completed)
            .await
            .unwrap();
        assert!(!exec.running_tasks.lock().await.contains(&task.id));
        assert!(exec.task_permits.lock().await.get(&task.id).is_none());
    }

    #[tokio::test]
    async fn execute_step_unknown_tool_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec.create_task("test").await.unwrap();
        let result = exec
            .execute_step(&task.id, "nonexistent_tool", serde_json::json!({}), 1)
            .await;
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

    #[tokio::test]
    async fn awaiting_answer_flag_set_and_cleared_on_reactivation() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("ask me").await.unwrap();

        assert!(!exec.is_awaiting_answer(&task.id).await);

        // Simulate the ask pause path: set the gate, then pause.
        exec.set_awaiting_answer(&task.id, true).await;
        exec.update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();
        assert!(exec.is_awaiting_answer(&task.id).await);

        // Reactivation (user answered 鈫?Pending) must clear the gate centrally.
        exec.update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        assert!(!exec.is_awaiting_answer(&task.id).await);
    }

    #[tokio::test]
    async fn job_completions_buffered_and_drained() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("bg job").await.unwrap();

        assert!(exec.drain_job_completions(&task.id).await.is_empty());

        exec.add_job_completion(&task.id, "job-1 done").await;
        exec.add_job_completion(&task.id, "job-2 failed").await;

        let drained = exec.drain_job_completions(&task.id).await;
        assert_eq!(drained, vec!["job-1 done", "job-2 failed"]);
        assert!(exec.drain_job_completions(&task.id).await.is_empty());
    }

    #[tokio::test]
    async fn remove_task_clears_awaiting_and_job_buffers() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("cleanup").await.unwrap();
        exec.set_awaiting_answer(&task.id, true).await;
        exec.add_job_completion(&task.id, "stranded").await;

        exec.remove_task(&task.id).await;
        assert!(!exec.is_awaiting_answer(&task.id).await);
        assert!(exec.drain_job_completions(&task.id).await.is_empty());
    }
}
