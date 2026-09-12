//! Agent-native media operations.
//!
//! The model-facing boundary is deliberately small: callers pass an opaque
//! `asset_id`, select a media operation, and receive a compact media reference
//! containing the selected representation and optional derived content. Host
//! paths and raw bytes stay inside this module; durable attachment metadata is
//! owned by `haven_common::media`.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use haven_common::config::GENERATED_MEDIA_RETENTION_SECS;
use haven_common::media::MediaModality;
use haven_common::prompts::{IMAGE_ANALYSIS_SYSTEM_PROMPT, OCR_SYSTEM_PROMPT};
use haven_common::types::RiskLevel;
use haven_llm::LlmRouter;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::document::{MAX_DOCUMENT_BYTES, extract_document_with_cancel, supports_document_path};
use crate::{
    ManagedAsset, ManagedAssetRegistry, OperationIdempotency, Tool, ToolConcurrency, ToolLlmUsage,
    ToolResult,
};

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
        || matches!(
            extension.as_str(),
            "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx"
        )
        || media_type.contains("wordprocessingml")
        || media_type.contains("spreadsheetml")
        || media_type.contains("presentationml")
        || matches!(
            media_type.as_str(),
            "application/msword" | "application/vnd.ms-excel" | "application/vnd.ms-powerpoint"
        )
    {
        return (MediaModality::Document, "document");
    }
    if media_type.starts_with("text/") {
        return (MediaModality::Text, "text");
    }
    (MediaModality::Text, "binary")
}

