use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::adapters::adapter_for;
use crate::client::LlmClient;
#[cfg(test)]
use crate::endpoint_health::{CircuitBreaker, CircuitState};
use crate::endpoint_health::{
    EndpointHealth, EndpointHealthSlots, health_index, new_endpoint_health_slots,
};
use crate::request_pipeline::{RequestPolicy, execute_with_retry, execute_with_timeout};
use haven_common::types::{CanonicalMessage, ContentPart};

use crate::stream_rules::{StreamRule, StreamRuleMatch, check_stream_rules};
use crate::streaming;
use crate::types::{
    Embedding, LlmConnectionStatus, LlmError, LlmResponse, StreamChunk, ToolDefinition, Usage,
};
use futures_util::future::join_all;
use haven_common::config::{ModelEndpoint, RouterConfig, compute_cost_usd};

/// Model slot roles. The canonical definition lives next to the config it
/// routes to (`haven_common::config::EndpointRole`); re-exported here so
/// callers keep a single `haven_llm::EndpointRole` path.
pub use haven_common::config::EndpointRole;

// ---------------------------------------------------------------------------
// §2.6: Circuit Breaker state
// ---------------------------------------------------------------------------

/// Stream wrapper that holds a role's concurrency permit until the stream is
/// dropped, so a raw `chat_stream` result cannot bypass the per-endpoint
/// in-flight cap once the caller starts consuming it.
struct PermitStream<S> {
    inner: S,
    _permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl<S> futures_util::Stream for PermitStream<S>
where
    S: futures_util::Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// The mutable runtime state every `LlmRouter` constructor initializes the
/// same way (health trackers, stream rules, semaphores, rate-limit cooldowns).
type RuntimeStateParts = (
    RwLock<EndpointHealthSlots>,
    RwLock<Vec<StreamRule>>,
    StdMutex<[Arc<tokio::sync::Semaphore>; 6]>,
    RwLock<[Option<Instant>; 6]>,
);

pub struct LlmRouter {
    config: Arc<RwLock<RouterConfig>>,
    pub small_model: Arc<dyn LlmClient>,
    pub default_model: Arc<dyn LlmClient>,
    pub balanced_model: Arc<dyn LlmClient>,
    pub image_model: Arc<dyn LlmClient>,
    pub audio_model: Arc<dyn LlmClient>,
    pub embedding_model: Arc<dyn LlmClient>,
    balanced_model_active: AtomicBool,
    // §5.3: per-endpoint health (index: 0=SmallModel, 1=DefaultModel, 2=BalancedModel, 3=ImageModel, 4=AudioModel, 5=EmbeddingModel)
    health: RwLock<[EndpointHealth; 6]>,
    /// Stream rules that are checked against accumulated output (§3.7)
    stream_rules: RwLock<Vec<StreamRule>>,
    /// Per-role concurrency limit: at most `llm.max_concurrent_requests`
    /// requests may be in flight per endpoint role. Parallel sessions hitting
    /// the same provider queue here instead of piling onto the provider
    /// (which would produce 429 storms and thundering-herd retries).
    /// Semaphores are created from the config at construction; a settings
    /// save rebuilds the router (`hot_swap_router`), so the limit is
    /// applied to new requests immediately. The mutex is only held to clone
    /// an `Arc<Semaphore>` (never across an await), so it adds no contention.
    semaphores: StdMutex<[Arc<tokio::sync::Semaphore>; 6]>,
    /// Shared rate-limit cooldown per role: when a request ends with a 429
    /// (RateLimit), subsequent callers to the same role wait until the
    /// deadline before dispatching, so a burst of parallel sessions does not
    /// retry simultaneously and amplify the load.
    rate_limited: RwLock<[Option<Instant>; 6]>,
}

/// Callbacks and output policy for one routed streaming request.
///
/// The provider may make several attempts for one logical Agent turn. The
/// chunk callback receives provider deltas; the attempt callback marks whether
/// a new attempt replaces the already visible output. Keeping these controls
/// together prevents callers from accidentally handling retry boundaries as
/// ordinary chunks.
pub struct StreamAttemptHooks {
    on_chunk: Box<dyn FnMut(&StreamChunk) + Send + 'static>,
    on_attempt_start: Box<dyn FnMut(bool) + Send + 'static>,
    replace_output_on_start: bool,
}

impl StreamAttemptHooks {
    pub fn new(
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
        on_attempt_start: impl FnMut(bool) + Send + 'static,
        replace_output_on_start: bool,
    ) -> Self {
        Self {
            on_chunk: Box::new(on_chunk),
            on_attempt_start: Box::new(on_attempt_start),
            replace_output_on_start,
        }
    }
}

type ChunkCallback = Box<dyn FnMut(&StreamChunk) + Send + 'static>;
type AttemptCallback = Box<dyn FnMut(bool) + Send + 'static>;

struct ActiveStreamHooks {
    on_chunk: Arc<StdMutex<ChunkCallback>>,
    on_attempt_start: Arc<StdMutex<AttemptCallback>>,
}

struct RetryStreamRequest<'a> {
    messages: &'a [CanonicalMessage],
    tools: &'a [ToolDefinition],
    cancel: CancellationToken,
    error: LlmError,
    replace_output: bool,
}

