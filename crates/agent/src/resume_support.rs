//! Pure and storage-facing helpers used by the session resume boundary.
//!
//! The resume driver owns lifecycle transitions and ReAct execution. This
//! module owns the small pieces that must remain deterministic and independent
//! of that orchestration: merging durable recovery candidates and decoding the
//! saved lazy-capability selections.

use std::collections::HashSet;

use haven_memory::repositories::messages::Message;
use haven_memory::repositories::session_steps::SessionStep;
use serde_json::Value;

use crate::types::{Action, TranscriptRecord};

/// Infer the next step when the optional checkpoint cache is unavailable or
/// is older than the durable event stream. A completed tool result advances
/// the loop; a user inject remains the input for its recorded step. This is a
/// conservative fallback for crash recovery — a current cache, when present,
/// still carries the exact next-step boundary.
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
            TranscriptRecord::CompactSummary { .. } => {
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
pub(crate) fn load_builtin_selection(input: &Value) -> (Option<Vec<String>>, Option<Vec<String>>) {
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

/// Reconcile a snapshot whose last event is an assistant tool call with the
/// durable action-step projection.
///
/// The normal write order is projection first, then the in-memory event log,
/// then a later snapshot checkpoint. A process crash can therefore leave a
/// snapshot ending at `ToolCall` while `session_steps` already knows that the
/// call completed (or that it crossed an uncertain side-effect boundary).
/// The old recovery path trimmed that call and minted a new identity, which
/// could repeat an external side effect. Recovery is fail-closed instead:
/// every call in the dangling batch receives either its durable observation or
/// an explicit unknown observation, and the caller can safely keep the event
/// log without executing the batch again.
///
/// The function is deliberately pure. The caller owns the one durable write
/// of the repaired snapshot, so a crash before that write leaves the same
/// dangling input and the next resume repeats this idempotent reconciliation.
pub(crate) fn reconcile_dangling_tool_call(
    events: &mut Vec<TranscriptRecord>,
    steps: &[SessionStep],
) -> bool {
    let Some(TranscriptRecord::ToolCall {
        step_number,
        tool_calls,
        ..
    }) = events.last()
    else {
        return false;
    };
    if tool_calls.is_empty() {
        return false;
    }

    let step_number = *step_number;
    let mut recovered = Vec::with_capacity(tool_calls.len());
    for (action_index, call) in tool_calls.iter().enumerate() {
        let matched = steps.iter().find(|step| {
            step.step_number == step_number as i32
                && step.action_index == action_index as i32
                && step.action_tool.as_deref() == Some(call.name.as_str())
                && (call.id.is_empty() || step.tool_call_id.as_deref() == Some(call.id.as_str()))
        });

        let (step_id, observation) = match matched {
            Some(step) => {
                let observation = match (step.status.as_str(), step.observation.as_deref()) {
                    ("completed" | "failed" | "cancelled", Some(text)) if !text.is_empty() => {
                        text.to_string()
                    }
                    (status, _) => format!(
                        "[recovery:unknown] durable tool intent is {status}; the operation may have produced an external side effect. Do not retry automatically; ask the user whether to verify it."
                    ),
                };
                (step.id.clone(), observation)
            }
            None => (
                haven_common::types::new_id("step"),
                "[recovery:unknown] the tool call was durable in the assistant transcript, but no matching action result was found. The operation may have produced an external side effect. Do not retry automatically; ask the user whether to verify it.".to_string(),
            ),
        };

        recovered.push(TranscriptRecord::ToolResult {
            step_number,
            action_index: action_index as u32,
            step_id,
            canonical_observation: observation.clone(),
            history_observation: observation,
            tool_call_id: (!call.id.is_empty()).then(|| call.id.clone()),
            action: Action {
                tool_name: call.name.clone(),
                tool_input: call.arguments.clone(),
                is_final: false,
                tool_call_id: (!call.id.is_empty()).then(|| call.id.clone()),
            },
        });
    }
    events.extend(recovered);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::CanonicalToolCall;

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
            load_builtin_selection(&serde_json::json!({
                "operations": [" files.read ", "files.read", 7],
                "roots": ["system", "system"]
            })),
            (Some(vec!["files.read".into()]), Some(vec!["system".into()]))
        );
        assert_eq!(
            load_builtin_selection(&serde_json::json!({"operations": "files.read"})),
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

    fn dangling_call() -> TranscriptRecord {
        TranscriptRecord::ToolCall {
            step_number: 4,
            text: String::new(),
            tool_calls: vec![CanonicalToolCall {
                id: "call-read".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "notes.txt"}),
            }],
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }
    }

    fn action_step(status: &str, observation: Option<&str>) -> SessionStep {
        SessionStep {
            id: "step-read".into(),
            session_id: "ses-test".into(),
            step_number: 4,
            action_index: 0,
            thought: None,
            action_tool: Some("read_file".into()),
            action_input: Some(r#"{"path":"notes.txt"}"#.into()),
            tool_call_id: Some("call-read".into()),
            observation: observation.map(str::to_string),
            status: status.into(),
            is_high_risk: false,
            confirmed: None,
            silent: false,
            started_at: None,
            completed_at: None,
            created_at: "2026-09-07T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn reconcile_reuses_completed_step_observation_without_replaying() {
        let mut events = vec![dangling_call()];
        assert!(reconcile_dangling_tool_call(
            &mut events,
            &[action_step("completed", Some("contents"))]
        ));
        assert!(matches!(
            events.last(),
            Some(TranscriptRecord::ToolResult {
                step_id,
                canonical_observation,
                ..
            }) if step_id == "step-read" && canonical_observation == "contents"
        ));
    }

    #[test]
    fn reconcile_pending_step_is_unknown_and_never_replayed() {
        let mut events = vec![dangling_call()];
        assert!(reconcile_dangling_tool_call(
            &mut events,
            &[action_step("running", None)]
        ));
        assert!(matches!(
            events.last(),
            Some(TranscriptRecord::ToolResult {
                canonical_observation,
                ..
            }) if canonical_observation.contains("recovery:unknown")
        ));
    }
}
