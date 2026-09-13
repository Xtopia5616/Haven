//! Agent-native media operations.
//!
//! The model-facing boundary is deliberately small: callers pass an opaque
//! `asset_id`, select a media operation, and receive a compact media reference
//! containing the selected representation and optional derived content. Host
//! paths and raw bytes stay inside this module; durable attachment metadata is
//! owned by `haven_common::media`.

use async_trait::async_trait;
use haven_common::media::MediaRepresentationKind;
use haven_common::types::RiskLevel;
use haven_llm::LlmRouter;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::{ManagedAssetRegistry, OperationIdempotency, Tool, ToolConcurrency, ToolResult};

use super::media_audio::AudioRuntime;

const MAX_FOCUS_CHARS: usize = 2_000;
const MAX_GENERATION_PROMPT_CHARS: usize = 4_000;

#[path = "media_asset.rs"]
mod media_asset;
#[path = "media_content.rs"]
mod media_content;
#[path = "media_generation.rs"]
mod media_generation;
#[path = "media_reference.rs"]
mod media_reference;
#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;

pub(crate) use media_asset::register_path_asset;
pub(crate) use media_generation::register_generated_asset;
pub(crate) use media_reference::classify_media;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaOperation {
    Inspect,
    Describe,
    Ocr,
    Transcribe,
    Extract,
    Render,
    Generate,
    Record,
    Play,
    Speak,
    VolumeGet,
    VolumeSet,
    MuteGet,
    MuteSet,
}

/// Internal grouping for operations that consume or produce a managed media
/// asset. The public `MediaOperation` stays flat because that is the stable
/// model-facing tool schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaAssetOperation {
    Inspect,
    Describe,
    Ocr,
    Transcribe,
    Extract,
    Render,
    Generate,
    Record,
}

/// Internal grouping for operations that act on the host audio device and do
/// not represent a managed asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioDeviceOperation {
    Play,
    Speak,
    VolumeGet,
    VolumeSet,
    MuteGet,
    MuteSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaOperationGroup {
    Asset(MediaAssetOperation),
    Device(AudioDeviceOperation),
}

impl MediaOperation {
    pub(crate) const fn group(self) -> MediaOperationGroup {
        match self {
            Self::Inspect => MediaOperationGroup::Asset(MediaAssetOperation::Inspect),
            Self::Describe => MediaOperationGroup::Asset(MediaAssetOperation::Describe),
            Self::Ocr => MediaOperationGroup::Asset(MediaAssetOperation::Ocr),
            Self::Transcribe => MediaOperationGroup::Asset(MediaAssetOperation::Transcribe),
            Self::Extract => MediaOperationGroup::Asset(MediaAssetOperation::Extract),
            Self::Render => MediaOperationGroup::Asset(MediaAssetOperation::Render),
            Self::Generate => MediaOperationGroup::Asset(MediaAssetOperation::Generate),
            Self::Record => MediaOperationGroup::Asset(MediaAssetOperation::Record),
            Self::Play => MediaOperationGroup::Device(AudioDeviceOperation::Play),
            Self::Speak => MediaOperationGroup::Device(AudioDeviceOperation::Speak),
            Self::VolumeGet => MediaOperationGroup::Device(AudioDeviceOperation::VolumeGet),
            Self::VolumeSet => MediaOperationGroup::Device(AudioDeviceOperation::VolumeSet),
            Self::MuteGet => MediaOperationGroup::Device(AudioDeviceOperation::MuteGet),
            Self::MuteSet => MediaOperationGroup::Device(AudioDeviceOperation::MuteSet),
        }
    }

    pub(crate) const fn is_asset_operation(self) -> bool {
        matches!(self.group(), MediaOperationGroup::Asset(_))
    }

    pub(crate) const fn is_device_operation(self) -> bool {
        matches!(self.group(), MediaOperationGroup::Device(_))
    }

    /// Recording is an asset operation semantically, but it still reserves
    /// the host audio device while the asset is being produced.
    pub(crate) const fn uses_audio_runtime(self) -> bool {
        self.is_device_operation() || matches!(self, Self::Record)
    }
}

