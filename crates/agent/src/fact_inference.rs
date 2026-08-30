use std::collections::HashMap;

use haven_common::prompts::COMPACTED_SUMMARY_PREFIX;
use haven_memory::repositories::facts::{
    ContradictionCandidate, ContradictionKind, Fact, fact_within_demote_age,
    is_canonical_merge_target, is_identity_predicate, is_sensitive_text, pick_contradiction_keeper,
};
use haven_memory::repositories::session_steps::SessionStep;
use serde::Deserialize;

use crate::fact_extraction::{coerce_to_string, normalize_predicate};

/// Max preceding assistant text turns kept per new user (M4). Closest first.
const EXTRACTION_MAX_ASSISTANTS_PER_TURN: usize = 2;
/// Max tool observations kept per new user turn (M4).
const EXTRACTION_MAX_TOOLS_PER_TURN: usize = 3;
/// Truncate each tool observation body before it enters the transcript (M4).
pub(crate) const EXTRACTION_TOOL_CONTENT_CHARS: usize = 300;

/// Incremental extraction window (M1+M4): cursor is on **user** message ids;
/// each new user turn may include a bounded assistant/tool slice from the
/// same turn (skip compacted summaries / reasoning). Not a full transcript.
///
/// X12: reads the `messages` / `session_steps` projections (not the events
/// blob). Cursor stays on the last processed **user** message id.
pub(crate) struct ExtractionWindow {
    pub(crate) messages: Vec<haven_memory::repositories::messages::Message>,
    pub(crate) cursor_last: Option<String>,
}

pub(crate) fn build_extraction_window(
    all: &[haven_memory::repositories::messages::Message],
    cursor: Option<&str>,
    steps: &[SessionStep],
) -> ExtractionWindow {
    let user_indices: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    let start_user = cursor
        .and_then(|c| user_indices.iter().position(|&i| all[i].id == c))
        .map(|i| i + 1)
        .unwrap_or(0);
    if start_user >= user_indices.len() {
        return ExtractionWindow {
            messages: Vec::new(),
            cursor_last: None,
        };
    }
    let mut messages = Vec::new();
    for (pos, &ui) in user_indices[start_user..].iter().enumerate() {
        // Peer kickoff / cross-session mail are low-trust and must not become
        // durable user facts (Plan A trust model).
        if is_low_trust_extraction_user(&all[ui]) {
            continue;
        }
        let abs_user_pos = start_user + pos;
        let (span_start, after_ts) = if abs_user_pos == 0 {
            (0, None)
        } else {
            let prev_ui = user_indices[abs_user_pos - 1];
            (prev_ui + 1, Some(all[prev_ui].created_at.as_str()))
        };
        let turn_slice = &all[span_start..ui];
        push_turn_context(&mut messages, turn_slice, &all[ui], after_ts, steps);
        messages.push(all[ui].clone());
    }
    let cursor_last = user_indices[start_user..]
        .last()
        .map(|&i| all[i].id.clone());
    ExtractionWindow {
        messages,
        cursor_last,
    }
}

fn is_extraction_assistant(m: &haven_memory::repositories::messages::Message) -> bool {
    if m.role != "assistant" {
        return false;
    }
    if m.content.starts_with(COMPACTED_SUMMARY_PREFIX) {
        return false;
    }
    !matches!(
        m.message_type.as_deref(),
        Some("reasoning") | Some("thought") | Some("action") | Some("observation")
    )
}

/// Low-trust user rows that must never seed durable facts: peer spawn kickoff
/// (`message_type=peer_kickoff` or delegated-task wrapper). Cross-session mail
/// is inject-only (not persisted as user rows), so it is not filtered here.
fn is_low_trust_extraction_user(m: &haven_memory::repositories::messages::Message) -> bool {
    if m.role != "user" {
        return false;
    }
    if m.message_type.as_deref() == Some("peer_kickoff") {
        return true;
    }
    m.content
        .trim_start()
        .starts_with(haven_common::types::PEER_KICKOFF_PREFIX)
}

