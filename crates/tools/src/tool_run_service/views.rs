use super::*;
use haven_common::ToolRunCompletionPayload;
use haven_memory::ToolRunRow;

/// A bounded request for terminal ToolRun history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunHistoryQuery {
    pub(super) kind: Option<ToolRunKind>,
    pub(super) session_id: Option<String>,
    pub(super) limit: usize,
}

impl ToolRunHistoryQuery {
    pub fn new(kind: Option<ToolRunKind>, session_id: Option<String>, limit: usize) -> Self {
        Self {
            kind,
            session_id,
            limit,
        }
    }
}

/// Safe, typed projection of a ToolRun for the application task panel.
///
/// This contains only fields used by the task panel. Execution inputs, log
/// paths, continuation prompts, and dependency watch ids stay inside Tools and
/// Memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolRunView {
    pub tool_run_id: String,
    pub kind: ToolRunKind,
    pub tool_name: String,
    pub status: ToolRunStatus,
    pub session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub preview: Option<String>,
}

pub(super) fn project_persisted_tool_run(row: ToolRunRow) -> anyhow::Result<ToolRunView> {
    let kind = match row.kind.as_str() {
        "background" => ToolRunKind::Background,
        other => anyhow::bail!("unknown persisted ToolRun kind '{other}'"),
    };
    Ok(ToolRunView {
        tool_run_id: row.tool_run_id,
        kind,
        tool_name: row.tool_name,
        status: row.status,
        session_id: row.session_id,
        source_step_id: row.source_step_id,
        started_at: row.started_at,
        finished_at: row.finished_at,
        output: row.output,
        error: row.error.or_else(|| row.error_reason.clone()),
        error_reason: row.error_reason,
        preview: None,
    })
}

pub(super) fn project_persisted_background_board_row(
    row: ToolRunRow,
) -> anyhow::Result<ToolRunView> {
    let mut view = project_persisted_tool_run(row)?;
    anyhow::ensure!(
        view.kind == ToolRunKind::Background,
        "board history query returned a non-background ToolRun"
    );
    view.preview = Some(
        view.output
            .as_deref()
            .or(view.error.as_deref())
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect(),
    );
    Ok(view)
}

/// Typed model-facing projection of a ToolRun status.
///
/// `ToolRunView` is intentionally a UI projection and therefore is not reused
/// here: the agent-facing shapes include completion output. JSON conversion
/// stays at the tool boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum ToolRunStatusView {
    NotFound {
        tool_run_id: String,
    },
    Background {
        tool_run_id: String,
        tool_name: String,
        source_step_id: Option<String>,
        state: ToolRunStateView,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ToolRunStateView {
    Running {
        started_at: String,
    },
    Completed {
        output: String,
        truncated: bool,
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
        error_reason: String,
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: String,
        finished_at: String,
    },
}

/// A scoped list row. Its status remains typed until the tool_runs tool has
/// applied filtering and built its final JSON result.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolRunListView {
    pub status: ToolRunStatus,
    pub projection: ToolRunStatusView,
    pub kind: ToolRunKind,
    pub tool_name: String,
    pub session_id: Option<String>,
    pub preview: String,
}

impl ToolRunStateView {
    pub(super) fn from_entry(entry: &ToolRunEntry) -> Self {
        Self::from_runtime_state(&entry.state)
    }

