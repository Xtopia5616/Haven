use haven_common::config::RequestKind;
use haven_common::types::{CanonicalMessage, CanonicalToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fmt;
use thiserror::Error;

pub use haven_common::types::CacheAccounting;

/// Non-sensitive prompt-cache request and provider outcome metadata. This is
/// persisted per call for diagnostics, never with the cache key or prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheDiagnostics {
    /// `off`, `key`, `split`, `implicit`, or `explicit` describes the effective
    /// wire strategy.
    #[serde(default)]
    pub mode: String,
    /// Configured provider identity for the request, independent of its wire
    /// protocol adapter (for example a named OpenAI-compatible gateway).
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub key_requested: bool,
    #[serde(default)]
    pub system_split: bool,
    /// True when an optional cache extension was rejected and retried safely.
    #[serde(default)]
    pub downgraded: bool,
    /// `disabled`, `unknown`, `hit`, or `miss`; no provider-usage response is
    /// deliberately kept as `unknown`, never guessed as a cache miss.
    #[serde(default)]
    pub outcome: String,
    /// `provider` means a cache usage field was present, including an
    /// explicit zero. `unavailable` means the response omitted cache usage.
    #[serde(default = "cache_usage_unavailable")]
    pub usage_source: String,
}

fn cache_usage_unavailable() -> String {
    "unavailable".into()
}

impl Default for CacheDiagnostics {
    fn default() -> Self {
        Self {
            mode: "off".into(),
            provider: String::new(),
            key_requested: false,
            system_split: false,
            downgraded: false,
            outcome: "unknown".into(),
            usage_source: cache_usage_unavailable(),
        }
    }
}

impl CacheDiagnostics {
    pub fn for_request(key_requested: bool, system_split: bool) -> Self {
        Self {
            mode: if system_split {
                "split".into()
            } else if key_requested {
                "key".into()
            } else {
                "off".into()
            },
            provider: String::new(),
            key_requested,
            system_split,
            downgraded: false,
            outcome: if key_requested || system_split {
                "unknown".into()
            } else {
                "disabled".into()
            },
            usage_source: cache_usage_unavailable(),
        }
    }

    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = provider.into();
        self
    }

    /// Use for providers with cache controls or automatic prefix caching but
    /// without an explicit routing key (for example Anthropic and the Gemini
    /// fallback path).
    pub fn for_provider_cache(system_split: bool) -> Self {
        Self {
            mode: if system_split {
                "split".into()
            } else {
                "implicit".into()
            },
            provider: String::new(),
            key_requested: false,
            system_split,
            downgraded: false,
            outcome: "unknown".into(),
            usage_source: cache_usage_unavailable(),
        }
    }

    /// Use when a provider resource explicitly owns the reusable prompt
    /// prefix (Gemini `cachedContent`).
    pub fn for_explicit_provider_cache(system_split: bool) -> Self {
        Self {
            mode: "explicit".into(),
            provider: String::new(),
            key_requested: false,
            system_split,
            downgraded: false,
            outcome: "unknown".into(),
            usage_source: cache_usage_unavailable(),
        }
    }

    pub fn with_provider_usage(mut self, cached_tokens: Option<u32>, usage_reported: bool) -> Self {
        self.usage_source = if usage_reported {
            "provider".into()
        } else {
            cache_usage_unavailable()
        };
        if usage_reported {
            self.outcome = match cached_tokens {
                Some(tokens) if tokens > 0 => "hit".into(),
                Some(_) => "miss".into(),
                None => "unknown".into(),
            };
        } else if self.outcome != "disabled" {
            self.outcome = "unknown".into();
        }
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    /// Prompt-cache hit / read tokens (OpenAI `cached_tokens`, Anthropic
    /// `cache_read_input_tokens`, DeepSeek hit, Gemini `cachedContentTokenCount`).
    /// Providers usually already include these inside `prompt_tokens` (OpenAI);
    /// Anthropic reports them separately from `input_tokens`.
    #[serde(default)]
    pub cached_tokens: u32,
    /// Prompt-cache write / creation tokens (Anthropic `cache_creation_input_tokens`).
    /// Other providers typically leave this at 0.
    #[serde(default)]
    pub cache_creation_tokens: u32,
    /// Input tokens that were not read from prompt cache. Adapters compute
    /// this from their explicit accounting contract before aggregation.
    #[serde(default)]
    pub cache_miss_tokens: u32,
    #[serde(default)]
    pub cache_accounting: CacheAccounting,
    #[serde(default)]
    pub cache_diagnostics: Option<CacheDiagnostics>,
    // §2.14: model name and cost tracking
    pub model_name: Option<String>,
    pub cost: Option<f64>,
}

