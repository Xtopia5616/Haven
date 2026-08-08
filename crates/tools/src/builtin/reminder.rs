use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// What happens when a reminder fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderMode {
    /// Call the tool in `tool_name` with `tool_args` (no LLM involved).
    /// To send a message at fire time, call the `notify` tool here.
    #[default]
    Tool,
    /// Resume the task that scheduled the reminder: the task continues with
    /// the reminder text as a new instruction in the same conversation.
    Continue,
}

impl ReminderMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ReminderMode::Tool => "tool",
            ReminderMode::Continue => "continue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "tool" => Some(ReminderMode::Tool),
            "continue" => Some(ReminderMode::Continue),
            _ => None,
        }
    }
}

/// A reminder that fired; delivered to the app layer so it can run a tool
/// (`Tool`, including `notify` for a message) or resume the scheduling task
/// (`Continue`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReminderFired {
    pub reminder_id: String,
    pub title: String,
    pub body: String,
    pub mode: ReminderMode,
    /// Task that scheduled the reminder 閳?resume target for `Continue` mode,
    /// tool-context scope for `Tool` mode. `None` on legacy rows.
    pub task_id: Option<String>,
    /// `Tool` mode: tool to call when the reminder fires.
    pub tool_name: Option<String>,
    /// `Tool` mode: arguments for the tool call.
    pub tool_args: Option<Value>,
    /// `Continue` mode: continuation message delivered to the task (falls
    /// back to `body`). On legacy rows it is the wake text for a new task.
    pub prompt: Option<String>,
}

/// Everything needed to schedule one reminder.
pub struct ReminderSpec {
    /// Absolute fire time (RFC3339, local time accepted). Use this OR
    /// `delay_secs`; exactly one is required.
    pub due_at: Option<String>,
    /// Delay in seconds before the reminder fires. Use this OR `due_at`.
    pub delay_secs: Option<u64>,
    pub title: String,
    pub body: String,
    pub mode: ReminderMode,
    /// The task that schedules the reminder (injected by the tool manager,
    /// not visible to the LLM). Resume target for `Continue`.
    pub task_id: Option<String>,
    /// `Tool` mode: tool to call when the reminder fires.
    pub tool_name: Option<String>,
    /// `Tool` mode: arguments for the tool call.
    pub tool_args: Option<Value>,
    /// `Continue` mode: continuation message delivered to the task.
    pub prompt: Option<String>,
}

/// Lifetime cap on reminders per process. Fired reminders are reaped on the
/// next `set`, so this bounds concurrent pending timers, not history.
/// Upper bound on a `due_at`-scheduled reminder (365 days) — guards against
/// typos like a swapped year. Delay-based reminders are capped separately.
struct ReminderEntry {
    title: String,
    body: String,
    due_at: String,
    mode: ReminderMode,
    task_id: Option<String>,
    tool_name: Option<String>,
    tool_args: Option<Value>,
    prompt: Option<String>,
    fired: bool,
}

/// Registry of in-process timers for the `reminder` tool, with a persistent
/// backing store: every `set` is written to the database so reminders
/// survive app restarts. On startup the agent layer calls `restore_pending`:
/// overdue reminders fire immediately (missed while the app was off), the
/// rest are re-armed with their remaining delay. The in-memory timer is the
/// delivery mechanism while the app runs; the DB is the source of truth.
pub struct ReminderCenter {
    reminders: RwLock<HashMap<String, ReminderEntry>>,
    fired_tx: tokio::sync::mpsc::UnboundedSender<ReminderFired>,
    fired_rx: Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<ReminderFired>>>,
    /// Persistent store; `None` in headless/test builds (in-memory only).
    db: RwLock<Option<Arc<Database>>>,
    /// Lifetime cap on pending reminders (from context limits).
    max_reminders: RwLock<usize>,
    max_due_horizon_secs: RwLock<i64>,
}

impl Default for ReminderCenter {
    fn default() -> Self {
        Self::new()
    }
}

