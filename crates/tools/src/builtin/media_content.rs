//! Provider-backed media interpretation and document extraction.

use haven_common::media::MediaModality;
use haven_common::prompts::{IMAGE_ANALYSIS_SYSTEM_PROMPT, OCR_SYSTEM_PROMPT};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::document::{
    MAX_DOCUMENT_BYTES, extract_document_page_with_cancel, supports_document_path,
};
use crate::{ManagedAsset, ToolLlmUsage, ToolResult};

use super::media_reference::{bound_text, classify_media, confidence_passes, operation_name};
use super::{MAX_FOCUS_CHARS, MediaOperation, MediaTool, model_media_reference_with_capabilities};

impl MediaTool {
    pub(super) fn model_media_reference(
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
        operation: &str,
        asset: &ManagedAsset,
        error: impl Into<String>,
    ) -> ToolResult {
        ToolResult::failed(
            json!({
                "operation": operation,
                "asset_id": asset.asset_id,
                "media": self.model_media_reference(asset, "managed_file_ref", None),
                "available": false,
            }),
            error,
        )
    }

    fn timed_out_media_result(&self, operation: &str, asset: &ManagedAsset) -> ToolResult {
        let mut result = ToolResult::timed_out(
            crate::ToolExecutionOutcome::TimedOutUnknown,
            format!("{operation} timed out after {}s", self.timeout_secs),
        );
        result.output = json!({
            "operation": operation,
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(asset, "managed_file_ref", None),
            "available": false,
        });
        result
    }

    pub(super) fn cancelled_media_result(
        &self,
        operation: &str,
        asset: &ManagedAsset,
        error: impl Into<String>,
    ) -> ToolResult {
        let mut result = ToolResult::cancelled(error);
        result.output = json!({
            "operation": operation,
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(asset, "managed_file_ref", None),
            "available": false,
        });
        result
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
            "image_description",
        )
        .await
    }

    pub(super) async fn ocr(
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
                    return Ok(if cancel.is_cancelled() {
                        self.cancelled_media_result("ocr", &asset, "OCR cancelled")
                    } else {
                        ToolResult::failed(
                            json!({
                                "operation": "ocr",
                                "asset_id": asset.asset_id,
                                "media": self.model_media_reference(&asset, "managed_file_ref", None),
                                "available": false,
                            }),
                            error.to_string(),
                        )
                    });
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
                    json!({
                        "operation": "ocr",
                        "asset_id": asset.asset_id,
                        "media": self.model_media_reference(&asset, "managed_file_ref", None),
                        "available": false,
                    }),
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
                return Ok(if cancel.is_cancelled() {
                    self.cancelled_media_result(operation_name, &asset, "vision request cancelled")
                } else {
                    ToolResult::failed(
                        json!({
                            "operation": operation_name,
                            "asset_id": asset.asset_id,
                            "media": unavailable,
                        }),
                        error.to_string(),
                    )
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
                    operation_name,
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

    pub(super) async fn transcribe(
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
        let bytes = match self.read_bounded(&asset, &cancel).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return Ok(if cancel.is_cancelled() {
                    self.cancelled_media_result("transcribe", &asset, "transcription cancelled")
                } else {
                    self.failed_media_result("transcribe", &asset, error.to_string())
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
                        "transcribe",
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
                            "transcribe",
                            &asset,
                            "STT provider returned no acceptable result and no LLM fallback is configured",
                        ));
                    };
                    let role = router.stt_role().await;
                    let result = match tokio::select! {
                        _ = cancel.cancelled() => {
                            return Ok(self.cancelled_media_result(
                                "transcribe",
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
                                "transcribe",
                                &asset,
                                format!("STT fallback failed: {error}"),
                            ));
                        }
                        Err(_) => return Ok(self.timed_out_media_result("transcribe", &asset)),
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
                        "transcribe",
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
                        "transcribe",
                        &asset,
                        format!("STT provider failed: {error}"),
                    ));
                }
                Err(_) => return Ok(self.timed_out_media_result("transcribe", &asset)),
            };
            (result, role)
        };
        let (text, text_truncated) = bound_text(result.text.trim(), self.max_output_chars);
        let output = json!({
            "operation": "transcribe",
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(&asset, "transcript", Some(&text)),
            "transcript": text,
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
}

impl MediaTool {
    pub(super) async fn extract(
        &self,
        asset: ManagedAsset,
        page_index: Option<u64>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if classify_media(&asset).0 != MediaModality::Document
            || !supports_document_path(&asset.path)
        {
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
        let representation = extracted.representation;
        let format = extracted.format;
        let text = extracted.text;
        let output = json!({
            "operation": "extract",
            "asset_id": asset.asset_id,
            "media": self.model_media_reference(&asset, representation, Some(&text)),
            "representation": representation,
            "format": format.as_str(),
            "page_index": extracted.page_index,
            "total_pages": extracted.total_pages,
            "next_page": extracted.next_page,
            "has_more": extracted.next_page.is_some(),
            "untrusted_content": true,
        });
        Ok(if truncated {
            ToolResult::truncated(output)
        } else {
            ToolResult::ok(output)
        })
    }
}
