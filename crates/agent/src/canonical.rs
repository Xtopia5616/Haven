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

/// True when the transcript already satisfies tool-call pairing invariants,
/// so [`sanitize_canonical`] can skip the drain/rebuild (common healthy path).
pub(crate) fn canonical_pairing_healthy(canonical: &[CanonicalMessage]) -> bool {
    let mut pending = 0usize;
    for m in canonical {
        match m.role {
            CanonicalRole::Tool => {
                if pending == 0 {
                    return false;
                }
                pending -= 1;
            }
            CanonicalRole::Assistant => {
                if pending > 0 {
                    return false;
                }
                pending = m.tool_calls.as_ref().map_or(0, |c| c.len());
            }
            _ => {
                if pending > 0 {
                    return false;
                }
            }
        }
    }
    pending == 0
}

/// Sanitize the canonical transcript. Returns the number of synthetic
/// "Interrupted" tool results inserted (Phase 7 / J2). Healthy step-head
/// paths should see `0`; non-zero means an upstream interrupt/compaction
/// left a dangling chain that the gate repaired.
pub(crate) fn sanitize_canonical(canonical: &mut Vec<CanonicalMessage>) -> usize {
    // Healthy arrays are the steady-state step head: skip drain + rebuild +
    // per-assistant `tool_calls` clones (O(n) alloc) when pairing already holds.
    if canonical_pairing_healthy(canonical) {
        return 0;
    }
    let mut out: Vec<CanonicalMessage> = Vec::with_capacity(canonical.len());
    // Buffer each assistant batch until all tool results arrive or the next
    // non-tool message begins. This restores call order when a process exits
    // after only some parallel results were durably committed.
    let mut pending_calls: Vec<PendingToolCall> = Vec::new();
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
                let matching_call = m.tool_call_id.as_ref().and_then(|call_id| {
                    pending_calls
                        .iter()
                        .position(|pending| pending.result.is_none() && &pending.call.id == call_id)
                });
                let result_index = matching_call.or_else(|| {
                    // Keep the fallback for providers that omit or alter
                    // tool_call_id: consume the last unresolved call.
                    pending_calls
                        .iter()
                        .rposition(|pending| pending.result.is_none())
                });
                if let Some(index) = result_index {
                    pending_calls[index].result = Some(m);
                }
                if pending_calls.iter().all(|pending| pending.result.is_some()) {
                    append_tool_batch_results(&mut out, &mut pending_calls);
                }
            }
            CanonicalRole::Assistant => {
                // A new assistant supersedes the previous assistant's
                // tool_calls: any still-unanswered ones were interrupted.
                repairs += append_tool_batch_results(&mut out, &mut pending_calls);
                pending_calls = m
                    .tool_calls
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|call| PendingToolCall { call, result: None })
                    .collect();
                out.push(m);
            }
            _ => {
                // A user/system/other message breaks the tool-call chain.
                repairs += append_tool_batch_results(&mut out, &mut pending_calls);
                out.push(m);
            }
        }
    }
    repairs += append_tool_batch_results(&mut out, &mut pending_calls);
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
struct PendingToolCall {
    call: CanonicalToolCall,
    result: Option<CanonicalMessage>,
}

fn append_tool_batch_results(
    out: &mut Vec<CanonicalMessage>,
    pending_calls: &mut Vec<PendingToolCall>,
) -> usize {
    let mut n = 0usize;
    for pending in pending_calls.drain(..) {
        if let Some(result) = pending.result {
            out.push(result);
        } else {
            tracing::info!(
                "repairing interrupted tool_call {} with an Interrupted result",
                pending.call.id
            );
            let text = interrupted_result_text(&pending.call.name, &pending.call.arguments);
            out.push(CanonicalMessage::tool(
                vec![ContentPart::text(text)],
                Some(pending.call.id),
            ));
            n += 1;
        }
    }
    n
}
