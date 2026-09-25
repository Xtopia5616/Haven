//! Execution boundary for an already-routed aggregated provider stream.
//!
//! The router selects the client, snapshots request policy, holds the model
//! permit, and owns stream rules and health state. This executor coordinates
//! the logical stream attempts while `streaming` drains and aggregates one
//! provider stream at a time.

use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use haven_common::types::CanonicalMessage;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, retry_delay};
use crate::request_pipeline::{RequestPolicy, RetryPolicy, execute_with_timeout};
use crate::stream_rules::StreamRule;
use crate::streaming;
use crate::types::{LlmError, LlmResponse, StreamChunk, StreamRequest, ToolDefinition};

pub(crate) type ChunkCallback = Box<dyn FnMut(&StreamChunk) + Send + 'static>;
pub(crate) type AttemptCallback = Box<dyn FnMut(bool) + Send + 'static>;

/// Shared immutable request data reused across every provider attempt.
#[derive(Clone)]
pub(crate) struct StreamContext {
    pub(crate) messages: Arc<[CanonicalMessage]>,
    pub(crate) tools: Arc<[ToolDefinition]>,
    pub(crate) max_output_tokens: Option<u32>,
}

impl StreamContext {
    pub(crate) fn from_request(request: StreamRequest<'_>) -> Self {
        Self {
            messages: Arc::from(request.messages),
            tools: Arc::from(request.tools),
            max_output_tokens: request.max_output_tokens,
        }
    }
}

/// Active callbacks shared by a logical stream and all its attempts.
pub(crate) struct ActiveStreamHooks {
    on_chunk: Arc<StdMutex<ChunkCallback>>,
    on_attempt_start: Arc<StdMutex<AttemptCallback>>,
}

impl ActiveStreamHooks {
    pub(crate) fn new(
        on_chunk: ChunkCallback,
        on_attempt_start: AttemptCallback,
        replace_output_on_start: bool,
    ) -> Self {
        let hooks = Self {
            on_chunk: Arc::new(StdMutex::new(on_chunk)),
            on_attempt_start: Arc::new(StdMutex::new(on_attempt_start)),
        };
        hooks.notify_attempt_start(replace_output_on_start);
        hooks
    }

    fn notify_attempt_start(&self, replace_output: bool) {
        self.on_attempt_start.lock().unwrap()(replace_output);
    }
}

/// Provider-neutral execution context for one selected model/client.
pub(crate) struct AggregatedStreamExecutor<'a> {
    client: Arc<dyn LlmClient>,
    policy: RequestPolicy,
    stream_rules: &'a RwLock<Vec<StreamRule>>,
    idle_timeout: Duration,
}

impl<'a> AggregatedStreamExecutor<'a> {
    pub(crate) fn new(
        client: Arc<dyn LlmClient>,
        policy: RequestPolicy,
        stream_rules: &'a RwLock<Vec<StreamRule>>,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            client,
            policy,
            stream_rules,
            idle_timeout,
        }
    }

    /// Run the aggregate stream state machine and pass its final result back
    /// to the router-owned outcome projector. Validation stays before the
    /// total timeout and is not projected as a provider outcome.
    pub(crate) async fn execute<F, Fut, I, IFut>(
        &self,
        context: StreamContext,
        hooks: ActiveStreamHooks,
        cancel: CancellationToken,
        refresh_guidance_idle_timeout: I,
        project_outcome: F,
    ) -> Result<LlmResponse, LlmError>
    where
        F: FnOnce(Result<LlmResponse, LlmError>) -> Fut,
        Fut: Future<Output = Result<LlmResponse, LlmError>>,
        I: FnOnce() -> IFut,
        IFut: Future<Output = Duration>,
    {
        self.client.validate_content(&context.messages)?;

        let client = self.client.clone();
        let policy = self.policy;
        let idle_timeout = self.idle_timeout;
        let stream_rules = self.stream_rules;

        execute_with_timeout(
            policy.total_timeout_secs,
            "router streaming",
            || async move {
                let attempt_result = aggregate_stream_with_retry_before_output(
                    client.clone(),
                    context.clone(),
                    hooks.on_chunk.clone(),
                    cancel.clone(),
                    stream_rules,
                    idle_timeout,
                    policy.retry,
                )
                .await;

                let result = match attempt_result {
                    Err(error @ LlmError::StreamAborted(_, _)) => {
                        Self::retry_stream_with_guidance(
                            client,
                            &hooks,
                            context,
                            cancel,
                            stream_rules,
                            error,
                            refresh_guidance_idle_timeout,
                        )
                        .await
                    }
                    result => result,
                };

                if matches!(result, Err(LlmError::Cancelled)) {
                    Err(LlmError::Cancelled)
                } else {
                    project_outcome(result).await
                }
            },
        )
        .await
    }

    async fn retry_stream_with_guidance<I, IFut>(
        client: Arc<dyn LlmClient>,
        hooks: &ActiveStreamHooks,
        context: StreamContext,
        cancel: CancellationToken,
        stream_rules: &RwLock<Vec<StreamRule>>,
        error: LlmError,
        refresh_idle_timeout: I,
    ) -> Result<LlmResponse, LlmError>
    where
        I: FnOnce() -> IFut,
        IFut: Future<Output = Duration>,
    {
        let LlmError::StreamAborted(rule_name, inject) = error else {
            return Err(error);
        };
        tracing::warn!(
            "stream aborted by rule '{}', injecting guidance and retrying with primary",
            rule_name
        );
        hooks.notify_attempt_start(true);
        // Keep this fresh read at the retry boundary: an in-flight guidance
        // retry historically observes the current idle setting, while the
        // initial attempts keep the request's original snapshot.
        let idle_timeout = refresh_idle_timeout().await;
        // Guidance is appended after the assistant's partial turn. A trailing
        // System message breaks OpenAI-compatible providers and is merged into
        // the top-level system field by Anthropic/Gemini, losing its position.
        // A User message is legal anywhere.
        streaming::aggregate_stream_cancellable_shared_with_guidance(
            client,
            context.messages,
            context.tools,
            Some(inject),
            hooks.on_chunk.clone(),
            cancel,
            stream_rules,
            idle_timeout,
            context.max_output_tokens,
        )
        .await
    }
}

