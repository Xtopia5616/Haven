//! Provider wire DTOs and event envelopes. No canonical mapping lives here.

use super::*;

// ---------------------------------------------------------------------------
// OpenAI-compatible request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct OpenAiMessage {
    pub(super) role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_call_id: Option<String>,
    /// Tool calls in assistant messages, serialized as the OpenAI tool_calls
    /// array so the API can link subsequent tool responses by tool_call_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_calls: Option<Vec<OpenAiMessageToolCall>>,
    /// DeepSeek et al. require the reasoning_content of prior assistant
    /// turns to be echoed back in the request for thinking-mode conversations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_content: Option<String>,
    /// DeepSeek web search: the provider's built-in search tool output must
    /// be passed back verbatim in the next request's assistant message so the
    /// server restores the search context (stateless chat API). Never parsed
    /// or rewritten.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) web_search_call: Vec<serde_json::Value>,
}

/// A tool call within an assistant message, matching the OpenAI API format.
#[derive(Debug, Serialize)]
pub(super) struct OpenAiMessageToolCall {
    pub(super) id: String,
    #[serde(rename = "type")]
    pub(super) call_type: String,
    pub(super) function: OpenAiMessageToolFunction,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAiMessageToolFunction {
    pub(super) name: String,
    pub(super) arguments: String,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAiRequest {
    pub(super) model: String,
    pub(super) messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f32>,
    pub(super) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<serde_json::Value>,
    // §2.8: additional model parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) response_format: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning_effort: Option<String>,
    /// Vendor thinking toggle (DeepSeek / Kimi): `{"type":"enabled|disabled"}`,
    /// optionally with Kimi `keep: "all"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) stream_options: Option<StreamOptions>,
    /// xAI Live Search (`search_parameters`). Omitted for non-xAI styles and
    /// when web search mode is `off`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) search_parameters: Option<Value>,
    /// Stable routing hint for OpenAI-compatible prompt caches. It does not
    /// alter the prompt; providers use it to keep matching prefixes on a cache
    /// shard. Unsupported gateways are detected and downgraded at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prompt_cache_key: Option<String>,
    #[serde(skip)]
    pub(super) cache_diagnostics: CacheDiagnostics,
}

#[derive(Debug, Serialize)]
pub(super) struct StreamOptions {
    pub(super) include_usage: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAiTool {
    #[serde(rename = "type")]
    pub(super) tool_type: String,
    pub(super) function: OpenAiToolFunction,
}

#[derive(Debug, Serialize)]
pub(super) struct OpenAiToolFunction {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) parameters: Value,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAiChoice {
    pub(super) message: Option<OpenAiMessageOut>,
    pub(super) delta: Option<OpenAiMessageOut>,
    #[serde(alias = "stop_reason")]
    pub(super) finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAiMessageOut {
    #[allow(dead_code)]
    pub(super) role: Option<String>,
    #[serde(default)]
    pub(super) content: Option<String>,
    #[serde(default)]
    pub(super) tool_calls: Option<Vec<OpenAiToolCallOut>>,
    #[serde(default)]
    pub(super) reasoning_content: Option<String>,
    /// DeepSeek's built-in web search output (`web_search_call` items).
    /// Accumulated and echoed back verbatim in the next request.
    #[serde(default)]
    pub(super) web_search_call: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAiToolCallOut {
    pub(super) id: Option<String>,
    pub(super) index: Option<i32>,
    #[serde(rename = "function")]
    pub(super) function: OpenAiFunctionOut,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAiFunctionOut {
    pub(super) name: Option<String>,
    pub(super) arguments: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct OpenAiPromptTokensDetails {
    #[serde(default)]
    pub(super) cached_tokens: u32,
    #[serde(default)]
    pub(super) cache_write_tokens: u32,
    #[serde(default)]
    pub(super) cache_creation_tokens: u32,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct OpenAiUsage {
    #[serde(default)]
    pub(super) prompt_tokens: u32,
    /// Some OpenAI-compatible proxies emit Responses-style names on chat.
    #[serde(default)]
    pub(super) input_tokens: u32,
    #[serde(default)]
    pub(super) completion_tokens: u32,
    #[serde(default)]
    pub(super) output_tokens: u32,
    #[serde(default)]
    pub(super) total_tokens: u32,
    /// OpenAI / compatible: nested cache hit count.
    #[serde(default)]
    pub(super) prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
    /// DeepSeek Chat Completions flat alias for cache hits.
    #[serde(default)]
    pub(super) prompt_cache_hit_tokens: u32,
    /// Kimi / Moonshot top-level cache hit count.
    #[serde(default)]
    pub(super) cached_tokens: u32,
    /// DeepSeek's reported normal (non-cache) input tokens.
    #[serde(default)]
    pub(super) prompt_cache_miss_tokens: u32,
    #[serde(default)]
    pub(super) cache_write_tokens: u32,
}

impl OpenAiUsage {
    pub(super) fn prompt(&self) -> u32 {
        self.prompt_tokens.max(self.input_tokens)
    }

    pub(super) fn completion(&self) -> u32 {
        self.completion_tokens.max(self.output_tokens)
    }

    pub(super) fn cached(&self) -> u32 {
        crate::adapters::resolve_cached_tokens(
            self.prompt_tokens_details.as_ref().map(|d| d.cached_tokens),
            self.prompt_cache_hit_tokens.max(self.cached_tokens),
        )
    }

    pub(super) fn cache_created(&self) -> u32 {
        self.prompt_tokens_details
            .as_ref()
            .map(|details| {
                details
                    .cache_write_tokens
                    .max(details.cache_creation_tokens)
            })
            .unwrap_or(0)
            .max(self.cache_write_tokens)
    }

    pub(super) fn to_usage(&self, model_name: Option<String>) -> Usage {
        let mut usage = Usage::from_counts_with_accounting(
            self.prompt(),
            self.completion(),
            self.total_tokens,
            self.cached(),
            self.cache_created(),
            CacheAccounting::Inclusive,
            model_name,
        );
        usage.cache_miss_tokens = self.prompt_cache_miss_tokens.max(usage.cache_miss_tokens());
        usage
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAiResponse {
    #[serde(alias = "candidates")]
    pub(super) choices: Vec<OpenAiChoice>,
    pub(super) usage: Option<OpenAiUsage>,
    pub(super) model: Option<String>,
    /// xAI Live Search citation URLs (top-level on the final response).
    #[serde(default)]
    pub(super) citations: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct OpenAiStreamResponse {
    #[serde(alias = "candidates")]
    pub(super) choices: Vec<OpenAiChoice>,
    pub(super) usage: Option<OpenAiUsage>,
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) citations: Vec<String>,
}