/// Compact model-facing media reference. Runtime-only lifecycle data (hash,
/// expiry, source, size and provider provenance) stays in the host and is not
/// repeated in every tool observation. Every media-producing observation has
/// the same discovery fields, so the model can choose its next operation
/// without knowing which producer created the asset.
pub(crate) fn model_media_reference_with_capabilities(
    asset: &ManagedAsset,
    representation: &str,
    content: Option<&str>,
    describe_available: bool,
    transcribe_available: bool,
) -> Value {
    let (modality, file_kind) = classify_media(asset);
    let mut available_representations = vec!["managed_file_ref"];
    match modality {
        MediaModality::Image if describe_available => {
            available_representations.extend(["image_description", "ocr_text"]);
        }
        MediaModality::Audio if transcribe_available => {
            available_representations.push("transcript");
        }
        MediaModality::Document if supports_document_path(&asset.path) => {
            available_representations.push("document_pages");
        }
        _ => {}
    }
    if !available_representations.contains(&representation) {
        available_representations.push(representation);
    }
    let recommended_next = match (representation, modality) {
        ("managed_file_ref", MediaModality::Image) if describe_available => Some("media.describe"),
        ("managed_file_ref", MediaModality::Audio) if transcribe_available => {
            Some("media.transcribe")
        }
        ("managed_file_ref", MediaModality::Document) if supports_document_path(&asset.path) => {
            Some("media.extract")
        }
        _ => None,
    };
    let mut media = json!({
        "asset_id": asset.asset_id.clone(),
        "media_type": asset.media_type.clone(),
        "modality": modality,
        "file_kind": file_kind,
        "representation": representation,
        "available_representations": available_representations,
        "recommended_next": recommended_next,
    });
    if let Some(filename) = asset.filename.as_deref() {
        media["filename"] = json!(filename);
    }
    if let Some(content) = content {
        media["content"] = json!(content);
    }
    media
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOperation {
    Inspect,
    Describe,
    Ocr,
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
    stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    describe_available: bool,
    transcribe_available: bool,
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
        let has_router = router.is_some();
        Self {
            router,
            stt_client: None,
            describe_available: has_router,
            transcribe_available: has_router,
            managed_assets,
            max_bytes,
            timeout_secs,
            max_output_chars: max_output_chars.max(1),
        }
    }

    pub(crate) fn with_capabilities(
        mut self,
        describe_available: bool,
        transcribe_available: bool,
    ) -> Self {
        self.describe_available = describe_available;
        // Keep the schema truthful even when callers apply capability
        // overrides after installing the dedicated STT client. The client is
        // the authoritative live route for audio transcription.
        self.transcribe_available = transcribe_available || self.stt_client.is_some();
        self
    }

    pub(crate) fn with_stt_client(
        mut self,
        stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    ) -> Self {
        if stt_client.is_some() {
            self.transcribe_available = true;
        }
        self.stt_client = stt_client;
        self
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
        match params.operation {
            MediaOperation::Inspect => Ok(ToolResult::ok(json!({
                "operation": "inspect",
                "asset_id": asset.asset_id,
                "modality": modality,
                "file_kind": file_kind,
                "media": self.model_media_reference(&asset, "managed_file_ref", None),
            }))),
            MediaOperation::Describe => self.describe(asset, params.focus, cancel).await,
            MediaOperation::Ocr => self.ocr(asset, params.focus, cancel).await,
            MediaOperation::Transcribe => self.transcribe(asset, cancel).await,
            MediaOperation::Extract => self.extract(asset, cancel).await,
        }
    }

    fn model_media_reference(
        &self,
        asset: &ManagedAsset,
        representation: &str,
        content: Option<&str>,
    ) -> Value {
        model_media_reference_with_capabilities(
            asset,
            representation,
            content,
            self.describe_available,
            self.transcribe_available,
        )
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
        focus: Option<String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.derive_image(
            asset,
            focus,
            cancel,
            MediaOperation::Describe,
            IMAGE_ANALYSIS_SYSTEM_PROMPT,
            "image_description",
        )
        .await
    }

    async fn ocr(
        &self,
        asset: ManagedAsset,
        focus: Option<String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.derive_image(
            asset,
            focus,
            cancel,
            MediaOperation::Ocr,
            OCR_SYSTEM_PROMPT,
            "ocr_text",
        )
        .await
    }

    async fn derive_image(
        &self,
        asset: ManagedAsset,
        focus: Option<String>,
        cancel: CancellationToken,
        operation: MediaOperation,
        system_prompt: &str,
        representation: &str,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaModality::Image {
            anyhow::bail!("{} requires an image asset", operation_name(operation));
        }
        let operation_name = operation_name(operation);
        let unavailable = self.model_media_reference(&asset, "managed_file_ref", None);
        if !self.describe_available {
            return Ok(ToolResult::ok(json!({
                "operation": operation_name,
                "asset_id": asset.asset_id,
                "media": unavailable,
                "available": false,
                "reason": "No vision-capable LLM router is configured.",
            })));
        }
        let Some(router) = self.router.clone() else {
            return Ok(ToolResult::ok(json!({
                "operation": operation_name,
                "asset_id": asset.asset_id,
                "media": unavailable,
                "available": false,
                "reason": "No vision-capable LLM router is configured.",
            })));
        };
        let bytes = match self.read_bounded(&asset, &cancel).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(ToolResult::failed(
                    json!({"operation": operation_name, "asset_id": asset.asset_id, "media": unavailable}),
                    error.to_string(),
                ));
            }
        };
        let focus = focus
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(MAX_FOCUS_CHARS).collect::<String>());
        let role = router.vision_role().await;
        let started = std::time::Instant::now();
        let response = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            router.analyze_image(&bytes, &asset.media_type, system_prompt, focus.as_deref()),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return Ok(ToolResult::failed(
                    json!({"operation": operation_name, "asset_id": asset.asset_id, "media": unavailable, "available": false}),
                    format!("vision call failed: {error}"),
                ));
            }
            Err(_) => {
                let mut result = ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("vision call timed out after {}s", self.timeout_secs),
                );
                result.output = json!({
                    "operation": operation_name,
                    "asset_id": asset.asset_id,
                    "media": unavailable,
                });
                return Ok(result);
            }
        };
        let (text, text_truncated) = bound_text(response.text.trim(), self.max_output_chars);
        let output = json!({
            "operation": operation_name,
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(&asset, representation, Some(&text)),
            "representation": representation,
            "untrusted_content": true,
        });
        let mut result = if text_truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        };
        result.llm_usage.push(ToolLlmUsage {
            call_kind: "media",
            role,
            usage: response.usage,
            model: response.model,
            duration_ms: Some(started.elapsed().as_millis() as u64),
        });
        Ok(result)
    }

    async fn transcribe(
        &self,
        asset: ManagedAsset,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaModality::Audio {
            anyhow::bail!("transcribe requires an audio asset");
        }
        if !self.transcribe_available {
            return Ok(ToolResult::ok(json!({
                "operation": "transcribe",
                "asset_id": asset.asset_id,
                "media": self.model_media_reference(&asset, "managed_file_ref", None),
                "available": false,
                "reason": "No speech-to-text provider is configured.",
            })));
        }
        let Some(router_or_client) = self
            .stt_client
            .clone()
            .map(TranscriptionRoute::Dedicated)
            .or_else(|| self.router.clone().map(TranscriptionRoute::Router))
        else {
            return Ok(ToolResult::ok(json!({
                "operation": "transcribe",
                "asset_id": asset.asset_id,
                "media": self.model_media_reference(&asset, "managed_file_ref", None),
                "available": false,
                "reason": "No speech-to-text provider is configured.",
            })));
        };
        let bytes = self.read_bounded(&asset, &cancel).await?;
        let role = match &router_or_client {
            TranscriptionRoute::Dedicated(_) => None,
            TranscriptionRoute::Router(router) => router.stt_role().await,
        };
        let started = std::time::Instant::now();
        let result = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            transcribe_with_route(router_or_client, bytes),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                return Ok(ToolResult::failed(
                    json!({"operation": "transcribe", "asset_id": asset.asset_id, "media": self.model_media_reference(&asset, "managed_file_ref", None), "available": false}),
                    format!("transcription failed: {error}"),
                ));
            }
            Err(_) => {
                let mut tool_result = ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("transcription timed out after {}s", self.timeout_secs),
                );
                tool_result.output = json!({
                    "operation": "transcribe",
                    "asset_id": asset.asset_id,
                    "media": self.model_media_reference(&asset, "managed_file_ref", None),
                });
                return Ok(tool_result);
            }
        };
        let (text, text_truncated) = bound_text(result.text.trim(), self.max_output_chars);
        let output = json!({
            "operation": "transcribe",
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(&asset, "transcript", Some(&text)),
            "representation": "transcript",
            "untrusted_content": true,
        });
        let mut tool_result = if text_truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        };
        if let Some(role) = role
            && let Some(usage) = result.usage
        {
            tool_result.llm_usage.push(ToolLlmUsage {
                call_kind: "media",
                role,
                usage,
                model: result.model,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            });
        }
        Ok(tool_result)
    }

    async fn extract(
        &self,
        asset: ManagedAsset,
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
        let truncated = extracted.truncated;
        let representation = extracted.representation;
        let format = extracted.format;
        let text = extracted.text;
        let output = json!({
            "operation": "extract",
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(&asset, representation, Some(&text)),
            "representation": representation,
            "format": format.as_str(),
            "untrusted_content": true,
        });
        Ok(if truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        })
    }
}