/// Runtime-only metadata for one model call owned by a higher-level
/// capability (for example media ingress or a tool). The Agent layer maps
/// this shared shape to durable `llm_usage` rows after it knows the owning
/// session. It deliberately contains no prompt, media bytes, or cache key.
#[derive(Debug, Clone)]
pub struct LlmCallUsage {
    pub request: RequestKind,
    pub usage: Usage,
    pub model: Option<String>,
    pub duration_ms: Option<u64>,
}

impl Usage {
    /// Build a canonical usage row and fill omitted `total_tokens`.
    pub fn from_counts(
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
        cache_creation_tokens: u32,
        model_name: Option<String>,
    ) -> Self {
        Self::from_counts_with_accounting(
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            CacheAccounting::Unknown,
            model_name,
        )
    }

    pub fn from_counts_with_accounting(
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
        cache_creation_tokens: u32,
        cache_accounting: CacheAccounting,
        model_name: Option<String>,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            cached_tokens,
            cache_creation_tokens,
            cache_miss_tokens: 0,
            cache_accounting,
            cache_diagnostics: None,
            model_name,
            cost: None,
        }
        .normalize()
    }

    /// Fill `total_tokens` when the provider omitted it using the explicit
    /// cache accounting contract supplied by the adapter.
    pub fn normalize(mut self) -> Self {
        if self.total_tokens == 0 {
            let extra = if self.cache_accounting == CacheAccounting::Exclusive {
                self.cached_tokens
                    .saturating_add(self.cache_creation_tokens)
            } else {
                0
            };
            self.total_tokens = self
                .prompt_tokens
                .saturating_add(self.completion_tokens)
                .saturating_add(extra);
        }
        self
    }

    /// True when cache read/write tokens are declared outside `prompt_tokens`.
    /// Unknown accounting metadata never infers token inclusion from totals.
    pub fn cache_exclusive_of_prompt(&self) -> bool {
        self.cache_accounting == CacheAccounting::Exclusive
    }

    /// Tokens occupying the model context window for this call.
    pub fn context_tokens(&self) -> u32 {
        if self.cache_exclusive_of_prompt() {
            self.prompt_tokens
                .saturating_add(self.cached_tokens)
                .saturating_add(self.cache_creation_tokens)
        } else {
            self.prompt_tokens
        }
    }

    /// Normal, non-cached input tokens used for cache-aware pricing.
    pub fn cache_miss_tokens(&self) -> u32 {
        if self.cache_miss_tokens > 0 {
            return self.cache_miss_tokens;
        }
        match self.cache_accounting {
            CacheAccounting::Inclusive => self
                .prompt_tokens
                .saturating_sub(self.cached_tokens)
                .saturating_sub(self.cache_creation_tokens),
            CacheAccounting::Exclusive | CacheAccounting::Unknown => self.prompt_tokens,
        }
    }
}

/// Result of a live connectivity probe to a model endpoint. The top-right
/// status chip maps these to 就绪 / 已断开 / 未配置.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmConnectionStatus {
    /// Endpoint reachable (GET /models succeeded).
    Ready,
    /// Endpoint configured but unreachable (network/auth/server failure).
    Disconnected,
    /// No api_key configured for the role — no network probe was attempted.
    Unconfigured,
}

