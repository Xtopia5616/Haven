use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;


use crate::client::{HttpLlmClient, LlmClient, with_retry};
use crate::stream_rules::{check_stream_rules, StreamRule, StreamRuleMatch, StreamRuleMode};
use crate::types::{ContentPart, FinishReason, LlmError, LlmMessage, LlmResponse, LlmRole, StreamChunk, ToolDefinition};
use futures_util::StreamExt;
use haven_common::config::LlmConfig;

#[derive(Debug, Clone, Copy)]
pub enum EndpointRole {
    DefaultModel,
    BalancedModel,
}

// ---------------------------------------------------------------------------
// §2.6: Circuit Breaker state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum CircuitState {
    Closed,       // normal operation
    Open,         // failing — requests rejected
    HalfOpen,     // probe one request
}

#[derive(Debug, Clone)]
struct CircuitBreaker {
    state: CircuitState,
    consecutive_failures: u32,
    last_failure_time: Option<Instant>,
    failure_count: u32,     // failures in recent window
    total_calls: u32,       // total calls in recent window
    opened_at: Option<Instant>,
}

impl CircuitBreaker {
    fn new() -> Self {
        Self {
            state: CircuitState::Closed,
            consecutive_failures: 0,
            last_failure_time: None,
            failure_count: 0,
            total_calls: 0,
            opened_at: None,
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.total_calls += 1;
        self.state = CircuitState::Closed;
        self.opened_at = None;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.failure_count += 1;
        self.total_calls += 1;
        self.last_failure_time = Some(Instant::now());

        // Open if >50% failure rate and >=3 consecutive failures
        if self.consecutive_failures >= 3
            && self.total_calls > 0
            && (self.failure_count as f32 / self.total_calls as f32) > 0.5
        {
            self.state = CircuitState::Open;
            self.opened_at = Some(Instant::now());
        }
    }

    fn allow_request(&mut self) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                // §2.6: 30s cool-down, then HalfOpen
                if let Some(opened) = self.opened_at {
                    if opened.elapsed() >= Duration::from_secs(30) {
                        self.state = CircuitState::HalfOpen;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
        }
    }

}

// ---------------------------------------------------------------------------
// Per-endpoint health state (§5.3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct EndpointHealth {
    consecutive_failures: u32,
    last_failure_time: Option<Instant>,
    is_healthy: bool,
    circuit_breaker: CircuitBreaker,
}

impl EndpointHealth {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            last_failure_time: None,
            is_healthy: true,
            circuit_breaker: CircuitBreaker::new(),
        }
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.is_healthy = true;
        self.circuit_breaker.record_success();
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_failure_time = Some(Instant::now());
        self.circuit_breaker.record_failure();
        // Mark unhealthy after 3 consecutive failures
        if self.consecutive_failures >= 3 {
            self.is_healthy = false;
        }
    }

    fn allow_request(&mut self) -> bool {
        self.circuit_breaker.allow_request()
    }
}



pub struct LlmRouter {
    config: Arc<RwLock<LlmConfig>>,
    pub reasoner: Arc<dyn LlmClient>,
    pub fallback: Arc<dyn LlmClient>,
    fallback_active: AtomicBool,
    // §5.3: per-endpoint health (index 0 = reasoner, 1 = fallback)
    health: RwLock<[EndpointHealth; 2]>,
    /// Stream rules that are checked against accumulated output (§3.7)
    stream_rules: RwLock<Vec<StreamRule>>,
}

impl LlmRouter {
    pub fn new(config: LlmConfig) -> Self {
        let reasoner = Arc::new(HttpLlmClient::new(config.default_model.clone()));
        let fallback = Arc::new(HttpLlmClient::new(config.balanced_model.clone()));
        Self {
            config: Arc::new(RwLock::new(config)),
            reasoner,
            fallback,
            fallback_active: AtomicBool::new(false),
            health: RwLock::new([EndpointHealth::new(), EndpointHealth::new()]),
            stream_rules: RwLock::new(Vec::new()),
        }
    }

