//! Provider-backed media interpretation and document extraction.

use haven_common::media::MediaRepresentationKind;
use haven_common::media_detection::MediaType;
use haven_common::prompts::IMAGE_ANALYSIS_SYSTEM_PROMPT;
use haven_llm::{LlmRouter, SttClient};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::document::{
    MAX_DOCUMENT_BYTES, extract_document_page_with_cancel, supports_document_path,
};
use crate::{ManagedAsset, ToolLlmUsage, ToolResult};

use super::media_reference::{
    bound_text, classify_media, confidence_passes, media_result_envelope,
    media_result_envelope_named, operation_name,
};
use super::{MAX_FOCUS_CHARS, MediaOperation, MediaTool};

/// Result of the shared media transcription boundary. `available` is not
/// inferred from a provider error: an unavailable capability is a successful,
/// explicit outcome, while provider failures remain execution failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTranscriptionStatus {
    Succeeded,
    Empty,
    Unavailable,
    Failed,
    TimedOut,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct MediaTranscriptionResult {
    pub status: MediaTranscriptionStatus,
    pub text: Option<String>,
    pub truncated: bool,
    pub error: Option<String>,
    pub llm_usage: Vec<haven_llm::LlmCallUsage>,
}

impl MediaTranscriptionResult {
    pub(crate) fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            status: MediaTranscriptionStatus::Unavailable,
            text: None,
            truncated: false,
            error: Some(reason.into()),
            llm_usage: Vec::new(),
        }
    }

    fn cancelled(reason: impl Into<String>) -> Self {
        Self {
            status: MediaTranscriptionStatus::Cancelled,
            text: None,
            truncated: false,
            error: Some(reason.into()),
            llm_usage: Vec::new(),
        }
    }

    fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: MediaTranscriptionStatus::Failed,
            text: None,
            truncated: false,
            error: Some(reason.into()),
            llm_usage: Vec::new(),
        }
    }

    fn timed_out(reason: impl Into<String>) -> Self {
        Self {
            status: MediaTranscriptionStatus::TimedOut,
            text: None,
            truncated: false,
            error: Some(reason.into()),
            llm_usage: Vec::new(),
        }
    }

    fn empty() -> Self {
        Self {
            status: MediaTranscriptionStatus::Empty,
            text: None,
            truncated: false,
            error: None,
            llm_usage: Vec::new(),
        }
    }
}

/// One provider-independent STT policy used by both managed-asset
/// transcription and app voice ingress. The dedicated route is attempted
/// first; an unacceptable result can fall back to the LLM route once.
#[derive(Clone)]
pub(crate) struct MediaTranscriber {
    router: Option<Arc<LlmRouter>>,
    stt_client: Option<Arc<dyn SttClient>>,
    timeout_secs: u64,
    min_confidence: f32,
    max_output_chars: usize,
}

#[derive(Debug, Clone, Copy)]
enum DedicatedOutcome {
    Empty,
    Failed,
    TimedOut,
}

impl MediaTranscriber {
    pub(crate) fn new(
        router: Option<Arc<LlmRouter>>,
        stt_client: Option<Arc<dyn SttClient>>,
        timeout_secs: u64,
        min_confidence: f32,
        max_output_chars: usize,
    ) -> Self {
        Self {
            router,
            stt_client,
            timeout_secs: timeout_secs.max(1),
            min_confidence,
            max_output_chars: max_output_chars.max(1),
        }
    }

    pub(crate) fn with_stt_client(mut self, stt_client: Option<Arc<dyn SttClient>>) -> Self {
        self.stt_client = stt_client;
        self
    }

    pub(crate) fn with_min_confidence(mut self, min_confidence: f32) -> Self {
        self.min_confidence = min_confidence;
        self
    }