impl ReminderCenter {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            reminders: RwLock::new(HashMap::new()),
            fired_tx: tx,
            fired_rx: Mutex::new(Some(rx)),
            db: RwLock::new(None),
            max_reminders: RwLock::new(32),
            max_due_horizon_secs: RwLock::new(365 * 24 * 3600),
        }
    }

    /// Replace the unified context limits (reminder caps).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_reminders.write().await = limits.reminders_max;
        *self.max_due_horizon_secs.write().await = limits.reminders_due_horizon_secs;
    }

    /// Attach the database used for persistence. Wired by the desktop shell
    /// (same handle the `self` tool receives); headless tests skip it.
    pub async fn set_db(&self, db: Option<Arc<Database>>) {
        *self.db.write().await = db;
    }

    /// Take the fired-reminder receiver exactly once (consumed by the agent
    /// layer, which emits Notification events). Returns `None` if already
    /// taken.
    pub fn take_fired_receiver(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<ReminderFired>> {
        self.fired_rx.lock().unwrap().take()
    }

    /// Re-arm all pending reminders from the database after a restart.
    ///
    /// - Reminders whose due time already passed (the app was off when they
    ///   expired) fire immediately and are marked fired.
    /// - Future reminders are re-armed in memory with their remaining delay.
    ///
    /// Returns the number of reminders fired as overdue. Called once from the
    /// agent layer startup; safe to call again (idempotent 閳?in-memory
    /// entries are skipped).
    pub async fn restore_pending(self: &Arc<Self>) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        let Ok(rows) = db.list_pending_reminders() else {
            return 0;
        };
        let now = chrono::Utc::now();
        let mut overdue = 0usize;
        for row in rows {
            // Idempotency: skip entries the in-memory map already holds
            // (restore was already run, or the reminder was re-set live).
            if self.reminders.read().await.contains_key(&row.id) {
                continue;
            }
            let due = chrono::DateTime::parse_from_rfc3339(&row.due_at)
                .map(|d| d.with_timezone(&chrono::Utc))
                .unwrap_or(now);
            let remaining = (due - now).num_seconds();
            let mode = ReminderMode::parse(&row.mode).unwrap_or(ReminderMode::Tool);
            let tool_args = row
                .tool_args
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok());
            let fired_payload = ReminderFired {
                reminder_id: row.id.clone(),
                title: row.title.clone(),
                body: row.body.clone(),
                mode,
                task_id: row.task_id.clone(),
                tool_name: row.tool_name.clone(),
                tool_args: tool_args.clone(),
                prompt: row.prompt.clone(),
            };
            if remaining <= 0 {
                // Overdue while the app was closed: fire now.
                self.reminders.write().await.insert(
                    row.id.clone(),
                    ReminderEntry {
                        title: row.title.clone(),
                        body: row.body.clone(),
                        due_at: row.due_at.clone(),
                        mode,
                        task_id: row.task_id.clone(),
                        tool_name: row.tool_name.clone(),
                        tool_args: tool_args.clone(),
                        prompt: row.prompt.clone(),
                        fired: true,
                    },
                );
                let _ = self.fired_tx.send(fired_payload);
                let _ = db.mark_reminder_fired(&row.id);
                overdue += 1;
            } else {
                let center = self.clone();
                let id = row.id.clone();
                self.reminders.write().await.insert(
                    id.clone(),
                    ReminderEntry {
                        title: row.title.clone(),
                        body: row.body.clone(),
                        due_at: row.due_at.clone(),
                        mode,
                        task_id: row.task_id.clone(),
                        tool_name: row.tool_name.clone(),
                        tool_args: tool_args.clone(),
                        prompt: row.prompt.clone(),
                        fired: false,
                    },
                );
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(remaining as u64)).await;
                    let mut reminders = center.reminders.write().await;
                    if let Some(entry) = reminders.get_mut(&id)
                        && !entry.fired
                    {
                        entry.fired = true;
                        let _ = center.fired_tx.send(ReminderFired {
                            reminder_id: id.clone(),
                            title: entry.title.clone(),
                            body: entry.body.clone(),
                            mode: entry.mode,
                            task_id: entry.task_id.clone(),
                            tool_name: entry.tool_name.clone(),
                            tool_args: entry.tool_args.clone(),
                            prompt: entry.prompt.clone(),
                        });
                        if let Some(db) = center.db.read().await.as_ref() {
                            let _ = db.mark_reminder_fired(&id);
                        }
                    }
                });
            }
        }
        overdue
    }

    /// Schedule a reminder to fire at an absolute time or after a delay.
    /// Exactly one of `spec.due_at` (RFC3339, local time accepted) or
    /// `spec.delay_secs` must be given. `spec.mode` selects what happens at
    /// fire time (see [`ReminderMode`]).
    ///
    /// Returns the reminder id; the timer runs detached from the ReAct loop
    /// and delivers a `ReminderFired` on the channel when it expires.
    pub async fn set(self: &Arc<Self>, spec: ReminderSpec) -> anyhow::Result<String> {
        let ReminderSpec {
            due_at,
            delay_secs,
            title,
            body,
            mode,
            task_id,
            tool_name,
            tool_args,
            prompt,
        } = spec;
        // Resolve when the reminder fires.
        let now = chrono::Utc::now();
        let (due, remaining) = match (due_at.as_deref(), delay_secs) {
            (Some(due_at), _) => {
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
                (parsed, remaining)
            }
            (None, Some(delay)) => {
                if delay == 0 || delay > 86_400 {
                    anyhow::bail!("delay_secs must be between 1 and 86400");
                }
                (now + chrono::Duration::seconds(delay as i64), delay as i64)
            }
            (None, None) => anyhow::bail!("either due_at or delay_secs is required"),
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
        if mode == ReminderMode::Tool && tool_name.is_none() {
            anyhow::bail!("tool_name is required when mode is 'tool'");
        }

        let id = format!("rem-{}", uuid::Uuid::new_v4().simple());
        let due_at_rfc = due.to_rfc3339();
        {
            let mut reminders = self.reminders.write().await;
            // Reap fired entries so they never occupy the cap.
            reminders.retain(|_, e| !e.fired);
            if reminders.len() >= *self.max_reminders.read().await {
                anyhow::bail!(
                    "too many pending reminders (limit {}); cancel some first",
                    *self.max_reminders.read().await
                );
            }
            reminders.insert(
                id.clone(),
                ReminderEntry {
                    title: title.clone(),
                    body: body.clone(),
                    due_at: due_at_rfc.clone(),
                    mode,
                    task_id: task_id.clone(),
                    tool_name: tool_name.clone(),
                    tool_args: tool_args.clone(),
                    prompt: prompt.clone(),
                    fired: false,
                },
            );
        }
        // Persist so the reminder survives restarts; without a DB this is
        // still a valid in-memory-only reminder (headless/test builds).
        if let Some(db) = self.db.read().await.as_ref() {
            let args_json = tool_args.as_ref().map(|v| v.to_string());
            if let Err(e) = db.save_reminder(
                &id,
                &due_at_rfc,
                &title,
                &body,
                mode.as_str(),
                task_id.as_deref(),
                tool_name.as_deref(),
                args_json.as_deref(),
                prompt.as_deref(),
            ) {
                tracing::warn!("failed to persist reminder {}: {}", id, e);
            }
        }

        let center = self.clone();
        let fired_id = id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(remaining.max(0) as u64)).await;
            let mut reminders = center.reminders.write().await;
            if let Some(entry) = reminders.get_mut(&fired_id)
                && !entry.fired
            {
                entry.fired = true;
                let _ = center.fired_tx.send(ReminderFired {
                    reminder_id: fired_id.clone(),
                    title: entry.title.clone(),
                    body: entry.body.clone(),
                    mode: entry.mode,
                    task_id: entry.task_id.clone(),
                    tool_name: entry.tool_name.clone(),
                    tool_args: entry.tool_args.clone(),
                    prompt: entry.prompt.clone(),
                });
                if let Some(db) = center.db.read().await.as_ref() {
                    let _ = db.mark_reminder_fired(&fired_id);
                }
            }
        });

        Ok(id)
    }

    /// List pending (not yet fired) reminders, newest first.
    pub async fn list(&self) -> Vec<Value> {
        let reminders = self.reminders.read().await;
        let mut rows: Vec<Value> = reminders
            .iter()
            .filter(|(_, e)| !e.fired)
            .map(|(id, e)| {
                serde_json::json!({
                    "id": id,
                    "title": e.title,
                    "body": e.body,
                    "mode": e.mode.as_str(),
                    "task_id": e.task_id,
                    "tool_name": e.tool_name,
                    "tool_args": e.tool_args,
                    "prompt": e.prompt,
                    "due_at": e.due_at,
                })
            })
            .collect();
        rows.sort_by(|a, b| b["due_at"].as_str().cmp(&a["due_at"].as_str()));
        rows
    }

    /// Cancel a pending reminder (no-op if already fired or unknown).
    pub async fn cancel(&self, id: &str) -> bool {
        let mut reminders = self.reminders.write().await;
        let cancelled = match reminders.get_mut(id) {
            Some(entry) if !entry.fired => {
                entry.fired = true;
                true
            }
            _ => false,
        };
        if cancelled && let Some(db) = self.db.read().await.as_ref() {
            let _ = db.delete_reminder(id);
        }
        cancelled
    }
}

