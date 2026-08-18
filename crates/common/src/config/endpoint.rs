//! LLM endpoint / provider / role / router configuration: [`ModelEndpoint`],
//! [`ProviderConfig`], [`RoleConfig`], [`EndpointRole`], [`LlmConfig`], and [`RouterConfig`].

use super::*;


#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelEndpoint {
    pub provider: String,
    /// Wire protocol style for this endpoint. One of:
    /// - `openai-chat` (default): OpenAI `/chat/completions` compatible
    ///   (also Ollama, vLLM, DeepSeek, and most gateways)
    /// - `llama.cpp`: llama.cpp server (OpenAI-compatible `/chat/completions`)
    /// - `openai-responses`: OpenAI Responses API (`/v1/responses`)
    /// - `anthropic`: Anthropic Messages API (`/v1/messages`)
    /// - `gemini`: Google Gemini `generateContent` / `streamGenerateContent`
    ///
    /// When empty/`None`, the style is derived from `provider`
    /// (`anthropic` → anthropic, `google`/`gemini` → gemini,
    /// `llama`/`llama.cpp`/`llamacpp` → llama.cpp, otherwise openai-chat).
    #[serde(default)]
    pub api_style: Option<String>,
    pub base_url: String,
    pub api_key: String,
    #[serde(alias = "model")]
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
    // §2.8: reasoning effort for reasoning models ("low" | "medium" | "high"),
    // forwarded to OpenAI-compatible APIs as `reasoning_effort`.
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
    /// True context window of the model in tokens. When unset (None), Haven
    /// resolves it from the builtin model catalog (by `model_name`), falling
    /// back to a 128K default. Used to drive context compaction and the
    /// token-usage display.
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
    prompt_tokens: u32,
    completion_tokens: u32,
) -> Option<f64> {
    if endpoint.cost_per_1k_input_tokens <= 0.0 && endpoint.cost_per_1k_output_tokens <= 0.0 {
        return None;
    }
    let input = (prompt_tokens as f64 / 1000.0) * endpoint.cost_per_1k_input_tokens;
    let output = (completion_tokens as f64 / 1000.0) * endpoint.cost_per_1k_output_tokens;
    Some(input + output)
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
            context_window: None,
            reasoning_echo_max_chars: None,
        }
    }
}

/// A configured LLM provider: connection-level endpoint definition identified
/// by `name`. The model library is no longer a manually-maintained list — it
/// is the union of each provider's `/models` fetch. Roles reference a provider
/// by name and pick a model id from that provider's fetched list; the router
/// materializes the six role endpoints from providers + role slots whenever it
/// is built or hot-swapped (see [`LlmConfig::materialize`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProviderConfig {
    /// Unique id referenced by [`RoleConfig::provider`] and the settings UI.
    pub name: String,
    /// Legacy provider hint used to derive the wire protocol when `api_style`
    /// is empty (e.g. `openai` / `anthropic` / `google` / `llama.cpp`).
    #[serde(default)]
    pub provider: String,
    /// Wire protocol style, mirroring [`ModelEndpoint::api_style`]. When empty
    /// the style is derived from `provider`.
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
    // —— optional per-provider defaults adopted by roles without overrides ——
    /// Default per-response token cap for roles on this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_max_tokens: Option<u32>,
    /// Default sampling temperature for roles on this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_temperature: Option<f32>,
    /// Default first-response timeout (secs) for roles on this provider.
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

/// Role→(provider, model) assignment for one of the six model slots
/// ([`EndpointRole`]). `role` holds the canonical slot name (stamped by
/// [`LlmConfig::set_role`]); `provider` names a [`ProviderConfig`]; `model` is
/// a model id on that provider. All tuning fields are optional overrides:
/// `None` falls back to the provider default, then the builtin model catalog
/// (context window) or [`ModelEndpoint`] built-in defaults. An empty
/// `provider` means the role is unconfigured.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RoleConfig {
    /// Canonical slot name (e.g. `default_model`). Stamped by the config
    /// writer, not the UI.
    #[serde(default)]
    pub role: String,
    /// Referenced provider name (empty = role unconfigured).
    pub provider: String,
    /// Model id on that provider.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_input_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_1k_output_tokens: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_echo_max_chars: Option<usize>,
}

