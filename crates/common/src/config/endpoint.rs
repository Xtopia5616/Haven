//! LLM endpoint / provider / model / request-policy configuration:
//! [`ModelEndpoint`], [`ProviderConfig`], [`ModelConfig`], [`RequestPolicy`],
//! [`LlmConfig`], and [`RouterConfig`].

use super::*;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelEndpoint {
    pub provider: String,
    /// Wire protocol style for this endpoint. One of:
    /// - `openai-chat` (default): OpenAI `/chat/completions` compatible
    ///   (also Ollama, vLLM, DeepSeek chat, and most gateways)
    /// - `llama.cpp`: llama.cpp server (OpenAI-compatible `/chat/completions`)
    /// - `openai-responses`: OpenAI / DeepSeek Responses API (`/v1/responses`)
    /// - `xai`: xAI Grok chat + Live Search (`search_parameters`)
    /// - `anthropic`: Anthropic Messages API (`/v1/messages`)
    /// - `gemini`: Google Gemini `generateContent` / `streamGenerateContent`
    /// - `deepgram` / `assemblyai`: speech-to-text only
    ///
    /// When empty/`None`, the endpoint uses the neutral `openai-chat` default.
    /// Vendor identity never selects a wire protocol implicitly.
    #[serde(default)]
    pub api_style: Option<String>,
    pub base_url: String,
    pub api_key: String,
    pub model_name: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub timeout_secs: u64,
    // §2.8: additional model parameters
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<u64>,
    pub response_format: Option<serde_json::Value>,
    // §2.5: proxy support
    pub proxy_url: Option<String>,
    pub no_proxy: Option<String>,
    // §2.15: auth header customization
    #[serde(default = "default_auth_header_name")]
    pub auth_header_name: String,
    #[serde(default = "default_auth_header_prefix")]
    pub auth_header_prefix: String,
    // §2.9: streaming timeout (None = no timeout until SSE ends)
    pub timeout_streaming_secs: Option<u64>,
    // §2.8: reasoning / thinking intensity from the chat UI ("low" | "medium" |
    // "high", plus "none"/"off"/"disabled" to turn thinking off). Chat adapter
    // forwards it as OpenAI `reasoning_effort` and, for DeepSeek/Kimi, also as
    // vendor `thinking` extras; Responses adapter maps it to `reasoning.effort`.
    pub reasoning_effort: Option<String>,
    /// Provider built-in web search mode for Responses-API endpoints
    /// (DeepSeek etc.): `"off"` | `"auto"` | `"always"`. `None` defers to the
    /// `HAVEN_WEB_SEARCH` environment variable, then defaults to `off`
    /// (web search is opt-in).
    #[serde(default)]
    pub web_search: Option<String>,
    // §3.16: cost tracking. USD per 1K tokens (input and output). When both
    // are zero, cost is reported as None.
    pub cost_per_1k_input_tokens: f64,
    pub cost_per_1k_output_tokens: f64,
    /// Optional discounted cache-read price. Falls back to normal input price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_cache_read_tokens: Option<f64>,
    /// Optional cache-write/creation price. Falls back to normal input price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_cache_write_tokens: Option<f64>,
    /// True context window of the model in tokens. When unset (None), Haven
    /// falls back to `context_limits.default_context_window`. Prefer filling
    /// this from provider `/models` metadata when the user picks a model.
    /// Used to drive context compaction and the token-usage display.
    #[serde(default)]
    pub context_window: Option<u32>,
    /// Per-endpoint override for the reasoning-echo cap (chars) sent back to
    /// OpenAI-compatible providers. `None` inherits the global
    /// `context_limits.reasoning_echo_max_chars` at router build time (see
    /// [`LlmConfig::with_reasoning_echo_cap`]). Kept out of the on-disk
    /// config when unset so the global default stays the single knob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_echo_max_chars: Option<usize>,
}

fn default_auth_header_name() -> String {
    "Authorization".into()
}

fn default_auth_header_prefix() -> String {
    "Bearer".into()
}

