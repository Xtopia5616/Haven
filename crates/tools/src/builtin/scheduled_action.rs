use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::bg::{BackgroundActions, EventSink, EventSinkState};
use crate::tool::RegistryProbe;
use crate::{Tool, ToolResult};

/// What happens when a scheduled_action fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleMode {
    /// Call the tool in `tool_name` with `tool_args` (no LLM involved).
    /// To send a message at fire time, call the `notify` tool here.
    #[default]
    Tool,
    /// Resume the session that scheduled the scheduled_action: the session continues with
    /// the scheduled_action text as a new instruction in the same conversation.
    /// Only works while the scheduling session is still alive (running/paused):
    /// scheduled_actions are cancelled automatically when their session ends or is
    /// removed, so a `continue` scheduled_action cannot resurrect a completed session.
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

/// A scheduled_action that fired; delivered to the app layer so it can run a tool
/// (`Tool`, including `notify` for a message) or resume the scheduling session
/// (`Continue`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledActionFired {
    pub action_id: String,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    /// Session that scheduled the scheduled_action —resume target for `Continue` mode,
    /// tool-context scope for `Tool` mode. `None` on legacy rows.
    pub session_id: Option<String>,
    /// `Tool` mode: tool to call when the scheduled_action fires.
    pub tool_name: Option<String>,
    /// `Tool` mode: arguments for the tool call.
    pub tool_args: Option<Value>,
    /// `Continue` mode: continuation message delivered to the session (falls
    /// back to `body`). On legacy rows it is the wake text for a new session.
    pub prompt: Option<String>,
}

/// Everything needed to schedule one scheduled_action.
pub struct ScheduledActionSpec {
    /// Absolute fire time (RFC3339, local time accepted). Use this OR
    /// `delay_secs` OR `watch_action_id`; exactly one is required.
    pub due_at: Option<String>,
    /// Delay in seconds before the scheduled_action fires. Use this OR `due_at` OR
    /// `watch_action_id`.
    pub delay_secs: Option<u64>,
    /// Background action to wait for: the scheduled_action fires when the action reaches a
    /// terminal state (completed/failed/cancelled) instead of on a timer,
    /// resuming the session with the action's result. Use this OR `due_at` OR
    /// `delay_secs`; in-memory only (the watched action cannot survive a
    /// restart, so these scheduled_actions are not persisted).
    pub watch_action_id: Option<String>,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    /// The session that schedules the scheduled_action (injected by the tool manager,
    /// not visible to the LLM). Resume target for `Continue`.
    pub session_id: Option<String>,
    /// `Tool` mode: tool to call when the scheduled_action fires.
    pub tool_name: Option<String>,
    /// `Tool` mode: arguments for the tool call.
    pub tool_args: Option<Value>,
    /// `Continue` mode: continuation message delivered to the session.
    pub prompt: Option<String>,
}

/// Lifetime cap on scheduled_actions per process. Fired scheduled_actions are reaped on the
/// next `set`, so this bounds concurrent pending timers, not history.
/// Upper bound on a `due_at`-scheduled scheduled_action (365 days) — guards against
/// typos like a swapped year. Delay-based scheduled_actions are capped separately.
#[derive(Clone)]
struct ScheduledActionEntry {
    title: String,
    body: String,
    due_at: String,
    mode: ScheduleMode,
    session_id: Option<String>,
    tool_name: Option<String>,
    tool_args: Option<Value>,
    prompt: Option<String>,
    /// Background action this scheduled_action waits for (empty for timer-based
    /// scheduled_actions). Fires when the action reaches a terminal state.
    watch_action_id: Option<String>,
    fired: bool,
}

/// Registry of in-process timers for the `scheduled_action` tool, with a persistent
/// backing store: every `set` is written to the database so scheduled_actions
/// survive app restarts. On startup the agent layer calls `restore_pending`:
/// overdue scheduled_actions fire immediately (missed while the app was off), the
/// rest are re-armed with their remaining delay. The in-memory timer is the
/// delivery mechanism while the app runs; the DB is the source of truth.
pub struct ScheduledActionCenter {
    scheduled_actions: RwLock<HashMap<String, ScheduledActionEntry>>,
    fired_tx: tokio::sync::mpsc::UnboundedSender<ScheduledActionFired>,
    fired_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ScheduledActionFired>>>,
    /// Persistent store; `None` in headless/test builds (in-memory only).
    db: RwLock<Option<Arc<Database>>>,
    /// Lifetime cap on pending scheduled_actions (from context limits).
    max_scheduled_actions: RwLock<usize>,
    max_due_horizon_secs: RwLock<i64>,
    /// Optional UI event sink (see `EventSink`). Wired by the desktop shell
    /// to forward lifecycle events as Tauri events.
    event_sink: EventSinkState,
    /// Background-action registry for `watch_action_id` scheduled_actions (polled for a
    /// terminal state). Wired by the tools manager; `None` in headless/test
    /// builds where action-watch scheduled_actions are rejected.
    actions: Mutex<Option<Arc<BackgroundActions>>>,
}

