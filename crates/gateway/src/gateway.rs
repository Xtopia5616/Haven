//! Stage 4: the gateway orchestrator.
//!
//! [`MediaGateway`] owns the routing pipeline over the `haven-llm` router
//! and the dedicated media clients (STT / OCR / TTS / image generation):
//!
//! - [`MediaGateway::process_attachment`] — for a binary attachment: detect
//!   modality, classify intent, run the coverage action. Extraction actions
//!   run through the dedicated provider with a confidence gate; a result
//!   below `min_confidence` (or an error / empty result) falls back to the
//!   main model, which is called directly with the media as a content part.
//! - [`MediaGateway::process_generate`] — pure-text generate requests
//!   (TTS / text-to-image), saving the generated file under the app data
//!   media directory.
//!
//! Everything is in-process: there is no separate HTTP service, the agent
//! calls these methods while building the user message.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use haven_common::config::MediaConfig;
use haven_common::prompts::{OCR_SYSTEM_PROMPT, TRANSCRIBE_SYSTEM_PROMPT};
use haven_common::types::new_id;
use haven_llm::types::{CanonicalMessage, CanonicalRole, ContentPart};
use haven_llm::{EndpointRole, ImageGenClient, LlmRouter, OcrClient, SttClient, TtsClient};

use crate::coverage::{CoverageAction, MediaDecision, coverage_for, coverage_for_generate};
use crate::intent::{GenerateKind, Intent, detect_generate_kind, detect_intent};
use crate::modality::{Modality, detect_media_type, detect_modality, extension_for_media_type};

/// Outcome of processing a binary attachment.
#[derive(Debug, Clone)]
pub enum AttachmentOutcome {
    /// The gateway converted the media to text itself (dedicated provider or
    /// main-model fallback). The agent injects this text into the
    /// conversation instead of the raw media.
    Extracted {
        text: String,
        decision: MediaDecision,
    },
    /// No dedicated coverage: the media passes through to the agent as
    /// content parts (image_model / audio_model / default model routing).
    PassThrough { decision: MediaDecision },
}

/// Outcome of a pure-text generate request.
#[derive(Debug, Clone)]
pub enum GenerateOutcome {
    /// A media file was generated and saved; `file_path` points at it.
    Generated {
        kind: GenerateKind,
        file_path: PathBuf,
        decision: MediaDecision,
    },
    /// The user text was not a generate request.
    NotGenerate,
    /// Generate intent, but the capability is not configured.
    Unsupported { reason: String },
}

/// The in-process multi-modal gateway.
pub struct MediaGateway {
    router: Arc<LlmRouter>,
    stt: Option<Arc<dyn SttClient>>,
    ocr: Option<Arc<dyn OcrClient>>,
    tts: Option<Arc<dyn TtsClient>>,
    image_gen: Option<Arc<dyn ImageGenClient>>,
    config: MediaConfig,
}

impl MediaGateway {
    pub fn new(
        router: Arc<LlmRouter>,
        stt: Option<Arc<dyn SttClient>>,
        ocr: Option<Arc<dyn OcrClient>>,
        tts: Option<Arc<dyn TtsClient>>,
        image_gen: Option<Arc<dyn ImageGenClient>>,
        config: MediaConfig,
    ) -> Self {
        Self {
            router,
            stt,
            ocr,
            tts,
            image_gen,
            config,
        }
    }

    pub fn router(&self) -> &Arc<LlmRouter> {
        &self.router
    }

    /// True when any specialized capability is configured (used by callers
    /// to decide whether gateway pre-processing is worth running at all).
    pub fn has_specialized(&self) -> bool {
        self.stt.is_some() || self.ocr.is_some() || self.tts.is_some() || self.image_gen.is_some()
    }

