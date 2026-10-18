use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::canonical_entity_id_schema;
use crate::ScheduleService;
use crate::registry::RegistryProbe;
use crate::{Tool, ToolConcurrency, ToolResult};
use haven_common::schedule::ScheduleMode;
use haven_memory::{SchedulePlanInput, ScheduleType};

/// Create, list, and cancel durable Schedule plans.
///
/// Fire behaviors are selected via `mode`:
/// - `tool` (default): call the tool in `tool_name` with `tool_args`; use the
///   `notify` tool with `{title, body}` to send a message at fire time.
/// - `continue`: resume the associated session, delivering `prompt` as a
///   continuation instruction in that session.
/// - `new_session`: start a fresh session from `prompt`; add
///   `repeat_interval_secs` to persist a recurring schedule.
pub(crate) struct ScheduleTool {
    pub(crate) service: Arc<ScheduleService>,
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

/// Typed parameters for `ScheduleTool`. Entry ① (native `run`) and
/// entry ② (`Tool::execute` with LLM JSON) both land in
/// `ScheduleTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ScheduleParams {
    /// set = schedule, list = pending, cancel = stop one.
    pub operation: ScheduleOperation,
    /// Delay in seconds before firing (set only).
    #[serde(default)]
    pub delay_secs: Option<i64>,
    /// Absolute fire time as ISO 8601 with an explicit timezone offset (set only).
    #[serde(default)]
    pub due_at: Option<String>,
    /// Set only: fire when the watched ToolRun reaches a terminal state.
    #[serde(default)]
    pub watch_tool_run_id: Option<String>,
    /// Behavior when it fires (set only): invoke a tool, resume this session, or start a new session.
    #[serde(default)]
    pub mode: Option<ScheduleMode>,
    /// Schedule title (defaults to 'Haven').
    #[serde(default)]
    pub title: Option<String>,
    /// User-visible summary shown in the task list and when it fires (set only).
    #[serde(default)]
    pub body: Option<String>,
    /// Tool to call when it fires (set only, mode=tool).
    #[serde(default)]
    pub tool_name: Option<String>,
    /// Arguments for the tool call (set only, mode=tool).
    #[serde(default)]
    pub tool_args: Option<Value>,
    /// Instruction delivered to the resumed or newly created session (set only).
    #[serde(default)]
    pub prompt: Option<String>,
    /// Repeat interval in seconds, allowed only with mode=new_session.
    #[serde(default)]
    pub repeat_interval_secs: Option<u64>,
    /// Schedule id returned by set (cancel only).
    #[serde(default)]
    pub schedule_id: Option<String>,
    /// Private owning session id, injected by the Tools facade.
    #[serde(default, rename = "_session_id")]
    pub session_id: Option<String>,
}

