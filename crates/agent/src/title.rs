use haven_common::prompts::TITLE_SYSTEM_PROMPT;
use haven_llm::{EndpointRole, LlmRouter};
use std::sync::Arc;

/// Generates concise conversation titles using the small_model endpoint.
#[derive(Clone)]
pub struct TitleGenerator {
    router: Arc<LlmRouter>,
}

impl TitleGenerator {
    pub fn new(router: Arc<LlmRouter>) -> Self {
        Self { router }
    }

    /// Generate a short title from conversation messages.
    /// Returns `None` if the LLM call fails or returns empty text.
    pub async fn generate(&self, conversation: &[String]) -> Option<String> {
        if conversation.is_empty() {
            return None;
        }
        // No-op without an outbound call when the small_model endpoint is not
        // configured (mirrors the guard in the file tool's `summarize`).
        if !self
            .router
            .is_role_configured(EndpointRole::SmallModel)
            .await
        {
            return None;
        }
        let conv_text = conversation.join("\n");

        match self
            .router
            .chat_with_prompt(EndpointRole::SmallModel, TITLE_SYSTEM_PROMPT, &conv_text)
            .await
        {
            Ok(response) => {
                let title = response.text.trim().trim_matches('"').trim().to_string();
                if title.is_empty() || title.len() > 100 {
                    None
                } else {
                    Some(title)
                }
            }
            Err(e) => {
                tracing::warn!("title generation failed: {}", e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::Stream;
    use haven_llm::OpenAiAdapter;
    use haven_llm::client::LlmClient;
    use haven_llm::types::{LlmError, LlmMessage, LlmResponse, LlmRole};
    use std::pin::Pin;
    use std::sync::Mutex;

    struct RecordingMock {
        result: Mutex<Result<LlmResponse, LlmError>>,
        calls: Mutex<Vec<Vec<LlmMessage>>>,
    }

    #[async_trait]
    impl LlmClient for RecordingMock {
        async fn chat(&self, messages: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            self.calls.lock().unwrap().push(messages);
            self.result.lock().unwrap().clone()
        }

        async fn chat_stream(
            &self,
            _messages: Vec<LlmMessage>,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<haven_llm::types::StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    /// Builds a router with all four slots, where the small_model slot is the
    /// recording mock so we can inspect the messages it receives.
    struct TestRouter {
        router: Arc<LlmRouter>,
        mock: Arc<RecordingMock>,
    }

    async fn test_router(result: Result<LlmResponse, LlmError>) -> TestRouter {
        let mock = Arc::new(RecordingMock {
            result: Mutex::new(result),
            calls: Mutex::new(Vec::new()),
        });
        // Real OpenAiAdapter for default_model, balanced_model, and
        // image_model slots so the router can dispatch them if a test ever
        // exercises the fallback chain.
        let default_client: Arc<dyn LlmClient> = Arc::new(OpenAiAdapter::new(
            haven_common::config::ModelEndpoint::default(),
        ));
        let balanced_client: Arc<dyn LlmClient> = Arc::new(OpenAiAdapter::new(
            haven_common::config::ModelEndpoint::default(),
        ));
        let image_client: Arc<dyn LlmClient> = Arc::new(OpenAiAdapter::new(
            haven_common::config::ModelEndpoint::default(),
        ));
        let audio_client: Arc<dyn LlmClient> = Arc::new(OpenAiAdapter::new(
            haven_common::config::ModelEndpoint::default(),
        ));
        let router = Arc::new(LlmRouter::new_with_clients(
            mock.clone(),
            default_client,
            balanced_client,
            image_client,
            audio_client,
        ));
        // Simulate a configured small_model so generate() passes the
        // is_role_configured guard and reaches the recording mock.
        router
            .force_role_configured(EndpointRole::SmallModel, true)
            .await;
        TestRouter { router, mock }
    }

    fn ok_response(text: &str) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: text.into(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn empty_conversation_returns_none() {
        let tr = test_router(ok_response("ignored")).await;
        let generator = TitleGenerator::new(tr.router);
        assert!(generator.generate(&[]).await.is_none());
        // No LLM call should be made for an empty conversation.
        assert!(tr.mock.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unconfigured_small_model_returns_none_without_calls() {
        let tr = test_router(ok_response("ignored")).await;
        // The default (empty-key) config simulates an unconfigured small_model.
        tr.router
            .force_role_configured(EndpointRole::SmallModel, false)
            .await;
        let generator = TitleGenerator::new(tr.router);
        assert!(generator.generate(&["hi".into()]).await.is_none());
        // The guard must skip the LLM call entirely (no outbound HTTP).
        assert!(tr.mock.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn generates_and_trims_title() {
        let tr = test_router(ok_response("  整理代码  ")).await;
        let generator = TitleGenerator::new(tr.router);
        assert_eq!(
            generator
                .generate(&["帮我整理代码".into()])
                .await
                .as_deref(),
            Some("整理代码")
        );

        // The prompt must be a system + user message pair carrying the conversation.
        let calls = tr.mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 2);
        assert!(matches!(calls[0][0].role, LlmRole::System));
        assert!(matches!(calls[0][1].role, LlmRole::User));
    }

    #[tokio::test]
    async fn strips_surrounding_quotes() {
        let tr = test_router(ok_response("\"整理代码\"")).await;
        let generator = TitleGenerator::new(tr.router);
        assert_eq!(
            generator.generate(&["hi".into()]).await.as_deref(),
            Some("整理代码")
        );
    }

    #[tokio::test]
    async fn whitespace_only_response_returns_none() {
        let tr = test_router(ok_response("   \n  ")).await;
        let generator = TitleGenerator::new(tr.router);
        assert!(generator.generate(&["hi".into()]).await.is_none());
    }

    #[tokio::test]
    async fn overly_long_response_returns_none() {
        let long = "x".repeat(101);
        let tr = test_router(ok_response(&long)).await;
        let generator = TitleGenerator::new(tr.router);
        assert!(generator.generate(&["hi".into()]).await.is_none());
    }

    #[tokio::test]
    async fn llm_error_returns_none() {
        // Use a non-retryable error: a retryable one (e.g. ServerError) would
        // make the router sleep through the full retry backoff (~17s) before
        // falling back, which is not what this unit test needs.
        let tr = test_router(Err(LlmError::Auth("boom".into()))).await;
        let generator = TitleGenerator::new(tr.router);
        assert!(generator.generate(&["hi".into()]).await.is_none());
    }
}
