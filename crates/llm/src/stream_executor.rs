//! Execution boundary for an already-routed raw provider stream.
//!
//! The router resolves the model/client, acquires its permit, applies cooldown
//! and circuit policy, and owns health state. This executor performs only the
//! raw stream establishment validation, retry, total timeout, and final
//! establishment-outcome projection. Once returned, stream consumption stays
//! with the caller and is never retried here.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;

use futures_util::Stream;
use haven_common::types::CanonicalMessage;
use tokio::sync::OwnedSemaphorePermit;

use crate::client::LlmClient;
use crate::request_descriptor::RequestDescriptor;
use crate::request_pipeline::{
    RequestExecutionPolicy, RequestOutcome, execute_with_retry, execute_with_timeout,
};
use crate::types::{LlmError, StreamChunk};

pub(crate) type RawStream = Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>;

/// Keep a model's in-flight permit for the complete lifetime of a raw stream.
struct PermitStream<S> {
    inner: S,
    _permit: Option<OwnedSemaphorePermit>,
}

impl<S> Stream for PermitStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub(crate) struct StreamExecutor {
    descriptor: RequestDescriptor,
    model_id: String,
    client: Arc<dyn LlmClient>,
    policy: RequestExecutionPolicy,
}

impl StreamExecutor {
    pub(crate) fn new(
        descriptor: RequestDescriptor,
        model_id: String,
        client: Arc<dyn LlmClient>,
        policy: RequestExecutionPolicy,
    ) -> Self {
        Self {
            descriptor,
            model_id,
            client,
            policy,
        }
    }