impl LlmRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self::with_default_context_window(config, crate::registry::FALLBACK_CONTEXT_WINDOW)
    }

    /// Like [`Self::new`], but clamp `max_tokens` against
    /// `context_limits.default_context_window` when a role has no explicit
    /// `context_window` (instead of the hardcoded 128K fallback).
    pub fn with_default_context_window(
        mut config: RouterConfig,
        default_context_window: u32,
    ) -> Self {
        // The per-response cap floor (`with_response_cap`) can push
        // `max_tokens` far above a provider's per-model output limit; sending
        // the raw value (e.g. the 1M default floor) makes Anthropic/OpenAI/
        // Gemini reject the request with HTTP 400. Clamp each endpoint's
        // effective `max_tokens` to its resolved context window here so the
        // floor can never exceed what the provider will accept, while small
        // legacy caps (8192) still get lifted so long outputs are not
        // truncated mid-stream.
        let fallback = if default_context_window > 0 {
            default_context_window
        } else {
            crate::registry::FALLBACK_CONTEXT_WINDOW
        };
        for ep in [
            &mut config.small_model,
            &mut config.default_model,
            &mut config.balanced_model,
            &mut config.image_model,
            &mut config.audio_model,
            &mut config.embedding_model,
        ] {
            let window = crate::registry::context_window_for(ep).unwrap_or(fallback);
            if window > 0 {
                ep.max_tokens = ep.max_tokens.min(window);
            }
        }
        // Failover sanity check: when the balanced slot points at the same
        // endpoint+model as the primary, any primary failure (timeout, empty
        // response, provider outage) will fail identically on failover — the
        // fallback is a copy, not a fallback. Warn loudly so the misconfig is
        // visible (the user's "对话不回复" reports all traced back to this).
        if config.balanced_model.base_url == config.default_model.base_url
            && config.balanced_model.model_name == config.default_model.model_name
        {
            tracing::warn!(
                "balanced_model points at the same endpoint+model as default_model ({} | {}) — \
                 failover will not help when the primary fails. Configure a different provider \
                 for the balanced slot.",
                config.default_model.base_url,
                config.default_model.model_name
            );
        }
        let small_model = Arc::from(adapter_for(&config.small_model));
        let default_model = Arc::from(adapter_for(&config.default_model));
        let balanced_model = Arc::from(adapter_for(&config.balanced_model));
        let image_model = Arc::from(adapter_for(&config.image_model));
        let audio_model = Arc::from(adapter_for(&config.audio_model));
        let embedding_model = Arc::from(adapter_for(&config.embedding_model));
        let request_limit = Self::request_limit(&config);
        let (health, _, semaphores, rate_limited) = Self::runtime_state(request_limit);
        Self {
            config: Arc::new(RwLock::new(config)),
            small_model,
            default_model,
            balanced_model,
            image_model,
            audio_model,
            embedding_model,
            balanced_model_active: AtomicBool::new(false),
            health,
            // Production routers start with the default no-code-block guard.
            // Test constructors keep an empty rule list via `runtime_state`.
            stream_rules: RwLock::new(Self::default_stream_rules()),
            semaphores,
            rate_limited,
        }
    }

    /// Default stream-output guards applied to every production router.
    /// Currently aborts when the model starts a fenced code block (it should
    /// call tools instead of dumping code into the chat).
    fn default_stream_rules() -> Vec<StreamRule> {
        match StreamRule::code_block_abort() {
            Ok(rule) => vec![rule],
            Err(e) => {
                tracing::error!("failed to compile default stream rule: {e}");
                Vec::new()
            }
        }
    }

    /// Config-driven per-role request limit. Clamped to >= 1 so a hand-edited
    /// 0 (or a config without the new field) can never deadlock the router on
    /// an unacquirable permit.
    fn request_limit(config: &RouterConfig) -> usize {
        config.max_concurrent_requests.max(1)
    }

    /// Default runtime state shared by every constructor: per-role health
    /// trackers, stream rules, concurrency semaphores, and rate-limit flags.
    fn runtime_state(request_limit: usize) -> RuntimeStateParts {
        (
            RwLock::new(new_endpoint_health_slots()),
            RwLock::new(Vec::new()),
            StdMutex::new(Self::make_semaphores(request_limit)),
            RwLock::new([None, None, None, None, None, None]),
        )
    }

    /// Clone the concurrency permit for a role (the mutex is released before
    /// any await; the Arc clone is cheap).
    fn role_permit(&self, idx: usize) -> Arc<tokio::sync::Semaphore> {
        self.semaphores.lock().unwrap()[idx].clone()
    }

    fn make_semaphores(limit: usize) -> [Arc<tokio::sync::Semaphore>; 6] {
        [
            Arc::new(tokio::sync::Semaphore::new(limit)),
            Arc::new(tokio::sync::Semaphore::new(limit)),
            Arc::new(tokio::sync::Semaphore::new(limit)),
            Arc::new(tokio::sync::Semaphore::new(limit)),
            Arc::new(tokio::sync::Semaphore::new(limit)),
            Arc::new(tokio::sync::Semaphore::new(limit)),
        ]
    }

    /// Wait out the shared rate-limit cooldown for a role (if any).
    async fn wait_rate_limit_cooldown(&self, role: &EndpointRole) {
        let idx = Self::health_index(role);
        let cooldown_until = self.rate_limited.read().await[idx];
        if let Some(until) = cooldown_until {
            let now = Instant::now();
            if until > now {
                tracing::debug!(
                    "router role {:?} rate-limited, waiting {:?} before dispatch",
                    role,
                    until - now
                );
                tokio::time::sleep(until - now).await;
            }
        }
    }

    /// Extend the shared cooldown for a role after a RateLimit result, so a
    /// burst of parallel sessions queues behind the longest wait instead of
    /// re-hammering the provider simultaneously.
    ///
    /// The wait is CLAMPED: `Retry-After` comes from the (possibly hostile or
    /// misbehaving) provider, and the cooldown blocks the whole role while
    /// holding a semaphore permit — an unbounded value would freeze every
    /// agent/embedding/STT request through that endpoint.
    async fn record_rate_limit(&self, role: &EndpointRole, retry_after: Option<Duration>) {
        let idx = Self::health_index(role);
        // Cap at 30s: long enough to ride out a provider-side rate window,
        // short enough that a hostile endpoint cannot pin the role.
        let wait = retry_after
            .unwrap_or(Duration::from_secs(5))
            .min(Duration::from_secs(30));
        let until = Instant::now() + wait;
        let mut rl = self.rate_limited.write().await;
        if rl[idx].is_none_or(|c| c < until) {
            rl[idx] = Some(until);
        }
    }

    /// Run `f` under the role's concurrency permit, waiting out any shared
    /// rate-limit cooldown first. The permit is held across the WHOLE call
    /// (including retries and stream consumption), so the concurrency cap is
    /// real provider load, not just request starts.
    ///
    /// After a RateLimit result, the role's cooldown is extended so other
    /// sessions queue behind this one instead of re-hammering the provider —
    /// `with_retry` already waits per-request, this paces the herd.
    async fn with_endpoint_permit<T, F, Fut>(
        &self,
        role: &EndpointRole,
        f: F,
    ) -> Result<T, LlmError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, LlmError>>,
    {
        let idx = Self::health_index(role);
        let permit = self
            .role_permit(idx)
            .acquire_owned()
            .await
            .map_err(|_| LlmError::ServerError("router semaphore closed".into()))?;
        self.wait_rate_limit_cooldown(role).await;
        let result = f().await;
        if let Err(LlmError::RateLimit { retry_after }) = &result {
            self.record_rate_limit(role, *retry_after).await;
        }
        drop(permit);
        result
    }

    /// Test utility: replace the per-role semaphores with a new limit, to
    /// exercise the concurrency cap without rebuilding the router with real
    /// HTTP adapters. Production limits come from `llm.max_concurrent_requests`
    /// at construction (and are refreshed by `hot_swap_router` on save).
    #[doc(hidden)]
    pub fn set_request_limit_for_test(&self, limit: usize) {
        *self.semaphores.lock().unwrap() = Self::make_semaphores(limit.max(1));
    }

    /// Test utility: read the current rate-limit cooldown deadline for a role.
    #[doc(hidden)]
    pub async fn rate_limit_deadline_for_test(&self, role: &EndpointRole) -> Option<Instant> {
        self.rate_limited.read().await[Self::health_index(role)]
    }

    pub fn new_with_clients(
        small_model: Arc<dyn LlmClient>,
        default_model: Arc<dyn LlmClient>,
        balanced_model: Arc<dyn LlmClient>,
        image_model: Arc<dyn LlmClient>,
        audio_model: Arc<dyn LlmClient>,
    ) -> Self {
        Self::new_with_clients_full(
            small_model,
            default_model,
            balanced_model,
            image_model,
            audio_model,
            Arc::from(adapter_for(&ModelEndpoint::default())),
        )
    }

    /// Like [`Self::new_with_clients`] but with an explicit embedding endpoint
    /// (tests that exercise the embeddings path pass a mock here).
    pub fn new_with_clients_full(
        small_model: Arc<dyn LlmClient>,
        default_model: Arc<dyn LlmClient>,
        balanced_model: Arc<dyn LlmClient>,
        image_model: Arc<dyn LlmClient>,
        audio_model: Arc<dyn LlmClient>,
        embedding_model: Arc<dyn LlmClient>,
    ) -> Self {
        let (health, stream_rules, semaphores, rate_limited) = Self::runtime_state(64);
        Self {
            config: Arc::new(RwLock::new(RouterConfig::default())),
            small_model,
            default_model,
            balanced_model,
            image_model,
            audio_model,
            embedding_model,
            balanced_model_active: AtomicBool::new(false),
            health,
            stream_rules,
            // Test constructors bypass the config, so use a high per-role
            // limit: the semaphore is meant to pace real provider traffic,
            // not to serialize mock-based tests.
            semaphores,
            rate_limited,
        }
    }

    pub fn select_endpoint(&self, role: EndpointRole) -> Arc<dyn LlmClient> {
        match role {
            EndpointRole::SmallModel => self.small_model.clone(),
            EndpointRole::DefaultModel => self.default_model.clone(),
            EndpointRole::BalancedModel => self.balanced_model.clone(),
            EndpointRole::ImageModel => self.image_model.clone(),
            EndpointRole::AudioModel => self.audio_model.clone(),
            EndpointRole::EmbeddingModel => self.embedding_model.clone(),
        }
    }

    fn health_index(role: &EndpointRole) -> usize {
        health_index(role)
    }

    /// Returns true if the role has a non-empty api_key configured.
    /// Used by tools that should no-op gracefully when an endpoint is not set up.
    pub async fn is_role_configured(&self, role: EndpointRole) -> bool {
        self.config.read().await.is_configured(role)
    }

    fn health(&self, role: &EndpointRole) -> usize {
        Self::health_index(role)
    }

    /// Test utility: force the configured state of a role (empty vs non-empty
    /// api_key). `new_with_clients` builds with a default config where all
    /// keys are empty; cross-crate tests that exercise `is_role_configured`
    /// guards use this to simulate a configured endpoint.
    #[doc(hidden)]
    pub async fn force_role_configured(&self, role: EndpointRole, configured: bool) {
        let mut cfg = self.config.write().await;
        if configured {
            cfg.endpoint_mut(role).api_key = "sk-test".to_string();
        } else {
            cfg.endpoint_mut(role).api_key = String::new();
        }
    }

    /// Test utility: set the routing flags (`stt_use_audio_model`,
    /// `vision_use_image_model`).
    #[doc(hidden)]
    pub async fn force_routing_flags(
        &self,
        stt_use_audio_model: bool,
        vision_use_image_model: bool,
    ) {
        let mut cfg = self.config.write().await;
        cfg.stt_use_audio_model = stt_use_audio_model;
        cfg.vision_use_image_model = vision_use_image_model;
    }

    /// Resolve the endpoint role used for speech-to-text transcription.
    /// Returns `Some(AudioModel)` when `stt_use_audio_model` is enabled and
    /// the audio_model endpoint is configured; `None` when the flag is enabled
    /// but the endpoint is missing (callers should surface a setup hint); and
    /// `Some(DefaultModel)` when the flag is disabled.
    pub async fn stt_role(&self) -> Option<EndpointRole> {
        let cfg = self.config.read().await;
        if cfg.stt_use_audio_model {
            if cfg.is_configured(EndpointRole::AudioModel) {
                Some(EndpointRole::AudioModel)
            } else {
                None
            }
        } else {
            Some(EndpointRole::DefaultModel)
        }
    }

    /// Transcribe WAV audio through the STT role (`audio_model` / default).
    /// Tries native [`LlmClient::transcribe`] first; when the adapter reports
    /// [`LlmError::UnsupportedCapability`], falls back to multimodal chat
    /// with an `input_audio` content part (gpt-4o-audio-preview etc.).
    pub async fn transcribe_audio(
        &self,
        wav_data: &[u8],
    ) -> Result<crate::types::SttResult, LlmError> {
        let role = match self.stt_role().await {
            Some(role) => role,
            None => {
                return Err(LlmError::RequestFailed(
                    "audio_model endpoint is not configured; configure it in Settings -> LLM to use LLM-based transcription"
                        .into(),
                ));
            }
        };
        let client = self.select_endpoint(role);
        match client.transcribe(wav_data).await {
            Ok(result) => Ok(result),
            Err(e) if e.is_unsupported() => {
                crate::stt::transcribe_via_chat(self, role, wav_data).await
            }
            Err(e) => Err(e),
        }
    }

    /// Resolve the endpoint role for image understanding in chat: the
    /// dedicated image_model when `vision_use_image_model` is enabled and the
    /// endpoint is configured, otherwise the default model.
    pub async fn vision_role(&self) -> EndpointRole {
        let cfg = self.config.read().await;
        if cfg.vision_use_image_model && cfg.is_configured(EndpointRole::ImageModel) {
            EndpointRole::ImageModel
        } else {
            EndpointRole::DefaultModel
        }
    }

    // §2.6: check circuit breaker before dispatching
    async fn check_circuit(&self, role: &EndpointRole) -> Result<(), LlmError> {
        let idx = self.health(role);
        let mut health = self.health.write().await;
        if !health[idx].allow_request() {
            return Err(LlmError::ServerError(format!(
                "circuit breaker open for {:?}",
                role
            )));
        }
        Ok(())
    }

    async fn record_success(&self, role: &EndpointRole) {
        let idx = self.health(role);
        let mut health = self.health.write().await;
        health[idx].record_success();
    }

    async fn record_failure(&self, role: &EndpointRole) {
        let idx = self.health(role);
        let mut health = self.health.write().await;
        health[idx].record_failure();
    }

    // §2.12: apply total timeout wrapper
    async fn with_total_timeout<F, Fut>(&self, f: F) -> Result<LlmResponse, LlmError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<LlmResponse, LlmError>>,
    {
        let cfg = self.config.read().await;
        let max_dur = cfg.max_total_duration_secs;
        drop(cfg);
        execute_with_timeout(max_dur, "router", f).await
    }

    // §2.11: execute with retry on endpoint, balanced_model with retry
    async fn call_with_retry_and_balanced_model(
        &self,
        primary: Arc<dyn LlmClient>,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        role: &EndpointRole,
    ) -> Result<LlmResponse, LlmError> {
        let cfg = self.config.read().await;
        let primary_policy = RequestPolicy::primary(&cfg);
        let fallback_policy = RequestPolicy::fallback(&cfg);
        drop(cfg);

        let primary_result = if tools.is_empty() {
            execute_with_retry(primary_policy.retry, None, || async {
                primary.chat(messages.clone()).await
            })
            .await
        } else {
            execute_with_retry(primary_policy.retry, None, || async {
                primary
                    .chat_with_tools(messages.clone(), tools.clone())
                    .await
            })
            .await
        };

        match primary_result {
            Ok(v) => {
                self.record_success(role).await;
                self.balanced_model_active.store(false, Ordering::SeqCst);
                Ok(v)
            }
            Err(primary_err) => {
                // §2.13: preserve primary error
                self.record_failure(role).await;
                // Record the shared cooldown at the source: the error is about
                // to be wrapped into AllEndpointsFailed, losing its type.
                if let LlmError::RateLimit { retry_after } = &primary_err {
                    self.record_rate_limit(role, *retry_after).await;
                }
                let primary_msg = primary_err.to_string();
                if !primary_err.is_retryable() && !primary_err.is_unsupported() {
                    return Err(primary_err);
                }
                tracing::warn!(
                    "primary endpoint failed: {}, attempting balanced model",
                    primary_msg
                );
                self.balanced_model_active.store(true, Ordering::SeqCst);

                // §2.11: balanced model also gets retry
                let balanced_result = if tools.is_empty() {
                    execute_with_retry(fallback_policy.retry, None, || async {
                        self.balanced_model.chat(messages.clone()).await
                    })
                    .await
                } else {
                    execute_with_retry(fallback_policy.retry, None, || async {
                        self.balanced_model
                            .chat_with_tools(messages.clone(), tools.clone())
                            .await
                    })
                    .await
                };

                match balanced_result {
                    Ok(v) => {
                        self.record_success(&EndpointRole::BalancedModel).await;
                        Ok(v)
                    }
                    Err(balanced_err) => {
                        if let LlmError::RateLimit { retry_after } = &balanced_err {
                            self.record_rate_limit(&EndpointRole::BalancedModel, *retry_after)
                                .await;
                        }
                        self.record_failure(&EndpointRole::BalancedModel).await;
                        let balanced_msg = balanced_err.to_string();
                        Err(LlmError::AllEndpointsFailed(primary_msg, balanced_msg))
                    }
                }
            }
        }
    }

    pub async fn chat(
        &self,
        role: EndpointRole,
        messages: Vec<CanonicalMessage>,
    ) -> Result<LlmResponse, LlmError> {
        self.with_endpoint_permit(&role, || async {
            self.check_circuit(&role).await?;
            let primary = self.select_endpoint(role);
            self.with_total_timeout(|| async {
                self.call_with_retry_and_balanced_model(primary, messages, Vec::new(), &role)
                    .await
            })
            .await
        })
        .await
    }

    /// Convenience wrapper that builds a `System + User` message pair (or just
    /// `User` when `system` is empty) and forwards to [`Self::chat`]. Used by
    /// one-shot prompts that don't need a full conversation history (title
    /// generation, fact extraction, conversation summarization).
    pub async fn chat_with_prompt(
        &self,
        role: EndpointRole,
        system: &str,
        user: &str,
    ) -> Result<LlmResponse, LlmError> {
        let mut messages = Vec::with_capacity(if system.is_empty() { 1 } else { 2 });
        if !system.is_empty() {
            messages.push(CanonicalMessage::system(vec![ContentPart::text(system)]));
        }
        messages.push(CanonicalMessage::user(vec![ContentPart::text(user)]));
        self.chat(role, messages).await
    }

    /// Embed a batch of texts into vectors via the dedicated `embedding_model`
    /// endpoint. No balanced-model fallback: the fallback slot is a chat
    /// endpoint and cannot produce embeddings. Applies the circuit breaker,
    /// retry, and the router-level total timeout like other calls.
    pub async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
        if input.is_empty() {
            return Ok(Embedding {
                vectors: Vec::new(),
                model: None,
                usage: Usage::default(),
            });
        }
        let role = EndpointRole::EmbeddingModel;
        self.with_endpoint_permit(&role, || async {
            self.check_circuit(&role).await?;
            let primary = self.select_endpoint(role);
            let cfg = self.config.read().await;
            let policy = RequestPolicy::primary(&cfg);
            drop(cfg);

            let result = execute_with_timeout(policy.total_timeout_secs, "embedding", || async {
                execute_with_retry(policy.retry, None, || primary.embed(input.clone())).await
            })
            .await;

            match result {
                Ok(v) => {
                    self.record_success(&role).await;
                    Ok(v)
                }
                Err(e) => {
                    self.record_failure(&role).await;
                    Err(e)
                }
            }
        })
        .await
    }

    /// Convenience wrapper for single-text embedding.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let emb = self.embed(vec![text.to_string()]).await?;
        Ok(emb.vectors.into_iter().next().unwrap_or_default())
    }

    pub async fn chat_with_tools(
        &self,
        role: EndpointRole,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.with_endpoint_permit(&role, || async {
            self.check_circuit(&role).await?;
            let primary = self.select_endpoint(role);
            self.with_total_timeout(|| async {
                self.call_with_retry_and_balanced_model(primary, messages, tools, &role)
                    .await
            })
            .await
        })
        .await
    }

    pub async fn chat_stream(
        &self,
        role: EndpointRole,
        messages: Vec<CanonicalMessage>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures_util::Stream<Item = Result<crate::types::StreamChunk, LlmError>> + Send,
            >,
        >,
        LlmError,
    > {
        let idx = Self::health_index(&role);
        let permit = self
            .role_permit(idx)
            .acquire_owned()
            .await
            .map_err(|_| LlmError::ServerError("router semaphore closed".into()))?;
        self.wait_rate_limit_cooldown(&role).await;
        self.check_circuit(&role).await?;
        let primary = self.select_endpoint(role);
        let cfg = self.config.read().await;
        let primary_policy = RequestPolicy::primary(&cfg);
        let fallback_policy = RequestPolicy::fallback(&cfg);
        drop(cfg);
        match execute_with_retry(primary_policy.retry, None, || {
            primary.chat_stream(messages.clone())
        })
        .await
        {
            Ok(stream) => {
                self.record_success(&role).await;
                self.balanced_model_active.store(false, Ordering::SeqCst);
                Ok(Box::pin(PermitStream {
                    inner: stream,
                    _permit: Some(permit),
                }))
            }
            Err(e) => {
                self.record_failure(&role).await;
                if !e.is_retryable() && !e.is_unsupported() {
                    return Err(e);
                }
                tracing::warn!(
                    "primary chat_stream failed: {}, attempting balanced model",
                    e
                );
                self.balanced_model_active.store(true, Ordering::SeqCst);
                let stream = execute_with_retry(fallback_policy.retry, None, || {
                    self.balanced_model.chat_stream(messages.clone())
                })
                .await?;
                self.record_success(&EndpointRole::BalancedModel).await;
                Ok(Box::pin(PermitStream {
                    inner: stream,
                    _permit: Some(permit),
                }))
            }
        }
    }

    /// Stream-chat a tool-aware request to the primary endpoint, aggregating
    /// the deltas into a final `LlmResponse`.
    ///
    /// `messages`/`tools` are borrowed: the router clones them for each
    /// attempt internally, so callers (e.g. the ReAct loop) can convert once
    /// per step and reuse the same converted messages across retries.
    pub async fn chat_stream_with_tools_aggregated(
        &self,
        role: EndpointRole,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_stream_with_tools_aggregated_cancellable(
            role,
            messages,
            tools,
            on_chunk,
            CancellationToken::new(),
        )
        .await
    }

    /// Stream-chat with cancellation, using the balanced model as a backup (§2.10, §2.11).
    /// Applies `max_total_duration_secs` as an overall deadline (§2.12).
    /// A transient stream failure is retried only when the failed attempt did
    /// not emit a chunk. Once anything has reached `on_chunk`, replaying would
    /// duplicate visible thought/reasoning output, so the router fails over
    /// rather than retrying that same stream.
    ///
    /// Runs under the role's concurrency permit (see
    /// [`Self::with_endpoint_permit`]): the permit covers the whole stream —
    /// retries, failover and chunk consumption — so parallel sessions cannot
    /// exceed the configured per-endpoint in-flight request cap.
    pub async fn chat_stream_with_tools_aggregated_cancellable(
        &self,
        role: EndpointRole,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_stream_with_tools_aggregated_cancellable_with_attempts(
            role,
            messages,
            tools,
            StreamAttemptHooks::new(on_chunk, |_| {}, false),
            cancel,
        )
        .await
    }

    /// Stream-chat with explicit output-attempt boundaries.
    ///
    /// `on_attempt_start(true)` means the new provider attempt replaces the
    /// previous visible output. The callback is deliberately separate from
    /// `on_chunk`: a provider failover can start a new response before its
    /// first chunk arrives, and concatenating both attempts is never valid.
    /// The legacy cancellable method above keeps the old callback-only API for
    /// non-agent callers.
    pub async fn chat_stream_with_tools_aggregated_cancellable_with_attempts(
        &self,
        role: EndpointRole,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        hooks: StreamAttemptHooks,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        self.with_endpoint_permit(&role, || async {
            self.chat_stream_with_tools_aggregated_cancellable_inner(
                role, messages, tools, hooks, cancel,
            )
            .await
        })
        .await
    }

    /// Re-run a stream on the primary endpoint after a stream rule aborted it,
    /// injecting the rule's guidance as a trailing user message. Shared by the
    /// single-attempt and balanced-failover streaming paths. `err` must be a
    /// `StreamAborted` variant.
    async fn retry_stream_with_guidance(
        &self,
        primary: &Arc<dyn LlmClient>,
        hooks: &ActiveStreamHooks,
        request: RetryStreamRequest<'_>,
    ) -> Result<LlmResponse, LlmError> {
        let RetryStreamRequest {
            messages,
            tools,
            cancel,
            error,
            replace_output,
        } = request;
        let LlmError::StreamAborted(rule_name, inject) = error else {
            return Err(error);
        };
        tracing::warn!(
            "stream aborted by rule '{}', injecting guidance and retrying with primary",
            rule_name
        );
        hooks.on_attempt_start.lock().unwrap()(replace_output);
        // Clamp to >= 1s: a hand-edited 0 would make every stream.first() poll
        // time out instantly, disabling all model replies.
        let idle_dur =
            Duration::from_secs(self.config.read().await.stream_idle_timeout_secs.max(1));
        let mut retry_msgs = messages.to_vec();
        // The guidance is appended AFTER the assistant's partial
        // turn. A trailing System message breaks OpenAI-compatible
        // providers (system must lead the request) and is merged
        // into the top-level system field by Anthropic/Gemini,
        // losing its position. A User message is legal anywhere.
        retry_msgs.push(CanonicalMessage::user_text(inject));
        streaming::aggregate_stream_cancellable(
            primary.clone(),
            retry_msgs,
            tools.to_vec(),
            hooks.on_chunk.clone(),
            cancel,
            &self.stream_rules,
            idle_dur,
        )
        .await
    }

    async fn chat_stream_with_tools_aggregated_cancellable_inner(
        &self,
        role: EndpointRole,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        hooks: StreamAttemptHooks,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        self.check_circuit(&role).await?;
        tracing::debug!(
            "router streaming LLM call, role={:?} messages={} tools={}",
            role,
            messages.len(),
            tools.len()
        );
        let primary = self.select_endpoint(role);
        let StreamAttemptHooks {
            on_chunk,
            on_attempt_start,
            replace_output_on_start,
        } = hooks;
        let hooks = ActiveStreamHooks {
            on_chunk: Arc::new(StdMutex::new(on_chunk)),
            on_attempt_start: Arc::new(StdMutex::new(on_attempt_start)),
        };
        hooks.on_attempt_start.lock().unwrap()(replace_output_on_start);

        let cfg = self.config.read().await;
        let primary_policy = RequestPolicy::primary(&cfg);
        let fallback_policy = RequestPolicy::fallback(&cfg);
        // Clamp to >= 1s: a hand-edited 0 would make every stream.first() poll
        // time out instantly, disabling all model replies.
        let idle_dur = Duration::from_secs(cfg.stream_idle_timeout_secs.max(1));
        drop(cfg);
        let stream_context = streaming::StreamContext { messages, tools };

        execute_with_timeout(primary_policy.total_timeout_secs, "router streaming", || async {
            let primary_result = streaming::aggregate_stream_with_retry_before_output(
                    primary.clone(),
                    stream_context,
                    hooks.on_chunk.clone(),
                    cancel.clone(),
                    &self.stream_rules,
                    idle_dur,
                    primary_policy.retry,
                )
                .await;

            match primary_result {
                Ok(resp) => {
                    self.record_success(&role).await;
                    self.balanced_model_active.store(false, Ordering::SeqCst);
                    Ok(resp)
                }
                Err(err @ LlmError::StreamAborted(_, _)) => {
                    self.retry_stream_with_guidance(
                        &primary,
                        &hooks,
                        RetryStreamRequest {
                            messages,
                            tools,
                            cancel: cancel.clone(),
                            error: err,
                            replace_output: true,
                        },
                    )
                    .await
                }
                Err(e) => {
                    if cancel.is_cancelled() {
                        return Err(LlmError::Cancelled);
                    }
                    // Record the shared cooldown at the source: the error is
                    // about to be wrapped/retried, losing its type (mirrors
                    // `call_with_retry_and_balanced_model`).
                    if let LlmError::RateLimit { retry_after } = &e {
                        self.record_rate_limit(&role, *retry_after).await;
                    }
                    if !e.is_retryable() {
                        return Err(e);
                    }
                    self.record_failure(&role).await;
                    tracing::debug!(
                        "primary stream failed after its retry budget: {}, switching to balanced model",
                        e
                    );
                    self.balanced_model_active.store(true, Ordering::SeqCst);
                    hooks.on_attempt_start.lock().unwrap()(true);
                    let fb_result = streaming::aggregate_stream_with_retry_before_output(
                            self.balanced_model.clone(),
                            stream_context,
                            hooks.on_chunk.clone(),
                            cancel,
                            &self.stream_rules,
                            idle_dur,
                            fallback_policy.retry,
                        )
                        .await;

                    match fb_result {
                        Ok(resp) => {
                            self.record_success(&EndpointRole::BalancedModel).await;
                            Ok(resp)
                        }
                        Err(fb_err) => {
                            if let LlmError::RateLimit { retry_after } = &fb_err {
                                self.record_rate_limit(&EndpointRole::BalancedModel, *retry_after)
                                    .await;
                            }
                            self.record_failure(&EndpointRole::BalancedModel).await;
                            Err(LlmError::AllEndpointsFailed(e.to_string(), fb_err.to_string()))
                        }
                    }
                }
            }
        })
        .await
    }

    /// §3.7: Set the active stream rules.
    pub async fn set_stream_rules(&self, rules: Vec<StreamRule>) {
        *self.stream_rules.write().await = rules;
    }

    /// §3.7: Check accumulated output text against active stream rules.
    /// Returns the first matching rule, if any.
    pub async fn check_stream_output(&self, text: &str) -> Option<StreamRuleMatch> {
        let rules = self.stream_rules.read().await;
        check_stream_rules(&rules, text)
    }

    pub async fn health_check(&self, role: EndpointRole) -> Result<(), LlmError> {
        let endpoint = self.select_endpoint(role);
        endpoint.health_check().await
    }

    /// Tri-state connectivity probe for the top-right status chip.
    ///
    /// - A role without a configured api_key short-circuits to
    ///   [`LlmConnectionStatus::Unconfigured`] **without any network I/O** —
    ///   neither the TCP/TLS handshake nor the GET is attempted, so an
    ///   unset-up install never wastes a probe on a bare default base_url.
    /// - A configured role runs the same `/models` health check as
    ///   [`LlmRouter::health_check`] and reports Ready on success /
    ///   Disconnected on failure.
    pub async fn connection_status(&self, role: EndpointRole) -> LlmConnectionStatus {
        if !self.is_role_configured(role).await {
            return LlmConnectionStatus::Unconfigured;
        }
        match self.health_check(role).await {
            Ok(()) => LlmConnectionStatus::Ready,
            Err(e) => {
                tracing::debug!("LLM connection probe failed for {}: {}", role.as_str(), e);
                LlmConnectionStatus::Disconnected
            }
        }
    }

    /// Pre-warm HTTP connections for every configured endpoint so the first
    /// request to any model slot skips TCP+TLS handshake (~50-200ms).
    /// Unconfigured roles (empty `api_key`) are skipped. Each endpoint is
    /// checked concurrently and retried once on transient failure.
    pub async fn prewarm_all(&self) {
        let cfg = self.config.read().await;
        let configured: Vec<EndpointRole> = EndpointRole::ALL
            .iter()
            .copied()
            .filter(|role| cfg.is_configured(*role))
            .collect();
        drop(cfg);

        if configured.is_empty() {
            tracing::info!("LLM pre-warm skipped: no configured endpoints");
            return;
        }

        let roles = configured.clone();
        let checks = roles.into_iter().map(|role| async move {
            let first = self.health_check(role).await;
            if first.is_err() {
                // One retry: transient failures (conn reset, 5xx) should not
                // leave the pool cold for the first user message.
                self.health_check(role).await
            } else {
                first
            }
        });
        let results = join_all(checks).await;

        let mut ok = 0;
        for (role, result) in configured.iter().zip(results.iter()) {
            match result {
                Ok(()) => {
                    ok += 1;
                    tracing::debug!("LLM endpoint {} pre-warmed", role.as_str());
                }
                Err(e) => tracing::warn!(
                    "LLM pre-warm failed for {} (will retry on first request): {}",
                    role.as_str(),
                    e
                ),
            }
        }
        tracing::info!(
            "LLM pre-warm finished: {ok}/{} endpoints warmed",
            results.len()
        );
    }

    pub async fn config(&self) -> tokio::sync::RwLockReadGuard<'_, RouterConfig> {
        self.config.read().await
    }

    /// Compute USD cost with provider-normalized cache accounting. Unset
    /// cache-lane prices fall back to the ordinary input rate.
    pub async fn compute_cost(&self, role: EndpointRole, usage: &Usage) -> Option<f64> {
        let cfg = self.config.read().await;
        compute_cost_usd(
            cfg.endpoint(role),
            usage.cache_miss_tokens(),
            usage.cached_tokens,
            usage.cache_creation_tokens,
            usage.completion_tokens,
        )
    }

    pub fn balanced_model_active(&self) -> bool {
        self.balanced_model_active.load(Ordering::SeqCst)
    }

    /// §5.4: run health check on balanced model endpoint
    pub async fn background_health_check(&self) -> bool {
        match self.balanced_model.health_check().await {
            Ok(()) => {
                self.balanced_model_active.store(false, Ordering::SeqCst);
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_rules::StreamRuleMode;
    use crate::streaming::{IDLE_SCALE_CAP_SECS, estimate_prompt_tokens, scale_stream_idle};
    use crate::types::{FinishReason, LlmError::Unknown, Usage};
    use async_trait::async_trait;
    use futures_util::stream;
    use haven_common::types::CanonicalToolCall;
    use std::pin::Pin;

    fn llm_message(content: Vec<ContentPart>) -> CanonicalMessage {
        CanonicalMessage {
            role: haven_common::types::CanonicalRole::User,
            content,
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    #[test]
    fn scale_stream_idle_is_identity_for_empty_or_small_prompts() {
        assert_eq!(
            scale_stream_idle(Duration::from_secs(20), &[]),
            Duration::from_secs(20)
        );
        // Under 1k estimated tokens: no extra budget.
        let small = vec![llm_message(vec![ContentPart::Text("x".repeat(3_000))])];
        assert_eq!(
            scale_stream_idle(Duration::from_secs(20), &small),
            Duration::from_secs(20)
        );
    }

    #[test]
    fn scale_stream_idle_grows_with_prompt_size() {
        // 40k chars ≈ 10k tokens → +20s on top of the 20s base.
        let msgs = vec![llm_message(vec![ContentPart::Text("x".repeat(40_000))])];
        assert_eq!(
            scale_stream_idle(Duration::from_secs(20), &msgs),
            Duration::from_secs(40)
        );
        // The estimate covers every part across all messages.
        let two = vec![
            llm_message(vec![ContentPart::Text("y".repeat(20_000))]),
            llm_message(vec![ContentPart::Text("z".repeat(20_000))]),
        ];
        assert_eq!(
            scale_stream_idle(Duration::from_secs(20), &two),
            Duration::from_secs(40)
        );
    }

    #[test]
    fn scale_stream_idle_is_capped_and_never_zero() {
        // A huge prompt cannot push the window past IDLE_SCALE_CAP_SECS.
        let huge = vec![llm_message(vec![ContentPart::Text("x".repeat(10_000_000))])];
        assert_eq!(
            scale_stream_idle(Duration::from_secs(20), &huge),
            Duration::from_secs(IDLE_SCALE_CAP_SECS)
        );
        // A hand-edited 0 base stays clamped to >= 1s.
        let msgs = vec![llm_message(vec![ContentPart::Text("x".repeat(8_000))])];
        assert_eq!(
            scale_stream_idle(Duration::ZERO, &msgs),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn estimate_prompt_tokens_counts_images_audio_and_tool_arguments() {
        let mut msg = llm_message(vec![ContentPart::Text("a".repeat(400))]);
        msg.content.push(ContentPart::Image {
            content_type: "image".into(),
            media_type: "image/png".into(),
            data: "base64".into(),
        });
        msg.tool_calls = Some(vec![CanonicalToolCall {
            id: "call-1".into(),
            name: "shell".into(),
            arguments: serde_json::json!({"cmd": "echo hi"}),
        }]);
        msg.reasoning = Some("reasoning text".repeat(100));
        let tokens = estimate_prompt_tokens(&[msg]);
        // 100 text chars ≈ 25 + 1k for the image + ~19 argument chars / 4 +
        // 1300 reasoning chars / 4 ≈ 325.
        assert!(tokens > 1_300, "estimated {} tokens", tokens);
    }

    struct MockStreamClient {
        chunks: Vec<Result<StreamChunk, LlmError>>,
        fail_chat: bool,
    }

    #[async_trait]
    impl LlmClient for MockStreamClient {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            if self.fail_chat {
                Err(LlmError::ServerError("mock: chat failed".into()))
            } else {
                Ok(LlmResponse {
                    text: "mock response".into(),
                    tool_calls: Vec::new(),
                    finish_reason: Some(FinishReason::Stop),
                    usage: Usage::default(),
                    model: None,
                    reasoning: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                })
            }
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            if self.fail_chat {
                Err(LlmError::ServerError("mock: chat_with_tools failed".into()))
            } else {
                Ok(LlmResponse {
                    text: "mock response".into(),
                    tool_calls: Vec::new(),
                    finish_reason: Some(FinishReason::Stop),
                    usage: Usage::default(),
                    model: None,
                    reasoning: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                })
            }
        }
        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            if self.fail_chat {
                Err(LlmError::ServerError("mock: chat_stream failed".into()))
            } else {
                Ok(Box::pin(stream::iter(self.chunks.clone())))
            }
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            if self.fail_chat {
                Err(Unknown("mock: chat_stream_with_tools failed".into()))
            } else {
                Ok(Box::pin(stream::iter(self.chunks.clone())))
            }
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[test]
    fn router_selects_correct_endpoint() {
        let cfg = RouterConfig::default();
        let router = LlmRouter::new(cfg);
        let _sm = router.select_endpoint(EndpointRole::SmallModel);
        let _re = router.select_endpoint(EndpointRole::DefaultModel);
        let _fa = router.select_endpoint(EndpointRole::BalancedModel);
        let _mm = router.select_endpoint(EndpointRole::ImageModel);
        let _au = router.select_endpoint(EndpointRole::AudioModel);
        let _em = router.select_endpoint(EndpointRole::EmbeddingModel);
    }

    #[tokio::test]
    async fn is_role_configured_reports_api_key_state() {
        let mut cfg = RouterConfig::default();
        cfg.small_model.api_key = "sk-test".into();
        cfg.default_model.api_key = String::new();
        cfg.balanced_model.api_key = "sk-bal".into();
        cfg.image_model.api_key = "sk-mm".into();
        cfg.audio_model.api_key = "sk-au".into();
        cfg.embedding_model.api_key = "sk-emb".into();
        let router = LlmRouter::new(cfg);
        assert!(
            router.is_role_configured(EndpointRole::SmallModel).await,
            "small_model api_key is set"
        );
        assert!(
            !router.is_role_configured(EndpointRole::DefaultModel).await,
            "default_model api_key is empty"
        );
        assert!(
            router.is_role_configured(EndpointRole::BalancedModel).await,
            "balanced_model api_key is set"
        );
        assert!(
            router.is_role_configured(EndpointRole::ImageModel).await,
            "image_model api_key is set"
        );
        assert!(
            router.is_role_configured(EndpointRole::AudioModel).await,
            "audio_model api_key is set"
        );
        assert!(
            router
                .is_role_configured(EndpointRole::EmbeddingModel)
                .await,
            "embedding_model api_key is set"
        );
    }

    #[tokio::test]
    async fn embed_routes_to_embedding_endpoint_and_tracks_health() {
        struct MockEmbedClient;
        #[async_trait]
        impl LlmClient for MockEmbedClient {
            async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
                Err(LlmError::Unknown("mock: no chat".into()))
            }
            async fn chat_stream(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                Err(LlmError::Unknown("mock: no stream".into()))
            }
            async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
                Ok(Embedding {
                    vectors: input.iter().map(|_| vec![1.0f32, 0.0]).collect(),
                    model: Some("test-emb".into()),
                    usage: Usage::default(),
                })
            }
            async fn health_check(&self) -> Result<(), LlmError> {
                Ok(())
            }
        }
        let chat: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        });
        let emb: Arc<dyn LlmClient> = Arc::new(MockEmbedClient);
        let router = LlmRouter::new_with_clients_full(
            chat.clone(),
            chat.clone(),
            chat.clone(),
            chat.clone(),
            chat,
            emb,
        );
        let result = router.embed(vec!["a".into(), "b".into()]).await.unwrap();
        assert_eq!(result.vectors.len(), 2);
        assert_eq!(result.vectors[0], vec![1.0f32, 0.0]);
        assert_eq!(result.model.as_deref(), Some("test-emb"));

        // Embedding failures trip the embedding endpoint's circuit breaker.
        struct FailingEmbed;
        #[async_trait]
        impl LlmClient for FailingEmbed {
            async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
                Err(LlmError::Unknown("mock: no chat".into()))
            }
            async fn chat_stream(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                Err(LlmError::Unknown("mock: no stream".into()))
            }
            async fn embed(&self, _: Vec<String>) -> Result<Embedding, LlmError> {
                Err(LlmError::ServerError("boom".into()))
            }
            async fn health_check(&self) -> Result<(), LlmError> {
                Ok(())
            }
        }
        let chat: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        });
        let router = LlmRouter::new_with_clients_full(
            chat.clone(),
            chat.clone(),
            chat.clone(),
            chat.clone(),
            chat,
            Arc::new(FailingEmbed),
        );
        assert!(router.embed(vec!["x".into()]).await.is_err());
    }

    #[tokio::test]
    async fn chat_stream_with_tools_aggregated_accumulates_text_and_tool_calls() {
        let chunks: Vec<Result<StreamChunk, LlmError>> = vec![
            Ok(StreamChunk {
                text: Some("Hello ".into()),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            Ok(StreamChunk {
                text: Some("world!".into()),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            Ok(StreamChunk {
                text: None,
                tool_calls: vec![CanonicalToolCall {
                    id: "tc_1".into(),
                    name: "file".into(),
                    arguments: serde_json::json!({"operation": "read", "path": "."}),
                }],
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            Ok(StreamChunk {
                text: None,
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::ToolCalls),
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    ..Default::default()
                }),
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ];

        let client = Arc::new(MockStreamClient {
            chunks,
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );

        use std::sync::Arc as StdArc;
        use std::sync::Mutex as StdMutex;
        let seen_text = StdArc::new(StdMutex::new(String::new()));
        let seen_clone = seen_text.clone();
        let resp = router
            .chat_stream_with_tools_aggregated(EndpointRole::DefaultModel, &[], &[], move |c| {
                if let Some(t) = &c.text {
                    seen_clone.lock().unwrap().push_str(t);
                }
            })
            .await
            .expect("aggregation succeeds");

        assert_eq!(resp.text, "Hello world!");
        assert_eq!(
            *seen_text.lock().unwrap(),
            "Hello world!",
            "on_chunk must see every text delta"
        );
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "file");
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(resp.usage.total_tokens, 15);
        assert!(
            !router.balanced_model_active(),
            "primary succeeded, no balanced model"
        );
    }

    /// Mock whose stream sleeps `first_delay` before the first chunk and
    /// `gap_delay` between the two text chunks, so the first-chunk grace and
    /// the data-gap idle timeout can be exercised independently.
    struct SlowStreamClient {
        first_delay: Duration,
        gap_delay: Duration,
    }

    #[async_trait]
    impl LlmClient for SlowStreamClient {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(Unknown("mock: no chat".into()))
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            Err(Unknown("mock: no chat_with_tools".into()))
        }
        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(Unknown("mock: no chat_stream".into()))
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let first_delay = self.first_delay;
            let gap_delay = self.gap_delay;
            let mk = |text: &'static str| {
                Ok(StreamChunk {
                    text: Some(text.into()),
                    tool_calls: Vec::new(),
                    finish_reason: None,
                    usage: None,
                    model: None,
                    reasoning: None,
                    web_search: None,
                    web_search_calls: Vec::new(),
                    thinking_blocks: Vec::new(),
                })
            };
            Ok(Box::pin(stream::unfold(0u8, move |i| async move {
                match i {
                    0 => {
                        tokio::time::sleep(first_delay).await;
                        Some((mk("hello"), 1))
                    }
                    1 => {
                        tokio::time::sleep(gap_delay).await;
                        Some((mk(" world"), 2))
                    }
                    _ => None,
                }
            })))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    async fn aggregate_direct(
        client: Arc<dyn LlmClient>,
        idle_timeout: Duration,
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
    ) -> Result<LlmResponse, LlmError> {
        let on_chunk = Arc::new(StdMutex::new(on_chunk));
        let rules = RwLock::new(Vec::<StreamRule>::new());
        crate::streaming::aggregate_stream_cancellable(
            client,
            Vec::new(),
            Vec::new(),
            on_chunk,
            CancellationToken::new(),
            &rules,
            idle_timeout,
        )
        .await
    }

    #[tokio::test]
    async fn stream_first_chunk_grace_tolerates_slow_start() {
        // First chunk arrives at 3s — far beyond the 1s idle timeout, but
        // within the 60s first-chunk grace: the slow start must NOT abort.
        let client: Arc<dyn LlmClient> = Arc::new(SlowStreamClient {
            first_delay: Duration::from_secs(3),
            gap_delay: Duration::ZERO,
        });
        let resp = aggregate_direct(client, Duration::from_secs(1), |_| {})
            .await
            .expect("slow first chunk must be tolerated by the grace window");
        assert_eq!(resp.text, "hello world");
    }

    #[tokio::test]
    async fn stream_data_gap_idle_timeout_aborts_after_first_chunk() {
        // First chunk arrives immediately; the second is 3s late — past the
        // 1s idle timeout. Once data is flowing, gaps are bounded tightly.
        let client: Arc<dyn LlmClient> = Arc::new(SlowStreamClient {
            first_delay: Duration::ZERO,
            gap_delay: Duration::from_secs(3),
        });
        let err = aggregate_direct(client, Duration::from_secs(1), |_| {})
            .await
            .expect_err("mid-stream gap past the idle timeout must abort");
        assert!(err.to_string().contains("idle timeout"));
    }

    #[tokio::test]
    async fn aggregate_stream_forwards_web_search_phases_and_collects_calls() {
        use crate::types::{WebSearchPhase, WebSearchUpdate};
        let chunks: Vec<Result<StreamChunk, LlmError>> = vec![
            Ok(StreamChunk {
                text: None,
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: Some(
                    WebSearchUpdate::new(WebSearchPhase::InProgress)
                        .with_meta(Some("ws_1".into()), Some("search".into())),
                ),
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            Ok(StreamChunk {
                text: None,
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: Some(
                    WebSearchUpdate::new(WebSearchPhase::Searching)
                        .with_meta(Some("ws_1".into()), Some("search".into())),
                ),
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
            Ok(StreamChunk {
                text: None,
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
                web_search: Some(
                    WebSearchUpdate::new(WebSearchPhase::Completed)
                        .with_meta(Some("ws_1".into()), Some("search".into())),
                ),
                web_search_calls: vec![serde_json::json!({
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed"
                })],
                thinking_blocks: Vec::new(),
            }),
            Ok(StreamChunk {
                text: Some("answer with citations".into()),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }),
        ];

        let client = Arc::new(MockStreamClient {
            chunks,
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );

        use std::sync::Arc as StdArc;
        use std::sync::Mutex as StdMutex;
        let phases = StdArc::new(StdMutex::new(Vec::new()));
        let phases_clone = phases.clone();
        let resp = router
            .chat_stream_with_tools_aggregated(EndpointRole::DefaultModel, &[], &[], move |c| {
                if let Some(p) = &c.web_search {
                    phases_clone
                        .lock()
                        .unwrap()
                        .push(p.phase.as_str().to_string());
                }
            })
            .await
            .expect("aggregation succeeds");

        assert_eq!(
            *phases.lock().unwrap(),
            vec!["in_progress", "searching", "completed"],
            "on_chunk must observe every web search phase in order"
        );
        assert_eq!(resp.text, "answer with citations");
        assert_eq!(resp.web_search_calls.len(), 1);
        assert_eq!(resp.web_search_calls[0]["id"], "ws_1");
    }

    #[tokio::test]
    async fn chat_balanced_model_on_primary_failure() {
        let failing = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let ok = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                text: Some("balanced model response".into()),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;

        let router = LlmRouter::new_with_clients(
            failing.clone(),
            failing.clone(),
            ok.clone(),
            ok.clone(),
            ok,
        );
        *router.config.write().await = RouterConfig {
            retry_base_secs: 0,
            retry_factor: 1,
            retry_max_secs: 0,
            retry_jitter: 0.0,
            ..Default::default()
        };

        let resp = router
            .chat(EndpointRole::DefaultModel, Vec::new())
            .await
            .expect("balanced model should succeed");
        assert_eq!(resp.text, "mock response");
        assert!(router.balanced_model_active());
    }

    #[tokio::test]
    async fn chat_small_model_uses_small_model_endpoint() {
        // Small model role should be routed to the small_model slot, and the
        // balanced model should NOT be activated when it succeeds.
        let small = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let default = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let balanced = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;

        let router = LlmRouter::new_with_clients(
            small,
            default,
            balanced.clone(),
            balanced.clone(),
            balanced,
        );

        let resp = router
            .chat(EndpointRole::SmallModel, Vec::new())
            .await
            .expect("small_model should succeed");
        assert_eq!(resp.text, "mock response");
        assert!(
            !router.balanced_model_active(),
            "small_model succeeded directly; balanced model should not be active"
        );
    }

    #[tokio::test]
    async fn circuit_breaker_opens_after_failures() {
        let failing = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let ok = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                text: Some("ok".into()),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;

        let router = LlmRouter::new_with_clients(
            failing.clone(),
            failing.clone(),
            ok.clone(),
            ok.clone(),
            ok,
        );

        // First 3 calls should fail and trigger circuit breaker
        for _ in 0..3 {
            let _ = router.chat(EndpointRole::DefaultModel, Vec::new()).await;
        }

        // Circuit breaker should reject requests directly
        let result = router.check_circuit(&EndpointRole::DefaultModel).await;
        // Should fail because circuit is open
        assert!(result.is_err());
    }

    #[test]
    fn circuit_breaker_new_is_closed() {
        let mut cb = CircuitBreaker::new();
        assert!(cb.allow_request());
    }

    #[test]
    fn circuit_breaker_record_success_resets_consecutive_failures() {
        let mut cb = CircuitBreaker::new();
        cb.consecutive_failures = 5;
        cb.failure_count = 5;
        cb.total_calls = 5;
        cb.record_success();
        assert_eq!(cb.consecutive_failures, 0);
        assert_eq!(cb.total_calls, 6);
        assert_eq!(cb.state, CircuitState::Closed);
        assert!(cb.opened_at.is_none());
    }

    #[test]
    fn circuit_breaker_success_does_not_close_open_breaker() {
        // A stale success from a request dispatched before the breaker tripped
        // must NOT close it — only a HalfOpen probe may (M8).
        let mut cb = CircuitBreaker::new();
        cb.state = CircuitState::Open;
        cb.opened_at = Some(Instant::now());
        cb.consecutive_failures = 3;
        cb.record_success();
        assert_eq!(cb.state, CircuitState::Open, "open breaker stays open");
        assert_eq!(cb.consecutive_failures, 3, "counters not reset");
        assert!(cb.opened_at.is_some());
        // Simulate the cooldown elapsing: probe goes HalfOpen, its success closes.
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert_eq!(cb.state, CircuitState::HalfOpen);
        cb.record_success();
        assert_eq!(cb.state, CircuitState::Closed);
    }

    #[test]
    fn circuit_breaker_record_failure_increments_counters() {
        let mut cb = CircuitBreaker::new();
        cb.record_failure();
        assert_eq!(cb.consecutive_failures, 1);
        assert_eq!(cb.failure_count, 1);
        assert_eq!(cb.total_calls, 1);
    }

    #[test]
    fn circuit_breaker_opens_at_threshold() {
        let mut cb = CircuitBreaker::new();
        cb.record_failure();
        cb.record_failure();
        assert!(cb.allow_request());
        cb.record_failure();
        // 3 consecutive out of 3 total > 50% → opens
        assert!(!cb.allow_request());
    }

    #[test]
    fn circuit_breaker_half_open_after_cooldown() {
        let mut cb = CircuitBreaker::new();
        // Force open with a past timestamp
        cb.state = CircuitState::Open;
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert_eq!(cb.state, CircuitState::HalfOpen);
    }

    #[test]
    fn circuit_breaker_stays_open_within_cooldown() {
        let mut cb = CircuitBreaker::new();
        cb.state = CircuitState::Open;
        cb.opened_at = Some(Instant::now());
        assert!(!cb.allow_request());
    }

    #[test]
    fn circuit_breaker_full_state_transition_cycle() {
        let mut cb = CircuitBreaker::new();
        // Closed → Open
        for _ in 0..3 {
            cb.record_failure();
        }
        assert!(!cb.allow_request());
        // Open → HalfOpen (simulate cooldown elapsed)
        cb.state = CircuitState::Open;
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert_eq!(cb.state, CircuitState::HalfOpen);
        // HalfOpen → Closed (on success)
        cb.record_success();
        assert_eq!(cb.state, CircuitState::Closed);
    }

    #[test]
    fn endpoint_health_tracks_consecutive_failures() {
        let mut health = EndpointHealth::new();
        assert!(health.is_healthy);
        health.record_failure();
        health.record_failure();
        assert!(health.is_healthy);
        health.record_failure();
        assert!(!health.is_healthy);
    }

    #[test]
    fn endpoint_health_success_resets_failure_count() {
        let mut health = EndpointHealth::new();
        health.record_failure();
        health.record_failure();
        assert!(health.is_healthy);
        // A success while the breaker is still Closed (or HalfOpen) resets.
        health.record_success();
        assert!(health.is_healthy);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn endpoint_health_success_does_not_recover_open_breaker() {
        let mut health = EndpointHealth::new();
        health.record_failure();
        health.record_failure();
        health.record_failure();
        assert!(!health.is_healthy);
        assert!(!health.allow_request(), "breaker is Open");
        // A stale success from a pre-open request must NOT mark it healthy.
        health.record_success();
        assert!(!health.is_healthy);
        assert_eq!(health.consecutive_failures, 3);
        // Only a HalfOpen probe success (after cooldown) recovers it.
        health.circuit_breaker.opened_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(31));
        assert!(health.allow_request(), "Open → HalfOpen after cooldown");
        health.record_success();
        assert!(health.is_healthy);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn endpoint_health_allow_request_delegates_to_circuit_breaker() {
        let mut health = EndpointHealth::new();
        assert!(health.allow_request());
    }

    #[test]
    fn balanced_model_active_defaults_to_false() {
        let cfg = RouterConfig::default();
        let router = LlmRouter::new(cfg);
        assert!(!router.balanced_model_active());
    }

    #[tokio::test]
    async fn production_router_seeds_code_block_abort_rule() {
        let router = LlmRouter::new(RouterConfig::default());
        let matched = router
            .check_stream_output("here:\n```rust\nfn main() {}\n```")
            .await;
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().mode, StreamRuleMode::Abort);
        // Test constructors keep an empty rule list.
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let test_router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        assert!(
            test_router
                .check_stream_output("here:\n```rust\nfn main() {}\n```")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn health_check_healthy_mock_endpoint() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        let result = router.health_check(EndpointRole::DefaultModel).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_stream_rules_and_check_output() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        let rule = StreamRule::new(
            "forbidden",
            r"secret_key",
            "do not reveal keys",
            StreamRuleMode::Abort,
        )
        .unwrap();
        router.set_stream_rules(vec![rule]).await;
        let result = router.check_stream_output("this is safe text").await;
        assert!(result.is_none());
        let result = router
            .check_stream_output("here is secret_key=abc123")
            .await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().rule_name, "forbidden");
    }

    #[test]
    fn endpoint_role_health_index_mapping() {
        assert_eq!(LlmRouter::health_index(&EndpointRole::SmallModel), 0);
        assert_eq!(LlmRouter::health_index(&EndpointRole::DefaultModel), 1);
        assert_eq!(LlmRouter::health_index(&EndpointRole::BalancedModel), 2);
        assert_eq!(LlmRouter::health_index(&EndpointRole::ImageModel), 3);
        assert_eq!(LlmRouter::health_index(&EndpointRole::AudioModel), 4);
        assert_eq!(LlmRouter::health_index(&EndpointRole::EmbeddingModel), 5);
    }

    #[tokio::test]
    async fn chat_stream_with_tools_no_chunks() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        let resp = router
            .chat_stream_with_tools_aggregated(EndpointRole::DefaultModel, &[], &[], |_| {})
            .await
            .expect("aggregation succeeds");
        assert!(resp.text.is_empty());
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn chat_stream_balanced_model_on_primary_failure() {
        let failing = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let ok = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                text: Some("balanced".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            failing.clone(),
            failing.clone(),
            ok.clone(),
            ok.clone(),
            ok,
        );
        let resp = router.chat_stream(EndpointRole::DefaultModel, vec![]).await;
        assert!(resp.is_ok());
        assert!(router.balanced_model_active());
    }

    #[tokio::test]
    async fn chat_stream_call_succeeds_primary() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                text: Some("hi".into()),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                reasoning: None,
                web_search: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        let result = router.chat_stream(EndpointRole::DefaultModel, vec![]).await;
        assert!(result.is_ok());
        assert!(!router.balanced_model_active());
    }

    /// Mock that tracks how many calls are in flight concurrently and stalls
    /// briefly, so the per-role semaphore's serialization is observable.
    struct ConcurrencyProbe {
        concurrent: Arc<std::sync::atomic::AtomicUsize>,
        max_seen: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl LlmClient for ConcurrencyProbe {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            let now = self
                .concurrent
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            self.max_seen
                .fetch_max(now, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            self.concurrent
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            Ok(LlmResponse {
                text: "probe".into(),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: Usage::default(),
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown("probe: no stream".into()))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn per_role_semaphore_caps_concurrent_requests() {
        let concurrent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe: Arc<dyn LlmClient> = Arc::new(ConcurrencyProbe {
            concurrent: concurrent.clone(),
            max_seen: max_seen.clone(),
        });
        let router = LlmRouter::new_with_clients(
            probe.clone(),
            probe.clone(),
            probe.clone(),
            probe.clone(),
            probe,
        );
        // Cap the default-model role at 1 in-flight request.
        router.set_request_limit_for_test(1);

        let router = Arc::new(router);
        let mut handles = Vec::new();
        for _ in 0..3 {
            let router = router.clone();
            handles.push(tokio::spawn(async move {
                router
                    .chat(EndpointRole::DefaultModel, vec![])
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            max_seen.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "per-role limit 1 must serialize concurrent calls to the same role"
        );
        // Different roles have independent permits: small_model can proceed
        // while default_model is capped.
        router.set_request_limit_for_test(1);
        let _ = router.chat(EndpointRole::SmallModel, vec![]).await.unwrap();
        assert_eq!(max_seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// Mock that ALWAYS returns RateLimit (with Retry-After), so the shared
    /// cooldown's effect on subsequent callers is observable.
    struct AlwaysRateLimited;

    #[async_trait]
    impl LlmClient for AlwaysRateLimited {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::RateLimit {
                retry_after: Some(Duration::from_millis(300)),
            })
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::RateLimit {
                retry_after: Some(Duration::from_millis(300)),
            })
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn rate_limit_sets_shared_cooldown_for_role() {
        let client: Arc<dyn LlmClient> = Arc::new(AlwaysRateLimited);
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        // Fast retry pacing so the RateLimit error surfaces immediately.
        let cfg = RouterConfig {
            retry_max_retries: 0,
            fallback_retry_max_retries: 0,
            retry_base_secs: 0,
            retry_factor: 1,
            retry_max_secs: 0,
            retry_jitter: 0.0,
            ..Default::default()
        };
        *router.config.write().await = cfg;

        let role = EndpointRole::DefaultModel;
        let err = router.chat(role, vec![]).await.unwrap_err();
        assert!(
            matches!(
                err,
                LlmError::RateLimit { .. } | LlmError::AllEndpointsFailed(..)
            ),
            "first call must surface the RateLimit failure: {}",
            err
        );
        // The cooldown deadline was recorded (at the source, before the
        // AllEndpointsFailed wrap): subsequent callers wait it out.
        let deadline = router.rate_limit_deadline_for_test(&role).await;
        assert!(
            deadline.is_some_and(|d| d > Instant::now()),
            "cooldown deadline must be set in the future"
        );
        // A second call (a different session) paces behind the cooldown instead
        // of firing immediately: it must take at least the Retry-After before
        // dispatching (it fails again, but only after the shared wait).
        let t0 = Instant::now();
        let err2 = router.chat(role, vec![]).await.unwrap_err();
        assert!(matches!(
            err2,
            LlmError::RateLimit { .. } | LlmError::AllEndpointsFailed(..)
        ));
        assert!(
            t0.elapsed() >= Duration::from_millis(250),
            "second call must wait out the shared cooldown (elapsed {:?})",
            t0.elapsed()
        );
        // A third caller starting later re-uses the (still running) cooldown
        // window: the deadline only moves forward, never backward.
        let deadline2 = router.rate_limit_deadline_for_test(&role).await;
        assert!(
            deadline2.unwrap() >= deadline.unwrap(),
            "cooldown deadline must never shrink"
        );
    }

    #[test]
    fn router_clamps_max_tokens_to_context_window() {
        // A huge response-cap floor (e.g. the 128k default) must not be sent
        // raw to providers with smaller output budgets: Anthropic/OpenAI/Gemini
        // reject max_tokens above the model limit with HTTP 400.
        let mut cfg = RouterConfig::default();
        cfg.default_model.model_name = "gpt-4o-mini".into(); // catalog: 128k
        cfg.default_model.max_tokens = 1_000_000; // absurd cap floor
        let router = LlmRouter::new(cfg);
        let built = router.config.try_read().expect("router config readable");
        assert!(
            built.default_model.max_tokens <= 128_000,
            "max_tokens must be clamped to the resolved context window, got {}",
            built.default_model.max_tokens
        );
    }
}