    /// Process one binary attachment: modality detection → intent
    /// classification → coverage routing → extraction with confidence gate.
    pub async fn process_attachment(
        &self,
        bytes: &[u8],
        filename: &str,
        user_text: &str,
        explicit_intent: Option<Intent>,
    ) -> anyhow::Result<AttachmentOutcome> {
        let modality = detect_modality(bytes, filename);
        let intent = detect_intent(user_text, explicit_intent);
        let action = coverage_for(modality, intent);
        let decision = MediaDecision::new(modality, intent, action);

        match action {
            CoverageAction::Ocr => {
                let Some(ocr) = &self.ocr else {
                    // No OCR provider configured: pass the image to the agent
                    // unchanged (the vision model still reads the text).
                    return Ok(AttachmentOutcome::PassThrough { decision });
                };
                let media_type = detect_media_type(bytes).to_string();
                let threshold = self.config.ocr.min_confidence;
                match ocr.recognize(bytes, &media_type).await {
                    Ok(res)
                        if !res.text.trim().is_empty()
                            && confidence_passes(res.confidence, threshold) =>
                    {
                        Ok(AttachmentOutcome::Extracted {
                            text: res.text.trim().to_string(),
                            decision,
                        })
                    }
                    Ok(_) | Err(_) => {
                        // Low confidence / empty / error → main model.
                        self.extract_with_main_model(decision, bytes, &media_type)
                            .await
                    }
                }
            }
            CoverageAction::Stt => {
                let Some(stt) = &self.stt else {
                    return Ok(AttachmentOutcome::PassThrough { decision });
                };
                let threshold = self.config.stt.min_confidence;
                match stt.transcribe(bytes).await {
                    Ok(res)
                        if !res.text.trim().is_empty()
                            && confidence_passes(res.confidence, threshold) =>
                    {
                        Ok(AttachmentOutcome::Extracted {
                            text: res.text.trim().to_string(),
                            decision,
                        })
                    }
                    Ok(_) | Err(_) => {
                        self.extract_with_main_model(decision, bytes, detect_media_type(bytes))
                            .await
                    }
                }
            }
            _ => Ok(AttachmentOutcome::PassThrough { decision }),
        }
    }

    /// Fall back to the main model for an extraction action: the media is sent
    /// as a content part (image → vision role, audio → STT role) with the
    /// extraction system prompt, and the model's reply becomes the extracted
    /// text. The decision carries `fallback = true`.
    async fn extract_with_main_model(
        &self,
        mut decision: MediaDecision,
        bytes: &[u8],
        media_type: &str,
    ) -> anyhow::Result<AttachmentOutcome> {
        decision.fallback = true;
        decision.routed_to = match decision.action {
            CoverageAction::Ocr => "llm:image".to_string(),
            CoverageAction::Stt => "llm:audio".to_string(),
            _ => decision.action.as_str().to_string(),
        };
        let (role, system_prompt, part) = match decision.action {
            CoverageAction::Ocr => (
                self.router.vision_role().await,
                OCR_SYSTEM_PROMPT,
                ContentPart::Image {
                    content_type: "image_url".into(),
                    media_type: media_type.to_string(),
                    data: base64::engine::general_purpose::STANDARD.encode(bytes),
                },
            ),
            CoverageAction::Stt => {
                let role = match self.router.stt_role().await {
                    Some(role) => role,
                    None => EndpointRole::DefaultModel,
                };
                (
                    role,
                    TRANSCRIBE_SYSTEM_PROMPT,
                    ContentPart::Audio {
                        content_type: "input_audio".into(),
                        media_type: media_type.to_string(),
                        data: base64::engine::general_purpose::STANDARD.encode(bytes),
                    },
                )
            }
            _ => unreachable!("extract_with_main_model only called for Ocr/Stt actions"),
        };
        let messages = vec![
            CanonicalMessage::system(vec![ContentPart::text(system_prompt)]),
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![part],
                tool_calls: None,
                tool_call_id: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let resp = self
            .router
            .chat_stream_with_tools_aggregated(role, &messages, &[], |_| {})
            .await
            .map_err(|e| anyhow::anyhow!("主模型提取失败: {e}"))?;
        let text = resp.text.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("主模型提取失败: 返回为空");
        }
        Ok(AttachmentOutcome::Extracted { text, decision })
    }

