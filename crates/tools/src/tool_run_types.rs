//! Domain types shared by ToolRunService and its callers.
//!
//! Tool-specific input and execution types remain in `builtin::scheduled_tool_run`.

use haven_common::tool_run::ScheduleMode;
use serde_json::Value;

/// Everything ToolRunService needs to admit one scheduled ToolRun.
pub struct ScheduledToolRunSpec {
    /// Absolute fire time as RFC3339 with an explicit timezone offset. Use
    /// this OR `delay_secs` OR `watch_tool_run_id`; exactly one is required.
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
    /// Owning session, injected by `ToolsFacade` rather than the LLM.
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
///
/// Tools owns this event's lifecycle and constructs it when the fire
/// transition is published. Consumers should treat it as a received contract.
#[non_exhaustive]
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
