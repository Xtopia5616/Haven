pub use haven_common::types::ContentPart;
use haven_common::types::{CanonicalMessage, CanonicalRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use thiserror::Error;

/// Convert provider-neutral canonical messages into provider-specific LlmMessage.
/// Called at the LLM invocation boundary — the Agent internally uses CanonicalMessage.
pub fn convert_to_llm(msgs: Vec<CanonicalMessage>) -> Vec<LlmMessage> {
    msgs.into_iter()
        .map(|m| {
            let role = match m.role {
                CanonicalRole::System => LlmRole::System,
                CanonicalRole::User => LlmRole::User,
                CanonicalRole::Assistant => LlmRole::Assistant,
                CanonicalRole::Tool => LlmRole::Tool,
            };
            LlmMessage {
                role,
                content: m.content,
                tool_call_id: m.tool_call_id,
                tool_calls: m.tool_calls.map(|calls| {
                    calls
                        .into_iter()
                        .map(|tc| ToolCall {
                            id: tc.id,
                            name: tc.name,
                            arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                        })
                        .collect()
                }),
                reasoning: m.reasoning,
            }
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: Vec<ContentPart>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool calls from this (assistant) message, forwarded to the API so
    /// subsequent tool responses can be linked by tool_call_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Internal reasoning/chain-of-thought from the model (e.g. DeepSeek's
    /// reasoning_content). Echoed back to APIs that require it in
    /// multi-turn requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum LlmRole {
    #[default]
    System,
    User,
    Assistant,
    Tool,
}

impl std::fmt::Display for LlmRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmRole::System => write!(f, "system"),
            LlmRole::User => write!(f, "user"),
            LlmRole::Assistant => write!(f, "assistant"),
            LlmRole::Tool => write!(f, "tool"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    // §2.14: model name and cost tracking
    pub model_name: Option<String>,
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    FunctionCall,
}

impl fmt::Display for FinishReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FinishReason::Stop => write!(f, "stop"),
            FinishReason::Length => write!(f, "length"),
            FinishReason::ToolCalls => write!(f, "tool_calls"),
            FinishReason::ContentFilter => write!(f, "content_filter"),
            FinishReason::FunctionCall => write!(f, "function_call"),
        }
    }
}