    pub(crate) fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs.max(1);
        self
    }

    pub(crate) fn has_stt_client(&self) -> bool {
        self.stt_client.is_some()
    }

    pub(crate) async fn transcribe_wav(
        &self,
        wav: &[u8],
        cancel: &CancellationToken,
    ) -> MediaTranscriptionResult {
        if self.stt_client.is_none() && self.router.is_none() {
            return MediaTranscriptionResult::unavailable(
                "No speech-to-text provider is configured.",
            );
        }

        if cancel.is_cancelled() {
            return MediaTranscriptionResult::cancelled("transcription cancelled");
        }

        let mut dedicated_outcome = None;
        if let Some(client) = self.stt_client.clone() {
            let dedicated = tokio::select! {
                _ = cancel.cancelled() => {
                    return MediaTranscriptionResult::cancelled("transcription cancelled");
                }
                result = tokio::time::timeout(
                    Duration::from_secs(self.timeout_secs),
                    client.transcribe(wav),
                ) => result,
            };
            match dedicated {
                Ok(Ok(result))
                    if !result.text.trim().is_empty()
                        && confidence_passes(result.confidence, self.min_confidence) =>
                {
                    return self.success(result.text, Vec::new());
                }
                Ok(Ok(result)) if result.text.trim().is_empty() => {
                    dedicated_outcome = Some(DedicatedOutcome::Empty);
                }
                Ok(Ok(_)) | Ok(Err(_)) => {
                    dedicated_outcome = Some(DedicatedOutcome::Failed);
                }
                Err(_) => {
                    dedicated_outcome = Some(DedicatedOutcome::TimedOut);
                }
            }
        }

        let Some(router) = self.router.clone() else {
            return self.result_without_fallback(dedicated_outcome);
        };
        if !router
            .is_request_configured(haven_common::config::RequestKind::Transcription)
            .await
        {
            return self.result_without_fallback(dedicated_outcome);
        }
        let started = std::time::Instant::now();
        let result = tokio::select! {
            _ = cancel.cancelled() => {
                return MediaTranscriptionResult::cancelled("transcription cancelled");
            }
            result = tokio::time::timeout(
                Duration::from_secs(self.timeout_secs),
                router.transcribe_audio(wav),
            ) => result,
        };
        let result = match result {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                return MediaTranscriptionResult::failed(format!("STT fallback failed: {error}"));
            }
            Err(_) => {
                return MediaTranscriptionResult::timed_out(format!(
                    "LLM STT fallback timed out after {}s",
                    self.timeout_secs
                ));
            }
        };
        let usage = result
            .usage
            .map(|usage| haven_llm::LlmCallUsage {
                request: haven_common::config::RequestKind::Transcription,
                usage,
                model: result.model.clone(),
                duration_ms: Some(started.elapsed().as_millis() as u64),
            })
            .into_iter()
            .collect();
        if result.text.trim().is_empty() {
            MediaTranscriptionResult::empty()
        } else {
            self.success(result.text, usage)
        }
    }

    fn result_without_fallback(
        &self,
        dedicated_outcome: Option<DedicatedOutcome>,
    ) -> MediaTranscriptionResult {
        match dedicated_outcome {
            Some(DedicatedOutcome::Empty) => MediaTranscriptionResult::empty(),
            Some(DedicatedOutcome::TimedOut) => MediaTranscriptionResult::timed_out(format!(
                "dedicated STT provider timed out after {}s",
                self.timeout_secs
            )),
            Some(DedicatedOutcome::Failed) => MediaTranscriptionResult::failed(
                "STT provider returned no acceptable result and no LLM fallback is configured",
            ),
            None => {
                MediaTranscriptionResult::unavailable("No speech-to-text provider is configured.")
            }
        }
    }

    fn success(
        &self,
        text: impl Into<String>,
        llm_usage: Vec<haven_llm::LlmCallUsage>,
    ) -> MediaTranscriptionResult {
        let text = text.into();
        let (text, truncated) = bound_text(text.trim(), self.max_output_chars);
        if text.is_empty() {
            return MediaTranscriptionResult::empty();
        }
        MediaTranscriptionResult {
            status: MediaTranscriptionStatus::Succeeded,
            text: Some(text),
            truncated,
            error: None,
            llm_usage,
        }
    }
}

impl MediaTool {
    /// Render one bounded document page through the existing document parser.
    /// This deliberately returns a managed, untrusted page representation; a
    /// future native PDF renderer can add an image representation without
    /// creating a second document ingestion path.
    pub(super) async fn render(
        &self,
        asset: ManagedAsset,
        page_index: Option<u64>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let mut result = self.extract(asset, page_index, cancel).await?;
        result.output["operation"] = Value::String("render".into());
        result.output["rendered"] = Value::Bool(true);
        result.output["representation"] =
            Value::String(MediaRepresentationKind::DocumentPages.as_str().into());
        Ok(result)
    }