fn media_operation_from_name(value: Option<&str>) -> Option<MediaOperation> {
    Some(match value? {
        "inspect" => MediaOperation::Inspect,
        "describe" => MediaOperation::Describe,
        "ocr" => MediaOperation::Ocr,
        "transcribe" => MediaOperation::Transcribe,
        "extract" => MediaOperation::Extract,
        "render" => MediaOperation::Render,
        "generate" => MediaOperation::Generate,
        "record" => MediaOperation::Record,
        "play" => MediaOperation::Play,
        "speak" => MediaOperation::Speak,
        "volume_get" => MediaOperation::VolumeGet,
        "volume_set" => MediaOperation::VolumeSet,
        "mute_get" => MediaOperation::MuteGet,
        "mute_set" => MediaOperation::MuteSet,
        _ => return None,
    })
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
    /// Zero-based document page/section cursor for `extract`.
    #[serde(default)]
    pub page_index: Option<u64>,
    /// Trusted host path accepted only for local audio playback.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Text accepted only for local TTS playback.
    #[serde(default)]
    pub text: Option<String>,
    /// Recording duration in seconds.
    #[serde(default)]
    pub duration: Option<f64>,
    /// Master output volume in the inclusive range 0..=1.
    #[serde(default)]
    pub volume: Option<f64>,
    /// Master output mute state.
    #[serde(default)]
    pub muted: Option<bool>,
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
    record_available: bool,
    tts_available: bool,
    pub(crate) audio_runtime: Arc<AudioRuntime>,
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
            // OCR is a dedicated capability. A router may support text-only
            // requests, so it must never make OCR appear available by itself.
            ocr_available: false,
            transcribe_available: has_router,
            generate_available: false,
            record_available: false,
            tts_available: false,
            audio_runtime: Arc::new(AudioRuntime::with_tts(None, None)),
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
        // OCR is deliberately independent from the vision description route.
        // The dedicated OCR client is the only supported OCR capability until
        // a renderer-backed OCR provider is installed.
        self.ocr_available = self.ocr_client.is_some();
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
        self.ocr_available = ocr_client.is_some();
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

    pub(crate) fn with_audio_runtime(mut self, audio_runtime: Arc<AudioRuntime>) -> Self {
        self.record_available = audio_runtime.record_available();
        self.tts_available = audio_runtime.tts_available();
        self.audio_runtime = audio_runtime;
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
        if params.operation.uses_audio_runtime() {
            return self.run_audio(params, cancel).await;
        }
        if params.operation == MediaOperation::Generate {
            return self.generate(params, cancel).await;
        }
        debug_assert!(params.operation.is_asset_operation());
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
            return Ok(self.cancelled_media_result(params.operation, &asset, "cancelled"));
        }

        let (modality, file_kind) = classify_media(&asset);
        match params.operation {
            MediaOperation::Inspect => {
                let mut output = self.media_result_output(
                    MediaOperation::Inspect,
                    Some(&asset),
                    Some(MediaRepresentationKind::ManagedFileRef),
                    None,
                );
                if let Some(object) = output.as_object_mut() {
                    object.insert("modality".into(), json!(modality));
                    object.insert("file_kind".into(), json!(file_kind));
                }
                Ok(ToolResult::ok(output))
            }
            MediaOperation::Describe => self.describe(asset, params.focus, cancel).await,
            MediaOperation::Ocr => self.ocr(asset, params.focus, cancel).await,
            MediaOperation::Transcribe => self.transcribe(asset, cancel).await,
            MediaOperation::Extract => self.extract(asset, params.page_index, cancel).await,
            MediaOperation::Render => self.render(asset, params.page_index, cancel).await,
            MediaOperation::Generate => unreachable!("generate handled before asset resolution"),
            _ => unreachable!("audio operation handled before asset resolution"),
        }
    }
}
#[async_trait]
impl Tool for MediaTool {
    fn name(&self) -> String {
        "media".into()
    }

