use haven_common::lifecycle::ToolRunStatus;
use tokio::sync::{Mutex, MutexGuard};

/// Runtime state of one detached background tool invocation.
#[derive(Clone, Debug)]
pub(crate) enum ToolRunState {
    Running {
        started_at: String,
    },
    Completed {
        output: String,
        truncated: bool,
        /// Full-output path when the captured output was capped.
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
        error_reason: String,
        /// Full-output path for diagnosis.
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: String,
        finished_at: String,
    },
}

impl ToolRunState {
    pub(crate) fn status(&self) -> ToolRunStatus {
        match self {
            Self::Running { .. } => ToolRunStatus::Running,
            Self::Completed { .. } => ToolRunStatus::Completed,
            Self::Failed { .. } => ToolRunStatus::Failed,
            Self::Cancelled { .. } => ToolRunStatus::Cancelled,
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }
}

/// Shared in-memory transition predicate. Terminal rows cannot be claimed a
/// second time, and every terminal transition starts from a running invocation.
pub(crate) fn can_claim_terminal(current: ToolRunStatus, next: ToolRunStatus) -> bool {
    current == ToolRunStatus::Running && next.is_terminal() && current.can_transition_to(next)
}

/// One serialization boundary for terminal commits across ToolRun completion paths.
/// Callers must re-check the in-memory state after acquiring it and keep the
/// guard until durable commit and its in-memory projection are complete.
#[derive(Default)]
pub(crate) struct TerminalTransitionGuard(Mutex<()>);

impl TerminalTransitionGuard {
    pub(crate) async fn lock(&self) -> MutexGuard<'_, ()> {
        self.0.lock().await
    }
}

/// Timestamps shared by all terminal states. `started_at` is preserved exactly;
/// an empty value is meaningful when restore repairs a malformed legacy row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalTimestamps {
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
}

impl TerminalTimestamps {
    pub(crate) fn new(started_at: impl Into<String>, finished_at: impl Into<String>) -> Self {
        Self {
            started_at: started_at.into(),
            finished_at: finished_at.into(),
        }
    }

    pub(crate) fn now(started_at: impl Into<String>) -> Self {
        Self::new(
            started_at,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        )
    }

    pub(crate) fn build(self, payload: TerminalPayload) -> ToolRunState {
        match payload {
            TerminalPayload::Completed {
                output,
                truncated,
                log_path,
            } => ToolRunState::Completed {
                output,
                truncated,
                log_path,
                started_at: self.started_at,
                finished_at: self.finished_at,
            },
            TerminalPayload::Failed {
                error,
                error_reason,
                log_path,
            } => ToolRunState::Failed {
                error,
                error_reason,
                log_path,
                started_at: self.started_at,
                finished_at: self.finished_at,
            },
            TerminalPayload::Cancelled => ToolRunState::Cancelled {
                started_at: self.started_at,
                finished_at: self.finished_at,
            },
        }
    }
}

pub(crate) enum TerminalPayload {
    Completed {
        output: String,
        truncated: bool,
        log_path: Option<String>,
    },
    Failed {
        error: String,
        error_reason: String,
        log_path: Option<String>,
    },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::{TerminalPayload, TerminalTimestamps, ToolRunState, can_claim_terminal};
    use haven_common::lifecycle::ToolRunStatus;

    #[test]
    fn completed_terminal_state_preserves_payload_and_timestamps() {
        let state = TerminalTimestamps::new("start", "finish").build(TerminalPayload::Completed {
            output: "out".into(),
            truncated: true,
            log_path: Some("log.txt".into()),
        });

        assert!(matches!(
            state,
            ToolRunState::Completed {
                output,
                truncated: true,
                log_path: Some(path),
                started_at,
                finished_at,
            } if output == "out" && path == "log.txt" && started_at == "start" && finished_at == "finish"
        ));
    }

    #[test]
    fn failed_terminal_state_preserves_payload_and_timestamps() {
        let state = TerminalTimestamps::new("start", "finish").build(TerminalPayload::Failed {
            error: "details".into(),
            error_reason: "summary".into(),
            log_path: Some("failure.log".into()),
        });

        assert!(matches!(
            state,
            ToolRunState::Failed {
                error,
                error_reason,
                log_path: Some(path),
                started_at,
                finished_at,
            } if error == "details" && error_reason == "summary" && path == "failure.log" && started_at == "start" && finished_at == "finish"
        ));
    }

    #[test]
    fn cancelled_terminal_state_preserves_start_and_finish_timestamps() {
        let state = TerminalTimestamps::new("", "finish").build(TerminalPayload::Cancelled);

        assert!(matches!(
            state,
            ToolRunState::Cancelled { started_at, finished_at }
                if started_at.is_empty() && finished_at == "finish"
        ));
    }

    #[test]
    fn now_timestamps_preserve_start_and_use_rfc3339_finish() {
        let timestamps = TerminalTimestamps::now("started");

        assert_eq!(timestamps.started_at, "started");
        assert!(chrono::DateTime::parse_from_rfc3339(&timestamps.finished_at).is_ok());
    }

    #[test]
    fn tool_run_status_transition_graph_is_exhaustive() {
        let allowed_transitions = [
            (ToolRunStatus::Running, ToolRunStatus::Completed),
            (ToolRunStatus::Running, ToolRunStatus::Failed),
            (ToolRunStatus::Running, ToolRunStatus::Cancelled),
        ];

        for current in ToolRunStatus::ALL {
            for next in ToolRunStatus::ALL {
                let expected = current == next || allowed_transitions.contains(&(current, next));
                assert_eq!(
                    current.can_transition_to(next),
                    expected,
                    "unexpected ToolRun status transition: {} -> {}",
                    current.as_str(),
                    next.as_str()
                );
            }
        }
    }

    #[test]
    fn terminal_claim_policy_covers_every_status_and_source_pair() {
        for current in ToolRunStatus::ALL {
            for target in ToolRunStatus::ALL {
                let expected = current == ToolRunStatus::Running && target.is_terminal();
                assert_eq!(
                    can_claim_terminal(current, target),
                    expected,
                    "unexpected terminal claim: {:?} -> {:?}",
                    current,
                    target
                );
            }
        }
    }
}