/// Compute USD cost for the given token counts using this endpoint's pricing.
/// Returns `None` when both pricing fields are zero (cost not configured).
pub fn compute_cost_usd(
    endpoint: &ModelEndpoint,
    cache_miss_tokens: u32,
    cached_tokens: u32,
    cache_creation_tokens: u32,
    completion_tokens: u32,
) -> Option<f64> {
    if endpoint.cost_per_1k_input_tokens <= 0.0
        && endpoint.cost_per_1k_output_tokens <= 0.0
        && endpoint.cost_per_1k_cache_read_tokens.unwrap_or(0.0) <= 0.0
        && endpoint.cost_per_1k_cache_write_tokens.unwrap_or(0.0) <= 0.0
    {
        return None;
    }
    let input = (cache_miss_tokens as f64 / 1000.0) * endpoint.cost_per_1k_input_tokens;
    let cache_read = (cached_tokens as f64 / 1000.0)
        * endpoint
            .cost_per_1k_cache_read_tokens
            .unwrap_or(endpoint.cost_per_1k_input_tokens);
    let cache_write = (cache_creation_tokens as f64 / 1000.0)
        * endpoint
            .cost_per_1k_cache_write_tokens
            .unwrap_or(endpoint.cost_per_1k_input_tokens);
    let output = (completion_tokens as f64 / 1000.0) * endpoint.cost_per_1k_output_tokens;
    Some(input + cache_read + cache_write + output)
}

impl Default for ModelEndpoint {
    fn default() -> Self {
        Self {
            provider: "openai".into(),
            api_style: None,
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            model_name: "gpt-4o-mini".into(),
            max_tokens: 8192,
            temperature: 0.7,
            timeout_secs: 7,
            top_p: None,
            top_k: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            seed: None,
            response_format: None,
            proxy_url: None,
            no_proxy: None,
            auth_header_name: default_auth_header_name(),
            auth_header_prefix: default_auth_header_prefix(),
            timeout_streaming_secs: None,
            reasoning_effort: None,
            web_search: None,
            cost_per_1k_input_tokens: 0.0,
            cost_per_1k_output_tokens: 0.0,
            cost_per_1k_cache_read_tokens: None,
            cost_per_1k_cache_write_tokens: None,
            context_window: None,
            reasoning_echo_max_chars: None,
        }
    }
}

/// A configured LLM provider: connection-level endpoint definition identified
/// by `name`. The model library is no longer a manually-maintained list — it
/// is the union of each provider's `/models` fetch. Named model assignments
/// reference a provider by name and pick a model id from that provider's
/// fetched list; request policies select among those assignments.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProviderConfig {
    /// Unique id referenced by [`ModelConfig::provider`] and the settings UI.
    pub name: String,
    /// Vendor identity used for provider-specific capabilities and display.
    #[serde(default)]
    pub provider: String,
    /// Explicit wire protocol style, mirroring [`ModelEndpoint::api_style`].
    /// An empty value means the neutral `openai-chat` protocol; it is never
    /// inferred from the vendor identity.
    #[serde(default)]
    pub api_style: Option<String>,
    pub base_url: String,
    pub api_key: String,
    // §2.15: auth header customization
    #[serde(default = "default_auth_header_name")]
    pub auth_header_name: String,
    #[serde(default = "default_auth_header_prefix")]
    pub auth_header_prefix: String,
    // §2.5: proxy support
    #[serde(default)]
    pub proxy_url: Option<String>,
    #[serde(default)]
    pub no_proxy: Option<String>,
    // —— optional per-provider defaults adopted by models without overrides ——
    /// Default per-response token cap for models on this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u32>,
    /// Default sampling temperature for models on this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_temperature: Option<f32>,
    /// Default first-response timeout (secs) for models on this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_timeout_secs: Option<u64>,
    /// Default streaming idle timeout (secs); `None` = no per-provider
    /// override (the router's global `stream_idle_timeout_secs` applies).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_timeout_streaming_secs: Option<u64>,
    /// Default provider built-in web search mode (`off`/`auto`/`always`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_web_search: Option<String>,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: "openai".into(),
            api_style: None,
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            auth_header_name: default_auth_header_name(),
            auth_header_prefix: default_auth_header_prefix(),
            proxy_url: None,
            no_proxy: None,
            default_max_tokens: None,
            default_temperature: None,
            default_timeout_secs: None,
            default_timeout_streaming_secs: None,
            default_web_search: None,
        }
    }
}

/// A capability a configured model advertises to the request router.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Chat,
    FastChat,
    Vision,
    AudioInput,
    Transcription,
    Embedding,
    ImageGeneration,
    SpeechSynthesis,
}

impl Capability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::FastChat => "fast_chat",
            Self::Vision => "vision",
            Self::AudioInput => "audio_input",
            Self::Transcription => "transcription",
            Self::Embedding => "embedding",
            Self::ImageGeneration => "image_generation",
            Self::SpeechSynthesis => "speech_synthesis",
        }
    }
}

/// A logical request made by Haven. Request kinds are separate from provider
/// wire styles and model identities, so a new request does not need a new
/// fixed endpoint slot.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Chat,
    FastChat,
    Vision,
    AudioChat,
    Transcription,
    Embedding,
    ImageGeneration,
    SpeechSynthesis,
}

