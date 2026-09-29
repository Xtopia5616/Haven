//! Media-related configuration under `[media.*]`: audio capture, STT, OCR,
//! TTS, and image generation. Capture (`audio`) and transcription (`stt`) are
//! separate structs but share one settings surface; same for image limits
//! (context_limits) + OCR.

use super::*;
use crate::media::MediaInputStrategy;

/// Microphone capture / VAD parameters. Lives under `[media.audio]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub max_duration_secs: u64,
    pub silence_timeout_ms: u64,
    pub vad_threshold: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            bits_per_sample: 16,
            max_duration_secs: 60,
            silence_timeout_ms: 1500,
            vad_threshold: 0.5,
        }
    }
}

/// Speech-to-text configuration. Lives under `[media.stt]`. Cloud providers
/// are materialized into a [`super::ModelEndpoint`] and dispatched through
/// the same `adapter_for` / `LlmClient::transcribe` path as chat requests;
/// `llm` uses the router's `transcription` request policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SttConfig {
    /// Speech-to-text provider. Prefer a name from `llm.providers` (reuses
    /// that provider's base URL + API key). Also accepts:
    /// - `llm`: transcribe via the configured `transcription` request policy
    /// - `mcp`: route through an MCP server exposing `stt.transcribe`
    /// - `none`: no transcription
    pub provider: String,
    /// MCP server name when `provider == "mcp"`.
    pub mcp_server: Option<String>,
    /// Model id for providers that require one (e.g. `whisper-1`,
    /// `nova-2`, `whisper-large-v3-turbo`).
    pub model: String,
    /// Transcription timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum transcription confidence (0.0-1.0) for the media tool's
    /// confidence gate: when the provider reports a lower confidence the
    /// tool falls back to the main model. Providers without confidence
    /// reporting (e.g. OpenAI Whisper) ignore this and fall back on error /
    /// empty result instead.
    pub min_confidence: f32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: "llm".into(),
            mcp_server: None,
            model: String::new(),
            timeout_secs: 30,
            min_confidence: 0.7,
        }
    }
}

/// OCR (image text extraction) configuration. Lives under `[media.ocr]`.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OcrConfig {
    /// OCR provider. One of:
    /// - `llm`: extract via the configured `vision` request policy
    /// - `baidu`: Baidu 通用文字识别（标准版）
    /// - `azure`: Azure AI Vision (Computer Vision 3.2 OCR)
    /// - `tencent`: Tencent Cloud 通用印刷体识别
    /// - `none`: no OCR client (extract intent passes the image through)
    pub provider: String,
    /// Runtime-only API key / access token for cloud OCR providers. Settings
    /// writes stage it first; plaintext TOML/Settings values are rejected.
    #[serde(default, skip_serializing)]
    pub api_key: String,
    /// Opaque credential reference persisted in TOML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_ref: Option<String>,
    /// Runtime-only secondary secret where a provider requires one (Baidu
    /// secret key). Settings writes stage it first; plaintext values are
    /// rejected.
    #[serde(default, skip_serializing)]
    pub api_secret: String,
    /// Opaque credential reference persisted in TOML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_secret_ref: Option<String>,
    /// Base URL override. Overrides the provider's default host when
    /// non-empty.
    pub base_url: String,
    /// OCR timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum recognition confidence (0.0-1.0) for the media tool's
    /// confidence gate: when the provider reports a lower average
    /// confidence the tool falls back to the main model. Providers
    /// without confidence reporting ignore this and fall back on error /
    /// empty result instead.
    pub min_confidence: f32,
}

impl std::fmt::Debug for OcrConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OcrConfig")
            .field("provider", &self.provider)
            .field("api_key", &"[REDACTED]")
            .field("api_key_ref", &self.api_key_ref)
            .field("api_secret", &"[REDACTED]")
            .field("api_secret_ref", &self.api_secret_ref)
            .field("base_url", &self.base_url)
            .field("timeout_secs", &self.timeout_secs)
            .field("min_confidence", &self.min_confidence)
            .finish()
    }
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            provider: "llm".into(),
            api_key: String::new(),
            api_key_ref: None,
            api_secret: String::new(),
            api_secret_ref: None,
            base_url: String::new(),
            timeout_secs: 20,
            min_confidence: 0.7,
        }
    }
}

/// Text-to-speech configuration. Lives under `[media.tts]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TtsConfig {
    /// TTS provider. Prefer a name from `llm.providers` (reuses that
    /// provider's base URL + API key). Also accepts:
    /// - `none` / empty: no TTS client
    pub provider: String,
    /// Model id for providers that require one (e.g. `tts-1`,
    /// `gpt-4o-mini-tts`).
    pub model: String,
    /// Voice id / name for providers that expose voices
    /// (e.g. `alloy`, `11labs_voice_id`).
    pub voice: String,
    /// TTS timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            model: String::new(),
            voice: String::new(),
            timeout_secs: 60,
        }
    }
}

/// Text-to-image generation configuration. Lives under `[media.image_gen]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ImageGenConfig {
    /// Image generation provider. A name from `llm.providers` reuses that
    /// provider's base URL and API key. `none` / empty disables the client.
    pub provider: String,
    /// Model id for providers that require one (e.g. `gpt-image-1`,
    /// `gemini-2.5-flash-image`).
    pub model: String,
    /// Image generation timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            model: String::new(),
            timeout_secs: 120,
        }
    }
}

/// Unified media configuration: capture + extract + generate capabilities.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct MediaConfig {
    /// Provider-facing input selection policy for user attachments.
    pub input_strategy: MediaInputStrategy,
    /// Microphone capture / VAD (voice input card).
    pub audio: AudioConfig,
    /// Speech-to-text (voice input transcription).
    pub stt: SttConfig,
    /// OCR (image text extraction).
    pub ocr: OcrConfig,
    /// Text-to-speech (voice output).
    pub tts: TtsConfig,
    /// Text-to-image generation.
    pub image_gen: ImageGenConfig,
}

#[cfg(test)]
mod tests {
    use super::OcrConfig;

    #[test]
    fn ocr_config_accepts_staged_secrets_and_redacts_debug() {
        let config: OcrConfig = serde_json::from_value(serde_json::json!({
            "api_key": "ocr-key",
            "api_secret": "ocr-secret"
        }))
        .expect("settings DTO accepts values before command validation");

        let debug = format!("{config:?}");
        assert!(!debug.contains("ocr-key"));
        assert!(!debug.contains("ocr-secret"));
        assert_eq!(debug.matches("[REDACTED]").count(), 2);
    }
}
