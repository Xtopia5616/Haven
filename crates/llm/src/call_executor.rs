//! Execution boundary for already-routed, one-shot provider calls.
//!
//! The router resolves the model and client and owns all mutable routing
//! state. This executor receives that identity plus one policy snapshot, then
//! performs the provider-neutral validation, retry, timeout, and outcome
//! projection for complete and embedding calls. Streaming stays in the router
//! because its permit and cancellation lifetime extends through consumption.

use std::future::Future;
use std::sync::Arc;

use haven_common::types::CanonicalMessage;

use crate::client::LlmClient;
use crate::request_descriptor::RequestDescriptor;
use crate::request_pipeline::{
    RequestOutcome, RequestPolicy, execute_with_retry, execute_with_timeout,
};
use crate::types::{Embedding, LlmError, LlmResponse, ToolDefinition};

pub(crate) struct CallExecutor {
    descriptor: RequestDescriptor,
    model_id: String,
    client: Arc<dyn LlmClient>,
    policy: RequestPolicy,
}

impl CallExecutor {
    pub(crate) fn new(
        descriptor: RequestDescriptor,
        model_id: String,
        client: Arc<dyn LlmClient>,
        policy: RequestPolicy,
    ) -> Self {
        Self {
            descriptor,
            model_id,
            client,
            policy,
        }
    }

    /// Execute an ordinary or tool-enabled complete call. The projection port
    /// is passed by the router so this module never owns health or cooldown
    /// state and cannot widen the crate's public API.
    pub(crate) async fn complete<F, Fut>(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
        project_outcome: F,
    ) -> Result<LlmResponse, LlmError>
    where
        F: FnOnce(String, RequestOutcome) -> Fut,
        Fut: Future<Output = ()>,
    {
        tracing::trace!(
            request_purpose = self.descriptor.purpose.as_str(),
            required_capability = self.descriptor.required_capability.as_str(),
            model_id = %self.model_id,
            "executing complete LLM request"
        );
        let client = self.client.clone();
        let model_id = &self.model_id;
        let policy = self.policy;

        execute_with_timeout(policy.total_timeout_secs, "router", || async {
            // Keep validation inside the existing total deadline, and preserve
            // its early-return behavior: validation errors are not provider
            // outcomes and therefore are not projected onto model health.
            client.validate_content(&messages)?;

            let result = if tools.is_empty() {
                execute_with_retry(policy.retry, None, || async {
                    client
                        .chat_with_output_cap(messages.clone(), max_output_tokens)
                        .await
                })
                .await
            } else {
                execute_with_retry(policy.retry, None, || async {
                    client
                        .chat_with_tools_output_cap(
                            messages.clone(),
                            tools.clone(),
                            max_output_tokens,
                        )
                        .await
                })
                .await
            };

            project_outcome(model_id.to_string(), RequestOutcome::from_result(&result)).await;
            result
        })
        .await
    }