impl RequestKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::FastChat => "fast_chat",
            Self::Vision => "vision",
            Self::AudioChat => "audio_chat",
            Self::Transcription => "transcription",
            Self::Embedding => "embedding",
            Self::ImageGeneration => "image_generation",
            Self::SpeechSynthesis => "speech_synthesis",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "chat" => Self::Chat,
            "fast_chat" => Self::FastChat,
            "vision" => Self::Vision,
            "audio_chat" => Self::AudioChat,
            "transcription" => Self::Transcription,
            "embedding" => Self::Embedding,
            "image_generation" => Self::ImageGeneration,
            "speech_synthesis" => Self::SpeechSynthesis,
            _ => return None,
        })
    }

    pub const fn required_capability(self) -> Capability {
        match self {
            Self::Chat => Capability::Chat,
            Self::FastChat => Capability::FastChat,
            Self::Vision => Capability::Vision,
            Self::AudioChat => Capability::AudioInput,
            Self::Transcription => Capability::Transcription,
            Self::Embedding => Capability::Embedding,
            Self::ImageGeneration => Capability::ImageGeneration,
            Self::SpeechSynthesis => Capability::SpeechSynthesis,
        }
    }
}

/// Explicit routing policy for a logical request.
///
/// A request has exactly one model assignment. Provider/model failover was
/// intentionally removed because changing the endpoint changes the provider
/// request and its cache namespace; transient failures are retried by the
/// router on this same endpoint instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RequestPolicy {
    pub request: RequestKind,
    pub primary: String,
}

impl Default for RequestPolicy {
    fn default() -> Self {
        Self {
            request: RequestKind::Chat,
            primary: String::new(),
        }
    }
}

/// Named model assignment. A model may advertise multiple capabilities and
/// can therefore serve several request kinds without being copied into slots.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelConfig {
    /// Stable user-chosen identity referenced by [`RequestPolicy`].
    pub id: String,
    /// In-memory compatibility spelling for old callers. It is never written
    /// to the new `[[llm.models]]` shape.
    #[serde(skip)]
    pub role: String,
    /// Referenced provider name (empty = model unconfigured).
    pub provider: String,
    /// Model id on that provider.
    pub model: String,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_cache_read_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_cache_write_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_echo_max_chars: Option<usize>,
}

impl ModelConfig {
    pub fn stamp_id(&mut self, id: &str) {
        self.id = id.to_string();
        self.role = id.to_string();
    }

    pub fn is_assigned(&self) -> bool {
        !self.provider.is_empty() && !self.model.is_empty()
    }
}

/// Model→(provider, model) assignment. New
/// configuration uses [`ModelConfig`] plus [`RequestPolicy`].
/// The transient `role` field only keeps old in-process callers source-compatible;
/// it is never persisted. `provider` names a [`ProviderConfig`]; `model` is
/// a model id on that provider. All tuning fields are optional overrides:
/// `None` falls back to the provider default, then
/// `context_limits.default_context_window` / [`ModelEndpoint`] built-ins.
/// An empty `provider` means the role is unconfigured.
/// Source-compatibility alias for code that only needs the model tuning
/// fields. It is not used by the persisted configuration shape.
pub type RoleConfig = ModelConfig;

/// Legacy request selectors accepted at the router and IPC boundaries while
/// callers migrate to [`RequestKind`]. These values are not persisted and do
/// not define the model configuration shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointRole {
    SmallModel,
    DefaultModel,
    ImageModel,
    AudioModel,
    EmbeddingModel,
}

impl EndpointRole {
    /// Canonical string identifier used in TOML, the frontend protocol, and
    /// the model commands. Single source of truth for the role name mapping.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SmallModel => "small_model",
            Self::DefaultModel => "default_model",
            Self::ImageModel => "image_model",
            Self::AudioModel => "audio_model",
            Self::EmbeddingModel => "embedding_model",
        }
    }

    /// Inverse of [`Self::as_str`]. Returns `None` for unknown role strings
    /// so callers can validate input from the frontend/CLI.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "small_model" => Self::SmallModel,
            "default_model" => Self::DefaultModel,
            "image_model" => Self::ImageModel,
            "audio_model" => Self::AudioModel,
            "embedding_model" => Self::EmbeddingModel,
            _ => return None,
        })
    }

    /// All variants in their canonical order. Useful for iterating every
    /// endpoint slot without duplicating the list at call sites.
    pub const ALL: &'static [EndpointRole] = &[
        Self::SmallModel,
        Self::DefaultModel,
        Self::ImageModel,
        Self::AudioModel,
        Self::EmbeddingModel,
    ];

    pub const fn request_kind(self) -> RequestKind {
        match self {
            Self::SmallModel => RequestKind::FastChat,
            Self::DefaultModel => RequestKind::Chat,
            Self::ImageModel => RequestKind::Vision,
            // The legacy audio selector represents audio-input chat. Native
            // transcription now uses `RequestKind::Transcription` directly;
            // the adapter still decides whether native transcription is
            // supported by this endpoint.
            Self::AudioModel => RequestKind::AudioChat,
            Self::EmbeddingModel => RequestKind::Embedding,
        }
    }
}