impl RoleConfig {
    /// Stamps the canonical role name (used by [`LlmConfig::set_role`]).
    pub fn stamp_role(&mut self, role: &str) {
        self.role = role.to_string();
    }

    /// True when the slot is fully configured (provider + model both set).
    pub fn is_assigned(&self) -> bool {
        !self.provider.is_empty() && !self.model.is_empty()
    }
}

/// The six model slots (roles) served by the router. Canonical role names
/// (`as_str`) are used in TOML, the frontend protocol, and the model
/// commands; [`LlmConfig::role`] / [`RouterConfig::endpoint`] map a role to
/// its slot, so the 6-arm role match lives in exactly one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointRole {
    SmallModel,
    DefaultModel,
    BalancedModel,
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
            Self::BalancedModel => "balanced_model",
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
            "balanced_model" => Self::BalancedModel,
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
        Self::BalancedModel,
        Self::ImageModel,
        Self::AudioModel,
        Self::EmbeddingModel,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LlmConfig {
    /// Configured providers — the connection-level model library. Roles
    /// reference these by name; the available model ids on each provider are
    /// fetched from its `/models` endpoint at settings time (see
    /// `commands::model::discover_all_models`).
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Role→(provider, model) assignments. A slot with an empty `provider` is
    /// unconfigured (the router materializes a no-key endpoint for it).
    #[serde(default)]
    pub roles: Vec<RoleConfig>,
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
    pub retry_base_secs: u64,
    pub retry_factor: u32,
    pub retry_max_secs: u64,
    pub retry_jitter: f32,
    /// Route recording transcription through the dedicated `audio_model`
    /// endpoint. When false (or the endpoint is unconfigured), the default
    /// model handles transcription.
    pub stt_use_audio_model: bool,
    /// Route image understanding (chat attachments and file-tool vision)
    /// through the dedicated `image_model` endpoint. When false, the default
    /// model handles images.
    pub vision_use_image_model: bool,
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
            roles: Vec::new(),
            max_total_duration_secs: 180,
            stream_idle_timeout_secs: 20,
            retry_base_secs: 2,
            retry_factor: 2,
            retry_max_secs: 30,
            retry_jitter: 0.2,
            stt_use_audio_model: true,
            vision_use_image_model: true,
            max_concurrent_requests: 2,
        }
    }
}

impl LlmConfig {
    /// Look up a configured role slot by its canonical role name.
    pub fn role(&self, role: EndpointRole) -> Option<&RoleConfig> {
        self.roles.iter().find(|r| r.role == role.as_str())
    }

    /// Mutable counterpart of [`Self::role`].
    pub fn role_mut(&mut self, role: EndpointRole) -> Option<&mut RoleConfig> {
        self.roles.iter_mut().find(|r| r.role == role.as_str())
    }

    /// Insert or replace the slot for a role (the `role` field is stamped by
    /// the canonical name so callers can build a [`RoleConfig`] without it).
    pub fn set_role(&mut self, role: EndpointRole, mut config: RoleConfig) {
        let name = role.as_str().to_string();
        match self.roles.iter_mut().find(|r| r.role == name) {
            Some(existing) => {
                config.stamp_role(&name);
                *existing = config;
            }
            None => {
                let mut config = config;
                config.stamp_role(&name);
                self.roles.push(config);
            }
        }
    }