    fn from_runtime_state(state: &ToolRunState) -> Self {
        match state {
            ToolRunState::Running { started_at } => Self::Running {
                started_at: started_at.clone(),
            },
            ToolRunState::Completed {
                output,
                truncated,
                log_path,
                started_at,
                finished_at,
            } => Self::Completed {
                output: output.clone(),
                truncated: *truncated,
                log_path: log_path.clone(),
                started_at: started_at.clone(),
                finished_at: finished_at.clone(),
            },
            ToolRunState::Failed {
                error,
                error_reason,
                log_path,
                started_at,
                finished_at,
            } => Self::Failed {
                error: error.clone(),
                error_reason: error_reason.clone(),
                log_path: log_path.clone(),
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
            Self::Running { started_at } => json!({
                "tool_run_id": tool_run_id,
                "status": "running",
                "started_at": started_at,
            }),
            Self::Completed {
                output,
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
            Self::Running { .. } => ToolRunStatus::Running,
            Self::Completed { .. } => ToolRunStatus::Completed,
            Self::Failed { .. } => ToolRunStatus::Failed,
            Self::Cancelled { .. } => ToolRunStatus::Cancelled,
        }
    }

    pub(super) fn preview(&self) -> String {
        let source = match self {
            Self::Running { .. } => None,
            Self::Completed { output, .. } => Some(output.as_str()),
            Self::Failed { error, .. } => Some(error.as_str()),
            Self::Cancelled { .. } => None,
        };
        source.unwrap_or_default().chars().take(200).collect()
    }

    fn started_at(&self) -> Option<&str> {
        match self {
            Self::Running { started_at, .. }
            | Self::Completed { started_at, .. }
            | Self::Failed { started_at, .. }
            | Self::Cancelled { started_at, .. } => Some(started_at),
        }
    }
}

impl ToolRunStatusView {
    pub fn status(&self) -> Option<ToolRunStatus> {
        match self {
            Self::NotFound { .. } => None,
            Self::Background { state, .. } => Some(state.status()),
        }
    }

    pub(crate) fn to_json(&self, include_background_wait: bool) -> Value {
        match self {
            Self::NotFound { tool_run_id } => json!({
                "tool_run_id": tool_run_id,
                "status": "not_found",
            }),
            Self::Background {
                tool_run_id,
                tool_name,
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
                value["tool_name"] = json!(tool_name);
                if let Some(source_step_id) = source_step_id {
                    value["source_step_id"] = json!(source_step_id);
                }
                value
            }
        }
    }
}

impl ToolRunListView {
    pub(crate) fn to_json(&self) -> Value {
        let mut value = self.projection.to_json(false);
        value["session_id"] = json!(self.session_id);
        value["kind"] = json!("background");
        value["tool_name"] = json!(self.tool_name);
        value["preview"] = json!(self.preview);
        value
    }
}

pub(super) fn list_view_started_at(view: &ToolRunListView) -> Option<&str> {
    match &view.projection {
        ToolRunStatusView::Background { state, .. } => state.started_at(),
        ToolRunStatusView::NotFound { .. } => None,
    }
}

pub(super) fn project_board_tool_run(tool_run_id: &str, entry: &ToolRunEntry) -> ToolRunView {
    let mut view = ToolRunView {
        tool_run_id: tool_run_id.to_string(),
        kind: entry.kind,
        tool_name: entry.tool_name.clone(),
        status: entry.state.status(),
        session_id: entry.session_id.clone(),
        source_step_id: entry.source_step_id.clone(),
        started_at: None,
        finished_at: None,
        output: None,
        error: None,
        error_reason: None,
        preview: None,
    };

    match &entry.state {
        ToolRunState::Running { started_at } => {
            view.started_at = Some(started_at.clone());
        }
        ToolRunState::Completed {
            output,
            started_at,
            finished_at,
            ..
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
            view.output = Some(output.clone());
        }
        ToolRunState::Failed {
            error,
            error_reason,
            started_at,
            finished_at,
            ..
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
            view.error_reason = Some(error_reason.clone());
            view.error = Some(error.clone());
        }
        ToolRunState::Cancelled {
            started_at,
            finished_at,
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
        }
    }

    let preview = view
        .output
        .as_deref()
        .or(view.error.as_deref())
        .unwrap_or("");
    view.preview = Some(preview.chars().take(200).collect());

    view
}

/// Project runtime state into the smaller event state contract. A cancelled
/// background ToolRun can have an empty start marker when it is cancelled
/// before a child process starts; the event represents that absence explicitly.
pub(super) fn tool_run_lifecycle_state(state: &ToolRunState) -> ToolRunLifecycleState {
    match state {
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
    tool_name: &str,
    state: &ToolRunState,
) -> ToolRunCompletionPayload {
    let mut payload = ToolRunCompletionPayload {
        tool_run_id: tool_run_id.to_owned(),
        tool_name: tool_name.to_owned(),
        status: state.status(),
        output: None,
        error: None,
        error_reason: None,
        log_path: None,
        started_at: None,
        finished_at: None,
        source_step_id: None,
        truncated: false,
    };
    match state {
        ToolRunState::Completed {
            output,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            payload.output = Some(output.clone());
            payload.truncated = *truncated;
            payload.log_path = log_path.clone();
            payload.started_at = Some(started_at.clone());
            payload.finished_at = Some(finished_at.clone());
        }
        ToolRunState::Failed {
            error,
            error_reason,
            log_path,
            started_at,
            finished_at,
        } => {
            payload.error = Some(error.clone());
            payload.error_reason = Some(error_reason.clone());
            payload.log_path = log_path.clone();
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
        ToolRunState::Running { .. } => {
            unreachable!("completion payload requires a terminal ToolRun state")
        }
    }
    payload
}

pub(super) fn project_background_completion_payload(
    tool_run_id: &str,
    tool_name: &str,
    state: &ToolRunState,
    source_step_id: Option<&str>,
) -> ToolRunCompletionPayload {
    let mut payload = project_completion_payload(tool_run_id, tool_name, state);
    payload.source_step_id = source_step_id.map(str::to_owned);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persisted_row(kind: &str) -> ToolRunRow {
        ToolRunRow {
            tool_run_id: "toolrun-00000000000000000000000000000001".into(),
            kind: kind.into(),
            tool_name: "shell".into(),
            session_id: Some("ses-00000000000000000000000000000001".into()),
            source_step_id: Some("step-00000000000000000000000000000001".into()),
            status: ToolRunStatus::Completed,
            output: Some("result".into()),
            error: None,
            error_reason: None,
            log_path: Some("C:/private/tool-run.log".into()),
            started_at: Some("started".into()),
            finished_at: Some("finished".into()),
            created_at: "created".into(),
        }
    }

    #[test]
    fn persisted_history_projection_is_typed_and_hides_execution_internals() {
        let view = project_persisted_tool_run(persisted_row("background")).unwrap();

        assert_eq!(view.kind, ToolRunKind::Background);
        assert_eq!(view.output.as_deref(), Some("result"));
        assert_eq!(view.preview, None);
    }

    #[test]
    fn background_board_history_caps_preview() {
        let mut row = persisted_row("background");
        row.output = Some("x".repeat(201));

        let view = project_persisted_background_board_row(row).unwrap();

        assert_eq!(view.kind, ToolRunKind::Background);
        assert_eq!(view.preview.as_deref().map(str::len), Some(200));
    }

    #[test]
    fn persisted_history_rejects_unknown_kind_and_keeps_requested_limit() {
        let error = project_persisted_tool_run(persisted_row("future_kind")).unwrap_err();
        assert!(error.to_string().contains("unknown persisted ToolRun kind"));

        let query = ToolRunHistoryQuery::new(None, None, usize::MAX);
        assert_eq!(query.limit, usize::MAX);
    }

    #[test]
    fn completion_payload_keeps_terminal_status_fields_and_background_source() {
        let state = ToolRunState::Completed {
            output: "result".into(),
            truncated: true,
            log_path: Some("C:/Temp/full.log".into()),
            started_at: "started".into(),
            finished_at: "finished".into(),
        };
        let expected = json!({
            "tool_run_id": "toolrun-0000000000000000000000000000001f",
            "tool_name": "shell",
            "status": "completed",
            "output": "result",
            "started_at": "started",
            "finished_at": "finished",
            "truncated": true,
            "log_path": "C:/Temp/full.log",
        });
        let mut expected_background = expected.clone();
        expected_background["source_step_id"] = json!("step-00000000000000000000000000000008");

        assert_eq!(
            serde_json::to_value(project_completion_payload(
                "toolrun-0000000000000000000000000000001f",
                "shell",
                &state
            ))
            .unwrap(),
            expected,
        );
        assert_eq!(
            serde_json::to_value(project_background_completion_payload(
                "toolrun-0000000000000000000000000000001f",
                "shell",
                &state,
                Some("step-00000000000000000000000000000008"),
            ))
            .unwrap(),
            expected_background,
        );
        assert_eq!(
            ToolRunStatusView::Background {
                tool_run_id: "toolrun-0000000000000000000000000000001f".into(),
                tool_name: "shell".into(),
                source_step_id: Some("step-00000000000000000000000000000008".into()),
                state: ToolRunStateView::from_runtime_state(&state),
            }
            .to_json(false),
            {
                let mut expected_status = expected_background.clone();
                expected_status["kind"] = json!("background");
                expected_status["tool_name"] = json!("shell");
                expected_status
            },
        );
    }
}
