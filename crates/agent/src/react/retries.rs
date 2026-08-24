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

/// Tool-call arguments arrived as truncated / unparseable JSON (stream
/// interrupted mid-`arguments`, or `finish_reason=length` before the object
/// closed). Re-emit the full tool call with complete valid JSON — do not
/// continue a half-written arguments string.
const INCOMPLETE_TOOL_ARGS_NUDGE: &str = "Your previous tool call was cut off mid-arguments (incomplete JSON). Emit the same tool call again with complete, valid JSON arguments. Do not continue a half-written JSON string.";

/// A stronger nudge for the mid-session retry. The model stopped with a text-only
/// reply while a tool result is still pending (it described the next step but
/// did not run it). The generic cut-off nudge ("continue and complete") does not
/// push it to actually issue the tool call it was narrating, so this variant
/// spells out that the session still needs a tool call.
///
/// Waiting on a still-running background action is handled before this nudge
/// fires — see [`ResponsePolicy::canonical_only_awaiting_background`].
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

        // Truncated tool-call JSON becomes Null in from_wire_args. Never
        // execute those with schema placeholders — retry so the model can
        // emit complete arguments (also covers continue-after-interrupt).
        let incomplete_tool_args = actions
            .iter()
            .any(|a| !a.is_final && a.tool_input.is_null());
        if incomplete_tool_args
            && !state.pending_ask
            && response.web_search_calls.is_empty()
            && state.cut_off_retries_used < state.cut_off_retries_max
        {
            return AfterLlmAction::RetryCutOff {
                nudge: INCOMPLETE_TOOL_ARGS_NUDGE,
            };
        }

        if state.pending_ask
            || !response.web_search_calls.is_empty()
            || state.cut_off_retries_used >= state.cut_off_retries_max
            || !Self::is_suspect_final(thought, actions, response)
        {
            return AfterLlmAction::Accept;
        }

        let nudge = if Self::canonical_has_pending_tool_context(canonical)
            && !Self::canonical_only_awaiting_background(canonical)
        {
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
    ///
    /// Deliberate sentence terminators (`。` / `.` / `！` / `!` / `？` / `?`)
    /// are never treated as cut-off — a complete final that ends with `！`
    /// must be accepted, not replayed into the same bubble.
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
                Some('，') | Some('：') | Some(',') | Some(';') | Some(':') | Some('…')
            )
    }

    /// True when the parsed response is a text-only "final" that must be
    /// retried before ending the turn. Trusts explicit tool calls (final or
    /// not) and empty responses (handled by the empty-response retry); only
    /// a thought without actions is examined.
    ///
    /// A clean `Stop` after a tool result is a normal ReAct turn end — do
    /// **not** blanket-retry every mid-session text-only reply. That false
    /// positive re-ran the same step (same msg-id bubble), so the UI showed a
    /// complete answer and then overwrote it when the cut-off retry streamed
    /// again / issued another tool call. Mid-session narration is still
    /// caught by [`Self::looks_cut_off`] (and non-Stop finishes); the stronger
    /// [`MID_ACTION_RETRY_NUDGE`] is selected in [`Self::classify`] when a
    /// pending tool context is present.
    pub(crate) fn is_suspect_final(
        thought: &Option<String>,
        actions: &[Action],
        response: &LlmResponse,
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
                response.finish_reason != Some(FinishReason::Stop) || Self::looks_cut_off(t)
            }
            None => false,
        }
    }

    /// True when the nearest non-assistant message scanning from the tail is
    /// a Tool result (tools ran this turn; no newer User message). Used to
    /// pick [`MID_ACTION_RETRY_NUDGE`] when a cut-off narration is retried —
    /// not as a blanket "text-only after tools is incomplete" signal.
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

    /// True when every trailing Tool observation (before the nearest User
    /// message) is a still-running background-action acknowledgement. In that
    /// case a deliberate text-only Stop is the correct turn end — auto-wake
    /// will resume the session when the action finishes.
    pub(crate) fn canonical_only_awaiting_background(canonical: &[CanonicalMessage]) -> bool {
        let mut saw_tool = false;
        for m in canonical.iter().rev() {
            match m.role {
                CanonicalRole::User => return saw_tool,
                CanonicalRole::Tool => {
                    saw_tool = true;
                    // Prefer the first text part as-is (no join alloc). Haven
                    // tool observations are a single JSON text part.
                    let text = m.content.iter().find_map(|p| match p {
                        ContentPart::Text(t) => Some(t.as_str()),
                        _ => None,
                    });
                    let Some(text) = text else {
                        return false;
                    };
                    if !Self::observation_is_background_wait(text) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        saw_tool
    }

    /// Detect shell/actions observations that mean "background still running;
    /// result will be auto-pushed".
    ///
    /// Prefer parseable JSON with Haven's `next_step: end_turn` (or legacy
    /// `background: true` + running). A short head gate skips full JSON parse
    /// of large non-wait observations. Truncated observations are invalid
    /// JSON; only a char-boundary head is scanned for the Haven-emitted
    /// `next_step` marker (producers put it first) — never the full string,
    /// and never deep `background`/`status` substrings from file contents.
    pub(crate) fn observation_is_background_wait(text: &str) -> bool {
        use haven_common::tools::{BACKGROUND_WAIT_NEXT_STEP, BACKGROUND_WAIT_NEXT_STEP_KEY};
        let head = Self::observation_head(text, 256);
        let compact = format!(
            "\"{BACKGROUND_WAIT_NEXT_STEP_KEY}\":\"{BACKGROUND_WAIT_NEXT_STEP}\""
        );
        let spaced = format!(
            "\"{BACKGROUND_WAIT_NEXT_STEP_KEY}\": \"{BACKGROUND_WAIT_NEXT_STEP}\""
        );
        let head_has_next_step = head.contains(&compact) || head.contains(&spaced);
        let head_looks_wait = head_has_next_step
            || head.contains("\"background\":true")
            || head.contains("\"background\": true");
        if !head_looks_wait {
            return false;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
            return Self::value_is_background_wait(&v);
        }
        // Truncated / invalid JSON: only the next_step marker (not buried
        // background/status) may accept as wait.
        head_has_next_step
    }

    /// First `max_bytes` of `text`, floored to a char boundary. Never returns
    /// the full string when the cut would split a multibyte char.
    fn observation_head(text: &str, max_bytes: usize) -> &str {
        if text.len() <= max_bytes {
            return text;
        }
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    }

    fn value_is_background_wait(v: &serde_json::Value) -> bool {
        use haven_common::tools::{BACKGROUND_WAIT_NEXT_STEP, BACKGROUND_WAIT_NEXT_STEP_KEY};
        let has_next_step = v
            .get(BACKGROUND_WAIT_NEXT_STEP_KEY)
            .and_then(|s| s.as_str())
            == Some(BACKGROUND_WAIT_NEXT_STEP);
        if v.get("background").and_then(|b| b.as_bool()) == Some(true) {
            return matches!(
                v.get("status").and_then(|s| s.as_str()),
                Some("running") | None
            );
        }
        if !has_next_step {
            return false;
        }
        if v.get("status").and_then(|s| s.as_str()) == Some("running")
            && v.get("action_id").and_then(|s| s.as_str()).is_some()
        {
            return true;
        }
        if let Some(arr) = v.get("actions").and_then(|a| a.as_array()) {
            return !arr.is_empty()
                && arr
                    .iter()
                    .all(|row| row.get("status").and_then(|s| s.as_str()) == Some("running"));
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
        assert!(!ResponsePolicy::looks_cut_off("完成了！"));
        assert!(!ResponsePolicy::looks_cut_off("Done!"));
        assert!(!ResponsePolicy::looks_cut_off("可以吗？"));
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
                ResponsePolicy::is_suspect_final(&Some("partial text".into()), &[], &r),
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
        ));
        let r2 = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r2,
        ));
    }

    #[test]
    fn is_suspect_final_ignores_empty_thought() {
        let r = resp("", Some(FinishReason::Length));
        assert!(!ResponsePolicy::is_suspect_final(&None, &[], &r));
    }

    #[test]
    fn is_suspect_final_accepts_complete_mid_session_text_only_stop() {
        // Tool result then a deliberate final is the normal ReAct end — must
        // NOT be retried (that replayed the same bubble and looked like the
        // whole step ran twice).
        let canonical = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        let r = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
        ));
        assert!(ResponsePolicy::canonical_has_pending_tool_context(&canonical));
    }

    #[test]
    fn observation_is_background_wait_detects_shell_and_actions() {
        assert!(ResponsePolicy::observation_is_background_wait(
            r#"{"next_step":"end_turn","background":true,"action_id":"act-1","status":"running"}"#
        ));
        assert!(ResponsePolicy::observation_is_background_wait(
            r#"{"background":true,"action_id":"act-1","status":"running"}"#
        ));
        assert!(ResponsePolicy::observation_is_background_wait(
            r#"{"next_step":"end_turn","action_id":"act-1","status":"running","hint":"still running"}"#
        ));
        assert!(!ResponsePolicy::observation_is_background_wait(
            r#"{"action_id":"act-1","status":"running","hint":"still running"}"#
        ));
        assert!(ResponsePolicy::observation_is_background_wait(
            r#"{"next_step":"end_turn","actions":[{"action_id":"act-1","status":"running"},{"action_id":"act-2","status":"running"}]}"#
        ));
        assert!(!ResponsePolicy::observation_is_background_wait(
            r#"{"actions":[{"action_id":"act-1","status":"running"},{"action_id":"act-2","status":"running"}]}"#
        ));
        assert!(!ResponsePolicy::observation_is_background_wait(
            r#"{"background":true,"action_id":"act-1","status":"completed"}"#
        ));
        assert!(!ResponsePolicy::observation_is_background_wait(r#"{"ok":true}"#));
        assert!(!ResponsePolicy::observation_is_background_wait(
            r#"{"background": true, "status": "running", "hint": "trun"#
        ));
        assert!(ResponsePolicy::observation_is_background_wait(
            r#"{"next_step":"end_turn","actions":[{"status":"running"...truncated# [... truncated 9000 chars omitted]"#
        ));
        assert!(!ResponsePolicy::observation_is_background_wait(
            r#"source has "background":true and "status":"running" buried deep [... truncated 12 chars omitted]"#
        ));
        // Mid-UTF-8 at the 256-byte cut must floor to a char boundary and
        // never fall back to scanning the full string for a buried marker.
        let mut mid_utf8 = String::from(r#"{"ok":true,"note":""#);
        while mid_utf8.len() < 254 {
            mid_utf8.push('x');
        }
        mid_utf8.push('你'); // 3-byte UTF-8 starting at offset 254
        mid_utf8.push_str(r#"","noise":"buried \"next_step\":\"end_turn\" far away"}"#);
        assert!(
            mid_utf8.is_char_boundary(254) && !mid_utf8.is_char_boundary(256),
            "fixture must split a multibyte char at 256"
        );
        assert!(
            !ResponsePolicy::observation_is_background_wait(&mid_utf8),
            "buried marker past a mid-UTF-8 cut must not accept"
        );
        // Truncated wait whose head still contains next_step must accept even
        // when a later multibyte char would split offset 256.
        let mut wait_mid = String::from(r#"{"next_step":"end_turn","n":""#);
        while wait_mid.len() < 254 {
            wait_mid.push('a');
        }
        wait_mid.push('你');
        wait_mid.push_str("TRUNCATED_NO_CLOSE");
        assert!(
            !wait_mid.is_char_boundary(256),
            "wait fixture must also split at 256"
        );
        assert!(
            ResponsePolicy::observation_is_background_wait(&wait_mid),
            "head next_step must still accept across a mid-UTF-8 cut"
        );
        // Large non-wait JSON must not require a full parse (head gate).
        let large = format!(
            r#"{{"output":"{}","shell":"cmd"}}"#,
            "y".repeat(12_000)
        );
        assert!(!ResponsePolicy::observation_is_background_wait(&large));
    }

    #[test]
    fn is_suspect_final_accepts_end_turn_while_awaiting_background() {
        let canonical = vec![
            CanonicalMessage::user_text("install deps"),
            CanonicalMessage::tool(
                vec![ContentPart::text(
                    r#"{"background":true,"action_id":"act-1","status":"running","next_step":"end_turn"}"#,
                )],
                Some("c1".into()),
            ),
        ];
        assert!(ResponsePolicy::canonical_only_awaiting_background(&canonical));
        let r = resp(
            "依赖已在后台安装，完成后会自动继续。",
            Some(FinishReason::Stop),
        );
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("依赖已在后台安装，完成后会自动继续。".into()),
            &[],
            &r,
        ));
    }

    #[test]
    fn is_suspect_final_still_flags_cut_off_while_awaiting_background() {
        let r = resp("接下来", Some(FinishReason::Stop));
        assert!(ResponsePolicy::is_suspect_final(
            &Some("接下来".into()),
            &[],
            &r,
        ));
    }

    #[test]
    fn classify_accepts_clean_stop_while_awaiting_background() {
        let canonical = vec![
            CanonicalMessage::user_text("clone repo"),
            CanonicalMessage::tool(
                vec![ContentPart::text(
                    r#"{"background":true,"action_id":"act-9","status":"running"}"#,
                )],
                Some("c9".into()),
            ),
        ];
        let r = resp(
            "克隆已在后台运行，完成后会自动继续。",
            Some(FinishReason::Stop),
        );
        assert_eq!(
            ResponsePolicy::classify(
                &Some("克隆已在后台运行，完成后会自动继续。".into()),
                &[],
                &r,
                &canonical,
                state(0, 0, 2, false),
            ),
            AfterLlmAction::Accept
        );
    }

    #[test]
    fn is_suspect_final_accepts_text_only_stop_on_fresh_turn() {
        let r = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert!(!ResponsePolicy::is_suspect_final(
            &Some("好的，已经完成了。".into()),
            &[],
            &r,
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
    fn classify_accepts_complete_mid_session_final() {
        let canonical = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        let r = resp("好的，已经完成了。", Some(FinishReason::Stop));
        assert_eq!(
            ResponsePolicy::classify(
                &Some("好的，已经完成了。".into()),
                &[],
                &r,
                &canonical,
                state(0, 0, 2, false),
            ),
            AfterLlmAction::Accept
        );
    }

    #[test]
    fn classify_retries_cut_off_with_mid_session_nudge() {
        // Only mid-sentence / planning narration after a tool is retried —
        // complete finals must Accept (see classify_accepts_complete_mid_session_final).
        let canonical = vec![
            CanonicalMessage::user_text("go"),
            CanonicalMessage::tool(vec![ContentPart::text("ok")], Some("c1".into())),
        ];
        let r = resp("让我先查一下，", Some(FinishReason::Stop));
        match ResponsePolicy::classify(
            &Some("让我先查一下，".into()),
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

    #[test]
    fn classify_retries_incomplete_tool_arg_json() {
        // Truncated mid-arguments → from_wire_args yields Null. Must retry
        // instead of trusting the tool call (is_suspect_final would Accept).
        let actions = vec![Action {
            tool_name: "files".into(),
            tool_input: serde_json::Value::Null,
            is_final: false,
            tool_call_id: Some("c1".into()),
        }];
        let r = resp("", Some(FinishReason::ToolCalls));
        match ResponsePolicy::classify(&None, &actions, &r, &[], state(0, 0, 2, false)) {
            AfterLlmAction::RetryCutOff { nudge } => {
                assert!(nudge.contains("incomplete JSON"));
            }
            other => panic!("expected RetryCutOff for Null tool args, got {other:?}"),
        }
        // Exhausted cut-off budget → Accept (supplement fills placeholders).
        assert_eq!(
            ResponsePolicy::classify(&None, &actions, &r, &[], state(0, 2, 2, false)),
            AfterLlmAction::Accept
        );
        // final_answer with Null input is fine (not a truncated tool call).
        let final_only = vec![Action {
            tool_name: "final_answer".into(),
            tool_input: serde_json::Value::Null,
            is_final: true,
            tool_call_id: None,
        }];
        assert_eq!(
            ResponsePolicy::classify(
                &Some("done".into()),
                &final_only,
                &resp("done", Some(FinishReason::Stop)),
                &[],
                state(0, 0, 2, false),
            ),
            AfterLlmAction::Accept
        );
    }
}
