//! Canonical lifecycle vocabularies shared by persistence, runtimes and IPC.
//!
//! The enums in this module are deliberately small.  They describe durable
//! lifecycle state only; transient execution details such as a run slot,
//! interaction reason or cancellation token belong to their owning runtime.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Durable state of a conversation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Error,
}

/// Derived explanation for a session that is durably `Paused`.
///
/// `SessionStatus` remains intentionally coarse because it is persisted and
/// participates in the lifecycle state machine. This value is a runtime/UI
/// projection: it explains what can make a paused session progress again and
/// must therefore be `None` for non-paused states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionWaitingReason {
    UserInput,
    UserInterrupt,
    Ask,
    Confirmation,
    ScheduledConfirmation,
    BackgroundTask,
    ScheduledTask,
    StepBudget,
    /// Explicit end could not confirm the durable cleanup of an owned
    /// scheduled ToolRun. The session remains resumable and end can be retried.
    EndIncomplete,
}

impl SessionWaitingReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserInput => "user_input",
            Self::UserInterrupt => "user_interrupt",
            Self::Ask => "ask",
            Self::Confirmation => "confirmation",
            Self::ScheduledConfirmation => "scheduled_confirmation",
            Self::BackgroundTask => "background_task",
            Self::ScheduledTask => "scheduled_task",
            Self::StepBudget => "step_budget",
            Self::EndIncomplete => "end_incomplete",
        }
    }
}

impl Serialize for SessionWaitingReason {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionWaitingReason {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match String::deserialize(deserializer)?.as_str() {
            "user_input" => Ok(Self::UserInput),
            "user_interrupt" => Ok(Self::UserInterrupt),
            "ask" => Ok(Self::Ask),
            "confirmation" => Ok(Self::Confirmation),
            "scheduled_confirmation" => Ok(Self::ScheduledConfirmation),
            "background_task" => Ok(Self::BackgroundTask),
            "scheduled_task" => Ok(Self::ScheduledTask),
            "step_budget" => Ok(Self::StepBudget),
            "end_incomplete" => Ok(Self::EndIncomplete),
            value => Err(serde::de::Error::custom(format!(
                "unknown session waiting reason '{value}'"
            ))),
        }
    }
}

impl SessionStatus {
    pub const ALL: [Self; 5] = [
        Self::Pending,
        Self::Running,
        Self::Paused,
        Self::Completed,
        Self::Error,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Error => "error",
        }
    }

    /// Decode persisted state fail-closed.  An unknown value must never
    /// resurrect a session into the dispatcher queue.
    pub fn from_status_str(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "error" => Self::Error,
            other => {
                tracing::warn!(status = other, "unknown session status; mapping to error");
                Self::Error
            }
        }
    }

    pub const fn is_paused(self) -> bool {
        matches!(self, Self::Paused)
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Error)
    }

    pub const fn is_dispatchable(self) -> bool {
        matches!(self, Self::Pending)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Pending,
                Self::Running | Self::Paused | Self::Completed | Self::Error
            ) | (
                Self::Running,
                Self::Paused | Self::Pending | Self::Completed | Self::Error
            ) | (
                Self::Paused,
                Self::Pending | Self::Running | Self::Completed | Self::Error
            ) | (Self::Completed, Self::Paused)
                | (Self::Error, Self::Paused | Self::Pending)
        )
    }
}

impl Serialize for SessionStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for SessionStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_status_str(&value))
    }
}

/// Durable state of any background or scheduled ToolRun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolRunStatus {
    Waiting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ToolRunStatus {
    pub const ALL: [Self; 5] = [
        Self::Waiting,
        Self::Running,
        Self::Completed,
        Self::Failed,
        Self::Cancelled,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Decode persisted ToolRun state fail-closed as failed. A malformed status
    /// must not make a ToolRun runnable or hide its history.
    pub fn from_status_str(value: &str) -> Self {
        match value {
            "waiting" => Self::Waiting,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => {
                tracing::warn!(status = other, "unknown ToolRun status; mapping to failed");
                Self::Failed
            }
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub const fn is_live(self) -> bool {
        matches!(self, Self::Waiting | Self::Running)
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }
        matches!(
            (self, next),
            (Self::Waiting, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Completed | Self::Failed | Self::Cancelled
                )
        )
    }
}

impl Serialize for ToolRunStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ToolRunStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_status_str(&value))
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionStatus, SessionWaitingReason, ToolRunStatus};

    #[test]
    fn waiting_reason_serializes_as_stable_wire_vocabulary() {
        assert_eq!(
            serde_json::to_string(&SessionWaitingReason::ScheduledConfirmation).unwrap(),
            "\"scheduled_confirmation\""
        );
        assert_eq!(
            serde_json::from_str::<SessionWaitingReason>("\"background_task\"").unwrap(),
            SessionWaitingReason::BackgroundTask
        );
        assert_eq!(
            serde_json::to_string(&SessionWaitingReason::EndIncomplete).unwrap(),
            "\"end_incomplete\""
        );
        assert!(serde_json::from_str::<SessionWaitingReason>("\"bogus\"").is_err());
    }

    #[test]
    fn session_transition_table_is_the_single_policy() {
        assert!(SessionStatus::Pending.can_transition_to(SessionStatus::Running));
        assert!(SessionStatus::Paused.can_transition_to(SessionStatus::Running));
        assert!(SessionStatus::Error.can_transition_to(SessionStatus::Pending));
        assert!(!SessionStatus::Completed.can_transition_to(SessionStatus::Pending));
    }

    #[test]
    fn tool_run_transition_table_has_no_terminal_resurrection() {
        assert!(ToolRunStatus::Waiting.can_transition_to(ToolRunStatus::Running));
        assert!(ToolRunStatus::Waiting.can_transition_to(ToolRunStatus::Cancelled));
        assert!(!ToolRunStatus::Waiting.can_transition_to(ToolRunStatus::Completed));
        assert!(!ToolRunStatus::Waiting.can_transition_to(ToolRunStatus::Failed));
        assert!(ToolRunStatus::Running.can_transition_to(ToolRunStatus::Failed));
        assert!(!ToolRunStatus::Completed.can_transition_to(ToolRunStatus::Waiting));
    }

    #[test]
    fn unknown_values_fail_closed() {
        assert_eq!(
            SessionStatus::from_status_str("bogus"),
            SessionStatus::Error
        );
        assert_eq!(
            ToolRunStatus::from_status_str("scheduled"),
            ToolRunStatus::Failed
        );
    }
}
