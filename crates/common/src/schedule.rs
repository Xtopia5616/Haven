//! Shared vocabulary for Schedule persistence and execution.

/// What action a Schedule occurrence performs.
///
/// This closed value is shared by the execution owner and Memory's write
/// boundary. Persisted rows are still read as raw text so restore can quarantine
/// malformed historical data instead of failing before it reaches that path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleMode {
    /// Call the tool in `tool_name` with `tool_args` (no LLM involved).
    /// To send a message at fire time, call the `notify` tool here.
    #[default]
    Tool,
    /// Resume the session that owns the Schedule, delivering its prompt as
    /// a new instruction in the same session.
    Continue,
    /// Create a fresh session and start it with the stored prompt.
    NewSession,
}

impl ScheduleMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Continue => "continue",
            Self::NewSession => "new_session",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "tool" => Some(Self::Tool),
            "continue" => Some(Self::Continue),
            "new_session" => Some(Self::NewSession),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScheduleMode;

    #[test]
    fn schedule_mode_uses_one_closed_persisted_vocabulary() {
        for (mode, value) in [
            (ScheduleMode::Tool, "tool"),
            (ScheduleMode::Continue, "continue"),
            (ScheduleMode::NewSession, "new_session"),
        ] {
            assert_eq!(mode.as_str(), value);
            assert_eq!(ScheduleMode::parse(value), Some(mode));
            assert_eq!(
                serde_json::to_string(&mode).unwrap(),
                format!("\"{value}\"")
            );
            assert_eq!(
                serde_json::from_str::<ScheduleMode>(&format!("\"{value}\"")).unwrap(),
                mode
            );
        }

        assert_eq!(ScheduleMode::parse("future"), None);
        assert!(serde_json::from_str::<ScheduleMode>("\"future\"").is_err());
    }
}