enum TranscriptionRoute {
    Dedicated(Arc<dyn haven_llm::SttClient>),
    Router(Arc<LlmRouter>),
}

async fn transcribe_with_route(
    route: TranscriptionRoute,
    bytes: Vec<u8>,
) -> anyhow::Result<haven_llm::SttResult> {
    match route {
        TranscriptionRoute::Dedicated(client) => client.transcribe(&bytes).await,
        TranscriptionRoute::Router(router) => router
            .transcribe_audio(&bytes)
            .await
            .map_err(|error| anyhow::anyhow!(error)),
    }
}

fn bound_text(text: &str, max_chars: usize) -> (String, bool) {
    let bounded: String = text.chars().take(max_chars).collect();
    (bounded, text.chars().count() > max_chars)
}

fn operation_name(operation: MediaOperation) -> &'static str {
    match operation {
        MediaOperation::Inspect => "inspect",
        MediaOperation::Describe => "describe",
        MediaOperation::Ocr => "ocr",
        MediaOperation::Transcribe => "transcribe",
        MediaOperation::Extract => "extract",
    }
}

#[async_trait]
impl Tool for MediaTool {
    fn name(&self) -> String {
        "media".into()
    }

    fn description(&self) -> String {
        "Operate on managed multimodal assets by asset_id: inspect metadata, describe or OCR images with vision, transcribe audio, or extract text/tables from supported documents. Use the asset_id returned by attachments or other media-producing tools; host paths and base64 are never accepted.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("ocr") => RiskLevel::High,
            Some("describe") | Some("transcribe") => RiskLevel::Medium,
            Some("extract") | Some("inspect") => RiskLevel::Low,
            _ => RiskLevel::Low,
        }
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input["operation"].as_str() {
            Some("inspect") | Some("describe") | Some("ocr") | Some("transcribe")
            | Some("extract") => OperationIdempotency::Idempotent,
            _ => OperationIdempotency::Unknown,
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
        let mut schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operation": {"type": "string", "enum": ["inspect", "describe", "ocr", "transcribe", "extract"]},
                "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"},
                "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}
            },
            "required": ["operation", "asset_id"],
            "oneOf": [
                {"properties": {"operation": {"const": "inspect"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "describe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "ocr"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "transcribe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]},
                {"properties": {"operation": {"const": "extract"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]}
            ]
        });
        let unavailable = [
            ("describe", self.describe_available),
            ("ocr", self.describe_available),
            ("transcribe", self.transcribe_available),
        ];
        if let Some(operations) = schema["properties"]["operation"]
            .get_mut("enum")
            .and_then(Value::as_array_mut)
        {
            operations.retain(|operation| {
                unavailable
                    .iter()
                    .all(|(name, available)| operation.as_str() != Some(*name) || *available)
            });
        }
        if let Some(branches) = schema.get_mut("oneOf").and_then(Value::as_array_mut) {
            branches.retain(|branch| {
                unavailable.iter().all(|(name, available)| {
                    branch["properties"]["operation"]["const"].as_str() != Some(*name) || *available
                })
            });
        }
        schema
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

