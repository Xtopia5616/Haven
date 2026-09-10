//! Agent-native media operations.
//!
//! The model-facing boundary is deliberately small: callers pass an opaque
//! `asset_id`, select a media operation, and receive a canonical
//! `MediaInput` containing the source reference plus any derived
//! representation. Host paths and raw bytes stay inside this module.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use haven_common::config::GENERATED_MEDIA_RETENTION_SECS;
use haven_common::media::{
    MediaAsset, MediaAssetLifecycle, MediaAssetSource, MediaDerivation, MediaInput, MediaModality,
    MediaProvenance, MediaRepresentation, MediaRepresentationKind, MediaRepresentationPayload,
};
use haven_common::prompts::IMAGE_ANALYSIS_SYSTEM_PROMPT;
use haven_common::types::RiskLevel;
use haven_llm::LlmRouter;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::document::{MAX_DOCUMENT_BYTES, extract_document_with_cancel, supports_document_path};
use crate::{ManagedAsset, ManagedAssetRegistry, Tool, ToolConcurrency, ToolResult};

const MAX_FOCUS_CHARS: usize = 2_000;

/// Coarse media classification shared by the media tool and window output
/// projection. MIME is authoritative when it is specific; the filename is a
/// controlled fallback for restored or loosely typed assets.
pub(crate) fn classify_media(asset: &ManagedAsset) -> (MediaModality, &'static str) {
    let media_type = asset.media_type.to_ascii_lowercase();
    let extension = asset
        .filename
        .as_deref()
        .and_then(|name| Path::new(name).extension())
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if media_type.starts_with("image/")
        || matches!(
            extension.as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"
        )
    {
        return (MediaModality::Image, "image");
    }
    if media_type.starts_with("audio/")
        || matches!(extension.as_str(), "wav" | "mp3" | "flac" | "ogg" | "m4a")
    {
        return (MediaModality::Audio, "audio");
    }
    if media_type == "application/pdf"
        || matches!(extension.as_str(), "pdf" | "docx" | "xlsx" | "pptx")
        || media_type.contains("wordprocessingml")
        || media_type.contains("spreadsheetml")
        || media_type.contains("presentationml")
    {
        return (MediaModality::Document, "document");
    }
    if media_type.starts_with("text/") {
        return (MediaModality::Text, "text");
    }
    (MediaModality::Text, "binary")
}

/// Project a trusted registry entry into the provider-neutral media contract.
/// The representation is a managed reference even for raw media: a later
/// operation is responsible for reading bytes and asking the planner whether
/// the selected provider can receive them.
pub(crate) fn managed_media_input(
    asset: &ManagedAsset,
    source: MediaAssetSource,
    lifecycle: MediaAssetLifecycle,
) -> MediaInput {
    let mut media_asset = MediaAsset::new(
        asset.media_type.clone(),
        asset.size_bytes.unwrap_or_default(),
        asset.filename.clone(),
        source,
        lifecycle,
    );
    media_asset.asset_id = asset.asset_id.clone();
    media_asset.content_hash = asset.sha256.clone().unwrap_or_default();
    media_asset.expires_at = asset.expires_at.map(|expiry| expiry.to_rfc3339());

    let representation = MediaRepresentation::available(
        MediaRepresentationKind::ManagedFileRef,
        MediaProvenance::Original,
        MediaRepresentationPayload::ManagedFileRef {
            asset_id: asset.asset_id.clone(),
            filename: asset.filename.clone(),
        },
    );
    MediaInput {
        asset: media_asset,
        representations: vec![representation],
        preferred_representation: None,
    }
}

