//! Empty-response and cut-off retry heuristics / ask-pending scanners.
//!
//! Split from `react.rs` (Phase 1 mechanical extract; behavior unchanged).

use super::*;
use haven_llm::{FinishReason, LlmResponse};

/// Nudge appended to the retry call when a text-only response looks cut off
/// (truncated generation or text ending mid-sentence). The retry is private
/// to the loop —the nudge is never persisted into the canonical, so the
/// conversation stream stays clean if the retry succeeds or falls back.
pub(super) const CUT_OFF_RETRY_NUDGE: &str =
    "Your previous response was cut off before you finished. Please continue and complete it.";

/// A stronger nudge for the mid-session retry. The model stopped with a text-only
/// reply while a tool result is still pending (it described the next step but
/// did not run it). The generic cut-off nudge ("continue and complete") does not
/// push it to actually issue the tool call it was narrating, so this variant
/// spells out that the session still needs a tool call.
pub(super) const MID_ACTION_RETRY_NUDGE: &str = "The session is not finished: the last step ran a tool and its result is in context, but your reply only described the next step instead of doing it. If the session still needs a tool call or a follow-up action, make that tool call NOW instead of describing it. Do not repeat work already done. Continue and finish the actual session.";

impl ReActEngine {
    /// Legacy fallback for unanswered `ask` detection (Phase 4 / C5).
    /// Prefer `SessionExecutor::get_awaiting_answer` / snapshot
    /// `awaiting_answer`; this JSON substring scan remains only for older
    /// snapshots that lack the explicit flag.
    ///
    /// True when the canonical ends with an unanswered `ask`: an `ask` tool
    /// result is present and no user message follows it.
    pub(super) fn canonical_has_pending_ask(canonical: &[CanonicalMessage]) -> bool {
        for m in canonical.iter().rev() {
            match m.role {
                CanonicalRole::User => return false,
                CanonicalRole::Tool
                    if m.content.iter().any(|p| {
                        matches!(p, ContentPart::Text(t) if t.contains("\"ask\":true") || t.contains("\"ask\": true"))
                    }) =>
                {
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    /// Extract the question text of the last unanswered `ask` tool result in
    /// the canonical. Falls back to a generic prompt when the tool output is
    /// truncated or unparseable.
    pub(super) fn extract_pending_ask_question(canonical: &[CanonicalMessage]) -> String {
        for m in canonical.iter().rev() {
            if m.role != CanonicalRole::Tool {
                continue;
            }
            for p in &m.content {
                let ContentPart::Text(t) = p else { continue };
                if !(t.contains("\"ask\":true") || t.contains("\"ask\": true")) {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(t)
                    && let Some(q) = v.get("question").and_then(|q| q.as_str())
                {
                    return q.to_string();
                }
            }
        }
        "I have a pending question for you.".into()
    }

    /// True when a text-only response should not be trusted as a deliberate
    /// final answer: either the provider did not report Stop (truncated /
    /// filtered / unknown finish), the text itself ends mid-sentence (trailing
    /// comma/connector/ellipsis —the generation was interrupted rather than
    /// concluded), or it ends on a planning/transition phrase (「接下来」「确认
    /// 一下」…) that signals the model was about to take a further action but
    /// stopped short of describing/issuing it.
    pub(super) fn looks_cut_off(text: &str) -> bool {
        const PLAN_ENDINGS: &[&str] = &[
            // Chinese: plan/transition phrases that expect a following action
            "接下来",
            "下一步",
            "然后",
            "接着",
            "再确认",
            "确认一下",
            "检查一下",
            "核对一下",
            "查看一下",
            "再看",
            "以便",
            "才能",
            // English: transition/plan phrases
            "next",
            "next step",
            "then",
            "let me",
            "I will",
            "I'll",
        ];
        let t = text.trim_end();
        text.ends_with("...")
            || text.ends_with("路路路")
            || PLAN_ENDINGS.iter().any(|w| t.ends_with(w))
            || matches!(
                t.chars().last(),
                Some('，')
                    | Some('：')
                    | Some('！')
                    | Some(',')
                    | Some(';')
                    | Some(':')
                    | Some('…')
            )
    }

    /// True when the parsed response is a text-only "final" that must be
    /// retried before ending the turn. Trusts explicit tool calls (final or
    /// not) and empty responses (handled by the empty-response retry); only
    /// a thought without actions is examined, and it must pass the
    /// finish-reason, mid-sentence, and mid-session checks.
    ///
    /// `canonical` supplies the mid-session signal: when the agent has pending
    /// tool context (the canonical ends in tool results with no user reply), a
    /// text-only Stop is far more likely to be "I'll do X next" narration than
    /// a deliberate final answer, so it is treated as suspect too.
    pub(super) fn is_suspect_final(
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
        canonical: &[CanonicalMessage],
    ) -> bool {
        if !actions.is_empty()
            && !actions
                .iter()
                .all(|a| a.is_final && a.tool_call_id.is_none())
        {
            return false;
        }
        match thought {
            Some(t) => {
                response.finish_reason != Some(FinishReason::Stop)
                    || Self::looks_cut_off(t)
                    || Self::canonical_has_pending_tool_context(canonical)
            }
            None => false,
        }
    }

    /// True when the agent is mid-session: scanning back from the tail, the first
    /// User message or Tool result decides. A Tool result before any User
    /// message means tool(s) ran this turn and the reply has not come yet, so
    /// a text-only Stop should not be trusted as final. A User message first
    /// means a fresh turn (the agent is answering, not continuing tool work).
    pub(super) fn canonical_has_pending_tool_context(canonical: &[CanonicalMessage]) -> bool {
        for m in canonical.iter().rev() {
            match m.role {
                CanonicalRole::User => return false,
                CanonicalRole::Tool => return true,
                _ => {}
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::ContentPart;
    use haven_llm::types::FinishReason;

    fn resp(text: &str, finish: Option<FinishReason>) -> LlmResponse {
        LlmResponse {
            text: text.to_string(),
            tool_calls: Vec::new(),
            finish_reason: finish,
            usage: haven_llm::types::Usage::default(),
            model: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }
    }

    #[test]
    fn looks_cut_off_detects_mid_sentence_and_planning() {
        assert!(ReActEngine::looks_cut_off("让我先查一下，"));
        assert!(ReActEngine::looks_cut_off("接下来"));
        assert!(ReActEngine::looks_cut_off("确认一下"));
        assert!(!ReActEngine::looks_cut_off("好的，已经完成了。"));
        assert!(!ReActEngine::looks_cut_off("完成"));
    }

    #[test]
    fn is_suspect_final_module_heuristics() {
        let r = resp("partial", Some(FinishReason::Length));
        assert!(ReActEngine::is_suspect_final(&Some("partial".into()), &[], &r, &[]));
        let mid = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        assert!(ReActEngine::canonical_has_pending_tool_context(&mid));
    }
}
