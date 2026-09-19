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
            ) | (Self::Paused, Self::Pending | Self::Completed | Self::Error)
                | (Self::Completed, Self::Paused)
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

/// Durable state of any background or scheduled action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionStatus {
    Waiting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl ActionStatus {
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

    /// Decode persisted action state fail-closed as a failed action.  A
    /// malformed status must not make an action runnable or hide its history.
    pub fn from_status_str(value: &str) -> Self {
        match value {
            "waiting" => Self::Waiting,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            other => {
                tracing::warn!(status = other, "unknown action status; mapping to failed");
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
            (
                Self::Waiting,
                Self::Running | Self::Cancelled | Self::Completed | Self::Failed
            ) | (
                Self::Running,
                Self::Completed | Self::Failed | Self::Cancelled
            )
        )
    }
}

impl Serialize for ActionStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ActionStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_status_str(&value))
    }
}

#[cfg(test)]
mod tests {
    use super::{ActionStatus, SessionStatus};

    #[test]
    fn session_transition_table_is_the_single_policy() {
        assert!(SessionStatus::Pending.can_transition_to(SessionStatus::Running));
        assert!(SessionStatus::Error.can_transition_to(SessionStatus::Pending));
        assert!(!SessionStatus::Completed.can_transition_to(SessionStatus::Pending));
    }

    #[test]
    fn action_transition_table_has_no_terminal_resurrection() {
        assert!(ActionStatus::Waiting.can_transition_to(ActionStatus::Running));
        assert!(ActionStatus::Running.can_transition_to(ActionStatus::Failed));
        assert!(!ActionStatus::Completed.can_transition_to(ActionStatus::Waiting));
    }

    #[test]
    fn unknown_values_fail_closed() {
        assert_eq!(
            SessionStatus::from_status_str("bogus"),
            SessionStatus::Error
        );
        assert_eq!(
            ActionStatus::from_status_str("scheduled"),
            ActionStatus::Failed
        );
    }
}
