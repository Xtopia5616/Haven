//! Media-related configuration under `[media.*]`: audio capture, STT, OCR,
//! TTS, and image generation. Capture (`audio`) and transcription (`stt`) are
//! separate structs but share one settings surface; same for image limits
//! (context_limits) + OCR.

use super::*;

/// Microphone capture / VAD parameters. Lives under `[media.audio]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
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
/// the same `adapter_for` / `LlmClient::transcribe` path as chat roles;
/// `llm` uses the router's `audio_model` (or default) slot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SttConfig {
    /// Speech-to-text provider. Prefer a name from `llm.providers` (reuses
    /// that provider's base URL + API key). Also accepts:
    /// - `llm`: transcribe via the configured `audio_model` LLM endpoint
    /// - `mcp`: route through an MCP server exposing `stt.transcribe`
    /// - `openai` / `groq` / `gemini` / `deepgram` / `assemblyai`: legacy
    ///   capability ids with local credentials
    /// - `none`: no transcription
    pub provider: String,
    /// MCP server name when `provider == "mcp"`.
    pub mcp_server: Option<String>,
    /// Legacy / resolved API key. Unused when `provider` names an LLM
    /// provider.
    pub api_key: String,
    /// Model id for providers that require one (e.g. `whisper-1`,
    /// `nova-2`, `whisper-large-v3-turbo`).
    pub model: String,
    /// Legacy / resolved base URL. Unused when `provider` names an LLM
    /// provider.
    pub base_url: String,
    /// Transcription timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum transcription confidence (0.0-1.0) for the gateway's
    /// confidence gate: when the provider reports a lower confidence the
    /// gateway falls back to the main model. Providers without confidence
    /// reporting (e.g. OpenAI Whisper) ignore this and fall back on error /
    /// empty result instead.
    pub min_confidence: f32,
}

impl Default for SttConfig {
    fn default() -> Self {
        Self {
            provider: "llm".into(),
            mcp_server: None,
            api_key: String::new(),
            model: String::new(),
            base_url: String::new(),
            timeout_secs: 30,
            min_confidence: 0.7,
        }
    }
}

/// OCR (image text extraction) configuration. Lives under `[media.ocr]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct OcrConfig {
    /// OCR provider. One of:
    /// - `llm`: extract via the configured `image_model` / vision role
    /// - `baidu`: Baidu 通用文字识别（标准版）
    /// - `azure`: Azure AI Vision (Computer Vision 3.2 OCR)
    /// - `tencent`: Tencent Cloud 通用印刷体识别
    /// - `none`: no OCR client (extract intent passes the image through)
    pub provider: String,
    /// API key / access token for cloud OCR providers.
    pub api_key: String,
    /// Secondary secret where a provider requires one (Baidu secret key).
    pub api_secret: String,
    /// Base URL override. Overrides the provider's default host when
    /// non-empty.
    pub base_url: String,
    /// OCR timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum recognition confidence (0.0-1.0) for the gateway's
    /// confidence gate: when the provider reports a lower average
    /// confidence the gateway falls back to the main model. Providers
    /// without confidence reporting ignore this and fall back on error /
    /// empty result instead.
    pub min_confidence: f32,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            provider: "llm".into(),
            api_key: String::new(),
            api_secret: String::new(),
            base_url: String::new(),
            timeout_secs: 20,
            min_confidence: 0.7,
        }
    }
}

/// Text-to-speech configuration. Lives under `[media.tts]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TtsConfig {
    /// TTS provider. Prefer a name from `llm.providers` (reuses that
    /// provider's base URL + API key). Also accepts:
    /// - `openai` / `elevenlabs`: legacy capability ids with local credentials
    /// - `none` / empty: no TTS client
    pub provider: String,
    /// Legacy / resolved API key. When `provider` names an LLM provider this
    /// is unused at build time (credentials come from that provider).
    pub api_key: String,
    /// Model id for providers that require one (e.g. `tts-1`,
    /// `gpt-4o-mini-tts`).
    pub model: String,
    /// Voice id / name for providers that expose voices
    /// (e.g. `alloy`, `11labs_voice_id`).
    pub voice: String,
    /// Legacy / resolved base URL. Unused when `provider` names an LLM
    /// provider (URL comes from that provider).
    pub base_url: String,
    /// TTS timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            api_key: String::new(),
            model: String::new(),
            voice: String::new(),
            base_url: String::new(),
            timeout_secs: 60,
        }
    }
}

/// Text-to-image generation configuration. Lives under `[media.image_gen]`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ImageGenConfig {
    /// Image generation provider. Prefer a name from `llm.providers` (reuses
    /// that provider's base URL + API key). Also accepts:
    /// - `openai` / `gemini`: legacy capability ids with local credentials
    /// - `none` / empty: no image generation client
    pub provider: String,
    /// Legacy / resolved API key. Unused when `provider` names an LLM
    /// provider.
    pub api_key: String,
    /// Model id for providers that require one (e.g. `gpt-image-1`,
    /// `gemini-2.5-flash-image`).
    pub model: String,
    /// Legacy / resolved base URL. Unused when `provider` names an LLM
    /// provider.
    pub base_url: String,
    /// Image generation timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for ImageGenConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            api_key: String::new(),
            model: String::new(),
            base_url: String::new(),
            timeout_secs: 120,
        }
    }
}

/// Unified media configuration: capture + extract + generate capabilities.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MediaConfig {
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