impl LlmConnectionStatus {
    /// Stable wire value used by the `check_llm_connection` Tauri command
    /// (`"ready"` / `"disconnected"` / `"unconfigured"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Disconnected => "disconnected",
            Self::Unconfigured => "unconfigured",
        }
    }
}

/// Non-sensitive classification for a failed connectivity probe. The raw
/// provider error stays in the backend log after sanitization; only this
/// stable category crosses the Tauri boundary so the UI can explain the
/// failure without exposing request details or credentials.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmConnectionFailureReason {
    Network,
    Timeout,
    Authentication,
    RateLimited,
    CircuitOpen,
    Server,
    RequestRejected,
    InvalidResponse,
    Configuration,
    Unknown,
}

impl LlmConnectionFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::Timeout => "timeout",
            Self::Authentication => "authentication",
            Self::RateLimited => "rate_limited",
            Self::CircuitOpen => "circuit_open",
            Self::Server => "server",
            Self::RequestRejected => "request_rejected",
            Self::InvalidResponse => "invalid_response",
            Self::Configuration => "configuration",
            Self::Unknown => "unknown",
        }
    }
}

/// Result returned by the live default-model connectivity probe.
///
/// `provider` and `model` are display metadata only. `reason` is omitted for
/// successful and unconfigured probes. No endpoint URL or provider response
/// body is included in this DTO.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmConnectionReport {
    pub status: LlmConnectionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<LlmConnectionFailureReason>,
    pub provider: String,
    pub model: String,
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

/// Live status of the provider's built-in web search tool (DeepSeek /
/// OpenAI Responses API). Forwarded to the UI so the user sees
/// "正在联网搜索…" while the search runs server-side.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchPhase {
    #[default]
    InProgress,
    Searching,
    Completed,
}

impl WebSearchPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            WebSearchPhase::InProgress => "in_progress",
            WebSearchPhase::Searching => "searching",
            WebSearchPhase::Completed => "completed",
        }
    }
}

/// One live web-search status update. DeepSeek can emit several
/// `web_search_call` items in a single turn (`search` → `open_page` →
/// `find_in_page`); `call_id` / `action` let the UI render each as its own
/// card instead of collapsing them into one indicator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebSearchUpdate {
    pub phase: WebSearchPhase,
    /// Provider item id (`ws_…` / `web_search_call` id). Optional when the
    /// SSE status event omitted `item_id` and no prior `output_item.added`
    /// was seen for this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// DeepSeek `action.type`: `search` / `open_page` / `find_in_page`.
    /// Often absent until `output_item.done` carries the full payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// Compact tool return `{queries, results:[{title,url,snippet}]}` captured
    /// from the completed `web_search_call` item (see
    /// [`crate::adapters::web_search_result_of`]). Present only when the
    /// provider returned citations / result content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
}

impl WebSearchUpdate {
    pub fn new(phase: WebSearchPhase) -> Self {
        Self {
            phase,
            call_id: None,
            action: None,
            result: None,
        }
    }

    pub fn with_meta(mut self, call_id: Option<String>, action: Option<String>) -> Self {
        self.call_id = call_id;
        self.action = action;
        self
    }

