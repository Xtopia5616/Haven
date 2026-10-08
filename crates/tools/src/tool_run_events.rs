use crate::{ScheduleMode, ToolRunKind};
use haven_common::ToolRunStatus;
#[cfg(test)]
use serde_json::{Value, json};

/// Closed set of lifecycle events emitted by `ToolRunService`.
///
/// Event names and payload fields stay together in this typed contract. The
/// App crate owns the separate Tauri DTO and projects only its approved fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRunLifecycleEvent {
    Created(ToolRunLifecyclePayload),
    Updated(ToolRunLifecycleUpdate),
    Output(ToolRunOutputPayload),
    Finished(ToolRunLifecyclePayload),
}

/// The two valid reasons for publishing `tool_run:updated`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRunLifecycleUpdate {
    StateChanged(Box<ToolRunLifecyclePayload>),
    SessionAttached(ToolRunSessionAttachedPayload),
}

/// Lifecycle status and its required timestamps travel as one value.
///
/// A running event cannot be constructed without `started_at`. Cancellation
/// is the only terminal state that may lack a start time because a scheduled
/// ToolRun can be cancelled while it is still waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRunLifecycleState {
    Waiting,
    Running {
        started_at: String,
    },
    Completed {
        started_at: String,
        finished_at: String,
    },
    Failed {
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: Option<String>,
        finished_at: String,
    },
}

impl ToolRunLifecycleState {
    pub fn status(&self) -> ToolRunStatus {
        match self {
            Self::Waiting => ToolRunStatus::Waiting,
            Self::Running { .. } => ToolRunStatus::Running,
            Self::Completed { .. } => ToolRunStatus::Completed,
            Self::Failed { .. } => ToolRunStatus::Failed,
            Self::Cancelled { .. } => ToolRunStatus::Cancelled,
        }
    }

    pub fn started_at(&self) -> Option<&str> {
        match self {
            Self::Waiting => None,
            Self::Running { started_at }
            | Self::Completed { started_at, .. }
            | Self::Failed { started_at, .. } => Some(started_at),
            Self::Cancelled { started_at, .. } => started_at.as_deref(),
        }
    }

    pub fn finished_at(&self) -> Option<&str> {
        match self {
            Self::Waiting | Self::Running { .. } => None,
            Self::Completed { finished_at, .. }
            | Self::Failed { finished_at, .. }
            | Self::Cancelled { finished_at, .. } => Some(finished_at),
        }
    }
}

/// Safe lifecycle fields shared by creation, update, and terminal events.
/// Fields that do not apply to a particular kind or event remain `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunLifecyclePayload {
    pub kind: ToolRunKind,
    pub tool_run_id: String,
    pub state: ToolRunLifecycleState,
    pub session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub due_at: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub mode: Option<ScheduleMode>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub exit_code: Option<i32>,
}

impl ToolRunLifecyclePayload {
    pub fn new(
        kind: ToolRunKind,
        tool_run_id: impl Into<String>,
        state: ToolRunLifecycleState,
    ) -> Self {
        Self {
            kind,
            tool_run_id: tool_run_id.into(),
            state,
            session_id: None,
            source_step_id: None,
            due_at: None,
            title: None,
            body: None,
            mode: None,
            output: None,
            error: None,
            error_reason: None,
            exit_code: None,
        }
    }
}

/// A metadata-only background ToolRun update emitted after session binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunSessionAttachedPayload {
    pub tool_run_id: String,
    pub session_id: String,
    pub source_step_id: Option<String>,
}

/// Bounded preview event for a running background ToolRun.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunOutputPayload {
    pub tool_run_id: String,
    pub source_step_id: Option<String>,
    pub output: String,
}

impl ToolRunLifecycleEvent {
    #[cfg(test)]
    pub(crate) fn into_test_parts(self) -> (String, Value) {
        fn set_optional_string(value: &mut Value, key: &str, field: Option<String>) {
            if let Some(field) = field {
                value[key] = json!(field);
            }
        }

        fn lifecycle_payload(payload: ToolRunLifecyclePayload) -> Value {
            let mut value = json!({
                "tool_run_id": payload.tool_run_id,
                "kind": match payload.kind {
                    ToolRunKind::Background => "background",
                    ToolRunKind::Scheduled => "scheduled",
                },
            });
            value["status"] = json!(payload.state.status().as_str());
            if let Some(started_at) = payload.state.started_at() {
                value["started_at"] = json!(started_at);
            }
            if let Some(finished_at) = payload.state.finished_at() {
                value["finished_at"] = json!(finished_at);
            }
            for (key, field) in [
                ("session_id", payload.session_id),
                ("source_step_id", payload.source_step_id),
                ("due_at", payload.due_at),
                ("title", payload.title),
                ("body", payload.body),
                ("output", payload.output),
                ("error", payload.error),
                ("error_reason", payload.error_reason),
            ] {
                set_optional_string(&mut value, key, field);
            }
            if let Some(mode) = payload.mode {
                value["mode"] = json!(mode);
            }
            if let Some(exit_code) = payload.exit_code {
                value["exit_code"] = json!(exit_code);
            }
            value
        }

        match self {
            Self::Created(payload) => ("tool_run:created".into(), lifecycle_payload(payload)),
            Self::Updated(ToolRunLifecycleUpdate::StateChanged(payload)) => {
                ("tool_run:updated".into(), lifecycle_payload(*payload))
            }
            Self::Updated(ToolRunLifecycleUpdate::SessionAttached(payload)) => {
                let mut value = json!({
                    "tool_run_id": payload.tool_run_id,
                    "kind": "background",
                    "session_id": payload.session_id,
                });
                set_optional_string(&mut value, "source_step_id", payload.source_step_id);
                ("tool_run:updated".into(), value)
            }
            Self::Output(payload) => {
                let mut value = json!({
                    "tool_run_id": payload.tool_run_id,
                    "status": "running",
                    "output": payload.output,
                });
                set_optional_string(&mut value, "source_step_id", payload.source_step_id);
                ("tool_run:output".into(), value)
            }
            Self::Finished(payload) => ("tool_run:finished".into(), lifecycle_payload(payload)),
        }
    }
}
