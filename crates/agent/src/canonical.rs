use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};
use serde_json::Value;

/// Repair a canonical message array so it is acceptable to tool-calling LLM
/// APIs: every `tool` message must be the response to a preceding assistant
/// message that declared `tool_calls`, and a trailing assistant message that
/// declares `tool_calls` must be followed by its results. Both violations are
/// rejected with a 400 by providers.
///
/// True when a single `CanonicalMessage` is part of a dangling boundary
/// that must not start a suffix ??either a `Tool` result or an `Assistant`
/// message that declared `tool_calls`. Providers reject the former when its
/// declaration is missing above it and the latter when its results are
/// missing below it, so both forms need to slide past (in
/// `ContextCompactor::safe_end_idx`) or get dropped (in
/// `sanitize_canonical`).
pub(crate) fn is_dangling_boundary(msg: &CanonicalMessage) -> bool {
    msg.role == CanonicalRole::Tool
        || (msg.role == CanonicalRole::Assistant && msg.tool_calls.is_some())
}

/// The ReAct loop only ever builds valid arrays, but snapshots/compaction
/// output can be corrupted by an interruption: a compaction split between an
/// assistant tool_call message and its tool results (the assistant is
/// summarized away while the results survive), an app exit right after the
/// assistant message was appended, or a tool batch cancelled mid-flight with
/// only some of its results appended. This drops orphaned `tool` messages
/// (no preceding assistant tool_calls) and, for every tool_call an assistant
/// declared without a matching result, inserts a synthetic `Tool` result
/// marked "Interrupted". Inserting an interrupted result (instead of trimming
/// the dangling assistant) keeps the array valid for providers that reject a
/// tool_call with no following result as a 400 — including a partial batch
/// where one of two declared calls never returned — and lets the loop see that
/// the tool was cut off and retry it if needed.
///
/// Text used for the synthetic interrupted result.
const INTERRUPTED_RESULT: &str =
    "Interrupted: the tool call was cut off before it returned a result.";

/// Enrich the interrupted-result text with the tool name and the arguments
/// that were attempted, so the model can see exactly which call was cut off
/// and retry it with the same input instead of guessing. Used by both the
/// live cancel path and the snapshot sanitize/repair path.
pub(crate) fn interrupted_result_text(tool_name: &str, arguments: &Value) -> String {
    if tool_name.is_empty() {
        INTERRUPTED_RESULT.to_string()
    } else {
        format!(
            "{} (tool: {}, arguments: {})",
            INTERRUPTED_RESULT, tool_name, arguments
        )
    }
}

/// Sanitize the canonical transcript. Returns the number of synthetic
/// "Interrupted" tool results inserted (Phase 7 / J2). Healthy step-head
/// paths should see `0`; non-zero means an upstream interrupt/compaction
/// left a dangling chain that the gate repaired.
pub(crate) fn sanitize_canonical(canonical: &mut Vec<CanonicalMessage>) -> usize {
    let mut out: Vec<CanonicalMessage> = Vec::with_capacity(canonical.len());
    // Tool_calls declared by the most recent assistant that have not yet been
    // answered by a tool result. Orphaned tool messages (this is empty) are
    // dropped; every call left pending when a non-tool message (or the array
    // end) arrives is repaired with an "Interrupted" result carrying the call's
    // own fields (id, name, arguments).
    let mut pending_calls: Vec<CanonicalToolCall> = Vec::new();
    let mut repairs = 0usize;
    for m in canonical.drain(..) {
        match m.role {
            CanonicalRole::Tool => {
                if pending_calls.is_empty() {
                    tracing::warn!(
                        "dropping orphaned tool message (tool_call_id={:?}) with no preceding assistant tool_calls",
                        m.tool_call_id
                    );
                    continue;
                }
                if let Some(cid) = &m.tool_call_id {
                    if let Some(pos) = pending_calls.iter().position(|c| &c.id == cid) {
                        pending_calls.remove(pos);
                    } else {
                        // The id doesn't match any outstanding call (some
                        // providers/agents don't echo it): consume the next
                        // pending call in order to keep the pairing aligned.
                        pending_calls.pop();
                    }
                } else {
                    pending_calls.pop();
                }
                out.push(m);
            }
            CanonicalRole::Assistant => {
                // A new assistant supersedes the previous assistant's
                // tool_calls: any still-unanswered ones were interrupted.
                repairs += repair_interrupted_tool_calls(&mut out, &mut pending_calls);
                pending_calls = m.tool_calls.clone().unwrap_or_default();
                out.push(m);
            }
            _ => {
                // A user/system/other message breaks the tool-call chain.
                repairs += repair_interrupted_tool_calls(&mut out, &mut pending_calls);
                out.push(m);
            }
        }
    }
    repairs += repair_interrupted_tool_calls(&mut out, &mut pending_calls);
    *canonical = out;
    repairs
}

/// Append a synthetic `Tool` result marked "Interrupted" for every tool_call
/// still pending (declared by an assistant but never answered). This keeps the
/// canonical array valid — providers reject an assistant tool_call with no
/// following result as a 400 — while preserving the fact that the tool was
/// attempted, so the model can retry it. The result text carries the call's
/// own name and arguments so the model sees exactly what was attempted.
/// Returns how many Interrupted results were inserted.
fn repair_interrupted_tool_calls(
    out: &mut Vec<CanonicalMessage>,
    pending_calls: &mut Vec<CanonicalToolCall>,
) -> usize {
    let mut n = 0usize;
    while let Some(call) = pending_calls.pop() {
        tracing::info!(
            "repairing interrupted tool_call {} with an Interrupted result",
            call.id
        );
        let text = interrupted_result_text(&call.name, &call.arguments);
        out.push(CanonicalMessage::tool(
            vec![ContentPart::text(text)],
            Some(call.id),
        ));
        n += 1;
    }
    n
}
