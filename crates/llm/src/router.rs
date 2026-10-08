use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::adapters::adapter_for;
use crate::aggregated_stream_executor::{
    ActiveStreamHooks, AggregatedStreamExecutor, StreamContext,
};
use crate::call_executor::CallExecutor;
use crate::client::{LlmClient, endpoint_host};
#[cfg(test)]
use crate::endpoint_circuit_breaker::EndpointCircuitState;
use crate::endpoint_circuit_breaker::{
    EndpointCircuitBreaker, EndpointCircuitBreakerMap, new_endpoint_circuit_breaker_map,
};
use crate::model_directory::{ModelDirectory, ResolvedModelClient, RouteMode};
use crate::request_descriptor::RequestDescriptor;
use crate::request_pipeline::{
    RequestExecutionPolicy, RequestOutcome, execute_with_retry, execute_with_timeout,
};
use crate::stream_executor::StreamExecutor;
use haven_common::types::{CanonicalMessage, ContentPart};

use crate::stream_rules::{StreamRule, StreamRuleMatch, check_stream_rules};
use crate::types::{
    CompleteRequest, Embedding, EmbeddingRequest, HealthCheckRequest, LlmConnectionReport,
    LlmConnectionStatus, LlmError, LlmResponse, LlmToolDefinition, PromptRequest, StreamChunk,
    StreamRequest, Usage,
};
use futures_util::future::join_all;
use haven_common::config::{
    Capability, ModelEndpoint, RequestKind, RoutedModel, RouterConfig, compute_cost_usd,
};
use haven_common::media::CapabilityProfile;

// ---------------------------------------------------------------------------
// §2.6: Per-endpoint circuit breaker state
// ---------------------------------------------------------------------------

/// Mutable state shared by all `LlmRouter` constructors.
struct LlmRouterRuntimeState {
    endpoint_circuits: RwLock<EndpointCircuitBreakerMap>,
    stream_rules: RwLock<Vec<StreamRule>>,
    semaphores: StdMutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    rate_limited: RwLock<HashMap<String, Instant>>,
}

pub struct LlmRouter {
    config: Arc<RwLock<RouterConfig>>,
    /// Fallback context window used when an endpoint does not declare one.
    /// Kept on the router so per-request output caps use the same resolved
    /// window as construction-time max-token clamping.
    default_context_window: u32,
    /// Provider clients and request-kind → primary model identity directory.
    /// Endpoint metadata is always read from `config`, the router's sole
    /// configuration snapshot.
    model_directory: ModelDirectory,
    // §5.3: per configured model circuit state. A model shared by several
    // request policies has one breaker, regardless of which policy selected it.
    endpoint_circuits: RwLock<EndpointCircuitBreakerMap>,
    /// Stream rules that are checked against accumulated output (§3.7)
    stream_rules: RwLock<Vec<StreamRule>>,
    /// Per-model concurrency limit: at most `llm.max_concurrent_requests`
    /// requests may be in flight per routed model. Parallel request kinds that
    /// share a model queue here instead of piling onto the provider.
    /// Semaphores are created from the config at construction; a settings
    /// save rebuilds the router (`hot_swap_router`), so the limit is
    /// applied to new requests immediately. The mutex is only held to clone
    /// an `Arc<Semaphore>` (never across an await), so it adds no contention.
    semaphores: StdMutex<HashMap<String, Arc<tokio::sync::Semaphore>>>,
    /// Shared rate-limit cooldown per model: when a request ends with a 429
    /// (RateLimit), subsequent callers to the same model wait until the
    /// deadline before dispatching, so a burst of parallel sessions does not
    /// retry simultaneously and amplify the load.
    rate_limited: RwLock<HashMap<String, Instant>>,
}

/// How a new provider attempt treats output already visible to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamAttemptOutputDisposition {
    /// Keep the output from the existing provider attempt.
    PreserveExisting,
    /// Clear the output from the existing provider attempt before new chunks.
    ReplaceExisting,
}

/// Callbacks and output policy for one routed streaming request.
///
/// The provider may make several attempts for one logical Agent turn. The
/// chunk callback receives provider deltas; the attempt callback receives the
/// disposition for output already visible. Keeping these controls
/// together prevents callers from accidentally handling retry boundaries as
/// ordinary chunks.
pub struct StreamAttemptHooks {
    on_chunk: Box<dyn FnMut(&StreamChunk) + Send + 'static>,
    on_attempt_start: Box<dyn FnMut(StreamAttemptOutputDisposition) + Send + 'static>,
    output_disposition_on_start: StreamAttemptOutputDisposition,
}

impl StreamAttemptHooks {
    pub fn new(
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
        on_attempt_start: impl FnMut(StreamAttemptOutputDisposition) + Send + 'static,
        output_disposition_on_start: StreamAttemptOutputDisposition,
    ) -> Self {
        Self {
            on_chunk: Box::new(on_chunk),
            on_attempt_start: Box::new(on_attempt_start),
            output_disposition_on_start,
        }
    }
}

impl LlmRouter {
    pub fn new(config: RouterConfig) -> Self {
        Self::with_default_context_window(config, crate::registry::FALLBACK_CONTEXT_WINDOW)
    }

