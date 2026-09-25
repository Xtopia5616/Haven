use haven_common::ActionStatus;
use tokio::sync::{Mutex, MutexGuard};

/// Runtime state for a background or scheduled action. Action families keep
/// their own admission, execution, persistence, and publication paths.
#[derive(Clone, Debug)]
pub(crate) enum ActionState {
    /// A scheduled trigger is admitted but has not fired.
    Waiting,
    Running {
        started_at: String,
    },
    Completed {
        output: String,
        exit_code: Option<i32>,
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
        exit_code: Option<i32>,
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: String,
        finished_at: String,
    },
}

impl ActionState {
    pub(crate) fn status(&self) -> ActionStatus {
        match self {
            Self::Waiting => ActionStatus::Waiting,
            Self::Running { .. } => ActionStatus::Running,
            Self::Completed { .. } => ActionStatus::Completed,
            Self::Failed { .. } => ActionStatus::Failed,
            Self::Cancelled { .. } => ActionStatus::Cancelled,
        }
    }

    pub(crate) fn is_waiting(&self) -> bool {
        matches!(self, Self::Waiting)
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }
}

/// A terminal transition may start from a running action, or from any live
/// state when cancelling a scheduled action that has not fired yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalSource {
    Running,
    Live,
}

/// Shared in-memory transition predicate. Terminal rows cannot be claimed a
/// second time, and completed/failed transitions cannot start from Waiting.
pub(crate) fn can_claim_terminal(
    current: ActionStatus,
    next: ActionStatus,
    source: TerminalSource,
) -> bool {
    next.is_terminal()
        && match source {
            TerminalSource::Running => current == ActionStatus::Running,
            TerminalSource::Live => current.is_live(),
        }
        && current.can_transition_to(next)
}

/// One serialization boundary for terminal commits across both action kinds.
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
/// an empty value is meaningful for cancellation of a waiting scheduled action.
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
        Self::new(started_at, chrono::Utc::now().to_rfc3339())
    }

    pub(crate) fn build(self, payload: TerminalPayload) -> ActionState {
        match payload {
            TerminalPayload::Completed {
                output,
                exit_code,
                truncated,
                log_path,
            } => ActionState::Completed {
                output,
                exit_code,
                truncated,
                log_path,
                started_at: self.started_at,
                finished_at: self.finished_at,
            },
            TerminalPayload::Failed {
                error,
                error_reason,
                log_path,
                exit_code,
            } => ActionState::Failed {
                error,
                error_reason,
                log_path,
                exit_code,
                started_at: self.started_at,
                finished_at: self.finished_at,
            },
            TerminalPayload::Cancelled => ActionState::Cancelled {
                started_at: self.started_at,
                finished_at: self.finished_at,
            },
        }
    }
}

pub(crate) enum TerminalPayload {
    Completed {
        output: String,
        exit_code: Option<i32>,
        truncated: bool,
        log_path: Option<String>,
    },
    Failed {
        error: String,
        error_reason: String,
        log_path: Option<String>,
        exit_code: Option<i32>,
    },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::{
        ActionState, TerminalPayload, TerminalSource, TerminalTimestamps, can_claim_terminal,
    };
    use haven_common::ActionStatus;

    #[test]
    fn completed_terminal_state_preserves_payload_and_timestamps() {
        let state = TerminalTimestamps::new("start", "finish").build(TerminalPayload::Completed {
            output: "out".into(),
            exit_code: Some(0),
            truncated: true,
            log_path: Some("log.txt".into()),
        });

        assert!(matches!(
            state,
            ActionState::Completed {
                output,
                exit_code: Some(0),
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
            exit_code: Some(7),
        });

        assert!(matches!(
            state,
            ActionState::Failed {
                error,
                error_reason,
                log_path: Some(path),
                exit_code: Some(7),
                started_at,
                finished_at,
            } if error == "details" && error_reason == "summary" && path == "failure.log" && started_at == "start" && finished_at == "finish"
        ));
    }

    #[test]
    fn cancelled_terminal_state_preserves_empty_start_for_waiting_schedule() {
        let state = TerminalTimestamps::new("", "finish").build(TerminalPayload::Cancelled);

        assert!(matches!(
            state,
            ActionState::Cancelled { started_at, finished_at }
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
    fn action_status_transition_graph_is_exhaustive() {
        let allowed_transitions = [
            (ActionStatus::Waiting, ActionStatus::Running),
            (ActionStatus::Waiting, ActionStatus::Cancelled),
            (ActionStatus::Running, ActionStatus::Completed),
            (ActionStatus::Running, ActionStatus::Failed),
            (ActionStatus::Running, ActionStatus::Cancelled),
        ];

        for current in ActionStatus::ALL {
            for next in ActionStatus::ALL {
                let expected = current == next || allowed_transitions.contains(&(current, next));
                assert_eq!(
                    current.can_transition_to(next),
                    expected,
                    "unexpected action status transition: {} -> {}",
                    current.as_str(),
                    next.as_str()
                );
            }
        }
    }

    #[test]
    fn terminal_claim_policy_covers_every_status_and_source_pair() {
        for current in ActionStatus::ALL {
            for target in ActionStatus::ALL {
                for source in [TerminalSource::Running, TerminalSource::Live] {
                    let expected = matches!(
                        (source, current, target),
                        (
                            TerminalSource::Running | TerminalSource::Live,
                            ActionStatus::Running,
                            ActionStatus::Completed
                                | ActionStatus::Failed
                                | ActionStatus::Cancelled
                        ) | (
                            TerminalSource::Live,
                            ActionStatus::Waiting,
                            ActionStatus::Cancelled
                        )
                    );

                    assert_eq!(
                        can_claim_terminal(current, target, source),
                        expected,
                        "unexpected terminal claim: {:?} {:?} -> {:?}",
                        source,
                        current,
                        target
                    );
                }
            }
        }
    }
}
