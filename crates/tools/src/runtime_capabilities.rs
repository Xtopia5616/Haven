//! Runtime capability resolution and projection for prompt-facing snapshots.
//!
//! The manager supplies one already-read platform snapshot and one built MCP
//! index. This module resolves the snapshot's typed capability inputs and
//! keeps the resulting policy separate from facade composition.

use crate::builtin::{self, media::MediaCapabilities};
use crate::catalog::McpServerIndexEntry;
use crate::tool_runtime::{PlatformRuntime, RuntimeCapabilities, WebSearchAvailability};
use haven_common::config::{ModelEndpoint, RequestKind};
use haven_llm::LlmRouter;
use std::sync::Arc;

pub(crate) async fn resolve_snapshot(
    platform: &PlatformRuntime,
    mcp_index: &[McpServerIndexEntry],
) -> ToolCapabilitySnapshot {
    let media = resolve_media_capabilities(platform).await;
    let chat_endpoint = configured_chat_endpoint(platform.router.as_ref()).await;
    let provider_search_available = provider_search_available(chat_endpoint.as_ref());
    assemble_tool_capability_snapshot(media, provider_search_available, mcp_index)
}

/// One freshly resolved view of capabilities owned by `ToolsManager`.
///
/// This value is deliberately not cached: platform replacement, the router's
/// own config publication, and MCP tools/list updates do not share one version
/// clock. The manager rebuilds it from current inputs for each read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolCapabilitySnapshot {
    pub(crate) media: MediaCapabilities,
    pub(crate) web_search: WebSearchAvailability,
}

impl ToolCapabilitySnapshot {
    pub(crate) fn runtime_capabilities(self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            vision: self.media.describe,
            image_generation: self.media.generate,
            transcription: self.media.transcribe,
            recording: self.media.record,
            tts: self.media.speak,
            web_search: self.web_search,
        }
    }
}

/// Resolve the media capability snapshot shared by prompt reporting and
/// transcription ingress. Capture, generation, OCR, and TTS remain distinct
/// from provider-backed vision and transcription capabilities.
pub(crate) async fn resolve_media_capabilities(platform: &PlatformRuntime) -> MediaCapabilities {
    let mut capabilities = builtin::resolve_media_capabilities(
        platform.router.as_ref(),
        platform.stt_client.is_some(),
    )
    .await;
    capabilities.record = platform.audio_pipeline.is_some();
    capabilities.ocr = platform.ocr_client.is_some();
    capabilities.generate = platform.image_gen_client.is_some();
    capabilities.speak = platform.tts_client.is_some();
    capabilities
}

async fn configured_chat_endpoint(router: Option<&Arc<LlmRouter>>) -> Option<ModelEndpoint> {
    let router = router?;
    let config = router.config().await;
    config
        .route(RequestKind::Chat)
        .map(|model| model.endpoint.clone())
}

fn provider_search_available(chat_endpoint: Option<&ModelEndpoint>) -> bool {
    let Some(endpoint) = chat_endpoint else {
        // A default endpoint is not evidence that a Chat route is configured.
        return false;
    };
    let style = haven_llm::adapters::api_style_for(endpoint);
    let mode = haven_llm::adapters::resolve_web_search_mode(endpoint);
    !matches!(mode, haven_llm::WebSearchMode::Off) && haven_llm::supports_builtin_web_search(style)
}

fn assemble_tool_capability_snapshot(
    media: MediaCapabilities,
    provider_search_available: bool,
    mcp_index: &[McpServerIndexEntry],
) -> ToolCapabilitySnapshot {
    let mcp_search_available = mcp_index.iter().any(mcp_index_entry_has_search_tool);
    ToolCapabilitySnapshot {
        media,
        web_search: resolve_web_search_availability(
            provider_search_available,
            mcp_search_available,
        ),
    }
}

fn resolve_web_search_availability(
    provider_search_available: bool,
    mcp_search_available: bool,
) -> WebSearchAvailability {
    if provider_search_available {
        WebSearchAvailability::Provider
    } else if mcp_search_available {
        WebSearchAvailability::Mcp
    } else {
        WebSearchAvailability::Unavailable
    }
}

pub(crate) fn mcp_index_entry_has_search_tool(entry: &McpServerIndexEntry) -> bool {
    entry
        .tool_names
        .iter()
        .any(|tool| tool.to_ascii_lowercase().contains("search"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mcp_entry(name: &str, tool_names: &[&str]) -> McpServerIndexEntry {
        McpServerIndexEntry {
            name: name.into(),
            tool_names: tool_names.iter().map(|name| (*name).into()).collect(),
        }
    }

    #[test]
    fn provider_search_takes_priority_over_mcp_search() {
        assert_eq!(
            resolve_web_search_availability(true, true),
            WebSearchAvailability::Provider
        );
        assert_eq!(
            resolve_web_search_availability(true, false),
            WebSearchAvailability::Provider
        );
        assert_eq!(
            resolve_web_search_availability(false, true),
            WebSearchAvailability::Mcp
        );
        assert_eq!(
            resolve_web_search_availability(false, false),
            WebSearchAvailability::Unavailable
        );
    }

    #[test]
    fn provider_search_requires_a_configured_chat_route() {
        assert!(!provider_search_available(None));
    }

    #[test]
    fn media_capabilities_map_to_prompt_runtime_capabilities() {
        let snapshot = assemble_tool_capability_snapshot(
            MediaCapabilities {
                describe: true,
                ocr: true,
                transcribe: true,
                generate: true,
                record: false,
                speak: true,
            },
            false,
            &[],
        );
        let capabilities = snapshot.runtime_capabilities();

        assert!(capabilities.vision);
        assert!(capabilities.image_generation);
        assert!(capabilities.transcription);
        assert!(!capabilities.recording);
        assert!(capabilities.tts);
        assert_eq!(capabilities.web_search, WebSearchAvailability::Unavailable);
    }

    #[test]
    fn recording_remains_available_without_transcription() {
        let snapshot = assemble_tool_capability_snapshot(
            MediaCapabilities {
                record: true,
                ..MediaCapabilities::default()
            },
            false,
            &[],
        );
        let capabilities = snapshot.runtime_capabilities();

        assert!(capabilities.recording);
        assert!(!capabilities.transcription);
        assert!(snapshot.media.record);
        assert!(!snapshot.media.transcribe);
    }

    #[test]
    fn snapshot_owns_media_and_provider_mcp_priority_together() {
        let snapshot = assemble_tool_capability_snapshot(
            MediaCapabilities {
                transcribe: true,
                record: true,
                ..MediaCapabilities::default()
            },
            true,
            &[mcp_entry("research", &["web_search"])],
        );

        assert!(snapshot.media.transcribe);
        assert!(snapshot.media.record);
        assert_eq!(snapshot.web_search, WebSearchAvailability::Provider);
        assert_eq!(
            snapshot.runtime_capabilities().web_search,
            WebSearchAvailability::Provider
        );
    }

    #[test]
    fn mcp_search_detection_only_uses_cached_tool_names() {
        assert!(mcp_index_entry_has_search_tool(&mcp_entry(
            "research",
            &["fetch", "web_search"]
        )));
        assert!(!mcp_index_entry_has_search_tool(&mcp_entry(
            "search-like-server",
            &[]
        )));
        assert!(!mcp_index_entry_has_search_tool(&mcp_entry(
            "research",
            &["fetch"]
        )));
    }
}
