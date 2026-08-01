use haven_common::config::ModelEndpoint;
use haven_llm::client::{HttpLlmClient, LlmClient};
use haven_llm::types::{ContentPart, LlmMessage, LlmRole};
use std::sync::Arc;

/// Generates concise conversation titles using the small_model endpoint.
#[derive(Clone)]
pub struct TitleGenerator {
    client: Arc<dyn LlmClient>,
}

impl TitleGenerator {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self {
            client: Arc::new(HttpLlmClient::new(endpoint)),
        }
    }

    /// Test hook: construct with an arbitrary client implementation.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new_with_client(client: Arc<dyn LlmClient>) -> Self {
        Self { client }
    }

    /// Generate a short title from conversation messages.
    /// Returns `None` if the LLM call fails or returns empty text.
    pub async fn generate(&self, conversation: &[String]) -> Option<String> {
        if conversation.is_empty() {
            return None;
        }
        let conv_text = conversation.join("\n");
        let messages = vec![
            LlmMessage {
                role: LlmRole::System,
                content: vec![ContentPart::text(
                    "You are a title generator. Generate a concise title (max 6 words, in the same language as the conversation) for this conversation. Respond with ONLY the title, no quotes, no punctuation, no explanation.",
                )],
                tool_call_id: None,
                tool_calls: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text(conv_text)],
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        match self.client.chat(messages).await {
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
    use haven_llm::types::{LlmError, LlmResponse};
    use std::pin::Pin;
    use std::sync::Mutex;

    struct MockClient {
        result: Mutex<Result<LlmResponse, LlmError>>,
        calls: Mutex<Vec<Vec<LlmMessage>>>,
    }

    impl MockClient {
        fn new(result: Result<LlmResponse, LlmError>) -> Self {
            Self {
                result: Mutex::new(result),
                calls: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LlmClient for MockClient {
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

    fn ok_response(text: &str) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            text: text.into(),
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn empty_conversation_returns_none() {
        let mock = Arc::new(MockClient::new(ok_response("ignored")));
        let client: Arc<dyn LlmClient> = mock.clone();
        let generator = TitleGenerator::new_with_client(client);
        assert!(generator.generate(&[]).await.is_none());
        // No LLM call should be made for an empty conversation.
        assert!(mock.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn generates_and_trims_title() {
        let mock = Arc::new(MockClient::new(ok_response("  整理代码  ")));
        let client: Arc<dyn LlmClient> = mock.clone();
        let generator = TitleGenerator::new_with_client(client);
        assert_eq!(
            generator
                .generate(&["帮我整理代码".into()])
                .await
                .as_deref(),
            Some("整理代码")
        );

        // The prompt must be a system + user message pair carrying the conversation.
        let calls = mock.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), 2);
        assert!(matches!(calls[0][0].role, LlmRole::System));
        assert!(matches!(calls[0][1].role, LlmRole::User));
    }

    #[tokio::test]
    async fn strips_surrounding_quotes() {
        let client: Arc<dyn LlmClient> = Arc::new(MockClient::new(ok_response("\"整理代码\"")));
        let generator = TitleGenerator::new_with_client(client.clone());
        assert_eq!(
            generator.generate(&["hi".into()]).await.as_deref(),
            Some("整理代码")
        );
    }

    #[tokio::test]
    async fn whitespace_only_response_returns_none() {
        let client: Arc<dyn LlmClient> = Arc::new(MockClient::new(ok_response("   \n  ")));
        let generator = TitleGenerator::new_with_client(client);
        assert!(generator.generate(&["hi".into()]).await.is_none());
    }

    #[tokio::test]
    async fn overly_long_response_returns_none() {
        let long = "x".repeat(101);
        let client: Arc<dyn LlmClient> = Arc::new(MockClient::new(ok_response(&long)));
        let generator = TitleGenerator::new_with_client(client);
        assert!(generator.generate(&["hi".into()]).await.is_none());
    }

    #[tokio::test]
    async fn llm_error_returns_none() {
        let client: Arc<dyn LlmClient> =
            Arc::new(MockClient::new(Err(LlmError::ServerError("boom".into()))));
        let generator = TitleGenerator::new_with_client(client);
        assert!(generator.generate(&["hi".into()]).await.is_none());
    }
}