    /// Look up a provider by name.
    pub fn provider(&self, name: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.name == name)
    }

    /// True when a role is usable at runtime: it references a configured
    /// provider (non-empty api_key) and names a model. Used by tools that
    /// should no-op gracefully when an endpoint is not set up.
    pub fn is_configured(&self, role: EndpointRole) -> bool {
        let Some(slot) = self.role(role) else {
            return false;
        };
        if !slot.is_assigned() {
            return false;
        }
        self.provider(slot.provider.as_str())
            .is_some_and(|p| !p.api_key.is_empty())
    }

    /// Materialize the endpoint backing a role from its provider + role slot.
    /// Unassigned roles (or roles referencing a missing provider) materialize
    /// a no-key [`ModelEndpoint::default`], so callers never panic and the
    /// router treats them as unconfigured.
    pub fn materialize_endpoint(&self, role: EndpointRole) -> ModelEndpoint {
        let mut ep = ModelEndpoint::default();
        let Some(slot) = self.role(role) else {
            return ep;
        };
        if !slot.is_assigned() {
            return ep;
        }
        let Some(p) = self.provider(slot.provider.as_str()) else {
            return ep;
        };
        ep.model_name = slot.model.clone();
        ep.api_key = p.api_key.clone();
        ep.api_style = p.api_style.clone();
        ep.provider = wire_provider_hint(&p.provider, &p.api_style);
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
        // Per-role overrides win over provider defaults.
        if let Some(t) = slot.temperature {
            ep.temperature = t;
        }
        if let Some(c) = slot.context_window {
            ep.context_window = Some(c);
        }
        if let Some(c) = slot.cost_per_1k_input_tokens {
            ep.cost_per_1k_input_tokens = c;
        }
        if let Some(c) = slot.cost_per_1k_output_tokens {
            ep.cost_per_1k_output_tokens = c;
        }
        if let Some(m) = slot.max_tokens {
            ep.max_tokens = m;
        }
        if let Some(r) = &slot.reasoning_effort {
            ep.reasoning_effort = Some(r.clone());
        }
        if let Some(w) = &slot.web_search {
            ep.web_search = Some(w.clone());
        }
        if let Some(r) = slot.reasoning_echo_max_chars {
            ep.reasoning_echo_max_chars = Some(r);
        }
        ep
    }

    /// Build the fully materialized router configuration: one endpoint per
    /// role plus every router-level tuning knob. Called whenever the router is
    /// constructed or hot-swapped. `response_cap` / `reasoning_echo_cap`
    /// mirror the legacy `with_response_cap` / `with_reasoning_echo_cap`
    /// transforms (applied to the materialized endpoints so hand-edited
    /// per-role overrides are still respected).
    pub fn materialize(
        &self,
        response_cap: Option<u32>,
        reasoning_echo_cap: Option<usize>,
    ) -> RouterConfig {
        let mut cap = RouterConfig {
            small_model: self.materialize_endpoint(EndpointRole::SmallModel),
            default_model: self.materialize_endpoint(EndpointRole::DefaultModel),
            balanced_model: self.materialize_endpoint(EndpointRole::BalancedModel),
            image_model: self.materialize_endpoint(EndpointRole::ImageModel),
            audio_model: self.materialize_endpoint(EndpointRole::AudioModel),
            embedding_model: self.materialize_endpoint(EndpointRole::EmbeddingModel),
            max_total_duration_secs: self.max_total_duration_secs,
            stream_idle_timeout_secs: self.stream_idle_timeout_secs,
            retry_base_secs: self.retry_base_secs,
            retry_factor: self.retry_factor,
            retry_max_secs: self.retry_max_secs,
            retry_jitter: self.retry_jitter,
            stt_use_audio_model: self.stt_use_audio_model,
            vision_use_image_model: self.vision_use_image_model,
            max_concurrent_requests: self.max_concurrent_requests,
        };
        cap.apply_caps(response_cap, reasoning_echo_cap);
        cap
    }
}

/// Normalize the legacy `provider` hint used by [`ModelEndpoint::api_style`]
/// derivation and the `discover_models` auth scheme when no explicit
/// `api_style` is configured.
fn wire_provider_hint(provider_hint: &str, api_style: &Option<String>) -> String {
    match api_style.as_deref() {
        Some("anthropic") => "anthropic".into(),
        Some("gemini") => "gemini".into(),
        Some("llama.cpp") => "llama.cpp".into(),
        Some(_) => "openai".into(),
        None if provider_hint.is_empty() => "openai".into(),
        None => provider_hint.to_string(),
    }
}

