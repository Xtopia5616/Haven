//! Classifies model responses and selects safe retries for incomplete tool calls.

use super::*;
use haven_llm::{FinishReason, LlmResponse};

/// Re-emit the full tool call with valid arguments; never continue a
/// half-written JSON string.
const INCOMPLETE_TOOL_ARGS_NUDGE: &str = "Your previous tool call had incomplete JSON arguments. Emit the same tool call again with complete, valid JSON arguments.";

/// Policy result that says whether to accept, retry, or fail a model response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResponsePolicyDecision {
    Accept,
    RetryIncompleteToolArgs { nudge: &'static str },
    Fail { reason: String },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResponsePolicyState {
    pub incomplete_tool_args_retries_used: u32,
    pub incomplete_tool_args_retries_max: u32,
    pub pending_ask: bool,
}

pub(crate) struct ResponsePolicy;

impl ResponsePolicy {
    pub(crate) fn classify(input: &ResponsePolicyInput<'_>) -> ResponsePolicyDecision {
        // An incomplete arguments object is the one response shape that is
        // safe and useful to retry automatically. Never dispatch placeholders.
        let incomplete_tool_args = input
            .tool_calls
            .iter()
            .any(|tool_call| !tool_call.is_final && tool_call.tool_input.is_null());
        if incomplete_tool_args {
            if !input.state.pending_ask
                && input.response.web_search_calls.is_empty()
                && input.state.incomplete_tool_args_retries_used
                    < input.state.incomplete_tool_args_retries_max
            {
                return ResponsePolicyDecision::RetryIncompleteToolArgs {
                    nudge: INCOMPLETE_TOOL_ARGS_NUDGE,
                };
            }
            return ResponsePolicyDecision::Fail {
                reason: "工具调用参数不是完整 JSON，无法安全执行；点击“继续生成”重新尝试。".into(),
            };
        }

        let empty = input.thought.is_none()
            && input.tool_calls.is_empty()
            && input.response.web_search_calls.is_empty();
        if empty {
            if input.state.pending_ask && input.response.finish_reason == Some(FinishReason::Stop) {
                return ResponsePolicyDecision::Accept;
            }
            let reason = if Self::has_normal_finish(input.response, input.tool_calls) {
                "模型返回了空响应；已保留当前输出，可点击“继续生成”重试。".into()
            } else {
                Self::abnormal_finish_reason(input.response)
            };
            return ResponsePolicyDecision::Fail { reason };
        }

        if !Self::has_normal_finish(input.response, input.tool_calls) {
            return ResponsePolicyDecision::Fail {
                reason: Self::abnormal_finish_reason(input.response),
            };
        }

        ResponsePolicyDecision::Accept
    }

    fn has_normal_finish(response: &LlmResponse, tool_calls: &[ToolCall]) -> bool {
        match response.finish_reason {
            Some(FinishReason::Stop) => true,
            Some(FinishReason::ToolCalls | FinishReason::FunctionCall) => !tool_calls.is_empty(),
            Some(FinishReason::Length | FinishReason::ContentFilter) | None => false,
        }
    }

    fn abnormal_finish_reason(response: &LlmResponse) -> String {
        match response.finish_reason {
            Some(reason) => format!(
                "模型未正常结束（finish_reason={reason}）；已保留当前输出，可点击“继续生成”重试。"
            ),
            None => "模型没有报告正常结束原因；已保留当前输出，可点击“继续生成”重试。".into(),
        }
    }
}

/// Parsed values and loop state that determine how a model response is handled.
pub(crate) struct ResponsePolicyInput<'a> {
    pub thought: &'a Option<String>,
    pub tool_calls: &'a [ToolCall],
    pub response: &'a LlmResponse,
    pub state: ResponsePolicyState,
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn state(retries_used: u32, retries_max: u32, pending_ask: bool) -> ResponsePolicyState {
        ResponsePolicyState {
            incomplete_tool_args_retries_used: retries_used,
            incomplete_tool_args_retries_max: retries_max,
            pending_ask,
        }
    }

    fn classify_response(
        thought: &Option<String>,
        tool_calls: &[ToolCall],
        response: &LlmResponse,
        state: ResponsePolicyState,
    ) -> ResponsePolicyDecision {
        ResponsePolicy::classify(&ResponsePolicyInput {
            thought,
            tool_calls,
            response,
            state,
        })
    }

    #[test]
    fn normal_stop_accepts_text_without_lexical_cutoff_heuristics() {
        for text in [
            "让我先查一下，",
            "接下来",
            "waiting for result...",
            "路路路",
        ] {
            assert_eq!(
                classify_response(
                    &Some(text.into()),
                    &[],
                    &resp(text, Some(FinishReason::Stop)),
                    state(0, 2, false),
                ),
                ResponsePolicyDecision::Accept
            );
        }
    }

    #[test]
    fn abnormal_text_finish_fails_without_automatic_retry() {
        for finish in [
            Some(FinishReason::Length),
            Some(FinishReason::ContentFilter),
            Some(FinishReason::ToolCalls),
            Some(FinishReason::FunctionCall),
            None,
        ] {
            let decision = classify_response(
                &Some("partial text".into()),
                &[],
                &resp("partial text", finish),
                state(0, 2, false),
            );
            assert!(
                matches!(decision, ResponsePolicyDecision::Fail { .. }),
                "{finish:?}"
            );
        }
    }

    #[test]
    fn incomplete_tool_args_retry_only_within_budget() {
        let tool_calls = [ToolCall {
            tool_name: "files".into(),
            tool_input: serde_json::Value::Null,
            is_final: false,
            tool_call_id: Some("c1".into()),
        }];
        let response = resp("", Some(FinishReason::ToolCalls));

        assert!(matches!(
            classify_response(&None, &tool_calls, &response, state(0, 2, false)),
            ResponsePolicyDecision::RetryIncompleteToolArgs { .. }
        ));
        for retry_state in [state(2, 2, false), state(0, 2, true)] {
            assert!(matches!(
                classify_response(&None, &tool_calls, &response, retry_state),
                ResponsePolicyDecision::Fail { .. }
            ));
        }
    }

    #[test]
    fn complete_tool_calls_accept_provider_tool_finish_reasons() {
        let tool_call = ToolCall {
            tool_name: "files".into(),
            tool_input: serde_json::json!({"path":"a.txt"}),
            is_final: false,
            tool_call_id: Some("c1".into()),
        };
        for finish in [FinishReason::ToolCalls, FinishReason::FunctionCall] {
            assert_eq!(
                classify_response(
                    &None,
                    std::slice::from_ref(&tool_call),
                    &resp("", Some(finish)),
                    state(0, 2, false),
                ),
                ResponsePolicyDecision::Accept
            );
        }
    }

    #[test]
    fn empty_response_fails_instead_of_retrying() {
        for finish in [Some(FinishReason::Stop), Some(FinishReason::Length), None] {
            assert!(matches!(
                classify_response(&None, &[], &resp("", finish), state(0, 2, false)),
                ResponsePolicyDecision::Fail { .. }
            ));
        }
    }
}