    pub fn new_with_clients(
        reasoner: Arc<dyn LlmClient>,
        fallback: Arc<dyn LlmClient>,
    ) -> Self {
        Self {
            config: Arc::new(RwLock::new(LlmConfig::default())),
            reasoner,
            fallback,
            fallback_active: AtomicBool::new(false),
            health: RwLock::new([EndpointHealth::new(), EndpointHealth::new()]),
            stream_rules: RwLock::new(Vec::new()),
        }
    }

    pub fn select_endpoint(&self, role: EndpointRole) -> Arc<dyn LlmClient> {
        match role {
            EndpointRole::DefaultModel => self.reasoner.clone(),
            EndpointRole::BalancedModel => self.fallback.clone(),
        }
    }

    fn health_index(role: &EndpointRole) -> usize {
        match role {
            EndpointRole::DefaultModel => 0,
            EndpointRole::BalancedModel => 1,
        }
    }

    fn health(&self, role: &EndpointRole) -> usize {
        Self::health_index(role)
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
    async fn with_total_timeout<F, Fut>(
        &self,
        f: F,
    ) -> Result<LlmResponse, LlmError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<LlmResponse, LlmError>>,
    {
        let cfg = self.config.read().await;
        let max_dur = cfg.max_total_duration_secs;
        drop(cfg);

        match tokio::time::timeout(Duration::from_secs(max_dur), f()).await {
            Ok(result) => result,
            Err(_) => Err(LlmError::Timeout(format!(
                "router total timeout after {}s",
                max_dur
            ))),
        }
    }

    // §2.11: execute with retry on endpoint, fallback with retry
    async fn call_with_retry_and_fallback(
        &self,
        primary: Arc<dyn LlmClient>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        role: &EndpointRole,
    ) -> Result<LlmResponse, LlmError> {
        let cfg = self.config.read().await;
        let base = cfg.retry_base_secs;
        let factor = cfg.retry_factor;
        let max_secs = cfg.retry_max_secs;
        let jitter = cfg.retry_jitter;
        drop(cfg);

        let primary_result = if tools.is_empty() {
            with_retry(3, base, factor, max_secs, jitter, None, || async {
                primary.chat(messages.clone()).await
            })
            .await
        } else {
            with_retry(3, base, factor, max_secs, jitter, None, || async {
                primary
                    .chat_with_tools(messages.clone(), tools.clone())
                    .await
            })
            .await
        };

        match primary_result {
            Ok(v) => {
                self.record_success(role).await;
                self.fallback_active.store(false, Ordering::SeqCst);
                Ok(v)
            }
            Err(primary_err) => {
                // §2.13: preserve primary error
                self.record_failure(role).await;
                let primary_msg = primary_err.to_string();
                tracing::warn!(
                    "primary endpoint failed: {}, attempting fallback",
                    primary_msg
                );
                self.fallback_active.store(true, Ordering::SeqCst);

                // §2.11: fallback also gets retry
                let fallback_result = if tools.is_empty() {
                    with_retry(2, base, factor, max_secs, jitter, None, || async {
                        self.fallback.chat(messages.clone()).await
                    })
                    .await
                } else {
                    with_retry(2, base, factor, max_secs, jitter, None, || async {
                        self.fallback
                            .chat_with_tools(messages.clone(), tools.clone())
                            .await
                    })
                    .await
                };

                match fallback_result {
                    Ok(v) => Ok(v),
                    Err(fallback_err) => {
                        let fallback_msg = fallback_err.to_string();
                        Err(LlmError::AllEndpointsFailed(primary_msg, fallback_msg))
                    }
                }
            }
        }
    }

    pub async fn chat(
        &self,
        role: EndpointRole,
        messages: Vec<LlmMessage>,
    ) -> Result<LlmResponse, LlmError> {
        self.check_circuit(&role).await?;
        let primary = self.select_endpoint(role);
        self.with_total_timeout(|| async {
            self.call_with_retry_and_fallback(primary, messages, Vec::new(), &role)
                .await
        })
        .await
    }

    pub async fn chat_with_tools(
        &self,
        role: EndpointRole,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.check_circuit(&role).await?;
        let primary = self.select_endpoint(role);
        self.with_total_timeout(|| async {
            self.call_with_retry_and_fallback(primary, messages, tools, &role)
                .await
        })
        .await
    }

    pub async fn chat_stream(
        &self,
        role: EndpointRole,
        messages: Vec<LlmMessage>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures_util::Stream<Item = Result<crate::types::StreamChunk, LlmError>> + Send,
            >,
        >,
        LlmError,
    > {
        self.check_circuit(&role).await?;
        let primary = self.select_endpoint(role);
        match primary.chat_stream(messages.clone()).await {
            Ok(stream) => {
                self.record_success(&role).await;
                self.fallback_active.store(false, Ordering::SeqCst);
                Ok(stream)
            }
            Err(e) => {
                self.record_failure(&role).await;
                tracing::warn!("primary chat_stream failed: {}, attempting fallback", e);
                self.fallback_active.store(true, Ordering::SeqCst);
                self.fallback.chat_stream(messages).await
            }
        }
    }

    /// Stream-chat a tool-aware request to the primary endpoint, aggregating
    /// the deltas into a final `LlmResponse`.
    pub async fn chat_stream_with_tools_aggregated(
        &self,
        role: EndpointRole,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        on_chunk: impl FnMut(&StreamChunk) + Send,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_stream_with_tools_aggregated_cancellable(
            role, messages, tools, on_chunk, CancellationToken::new(),
        )
        .await
    }

    /// Stream-chat with cancellation and fallback (§2.10, §2.11).
    /// Applies `max_total_duration_secs` as an overall deadline (§2.12).
    /// Each endpoint is tried at most once (no retry) to avoid duplicating
    /// thought/reasoning chunks in the shared `on_chunk` callback.
    pub async fn chat_stream_with_tools_aggregated_cancellable(
        &self,
        role: EndpointRole,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        mut on_chunk: impl FnMut(&StreamChunk) + Send,
        cancel: CancellationToken,
    ) -> Result<LlmResponse, LlmError> {
        self.check_circuit(&role).await?;
        tracing::info!("router streaming LLM call, role={:?} messages={} tools={}", role, messages.len(), tools.len());
        let primary = self.select_endpoint(role);

        // Primary: single attempt with cancellation
        let primary_result = Self::aggregate_stream_cancellable(
            primary.clone(), messages.clone(), tools.clone(), &mut on_chunk, cancel.clone(), &self.stream_rules,
        )
        .await;

        match primary_result {
            Ok(resp) => {
                self.record_success(&role).await;
                self.fallback_active.store(false, Ordering::SeqCst);
                Ok(resp)
            }
            Err(LlmError::StreamAborted(rule_name, inject)) => {
                tracing::warn!(
                    "stream aborted by rule '{}', injecting guidance and retrying with primary",
                    rule_name
                );
                let mut retry_msgs = messages.clone();
                retry_msgs.push(LlmMessage {
                    role: LlmRole::System,
                    content: vec![ContentPart::text(inject)],
                    tool_call_id: None,
                    tool_calls: None,
                });
                Self::aggregate_stream_cancellable(
                    primary, retry_msgs, tools, &mut on_chunk, cancel, &self.stream_rules,
                )
                .await
            }
            Err(e) => {
                if cancel.is_cancelled() {
                    return Err(LlmError::Cancelled);
                }
                if !e.is_retryable() {
                    return Err(e);
                }
                self.record_failure(&role).await;
                tracing::info!(
                    "primary stream failed: {}, waiting before fallback", e
                );

                // Delay before fallback to allow transient issues to settle
                let cfg = self.config.read().await;
                let base = cfg.retry_base_secs;
                let jitter = cfg.retry_jitter;
                drop(cfg);
                let jitter_ms = (base as f32 * jitter * 1000.0) as u64;
                tokio::time::sleep(Duration::from_secs(base) + Duration::from_millis(jitter_ms)).await;

                if cancel.is_cancelled() {
                    return Err(LlmError::Cancelled);
                }
                self.fallback_active.store(true, Ordering::SeqCst);

                // Fallback: single attempt with cancellation
                let fb_result = Self::aggregate_stream_cancellable(
                    self.fallback.clone(), messages, tools, &mut on_chunk, cancel, &self.stream_rules,
                )
                .await;

                match fb_result {
                    Ok(resp) => Ok(resp),
                    Err(fb_err) => {
                        Err(LlmError::AllEndpointsFailed(e.to_string(), fb_err.to_string()))
                    }
                }
            }
        }
    }

    async fn aggregate_stream_cancellable(
        client: Arc<dyn LlmClient>,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        on_chunk: &mut (impl FnMut(&StreamChunk) + Send),
        cancel: CancellationToken,
        stream_rules: &RwLock<Vec<StreamRule>>,
    ) -> Result<LlmResponse, LlmError> {
        let mut stream = client.chat_stream_with_tools(messages, tools).await?;
        tracing::info!("aggregate_stream_cancellable start");
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason: Option<FinishReason> = None;
        let mut usage: Option<crate::types::Usage> = None;
        let mut model: Option<String> = None;
        let mut reasoning = String::new();

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(LlmError::Cancelled);
                }
                item = stream.next() => {
                    match item {
                        Some(Ok(chunk)) => {
                            if let Some(ref delta) = chunk.text {
                                text.push_str(delta);
                            }
                            if let Some(ref r) = chunk.reasoning {
                                reasoning.push_str(r);
                            }
                            if !chunk.tool_calls.is_empty() {
                                tool_calls.extend(chunk.tool_calls.clone());
                            }
                            if chunk.finish_reason.is_some() {
                                finish_reason = chunk.finish_reason;
                            }
                            if chunk.usage.is_some() {
                                usage = chunk.usage.clone();
                            }
                            if chunk.model.is_some() {
                                model = chunk.model.clone();
                            }
                            on_chunk(&chunk);

                            // Check stream rules against accumulated output
                            if !text.is_empty() {
                                let rules = stream_rules.read().await;
                                if let Some(match_result) = check_stream_rules(&rules, &text) {
                                    drop(rules);
                                    match match_result.mode {
                                        StreamRuleMode::Warn => {
                                            tracing::warn!(
                                                "stream rule '{}' triggered (warn): matched '{}'",
                                                match_result.rule_name, match_result.matched_text
                                            );
                                        }
                                        StreamRuleMode::Abort => {
                                            tracing::warn!(
                                                "stream rule '{}' triggered (abort): matched '{}'",
                                                match_result.rule_name, match_result.matched_text
                                            );
                                            return Err(LlmError::StreamAborted(
                                                match_result.rule_name,
                                                match_result.inject,
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => return Err(e),
                        None => break,
                    }
                }
            }
        }

        Ok(LlmResponse {
            text,
            tool_calls,
            finish_reason,
            usage: usage.unwrap_or_default(),
            model,
            reasoning: if reasoning.is_empty() { None } else { Some(reasoning) },
        })
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

    pub fn fallback_active(&self) -> bool {
        self.fallback_active.load(Ordering::SeqCst)
    }

    /// §5.4: run health check on fallback endpoint
    pub async fn background_health_check(&self) -> bool {
        match self.fallback.health_check().await {
            Ok(()) => {
                self.fallback_active.store(false, Ordering::SeqCst);
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
    use crate::types::LlmError::Unknown;
    use crate::{ToolCall, Usage};
    use async_trait::async_trait;
    use futures_util::stream;
    use std::pin::Pin;

    struct MockStreamClient {
        chunks: Vec<Result<StreamChunk, LlmError>>,
        fail_chat: bool,
    }

    #[async_trait]
    impl LlmClient for MockStreamClient {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            if self.fail_chat {
                Err(Unknown("mock: chat failed".into()))
            } else {
                Ok(LlmResponse {
                    text: "mock response".into(),
                    tool_calls: Vec::new(),
                    finish_reason: Some(FinishReason::Stop),
                    usage: Usage::default(),
                    model: None,
                            reasoning: None,
        })
            }
        }
        async fn chat_with_tools(
            &self,
            _: Vec<LlmMessage>,
            _: Vec<ToolDefinition>,
        ) -> Result<LlmResponse, LlmError> {
            if self.fail_chat {
                Err(Unknown("mock: chat_with_tools failed".into()))
            } else {
                Ok(LlmResponse {
                    text: "mock response".into(),
                    tool_calls: Vec::new(),
                    finish_reason: Some(FinishReason::Stop),
                    usage: Usage::default(),
                    model: None,
                            reasoning: None,
        })
            }
        }
        async fn chat_stream(
            &self,
            _messages: Vec<LlmMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            if self.fail_chat {
                Err(Unknown("mock: chat_stream failed".into()))
            } else {
                Ok(Box::pin(stream::iter(self.chunks.clone())))
            }
        }
        async fn chat_stream_with_tools(
            &self,
            _: Vec<LlmMessage>,
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
        let cfg = LlmConfig::default();
        let router = LlmRouter::new(cfg);
        let _re = router.select_endpoint(EndpointRole::DefaultModel);
        let _fa = router.select_endpoint(EndpointRole::BalancedModel);
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
        }),
            Ok(StreamChunk {
                text: Some("world!".into()),
                tool_calls: Vec::new(),
                finish_reason: None,
                usage: None,
                model: None,
                        reasoning: None,
        }),
            Ok(StreamChunk {
                text: None,
                tool_calls: vec![ToolCall {
                    id: "tc_1".into(),
                    name: "file".into(),
                    arguments: "{\"operation\":\"read\",\"path\":\".\"}".into(),
                }],
                finish_reason: None,
                usage: None,
                model: None,
                reasoning: None,
            }),
            Ok(StreamChunk {
                text: None,
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::ToolCalls),
                usage: Some(Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                    model_name: None,
                    cost: None,
        }),
                model: None,
                reasoning: None,
            }),
        ];

        let client = Arc::new(MockStreamClient {
            chunks,
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router = LlmRouter::new_with_clients(client.clone(), client);

        let mut seen_text = String::new();
        let resp = router
            .chat_stream_with_tools_aggregated(
                EndpointRole::DefaultModel,
                Vec::new(),
                Vec::new(),
                |c| {
                    if let Some(t) = &c.text {
                        seen_text.push_str(t);
                    }
                },
            )
            .await
            .expect("aggregation succeeds");

        assert_eq!(resp.text, "Hello world!");
        assert_eq!(
            seen_text, "Hello world!",
            "on_chunk must see every text delta"
        );
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "file");
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
        assert_eq!(resp.usage.total_tokens, 15);
        assert!(!router.fallback_active(), "primary succeeded, no fallback");
    }

    #[tokio::test]
    async fn chat_fallback_on_primary_failure() {
        let failing = Arc::new(MockStreamClient {
            chunks: Vec::new(),
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let ok = Arc::new(MockStreamClient {
            chunks: vec![Ok(StreamChunk {
                text: Some("fallback response".into()),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: None,
                model: None,
                        reasoning: None,
        })],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;

        let router = LlmRouter::new_with_clients(failing, ok);

        let resp = router
            .chat(EndpointRole::DefaultModel, Vec::new())
            .await
            .expect("fallback should succeed");
        assert_eq!(resp.text, "mock response");
        assert!(router.fallback_active());
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
        })],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;

        let router = LlmRouter::new_with_clients(failing, ok);

        // First 3 calls should fail and trigger circuit breaker
        for _ in 0..3 {
            let _ = router.chat(EndpointRole::DefaultModel, Vec::new()).await;
        }

        // Circuit breaker should reject requests directly
        let result = router
            .check_circuit(&EndpointRole::DefaultModel)
            .await;
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
        cb.state = CircuitState::Open;
        cb.opened_at = Some(Instant::now());
        cb.record_success();
        assert_eq!(cb.consecutive_failures, 0);
        assert_eq!(cb.total_calls, 6);
        assert_eq!(cb.state, CircuitState::Closed);
        assert!(cb.opened_at.is_none());
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
        health.record_failure();
        assert!(!health.is_healthy);
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
    fn fallback_active_defaults_to_false() {
        let cfg = LlmConfig::default();
        let router = LlmRouter::new(cfg);
        assert!(!router.fallback_active());
    }

    #[tokio::test]
    async fn health_check_healthy_mock_endpoint() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(client.clone(), client);
        let result = router.health_check(EndpointRole::DefaultModel).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn set_stream_rules_and_check_output() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(client.clone(), client);
        let rule = StreamRule::new(
            "forbidden",
            r"secret_key",
            "do not reveal keys",
            StreamRuleMode::Abort,
        )
        .unwrap();
        router.set_stream_rules(vec![rule]).await;
        let result = router
            .check_stream_output("this is safe text")
            .await;
        assert!(result.is_none());
        let result = router
            .check_stream_output("here is secret_key=abc123")
            .await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().rule_name, "forbidden");
    }

    #[test]
    fn endpoint_role_health_index_mapping() {
        assert_eq!(
            LlmRouter::health_index(&EndpointRole::DefaultModel),
            0
        );
        assert_eq!(
            LlmRouter::health_index(&EndpointRole::BalancedModel),
            1
        );
    }

    #[tokio::test]
    async fn chat_stream_with_tools_no_chunks() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(client.clone(), client);
        let resp = router
            .chat_stream_with_tools_aggregated(
                EndpointRole::DefaultModel,
                vec![],
                vec![],
                |_| {},
            )
            .await
            .expect("aggregation succeeds");
        assert!(resp.text.is_empty());
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn chat_stream_fallback_on_primary_failure() {
        let failing = Arc::new(MockStreamClient {
            chunks: vec![],
            fail_chat: true,
        }) as Arc<dyn LlmClient>;
        let ok = Arc::new(MockStreamClient {
            chunks: vec![
                Ok(StreamChunk {
                    text: Some("fallback".into()),
                    tool_calls: vec![],
                    finish_reason: Some(FinishReason::Stop),
                    usage: None,
                    model: None,
                            reasoning: None,
        }),
            ],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(failing, ok);
        let resp = router
            .chat_stream(EndpointRole::DefaultModel, vec![])
            .await;
        assert!(resp.is_ok());
        assert!(router.fallback_active());
    }

    #[tokio::test]
    async fn chat_stream_call_succeeds_primary() {
        let client = Arc::new(MockStreamClient {
            chunks: vec![
                Ok(StreamChunk {
                    text: Some("hi".into()),
                    tool_calls: vec![],
                    finish_reason: Some(FinishReason::Stop),
                    usage: None,
                    model: None,
                            reasoning: None,
        }),
            ],
            fail_chat: false,
        }) as Arc<dyn LlmClient>;
        let router =
            LlmRouter::new_with_clients(client.clone(), client);
        let result = router
            .chat_stream(EndpointRole::DefaultModel, vec![])
            .await;
        assert!(result.is_ok());
        assert!(!router.fallback_active());
    }
}
