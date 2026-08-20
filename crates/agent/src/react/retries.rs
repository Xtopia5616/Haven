//! Empty-response and cut-off response policy / ask-pending scanners.
//!
//! Phase 5 / G3: heuristics and nudge literals live in [`ResponsePolicy`];
//! the thin loop only consumes [`AfterLlmAction`] (via `LoopHooks::after_llm`).

use super::*;
use haven_llm::{FinishReason, LlmResponse};

/// Nudge appended to the retry call when a text-only response looks cut off
/// (truncated generation or text ending mid-sentence). The retry is private
/// to the loop — the nudge is never persisted into the canonical, so the
/// conversation stream stays clean if the retry succeeds or falls back.
const CUT_OFF_RETRY_NUDGE: &str =
    "Your previous response was cut off before you finished. Please continue and complete it.";

/// A stronger nudge for the mid-session retry. The model stopped with a text-only
/// reply while a tool result is still pending (it described the next step but
/// did not run it). The generic cut-off nudge ("continue and complete") does not
/// push it to actually issue the tool call it was narrating, so this variant
/// spells out that the session still needs a tool call.
const MID_ACTION_RETRY_NUDGE: &str = "The session is not finished: the last step ran a tool and its result is in context, but your reply only described the next step instead of doing it. If the session still needs a tool call or a follow-up action, make that tool call NOW instead of describing it. Do not repeat work already done. Continue and finish the actual session.";

/// What the loop should do after an LLM response (Phase 5 / G3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterLlmAction {
    /// Accept the parsed response and continue the turn.
    Accept,
    /// Empty response — settle briefly and retry the same context.
    RetryEmpty { delay_ms: u64 },
    /// Suspect cut-off / mid-session narration — retry with an ephemeral nudge.
    RetryCutOff { nudge: &'static str },
}

/// Budgets / gates the policy needs beyond the parsed response itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ResponsePolicyState {
    pub empty_retries_remaining: u32,
    pub empty_retry_delay_ms: u64,
    pub cut_off_retries_used: u32,
    pub cut_off_retries_max: u32,
    /// Explicit ask-awaiting (C5) or legacy JSON scan.
    pub pending_ask: bool,
}

/// Stateless empty / cut-off classifier. Phrase tables and nudge strings live
/// here so the thin loop has no response-policy literals.
pub(crate) struct ResponsePolicy;

impl ResponsePolicy {
    /// Classify a parsed LLM step into accept / retry.
    ///
    /// Order: empty (transient glitch) before cut-off (truncated / mid-session).
    /// Web-search rounds are never retried (duplicate server-side search).
    pub(crate) fn classify(
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
        canonical: &[CanonicalMessage],
        state: ResponsePolicyState,
    ) -> AfterLlmAction {
        let empty = thought.is_none()
            && actions.is_empty()
            && response.web_search_calls.is_empty();
        if empty {
            if state.empty_retries_remaining > 0 {
                return AfterLlmAction::RetryEmpty {
                    delay_ms: state.empty_retry_delay_ms,
                };
            }
            return AfterLlmAction::Accept;
        }

        if state.pending_ask
            || !response.web_search_calls.is_empty()
            || state.cut_off_retries_used >= state.cut_off_retries_max
            || !Self::is_suspect_final(thought, actions, response, canonical)
        {
            return AfterLlmAction::Accept;
        }

        let nudge = if Self::canonical_has_pending_tool_context(canonical) {
            MID_ACTION_RETRY_NUDGE
        } else {
            CUT_OFF_RETRY_NUDGE
        };
        AfterLlmAction::RetryCutOff { nudge }
    }

