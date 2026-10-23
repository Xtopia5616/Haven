//! Stable per-session working-directory binding stored in the durable event stream.

use anyhow::{Context, ensure};
use haven_common::types::is_canonical_id;
use haven_memory::{SESSION_EXECUTION_WORKSPACE_EVENT_TYPE, SessionEvent};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionExecutionWorkspaceEvent {
    pub root: Option<PathBuf>,
}

/// `None` means no binding event exists. `Some(None)` is a durable binding to
/// an unavailable/automatic-free state; `Some(Some(path))` is a chosen path.
pub(crate) fn session_execution_workspace_from_events(
    events: &[SessionEvent],
) -> anyhow::Result<Option<Option<PathBuf>>> {
    let Some(event) = events
        .iter()
        .filter(|event| event.event_type == SESSION_EXECUTION_WORKSPACE_EVENT_TYPE)
        .max_by_key(|event| event.sequence)
    else {
        return Ok(None);
    };
    let payload: SessionExecutionWorkspaceEvent = serde_json::from_str(&event.payload)
        .context("invalid session execution workspace event payload")?;
    if let Some(root) = &payload.root {
        validate_session_execution_workspace_root(root)?;
    }
    Ok(Some(payload.root))
}

pub(crate) fn validate_session_execution_workspace_root(
    root: &std::path::Path,
) -> anyhow::Result<()> {
    ensure!(
        root.is_absolute() && !is_unc_or_device_path(root),
        "session execution workspace must be an absolute local path"
    );
    Ok(())
}

pub(crate) fn validate_session_id(session_id: &str) -> anyhow::Result<()> {
    ensure!(
        is_canonical_id(session_id, "ses"),
        "session_id must be a canonical ses-* id"
    );
    Ok(())
}

fn is_unc_or_device_path(path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        value.starts_with("\\\\") || value.starts_with("//")
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: i64, root: Option<PathBuf>) -> SessionEvent {
        SessionEvent {
            session_id: "ses-00000000000000000000000000000001".into(),
            sequence,
            event_type: SESSION_EXECUTION_WORKSPACE_EVENT_TYPE.into(),
            payload: serde_json::to_string(&SessionExecutionWorkspaceEvent { root }).unwrap(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            run_id: None,
            step_number: None,
        }
    }

    #[test]
    fn latest_binding_can_select_or_clear_the_workspace() {
        let first = std::env::temp_dir().join("haven-first");
        let latest = std::env::temp_dir().join("haven-latest");
        assert_eq!(
            session_execution_workspace_from_events(&[
                event(1, Some(first.clone())),
                event(2, Some(latest.clone())),
            ])
            .unwrap(),
            Some(Some(latest))
        );
        assert_eq!(
            session_execution_workspace_from_events(&[event(1, Some(first)), event(2, None)])
                .unwrap(),
            Some(None)
        );
    }

    #[test]
    fn malformed_or_relative_binding_fails_closed() {
        let mut latest = event(1, Some(PathBuf::from("relative")));
        assert!(session_execution_workspace_from_events(&[latest.clone()]).is_err());
        latest.payload = "not-json".into();
        assert!(session_execution_workspace_from_events(&[latest]).is_err());
    }
}
