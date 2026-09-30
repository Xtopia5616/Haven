//! Pure and storage-facing helpers used by the session resume boundary.
//!
//! The resume driver owns lifecycle transitions and ReAct execution. This
//! module owns the small pieces that must remain deterministic and independent
//! of that orchestration: merging durable recovery candidates and decoding the
//! saved lazy-capability selections.

use std::collections::HashSet;

use haven_memory::repositories::messages::Message;
use serde_json::Value;

#[cfg(test)]
use crate::types::Action;
use crate::types::TranscriptRecord;

/// Infer the next step from the durable event tail. A completed tool result
/// advances the loop; a user inject remains the input for its recorded step.
/// This is the only resume boundary after the JSON checkpoint was removed from
/// the recovery path.
pub(crate) fn infer_resume_step(events: &[TranscriptRecord]) -> u32 {
    let mut max_step = 0;
    let mut last = None;
    for event in events {
        match event {
            TranscriptRecord::Thought { step_number, .. }
            | TranscriptRecord::Reasoning { step_number, .. }
            | TranscriptRecord::ToolCall { step_number, .. }
            | TranscriptRecord::ToolResult { step_number, .. }
            | TranscriptRecord::UserInject { step_number, .. }
            | TranscriptRecord::MediaPlan { step_number, .. } => {
                max_step = max_step.max(*step_number);
                if !matches!(event, TranscriptRecord::MediaPlan { .. }) {
                    last = Some(event);
                }
            }
            TranscriptRecord::CompactSummary { step_number, .. } => {
                max_step = max_step.max(*step_number);
                last = Some(event);
            }
        }
    }
    match last {
        Some(TranscriptRecord::ToolResult { .. }) => max_step.saturating_add(1).max(1),
        Some(TranscriptRecord::UserInject { step_number, .. }) => (*step_number).max(1),
        _ => max_step.max(1),
    }
}

