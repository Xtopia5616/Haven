//! State owned by one ReAct run.
//!
//! A run has one durable transcript and one in-memory projection of that
//! transcript. Keeping them together makes the authority rule executable at
//! the type boundary: Run, Turn and ToolBatch all receive the same state
//! object instead of independently borrowing three collections.
//!
//! ADR 0214：生产 run 的 `ReActState` 被 active run future 捕获；该 future
//! 存在所属 session 的 `SessionState::react_run` 中，并由同一个 actor task
//! 与 mailbox 一起轮询。它只在当前 run 内可变，不跨 session 共享，也不经
//! mailbox 往返。单测可直接构造该 projection 验证纯状态转换。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use super::identity::{IdentityMap, StreamBlockIdentity};
use super::{CanonicalMediaSummary, MediaRequirements, canonical_media_summary};
use crate::token_budget::estimate_message_tokens;
use crate::types::{BranchPoint, TranscriptRecord};
use haven_common::types::CanonicalMessage;

#[derive(Debug, Clone, Copy)]
struct TokenEstimate {
    message_count: usize,
    tokens: u32,
}

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
    /// Shared immutable request snapshots avoid copying the complete
    /// transcript at every turn head. Transcript writers use `Arc::make_mut`
    /// so a still-live request snapshot remains isolated from durable state.
    pub(crate) canonical: Arc<Vec<CanonicalMessage>>,
    media_summary: CanonicalMediaSummary,
    pub(crate) branch_points: HashMap<u32, BranchPoint>,
    /// Stable ids of already projected user injections.  This index is built
    /// once when the durable state is loaded and updated on append, so an
    /// inbox redelivery does not rescan the complete event vector.
    applied_inject_message_ids: HashSet<String>,
    /// Process-local token estimate for this canonical projection. It is
    /// intentionally owned by the run state so it cannot be reused by another
    /// session or by a rebuilt projection with the same message count.
    token_estimate: Option<TokenEstimate>,
    /// Streamed assistant block ids shared by stream events and the final
    /// transcript projection for this run. The state lifetime scopes the map
    /// to one session run.
    pub(super) identity_map: Arc<IdentityMap>,
    retry_nudge: Option<RetryNudge>,
    /// The cancellation token for the currently executing turn. This is
    /// process-local and lets synchronous persistence share the same deadline
    /// as provider/tool execution without putting a token in the snapshot.
    pub(crate) turn_cancel: Option<CancellationToken>,
}