/// A materialized model plus its declared request capabilities.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoutedModel {
    pub id: String,
    pub endpoint: ModelEndpoint,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmConfig {
    /// Configured providers — connection-level endpoint definitions.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Named model assignments. A model may advertise multiple capabilities.
    #[serde(default)]
    pub models: Vec<ModelConfig>,
    /// Explicit request policies. Each policy names exactly one model.
    #[serde(default)]
    pub request_policies: Vec<RequestPolicy>,
    // §2.12: router-level total timeout
    pub max_total_duration_secs: u64,
    /// Streaming idle timeout: a stream that delivers no chunk for this long
    /// (headers received, body stalled) is aborted as a timeout instead of
    /// blocking until `max_total_duration_secs`. Providers occasionally hang
    /// with the connection half-open; without this the UI waits minutes for
    /// a reply that never comes. The router gives the FIRST chunk a longer
    /// grace (provider-side "thinking" delays it), so this value only bounds
    /// data gaps after the stream started flowing. The effective window is
    /// scaled UP with the request's prompt size (long contexts make
    /// provider-side gaps slower), capped at 90s — see the router's
    /// `scale_stream_idle`.
    pub stream_idle_timeout_secs: u64,
    // §2.3/5.1: retry backoff parameters
    /// Retry attempts for the selected endpoint after the initial request.
    /// Only transient provider failures (timeouts, rate limits, and 5xxs) are
    /// retried; invalid requests, authentication, billing, and cancellation
    /// always fail immediately.
    pub retry_max_retries: u32,
    pub retry_base_secs: u64,
    pub retry_factor: u32,
    pub retry_max_secs: u64,
    pub retry_jitter: f32,
    /// Per-endpoint (role) cap on concurrent LLM requests, applied by the
    /// router with a semaphore per role. Prevents N parallel sessions from
    /// hammering the same provider simultaneously (thundering-herd retries on
    /// 429). A session whose LLM call is queued behind this limit waits; its
    /// slot in `session.max_concurrent` is still held, so set it below the session
    /// concurrency when the provider is rate-limit sensitive.
    pub max_concurrent_requests: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            models: Vec::new(),
            request_policies: Vec::new(),
            max_total_duration_secs: 600,
            stream_idle_timeout_secs: 20,
            retry_max_retries: 2,
            retry_base_secs: 2,
            retry_factor: 2,
            retry_max_secs: 30,
            retry_jitter: 0.2,
            max_concurrent_requests: 2,
        }
    }
}

impl LlmConfig {
    pub fn model(&self, id: &str) -> Option<&ModelConfig> {
        self.models
            .iter()
            .find(|model| model.id == id || model.role == id)
    }

    pub fn model_mut(&mut self, id: &str) -> Option<&mut ModelConfig> {
        self.models
            .iter_mut()
            .find(|model| model.id == id || model.role == id)
    }

    /// Compatibility lookup for callers that still use the legacy selector.
    /// It resolves through the request policy before falling back to the
    /// legacy selector spelling as a model id.
    pub fn role(&self, role: EndpointRole) -> Option<&ModelConfig> {
        let id = self
            .policy(role.request_kind())
            .map(|policy| policy.primary.as_str())
            .unwrap_or_else(|| role.as_str());
        self.model(id)
    }

    pub fn role_mut(&mut self, role: EndpointRole) -> Option<&mut ModelConfig> {
        let id = self
            .policy(role.request_kind())
            .map(|policy| policy.primary.clone())
            .unwrap_or_else(|| role.as_str().to_string());
        self.model_mut(&id)
    }

    /// Compatibility writer. New code should use [`Self::set_model`] and
    /// [`Self::set_policy`] explicitly.
    pub fn set_role(&mut self, role: EndpointRole, mut config: ModelConfig) {
        let id = role.as_str();
        if config.capabilities.is_empty() {
            config.capabilities = vec![role.request_kind().required_capability()];
        }
        self.set_model(id, config);
        self.set_policy(role.request_kind(), id);
    }

    pub fn set_model(&mut self, id: impl Into<String>, mut config: ModelConfig) {
        let id = id.into();
        match self.models.iter_mut().find(|model| model.id == id) {
            Some(existing) => {
                config.stamp_id(&id);
                *existing = config;
            }
            None => {
                config.stamp_id(&id);
                self.models.push(config);
            }
        }
    }

