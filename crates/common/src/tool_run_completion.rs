//! Canonical payload for delivering a terminal ToolRun result.

use serde::{Deserialize, Serialize};

use crate::ToolRunStatus;

/// Result fields shared by live ToolRun completion events and the durable
/// completion outbox. This is an internal delivery contract, not the ToolRun
/// status query's UI projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRunCompletionPayload {
    pub tool_run_id: String,
    pub status: ToolRunStatus,
    /// Background status projection label retained under the legacy JSON key.
    /// Completion kind ownership remains with the Tools runtime/event variant.
    #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
    pub status_projection_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_step_id: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}
