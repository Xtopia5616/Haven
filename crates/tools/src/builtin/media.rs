//! Agent-native media operations.
//!
//! The model-facing boundary is deliberately small: callers pass an opaque
//! `asset_id`, select a media operation, and receive a compact media reference
//! containing the selected representation and optional derived content. Host
//! paths and raw bytes stay inside this module; durable attachment metadata is
//! owned by `haven_common::media`.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use haven_common::config::{GENERATED_MEDIA_RETENTION_SECS, default_generated_media_dir};
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
const MAX_GENERATION_PROMPT_CHARS: usize = 4_000;
const MAX_GENERATED_MEDIA_BYTES: usize = 16 * 1024 * 1024;

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
    ocr_available: bool,
    transcribe_available: bool,
) -> Value {
    let (modality, file_kind) = classify_media(asset);
    let mut available_representations = vec!["managed_file_ref"];
    match modality {
        MediaModality::Image => {
            if describe_available {
                available_representations.push("image_description");
            }
            if ocr_available {
                available_representations.push("ocr_text");
            }
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
        ("managed_file_ref", MediaModality::Image) if ocr_available => Some("media.ocr"),
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
    Generate,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct MediaParams {
    pub operation: MediaOperation,
    #[serde(default)]
    pub asset_id: Option<String>,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(rename = "_session_id", default, skip_serializing)]
    pub(crate) session_id: Option<String>,
}

pub struct MediaTool {
    router: Option<Arc<LlmRouter>>,
    stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    describe_available: bool,
    ocr_available: bool,
    transcribe_available: bool,
    generate_available: bool,
    ocr_min_confidence: f32,
    stt_min_confidence: f32,
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
            ocr_client: None,
            image_gen_client: None,
            describe_available: has_router,
            ocr_available: has_router,
            transcribe_available: has_router,
            generate_available: false,
            ocr_min_confidence: 0.0,
            stt_min_confidence: 0.0,
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
        self.ocr_available = self.ocr_available || describe_available;
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

    pub(crate) fn with_ocr_client(
        mut self,
        ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    ) -> Self {
        if ocr_client.is_some() {
            self.ocr_available = true;
        }
        self.ocr_client = ocr_client;
        self
    }

    pub(crate) fn with_image_gen_client(
        mut self,
        image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    ) -> Self {
        self.generate_available = image_gen_client.is_some();
        self.image_gen_client = image_gen_client;
        self
    }

    pub(crate) fn with_confidence_thresholds(
        mut self,
        ocr_min_confidence: f32,
        stt_min_confidence: f32,
    ) -> Self {
        self.ocr_min_confidence = ocr_min_confidence;
        self.stt_min_confidence = stt_min_confidence;
        self
    }

    pub async fn run(
        &self,
        params: MediaParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if params.operation == MediaOperation::Generate {
            return self.generate(params, cancel).await;
        }
        let asset_id = params
            .asset_id
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
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
            MediaOperation::Generate => unreachable!("generate handled before asset resolution"),
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
            self.ocr_available,
            self.transcribe_available,
        )
    }

    pub(crate) fn managed_media_reference(&self, asset: &ManagedAsset) -> Value {
        self.model_media_reference(asset, "managed_file_ref", None)
    }

    pub(crate) fn ocr_available(&self) -> bool {
        self.ocr_available
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
        if !self.ocr_available {
            return Ok(ToolResult::ok(json!({
                "operation": "ocr",
                "asset_id": asset.asset_id,
                "media": self.model_media_reference(&asset, "managed_file_ref", None),
                "available": false,
                "reason": "No OCR or vision-capable LLM provider is configured.",
            })));
        }
        if let Some(client) = self.ocr_client.clone() {
            let bytes = match self.read_bounded(&asset, &cancel).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return Ok(ToolResult::failed(
                        json!({"operation": "ocr", "asset_id": asset.asset_id}),
                        error.to_string(),
                    ));
                }
            };
            let dedicated = tokio::time::timeout(
                Duration::from_secs(self.timeout_secs),
                client.recognize(&bytes, &asset.media_type),
            )
            .await;
            if let Ok(Ok(response)) = dedicated
                && !response.text.trim().is_empty()
                && confidence_passes(response.confidence, self.ocr_min_confidence)
            {
                let (text, text_truncated) =
                    bound_text(response.text.trim(), self.max_output_chars);
                let output = json!({
                    "operation": "ocr",
                    "asset_id": asset.asset_id,
                    "media": self.model_media_reference(&asset, "ocr_text", Some(&text)),
                    "representation": "ocr_text",
                    "untrusted_content": true,
                });
                return Ok(if text_truncated {
                    ToolResult::truncated(output)
                } else {
                    ToolResult::ok(output)
                });
            }
            if self.router.is_none() {
                return Ok(ToolResult::failed(
                    json!({"operation": "ocr", "asset_id": asset.asset_id}),
                    "OCR provider returned no acceptable result and no LLM fallback is configured",
                ));
            }
        }
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
        let available = match operation {
            MediaOperation::Ocr => self.ocr_available,
            _ => self.describe_available,
        };
        if !available {
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
        if self.stt_client.is_none() && self.router.is_none() {
            return Ok(ToolResult::ok(json!({
                "operation": "transcribe",
                "asset_id": asset.asset_id,
                "media": self.model_media_reference(&asset, "managed_file_ref", None),
                "available": false,
                "reason": "No speech-to-text provider is configured.",
            })));
        };
        let bytes = self.read_bounded(&asset, &cancel).await?;
        let dedicated = self.stt_client.clone();
        let router = self.router.clone();
        let started = std::time::Instant::now();
        let (result, role) = if let Some(client) = dedicated {
            match tokio::time::timeout(
                Duration::from_secs(self.timeout_secs),
                client.transcribe(&bytes),
            )
            .await
            {
                Ok(Ok(result))
                    if !result.text.trim().is_empty()
                        && confidence_passes(result.confidence, self.stt_min_confidence) =>
                {
                    (result, None)
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                    let Some(router) = router else {
                        return Ok(ToolResult::failed(
                            json!({"operation": "transcribe", "asset_id": asset.asset_id}),
                            "STT provider returned no acceptable result and no LLM fallback is configured",
                        ));
                    };
                    let role = router.stt_role().await;
                    let result = match tokio::time::timeout(
                        Duration::from_secs(self.timeout_secs),
                        router.transcribe_audio(&bytes),
                    )
                    .await
                    {
                        Ok(Ok(result)) => result,
                        Ok(Err(error)) => return Err(anyhow::anyhow!(error)),
                        Err(_) => {
                            anyhow::bail!("transcription timed out after {}s", self.timeout_secs)
                        }
                    };
                    (result, role)
                }
            }
        } else {
            let Some(router) = router else {
                unreachable!("availability checked above")
            };
            let role = router.stt_role().await;
            let result = match tokio::time::timeout(
                Duration::from_secs(self.timeout_secs),
                router.transcribe_audio(&bytes),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => return Err(anyhow::anyhow!(error)),
                Err(_) => anyhow::bail!("transcription timed out after {}s", self.timeout_secs),
            };
            (result, role)
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

    async fn generate(
        &self,
        params: MediaParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let prompt = params
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .map(|prompt| {
                prompt
                    .chars()
                    .take(MAX_GENERATION_PROMPT_CHARS)
                    .collect::<String>()
            })
            .ok_or_else(|| anyhow::anyhow!("prompt is required"))?;
        let Some(client) = self.image_gen_client.clone() else {
            return Ok(ToolResult::ok(json!({
                "operation": "generate",
                "available": false,
                "reason": "No image-generation provider is configured.",
            })));
        };
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let image = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            client.generate(&prompt),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!("image generation timed out after {}s", self.timeout_secs)
        })??;
        if image.data.is_empty() {
            anyhow::bail!("image generation returned empty media");
        }
        if image.data.len() > MAX_GENERATED_MEDIA_BYTES {
            anyhow::bail!("image generation output exceeds the media size limit");
        }
        if !image.media_type.starts_with("image/") {
            anyhow::bail!("image generation returned a non-image media type");
        }
        let root = default_generated_media_dir();
        tokio::fs::create_dir_all(&root).await?;
        let extension = haven_llm::media::extension_for_media_type(&image.media_type);
        let path = root.join(format!(
            "{}.{}",
            haven_common::types::new_id("file"),
            extension
        ));
        let write_path = path.clone();
        let bytes = image.data;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&write_path)?;
            if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                drop(file);
                let _ = std::fs::remove_file(&write_path);
                return Err(error.into());
            }
            Ok(())
        })
        .await??;
        let size = tokio::fs::metadata(&path).await?.len();
        let asset = match register_generated_asset(
            &self.managed_assets,
            params.session_id.as_deref(),
            &root,
            path.clone(),
            Some(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ),
            &image.media_type,
            size,
        ) {
            Ok(asset) => asset,
            Err(error) => {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(error);
            }
        };
        Ok(ToolResult::ok(json!({
            "operation": "generate",
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(&asset, "generated_image", None),
            "representation": "generated_image",
        })))
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
        MediaOperation::Generate => "generate",
    }
}