    pub(crate) fn media_result_output(
        &self,
        operation: MediaOperation,
        asset: Option<&ManagedAsset>,
        representation: Option<MediaRepresentationKind>,
        content: Option<&str>,
    ) -> Value {
        media_result_envelope(operation, asset, representation, content, self.capabilities)
    }

    /// Same typed envelope for producer operations outside the flat media
    /// operation enum, such as `window.screenshot`.
    pub(crate) fn media_result_output_named(
        &self,
        operation: impl Into<String>,
        asset: Option<&ManagedAsset>,
        representation: Option<MediaRepresentationKind>,
        content: Option<&str>,
    ) -> Value {
        media_result_envelope_named(operation, asset, representation, content, self.capabilities)
    }

    /// Shared STT consumer for model-facing `media.transcribe` and the
    /// capture half of `media.record`. The latter owns capture, but it must not own a second
    /// timeout/fallback/confidence policy.
    pub(crate) async fn transcribe_asset(
        &self,
        asset: ManagedAsset,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.transcribe(asset, cancel).await
    }

    pub(crate) fn failed_media_result(
        &self,
        operation: MediaOperation,
        asset: &ManagedAsset,
        error: impl Into<String>,
    ) -> ToolResult {
        let mut output = self.media_result_output(
            operation,
            Some(asset),
            Some(MediaRepresentationKind::ManagedFileRef),
            None,
        );
        // The route existed but its provider failed. Keep `available=true` so
        // execution failure is not mistaken for a missing capability.
        output["available"] = Value::Bool(true);
        ToolResult::failed(output, error)
    }

    fn timed_out_media_result(
        &self,
        operation: MediaOperation,
        asset: &ManagedAsset,
    ) -> ToolResult {
        let mut result = ToolResult::timed_out(
            crate::ToolExecutionOutcome::TimedOutUnknown,
            format!(
                "{} timed out after {}s",
                operation_name(operation),
                self.timeout_secs
            ),
        );
        result.output = self.media_result_output(
            operation,
            Some(asset),
            Some(MediaRepresentationKind::ManagedFileRef),
            None,
        );
        result.output["available"] = Value::Bool(true);
        result
    }

    pub(super) fn cancelled_media_result(
        &self,
        operation: MediaOperation,
        asset: &ManagedAsset,
        error: impl Into<String>,
    ) -> ToolResult {
        let mut result = ToolResult::cancelled(error);
        result.output = self.media_result_output(
            operation,
            Some(asset),
            Some(MediaRepresentationKind::ManagedFileRef),
            None,
        );
        result.output["available"] = Value::Bool(true);
        result
    }

    fn unavailable_media_result(
        &self,
        operation: MediaOperation,
        asset: &ManagedAsset,
        reason: impl Into<String>,
    ) -> ToolResult {
        let mut output = self.media_result_output(
            operation,
            Some(asset),
            Some(MediaRepresentationKind::ManagedFileRef),
            None,
        );
        output["available"] = Value::Bool(false);
        output["capability"] = Value::String(operation_name(operation).to_owned());
        output["reason_code"] = Value::String(format!("{}_unavailable", operation_name(operation)));
        output["reason"] = Value::String(reason.into());
        ToolResult::ok(output)
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

    pub(super) async fn describe(
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
            MediaRepresentationKind::ImageDescription,
        )
        .await
    }

