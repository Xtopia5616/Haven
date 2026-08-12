use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::bg::{BackgroundTasks, EventSink, EventSinkState};
use crate::tool::RegistryProbe;
use crate::{Tool, ToolResult};

/// What happens when a scheduled_task fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleMode {
    /// Call the tool in `tool_name` with `tool_args` (no LLM involved).
    /// To send a message at fire time, call the `notify` tool here.
    #[default]
    Tool,
    /// Resume the session that scheduled the scheduled_task: the session continues with
    /// the scheduled_task text as a new instruction in the same conversation.
    /// Only works while the scheduling session is still alive (running/paused):
    /// scheduled_tasks are cancelled automatically when their session ends or is
    /// removed, so a `continue` scheduled_task cannot resurrect a completed session.
    Continue,
}

impl ScheduleMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ScheduleMode::Tool => "tool",
            ScheduleMode::Continue => "continue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tool" => Some(ScheduleMode::Tool),
            "continue" => Some(ScheduleMode::Continue),
            _ => None,
        }
    }
}

/// A scheduled_task that fired; delivered to the app layer so it can run a tool
/// (`Tool`, including `notify` for a message) or resume the scheduling session
/// (`Continue`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledTaskFired {
    pub task_id: String,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    /// Session that scheduled the scheduled_task —resume target for `Continue` mode,
    /// tool-context scope for `Tool` mode. `None` on legacy rows.
    pub session_id: Option<String>,
    /// `Tool` mode: tool to call when the scheduled_task fires.
    pub tool_name: Option<String>,
    /// `Tool` mode: arguments for the tool call.
    pub tool_args: Option<Value>,
    /// `Continue` mode: continuation message delivered to the session (falls
    /// back to `body`). On legacy rows it is the wake text for a new session.
    pub prompt: Option<String>,
}

/// Everything needed to schedule one scheduled_task.
pub struct ScheduledTaskSpec {
    /// Absolute fire time (RFC3339, local time accepted). Use this OR
    /// `delay_secs` OR `watch_task_id`; exactly one is required.
    pub due_at: Option<String>,
    /// Delay in seconds before the scheduled_task fires. Use this OR `due_at` OR
    /// `watch_task_id`.
    pub delay_secs: Option<u64>,
    /// Background task to wait for: the scheduled_task fires when the task reaches a
    /// terminal state (completed/failed/cancelled) instead of on a timer,
    /// resuming the session with the task's result. Use this OR `due_at` OR
    /// `delay_secs`; in-memory only (the watched task cannot survive a
    /// restart, so these scheduled_tasks are not persisted).
    pub watch_task_id: Option<String>,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    /// The session that schedules the scheduled_task (injected by the tool manager,
    /// not visible to the LLM). Resume target for `Continue`.
    pub session_id: Option<String>,
    /// `Tool` mode: tool to call when the scheduled_task fires.
    pub tool_name: Option<String>,
    /// `Tool` mode: arguments for the tool call.
    pub tool_args: Option<Value>,
    /// `Continue` mode: continuation message delivered to the session.
    pub prompt: Option<String>,
}

/// Lifetime cap on scheduled_tasks per process. Fired scheduled_tasks are reaped on the
/// next `set`, so this bounds concurrent pending timers, not history.
/// Upper bound on a `due_at`-scheduled scheduled_task (365 days) — guards against
/// typos like a swapped year. Delay-based scheduled_tasks are capped separately.
#[derive(Clone)]
struct ScheduledTaskEntry {
    title: String,
    body: String,
    due_at: String,
    mode: ScheduleMode,
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_args: Option<Value>,
    prompt: Option<String>,
    /// Background task this scheduled_task waits for (empty for timer-based
    /// scheduled_tasks). Fires when the task reaches a terminal state.
    watch_task_id: Option<String>,
    fired: bool,
}

/// Registry of in-process timers for the `scheduled_task` tool, with a persistent
/// backing store: every `set` is written to the database so scheduled_tasks
/// survive app restarts. On startup the agent layer calls `restore_pending`:
/// overdue scheduled_tasks fire immediately (missed while the app was off), the
/// rest are re-armed with their remaining delay. The in-memory timer is the
/// delivery mechanism while the app runs; the DB is the source of truth.
pub struct ScheduledTaskCenter {
    scheduled_tasks: RwLock<HashMap<String, ScheduledTaskEntry>>,
    fired_tx: tokio::sync::mpsc::UnboundedSender<ScheduledTaskFired>,
    fired_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ScheduledTaskFired>>>,
    /// Persistent store; `None` in headless/test builds (in-memory only).
    db: RwLock<Option<Arc<Database>>>,
    /// Lifetime cap on pending scheduled_tasks (from context limits).
    max_scheduled_tasks: RwLock<usize>,
    max_due_horizon_secs: RwLock<i64>,
    /// Optional UI event sink (see `EventSink`). Wired by the desktop shell
    /// to forward lifecycle events as Tauri events.
    event_sink: EventSinkState,
    /// Background-task registry for `watch_task_id` scheduled_tasks (polled for a
    /// terminal state). Wired by the tools manager; `None` in headless/test
    /// builds where task-watch scheduled_tasks are rejected.
    tasks: Mutex<Option<Arc<BackgroundTasks>>>,
}