/// Register a host-selected file as a short-lived media source. This is the
/// producer half of `files.read` for rich files: the path is accepted only at
/// the trusted filesystem boundary and the model receives the resulting
/// opaque id instead.
pub(crate) fn register_path_asset(
    registry: &ManagedAssetRegistry,
    session_id: Option<&str>,
    path: &Path,
    media_type: &str,
    filename: Option<String>,
    size_bytes: u64,
) -> anyhow::Result<ManagedAsset> {
    let root = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("media source has no managed parent"))?;
    let asset_id = haven_common::types::new_id("asset");
    let expires_at = Utc::now() + ChronoDuration::seconds(GENERATED_MEDIA_RETENTION_SECS as i64);
    let registered = if let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) {
        registry.register_under_root_for_session_with_metadata(
            session_id,
            root,
            asset_id.clone(),
            path.to_path_buf(),
            filename,
            media_type.to_string(),
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    } else {
        registry.register_under_root_with_metadata(
            root,
            asset_id.clone(),
            path.to_path_buf(),
            filename,
            media_type.to_string(),
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    };
    if !registered {
        anyhow::bail!("failed to register media source");
    }
    registry
        .resolve(&asset_id)
        .ok_or_else(|| anyhow::anyhow!("media source disappeared after registration"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    struct DedicatedSttClient {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl haven_llm::SttClient for DedicatedSttClient {
        async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<haven_llm::SttResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(haven_llm::SttResult {
                text: "dedicated transcript".into(),
                confidence: None,
                usage: None,
                model: None,
            })
        }
    }

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
                "operation": "inspect",
                "asset_id": "asset-0123456789abcdef0123456789abcdef"
            }))
            .is_ok()
        );
        assert!(
            tool.validate_input(&json!({
                "operation": "describe",
                "asset_id": "asset-0123456789abcdef0123456789abcdef"
            }))
            .is_err()
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
    async fn inspect_returns_compact_media_reference_without_host_path() {
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

    #[test]
    fn model_media_reference_has_one_content_slot_and_no_runtime_metadata() {
        let root = TempDir::new().unwrap();
        let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
        let asset = registry.resolve(&asset_id).unwrap();
        let output = model_media_reference_with_capabilities(
            &asset,
            "image_description",
            Some("same text"),
            true,
            true,
        );
        let serialized = serde_json::to_string(&output).unwrap();
        assert_eq!(serialized.matches("same text").count(), 1);
        assert!(!serialized.contains("content_hash"));
        assert!(!serialized.contains("expires_at"));
        assert!(!serialized.contains("source"));
        assert!(!serialized.contains("provenance"));
    }

    #[tokio::test]
    async fn describe_without_router_is_explicitly_unavailable() {
        let root = TempDir::new().unwrap();
        let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
        let tool = MediaTool::new(None, registry, 1024, 10, 2_000);
        let result = tool
            .run(
                MediaParams {
                    operation: MediaOperation::Describe,
                    asset_id,
                    focus: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["available"], false);
        assert!(
            result.output["media"]["asset_id"]
                .as_str()
                .unwrap()
                .starts_with("asset-")
        );
    }

    #[tokio::test]
    async fn transcribe_prefers_dedicated_stt_without_router() {
        let root = TempDir::new().unwrap();
        let (registry, asset_id) = registered_asset(root.path(), "recording.wav", "audio/wav");
        let client = Arc::new(DedicatedSttClient {
            calls: AtomicUsize::new(0),
        });
        let tool = MediaTool::new(None, registry, 1024, 10, 2_000)
            .with_stt_client(Some(client.clone()))
            .with_capabilities(false, false);
        assert!(
            tool.input_schema()["properties"]["operation"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation == "transcribe")
        );

        let result = tool
            .run(
                MediaParams {
                    operation: MediaOperation::Transcribe,
                    asset_id,
                    focus: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["media"]["content"], "dedicated transcript");
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        assert!(result.llm_usage.is_empty());
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
        let serialized = serde_json::to_string(&model_media_reference_with_capabilities(
            &asset,
            "managed_file_ref",
            None,
            true,
            true,
        ))
        .unwrap();
        assert!(!serialized.contains(&path.to_string_lossy().to_string()));
        assert!(serialized.contains("screenshot.png"));
    }
}
