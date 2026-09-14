use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::ActionService;
use crate::registry::RegistryProbe;
use crate::{Tool, ToolConcurrency, ToolResult};

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

/// Typed payload published by the unified [`crate::ActionService`] completion
/// bus when a scheduled action enters its fire transition.
pub use crate::action_service::ScheduledActionFired;

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
    pub service: Arc<ActionService>,
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
                if due_at.is_some() && delay.is_some() {
                    anyhow::bail!("use exactly one of due_at or delay_secs, not both");
                }
                if let Some(delay) = delay
                    && !(1..=86_400).contains(&delay)
                {
                    anyhow::bail!("delay_secs must be between 1 and 86400");
                }
                let title = params.title.as_deref().unwrap_or("Haven");
                let body = params
                    .body
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("body is required for set"))?;
                let mode = params.mode.unwrap_or(ScheduleMode::Tool);
                if watch.is_some() && mode != ScheduleMode::Continue {
                    anyhow::bail!(
                        "watch_action_id requires mode 'continue' (the schedule action fires by resuming the session with the action result)"
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
                match mode {
                    ScheduleMode::Tool => {
                        if tool_name
                            .as_deref()
                            .is_none_or(|name| name.trim().is_empty())
                        {
                            anyhow::bail!("tool_name is required when mode is 'tool'");
                        }
                    }
                    ScheduleMode::Continue => {
                        if params
                            .prompt
                            .as_deref()
                            .is_none_or(|prompt| prompt.trim().is_empty())
                        {
                            anyhow::bail!("prompt is required when mode is 'continue'");
                        }
                    }
                }
                if let Some(args) = &tool_args
                    && !args.is_object()
                {
                    anyhow::bail!("tool_args must be a JSON object");
                }
                if mode != ScheduleMode::Tool && (tool_name.is_some() || tool_args.is_some()) {
                    anyhow::bail!("tool_name and tool_args require mode 'tool'");
                }
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
                            let empty_args = Value::Object(serde_json::Map::new());
                            let args = tool_args.as_ref().unwrap_or(&empty_args);
                            tool.validate_input(args).map_err(|error| {
                                anyhow::anyhow!(
                                    "invalid arguments for scheduled tool '{}': {}",
                                    tool_name,
                                    error
                                )
                            })?;
                            Some(tool.risk_level(args))
                        }
                        None => None,
                    }
                } else {
                    None
                };
                let prompt = params.prompt.map(|prompt| prompt.trim().to_string());
                let id = self
                    .service
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
                    "operation": "set",
                    "id": id,
                    "mode": mode.as_str(),
                    "fires_at": fires_at,
                    "wakes_session": mode == ScheduleMode::Continue,
                    "note": "The schedule action fires while the app is running; overdue ones fire on next startup.",
                });
                if let Some(action_id) = &watch_action_id {
                    output["watch_action_id"] = serde_json::json!(action_id);
                    output["note"] = serde_json::json!(format!(
                        "Fires when background action {action_id} finishes or fails, resuming this session with the action's result. Action-watch schedule actions are in-memory only (the watched action cannot survive a restart)."
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
                let session_id = params
                    .session_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("schedule list requires a session context"))?;
                let rows = self.service.list_scheduled_for_session(session_id).await;
                Ok(ToolResult::ok(
                    serde_json::json!({ "operation": "list", "scheduled_actions": rows }),
                ))
            }
            ScheduleOperation::Cancel => {
                let session_id = params
                    .session_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("schedule cancel requires a session context"))?;
                let id = params
                    .action_id
                    .ok_or_else(|| anyhow::anyhow!("action_id is required for cancel"))?;
                if self.service.cancel_for_session(&id, session_id).await {
                    Ok(ToolResult::ok(
                        serde_json::json!({ "operation": "cancel", "cancelled": id }),
                    ))
                } else {
                    anyhow::bail!("schedule action '{}' not found or already fired", id)
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
        crate::prompts::SCHEDULE_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            // set only schedules a local timer —no system mutation.
            Some("set") => RiskLevel::Low,
            _ => RiskLevel::Safe,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        if input["operation"].as_str() == Some("list") {
            ToolConcurrency::SharedResource("scheduled_actions".into())
        } else {
            ToolConcurrency::Resource("scheduled_actions".into())
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
                "operation": { "type": "string", "enum": ["set", "list", "cancel"] },
                "delay_secs": { "type": "integer", "minimum": 1, "maximum": 86400 },
                "due_at": { "type": "string", "minLength": 1 },
                "watch_action_id": { "type": "string", "minLength": 1 },
                "mode": { "type": "string", "enum": ["tool", "continue"] },
                "title": { "type": "string", "minLength": 1 },
                "body": { "type": "string", "minLength": 1 },
                "tool_name": { "type": "string", "minLength": 1 },
                "tool_args": { "type": "object" },
                "prompt": { "type": "string", "minLength": 1 },
                "action_id": { "type": "string", "minLength": 1 }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "list" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "cancel" }, "action_id": { "type": "string", "minLength": 1 } },
                    "required": ["operation", "action_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "set" },
                        "delay_secs": { "type": "integer", "minimum": 1, "maximum": 86400 },
                        "due_at": { "type": "string", "minLength": 1 },
                        "mode": { "type": "string", "enum": ["tool", "continue"] },
                        "title": { "type": "string", "minLength": 1 },
                        "body": { "type": "string", "minLength": 1 },
                        "tool_name": { "type": "string", "minLength": 1 },
                        "tool_args": { "type": "object" },
                        "prompt": { "type": "string", "minLength": 1 }
                    },
                    "required": ["operation", "body"],
                    "oneOf": [
                        { "required": ["delay_secs"], "not": { "anyOf": [{ "required": ["due_at"] }, { "required": ["watch_action_id"] }] } },
                        { "required": ["due_at"], "not": { "required": ["delay_secs"] } }
                    ],
                    "allOf": [
                        {
                            "if": { "properties": { "mode": { "const": "continue" } } },
                            "then": {
                                "required": ["prompt"],
                                "not": { "anyOf": [{ "required": ["tool_name"] }, { "required": ["tool_args"] }] }
                            }
                        },
                        {
                            "if": { "not": { "required": ["mode"] } },
                            "then": { "required": ["tool_name"], "not": { "required": ["prompt"] } }
                        },
                        {
                            "if": { "properties": { "mode": { "const": "tool" } } },
                            "then": { "required": ["tool_name"], "not": { "required": ["prompt"] } }
                        }
                    ]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "set" },
                        "watch_action_id": { "type": "string", "minLength": 1 },
                        "mode": { "const": "continue" },
                        "title": { "type": "string", "minLength": 1 },
                        "body": { "type": "string", "minLength": 1 },
                        "prompt": { "type": "string", "minLength": 1 }
                    },
                    "required": ["operation", "watch_action_id", "mode", "body", "prompt"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into
    /// `ScheduledActionParams`, then land in the same implementation as
    /// entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<ScheduledActionParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActionCompletion, ActionCompletionReceiver, Tool, ToolRegistry};
    use haven_memory::Database;
    use serde_json::json;
    use std::sync::Mutex;
    use std::time::Duration;

    async fn recv_scheduled(rx: &mut ActionCompletionReceiver) -> ScheduledActionFired {
        loop {
            match rx.recv().await {
                Some(ActionCompletion::Scheduled(fired)) => return fired,
                Some(ActionCompletion::Background(_)) => continue,
                None => panic!("action completion channel closed"),
            }
        }
    }

    fn make_tool() -> ScheduledActionTool {
        ScheduledActionTool {
            service: Arc::new(ActionService::new()),
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
    async fn test_native_set_rejects_negative_delay_before_cast() {
        let err = make_tool()
            .run(
                ScheduledActionParams {
                    operation: ScheduleOperation::Set,
                    delay_secs: Some(-1),
                    due_at: None,
                    watch_action_id: None,
                    mode: None,
                    title: Some("Test".into()),
                    body: Some("body".into()),
                    tool_name: None,
                    tool_args: None,
                    prompt: None,
                    action_id: None,
                    session_id: None,
                },
                CancellationToken::new(),
            )
            .await
            .expect_err("negative native delay must be rejected");
        assert!(err.to_string().contains("between 1 and 86400"));
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
                    "tool_name": "files",
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
        let center = Arc::new(ActionService::new());
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
    async fn test_watch_uses_the_unified_action_service() {
        // A dependency is owned by the same ActionService as its producer.
        // Unknown producers are retained as an in-memory dependency and resolve
        // to a not_found result rather than hanging forever.
        let center = Arc::new(ActionService::new());
        let id = center
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
            .await
            .expect("unified service accepts a dependency action");
        assert_eq!(center.list().await[0]["watch_action_id"], "action-1");
        assert!(id.starts_with("act-"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_watch_action_fires_with_result_when_action_finishes() {
        let center = Arc::new(ActionService::new());
        let mut rx = center.take_action_receiver().expect("receiver available");

        let action_id = center
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
        let fired = tokio::time::timeout(Duration::from_secs(15), recv_scheduled(&mut rx))
            .await
            .expect("timed out waiting for action-watch fire");
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
        let center = Arc::new(ActionService::new());
        center.set_db(Some(db.clone())).await;
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
            .await
            .unwrap();
        let tool = ScheduledActionTool {
            service: Arc::new(ActionService::new()),
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
            .await
            .unwrap();
        registry
            .register(Arc::new(DummyTool {
                name: "notify",
                risk: RiskLevel::Safe,
            }))
            .await
            .unwrap();
        let tool = ScheduledActionTool {
            service: Arc::new(ActionService::new()),
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
        let center = Arc::new(ActionService::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_action_receiver().expect("receiver available");

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
        let fired = tokio::time::timeout(Duration::from_secs(5), recv_scheduled(&mut rx))
            .await
            .expect("timed out waiting for scheduled_action");
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
        let center = Arc::new(ActionService::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_action_receiver().expect("receiver available");

        let id = center
            .set(ScheduledActionSpec {
                due_at: None,
                delay_secs: Some(1),
                watch_action_id: None,
                title: "Backup".into(),
                body: "running backup".into(),
                mode: ScheduleMode::Tool,
                session_id: Some("ses-1".into()),
                tool_name: Some("files".into()),
                tool_args: Some(json!({"operation": "read", "path": "C:/x"})),
                prompt: None,
            })
            .await
            .unwrap();

        // Persisted with the tool payload.
        let pending = db.list_pending_scheduled_actions().unwrap();
        assert_eq!(pending[0].mode, "tool");
        assert_eq!(pending[0].tool_name.as_deref(), Some("files"));
        assert!(pending[0].tool_args.as_deref().unwrap().contains("C:/x"));

        // Fires with the payload attached.
        let fired = tokio::time::timeout(Duration::from_secs(5), recv_scheduled(&mut rx))
            .await
            .expect("timed out waiting for scheduled_action");
        assert_eq!(fired.action_id, id);
        assert_eq!(fired.mode, ScheduleMode::Tool);
        assert_eq!(fired.tool_name.as_deref(), Some("files"));
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
                    "tool_name": "notify",
                    "_session_id": "ses-test"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let id = result.output["id"].as_str().unwrap().to_string();

        let list = tool
            .execute(
                json!({"operation": "list", "_session_id": "ses-test"}),
                CancellationToken::new(),
            )
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
                json!({"operation": "cancel", "action_id": id, "_session_id": "ses-test"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(cancelled.output["cancelled"], json!(id));

        // Cancelling again fails.
        let err = tool
            .execute(
                json!({"operation": "cancel", "action_id": id, "_session_id": "ses-test"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_err());

        // List is empty after cancel.
        let list = tool
            .execute(
                json!({"operation": "list", "_session_id": "ses-test"}),
                CancellationToken::new(),
            )
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
        let center = Arc::new(ActionService::new());
        let mut rx = center.take_action_receiver().expect("receiver available");
        let tool = ScheduledActionTool {
            service: center.clone(),
            registry: None,
        };
        let id = center.set(tool_spec(1, "Test", "fire now")).await.unwrap();
        let fired = tokio::time::timeout(Duration::from_secs(5), recv_scheduled(&mut rx))
            .await
            .expect("timed out waiting for scheduled_action");
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
        let center = Arc::new(ActionService::new());
        center.set_db(Some(db.clone())).await;
        let mut rx = center.take_action_receiver().expect("receiver available");

        // A future scheduled_action (5s out) and an overdue one (already past).
        let future_id = center.set(tool_spec(5, "Future", "later")).await.unwrap();
        let overdue_id = haven_common::types::new_id("act");
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
        let restored = Arc::new(ActionService::new());
        restored.set_db(Some(db.clone())).await;
        let mut rx2 = restored.take_action_receiver().expect("receiver available");
        let overdue_count = restored.restore_pending().await;
        assert_eq!(overdue_count, 1, "exactly one scheduled_action was overdue");

        // Overdue scheduled_action fired immediately with its mode payload.
        let fired = tokio::time::timeout(Duration::from_secs(5), recv_scheduled(&mut rx2))
            .await
            .expect("timed out waiting for overdue fire");
        assert_eq!(fired.action_id, overdue_id);
        assert_eq!(fired.title, "Overdue");
        assert_eq!(fired.mode, ScheduleMode::Continue);
        assert_eq!(fired.session_id.as_deref(), Some("ses-1"));
        assert_eq!(fired.prompt.as_deref(), Some("keep going"));

        // Future scheduled_action re-armed and fires after its remaining delay.
        let fired = tokio::time::timeout(Duration::from_secs(10), recv_scheduled(&mut rx))
            .await
            .expect("timed out waiting for future fire");
        assert_eq!(fired.action_id, future_id);

        // Both are marked fired in the DB; pending list is empty.
        assert!(db.list_pending_scheduled_actions().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_restore_pending_skips_corrupt_rows_without_defaults() {
        let (db, _dir) = test_db();
        let center = Arc::new(ActionService::new());
        center.set_db(Some(db.clone())).await;

        db.save_scheduled_action(
            "act-invalid-due",
            "not-a-timestamp",
            "Bad due",
            "must not run",
            "continue",
            Some("ses-1"),
            None,
            None,
            Some("prompt"),
        )
        .unwrap();
        db.save_scheduled_action(
            "act-invalid-mode",
            &(chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "Bad mode",
            "must not run",
            "unknown-mode",
            None,
            None,
            None,
            None,
        )
        .unwrap();
        db.save_scheduled_action(
            "act-invalid-args",
            &(chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            "Bad args",
            "must not run",
            "tool",
            None,
            Some("notify"),
            Some("{not-json"),
            None,
        )
        .unwrap();

        assert_eq!(center.restore_pending().await, 0);
        assert!(center.list().await.is_empty());
        assert_eq!(db.list_pending_scheduled_actions().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_reminder_set_persists_to_db() {
        let (db, _dir) = test_db();
        let center = Arc::new(ActionService::new());
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
        let center = Arc::new(ActionService::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = events.clone();
        center.set_event_sink(Arc::new(move |name, payload| {
            sink_events.lock().unwrap().push((name, payload));
        }));
        let mut rx = center.take_action_receiver().expect("receiver available");

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
        let fired = tokio::time::timeout(Duration::from_secs(5), recv_scheduled(&mut rx))
            .await
            .expect("timed out waiting for fire");
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
