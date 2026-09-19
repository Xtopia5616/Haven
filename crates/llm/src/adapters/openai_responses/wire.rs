//! Provider wire DTOs and event envelopes. No canonical mapping lives here.

use super::*;

// ---------------------------------------------------------------------------
// OpenAI Responses API request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct ResponsesTool {
    #[serde(rename = "type")]
    pub(super) tool_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parameters: Option<Value>,
    /// Haven validates tool calls locally. Explicitly keep Responses in its
    /// non-strict schema mode so it does not auto-rewrite a richer JSON Schema
    /// into its strict subset before accepting the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) strict: Option<bool>,
}

#[derive(Debug, Serialize)]
pub(super) struct ResponsesRequest {
    pub(super) model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) instructions: Option<String>,
    pub(super) input: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) top_p: Option<f32>,
    pub(super) stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tool_choice: Option<Value>,
    /// OpenAI / DeepSeek Responses reasoning config: `{ "effort": "…" }`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reasoning: Option<Value>,
    /// DeepSeek Responses uses `output_config.effort` for thinking depth.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) output_config: Option<Value>,
    /// Responses structured-output configuration is nested under `text`;
    /// Haven's endpoint config stores the inner `format` object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<Value>,
    /// Stable routing hint for Responses-compatible prompt caches. Unsupported
    /// gateways are detected and retried once without this optional field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) prompt_cache_key: Option<String>,
    #[serde(skip)]
    pub(super) cache_diagnostics: CacheDiagnostics,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ResponsesItem {
    #[serde(rename = "type")]
    pub(super) item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) content: Vec<ResponsesContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) arguments: Option<String>,
    /// Fields not covered by the named members (e.g. a `web_search_call`
    /// item's `status`). Kept so output items can be round-tripped back into
    /// the next request's input verbatim.
    #[serde(flatten)]
    pub(super) extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct ResponsesContentPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ResponsesInputTokensDetails {
    #[serde(default)]
    pub(super) cached_tokens: u32,
    #[serde(default)]
    pub(super) cache_write_tokens: u32,
}

#[derive(Debug, Deserialize, Default)]
pub(super) struct ResponsesUsage {
    #[serde(default)]
    pub(super) input_tokens: u32,
    #[serde(default)]
    pub(super) prompt_tokens: u32,
    #[serde(default)]
    pub(super) output_tokens: u32,
    #[serde(default)]
    pub(super) completion_tokens: u32,
    #[serde(default)]
    pub(super) total_tokens: u32,
    #[serde(default)]
    pub(super) input_tokens_details: Option<ResponsesInputTokensDetails>,
    /// DeepSeek Responses flat alias for cache hits.
    #[serde(default)]
    pub(super) prompt_cache_hit_tokens: u32,
    #[serde(default)]
    pub(super) cached_tokens: u32,
    #[serde(default)]
    pub(super) prompt_cache_miss_tokens: u32,
    #[serde(default)]
    pub(super) cache_write_tokens: u32,
}

impl ResponsesUsage {
    pub(super) fn prompt(&self) -> u32 {
        self.input_tokens.max(self.prompt_tokens)
    }

    pub(super) fn completion(&self) -> u32 {
        self.output_tokens.max(self.completion_tokens)
    }

    pub(super) fn cached(&self) -> u32 {
        crate::adapters::resolve_cached_tokens(
            self.input_tokens_details.as_ref().map(|d| d.cached_tokens),
            self.prompt_cache_hit_tokens.max(self.cached_tokens),
        )
    }

    pub(super) fn cache_created(&self) -> u32 {
        self.input_tokens_details
            .as_ref()
            .map(|details| details.cache_write_tokens)
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
pub(super) struct ResponsesResponse {
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) output: Vec<ResponsesItem>,
    pub(super) usage: Option<ResponsesUsage>,
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) error: Option<Value>,
}

// Streaming SSE events (https://platform.openai.com/docs/api-reference/responses)
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub(super) enum ResponsesStreamEvent {
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: Option<String> },
    /// DeepSeek's thinking-mode compat layer streams the assistant's
    /// reasoning via this event. It must be accumulated and echoed back in
    /// the next request's input (see `convert_input`), or the provider
    /// rejects tool-call history with a 400 ("The `reasoning_text` in the
    /// thinking mode must be passed back to the API.").
    #[serde(rename = "response.reasoning_text.delta")]
    ReasoningTextDelta { delta: Option<String> },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgsDelta {
        item_id: Option<String>,
        delta: Option<String>,
    },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { item: Option<ResponsesItem> },
    /// DeepSeek streams the complete output item here: for a
    /// `web_search_call` this is the ONLY event that carries the full
    /// payload (`action` with `queries`, `status: "completed"`) — the
    /// `output_item.added` skeleton and the `web_search_call.*` status
    /// events lack it, and echoing the bare skeleton back 400s ("missing
    /// field `action`"). The full item must replace the skeleton (by id)
    /// before the round-trip.
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: Option<ResponsesItem> },
    #[serde(rename = "response.web_search_call.in_progress")]
    WebSearchInProgress {
        #[serde(default)]
        item_id: Option<String>,
    },
    #[serde(rename = "response.web_search_call.searching")]
    WebSearchSearching {
        #[serde(default)]
        item_id: Option<String>,
    },
    #[serde(rename = "response.web_search_call.completed")]
    WebSearchCompleted {
        #[serde(default)]
        item_id: Option<String>,
        #[serde(default)]
        item: Option<ResponsesItem>,
    },
    #[serde(rename = "response.completed")]
    Completed {
        response: Option<ResponsesStreamResponse>,
    },
    #[serde(rename = "response.incomplete")]
    Incomplete {
        response: Option<ResponsesStreamResponse>,
    },
    #[serde(rename = "response.failed")]
    Failed {
        response: Option<ResponsesStreamResponse>,
    },
    #[serde(rename = "error")]
    Error { message: Option<String> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
pub(super) struct ResponsesStreamResponse {
    #[serde(default)]
    pub(super) status: Option<String>,
    /// Some OpenAI-compatible Responses gateways omit trailing
    /// `response.output_text.delta` events around a function call and only
    /// include the complete assistant output in `response.completed`.
    #[serde(default)]
    pub(super) output: Vec<ResponsesItem>,
    #[serde(default)]
    pub(super) usage: Option<ResponsesUsage>,
    #[serde(default)]
    pub(super) model: Option<String>,
    #[serde(default)]
    pub(super) error: Option<Value>,
}
