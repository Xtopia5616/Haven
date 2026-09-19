//! Shared one-shot image understanding through the canonical media boundary.
//!
//! Provider-wire adapter for one already-read image request. Asset lookup,
//! tool policy, lifecycle, cross-provider fallback and structured media
//! results belong to `haven-tools::builtin::media::MediaTool`; this module
//! only builds the provider-neutral request and dispatches it through the
//! selected vision role.

use base64::Engine;
use haven_common::media::{
    MediaAsset, MediaAssetLifecycle, MediaAssetSource, MediaInput, MediaInputStrategy,
    MediaProvenance, MediaRepresentation, MediaRepresentationKind, MediaRepresentationPayload,
    build_media_plan,
};
use haven_common::text::sanitize_prompt_field;
use haven_common::types::{CanonicalMessage, ContentPart};

use crate::LlmRouter;
use crate::types::{LlmError, LlmResponse};
use haven_common::config::RequestKind;

const MAX_PROMPT_FIELD_CHARS: usize = 32_000;

/// Dispatch one image request through the vision provider role. This function
/// is a wire adapter, not an asset-aware or model-facing orchestration API.
pub async fn analyze_image(
    router: &LlmRouter,
    bytes: &[u8],
    media_type: &str,
    system_prompt: &str,
    focus: Option<&str>,
) -> Result<LlmResponse, LlmError> {
    if bytes.is_empty() {
        return Err(LlmError::RequestFailed("image payload is empty".into()));
    }
    if !media_type.starts_with("image/") {
        return Err(LlmError::UnsupportedCapability(format!(
            "vision input must be an image, got {media_type}"
        )));
    }

    let input = MediaInput {
        asset: MediaAsset::new(
            media_type,
            bytes.len() as u64,
            None,
            MediaAssetSource::ToolOutput,
            MediaAssetLifecycle::Request,
        ),
        representations: vec![MediaRepresentation::available(
            MediaRepresentationKind::RawImage,
            MediaProvenance::Original,
            MediaRepresentationPayload::InlineData {
                media_type: media_type.to_string(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        )],
        preferred_representation: Some(MediaRepresentationKind::RawImage),
    };
    let capabilities = router.capability_profile_for_request(RequestKind::Vision);
    let plan = build_media_plan(
        std::slice::from_ref(&input),
        &capabilities,
        MediaInputStrategy::RawPreferred,
    );
    if plan.projections.is_empty() {
        return Err(LlmError::UnsupportedCapability(format!(
            "vision endpoint cannot accept image media type {media_type}"
        )));
    }
    let parts = crate::media::project_media_plan(&plan, std::slice::from_ref(&input))?;
    let Some(image_part) = parts.into_iter().next() else {
        return Err(LlmError::UnsupportedCapability(
            "vision planner produced no image content".into(),
        ));
    };

    let mut system = sanitize_prompt_field(system_prompt, MAX_PROMPT_FIELD_CHARS);
    if let Some(focus) = focus.map(str::trim).filter(|focus| !focus.is_empty()) {
        let focus = sanitize_prompt_field(focus, 2_000);
        if !system.is_empty() {
            system.push(' ');
        }
        system.push_str("Pay special attention to: ");
        system.push_str(&focus);
    }

    let messages = vec![
        CanonicalMessage::system(vec![ContentPart::text(system)]),
        CanonicalMessage::user(vec![image_part]),
    ];
    router.chat_request(RequestKind::Vision, messages).await
}

/// Small test-only capability profile helper kept private to the module.
#[cfg(test)]
fn image_capabilities() -> haven_common::media::CapabilityProfile {
    haven_common::media::CapabilityProfile {
        image: haven_common::media::CapabilitySupport::Supported,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LlmClient;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct MockImageClient;

    #[async_trait]
    impl LlmClient for MockImageClient {
        fn capability_profile(&self) -> haven_common::media::CapabilityProfile {
            image_capabilities()
        }

        async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            assert!(matches!(
                messages[1].content.as_slice(),
                [ContentPart::Image { .. }]
            ));
            Ok(LlmResponse {
                text: "ok".into(),
                ..Default::default()
            })
        }

        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(messages).await
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures_util::Stream<Item = Result<crate::types::StreamChunk, LlmError>>
                        + Send,
                >,
            >,
            LlmError,
        > {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn rejects_non_image_before_dispatch() {
        let client = Arc::new(MockImageClient);
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        let error = analyze_image(&router, b"bytes", "text/plain", "inspect", None)
            .await
            .unwrap_err();
        assert!(matches!(error, LlmError::UnsupportedCapability(_)));
    }

    #[tokio::test]
    async fn plans_and_dispatches_image_through_vision_role() {
        let client = Arc::new(MockImageClient);
        let router =
            LlmRouter::new_with_clients(client.clone(), client.clone(), client.clone(), client);
        let response = analyze_image(
            &router,
            b"encoded image bytes",
            "image/png",
            "inspect the image",
            Some("labels\nwith control text"),
        )
        .await
        .unwrap();
        assert_eq!(response.text, "ok");
    }
}
