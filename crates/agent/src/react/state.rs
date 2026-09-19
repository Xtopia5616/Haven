//! State owned by one ReAct run.
//!
//! A run has one durable transcript and one in-memory projection of that
//! transcript. Keeping them together makes the authority rule executable at
//! the type boundary: Run, Turn and ToolBatch all receive the same state
//! object instead of independently borrowing three collections.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::types::{BranchPoint, TranscriptRecord};
use haven_common::types::CanonicalMessage;

// A revision starts at zero for every freshly rebuilt state.  The generation
// distinguishes two different in-memory projections for the same session
// (most importantly rollback/resume) so a per-session sidecar can never
// mistake a new canonical vector for the old revision zero.
static NEXT_CANONICAL_GENERATION: AtomicU64 = AtomicU64::new(1);

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
    /// Event indexes that can carry durable media metadata. Request-context
    /// reconstruction walks this compact index instead of scanning every
    /// thought/tool event in a long session.
    pub(crate) media_event_indices: Vec<usize>,
    pub(crate) canonical: Vec<CanonicalMessage>,
    pub(crate) branch_points: HashMap<u32, BranchPoint>,
    /// Stable ids of already projected user injections.  This index is built
    /// once when the durable state is loaded and updated on append, so an
    /// inbox redelivery does not rescan the complete event vector.
    applied_inject_message_ids: HashSet<String>,
    /// Monotonic revision for the in-memory canonical projection.  The
    /// token-estimate sidecar uses this instead of serializing the whole
    /// transcript just to prove that its cached value is still current.
    canonical_revision: u64,
    /// Identity of this in-memory canonical projection.  This is deliberately
    /// not persisted: a rebuilt state must cold-start its process-local cache.
    canonical_generation: u64,
    retry_nudge: Option<RetryNudge>,
}

impl ReActState {
    pub(crate) fn new(
        events: Vec<TranscriptRecord>,
        canonical: Vec<CanonicalMessage>,
        branch_points: HashMap<u32, BranchPoint>,
    ) -> Self {
        let mut media_event_indices = Vec::new();
        for (index, event) in events.iter().enumerate() {
            if matches!(
                event,
                TranscriptRecord::UserInject { .. }
                    | TranscriptRecord::MediaPlan { .. }
                    | TranscriptRecord::CompactSummary { .. }
            ) {
                if matches!(event, TranscriptRecord::CompactSummary { .. }) {
                    media_event_indices.clear();
                }
                media_event_indices.push(index);
            }
        }
        Self {
            applied_inject_message_ids: events
                .iter()
                .filter_map(|event| match event {
                    TranscriptRecord::UserInject {
                        message_id: Some(message_id),
                        ..
                    } => Some(message_id.clone()),
                    _ => None,
                })
                .collect(),
            events,
            media_event_indices,
            canonical,
            branch_points,
            canonical_revision: 0,
            canonical_generation: NEXT_CANONICAL_GENERATION.fetch_add(1, Ordering::Relaxed),
            retry_nudge: None,
        }
    }

    pub(crate) fn canonical_revision(&self) -> u64 {
        self.canonical_revision
    }

    pub(crate) fn canonical_generation(&self) -> u64 {
        self.canonical_generation
    }

    /// Mark a non-append canonical edit (for example a MEMORY fence refresh).
    /// The next estimate will perform one full tokenization pass because the
    /// prior append delta can no longer be trusted.
    pub(crate) fn mark_canonical_changed(&mut self) {
        self.canonical_revision = self.canonical_revision.wrapping_add(1);
    }

    /// Mark one message appended to the canonical projection.
    pub(crate) fn mark_canonical_append(&mut self) {
        self.mark_canonical_changed();
    }

    pub(crate) fn stage_retry_nudge(&mut self, tool_call_id: String, text: String) {
        self.retry_nudge = Some(RetryNudge { tool_call_id, text });
    }

    pub(crate) fn take_retry_nudge(&mut self) -> Option<RetryNudge> {
        self.retry_nudge.take()
    }

    pub(crate) fn has_applied_inject(&self, message_id: &str) -> bool {
        self.applied_inject_message_ids.contains(message_id)
    }

    pub(crate) fn push_event(&mut self, record: TranscriptRecord) {
        let media_event = matches!(
            &record,
            TranscriptRecord::UserInject { .. }
                | TranscriptRecord::MediaPlan { .. }
                | TranscriptRecord::CompactSummary { .. }
        );
        if matches!(&record, TranscriptRecord::CompactSummary { .. }) {
            self.media_event_indices.clear();
        }
        let event_index = self.events.len();
        if let TranscriptRecord::UserInject {
            message_id: Some(message_id),
            ..
        } = &record
        {
            self.applied_inject_message_ids.insert(message_id.clone());
        }
        self.events.push(record);
        if media_event {
            self.media_event_indices.push(event_index);
        }
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
        self.media_event_indices = vec![0];
        self.applied_inject_message_ids = self
            .events
            .iter()
            .filter_map(|event| match event {
                TranscriptRecord::UserInject {
                    message_id: Some(message_id),
                    ..
                } => Some(message_id.clone()),
                _ => None,
            })
            .collect();
        self.canonical = compacted;
        self.branch_points.clear();
        self.mark_canonical_changed();
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
            media_inputs: Vec::new(),
            summary: "summary".into(),
            tokens_before: 100,
            tokens_after: 20,
            episode_id: "msg-summary".into(),
            degraded: false,
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

    #[test]
    fn rebuilt_state_gets_a_new_generation_even_when_revision_restarts_at_zero() {
        let first = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user_text("first")],
            HashMap::new(),
        );
        let second = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user_text("second")],
            HashMap::new(),
        );

        assert_eq!(first.canonical_revision(), 0);
        assert_eq!(second.canonical_revision(), 0);
        assert_ne!(first.canonical_generation(), second.canonical_generation());
    }
}
