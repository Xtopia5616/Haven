use super::*;

/// Action kind exposed by the task panel projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionViewKind {
    Background,
    Scheduled,
}

/// Safe, typed projection of one in-memory task board row.
///
/// This contains only fields used by the task panel. Execution internals such
/// as shell metadata, log paths, scheduled tool arguments, continuation
/// prompts, and dependency watch ids deliberately stay in `ActionEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionView {
    pub id: String,
    pub kind: ActionViewKind,
    pub status: ActionStatus,
    pub session_id: Option<String>,
    pub source_step_id: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub due_at: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub mode: Option<String>,
    pub command: Option<String>,
    pub output: Option<String>,
    pub error: Option<String>,
    pub error_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub preview: Option<String>,
}

/// Typed model-facing projection of an action status.
///
/// `ActionView` is intentionally a UI projection and therefore is not reused
/// here: the agent-facing shapes include live shell fields, completion output,
/// and scheduled-action metadata. The final JSON conversion stays at the tool
/// boundary (or in the legacy compatibility wrappers below).
#[derive(Clone, Debug, PartialEq)]
pub enum ActionStatusView {
    NotFound {
        action_id: String,
    },
    Background {
        action_id: String,
        source_step_id: Option<String>,
        state: ActionStateView,
    },
    Scheduled {
        action_id: String,
        session_id: Option<String>,
        schedule: Box<ScheduledActionView>,
        state: ActionStateView,
    },
    ScheduledTerminal {
        action_id: String,
        status: ActionStatus,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ActionStateView {
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
pub struct ScheduledActionView {
    pub title: String,
    pub body: String,
    pub due_at: String,
    pub mode: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<Value>,
    pub prompt: Option<String>,
    pub watch_action_id: Option<String>,
}

/// A scoped list row. Its status remains typed until the actions tool has
/// applied filtering and built its final JSON result.
#[derive(Clone, Debug, PartialEq)]
pub struct ActionListView {
    pub status: ActionStatus,
    pub projection: ActionStatusView,
    pub kind: ActionViewKind,
    pub session_id: Option<String>,
    pub preview: String,
}

impl ActionStateView {
    pub(super) fn from_entry(entry: &ActionEntry) -> Self {
        match &entry.state {
            ActionState::Waiting => Self::Waiting,
            ActionState::Running { started_at } => Self::Running {
                started_at: started_at.clone(),
                command: (entry.kind == ActionKind::Background).then(|| entry.command.clone()),
                shell: (entry.kind == ActionKind::Background).then(|| entry.shell.clone()),
                output: entry.tail.as_ref().and_then(|tail| {
                    let output = tail.snapshot();
                    (!output.is_empty()).then(|| output.as_str().to_string())
                }),
            },
            ActionState::Completed {
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
            ActionState::Failed {
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
            ActionState::Cancelled {
                started_at,
                finished_at,
            } => Self::Cancelled {
                started_at: started_at.clone(),
                finished_at: finished_at.clone(),
            },
        }
    }

    pub fn status(&self) -> ActionStatus {
        match self {
            Self::Waiting => ActionStatus::Waiting,
            Self::Running { .. } => ActionStatus::Running,
            Self::Completed { .. } => ActionStatus::Completed,
            Self::Failed { .. } => ActionStatus::Failed,
            Self::Cancelled { .. } => ActionStatus::Cancelled,
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

impl ActionStatusView {
    pub fn status(&self) -> Option<ActionStatus> {
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
            Self::NotFound { action_id } => json!({
                "action_id": action_id,
                "status": "not_found",
            }),
            Self::ScheduledTerminal { action_id, status } => json!({
                "action_id": action_id,
                "status": status.as_str(),
            }),
            Self::Background {
                action_id,
                source_step_id,
                state,
            } => background_status_json(
                action_id,
                source_step_id.as_deref(),
                state,
                include_background_wait,
            ),
            Self::Scheduled {
                action_id,
                session_id,
                schedule,
                state,
            } => {
                let mut value = json!({
                    "id": action_id,
                    "action_id": action_id,
                    "kind": "scheduled",
                    "status": state.status().as_str(),
                    "title": schedule.title,
                    "body": schedule.body,
                    "mode": schedule.mode,
                    "session_id": session_id,
                    "tool_name": schedule.tool_name,
                    "tool_args": schedule.tool_args,
                    "prompt": schedule.prompt,
                    "watch_action_id": schedule.watch_action_id,
                    "due_at": schedule.due_at,
                });
                if let ActionStateView::Running { started_at, .. } = state {
                    value["started_at"] = json!(started_at);
                }
                value
            }
        }
    }
}

impl ActionListView {
    pub(crate) fn to_json(&self) -> Value {
        let mut value = self.projection.to_json(false);
        if self.kind == ActionViewKind::Background {
            value["session_id"] = json!(self.session_id);
            value["kind"] = json!("background");
            value["preview"] = json!(self.preview);
        }
        value
    }
}

pub(super) fn list_view_started_at(view: &ActionListView) -> Option<&str> {
    match &view.projection {
        ActionStatusView::Background { state, .. } | ActionStatusView::Scheduled { state, .. } => {
            state.started_at()
        }
        ActionStatusView::NotFound { .. } | ActionStatusView::ScheduledTerminal { .. } => None,
    }
}

pub(super) fn scheduled_action_view(entry: &ScheduledActionEntry) -> ScheduledActionView {
    ScheduledActionView {
        title: entry.title.clone(),
        body: entry.body.clone(),
        due_at: entry.due_at.clone(),
        mode: entry.mode.as_str().to_string(),
        tool_name: entry.tool_name.clone(),
        tool_args: entry.tool_args.clone(),
        prompt: entry.prompt.clone(),
        watch_action_id: entry.watch_action_id.clone(),
    }
}

fn background_status_json(
    action_id: &str,
    source_step_id: Option<&str>,
    state: &ActionStateView,
    include_wait: bool,
) -> Value {
    let mut value = match state {
        ActionStateView::Waiting => json!({
            "action_id": action_id,
            "status": "waiting",
        }),
        ActionStateView::Running {
            started_at,
            command,
            shell,
            output,
        } => {
            let mut value = if include_wait {
                haven_common::tools::background_wait_object(
                    std::iter::once(action_id.to_string()),
                    "The action is still running. END YOUR TURN if you have nothing else useful to do — do not poll. The result is auto-pushed and the session is auto-woken when it finishes.",
                )
            } else {
                serde_json::Map::new()
            };
            value.insert("action_id".into(), json!(action_id));
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
        ActionStateView::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            let mut value = json!({
                "action_id": action_id,
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
        ActionStateView::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            let mut value = json!({
                "action_id": action_id,
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
        ActionStateView::Cancelled {
            started_at,
            finished_at,
        } => json!({
            "action_id": action_id,
            "status": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
        }),
    };
    value["kind"] = json!("background");
    if let Some(source_step_id) = source_step_id {
        value["source_step_id"] = json!(source_step_id);
    }
    value
}

pub(super) fn project_board_action(action_id: &str, entry: &ActionEntry) -> ActionView {
    let mut view = ActionView {
        id: action_id.to_string(),
        kind: match entry.kind {
            ActionKind::Background => ActionViewKind::Background,
            ActionKind::Scheduled => ActionViewKind::Scheduled,
        },
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
        ActionState::Waiting => {}
        ActionState::Running { started_at } => {
            view.started_at = Some(started_at.clone());
            if entry.kind == ActionKind::Background {
                view.command = Some(entry.command.clone());
                view.output = entry.tail.as_ref().and_then(|tail| {
                    let output = tail.snapshot();
                    (!output.is_empty()).then(|| output.as_str().to_string())
                });
            }
        }
        ActionState::Completed {
            output,
            exit_code,
            started_at,
            finished_at,
            ..
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
            if entry.kind == ActionKind::Background {
                view.output = Some(output.clone());
                view.exit_code = *exit_code;
            }
        }
        ActionState::Failed {
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
            if entry.kind == ActionKind::Background {
                view.error = Some(error.clone());
                view.exit_code = *exit_code;
            }
        }
        ActionState::Cancelled {
            started_at,
            finished_at,
        } => {
            view.started_at = Some(started_at.clone());
            view.finished_at = Some(finished_at.clone());
        }
    }

    if entry.kind == ActionKind::Scheduled {
        if let Some(schedule) = &entry.scheduled {
            view.due_at = Some(schedule.due_at.clone());
            view.title = Some(schedule.title.clone());
            view.body = Some(schedule.body.clone());
            view.mode = Some(schedule.mode.as_str().to_string());
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

pub(super) fn scheduled_status_json(
    id: &str,
    session_id: Option<&str>,
    entry: &ScheduledActionEntry,
    state: &ActionState,
) -> Value {
    let mut value = json!({
        "id": id,
        "action_id": id,
        "kind": "scheduled",
        "status": state.status().as_str(),
        "title": entry.title,
        "body": entry.body,
        "mode": entry.mode.as_str(),
        "session_id": session_id,
        "tool_name": entry.tool_name,
        "tool_args": entry.tool_args,
        "prompt": entry.prompt,
        "watch_action_id": entry.watch_action_id,
        "due_at": entry.due_at,
    });
    if let ActionState::Running { started_at } = state {
        value["started_at"] = json!(started_at);
    }
    value
}

pub(super) fn scheduled_finished_json(
    id: &str,
    session_id: Option<&str>,
    entry: &ScheduledActionEntry,
    state: &ActionState,
) -> Value {
    let mut value = json!({
        "id": id,
        "action_id": id,
        "kind": "scheduled",
        "status": state.status().as_str(),
        "title": entry.title,
        "body": entry.body,
        "mode": entry.mode.as_str(),
        "session_id": session_id,
        "due_at": entry.due_at,
    });
    match state {
        ActionState::Completed {
            started_at,
            finished_at,
            ..
        }
        | ActionState::Cancelled {
            started_at,
            finished_at,
        }
        | ActionState::Failed {
            started_at,
            finished_at,
            ..
        } => {
            if !started_at.is_empty() {
                value["started_at"] = json!(started_at);
            }
            value["finished_at"] = json!(finished_at);
        }
        _ => {}
    }
    if let ActionState::Failed { error_reason, .. } = state {
        value["error_reason"] = json!(error_reason);
    }
    value
}

/// Render the terminal status JSON for a action (mirrors `status()` output for
/// completed/failed/cancelled states), used in completion notifications.
pub(super) fn render_status_json(action_id: &str, state: &ActionState) -> Value {
    match state {
        ActionState::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "action_id": action_id,
                "status": "completed",
                "output": output,
                "started_at": started_at,
                "finished_at": finished_at,
            });
            if let Some(code) = exit_code {
                v["exit_code"] = json!(code);
            }
            if *truncated {
                v["truncated"] = json!(true);
            }
            if let Some(p) = log_path {
                v["log_path"] = json!(p);
            }
            v
        }
        ActionState::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "action_id": action_id,
                "status": "failed",
                "error": error,
                "error_reason": error_reason,
                "started_at": started_at,
                "finished_at": finished_at,
            });
            if let Some(code) = exit_code {
                v["exit_code"] = json!(code);
            }
            if let Some(p) = log_path {
                v["log_path"] = json!(p);
            }
            v
        }
        ActionState::Cancelled {
            started_at,
            finished_at,
        } => json!({
            "action_id": action_id,
            "status": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
        }),
        ActionState::Waiting => json!({ "action_id": action_id, "status": "waiting" }),
        ActionState::Running { .. } => {
            json!({ "action_id": action_id, "status": "running" })
        }
    }
}

pub(super) fn render_background_status_json(
    action_id: &str,
    state: &ActionState,
    source_step_id: Option<&str>,
) -> Value {
    let mut value = render_status_json(action_id, state);
    value["kind"] = json!("background");
    if let Some(source_step_id) = source_step_id {
        value["source_step_id"] = json!(source_step_id);
    }
    value
}