    /// Establish one raw provider stream under the router's already-acquired
    /// permit. The outcome callback projects only the final stream-creation
    /// result back into Router-owned health and cooldown state.
    pub(crate) async fn chat_stream<F, Fut>(
        &self,
        messages: Vec<CanonicalMessage>,
        permit: OwnedSemaphorePermit,
        project_outcome: F,
    ) -> Result<RawStream, LlmError>
    where
        F: FnOnce(String, RequestOutcome) -> Fut,
        Fut: Future<Output = ()>,
    {
        tracing::trace!(
            request_purpose = self.descriptor.purpose.as_str(),
            required_capability = self.descriptor.required_capability.as_str(),
            model_id = %self.model_id,
            "establishing raw LLM stream"
        );
        let client = self.client.clone();
        let model_id = &self.model_id;
        let policy = self.policy;

        let result = execute_with_timeout(policy.total_timeout_secs, "router stream", || async {
            client.validate_content(&messages)?;
            let result =
                execute_with_retry(policy.retry, None, || client.chat_stream(messages.clone()))
                    .await;
            project_outcome(model_id.to_string(), RequestOutcome::from_result(&result)).await;
            result
        })
        .await;

        match result {
            Ok(stream) => {
                let stream: RawStream = Box::pin(PermitStream {
                    inner: stream,
                    _permit: Some(permit),
                });
                Ok(stream)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use futures_util::{StreamExt, stream};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::request_descriptor::RequestDescriptor;
    use crate::request_pipeline::RetryPolicy;
    use crate::types::LlmResponse;

    fn chat_descriptor() -> RequestDescriptor {
        RequestDescriptor::from(haven_common::config::RequestKind::Chat)
    }

    #[derive(Clone, Copy)]
    enum Behavior {
        RetryThenSuccess,
        Error,
        Pending,
        Success,
    }

    struct StreamProbe {
        behavior: Behavior,
        attempts: AtomicUsize,
        messages: Mutex<Vec<serde_json::Value>>,
    }

    impl StreamProbe {
        fn new(behavior: Behavior) -> Self {
            Self {
                behavior,
                attempts: AtomicUsize::new(0),
                messages: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for StreamProbe {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse::default())
        }

        async fn chat_stream(
            &self,
            messages: Vec<CanonicalMessage>,
        ) -> Result<RawStream, LlmError> {
            self.messages
                .lock()
                .unwrap()
                .push(serde_json::to_value(messages).unwrap());
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                Behavior::RetryThenSuccess if attempt == 0 => {
                    Err(LlmError::ServerError("first stream setup failed".into()))
                }
                Behavior::Error => Err(LlmError::Unknown("stream setup failed".into())),
                Behavior::Pending => std::future::pending().await,
                Behavior::RetryThenSuccess | Behavior::Success => {
                    Ok(Box::pin(stream::iter([Ok(StreamChunk {
                        tool_call_updates: Vec::new(),
                        text: Some("ready".into()),
                        ..StreamChunk::default()
                    })])))
                }
            }
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn policy(max_retries: u32, total_timeout_secs: u64) -> RequestExecutionPolicy {
        RequestExecutionPolicy {
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

    fn permit_source() -> Arc<tokio::sync::Semaphore> {
        Arc::new(tokio::sync::Semaphore::new(1))
    }

    #[tokio::test]
    async fn retries_only_stream_establishment_and_projects_final_success_once() {
        let client = Arc::new(StreamProbe::new(Behavior::RetryThenSuccess));
        let executor = StreamExecutor::new(
            chat_descriptor(),
            "stream-model".into(),
            client.clone(),
            policy(1, 5),
        );
        let semaphore = permit_source();
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = outcomes.clone();

        let mut stream = executor
            .chat_stream(
                vec![CanonicalMessage::user(vec![
                    haven_common::types::ContentPart::text("retry snapshot"),
                ])],
                semaphore.clone().acquire_owned().await.unwrap(),
                move |model_id, outcome| async move {
                    recorded.lock().unwrap().push((model_id, outcome));
                },
            )
            .await
            .expect("second stream establishment succeeds");

        assert_eq!(client.attempts.load(Ordering::SeqCst), 2);
        let messages = client.messages.lock().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0], messages[1],
            "each attempt gets the same message clone"
        );
        drop(messages);
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![("stream-model".into(), RequestOutcome::Success)]
        );

        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.text.as_deref(), Some("ready"));
    }

    #[tokio::test]
    async fn provider_stream_setup_error_is_returned_and_projected_once() {
        let executor = StreamExecutor::new(
            chat_descriptor(),
            "stream-model".into(),
            Arc::new(StreamProbe::new(Behavior::Error)),
            policy(0, 5),
        );
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = outcomes.clone();
        let result = executor
            .chat_stream(
                Vec::new(),
                permit_source().acquire_owned().await.unwrap(),
                move |model_id, outcome| async move {
                    recorded.lock().unwrap().push((model_id, outcome));
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("provider setup error is returned"),
            Err(error) => error,
        };

        assert!(matches!(error, LlmError::Unknown(message) if message == "stream setup failed"));
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![(
                "stream-model".into(),
                RequestOutcome::Failure {
                    rate_limit_after: None
                }
            )]
        );
    }

    #[tokio::test]
    async fn router_stream_timeout_text_is_preserved_without_provider_outcome_projection() {
        let semaphore = permit_source();
        let executor = StreamExecutor::new(
            chat_descriptor(),
            "stream-model".into(),
            Arc::new(StreamProbe::new(Behavior::Pending)),
            policy(0, 1),
        );
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recorded = outcomes.clone();
        let result = executor
            .chat_stream(
                Vec::new(),
                semaphore.clone().acquire_owned().await.unwrap(),
                move |model_id, outcome| async move {
                    recorded.lock().unwrap().push((model_id, outcome));
                },
            )
            .await;
        let error = match result {
            Ok(_) => panic!("pending stream setup reaches the total deadline"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            LlmError::Timeout(message)
                if message == "router stream total timeout after 1s"
        ));
        assert!(outcomes.lock().unwrap().is_empty());
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn stream_holds_permit_until_the_returned_stream_is_dropped() {
        let semaphore = permit_source();
        let executor = StreamExecutor::new(
            chat_descriptor(),
            "stream-model".into(),
            Arc::new(StreamProbe::new(Behavior::Success)),
            policy(0, 5),
        );
        let mut stream = executor
            .chat_stream(
                Vec::new(),
                semaphore.clone().acquire_owned().await.unwrap(),
                |_, _| async {},
            )
            .await
            .unwrap();

        assert_eq!(semaphore.available_permits(), 0);
        assert!(stream.next().await.unwrap().is_ok());
        assert!(stream.next().await.is_none());
        assert_eq!(semaphore.available_permits(), 0);

        assert!(
            tokio::time::timeout(Duration::from_millis(20), semaphore.clone().acquire_owned())
                .await
                .is_err()
        );
        drop(stream);
        let _permit = tokio::time::timeout(Duration::from_millis(100), semaphore.acquire_owned())
            .await
            .expect("dropping the raw stream releases its permit")
            .unwrap();
    }

    #[tokio::test]
    async fn failed_stream_establishment_releases_the_permit() {
        let semaphore = permit_source();
        let executor = StreamExecutor::new(
            chat_descriptor(),
            "stream-model".into(),
            Arc::new(StreamProbe::new(Behavior::Error)),
            policy(0, 5),
        );
        let result = executor
            .chat_stream(
                Vec::new(),
                semaphore.clone().acquire_owned().await.unwrap(),
                |_, _| async {},
            )
            .await;

        assert!(result.is_err());
        let _permit = tokio::time::timeout(Duration::from_millis(100), semaphore.acquire_owned())
            .await
            .expect("a failed setup releases its permit")
            .unwrap();
    }

    #[test]
    fn executor_keeps_the_explicit_routed_descriptor() {
        let descriptor = chat_descriptor();
        let executor = StreamExecutor::new(
            descriptor,
            "stream-model".into(),
            Arc::new(StreamProbe::new(Behavior::Success)),
            policy(0, 5),
        );

        assert_eq!(executor.descriptor, descriptor);
    }
}