    pub fn with_result(mut self, result: Option<serde_json::Value>) -> Self {
        self.result = result;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmResponse {
    pub text: String,
    pub tool_calls: Vec<CanonicalToolCall>,
    pub finish_reason: Option<FinishReason>,
    pub usage: Usage,
    // §2.14: which model produced this response
    pub model: Option<String>,
    /// Internal reasoning/chain-of-thought produced by the model (e.g.
    /// DeepSeek-R1's reasoning_content, Claude's extended thinking).
    pub reasoning: Option<String>,
    /// Raw `web_search_call` output items (see
    /// [`haven_common::types::CanonicalMessage::web_search_calls`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub web_search_calls: Vec<serde_json::Value>,
    /// Provider-opaque thinking state (see
    /// [`haven_common::types::CanonicalMessage::thinking_blocks`]). Carried so
    /// adapters can echo signed/opaque provider state on later turns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thinking_blocks: Vec<serde_json::Value>,
}

/// A batch of text embeddings produced by the dedicated `embedding_model`
/// endpoint. `vectors[i]` corresponds to `input[i]` of the request.
#[derive(Debug, Clone)]
pub struct Embedding {
    pub vectors: Vec<Vec<f32>>,
    pub model: Option<String>,
    pub usage: Usage,
}

/// Owned input for one batch embedding request routed through [`crate::LlmRouter`].
#[derive(Debug, Clone)]
pub struct EmbeddingRequest {
    pub input: Vec<String>,
}

/// Owned input for a one-shot system/user prompt routed through
/// [`crate::LlmRouter`]. An empty `system_prompt` preserves the user-only
/// message behavior used by the prompt convenience API.
#[derive(Debug, Clone)]
pub struct PromptRequest {
    pub request: RequestKind,
    pub system_prompt: String,
    pub user_prompt: String,
}

impl PromptRequest {
    pub fn new(
        request: RequestKind,
        system_prompt: impl Into<String>,
        user_prompt: impl Into<String>,
    ) -> Self {
        Self {
            request,
            system_prompt: system_prompt.into(),
            user_prompt: user_prompt.into(),
        }
    }
}

/// Selects the configured request route for a router health check.
#[derive(Debug, Clone, Copy)]
pub struct HealthCheckRequest {
    pub request: RequestKind,
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

/// Borrowed input for one aggregated streaming request.
///
/// Cancellation and attempt callbacks are lifecycle controls supplied
/// separately to the router; this DTO contains only the routed request data.
#[derive(Debug, Clone, Copy)]
pub struct StreamRequest<'a> {
    pub request: RequestKind,
    pub messages: &'a [CanonicalMessage],
    pub tools: &'a [ToolDefinition],
    pub max_output_tokens: Option<u32>,
}

/// One complete, non-streaming request routed through [`crate::LlmRouter`].
/// Empty `tools` preserves the ordinary chat path; a non-empty list selects
/// the provider's tool-capable path.
#[derive(Debug, Clone)]
pub struct CompleteRequest {
    pub request: RequestKind,
    pub messages: Vec<CanonicalMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_output_tokens: Option<u32>,
}

impl CompleteRequest {
    /// Build an ordinary chat request with no explicit output cap.
    pub fn new(request: RequestKind, messages: Vec<CanonicalMessage>) -> Self {
        Self {
            request,
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
        }
    }

    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_max_output_tokens(mut self, max_output_tokens: u32) -> Self {
        self.max_output_tokens = Some(max_output_tokens);
        self
    }
}

/// Canonicalize JSON object keys recursively for cache identity and provider
/// wire stability. Object key order is not schema semantics; array order is
/// preserved because it can be meaningful in JSON Schema (`allOf`, `oneOf`,
/// `prefixItems`, and enum values).
pub fn canonicalize_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonicalize_json).collect()),
        Value::Object(object) => {
            let mut entries: Vec<_> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            let sorted: Map<String, Value> = entries
                .into_iter()
                .map(|(key, value)| (key, canonicalize_json(value)))
                .collect();
            Value::Object(sorted)
        }
        other => other,
    }
}

/// Serialize a JSON value with [`canonicalize_json`] ordering. This is used
/// only for cache identity; provider request serialization remains owned by
/// each adapter.
pub fn stable_json_bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(&canonicalize_json(value.clone())).unwrap_or_default()
}

/// Canonical tool definition → LLM-boundary tool definition. The agent and
/// providers consume the shared `haven_common::tools::ToolDef`; only at the
/// provider boundary is it expressed as the OpenAI-shaped `ToolDefinition`
/// each adapter converts to its own wire format.
impl From<haven_common::tools::ToolDef> for ToolDefinition {
    fn from(def: haven_common::tools::ToolDef) -> Self {
        ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: def.name,
                description: def.description,
                parameters: sanitize_tool_parameters(def.input_schema),
            },
        }
    }
}

