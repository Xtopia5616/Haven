use super::*;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub sequence: i64,
    pub event_type: String,
    pub payload: String,
    pub created_at: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
}

/// One bounded page from the append-only session event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventPage {
    pub events: Vec<SessionEvent>,
    /// Sequence to pass as `after_sequence` for the next page. Remains the
    /// input cursor when this page contains no events.
    pub next_cursor: i64,
    pub has_more: bool,
}

/// One bounded transcript event returned by model-facing history search.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionTranscriptHistoryMatch {
    pub sequence: i64,
    pub created_at: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
    pub redacted_fields: Vec<String>,
    pub payload_excerpt: String,
    pub payload_offset_bytes: usize,
    pub payload_source_bytes: usize,
    pub payload_truncated: bool,
}

/// Bounded search results over the current session's non-rolled-back raw
/// transcript. `has_more` means older matches exist before the oldest result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionTranscriptHistorySearch {
    pub matches: Vec<SessionTranscriptHistoryMatch>,
    pub latest_sequence: i64,
    pub before_sequence: Option<i64>,
    pub scanned_events: usize,
    pub scanned_bytes: usize,
    pub has_more: bool,
}

/// Cursor for paging newest-first across the current session's directly
/// linked sessions. The tuple is ordered by timestamp, then session identity,
/// then sequence so equal timestamps still have a stable page boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionLinkedTranscriptHistoryCursor {
    pub created_at: String,
    pub session_id: String,
    pub sequence: i64,
}

/// One transcript match from a directly linked session. Transcript fields are
/// flattened so the source identity sits beside sequence and excerpt data.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionLinkedTranscriptHistoryMatch {
    pub session_id: String,
    #[serde(flatten)]
    pub transcript: SessionTranscriptHistoryMatch,
}

/// Bounded search results across the current session's direct links.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionLinkedTranscriptHistorySearch {
    pub matches: Vec<SessionLinkedTranscriptHistoryMatch>,
    pub linked_sessions_searched: usize,
    pub next_before: Option<SessionLinkedTranscriptHistoryCursor>,
    pub scanned_events: usize,
    pub scanned_bytes: usize,
    pub has_more: bool,
}

/// One managed asset linked to a transcript event, with bounded text context.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionTranscriptAsset {
    pub asset_id: String,
    pub media_type: String,
    pub media_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_representation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_content_excerpt: Option<String>,
    pub cached_content_truncated: bool,
}

/// One user-input or tool-output event associated with one or more assets.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionTranscriptAssetMatch {
    pub sequence: i64,
    pub created_at: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tool: Option<String>,
    pub context_excerpt: String,
    pub assets: Vec<SessionTranscriptAsset>,
}

/// Bounded asset/context search over the current non-rolled-back transcript.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionTranscriptAssetSearch {
    pub matches: Vec<SessionTranscriptAssetMatch>,
    pub latest_sequence: i64,
    pub before_sequence: Option<i64>,
    pub scanned_events: usize,
    pub scanned_bytes: usize,
    pub has_more: bool,
}

/// One bounded raw transcript event returned by an exact sequence lookup.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionTranscriptHistoryRecord {
    pub sequence: i64,
    pub created_at: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
    pub redacted_fields: Vec<String>,
    pub payload: String,
    pub payload_source_bytes: usize,
    pub payload_truncated: bool,
}

/// Public limits for the model-facing transcript history query contract.
pub const MAX_SESSION_HISTORY_QUERY_BYTES: usize = 256;
pub const MAX_SESSION_HISTORY_SEARCH_RESULTS: usize = 5;
pub const MAX_SESSION_ASSET_SEARCH_RESULTS: usize = 5;
/// Maximum direct links considered by one linked-history search.
pub const MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS: usize = 32;
/// Maximum timestamp bytes accepted in a linked-history paging cursor.
pub const MAX_SESSION_LINKED_HISTORY_CURSOR_TIMESTAMP_BYTES: usize = 128;

/// Maximum number of durable events returned by a single replay page.
pub(super) const MAX_SESSION_EVENT_REPLAY_PAGE_SIZE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionEventInput {
    pub event_type: String,
    pub payload: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
}

#[derive(serde::Serialize)]
pub(super) struct SessionEventInputRef<'a> {
    pub(super) event_type: &'a str,
    pub(super) payload: &'a str,
    pub(super) run_id: Option<u64>,
    pub(super) step_number: Option<u32>,
}

impl<'a> From<&'a SessionEventInput> for SessionEventInputRef<'a> {
    fn from(event: &'a SessionEventInput) -> Self {
        Self {
            event_type: &event.event_type,
            payload: &event.payload,
            run_id: event.run_id,
            step_number: event.step_number,
        }
    }
}

#[derive(Default)]
pub(super) struct BoundedByteCounter {
    pub(super) bytes: usize,
    pub(super) exceeded: bool,
}

impl std::io::Write for BoundedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next_bytes = self.bytes.saturating_add(buffer.len());
        if next_bytes > MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "session event payload budget exceeded",
            ));
        }
        self.bytes = next_bytes;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn session_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionEvent> {
    let session_id: String = row.get(0)?;
    if !is_canonical_id(&session_id, "ses") {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            "invalid persisted session_events.session_id".into(),
        ));
    }
    let run_id = row
        .get::<_, Option<i64>>(5)?
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Integer,
                    error.into(),
                )
            })
        })
        .transpose()?;
    Ok(SessionEvent {
        session_id,
        sequence: row.get(1)?,
        event_type: row.get(2)?,
        payload: row.get(3)?,
        created_at: row.get(4)?,
        run_id,
        step_number: row.get(6)?,
    })
}