pub(crate) fn add_derived_text(
    mut input: MediaInput,
    kind: MediaRepresentationKind,
    operation: MediaDerivation,
    source_kind: Option<MediaRepresentationKind>,
    text: String,
    provider: Option<String>,
) -> MediaInput {
    input.representations.push(MediaRepresentation::available(
        kind,
        MediaProvenance::Derived {
            operation,
            provider,
            source_kind,
        },
        MediaRepresentationPayload::Text(text),
    ));
    input.preferred_representation = Some(kind);
    input
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOperation {
    Inspect,
    Describe,
    Transcribe,
    Extract,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MediaParams {
    pub operation: MediaOperation,
    pub asset_id: String,
    #[serde(default)]
    pub focus: Option<String>,
}

pub struct MediaTool {
    router: Option<Arc<LlmRouter>>,
    managed_assets: ManagedAssetRegistry,
    max_bytes: u64,
    timeout_secs: u64,
    max_output_chars: usize,
}

impl MediaTool {
    pub fn new(
        router: Option<Arc<LlmRouter>>,
        managed_assets: ManagedAssetRegistry,
        max_bytes: u64,
        timeout_secs: u64,
        max_output_chars: usize,
    ) -> Self {
        Self {
            router,
            managed_assets,
            max_bytes,
            timeout_secs,
            max_output_chars: max_output_chars.max(1),
        }
    }

    pub async fn run(
        &self,
        params: MediaParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let asset_id = params.asset_id.trim();
        if asset_id.is_empty() {
            anyhow::bail!("asset_id is required");
        }
        let asset = self
            .managed_assets
            .resolve(asset_id)
            .ok_or_else(|| anyhow::anyhow!("media asset is unavailable or expired"))?;
        if !self.managed_assets.revalidate(&asset) {
            anyhow::bail!("media asset changed or is no longer inside its managed root");
        }
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let (modality, file_kind) = classify_media(&asset);
        let input = managed_media_input(
            &asset,
            MediaAssetSource::ToolOutput,
            MediaAssetLifecycle::Managed,
        );
        match params.operation {
            MediaOperation::Inspect => Ok(ToolResult::ok(json!({
                "operation": "inspect",
                "asset_id": asset.asset_id,
                "modality": modality,
                "file_kind": file_kind,
                "media": input,
            }))),
            MediaOperation::Describe => self.describe(asset, input, params.focus, cancel).await,
            MediaOperation::Transcribe => self.transcribe(asset, input, cancel).await,
            MediaOperation::Extract => self.extract(asset, input, cancel).await,
        }
    }

    async fn read_bounded(
        &self,
        asset: &ManagedAsset,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Vec<u8>> {
        let metadata = tokio::fs::metadata(&asset.path).await?;
        let size = metadata.len();
        if size > self.max_bytes {
            anyhow::bail!(
                "media asset is {} bytes, above the {} byte media limit",
                size,
                self.max_bytes
            );
        }
        let bytes = tokio::fs::read(&asset.path).await?;
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        Ok(bytes)
    }

    async fn describe(
        &self,
        asset: ManagedAsset,
        input: MediaInput,
        focus: Option<String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaModality::Image {
            anyhow::bail!("describe requires an image asset");
        }
        let Some(router) = self.router.clone() else {
            return Ok(ToolResult::ok(json!({
                "operation": "describe",
                "asset_id": asset.asset_id,
                "media": input,
                "available": false,
                "reason": "No vision-capable LLM router is configured.",
            })));
        };
        let bytes = match self.read_bounded(&asset, &cancel).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(ToolResult::failed(
                    json!({"operation": "describe", "asset_id": asset.asset_id, "media": input}),
                    error.to_string(),
                ));
            }
        };
        let focus = focus
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(MAX_FOCUS_CHARS).collect::<String>());
        let response = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            router.analyze_image(
                &bytes,
                &asset.media_type,
                IMAGE_ANALYSIS_SYSTEM_PROMPT,
                focus.as_deref(),
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return Ok(ToolResult::failed(
                    json!({"operation": "describe", "asset_id": asset.asset_id, "media": input, "available": false}),
                    format!("vision call failed: {error}"),
                ));
            }
            Err(_) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("vision call timed out after {}s", self.timeout_secs),
                ));
            }
        };
        let (text, text_truncated) = bound_text(response.text.trim(), self.max_output_chars);
        let media = add_derived_text(
            input,
            MediaRepresentationKind::ImageDescription,
            MediaDerivation::ImageDescribe,
            Some(MediaRepresentationKind::RawImage),
            text.clone(),
            response.model.clone(),
        );
        let output = json!({
            "operation": "describe",
            "asset_id": asset.asset_id,
            "text": text,
            "media": media,
            "representation": "image_description",
            "model": response.model,
            "untrusted_content": true,
        });
        Ok(if text_truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        })
    }

    async fn transcribe(
        &self,
        asset: ManagedAsset,
        input: MediaInput,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaModality::Audio {
            anyhow::bail!("transcribe requires an audio asset");
        }
        let Some(router) = self.router.clone() else {
            return Ok(ToolResult::ok(json!({
                "operation": "transcribe",
                "asset_id": asset.asset_id,
                "media": input,
                "available": false,
                "reason": "No speech-to-text LLM router is configured.",
            })));
        };
        let bytes = self.read_bounded(&asset, &cancel).await?;
        let result = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            router.transcribe_audio(&bytes),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                return Ok(ToolResult::failed(
                    json!({"operation": "transcribe", "asset_id": asset.asset_id, "media": input, "available": false}),
                    format!("transcription failed: {error}"),
                ));
            }
            Err(_) => {
                return Ok(ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("transcription timed out after {}s", self.timeout_secs),
                ));
            }
        };
        let (text, text_truncated) = bound_text(result.text.trim(), self.max_output_chars);
        let media = add_derived_text(
            input,
            MediaRepresentationKind::Transcript,
            MediaDerivation::Stt,
            Some(MediaRepresentationKind::RawAudio),
            text.clone(),
            None,
        );
        let output = json!({
            "operation": "transcribe",
            "asset_id": asset.asset_id,
            "text": text,
            "media": media,
            "representation": "transcript",
            "confidence": result.confidence,
            "untrusted_content": true,
        });
        Ok(if text_truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        })
    }

    async fn extract(
        &self,
        asset: ManagedAsset,
        input: MediaInput,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaModality::Document
            || !supports_document_path(&asset.path)
        {
            anyhow::bail!("extract requires a supported PDF, DOCX, XLSX, or PPTX asset");
        }
        let path = asset.path.clone();
        let max_chars = self.max_output_chars.min(100_000);
        let extracted = tokio::task::spawn_blocking(move || {
            extract_document_with_cancel(&path, max_chars, MAX_DOCUMENT_BYTES, &cancel)
        })
        .await??;
        let kind = if extracted.representation == "table_data" {
            MediaRepresentationKind::TableData
        } else {
            MediaRepresentationKind::DocumentPages
        };
        let truncated = extracted.truncated;
        let representation = extracted.representation;
        let format = extracted.format;
        let sections = extracted.sections;
        let size_bytes = extracted.size_bytes;
        let text = extracted.text;
        let media = add_derived_text(
            input,
            kind,
            MediaDerivation::DocumentExtract,
            Some(MediaRepresentationKind::ManagedFileRef),
            text.clone(),
            None,
        );
        Ok(if truncated {
            ToolResult::truncated(json!({
                "operation": "extract",
                "asset_id": asset.asset_id,
                "text": text,
                "media": media,
                "representation": representation,
                "format": format.as_str(),
                "sections": sections,
                "size_bytes": size_bytes,
                "untrusted_content": true,
            }))
        } else {
            ToolResult::ok(json!({
                "operation": "extract",
                "asset_id": asset.asset_id,
                "text": text,
                "media": media,
                "representation": representation,
                "format": format.as_str(),
                "sections": sections,
                "size_bytes": size_bytes,
                "untrusted_content": true,
            }))
        })
    }
}