/// Ensure tool `parameters` is a JSON Schema object acceptable to strict
/// providers (OpenAI Responses meta-schema: schema = boolean | object).
///
/// - Every object root is projected as `type: object`, because function-call
///   arguments are always objects (external MCP schemas may omit the keyword).
/// - Null / non-object roots become `{"type":"object","properties":{}}`.
/// - Keywords that must be boolean|object (`additionalProperties`,
///   `additionalItems`, `items`, `not`, `if`/`then`/`else`, …) drop `null`
///   (or coerce `additionalProperties`/`additionalItems` null → `false`).
pub fn sanitize_tool_parameters(schema: Value) -> Value {
    match schema {
        Value::Object(mut map) => {
            sanitize_schema_object(&mut map);
            map.insert("type".into(), Value::String("object".into()));
            Value::Object(map)
        }
        _ => serde_json::json!({"type": "object", "properties": {}}),
    }
}

fn sanitize_schema_value(value: &mut Value) {
    match value {
        Value::Object(map) => sanitize_schema_object(map),
        Value::Array(items) => {
            for item in items {
                sanitize_schema_value(item);
            }
        }
        _ => {}
    }
}

fn sanitize_schema_object(map: &mut serde_json::Map<String, Value>) {
    for key in ["additionalProperties", "additionalItems"] {
        if matches!(map.get(key), Some(Value::Null)) {
            map.insert(key.to_string(), Value::Bool(false));
        }
    }
    for key in [
        "items",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "propertyNames",
        "unevaluatedItems",
        "unevaluatedProperties",
    ] {
        match map.get(key) {
            Some(Value::Null) => {
                map.remove(key);
            }
            Some(_) => {
                if let Some(v) = map.get_mut(key) {
                    sanitize_schema_value(v);
                }
            }
            None => {}
        }
    }
    for key in [
        "properties",
        "patternProperties",
        "dependentSchemas",
        "$defs",
        "definitions",
    ] {
        if let Some(Value::Object(nested)) = map.get_mut(key) {
            for prop in nested.values_mut() {
                sanitize_schema_value(prop);
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(items)) = map.get_mut(key) {
            for item in items {
                sanitize_schema_value(item);
            }
        }
    }
}

/// Outcome of a speech-to-text call: the transcript plus an optional
/// confidence (0.0-1.0) reported by the provider. `None` confidence means
/// the provider does not report one; the gateway's confidence gate treats
/// that as "no signal" and falls back on error / empty text instead.
#[derive(Debug, Clone, Default)]
pub struct SttResult {
    pub text: String,
    pub confidence: Option<f32>,
    /// Provider-reported usage when transcription is implemented by a chat
    /// model. Native STT providers may leave this unset.
    pub usage: Option<Usage>,
    /// Model identifier for chat-based transcription when the provider
    /// returns one.
    pub model: Option<String>,
}

#[derive(Debug, Clone, Error)]
pub enum LlmError {
    /// Local provider configuration is invalid or could not be materialized.
    /// This is deliberately distinct from a request/response failure so the
    /// router cannot mistake a bad endpoint for an empty/default client.
    #[error("invalid LLM configuration: {0}")]
    Configuration(String),

    #[error("network timeout: {0}")]
    Timeout(String),

    #[error("network error: {0}")]
    Network(String),

    #[error("authentication failed: {0}")]
    Auth(String),

    #[error("rate limited by provider")]
    RateLimit {
        retry_after: Option<std::time::Duration>,
    },

    #[error("server error: {0}")]
    ServerError(String),

    #[error("server error: circuit breaker open for model {model_id}")]
    CircuitOpen { model_id: String },

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

    /// Adapter does not implement the requested capability (e.g. STT /
    /// embeddings). Callers may fall back to an alternate path.
    #[error("unsupported capability: {0}")]
    UnsupportedCapability(String),

    #[error("unknown error: {0}")]
    Unknown(String),

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
                | LlmError::Network(_)
                | LlmError::ServerError(_)
                | LlmError::RateLimit { .. }
                | LlmError::StreamTruncated
        )
    }

    /// Map a provider error to the stable category exposed by the
    /// connectivity probe. Request details remain available to backend
    /// logging through `Display`, but never need to cross the UI boundary.
    pub fn connection_failure_reason(&self) -> LlmConnectionFailureReason {
        match self {
            Self::Configuration(_) => LlmConnectionFailureReason::Configuration,
            Self::Timeout(_) => LlmConnectionFailureReason::Timeout,
            Self::Network(_) => LlmConnectionFailureReason::Network,
            Self::Auth(_) => LlmConnectionFailureReason::Authentication,
            Self::RateLimit { .. } => LlmConnectionFailureReason::RateLimited,
            Self::CircuitOpen { .. } => LlmConnectionFailureReason::CircuitOpen,
            Self::ServerError(_) => LlmConnectionFailureReason::Server,
            Self::RequestFailed(_) => LlmConnectionFailureReason::RequestRejected,
            Self::InvalidResponse(_) => LlmConnectionFailureReason::InvalidResponse,
            _ => LlmConnectionFailureReason::Unknown,
        }
    }

    /// True when the adapter lacks the requested capability (STT, embeddings,
    /// …). Distinct from a hard request failure so callers can fall back.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, LlmError::UnsupportedCapability(_))
    }
}