    /// True when a text-only response should not be trusted as a deliberate
    /// final answer: either the provider did not report Stop (truncated /
    /// filtered / unknown finish), the text itself ends mid-sentence (trailing
    /// comma/connector/ellipsis — the generation was interrupted rather than
    /// concluded), or it ends on a planning/transition phrase that signals
    /// the model was about to take a further action but stopped short.
    pub(crate) fn looks_cut_off(text: &str) -> bool {
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
    /// a thought without actions is examined.
    pub(crate) fn is_suspect_final(
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

    /// True when the agent is mid-session: scanning back from the tail, the
    /// first User message or Tool result decides. A Tool result before any
    /// User message means tool(s) ran this turn and the reply has not come
    /// yet, so a text-only Stop should not be trusted as final.
    pub(crate) fn canonical_has_pending_tool_context(canonical: &[CanonicalMessage]) -> bool {
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

impl ReActEngine {
    /// Legacy fallback for unanswered `ask` detection (Phase 4 / C5).
    /// Prefer `SessionExecutor::get_awaiting_answer` / snapshot
    /// `awaiting_answer`; this JSON substring scan remains only for older
    /// snapshots that lack the explicit flag.
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

    fn state(empty_left: u32, cut_used: u32, cut_max: u32, pending_ask: bool) -> ResponsePolicyState {
        ResponsePolicyState {
            empty_retries_remaining: empty_left,
            empty_retry_delay_ms: 10,
            cut_off_retries_used: cut_used,
            cut_off_retries_max: cut_max,
            pending_ask,
        }
    }

    #[test]
    fn looks_cut_off_detects_mid_sentence_and_planning() {
        assert!(ResponsePolicy::looks_cut_off("让我先查一下，"));
        assert!(ResponsePolicy::looks_cut_off("checking the file,"));
        assert!(ResponsePolicy::looks_cut_off("waiting for result..."));
        assert!(ResponsePolicy::looks_cut_off("然后需要："));
        assert!(ResponsePolicy::looks_cut_off("接下来"));
        assert!(ResponsePolicy::looks_cut_off("确认一下"));
        assert!(!ResponsePolicy::looks_cut_off("好的，已经完成了。"));
        assert!(!ResponsePolicy::looks_cut_off("The answer is 42."));
        assert!(!ResponsePolicy::looks_cut_off("完成"));
    }

    #[test]
    fn is_suspect_final_trusts_explicit_tool_calls() {
        let explicit = Action {
            tool_name: "final_answer".into(),
            tool_input: serde_json::Value::Null,
            is_final: true,
            tool_call_id: Some("c1".into()),
        };
        let r = resp("done", Some(FinishReason::ToolCalls));
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("done".into()),
            &[explicit],
            &r,
            &[]
        ));
    }

    #[test]
    fn is_suspect_final_flags_truncated_finish() {
        for finish in [
            Some(FinishReason::Length),
            Some(FinishReason::ContentFilter),
            None,
        ] {
            let r = resp("partial text", finish);
            assert!(
                ResponsePolicy::is_suspect_final(&Some("partial text".into()), &[], &r, &[]),
                "finish={finish:?} must be suspect"
            );
        }
    }

    #[test]
    fn is_suspect_final_flags_stop_with_cut_off_text_but_accepts_complete() {
        let r = resp("让我先查一下，", Some(FinishReason::Stop));
        assert!(ResponsePolicy::is_suspect_final(
            &Some("让我先查一下，".into()),
            &[],
            &r,
            &[]
        ));
        let r2 = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r2,
            &[]
        ));
    }

    #[test]
    fn is_suspect_final_ignores_empty_thought() {
        let r = resp("", Some(FinishReason::Length));
        assert!(!ResponsePolicy::is_suspect_final(&None, &[], &r, &[]));
    }

    #[test]
    fn is_suspect_final_flags_mid_session_text_only_stop() {
        let canonical = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        let r = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert!(ResponsePolicy::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
            &canonical
        ));
        assert!(ResponsePolicy::canonical_has_pending_tool_context(&canonical));
    }

    #[test]
    fn is_suspect_final_accepts_text_only_stop_on_fresh_turn() {
        let canonical = vec![CanonicalMessage::user_text("你好")];
        let r = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
            &canonical
        ));
    }

    #[test]
    fn classify_retries_empty_then_accepts_when_exhausted() {
        let r = resp("", None);
        assert_eq!(
            ResponsePolicy::classify(&None, &[], &r, &[], state(2, 0, 2, false)),
            AfterLlmAction::RetryEmpty { delay_ms: 10 }
        );
        assert_eq!(
            ResponsePolicy::classify(&None, &[], &r, &[], state(0, 0, 2, false)),
            AfterLlmAction::Accept
        );
    }

    #[test]
    fn classify_retries_cut_off_with_mid_session_nudge() {
        let canonical = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        let r = resp("好的，已经完成了。", Some(FinishReason::Stop));
        match ResponsePolicy::classify(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
            &canonical,
            state(0, 0, 2, false),
        ) {
            AfterLlmAction::RetryCutOff { nudge } => {
                assert!(nudge.contains("session is not finished"));
            }
            other => panic!("expected RetryCutOff, got {other:?}"),
        }
    }

    #[test]
    fn classify_skips_cut_off_when_pending_ask() {
        let r = resp("让我先查一下，", Some(FinishReason::Stop));
        assert_eq!(
            ResponsePolicy::classify(
                &Some("让我先查一下，".into()),
                &[],
                &r,
                &[],
                state(0, 0, 2, true),
            ),
            AfterLlmAction::Accept
        );
    }
}