impl Default for ScheduledTaskCenter {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduledTaskCenter {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            scheduled_tasks: RwLock::new(HashMap::new()),
            fired_tx: tx,
            fired_rx: Mutex::new(Some(rx)),
            db: RwLock::new(None),
            max_scheduled_tasks: RwLock::new(32),
            max_due_horizon_secs: RwLock::new(365 * 24 * 3600),
            event_sink: EventSinkState::default(),
            tasks: Mutex::new(None),
        }
    }

    /// Attach the background-task registry so `watch_task_id` scheduled_tasks can
    /// wait for a task to finish. Wired once by the tools manager.
    pub fn set_tasks(&self, tasks: Option<Arc<BackgroundTasks>>) {
        *self.tasks.lock().unwrap() = tasks;
    }

    /// Install the UI event sink (called once by the desktop shell).
    pub fn set_event_sink(&self, sink: EventSink) {
        self.event_sink.set(sink);
    }

    /// Forward a lifecycle event to the installed sink (no-op without one).
    fn emit(&self, event: &str, payload: Value) {
        self.event_sink.emit(event, payload);
    }

    /// Emit the `task:finished` event for a scheduled_task (delivered alongside
    /// the `ScheduledTaskFired` channel message so the UI can drop it from the
    /// pending list and surface its own toast).
    fn emit_fired(&self, id: &str, entry: &ScheduledTaskEntry) {
        self.emit(
            "task:finished",
            serde_json::json!({
                "id": id,
                "title": entry.title,
                "body": entry.body,
                "mode": entry.mode.as_str(),
                "session_id": entry.session_id.clone(),
                "due_at": entry.due_at,
            }),
        );
    }

    /// Replace the unified context limits (scheduled_task caps).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_scheduled_tasks.write().await = limits.scheduled_tasks_max;
        *self.max_due_horizon_secs.write().await = limits.scheduled_tasks_due_horizon_secs;
    }

    /// Attach the database used for persistence. Wired by the desktop shell
    /// (same handle the `self` tool receives); headless tests skip it.
    pub async fn set_db(&self, db: Option<Arc<Database>>) {
        *self.db.write().await = db;
    }

    /// Take the fired-scheduled_task receiver exactly once (consumed by the agent
    /// layer, which emits Notification events). Returns `None` if already
    /// taken.
    pub fn take_fired_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<ScheduledTaskFired>> {
        self.fired_rx.lock().unwrap().take()
    }

    /// Mark a scheduled_task fired, emit its `task:finished` event, deliver the
    /// `ScheduledTaskFired` payload, and persist the fired flag. Shared by the
    /// overdue re-arm path (`restore_pending`) and the task-watch timer.
    async fn fire_entry(self: &Arc<Self>, id: &str) {
        let mut scheduled_tasks = self.scheduled_tasks.write().await;
        if let Some(entry) = scheduled_tasks.get_mut(id)
            && !entry.fired
        {
            entry.fired = true;
            self.emit_fired(id, entry);
            let _ = self.fired_tx.send(ScheduledTaskFired {
                task_id: id.to_string(),
                title: entry.title.clone(),
                body: entry.body.clone(),
                mode: entry.mode,
                session_id: entry.session_id.clone(),
                tool_name: entry.tool_name.clone(),
                tool_args: entry.tool_args.clone(),
                prompt: entry.prompt.clone(),
            });
            if let Some(db) = self.db.read().await.as_ref() {
                let _ = db.mark_scheduled_task_fired(id);
            }
        }
    }

    /// Re-arm all pending scheduled_tasks from the database after a restart.
    ///
    /// - ScheduledTasks whose due time already passed (the app was off when they
    ///   expired) fire immediately and are marked fired.
    /// - Future scheduled_tasks are re-armed in memory with their remaining delay.
    ///
    /// Returns the number of scheduled_tasks fired as overdue. Called once from the
    /// agent layer startup; safe to call again (idempotent —in-memory
    /// entries are skipped).
    pub async fn restore_pending(self: &Arc<Self>) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        let Ok(rows) = db.list_pending_scheduled_tasks() else {
            return 0;
        };
        let now = chrono::Utc::now();
        let mut overdue = 0usize;
        for row in rows {
            // Idempotency: skip entries the in-memory map already holds
            // (restore was already run, or the scheduled_task was re-set live).
            if self.scheduled_tasks.read().await.contains_key(&row.id) {
                continue;
            }
            let due = chrono::DateTime::parse_from_rfc3339(&row.due_at)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or(now);
            let remaining = (due - now).num_seconds();
            let mode = ScheduleMode::parse(&row.mode).unwrap_or(ScheduleMode::Tool);
            let tool_args = row
                .tool_args
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            let fired_payload = ScheduledTaskFired {
                task_id: row.id.clone(),
                title: row.title.clone(),
                body: row.body.clone(),
                mode,
                session_id: row.session_id.clone(),
                tool_name: row.tool_name.clone(),
                tool_args: tool_args.clone(),
                prompt: row.prompt.clone(),
            };
            if remaining <= 0 {
                // Overdue while the app was closed: fire now.
                let entry = ScheduledTaskEntry {
                    title: row.title.clone(),
                    body: row.body.clone(),
                    due_at: row.due_at.clone(),
                    mode,
                    session_id: row.session_id.clone(),
                    tool_name: row.tool_name.clone(),
                    tool_args: tool_args.clone(),
                    prompt: row.prompt.clone(),
                    watch_task_id: None,
                    fired: true,
                };
                self.scheduled_tasks
                    .write()
                    .await
                    .insert(row.id.clone(), entry.clone());
                self.emit_fired(&row.id, &entry);
                let _ = self.fired_tx.send(fired_payload);
                let _ = db.mark_scheduled_task_fired(&row.id);
                overdue += 1;
            } else {
                let center = self.clone();
                let id = row.id.clone();
                self.scheduled_tasks.write().await.insert(
                    id.clone(),
                    ScheduledTaskEntry {
                        title: row.title.clone(),
                        body: row.body.clone(),
                        due_at: row.due_at.clone(),
                        mode,
                        session_id: row.session_id.clone(),
                        tool_name: row.tool_name.clone(),
                        tool_args: tool_args.clone(),
                        prompt: row.prompt.clone(),
                        watch_task_id: None,
                        fired: false,
                    },
                );
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(remaining as u64)).await;
                    center.fire_entry(&id).await;
                });
            }
        }
        overdue
    }

    /// Schedule a scheduled_task to fire at an absolute time, after a delay, or
    /// when a background task reaches a terminal state. Exactly one of
    /// `spec.due_at` (RFC3339, local time accepted), `spec.delay_secs` or
    /// `spec.watch_task_id` must be given. `spec.mode` selects what happens
    /// at fire time (see [`ScheduleMode`]).
    ///
    /// Returns the scheduled_task id; the timer (or task watcher) runs detached from
    /// the ReAct loop and delivers a `ScheduledTaskFired` on the channel when it
    /// expires. Task-watch scheduled_tasks are in-memory only: the watched task
    /// cannot survive a restart, so they are not persisted.
    pub async fn set(self: &Arc<Self>, spec: ScheduledTaskSpec) -> anyhow::Result<String> {
        let ScheduledTaskSpec {
            due_at,
            delay_secs,
            watch_task_id,
            title,
            body,
            mode,
            session_id,
            tool_name,
            tool_args,
            prompt,
        } = spec;
        let watch_task_id = watch_task_id
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty());
        if watch_task_id.is_some() && (due_at.is_some() || delay_secs.is_some()) {
            anyhow::bail!("watch_task_id cannot be combined with due_at or delay_secs");
        }
        // Resolve when the scheduled_task fires. Exactly one of `due_at` /
        // `delay_secs` / `watch_task_id` must be given; passing two timing
        // styles is an error (silently preferring one would hide the mistake
        // until the scheduled_task fires at the wrong time).
        let now = chrono::Utc::now();
        let (due, remaining) = if watch_task_id.is_some() {
            if self.tasks.lock().unwrap().is_none() {
                anyhow::bail!(
                    "watch_task_id requires the background-tasks registry (internal error)"
                );
            }
            (None::<chrono::DateTime<chrono::Utc>>, 0)
        } else {
            match (due_at.as_deref(), delay_secs) {
                (Some(_), Some(_)) => {
                    anyhow::bail!("use exactly one of due_at or delay_secs, not both")
                }
                (Some(due_at), None) => {
                    let parsed = chrono::DateTime::parse_from_rfc3339(due_at.trim())
                        .map_err(|_| {
                            anyhow::anyhow!(
                                "due_at must be an ISO 8601 timestamp, e.g. 2026-08-05T15:00:00+08:00 (got '{}')",
                                due_at
                            )
                        })?
                        .with_timezone(&chrono::Utc);
                    let remaining = (parsed - now).num_seconds();
                    if remaining <= 0 {
                        anyhow::bail!("due_at must be in the future");
                    }
                    if remaining > *self.max_due_horizon_secs.read().await {
                        anyhow::bail!("due_at is more than 365 days in the future");
                    }
                    (Some(parsed), remaining)
                }
                (None, Some(delay)) => {
                    if delay == 0 || delay > 86_400 {
                        anyhow::bail!("delay_secs must be between 1 and 86400");
                    }
                    (
                        Some(now + chrono::Duration::seconds(delay as i64)),
                        delay as i64,
                    )
                }
                (None, None) => {
                    anyhow::bail!("either due_at, delay_secs or watch_task_id is required")
                }
            }
        };
        let body = body.trim().to_string();
        if body.is_empty() {
            anyhow::bail!("body is required");
        }
        let title = title.trim().to_string();
        let title = if title.is_empty() {
            "Haven".to_string()
        } else {
            title
        };
        let prompt = prompt
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let tool_name = tool_name
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        if mode == ScheduleMode::Tool && tool_name.is_none() {
            anyhow::bail!("tool_name is required when mode is 'tool'");
        }

        let id = haven_common::types::new_id("act");
        let due_at_rfc = due.map(|d| d.to_rfc3339()).unwrap_or_default();
        {
            let mut scheduled_tasks = self.scheduled_tasks.write().await;
            // Reap fired entries so they never occupy the cap.
            scheduled_tasks.retain(|_, e| !e.fired);
            if scheduled_tasks.len() >= *self.max_scheduled_tasks.read().await {
                anyhow::bail!(
                    "too many pending scheduled_tasks (limit {}); cancel some first",
                    *self.max_scheduled_tasks.read().await
                );
            }
            // Persist BEFORE inserting into memory so a failed DB write
            // aborts the whole `set` with an explicit error (the scheduled_task
            // would otherwise silently exist only in memory and vanish on
            // restart — the DB is the source of truth). The write lock is
            // held across the insert, so the cap check above and the insert
            // below cannot be interleaved by a concurrent `set`.
            // Task-watch scheduled_tasks skip the DB entirely: the watched task
            // cannot survive a restart, so persisting them would just leave
            // dangling rows that restore_pending could never satisfy.
            if watch_task_id.is_none()
                && let Some(db) = self.db.read().await.as_ref()
            {
                let args_json = tool_args.as_ref().map(|v| v.to_string());
                db.save_scheduled_task(
                    &id,
                    &due_at_rfc,
                    &title,
                    &body,
                    mode.as_str(),
                    session_id.as_deref(),
                    tool_name.as_deref(),
                    args_json.as_deref(),
                    prompt.as_deref(),
                )
                .map_err(|e| anyhow::anyhow!("failed to persist scheduled_task '{}': {}", id, e))?;
            }
            scheduled_tasks.insert(
                id.clone(),
                ScheduledTaskEntry {
                    title: title.clone(),
                    body: body.clone(),
                    due_at: due_at_rfc.clone(),
                    mode,
                    session_id: session_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_args: tool_args.clone(),
                    prompt: prompt.clone(),
                    watch_task_id: watch_task_id.clone(),
                    fired: false,
                },
            );
        }
        self.emit(
            "task:created",
            serde_json::json!({
                "id": id,
                "title": title,
                "body": body,
                "mode": mode.as_str(),
                "session_id": session_id,
                "tool_name": tool_name,
                "watch_task_id": watch_task_id,
                "due_at": due_at_rfc,
            }),
        );

        let center = self.clone();
        let fired_id = id.clone();
        if let Some(task_id) = watch_task_id {
            // Condition-based wake: poll the watched task until it reaches a
            // terminal state, then fire. Decoupled from the single completion
            // channel the agent layer consumes.
            tokio::spawn(async move {
                center.watch_task_timer(fired_id, &task_id).await;
            });
        } else {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(remaining.max(0) as u64)).await;
                center.fire_entry(&fired_id).await;
            });
        }

        Ok(id)
    }

    /// Poll the watched background task until it reaches a terminal state,
    /// then fire the scheduled_task: the session is resumed with the task's result
    /// (mirroring `continue`-mode payloads). `not_found` is treated as
    /// terminal (the task finished long ago and was reaped, or never existed)
    /// so a task-watch scheduled_task can never hang forever.
    async fn watch_task_timer(self: &Arc<Self>, id: String, task_id: &str) {
        let poll = Duration::from_millis(1000);
        loop {
            tokio::time::sleep(poll).await;
            let Some(tasks) = self.tasks.lock().unwrap().clone() else {
                return;
            };
            let status = tasks.status(task_id).await;
            if status["status"].as_str() == Some("running") {
                continue;
            }
            let mut scheduled_tasks = self.scheduled_tasks.write().await;
            let Some(entry) = scheduled_tasks.get_mut(&id) else {
                return;
            };
            if entry.fired {
                return;
            }
            entry.fired = true;
            let prompt = task_finished_prompt(task_id, &status);
            self.emit_fired(&id, entry);
            let _ = self.fired_tx.send(ScheduledTaskFired {
                task_id: id.clone(),
                title: entry.title.clone(),
                body: entry.body.clone(),
                mode: entry.mode,
                session_id: entry.session_id.clone(),
                tool_name: None,
                tool_args: None,
                prompt: Some(prompt),
            });
            return;
        }
    }

    /// List pending (not yet fired) scheduled_tasks, newest first.
    pub async fn list(&self) -> Vec<Value> {
        let scheduled_tasks = self.scheduled_tasks.read().await;
        let mut rows: Vec<Value> = scheduled_tasks
            .iter()
            .filter(|(_, e)| !e.fired)
            .map(|(id, e)| {
                serde_json::json!({
                    "id": id,
                    "title": e.title,
                    "body": e.body,
                    "mode": e.mode.as_str(),
                    "session_id": e.session_id,
                    "tool_name": e.tool_name,
                    "tool_args": e.tool_args,
                    "prompt": e.prompt,
                    "watch_task_id": e.watch_task_id,
                    "due_at": e.due_at,
                })
            })
            .collect();
        rows.sort_by(|a, b| b["due_at"].as_str().cmp(&a["due_at"].as_str()));
        rows
    }

    /// Cancel a pending scheduled_task (no-op if already fired or unknown).
    pub async fn cancel(&self, id: &str) -> bool {
        let mut scheduled_tasks = self.scheduled_tasks.write().await;
        let cancelled = match scheduled_tasks.get_mut(id) {
            Some(entry) if !entry.fired => {
                entry.fired = true;
                true
            }
            _ => false,
        };
        if cancelled {
            self.emit("task:updated", serde_json::json!({ "id": id }));
            if let Some(db) = self.db.read().await.as_ref() {
                let _ = db.delete_scheduled_task(id);
            }
        }
        cancelled
    }

    /// Cancel every pending scheduled_task owned by `session_id`. Called when the
    /// session ends, is removed, or is rolled back so its scheduled_tasks cannot
    /// fire against a session that no longer exists.
    pub async fn cancel_for_session(&self, session_id: &str) {
        let mut scheduled_tasks = self.scheduled_tasks.write().await;
        let ids: Vec<String> = scheduled_tasks
            .iter()
            .filter(|(_, e)| !e.fired && e.session_id.as_deref() == Some(session_id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(entry) = scheduled_tasks.get_mut(&id) {
                entry.fired = true;
            }
            self.emit("task:updated", serde_json::json!({ "id": id }));
            if let Some(db) = self.db.read().await.as_ref() {
                let _ = db.delete_scheduled_task(&id);
            }
        }
    }
}