    /// Like [`Self::new`], but clamp `max_tokens` against
    /// `context_limits.default_context_window` when a model has no explicit
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
        // configured caps are lifted so long outputs are not
        // truncated mid-stream.
        let fallback = if default_context_window > 0 {
            default_context_window
        } else {
            crate::registry::FALLBACK_CONTEXT_WINDOW
        };
        ModelDirectory::clamp_max_tokens_to_context_windows(&mut config, fallback);
        let model_directory = ModelDirectory::from_config(&config);
        let request_limit = Self::request_limit(&config);
        let LlmRouterRuntimeState {
            endpoint_circuits,
            stream_rules,
            semaphores,
            rate_limited,
        } = Self::runtime_state(
            request_limit,
            model_directory.model_ids().map(str::to_string),
        );
        Self {
            config: Arc::new(RwLock::new(config)),
            default_context_window: fallback,
            model_directory,
            endpoint_circuits,
            // Stream rules start empty; code-block output remains ordinary
            // assistant text unless a caller explicitly installs a rule.
            stream_rules,
            semaphores,
            rate_limited,
        }
    }

    /// Config-driven per-model request limit. Clamped to >= 1 so a hand-edited
    /// 0 (or a config without the new field) can never deadlock the router on
    /// an unacquirable permit.
    fn request_limit(config: &RouterConfig) -> usize {
        config.max_concurrent_requests.max(1)
    }

    /// Default runtime state shared by every constructor: per-model circuit
    /// breakers, stream rules, concurrency semaphores, and rate-limit flags.
    fn runtime_state(
        request_limit: usize,
        model_ids: impl IntoIterator<Item = String>,
    ) -> LlmRouterRuntimeState {
        let model_ids = model_ids.into_iter().collect::<Vec<_>>();
        LlmRouterRuntimeState {
            endpoint_circuits: RwLock::new(new_endpoint_circuit_breaker_map(
                model_ids.iter().cloned(),
            )),
            stream_rules: RwLock::new(Vec::new()),
            semaphores: StdMutex::new(Self::make_semaphores(request_limit, model_ids)),
            rate_limited: RwLock::new(HashMap::new()),
        }
    }

    /// Clone the concurrency permit for a model (the mutex is released before
    /// any await; the Arc clone is cheap).
    fn model_permit(&self, model_id: &str) -> Arc<tokio::sync::Semaphore> {
        self.semaphores
            .lock()
            .unwrap()
            .entry(model_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    }

    fn make_semaphores(
        limit: usize,
        model_ids: impl IntoIterator<Item = String>,
    ) -> HashMap<String, Arc<tokio::sync::Semaphore>> {
        model_ids
            .into_iter()
            .map(|id| (id, Arc::new(tokio::sync::Semaphore::new(limit))))
            .collect()
    }

    /// Wait out the shared rate-limit cooldown for a model (if any).
    async fn wait_rate_limit_cooldown(&self, model_id: &str) {
        let cooldown_until = self.rate_limited.read().await.get(model_id).copied();
        if let Some(until) = cooldown_until {
            let now = Instant::now();
            if until > now {
                tracing::debug!(
                    model_id,
                    wait = ?(until - now),
                    "router model is rate-limited, waiting before dispatch"
                );
                tokio::time::sleep(until - now).await;
            }
        }
    }

    /// Extend the shared cooldown for a model after a RateLimit result, so a
    /// burst of parallel sessions queues behind the longest wait instead of
    /// re-hammering the provider simultaneously.
    ///
    /// The wait is CLAMPED: `Retry-After` comes from the (possibly hostile or
    /// misbehaving) provider, and the cooldown blocks the whole model while
    /// holding a semaphore permit — an unbounded value would freeze every
    /// agent/embedding/STT request through that endpoint.
    async fn record_rate_limit(&self, model_id: &str, retry_after: Option<Duration>) {
        // Cap at 30s: long enough to ride out a provider-side rate window,
        // short enough that a hostile endpoint cannot pin the model.
        let wait = retry_after
            .unwrap_or(Duration::from_secs(5))
            .min(Duration::from_secs(30));
        let until = Instant::now() + wait;
        let mut rl = self.rate_limited.write().await;
        if rl.get(model_id).is_none_or(|current| *current < until) {
            rl.insert(model_id.to_string(), until);
        }
    }

    async fn acquire_model_permit(
        &self,
        model_id: &str,
    ) -> Result<tokio::sync::OwnedSemaphorePermit, LlmError> {
        self.model_permit(model_id)
            .acquire_owned()
            .await
            .map_err(|_| LlmError::ServerError("router semaphore closed".into()))
    }

    /// Run `f` under the selected model's concurrency permit, waiting out any shared
    /// rate-limit cooldown first. The permit is held across the WHOLE call
    /// (including retries and stream consumption), so the concurrency cap is
    /// real provider load, not just request starts.
    ///
    /// Cooldown projection happens at the request outcome boundary. Keeping it
    /// out of this permit wrapper ensures that a 429 is projected only once;
    /// this wrapper only paces the next call and holds the permit lifetime.
    async fn with_model_permit<T, F, Fut>(&self, model_id: String, f: F) -> Result<T, LlmError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, LlmError>>,
    {
        let permit = self.acquire_model_permit(&model_id).await?;
        self.wait_rate_limit_cooldown(&model_id).await;
        let result = f().await;
        drop(permit);
        result
    }

    /// Resolve a request, then run it under the selected model's circuit,
    /// concurrency and rate-limit state. The model id is passed to the
    /// operation so health accounting stays keyed to the selected model.
    async fn with_request_permit<T, F, Fut>(
        &self,
        descriptor: RequestDescriptor,
        f: F,
    ) -> Result<T, LlmError>
    where
        F: FnOnce(String, Arc<dyn LlmClient>) -> Fut,
        Fut: std::future::Future<Output = Result<T, LlmError>>,
    {
        let ResolvedModelClient { model_id, client } =
            self.model_directory.resolve_client(descriptor)?;
        let check_id = model_id.clone();
        self.with_model_permit(model_id.clone(), || async move {
            self.check_circuit(&check_id).await?;
            f(model_id, client).await
        })
        .await
    }

    /// Test utility: replace the per-model semaphores with a new limit, to
    /// exercise the concurrency cap without rebuilding the router with real
    /// HTTP adapters. Production limits come from `llm.max_concurrent_requests`
    /// at construction (and are refreshed by `hot_swap_router` on save).
    #[cfg(test)]
    fn set_request_limit_for_test(&self, limit: usize) {
        let mut semaphores = self.semaphores.lock().unwrap();
        let model_ids = semaphores.keys().cloned().collect::<Vec<_>>();
        *semaphores = Self::make_semaphores(limit.max(1), model_ids);
    }

    /// Test utility: read the current rate-limit cooldown deadline for a model.
    #[cfg(test)]
    async fn rate_limit_deadline_for_test(&self, model_id: &str) -> Option<Instant> {
        self.rate_limited.read().await.get(model_id).copied()
    }

    pub fn new_with_clients(
        small_model: Arc<dyn LlmClient>,
        default_model: Arc<dyn LlmClient>,
        image_model: Arc<dyn LlmClient>,
        audio_model: Arc<dyn LlmClient>,
    ) -> Self {
        Self::new_with_clients_full(
            small_model,
            default_model,
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
        image_model: Arc<dyn LlmClient>,
        audio_model: Arc<dyn LlmClient>,
        embedding_model: Arc<dyn LlmClient>,
    ) -> Self {
        let config = Self::test_config();
        // Injected test clients intentionally do not carry credentials. Keep
        // the test constructor's explicit primary assignments routable while
        // production construction remains credential-gated.
        let model_directory = ModelDirectory::with_injected_clients(
            &config,
            [
                ("small_model", small_model),
                ("default_model", default_model),
                ("image_model", image_model),
                ("audio_model", audio_model),
                ("embedding_model", embedding_model),
            ]
            .into_iter()
            .map(|(id, client)| (id.to_string(), client)),
        );
        let LlmRouterRuntimeState {
            endpoint_circuits,
            stream_rules,
            semaphores,
            rate_limited,
        } = Self::runtime_state(64, model_directory.model_ids().map(str::to_string));
        Self {
            config: Arc::new(RwLock::new(config)),
            default_context_window: crate::registry::FALLBACK_CONTEXT_WINDOW,
            model_directory,
            endpoint_circuits,
            stream_rules,
            // Test constructors bypass the config, so use a high per-model
            // limit: the semaphore is meant to pace real provider traffic,
            // not to serialize mock-based tests.
            semaphores,
            rate_limited,
        }
    }

    fn test_config() -> RouterConfig {
        let models = [
            ("small_model", vec![Capability::FastChat]),
            (
                "default_model",
                vec![
                    Capability::Chat,
                    Capability::Vision,
                    Capability::AudioInput,
                    Capability::Transcription,
                ],
            ),
            ("image_model", vec![Capability::Vision]),
            (
                "audio_model",
                vec![Capability::AudioInput, Capability::Transcription],
            ),
            ("embedding_model", vec![Capability::Embedding]),
        ]
        .into_iter()
        .map(|(id, capabilities)| RoutedModel {
            id: id.into(),
            endpoint: ModelEndpoint {
                model_name: id.into(),
                ..Default::default()
            },
            capabilities,
        })
        .collect();
        let request_policies = [
            (RequestKind::FastChat, "small_model"),
            (RequestKind::Chat, "default_model"),
            (RequestKind::Vision, "image_model"),
            (RequestKind::AudioChat, "audio_model"),
            (RequestKind::Transcription, "audio_model"),
            (RequestKind::Embedding, "embedding_model"),
        ]
        .into_iter()
        .map(|(request, primary)| haven_common::config::RequestPolicy {
            request,
            primary: primary.into(),
        })
        .collect();
        RouterConfig {
            models,
            request_policies,
            ..Default::default()
        }
    }

    pub fn select_request(&self, request: RequestKind) -> Arc<dyn LlmClient> {
        self.model_directory.select_client(request)
    }

    /// Return the selected adapter's wire-level media profile. This is kept
    /// separate from request selection so the pure planner can make a request
    /// projection without inferring capabilities from a model id.
    pub fn capability_profile_for_request(&self, request: RequestKind) -> CapabilityProfile {
        self.model_directory.capability_profile_for_request(request)
    }

    /// Resolve the model context window using the same endpoint metadata and
    /// fallback used during router construction.
    pub async fn context_window_for_request(&self, request: RequestKind) -> u32 {
        let config = self.config.read().await;
        self.model_directory
            .context_window_for_request(&config, request)
            .unwrap_or(self.default_context_window)
            .max(1)
    }

    /// Calculate the maximum output budget that can be requested for one
    /// provider call after accounting for the estimated input. Providers
    /// commonly validate input + requested output against one shared window.
    pub async fn effective_output_tokens(
        &self,
        request: RequestKind,
        estimated_input_tokens: u32,
    ) -> u32 {
        const REQUEST_SAFETY_MARGIN: u32 = 256;
        let config = self.config.read().await;
        let Some(endpoint) = self.model_directory.endpoint_for_request(&config, request) else {
            return 1;
        };
        let window = crate::registry::context_window_for(endpoint)
            .unwrap_or(self.default_context_window)
            .max(1);
        let remaining = window
            .saturating_sub(estimated_input_tokens)
            .saturating_sub(REQUEST_SAFETY_MARGIN)
            .max(1);
        endpoint.max_tokens.max(1).min(remaining)
    }

    /// Returns true if the request has a usable configured model.
    /// Used by tools that should no-op gracefully when an endpoint is not set up.
    pub async fn is_request_configured(&self, request: RequestKind) -> bool {
        let config = self.config.read().await;
        self.model_directory.is_request_configured(&config, request)
    }

    /// Test utility: force the configured state of a request (empty vs non-empty
    /// api_key). `new_with_clients` builds with a default config where all
    /// keys are empty; cross-crate tests that exercise `is_request_configured`
    /// guards use this to simulate a configured endpoint.
    #[doc(hidden)]
    pub async fn force_request_configured(&self, request: RequestKind, configured: bool) {
        let mut cfg = self.config.write().await;
        if let Some(id) = cfg.policy(request).map(|policy| policy.primary.clone())
            && let Some(model) = cfg.model_mut(&id)
        {
            model.endpoint.api_key = if configured {
                "sk-test".to_string()
            } else {
                String::new()
            };
        }
        let route_mode = if configured {
            RouteMode::InjectedClients
        } else {
            RouteMode::Production
        };
        self.model_directory
            .rebuild_primary_routes(&cfg, route_mode);
    }

    /// Test utility for cross-crate tests: assign one request kind to a
    /// configured injected model and rebuild the route table from that policy.
    #[doc(hidden)]
    pub async fn force_request_primary_for_test(
        &self,
        request: RequestKind,
        model_id: &str,
    ) -> anyhow::Result<()> {
        let mut cfg = self.config.write().await;
        let model = cfg
            .model(model_id)
            .ok_or_else(|| anyhow::anyhow!("test model '{model_id}' is not configured"))?;
        if !model.capabilities.contains(&request.required_capability()) {
            anyhow::bail!("test model '{model_id}' lacks capability for {request:?}");
        }
        let policy = cfg
            .policy_mut(request)
            .ok_or_else(|| anyhow::anyhow!("test request policy {request:?} is not configured"))?;
        policy.primary = model_id.to_string();
        self.model_directory
            .rebuild_primary_routes(&cfg, RouteMode::InjectedClients);
        Ok(())
    }

    /// Transcribe WAV audio through the `transcription` request policy.
    /// Tries native [`LlmClient::transcribe`] first; when the adapter reports
    /// [`LlmError::UnsupportedCapability`], falls back to multimodal chat
    /// with an `input_audio` content part (gpt-4o-audio-preview etc.).
    pub async fn transcribe_audio(
        &self,
        wav_data: &[u8],
    ) -> Result<crate::types::SttResult, LlmError> {
        if !self.is_request_configured(RequestKind::Transcription).await {
            return Err(LlmError::RequestFailed(
                "transcription request is not configured; configure a transcription policy in Settings -> Models"
                    .into(),
            ));
        }
        // Keep native STT under the same permit, circuit, retry, and timeout
        // policy as chat/embedding. The capability fallback to multimodal
        // chat is deliberately outside this closure so it can acquire the
        // audio-chat model's own permit after native transcription releases
        // its permit.
        let native_result = self
            .with_request_permit(
                RequestDescriptor::from(RequestKind::Transcription),
                |model_id, client| async move {
                    let cfg = self.config.read().await;
                    let policy = RequestExecutionPolicy::primary(&cfg);
                    drop(cfg);

                    execute_with_timeout(policy.total_timeout_secs, "transcription", || async {
                        let result = execute_with_retry(policy.retry, None, || async {
                            client.transcribe(wav_data).await
                        })
                        .await;
                        self.record_request_outcome(&model_id, &result).await;
                        result
                    })
                    .await
                },
            )
            .await;
        match native_result {
            Err(error) if error.is_unsupported() => {
                crate::stt::transcribe_via_chat(self, wav_data).await
            }
            result => result,
        }
    }

    /// Provider-wire adapter for one already-read image payload.
    ///
    /// This is intentionally not a model-facing media orchestration entry
    /// point: `haven-tools::builtin::media::MediaTool` owns asset lookup,
    /// operation policy, lifecycle, fallback and structured results. This
    /// method only routes one image request through the provider adapter.
    pub async fn analyze_image(
        &self,
        bytes: &[u8],
        media_type: &str,
        system_prompt: &str,
        focus: Option<&str>,
    ) -> Result<LlmResponse, LlmError> {
        crate::media::analyze_image(self, bytes, media_type, system_prompt, focus).await
    }

    // §2.6: check the selected model's circuit breaker before dispatching.
    async fn check_circuit(&self, model_id: &str) -> Result<(), LlmError> {
        let mut circuits = self.endpoint_circuits.write().await;
        let endpoint = circuits
            .entry(model_id.to_string())
            .or_insert_with(EndpointCircuitBreaker::new);
        if !endpoint.allow_request() {
            return Err(LlmError::CircuitOpen {
                model_id: model_id.to_string(),
            });
        }
        Ok(())
    }

    /// Clear the selected endpoint's consecutive-failure gate before an
    /// explicit user retry (for example, Continue on an errored session).
    /// This is process-local circuit state; provider rate-limit cooldowns and
    /// lifetime call counters are intentionally preserved.
    pub async fn prepare_manual_retry(&self, request: RequestKind) {
        let Ok(resolved_client) = self
            .model_directory
            .resolve_client(RequestDescriptor::from(request))
        else {
            return;
        };
        let model_id = resolved_client.model_id;
        self.endpoint_circuits
            .write()
            .await
            .entry(model_id)
            .or_insert_with(EndpointCircuitBreaker::new)
            .reset_for_manual_retry();
    }

    async fn record_success(&self, model_id: &str) {
        let mut circuits = self.endpoint_circuits.write().await;
        circuits
            .entry(model_id.to_string())
            .or_insert_with(EndpointCircuitBreaker::new)
            .record_success();
    }

    async fn record_failure(&self, model_id: &str) {
        let mut circuits = self.endpoint_circuits.write().await;
        circuits
            .entry(model_id.to_string())
            .or_insert_with(EndpointCircuitBreaker::new)
            .record_failure();
    }

    /// Project one completed router request onto endpoint circuit and rate-limit
    /// state. Callers invoke this at the same point they receive the logical
    /// request result, before returning it to their caller.
    async fn record_request_outcome<T>(&self, model_id: &str, result: &Result<T, LlmError>) {
        self.project_request_outcome(model_id.to_string(), RequestOutcome::from_result(result))
            .await;
    }

    async fn project_request_outcome(&self, model_id: String, outcome: RequestOutcome) {
        match outcome {
            RequestOutcome::Success => self.record_success(&model_id).await,
            RequestOutcome::Failure { rate_limit_after } => {
                self.record_failure(&model_id).await;
                if let Some(retry_after) = rate_limit_after {
                    self.record_rate_limit(&model_id, Some(retry_after)).await;
                }
            }
        }
    }

    /// Ordinary and tool chat share one policy snapshot and execution boundary.
    async fn execute_chat_request(
        &self,
        request: CompleteRequest,
    ) -> Result<LlmResponse, LlmError> {
        let CompleteRequest {
            request,
            messages,
            tools,
            max_output_tokens,
        } = request;
        let descriptor = RequestDescriptor::from(request);
        self.with_request_permit(descriptor, |model_id, client| async move {
            let config = self.config.read().await;
            let policy = RequestExecutionPolicy::primary(&config);
            drop(config);

            CallExecutor::new(descriptor, model_id, client, policy)
                .complete(messages, tools, max_output_tokens, |model_id, outcome| {
                    self.project_request_outcome(model_id, outcome)
                })
                .await
        })
        .await
    }

    /// Complete one ordinary or tool-enabled request through its request policy.
    pub async fn complete(&self, request: CompleteRequest) -> Result<LlmResponse, LlmError> {
        self.execute_chat_request(request).await
    }

    fn prompt_request(request: PromptRequest) -> CompleteRequest {
        let mut messages = Vec::with_capacity(if request.system_prompt.is_empty() {
            1
        } else {
            2
        });
        if !request.system_prompt.is_empty() {
            messages.push(CanonicalMessage::system(vec![ContentPart::text(
                request.system_prompt,
            )]));
        }
        messages.push(CanonicalMessage::user(vec![ContentPart::text(
            request.user_prompt,
        )]));
        CompleteRequest {
            request: request.request,
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
        }
    }

    /// Route one owned `System + User` prompt (or just `User` when the system
    /// prompt is empty) through the ordinary completion policy.
    pub async fn chat_with_prompt_request(
        &self,
        request: PromptRequest,
    ) -> Result<LlmResponse, LlmError> {
        self.complete(Self::prompt_request(request)).await
    }

    /// Cancellable one-shot chat used by compaction and other maintenance
    /// calls. Dropping the in-flight request releases the provider future as
    /// soon as the user cancels the owning session.
    pub async fn chat_messages_cancellable(
        &self,
        request: RequestKind,
        messages: Vec<CanonicalMessage>,
        max_output_tokens: Option<u32>,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(LlmError::Cancelled),
            result = self.complete(CompleteRequest {
                request,
                messages,
                tools: Vec::new(),
                max_output_tokens,
            }) => result,
        }
    }

    /// Embed a batch of texts into vectors via the dedicated `embedding_model`
    /// endpoint. Applies the circuit breaker, retry, and router-level total
    /// timeout like other calls.
    pub async fn embed(&self, request: EmbeddingRequest) -> Result<Embedding, LlmError> {
        let input = request.input;
        if input.is_empty() {
            return Ok(Embedding {
                vectors: Vec::new(),
                model: None,
                usage: Usage::default(),
            });
        }
        let descriptor = RequestDescriptor::from(RequestKind::Embedding);
        self.with_request_permit(descriptor, |model_id, client| async move {
            let cfg = self.config.read().await;
            let policy = RequestExecutionPolicy::primary(&cfg);
            drop(cfg);

            CallExecutor::new(descriptor, model_id, client, policy)
                .embed(input, |model_id, outcome| {
                    self.project_request_outcome(model_id, outcome)
                })
                .await
        })
        .await
    }

    /// Convenience wrapper for single-text embedding.
    pub async fn embed_text(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        let emb = self
            .embed(EmbeddingRequest {
                input: vec![text.to_string()],
            })
            .await?;
        Ok(emb.vectors.into_iter().next().unwrap_or_default())
    }

    pub async fn chat_stream(
        &self,
        request: RequestKind,
        messages: Vec<CanonicalMessage>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures_util::Stream<Item = Result<crate::types::StreamChunk, LlmError>> + Send,
            >,
        >,
        LlmError,
    > {
        let descriptor = RequestDescriptor::from(request);
        let ResolvedModelClient { model_id, client } =
            self.model_directory.resolve_client(descriptor)?;
        let permit = self.acquire_model_permit(&model_id).await?;
        self.wait_rate_limit_cooldown(&model_id).await;
        self.check_circuit(&model_id).await?;
        let cfg = self.config.read().await;
        let primary_policy = RequestExecutionPolicy::primary(&cfg);
        drop(cfg);
        // Raw stream callers own consumption. Once a stream is returned, its
        // later transport error must be handled by the caller without
        // replaying already-consumed deltas.
        StreamExecutor::new(descriptor, model_id, client, primary_policy)
            .chat_stream(messages, permit, |model_id, outcome| {
                self.project_request_outcome(model_id, outcome)
            })
            .await
    }

    /// Stream-chat a tool-aware request to the primary endpoint, aggregating
    /// the deltas into a final `LlmResponse`.
    ///
    /// `messages`/`tools` are borrowed: the router clones them for each
    /// attempt internally, so callers (e.g. the ReAct loop) can convert once
    /// per step and reuse the same converted messages across retries.
    pub async fn chat_stream_with_tools_aggregated(
        &self,
        request: RequestKind,
        messages: &[CanonicalMessage],
        tools: &[LlmToolDefinition],
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_stream_with_tools_aggregated_cancellable(
            request,
            messages,
            tools,
            on_chunk,
            CancellationToken::new(),
        )
        .await
    }

    /// Stream-chat with cancellation on the selected endpoint (§2.10, §2.11).
    /// Applies `max_total_duration_secs` as an overall deadline (§2.12).
    /// A transient stream failure is retried only when the failed attempt did
    /// not emit a chunk. Once anything has reached `on_chunk`, replaying would
    /// duplicate visible thought/reasoning or tool-argument previews, so the
    /// router returns the stream error rather than replaying that same stream.
    ///
    /// Runs under the model's concurrency permit (see
    /// [`Self::with_request_permit`]): the permit covers the whole stream —
    /// retries and chunk consumption — so parallel sessions cannot
    /// exceed the configured per-endpoint in-flight request cap.
    pub async fn chat_stream_with_tools_aggregated_cancellable(
        &self,
        request: RequestKind,
        messages: &[CanonicalMessage],
        tools: &[LlmToolDefinition],
        on_chunk: impl FnMut(&StreamChunk) + Send + 'static,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_stream_with_tools_aggregated_cancellable_with_attempts(
            StreamRequest {
                request,
                messages,
                tools,
                max_output_tokens: None,
            },
            StreamAttemptHooks::new(
                on_chunk,
                |_| {},
                StreamAttemptOutputDisposition::PreserveExisting,
            ),
            cancel,
        )
        .await
    }

    /// Stream-chat with explicit output-attempt boundaries.
    ///
    /// `on_attempt_start` receives whether the new attempt preserves or
    /// replaces previous visible output. The callback is deliberately separate from
    /// `on_chunk`: a provider retry can start a new response before its first
    /// chunk arrives, and concatenating both attempts is never valid.
    /// The cancellable method above keeps the callback-only API for
    /// non-agent callers.
    pub async fn chat_stream_with_tools_aggregated_cancellable_with_attempts(
        &self,
        stream_request: StreamRequest<'_>,
        hooks: StreamAttemptHooks,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        let descriptor = RequestDescriptor::from(stream_request.request);
        let operation = self.with_request_permit(descriptor, |model_id, _client| {
            let cancel = cancel.clone();
            async move {
                self.chat_stream_with_tools_aggregated_cancellable_inner(
                    stream_request,
                    descriptor,
                    model_id,
                    hooks,
                    cancel,
                )
                .await
            }
        });
        // Cancellation also covers waiting for a concurrency permit or a
        // shared rate-limit cooldown. Dropping the operation releases the
        // permit and prevents a stopped session from starting later.
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(LlmError::Cancelled),
            result = operation => result,
        }
    }

    async fn chat_stream_with_tools_aggregated_cancellable_inner(
        &self,
        stream_request: StreamRequest<'_>,
        descriptor: RequestDescriptor,
        model_id: String,
        hooks: StreamAttemptHooks,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        tracing::debug!(
            "router streaming LLM call, request={:?} messages={} tools={}",
            stream_request.request,
            stream_request.messages.len(),
            stream_request.tools.len()
        );
        let StreamAttemptHooks {
            on_chunk,
            on_attempt_start,
            output_disposition_on_start,
        } = hooks;
        let hooks = ActiveStreamHooks::new(on_chunk, on_attempt_start, output_disposition_on_start);

        let cfg = self.config.read().await;
        let primary_policy = RequestExecutionPolicy::primary(&cfg);
        // Clamp to >= 1s: a hand-edited 0 would make every stream.first() poll
        // time out instantly, disabling all model replies.
        let idle_dur = Duration::from_secs(cfg.stream_idle_timeout_secs.max(1));
        drop(cfg);
        let stream_context = StreamContext::from_request(stream_request);
        let candidate = self.model_directory.client_for_model_id(&model_id)?;
        let model_id_for_projection = model_id.clone();
        AggregatedStreamExecutor::new(
            descriptor,
            candidate,
            primary_policy,
            &self.stream_rules,
            idle_dur,
        )
        .execute(
            stream_context,
            hooks,
            cancel,
            || async {
                Duration::from_secs(self.config.read().await.stream_idle_timeout_secs.max(1))
            },
            |result| async move {
                self.record_request_outcome(&model_id_for_projection, &result)
                    .await;
                result
            },
        )
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

    pub async fn health_check(&self, request: HealthCheckRequest) -> Result<(), LlmError> {
        self.with_request_permit(
            RequestDescriptor::from(request.request),
            |model_id, candidate| async move {
                let cfg = self.config.read().await;
                let policy = RequestExecutionPolicy::primary(&cfg);
                drop(cfg);

                execute_with_timeout(policy.total_timeout_secs, "health check", || async {
                    let result = candidate.health_check().await;
                    self.record_request_outcome(&model_id, &result).await;
                    result
                })
                .await
            },
        )
        .await
    }

    /// Tri-state connectivity probe for the top-right status chip.
    ///
    /// - A request without configured credentials short-circuits to
    ///   [`LlmConnectionStatus::Unconfigured`] **without any network I/O** —
    ///   neither the TCP/TLS handshake nor the GET is attempted, so an
    ///   unset-up install never wastes a probe on a bare default base_url.
    /// - A configured request runs the same `/models` health check as
    ///   [`LlmRouter::health_check`] and reports Ready on success /
    ///   Disconnected on failure.
    pub async fn connection_status(&self, request: RequestKind) -> LlmConnectionReport {
        let endpoint = {
            let config = self.config.read().await;
            self.model_directory
                .endpoint_for_request(&config, request)
                .cloned()
        };
        let Some(endpoint) = endpoint else {
            return LlmConnectionReport {
                status: LlmConnectionStatus::Unconfigured,
                reason: None,
                provider: String::new(),
                model: String::new(),
            };
        };
        if !haven_common::config::endpoint_credentials_ready(&endpoint) {
            return LlmConnectionReport {
                status: LlmConnectionStatus::Unconfigured,
                reason: None,
                provider: String::new(),
                model: String::new(),
            };
        }
        match self.health_check(HealthCheckRequest { request }).await {
            Ok(()) => LlmConnectionReport {
                status: LlmConnectionStatus::Ready,
                reason: None,
                provider: endpoint.provider,
                model: endpoint.model_name,
            },
            Err(e) => {
                let reason = e.connection_failure_reason();
                if matches!(&e, LlmError::CircuitOpen { .. }) {
                    tracing::debug!(
                        request = request.as_str(),
                        provider = %endpoint.provider,
                        model = %endpoint.model_name,
                        reason = reason.as_str(),
                        "LLM connection probe deferred by open circuit"
                    );
                } else {
                    tracing::warn!(
                        request = request.as_str(),
                        provider = %endpoint.provider,
                        model = %endpoint.model_name,
                        endpoint_host = %endpoint_host(&endpoint.base_url),
                        reason = reason.as_str(),
                        error = %haven_common::error::sanitize_error_text(&e.to_string()),
                        "LLM connection probe failed"
                    );
                }
                LlmConnectionReport {
                    status: LlmConnectionStatus::Disconnected,
                    reason: Some(reason),
                    provider: endpoint.provider,
                    model: endpoint.model_name,
                }
            }
        }
    }

    /// Pre-warm HTTP connections for every configured request model so the
    /// first request to any model skips TCP+TLS handshake (~50-200ms).
    /// Unconfigured requests are skipped. Each model is
    /// checked concurrently and retried once on transient failure.
    pub async fn prewarm_all(&self) {
        let cfg = self.config.read().await;
        let configured = self.model_directory.configured_requests(&cfg);
        drop(cfg);

        if configured.is_empty() {
            tracing::info!("LLM pre-warm skipped: no configured endpoints");
            return;
        }

        let requests = configured.clone();
        let checks = requests.into_iter().map(|request| async move {
            let first = self.health_check(HealthCheckRequest { request }).await;
            if first.is_err() {
                // One retry: transient failures (conn reset, 5xx) should not
                // leave the pool cold for the first user message.
                self.health_check(HealthCheckRequest { request }).await
            } else {
                first
            }
        });
        let results = join_all(checks).await;

        let mut ok = 0;
        for (request, result) in configured.iter().zip(results.iter()) {
            match result {
                Ok(()) => {
                    ok += 1;
                    tracing::debug!("LLM request {} pre-warmed", request.as_str());
                }
                Err(e) => tracing::warn!(
                    request = request.as_str(),
                    reason = e.connection_failure_reason().as_str(),
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "LLM pre-warm failed (will retry on first request)"
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
    pub async fn compute_cost(&self, request: RequestKind, usage: &Usage) -> Option<f64> {
        let cfg = self.config.read().await;
        let endpoint = self.model_directory.endpoint_for_request(&cfg, request)?;
        compute_cost_usd(
            endpoint,
            usage.cache_miss_tokens(),
            usage.cached_tokens,
            usage.cache_creation_tokens,
            usage.completion_tokens,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream_rules::StreamRuleMode;
    use crate::streaming::{IDLE_SCALE_CAP_SECS, estimate_prompt_tokens, scale_stream_idle};
    use crate::types::{FinishReason, LlmError::Unknown, PromptRequest, Usage};
    use async_trait::async_trait;
    use futures_util::{StreamExt, stream};
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

    #[derive(Default)]
    struct RouterRequestProbe {
        llm_calls: Arc<std::sync::atomic::AtomicUsize>,
        health_calls: Arc<std::sync::atomic::AtomicUsize>,
        transcription_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl RouterRequestProbe {
        fn record_llm_call(&self) {
            self.llm_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    struct RequestProfileClient {
        service_delay: Duration,
    }

    #[async_trait]
    impl LlmClient for RequestProfileClient {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            if !self.service_delay.is_zero() {
                tokio::time::sleep(self.service_delay).await;
            }
            Ok(LlmResponse::default())
        }

        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(messages).await
        }

        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::UnsupportedCapability(
                "request profile is non-streaming".into(),
            ))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn request_profile_distribution(samples_ns: &[u128], numerator: usize) -> f64 {
        let mut ordered = samples_ns.to_vec();
        ordered.sort_unstable();
        let rank = ordered.len().saturating_mul(numerator).div_ceil(100);
        ordered[rank.saturating_sub(1)] as f64 / 1_000.0
    }

    fn request_profile_request() -> CompleteRequest {
        CompleteRequest::new(
            RequestKind::Chat,
            vec![llm_message(vec![ContentPart::text("profile")])],
        )
    }

    #[async_trait]
    impl LlmClient for RouterRequestProbe {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            self.record_llm_call();
            Ok(LlmResponse::default())
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.record_llm_call();
            Ok(Box::pin(stream::empty()))
        }

        async fn embed(&self, _input: Vec<String>) -> Result<Embedding, LlmError> {
            self.record_llm_call();
            Ok(Embedding {
                vectors: Vec::new(),
                model: None,
                usage: Usage::default(),
            })
        }

        async fn transcribe(&self, _wav_data: &[u8]) -> Result<crate::types::SttResult, LlmError> {
            self.record_llm_call();
            self.transcription_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::types::SttResult {
                text: "native transcript".into(),
                ..Default::default()
            })
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            self.health_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn request_outcome_projection_preserves_circuit_and_cooldown_mapping() {
        let router = LlmRouter::new(RouterConfig::default());
        let model_id = "outcome-projection-test";

        let failure: Result<(), LlmError> = Err(LlmError::Unknown("provider failed".into()));
        router.record_request_outcome(model_id, &failure).await;
        assert_eq!(
            router.endpoint_circuits.read().await[model_id].consecutive_failures,
            1
        );
        assert!(
            router
                .rate_limit_deadline_for_test(model_id)
                .await
                .is_none()
        );

        let success: Result<(), LlmError> = Ok(());
        router.record_request_outcome(model_id, &success).await;
        assert_eq!(
            router.endpoint_circuits.read().await[model_id].consecutive_failures,
            0
        );

        let rate_limit: Result<(), LlmError> = Err(LlmError::RateLimit {
            retry_after: Some(Duration::from_secs(7)),
        });
        let started_at = Instant::now();
        router.record_request_outcome(model_id, &rate_limit).await;
        let deadline = router
            .rate_limit_deadline_for_test(model_id)
            .await
            .expect("rate-limited outcomes establish a model cooldown");
        assert!(deadline >= started_at + Duration::from_secs(6));
        assert_eq!(
            router.endpoint_circuits.read().await[model_id].consecutive_failures,
            1
        );
    }

    struct PromptRequestProbe {
        seen: Arc<StdMutex<Vec<Vec<CanonicalMessage>>>>,
        rate_limited: bool,
    }

    impl PromptRequestProbe {
        fn respond(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            self.seen.lock().unwrap().push(messages);
            if self.rate_limited {
                Err(LlmError::RateLimit {
                    retry_after: Some(Duration::from_secs(5)),
                })
            } else {
                Ok(LlmResponse {
                    text: "prompt response".into(),
                    ..LlmResponse::default()
                })
            }
        }
    }

    #[async_trait]
    impl LlmClient for PromptRequestProbe {
        async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            self.respond(messages)
        }

        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.respond(messages)
        }

        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::UnsupportedCapability(
                "prompt request probe does not stream".into(),
            ))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn assert_prompt_message(
        message: &CanonicalMessage,
        role: haven_common::types::CanonicalRole,
        expected_text: &str,
    ) {
        assert_eq!(message.role, role);
        match message.content.as_slice() {
            [ContentPart::Text(text)] => assert_eq!(text, expected_text),
            content => panic!("expected one text content part, got {content:?}"),
        }
    }

    #[tokio::test]
    async fn complete_request_uses_logical_purpose_to_select_primary_model() {
        let fast_seen = Arc::new(StdMutex::new(Vec::new()));
        let chat_seen = Arc::new(StdMutex::new(Vec::new()));
        let fast_client: Arc<dyn LlmClient> = Arc::new(PromptRequestProbe {
            seen: fast_seen.clone(),
            rate_limited: false,
        });
        let chat_client: Arc<dyn LlmClient> = Arc::new(PromptRequestProbe {
            seen: chat_seen.clone(),
            rate_limited: false,
        });
        let router = LlmRouter::new_with_clients(
            fast_client,
            chat_client.clone(),
            chat_client.clone(),
            chat_client,
        );

        router
            .complete(CompleteRequest::new(RequestKind::FastChat, Vec::new()))
            .await
            .expect("fast_chat policy should route to its primary model");
        assert_eq!(fast_seen.lock().unwrap().len(), 1);
        assert!(chat_seen.lock().unwrap().is_empty());

        router
            .complete(CompleteRequest::new(RequestKind::Chat, Vec::new()))
            .await
            .expect("chat policy should route to its primary model");
        assert_eq!(fast_seen.lock().unwrap().len(), 1);
        assert_eq!(chat_seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prompt_request_preserves_request_route_and_prompt_messages() {
        let fast_seen = Arc::new(StdMutex::new(Vec::new()));
        let chat_seen = Arc::new(StdMutex::new(Vec::new()));
        let fast_client: Arc<dyn LlmClient> = Arc::new(PromptRequestProbe {
            seen: fast_seen.clone(),
            rate_limited: false,
        });
        let chat_client: Arc<dyn LlmClient> = Arc::new(PromptRequestProbe {
            seen: chat_seen.clone(),
            rate_limited: false,
        });
        let router = LlmRouter::new_with_clients(
            fast_client,
            chat_client.clone(),
            chat_client.clone(),
            chat_client,
        );

        let response = router
            .chat_with_prompt_request(PromptRequest::new(
                RequestKind::FastChat,
                "fast system prompt",
                "fast user prompt",
            ))
            .await
            .unwrap();
        assert_eq!(response.text, "prompt response");
        {
            let fast_calls = fast_seen.lock().unwrap();
            assert_eq!(fast_calls.len(), 1);
            assert_eq!(fast_calls[0].len(), 2);
            assert_prompt_message(
                &fast_calls[0][0],
                haven_common::types::CanonicalRole::System,
                "fast system prompt",
            );
            assert_prompt_message(
                &fast_calls[0][1],
                haven_common::types::CanonicalRole::User,
                "fast user prompt",
            );
        }
        assert!(chat_seen.lock().unwrap().is_empty());

        router
            .chat_with_prompt_request(PromptRequest::new(
                RequestKind::Chat,
                "",
                "user-only prompt",
            ))
            .await
            .unwrap();
        {
            let chat_calls = chat_seen.lock().unwrap();
            assert_eq!(chat_calls.len(), 1);
            assert_eq!(chat_calls[0].len(), 1);
            assert_prompt_message(
                &chat_calls[0][0],
                haven_common::types::CanonicalRole::User,
                "user-only prompt",
            );
        }
    }

    #[tokio::test]
    async fn prompt_request_preserves_rate_limit_error_and_router_projection() {
        let fast_seen = Arc::new(StdMutex::new(Vec::new()));
        let chat_seen = Arc::new(StdMutex::new(Vec::new()));
        let fast_client: Arc<dyn LlmClient> = Arc::new(PromptRequestProbe {
            seen: fast_seen.clone(),
            rate_limited: false,
        });
        let chat_client: Arc<dyn LlmClient> = Arc::new(PromptRequestProbe {
            seen: chat_seen.clone(),
            rate_limited: true,
        });
        let router = LlmRouter::new_with_clients(
            fast_client,
            chat_client.clone(),
            chat_client.clone(),
            chat_client,
        );
        router.config.write().await.retry_max_retries = 0;

        let error = router
            .chat_with_prompt_request(PromptRequest::new(
                RequestKind::Chat,
                "failure system prompt",
                "failure user prompt",
            ))
            .await
            .expect_err("the provider's rate-limit error must reach the caller");
        assert!(matches!(
            error,
            LlmError::RateLimit {
                retry_after: Some(delay)
            } if delay == Duration::from_secs(5)
        ));

        {
            let chat_calls = chat_seen.lock().unwrap();
            assert_eq!(chat_calls.len(), 1);
            assert_eq!(chat_calls[0].len(), 2);
            assert_prompt_message(
                &chat_calls[0][0],
                haven_common::types::CanonicalRole::System,
                "failure system prompt",
            );
            assert_prompt_message(
                &chat_calls[0][1],
                haven_common::types::CanonicalRole::User,
                "failure user prompt",
            );
        }
        assert!(fast_seen.lock().unwrap().is_empty());
        assert_eq!(
            router.endpoint_circuits.read().await["default_model"].consecutive_failures,
            1
        );
        assert!(
            router
                .rate_limit_deadline_for_test("default_model")
                .await
                .is_some_and(|deadline| deadline > Instant::now())
        );
        assert!(
            router
                .rate_limit_deadline_for_test("small_model")
                .await
                .is_none()
        );
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
        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(messages).await
        }
        async fn chat_with_tools(
            &self,
            _: Vec<CanonicalMessage>,
            _: Vec<LlmToolDefinition>,
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
        async fn chat_with_tools_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            tools: Vec<LlmToolDefinition>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat_with_tools(messages, tools).await
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
            _: Vec<LlmToolDefinition>,
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
        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _messages: Arc<[CanonicalMessage]>,
            _tools: Arc<[LlmToolDefinition]>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            if self.fail_chat {
                Err(LlmError::ServerError(
                    "mock: chat_stream_with_tools failed".into(),
                ))
            } else {
                Ok(Box::pin(stream::iter(self.chunks.clone())))
            }
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    struct StreamRequestProbe {
        seen: std::sync::Mutex<Vec<serde_json::Value>>,
        chunks: Vec<StreamChunk>,
    }

    #[async_trait]
    impl LlmClient for StreamRequestProbe {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(Unknown("probe: chat not implemented".into()))
        }

        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(Unknown("probe: raw chat_stream not implemented".into()))
        }

        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            messages: Arc<[CanonicalMessage]>,
            tools: Arc<[LlmToolDefinition]>,
            max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            self.seen.lock().unwrap().push(serde_json::json!({
                "messages": serde_json::to_value(messages.as_ref()).unwrap(),
                "tools": serde_json::to_value(tools.as_ref()).unwrap(),
                "max_output_tokens": max_output_tokens,
            }));
            Ok(Box::pin(stream::iter(
                self.chunks.clone().into_iter().map(Ok),
            )))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn stream_request_preserves_payload_for_plain_and_tool_aggregated_streams() {
        let ordinary_message = llm_message(vec![ContentPart::text("ordinary prompt")]);
        let mut tool_message = llm_message(vec![ContentPart::text("tool prompt")]);
        tool_message.reasoning = Some("opaque reasoning context".into());
        tool_message.tool_calls = Some(vec![CanonicalToolCall {
            id: "call-1".into(),
            name: "prior_tool".into(),
            arguments: serde_json::json!({"arg": "value"}),
        }]);
        let ordinary_messages = vec![ordinary_message];
        let tool_messages = vec![tool_message];
        let tools = vec![LlmToolDefinition {
            tool_type: "function".into(),
            function: crate::types::ToolFunction {
                name: "probe_tool".into(),
                description: "preserve this definition".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"value": {"type": "string"}}
                }),
            },
        }];
        let ordinary_probe = Arc::new(StreamRequestProbe {
            seen: std::sync::Mutex::new(Vec::new()),
            chunks: vec![StreamChunk {
                tool_call_updates: Vec::new(),
                text: Some("ordinary response".into()),
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            }],
        });
        let tool_probe = Arc::new(StreamRequestProbe {
            seen: std::sync::Mutex::new(Vec::new()),
            chunks: vec![StreamChunk {
                tool_call_updates: Vec::new(),
                tool_calls: vec![CanonicalToolCall {
                    id: "result-call".into(),
                    name: "probe_tool".into(),
                    arguments: serde_json::json!({"value": "ok"}),
                }],
                finish_reason: Some(FinishReason::ToolCalls),
                ..Default::default()
            }],
        });
        let ordinary_client: Arc<dyn LlmClient> = ordinary_probe.clone();
        let tool_client: Arc<dyn LlmClient> = tool_probe.clone();
        let router = LlmRouter::new_with_clients(
            ordinary_client.clone(),
            ordinary_client.clone(),
            tool_client,
            ordinary_client,
        );

        let ordinary = router
            .chat_stream_with_tools_aggregated_cancellable_with_attempts(
                StreamRequest {
                    request: RequestKind::Chat,
                    messages: &ordinary_messages,
                    tools: &[],
                    max_output_tokens: Some(23),
                },
                StreamAttemptHooks::new(
                    |_| {},
                    |_| {},
                    StreamAttemptOutputDisposition::PreserveExisting,
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let with_tools = router
            .chat_stream_with_tools_aggregated_cancellable_with_attempts(
                StreamRequest {
                    request: RequestKind::Vision,
                    messages: &tool_messages,
                    tools: &tools,
                    max_output_tokens: Some(41),
                },
                StreamAttemptHooks::new(
                    |_| {},
                    |_| {},
                    StreamAttemptOutputDisposition::PreserveExisting,
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(ordinary.text, "ordinary response");
        assert_eq!(ordinary.finish_reason, Some(FinishReason::Stop));
        assert_eq!(with_tools.tool_calls.len(), 1);
        assert_eq!(with_tools.tool_calls[0].name, "probe_tool");
        assert_eq!(with_tools.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(
            *ordinary_probe.seen.lock().unwrap(),
            vec![serde_json::json!({
                "messages": serde_json::to_value(&ordinary_messages).unwrap(),
                "tools": serde_json::to_value(Vec::<LlmToolDefinition>::new()).unwrap(),
                "max_output_tokens": 23,
            })],
            "the Chat request must route its borrowed message slice unchanged"
        );
        assert_eq!(
            *tool_probe.seen.lock().unwrap(),
            vec![serde_json::json!({
                "messages": serde_json::to_value(&tool_messages).unwrap(),
                "tools": serde_json::to_value(&tools).unwrap(),
                "max_output_tokens": 41,
            })],
            "the Vision request must route tools, messages, and output cap unchanged"
        );
    }

    #[test]
    fn router_selects_correct_endpoint() {
        let cfg = RouterConfig::default();
        let router = LlmRouter::new(cfg);
        let _sm = router.select_request(RequestKind::FastChat);
        let _re = router.select_request(RequestKind::Chat);
        let _mm = router.select_request(RequestKind::Vision);
        let _au = router.select_request(RequestKind::AudioChat);
        let _em = router.select_request(RequestKind::Embedding);
    }

    #[tokio::test]
    async fn chat_does_not_fail_over_to_another_provider() {
        let failing: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        });
        let healthy: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        });
        let router = LlmRouter::new_with_clients_full(
            healthy.clone(),
            failing,
            healthy.clone(),
            healthy.clone(),
            healthy,
        );
        {
            let mut config = router.config.write().await;
            config.retry_max_retries = 0;
            config.max_total_duration_secs = 1;
        }

        let error = router
            .complete(CompleteRequest::new(RequestKind::Chat, Vec::new()))
            .await
            .expect_err("the selected provider error must be returned");
        assert!(matches!(error, LlmError::ServerError(_)));
    }

    #[tokio::test]
    async fn open_primary_circuit_does_not_select_an_alternate_provider() {
        let failing: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        });
        let healthy: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        });
        let router = LlmRouter::new_with_clients_full(
            healthy.clone(),
            failing,
            healthy.clone(),
            healthy.clone(),
            healthy,
        );
        {
            let mut config = router.config.write().await;
            config.retry_max_retries = 0;
            config.max_total_duration_secs = 1;
        }

        for _ in 0..3 {
            router
                .complete(CompleteRequest::new(RequestKind::Chat, Vec::new()))
                .await
                .expect_err("the selected provider must remain the only target");
        }
        let before = router.endpoint_circuits.read().await;
        assert_eq!(before["default_model"].state, EndpointCircuitState::Open);
        assert_eq!(before["small_model"].consecutive_failures, 0);
        drop(before);

        // The fourth request must not skip to the other provider.
        let error = router
            .complete(CompleteRequest::new(RequestKind::Chat, Vec::new()))
            .await
            .expect_err("an open circuit must fail instead of changing namespace");
        assert!(matches!(error, LlmError::CircuitOpen { .. }));
        let after = router.endpoint_circuits.read().await;
        assert_eq!(after["default_model"].consecutive_failures, 3);
        assert_eq!(after["small_model"].consecutive_failures, 0);
    }

    #[tokio::test]
    async fn streaming_does_not_fail_over_before_visible_output() {
        let failing: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        });
        let healthy: Arc<dyn LlmClient> = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                tool_call_updates: Vec::new(),
                text: Some("recovered".into()),
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
        });
        let router = LlmRouter::new_with_clients_full(
            healthy.clone(),
            failing,
            healthy.clone(),
            healthy.clone(),
            healthy,
        );
        {
            let mut config = router.config.write().await;
            config.retry_max_retries = 0;
            config.max_total_duration_secs = 1;
        }

        let error = router
            .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], |_| {})
            .await
            .expect_err("stream errors must stay on the selected provider");
        assert!(matches!(error, LlmError::ServerError(_)));
    }

    #[tokio::test]
    async fn is_request_configured_reports_api_key_state() {
        let mut cfg = LlmRouter::test_config();
        cfg.model_mut("small_model").unwrap().endpoint.api_key = "sk-test".into();
        cfg.model_mut("default_model").unwrap().endpoint.api_key = String::new();
        cfg.model_mut("image_model").unwrap().endpoint.api_key = "sk-mm".into();
        cfg.model_mut("audio_model").unwrap().endpoint.api_key = "sk-au".into();
        cfg.model_mut("embedding_model").unwrap().endpoint.api_key = "sk-emb".into();
        let router = LlmRouter::new(cfg);
        assert!(
            router.is_request_configured(RequestKind::FastChat).await,
            "small_model api_key is set"
        );
        assert!(
            !router.is_request_configured(RequestKind::Chat).await,
            "default_model api_key is empty"
        );
        assert!(
            router.is_request_configured(RequestKind::Vision).await,
            "image_model api_key is set"
        );
        assert!(
            router.is_request_configured(RequestKind::AudioChat).await,
            "audio_model api_key is set"
        );
        assert!(
            router.is_request_configured(RequestKind::Embedding).await,
            "embedding_model api_key is set"
        );
    }

    #[tokio::test]
    async fn embed_routes_to_embedding_endpoint_and_tracks_health() {
        struct MockEmbedClient {
            seen: Arc<std::sync::Mutex<Vec<Vec<String>>>>,
        }
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
                self.seen.lock().unwrap().push(input.clone());
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
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let emb: Arc<dyn LlmClient> = Arc::new(MockEmbedClient { seen: seen.clone() });
        let router =
            LlmRouter::new_with_clients_full(chat.clone(), chat.clone(), chat.clone(), chat, emb);
        let input = vec!["a".into(), " b ".into(), "a".into()];
        let result = router
            .embed(EmbeddingRequest {
                input: input.clone(),
            })
            .await
            .unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![input]);
        assert_eq!(result.vectors.len(), 3);
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
            chat,
            Arc::new(FailingEmbed),
        );
        assert!(
            router
                .embed(EmbeddingRequest {
                    input: vec!["x".into()],
                })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn embed_empty_batch_short_circuits_without_a_configured_route() {
        let router = LlmRouter::new(RouterConfig::default());
        let embedding = router
            .embed(EmbeddingRequest { input: Vec::new() })
            .await
            .unwrap();

        assert!(embedding.vectors.is_empty());
        assert_eq!(embedding.model, None);
        assert_eq!(embedding.usage.total_tokens, 0);
    }

    #[tokio::test]
    async fn chat_stream_with_tools_aggregated_accumulates_text_and_tool_calls() {
        let chunks: Vec<Result<StreamChunk, LlmError>> = vec![
            Ok(StreamChunk {
                tool_call_updates: Vec::new(),
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
                tool_call_updates: Vec::new(),
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
                tool_call_updates: Vec::new(),
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
                tool_call_updates: Vec::new(),
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
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);

        use std::sync::Arc as StdArc;
        use std::sync::Mutex as StdMutex;
        let seen_text = StdArc::new(StdMutex::new(String::new()));
        let seen_clone = seen_text.clone();
        let resp = router
            .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], move |c| {
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
            _: Vec<LlmToolDefinition>,
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
            _: Vec<LlmToolDefinition>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let first_delay = self.first_delay;
            let gap_delay = self.gap_delay;
            let mk = |text: &'static str| {
                Ok(StreamChunk {
                    tool_call_updates: Vec::new(),
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
        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _messages: Arc<[CanonicalMessage]>,
            _tools: Arc<[LlmToolDefinition]>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let first_delay = self.first_delay;
            let gap_delay = self.gap_delay;
            let mk = |text: &'static str| {
                Ok(StreamChunk {
                    tool_call_updates: Vec::new(),
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
        crate::streaming::aggregate_stream_cancellable_shared(
            client,
            Arc::<[CanonicalMessage]>::from(Vec::new()),
            Arc::<[LlmToolDefinition]>::from(Vec::new()),
            on_chunk,
            CancellationToken::new(),
            &rules,
            idle_timeout,
            None,
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
                tool_call_updates: Vec::new(),
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
                tool_call_updates: Vec::new(),
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
                tool_call_updates: Vec::new(),
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
                tool_call_updates: Vec::new(),
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
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);

        use std::sync::Arc as StdArc;
        use std::sync::Mutex as StdMutex;
        let phases = StdArc::new(StdMutex::new(Vec::new()));
        let phases_clone = phases.clone();
        let resp = router
            .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], move |c| {
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
    async fn chat_small_model_uses_small_model_endpoint() {
        // Fast-chat request should be routed to the small_model test model.
        let small = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let default = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(
            small,
            default,
            Arc::new(MockStreamClient {
                chunks: Vec::new(),
                fail_chat: true,
            }),
            Arc::new(MockStreamClient {
                chunks: Vec::new(),
                fail_chat: true,
            }),
        );

        let resp = router
            .complete(CompleteRequest::new(RequestKind::FastChat, Vec::new()))
            .await
            .expect("small_model should succeed");
        assert_eq!(resp.text, "mock response");
    }

    #[tokio::test]
    async fn circuit_breaker_opens_after_failures() {
        let failing = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let ok = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                tool_call_updates: Vec::new(),
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

        let router = LlmRouter::new_with_clients(failing.clone(), failing.clone(), ok.clone(), ok);

        // First 3 calls should fail and trigger circuit breaker
        for _ in 0..3 {
            let _ = router
                .complete(CompleteRequest::new(RequestKind::Chat, Vec::new()))
                .await;
        }

        // Circuit breaker should reject requests directly
        let result = router.check_circuit("default_model").await;
        // Should fail because circuit is open
        assert!(result.is_err());
    }

    #[test]
    fn circuit_breaker_new_is_closed() {
        let mut cb = EndpointCircuitBreaker::new();
        assert!(cb.allow_request());
    }

    #[test]
    fn circuit_breaker_record_success_resets_consecutive_failures() {
        let mut cb = EndpointCircuitBreaker::new();
        cb.consecutive_failures = 5;
        cb.failure_count = 5;
        cb.total_calls = 5;
        cb.record_success();
        assert_eq!(cb.consecutive_failures, 0);
        assert_eq!(cb.total_calls, 6);
        assert_eq!(cb.state, EndpointCircuitState::Closed);
        assert!(cb.opened_at.is_none());
    }

    #[test]
    fn circuit_breaker_success_does_not_close_open_breaker() {
        // A stale success from a request dispatched before the breaker tripped
        // must NOT close it — only a HalfOpen probe may (M8).
        let mut cb = EndpointCircuitBreaker::new();
        cb.state = EndpointCircuitState::Open;
        cb.opened_at = Some(Instant::now());
        cb.consecutive_failures = 3;
        cb.record_success();
        assert_eq!(
            cb.state,
            EndpointCircuitState::Open,
            "open breaker stays open"
        );
        assert_eq!(cb.consecutive_failures, 3, "counters not reset");
        assert!(cb.opened_at.is_some());
        // Simulate the cooldown elapsing: probe goes HalfOpen, its success closes.
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert_eq!(cb.state, EndpointCircuitState::HalfOpen);
        cb.record_success();
        assert_eq!(cb.state, EndpointCircuitState::Closed);
    }

    #[test]
    fn circuit_breaker_record_failure_increments_counters() {
        let mut cb = EndpointCircuitBreaker::new();
        cb.record_failure();
        assert_eq!(cb.consecutive_failures, 1);
        assert_eq!(cb.failure_count, 1);
        assert_eq!(cb.total_calls, 1);
    }

    #[test]
    fn circuit_breaker_opens_at_threshold() {
        let mut cb = EndpointCircuitBreaker::new();
        cb.record_failure();
        cb.record_failure();
        assert!(cb.allow_request());
        cb.record_failure();
        // Three consecutive failures open the breaker regardless of older
        // successes.
        assert!(!cb.allow_request());
    }

    #[test]
    fn circuit_breaker_ignores_historical_success_rate() {
        let mut cb = EndpointCircuitBreaker::new();
        for _ in 0..100 {
            cb.record_success();
        }
        for _ in 0..3 {
            cb.record_failure();
        }
        assert_eq!(cb.state, EndpointCircuitState::Open);
        assert!(!cb.allow_request());
    }

    #[test]
    fn circuit_breaker_half_open_after_cooldown() {
        let mut cb = EndpointCircuitBreaker::new();
        // Force open with a past timestamp
        cb.state = EndpointCircuitState::Open;
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert_eq!(cb.state, EndpointCircuitState::HalfOpen);
    }

    #[test]
    fn circuit_breaker_allows_only_one_half_open_probe() {
        let mut cb = EndpointCircuitBreaker::new();
        cb.state = EndpointCircuitState::Open;
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert!(!cb.allow_request());
        cb.record_failure();
        assert_eq!(cb.state, EndpointCircuitState::Open);
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        cb.record_success();
        assert_eq!(cb.state, EndpointCircuitState::Closed);
    }

    #[test]
    fn circuit_breaker_stays_open_within_cooldown() {
        let mut cb = EndpointCircuitBreaker::new();
        cb.state = EndpointCircuitState::Open;
        cb.opened_at = Some(Instant::now());
        assert!(!cb.allow_request());
    }

    #[test]
    fn circuit_breaker_full_state_transition_cycle() {
        let mut cb = EndpointCircuitBreaker::new();
        // Closed → Open
        for _ in 0..3 {
            cb.record_failure();
        }
        assert!(!cb.allow_request());
        // Open → HalfOpen (simulate cooldown elapsed)
        cb.state = EndpointCircuitState::Open;
        cb.opened_at = Some(Instant::now() - Duration::from_secs(31));
        assert!(cb.allow_request());
        assert_eq!(cb.state, EndpointCircuitState::HalfOpen);
        // HalfOpen → Closed (on success)
        cb.record_success();
        assert_eq!(cb.state, EndpointCircuitState::Closed);
    }

    #[test]
    fn endpoint_circuit_breaker_manual_retry_resets_streak_and_preserves_counts() {
        let mut breaker = EndpointCircuitBreaker::new();
        breaker.state = EndpointCircuitState::Open;
        breaker.consecutive_failures = 3;
        breaker.failure_count = 3;
        breaker.total_calls = 7;
        breaker.opened_at = Some(Instant::now());

        breaker.reset_for_manual_retry();

        assert_eq!(breaker.state, EndpointCircuitState::Closed);
        assert_eq!(breaker.consecutive_failures, 0);
        assert_eq!(breaker.failure_count, 3);
        assert_eq!(breaker.total_calls, 7);
        assert!(breaker.opened_at.is_none());
    }

    #[tokio::test]
    async fn production_router_allows_fenced_code_output_by_default() {
        let router = LlmRouter::new(RouterConfig::default());
        assert!(
            router
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
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        let result = router
            .health_check(HealthCheckRequest {
                request: RequestKind::Chat,
            })
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn health_check_request_preserves_unconfigured_route_semantics() {
        let router = LlmRouter::new(RouterConfig::default());
        let result = router
            .health_check(HealthCheckRequest {
                request: RequestKind::Chat,
            })
            .await;
        assert!(matches!(result, Err(LlmError::Configuration(_))));

        let report = router.connection_status(RequestKind::Chat).await;
        assert_eq!(report.status, LlmConnectionStatus::Unconfigured);
    }

    #[tokio::test]
    async fn open_circuit_probe_is_classified_and_manual_retry_can_probe_again() {
        let probe = Arc::new(RouterRequestProbe::default());
        let client: Arc<dyn LlmClient> = probe.clone();
        let router = LlmRouter::new_with_clients_full(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        router
            .force_request_configured(RequestKind::Chat, true)
            .await;
        {
            let mut circuits = router.endpoint_circuits.write().await;
            let endpoint = circuits.get_mut("default_model").unwrap();
            endpoint.state = EndpointCircuitState::Open;
            endpoint.consecutive_failures = 3;
            endpoint.opened_at = Some(Instant::now());
        }

        let blocked = router.connection_status(RequestKind::Chat).await;
        assert_eq!(blocked.status, LlmConnectionStatus::Disconnected);
        assert_eq!(
            blocked.reason,
            Some(crate::types::LlmConnectionFailureReason::CircuitOpen)
        );
        assert_eq!(
            probe.health_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an open-circuit status result must not claim a provider probe ran"
        );

        router.prepare_manual_retry(RequestKind::Chat).await;
        let recovered = router.connection_status(RequestKind::Chat).await;
        assert_eq!(recovered.status, LlmConnectionStatus::Ready);
        assert_eq!(
            probe.health_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "manual retry must clear the open gate so the next request reaches the provider"
        );
    }

    #[tokio::test]
    async fn health_and_native_transcription_share_the_transcription_capability_route() {
        let probe = Arc::new(RouterRequestProbe::default());
        let client: Arc<dyn LlmClient> = probe.clone();
        let router = LlmRouter::new_with_clients_full(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        router
            .force_request_configured(RequestKind::Transcription, true)
            .await;

        router
            .health_check(HealthCheckRequest {
                request: RequestKind::Transcription,
            })
            .await
            .expect("the transcription route must accept its declared capability");
        let transcription = router
            .transcribe_audio(&[0; 44])
            .await
            .expect("the native transcription route must be called");
        assert_eq!(transcription.text, "native transcript");
        assert_eq!(
            probe.health_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            probe
                .transcription_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        {
            let mut config = router.config.write().await;
            config
                .model_mut("audio_model")
                .expect("test audio model")
                .capabilities
                .retain(|capability| *capability != Capability::Transcription);
            router
                .model_directory
                .rebuild_primary_routes(&config, RouteMode::InjectedClients);
        }

        assert!(router.is_request_configured(RequestKind::AudioChat).await);
        assert!(
            !router
                .is_request_configured(RequestKind::Transcription)
                .await
        );
        assert!(matches!(
            router
                .health_check(HealthCheckRequest {
                    request: RequestKind::Transcription,
                })
                .await,
            Err(LlmError::Configuration(message)) if message == "no configured model for transcription"
        ));
        assert!(matches!(
            router.transcribe_audio(&[0; 44]).await,
            Err(LlmError::RequestFailed(message)) if message.contains("transcription request is not configured")
        ));
        assert_eq!(
            probe.health_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a route missing the transcription capability must not health-probe its model"
        );
        assert_eq!(
            probe
                .transcription_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a route missing the transcription capability must not invoke native STT"
        );
        assert_eq!(
            probe.llm_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a missing native route must not silently fall back to AudioChat"
        );
    }

    #[tokio::test]
    async fn metadata_helpers_do_not_call_providers_or_project_circuit_state_or_usage() {
        let probe = Arc::new(RouterRequestProbe::default());
        let client: Arc<dyn LlmClient> = probe.clone();
        let router = LlmRouter::new_with_clients_full(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        );
        router
            .force_request_configured(RequestKind::Chat, true)
            .await;
        {
            let mut config = router.config.write().await;
            let endpoint = &mut config
                .model_mut("default_model")
                .expect("test chat model")
                .endpoint;
            endpoint.context_window = Some(8_192);
            endpoint.max_tokens = 2_048;
            endpoint.cost_per_1k_input_tokens = 0.001;
            endpoint.cost_per_1k_output_tokens = 0.002;
        }

        let circuit_state_before = router
            .endpoint_circuits
            .read()
            .await
            .iter()
            .map(|(model_id, health)| {
                (
                    model_id.clone(),
                    (health.consecutive_failures, health.total_calls),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert!(router.is_request_configured(RequestKind::Chat).await);
        let _adapter = router.select_request(RequestKind::Chat);
        let _profile = router.capability_profile_for_request(RequestKind::Chat);
        assert_eq!(
            router.context_window_for_request(RequestKind::Chat).await,
            8_192
        );
        assert_eq!(
            router
                .effective_output_tokens(RequestKind::Chat, 3_000)
                .await,
            2_048
        );
        let usage = Usage {
            prompt_tokens: 1_000,
            completion_tokens: 1_000,
            total_tokens: 2_000,
            ..Usage::default()
        };
        assert!(
            router
                .compute_cost(RequestKind::Chat, &usage)
                .await
                .is_some()
        );
        assert!(usage.cost.is_none(), "cost lookup must not mutate usage");

        assert_eq!(probe.llm_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(
            probe.health_calls.load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        assert_eq!(
            probe
                .transcription_calls
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        let circuit_state_after = router
            .endpoint_circuits
            .read()
            .await
            .iter()
            .map(|(model_id, health)| {
                (
                    model_id.clone(),
                    (health.consecutive_failures, health.total_calls),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(circuit_state_after, circuit_state_before);
        assert!(router.rate_limited.read().await.is_empty());
    }

    #[tokio::test]
    async fn set_stream_rules_and_check_output() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
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

    #[tokio::test]
    async fn chat_stream_with_tools_no_chunks() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        let resp = router
            .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], |_| {})
            .await
            .expect("aggregation succeeds");
        assert!(resp.text.is_empty());
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn chat_stream_call_succeeds_primary() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                tool_call_updates: Vec::new(),
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
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        let result = router.chat_stream(RequestKind::Chat, vec![]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn raw_chat_stream_keeps_request_kind_route_selection() {
        struct NamedStreamClient(&'static str);

        #[async_trait]
        impl LlmClient for NamedStreamClient {
            async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
                Ok(LlmResponse::default())
            }

            async fn chat_stream(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                Ok(Box::pin(stream::iter([Ok(StreamChunk {
                    tool_call_updates: Vec::new(),
                    text: Some(self.0.into()),
                    ..StreamChunk::default()
                })])))
            }

            async fn health_check(&self) -> Result<(), LlmError> {
                Ok(())
            }
        }

        let router = LlmRouter::new_with_clients(
            Arc::new(NamedStreamClient("fast-chat route")),
            Arc::new(NamedStreamClient("chat route")),
            Arc::new(NamedStreamClient("vision route")),
            Arc::new(NamedStreamClient("audio route")),
        );

        for (request, expected) in [
            (RequestKind::FastChat, "fast-chat route"),
            (RequestKind::Chat, "chat route"),
        ] {
            let mut stream = router.chat_stream(request, Vec::new()).await.unwrap();
            let chunk = stream.next().await.unwrap().unwrap();
            assert_eq!(chunk.text.as_deref(), Some(expected));
        }
    }

    /// Mock that tracks how many calls are in flight concurrently and stalls
    /// briefly, so the per-model semaphore's serialization is observable.
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
        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(messages).await
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
    async fn per_model_semaphore_caps_concurrent_requests() {
        let concurrent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let probe: Arc<dyn LlmClient> = Arc::new(ConcurrencyProbe {
            concurrent: concurrent.clone(),
            max_seen: max_seen.clone(),
        });
        let router =
            LlmRouter::new_with_clients(probe.clone(), probe.clone(), probe.clone(), probe);
        // Cap the default test model at 1 in-flight request.
        router.set_request_limit_for_test(1);

        let router = Arc::new(router);
        let mut handles = Vec::new();
        for _ in 0..3 {
            let router = router.clone();
            handles.push(tokio::spawn(async move {
                router
                    .complete(CompleteRequest::new(RequestKind::Chat, vec![]))
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
            "per-model limit 1 must serialize concurrent calls to the same model"
        );
        // Different models have independent permits: small_model can proceed
        // while default_model is capped.
        router.set_request_limit_for_test(1);
        let _ = router
            .complete(CompleteRequest::new(RequestKind::FastChat, vec![]))
            .await
            .unwrap();
        assert_eq!(max_seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    struct AggregatedPermitProbe {
        stream_calls: Arc<std::sync::atomic::AtomicUsize>,
        first_stream_waiting: Arc<tokio::sync::Notify>,
        first_chunk_gate: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait]
    impl LlmClient for AggregatedPermitProbe {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(Unknown("aggregated permit probe does not chat".into()))
        }

        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(Unknown(
                "aggregated permit probe does not raw-stream".into(),
            ))
        }

        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _: Arc<[CanonicalMessage]>,
            _: Arc<[LlmToolDefinition]>,
            _: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            let call = self
                .stream_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                let first_stream_waiting = self.first_stream_waiting.clone();
                let first_chunk_gate = self.first_chunk_gate.clone();
                let stream = stream::once(async move {
                    first_stream_waiting.notify_one();
                    first_chunk_gate
                        .acquire_owned()
                        .await
                        .expect("test chunk gate remains open")
                        .forget();
                    Ok(StreamChunk {
                        tool_call_updates: Vec::new(),
                        text: Some("first request".into()),
                        ..StreamChunk::default()
                    })
                });
                Ok(Box::pin(stream))
            } else {
                Ok(Box::pin(stream::empty()))
            }
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn aggregated_stream_holds_model_permit_through_stream_consumption() {
        let probe = Arc::new(AggregatedPermitProbe {
            stream_calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            first_stream_waiting: Arc::new(tokio::sync::Notify::new()),
            first_chunk_gate: Arc::new(tokio::sync::Semaphore::new(0)),
        });
        let first_stream_waiting = probe.first_stream_waiting.clone();
        let first_chunk_gate = probe.first_chunk_gate.clone();
        let stream_calls = probe.stream_calls.clone();
        let client: Arc<dyn LlmClient> = probe.clone();
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        router.set_request_limit_for_test(1);

        let first_router = router.clone();
        let first = tokio::spawn(async move {
            first_router
                .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], |_| {})
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), first_stream_waiting.notified())
            .await
            .expect("the first provider stream should wait for its first chunk");
        assert_eq!(router.model_permit("default_model").available_permits(), 0);

        let second_router = router.clone();
        let mut second = tokio::spawn(async move {
            second_router
                .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], |_| {})
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(
            stream_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the queued logical stream must not reach the provider while the first is aggregating"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut second)
                .await
                .is_err(),
            "the second aggregate call stays queued until the first releases its permit"
        );

        first_chunk_gate.add_permits(1);
        first.await.unwrap().expect("first stream completes");
        second
            .await
            .unwrap()
            .expect("queued stream runs after permit release");
        assert_eq!(stream_calls.load(std::sync::atomic::Ordering::SeqCst), 2);
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
        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(messages).await
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
        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _: Arc<[CanonicalMessage]>,
            _: Arc<[LlmToolDefinition]>,
            _: Option<u32>,
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
    async fn raw_chat_stream_projects_rate_limit_once_to_router_state() {
        let client: Arc<dyn LlmClient> = Arc::new(AlwaysRateLimited);
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        {
            let mut config = router.config.write().await;
            config.retry_max_retries = 0;
        }

        let result = router.chat_stream(RequestKind::Chat, Vec::new()).await;
        let error = match result {
            Ok(_) => panic!("rate-limited stream setup returns its provider error"),
            Err(error) => error,
        };
        assert!(matches!(error, LlmError::RateLimit { .. }));

        let circuits = router.endpoint_circuits.read().await;
        assert_eq!(circuits["default_model"].consecutive_failures, 1);
        drop(circuits);
        let deadline = router
            .rate_limit_deadline_for_test("default_model")
            .await
            .expect("raw stream 429 establishes the shared cooldown");
        assert!(deadline > Instant::now());
    }

    #[tokio::test]
    async fn aggregated_stream_projects_rate_limit_once_to_router_state() {
        let client: Arc<dyn LlmClient> = Arc::new(AlwaysRateLimited);
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        router.config.write().await.retry_max_retries = 0;

        let error = router
            .chat_stream_with_tools_aggregated(RequestKind::Chat, &[], &[], |_| {})
            .await
            .expect_err("aggregated stream setup preserves the provider rate-limit error");
        assert!(matches!(error, LlmError::RateLimit { .. }));

        assert_eq!(
            router.endpoint_circuits.read().await["default_model"].consecutive_failures,
            1,
            "one logical stream result is projected once"
        );
        assert!(
            router
                .rate_limit_deadline_for_test("default_model")
                .await
                .is_some_and(|deadline| deadline > Instant::now())
        );
    }

    #[tokio::test]
    async fn rate_limit_sets_shared_cooldown_for_model() {
        let client: Arc<dyn LlmClient> = Arc::new(AlwaysRateLimited);
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        // Fast retry pacing so the RateLimit error surfaces immediately.
        let cfg = RouterConfig {
            retry_max_retries: 0,
            retry_base_secs: 0,
            retry_factor: 1,
            retry_max_secs: 0,
            retry_jitter: 0.0,
            ..Default::default()
        };
        *router.config.write().await = cfg;

        let model_id = "default_model";
        let err = router
            .complete(CompleteRequest::new(RequestKind::Chat, vec![]))
            .await
            .unwrap_err();
        assert!(
            matches!(err, LlmError::RateLimit { .. }),
            "first call must surface the RateLimit failure: {}",
            err
        );
        // The cooldown deadline was recorded before returning the provider
        // error, so subsequent callers wait it out.
        let deadline = router.rate_limit_deadline_for_test(model_id).await;
        assert!(
            deadline.is_some_and(|d| d > Instant::now()),
            "cooldown deadline must be set in the future"
        );
        // A second call (a different session) paces behind the cooldown instead
        // of firing immediately: it must take at least the Retry-After before
        // dispatching (it fails again, but only after the shared wait).
        let t0 = Instant::now();
        let err2 = router
            .complete(CompleteRequest::new(RequestKind::Chat, vec![]))
            .await
            .unwrap_err();
        assert!(matches!(err2, LlmError::RateLimit { .. }));
        assert!(
            t0.elapsed() >= Duration::from_millis(250),
            "second call must wait out the shared cooldown (elapsed {:?})",
            t0.elapsed()
        );
        // A third caller starting later re-uses the (still running) cooldown
        // window: the deadline only moves forward, never backward.
        let deadline2 = router.rate_limit_deadline_for_test(model_id).await;
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
        let mut cfg = LlmRouter::test_config();
        let default_model = cfg.model_mut("default_model").unwrap();
        default_model.endpoint.model_name = "gpt-4o-mini".into(); // catalog: 128k
        default_model.endpoint.max_tokens = 1_000_000; // absurd cap floor
        let router = LlmRouter::new(cfg);
        let built = router.config.try_read().expect("router config readable");
        let default_model = built.model("default_model").unwrap();
        assert!(
            default_model.endpoint.max_tokens <= 128_000,
            "max_tokens must be clamped to the resolved context window, got {}",
            default_model.endpoint.max_tokens
        );
    }

    #[tokio::test]
    async fn effective_output_tokens_leaves_request_safety_margin() {
        let mut cfg = LlmRouter::test_config();
        cfg.model_mut("default_model")
            .unwrap()
            .endpoint
            .context_window = Some(4_096);
        cfg.model_mut("default_model").unwrap().endpoint.max_tokens = 8_192;
        cfg.model_mut("default_model").unwrap().endpoint.api_key = "sk-test".into();
        let router = LlmRouter::with_default_context_window(cfg, 128_000);

        assert_eq!(
            router.context_window_for_request(RequestKind::Chat).await,
            4_096
        );
        // 4,096 - 3,000 input - 256 safety margin.
        assert_eq!(
            router
                .effective_output_tokens(RequestKind::Chat, 3_000)
                .await,
            840
        );
        assert_eq!(
            router
                .effective_output_tokens(RequestKind::Chat, 10_000)
                .await,
            1,
            "an over-window request still receives a valid positive provider cap"
        );
    }

    #[tokio::test]
    async fn complete_preserves_chat_tool_branch_output_cap_and_usage() {
        struct ChatPathProbe(Arc<StdMutex<Vec<(bool, Option<u32>, usize)>>>);

        fn response() -> LlmResponse {
            LlmResponse {
                text: "ok".into(),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: Usage {
                    prompt_tokens: 23,
                    completion_tokens: 11,
                    total_tokens: 34,
                    ..Usage::default()
                },
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }
        }

        #[async_trait]
        impl LlmClient for ChatPathProbe {
            async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
                Ok(response())
            }

            async fn chat_with_output_cap(
                &self,
                _: Vec<CanonicalMessage>,
                max_output_tokens: Option<u32>,
            ) -> Result<LlmResponse, LlmError> {
                self.0.lock().unwrap().push((false, max_output_tokens, 0));
                Ok(response())
            }

            async fn chat_with_tools_output_cap(
                &self,
                _: Vec<CanonicalMessage>,
                tools: Vec<LlmToolDefinition>,
                max_output_tokens: Option<u32>,
            ) -> Result<LlmResponse, LlmError> {
                self.0
                    .lock()
                    .unwrap()
                    .push((true, max_output_tokens, tools.len()));
                Ok(response())
            }

            async fn chat_stream(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                Err(LlmError::UnsupportedCapability("not used".into()))
            }

            async fn health_check(&self) -> Result<(), LlmError> {
                Ok(())
            }
        }

        let seen = Arc::new(StdMutex::new(Vec::new()));
        let client: Arc<dyn LlmClient> = Arc::new(ChatPathProbe(seen.clone()));
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        let ordinary = router
            .complete(
                CompleteRequest::new(RequestKind::Chat, Vec::new()).with_max_output_tokens(37),
            )
            .await
            .unwrap();
        let empty_tools = router
            .complete(
                CompleteRequest::new(RequestKind::Chat, Vec::new())
                    .with_tools(Vec::new())
                    .with_max_output_tokens(39),
            )
            .await
            .unwrap();
        let tools = vec![LlmToolDefinition {
            tool_type: "function".into(),
            function: crate::types::ToolFunction {
                name: "probe".into(),
                description: "test tool".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let tool_response = router
            .complete(
                CompleteRequest::new(RequestKind::Chat, Vec::new())
                    .with_tools(tools)
                    .with_max_output_tokens(41),
            )
            .await
            .unwrap();

        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                (false, Some(37), 0),
                (false, Some(39), 0),
                (true, Some(41), 1)
            ]
        );
        for result in [ordinary, empty_tools, tool_response] {
            assert_eq!(result.usage.prompt_tokens, 23);
            assert_eq!(result.usage.completion_tokens, 11);
            assert_eq!(result.usage.total_tokens, 34);
        }
    }

    #[tokio::test]
    async fn complete_rejects_request_without_required_capability() {
        let mut config = LlmRouter::test_config();
        config
            .model_mut("image_model")
            .unwrap()
            .capabilities
            .retain(|capability| *capability != Capability::Vision);
        let router = LlmRouter::new(config);

        let error = router
            .complete(CompleteRequest::new(RequestKind::Vision, Vec::new()))
            .await
            .expect_err("a model without vision capability must not be routed");

        assert!(
            matches!(error, LlmError::Configuration(message) if message.contains("no configured model for vision"))
        );
    }

    #[tokio::test]
    async fn cancellable_chat_returns_cancelled_while_client_is_pending() {
        struct PendingClient {
            stream_started: Arc<std::sync::atomic::AtomicBool>,
        }

        #[async_trait]
        impl LlmClient for PendingClient {
            async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
                std::future::pending().await
            }

            async fn chat_with_output_cap(
                &self,
                _: Vec<CanonicalMessage>,
                _: Option<u32>,
            ) -> Result<LlmResponse, LlmError> {
                std::future::pending().await
            }

            async fn chat_stream(
                &self,
                _: Vec<CanonicalMessage>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                std::future::pending().await
            }

            async fn chat_stream_with_tools_output_cap_shared(
                &self,
                _: Arc<[CanonicalMessage]>,
                _: Arc<[LlmToolDefinition]>,
                _: Option<u32>,
            ) -> Result<
                Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
                LlmError,
            > {
                self.stream_started
                    .store(true, std::sync::atomic::Ordering::Release);
                std::future::pending().await
            }

            async fn health_check(&self) -> Result<(), LlmError> {
                Ok(())
            }
        }

        let stream_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client: Arc<dyn LlmClient> = Arc::new(PendingClient {
            stream_started: stream_started.clone(),
        });
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task_router = router.clone();
        let task = tokio::spawn(async move {
            task_router
                .chat_messages_cancellable(RequestKind::Chat, Vec::new(), Some(64), task_cancel)
                .await
        });
        tokio::task::yield_now().await;
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(LlmError::Cancelled)));

        let stream_cancel = CancellationToken::new();
        let task_cancel = stream_cancel.clone();
        let task_router = router.clone();
        let stream_task = tokio::spawn(async move {
            let messages = Vec::new();
            let tools = Vec::new();
            task_router
                .chat_stream_with_tools_aggregated_cancellable_with_attempts(
                    StreamRequest {
                        request: RequestKind::Chat,
                        messages: &messages,
                        tools: &tools,
                        max_output_tokens: Some(64),
                    },
                    StreamAttemptHooks::new(
                        |_| {},
                        |_| {},
                        StreamAttemptOutputDisposition::PreserveExisting,
                    ),
                    task_cancel,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !stream_started.load(std::sync::atomic::Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the provider stream should start before cancellation");
        stream_cancel.cancel();
        assert!(matches!(
            stream_task.await.unwrap(),
            Err(LlmError::Cancelled)
        ));
        assert_eq!(
            router.endpoint_circuits.read().await["default_model"].consecutive_failures,
            0,
            "cancellation is not a provider health failure"
        );

        // Cancellation wins even when the provider future is immediately ready.
        let ready_client = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let ready_router = LlmRouter::new_with_clients(
            ready_client.clone(),
            ready_client.clone(),
            ready_client.clone(),
            ready_client,
        );
        let already_cancelled = CancellationToken::new();
        already_cancelled.cancel();
        let result = ready_router
            .chat_messages_cancellable(RequestKind::Chat, Vec::new(), None, already_cancelled)
            .await;
        assert!(matches!(result, Err(LlmError::Cancelled)));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "manual performance profile; run with --ignored --nocapture"]
    async fn llm_request_latency_and_concurrency_profile_with_mock_provider() {
        const WARMUP_COUNT: usize = 16;
        const IMMEDIATE_SAMPLE_COUNT: usize = 513;
        const QUEUED_BATCHES: usize = 16;
        const BATCH_SIZE: usize = 32;
        const PER_MODEL_LIMIT: usize = 8;

        let immediate: Arc<dyn LlmClient> = Arc::new(RequestProfileClient {
            service_delay: Duration::ZERO,
        });
        let router = LlmRouter::new_with_clients(
            immediate.clone(),
            immediate.clone(),
            immediate.clone(),
            immediate,
        );
        for _ in 0..WARMUP_COUNT {
            router
                .complete(request_profile_request())
                .await
                .expect("immediate mock request");
        }

        let immediate_wall_started = Instant::now();
        let mut immediate_samples_ns = Vec::with_capacity(IMMEDIATE_SAMPLE_COUNT);
        for _ in 0..IMMEDIATE_SAMPLE_COUNT {
            let started = Instant::now();
            router
                .complete(request_profile_request())
                .await
                .expect("immediate mock request");
            immediate_samples_ns.push(started.elapsed().as_nanos());
        }
        let immediate_wall = immediate_wall_started.elapsed();
        let immediate_p50 = request_profile_distribution(&immediate_samples_ns, 50);
        let immediate_p95 = request_profile_distribution(&immediate_samples_ns, 95);
        println!(
            "profile llm_request provider=mock_immediate samples={IMMEDIATE_SAMPLE_COUNT} warmup={WARMUP_COUNT} boundary=router_complete_to_response p50_us={immediate_p50:.2} p95_us={immediate_p95:.2} throughput_per_s={:.1}",
            IMMEDIATE_SAMPLE_COUNT as f64 / immediate_wall.as_secs_f64(),
        );

        let delayed: Arc<dyn LlmClient> = Arc::new(RequestProfileClient {
            service_delay: Duration::from_millis(2),
        });
        let queued_router = Arc::new(LlmRouter::new_with_clients(
            delayed.clone(),
            delayed.clone(),
            delayed.clone(),
            delayed,
        ));
        queued_router.semaphores.lock().unwrap().insert(
            "default_model".into(),
            Arc::new(tokio::sync::Semaphore::new(PER_MODEL_LIMIT)),
        );

        let mut queued_samples_ns = Vec::with_capacity(QUEUED_BATCHES * BATCH_SIZE);
        let mut queued_wall = Duration::ZERO;
        for _ in 0..QUEUED_BATCHES {
            let futures = (0..BATCH_SIZE).map(|_| {
                let router = queued_router.clone();
                async move {
                    let started = Instant::now();
                    router
                        .complete(request_profile_request())
                        .await
                        .expect("delayed mock request");
                    started.elapsed().as_nanos()
                }
            });
            let batch_started = Instant::now();
            queued_samples_ns.extend(futures_util::future::join_all(futures).await);
            queued_wall += batch_started.elapsed();
        }
        let queued_p50 = request_profile_distribution(&queued_samples_ns, 50);
        let queued_p95 = request_profile_distribution(&queued_samples_ns, 95);
        let queued_total = QUEUED_BATCHES * BATCH_SIZE;
        println!(
            "profile llm_request provider=mock_fixed_delay service_delay_ms=2 samples={queued_total} batches={QUEUED_BATCHES} concurrency_per_model={PER_MODEL_LIMIT} requests_per_batch={BATCH_SIZE} p50_us={queued_p50:.2} p95_us={queued_p95:.2} throughput_per_s={:.1}",
            queued_total as f64 / queued_wall.as_secs_f64(),
        );
    }
}
