use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::messages::MessageAttachment;
use haven_memory::repositories::tasks::Task as DbTask;
use haven_tools::{ConfirmationResult, ToolResult, ToolsManager, is_silent_action};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

pub mod partial;

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

const DISPATCH_LOG_INTERVAL: u64 = 200; // log every ~20s instead of every 100ms

/// Bounded wait for a user confirmation. A pending confirmation whose
/// frontend answer never arrives (window closed, dialog lost, reminder fired
/// with no UI attached) must fail CLOSED instead of blocking the task — or,
/// for the reminder path, the whole sequential reminder consumer — for an
/// unbounded time. The bound is short enough that a headless queue recovers
/// quickly, yet still gives an interactive user a comfortable window to
/// approve/deny a dialog.
const CONFIRM_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Paused,
    /// Paused because the `ask` tool is awaiting a human answer. Background-job
    /// completions must NOT auto-wake this state: the model is blocked on the
    /// user, not on job results, and resuming would let the agent continue
    /// (and run tools) without the user's consent. Serialized as "paused" so
    /// the wire/DB format is unchanged.
    PausedAwaitingAnswer,
    Completed,
    Error,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Running => "running",
            TaskStatus::Paused | TaskStatus::PausedAwaitingAnswer => "paused",
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
            // Unknown/corrupt DB statuses must not silently map to Pending:
            // that would auto-resurrect the task on the next dispatcher
            // reload. Error is the safe interpretation (visible, inert).
            other => {
                tracing::warn!("unknown task status string {:?}; mapping to Error", other);
                TaskStatus::Error
            }
        }
    }

    /// True for both pause flavors: scheduling pause and ask-awaiting pause.
    pub fn is_paused(&self) -> bool {
        matches!(self, TaskStatus::Paused | TaskStatus::PausedAwaitingAnswer)
    }

    /// True when the pause is blocked on a human answer to an `ask` tool.
    pub fn is_awaiting_answer(&self) -> bool {
        matches!(self, TaskStatus::PausedAwaitingAnswer)
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, TaskStatus::Completed | TaskStatus::Error)
    }
}

impl serde::Serialize for TaskStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for TaskStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_status_str(&s))
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
    pub step_number: i32,
    pub tool_name: String,
    pub input: Value,
    pub output: Option<Value>,
    pub status: String,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

type ConfirmRequestCallback = Arc<
    Mutex<Option<Box<dyn Fn(haven_common::types::ConfirmId, String, String, RiskLevel) + Send>>>,
>;

/// Terminal-failure callback: invoked when the dispatcher marks a task as
/// Error on a path that bypasses the ReAct loop's normal error emission
/// (handler panic / abort). The app layer wires it to emit `task:error` and
/// the `task:updated` secondary broadcast so the UI never misses a terminal
/// transition (busy indicators, status chip, task list refresh).
type TaskErrorCallback = Arc<Mutex<Option<Box<dyn Fn(String, String) + Send>>>>;

/// A pending safety-gateway confirmation wait, keyed by a generated step id
/// in `TaskExecutor::confirm_waits`. The executing task blocks on the oneshot
/// receiver until the frontend resolves the confirmation via
/// `resolve_confirmation` (or the task is cancelled).
struct ConfirmWait {
    risk_level: RiskLevel,
    tx: tokio::sync::oneshot::Sender<bool>,
}