/// Build the continuation message for a fired task-watch scheduled_task: the task's
/// terminal status and its result payload, so the resumed session continues
/// from the actual outcome instead of a generic wake text.
fn task_finished_prompt(task_id: &str, status: &Value) -> String {
    let st = status["status"].as_str().unwrap_or("unknown");
    if st == "not_found" {
        return format!(
            "Background task {task_id} not found (it may have finished long ago and been cleaned up, or never existed)."
        );
    }
    let payload = status["output"]
        .as_str()
        .or_else(|| status["error_reason"].as_str())
        .or_else(|| status["error"].as_str())
        .unwrap_or_default();
    format!("Background task {task_id} {st}.\nOutput:\n{payload}")
}

/// Schedule in-app scheduled_tasks: set a timer that fires an action after a
/// delay, list pending ones, or cancel one. Timers run detached from the
/// ReAct loop, so the agent can schedule and continue working.
///
/// Two fire behaviors are available via `mode`:
/// - `tool` (default): call the tool in `tool_name` with `tool_args` —///   use `tool_name` `notify` with `tool_args` `{title, body}` to send a
///   message at fire time.
/// - `continue`: resume the session that scheduled the scheduled_task, delivering
///   `prompt` as the continuation instruction in the same conversation.
pub struct ScheduleTool {
    pub center: Arc<ScheduledTaskCenter>,
    /// Weak probe into the tool registry so `set` can reject unknown
    /// `tool_name` values and report the scheduled tool's risk level at
    /// schedule time. `None` in headless/test builds (checks skipped).
    pub(crate) registry: Option<RegistryProbe>,
}

