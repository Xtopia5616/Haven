use std::sync::Arc;
use haven_common::config::ModelEndpoint;
use haven_llm::client::{HttpLlmClient, LlmClient};
use haven_llm::types::{ContentPart, LlmMessage, LlmRole};

/// Generates concise conversation titles using the small_model endpoint.
#[derive(Clone)]
pub struct TitleGenerator {
    client: Arc<HttpLlmClient>,
}

impl TitleGenerator {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self {
            client: Arc::new(HttpLlmClient::new(endpoint)),
        }
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
