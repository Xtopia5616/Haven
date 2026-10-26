use crate::ToolRunKind;
use haven_common::lifecycle::ToolRunStatus;
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
/// is the only terminal state that may lack a start time because a child
/// process can be cancelled before it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolRunLifecycleState {
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
            Self::Running { .. } => ToolRunStatus::Running,
            Self::Completed { .. } => ToolRunStatus::Completed,
            Self::Failed { .. } => ToolRunStatus::Failed,
            Self::Cancelled { .. } => ToolRunStatus::Cancelled,
        }
    }

    pub fn started_at(&self) -> Option<&str> {
        match self {
            Self::Running { started_at }
            | Self::Completed { started_at, .. }
            | Self::Failed { started_at, .. } => Some(started_at),
            Self::Cancelled { started_at, .. } => started_at.as_deref(),
        }
    }

    pub fn finished_at(&self) -> Option<&str> {
        match self {
            Self::Running { .. } => None,
            Self::Completed { finished_at, .. }
            | Self::Failed { finished_at, .. }
            | Self::Cancelled { finished_at, .. } => Some(finished_at),
        }
    }
}

/// Safe lifecycle fields shared by creation, update, and terminal events.
/// Fields that do not apply to a particular kind or event remain `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolRunLifecyclePayload {
    pub kind: ToolRunKind,
    pub tool_run_id: String,
    pub tool_name: String,
    pub state: ToolRunLifecycleState,
    pub session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
}

impl ToolRunLifecyclePayload {
    pub(crate) fn new(
        kind: ToolRunKind,
        tool_run_id: impl Into<String>,
        tool_name: impl Into<String>,
        state: ToolRunLifecycleState,
    ) -> Self {
        Self {
            kind,
            tool_run_id: tool_run_id.into(),
            tool_name: tool_name.into(),
            state,
            session_id: None,
            source_step_id: None,
            output: None,
            error: None,
            error_reason: None,
        }
    }

    /// Construct a ToolRun event fixture for consumers that test App projection.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn new_for_test(
        kind: ToolRunKind,
        tool_run_id: impl Into<String>,
        tool_name: impl Into<String>,
        state: ToolRunLifecycleState,
    ) -> Self {
        Self::new(kind, tool_run_id, tool_name, state)
    }
}

/// A metadata-only background ToolRun update emitted after session binding.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolRunSessionAttachedPayload {
    pub tool_run_id: String,
    pub tool_name: String,
    pub session_id: String,
    pub source_step_id: Option<String>,
    /// State observed after the owner binding commits. Consumers may use this
    /// to recover lifecycle notifications when a ToolRun finished before it
    /// could be attached to its session.
    pub status: Option<ToolRunStatus>,
}

impl ToolRunSessionAttachedPayload {
    /// Construct a session-attachment event fixture for App projection tests.
    #[cfg(feature = "test-support")]
    #[doc(hidden)]
    pub fn new_for_test(
        tool_run_id: impl Into<String>,
        tool_name: impl Into<String>,
        session_id: impl Into<String>,
        source_step_id: Option<String>,
    ) -> Self {
        Self {
            tool_run_id: tool_run_id.into(),
            tool_name: tool_name.into(),
            session_id: session_id.into(),
            source_step_id,
            status: None,
        }
    }
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
                "tool_name": payload.tool_name,
                "kind": match payload.kind {
                    ToolRunKind::Background => "background",
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
                ("output", payload.output),
                ("error", payload.error),
                ("error_reason", payload.error_reason),
            ] {
                set_optional_string(&mut value, key, field);
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
                    "tool_name": payload.tool_name,
                    "kind": "background",
                    "session_id": payload.session_id,
                });
                set_optional_string(&mut value, "source_step_id", payload.source_step_id);
                ("tool_run:updated".into(), value)
            }
            Self::Finished(payload) => ("tool_run:finished".into(), lifecycle_payload(payload)),
        }
    }
}