    pub fn policy(&self, request: RequestKind) -> Option<&RequestPolicy> {
        self.request_policies
            .iter()
            .find(|policy| policy.request == request)
    }

    pub fn policy_mut(&mut self, request: RequestKind) -> Option<&mut RequestPolicy> {
        self.request_policies
            .iter_mut()
            .find(|policy| policy.request == request)
    }

    pub fn set_policy(&mut self, request: RequestKind, primary: impl Into<String>) {
        let policy = RequestPolicy {
            request,
            primary: primary.into(),
        };
        if let Some(existing) = self.policy_mut(request) {
            *existing = policy;
        } else {
            self.request_policies.push(policy);
        }
    }

    /// Resolve a request through its explicit policy, rejecting incomplete
    /// models and models that do not advertise the required capability.
    pub fn route_model(&self, request: RequestKind) -> Option<&ModelConfig> {
        let policy = self.policy(request)?;
        let model = self.model(&policy.primary)?;
        (model.is_assigned()
            && self
                .provider(model.provider.as_str())
                .is_some_and(provider_credentials_ready)
            && model.capabilities.contains(&request.required_capability()))
        .then_some(model)
    }

    pub fn is_request_configured(&self, request: RequestKind) -> bool {
        self.route_model(request).is_some()
    }

    /// Look up a provider by name.
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// True when a role is usable at runtime: it references a configured
    /// provider (API key present, or a keyless local server) and names a
    /// model. Used by tools that should no-op gracefully when an endpoint is
    /// not set up.
    pub fn is_configured(&self, role: EndpointRole) -> bool {
        self.is_request_configured(role.request_kind())
    }

    /// Materialize a named model from its provider and per-model overrides.
    /// Missing or incomplete models become a no-key endpoint so router
    /// construction remains non-panicking and the route is simply ineligible.
    pub fn materialize_model(&self, id: &str) -> RoutedModel {
        let mut ep = ModelEndpoint::default();
        let Some(model) = self.model(id) else {
            return RoutedModel {
                id: id.to_string(),
                endpoint: ep,
                capabilities: Vec::new(),
            };
        };
        if !model.is_assigned() {
            return RoutedModel {
                id: model.id.clone(),
                endpoint: ep,
                capabilities: model.capabilities.clone(),
            };
        }
        let Some(p) = self.provider(model.provider.as_str()) else {
            return RoutedModel {
                id: model.id.clone(),
                endpoint: ep,
                capabilities: model.capabilities.clone(),
            };
        };
        ep.model_name = model.model.clone();
        ep.api_key = p.api_key.clone();
        ep.api_style = p.api_style.clone();
        ep.provider = p.provider.clone();
        ep.base_url = p.base_url.clone();
        ep.auth_header_name = p.auth_header_name.clone();
        ep.auth_header_prefix = p.auth_header_prefix.clone();
        ep.proxy_url = p.proxy_url.clone();
        ep.no_proxy = p.no_proxy.clone();
        ep.max_tokens = p.default_max_tokens.unwrap_or(ep.max_tokens);
        ep.temperature = p.default_temperature.unwrap_or(ep.temperature);
        ep.timeout_secs = p.default_timeout_secs.unwrap_or(ep.timeout_secs);
        ep.timeout_streaming_secs = p.default_timeout_streaming_secs;
        ep.web_search = p.default_web_search.clone();
        // Per-model overrides win over provider defaults.
        if let Some(t) = model.temperature {
            ep.temperature = t;
        }
        if let Some(c) = model.context_window {
            ep.context_window = Some(c);
        }
        if let Some(c) = model.cost_per_1k_input_tokens {
            ep.cost_per_1k_input_tokens = c;
        }
        if let Some(c) = model.cost_per_1k_output_tokens {
            ep.cost_per_1k_output_tokens = c;
        }
        ep.cost_per_1k_cache_read_tokens = model.cost_per_1k_cache_read_tokens;
        ep.cost_per_1k_cache_write_tokens = model.cost_per_1k_cache_write_tokens;
        if let Some(m) = model.max_tokens {
            ep.max_tokens = m;
        }
        if let Some(r) = &model.reasoning_effort {
            ep.reasoning_effort = Some(r.clone());
        }
        if let Some(w) = &model.web_search {
            ep.web_search = Some(w.clone());
        }
        if let Some(r) = model.reasoning_echo_max_chars {
            ep.reasoning_echo_max_chars = Some(r);
        }
        // Sticky role/provider `web_search` must not reshape requests for
        // styles without a built-in search tool (e.g. openai-chat leftovers
        // from when the chat UI had no capability gate).
        let style = ep
            .api_style
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("openai-chat");
        if !supports_builtin_web_search(style) {
            ep.web_search = None;
        }
        RoutedModel {
            id: model.id.clone(),
            endpoint: ep,
            capabilities: model.capabilities.clone(),
        }
    }