    pub(super) async fn ocr(
        &self,
        asset: ManagedAsset,
        _focus: Option<String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if !self.capabilities.ocr || self.ocr_client.is_none() {
            return Ok(self.unavailable_media_result(
                MediaOperation::Ocr,
                &asset,
                "No dedicated OCR provider is configured.",
            ));
        }
        let client = self
            .ocr_client
            .clone()
            .expect("ocr_available implies an OCR client");
        let bytes = match self.read_bounded(&asset, &cancel).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(if cancel.is_cancelled() {
                    self.cancelled_media_result(MediaOperation::Ocr, &asset, "OCR cancelled")
                } else {
                    self.failed_media_result(MediaOperation::Ocr, &asset, error.to_string())
                });
            }
        };
        let dedicated = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            client.recognize(&bytes, &asset.media_type),
        )
        .await;
        match dedicated {
            Ok(Ok(response))
                if !response.text.trim().is_empty()
                    && confidence_passes(response.confidence, self.ocr_min_confidence) =>
            {
                let (text, text_truncated) =
                    bound_text(response.text.trim(), self.max_output_chars);
                let mut output = self.media_result_output(
                    MediaOperation::Ocr,
                    Some(&asset),
                    Some(MediaRepresentationKind::OcrText),
                    Some(&text),
                );
                output["untrusted_content"] = Value::Bool(true);
                Ok(if text_truncated {
                    ToolResult::truncated(output)
                } else {
                    ToolResult::ok(output)
                })
            }
            Ok(Ok(_)) => Ok(self.failed_media_result(
                MediaOperation::Ocr,
                &asset,
                "OCR provider returned no acceptable result",
            )),
            Ok(Err(error)) => Ok(self.failed_media_result(
                MediaOperation::Ocr,
                &asset,
                format!("OCR call failed: {error}"),
            )),
            Err(_) => Ok(self.timed_out_media_result(MediaOperation::Ocr, &asset)),
        }
    }

    async fn derive_image(
        &self,
        asset: ManagedAsset,
        focus: Option<String>,
        cancel: CancellationToken,
        operation: MediaOperation,
        system_prompt: &str,
        representation: MediaRepresentationKind,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaType::Image {
            anyhow::bail!("{} requires an image asset", operation_name(operation));
        }
        let available = match operation {
            MediaOperation::Ocr => self.capabilities.ocr,
            _ => self.capabilities.describe,
        };
        if !available {
            return Ok(self.unavailable_media_result(
                operation,
                &asset,
                "No vision-capable LLM router is configured.",
            ));
        }
        let Some(router) = self.router.clone() else {
            return Ok(self.unavailable_media_result(
                operation,
                &asset,
                "No vision-capable LLM router is configured.",
            ));
        };
        let bytes = match self.read_bounded(&asset, &cancel).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(if cancel.is_cancelled() {
                    self.cancelled_media_result(operation, &asset, "vision request cancelled")
                } else {
                    self.failed_media_result(operation, &asset, error.to_string())
                });
            }
        };
        let focus = focus
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.chars().take(MAX_FOCUS_CHARS).collect::<String>());
        let started = std::time::Instant::now();
        let response = match tokio::select! {
            _ = cancel.cancelled() => {
                return Ok(self.cancelled_media_result(
                    operation,
                    &asset,
                    "vision request cancelled",
                ));
            }
            response = tokio::time::timeout(
                Duration::from_secs(self.timeout_secs),
                router.analyze_image(&bytes, &asset.media_type, system_prompt, focus.as_deref()),
            ) => response,
        } {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                return Ok(self.failed_media_result(
                    operation,
                    &asset,
                    format!("vision call failed: {error}"),
                ));
            }
            Err(_) => {
                let mut result = ToolResult::timed_out(
                    crate::ToolExecutionOutcome::TimedOutUnknown,
                    format!("vision call timed out after {}s", self.timeout_secs),
                );
                result.output = self.media_result_output(
                    operation,
                    Some(&asset),
                    Some(MediaRepresentationKind::ManagedFileRef),
                    None,
                );
                return Ok(result);
            }
        };
        let (text, text_truncated) = bound_text(response.text.trim(), self.max_output_chars);
        let mut output =
            self.media_result_output(operation, Some(&asset), Some(representation), Some(&text));
        output["untrusted_content"] = Value::Bool(true);
        let mut result = if text_truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        };
        result.llm_usage.push(ToolLlmUsage {
            call_kind: "media",
            request: haven_common::config::RequestKind::Vision,
            usage: response.usage,
            model: response.model,
            duration_ms: Some(started.elapsed().as_millis() as u64),
        });
        Ok(result)
    }

    pub(super) async fn transcribe(
        &self,
        asset: ManagedAsset,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaType::Audio {
            anyhow::bail!("transcribe requires an audio asset");
        }
        if !self.capabilities.transcribe {
            return Ok(self.unavailable_media_result(
                MediaOperation::Transcribe,
                &asset,
                "No speech-to-text provider is configured.",
            ));
        }
        let bytes = match self.read_bounded(&asset, &cancel).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(if cancel.is_cancelled() {
                    self.cancelled_media_result(
                        MediaOperation::Transcribe,
                        &asset,
                        "transcription cancelled",
                    )
                } else {
                    self.failed_media_result(MediaOperation::Transcribe, &asset, error.to_string())
                });
            }
        };
        let transcription = self.transcriber.transcribe_wav(&bytes, &cancel).await;
        match transcription.status {
            MediaTranscriptionStatus::Succeeded => {
                let text = transcription
                    .text
                    .expect("successful transcription has text");
                let mut output = self.media_result_output(
                    MediaOperation::Transcribe,
                    Some(&asset),
                    Some(MediaRepresentationKind::Transcript),
                    Some(&text),
                );
                output["transcript"] = Value::String(text);
                output["untrusted_content"] = Value::Bool(true);
                let mut tool_result = if transcription.truncated {
                    ToolResult::truncated(output)
                } else {
                    ToolResult::ok(output)
                };
                for usage in transcription.llm_usage {
                    tool_result.llm_usage.push(ToolLlmUsage {
                        call_kind: "media",
                        request: usage.request,
                        usage: usage.usage,
                        model: usage.model,
                        duration_ms: usage.duration_ms,
                    });
                }
                Ok(tool_result)
            }
            MediaTranscriptionStatus::Empty => {
                let mut output = self.media_result_output(
                    MediaOperation::Transcribe,
                    Some(&asset),
                    Some(MediaRepresentationKind::Transcript),
                    Some(""),
                );
                output["transcript"] = Value::String(String::new());
                output["untrusted_content"] = Value::Bool(true);
                Ok(ToolResult::ok(output))
            }
            MediaTranscriptionStatus::Unavailable => Ok(self.unavailable_media_result(
                MediaOperation::Transcribe,
                &asset,
                transcription
                    .error
                    .unwrap_or_else(|| "No speech-to-text provider is configured.".into()),
            )),
            MediaTranscriptionStatus::Cancelled => Ok(self.cancelled_media_result(
                MediaOperation::Transcribe,
                &asset,
                transcription
                    .error
                    .unwrap_or_else(|| "transcription cancelled".into()),
            )),
            MediaTranscriptionStatus::TimedOut => {
                Ok(self.timed_out_media_result(MediaOperation::Transcribe, &asset))
            }
            MediaTranscriptionStatus::Failed => Ok(self.failed_media_result(
                MediaOperation::Transcribe,
                &asset,
                transcription
                    .error
                    .unwrap_or_else(|| "STT provider failed".into()),
            )),
        }
    }
}

