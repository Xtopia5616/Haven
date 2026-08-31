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
/// A valid snapshot keeps `events` as the sole authority. When no snapshot
/// exists, this helper uses the durable step identity directly; legacy rows
/// without a provider id receive a deterministic local id derived from the
/// persisted step id. It never matches tool names, arguments, or observation
/// text to infer an association.
pub(crate) fn project_tool_chain_from_steps(
    db: &Database,
    session_id: &str,
    canonical: &mut Vec<CanonicalMessage>,
) {
    let Ok(steps) = db.get_session_steps(session_id) else {
        return;
    };
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
        let call_id = step
            .tool_call_id
            .unwrap_or_else(|| format!("resumed_{}", step.id));
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

    #[test]
    fn project_tool_chain_preserves_duplicate_calls_by_step_identity() {
        let db_path = std::env::temp_dir().join(format!(
            "haven_resume_identity_test_{}.db",
            uuid::Uuid::new_v4()
        ));
        let db = Database::open(&db_path).unwrap();
        let session = db.create_session("resume", "").unwrap();
        for (index, id) in [(0, "call-a"), (1, "call-b")] {
            let step = db
                .create_action_step_with_identity(
                    &session.id,
                    1,
                    index,
                    "echo",
                    r#"{"text":"same"}"#,
                    Some(id),
                    false,
                    false,
                    None,
                    None,
                )
                .unwrap();
            db.complete_action_step(&step.id, "same", true).unwrap();
        }

        let mut canonical = Vec::new();
        project_tool_chain_from_steps(&db, &session.id, &mut canonical);

        let call_ids: Vec<_> = canonical
            .iter()
            .filter(|message| message.role == haven_common::types::CanonicalRole::Assistant)
            .flat_map(|message| message.tool_calls.as_deref().unwrap_or_default())
            .map(|call| call.id.as_str())
            .collect();
        assert_eq!(call_ids, ["call-a", "call-b"]);
        let result_ids: Vec<_> = canonical
            .iter()
            .filter_map(|message| message.tool_call_id.as_deref())
            .collect();
        assert_eq!(result_ids, ["call-a", "call-b"]);
        drop(db);
        let _ = std::fs::remove_file(db_path);
    }
}
