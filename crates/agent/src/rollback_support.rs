//! Pure event-log operations used by the rollback boundary.
//!
//! Database truncation and lifecycle coordination stay in `rollback.rs`.
//! These helpers only mutate the in-memory events authority and its branch
//! cursor metadata, which keeps the dangerous timeline operation deterministic
//! and directly unit-testable.

use std::collections::HashMap;

use haven_common::types::{CanonicalRole, ContentPart, InjectSource};

use crate::types::{BranchPoint, TranscriptRecord};

/// Remove a dangling assistant tool call at the end of an event log.
///
/// A branch point can be persisted after `ToolCall` but before its results.
/// Keeping that unmatched call would make the next provider request invalid;
/// its same-step thought is removed with it so the loop can request the step
/// again cleanly. Empty tool-call lists represent final-answer/search turns
/// and are intentionally preserved.
pub(crate) fn trim_dangling_tool_call(events: &mut Vec<TranscriptRecord>) {
    let Some(TranscriptRecord::ToolCall {
        step_number,
        tool_calls,
        ..
    }) = events.last()
    else {
        return;
    };
    if tool_calls.is_empty() {
        return;
    }
    let step = *step_number;
    events.pop();
    if let Some(TranscriptRecord::Thought { step_number, .. }) = events.last()
        && *step_number == step
    {
        events.pop();
    }
}

/// Remove the target user inject and all following events.
///
/// New event records carry the durable message id, so that id is always
/// resolved first. Content matching is restricted to id-less legacy
/// `UserInject` records (or compacted canonical user rows, which predate the
/// message-id field). This prevents two identical user messages from rolling
/// back the wrong branch.
pub(crate) fn truncate_at_user_message(
    events: &mut Vec<TranscriptRecord>,
    branch_points: &mut HashMap<u32, BranchPoint>,
    target_step: u32,
    target_message_id: &str,
    target_content: &str,
) -> bool {
    let exact_position = events.iter().rposition(|event| {
        matches!(
            event,
            TranscriptRecord::UserInject {
                message_id: Some(message_id),
                ..
            } if message_id == target_message_id
        )
    });
    if let Some(position) = exact_position {
        truncate_events(events, branch_points, target_step, position);
        return true;
    }

    let prefixes = InjectSource::match_prefixes();
    let matches_legacy_content = |text: &str| {
        text == target_content
            || prefixes.iter().any(|prefix| {
                text.strip_prefix(prefix.as_str())
                    .is_some_and(|rest| rest == target_content)
            })
    };
    if let Some(position) = events.iter().rposition(|event| {
        matches!(
            event,
            TranscriptRecord::UserInject {
                message_id: None,
                text,
                ..
            } if matches_legacy_content(text)
        )
    }) {
        truncate_events(events, branch_points, target_step, position);
        return true;
    }

    // Compaction intentionally stores canonical messages, not their original
    // message ids. Content is the only available legacy fallback there.
    for index in (0..events.len()).rev() {
        if let TranscriptRecord::CompactSummary { compacted, .. } = &mut events[index]
            && let Some(position) = compacted.iter().rposition(|message| {
                message.role == CanonicalRole::User
                    && message.content.iter().any(|part| {
                        matches!(part, ContentPart::Text(text) if matches_legacy_content(text))
                    })
            })
        {
            compacted.truncate(position);
            events.truncate(index + 1);
            clamp_branch_points(events, branch_points, target_step);
            return true;
        }
    }
    false
}

fn truncate_events(
    events: &mut Vec<TranscriptRecord>,
    branch_points: &mut HashMap<u32, BranchPoint>,
    target_step: u32,
    position: usize,
) {
    events.truncate(position);
    clamp_branch_points(events, branch_points, target_step);
}

fn clamp_branch_points(
    events: &[TranscriptRecord],
    branch_points: &mut HashMap<u32, BranchPoint>,
    target_step: u32,
) {
    let event_len = events.len();
    branch_points.retain(|&step, branch| step <= target_step && branch.event_cursor <= event_len);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TranscriptRecord;
    use haven_common::types::InjectSource;

    fn inject(id: Option<&str>, text: &str) -> TranscriptRecord {
        TranscriptRecord::UserInject {
            step_number: 1,
            source: InjectSource::FollowUp,
            text: text.into(),
            attachments: Vec::new(),
            message_id: id.map(str::to_string),
        }
    }

    #[test]
    fn exact_message_id_wins_over_a_later_duplicate_text() {
        let mut events = vec![inject(Some("msg-target"), "same"), inject(None, "same")];
        let mut branch_points = HashMap::new();
        branch_points.insert(
            1,
            BranchPoint {
                event_cursor: 2,
                step_number: 1,
                last_msg_at: None,
            },
        );

        assert!(truncate_at_user_message(
            &mut events,
            &mut branch_points,
            1,
            "msg-target",
            "same"
        ));
        assert!(events.is_empty());
        assert!(branch_points.is_empty());
    }

    #[test]
    fn legacy_content_fallback_only_matches_idless_injects() {
        let mut events = vec![inject(Some("msg-other"), "same"), inject(None, "same")];
        let mut branch_points = HashMap::new();
        assert!(truncate_at_user_message(
            &mut events,
            &mut branch_points,
            1,
            "msg-missing",
            "same"
        ));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            TranscriptRecord::UserInject {
                message_id: Some(id),
                ..
            } if id == "msg-other"
        ));
    }

    #[test]
    fn empty_tool_call_is_not_trimmed() {
        let mut events = vec![TranscriptRecord::ToolCall {
            step_number: 1,
            text: "done".into(),
            tool_calls: Vec::new(),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        trim_dangling_tool_call(&mut events);
        assert_eq!(events.len(), 1);
    }
}
