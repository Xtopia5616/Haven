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
use haven_memory::repositories::session_steps::SessionStep;
use serde_json::Value;

use crate::types::{Action, TranscriptRecord};

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

/// Project completed tool calls from the materialized step projection when the
/// snapshot row is absent.
///
/// A valid snapshot keeps `events` as the sole authority. When no snapshot
/// exists, this helper uses the durable step identity directly; legacy rows
/// without a provider id receive a fresh local id from the canonical `call-*`
/// namespace. It never matches tool names, arguments, or observation text to
/// infer an association.
pub(crate) fn project_tool_chain_from_steps(
    db: &Database,
    session_id: &str,
    canonical: &mut Vec<CanonicalMessage>,
) -> anyhow::Result<()> {
    let steps = db.get_session_steps(session_id)?;
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
            .unwrap_or_else(|| haven_common::types::new_id("call"));
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
    Ok(())
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
        project_tool_chain_from_steps(&db, &session.id, &mut canonical).unwrap();

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
