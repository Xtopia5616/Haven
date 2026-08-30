//! Pure and storage-facing helpers used by the session resume boundary.
//!
//! The resume driver owns lifecycle transitions and ReAct execution. This
//! module owns the small pieces that must remain deterministic and independent
//! of that orchestration: merging durable recovery candidates and decoding the
//! saved `load_mcp` selection.

use std::collections::HashSet;

use haven_common::types::{CanonicalMessage, CanonicalToolCall, ContentPart};
use haven_memory::Database;
use haven_memory::repositories::messages::Message;
use serde_json::Value;

/// Merge the two durable recovery scans in their read order and deduplicate by
/// the persisted message id.
///
/// The first scan contains rows newer than the snapshot's `saved_at`; the
/// second contains recent anchor-less rows. A row can occur in both scans, so
/// resume must enqueue it once without comparing message text. Non-user rows
/// are intentionally ignored here because only user input can be re-queued.
pub(crate) fn merge_recovery_candidates(
    post_snapshot: Vec<Message>,
    undelivered: Vec<Message>,
) -> Vec<Message> {
    let mut seen = HashSet::new();
    post_snapshot
        .into_iter()
        .chain(undelivered)
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

/// Project completed tool calls from the materialized step projection when the
/// snapshot row is absent.
///
/// This is the one intentionally lossy recovery path. A valid snapshot keeps
/// `events` as the sole authority; this helper is used only when no snapshot
/// exists and therefore synthesizes a stable local call id when the old tool
/// message did not preserve the provider id.
pub(crate) fn project_tool_chain_from_steps(
    db: &Database,
    session_id: &str,
    canonical: &mut Vec<CanonicalMessage>,
) {
    let Ok(steps) = db.get_session_steps(session_id) else {
        return;
    };
    let unused_tool_ids: Vec<(String, String)> = db
        .get_session_messages(session_id)
        .ok()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|message| {
            if message.role == "tool" {
                message.tool_call_id.map(|id| (message.content, id))
            } else {
                None
            }
        })
        .collect();
    let mut unused_tool_ids = unused_tool_ids;
    let mut projected = 0usize;
    for step in steps {
        let Some(tool) = step.action_tool else {
            continue;
        };
        if tool == "ask" {
            continue;
        }
        let Some(observation) = step.observation else {
            continue;
        };
        let arguments: Value = step
            .action_input
            .as_deref()
            .and_then(|input| serde_json::from_str(input).ok())
            .unwrap_or(Value::Null);
        let call_id = if let Some(position) = unused_tool_ids
            .iter()
            .position(|(content, _)| content == &observation)
        {
            let (_content, id) = unused_tool_ids.remove(position);
            id
        } else {
            format!("resumed_{}", step.id)
        };
        canonical.push(CanonicalMessage::assistant(
            Vec::new(),
            Some(vec![CanonicalToolCall {
                id: call_id.clone(),
                name: tool,
                arguments,
            }]),
            None,
            Vec::new(),
            Vec::new(),
        ));
        canonical.push(CanonicalMessage::tool(
            vec![ContentPart::text(observation)],
            Some(call_id),
        ));
        projected += 1;
    }
    if projected > 0 {
        tracing::info!(
            "run_session: projected {} tool step(s) from session_steps for session {}",
            projected,
            session_id
        );
    }
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
            voice: false,
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
}