impl Default for ScheduledActionCenter {
    fn default() -> Self {
        Self::new()
    }
}

impl ScheduledActionCenter {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            scheduled_actions: RwLock::new(HashMap::new()),
            fired_tx: tx,
            fired_rx: Mutex::new(Some(rx)),
            db: RwLock::new(None),
            max_scheduled_actions: RwLock::new(32),
            max_due_horizon_secs: RwLock::new(365 * 24 * 3600),
            event_sink: EventSinkState::default(),
            actions: Mutex::new(None),
        }
    }

    /// Attach the background-action registry so `watch_action_id` scheduled_actions can
    /// wait for a action to finish. Wired once by the tools manager.
    pub fn set_actions(&self, actions: Option<Arc<BackgroundActions>>) {
        *self.actions.lock().unwrap() = actions;
    }

    /// Install the UI event sink (called once by the desktop shell).
    pub fn set_event_sink(&self, sink: EventSink) {
        self.event_sink.set(sink);
    }

    /// Forward a lifecycle event to the installed sink (no-op without one).
    fn emit(&self, event: &str, payload: Value) {
        self.event_sink.emit(event, payload);
    }

    /// Emit the `action:finished` event for a scheduled_action (delivered alongside
    /// the `ScheduledActionFired` channel message so the UI can drop it from the
    /// pending list and surface its own toast).
    fn emit_fired(&self, id: &str, entry: &ScheduledActionEntry) {
        self.emit(
            "action:finished",
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

    /// Replace the unified context limits (scheduled_action caps).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_scheduled_actions.write().await = limits.scheduled_actions_max;
        *self.max_due_horizon_secs.write().await = limits.scheduled_actions_due_horizon_secs;
    }

    /// Attach the database used for persistence. Wired by the desktop shell
    /// (same handle the `self` tool receives); headless tests skip it.
    pub async fn set_db(&self, db: Option<Arc<Database>>) {
        *self.db.write().await = db;
    }

    /// Take the fired-scheduled_action receiver exactly once (consumed by the agent
    /// layer, which emits Notification events). Returns `None` if already
    /// taken.
    pub fn take_fired_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<ScheduledActionFired>> {
        self.fired_rx.lock().unwrap().take()
    }

    /// Mark a scheduled_action fired, emit its `action:finished` event, deliver the
    /// `ScheduledActionFired` payload, and persist the fired flag. Shared by the
    /// overdue re-arm path (`restore_pending`) and the action-watch timer.
    async fn fire_entry(self: &Arc<Self>, id: &str) {
        let mut scheduled_actions = self.scheduled_actions.write().await;
        if let Some(entry) = scheduled_actions.get_mut(id)
            && !entry.fired
        {
            entry.fired = true;
            self.emit_fired(id, entry);
            let _ = self.fired_tx.send(ScheduledActionFired {
                action_id: id.to_string(),
                title: entry.title.clone(),
                body: entry.body.clone(),
                mode: entry.mode,
                session_id: entry.session_id.clone(),
                tool_name: entry.tool_name.clone(),
                tool_args: entry.tool_args.clone(),
                prompt: entry.prompt.clone(),
            });
            if let Some(db) = self.db.read().await.as_ref() {
                let _ = db.mark_scheduled_action_fired(id);
            }
        }
    }

    /// Re-arm all pending scheduled_actions from the database after a restart.
    ///
    /// - ScheduledActions whose due time already passed (the app was off when they
    ///   expired) fire immediately and are marked fired.
    /// - Future scheduled_actions are re-armed in memory with their remaining delay.
    ///
    /// Returns the number of scheduled_actions fired as overdue. Called once from the
    /// agent layer startup; safe to call again (idempotent —in-memory
    /// entries are skipped).
    pub async fn restore_pending(self: &Arc<Self>) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        let Ok(rows) = db.list_pending_scheduled_actions() else {
            return 0;
        };
        let now = chrono::Utc::now();
        let mut overdue = 0usize;
        for row in rows {
            // Idempotency: skip entries the in-memory map already holds
            // (restore was already run, or the scheduled_action was re-set live).
            if self.scheduled_actions.read().await.contains_key(&row.id) {
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
            let fired_payload = ScheduledActionFired {
                action_id: row.id.clone(),
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
                let entry = ScheduledActionEntry {
                    title: row.title.clone(),
                    body: row.body.clone(),
                    due_at: row.due_at.clone(),
                    mode,
                    session_id: row.session_id.clone(),
                    tool_name: row.tool_name.clone(),
                    tool_args: tool_args.clone(),
                    prompt: row.prompt.clone(),
                    watch_action_id: None,
                    fired: true,
                };
                self.scheduled_actions
                    .write()
                    .await
                    .insert(row.id.clone(), entry.clone());
                self.emit_fired(&row.id, &entry);
                let _ = self.fired_tx.send(fired_payload);
                let _ = db.mark_scheduled_action_fired(&row.id);
                overdue += 1;
            } else {
                let center = self.clone();
                let id = row.id.clone();
                self.scheduled_actions.write().await.insert(
                    id.clone(),
                    ScheduledActionEntry {
                        title: row.title.clone(),
                        body: row.body.clone(),
                        due_at: row.due_at.clone(),
                        mode,
                        session_id: row.session_id.clone(),
                        tool_name: row.tool_name.clone(),
                        tool_args: tool_args.clone(),
                        prompt: row.prompt.clone(),
                        watch_action_id: None,
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

    /// Schedule a scheduled_action to fire at an absolute time, after a delay, or
    /// when a background action reaches a terminal state. Exactly one of
    /// `spec.due_at` (RFC3339, local time accepted), `spec.delay_secs` or
    /// `spec.watch_action_id` must be given. `spec.mode` selects what happens
    /// at fire time (see [`ScheduleMode`]).
    ///
    /// Returns the scheduled_action id; the timer (or action watcher) runs detached from
    /// the ReAct loop and delivers a `ScheduledActionFired` on the channel when it
    /// expires. Action-watch scheduled_actions are in-memory only: the watched action
    /// cannot survive a restart, so they are not persisted.
    pub async fn set(self: &Arc<Self>, spec: ScheduledActionSpec) -> anyhow::Result<String> {
        let ScheduledActionSpec {
            due_at,
            delay_secs,
            watch_action_id,
            title,
            body,
            mode,
            session_id,
            tool_name,
            tool_args,
            prompt,
        } = spec;
        let watch_action_id = watch_action_id
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty());
        if watch_action_id.is_some() && (due_at.is_some() || delay_secs.is_some()) {
            anyhow::bail!("watch_action_id cannot be combined with due_at or delay_secs");
        }
        // Resolve when the scheduled_action fires. Exactly one of `due_at` /
        // `delay_secs` / `watch_action_id` must be given; passing two timing
        // styles is an error (silently preferring one would hide the mistake
        // until the scheduled_action fires at the wrong time).
        let now = chrono::Utc::now();
        let (due, remaining) = if watch_action_id.is_some() {
            if self.actions.lock().unwrap().is_none() {
                anyhow::bail!(
                    "watch_action_id requires the background-actions registry (internal error)"
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
                    anyhow::bail!("either due_at, delay_secs or watch_action_id is required")
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
            let mut scheduled_actions = self.scheduled_actions.write().await;
            // Reap fired entries so they never occupy the cap.
            scheduled_actions.retain(|_, e| !e.fired);
            if scheduled_actions.len() >= *self.max_scheduled_actions.read().await {
                anyhow::bail!(
                    "too many pending scheduled_actions (limit {}); cancel some first",
                    *self.max_scheduled_actions.read().await
                );
            }
            // Persist BEFORE inserting into memory so a failed DB write
            // aborts the whole `set` with an explicit error (the scheduled_action
            // would otherwise silently exist only in memory and vanish on
            // restart — the DB is the source of truth). The write lock is
            // held across the insert, so the cap check above and the insert
            // below cannot be interleaved by a concurrent `set`.
            // Action-watch scheduled_actions skip the DB entirely: the watched action
            // cannot survive a restart, so persisting them would just leave
            // dangling rows that restore_pending could never satisfy.
            if watch_action_id.is_none()
                && let Some(db) = self.db.read().await.as_ref()
            {
                let args_json = tool_args.as_ref().map(|v| v.to_string());
                db.save_scheduled_action(
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
                .map_err(|e| {
                    anyhow::anyhow!("failed to persist scheduled_action '{}': {}", id, e)
                })?;
            }
            scheduled_actions.insert(
                id.clone(),
                ScheduledActionEntry {
                    title: title.clone(),
                    body: body.clone(),
                    due_at: due_at_rfc.clone(),
                    mode,
                    session_id: session_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_args: tool_args.clone(),
                    prompt: prompt.clone(),
                    watch_action_id: watch_action_id.clone(),
                    fired: false,
                },
            );
        }
        self.emit(
            "action:created",
            serde_json::json!({
                "id": id,
                "title": title,
                "body": body,
                "mode": mode.as_str(),
                "session_id": session_id,
                "tool_name": tool_name,
                "watch_action_id": watch_action_id,
                "due_at": due_at_rfc,
            }),
        );

        let center = self.clone();
        let fired_id = id.clone();
        if let Some(action_id) = watch_action_id {
            // Condition-based wake: poll the watched action until it reaches a
            // terminal state, then fire. Decoupled from the single completion
            // channel the agent layer consumes.
            tokio::spawn(async move {
                center.watch_action_timer(fired_id, &action_id).await;
            });
        } else {
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(remaining.max(0) as u64)).await;
                center.fire_entry(&fired_id).await;
            });
        }

        Ok(id)
    }

    /// Poll the watched background action until it reaches a terminal state,
    /// then fire the scheduled_action: the session is resumed with the action's result
    /// (mirroring `continue`-mode payloads). `not_found` is treated as
    /// terminal (the action finished long ago and was reaped, or never existed)
    /// so a action-watch scheduled_action can never hang forever.
    async fn watch_action_timer(self: &Arc<Self>, id: String, action_id: &str) {
        let poll = Duration::from_millis(1000);
        loop {
            tokio::time::sleep(poll).await;
            let Some(actions) = self.actions.lock().unwrap().clone() else {
                return;
            };
            let status = actions.status(action_id).await;
            if status["status"].as_str() == Some("running") {
                continue;
            }
            let mut scheduled_actions = self.scheduled_actions.write().await;
            let Some(entry) = scheduled_actions.get_mut(&id) else {
                return;
            };
            if entry.fired {
                return;
            }
            entry.fired = true;
            let prompt = action_finished_prompt(action_id, &status);
            self.emit_fired(&id, entry);
            let _ = self.fired_tx.send(ScheduledActionFired {
                action_id: id.clone(),
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

    /// List pending (not yet fired) scheduled_actions, newest first.
    pub async fn list(&self) -> Vec<Value> {
        let scheduled_actions = self.scheduled_actions.read().await;
        let mut rows: Vec<Value> = scheduled_actions
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
                    "watch_action_id": e.watch_action_id,
                    "due_at": e.due_at,
                })
            })
            .collect();
        rows.sort_by(|a, b| b["due_at"].as_str().cmp(&a["due_at"].as_str()));
        rows
    }

    /// Cancel a pending scheduled_action (no-op if already fired or unknown).
    pub async fn cancel(&self, id: &str) -> bool {
        let mut scheduled_actions = self.scheduled_actions.write().await;
        let cancelled = match scheduled_actions.get_mut(id) {
            Some(entry) if !entry.fired => {
                entry.fired = true;
                true
            }
            _ => false,
        };
        if cancelled {
            self.emit("action:updated", serde_json::json!({ "id": id }));
            if let Some(db) = self.db.read().await.as_ref() {
                let _ = db.delete_scheduled_action(id);
            }
        }
        cancelled
    }

    /// Cancel every pending scheduled_action owned by `session_id`. Called when the
    /// session ends, is removed, or is rolled back so its scheduled_actions cannot
    /// fire against a session that no longer exists.
    pub async fn cancel_for_session(&self, session_id: &str) {
        let mut scheduled_actions = self.scheduled_actions.write().await;
        let ids: Vec<String> = scheduled_actions
            .iter()
            .filter(|(_, e)| !e.fired && e.session_id.as_deref() == Some(session_id))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(entry) = scheduled_actions.get_mut(&id) {
                entry.fired = true;
            }
            self.emit("action:updated", serde_json::json!({ "id": id }));
            if let Some(db) = self.db.read().await.as_ref() {
                let _ = db.delete_scheduled_action(&id);
            }
        }
    }
}

/// Build the continuation message for a fired action-watch scheduled_action: the action's
/// terminal status and its result payload, so the resumed session continues
/// from the actual outcome instead of a generic wake text.
fn action_finished_prompt(action_id: &str, status: &Value) -> String {
    let st = status["status"].as_str().unwrap_or("unknown");
    if st == "not_found" {
        return format!(
            "Background action {action_id} not found (it may have finished long ago and been cleaned up, or never existed)."
        );
    }
    let payload = status["output"]
        .as_str()
        .or_else(|| status["error_reason"].as_str())
        .or_else(|| status["error"].as_str())
        .unwrap_or_default();
    format!("Background action {action_id} {st}.\nOutput:\n{payload}")
}

/// Schedule in-app scheduled_actions: set a timer that fires an action after a
/// delay, list pending ones, or cancel one. Timers run detached from the
/// ReAct loop, so the agent can schedule and continue working.
///
/// Two fire behaviors are available via `mode`:
/// - `tool` (default): call the tool in `tool_name` with `tool_args` —///   use `tool_name` `notify` with `tool_args` `{title, body}` to send a
///   message at fire time.
/// - `continue`: resume the session that scheduled the scheduled_action, delivering
///   `prompt` as the continuation instruction in the same conversation.
pub struct ScheduledActionTool {
    pub center: Arc<ScheduledActionCenter>,
    /// Weak probe into the tool registry so `set` can reject unknown
    /// `tool_name` values and report the scheduled tool's risk level at
    /// schedule time. `None` in headless/test builds (checks skipped).
    pub(crate) registry: Option<RegistryProbe>,
}

/// Schedule operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleOperation {
    Set,
    List,
    Cancel,
}

/// Typed parameters for `ScheduledActionTool`. Entry ① (native `run`) and
/// entry ② (`Tool::execute` with LLM JSON) both land in
/// `ScheduledActionTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ScheduledActionParams {
    /// set = schedule, list = pending, cancel = stop one.
    pub operation: ScheduleOperation,
    /// Delay in seconds before firing (set only).
    #[serde(default)]
    pub delay_secs: Option<i64>,
    /// Absolute fire time, ISO 8601 (set only).
    #[serde(default)]
    pub due_at: Option<String>,
    /// Set only: fire when this background action finishes or fails.
    #[serde(default)]
    pub watch_action_id: Option<String>,
    /// Action when it fires (set only): tool or continue.
    #[serde(default)]
    pub mode: Option<ScheduleMode>,
    /// Scheduled action title (defaults to 'Haven').
    #[serde(default)]
    pub title: Option<String>,
    /// Scheduled action message shown when it fires (set only).
    #[serde(default)]
    pub body: Option<String>,
    /// Tool to call when it fires (set only, mode=tool).
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Arguments for the tool call (set only, mode=tool).
    #[serde(default)]
    pub tool_args: Option<Value>,
    /// Continuation instruction delivered on resume (set only, mode=continue).
    #[serde(default)]
    pub prompt: Option<String>,
    /// Scheduled action id returned by set (cancel only).
    #[serde(default)]
    pub action_id: Option<String>,
    /// Private owning session id, injected by the tools manager.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl ScheduledActionTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ScheduledActionParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        match params.operation {
            ScheduleOperation::Set => {
                let delay = params.delay_secs;
                let due_at = params.due_at;
                let watch_action_id = params.watch_action_id;
                let watch = watch_action_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|w| !w.is_empty());
                if watch.is_some() && (delay.is_some() || due_at.is_some()) {
                    anyhow::bail!("watch_action_id cannot be combined with delay_secs or due_at");
                }
                if delay.is_none() && due_at.is_none() && watch.is_none() {
                    anyhow::bail!(
                        "one of delay_secs, due_at or watch_action_id is required for set"
                    );
                }
                let title = params.title.as_deref().unwrap_or("Haven");
                let body = params
                    .body
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("body is required for set"))?;
                let mode = params.mode.unwrap_or(ScheduleMode::Tool);
                if watch.is_some() && mode != ScheduleMode::Continue {
                    anyhow::bail!(
                        "watch_action_id requires mode 'continue' (the scheduled_action fires by resuming the session with the action result)"
                    );
                }
                // `_session_id` is injected privately by ToolsManager::execute_tool
                // (never part of the LLM-visible schema or step history) so the
                // scheduled_action knows which session to resume in continue mode.
                let session_id = params.session_id;
                if mode == ScheduleMode::Continue && session_id.is_none() {
                    anyhow::bail!(
                        "continue mode requires an active session to resume (internal error)"
                    );
                }
                let tool_name = params.tool_name;
                let tool_args = params.tool_args.filter(|v| !v.is_null());
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
                let prompt = params.prompt;
                let id = self
                    .center
                    .set(ScheduledActionSpec {
                        due_at: due_at.clone(),
                        delay_secs: delay.map(|d| d as u64),
                        watch_action_id: watch_action_id.clone(),
                        title: title.to_string(),
                        body: body.to_string(),
                        mode,
                        session_id,
                        tool_name,
                        tool_args,
                        prompt,
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
                    "note": "The scheduled_action fires while the app is running; overdue ones fire on next startup.",
                });
                if let Some(action_id) = &watch_action_id {
                    output["watch_action_id"] = serde_json::json!(action_id);
                    output["note"] = serde_json::json!(format!(
                        "Fires when background action {action_id} finishes or fails, resuming this session with the action's result. Action-watch scheduled_actions are in-memory only (the watched action cannot survive a restart)."
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
            ScheduleOperation::List => {
                let rows = self.center.list().await;
                Ok(ToolResult::ok(
                    serde_json::json!({ "scheduled_actions": rows }),
                ))
            }
            ScheduleOperation::Cancel => {
                let id = params
                    .action_id
                    .ok_or_else(|| anyhow::anyhow!("action_id is required for cancel"))?;
                if self.center.cancel(&id).await {
                    Ok(ToolResult::ok(serde_json::json!({ "cancelled": id })))
                } else {
                    anyhow::bail!("scheduled_action '{}' not found or already fired", id)
                }
            }
        }
    }
}

#[async_trait]
impl Tool for ScheduledActionTool {
    fn name(&self) -> String {
        "schedule".into()
    }

    fn description(&self) -> String {
        "Schedule actions to run later, mode picks what happens when \
         it fires: tool (default) calls the tool; continue \
         resumes the current session later (only while the session is still \
         active — it is cancelled when the session ends). With watch_action_id \
         the scheduled action fires when a background action finishes instead of on a \
         timer, resuming the session with the action's result."
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
                    "description": "Delay in seconds before firing (set only; use delay_secs OR due_at OR watch_action_id, exactly one, not combined)"
                },
                "due_at": {
                    "type": "string",
                    "description": "Absolute fire time, ISO 8601 e.g. 2026-08-05T15:00:00+08:00 (set only; use due_at OR delay_secs OR watch_action_id, exactly one, not combined)"
                },
                "watch_action_id": {
                    "type": "string",
                    "description": "Set only: fire when this background action (id from shell background:true) finishes or fails, instead of on a timer. Requires mode 'continue' — the session is resumed with the action's result. Exclusive with delay_secs and due_at; in-memory only (the watched action cannot survive a restart)."
                },
                "mode": {
                    "type": "string",
                    "enum": ["tool", "continue"],
                    "description": "Action when it fires (set only): tool (default) = call tool_name with tool_args, e.g. tool_name 'notify' to send a message; continue = resume the current session with prompt as the continuation instruction (or with the action's result when watch_action_id is set; only works while that session is still active — it is cancelled when the session ends)"
                },
                "title": {
                    "type": "string",
                    "description": "Scheduled action title (defaults to 'Haven')"
                },
                "body": {
                    "type": "string",
                    "description": "Scheduled action message shown when it fires (set only)"
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
                "action_id": {
                    "type": "string",
                    "description": "Scheduled action id returned by set (cancel only)"
                }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into
    /// `ScheduledActionParams`, then land in the same implementation as
    /// entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<ScheduledActionParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Tool, ToolRegistry};
    use serde_json::json;

    fn make_tool() -> ScheduledActionTool {
        ScheduledActionTool {
            center: Arc::new(ScheduledActionCenter::new()),
            registry: None,
        }
    }

    fn tool_spec(delay: u64, title: &str, body: &str) -> ScheduledActionSpec {
        ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(delay),
            watch_action_id: None,
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
        let center = Arc::new(ScheduledActionCenter::new());
        let err = center
            .set(ScheduledActionSpec {
                due_at: Some((chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339()),
                delay_secs: Some(60),
                watch_action_id: None,
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
                    "watch_action_id": "action-1",
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
                    "watch_action_id": "action-1",
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
                    "watch_action_id": "action-1",
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
        // No actions registry wired (headless/test build): watch scheduled_actions fail
        // fast instead of never firing.
        let center = Arc::new(ScheduledActionCenter::new());
        let err = center
            .set(ScheduledActionSpec {
                due_at: None,
                delay_secs: None,
                watch_action_id: Some("action-1".into()),
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
            msg.contains("requires the background-actions registry"),
            "unexpected error: {msg}"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_watch_action_fires_with_result_when_action_finishes() {
        use crate::bg::BackgroundActions;
        let actions = Arc::new(BackgroundActions::new());
        let center = Arc::new(ScheduledActionCenter::new());
        center.set_actions(Some(actions.clone()));
        let mut rx = center.take_fired_receiver().expect("receiver available");

        let action_id = actions
            .spawn_shell("echo action-watch-result", "cmd", 20_000, None)
            .await
            .unwrap();
        let id = center
            .set(ScheduledActionSpec {
                due_at: None,
                delay_secs: None,
                watch_action_id: Some(action_id.clone()),
                title: "Watch".into(),
                body: "action done".into(),
                mode: ScheduleMode::Continue,
                session_id: Some("ses-1".into()),
                tool_name: None,
                tool_args: None,
                prompt: None,
            })
            .await
            .unwrap();

        // Fires once the action completes, resuming the session with the result.
        let fired = tokio::time::timeout(Duration::from_secs(15), rx.recv())
            .await
            .expect("timed out waiting for action-watch fire")
            .expect("channel closed");
        assert_eq!(fired.action_id, id);
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        let prompt = fired.prompt.expect("watch fire must carry the result");
        assert!(
            prompt.contains("action-watch-result"),
            "prompt must carry the action output: {prompt}"
        );
        assert!(
            prompt.contains("completed"),
            "prompt must carry the status: {prompt}"
        );
        // Not persisted (in-memory only).
        assert!(center.list().await.is_empty());
    }

    #[tokio::test]
    async fn test_watch_action_not_persisted_to_db() {
        let (db, _dir) = test_db();
        let actions = Arc::new(crate::bg::BackgroundActions::new());
        let center = Arc::new(ScheduledActionCenter::new());
        center.set_db(Some(db.clone())).await;
        center.set_actions(Some(actions));
        center
            .set(ScheduledActionSpec {
                due_at: None,
                delay_secs: None,
                watch_action_id: Some("action-nope".into()),
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
        // The watched action cannot survive a restart: nothing in the DB.
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
        // Listed in memory (with the watched action id).
        let rows = center.list().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["watch_action_id"], json!("action-nope"));
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
        let tool = ScheduledActionTool {
            center: Arc::new(ScheduledActionCenter::new()),
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
        let tool = ScheduledActionTool {
            center: Arc::new(ScheduledActionCenter::new()),
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
        let center = Arc::new(ScheduledActionCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // Absolute time 2s out, continue mode with a wake prompt.
        let due = (chrono::Utc::now() + chrono::Duration::seconds(2)).to_rfc3339();
        let id = center
            .set(ScheduledActionSpec {
                due_at: Some(due),
                delay_secs: None,
                watch_action_id: None,
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
        let pending = db.list_pending_scheduled_actions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].session_id.as_deref(), Some("ses-1"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for scheduled_action")
            .expect("channel closed");
        assert_eq!(fired.action_id, id);
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        assert_eq!(fired.prompt.as_deref(), Some("check the weather"));
        assert_eq!(fired.title, "Wake");
        assert_eq!(fired.body, "body text");
    }

    #[tokio::test]
    async fn test_set_tool_mode_records_call_and_fires() {
        let (db, _dir) = test_db();
        let center = Arc::new(ScheduledActionCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        let id = center
            .set(ScheduledActionSpec {
                due_at: None,
                delay_secs: Some(1),
                watch_action_id: None,
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
        let pending = db.list_pending_scheduled_actions().unwrap();
        assert_eq!(pending[0].mode, "tool");
        assert_eq!(pending[0].tool_name.as_deref(), Some("file"));
        assert!(pending[0].tool_args.as_deref().unwrap().contains("C:/x"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for scheduled_action")
            .expect("channel closed");
        assert_eq!(fired.action_id, id);
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
        assert_eq!(
            list.output["scheduled_actions"].as_array().unwrap().len(),
            1
        );
        assert_eq!(list.output["scheduled_actions"][0]["id"], json!(id));
        assert_eq!(list.output["scheduled_actions"][0]["body"], json!("water"));
        assert_eq!(list.output["scheduled_actions"][0]["mode"], json!("tool"));

        let cancelled = tool
            .execute(
                json!({"operation": "cancel", "action_id": id}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.output["cancelled"], json!(id));

        // Cancelling again fails.
        let err = tool
            .execute(
                json!({"operation": "cancel", "action_id": id}),
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
            list.output["scheduled_actions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn test_reminder_fires_and_delivers() {
        let center = Arc::new(ScheduledActionCenter::new());
        let mut rx = center.take_fired_receiver().expect("receiver available");
        let tool = ScheduledActionTool {
            center: center.clone(),
            registry: None,
        };
        let id = center.set(tool_spec(1, "Test", "fire now")).await.unwrap();
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for scheduled_action")
            .expect("channel closed");
        assert_eq!(fired.action_id, id);
        assert_eq!(fired.mode, ScheduleMode::Tool);
        assert_eq!(fired.tool_name.as_deref(), Some("notify"));
        assert_eq!(fired.title, "Test");
        assert_eq!(fired.body, "fire now");

        // Fired scheduled_actions are reaped by the next set (cap stays clean).
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
        let center = Arc::new(ScheduledActionCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // A future scheduled_action (5s out) and an overdue one (already past).
        let future_id = center.set(tool_spec(5, "Future", "later")).await.unwrap();
        let overdue_id = format!("action-{}", uuid::Uuid::new_v4().simple());
        db.save_scheduled_action(
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
        let restored = Arc::new(ScheduledActionCenter::new());
        restored.set_db(Some(db.clone())).await;
        let mut rx2 = restored.take_fired_receiver().expect("receiver available");
        let overdue_count = restored.restore_pending().await;
        assert_eq!(overdue_count, 1, "exactly one scheduled_action was overdue");

        // Overdue scheduled_action fired immediately with its mode payload.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
            .await
            .expect("timed out waiting for overdue fire")
            .expect("channel closed");
        assert_eq!(fired.action_id, overdue_id);
        assert_eq!(fired.title, "Overdue");
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        assert_eq!(fired.prompt.as_deref(), Some("keep going"));

        // Future scheduled_action re-armed and fires after its remaining delay.
        let fired = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for future fire")
            .expect("channel closed");
        assert_eq!(fired.action_id, future_id);

        // Both are marked fired in the DB; pending list is empty.
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_reminder_set_persists_to_db() {
        let (db, _dir) = test_db();
        let center = Arc::new(ScheduledActionCenter::new());
        center.set_db(Some(db.clone())).await;
        let id = center.set(tool_spec(3600, "Drink", "water")).await.unwrap();
        let pending = db.list_pending_scheduled_actions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].body, "water");
        assert_eq!(pending[0].mode, "tool");

        // Cancel removes the row.
        assert!(center.cancel(&id).await);
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
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
        let center = Arc::new(ScheduledActionCenter::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        center.set_event_sink(Arc::new(move |name, payload| {
            sink_events.lock().unwrap().push((name, payload));
        }));
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // set -> action:created event with the payload.
        let id = center.set(tool_spec(1, "Evt", "fire me")).await.unwrap();
        {
            let evs = events.lock().unwrap();
            let set_evt = evs
                .iter()
                .find(|(n, _)| n == "action:created")
                .expect("action:created emitted");
            assert_eq!(set_evt.1["id"], id);
            assert_eq!(set_evt.1["body"], "fire me");
            assert!(set_evt.1["due_at"].as_str().is_some());
        }

        // Fire -> action:finished event.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for fire")
            .expect("channel closed");
        assert_eq!(fired.action_id, id);
        {
            let evs = events.lock().unwrap();
            let fired_evt = evs
                .iter()
                .find(|(n, _)| n == "action:finished")
                .expect("action:finished emitted");
            assert_eq!(fired_evt.1["id"], id);
            assert_eq!(fired_evt.1["mode"], "tool");
        }

        // cancel -> action:updated event.
        let id2 = center
            .set(tool_spec(3600, "Keep", "pending"))
            .await
            .unwrap();
        assert!(center.cancel(&id2).await);
        {
            let evs = events.lock().unwrap();
            let cancel_evt = evs
                .iter()
                .find(|(n, _)| n == "action:updated")
                .expect("action:updated emitted");
            assert_eq!(cancel_evt.1["id"], id2);
        }
    }
}
