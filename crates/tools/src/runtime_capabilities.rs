//! Runtime capability resolution and projection for prompt-facing snapshots.
//!
//! The Tools facade supplies one already-read platform snapshot and one built MCP
//! index. This module resolves the snapshot's typed capability inputs and
//! keeps the resulting policy separate from facade composition.

use crate::builtin::media::MediaCapabilities;
use crate::catalog::McpServerIndexEntry;
use crate::tool_runtime::{PlatformRuntime, RuntimeCapabilities, WebSearchAvailability};
use haven_common::config::RequestKind;
use haven_llm::LlmRouter;
use std::sync::Arc;

pub(crate) async fn resolve_snapshot(
    platform: &PlatformRuntime,
    mcp_index: &[McpServerIndexEntry],
) -> ToolCapabilitySnapshot {
    let media = resolve_tool_media_capabilities(platform).await;
    let provider_search_available = match &platform.router {
        Some(router) => {
            router
                .provider_web_search_available(RequestKind::Chat)
                .await
        }
        None => false,
    };
    let independent_search_available = match &platform.router {
        Some(router) => {
            platform
                .tool_settings
                .get("web_search_api")
                .map(|setting| setting.enabled)
                .unwrap_or(true)
                && router
                    .independent_web_search_available(RequestKind::Chat)
                    .await
        }
        None => false,
    };
    assemble_tool_capability_snapshot(
        media,
        independent_search_available,
        provider_search_available,
        mcp_index,
    )
}

/// One freshly resolved view of capabilities owned by `ToolsFacade`.
///
/// This value is deliberately not cached: platform replacement, the router's
/// own config publication, and MCP tools/list updates do not share one version
/// clock. The facade rebuilds it from current inputs for each read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ToolCapabilitySnapshot {
    pub(crate) media: MediaCapabilities,
    pub(crate) web_search: WebSearchAvailability,
}

impl ToolCapabilitySnapshot {
    pub(crate) fn project_runtime_capabilities(self) -> RuntimeCapabilities {
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

/// Combine configured LLM routes and available runtime services into the
/// media capability snapshot shared by prompt reporting and tool admission.
/// Capture, generation, OCR, and TTS remain independent of transcription.
async fn resolve_tool_media_capabilities(platform: &PlatformRuntime) -> MediaCapabilities {
    let mut capabilities =
        resolve_backend_media_capabilities(platform.router.as_ref(), platform.stt_client.is_some())
            .await;
    capabilities.record = platform.input_pipeline.is_some();
    capabilities.ocr = platform.ocr_client.is_some();
    capabilities.generate = platform.image_gen_client.is_some();
    capabilities.speak = platform.tts_client.is_some();
    capabilities
}

/// Resolve the LLM route and dedicated STT capabilities available to media
/// operations. The module-level tool resolver adds the remaining local
/// runtime services before publishing one `MediaCapabilities` value.
async fn resolve_backend_media_capabilities(
    router: Option<&Arc<LlmRouter>>,
    dedicated_stt_available: bool,
) -> MediaCapabilities {
    let Some(router) = router else {
        return MediaCapabilities {
            transcribe: dedicated_stt_available,
            ..MediaCapabilities::default()
        };
    };

    let vision_available = router.is_request_configured(RequestKind::Vision).await
        && router
            .capability_profile_for_request(RequestKind::Vision)
            .image
            .is_supported();

    let transcribe_available = router.transcription_available().await;

    MediaCapabilities {
        describe: vision_available,
        transcribe: transcribe_available || dedicated_stt_available,
        ..MediaCapabilities::default()
    }
}

fn assemble_tool_capability_snapshot(
    media: MediaCapabilities,
    independent_search_available: bool,
    provider_search_available: bool,
    mcp_index: &[McpServerIndexEntry],
) -> ToolCapabilitySnapshot {
    let mcp_search_available = mcp_index.iter().any(mcp_index_entry_has_search_tool);
    ToolCapabilitySnapshot {
        media,
        web_search: resolve_web_search_availability(
            independent_search_available,
            provider_search_available,
            mcp_search_available,
        ),
    }
}

fn resolve_web_search_availability(
    independent_search_available: bool,
    provider_search_available: bool,
    mcp_search_available: bool,
) -> WebSearchAvailability {
    if independent_search_available {
        WebSearchAvailability::Independent
    } else if provider_search_available {
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
            resolve_web_search_availability(false, true, true),
            WebSearchAvailability::Provider
        );
        assert_eq!(
            resolve_web_search_availability(false, true, false),
            WebSearchAvailability::Provider
        );
        assert_eq!(
            resolve_web_search_availability(false, false, true),
            WebSearchAvailability::Mcp
        );
        assert_eq!(
            resolve_web_search_availability(false, false, false),
            WebSearchAvailability::Unavailable
        );
        assert_eq!(
            resolve_web_search_availability(true, true, true),
            WebSearchAvailability::Independent
        );
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
            false,
            &[],
        );
        let capabilities = snapshot.project_runtime_capabilities();

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
            false,
            &[],
        );
        let capabilities = snapshot.project_runtime_capabilities();

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
            false,
            true,
            &[mcp_entry("research", &["web_search"])],
        );

        assert!(snapshot.media.transcribe);
        assert!(snapshot.media.record);
        assert_eq!(snapshot.web_search, WebSearchAvailability::Provider);
        assert_eq!(
            snapshot.project_runtime_capabilities().web_search,
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