    /// Compatibility helper for callers that still use the legacy selector.
    /// The request policy, rather than the selector name, chooses the model.
    pub fn materialize_endpoint(&self, role: EndpointRole) -> ModelEndpoint {
        self.route_model(role.request_kind())
            .map(|model| {
                self.materialize_model(if model.id.is_empty() {
                    &model.role
                } else {
                    &model.id
                })
                .endpoint
            })
            .unwrap_or_default()
    }

    /// Build the fully materialized router configuration: a dynamic model
    /// registry plus request policies and every router-level tuning knob. Called whenever the router is
    /// constructed or hot-swapped. `response_cap` / `reasoning_echo_cap`
    /// mirror the `with_response_cap` / `with_reasoning_echo_cap` transforms
    /// (applied to the materialized endpoints so hand-edited
    /// per-role overrides are still respected).
    pub fn materialize(
        &self,
        response_cap: Option<u32>,
        reasoning_echo_cap: Option<usize>,
    ) -> RouterConfig {
        let mut cap = RouterConfig {
            models: self
                .models
                .iter()
                .map(|model| self.materialize_model(&model.id))
                .collect(),
            request_policies: self.request_policies.clone(),
            max_total_duration_secs: self.max_total_duration_secs,
            stream_idle_timeout_secs: self.stream_idle_timeout_secs,
            retry_max_retries: self.retry_max_retries,
            retry_base_secs: self.retry_base_secs,
            retry_factor: self.retry_factor,
            retry_max_secs: self.retry_max_secs,
            retry_jitter: self.retry_jitter,
            max_concurrent_requests: self.max_concurrent_requests,
        };
        cap.apply_caps(response_cap, reasoning_echo_cap);
        cap
    }
}

/// True when a provider has usable credentials: a non-empty API key, or a
/// keyless local server (llama.cpp / Ollama).
pub fn provider_credentials_ready(p: &ProviderConfig) -> bool {
    if !p.api_key.is_empty() {
        return true;
    }
    let style = provider_config_wire_style(p);
    style == "llama.cpp"
        || p.provider.eq_ignore_ascii_case("ollama")
        || p.provider.eq_ignore_ascii_case("llama.cpp")
}

/// True when a materialized endpoint has usable credentials (same keyless
/// rules as [`provider_credentials_ready`]).
pub fn endpoint_credentials_ready(ep: &ModelEndpoint) -> bool {
    if !ep.api_key.is_empty() {
        return true;
    }
    let style = ep
        .api_style
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_api_style)
        .unwrap_or("openai-chat");
    style == "llama.cpp"
        || ep.provider.eq_ignore_ascii_case("ollama")
        || ep.provider.eq_ignore_ascii_case("llama.cpp")
}

/// Canonical wire-protocol id used for invalid-style detection. An empty style
/// is handled by the caller as the neutral `openai-chat` default; non-empty
/// unknown values become `invalid` and are never routed to another adapter.
pub fn normalize_api_style(style: &str) -> &'static str {
    match style.trim().to_ascii_lowercase().as_str() {
        "openai-responses" => "openai-responses",
        "openai-chat" => "openai-chat",
        "llama.cpp" => "llama.cpp",
        "xai" => "xai",
        "anthropic" => "anthropic",
        "gemini" => "gemini",
        "deepgram" => "deepgram",
        "assemblyai" => "assemblyai",
        "elevenlabs" => "elevenlabs",
        _ => "invalid",
    }
}

/// True when `style` is a canonical wire-protocol id (not a typo or alias).
pub fn is_known_api_style(style: &str) -> bool {
    matches!(
        style.trim().to_ascii_lowercase().as_str(),
        "openai-responses"
            | "openai-chat"
            | "llama.cpp"
            | "xai"
            | "anthropic"
            | "gemini"
            | "deepgram"
            | "assemblyai"
            | "elevenlabs"
    )
}

/// OpenAI-compatible family used by STT / TTS / image-gen allowlists.
pub fn is_openai_family_wire_style(style: &str) -> bool {
    matches!(
        normalize_api_style(style),
        "openai-chat" | "openai-responses" | "llama.cpp" | "xai"
    )
}

/// True when the style is TTS-only (no chat / STT / image gen).
pub fn is_tts_only_style(style: &str) -> bool {
    normalize_api_style(style) == "elevenlabs"
}