    fn description(&self) -> String {
        crate::prompts::MEDIA_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("ocr") => RiskLevel::High,
            Some("describe") | Some("transcribe") => RiskLevel::Medium,
            Some("extract") | Some("render") | Some("inspect") => RiskLevel::Low,
            Some("generate") => RiskLevel::Medium,
            Some("record") | Some("volume_set") | Some("mute_set") => RiskLevel::Medium,
            Some("play") | Some("speak") | Some("volume_get") | Some("mute_get") => RiskLevel::Low,
            _ => RiskLevel::Low,
        }
    }

    fn idempotency(&self, input: &Value) -> OperationIdempotency {
        match input["operation"].as_str() {
            Some("inspect") | Some("describe") | Some("ocr") | Some("transcribe")
            | Some("extract") | Some("render") => OperationIdempotency::Idempotent,
            Some("generate") | Some("record") | Some("play") | Some("speak")
            | Some("volume_set") | Some("mute_set") => OperationIdempotency::NonIdempotent,
            Some("volume_get") | Some("mute_get") => OperationIdempotency::Idempotent,
            _ => OperationIdempotency::Unknown,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        if media_operation_from_name(input["operation"].as_str())
            .is_some_and(MediaOperation::uses_audio_runtime)
        {
            return ToolConcurrency::Resource("media:audio-device".into());
        }
        let resource = input["asset_id"]
            .as_str()
            .or_else(|| input["prompt"].as_str())
            .unwrap_or("unknown");
        ToolConcurrency::Resource(format!("media:{resource}"))
    }

    fn default_timeout_secs(&self) -> u64 {
        self.timeout_secs.saturating_add(5).max(30)
    }

    fn timeout_secs_for(&self, input: &Value) -> u64 {
        if input["operation"].as_str() == Some("record") {
            let duration = input["duration"].as_f64().unwrap_or(10.0).clamp(1.0, 60.0);
            return self
                .timeout_secs
                .max(duration.ceil() as u64 + 30)
                .saturating_add(5)
                .max(30);
        }
        self.default_timeout_secs()
    }

    fn requires_session_id(&self) -> bool {
        true
    }

    fn input_schema(&self) -> Value {
        let mut schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "operation": {"type": "string", "enum": ["inspect", "describe", "ocr", "transcribe", "extract", "render", "generate", "record", "play", "speak", "volume_get", "volume_set", "mute_get", "mute_set"]},
                "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"},
                "page_index": {"type": "integer", "minimum": 0, "description": "Zero-based document page/section cursor; extract returns next_page when available"},
                "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS},
                "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_GENERATION_PROMPT_CHARS},
                "file_path": {"type": "string", "minLength": 1, "description": "Trusted local .wav path; only accepted by play"},
                "text": {"type": "string", "minLength": 1, "maxLength": 4000, "description": "Text for local TTS; only accepted by speak"},
                "duration": {"type": "number", "minimum": 1, "maximum": 60, "description": "Recording duration in seconds"},
                "volume": {"type": "number", "minimum": 0, "maximum": 1, "description": "Default output volume from 0 to 1"},
                "muted": {"type": "boolean", "description": "Default output mute state"}
            },
            "required": ["operation"],
            "oneOf": [
                {"additionalProperties": false, "properties": {"operation": {"const": "inspect"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "describe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "ocr"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "focus": {"type": "string", "maxLength": MAX_FOCUS_CHARS}}, "required": ["operation", "asset_id"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "transcribe"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}}, "required": ["operation", "asset_id"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "extract"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "page_index": {"type": "integer", "minimum": 0}}, "required": ["operation", "asset_id"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "render"}, "asset_id": {"type": "string", "pattern": "^asset-[0-9a-f]{32}$"}, "page_index": {"type": "integer", "minimum": 0}}, "required": ["operation", "asset_id"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "generate"}, "prompt": {"type": "string", "minLength": 1, "maxLength": MAX_GENERATION_PROMPT_CHARS}}, "required": ["operation", "prompt"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "record"}, "duration": {"type": "number", "minimum": 1, "maximum": 60}}, "required": ["operation"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "play"}, "file_path": {"type": "string", "minLength": 1}}, "required": ["operation", "file_path"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "speak"}, "text": {"type": "string", "minLength": 1, "maxLength": 4000}}, "required": ["operation", "text"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "volume_get"}}, "required": ["operation"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "volume_set"}, "volume": {"type": "number", "minimum": 0, "maximum": 1}}, "required": ["operation", "volume"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "mute_get"}}, "required": ["operation"]},
                {"additionalProperties": false, "properties": {"operation": {"const": "mute_set"}, "muted": {"type": "boolean"}}, "required": ["operation", "muted"]}
            ]
        });
        let unavailable = [
            ("describe", self.describe_available),
            ("ocr", self.ocr_available),
            ("transcribe", self.transcribe_available),
            ("generate", self.generate_available),
            ("record", self.record_available),
            ("speak", self.tts_available),
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
