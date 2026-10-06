//! Pure and storage-facing helpers used by the session resume boundary.
//!
//! The resume driver owns lifecycle transitions and ReAct execution. This
//! module owns the small pieces that must remain deterministic and independent
//! of that orchestration: merging durable recovery candidates and decoding the
//! saved lazy-capability selections.

use std::collections::HashSet;

use serde_json::Value;

#[cfg(test)]
use crate::types::ToolCall;
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

/// Decode the optional tool subset recorded by a `load_mcp` ToolCall.
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
/// so a malformed historical ToolCall cannot widen a selection during resume.
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
                tool_index: 0,
                step_id: "step-tool".into(),
                canonical_observation: "ok".into(),
                history_observation: "ok".into(),
                tool_call_id: Some("call-tool".into()),
                tool_call: ToolCall {
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