/// Effective wire style for a [`ProviderConfig`]. An omitted style is the
/// neutral OpenAI-compatible protocol; vendor identity is not consulted.
pub fn provider_config_wire_style(p: &ProviderConfig) -> &'static str {
    p.api_style
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(normalize_api_style)
        .unwrap_or("openai-chat")
}

/// True when the wire style can drive a provider built-in web search tool from
/// the named model's `off|auto|always` mode.
pub fn supports_builtin_web_search(style: &str) -> bool {
    matches!(
        normalize_api_style(style),
        "openai-responses" | "xai" | "anthropic" | "gemini"
    )
}

/// True when the style is speech-to-text only (no chat).
pub fn is_stt_only_style(style: &str) -> bool {
    matches!(normalize_api_style(style), "deepgram" | "assemblyai")
}

/// The materialized router configuration. Models and request policies remain
/// dynamic; only router-wide execution limits are fixed fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RouterConfig {
    pub models: Vec<RoutedModel>,
    pub request_policies: Vec<RequestPolicy>,
    pub max_total_duration_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub retry_max_retries: u32,
    pub retry_base_secs: u64,
    pub retry_factor: u32,
    pub retry_max_secs: u64,
    pub retry_jitter: f32,
    pub max_concurrent_requests: usize,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            models: Vec::new(),
            request_policies: Vec::new(),
            max_total_duration_secs: 600,
            stream_idle_timeout_secs: 20,
            retry_max_retries: 2,
            retry_base_secs: 2,
            retry_factor: 2,
            retry_max_secs: 30,
            retry_jitter: 0.2,
            max_concurrent_requests: 2,
        }
    }
}

impl RouterConfig {
    pub fn model(&self, id: &str) -> Option<&RoutedModel> {
        self.models.iter().find(|model| model.id == id)
    }

    pub fn model_mut(&mut self, id: &str) -> Option<&mut RoutedModel> {
        self.models.iter_mut().find(|model| model.id == id)
    }

    pub fn policy(&self, request: RequestKind) -> Option<&RequestPolicy> {
        self.request_policies
            .iter()
            .find(|policy| policy.request == request)
    }

    /// Resolve a request policy to its configured model with the required
    /// declared capability.
    pub fn route(&self, request: RequestKind) -> Option<&RoutedModel> {
        let policy = self.policy(request)?;
        let model = self.model(&policy.primary)?;
        (endpoint_credentials_ready(&model.endpoint)
            && model.capabilities.contains(&request.required_capability()))
        .then_some(model)
    }

    /// Legacy selector view. The selected endpoint is still policy-driven.
    pub fn endpoint(&self, role: EndpointRole) -> &ModelEndpoint {
        static EMPTY: OnceLock<ModelEndpoint> = OnceLock::new();
        self.route(role.request_kind())
            .map(|model| &model.endpoint)
            .unwrap_or_else(|| EMPTY.get_or_init(ModelEndpoint::default))
    }

    pub fn endpoint_mut(&mut self, role: EndpointRole) -> Option<&mut ModelEndpoint> {
        let id = self
            .policy(role.request_kind())
            .map(|policy| policy.primary.clone())?;
        self.model_mut(&id).map(|model| &mut model.endpoint)
    }

    pub fn policy_mut(&mut self, request: RequestKind) -> Option<&mut RequestPolicy> {
        self.request_policies
            .iter_mut()
            .find(|policy| policy.request == request)
    }

    /// True when the role has usable credentials (API key or keyless local
    /// server). Used by tools that should no-op gracefully when an endpoint
    /// is not set up.
    pub fn is_configured(&self, role: EndpointRole) -> bool {
        self.route(role.request_kind()).is_some()
    }

    /// Iterate over every configured model, regardless of its capabilities.
    pub fn endpoints_mut(&mut self) -> impl Iterator<Item = &mut ModelEndpoint> {
        self.models.iter_mut().map(|model| &mut model.endpoint)
    }