    /// Execute one non-empty embedding batch through the same captured retry
    /// and total-timeout policy as complete calls. Empty batches are handled
    /// by the router before route resolution, preserving their fast path.
    pub(crate) async fn embed<F, Fut>(
        &self,
        input: Vec<String>,
        project_outcome: F,
    ) -> Result<Embedding, LlmError>
    where
        F: FnOnce(String, RequestOutcome) -> Fut,
        Fut: Future<Output = ()>,
    {
        tracing::trace!(
            request_purpose = self.descriptor.purpose.as_str(),
            required_capability = self.descriptor.required_capability.as_str(),
            model_id = %self.model_id,
            "executing embedding LLM request"
        );
        let client = self.client.clone();
        let model_id = &self.model_id;
        let policy = self.policy;

        execute_with_timeout(policy.total_timeout_secs, "embedding", || async {
            let result =
                execute_with_retry(policy.retry, None, || client.embed(input.clone())).await;
            project_outcome(model_id.to_string(), RequestOutcome::from_result(&result)).await;
            result
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use futures_util::{Stream, stream};
    use haven_common::types::ContentPart;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::request_descriptor::RequestDescriptor;
    use crate::request_pipeline::RetryPolicy;
    use crate::types::{StreamChunk, Usage};

    #[derive(Debug, PartialEq)]
    enum ObservedCall {
        Plain {
            message_count: usize,
            max_output_tokens: Option<u32>,
        },
        Tools {
            message_count: usize,
            tool_count: usize,
            max_output_tokens: Option<u32>,
        },
        Embedding {
            input_count: usize,
        },
    }

    struct ExecutorProbe {
        calls: Mutex<Vec<ObservedCall>>,
        complete_attempts: AtomicUsize,
        fail_first_complete: bool,
        pending_complete: bool,
        rate_limit_embedding: bool,
    }

    impl ExecutorProbe {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                complete_attempts: AtomicUsize::new(0),
                fail_first_complete: false,
                pending_complete: false,
                rate_limit_embedding: false,
            }
        }
    }

    #[async_trait]
    impl LlmClient for ExecutorProbe {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse::default())
        }

        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.calls.lock().unwrap().push(ObservedCall::Plain {
                message_count: messages.len(),
                max_output_tokens,
            });
            let attempt = self.complete_attempts.fetch_add(1, Ordering::SeqCst);
            if self.pending_complete {
                return std::future::pending().await;
            }
            if self.fail_first_complete && attempt == 0 {
                return Err(LlmError::Timeout("provider timeout".into()));
            }
            Ok(LlmResponse {
                text: "complete".into(),
                ..LlmResponse::default()
            })
        }

        async fn chat_with_tools_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            tools: Vec<ToolDefinition>,
            max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.calls.lock().unwrap().push(ObservedCall::Tools {
                message_count: messages.len(),
                tool_count: tools.len(),
                max_output_tokens,
            });
            Ok(LlmResponse {
                text: "tools".into(),
                ..LlmResponse::default()
            })
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError>
        {
            Ok(Box::pin(stream::empty()))
        }

        async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
            self.calls.lock().unwrap().push(ObservedCall::Embedding {
                input_count: input.len(),
            });
            if self.rate_limit_embedding {
                return Err(LlmError::RateLimit {
                    retry_after: Some(Duration::from_secs(5)),
                });
            }
            Ok(Embedding {
                vectors: vec![vec![0.5]],
                model: Some("provider-model".into()),
                usage: Usage::default(),
            })
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn policy(max_retries: u32, total_timeout_secs: u64) -> RequestPolicy {
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

    fn executor(
        client: Arc<ExecutorProbe>,
        policy: RequestPolicy,
        request: haven_common::config::RequestKind,
    ) -> CallExecutor {
        CallExecutor::new(
            RequestDescriptor::from(request),
            "resolved-model-id".into(),
            client,
            policy,
        )
    }

    fn messages() -> Vec<CanonicalMessage> {
        vec![CanonicalMessage::user(vec![ContentPart::text("hello")])]
    }

    fn tool() -> ToolDefinition {
        ToolDefinition {
            tool_type: "function".into(),
            function: crate::types::ToolFunction {
                name: "lookup".into(),
                description: "Look up a value".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }
    }

    #[tokio::test]
    async fn complete_dispatches_plain_and_tool_calls_with_their_request_data() {
        let client = Arc::new(ExecutorProbe::new());
        let executor = executor(
            client.clone(),
            policy(0, 2),
            haven_common::config::RequestKind::Chat,
        );

        executor
            .complete(messages(), Vec::new(), Some(64), |_, _| async {})
            .await
            .unwrap();
        executor
            .complete(messages(), vec![tool()], Some(32), |_, _| async {})
            .await
            .unwrap();

        assert_eq!(
            *client.calls.lock().unwrap(),
            vec![
                ObservedCall::Plain {
                    message_count: 1,
                    max_output_tokens: Some(64),
                },
                ObservedCall::Tools {
                    message_count: 1,
                    tool_count: 1,
                    max_output_tokens: Some(32),
                },
            ]
        );
    }

    #[tokio::test]
    async fn complete_retries_provider_timeout_and_projects_final_result_once() {
        let client = Arc::new(ExecutorProbe {
            fail_first_complete: true,
            ..ExecutorProbe::new()
        });
        let executor = executor(
            client.clone(),
            policy(1, 2),
            haven_common::config::RequestKind::Chat,
        );
        let projections = Arc::new(AtomicUsize::new(0));

        executor
            .complete(messages(), Vec::new(), None, {
                let projections = projections.clone();
                move |_, _| async move {
                    projections.fetch_add(1, Ordering::SeqCst);
                }
            })
            .await
            .unwrap();

        assert_eq!(client.complete_attempts.load(Ordering::SeqCst), 2);
        assert_eq!(projections.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn complete_total_timeout_keeps_router_error_text_and_skips_outcome_projection() {
        let client = Arc::new(ExecutorProbe {
            pending_complete: true,
            ..ExecutorProbe::new()
        });
        let executor = executor(
            client,
            policy(0, 0),
            haven_common::config::RequestKind::Chat,
        );
        let projections = Arc::new(AtomicUsize::new(0));

        let error = executor
            .complete(messages(), Vec::new(), None, {
                let projections = projections.clone();
                move |_, _| async move {
                    projections.fetch_add(1, Ordering::SeqCst);
                }
            })
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            LlmError::Timeout(message) if message == "router total timeout after 0s"
        ));
        assert_eq!(projections.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn embedding_rate_limit_failure_is_projected_once() {
        let client = Arc::new(ExecutorProbe {
            rate_limit_embedding: true,
            ..ExecutorProbe::new()
        });
        let executor = executor(
            client.clone(),
            policy(0, 2),
            haven_common::config::RequestKind::Embedding,
        );
        let projections = Arc::new(AtomicUsize::new(0));

        let error = executor
            .embed(vec!["hello".into()], {
                let projections = projections.clone();
                move |model_id, outcome| {
                    assert_eq!(model_id, "resolved-model-id");
                    assert_eq!(
                        outcome,
                        RequestOutcome::Failure {
                            rate_limit_after: Some(Duration::from_secs(5)),
                        }
                    );
                    async move {
                        projections.fetch_add(1, Ordering::SeqCst);
                    }
                }
            })
            .await
            .unwrap_err();

        assert!(matches!(error, LlmError::RateLimit { .. }));
        assert_eq!(projections.load(Ordering::SeqCst), 1);
        assert_eq!(
            *client.calls.lock().unwrap(),
            vec![ObservedCall::Embedding { input_count: 1 }]
        );
    }

    #[test]
    fn executor_keeps_the_explicit_routed_descriptor() {
        let descriptor = RequestDescriptor::from(haven_common::config::RequestKind::AudioChat);
        let executor = CallExecutor::new(
            descriptor,
            "resolved-model-id".into(),
            Arc::new(ExecutorProbe::new()),
            policy(0, 2),
        );

        assert_eq!(executor.descriptor, descriptor);
    }
}