fn confidence_passes(reported: Option<f32>, threshold: f32) -> bool {
    reported
        .map(|confidence| confidence >= threshold)
        .unwrap_or(true)
}

#[async_trait]
impl Tool for MediaTool {
    fn name(&self) -> String {
        "media".into()
    }

    fn description(&self) -> String {
        "Operate on managed multimodal assets by asset_id: inspect metadata, describe or OCR images, transcribe audio, or extract text/tables from documents; generate images with an explicit prompt. Use asset_id from attachments or other media-producing tools; host paths and base64 are never accepted.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("ocr") => RiskLevel::High,
            Some("describe") | Some("transcribe") => RiskLevel::Medium,
            Some("extract") | Some("inspect") => RiskLevel::Low,
            Some("generate") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input["operation"].as_str() {
            Some("inspect") | Some("describe") | Some("ocr") | Some("transcribe")
            | Some("extract") => OperationIdempotency::Idempotent,
            Some("generate") => OperationIdempotency::NonIdempotent,
            _ => OperationIdempotency::Unknown,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        let resource = input["asset_id"]
            .as_str()
            .or_else(|| input["prompt"].as_str())
            .unwrap_or("unknown");
        ToolConcurrency::Resource(format!("media:{resource}"))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.timeout_secs.max(30)
    }

    fn input_schema(&self) -> Value {
        let mut schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operation": {"type": "string", "enum": ["inspect", "describe", "ocr", "transcribe", "extract", "generate"]},
                "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"},
                "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS},
                "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_GENERATION_PROMPT_CHARS}
            },
            "required": ["operation"],
            "oneOf": [
                {"properties": {"operation": {"const": "inspect"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"], "not": {"required": ["prompt"]}},
                {"properties": {"operation": {"const": "describe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"], "not": {"required": ["prompt"]}},
                {"properties": {"operation": {"const": "ocr"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"], "not": {"required": ["prompt"]}},
                {"properties": {"operation": {"const": "transcribe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"], "not": {"required": ["prompt"]}},
                {"properties": {"operation": {"const": "extract"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"], "not": {"required": ["prompt"]}},
                {"properties": {"operation": {"const": "generate"}, "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_GENERATION_PROMPT_CHARS}}, "required": ["operation", "prompt"], "not": {"required": ["asset_id"]}}
            ]
        });
        let unavailable = [
            ("describe", self.describe_available),
            ("ocr", self.ocr_available),
            ("transcribe", self.transcribe_available),
            ("generate", self.generate_available),
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

    struct DedicatedOcrClient;

    #[async_trait]
    impl haven_llm::OcrClient for DedicatedOcrClient {
        async fn recognize(
            &self,
            _image_bytes: &[u8],
            _media_type: &str,
        ) -> anyhow::Result<haven_llm::OcrResult> {
            Ok(haven_llm::OcrResult {
                text: "dedicated OCR".into(),
                confidence: Some(0.99),
            })
        }
    }

    struct DummyImageGenClient;

    #[async_trait]
    impl haven_llm::ImageGenClient for DummyImageGenClient {
        async fn generate(&self, _prompt: &str) -> anyhow::Result<haven_llm::GeneratedImage> {
            Ok(haven_llm::GeneratedImage {
                media_type: "image/png".into(),
                data: b"png".to_vec(),
            })
        }
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
                    asset_id: Some(asset_id),
                    focus: None,
                    prompt: None,
                    session_id: None,
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
                    asset_id: Some(asset_id),
                    focus: None,
                    prompt: None,
                    session_id: None,
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

    #[tokio::test]
    async fn ocr_prefers_dedicated_provider_without_router() {
        let root = TempDir::new().unwrap();
        let (registry, asset_id) = registered_asset(root.path(), "photo.png", "image/png");
        let tool = MediaTool::new(None, registry, 1024, 10, 2_000)
            .with_ocr_client(Some(Arc::new(DedicatedOcrClient)))
            .with_capabilities(false, false);
        let result = tool
            .run(
                MediaParams {
                    operation: MediaOperation::Ocr,
                    asset_id: Some(asset_id),
                    focus: None,
                    prompt: None,
                    session_id: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output["media"]["content"], "dedicated OCR");
        assert!(result.llm_usage.is_empty());
    }

    #[test]
    fn generation_is_explicit_and_capability_pruned() {
        let unavailable = MediaTool::new(None, ManagedAssetRegistry::default(), 1024, 10, 2_000);
        assert!(
            !unavailable.input_schema()["properties"]["operation"]["enum"]
                .as_array()
                .unwrap()
                .iter()
                .any(|operation| operation == "generate")
        );

        let available = unavailable.with_image_gen_client(Some(Arc::new(DummyImageGenClient)));
        assert!(
            available
                .validate_input(&json!({
                    "operation": "generate",
                    "prompt": "a red fox in watercolor"
                }))
                .is_ok()
        );
        assert!(
            available
                .validate_input(&json!({
                    "operation": "generate",
                    "prompt": "a red fox in watercolor",
                    "asset_id": "asset-0123456789abcdef0123456789abcdef"
                }))
                .is_err()
        );
        assert!(
            available
                .validate_input(&json!({
                    "operation": "inspect",
                    "asset_id": "asset-0123456789abcdef0123456789abcdef",
                    "prompt": "unexpected"
                }))
                .is_err()
        );
        assert!(
            available
                .validate_input(&json!({"operation": "generate"}))
                .is_err()
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
        let serialized = serde_json::to_string(&model_media_reference_with_capabilities(
            &asset,
            "managed_file_ref",
            None,
            true,
            true,
            true,
        ))
        .unwrap();
        assert!(!serialized.contains(&path.to_string_lossy().to_string()));
        assert!(serialized.contains("screenshot.png"));
    }
}