/// Result of a safety-gated tool execution: the tool result plus the
/// risk level and confirmation state recorded for the step.
pub struct ToolExecution {
    pub result: ToolResult,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

pub struct TaskExecutor {
    db: Arc<Database>,
    tools: Arc<ToolsManager>,
    /// Per-task working set. Keyed by task id; each entry is behind its own
    /// mutex so a slow transition of one task (DB write under the entry lock)
    /// never serializes the other tasks' operations on a global lock. The map
    /// lock itself is only held for lookup/insert/remove (never while
    /// awaiting an entry lock), keeping the lock order acyclic.
    tasks: Arc<Mutex<HashMap<String, Arc<Mutex<TaskInfo>>>>>,
    running_tasks: Arc<Mutex<HashSet<String>>>,
    semaphore: Arc<Semaphore>,
    /// Current configured task concurrency ceiling. Kept separate from the
    /// semaphore's live permit count so `set_max_concurrent` can compute the
    /// delta when the user changes the setting at runtime.
    max_concurrent: std::sync::atomic::AtomicUsize,
    /// Tracks the semaphore permit held by each running task's handler.
    /// When a task is paused, its permit is dropped so the dispatcher slot
    /// is freed. On resume the dispatcher re-acquires a permit.
    task_permits: Arc<Mutex<HashMap<String, OwnedSemaphorePermit>>>,
    /// FIFO dispatch queue: task ids in the order they became `Pending`
    /// (insertion order ≈ creation order for fresh tasks). The dispatcher
    /// claims from the front, so queued tasks run in submission order instead
    /// of the nondeterministic `HashMap` iteration order a full scan would
    /// produce. Entries are (re-)enqueued on every transition to Pending and
    /// removed on terminal states / claims / explicit removal.
    pending_queue: Arc<Mutex<VecDeque<String>>>,
    /// Cancellation tokens for each task, used to abort in-flight LLM calls.
    task_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Per-task level-triggered status watchers: the ReAct loop blocks on the
    /// receiver (`subscribe_status`) instead of polling, and a transition
    /// that lands between a state read and the wait is never lost (unlike the
    /// edge-triggered Notify it replaced, the stored value makes `changed()`
    /// resolve immediately when the value moved).
    status_tx: Arc<Mutex<HashMap<String, watch::Sender<TaskStatus>>>>,
    /// Dispatch wake counter: incremented on every transition to Pending.
    /// The dispatcher waits on a receiver of this watch, so a task that
    /// becomes Pending right after a failed claim still wakes it (no missed
    /// notification, no polling fallback).
    dispatch_tx: watch::Sender<u64>,
    /// Per-task buffer of completed background-job results, delivered to the
    /// ReAct loop as context at the next step start. Kept separate from the
    /// steering queue so job output is never mistaken for a user reply (the
    /// `ask` pause path keys resume off the steering queue, which now holds
    /// only genuine user interjections).
    job_completions: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Pending user confirmations for safety-gated tool calls, keyed by the
    /// generated step id reported in the `confirm:requested` event.
    confirm_waits: Arc<Mutex<HashMap<haven_common::types::ConfirmId, ConfirmWait>>>,
    /// Coordinated lifecycle for checkpointed stream text (checkpoint /
    /// promote / discard), shared with the agent loop and the end/rollback
    /// paths.
    pub partials: Arc<crate::partial::PartialStore>,
    pub on_confirm_request: ConfirmRequestCallback,
    pub on_task_error: TaskErrorCallback,
}

impl TaskExecutor {
    pub fn new(db: Arc<Database>, tools: Arc<ToolsManager>, max_concurrent: usize) -> Self {
        Self {
            partials: Arc::new(crate::partial::PartialStore::new(db.clone())),
            db,
            tools,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            running_tasks: Arc::new(Mutex::new(HashSet::new())),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent: std::sync::atomic::AtomicUsize::new(max_concurrent),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            task_permits: Arc::new(Mutex::new(HashMap::new())),
            task_cancellations: Arc::new(Mutex::new(HashMap::new())),
            status_tx: Arc::new(Mutex::new(HashMap::new())),
            dispatch_tx: watch::channel(0).0,
            job_completions: Arc::new(Mutex::new(HashMap::new())),
            confirm_waits: Arc::new(Mutex::new(HashMap::new())),
            on_confirm_request: Arc::new(Mutex::new(None)),
            on_task_error: Arc::new(Mutex::new(None)),
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
        tasks.insert(task.id.clone(), Arc::new(Mutex::new(task.clone())));

        // FIFO dispatch: queue the task before waking so the dispatcher's
        // first claim finds it at the tail, in submission order.
        self.enqueue_pending(&task.id).await;

        // Wake the dispatcher so it picks up this Pending task immediately.
        self.wake_dispatcher();
        Ok(task)
    }

    /// Bump the dispatcher wake counter. Level-triggered: a bump that lands
    /// between a failed claim and the dispatcher's wait resolves `changed()`
    /// immediately, so no transition is ever lost.
    fn wake_dispatcher(&self) {
        self.dispatch_tx.send_modify(|c| *c += 1);
    }

    /// Enqueue a task id at the tail of the FIFO dispatch queue (idempotent:
    /// a task already queued is not duplicated).
    async fn enqueue_pending(&self, task_id: &str) {
        let mut q = self.pending_queue.lock().await;
        if !q.iter().any(|t| t == task_id) {
            q.push_back(task_id.to_string());
        }
    }

    /// Remove a task id from the FIFO dispatch queue (no-op when absent).
    async fn dequeue_pending(&self, task_id: &str) {
        let mut q = self.pending_queue.lock().await;
        q.retain(|t| t != task_id);
    }

    /// Subscribe to dispatch wake signals (a `watch` receiver on the wake
    /// counter). The receiver resolves as soon as the counter moved past the
    /// version it has seen, so it must be created before the first claim.
    pub fn subscribe_dispatch(&self) -> watch::Receiver<u64> {
        self.dispatch_tx.subscribe()
    }

    /// Adjust the task concurrency ceiling at runtime (settings save). The
    /// semaphore permit count is updated by the delta:
    /// - Raising: `add_permits` grows the cap immediately; queued tasks start
    ///   as soon as a permit is free.
    /// - Lowering: unused permits are reclaimed best-effort. Permits held by
    ///   in-flight tasks cannot be revoked (they finish and release naturally),
    ///   so the effective concurrency may stay above the new target until the
    ///   current tasks complete — never forcibly cancelled.
    pub fn set_max_concurrent(&self, new_max: usize) {
        let new_max = new_max.max(1);
        let cur = self
            .max_concurrent
            .load(std::sync::atomic::Ordering::Relaxed);
        if new_max == cur {
            return;
        }
        if new_max > cur {
            self.semaphore.add_permits(new_max - cur);
        } else {
            let mut reclaimed = 0usize;
            while reclaimed < cur - new_max {
                match self.semaphore.clone().try_acquire_owned() {
                    Ok(p) => {
                        // `forget` (not `drop`): dropping a permit returns it
                        // to the semaphore, which would make the reclaim loop
                        // a no-op and let the ceiling drift (a later raise
                        // would then overshoot by the stale delta).
                        p.forget();
                        reclaimed += 1;
                    }
                    Err(_) => break,
                }
            }
            if reclaimed < cur - new_max {
                tracing::warn!(
                    "set_max_concurrent: reclaimed {}/{} permits; in-flight tasks keep \
                     the effective concurrency above the new target until they finish",
                    reclaimed,
                    cur - new_max
                );
            }
        }
        self.max_concurrent
            .store(new_max, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("task concurrency ceiling: {} -> {}", cur, new_max);
    }

    /// Persist a task status to the DB with a small number of retries. SQLite
    /// writes through the blocking pool can transiently fail with SQLITE_BUSY;
    /// a short retry turns that into extra latency instead of a diverged
    /// memory/DB state. Returns the last failure after exhausting the retries.
    async fn persist_status(db: &Arc<Database>, task_id: &str, status: &str) -> anyhow::Result<()> {
        let mut last_err = None;
        for attempt in 0..3 {
            let db = db.clone();
            let tid = task_id.to_string();
            let st = status.to_string();
            match db
                .run_blocking(move |db| db.update_task_status(&tid, &st))
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("status persist failed")))
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
            // Subscribe BEFORE the first claim so a Pending transition that
            // lands between a failed claim and the wait below is never lost:
            // `changed()` resolves immediately when the counter moved.
            let mut dispatch_rx = exec.subscribe_dispatch();
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
                    // Wait for the next Pending transition, then re-claim.
                    let _ = dispatch_rx.changed().await;
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
                tracing::info!(task_id = %task_id, "dispatcher spawning handler");
                // Run the handler on a nested task so a panic in the ReAct
                // loop is contained: the JoinHandle turns it into an Err and
                // the cleanup below still runs. Without this, a panicked
                // handler would skip the Error marking and unmark_running,
                // leaving the task stuck in Running (memory + DB) forever.
                //
                // The handler runs inside a task-level span so every log line
                // emitted by the ReAct loop (agent, compactor, title,
                // inference) carries the task_id even when the call site does
                // not name it — parallel tasks stay distinguishable in logs.
                let task_span = tracing::info_span!("run_task", task_id = %task_id);
                tokio::spawn(async move {
                    let result =
                        tokio::spawn(handler_inner(task_id.clone()).instrument(task_span)).await;
                    let failed = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(format!("handler failed: {}", e)),
                        Err(join_err) if join_err.is_panic() => {
                            Some(format!("handler panicked: {}", join_err))
                        }
                        Err(join_err) => Some(format!("handler aborted: {}", join_err)),
                    };
                    if let Some(reason) = failed {
                        tracing::error!(task_id = %task_id, "dispatcher task {} {}", task_id, reason);
                        let _ = exec_inner
                            .update_task_status(&task_id, TaskStatus::Error)
                            .await;
                        // The ReAct loop errored out: kill any background jobs
                        // the task spawned so their children cannot leak.
                        exec_inner.cancel_task_jobs(&task_id).await;
                        // The ReAct loop never emitted a terminal event for
                        // this failure (panic bypasses its error path), so
                        // surface it through the wired callback — otherwise
                        // the UI keeps the task in its busy set and the chip
                        // would stay stuck on "waiting" forever.
                        if let Some(cb) = exec_inner.on_task_error.lock().await.as_ref() {
                            cb(task_id.clone(), reason);
                        }
                    }
                    exec_inner.unmark_running(&task_id).await;
                });
            }
        });
    }

    /// Claim the oldest `Pending` task from the FIFO dispatch queue, flip it
    /// to `Running` (memory + DB) and insert it into the running set. Returns
    /// the claimed task id, or `None` if nothing is dispatchable.
    ///
    /// Stale queue entries are skipped without re-queuing: a task whose
    /// status moved away from Pending (paused, cancelled, ended) or whose
    /// handler is still alive (a supplement flipped it Paused → Pending —
    /// its own loop picks up the supplement via the status watcher, and only
    /// the dispatcher inserts into `running_tasks`, so a re-claim would be a
    /// double-dispatch) must not be started again.
    ///
    /// The status flip happens under the task's own entry lock (never under
    /// the map lock), so a slow transition of another task cannot block the
    /// claim. The DB write precedes the memory flip: on a persistent DB
    /// failure the claim is aborted before memory and the running set diverge,
    /// keeping the memory/DB error policy consistent with `update_task_status`.
    /// The task is re-queued at the tail so it is not lost.
    async fn try_claim_pending(&self) -> Option<String> {
        loop {
            let task_id = {
                let mut q = self.pending_queue.lock().await;
                match q.pop_front() {
                    Some(id) => id,
                    None => return None,
                }
            };
            let entry = { self.tasks.lock().await.get(&task_id).cloned() };
            let Some(entry) = entry else {
                // Task removed between enqueue and claim (end_task /
                // remove_task / terminal cleanup): stale queue entry.
                continue;
            };
            let mut task = entry.lock().await;
            if task.status != TaskStatus::Pending {
                // Status moved (paused, ended, errored) while queued: no
                // longer dispatchable, and the transition path already
                // re-enqueued it if it became Pending again.
                continue;
            }
            // The `running_tasks` check prevents double-dispatch: a task whose
            // handler is still alive (e.g. blocked in a pause-wait after a
            // supplement flipped it Paused → Pending) must not be claimed
            // again — its own loop picks up the supplement via the status
            // watcher. Only the dispatcher inserts into this set, so the
            // check-then-insert below cannot race.
            if self.running_tasks.lock().await.contains(&task_id) {
                continue;
            }
            if let Err(e) = Self::persist_status(&self.db, &task_id, "running").await {
                tracing::error!(
                    "try_claim_pending: DB persist failed for task {}; re-queuing it: {}",
                    task_id,
                    e
                );
                self.pending_queue.lock().await.push_back(task_id);
                return None;
            }
            task.status = TaskStatus::Running;
            task.updated_at = chrono::Utc::now().to_rfc3339();
            self.running_tasks.lock().await.insert(task_id.clone());
            tracing::debug!("try_claim_pending: claimed task {}", task_id);
            return Some(task_id);
        }
    }

    /// Remove a task from the running set. Terminal status updates are
    /// performed by the handler / agent loop. Also removes terminal-status
    /// tasks from the in-memory list so `try_claim_pending` only counts
    /// active (Pending / Running) tasks.
    async fn unmark_running(&self, task_id: &str) {
        self.cleanup_task_maps(task_id).await;
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            return;
        };
        let status = entry.lock().await.status.clone();
        if status == TaskStatus::Error || status == TaskStatus::Completed {
            tracing::debug!(
                "task {} unmark_running: {:?}, removing from list",
                task_id,
                status
            );
            self.dequeue_pending(task_id).await;
            self.tasks.lock().await.remove(task_id);
        } else {
            // The handler has exited (unmark_running runs after the handler
            // future completes). A task left Pending here is claimable again
            // — re-enqueue it, or it would strand forever: the FIFO claim
            // consumed its queue entry when it skipped it while the handler
            // was still alive, and no later Pending transition re-queues it.
            // The alive-handler case is safe: a task whose handler is truly
            // still running is claimed only after `running_tasks` re-check.
            if status == TaskStatus::Pending {
                self.enqueue_pending(task_id).await;
                // The dispatcher may be parked on its wake channel after a
                // failed claim; re-queueing alone would not wake it.
                self.wake_dispatcher();
            }
            tracing::debug!(
                "task {} unmark_running: {:?}, keeping in list",
                task_id,
                status
            );
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
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            anyhow::bail!("task '{}' not found", task_id)
        };
        let mut task = entry.lock().await;
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
    }

    pub async fn get_supplements(&self, task_id: &str) -> Vec<Supplement> {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            return Vec::new();
        };
        entry.lock().await.supplement_queue.drain(..).collect()
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
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            anyhow::bail!("task '{}' not found", task_id)
        };
        let mut task = entry.lock().await;
        task.steering_queue
            .push(Supplement::new(text, attachments.to_vec()));
        tracing::debug!(
            "task {} steering added ({} chars, {} attachments)",
            task_id,
            text.len(),
            attachments.len()
        );
        Ok(())
    }

    /// Drain the steering queue for a task.
    pub async fn get_steering(&self, task_id: &str) -> Vec<Supplement> {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            return Vec::new();
        };
        entry.lock().await.steering_queue.drain(..).collect()
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

    /// Drain all pending user-facing context for a task in one lock pass:
    /// supplements (paused-task replies / `ask` answers), steering (mid-run
    /// user interjections) and buffered background-job results. The ReAct loop
    /// calls this once per step instead of three separate queue drains (three
    /// global task-map lock acquisitions per step), so the three batches can
    /// never drift apart either.
    pub async fn drain_pending_context(
        &self,
        task_id: &str,
    ) -> (Vec<Supplement>, Vec<Supplement>, Vec<String>) {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let (supplements, steering) = match entry {
            Some(entry) => {
                let mut task = entry.lock().await;
                (
                    task.supplement_queue.drain(..).collect(),
                    task.steering_queue.drain(..).collect(),
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let job_results = self
            .job_completions
            .lock()
            .await
            .remove(task_id)
            .unwrap_or_default();
        (supplements, steering, job_results)
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
        // Promote checkpointed stream text into history (skip when a real
        // message already supersedes it). Runs BEFORE the task is torn down;
        // the PartialStore's generation bump also invalidates any in-flight
        // checkpoint so it cannot re-create the row afterwards.
        if let Err(e) = self.partials.promote(task_id).await {
            tracing::warn!(
                "end_task: failed to promote partial reply for task {}: {}",
                task_id,
                e
            );
        }
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            // Task not in memory (e.g. after restart) 鈥?end it regardless of
            // its DB state; the user asked to finish it.
            if let Err(e) = Self::persist_status(&self.db, task_id, "completed").await {
                tracing::error!("end_task: DB persist failed for task {}: {}", task_id, e);
                return Err(e);
            }
            return Ok(TaskStatus::Completed);
        };
        {
            let mut task = entry.lock().await;
            if let Err(e) = Self::persist_status(&self.db, task_id, "completed").await {
                tracing::error!("end_task: DB persist failed for task {}: {}", task_id, e);
                return Err(e);
            }
            task.status = TaskStatus::Completed;
            task.updated_at = chrono::Utc::now().to_rfc3339();
        }
        // Wake any ReAct-loop status waiter before tearing down the rest of
        // the per-task state.
        if let Some(tx) = self.status_tx.lock().await.remove(task_id) {
            let _ = tx.send(TaskStatus::Completed);
        }
        self.dequeue_pending(task_id).await;
        self.cleanup_task_maps(task_id).await;
        self.tasks.lock().await.remove(task_id);
        Ok(TaskStatus::Completed)
    }

    /// Remove a task entirely from the in-memory state.
    /// This does NOT delete from DB 鈥?the caller handles that.
    /// Succeeds even if the task is not in memory (e.g. after restart).
    pub async fn remove_task(&self, task_id: &str) {
        self.cancel_task_jobs(task_id).await;
        self.tasks.lock().await.remove(task_id);
        self.dequeue_pending(task_id).await;
        self.cleanup_task_maps(task_id).await;
        self.status_tx.lock().await.remove(task_id);
        self.job_completions.lock().await.remove(task_id);
    }

    pub async fn update_task_title(&self, task_id: &str, title: &str) {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        if let Some(entry) = entry {
            entry.lock().await.title = Some(title.into());
        }
    }

    pub async fn list_tasks(&self) -> Vec<TaskInfo> {
        let entries: Vec<Arc<Mutex<TaskInfo>>> =
            self.tasks.lock().await.values().cloned().collect();
        let mut tasks: Vec<TaskInfo> = Vec::with_capacity(entries.len());
        for entry in entries {
            tasks.push(entry.lock().await.clone());
        }
        // Preserve the insertion-order semantics of the former Vec storage
        // (the map itself is unordered).
        tasks.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        tasks
    }

    /// Remove all tasks from memory and clean up running state.
    /// Used when the user clears history 鈥?the DB is already wiped.
    pub async fn clear_all_tasks(&self) {
        self.tasks.lock().await.clear();
        self.pending_queue.lock().await.clear();
        self.running_tasks.lock().await.clear();
        self.task_permits.lock().await.clear();
        self.task_cancellations.lock().await.clear();
        self.status_tx.lock().await.clear();
        self.job_completions.lock().await.clear();
    }

    /// Subscribe to a task's status changes. Level-triggered: the receiver
    /// holds the CURRENT status, so a transition that happened before the
    /// subscription is visible immediately, and `changed()` resolves as soon
    /// as the status moves after the receiver's last observed value. Callers
    /// must re-read the authoritative state after waking (the watch value is
    /// a hint, not a lock-free source of truth).
    pub async fn subscribe_status(&self, task_id: &str) -> watch::Receiver<TaskStatus> {
        // Initial value: the task's current status so a receiver created
        // after a transition observes it; Pending when the task is absent
        // (the caller re-checks state after waking anyway).
        let initial = {
            let entry = self.tasks.lock().await.get(task_id).cloned();
            match entry {
                Some(e) => e.lock().await.status.clone(),
                None => TaskStatus::Pending,
            }
        };
        self.status_tx
            .lock()
            .await
            .entry(task_id.to_string())
            .or_insert_with(|| watch::channel(initial).0)
            .subscribe()
    }

    /// Transition a task's status through the centralized state machine.
    ///
    /// Ordering guarantees (all under the task's own entry lock, so
    /// transitions of different tasks never serialize on a global lock):
    /// 1. The transition is validated against `can_transition`; illegal
    ///    transitions (e.g. mutating a terminal state) are rejected with a
    ///    warning and leave the state untouched.
    /// 2. The DB write happens BEFORE the memory flip, with a short retry, so
    ///    a persistent DB failure aborts the transition with memory/DB
    ///    consistent (the DB is the source of truth across restarts).
    /// 3. The status watcher is notified and, for Pending transitions, the
    ///    dispatcher is woken — outside the entry lock.
    /// 4. Terminal transitions run cleanup (maps, per-task tools, working
    ///    set) after the wake so a waiter observing the terminal status
    ///    always sees the task still resolvable.
    pub async fn update_task_status(
        &self,
        task_id: &str,
        status: TaskStatus,
    ) -> anyhow::Result<()> {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        let Some(entry) = entry else {
            // Task not in memory (e.g. already removed): no-op, matching the
            // historical behavior of silently succeeding.
            return Ok(());
        };
        let mut task = entry.lock().await;
        let old_status = task.status.clone();
        if old_status == status {
            // Same-status refresh: still wake the dispatcher so a task
            // re-registered as Pending (e.g. `create_task_with_first_message`)
            // is picked up even though its status did not change.
            if status == TaskStatus::Pending {
                self.enqueue_pending(task_id).await;
                self.wake_dispatcher();
            }
            return Ok(());
        }
        if !Self::can_transition(&old_status, &status) {
            tracing::warn!(
                "update_task_status: rejected illegal transition task={} {:?} -> {:?}",
                task_id,
                old_status,
                status
            );
            return Ok(());
        }
        if let Err(e) = Self::persist_status(&self.db, task_id, status.as_str()).await {
            tracing::error!(
                "update_task_status: DB persist failed for task {}; transition {:?} -> {:?} aborted: {}",
                task_id,
                old_status,
                status,
                e
            );
            return Err(e);
        }
        task.status = status.clone();
        task.updated_at = chrono::Utc::now().to_rfc3339();
        tracing::info!(
            "update_task_status: task={} {} -> {}",
            task_id,
            old_status.as_str(),
            status.as_str()
        );
        let is_pending = status == TaskStatus::Pending;
        let is_terminal = status.is_terminal();
        drop(task);
        drop(entry);
        // Level-triggered wake: send on the existing watcher channel (or
        // lazily create one) so the ReAct loop's pause-wait resolves.
        let tx = {
            let mut map = self.status_tx.lock().await;
            map.entry(task_id.to_string())
                .or_insert_with(|| watch::channel(status.clone()).0)
                .clone()
        };
        let _ = tx.send(status.clone());
        if is_pending {
            self.enqueue_pending(task_id).await;
            self.wake_dispatcher();
        }
        if is_terminal {
            self.dequeue_pending(task_id).await;
            self.cleanup_task_maps(task_id).await;
            self.tools.unregister_task(task_id).await;
            if let Some(tx) = self.status_tx.lock().await.remove(task_id) {
                let _ = tx.send(status);
            }
            self.tasks.lock().await.remove(task_id);
        }
        Ok(())
    }

    /// Centralized transition validation. Only transitions reachable from
    /// real call sites are allowed; anything else (notably any mutation of a
    /// terminal state except the explicit reopen/continue flows) is a bug and
    /// is rejected.
    fn can_transition(from: &TaskStatus, to: &TaskStatus) -> bool {
        use TaskStatus::*;
        match (from, to) {
            // Claim by the dispatcher.
            (Pending, Running) => true,
            // Park / finish a queued task without dispatching it.
            (Pending, Paused) | (Pending, PausedAwaitingAnswer) => true,
            (Pending, Completed) | (Pending, Error) => true,
            // Pause for a user reply / scheduling / budget checkpoint.
            (Running, Paused) | (Running, PausedAwaitingAnswer) => true,
            // Immediate resume: the ask was answered in the same turn
            // (pause_turn → Pending while the handler is still alive).
            (Running, Pending) => true,
            // Natural completion / failure.
            (Running, Completed) | (Running, Error) => true,
            // Resume paths (user message, job completion, continue flow).
            (Paused, Pending) | (PausedAwaitingAnswer, Pending) => true,
            // Re-pause with an answer requirement.
            (Paused, PausedAwaitingAnswer) => true,
            // Defensive force-resume when a tool executes on a paused task
            // (see `execute_step`; kept from the pre-validation era).
            (Paused, Running) | (PausedAwaitingAnswer, Running) => true,
            // Finish / fail a paused task (end_task's own path also exists,
            // but explicit transitions are kept valid).
            (Paused, Completed) | (Paused, Error) => true,
            (PausedAwaitingAnswer, Completed) | (PausedAwaitingAnswer, Error) => true,
            // User-driven exceptions: reopen a finished task for review
            // (history flow), retry an errored task from its snapshot.
            (Completed, Paused) | (Error, Paused) => true,
            (Error, Pending) => true,
            _ => false,
        }
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
    /// Does NOT touch `tasks` (working set) or `status_tx` — those have
    /// ordering-sensitive callers (`update_task_status`, `unmark_running`)
    /// that need to remain in the lock-order path.
    pub async fn cleanup_task_maps(&self, task_id: &str) {
        self.running_tasks.lock().await.remove(task_id);
        self.task_permits.lock().await.remove(task_id);
        self.task_cancellations.lock().await.remove(task_id);
    }

    /// Look up an in-memory `TaskInfo` by id (O(1), per-task lock only).
    pub async fn get_task(&self, task_id: &str) -> Option<TaskInfo> {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        Some(entry?.lock().await.clone())
    }

    /// Load a task from the database into the in-memory list if it is not
    /// already there (e.g. after an app restart). Used by `process_input`
    /// so that follow-up messages can reach tasks that were paused before
    /// the restart and never re-entered the executor's working set.
    pub async fn ensure_task_loaded(&self, task_id: &str) -> anyhow::Result<()> {
        {
            let tasks = self.tasks.lock().await;
            if tasks.contains_key(task_id) {
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
        if !tasks.contains_key(task_id) {
            tasks.insert(task_id.to_string(), Arc::new(Mutex::new(task)));
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
        let mut queued = Vec::new();
        {
            let mut tasks = self.tasks.lock().await;
            for record in pending {
                if tasks.contains_key(&record.id) {
                    continue;
                }
                // Force Pending: this loader only ever rehydrates tasks
                // whose DB status is already "pending" (the SQL filter
                // guarantees that), so the override is a no-op but keeps
                // the invariant explicit at the call site.
                let mut info = TaskInfo::from_db_record(&record);
                info.status = TaskStatus::Pending;
                tasks.insert(record.id.clone(), Arc::new(Mutex::new(info)));
                queued.push(record.id);
                loaded += 1;
            }
        }
        // FIFO: enqueue after releasing the working-set lock (the queue lock
        // is never held across the map lock to keep the order acyclic).
        for id in queued {
            self.enqueue_pending(&id).await;
        }
        if loaded > 0 {
            self.wake_dispatcher();
        }
        loaded
    }

    /// Current in-memory status of a task, or `None` when the task is not in
    /// the working set (removed on terminal cleanup / `end_task` / restart).
    /// Deliberately does NOT conflate "absent" with `Error`: callers that
    /// previously probed for `Error` to detect removal must check for `None`.
    pub async fn get_task_state(&self, task_id: &str) -> Option<TaskStatus> {
        let entry = { self.tasks.lock().await.get(task_id).cloned() };
        Some(entry?.lock().await.status.clone())
    }

    pub fn get_tools(&self) -> Arc<ToolsManager> {
        self.tools.clone()
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Cancel and drop all background jobs owned by a task, and cancel its
    /// pending reminders. Called when the task ends, is removed, or is
    /// rolled back so child processes cannot leak past their task and no
    /// reminder fires against a task that no longer exists.
    pub async fn cancel_task_jobs(&self, task_id: &str) {
        self.tools.background_jobs.cancel_for_task(task_id).await;
        self.tools.reminders.cancel_for_task(task_id).await;
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
            let entry = { self.tasks.lock().await.get(task_id).cloned() };
            if let Some(entry) = entry {
                let mut task = entry.lock().await;
                let prev = task.status.clone();
                if prev.is_terminal() {
                    // Executing a tool on a Completed/Error task is always a
                    // stale call from a racing loop (end/rollback already
                    // finished this task). Failing fast here prevents the
                    // force below from resurrecting a dead task.
                    return Err(anyhow::anyhow!(
                        "execute_step: task {} is {}; refusing to execute tool '{}'",
                        task_id,
                        prev.as_str(),
                        tool_name
                    ));
                }
                if prev != TaskStatus::Running {
                    // The ReAct loop runs with status Pending (the dispatcher
                    // never flips it to Running), and scheduled reminders fire
                    // on Paused tasks; both are legitimate tool-call moments.
                    task.status = TaskStatus::Running;
                    tracing::warn!(
                        "execute_step: task {} was {} before tool call, forcing Running",
                        task_id,
                        prev.as_str()
                    );
                }
            }
        }

        let cancel = self.cancellation_token(task_id).await;
        let gated = self
            .execute_gated(Some(task_id), tool_name, input.clone(), cancel)
            .await?;
        let ToolExecution {
            result,
            risk_level,
            confirmed,
        } = gated;
        tracing::info!(
            "execute_step result: tool={} success={}",
            tool_name,
            result.success
        );

        // Apply the tool's declared per-task side effects (skill/MCP adapter
        // registration) instead of name-matching load_skill/load_mcp here —
        // a new tool with a side effect declares it via `Tool::registrations`
        // and nothing in this executor needs to change. Background-job
        // bindings are applied after the running-set guard below (a job
        // spawned in a concurrently-rolled-back step must not attach past
        // the cleanup sweep). `registrations` is extracted ONCE: calling it
        // twice could yield divergent results for stateful tools, and the
        // variant split below is explicit rather than silently partitioned.
        let registrations = if result.success {
            self.tools
                .get_tool_for_task(Some(task_id), tool_name)
                .await
                .map(|t| t.registrations(&result.output))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for reg in &registrations {
            match reg {
                haven_tools::ToolRegistration::Skill(name) => {
                    self.tools.register_skill_for_task(task_id, name).await;
                }
                haven_tools::ToolRegistration::McpServer(name) => {
                    self.tools.register_mcp_for_task(task_id, name).await;
                }
                // Activity is applied after the running-set guard.
                haven_tools::ToolRegistration::Activity(_) => {}
            }
        }
        let step_number = step_num as i32;
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
        // Applied only AFTER the running-set guard above passed (a rollback
        // racing this step may have removed the task); the registrations were
        // extracted once, before the guard.
        for reg in &registrations {
            if let haven_tools::ToolRegistration::Activity(job_id) = reg {
                self.tools
                    .background_jobs
                    .attach_task(job_id, task_id)
                    .await;
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
                        step_number,
                        &tool_name,
                        &tool_input,
                        risk_level != RiskLevel::Safe,
                        silent,
                        confirmed,
                    )
                }
            })
            .await?;
        let obs = result.summary_text();
        let step_id = step_record.id.clone();
        let success = result.success;
        // The in-memory StepInfo reuses the persisted step row's id so the
        // live task state and the review history reference the same step.
        if let Some(entry) = self.tasks.lock().await.get(task_id).cloned() {
            let mut task = entry.lock().await;
            task.steps.push(StepInfo {
                id: step_id.clone(),
                step_number,
                tool_name: tool_name.into(),
                input: input.clone(),
                output: Some(result.output.clone()),
                status: if success {
                    "completed".into()
                } else {
                    "failed".into()
                },
                risk_level,
                confirmed,
            });
            task.updated_at = chrono::Utc::now().to_rfc3339();
        }
        self.db
            .run_blocking(move |db| db.complete_action_step(&step_id, &obs, success))
            .await?;
        Ok(result)
    }

    /// Execute a tool through the safety gateway. The tool's risk level is
    /// checked against the configured threshold BEFORE anything runs; an
    /// operation at/above the threshold blocks on the user's confirmation
    /// (`confirm:requested` event + `resolve_confirmation`), and is aborted
    /// when the user declines or the task is cancelled. Returns a failed
    /// `ToolResult` for declined operations so the ReAct loop sees a normal
    /// tool failure the model can react to.
    pub async fn execute_gated(
        &self,
        task_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolExecution> {
        let risk_level = self.tools.get_risk_level(task_id, tool_name, &input).await;
        let mut confirmed: Option<bool> = None;
        match self
            .tools
            .safety_gateway
            .check(tool_name, &input, risk_level)
            .await
        {
            ConfirmationResult::AutoApproved => {}
            ConfirmationResult::Blocked => {
                return Ok(ToolExecution {
                    result: ToolResult {
                        success: false,
                        output: Value::Null,
                        error: Some(format!(
                            "operation '{}' is blocked by the security policy. Do NOT retry it — ask the user what to do instead or choose a different approach.",
                            tool_name
                        )),
                        truncated: false,
                        signals: haven_tools::ToolSignals::default(),
                    },
                    risk_level,
                    confirmed: Some(false),
                });
            }
            ConfirmationResult::RequiresConfirmation { .. } => {
                if !self
                    .await_confirmation(task_id, tool_name, risk_level)
                    .await
                {
                    return Ok(ToolExecution {
                        result: ToolResult {
                            success: false,
                            output: Value::Null,
                            error: Some(format!(
                                "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                tool_name
                            )),
                            truncated: false,
                            signals: haven_tools::ToolSignals::default(),
                        },
                        risk_level,
                        confirmed: Some(false),
                    });
                }
                confirmed = Some(true);
            }
        }
        let result = self
            .tools
            .execute_tool(task_id, tool_name, input, cancel)
            .await?;
        Ok(ToolExecution {
            result,
            risk_level,
            confirmed,
        })
    }

    /// Request user confirmation for a safety-gated tool call and wait for
    /// the answer. Emits `confirm:requested` through the wired callback and
    /// blocks until `resolve_confirmation` resolves the generated step id, or
    /// the task's cancellation token fires (end/rollback/stop). Returns
    /// `true` when the user approved.
    async fn await_confirmation(
        &self,
        task_id: Option<&str>,
        tool_name: &str,
        risk_level: RiskLevel,
    ) -> bool {
        let step_id: haven_common::types::ConfirmId = haven_common::types::new_id("conf").into();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.confirm_waits
            .lock()
            .await
            .insert(step_id.clone(), ConfirmWait { risk_level, tx });
        let tid = task_id.unwrap_or("activity").to_string();
        // No confirmation callback wired (unit tests, degraded startup):
        // there is no UI that could ever answer — fail closed so the tool
        // never runs without approval, instead of blocking the task forever.
        if self.on_confirm_request.lock().await.as_ref().is_none() {
            self.confirm_waits.lock().await.remove(&step_id);
            tracing::info!(
                "confirmation for tool '{}' on task {} rejected: no confirmation channel wired",
                tool_name,
                tid
            );
            return false;
        }
        if let Some(cb) = self.on_confirm_request.lock().await.as_ref() {
            cb(
                step_id.clone(),
                tid.clone(),
                tool_name.to_string(),
                risk_level,
            );
        }
        let cancel = self.cancellation_token(&tid).await;
        let decision = tokio::select! {
            r = rx => r.ok(),
            _ = cancel.cancelled() => None,
            // Bounded, fail-closed fallback: an unanswered confirmation (e.g.
            // the app window is closed when a scheduled reminder fires) must
            // not wedge the task — or the sequential reminder consumer —
            // forever.
            _ = tokio::time::sleep(CONFIRM_WAIT_TIMEOUT) => {
                tracing::warn!(
                    "confirmation for tool '{}' on task {} timed out after {:?}; treating as rejected",
                    tool_name,
                    tid,
                    CONFIRM_WAIT_TIMEOUT
                );
                None
            }
        };
        self.confirm_waits.lock().await.remove(&step_id);
        match decision {
            Some(true) => true,
            Some(false) | None => {
                tracing::info!(
                    "confirmation for tool '{}' on task {} not approved (answer={:?})",
                    tool_name,
                    tid,
                    decision
                );
                false
            }
        }
    }

    /// Resolve a pending safety-gateway confirmation and return the risk level
    /// the gate attached to it, so the caller can trust the level for the
    /// session. The approval/denial itself is persisted on the real `task_steps`
    /// row when `create_action_step` records the step (via the `confirmed`
    /// returned by `execute_gated`); this method only unblocks the ReAct loop
    /// waiting on the oneshot. Every step id handed here comes from a
    /// `confirm:requested` payload, which is only emitted by `await_confirmation`
    /// — so an id not present in `confirm_waits` is stale (already resolved or
    /// cancelled); there is no legacy path.
    pub async fn resolve_confirmation(
        &self,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> anyhow::Result<Option<RiskLevel>> {
        if let Some(wait) = self.confirm_waits.lock().await.remove(step_id) {
            let level = wait.risk_level;
            let _ = wait.tx.send(confirmed);
            return Ok(Some(level));
        }
        Ok(None)
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

        // The panic path bypasses the ReAct loop's event emission, so the
        // wired on_task_error callback must fire — otherwise the UI would
        // never learn about the terminal transition.
        let notified = Arc::new(tokio::sync::Mutex::new(None::<(String, String)>));
        let nt = notified.clone();
        *exec.on_task_error.lock().await =
            Some(Box::new(move |task_id: String, reason: String| {
                *nt.try_lock().unwrap() = Some((task_id, reason));
            }));

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
        // the running slot; the task is absent, not "error" in memory.
        assert!(!exec.running_tasks.lock().await.contains(&task.id));
        assert_eq!(exec.get_task_state(&task.id).await, None);
        // The wired failure callback fired with the task id and a panic
        // reason (the UI clears its busy set from this signal). Poll: the
        // callback runs right after the DB write in the dispatcher's spawned
        // task.
        let mut seen = None;
        for _ in 0..100 {
            seen = notified.lock().await.clone();
            if seen.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (seen_id, seen_reason) = seen.expect("on_task_error callback must fire");
        assert_eq!(seen_id, task.id);
        assert!(seen_reason.contains("panicked"), "reason: {seen_reason}");
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
        assert_eq!(state, Some(TaskStatus::Running));
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
    /// The stale queue entry is consumed on the skip: the alive handler picks
    /// up the supplement via the status watcher itself, and a later transition
    /// to Pending re-enqueues the task if it ever becomes claimable again.
    #[tokio::test]
    async fn try_claim_pending_skips_task_already_in_running_set() {
        let exec = make_executor(2);
        let task = exec.create_task("t1").await.unwrap();
        exec.running_tasks.lock().await.insert(task.id.clone());

        assert!(exec.try_claim_pending().await.is_none());

        // Once the handler releases the slot, the task only becomes claimable
        // again after it re-enters the FIFO queue (a fresh Pending transition).
        exec.running_tasks.lock().await.remove(&task.id);
        assert!(exec.try_claim_pending().await.is_none());
        exec.enqueue_pending(&task.id).await;
        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(task.id.as_str()));
        assert_eq!(
            exec.get_task_state(&task.id).await,
            Some(TaskStatus::Running)
        );
    }

    /// Claims follow FIFO submission order: the oldest Pending task is
    /// claimed first, not a HashMap-iteration lottery.
    #[tokio::test]
    async fn try_claim_pending_is_fifo_by_submission_order() {
        let exec = make_executor(1);
        let t1 = exec.create_task("first").await.unwrap();
        let t2 = exec.create_task("second").await.unwrap();
        let t3 = exec.create_task("third").await.unwrap();

        let c1 = exec.try_claim_pending().await;
        let c2 = exec.try_claim_pending().await;
        let c3 = exec.try_claim_pending().await;
        assert_eq!(c1.as_deref(), Some(t1.id.as_str()));
        assert_eq!(c2.as_deref(), Some(t2.id.as_str()));
        assert_eq!(c3.as_deref(), Some(t3.id.as_str()));
        assert!(exec.try_claim_pending().await.is_none());
    }

    /// `set_max_concurrent` must reclaim permits on lowering (not return them
    /// to the semaphore — that would be a no-op) and must not overshoot on a
    /// later raise. The effective ceiling is measured by how many concurrent
    /// dispatcher acquisitions succeed without blocking.
    #[tokio::test]
    async fn set_max_concurrent_reclaims_and_does_not_overshoot() {
        let exec = make_executor(4);
        exec.set_max_concurrent(1);
        // Idle pool: exactly one permit may be acquired without waiting.
        let first = exec.semaphore.clone().try_acquire_owned();
        assert!(
            first.is_ok(),
            "one permit must be available after lowering to 1"
        );
        let second = exec.semaphore.clone().try_acquire_owned();
        assert!(
            second.is_err(),
            "lowering must reclaim unused permits (no-op reclaim would leave 3 free)"
        );
        drop(first.unwrap());
        // Raise back to 3: available permits must be 3, not 3 + stale 3.
        exec.set_max_concurrent(3);
        let mut held = Vec::new();
        for _ in 0..3 {
            match exec.semaphore.clone().try_acquire_owned() {
                Ok(p) => held.push(p),
                Err(_) => break,
            }
        }
        assert_eq!(
            held.len(),
            3,
            "raise after lower must yield exactly 3 permits"
        );
        assert!(
            exec.semaphore.clone().try_acquire_owned().is_err(),
            "no extra permits may leak from the lower→raise cycle"
        );
        drop(held);
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
        // end_task removes the task from the working set entirely.
        assert_eq!(exec.get_task_state(&task.id).await, None);
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
        assert_eq!(
            exec.get_task_state(&task.id).await,
            Some(TaskStatus::Pending)
        );
    }

    #[tokio::test]
    async fn get_task_state_nonexistent_returns_none() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        // Absent means "not in the working set", NOT Error.
        assert_eq!(exec.get_task_state("nonexistent").await, None);
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
        assert_eq!(
            exec2.get_task_state(&task.id).await,
            Some(TaskStatus::Running)
        );
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
        // Terminal status removes the task from the in-memory working set.
        assert_eq!(exec.get_task_state(&task.id).await, None);
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

    #[tokio::test]
    async fn awaiting_answer_pause_is_distinct_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("ask me").await.unwrap();

        // The ask pause path pauses in PausedAwaitingAnswer.
        exec.update_task_status(&task.id, TaskStatus::PausedAwaitingAnswer)
            .await
            .unwrap();
        assert_eq!(
            exec.get_task_state(&task.id).await,
            Some(TaskStatus::PausedAwaitingAnswer)
        );
        // Both pause flavors report is_paused; only the answer variant
        // reports is_awaiting_answer.
        let state = exec.get_task_state(&task.id).await.unwrap();
        assert!(state.is_paused());
        assert!(state.is_awaiting_answer());
        // The wire/DB form stays "paused".
        assert_eq!(state.as_str(), "paused");

        // Reactivation (user answered → Pending) exits the awaiting state.
        exec.update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        assert_eq!(
            exec.get_task_state(&task.id).await,
            Some(TaskStatus::Pending)
        );
    }

    #[tokio::test]
    async fn plain_pause_is_not_awaiting_answer() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("pause me").await.unwrap();
        exec.update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();
        let state = exec.get_task_state(&task.id).await.unwrap();
        assert!(state.is_paused());
        assert!(!state.is_awaiting_answer());
        assert_eq!(state.as_str(), "paused");
    }

    #[tokio::test]
    async fn terminal_tasks_need_explicit_reopen_to_reactivate() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("t").await.unwrap();
        exec.update_task_status(&task.id, TaskStatus::Completed)
            .await
            .unwrap();
        // The terminal task was removed from the working set; any later
        // update on the absent entry is a silent no-op, not a resurrection.
        exec.update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        assert_eq!(exec.get_task_state(&task.id).await, None);
        // In-memory resurrection is only possible through the explicit
        // reopen path (Completed → Paused) after ensure_task_loaded.
        exec.ensure_task_loaded(&task.id).await.unwrap();
        exec.update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();
        assert_eq!(
            exec.get_task_state(&task.id).await,
            Some(TaskStatus::Paused)
        );
        // And from Paused the task resumes via the normal Paused → Pending
        // path (e.g. process_input / continue flow).
        exec.update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        assert_eq!(
            exec.get_task_state(&task.id).await,
            Some(TaskStatus::Pending)
        );
    }

    #[tokio::test]
    async fn status_watch_wakes_waiter_on_transition() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec.create_task("wait").await.unwrap();
        exec.update_task_status(&task.id, TaskStatus::Running)
            .await
            .unwrap();
        exec.update_task_status(&task.id, TaskStatus::Paused)
            .await
            .unwrap();

        // Waiter subscribes AFTER the pause (the level-triggered value must
        // still be visible) and wakes on the resume transition.
        let exec2 = exec.clone();
        let tid = task.id.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut rx = exec2.subscribe_status(&tid).await;
            let _ = rx.changed().await;
            let _ = done_tx.send(exec2.get_task_state(&tid).await);
        });

        // Give the waiter a moment to register, then transition.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        exec.update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();

        let state = tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .expect("waiter must wake within 2s")
            .unwrap();
        assert_eq!(state, Some(TaskStatus::Pending));
    }

    #[tokio::test]
    async fn same_status_pending_still_wakes_dispatcher() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(TaskExecutor::new(db, tools, 3));
        let task = exec.create_task("already pending").await.unwrap();

        // Re-registering as Pending (as `create_task_with_first_message`
        // does after `ensure_task_loaded`) must wake the dispatcher even
        // though the status did not change.
        let mut rx = exec.subscribe_dispatch();
        let before = *rx.borrow();
        exec.update_task_status(&task.id, TaskStatus::Pending)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            rx.changed().await.expect("dispatcher must wake");
        })
        .await
        .expect("dispatcher must be woken by a same-status Pending update");
        assert!(*rx.borrow() > before);
    }

    #[tokio::test]
    async fn unknown_status_string_maps_to_error() {
        assert_eq!(TaskStatus::from_status_str("bogus"), TaskStatus::Error);
        assert_eq!(TaskStatus::from_status_str("cancelled"), TaskStatus::Error);
        assert_eq!(TaskStatus::from_status_str("paused"), TaskStatus::Paused);
        // Both pause flavors serialize to the same wire string.
        assert_eq!(TaskStatus::PausedAwaitingAnswer.as_str(), "paused");
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
    async fn remove_task_clears_job_buffers_and_status_watcher() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = TaskExecutor::new(db, tools, 3);
        let task = exec.create_task("cleanup").await.unwrap();
        exec.update_task_status(&task.id, TaskStatus::PausedAwaitingAnswer)
            .await
            .unwrap();
        exec.add_job_completion(&task.id, "stranded").await;
        let rx = exec.subscribe_status(&task.id).await;
        let _ = rx; // a subscriber must not keep the task alive after removal

        exec.remove_task(&task.id).await;
        assert_eq!(exec.get_task_state(&task.id).await, None);
        assert!(exec.drain_job_completions(&task.id).await.is_empty());
    }
}