impl From<reqwest::Error> for LlmError {
    fn from(e: reqwest::Error) -> Self {
        let detail = error_chain(&e);
        if e.is_timeout() {
            LlmError::Timeout(detail)
        } else if e.is_connect() || e.is_body() || e.is_request() {
            LlmError::Network(detail)
        } else if let Some(status) = e.status() {
            if status.as_u16() == 401 || status.as_u16() == 403 {
                LlmError::Auth(detail)
            } else if status.as_u16() == 429 {
                LlmError::RateLimit { retry_after: None }
            } else if status.is_server_error() {
                LlmError::ServerError(detail)
            } else {
                LlmError::RequestFailed(detail)
            }
        } else {
            LlmError::Unknown(detail)
        }
    }
}

/// Keep the underlying transport cause when reqwest's top-level Display only
/// says "error sending request". This is used for backend diagnostics; callers
/// must sanitize it before logging or returning it across a UI boundary.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut parts = Vec::new();
    let mut current = Some(error);
    while let Some(error) = current {
        let text = redact_request_url(&error.to_string());
        if !text.is_empty() && parts.last() != Some(&text) {
            parts.push(text);
        }
        current = error.source();
    }
    parts.join(": ")
}

fn redact_request_url(text: &str) -> String {
    let Some(start) = text.find(" for url (") else {
        return text.to_string();
    };
    let Some(_) = text[start..].find(')') else {
        return text.to_string();
    };
    format!("{} [URL redacted]", &text[..start])
}

#[derive(Debug, Clone, Default)]
pub struct StreamChunk {
    pub text: Option<String>,
    pub tool_calls: Vec<CanonicalToolCall>,
    pub finish_reason: Option<FinishReason>,
    pub usage: Option<Usage>,
    pub model: Option<String>,
    /// Internal reasoning/chain-of-thought delta (e.g. DeepSeek-R1's
    /// reasoning_content, Claude's extended thinking).
    pub reasoning: Option<String>,
    /// Live web search status (in_progress → searching → completed). Set on
    /// the chunk matching the provider's stream event; the UI renders one
    /// card per `call_id` from it.
    pub web_search: Option<WebSearchUpdate>,
    /// Raw `web_search_call` items accumulated while streaming (see
    /// [`haven_common::types::CanonicalMessage::web_search_calls`]).
    pub web_search_calls: Vec<serde_json::Value>,
    /// Provider-opaque thinking state accumulated while streaming (see
    /// [`haven_common::types::CanonicalMessage::thinking_blocks`]). Emitted
    /// when the provider requires it to be echoed on a later turn.
    pub thinking_blocks: Vec<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_diagnostics_distinguish_missing_usage_from_explicit_zero() {
        let base = CacheDiagnostics::for_request(true, false);
        let unavailable = base.clone().with_provider_usage(None, false);
        assert_eq!(unavailable.outcome, "unknown");
        assert_eq!(unavailable.usage_source, "unavailable");

        let zero = base.with_provider_usage(Some(0), true);
        assert_eq!(zero.outcome, "miss");
        assert_eq!(zero.usage_source, "provider");

        let hit = CacheDiagnostics::for_provider_cache(false).with_provider_usage(Some(8), true);
        assert_eq!(hit.outcome, "hit");
        assert_eq!(hit.usage_source, "provider");

        let off_but_reported =
            CacheDiagnostics::for_request(false, false).with_provider_usage(Some(8), true);
        assert_eq!(off_but_reported.outcome, "hit");
        assert_eq!(off_but_reported.usage_source, "provider");

        let disabled_without_usage =
            CacheDiagnostics::for_request(false, false).with_provider_usage(None, false);
        assert_eq!(disabled_without_usage.outcome, "disabled");
    }

