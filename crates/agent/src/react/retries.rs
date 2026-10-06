//! Policy for structurally incomplete tool-call arguments.

use super::*;
use haven_llm::{FinishReason, LlmResponse};

/// Re-emit the full tool call with valid arguments; never continue a
/// half-written JSON string.
const INCOMPLETE_TOOL_ARGS_NUDGE: &str = "Your previous tool call had incomplete JSON arguments. Emit the same tool call again with complete, valid JSON arguments.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AfterLlmAction {
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
    pub(crate) fn classify(
        thought: &Option<String>,
        tool_calls: &[ToolCall],
        response: &LlmResponse,
        state: ResponsePolicyState,
    ) -> AfterLlmAction {
        // An incomplete arguments object is the one response shape that is
        // safe and useful to retry automatically. Never dispatch placeholders.
        let incomplete_tool_args = tool_calls
            .iter()
            .any(|tool_call| !tool_call.is_final && tool_call.tool_input.is_null());
        if incomplete_tool_args {
            if !state.pending_ask
                && response.web_search_calls.is_empty()
                && state.incomplete_tool_args_retries_used < state.incomplete_tool_args_retries_max
            {
                return AfterLlmAction::RetryIncompleteToolArgs {
                    nudge: INCOMPLETE_TOOL_ARGS_NUDGE,
                };
            }
            return AfterLlmAction::Fail {
                reason: "工具调用参数不是完整 JSON，无法安全执行；点击“继续生成”重新尝试。".into(),
            };
        }

        let empty =
            thought.is_none() && tool_calls.is_empty() && response.web_search_calls.is_empty();
        if empty {
            if state.pending_ask && response.finish_reason == Some(FinishReason::Stop) {
                return AfterLlmAction::Accept;
            }
            let reason = if Self::has_normal_finish(response, tool_calls) {
                "模型返回了空响应；已保留当前输出，可点击“继续生成”重试。".into()
            } else {
                Self::abnormal_finish_reason(response)
            };
            return AfterLlmAction::Fail { reason };
        }

        if !Self::has_normal_finish(response, tool_calls) {
            return AfterLlmAction::Fail {
                reason: Self::abnormal_finish_reason(response),
            };
        }

        AfterLlmAction::Accept
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

    #[test]
    fn normal_stop_accepts_text_without_lexical_cutoff_heuristics() {
        for text in [
            "让我先查一下，",
            "接下来",
            "waiting for result...",
            "路路路",
        ] {
            assert_eq!(
                ResponsePolicy::classify(
                    &Some(text.into()),
                    &[],
                    &resp(text, Some(FinishReason::Stop)),
                    state(0, 2, false),
                ),
                AfterLlmAction::Accept
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
            let action = ResponsePolicy::classify(
                &Some("partial text".into()),
                &[],
                &resp("partial text", finish),
                state(0, 2, false),
            );
            assert!(matches!(action, AfterLlmAction::Fail { .. }), "{finish:?}");
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
            ResponsePolicy::classify(&None, &tool_calls, &response, state(0, 2, false)),
            AfterLlmAction::RetryIncompleteToolArgs { .. }
        ));
        for retry_state in [state(2, 2, false), state(0, 2, true)] {
            assert!(matches!(
                ResponsePolicy::classify(&None, &tool_calls, &response, retry_state),
                AfterLlmAction::Fail { .. }
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
                ResponsePolicy::classify(
                    &None,
                    std::slice::from_ref(&tool_call),
                    &resp("", Some(finish)),
                    state(0, 2, false),
                ),
                AfterLlmAction::Accept
            );
        }
    }

    #[test]
    fn empty_response_fails_instead_of_retrying() {
        for finish in [Some(FinishReason::Stop), Some(FinishReason::Length), None] {
            assert!(matches!(
                ResponsePolicy::classify(&None, &[], &resp("", finish), state(0, 2, false)),
                AfterLlmAction::Fail { .. }
            ));
        }
    }
}