impl MediaTool {
    pub(super) async fn extract(
        &self,
        asset: ManagedAsset,
        page_index: Option<u64>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaType::Document || !supports_document_path(&asset.path) {
            anyhow::bail!("extract requires a supported PDF, DOCX, XLSX, or PPTX asset");
        }
        let path = asset.path.clone();
        let max_chars = self.max_output_chars.min(100_000);
        let page_index = page_index.unwrap_or(0);
        let page_index =
            usize::try_from(page_index).map_err(|_| anyhow::anyhow!("page_index is too large"))?;
        let extracted = tokio::task::spawn_blocking(move || {
            extract_document_page_with_cancel(
                &path,
                max_chars,
                MAX_DOCUMENT_BYTES,
                page_index,
                &cancel,
            )
        })
        .await??;
        let truncated = extracted.truncated;
        let representation = MediaRepresentationKind::parse(extracted.representation)
            .ok_or_else(|| anyhow::anyhow!("unknown document representation"))?;
        let format = extracted.format;
        let text = extracted.text;
        let mut output = self.media_result_output(
            MediaOperation::Extract,
            Some(&asset),
            Some(representation),
            Some(&text),
        );
        output["format"] = Value::String(format.as_str().to_owned());
        output["page_index"] = serde_json::json!(extracted.page_index);
        output["total_pages"] = serde_json::json!(extracted.total_pages);
        output["next_page"] = serde_json::json!(extracted.next_page);
        output["has_more"] = Value::Bool(extracted.next_page.is_some());
        output["untrusted_content"] = Value::Bool(true);
        Ok(if truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        })
    }
}