    /// Handle a pure-text generate request (TTS / text-to-image). Returns
    /// [`GenerateOutcome::NotGenerate`] when the intent is not generate, and
    /// [`GenerateOutcome::Unsupported`] when the capability is unconfigured.
    pub async fn process_generate(
        &self,
        user_text: &str,
        explicit_intent: Option<Intent>,
    ) -> anyhow::Result<GenerateOutcome> {
        let intent = detect_intent(user_text, explicit_intent);
        if intent != Intent::Generate {
            return Ok(GenerateOutcome::NotGenerate);
        }
        let kind = detect_generate_kind(user_text);
        let action = coverage_for_generate(user_text);
        let decision = MediaDecision::new(Modality::Text, Intent::Generate, action);

        match kind {
            GenerateKind::Speech => {
                let Some(tts) = &self.tts else {
                    return Ok(GenerateOutcome::Unsupported {
                        reason: "未配置 TTS（设置 → 媒体 → TTS）".into(),
                    });
                };
                let audio = tts
                    .synthesize(user_text)
                    .await
                    .map_err(|e| anyhow::anyhow!("TTS 合成失败: {e}"))?;
                let path = save_media_file(&audio, "audio/mpeg")?;
                Ok(GenerateOutcome::Generated {
                    kind,
                    file_path: path,
                    decision,
                })
            }
            GenerateKind::Image => {
                let Some(image_gen) = &self.image_gen else {
                    return Ok(GenerateOutcome::Unsupported {
                        reason: "未配置文生图（设置 → 媒体 → 文生图）".into(),
                    });
                };
                let img = image_gen
                    .generate(user_text)
                    .await
                    .map_err(|e| anyhow::anyhow!("文生图失败: {e}"))?;
                let path = save_media_file(&img.data, &img.media_type)?;
                Ok(GenerateOutcome::Generated {
                    kind,
                    file_path: path,
                    decision,
                })
            }
        }
    }
}