/// Collect up to [`EXTRACTION_MAX_ASSISTANTS_PER_TURN`] assistants (closest to
/// the user) and up to [`EXTRACTION_MAX_TOOLS_PER_TURN`] tool observations for
/// one user turn. Tool rows prefer `role=tool` messages in the span; otherwise
/// recent `session_steps` observations between the previous and current user
/// timestamps are synthesized as `tool(name): …` lines (M4).
fn push_turn_context(
    out: &mut Vec<haven_memory::repositories::messages::Message>,
    turn_slice: &[haven_memory::repositories::messages::Message],
    user: &haven_memory::repositories::messages::Message,
    after_ts: Option<&str>,
    steps: &[SessionStep],
) {
    let mut assistants: Vec<&haven_memory::repositories::messages::Message> = turn_slice
        .iter()
        .filter(|m| is_extraction_assistant(m))
        .collect();
    if assistants.len() > EXTRACTION_MAX_ASSISTANTS_PER_TURN {
        assistants = assistants[assistants.len() - EXTRACTION_MAX_ASSISTANTS_PER_TURN..].to_vec();
    }
    for m in assistants {
        out.push(m.clone());
    }

    let mut tools: Vec<haven_memory::repositories::messages::Message> = turn_slice
        .iter()
        .filter(|m| m.role == "tool")
        .cloned()
        .collect();
    if tools.is_empty() {
        let user_ts = user.created_at.as_str();
        for step in steps.iter().rev() {
            if tools.len() >= EXTRACTION_MAX_TOOLS_PER_TURN {
                break;
            }
            let Some(obs) = step.observation.as_deref() else {
                continue;
            };
            if obs.trim().is_empty() {
                continue;
            }
            if is_sensitive_text(obs) {
                continue;
            }
            let ts = step
                .completed_at
                .as_deref()
                .or(step.started_at.as_deref())
                .unwrap_or(step.created_at.as_str());
            if !timestamp_in_turn(ts, after_ts, user_ts) {
                continue;
            }
            let name = step
                .action_tool
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("tool");
            let body =
                haven_common::text::sanitize_prompt_field(obs, EXTRACTION_TOOL_CONTENT_CHARS);
            if body.trim().is_empty() {
                continue;
            }
            tools.push(haven_memory::repositories::messages::Message {
                id: step.id.clone(),
                session_id: step.session_id.clone(),
                role: "tool".into(),
                content: format!("tool({name}): {body}"),
                message_type: Some("observation".into()),
                created_at: ts.to_string(),
                tool_call_id: None,
                attachments: vec![],
                voice: false,
            });
        }
        tools.reverse();
    } else {
        tools.retain(|t| !is_sensitive_text(&t.content));
        for t in &mut tools {
            t.content = haven_common::text::sanitize_prompt_field(
                &t.content,
                EXTRACTION_TOOL_CONTENT_CHARS,
            );
        }
        tools.retain(|t| !t.content.trim().is_empty());
        if tools.len() > EXTRACTION_MAX_TOOLS_PER_TURN {
            tools = tools[tools.len() - EXTRACTION_MAX_TOOLS_PER_TURN..].to_vec();
        }
    }
    out.extend(tools);
}