pub(crate) async fn aggregate_stream_with_retry_before_output(
    client: Arc<dyn LlmClient>,
    context: StreamContext,
    on_chunk: Arc<StdMutex<impl FnMut(&StreamChunk) + Send + 'static>>,
    cancel: CancellationToken,
    stream_rules: &RwLock<Vec<StreamRule>>,
    idle_timeout: Duration,
    retry: RetryPolicy,
) -> Result<LlmResponse, LlmError> {
    // Materialize the retry-invariant request once. Each attempt below only
    // clones these Arcs; the canonical message/tool graph is not deep-cloned
    // by the retry loop.
    let messages = context.messages;
    let tools = context.tools;
    for attempt in 0..=retry.max_retries {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let emitted = Arc::new(AtomicBool::new(false));
        let callback = {
            let on_chunk = on_chunk.clone();
            let emitted = emitted.clone();
            Arc::new(StdMutex::new(move |chunk: &StreamChunk| {
                emitted.store(true, Ordering::SeqCst);
                let mut callback = on_chunk.lock().unwrap();
                callback(chunk);
            }))
        };
        let result = streaming::aggregate_stream_cancellable_shared(
            client.clone(),
            messages.clone(),
            tools.clone(),
            callback,
            cancel.clone(),
            stream_rules,
            idle_timeout,
            context.max_output_tokens,
        )
        .await;
        let Err(err) = result else {
            return result;
        };
        if !err.is_retryable() || emitted.load(Ordering::SeqCst) || attempt == retry.max_retries {
            return Err(err);
        }
        let delay = retry_delay(
            retry.base_secs,
            retry.factor,
            retry.max_secs,
            retry.jitter,
            attempt,
            err.retry_after(),
        );
        tracing::debug!(
            "stream attempt {}/{} failed before output, retrying after {:?}: {}",
            attempt + 1,
            retry.max_retries + 1,
            delay,
            err
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        }
    }
    Err(LlmError::Unknown("stream retry loop exhausted".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use futures_util::{Stream, stream};
    use std::pin::Pin;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::Notify;

    use crate::request_pipeline::RetryPolicy;
    use crate::stream_rules::StreamRuleMode;
    use crate::types::{Embedding, SttResult, Usage};

    type MockStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>;

    #[derive(Clone, Copy)]
    enum ProbeMode {
        RetryThenSuccess,
        RuleAbortThenGuidance,
        Pending,
    }

    struct ExecutorProbe {
        mode: ProbeMode,
        attempts: AtomicUsize,
        started: Arc<Notify>,
        seen: StdMutex<Vec<(usize, usize, Option<u32>)>>,
        guidance_seen: StdMutex<Vec<(usize, usize, String, Option<u32>)>>,
    }

    impl ExecutorProbe {
        fn new(mode: ProbeMode) -> Self {
            Self {
                mode,
                attempts: AtomicUsize::new(0),
                started: Arc::new(Notify::new()),
                seen: StdMutex::new(Vec::new()),
                guidance_seen: StdMutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for ExecutorProbe {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown(
                "aggregate executor probe does not chat".into(),
            ))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<MockStream, LlmError> {
            Ok(Box::pin(stream::empty()))
        }

        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            messages: Arc<[CanonicalMessage]>,
            tools: Arc<[ToolDefinition]>,
            max_output_tokens: Option<u32>,
        ) -> Result<MockStream, LlmError> {
            self.seen
                .lock()
                .unwrap()
                .push((messages.len(), tools.len(), max_output_tokens));
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            match self.mode {
                ProbeMode::RetryThenSuccess if attempt == 0 => {
                    Err(LlmError::ServerError("retry before output".into()))
                }
                ProbeMode::Pending => {
                    self.started.notify_one();
                    std::future::pending().await
                }
                ProbeMode::RuleAbortThenGuidance => {
                    Ok(Box::pin(stream::iter(vec![Ok(StreamChunk {
                        text: Some("forbidden".into()),
                        ..StreamChunk::default()
                    })])))
                }
                ProbeMode::RetryThenSuccess => Ok(Box::pin(stream::iter(vec![Ok(StreamChunk {
                    text: Some("completed".into()),
                    usage: Some(Usage {
                        prompt_tokens: 9,
                        completion_tokens: 4,
                        total_tokens: 13,
                        ..Usage::default()
                    }),
                    ..StreamChunk::default()
                })]))),
            }
        }

        async fn chat_stream_with_tools_output_cap_shared_guidance(
            &self,
            messages: Arc<[CanonicalMessage]>,
            tools: Arc<[ToolDefinition]>,
            guidance: String,
            max_output_tokens: Option<u32>,
        ) -> Result<MockStream, LlmError> {
            self.guidance_seen.lock().unwrap().push((
                messages.as_ptr() as usize,
                tools.as_ptr() as usize,
                guidance,
                max_output_tokens,
            ));
            Ok(Box::pin(stream::iter(vec![Ok(StreamChunk {
                text: Some("safe answer".into()),
                ..StreamChunk::default()
            })])))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }

        async fn embed(&self, _input: Vec<String>) -> Result<Embedding, LlmError> {
            Err(LlmError::UnsupportedCapability("probe".into()))
        }

        async fn transcribe(&self, _wav_data: &[u8]) -> Result<SttResult, LlmError> {
            Err(LlmError::UnsupportedCapability("probe".into()))
        }
    }

    fn policy(total_timeout_secs: u64, max_retries: u32) -> RequestPolicy {
        RequestPolicy {
            retry: RetryPolicy {
                max_retries,
                base_secs: 0,
                factor: 1,
                max_secs: 0,
                jitter: 0.0,
            },
            total_timeout_secs,
        }
    }

    fn context() -> StreamContext {
        StreamContext {
            messages: Arc::from(vec![CanonicalMessage::user_text("prompt")]),
            tools: Arc::from(vec![ToolDefinition {
                tool_type: "function".into(),
                function: crate::types::ToolFunction {
                    name: "probe".into(),
                    description: "probe tool".into(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            }]),
            max_output_tokens: Some(64),
        }
    }

    fn hooks() -> ActiveStreamHooks {
        ActiveStreamHooks::new(Box::new(|_| {}), Box::new(|_| {}), false)
    }

    #[tokio::test]
    async fn retries_before_output_and_projects_final_usage_once() {
        let probe = Arc::new(ExecutorProbe::new(ProbeMode::RetryThenSuccess));
        let rules = RwLock::new(Vec::new());
        let executor = AggregatedStreamExecutor::new(
            probe.clone(),
            policy(10, 1),
            &rules,
            Duration::from_secs(1),
        );
        let projection_calls = Arc::new(AtomicUsize::new(0));
        let result = executor
            .execute(
                context(),
                hooks(),
                CancellationToken::new(),
                || async { Duration::from_secs(1) },
                {
                    let projection_calls = projection_calls.clone();
                    move |result| async move {
                        projection_calls.fetch_add(1, Ordering::SeqCst);
                        result
                    }
                },
            )
            .await
            .expect("the retry should produce an aggregated response");

        assert_eq!(probe.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(*probe.seen.lock().unwrap(), vec![(1, 1, Some(64)); 2]);
        assert_eq!(result.text, "completed");
        assert_eq!(result.usage.total_tokens, 13);
        assert_eq!(projection_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_rule_guidance_retry_replaces_output_and_reuses_request_snapshot() {
        let probe = Arc::new(ExecutorProbe::new(ProbeMode::RuleAbortThenGuidance));
        let rules = RwLock::new(vec![
            crate::stream_rules::StreamRule::new(
                "forbidden-output",
                "forbidden",
                "continue without forbidden output",
                StreamRuleMode::Abort,
            )
            .unwrap(),
        ]);
        let executor = AggregatedStreamExecutor::new(
            probe.clone(),
            policy(10, 0),
            &rules,
            Duration::from_secs(1),
        );
        let context = context();
        let message_ptr = context.messages.as_ptr() as usize;
        let tool_ptr = context.tools.as_ptr() as usize;
        let attempt_starts = Arc::new(StdMutex::new(Vec::new()));
        let delivered_chunks = Arc::new(StdMutex::new(Vec::new()));
        let projection_calls = Arc::new(AtomicUsize::new(0));
        let idle_refreshes = Arc::new(AtomicUsize::new(0));
        let result = executor
            .execute(
                context,
                ActiveStreamHooks::new(
                    {
                        let delivered_chunks = delivered_chunks.clone();
                        Box::new(move |chunk| {
                            delivered_chunks.lock().unwrap().push(chunk.text.clone());
                        })
                    },
                    {
                        let attempt_starts = attempt_starts.clone();
                        Box::new(move |replace| attempt_starts.lock().unwrap().push(replace))
                    },
                    false,
                ),
                CancellationToken::new(),
                {
                    let idle_refreshes = idle_refreshes.clone();
                    move || async move {
                        idle_refreshes.fetch_add(1, Ordering::SeqCst);
                        Duration::from_secs(2)
                    }
                },
                {
                    let projection_calls = projection_calls.clone();
                    move |result| async move {
                        projection_calls.fetch_add(1, Ordering::SeqCst);
                        result
                    }
                },
            )
            .await
            .expect("guidance retry should return the replacement response");

        assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(
            *probe.guidance_seen.lock().unwrap(),
            vec![(
                message_ptr,
                tool_ptr,
                "continue without forbidden output".into(),
                Some(64),
            )]
        );
        assert_eq!(result.text, "safe answer");
        assert_eq!(
            *delivered_chunks.lock().unwrap(),
            vec![Some("safe answer".into())]
        );
        assert_eq!(*attempt_starts.lock().unwrap(), vec![false, true]);
        assert_eq!(idle_refreshes.load(Ordering::SeqCst), 1);
        assert_eq!(projection_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cancellation_skips_outcome_projection() {
        let probe = Arc::new(ExecutorProbe::new(ProbeMode::Pending));
        let rules = Arc::new(RwLock::new(Vec::new()));
        let projection_calls = Arc::new(AtomicUsize::new(0));
        let cancel = CancellationToken::new();
        let task = tokio::spawn({
            let probe = probe.clone();
            let rules = rules.clone();
            let cancel = cancel.clone();
            let projection_calls = projection_calls.clone();
            async move {
                let executor = AggregatedStreamExecutor::new(
                    probe,
                    policy(10, 0),
                    &rules,
                    Duration::from_secs(1),
                );
                executor
                    .execute(
                        context(),
                        hooks(),
                        cancel,
                        || async { Duration::from_secs(1) },
                        move |result| async move {
                            projection_calls.fetch_add(1, Ordering::SeqCst);
                            result
                        },
                    )
                    .await
            }
        });

        probe.started.notified().await;
        cancel.cancel();
        assert!(matches!(task.await.unwrap(), Err(LlmError::Cancelled)));
        assert_eq!(projection_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn total_timeout_text_and_projection_boundary_are_preserved() {
        let probe = Arc::new(ExecutorProbe::new(ProbeMode::Pending));
        let rules = RwLock::new(Vec::new());
        let projection_calls = Arc::new(AtomicUsize::new(0));
        let executor = AggregatedStreamExecutor::new(
            probe.clone(),
            policy(1, 0),
            &rules,
            Duration::from_secs(1),
        );
        let result = executor
            .execute(
                context(),
                hooks(),
                CancellationToken::new(),
                || async { Duration::from_secs(1) },
                {
                    let projection_calls = projection_calls.clone();
                    move |result| async move {
                        projection_calls.fetch_add(1, Ordering::SeqCst);
                        result
                    }
                },
            )
            .await;

        assert!(matches!(
            result,
            Err(LlmError::Timeout(message))
                if message == "router streaming total timeout after 1s"
        ));
        assert_eq!(probe.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(projection_calls.load(Ordering::SeqCst), 0);
    }
}
