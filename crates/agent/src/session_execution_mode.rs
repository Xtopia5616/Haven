//! Durable user-facing execution preset bound to one session.

use anyhow::Context;
use haven_common::types::ExecutionMode;
use haven_memory::{SESSION_EXECUTION_MODE_EVENT_TYPE, SessionEvent};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionExecutionModeEvent {
    pub mode: ExecutionMode,
}

pub(crate) fn session_execution_mode_from_events(
    events: &[SessionEvent],
) -> anyhow::Result<Option<ExecutionMode>> {
    let Some(event) = events
        .iter()
        .filter(|event| event.event_type == SESSION_EXECUTION_MODE_EVENT_TYPE)
        .max_by_key(|event| event.sequence)
    else {
        return Ok(None);
    };
    let payload: SessionExecutionModeEvent = serde_json::from_str(&event.payload)
        .context("invalid session execution mode event payload")?;
    Ok(Some(payload.mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: i64, mode: ExecutionMode) -> SessionEvent {
        SessionEvent {
            session_id: "ses-00000000000000000000000000000001".into(),
            sequence,
            event_type: SESSION_EXECUTION_MODE_EVENT_TYPE.into(),
            payload: serde_json::to_string(&SessionExecutionModeEvent { mode }).unwrap(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            run_id: None,
            step_number: None,
        }
    }

    #[test]
    fn latest_mode_is_the_durable_session_choice() {
        assert_eq!(
            session_execution_mode_from_events(&[
                event(1, ExecutionMode::Analyze),
                event(2, ExecutionMode::Unattended),
            ])
            .unwrap(),
            Some(ExecutionMode::Unattended)
        );
    }

    #[test]
    fn malformed_mode_event_fails_closed() {
        let mut invalid = event(1, ExecutionMode::Analyze);
        invalid.payload = "not-json".into();
        assert!(session_execution_mode_from_events(&[invalid]).is_err());
    }
}