impl ScheduleTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ScheduleParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        match params.operation {
            ScheduleOperation::Set => {
                let delay = params.delay_secs;
                let due_at = params.due_at;
                let watch_tool_run_id = params.watch_tool_run_id;
                if let Some(watch_tool_run_id) = watch_tool_run_id.as_deref() {
                    crate::tool_contract::validate_entity_id(
                        watch_tool_run_id,
                        "watch_tool_run_id",
                        "toolrun",
                    )?;
                }
                let watch = watch_tool_run_id.as_deref();
                if watch.is_some() && (delay.is_some() || due_at.is_some()) {
                    anyhow::bail!("watch_tool_run_id cannot be combined with delay_secs or due_at");
                }
                if delay.is_none() && due_at.is_none() && watch.is_none() {
                    anyhow::bail!(
                        "one of delay_secs, due_at or watch_tool_run_id is required for set"
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
                let repeat_interval_secs = params.repeat_interval_secs;
                if repeat_interval_secs.is_some() && mode != ScheduleMode::NewSession {
                    anyhow::bail!("repeat_interval_secs requires mode 'new_session'");
                }
                if watch.is_some() && mode != ScheduleMode::Continue {
                    anyhow::bail!(
                        "watch_tool_run_id requires mode 'continue' (the schedule fires by resuming the session with the ToolRun result)"
                    );
                }
                // `_session_id` is injected privately by ToolsFacade and is the
                // durable owner for every Schedule plan.
                let session_id = params
                    .session_id
                    .ok_or_else(|| anyhow::anyhow!("schedule set requires a session context"))?;
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
                    ScheduleMode::NewSession => {
                        if params
                            .prompt
                            .as_deref()
                            .is_none_or(|prompt| prompt.trim().is_empty())
                        {
                            anyhow::bail!("prompt is required when mode is 'new_session'");
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
                let prompt = params
                    .prompt
                    .map(|prompt| prompt.trim().to_string())
                    .filter(|prompt| !prompt.is_empty());
                let due_at = due_at.or_else(|| {
                    delay.map(|delay| {
                        (chrono::Utc::now() + chrono::Duration::seconds(delay))
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                    })
                });
                let plan = self
                    .service
                    .create(SchedulePlanInput {
                        owner_session_id: session_id,
                        schedule_type: if repeat_interval_secs.is_some() {
                            ScheduleType::RecurringSession
                        } else {
                            ScheduleType::Once
                        },
                        title: title.to_string(),
                        body: body.to_string(),
                        mode,
                        due_at,
                        watch_tool_run_id: watch_tool_run_id.clone(),
                        tool_name,
                        tool_args,
                        prompt,
                        interval_seconds: repeat_interval_secs,
                    })
                    .await?;
                let fires_at = plan
                    .next_due_at
                    .clone()
                    .or_else(|| plan.due_at.clone())
                    .unwrap_or_default();
                let mut output = serde_json::json!({
                    "operation": "set",
                    "schedule_id": plan.schedule_id,
                    "schedule_run_id": plan.active_schedule_run_id,
                    "mode": mode.as_str(),
                    "fires_at": fires_at,
                    "wakes_session": mode == ScheduleMode::Continue,
                    "creates_session": mode == ScheduleMode::NewSession,
                    "repeat_interval_secs": repeat_interval_secs,
                    "note": "Schedule plans and occurrences persist independently from background ToolRuns; overdue work is checked after app startup.",
                });
                if repeat_interval_secs.is_some() {
                    output["note"] = serde_json::json!(
                        "This recurring Schedule creates a fresh session on each fixed UTC interval. Missed ticks are skipped."
                    );
                }
                if let Some(tool_run_id) = &watch_tool_run_id {
                    output["watch_tool_run_id"] = serde_json::json!(tool_run_id);
                    output["note"] = serde_json::json!(format!(
                        "Fires when background ToolRun {tool_run_id} reaches a terminal state, then resumes this session with its result."
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
                let plans = self.service.list_for_session(session_id).await?;
                let rows = plans
                    .into_iter()
                    .map(|plan| {
                        serde_json::json!({
                            "schedule_id": plan.schedule_id,
                            "schedule_type": plan.schedule_type,
                            "title": plan.title,
                            "body": plan.body,
                            "mode": plan.mode,
                            "due_at": plan.due_at,
                            "watch_tool_run_id": plan.watch_tool_run_id,
                            "tool_name": plan.tool_name,
                            "tool_args": plan.tool_args,
                            "prompt": plan.prompt,
                            "interval_seconds": plan.interval_seconds,
                            "next_due_at": plan.next_due_at,
                            "active_schedule_run_id": plan.active_schedule_run_id,
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(ToolResult::ok(
                    serde_json::json!({ "operation": "list", "schedules": rows }),
                ))
            }
            ScheduleOperation::Cancel => {
                let session_id = params
                    .session_id
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("schedule cancel requires a session context"))?;
                let schedule_id = params
                    .schedule_id
                    .ok_or_else(|| anyhow::anyhow!("schedule_id is required for cancel"))?;
                crate::tool_contract::validate_entity_id(&schedule_id, "schedule_id", "sched")?;
                if self.service.cancel(&schedule_id, session_id).await? {
                    Ok(ToolResult::ok(
                        serde_json::json!({ "operation": "cancel", "schedule_id": schedule_id }),
                    ))
                } else {
                    anyhow::bail!(
                        "Schedule '{}' not found or execution already claimed",
                        schedule_id
                    )
                }
            }
        }
    }
}

#[async_trait]
impl Tool for ScheduleTool {
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
            ToolConcurrency::SharedResource("schedules".into())
        } else {
            ToolConcurrency::Resource("schedules".into())
        }
    }

    /// Needs the private `_session_id` input for ownership and continue mode.
    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["set", "list", "cancel"], "description": "Schedule future work, list schedules, or cancel one" },
                "delay_secs": { "type": "integer", "minimum": 1, "maximum": 86400, "description": "Fire after this many seconds; choose this or due_at, not both" },
                "due_at": { "type": "string", "minLength": 1, "description": "Absolute fire time as an ISO 8601 timestamp with an explicit timezone offset; choose this or delay_secs, not both" },
                "watch_tool_run_id": canonical_entity_id_schema("toolrun", Some("Schedule continuation when this background task reaches a terminal state; requires mode=continue")),
                "mode": { "type": "string", "enum": ["tool", "continue", "new_session"], "default": "tool", "description": "Call tool_name, resume this session, or create a fresh session from a self-contained prompt; a fresh session has no source-session transcript" },
                "repeat_interval_secs": { "type": "integer", "minimum": 60, "maximum": 31536000, "description": "Repeat the same new-session prompt at this fixed number of seconds (mode=new_session only; missed intervals are skipped; local wall-clock times are unsupported)" },
                "title": { "type": "string", "minLength": 1, "description": "Short title shown for the scheduled task; defaults to Haven" },
                "body": { "type": "string", "minLength": 1, "description": "Required short user-visible summary shown when the schedule fires; the execution instruction belongs in prompt" },
                "tool_name": { "type": "string", "minLength": 1, "description": "Available built-in tool to call when mode=tool" },
                "tool_args": { "type": "object", "description": "Arguments for tool_name when mode=tool; omit or use an empty object for no arguments" },
                "prompt": { "type": "string", "minLength": 1, "description": "Execution instruction for the resumed or newly created session; for new_session write a self-contained task because it has no source-session transcript" },
                "schedule_id": canonical_entity_id_schema("sched", Some("schedule plan id returned by schedule.set; cancel stops future/unclaimed occurrences"))
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
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "cancel" }, "schedule_id": canonical_entity_id_schema("sched", Some("Schedule plan id returned by schedule.set")) },
                    "required": ["operation", "schedule_id"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "set" },
                        "delay_secs": { "type": "integer", "minimum": 1, "maximum": 86400, "description": "Fire after this many seconds; choose this or due_at, not both" },
                        "due_at": { "type": "string", "minLength": 1, "description": "Absolute fire time as an ISO 8601 timestamp with an explicit timezone offset; choose this or delay_secs, not both" },
                        "mode": { "type": "string", "enum": ["tool", "continue", "new_session"], "default": "tool", "description": "Call tool_name, resume this session, or create a fresh session from a self-contained prompt; a fresh session has no source-session transcript" },
                        "repeat_interval_secs": { "type": "integer", "minimum": 60, "maximum": 31536000, "description": "Repeat the same new-session prompt at this fixed number of seconds (mode=new_session only; missed intervals are skipped; local wall-clock times are unsupported)" },
                        "title": { "type": "string", "minLength": 1, "description": "Short task title; defaults to Haven" },
                        "body": { "type": "string", "minLength": 1, "description": "Required short user-visible summary shown when the schedule fires; the execution instruction belongs in prompt" },
                        "tool_name": { "type": "string", "minLength": 1, "description": "Available built-in tool to call when mode=tool" },
                        "tool_args": { "type": "object", "description": "Arguments for tool_name when mode=tool; omit or use an empty object for no arguments" },
                        "prompt": { "type": "string", "minLength": 1, "description": "Execution instruction for the resumed or newly created session; for new_session write a self-contained task because it has no source-session transcript" }
                    },
                    "required": ["operation", "body"],
                    "oneOf": [
                        { "required": ["delay_secs"], "not": { "anyOf": [{ "required": ["due_at"] }, { "required": ["watch_tool_run_id"] }] } },
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
                        },
                        {
                            "if": { "properties": { "mode": { "const": "new_session" } } },
                            "then": { "required": ["prompt"], "not": { "anyOf": [{ "required": ["tool_name"] }, { "required": ["tool_args"] }] } }
                        },
                        {
                            "if": { "required": ["repeat_interval_secs"] },
                            "then": { "properties": { "mode": { "const": "new_session" } }, "required": ["mode", "prompt"] }
                        }
                    ]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "set" },
                        "watch_tool_run_id": canonical_entity_id_schema("toolrun", Some("Fire when this background task completes, fails, or is cancelled")),
                        "mode": { "const": "continue" },
                        "title": { "type": "string", "minLength": 1, "description": "Short task title; defaults to Haven" },
                        "body": { "type": "string", "minLength": 1, "description": "Required message shown when the background task finishes" },
                        "prompt": { "type": "string", "minLength": 1, "description": "Instruction delivered to this session with the completed task result" }
                    },
                    "required": ["operation", "watch_tool_run_id", "mode", "body", "prompt"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into
    /// `ScheduleParams`, then land in the same implementation as
    /// entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<ScheduleParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

#[cfg(test)]
#[path = "schedule_tool_tests.rs"]
mod tests;