#[async_trait]
impl Tool for ScheduleTool {
    fn name(&self) -> String {
        "schedule".into()
    }

    fn description(&self) -> String {
        "Schedule actions to run later, mode picks what happens when \
         it fires: tool (default) calls the tool; continue \
         resumes the current session later (only while the session is still \
         active — it is cancelled when the session ends). With watch_task_id \
         the scheduled task fires when a background task finishes instead of on a \
         timer, resuming the session with the task's result."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            // set only schedules a local timer —no system mutation.
            Some("set") => RiskLevel::Low,
            _ => RiskLevel::Safe,
        }
    }

    /// Needs the private `_session_id` input so `continue` mode knows which
    /// session to resume.
    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["set", "list", "cancel"],
                    "description": "set = schedule, list = pending, cancel = stop one"
                },
                "delay_secs": {
                    "type": "integer",
                    "description": "Delay in seconds before firing (set only; use delay_secs OR due_at OR watch_task_id, exactly one, not combined)"
                },
                "due_at": {
                    "type": "string",
                    "description": "Absolute fire time, ISO 8601 e.g. 2026-08-05T15:00:00+08:00 (set only; use due_at OR delay_secs OR watch_task_id, exactly one, not combined)"
                },
                "watch_task_id": {
                    "type": "string",
                    "description": "Set only: fire when this background task (id from shell background:true) finishes or fails, instead of on a timer. Requires mode 'continue' — the session is resumed with the task's result. Exclusive with delay_secs and due_at; in-memory only (the watched task cannot survive a restart)."
                },
                "mode": {
                    "type": "string",
                    "enum": ["tool", "continue"],
                    "description": "Action when it fires (set only): tool (default) = call tool_name with tool_args, e.g. tool_name 'notify' to send a message; continue = resume the current session with prompt as the continuation instruction (or with the task's result when watch_task_id is set; only works while that session is still active — it is cancelled when the session ends)"
                },
                "title": {
                    "type": "string",
                    "description": "Scheduled task title (defaults to 'Haven')"
                },
                "body": {
                    "type": "string",
                    "description": "Scheduled task message shown when it fires (set only)"
                },
                "tool_name": {
                    "type": "string",
                    "description": "Tool to call when it fires (set only, mode=tool), e.g. 'notify'"
                },
                "tool_args": {
                    "type": "object",
                    "description": "Arguments for the tool call (set only, mode=tool)"
                },
                "prompt": {
                    "type": "string",
                    "description": "Continuation instruction delivered to the session when it resumes (set only, mode=continue)"
                },
                "task_id": {
                    "type": "string",
                    "description": "Scheduled task id returned by set (cancel only)"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let op = input["operation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("operation is required (set, list or cancel)"))?;
        match op {
            "set" => {
                let delay = input["delay_secs"].as_i64();
                let due_at = input["due_at"].as_str();
                let watch_task_id = input["watch_task_id"].as_str().map(str::to_string);
                let watch = watch_task_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|w| !w.is_empty());
                if watch.is_some() && (delay.is_some() || due_at.is_some()) {
                    anyhow::bail!("watch_task_id cannot be combined with delay_secs or due_at");
                }
                if delay.is_none() && due_at.is_none() && watch.is_none() {
                    anyhow::bail!("one of delay_secs, due_at or watch_task_id is required for set");
                }
                let title = input["title"].as_str().unwrap_or("Haven");
                let body = input["body"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("body is required for set"))?;
                let mode = match input["mode"].as_str().unwrap_or("tool") {
                    "tool" => ScheduleMode::Tool,
                    "continue" => ScheduleMode::Continue,
                    other => {
                        anyhow::bail!(
                            "unknown scheduled_task mode: {other} (expected tool or continue)"
                        )
                    }
                };
                if watch.is_some() && mode != ScheduleMode::Continue {
                    anyhow::bail!(
                        "watch_task_id requires mode 'continue' (the scheduled_task fires by resuming the session with the task result)"
                    );
                }
                // `_session_id` is injected privately by ToolsManager::execute_tool
                // (never part of the LLM-visible schema or step history) so the
                // scheduled_task knows which session to resume in continue mode.
                let session_id = input["_session_id"].as_str().map(str::to_string);
                if mode == ScheduleMode::Continue && session_id.is_none() {
                    anyhow::bail!(
                        "continue mode requires an active session to resume (internal error)"
                    );
                }
                let tool_name = input["tool_name"].as_str().map(str::to_string);
                let tool_args = input.get("tool_args").filter(|v| !v.is_null()).cloned();
                // Eager existence check at schedule time: a typo'd tool name
                // would otherwise fail only at fire time (in a detached timer,
                // hours later, with no LLM to recover). Per-session skill/MCP
                // adapters are not in the global registry and cannot be
                // scheduled as fire-time calls. Skipped in headless/test
                // builds where no registry is wired.
                let risk_level = if mode == ScheduleMode::Tool {
                    match &self.registry {
                        Some(probe) => {
                            let Some(tool_name) = tool_name.as_deref() else {
                                anyhow::bail!("tool_name is required when mode is 'tool'");
                            };
                            let Some(tool) = probe.find(tool_name).await else {
                                anyhow::bail!(
                                    "tool '{}' is not a registered tool; schedule a builtin tool call instead (per-session skill/MCP tools cannot be scheduled for fire time)",
                                    tool_name
                                );
                            };
                            Some(tool.risk_level(tool_args.as_ref().unwrap_or(&Value::Null)))
                        }
                        None => None,
                    }
                } else {
                    None
                };
                let prompt = input["prompt"].as_str();
                let id = self
                    .center
                    .set(ScheduledTaskSpec {
                        due_at: due_at.map(str::to_string),
                        delay_secs: delay.map(|d| d as u64),
                        watch_task_id: watch_task_id.clone(),
                        title: title.to_string(),
                        body: body.to_string(),
                        mode,
                        session_id,
                        tool_name,
                        tool_args,
                        prompt: prompt.map(str::to_string),
                    })
                    .await?;
                let fires_at = if watch.is_some() {
                    String::new()
                } else {
                    due_at
                        .and_then(|d| chrono::DateTime::parse_from_rfc3339(d.trim()).ok())
                        .map(|d| d.to_rfc3339())
                        .unwrap_or_else(|| {
                            (chrono::Utc::now() + chrono::Duration::seconds(delay.unwrap_or(0)))
                                .to_rfc3339()
                        })
                };
                let mut output = serde_json::json!({
                    "id": id,
                    "mode": mode.as_str(),
                    "fires_at": fires_at,
                    "wakes_session": mode == ScheduleMode::Continue,
                    "note": "The scheduled_task fires while the app is running; overdue ones fire on next startup.",
                });
                if let Some(task_id) = &watch_task_id {
                    output["watch_task_id"] = serde_json::json!(task_id);
                    output["note"] = serde_json::json!(format!(
                        "Fires when background task {task_id} finishes or fails, resuming this session with the task's result. Task-watch scheduled_tasks are in-memory only (the watched task cannot survive a restart)."
                    ));
                }
                if let Some(risk) = risk_level {
                    output["risk_level"] = serde_json::json!(risk);
                    if risk >= RiskLevel::Medium {
                        output["may_require_confirmation"] = serde_json::json!(true);
                        if let Some(note) = output["note"].as_str() {
                            output["note"] = serde_json::json!(format!(
                                "{} The scheduled tool may require user confirmation when it fires; if nobody confirms in time, the call is skipped.",
                                note
                            ));
                        }
                    }
                }
                Ok(ToolResult::ok(output))
            }
            "list" => {
                let rows = self.center.list().await;
                Ok(ToolResult::ok(
                    serde_json::json!({ "scheduled_tasks": rows }),
                ))
            }
            "cancel" => {
                let id = input["task_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("task_id is required for cancel"))?;
                if self.center.cancel(id).await {
                    Ok(ToolResult::ok(serde_json::json!({ "cancelled": id })))
                } else {
                    anyhow::bail!("scheduled_task '{}' not found or already fired", id)
                }
            }
            _ => anyhow::bail!("unknown scheduled_task operation: {}", op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolRegistry};
    use serde_json::json;

    fn make_tool() -> ScheduleTool {
        ScheduleTool {
            center: Arc::new(ScheduledTaskCenter::new()),
            registry: None,
        }
    }

    fn tool_spec(delay: u64, title: &str, body: &str) -> ScheduledTaskSpec {
        ScheduledTaskSpec {
            due_at: None,
            delay_secs: Some(delay),
            watch_task_id: None,
            title: title.into(),
            body: body.into(),
            mode: ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        }
    }

    #[test]
    fn test_reminder_name() {
        assert_eq!(make_tool().name(), "schedule");
    }

    #[test]
    fn test_reminder_risk_levels() {
        let tool = make_tool();
        assert_eq!(
            tool.risk_level(&json!({"operation": "set"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "list"})),
            RiskLevel::Safe
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "cancel"})),
            RiskLevel::Safe
        );
    }

    #[test]
    fn test_reminder_mode_roundtrip() {
        assert_eq!(ScheduleMode::parse("tool"), Some(ScheduleMode::Tool));
        assert_eq!(
            ScheduleMode::parse("continue"),
            Some(ScheduleMode::Continue)
        );
        assert_eq!(ScheduleMode::parse("notify"), None);
        assert_eq!(ScheduleMode::parse("bogus"), None);
        assert_eq!(ScheduleMode::default(), ScheduleMode::Tool);
        assert_eq!(ScheduleMode::Tool.as_str(), "tool");
        assert_eq!(ScheduleMode::Continue.as_str(), "continue");
    }

    #[tokio::test]
    async fn test_set_validates_input() {
        let tool = make_tool();
        // Missing body.
        let err = tool
            .execute(
                json!({"operation": "set", "delay_secs": 5}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // Zero delay.
        let err = tool
            .execute(
                json!({"operation": "set", "delay_secs": 0, "body": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // Oversized delay.
        let err = tool
            .execute(
                json!({"operation": "set", "delay_secs": 999999, "body": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // Neither delay nor due_at.
        let err = tool
            .execute(
                json!({"operation": "set", "body": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // Malformed due_at.
        let err = tool
            .execute(
                json!({"operation": "set", "due_at": "tomorrow-ish", "body": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // due_at in the past.
        let err = tool
            .execute(
                json!({"operation": "set", "due_at": "2020-01-01T00:00:00+08:00", "body": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_set_mode_validation() {
        let tool = make_tool();
        // Unknown mode.
        let err = tool
            .execute(
                json!({"operation": "set", "delay_secs": 60, "body": "x", "mode": "bogus"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // tool mode without tool_name.
        let err = tool
            .execute(
                json!({"operation": "set", "delay_secs": 60, "body": "x", "mode": "tool"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // continue mode without the injected session id.
        let err = tool
            .execute(
                json!({"operation": "set", "delay_secs": 60, "body": "x", "mode": "continue"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        // Valid tool mode passes.
        let result = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 3600,
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "file",
                    "tool_args": {"operation": "read", "path": "C:/x"}
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_set_rejects_both_delay_and_due_at() {
        let tool = make_tool();
        let err = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 60,
                    "due_at": (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339(),
                    "body": "x"
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("exactly one"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn test_center_set_rejects_both_delay_and_due_at() {
        let center = Arc::new(ScheduledTaskCenter::new());
        let err = center
            .set(ScheduledTaskSpec {
                due_at: Some((chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339()),
                delay_secs: Some(60),
                watch_task_id: None,
                title: "T".into(),
                body: "B".into(),
                mode: ScheduleMode::Tool,
                session_id: None,
                tool_name: Some("notify".into()),
                tool_args: None,
                prompt: None,
            })
            .await;
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("exactly one"), "unexpected error: {msg}");
    }

    #[tokio::test]
    async fn test_set_rejects_watch_with_delay_or_due_at() {
        let tool = make_tool();
        let err = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 60,
                    "watch_task_id": "task-1",
                    "body": "x",
                    "mode": "continue"
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("cannot be combined"),
            "unexpected error: {msg}"
        );
        let err = tool
            .execute(
                json!({
                    "operation": "set",
                    "due_at": (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339(),
                    "watch_task_id": "task-1",
                    "body": "x",
                    "mode": "continue"
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_set_watch_requires_continue_mode() {
        let tool = make_tool();
        let err = tool
            .execute(
                json!({
                    "operation": "set",
                    "watch_task_id": "task-1",
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "notify"
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("requires mode 'continue'"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn test_watch_requires_jobs_registry() {
        // No tasks registry wired (headless/test build): watch scheduled_tasks fail
        // fast instead of never firing.
        let center = Arc::new(ScheduledTaskCenter::new());
        let err = center
            .set(ScheduledTaskSpec {
                due_at: None,
                delay_secs: None,
                watch_task_id: Some("task-1".into()),
                title: "T".into(),
                body: "B".into(),
                mode: ScheduleMode::Continue,
                session_id: Some("ses-1".into()),
                tool_name: None,
                tool_args: None,
                prompt: None,
            })
            .await;
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("requires the background-tasks registry"),
            "unexpected error: {msg}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_watch_task_fires_with_result_when_task_finishes() {
        use crate::bg::BackgroundTasks;
        let tasks = Arc::new(BackgroundTasks::new());
        let center = Arc::new(ScheduledTaskCenter::new());
        center.set_tasks(Some(tasks.clone()));
        let mut rx = center.take_fired_receiver().expect("receiver available");

        let task_id = tasks
            .spawn_shell("echo task-watch-result", "cmd", 20_000, None)
            .await
            .unwrap();
        let id = center
            .set(ScheduledTaskSpec {
                due_at: None,
                delay_secs: None,
                watch_task_id: Some(task_id.clone()),
                title: "Watch".into(),
                body: "task done".into(),
                mode: ScheduleMode::Continue,
                session_id: Some("ses-1".into()),
                tool_name: None,
                tool_args: None,
                prompt: None,
            })
            .await
            .unwrap();

        // Fires once the task completes, resuming the session with the result.
        let fired = tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .expect("timed out waiting for task-watch fire")
            .expect("channel closed");
        assert_eq!(fired.task_id, id);
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        let prompt = fired.prompt.expect("watch fire must carry the result");
        assert!(
            prompt.contains("task-watch-result"),
            "prompt must carry the task output: {prompt}"
        );
        assert!(
            prompt.contains("completed"),
            "prompt must carry the status: {prompt}"
        );
        // Not persisted (in-memory only).
        assert!(center.list().await.is_empty());
    }

    #[tokio::test]
    async fn test_watch_task_not_persisted_to_db() {
        let (db, _dir) = test_db();
        let tasks = Arc::new(crate::bg::BackgroundTasks::new());
        let center = Arc::new(ScheduledTaskCenter::new());
        center.set_db(Some(db.clone())).await;
        center.set_tasks(Some(tasks));
        center
            .set(ScheduledTaskSpec {
                due_at: None,
                delay_secs: None,
                watch_task_id: Some("task-nope".into()),
                title: "T".into(),
                body: "B".into(),
                mode: ScheduleMode::Continue,
                session_id: Some("ses-1".into()),
                tool_name: None,
                tool_args: None,
                prompt: None,
            })
            .await
            .unwrap();
        // The watched task cannot survive a restart: nothing in the DB.
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());
        // Listed in memory (with the watched task id).
        let rows = center.list().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["watch_task_id"], json!("task-nope"));
    }
    /// Minimal tool stub for registry-backed validation tests.
    struct DummyTool {
        name: &'static str,
        risk: RiskLevel,
    }

    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> String {
            self.name.into()
        }
        fn description(&self) -> String {
            "dummy test tool".into()
        }
        fn risk_level(&self, _input: &Value) -> RiskLevel {
            self.risk
        }
        fn input_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<ToolResult> {
            Ok(ToolResult::ok(serde_json::json!({})))
        }
    }

    #[tokio::test]
    async fn test_set_rejects_unknown_tool_name() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(DummyTool {
                name: "notify",
                risk: RiskLevel::Safe,
            }))
            .await;
        let tool = ScheduleTool {
            center: Arc::new(ScheduledTaskCenter::new()),
            registry: Some(registry.probe()),
        };
        // A typo'd tool name fails at schedule time instead of at fire time.
        let err = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 60,
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "notiy"
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(
            msg.contains("not a registered tool"),
            "unexpected error: {msg}"
        );
        // A known tool passes.
        let ok = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 60,
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "notify",
                    "tool_args": {"title": "T", "body": "B"}
                }),
                CancellationToken::new(),
            )
            .await;
        assert!(ok.is_ok());
    }

    #[tokio::test]
    async fn test_set_reports_risk_and_confirmation_flag() {
        let registry = ToolRegistry::new();
        registry
            .register(Arc::new(DummyTool {
                name: "shell",
                risk: RiskLevel::High,
            }))
            .await;
        registry
            .register(Arc::new(DummyTool {
                name: "notify",
                risk: RiskLevel::Safe,
            }))
            .await;
        let tool = ScheduleTool {
            center: Arc::new(ScheduledTaskCenter::new()),
            registry: Some(registry.probe()),
        };
        // High-risk scheduled tool: flagged so the user knows the fire-time
        // call may be skipped when nobody confirms.
        let res = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 60,
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "shell",
                    "tool_args": {}
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(res.output["risk_level"], json!("high"));
        assert_eq!(res.output["may_require_confirmation"], json!(true));
        // Safe tool: no flag.
        let res = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 60,
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "notify",
                    "tool_args": {}
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(res.output["risk_level"], json!("safe"));
        assert!(res.output.get("may_require_confirmation").is_none());
    }

    #[tokio::test]
    async fn test_set_with_due_at_and_prompt() {
        let (db, _dir) = test_db();
        let center = Arc::new(ScheduledTaskCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // Absolute time 2s out, continue mode with a wake prompt.
        let due = (chrono::Utc::now() + chrono::Duration::seconds(2)).to_rfc3339();
        let id = center
            .set(ScheduledTaskSpec {
                due_at: Some(due),
                delay_secs: None,
                watch_task_id: None,
                title: "Wake".into(),
                body: "body text".into(),
                mode: ScheduleMode::Continue,
                session_id: Some("ses-1".into()),
                tool_name: None,
                tool_args: None,
                prompt: Some("check the weather".into()),
            })
            .await
            .unwrap();

        // Persisted with mode + session + prompt.
        let pending = db.list_pending_scheduled_tasks().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].session_id.as_deref(), Some("ses-1"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for scheduled_task")
            .expect("channel closed");
        assert_eq!(fired.task_id, id);
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        assert_eq!(fired.prompt.as_deref(), Some("check the weather"));
        assert_eq!(fired.title, "Wake");
        assert_eq!(fired.body, "body text");
    }

    #[tokio::test]
    async fn test_set_tool_mode_records_call_and_fires() {
        let (db, _dir) = test_db();
        let center = Arc::new(ScheduledTaskCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        let id = center
            .set(ScheduledTaskSpec {
                due_at: None,
                delay_secs: Some(1),
                watch_task_id: None,
                title: "Backup".into(),
                body: "running backup".into(),
                mode: ScheduleMode::Tool,
                session_id: Some("ses-1".into()),
                tool_name: Some("file".into()),
                tool_args: Some(json!({"operation": "read", "path": "C:/x"})),
                prompt: None,
            })
            .await
            .unwrap();

        // Persisted with the tool payload.
        let pending = db.list_pending_scheduled_tasks().unwrap();
        assert_eq!(pending[0].mode, "tool");
        assert_eq!(pending[0].tool_name.as_deref(), Some("file"));
        assert!(pending[0].tool_args.as_deref().unwrap().contains("C:/x"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for scheduled_task")
            .expect("channel closed");
        assert_eq!(fired.task_id, id);
        assert_eq!(fired.mode, ScheduleMode::Tool);
        assert_eq!(fired.tool_name.as_deref(), Some("file"));
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        assert_eq!(fired.tool_args.as_ref().unwrap()["path"], "C:/x");
    }

    #[tokio::test]
    async fn test_set_continue_wakes_session_flag_in_output() {
        let tool = make_tool();
        let result = tool
            .execute(
                json!({
                    "operation": "set",
                    "due_at": (chrono::Utc::now() + chrono::Duration::seconds(3600)).to_rfc3339(),
                    "body": "x",
                    "mode": "continue",
                    "prompt": "summarize my notes",
                    "_session_id": "ses-9"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["mode"], json!("continue"));
        assert_eq!(result.output["wakes_session"], json!(true));

        let plain = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 3600,
                    "body": "x",
                    "mode": "tool",
                    "tool_name": "notify"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(plain.output["mode"], json!("tool"));
        assert_eq!(plain.output["wakes_session"], json!(false));
    }

    #[tokio::test]
    async fn test_set_list_cancel_flow() {
        let tool = make_tool();
        let result = tool
            .execute(
                json!({
                    "operation": "set",
                    "delay_secs": 3600,
                    "title": "Drink",
                    "body": "water",
                    "mode": "tool",
                    "tool_name": "notify"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let id = result.output["id"].as_str().unwrap().to_string();

        let list = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(list.output["scheduled_tasks"].as_array().unwrap().len(), 1);
        assert_eq!(list.output["scheduled_tasks"][0]["id"], json!(id));
        assert_eq!(list.output["scheduled_tasks"][0]["body"], json!("water"));
        assert_eq!(list.output["scheduled_tasks"][0]["mode"], json!("tool"));

        let cancelled = tool
            .execute(
                json!({"operation": "cancel", "task_id": id}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.output["cancelled"], json!(id));

        // Cancelling again fails.
        let err = tool
            .execute(
                json!({"operation": "cancel", "task_id": id}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());

        // List is empty after cancel.
        let list = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(
            list.output["scheduled_tasks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_reminder_fires_and_delivers() {
        let center = Arc::new(ScheduledTaskCenter::new());
        let mut rx = center.take_fired_receiver().expect("receiver available");
        let tool = ScheduleTool {
            center: center.clone(),
            registry: None,
        };
        let id = center.set(tool_spec(1, "Test", "fire now")).await.unwrap();
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for scheduled_task")
            .expect("channel closed");
        assert_eq!(fired.task_id, id);
        assert_eq!(fired.mode, ScheduleMode::Tool);
        assert_eq!(fired.tool_name.as_deref(), Some("notify"));
        assert_eq!(fired.title, "Test");
        assert_eq!(fired.body, "fire now");

        // Fired scheduled_tasks are reaped by the next set (cap stays clean).
        center
            .set(tool_spec(3600, "Next", "still pending"))
            .await
            .unwrap();
        assert_eq!(center.list().await.len(), 1);
        let _ = tool;
    }

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
        (db, dir)
    }

    #[tokio::test]
    async fn test_restore_pending_rearms_and_fires_overdue() {
        let (db, _dir) = test_db();
        let center = Arc::new(ScheduledTaskCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // A future scheduled_task (5s out) and an overdue one (already past).
        let future_id = center.set(tool_spec(5, "Future", "later")).await.unwrap();
        let overdue_id = format!("task-{}", uuid::Uuid::new_v4().simple());
        db.save_scheduled_task(
            &overdue_id,
            &(chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339(),
            "Overdue",
            "should fire now",
            "continue",
            Some("ses-1"),
            None,
            None,
            Some("keep going"),
        )
        .unwrap();

        // A fresh center (simulating app restart) restores from the DB.
        let restored = Arc::new(ScheduledTaskCenter::new());
        restored.set_db(Some(db.clone())).await;
        let mut rx2 = restored.take_fired_receiver().expect("receiver available");
        let overdue_count = restored.restore_pending().await;
        assert_eq!(overdue_count, 1, "exactly one scheduled_task was overdue");

        // Overdue scheduled_task fired immediately with its mode payload.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
            .await
            .expect("timed out waiting for overdue fire")
            .expect("channel closed");
        assert_eq!(fired.task_id, overdue_id);
        assert_eq!(fired.title, "Overdue");
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        assert_eq!(fired.prompt.as_deref(), Some("keep going"));

        // Future scheduled_task re-armed and fires after its remaining delay.
        let fired = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for future fire")
            .expect("channel closed");
        assert_eq!(fired.task_id, future_id);

        // Both are marked fired in the DB; pending list is empty.
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_reminder_set_persists_to_db() {
        let (db, _dir) = test_db();
        let center = Arc::new(ScheduledTaskCenter::new());
        center.set_db(Some(db.clone())).await;
        let id = center.set(tool_spec(3600, "Drink", "water")).await.unwrap();
        let pending = db.list_pending_scheduled_tasks().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].body, "water");
        assert_eq!(pending[0].mode, "tool");

        // Cancel removes the row.
        assert!(center.cancel(&id).await);
        assert!(db.list_pending_scheduled_tasks().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_unknown_operation_rejected() {
        let tool = make_tool();
        let err = tool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_event_sink_receives_set_fire_cancel() {
        let center = Arc::new(ScheduledTaskCenter::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        center.set_event_sink(Arc::new(move |name, payload| {
            sink_events.lock().unwrap().push((name, payload));
        }));
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // set -> task:created event with the payload.
        let id = center.set(tool_spec(1, "Evt", "fire me")).await.unwrap();
        {
            let evs = events.lock().unwrap();
            let set_evt = evs
                .iter()
                .find(|(n, _)| n == "task:created")
                .expect("task:created emitted");
            assert_eq!(set_evt.1["id"], id);
            assert_eq!(set_evt.1["body"], "fire me");
            assert!(set_evt.1["due_at"].as_str().is_some());
        }

        // Fire -> task:finished event.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for fire")
            .expect("channel closed");
        assert_eq!(fired.task_id, id);
        {
            let evs = events.lock().unwrap();
            let fired_evt = evs
                .iter()
                .find(|(n, _)| n == "task:finished")
                .expect("task:finished emitted");
            assert_eq!(fired_evt.1["id"], id);
            assert_eq!(fired_evt.1["mode"], "tool");
        }

        // cancel -> task:updated event.
        let id2 = center
            .set(tool_spec(3600, "Keep", "pending"))
            .await
            .unwrap();
        assert!(center.cancel(&id2).await);
        {
            let evs = events.lock().unwrap();
            let cancel_evt = evs
                .iter()
                .find(|(n, _)| n == "task:updated")
                .expect("task:updated emitted");
            assert_eq!(cancel_evt.1["id"], id2);
        }
    }
}