    #[test]
    fn old_cache_diagnostics_deserialize_with_missing_metadata() {
        let diagnostics: CacheDiagnostics = serde_json::from_str(
            r#"{"mode":"key","key_requested":true,"system_split":false,"downgraded":false,"outcome":"unknown"}"#,
        )
        .unwrap();
        assert_eq!(diagnostics.provider, "");
        assert_eq!(diagnostics.usage_source, "unavailable");
    }
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
    fn usage_default_values_are_zero() {
        let u = Usage::default();
        assert_eq!(u.prompt_tokens, 0);
        assert_eq!(u.completion_tokens, 0);
        assert_eq!(u.total_tokens, 0);
        assert_eq!(u.cached_tokens, 0);
        assert_eq!(u.cache_creation_tokens, 0);
        assert_eq!(u.cache_accounting, CacheAccounting::Unknown);
        assert!(u.model_name.is_none());
        assert!(u.cost.is_none());
    }

    #[test]
    fn usage_normalize_fills_omitted_total_inclusive() {
        let u =
            Usage::from_counts_with_accounting(100, 20, 0, 80, 0, CacheAccounting::Inclusive, None);
        assert_eq!(u.total_tokens, 120);
        assert!(!u.cache_exclusive_of_prompt());
        assert_eq!(u.context_tokens(), 100);
    }

    #[test]
    fn usage_normalize_fills_omitted_total_exclusive_cache() {
        let u = Usage::from_counts_with_accounting(
            100,
            20,
            0,
            400,
            50,
            CacheAccounting::Exclusive,
            None,
        );
        assert_eq!(u.total_tokens, 570);
        assert!(u.cache_exclusive_of_prompt());
        assert_eq!(u.context_tokens(), 550);
    }

    #[test]
    fn usage_cache_miss_uses_explicit_provider_value() {
        let mut usage = Usage::from_counts_with_accounting(
            100,
            10,
            110,
            70,
            10,
            CacheAccounting::Inclusive,
            None,
        );
        usage.cache_miss_tokens = 24;
        assert_eq!(usage.cache_miss_tokens(), 24);
    }

    #[test]
    fn usage_normalize_keeps_provider_total() {
        let u = Usage::from_counts(100, 20, 125, 80, 0, None);
        assert_eq!(u.total_tokens, 125);
        assert_eq!(u.context_tokens(), 100);
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
    fn connection_failure_reason_preserves_transport_categories() {
        assert_eq!(
            LlmError::Network("dns failed".into()).connection_failure_reason(),
            LlmConnectionFailureReason::Network
        );
        assert_eq!(
            LlmError::Timeout("deadline".into()).connection_failure_reason(),
            LlmConnectionFailureReason::Timeout
        );
        assert_eq!(
            LlmError::Auth("unauthorized".into()).connection_failure_reason(),
            LlmConnectionFailureReason::Authentication
        );
        assert_eq!(
            LlmError::RequestFailed("bad request".into()).connection_failure_reason(),
            LlmConnectionFailureReason::RequestRejected
        );
        let circuit_open = LlmError::CircuitOpen {
            model_id: "default_model".into(),
        };
        assert_eq!(
            circuit_open.connection_failure_reason(),
            LlmConnectionFailureReason::CircuitOpen
        );
        assert!(!circuit_open.is_retryable());
    }

    #[test]
    fn connection_report_serializes_without_reason_when_ready() {
        let report = LlmConnectionReport {
            status: LlmConnectionStatus::Ready,
            reason: None,
            provider: "PackyAPI".into(),
            model: "grok-4.6".into(),
        };
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["status"], "ready");
        assert_eq!(json["provider"], "PackyAPI");
        assert_eq!(json["model"], "grok-4.6");
        assert!(json.get("reason").is_none());
        assert!(json.get("endpoint").is_none());
    }

