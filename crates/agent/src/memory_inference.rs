#[cfg(test)]
use std::sync::Arc;

use async_trait::async_trait;
use haven_common::config::RequestKind;
#[cfg(test)]
use haven_llm::LlmRouter;
use haven_llm::PromptRequest;

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
    router: crate::model_runtime::ModelRuntime,
}

impl RouterMemoryInferencePort {
    pub(crate) fn with_runtime(router: crate::model_runtime::ModelRuntime) -> Self {
        Self { router }
    }

    #[cfg(test)]
    pub(crate) fn new(router: Arc<LlmRouter>) -> Self {
        Self {
            router: crate::model_runtime::ModelRuntime::new(router),
        }
    }
}

#[async_trait]
impl MemoryInferencePort for RouterMemoryInferencePort {
    async fn is_fast_chat_configured(&self) -> bool {
        self.router
            .snapshot()
            .is_request_configured(RequestKind::FastChat)
            .await
    }

    async fn fast_chat(&self, system_prompt: &str, user_prompt: &str) -> anyhow::Result<String> {
        let router = self.router.snapshot();
        let response = router
            .chat_with_prompt_request(PromptRequest::new(
                RequestKind::FastChat,
                system_prompt,
                user_prompt,
            ))
            .await?;
        Ok(response.text)
    }
}