/// Merge the two durable recovery scans in their read order and deduplicate by
/// the persisted message id.
///
/// The first scan contains rows newer than the snapshot's ingress cursor; the
/// second contains recent anchor-less rows. A row can occur in both scans, so
/// resume must enqueue it once without comparing message text. Non-user rows
/// are intentionally ignored here because only user input can be re-queued.
pub(crate) fn merge_recovery_candidates(
    post_snapshot: Vec<Message>,
    undelivered: Vec<Message>,
) -> Vec<Message> {
    let mut candidates: Vec<_> = post_snapshot.into_iter().chain(undelivered).collect();
    candidates.sort_by(|left, right| {
        left.ingress_seq
            .cmp(&right.ingress_seq)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|message| message.role == "user" && seen.insert(message.id.clone()))
        .collect()
}

/// Decode the optional tool subset recorded by a `load_mcp` action.
///
/// Missing `tool_names` means load all tools (`None`). An explicitly empty
/// array means load no tools (`Some(vec![])`), and malformed entries are
/// ignored rather than widening the selection.
pub(crate) fn load_mcp_tool_names(input: &Value) -> Option<Vec<String>> {
    let array = input.get("tool_names")?.as_array()?;
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for value in array {
        let Some(name) = value.as_str() else {
            continue;
        };
        let name = name.trim();
        if !name.is_empty() && seen.insert(name.to_string()) {
            names.push(name.to_string());
        }
    }
    Some(names)
}

/// Decode the saved built-in lazy-load request. Missing arrays remain `None`
/// so a malformed historical action cannot widen a selection during resume.
pub(crate) fn builtin_selection(input: &Value) -> (Option<Vec<String>>, Option<Vec<String>>) {
    fn names(input: &Value, key: &str) -> Option<Vec<String>> {
        let array = input.get(key)?.as_array()?;
        let mut values = Vec::new();
        let mut seen = HashSet::new();
        for value in array {
            let Some(name) = value.as_str() else {
                continue;
            };
            let name = name.trim();
            if !name.is_empty() && seen.insert(name.to_string()) {
                values.push(name.to_string());
            }
        }
        Some(values)
    }

    (names(input, "operations"), names(input, "roots"))
}

/// Decode the saved Skill names without allowing malformed input to turn into
/// an implicit load-all request.
pub(crate) fn load_skill_names(input: &Value) -> Vec<String> {
    let Some(array) = input.get("skill_names").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for value in array {
        let Some(name) = value.as_str() else {
            continue;
        };
        let name = name.trim().trim_start_matches("skill__");
        if !name.is_empty() && seen.insert(name.to_string()) {
            names.push(name.to_string());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(id: &str, role: &str) -> Message {
        Message {
            id: id.into(),
            session_id: "ses-test".into(),
            role: role.into(),
            content: id.into(),
            message_type: Some("text".into()),
            created_at: "2026-08-27T00:00:00.000Z".into(),
            tool_call_id: None,
            attachments: Vec::new(),
            media_inputs: Vec::new(),
            voice: false,
            ingress_seq: 0,
        }
    }

    #[test]
    fn recovery_candidates_deduplicate_by_id_without_content_matching() {
        let candidates = merge_recovery_candidates(
            vec![
                message("msg-first", "user"),
                message("msg-assistant", "assistant"),
            ],
            vec![message("msg-first", "user"), message("msg-second", "user")],
        );
        let ids: Vec<_> = candidates.into_iter().map(|message| message.id).collect();
        assert_eq!(ids, ["msg-first", "msg-second"]);
    }

    #[test]
    fn infer_resume_step_uses_the_durable_tail() {
        let events = vec![
            TranscriptRecord::CompactSummary {
                step_number: 1,
                compacted: Vec::new(),
                media_inputs: Vec::new(),
                summary: String::new(),
                tokens_before: 0,
                tokens_after: 0,
                episode_id: "msg-summary".into(),
                degraded: false,
            },
            TranscriptRecord::ToolResult {
                step_number: 4,
                action_index: 0,
                step_id: "step-tool".into(),
                canonical_observation: "ok".into(),
                history_observation: "ok".into(),
                tool_call_id: Some("call-tool".into()),
                action: Action {
                    tool_name: "read".into(),
                    tool_input: serde_json::json!({}),
                    is_final: false,
                    tool_call_id: Some("call-tool".into()),
                },
            },
        ];
        assert_eq!(infer_resume_step(&events), 5);
    }

    #[test]
    fn infer_resume_step_preserves_the_compaction_root_step() {
        let events = vec![TranscriptRecord::CompactSummary {
            step_number: 17,
            compacted: Vec::new(),
            media_inputs: Vec::new(),
            summary: "summary".into(),
            tokens_before: 100,
            tokens_after: 20,
            episode_id: "msg-summary".into(),
            degraded: false,
        }];

        assert_eq!(infer_resume_step(&events), 17);
    }

    #[test]
    fn load_mcp_tool_names_preserves_empty_subset_and_deduplicates() {
        assert_eq!(
            load_mcp_tool_names(&serde_json::json!({"server_name": "srv"})),
            None
        );
        assert_eq!(
            load_mcp_tool_names(&serde_json::json!({
                "tool_names": [" a ", "", "a", 7, "b"]
            })),
            Some(vec!["a".into(), "b".into()])
        );
        assert_eq!(
            load_mcp_tool_names(&serde_json::json!({"tool_names": []})),
            Some(Vec::new())
        );
    }

    #[test]
    fn lazy_loader_selections_deduplicate_without_widening() {
        assert_eq!(
            builtin_selection(&serde_json::json!({
                "operations": [" files.read ", "files.read", 7],
                "roots": ["system", "system"]
            })),
            (Some(vec!["files.read".into()]), Some(vec!["system".into()]))
        );
        assert_eq!(
            builtin_selection(&serde_json::json!({"operations": "files.read"})),
            (None, None)
        );
        assert_eq!(
            load_skill_names(&serde_json::json!({
                "skill_names": [" echo ", "skill__echo", "", 4, "writer"]
            })),
            ["echo", "writer"]
        );
        assert!(load_skill_names(&serde_json::json!({"skill_names": "echo"})).is_empty());
    }
}
