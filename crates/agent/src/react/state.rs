//! State owned by one ReAct run.
//!
//! A run has one durable transcript and one in-memory projection of that
//! transcript. Keeping them together makes the authority rule executable at
//! the type boundary: Run, Turn and ToolBatch all receive the same state
//! object instead of independently borrowing three collections.

use std::collections::HashMap;

use crate::types::{BranchPoint, TranscriptRecord};
use haven_common::types::CanonicalMessage;

/// A retry hint that belongs to the next provider request only.
///
/// Tool failures are durable observations, but the instruction to retry is a
/// control-plane concern. Keeping it out of `canonical` means a pause/resume
/// cycle cannot turn an internal nudge into a user-visible transcript row.
pub(crate) struct RetryNudge {
    pub(crate) tool_call_id: String,
    pub(crate) text: String,
}

/// Mutable state for one session run.
///
/// `events` is the recovery authority. `canonical` is a hot projection used
/// for the next model request. Branch points index the current event log and
/// therefore live with it. `retry_nudge` is intentionally not part of either
/// persisted representation.
pub(crate) struct ReActState {
    pub(crate) events: Vec<TranscriptRecord>,
    pub(crate) canonical: Vec<CanonicalMessage>,
    pub(crate) branch_points: HashMap<u32, BranchPoint>,
    retry_nudge: Option<RetryNudge>,
}

impl ReActState {
    pub(crate) fn new(
        events: Vec<TranscriptRecord>,
        canonical: Vec<CanonicalMessage>,
        branch_points: HashMap<u32, BranchPoint>,
    ) -> Self {
        Self {
            events,
            canonical,
            branch_points,
            retry_nudge: None,
        }
    }

    pub(crate) fn stage_retry_nudge(&mut self, tool_call_id: String, text: String) {
        self.retry_nudge = Some(RetryNudge { tool_call_id, text });
    }

    pub(crate) fn take_retry_nudge(&mut self) -> Option<RetryNudge> {
        self.retry_nudge.take()
    }

    /// Replace the transcript root after compaction.
    ///
    /// Compaction changes both projections at once, so branch points into the
    /// discarded prefix must be invalidated in the same state transition.
    /// Keeping this invariant here prevents callers from updating only one of
    /// the three pieces of run state.
    pub(crate) fn replace_with_compaction(
        &mut self,
        record: TranscriptRecord,
        compacted: Vec<CanonicalMessage>,
    ) {
        self.events = vec![record];
        self.canonical = compacted;
        self.branch_points.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_keeps_event_projection_and_branch_index_together() {
        let canonical = vec![CanonicalMessage::user_text("hello")];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            3,
            BranchPoint {
                event_cursor: 2,
                step_number: 3,
                last_msg_at: None,
            },
        );

        let state = ReActState::new(Vec::new(), canonical, branch_points);

        assert_eq!(state.events.len(), 0);
        assert_eq!(state.canonical.len(), 1);
        assert!(state.branch_points.contains_key(&3));
        assert!(state.retry_nudge.is_none());
    }

    #[test]
    fn compaction_replaces_transcript_and_invalidates_branch_points() {
        let mut state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());
        state.branch_points.insert(
            1,
            BranchPoint {
                event_cursor: 1,
                step_number: 1,
                last_msg_at: None,
            },
        );

        let record = TranscriptRecord::CompactSummary {
            compacted: Vec::new(),
            summary: "summary".into(),
            tokens_before: 100,
            tokens_after: 20,
            episode_id: "msg-summary".into(),
        };
        state.replace_with_compaction(record, vec![CanonicalMessage::user_text("recent")]);

        assert_eq!(state.events.len(), 1);
        assert_eq!(state.canonical.len(), 1);
        assert!(state.branch_points.is_empty());
    }

    #[test]
    fn retry_nudge_is_staged_without_mutating_the_canonical_projection() {
        use haven_common::types::ContentPart;

        let canonical = vec![CanonicalMessage::user_text("hello")];
        let mut state = ReActState::new(Vec::new(), canonical.clone(), HashMap::new());

        state.stage_retry_nudge("call-1".into(), "retry safely".into());

        assert_eq!(state.canonical.len(), canonical.len());
        assert!(matches!(
            state.canonical[0].content.as_slice(),
            [ContentPart::Text(text)] if text == "hello"
        ));
        let nudge = state.take_retry_nudge().expect("staged retry nudge");
        assert_eq!(nudge.tool_call_id, "call-1");
        assert_eq!(nudge.text, "retry safely");
        assert!(state.take_retry_nudge().is_none());
    }
}