    /// Apply the global per-response output-cap floor and the global
    /// reasoning-echo cap to every model endpoint. The response-cap floor
    /// raises small `max_tokens` values so long outputs are never
    /// truncated mid-stream (per-endpoint values above the floor are
    /// preserved); the reasoning-echo cap fills `reasoning_echo_max_chars`
    /// only where the endpoint does not set its own override.
    pub fn apply_caps(&mut self, response_cap: Option<u32>, reasoning_echo_cap: Option<usize>) {
        if let Some(cap) = response_cap {
            for ep in self.endpoints_mut() {
                ep.max_tokens = ep.max_tokens.max(cap);
            }
        }
        if let Some(cap) = reasoning_echo_cap {
            for ep in self.endpoints_mut() {
                if ep.reasoning_echo_max_chars.is_none() {
                    ep.reasoning_echo_max_chars = Some(cap);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialize_clears_web_search_for_unsupported_style() {
        let llm = LlmConfig {
            providers: vec![ProviderConfig {
                name: "chat".into(),
                provider: "openai".into(),
                api_style: Some("openai-chat".into()),
                api_key: "sk".into(),
                base_url: "https://api.openai.com/v1".into(),
                ..Default::default()
            }],
            models: vec![RoleConfig {
                role: "default_model".into(),
                provider: "chat".into(),
                model: "gpt-4o".into(),
                capabilities: vec![Capability::Chat],
                web_search: Some("auto".into()),
                ..Default::default()
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Chat,
                primary: "default_model".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let ep = llm.materialize_endpoint(EndpointRole::DefaultModel);
        assert!(ep.web_search.is_none());
    }

    #[test]
    fn materialize_keeps_web_search_for_responses_style() {
        let llm = LlmConfig {
            providers: vec![ProviderConfig {
                name: "ds".into(),
                provider: "deepseek".into(),
                api_style: Some("openai-responses".into()),
                api_key: "sk".into(),
                base_url: "https://api.deepseek.com".into(),
                ..Default::default()
            }],
            models: vec![RoleConfig {
                role: "default_model".into(),
                provider: "ds".into(),
                model: "deepseek-reasoner".into(),
                capabilities: vec![Capability::Chat],
                web_search: Some("always".into()),
                ..Default::default()
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Chat,
                primary: "default_model".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let ep = llm.materialize_endpoint(EndpointRole::DefaultModel);
        assert_eq!(ep.web_search.as_deref(), Some("always"));
        assert_eq!(ep.provider, "deepseek");
    }

    #[test]
    fn keyless_local_providers_count_as_configured() {
        let ollama = ProviderConfig {
            name: "local".into(),
            provider: "ollama".into(),
            api_style: Some("openai-chat".into()),
            api_key: String::new(),
            base_url: "http://127.0.0.1:11434/v1".into(),
            ..Default::default()
        };
        assert!(provider_credentials_ready(&ollama));
        let llm = LlmConfig {
            providers: vec![ollama],
            models: vec![RoleConfig {
                role: "default_model".into(),
                provider: "local".into(),
                model: "llama3.2".into(),
                capabilities: vec![Capability::Chat],
                ..Default::default()
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Chat,
                primary: "default_model".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(llm.is_configured(EndpointRole::DefaultModel));
        let ep = llm.materialize_endpoint(EndpointRole::DefaultModel);
        assert!(endpoint_credentials_ready(&ep));
    }

    #[test]
    fn materialize_preserves_retry_budgets() {
        let llm = LlmConfig {
            retry_max_retries: 4,
            ..Default::default()
        };
        let config = llm.materialize(None, None);
        assert_eq!(config.retry_max_retries, 4);
    }

    #[test]
    fn default_total_duration_is_ten_minutes() {
        assert_eq!(LlmConfig::default().max_total_duration_secs, 600);
        assert_eq!(RouterConfig::default().max_total_duration_secs, 600);
    }

    #[test]
    fn request_policy_requires_the_configured_primary() {
        let llm = LlmConfig {
            providers: vec![ProviderConfig {
                name: "primary".into(),
                api_key: String::new(),
                ..Default::default()
            }],
            models: vec![ModelConfig {
                id: "vision-primary".into(),
                provider: "primary".into(),
                model: "vision-a".into(),
                capabilities: vec![Capability::Vision],
                ..Default::default()
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Vision,
                primary: "vision-primary".into(),
            }],
            ..Default::default()
        };

        assert!(llm.route_model(RequestKind::Vision).is_none());
    }

    #[test]
    fn one_model_can_serve_multiple_request_policies() {
        let llm = LlmConfig {
            providers: vec![ProviderConfig {
                name: "shared".into(),
                api_key: "sk-shared".into(),
                ..Default::default()
            }],
            models: vec![ModelConfig {
                id: "shared-model".into(),
                provider: "shared".into(),
                model: "gpt-4o".into(),
                capabilities: vec![Capability::Chat, Capability::Vision],
                ..Default::default()
            }],
            request_policies: vec![
                RequestPolicy {
                    request: RequestKind::Chat,
                    primary: "shared-model".into(),
                    ..Default::default()
                },
                RequestPolicy {
                    request: RequestKind::Vision,
                    primary: "shared-model".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };

        assert!(llm.is_request_configured(RequestKind::Chat));
        assert!(llm.is_request_configured(RequestKind::Vision));
        assert!(!llm.is_request_configured(RequestKind::Embedding));
    }
}
