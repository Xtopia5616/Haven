//! Provider-backed media interpretation and document extraction.

use haven_common::media::MediaRepresentationKind;
use haven_common::media_detection::MediaType;
use haven_common::prompts::IMAGE_ANALYSIS_SYSTEM_PROMPT;
use serde_json::Value;
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
        media_result_envelope(
            operation,
            asset,
            representation,
            content,
            self.describe_available,
            self.ocr_available,
            self.transcribe_available,
        )
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
        media_result_envelope_named(
            operation,
            asset,
            representation,
            content,
            self.describe_available,
            self.ocr_available,
            self.transcribe_available,
        )
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

    fn failed_media_result(
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
        output["available"] = Value::Bool(false);
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
        result.output["available"] = Value::Bool(false);
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
        result.output["available"] = Value::Bool(false);
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
        output["reason"] = Value::String(reason.into());
        ToolResult::ok(output)
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
        if !self.ocr_available || self.ocr_client.is_none() {
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
            MediaOperation::Ocr => self.ocr_available,
            _ => self.describe_available,
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
        let role = router.vision_role().await;
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
            role,
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
        if !self.transcribe_available {
            return Ok(self.unavailable_media_result(
                MediaOperation::Transcribe,
                &asset,
                "No speech-to-text provider is configured.",
            ));
        }
        if self.stt_client.is_none() && self.router.is_none() {
            return Ok(self.unavailable_media_result(
                MediaOperation::Transcribe,
                &asset,
                "No speech-to-text provider is configured.",
            ));
        };
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
        let dedicated = self.stt_client.clone();
        let router = self.router.clone();
        let started = std::time::Instant::now();
        let (result, role) = if let Some(client) = dedicated {
            let dedicated = tokio::select! {
                _ = cancel.cancelled() => {
                    return Ok(self.cancelled_media_result(
                        MediaOperation::Transcribe,
                        &asset,
                        "transcription cancelled",
                    ));
                }
                result = tokio::time::timeout(
                    Duration::from_secs(self.timeout_secs),
                    client.transcribe(&bytes),
                ) => result,
            };
            match dedicated {
                Ok(Ok(result))
                    if !result.text.trim().is_empty()
                        && confidence_passes(result.confidence, self.stt_min_confidence) =>
                {
                    (result, None)
                }
                Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                    let Some(router) = router else {
                        return Ok(self.failed_media_result(
                            MediaOperation::Transcribe,
                            &asset,
                            "STT provider returned no acceptable result and no LLM fallback is configured",
                        ));
                    };
                    let role = router.stt_role().await;
                    let result = match tokio::select! {
                        _ = cancel.cancelled() => {
                            return Ok(self.cancelled_media_result(
                                MediaOperation::Transcribe,
                                &asset,
                                "transcription cancelled",
                            ));
                        }
                        result = tokio::time::timeout(
                            Duration::from_secs(self.timeout_secs),
                            router.transcribe_audio(&bytes),
                        ) => result,
                    } {
                        Ok(Ok(result)) => result,
                        Ok(Err(error)) => {
                            return Ok(self.failed_media_result(
                                MediaOperation::Transcribe,
                                &asset,
                                format!("STT fallback failed: {error}"),
                            ));
                        }
                        Err(_) => {
                            return Ok(
                                self.timed_out_media_result(MediaOperation::Transcribe, &asset)
                            );
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
            let result = match tokio::select! {
                _ = cancel.cancelled() => {
                    return Ok(self.cancelled_media_result(
                        MediaOperation::Transcribe,
                        &asset,
                        "transcription cancelled",
                    ));
                }
                result = tokio::time::timeout(
                    Duration::from_secs(self.timeout_secs),
                    router.transcribe_audio(&bytes),
                ) => result,
            } {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => {
                    return Ok(self.failed_media_result(
                        MediaOperation::Transcribe,
                        &asset,
                        format!("STT provider failed: {error}"),
                    ));
                }
                Err(_) => {
                    return Ok(self.timed_out_media_result(MediaOperation::Transcribe, &asset));
                }
            };
            (result, role)
        };
        let (text, text_truncated) = bound_text(result.text.trim(), self.max_output_chars);
        let mut output = self.media_result_output(
            MediaOperation::Transcribe,
            Some(&asset),
            Some(MediaRepresentationKind::Transcript),
            Some(&text),
        );
        output["transcript"] = Value::String(text);
        output["untrusted_content"] = Value::Bool(true);
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