impl FinishReason {
    /// Parse a finish_reason string from any OpenAI-compatible provider.
    /// Accepts standard OpenAI values plus common non-standard variants
    /// from Ollama, vLLM, Google Gemini, Anthropic, etc.
    pub fn from_openai(s: &str) -> Option<Self> {
        match s {
            "stop" | "end" | "end_turn" | "completed" | "done" => Some(FinishReason::Stop),
            "length" | "max_tokens" | "incomplete" | "max_length" => Some(FinishReason::Length),
            "tool_calls" | "tool_use" | "tools" => Some(FinishReason::ToolCalls),
            "function_call" => Some(FinishReason::FunctionCall),
            "content_filter" | "safety" | "blocked" | "moderation" => {
                Some(FinishReason::ContentFilter)
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<FinishReason>,
    pub usage: Usage,
    // §2.14: which model produced this response
    pub model: Option<String>,
    /// Internal reasoning/chain-of-thought produced by the model (e.g.
    /// DeepSeek-R1's reasoning_content, Claude's extended thinking).
    pub reasoning: Option<String>,
}

/// OpenAI-compatible tool definition for function calling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Error)]
pub enum LlmError {
    #[error("network timeout: {0}")]
    Timeout(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("rate limited by provider")]
    RateLimit {
        retry_after: Option<std::time::Duration>,
    },

    #[error("server error: {0}")]
    ServerError(String),

    #[error("invalid response: {0}")]
    InvalidResponse(String),

    #[error("request failed: {0}")]
    RequestFailed(String),

    #[error("cancelled by user")]
    Cancelled,

    #[error("stream truncated")]
    StreamTruncated,

    #[error("content filtered by provider")]
    ContentFilter,

    #[error("context length exceeded")]
    ContextLengthExceeded,

    #[error("billing issue: {0}")]
    Billing(String),

    #[error("unknown error: {0}")]
    Unknown(String),

    /// Composite error: primary + balanced model both failed
    #[error("all endpoints failed: primary={0}, balanced_model={1}")]
    AllEndpointsFailed(String, String),

    /// Stream aborted by a configured stream rule (Abort mode).
    /// Contains (rule_name, inject_text).
    #[error("stream aborted by rule '{0}': {1}")]
    StreamAborted(String, String),
}

impl LlmError {
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            LlmError::RateLimit { retry_after } => *retry_after,
            _ => None,
        }
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LlmError::Timeout(_)
                | LlmError::ServerError(_)
                | LlmError::RateLimit { .. }
                | LlmError::StreamTruncated
        )
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            LlmError::Timeout(e.to_string())
        } else if e.is_connect() {
            LlmError::ServerError(e.to_string())
        } else if let Some(status) = e.status() {
            if status.as_u16() == 401 || status.as_u16() == 403 {
                LlmError::Auth(e.to_string())
            } else if status.as_u16() == 429 {
                LlmError::RateLimit { retry_after: None }
            } else if status.is_server_error() {
                LlmError::ServerError(e.to_string())
            } else {
                LlmError::RequestFailed(e.to_string())
            }
        } else {
            LlmError::Unknown(e.to_string())
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct StreamChunk {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: Option<FinishReason>,
    pub usage: Option<Usage>,
    pub model: Option<String>,
    /// Internal reasoning/chain-of-thought delta (e.g. DeepSeek-R1's
    /// reasoning_content, Claude's extended thinking).
    pub reasoning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::{CanonicalMessage, CanonicalRole};
    use std::time::Duration;

    #[test]
    fn finish_reason_all_variants_exist() {
        let stop = FinishReason::Stop;
        let length = FinishReason::Length;
        let tool_calls = FinishReason::ToolCalls;
        let content_filter = FinishReason::ContentFilter;
        let function_call = FinishReason::FunctionCall;
        assert_eq!(stop, FinishReason::Stop);
        assert_eq!(length, FinishReason::Length);
        assert_eq!(tool_calls, FinishReason::ToolCalls);
        assert_eq!(content_filter, FinishReason::ContentFilter);
        assert_eq!(function_call, FinishReason::FunctionCall);
    }

    #[test]
    fn finish_reason_display() {
        assert_eq!(FinishReason::Stop.to_string(), "stop");
        assert_eq!(FinishReason::Length.to_string(), "length");
        assert_eq!(FinishReason::ToolCalls.to_string(), "tool_calls");
        assert_eq!(FinishReason::ContentFilter.to_string(), "content_filter");
        assert_eq!(FinishReason::FunctionCall.to_string(), "function_call");
    }

    #[test]
    fn finish_reason_from_openai_known_strings() {
        assert_eq!(FinishReason::from_openai("stop"), Some(FinishReason::Stop));
        assert_eq!(
            FinishReason::from_openai("length"),
            Some(FinishReason::Length)
        );
        assert_eq!(
            FinishReason::from_openai("tool_calls"),
            Some(FinishReason::ToolCalls)
        );
        assert_eq!(
            FinishReason::from_openai("content_filter"),
            Some(FinishReason::ContentFilter)
        );
        assert_eq!(
            FinishReason::from_openai("function_call"),
            Some(FinishReason::FunctionCall)
        );
    }

    #[test]
    fn finish_reason_from_openai_unknown_returns_none() {
        assert_eq!(FinishReason::from_openai("unknown_reason"), None);
        assert_eq!(FinishReason::from_openai(""), None);
        assert_eq!(FinishReason::from_openai("STOP"), None);
    }

    #[test]
    fn llm_role_display_outputs_lowercase() {
        assert_eq!(LlmRole::System.to_string(), "system");
        assert_eq!(LlmRole::User.to_string(), "user");
        assert_eq!(LlmRole::Assistant.to_string(), "assistant");
        assert_eq!(LlmRole::Tool.to_string(), "tool");
    }

    #[test]
    fn llm_role_default_is_system() {
        assert_eq!(LlmRole::default().to_string(), "system");
    }

    #[test]
    fn llm_message_system_role_with_content() {
        let msg = LlmMessage {
            role: LlmRole::System,
            content: vec![ContentPart::Text("system prompt".into())],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        };
        assert_eq!(msg.role.to_string(), "system");
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn llm_message_user_role_with_text() {
        let msg = LlmMessage {
            role: LlmRole::User,
            content: vec![ContentPart::text("hello")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        };
        assert_eq!(msg.role.to_string(), "user");
        assert_eq!(msg.content.len(), 1);
    }

    #[test]
    fn llm_message_assistant_role_empty_content() {
        let msg = LlmMessage {
            role: LlmRole::Assistant,
            content: vec![],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        };
        assert_eq!(msg.role.to_string(), "assistant");
        assert!(msg.content.is_empty());
    }

    #[test]
    fn usage_default_values_are_zero() {
        let u = Usage::default();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.total_tokens, 0);
        assert!(u.model_name.is_none());
        assert!(u.cost.is_none());
    }

    #[test]
    fn tool_call_construction() {
        let tc = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"/tmp"}"#.into(),
        };
        assert_eq!(tc.id, "call_1");
        assert_eq!(tc.name, "read_file");
        assert_eq!(tc.arguments, r#"{"path":"/tmp"}"#);
    }

    #[test]
    fn llm_error_display_request_failed() {
        let err = LlmError::RequestFailed("connection refused".into());
        assert!(err.to_string().contains("connection refused"));
    }

    #[test]
    fn llm_error_display_rate_limit() {
        let err = LlmError::RateLimit { retry_after: None };
        assert!(err.to_string().contains("rate limited"));
    }

    #[test]
    fn llm_error_display_unauthorized() {
        let err = LlmError::Auth("invalid api key".into());
        assert!(err.to_string().contains("invalid api key"));
    }

    #[test]
    fn llm_error_display_stream_truncated() {
        assert!(LlmError::StreamTruncated.to_string().contains("truncated"));
    }

    #[test]
    fn llm_error_display_context_length_exceeded() {
        assert!(
            LlmError::ContextLengthExceeded
                .to_string()
                .contains("context length")
        );
    }

    #[test]
    fn llm_error_retry_after_returns_stored_duration() {
        let d = Duration::from_secs(15);
        let err = LlmError::RateLimit {
            retry_after: Some(d),
        };
        assert_eq!(err.retry_after(), Some(d));
    }

    #[test]
    fn llm_error_retry_after_returns_none_for_non_rate_limit() {
        let err = LlmError::RequestFailed("boom".into());
        assert_eq!(err.retry_after(), None);
    }

    #[test]
    fn llm_error_is_retryable_rate_limit() {
        assert!(LlmError::RateLimit { retry_after: None }.is_retryable());
    }

    #[test]
    fn llm_error_is_retryable_timeout() {
        assert!(LlmError::Timeout("t".into()).is_retryable());
    }

    #[test]
    fn llm_error_is_retryable_server_error() {
        assert!(LlmError::ServerError("s".into()).is_retryable());
    }

    #[test]
    fn llm_error_is_retryable_stream_truncated() {
        assert!(LlmError::StreamTruncated.is_retryable());
    }

    #[test]
    fn llm_error_is_not_retryable_unauthorized() {
        assert!(!LlmError::Auth("bad key".into()).is_retryable());
    }

    #[test]
    fn llm_error_is_not_retryable_request_failed() {
        assert!(!LlmError::RequestFailed("x".into()).is_retryable());
    }

    #[test]
    fn llm_error_is_not_retryable_cancelled() {
        assert!(!LlmError::Cancelled.is_retryable());
    }

    #[test]
    fn stream_chunk_construction_with_text_only() {
        let chunk = StreamChunk {
            text: Some("delta".into()),
            tool_calls: vec![],
            finish_reason: None,
            usage: None,
            model: None,
            reasoning: None,
        };
        assert_eq!(chunk.text, Some("delta".into()));
        assert!(chunk.tool_calls.is_empty());
    }

    #[test]
    fn stream_chunk_construction_with_tool_calls_and_finish_reason() {
        let chunk = StreamChunk {
            text: None,
            tool_calls: vec![ToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                arguments: "{}".into(),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                model_name: None,
                cost: None,
            }),
            model: Some("gpt-4o".into()),
            reasoning: None,
        };
        assert_eq!(chunk.tool_calls.len(), 1);
        assert_eq!(chunk.tool_calls[0].name, "shell");
        assert_eq!(chunk.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 15);
        assert_eq!(chunk.model.as_deref(), Some("gpt-4o"));
    }

    #[test]
    fn tool_definition_construction() {
        let td = ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "my_tool".into(),
                description: "does something useful".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        };
        assert_eq!(td.tool_type, "function");
        assert_eq!(td.function.name, "my_tool");
        assert_eq!(td.function.description, "does something useful");
    }