fn bound_text(text: &str, max_chars: usize) -> (String, bool) {
    let bounded: String = text.chars().take(max_chars).collect();
    (bounded, text.chars().count() > max_chars)
}

#[async_trait]
impl Tool for MediaTool {
    fn name(&self) -> String {
        "media".into()
    }

    fn description(&self) -> String {
        "Operate on managed multimodal assets by asset_id: inspect metadata, describe images with vision, transcribe audio, or extract text/tables from supported documents. Use the asset_id returned by attachments or other media-producing tools; host paths and base64 are never accepted.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("describe") | Some("transcribe") => RiskLevel::Medium,
            Some("extract") | Some("inspect") => RiskLevel::Low,
            _ => RiskLevel::Low,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        let asset_id = input["asset_id"].as_str().unwrap_or("unknown");
        ToolConcurrency::Resource(format!("media:{asset_id}"))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.timeout_secs.max(30)
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operation": {"type": "string", "enum": ["inspect", "describe", "transcribe", "extract"]},
                "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"},
                "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}
            },
            "required": ["operation", "asset_id"],
            "oneOf": [
                {"properties": {"operation": {"const": "inspect"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "describe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "transcribe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "extract"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]}
            ]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<MediaParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

/// Register a generated tool output in the same lifecycle used by generated
/// attachments. Window capture uses this helper so its asset id is usable by
/// the media tool without introducing a second storage/cleanup path.
pub(crate) fn register_generated_asset(
    registry: &ManagedAssetRegistry,
    session_id: Option<&str>,
    root: &Path,
    path: std::path::PathBuf,
    filename: Option<String>,
    media_type: &str,
    size_bytes: u64,
) -> anyhow::Result<ManagedAsset> {
    let asset_id = haven_common::types::new_id("asset");
    let expires_at = Utc::now() + ChronoDuration::seconds(GENERATED_MEDIA_RETENTION_SECS as i64);
    let registered = if let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) {
        registry.register_under_root_for_session_with_metadata(
            session_id,
            root,
            asset_id.clone(),
            path.clone(),
            filename,
            media_type,
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    } else {
        registry.register_under_root_with_metadata(
            root,
            asset_id.clone(),
            path.clone(),
            filename,
            media_type.to_string(),
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    };
    if !registered {
        anyhow::bail!("failed to register generated media asset");
    }
    registry
        .resolve(&asset_id)
        .ok_or_else(|| anyhow::anyhow!("generated media asset disappeared after registration"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn registered_asset(
        root: &Path,
        name: &str,
        media_type: &str,
    ) -> (ManagedAssetRegistry, String) {
        let path = root.join(name);
        std::fs::write(&path, b"asset data").unwrap();
        let registry = ManagedAssetRegistry::default();
        let asset_id = haven_common::types::new_id("asset");
        assert!(registry.register_under_root(
            root,
            asset_id.clone(),
            path,
            Some(name.into()),
            media_type,
        ));
        (registry, asset_id)
    }

    #[test]
    fn schema_is_asset_only_and_operation_specific() {
        let tool = MediaTool::new(None, ManagedAssetRegistry::default(), 1024, 10, 2_000);
        assert!(
            tool.validate_input(&json!({
                "operation": "describe",
                "asset_id": "asset-0123456789abcdef0123456789abcdef"
            }))
            .is_ok()
        );
        assert!(
            tool.validate_input(&json!({
                "operation": "describe",
                "asset_id": "asset-0123456789abcdef0123456789abcdef",
                "path": "C:\\secret.png"
            }))
            .is_err()
        );
    }

    #[tokio::test]
    async fn inspect_returns_canonical_media_input_without_host_path() {
        let root = TempDir::new().unwrap();
        let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
        let tool = MediaTool::new(None, registry, 1024, 10, 2_000);
        let result = tool
            .execute(
                json!({"operation": "inspect", "asset_id": asset_id}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let serialized = serde_json::to_string(&result.output).unwrap();
        assert!(serialized.contains("managed_file_ref"));
        assert!(!serialized.contains(&root.path().to_string_lossy().to_string()));
    }

    #[tokio::test]
    async fn describe_without_router_is_explicitly_unavailable() {
        let root = TempDir::new().unwrap();
        let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
        let tool = MediaTool::new(None, registry, 1024, 10, 2_000);
        let result = tool
            .execute(
                json!({"operation": "describe", "asset_id": asset_id}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["available"], false);
        assert!(
            result.output["media"]["asset"]["asset_id"]
                .as_str()
                .unwrap()
                .starts_with("asset-")
        );
    }

    #[test]
    fn generated_asset_is_registered_with_expiry_and_opaque_metadata() {
        let root = TempDir::new().unwrap();
        let path = root
            .path()
            .join("file-0123456789abcdef0123456789abcdef.png");
        std::fs::write(&path, b"png-bytes").unwrap();
        let registry = ManagedAssetRegistry::default();
        let asset = register_generated_asset(
            &registry,
            None,
            root.path(),
            path.clone(),
            Some("screenshot.png".into()),
            "image/png",
            9,
        )
        .unwrap();
        assert!(asset.asset_id.starts_with("asset-"));
        assert!(asset.expires_at.is_some());
        let input = managed_media_input(
            &asset,
            MediaAssetSource::WindowCapture,
            MediaAssetLifecycle::Session,
        );
        let serialized = serde_json::to_string(&input).unwrap();
        assert!(!serialized.contains(&path.to_string_lossy().to_string()));
        assert!(serialized.contains("screenshot.png"));
    }
}
