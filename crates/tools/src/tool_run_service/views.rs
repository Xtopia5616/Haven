use super::*;
use haven_common::ToolRunCompletionPayload;

/// Safe, typed projection of one in-memory task board row.
///
/// This contains only fields used by the task panel. Execution internals such
/// as shell metadata, log paths, scheduled tool arguments, continuation
/// prompts, and dependency watch ids deliberately stay in `ToolRunEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunView {
    pub tool_run_id: String,
    pub kind: ToolRunKind,
    pub status: ToolRunStatus,
    pub session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub due_at: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub mode: Option<ScheduleMode>,
    pub command: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub preview: Option<String>,
}

/// Typed model-facing projection of a ToolRun status.
///
/// `ToolRunView` is intentionally a UI projection and therefore is not reused
/// here: the agent-facing shapes include live shell fields, completion output,
/// and scheduled ToolRun metadata. JSON conversion stays at the tool boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolRunStatusView {
    NotFound {
        tool_run_id: String,
    },
    Background {
        tool_run_id: String,
        source_step_id: Option<String>,
        state: ToolRunStateView,
    },
    Scheduled {
        tool_run_id: String,
        session_id: Option<String>,
        schedule: Box<ScheduledToolRunView>,
        state: ToolRunStateView,
    },
    ScheduledTerminal {
        tool_run_id: String,
        status: ToolRunStatus,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolRunStateView {
    Waiting,
    Running {
        started_at: String,
        command: Option<String>,
        shell: Option<String>,
        output: Option<String>,
    },
    Completed {
        output: String,
        exit_code: Option<i32>,
        truncated: bool,
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
        error_reason: String,
        log_path: Option<String>,
        exit_code: Option<i32>,
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: String,
        finished_at: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScheduledToolRunView {
    pub title: String,
    pub body: String,
    pub due_at: String,
    pub mode: ScheduleMode,
    pub tool_name: Option<String>,
    pub tool_args: Option<Value>,
    pub prompt: Option<String>,
    pub watch_tool_run_id: Option<String>,
}

/// A scoped list row. Its status remains typed until the tool_runs tool has
/// applied filtering and built its final JSON result.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRunListView {
    pub status: ToolRunStatus,
    pub projection: ToolRunStatusView,
    pub kind: ToolRunKind,
    pub session_id: Option<String>,
    pub preview: String,
}

impl ToolRunStateView {
    pub(super) fn from_entry(entry: &ToolRunEntry) -> Self {
        let mut view = Self::from_runtime_state(&entry.state);
        if let Self::Running {
            command,
            shell,
            output,
            ..
        } = &mut view
        {
            *command = (entry.kind == ToolRunKind::Background).then(|| entry.command.clone());
            *shell = (entry.kind == ToolRunKind::Background).then(|| entry.shell.clone());
            *output = entry.tail.as_ref().and_then(|tail| {
                let output = tail.snapshot();
                (!output.is_empty()).then(|| output.as_str().to_string())
            });
        }
        view
    }

    fn from_runtime_state(state: &ToolRunState) -> Self {
        match state {
            ToolRunState::Waiting => Self::Waiting,
            ToolRunState::Running { started_at } => Self::Running {
                started_at: started_at.clone(),
                command: None,
                shell: None,
                output: None,
            },
            ToolRunState::Completed {
                output,
                exit_code,
                truncated,
                log_path,
                started_at,
                finished_at,
            } => Self::Completed {
                output: output.clone(),
                exit_code: *exit_code,
                truncated: *truncated,
                log_path: log_path.clone(),
                started_at: started_at.clone(),
                finished_at: finished_at.clone(),
            },
            ToolRunState::Failed {
                error,
                error_reason,
                log_path,
                exit_code,
                started_at,
                finished_at,
            } => Self::Failed {
                error: error.clone(),
                error_reason: error_reason.clone(),
                log_path: log_path.clone(),
                exit_code: *exit_code,
                started_at: started_at.clone(),
                finished_at: finished_at.clone(),
            },
            ToolRunState::Cancelled {
                started_at,
                finished_at,
            } => Self::Cancelled {
                started_at: started_at.clone(),
                finished_at: finished_at.clone(),
            },
        }
    }

    fn status_json(&self, tool_run_id: &str) -> Value {
        match self {
            Self::Waiting => json!({
                "tool_run_id": tool_run_id,
                "status": "waiting",
            }),
            Self::Running {
                started_at,
                command,
                shell,
                output,
            } => {
                let mut value = serde_json::Map::new();
                value.insert("tool_run_id".into(), json!(tool_run_id));
                value.insert("status".into(), json!("running"));
                if let Some(command) = command {
                    value.insert("command".into(), json!(command));
                }
                if let Some(shell) = shell {
                    value.insert("shell".into(), json!(shell));
                }
                value.insert("started_at".into(), json!(started_at));
                if let Some(output) = output {
                    value.insert("output".into(), json!(output));
                }
                Value::Object(value)
            }
            Self::Completed {
                output,
                exit_code,
                truncated,
                log_path,
                started_at,
                finished_at,
            } => {
                let mut value = json!({
                    "tool_run_id": tool_run_id,
                    "status": "completed",
                    "output": output,
                    "started_at": started_at,
                    "finished_at": finished_at,
                });
                if let Some(code) = exit_code {
                    value["exit_code"] = json!(code);
                }
                if *truncated {
                    value["truncated"] = json!(true);
                }
                if let Some(path) = log_path {
                    value["log_path"] = json!(path);
                }
                value
            }
            Self::Failed {
                error,
                error_reason,
                log_path,
                exit_code,
                started_at,
                finished_at,
            } => {
                let mut value = json!({
                    "tool_run_id": tool_run_id,
                    "status": "failed",
                    "error": error,
                    "error_reason": error_reason,
                    "started_at": started_at,
                    "finished_at": finished_at,
                });
                if let Some(code) = exit_code {
                    value["exit_code"] = json!(code);
                }
                if let Some(path) = log_path {
                    value["log_path"] = json!(path);
                }
                value
            }
            Self::Cancelled {
                started_at,
                finished_at,
            } => json!({
                "tool_run_id": tool_run_id,
                "status": "cancelled",
                "started_at": started_at,
                "finished_at": finished_at,
            }),
        }
    }

    pub fn status(&self) -> ToolRunStatus {
        match self {
            Self::Waiting => ToolRunStatus::Waiting,
            Self::Running { .. } => ToolRunStatus::Running,
            Self::Completed { .. } => ToolRunStatus::Completed,
            Self::Failed { .. } => ToolRunStatus::Failed,
            Self::Cancelled { .. } => ToolRunStatus::Cancelled,
        }
    }

    pub(super) fn preview(&self) -> String {
        let source = match self {
            Self::Running { output, .. } => output.as_deref(),
            Self::Completed { output, .. } => Some(output.as_str()),
            Self::Failed { error, .. } => Some(error.as_str()),
            Self::Waiting | Self::Cancelled { .. } => None,
        };
        source.unwrap_or_default().chars().take(200).collect()
    }

    fn started_at(&self) -> Option<&str> {
        match self {
            Self::Running { started_at, .. }
            | Self::Completed { started_at, .. }
            | Self::Failed { started_at, .. }
            | Self::Cancelled { started_at, .. } => Some(started_at),
            Self::Waiting => None,
        }
    }
}

impl ToolRunStatusView {
    pub fn status(&self) -> Option<ToolRunStatus> {
        match self {
            Self::NotFound { .. } | Self::ScheduledTerminal { .. } => match self {
                Self::ScheduledTerminal { status, .. } => Some(*status),
                _ => None,
            },
            Self::Background { state, .. } | Self::Scheduled { state, .. } => Some(state.status()),
        }
    }

    pub(crate) fn to_json(&self, include_background_wait: bool) -> Value {
        match self {
            Self::NotFound { tool_run_id } => json!({
                "tool_run_id": tool_run_id,
                "status": "not_found",
            }),
            Self::ScheduledTerminal {
                tool_run_id,
                status,
            } => json!({
                "tool_run_id": tool_run_id,
                "status": status.as_str(),
            }),
            Self::Background {
                tool_run_id,
                source_step_id,
                state,
            } => {
                let mut value = state.status_json(tool_run_id);
                if include_background_wait && matches!(state, ToolRunStateView::Running { .. }) {
                    let mut background_wait = haven_common::tools::background_wait_object(
                        std::iter::once(tool_run_id.to_string()),
                        "The ToolRun is still running. END YOUR TURN if you have nothing else useful to do — do not poll. The result is auto-pushed and the session is auto-woken when it finishes.",
                    );
                    if let Some(status) = value.as_object() {
                        background_wait.extend(status.clone());
                    }
                    value = Value::Object(background_wait);
                }
                value["kind"] = json!("background");
                if let Some(source_step_id) = source_step_id {
                    value["source_step_id"] = json!(source_step_id);
                }
                value
            }
            Self::Scheduled {
                tool_run_id,
                session_id,
                schedule,
                state,
            } => {
                let mut value = json!({
                    "tool_run_id": tool_run_id,
                    "kind": "scheduled",
                    "status": state.status().as_str(),
                    "title": schedule.title,
                    "body": schedule.body,
                    "mode": schedule.mode,
                    "session_id": session_id,
                    "tool_name": schedule.tool_name,
                    "tool_args": schedule.tool_args,
                    "prompt": schedule.prompt,
                    "watch_tool_run_id": schedule.watch_tool_run_id,
                    "due_at": schedule.due_at,
                });
                if let ToolRunStateView::Running { started_at, .. } = state {
                    value["started_at"] = json!(started_at);
                }
                value
            }
        }
    }
}

impl ToolRunListView {
    pub(crate) fn to_json(&self) -> Value {
        let mut value = self.projection.to_json(false);
        if self.kind == ToolRunKind::Background {
            value["session_id"] = json!(self.session_id);
            value["kind"] = json!("background");
            value["preview"] = json!(self.preview);
        }
        value
    }
}

pub(super) fn list_view_started_at(view: &ToolRunListView) -> Option<&str> {
    match &view.projection {
        ToolRunStatusView::Background { state, .. }
        | ToolRunStatusView::Scheduled { state, .. } => state.started_at(),
        ToolRunStatusView::NotFound { .. } | ToolRunStatusView::ScheduledTerminal { .. } => None,
    }
}

pub(super) fn scheduled_tool_run_view(entry: &ScheduledToolRunEntry) -> ScheduledToolRunView {
    ScheduledToolRunView {
        title: entry.title.clone(),
        body: entry.body.clone(),
        due_at: entry.due_at.clone(),
        mode: entry.mode,
        tool_name: entry.tool_name.clone(),
        tool_args: entry.tool_args.clone(),
        prompt: entry.prompt.clone(),
        watch_tool_run_id: entry.watch_tool_run_id.clone(),
    }
}

pub(super) fn project_board_tool_run(tool_run_id: &str, entry: &ToolRunEntry) -> ToolRunView {
    let mut view = ToolRunView {
        tool_run_id: tool_run_id.to_string(),
        kind: entry.kind,
        status: entry.state.status(),
        session_id: entry.session_id.clone(),
        source_step_id: entry.source_step_id.clone(),
        started_at: None,
        finished_at: None,
        due_at: None,
        title: None,
        body: None,
        mode: None,
        command: None,
        output: None,
        error: None,
        error_reason: None,
        exit_code: None,
        preview: None,
    };

    match &entry.state {
        ToolRunState::Waiting => {}
        ToolRunState::Running { started_at } => {
            view.started_at = Some(started_at.clone());
            if entry.kind == ToolRunKind::Background {
                view.command = Some(entry.command.clone());
                view.output = entry.tail.as_ref().and_then(|tail| {
                    let output = tail.snapshot();
                    (!output.is_empty()).then(|| output.as_str().to_string())
                });
            }
        }
        ToolRunState::Completed {
            output,
            exit_code,
            started_at,
            finished_at,
            ..
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
            if entry.kind == ToolRunKind::Background {
                view.output = Some(output.clone());
                view.exit_code = *exit_code;
            }
        }
        ToolRunState::Failed {
            error,
            error_reason,
            exit_code,
            started_at,
            finished_at,
            ..
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
            view.error_reason = Some(error_reason.clone());
            if entry.kind == ToolRunKind::Background {
                view.error = Some(error.clone());
                view.exit_code = *exit_code;
            }
        }
        ToolRunState::Cancelled {
            started_at,
            finished_at,
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
        }
    }

    if entry.kind == ToolRunKind::Scheduled {
        if let Some(schedule) = &entry.scheduled {
            view.due_at = Some(schedule.due_at.clone());
            view.title = Some(schedule.title.clone());
            view.body = Some(schedule.body.clone());
            view.mode = Some(schedule.mode);
        }
    } else {
        let preview = view
            .output
            .as_deref()
            .or(view.error.as_deref())
            .unwrap_or("");
        view.preview = Some(preview.chars().take(200).collect());
    }

    view
}

#[cfg(test)]
pub(super) fn scheduled_status_json(
    tool_run_id: &str,
    session_id: Option<&str>,
    entry: &ScheduledToolRunEntry,
    state: &ToolRunState,
) -> Value {
    let mut value = json!({
        "tool_run_id": tool_run_id,
        "kind": "scheduled",
        "status": state.status().as_str(),
        "title": entry.title,
        "body": entry.body,
        "mode": entry.mode.as_str(),
        "session_id": session_id,
        "tool_name": entry.tool_name,
        "tool_args": entry.tool_args,
        "prompt": entry.prompt,
        "watch_tool_run_id": entry.watch_tool_run_id,
        "due_at": entry.due_at,
    });
    if let ToolRunState::Running { started_at } = state {
        value["started_at"] = json!(started_at);
    }
    value
}

pub(super) fn scheduled_lifecycle_payload(
    tool_run_id: &str,
    session_id: Option<&str>,
    entry: &ScheduledToolRunEntry,
    state: &ToolRunState,
) -> ToolRunLifecyclePayload {
    let mut payload = ToolRunLifecyclePayload::new(
        ToolRunKind::Scheduled,
        tool_run_id,
        tool_run_lifecycle_state(state),
    );
    payload.session_id = session_id.map(str::to_string);
    payload.title = Some(entry.title.clone());
    payload.body = Some(entry.body.clone());
    payload.mode = Some(entry.mode);
    payload.due_at = Some(entry.due_at.clone());
    if let ToolRunState::Failed { error_reason, .. } = state {
        payload.error_reason = Some(error_reason.clone());
    }
    payload
}

/// Project runtime state into the smaller event state contract. A cancelled
/// scheduled ToolRun can have an empty runtime start marker when cancelled
/// before firing; the event represents that absence explicitly as `None`.
pub(super) fn tool_run_lifecycle_state(state: &ToolRunState) -> ToolRunLifecycleState {
    match state {
        ToolRunState::Waiting => ToolRunLifecycleState::Waiting,
        ToolRunState::Running { started_at } => ToolRunLifecycleState::Running {
            started_at: started_at.clone(),
        },
        ToolRunState::Completed {
            started_at,
            finished_at,
            ..
        } => ToolRunLifecycleState::Completed {
            started_at: started_at.clone(),
            finished_at: finished_at.clone(),
        },
        ToolRunState::Failed {
            started_at,
            finished_at,
            ..
        } => ToolRunLifecycleState::Failed {
            started_at: started_at.clone(),
            finished_at: finished_at.clone(),
        },
        ToolRunState::Cancelled {
            started_at,
            finished_at,
        } => ToolRunLifecycleState::Cancelled {
            started_at: (!started_at.is_empty()).then(|| started_at.clone()),
            finished_at: finished_at.clone(),
        },
    }
}

/// Project terminal runtime fields into the completion delivery contract.
/// The UI status projection has its own field policy in `ToolRunStatusView`.
pub(super) fn project_completion_payload(
    tool_run_id: &str,
    state: &ToolRunState,
) -> ToolRunCompletionPayload {
    let mut payload = ToolRunCompletionPayload {
        tool_run_id: tool_run_id.to_owned(),
        status: state.status(),
        status_projection_kind: None,
        output: None,
        error: None,
        error_reason: None,
        log_path: None,
        exit_code: None,
        started_at: None,
        finished_at: None,
        source_step_id: None,
        truncated: false,
    };
    match state {
        ToolRunState::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            payload.output = Some(output.clone());
            payload.exit_code = *exit_code;
            payload.truncated = *truncated;
            payload.log_path = log_path.clone();
            payload.started_at = Some(started_at.clone());
            payload.finished_at = Some(finished_at.clone());
        }
        ToolRunState::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            payload.error = Some(error.clone());
            payload.error_reason = Some(error_reason.clone());
            payload.log_path = log_path.clone();
            payload.exit_code = *exit_code;
            payload.started_at = Some(started_at.clone());
            payload.finished_at = Some(finished_at.clone());
        }
        ToolRunState::Cancelled {
            started_at,
            finished_at,
        } => {
            payload.started_at = Some(started_at.clone());
            payload.finished_at = Some(finished_at.clone());
        }
        ToolRunState::Waiting | ToolRunState::Running { .. } => {
            unreachable!("completion payload requires a terminal ToolRun state")
        }
    }
    payload
}

