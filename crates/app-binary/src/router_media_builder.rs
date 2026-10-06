//! Shared construction for the router and its configured media clients.
//!
//! Callers own failure policy and publication. Keeping each capability's
//! construction result independent lets startup degrade one optional client
//! while configuration updates can reject the whole prepared generation.

use haven_common::config::{AppConfig, MediaConfig};
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use std::sync::Arc;

pub(crate) struct RouterMediaBuild {
    pub(crate) router: Arc<LlmRouter>,
    pub(crate) media: MediaConfig,
    pub(crate) stt_client: Result<Option<Arc<dyn haven_llm::SttClient>>, anyhow::Error>,
    pub(crate) ocr_client: Result<Option<Arc<dyn haven_llm::OcrClient>>, anyhow::Error>,
    pub(crate) tts_client: Result<Option<Arc<dyn haven_llm::TtsClient>>, anyhow::Error>,
    pub(crate) image_gen_client: Result<Option<Arc<dyn haven_llm::ImageGenClient>>, anyhow::Error>,
}

pub(crate) fn build_router_media(
    config: &AppConfig,
    mcp_caller: Option<Arc<dyn haven_llm::McpToolCaller>>,
) -> RouterMediaBuild {
    let router = Arc::new(LlmRouter::with_default_context_window(
        config.llm.materialize(
            Some(config.context_limits.max_response_tokens),
            Some(config.context_limits.reasoning_echo_max_chars),
        ),
        config.context_limits.default_context_window,
    ));
    let media = config.media.clone();
    let providers = &config.llm.providers;

    let stt_client =
        build_stt_client(mcp_caller, &media.stt, providers).map(|client| client.map(Arc::from));
    let ocr_client = haven_llm::build_ocr_client(&media.ocr).map(|client| client.map(Arc::from));
    let tts_client =
        haven_llm::build_tts_client(&media.tts, providers).map(|client| client.map(Arc::from));
    let image_gen_client = haven_llm::build_image_gen_client(&media.image_gen, providers)
        .map(|client| client.map(Arc::from));

    RouterMediaBuild {
        router,
        media,
        stt_client,
        ocr_client,
        tts_client,
        image_gen_client,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_independent_results_for_each_media_capability() {
        let mut config = AppConfig::default();
        config.media.ocr.provider = "invalid-provider".into();

        let built = build_router_media(&config, None);

        assert!(built.ocr_client.is_err());
        assert!(built.stt_client.is_ok());
        assert!(built.tts_client.is_ok());
        assert!(built.image_gen_client.is_ok());
        assert_eq!(built.media, config.media);
    }
}