impl ReActState {
    pub(crate) fn new(
        events: Vec<TranscriptRecord>,
        canonical: Vec<CanonicalMessage>,
        branch_points: HashMap<u32, BranchPoint>,
    ) -> Self {
        let media_summary = canonical_media_summary(&canonical);
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
            canonical: Arc::new(canonical),
            media_summary,
            branch_points,
            token_estimate: None,
            identity_map: Arc::new(IdentityMap::default()),
            retry_nudge: None,
            turn_cancel: None,
        }
    }

    pub(super) fn stream_block_message_id_or_new(&self, identity: StreamBlockIdentity) -> String {
        self.identity_map.stream_block_message_id_or_new(identity)
    }

    /// Mark a non-append canonical edit (for example a MEMORY fence refresh).
    /// The next estimate will perform one full tokenization pass because any
    /// prior append delta can no longer be trusted.
    pub(crate) fn mark_canonical_changed(&mut self) {
        self.token_estimate = None;
        self.media_summary = canonical_media_summary(&self.canonical);
    }

    /// Mark one message appended to the canonical projection.
    pub(crate) fn mark_canonical_append(&mut self) {
        let Some(message) = self.canonical.last() else {
            self.token_estimate = None;
            return;
        };
        let appended_summary = canonical_media_summary(std::slice::from_ref(message));
        self.media_summary.requirements.image |= appended_summary.requirements.image;
        self.media_summary.requirements.audio |= appended_summary.requirements.audio;
        self.media_summary.requirements.video |= appended_summary.requirements.video;
        self.media_summary.media_part_count = self
            .media_summary
            .media_part_count
            .saturating_add(appended_summary.media_part_count);
        let Some(estimate) = &mut self.token_estimate else {
            return;
        };
        if estimate.message_count.saturating_add(1) != self.canonical.len() {
            self.token_estimate = None;
            return;
        }
        estimate.tokens = estimate
            .tokens
            .saturating_add(estimate_message_tokens(std::slice::from_ref(message)));
        estimate.message_count = self.canonical.len();
    }

    pub(crate) fn estimate_canonical_tokens(&mut self) -> u32 {
        if let Some(estimate) = self.token_estimate
            && estimate.message_count == self.canonical.len()
        {
            return estimate.tokens;
        }
        let tokens = estimate_message_tokens(&self.canonical);
        self.token_estimate = Some(TokenEstimate {
            message_count: self.canonical.len(),
            tokens,
        });
        tokens
    }

    pub(crate) fn media_requirements(&self) -> MediaRequirements {
        self.media_summary.requirements
    }

    pub(crate) fn media_summary(&self) -> CanonicalMediaSummary {
        self.media_summary
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
        self.canonical = Arc::new(compacted);
        self.media_summary = canonical_media_summary(&self.canonical);
        self.branch_points.clear();
        // Compaction creates a new canonical root. Any prior incremental
        // estimate described the discarded projection and must be rebuilt.
        self.token_estimate = None;
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
    fn stream_identity_is_shared_within_a_state_and_isolated_between_states() {
        let first = ReActState::new(Vec::new(), Vec::new(), HashMap::new());
        let second = ReActState::new(Vec::new(), Vec::new(), HashMap::new());

        let identity = StreamBlockIdentity::thought(3, 8);
        let first_id = first.identity_map.ensure_stream_block_message_id(identity);
        assert_eq!(
            first.identity_map.ensure_stream_block_message_id(identity),
            first_id
        );
        assert_eq!(first.stream_block_message_id_or_new(identity), first_id);
        assert_ne!(
            second.identity_map.ensure_stream_block_message_id(identity),
            first_id
        );
    }

    #[test]
    fn compaction_replaces_transcript_and_invalidates_branch_points() {
        let mut state = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user(vec![
                haven_common::types::ContentPart::Image {
                    content_type: "image".into(),
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                },
            ])],
            HashMap::new(),
        );
        assert_eq!(state.media_summary().media_part_count, 1);
        assert!(state.media_requirements().image);
        state.branch_points.insert(
            1,
            BranchPoint {
                event_cursor: 1,
                step_number: 1,
                last_msg_at: None,
            },
        );

        let record = TranscriptRecord::CompactSummary {
            step_number: 1,
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
        assert_eq!(state.media_summary().media_part_count, 0);
        assert_eq!(state.media_requirements(), MediaRequirements::default());
        assert!(state.branch_points.is_empty());
    }

    #[test]
    fn media_summary_updates_on_canonical_append() {
        let mut state = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user_text("hello")],
            HashMap::new(),
        );
        assert_eq!(state.media_summary().media_part_count, 0);

        Arc::make_mut(&mut state.canonical).push(CanonicalMessage::user(vec![
            haven_common::types::ContentPart::Audio {
                content_type: "audio".into(),
                media_type: "audio/wav".into(),
                data: "YXVkaW8=".into(),
            },
        ]));
        state.mark_canonical_append();

        assert_eq!(state.media_summary().media_part_count, 1);
        assert!(state.media_requirements().audio);
        assert!(!state.media_requirements().image);
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
    fn token_estimate_is_state_local_and_tracks_append_or_replacement() {
        let mut first = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user_text("short")],
            HashMap::new(),
        );
        let first_tokens = first.estimate_canonical_tokens();
        assert_eq!(first_tokens, estimate_message_tokens(&first.canonical));

        Arc::make_mut(&mut first.canonical).push(CanonicalMessage::user_text("appended"));
        first.mark_canonical_append();
        assert_eq!(
            first.estimate_canonical_tokens(),
            estimate_message_tokens(&first.canonical)
        );

        Arc::make_mut(&mut first.canonical)[0] = CanonicalMessage::user_text(
            "a substantially longer replacement with a different token cost",
        );
        first.mark_canonical_changed();
        assert_eq!(
            first.estimate_canonical_tokens(),
            estimate_message_tokens(&first.canonical)
        );

        let mut second = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user_text("a different state")],
            HashMap::new(),
        );
        assert_eq!(
            second.estimate_canonical_tokens(),
            estimate_message_tokens(&second.canonical)
        );
        assert_ne!(first_tokens, second.estimate_canonical_tokens());
    }

    #[test]
    fn compaction_invalidates_the_state_token_estimate() {
        let mut state = ReActState::new(
            Vec::new(),
            vec![CanonicalMessage::user_text("before")],
            HashMap::new(),
        );
        let _ = state.estimate_canonical_tokens();
        let record = TranscriptRecord::CompactSummary {
            step_number: 1,
            compacted: Vec::new(),
            media_inputs: Vec::new(),
            summary: "summary".into(),
            tokens_before: 100,
            tokens_after: 20,
            episode_id: "msg-summary".into(),
            degraded: false,
        };

        state.replace_with_compaction(record, vec![CanonicalMessage::user_text("after")]);

        assert_eq!(
            state.estimate_canonical_tokens(),
            estimate_message_tokens(&state.canonical)
        );
    }
}