    #[test]
    fn error_chain_redacts_request_url() {
        assert_eq!(
            redact_request_url("error sending request for url (https://example.test/v1/models)"),
            "error sending request [URL redacted]"
        );
        assert_eq!(
            redact_request_url("dns error: host not found"),
            "dns error: host not found"
        );
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
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        assert_eq!(chunk.text, Some("delta".into()));
        assert!(chunk.tool_calls.is_empty());
    }

    #[test]
    fn stream_chunk_construction_with_tool_calls_and_finish_reason() {
        let chunk = StreamChunk {
            text: None,
            tool_calls: vec![CanonicalToolCall {
                id: "tc1".into(),
                name: "shell".into(),
                arguments: serde_json::json!({}),
            }],
            finish_reason: Some(FinishReason::ToolCalls),
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                ..Default::default()
            }),
            model: Some("gpt-4o".into()),
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
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
    fn tool_definition_from_tool_def() {
        let def = haven_common::tools::ToolDef::new(
            "files",
            "Read and write files",
            serde_json::json!({"type": "object"}),
            haven_common::types::RiskLevel::Safe,
        );
        let td = ToolDefinition::from(def);
        assert_eq!(td.tool_type, "function");
        assert_eq!(td.function.name, "files");
        assert_eq!(td.function.description, "Read and write files");
        assert_eq!(
            td.function.parameters,
            serde_json::json!({"type": "object"})
        );
    }

    #[test]
    fn sanitize_tool_parameters_replaces_null_root() {
        let cleaned = sanitize_tool_parameters(Value::Null);
        assert_eq!(cleaned["type"], "object");
        assert!(cleaned["properties"].is_object());
    }

    #[test]
    fn sanitize_tool_parameters_adds_object_type_to_mcp_schema_without_one() {
        let cleaned = sanitize_tool_parameters(serde_json::json!({
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }));

        assert_eq!(cleaned["type"], "object");
        assert_eq!(cleaned["properties"]["path"]["type"], "string");
        assert_eq!(cleaned["required"], serde_json::json!(["path"]));
    }

    #[test]
    fn sanitize_tool_parameters_coerces_additional_properties_null() {
        let cleaned = sanitize_tool_parameters(serde_json::json!({
            "type": "object",
            "properties": {
                "cwd": {
                    "type": "string",
                    "additionalProperties": null
                }
            },
            "additionalProperties": null
        }));
        assert_eq!(cleaned["additionalProperties"], false);
        assert_eq!(cleaned["properties"]["cwd"]["additionalProperties"], false);
    }

    #[test]
    fn tool_definition_from_null_schema_is_object() {
        let def = haven_common::tools::ToolDef::new(
            "mcp_broken",
            "schema was null",
            Value::Null,
            haven_common::types::RiskLevel::High,
        );
        let td = ToolDefinition::from(def);
        assert!(td.function.parameters.is_object());
        assert!(!td.function.parameters.is_null());
    }

    #[test]
    fn stable_json_sorts_objects_but_preserves_schema_arrays() {
        let value = serde_json::json!({
            "z": {"b": 1, "a": 2},
            "a": [{"d": 4, "c": 3}, "keep-order"]
        });
        assert_eq!(
            String::from_utf8(stable_json_bytes(&value)).unwrap(),
            r#"{"a":[{"c":3,"d":4},"keep-order"],"z":{"a":2,"b":1}}"#
        );
    }
}