/// Schedule in-app reminders: set a timer that fires an action after a
/// delay, list pending ones, or cancel one. Timers run detached from the
/// ReAct loop, so the agent can schedule and continue working.
///
/// Two fire behaviors are available via `mode`:
/// - `tool` (default): call the tool in `tool_name` with `tool_args` 閳?///   use `tool_name` `notify` with `tool_args` `{title, body}` to send a
///   message at fire time.
/// - `continue`: resume the task that scheduled the reminder, delivering
///   `prompt` as the continuation instruction in the same conversation.
pub struct ReminderTool {
    pub center: Arc<ReminderCenter>,
}

#[async_trait]
impl Tool for ReminderTool {
    fn name(&self) -> String {
        "reminder".into()
    }

    fn description(&self) -> String {
        "Schedule actions to run later, mode picks what happens when \
         it fires: tool (default) calls the tool; continue \
         resumes the current task later."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            // set only schedules a local timer 閳?no system mutation.
            Some("set") => RiskLevel::Low,
            _ => RiskLevel::Safe,
        }
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
                    "description": "Delay in seconds before firing (set only; use delay_secs OR due_at)"
                },
                "due_at": {
                    "type": "string",
                    "description": "Absolute fire time, ISO 8601 e.g. 2026-08-05T15:00:00+08:00 (set only; use due_at OR delay_secs)"
                },
                "mode": {
                    "type": "string",
                    "enum": ["tool", "continue"],
                    "description": "Action when it fires (set only): tool (default) = call tool_name with tool_args, e.g. tool_name 'notify' to send a message; continue = resume the current task with prompt as the continuation instruction"
                },
                "title": {
                    "type": "string",
                    "description": "Reminder title (defaults to 'Haven')"
                },
                "body": {
                    "type": "string",
                    "description": "Reminder message shown when it fires (set only)"
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
                    "description": "Continuation instruction delivered to the task when it resumes (set only, mode=continue)"
                },
                "reminder_id": {
                    "type": "string",
                    "description": "Reminder id returned by set (cancel only)"
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
                if delay.is_none() && due_at.is_none() {
                    anyhow::bail!("either delay_secs or due_at is required for set");
                }
                let title = input["title"].as_str().unwrap_or("Haven");
                let body = input["body"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("body is required for set"))?;
                let mode = match input["mode"].as_str().unwrap_or("tool") {
                    "tool" => ReminderMode::Tool,
                    "continue" => ReminderMode::Continue,
                    other => {
                        anyhow::bail!("unknown reminder mode: {other} (expected tool or continue)")
                    }
                };
                // `_task_id` is injected privately by ToolsManager::execute_tool
                // (never part of the LLM-visible schema or step history) so the
                // reminder knows which task to resume in continue mode.
                let task_id = input["_task_id"].as_str().map(str::to_string);
                if mode == ReminderMode::Continue && task_id.is_none() {
                    anyhow::bail!(
                        "continue mode requires an active task to resume (internal error)"
                    );
                }
                let tool_name = input["tool_name"].as_str().map(str::to_string);
                let tool_args = input.get("tool_args").filter(|v| !v.is_null()).cloned();
                let prompt = input["prompt"].as_str();
                let id = self
                    .center
                    .set(ReminderSpec {
                        due_at: due_at.map(str::to_string),
                        delay_secs: delay.map(|d| d as u64),
                        title: title.to_string(),
                        body: body.to_string(),
                        mode,
                        task_id,
                        tool_name,
                        tool_args,
                        prompt: prompt.map(str::to_string),
                    })
                    .await?;
                let fires_at = due_at
                    .and_then(|d| chrono::DateTime::parse_from_rfc3339(d.trim()).ok())
                    .map(|d| d.to_rfc3339())
                    .unwrap_or_else(|| {
                        (chrono::Utc::now() + chrono::Duration::seconds(delay.unwrap_or(0)))
                            .to_rfc3339()
                    });
                Ok(ToolResult::ok(serde_json::json!({
                    "id": id,
                    "mode": mode.as_str(),
                    "fires_at": fires_at,
                    "wakes_task": mode == ReminderMode::Continue,
                    "note": "The reminder fires while the app is running; overdue ones fire on next startup.",
                })))
            }
            "list" => {
                let rows = self.center.list().await;
                Ok(ToolResult::ok(serde_json::json!({ "reminders": rows })))
            }
            "cancel" => {
                let id = input["reminder_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("reminder_id is required for cancel"))?;
                if self.center.cancel(id).await {
                    Ok(ToolResult::ok(serde_json::json!({ "cancelled": id })))
                } else {
                    anyhow::bail!("reminder '{}' not found or already fired", id)
                }
            }
            _ => anyhow::bail!("unknown reminder operation: {}", op),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    fn make_tool() -> ReminderTool {
        ReminderTool {
            center: Arc::new(ReminderCenter::new()),
        }
    }

    fn tool_spec(delay: u64, title: &str, body: &str) -> ReminderSpec {
        ReminderSpec {
            due_at: None,
            delay_secs: Some(delay),
            title: title.into(),
            body: body.into(),
            mode: ReminderMode::Tool,
            task_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        }
    }

    #[test]
    fn test_reminder_name() {
        assert_eq!(make_tool().name(), "reminder");
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
        assert_eq!(ReminderMode::parse("tool"), Some(ReminderMode::Tool));
        assert_eq!(
            ReminderMode::parse("continue"),
            Some(ReminderMode::Continue)
        );
        assert_eq!(ReminderMode::parse("notify"), None);
        assert_eq!(ReminderMode::parse("bogus"), None);
        assert_eq!(ReminderMode::default(), ReminderMode::Tool);
        assert_eq!(ReminderMode::Tool.as_str(), "tool");
        assert_eq!(ReminderMode::Continue.as_str(), "continue");
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
        // continue mode without the injected task id.
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
    async fn test_set_with_due_at_and_prompt() {
        let (db, _dir) = test_db();
        let center = Arc::new(ReminderCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // Absolute time 2s out, continue mode with a wake prompt.
        let due = (chrono::Utc::now() + chrono::Duration::seconds(2)).to_rfc3339();
        let id = center
            .set(ReminderSpec {
                due_at: Some(due),
                delay_secs: None,
                title: "Wake".into(),
                body: "body text".into(),
                mode: ReminderMode::Continue,
                task_id: Some("task-1".into()),
                tool_name: None,
                tool_args: None,
                prompt: Some("check the weather".into()),
            })
            .await
            .unwrap();

        // Persisted with mode + task + prompt.
        let pending = db.list_pending_reminders().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].mode, "continue");
        assert_eq!(pending[0].task_id.as_deref(), Some("task-1"));
        assert_eq!(pending[0].prompt.as_deref(), Some("check the weather"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reminder")
            .expect("channel closed");
        assert_eq!(fired.reminder_id, id);
        assert_eq!(fired.mode, ReminderMode::Continue);
        assert_eq!(fired.task_id.as_deref(), Some("task-1"));
        assert_eq!(fired.prompt.as_deref(), Some("check the weather"));
        assert_eq!(fired.title, "Wake");
        assert_eq!(fired.body, "body text");
    }

    #[tokio::test]
    async fn test_set_tool_mode_records_call_and_fires() {
        let (db, _dir) = test_db();
        let center = Arc::new(ReminderCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        let id = center
            .set(ReminderSpec {
                due_at: None,
                delay_secs: Some(1),
                title: "Backup".into(),
                body: "running backup".into(),
                mode: ReminderMode::Tool,
                task_id: Some("task-1".into()),
                tool_name: Some("file".into()),
                tool_args: Some(json!({"operation": "read", "path": "C:/x"})),
                prompt: None,
            })
            .await
            .unwrap();

        // Persisted with the tool payload.
        let pending = db.list_pending_reminders().unwrap();
        assert_eq!(pending[0].mode, "tool");
        assert_eq!(pending[0].tool_name.as_deref(), Some("file"));
        assert!(pending[0].tool_args.as_deref().unwrap().contains("C:/x"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reminder")
            .expect("channel closed");
        assert_eq!(fired.reminder_id, id);
        assert_eq!(fired.mode, ReminderMode::Tool);
        assert_eq!(fired.tool_name.as_deref(), Some("file"));
        assert_eq!(fired.task_id.as_deref(), Some("task-1"));
        assert_eq!(fired.tool_args.as_ref().unwrap()["path"], "C:/x");
    }

    #[tokio::test]
    async fn test_set_continue_wakes_task_flag_in_output() {
        let tool = make_tool();
        let result = tool
            .execute(
                json!({
                    "operation": "set",
                    "due_at": (chrono::Utc::now() + chrono::Duration::seconds(3600)).to_rfc3339(),
                    "body": "x",
                    "mode": "continue",
                    "prompt": "summarize my notes",
                    "_task_id": "task-9"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["mode"], json!("continue"));
        assert_eq!(result.output["wakes_task"], json!(true));

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
        assert_eq!(plain.output["wakes_task"], json!(false));
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
        assert_eq!(list.output["reminders"].as_array().unwrap().len(), 1);
        assert_eq!(list.output["reminders"][0]["id"], json!(id));
        assert_eq!(list.output["reminders"][0]["body"], json!("water"));
        assert_eq!(list.output["reminders"][0]["mode"], json!("tool"));

        let cancelled = tool
            .execute(
                json!({"operation": "cancel", "reminder_id": id}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.output["cancelled"], json!(id));

        // Cancelling again fails.
        let err = tool
            .execute(
                json!({"operation": "cancel", "reminder_id": id}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());

        // List is empty after cancel.
        let list = tool
            .execute(json!({"operation": "list"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(list.output["reminders"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_reminder_fires_and_delivers() {
        let center = Arc::new(ReminderCenter::new());
        let mut rx = center.take_fired_receiver().expect("receiver available");
        let tool = ReminderTool {
            center: center.clone(),
        };
        let id = center.set(tool_spec(1, "Test", "fire now")).await.unwrap();
        let fired = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for reminder")
            .expect("channel closed");
        assert_eq!(fired.reminder_id, id);
        assert_eq!(fired.mode, ReminderMode::Tool);
        assert_eq!(fired.tool_name.as_deref(), Some("notify"));
        assert_eq!(fired.title, "Test");
        assert_eq!(fired.body, "fire now");

        // Fired reminders are reaped by the next set (cap stays clean).
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
        let center = Arc::new(ReminderCenter::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_fired_receiver().expect("receiver available");

        // A future reminder (5s out) and an overdue one (already past).
        let future_id = center.set(tool_spec(5, "Future", "later")).await.unwrap();
        let overdue_id = format!("rem-{}", uuid::Uuid::new_v4().simple());
        db.save_reminder(
            &overdue_id,
            &(chrono::Utc::now() - chrono::Duration::seconds(60)).to_rfc3339(),
            "Overdue",
            "should fire now",
            "continue",
            Some("task-1"),
            None,
            None,
            Some("keep going"),
        )
        .unwrap();

        // A fresh center (simulating app restart) restores from the DB.
        let restored = Arc::new(ReminderCenter::new());
        restored.set_db(Some(db.clone())).await;
        let mut rx2 = restored.take_fired_receiver().expect("receiver available");
        let overdue_count = restored.restore_pending().await;
        assert_eq!(overdue_count, 1, "exactly one reminder was overdue");

        // Overdue reminder fired immediately with its mode payload.
        let fired = tokio::time::timeout(Duration::from_secs(5), rx2.recv())
            .await
            .expect("timed out waiting for overdue fire")
            .expect("channel closed");
        assert_eq!(fired.reminder_id, overdue_id);
        assert_eq!(fired.title, "Overdue");
        assert_eq!(fired.mode, ReminderMode::Continue);
        assert_eq!(fired.task_id.as_deref(), Some("task-1"));
        assert_eq!(fired.prompt.as_deref(), Some("keep going"));

        // Future reminder re-armed and fires after its remaining delay.
        let fired = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("timed out waiting for future fire")
            .expect("channel closed");
        assert_eq!(fired.reminder_id, future_id);

        // Both are marked fired in the DB; pending list is empty.
        assert!(db.list_pending_reminders().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_reminder_set_persists_to_db() {
        let (db, _dir) = test_db();
        let center = Arc::new(ReminderCenter::new());
        center.set_db(Some(db.clone())).await;
        let id = center.set(tool_spec(3600, "Drink", "water")).await.unwrap();
        let pending = db.list_pending_reminders().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, id);
        assert_eq!(pending[0].body, "water");
        assert_eq!(pending[0].mode, "tool");

        // Cancel removes the row.
        assert!(center.cancel(&id).await);
        assert!(db.list_pending_reminders().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_unknown_operation_rejected() {
        let tool = make_tool();
        let err = tool
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(err.is_err());
    }
}
