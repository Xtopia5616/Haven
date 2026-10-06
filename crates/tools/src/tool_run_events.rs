use crate::ToolRunKind;
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
    Updated(ToolRunLifecyclePayload),
    Output(ToolRunOutputPayload),
    Finished(ToolRunLifecyclePayload),
}

/// Safe lifecycle fields shared by creation, update, and terminal events.
/// Fields that do not apply to a particular kind or event remain `None`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunLifecyclePayload {
    pub kind: ToolRunKind,
    pub tool_run_id: String,
    pub status: Option<ToolRunStatus>,
    pub session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub due_at: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub mode: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub exit_code: Option<i32>,
}

impl ToolRunLifecyclePayload {
    pub fn new(kind: ToolRunKind, tool_run_id: impl Into<String>) -> Self {
        Self {
            kind,
            tool_run_id: tool_run_id.into(),
            status: None,
            session_id: None,
            source_step_id: None,
            started_at: None,
            finished_at: None,
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
            let is_scheduled = payload.kind == ToolRunKind::Scheduled;
            let mut value = json!({
                "tool_run_id": payload.tool_run_id,
                "kind": match payload.kind {
                    ToolRunKind::Background => "background",
                    ToolRunKind::Scheduled => "scheduled",
                },
            });
            if is_scheduled {
                value["id"] = value["tool_run_id"].clone();
            }
            if let Some(status) = payload.status {
                value["status"] = json!(status.as_str());
            }
            for (key, field) in [
                ("session_id", payload.session_id),
                ("source_step_id", payload.source_step_id),
                ("started_at", payload.started_at),
                ("finished_at", payload.finished_at),
                ("due_at", payload.due_at),
                ("title", payload.title),
                ("body", payload.body),
                ("mode", payload.mode),
                ("output", payload.output),
                ("error", payload.error),
                ("error_reason", payload.error_reason),
            ] {
                set_optional_string(&mut value, key, field);
            }
            if let Some(exit_code) = payload.exit_code {
                value["exit_code"] = json!(exit_code);
            }
            value
        }

        match self {
            Self::Created(payload) => ("tool_run:created".into(), lifecycle_payload(payload)),
            Self::Updated(payload) => ("tool_run:updated".into(), lifecycle_payload(payload)),
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
