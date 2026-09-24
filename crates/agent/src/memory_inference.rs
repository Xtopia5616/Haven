use std::sync::Arc;

use async_trait::async_trait;
use haven_common::config::RequestKind;
use haven_llm::{LlmRouter, PromptRequest};

/// The model capability used by background memory extraction and maintenance.
///
/// Keeping routing behind this port lets the memory worker own its job policy
/// without depending on the general-purpose router surface.
#[async_trait]
pub(crate) trait MemoryInferencePort: Send + Sync {
    async fn is_fast_chat_configured(&self) -> bool;

    async fn fast_chat(&self, system_prompt: &str, user_prompt: &str) -> anyhow::Result<String>;
}

pub(crate) struct RouterMemoryInferencePort {
    router: Arc<LlmRouter>,
}

impl RouterMemoryInferencePort {
    pub(crate) fn new(router: Arc<LlmRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl MemoryInferencePort for RouterMemoryInferencePort {
    async fn is_fast_chat_configured(&self) -> bool {
        self.router
            .is_request_configured(RequestKind::FastChat)
            .await
    }

    async fn fast_chat(&self, system_prompt: &str, user_prompt: &str) -> anyhow::Result<String> {
        let response = self
            .router
            .chat_with_prompt_request(PromptRequest::new(
                RequestKind::FastChat,
                system_prompt,
                user_prompt,
            ))
            .await?;
        Ok(response.text)
    }
}