/// Inclusive turn window for step timestamps. Parses RFC3339 when possible so
/// millis (`…Z`) and offset (`…+00:00`) shapes compare correctly (M4).
fn timestamp_in_turn(ts: &str, after_ts: Option<&str>, user_ts: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(ts),
        chrono::DateTime::parse_from_rfc3339(user_ts),
    ) {
        (Ok(step_dt), Ok(user_dt)) => {
            if step_dt > user_dt {
                return false;
            }
            if let Some(bound) = after_ts
                && let Ok(bound_dt) = chrono::DateTime::parse_from_rfc3339(bound)
            {
                return step_dt > bound_dt;
            }
            true
        }
        _ => {
            // Fallback: lexicographic only when both sides share a shape.
            if ts > user_ts {
                return false;
            }
            after_ts.is_none_or(|bound| ts > bound)
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct PredicateMergeProposal {
    #[serde(deserialize_with = "coerce_to_string")]
    pub(crate) from: String,
    #[serde(deserialize_with = "coerce_to_string")]
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) confidence: f64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ContradictionDemoteProposal {
    #[serde(deserialize_with = "coerce_to_string")]
    pub(crate) demote_id: String,
    #[serde(default)]
    pub(crate) confidence: f64,
}

pub(crate) fn format_contradiction_groups(groups: &[ContradictionCandidate]) -> String {
    let payload: Vec<serde_json::Value> = groups
        .iter()
        .take(20)
        .map(|g| {
            let kind = match g.kind {
                ContradictionKind::Polarity => "polarity",
                ContradictionKind::SingleValued => "single_valued",
            };
            let facts: Vec<serde_json::Value> = g
                .facts
                .iter()
                .map(|f| {
                    let mut row = serde_json::json!({
                        "id": f.id,
                        "subject": f.subject,
                        "predicate": f.predicate,
                        "object": f.object,
                        "source": f.source,
                        "confidence": f.confidence,
                    });
                    if let Some(refer) = f.source_ref.as_ref()
                        && !is_sensitive_text(&refer.snippet)
                    {
                        row["source_snippet"] = serde_json::json!(refer.snippet);
                    }
                    row
                })
                .collect();
            serde_json::json!({ "kind": kind, "facts": facts })
        })
        .collect();
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".into())
}

/// Gate an LLM contradiction demotion (X5). Accept when confidence ≥ 0.85,
/// `demote_id` belongs to a listed group, the target is within the demote age
/// cap, is not the rule-chosen keeper (always leave ≥1 survivor), and is not
/// a user-stated fact challenged by an inferred peer (user always wins).
pub(crate) fn gate_contradiction_demote(
    p: &ContradictionDemoteProposal,
    allowed: &HashMap<String, &Fact>,
    groups: &[ContradictionCandidate],
) -> Option<String> {
    let id = p.demote_id.trim();
    if id.is_empty() || p.confidence < 0.85 {
        return None;
    }
    let fact = *allowed.get(id)?;
    let group = groups
        .iter()
        .find(|g| g.facts.iter().any(|f| f.id == fact.id))?;
    if !fact_within_demote_age(fact, chrono::Utc::now()) {
        return None;
    }
    if let Some((keeper, _)) = pick_contradiction_keeper(group.kind, &group.facts)
        && keeper.id == fact.id
    {
        // Never wipe a whole group — keeper always survives.
        return None;
    }
    if fact.source == "user"
        && group
            .facts
            .iter()
            .any(|f| f.id != fact.id && f.source != "user")
    {
        // User-stated beats inferred for every predicate (not only identity).
        return None;
    }
    Some(fact.id.clone())
}

/// Gate an LLM merge proposal (M6). Accept when the static alias map already
/// maps `from`→`to` (incl. case-only folds), or when confidence ≥ 0.85 and
/// `to` is canonical while `from` is still free-form. Never rewrite identity
/// or already-canonical keys onto a different key; never merge likes↔dislikes.
/// `from` is kept as listed so `rewrite_predicate` matches the exact DB spelling.
pub(crate) fn gate_predicate_merge(p: &PredicateMergeProposal) -> Option<(String, String)> {
    let from_raw = p.from.trim();
    let to_raw = p.to.trim();
    if from_raw.is_empty() || to_raw.is_empty() {
        return None;
    }
    let to = normalize_predicate(to_raw);
    let from_norm = normalize_predicate(from_raw);
    if from_raw == to {
        return None;
    }
    let polarity_clash =
        (from_norm == "likes" && to == "dislikes") || (from_norm == "dislikes" && to == "likes");
    if polarity_clash {
        return None;
    }
    // Never move identity / already-canonical keys onto a different key.
    if (is_identity_predicate(&from_norm) || is_canonical_merge_target(&from_norm))
        && from_norm != to
    {
        return None;
    }
    // Alias map or case-only fold onto the same canonical key.
    if from_norm == to && is_canonical_merge_target(&to) {
        return Some((from_raw.to_string(), to));
    }
    // Free-form → canonical only at high confidence.
    if p.confidence >= 0.85
        && is_canonical_merge_target(&to)
        && !is_canonical_merge_target(&from_norm)
    {
        return Some((from_raw.to_string(), to));
    }
    None
}

/// Prefer the user line in an assistant+user pair for `source_ref` (M1).
pub(crate) fn resolve_source_message(
    messages: &[haven_memory::repositories::messages::Message],
    idx: usize,
) -> Option<&haven_memory::repositories::messages::Message> {
    let m = messages.get(idx)?;
    if m.role == "user" {
        return Some(m);
    }
    messages[idx + 1..].iter().find(|n| n.role == "user")
}

/// Build a transcript string truncated to `max_chars`. Recent messages take
/// priority. Each line is `[N] role: content` — numbering stays absolute in
/// the input slice so `message_index` maps straight back.
pub(crate) fn build_numbered_transcript(
    messages: &[haven_memory::repositories::messages::Message],
    max_chars: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut total_len = 0;
    // Walk backwards so the most recent messages are kept when truncating.
    for (i, m) in messages.iter().enumerate().rev() {
        // Sanitize content so tool/assistant bodies cannot inject newlines that
        // forge extra `[N] user:` lines in the extraction prompt (M4).
        let body = haven_common::text::sanitize_prompt_field(&m.content, max_chars);
        let line = format!("[{}] {}: {}", i, m.role, body);
        if total_len + line.len() + 1 > max_chars {
            break;
        }
        total_len += line.len() + 1;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}