/// Save generated media under `<data_dir>/media` with a canonical
/// `file-{uuid32}.{ext}` name.
fn save_media_file(bytes: &[u8], media_type: &str) -> anyhow::Result<PathBuf> {
    let dir = haven_common::config::ConfigLoader::data_dir().join("media");
    std::fs::create_dir_all(&dir)?;
    let ext = extension_for_media_type(media_type);
    let path = dir.join(format!("{}.{}", new_id("file"), ext));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

/// Confidence gate: a reported confidence below `threshold` triggers the
/// main-model fallback. Providers without confidence reporting pass (they
/// fall back on error / empty result instead).
fn confidence_passes(reported: Option<f32>, threshold: f32) -> bool {
    match reported {
        Some(c) => c >= threshold,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use haven_common::config::OcrConfig;
    use haven_llm::LlmClient;
    use haven_llm::types::{LlmError, LlmResponse};
    use std::sync::atomic::{AtomicU64, Ordering};

    // --- mock clients ------------------------------------------------------

    struct MockOcr {
        text: String,
        confidence: Option<f32>,
        calls: AtomicU64,
    }

    #[async_trait]
    impl OcrClient for MockOcr {
        async fn recognize(&self, _bytes: &[u8], _media_type: &str) -> anyhow::Result<OcrResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(OcrResult {
                text: self.text.clone(),
                confidence: self.confidence,
            })
        }
    }

    use haven_llm::OcrResult;

    struct MockStt {
        text: String,
        confidence: Option<f32>,
    }

    #[async_trait]
    impl SttClient for MockStt {
        async fn transcribe(&self, _wav: &[u8]) -> anyhow::Result<haven_llm::SttResult> {
            Ok(haven_llm::SttResult {
                text: self.text.clone(),
                confidence: self.confidence,
            })
        }
    }

    struct MockLlm {
        text: String,
        calls: AtomicU64,
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(LlmResponse {
                text: self.text.clone(),
                ..Default::default()
            })
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
        ) -> Result<
            std::pin::Pin<
                Box<
                    dyn futures_util::Stream<Item = Result<haven_llm::StreamChunk, LlmError>>
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

    fn mock_router(text: &str) -> Arc<LlmRouter> {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlm {
            text: text.into(),
            calls: AtomicU64::new(0),
        });
        Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ))
    }

    fn test_config() -> MediaConfig {
        MediaConfig::default()
    }

    fn image_bytes() -> Vec<u8> {
        b"\xFF\xD8\xFF\xE0fake-jpeg".to_vec()
    }

    fn wav_bytes() -> Vec<u8> {
        b"RIFF\x24\x00\x00\x00WAVEfake".to_vec()
    }

    // --- attachment routing ------------------------------------------------

    #[tokio::test]
    async fn image_extract_high_confidence_uses_ocr() {
        let ocr: Arc<dyn OcrClient> = Arc::new(MockOcr {
            text: "识别出的文字".into(),
            confidence: Some(0.95),
            calls: AtomicU64::new(0),
        });
        let gw = MediaGateway::new(
            mock_router("unused"),
            None,
            Some(ocr),
            None,
            None,
            test_config(),
        );
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "提取文字", None)
            .await
            .unwrap();
        let AttachmentOutcome::Extracted { text, decision } = outcome else {
            panic!("expected Extracted");
        };
        assert_eq!(text, "识别出的文字");
        assert_eq!(decision.routed_to, "ocr");
        assert!(!decision.fallback);
    }

    #[tokio::test]
    async fn image_extract_low_confidence_falls_back_to_main_model() {
        let ocr: Arc<dyn OcrClient> = Arc::new(MockOcr {
            text: "低置信度结果".into(),
            confidence: Some(0.3),
            calls: AtomicU64::new(0),
        });
        let gw = MediaGateway::new(
            mock_router("主模型提取的文本"),
            None,
            Some(ocr),
            None,
            None,
            test_config(),
        );
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "提取文字", None)
            .await
            .unwrap();
        let AttachmentOutcome::Extracted { text, decision } = outcome else {
            panic!("expected Extracted");
        };
        assert_eq!(text, "主模型提取的文本");
        assert!(decision.fallback);
        assert_eq!(decision.routed_to, "llm:image");
    }

    #[tokio::test]
    async fn image_extract_without_confidence_uses_ocr_result() {
        // No confidence reported → no gate → specialized result wins.
        let ocr: Arc<dyn OcrClient> = Arc::new(MockOcr {
            text: "无置信度结果".into(),
            confidence: None,
            calls: AtomicU64::new(0),
        });
        let gw = MediaGateway::new(
            mock_router("unused"),
            None,
            Some(ocr),
            None,
            None,
            test_config(),
        );
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "提取文字", None)
            .await
            .unwrap();
        let AttachmentOutcome::Extracted { text, decision } = outcome else {
            panic!("expected Extracted");
        };
        assert_eq!(text, "无置信度结果");
        assert!(!decision.fallback);
    }

    #[tokio::test]
    async fn image_extract_empty_result_falls_back() {
        let ocr: Arc<dyn OcrClient> = Arc::new(MockOcr {
            text: "   ".into(),
            confidence: Some(0.99),
            calls: AtomicU64::new(0),
        });
        let gw = MediaGateway::new(
            mock_router("兜底文本"),
            None,
            Some(ocr),
            None,
            None,
            test_config(),
        );
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "提取文字", None)
            .await
            .unwrap();
        let AttachmentOutcome::Extracted { text, .. } = outcome else {
            panic!("expected Extracted");
        };
        assert_eq!(text, "兜底文本");
    }

    #[tokio::test]
    async fn audio_extract_routes_through_stt() {
        let stt: Arc<dyn SttClient> = Arc::new(MockStt {
            text: "转写结果".into(),
            confidence: Some(0.9),
        });
        let gw = MediaGateway::new(
            mock_router("unused"),
            Some(stt),
            None,
            None,
            None,
            test_config(),
        );
        let outcome = gw
            .process_attachment(&wav_bytes(), "a.wav", "转文字", None)
            .await
            .unwrap();
        let AttachmentOutcome::Extracted { text, decision } = outcome else {
            panic!("expected Extracted");
        };
        assert_eq!(text, "转写结果");
        assert_eq!(decision.routed_to, "stt");
        assert!(!decision.fallback);
    }

    #[tokio::test]
    async fn audio_generate_keywords_collapse_to_stt() {
        // "把这段音频读出来" — generate keyword, but the input is audio →
        // extraction (transcription).
        let stt: Arc<dyn SttClient> = Arc::new(MockStt {
            text: "读出来的内容".into(),
            confidence: Some(0.9),
        });
        let gw = MediaGateway::new(
            mock_router("unused"),
            Some(stt),
            None,
            None,
            None,
            test_config(),
        );
        let outcome = gw
            .process_attachment(&wav_bytes(), "a.wav", "把这段音频读出来", None)
            .await
            .unwrap();
        let AttachmentOutcome::Extracted { text, .. } = outcome else {
            panic!("expected Extracted");
        };
        assert_eq!(text, "读出来的内容");
    }

    #[tokio::test]
    async fn understand_intent_passes_through() {
        let gw = MediaGateway::new(mock_router("unused"), None, None, None, None, test_config());
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "描述一下这张图", None)
            .await
            .unwrap();
        let AttachmentOutcome::PassThrough { decision } = outcome else {
            panic!("expected PassThrough");
        };
        assert_eq!(decision.action, CoverageAction::LlmImage);
        assert!(!decision.fallback);
    }

    #[tokio::test]
    async fn no_ocr_configured_passes_through() {
        let gw = MediaGateway::new(mock_router("unused"), None, None, None, None, test_config());
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "提取文字", None)
            .await
            .unwrap();
        let AttachmentOutcome::PassThrough { decision } = outcome else {
            panic!("expected PassThrough");
        };
        assert_eq!(decision.action, CoverageAction::Ocr);
        assert!(!decision.fallback);
    }

    #[tokio::test]
    async fn explicit_intent_override_wins() {
        let gw = MediaGateway::new(mock_router("unused"), None, None, None, None, test_config());
        let outcome = gw
            .process_attachment(&image_bytes(), "a.png", "随便聊聊", Some(Intent::Extract))
            .await
            .unwrap();
        let AttachmentOutcome::PassThrough { decision } = outcome else {
            panic!("expected PassThrough");
        };
        assert_eq!(decision.action, CoverageAction::Ocr);
    }

    // --- generate ----------------------------------------------------------

    struct MockTts;

    #[async_trait]
    impl TtsClient for MockTts {
        async fn synthesize(&self, _text: &str) -> anyhow::Result<Vec<u8>> {
            Ok(b"mp3-bytes".to_vec())
        }
    }

    struct MockImageGen;

    #[async_trait]
    impl ImageGenClient for MockImageGen {
        async fn generate(&self, _prompt: &str) -> anyhow::Result<haven_llm::GeneratedImage> {
            Ok(haven_llm::GeneratedImage {
                media_type: "image/png".into(),
                data: b"png-bytes".to_vec(),
            })
        }
    }

    #[tokio::test]
    async fn generate_speech_saves_mp3() {
        let tts: Arc<dyn TtsClient> = Arc::new(MockTts);
        let gw = MediaGateway::new(
            mock_router("unused"),
            None,
            None,
            Some(tts),
            None,
            test_config(),
        );
        let outcome = gw.process_generate("朗读这段话", None).await.unwrap();
        let GenerateOutcome::Generated {
            kind, file_path, ..
        } = outcome
        else {
            panic!("expected Generated");
        };
        assert_eq!(kind, GenerateKind::Speech);
        assert!(file_path.to_string_lossy().ends_with(".mp3"));
        assert!(file_path.exists());
        let _ = std::fs::remove_file(&file_path);
    }

    #[tokio::test]
    async fn generate_image_saves_png() {
        let ig: Arc<dyn ImageGenClient> = Arc::new(MockImageGen);
        let gw = MediaGateway::new(
            mock_router("unused"),
            None,
            None,
            None,
            Some(ig),
            test_config(),
        );
        let outcome = gw.process_generate("画一只猫", None).await.unwrap();
        let GenerateOutcome::Generated {
            kind, file_path, ..
        } = outcome
        else {
            panic!("expected Generated");
        };
        assert_eq!(kind, GenerateKind::Image);
        assert!(file_path.to_string_lossy().ends_with(".png"));
        assert!(file_path.exists());
        let _ = std::fs::remove_file(&file_path);
    }

    #[tokio::test]
    async fn generate_without_capability_is_unsupported() {
        let gw = MediaGateway::new(mock_router("unused"), None, None, None, None, test_config());
        let outcome = gw.process_generate("画一只猫", None).await.unwrap();
        let GenerateOutcome::Unsupported { reason } = outcome else {
            panic!("expected Unsupported");
        };
        assert!(reason.contains("文生图"));
    }

    #[tokio::test]
    async fn non_generate_text_returns_not_generate() {
        let gw = MediaGateway::new(mock_router("unused"), None, None, None, None, test_config());
        let outcome = gw.process_generate("你好呀", None).await.unwrap();
        assert!(matches!(outcome, GenerateOutcome::NotGenerate));
    }

    // --- config helpers ----------------------------------------------------

    #[test]
    fn default_media_config_has_expected_thresholds() {
        let cfg = test_config();
        assert_eq!(cfg.ocr.min_confidence, 0.7);
        assert_eq!(cfg.stt.min_confidence, 0.7);
        assert_eq!(OcrConfig::default().provider, "none");
    }
}