    #[test]
    fn tool_function_construction() {
        let tf = ToolFunction {
            name: "echo".into(),
            description: "echoes back the input".into(),
            parameters: serde_json::json!({}),
        };
        assert_eq!(tf.name, "echo");
        assert!(tf.description.contains("echoes"));
    }

    #[test]
    fn convert_to_llm_system_role() {
        let cm = CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::Text("system prompt".into())],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
            reasoning: None,
        };
        let msgs = convert_to_llm(vec![cm]);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role.to_string(), "system");
    }

    #[test]
    fn convert_to_llm_user_role() {
        let cm = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hi")],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
            reasoning: None,
        };
        let msgs = convert_to_llm(vec![cm]);
        assert_eq!(msgs[0].role.to_string(), "user");
    }

    #[test]
    fn convert_to_llm_tool_maps_to_tool_role() {
        let cm = CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::Text("tool result".into())],
            tool_calls: None,
            tool_call_id: Some("tid".into()),
            parent_message_id: None,
            reasoning: None,
        };
        let msgs = convert_to_llm(vec![cm]);
        assert_eq!(msgs[0].role.to_string(), "tool");
        assert_eq!(msgs[0].tool_call_id.as_deref(), Some("tid"));
    }

    #[test]
    fn convert_to_llm_assistant_role() {
        let cm = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("answer")],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
            reasoning: None,
        };
        let msgs = convert_to_llm(vec![cm]);
        assert_eq!(msgs[0].role.to_string(), "assistant");
    }

    #[test]
    fn convert_to_llm_preserves_content_parts() {
        let cm = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![
                ContentPart::Text("part1".into()),
                ContentPart::Text("part2".into()),
            ],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
            reasoning: None,
        };
        let msgs = convert_to_llm(vec![cm]);
        assert_eq!(msgs[0].content.len(), 2);
    }

    #[test]
    fn convert_to_llm_multiple_messages() {
        let cms = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::Text("prompt".into())],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("question")],
                tool_calls: None,
                tool_call_id: None,
                parent_message_id: None,
                reasoning: None,
            },
        ];
        let msgs = convert_to_llm(cms);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role.to_string(), "system");
        assert_eq!(msgs[1].role.to_string(), "user");
    }
}