pub(super) fn project_background_completion_payload(
    tool_run_id: &str,
    state: &ToolRunState,
    source_step_id: Option<&str>,
) -> ToolRunCompletionPayload {
    let mut payload = project_completion_payload(tool_run_id, state);
    payload.status_projection_kind = Some("background".to_owned());
    payload.source_step_id = source_step_id.map(str::to_owned);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_payload_keeps_terminal_status_fields_and_background_source() {
        let state = ToolRunState::Completed {
            output: "result".into(),
            exit_code: Some(0),
            truncated: true,
            log_path: Some("C:/Temp/full.log".into()),
            started_at: "started".into(),
            finished_at: "finished".into(),
        };
        let expected = json!({
            "tool_run_id": "toolrun-result",
            "status": "completed",
            "output": "result",
            "started_at": "started",
            "finished_at": "finished",
            "exit_code": 0,
            "truncated": true,
            "log_path": "C:/Temp/full.log",
        });
        let mut expected_background = expected.clone();
        expected_background["kind"] = json!("background");
        expected_background["source_step_id"] = json!("step-source");

        assert_eq!(
            serde_json::to_value(project_completion_payload("toolrun-result", &state)).unwrap(),
            expected,
        );
        assert_eq!(
            serde_json::to_value(project_background_completion_payload(
                "toolrun-result",
                &state,
                Some("step-source"),
            ))
            .unwrap(),
            expected_background,
        );
        assert_eq!(
            ToolRunStatusView::Background {
                tool_run_id: "toolrun-result".into(),
                source_step_id: Some("step-source".into()),
                state: ToolRunStateView::from_runtime_state(&state),
            }
            .to_json(false),
            expected_background,
        );
    }
}