/// The fully materialized router configuration: six role endpoints plus the
/// router-level tuning knobs. Built from [`LlmConfig`] (providers + role
/// slots) via [`LlmConfig::materialize`] whenever the router is constructed
/// or hot-swapped — this is exactly the shape [`LlmRouter`] stores and reads,
/// so runtime hot paths keep working on plain endpoint references.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RouterConfig {
    pub small_model: ModelEndpoint,
    pub default_model: ModelEndpoint,
    pub balanced_model: ModelEndpoint,
    pub image_model: ModelEndpoint,
    pub audio_model: ModelEndpoint,
    pub embedding_model: ModelEndpoint,
    pub max_total_duration_secs: u64,
    pub stream_idle_timeout_secs: u64,
    pub retry_base_secs: u64,
    pub retry_factor: u32,
    pub retry_max_secs: u64,
    pub retry_jitter: f32,
    pub stt_use_audio_model: bool,
    pub vision_use_image_model: bool,
    pub max_concurrent_requests: usize,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            small_model: ModelEndpoint::default(),
            default_model: ModelEndpoint::default(),
            balanced_model: ModelEndpoint::default(),
            image_model: ModelEndpoint::default(),
            audio_model: ModelEndpoint::default(),
            embedding_model: ModelEndpoint::default(),
            max_total_duration_secs: 180,
            stream_idle_timeout_secs: 20,
            retry_base_secs: 2,
            retry_factor: 2,
            retry_max_secs: 30,
            retry_jitter: 0.2,
            stt_use_audio_model: true,
            vision_use_image_model: true,
            max_concurrent_requests: 2,
        }
    }
}

impl RouterConfig {
    /// The endpoint slot backing a role. Central role→field mapping: the
    /// router, agent, and model commands all route through this instead of
    /// repeating the per-role match across the codebase.
    pub fn endpoint(&self, role: EndpointRole) -> &ModelEndpoint {
        match role {
            EndpointRole::SmallModel => &self.small_model,
            EndpointRole::DefaultModel => &self.default_model,
            EndpointRole::BalancedModel => &self.balanced_model,
            EndpointRole::ImageModel => &self.image_model,
            EndpointRole::AudioModel => &self.audio_model,
            EndpointRole::EmbeddingModel => &self.embedding_model,
        }
    }

    /// Mutable counterpart of [`Self::endpoint`].
    pub fn endpoint_mut(&mut self, role: EndpointRole) -> &mut ModelEndpoint {
        match role {
            EndpointRole::SmallModel => &mut self.small_model,
            EndpointRole::DefaultModel => &mut self.default_model,
            EndpointRole::BalancedModel => &mut self.balanced_model,
            EndpointRole::ImageModel => &mut self.image_model,
            EndpointRole::AudioModel => &mut self.audio_model,
            EndpointRole::EmbeddingModel => &mut self.embedding_model,
        }
    }

    /// True when the role has a non-empty api_key configured. Used by tools
    /// that should no-op gracefully when an endpoint is not set up.
    pub fn is_configured(&self, role: EndpointRole) -> bool {
        !self.endpoint(role).api_key.is_empty()
    }

    /// Owned iteration over every role slot in canonical order (a fixed 6
    /// elements), used by transforms that apply a value to all endpoints.
    pub fn endpoints_mut(&mut self) -> impl Iterator<Item = &mut ModelEndpoint> {
        [
            &mut self.small_model,
            &mut self.default_model,
            &mut self.balanced_model,
            &mut self.image_model,
            &mut self.audio_model,
            &mut self.embedding_model,
        ]
        .into_iter()
    }

    /// Apply the global per-response output-cap floor and the global
    /// reasoning-echo cap to every role endpoint. The response-cap floor
    /// raises small legacy `max_tokens` values so long outputs are never
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
