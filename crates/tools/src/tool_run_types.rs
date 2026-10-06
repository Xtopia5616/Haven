//! Domain types shared by ToolRunService and its callers.
//!
//! Tool-specific input and execution types remain in `builtin::scheduled_tool_run`.

use serde_json::Value;

/// What happens when a scheduled ToolRun fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScheduleMode {
    /// Call the tool in `tool_name` with `tool_args` (no LLM involved).
    /// To send a message at fire time, call the `notify` tool here.
    #[default]
    Tool,
    /// Resume the session that scheduled this ToolRun, delivering its prompt as
    /// a new instruction in the same conversation. This is only available while
    /// the owning session is live; ending or removing that session cancels it.
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

/// Everything ToolRunService needs to admit one scheduled ToolRun.
pub struct ScheduledToolRunSpec {
    /// Absolute fire time (RFC3339, local time accepted). Use this OR
    /// `delay_secs` OR `watch_tool_run_id`; exactly one is required.
    pub due_at: Option<String>,
    /// Delay in seconds before the ToolRun fires. Use this OR `due_at` OR
    /// `watch_tool_run_id`.
    pub delay_secs: Option<u64>,
    /// ToolRun to wait for; this schedule fires when the producer reaches a terminal state
    /// (completed/failed/cancelled), resuming the session with its terminal
    /// status and available result. The relation is persisted and its watcher
    /// is restored after restart. Use this OR `due_at` OR `delay_secs`.
    pub watch_tool_run_id: Option<String>,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    /// Owning session, injected by the tool manager rather than the LLM.
    pub session_id: Option<String>,
    /// Tool called when `mode` is `Tool`.
    pub tool_name: Option<String>,
    /// Arguments for `tool_name` when `mode` is `Tool`.
    pub tool_args: Option<Value>,
    /// Continuation message delivered when `mode` is `Continue`.
    pub prompt: Option<String>,
}

/// Typed payload published when a scheduled ToolRun enters its fire transition.
/// The agent acknowledges the actual work with `complete_scheduled` or
/// `fail_scheduled`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScheduledToolRunFired {
    pub tool_run_id: String,
    pub title: String,
    pub body: String,
    pub mode: ScheduleMode,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<Value>,
    pub prompt: Option<String>,
}
