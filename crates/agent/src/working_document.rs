//! Session-scoped, freeform working notes persisted in the append-only event stream.

use anyhow::{Context, ensure};
use haven_common::types::is_canonical_id;
use haven_memory::SessionEvent;
use serde::{Deserialize, Serialize};

pub const SESSION_WORKING_DOCUMENT_EVENT_TYPE: &str = "session_working_document_replaced";
pub const MAX_SESSION_WORKING_DOCUMENT_BYTES: usize =
    haven_common::types::MAX_SESSION_WORKING_DOCUMENT_BYTES;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SessionWorkingDocumentEvent {
    pub markdown: String,
}

/// Return the latest active working document. Empty text represents a cleared document.
pub fn session_working_document_from_events(
    events: &[SessionEvent],
) -> anyhow::Result<Option<String>> {
    let Some(event) = events
        .iter()
        .filter(|event| event.event_type == SESSION_WORKING_DOCUMENT_EVENT_TYPE)
        .max_by_key(|event| event.sequence)
    else {
        return Ok(None);
    };
    let payload: SessionWorkingDocumentEvent = serde_json::from_str(&event.payload)
        .context("invalid session working document event payload")?;
    validate_session_working_document(&payload.markdown)?;
    Ok((!payload.markdown.is_empty()).then_some(payload.markdown))
}

pub fn validate_session_working_document(markdown: &str) -> anyhow::Result<()> {
    ensure!(
        markdown.len() <= MAX_SESSION_WORKING_DOCUMENT_BYTES,
        "working document exceeds the {MAX_SESSION_WORKING_DOCUMENT_BYTES}-byte limit"
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

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: i64, markdown: &str) -> SessionEvent {
        SessionEvent {
            session_id: "ses-00000000000000000000000000000001".into(),
            sequence,
            event_type: SESSION_WORKING_DOCUMENT_EVENT_TYPE.into(),
            payload: serde_json::to_string(&SessionWorkingDocumentEvent {
                markdown: markdown.into(),
            })
            .unwrap(),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            run_id: None,
            step_number: None,
        }
    }

    #[test]
    fn latest_event_replaces_or_clears_the_document() {
        assert_eq!(
            session_working_document_from_events(&[event(1, "old"), event(2, "# latest")])
                .unwrap()
                .as_deref(),
            Some("# latest")
        );
        assert_eq!(
            session_working_document_from_events(&[event(1, "old"), event(2, "")]).unwrap(),
            None
        );
    }

    #[test]
    fn working_document_size_is_bounded_in_utf8_bytes() {
        validate_session_working_document(&"a".repeat(MAX_SESSION_WORKING_DOCUMENT_BYTES)).unwrap();
        assert!(
            validate_session_working_document(&"a".repeat(MAX_SESSION_WORKING_DOCUMENT_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn malformed_latest_event_fails_closed() {
        let mut latest = event(2, "ok");
        latest.payload = "not-json".into();
        assert!(session_working_document_from_events(&[event(1, "earlier"), latest]).is_err());
    }
}
