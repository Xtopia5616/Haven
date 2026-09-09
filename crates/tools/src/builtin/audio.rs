use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_input::InputPipeline;
use haven_llm::TtsClient;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Default capture window when the LLM omits `duration`.
const DEFAULT_RECORD_SECS: f64 = 10.0;
/// Hard cap: the microphone belongs to the user first, and a runaway tool
/// call must not monopolize it (the pipeline's own `max_duration_secs`
/// config still applies as a final bound).
const MAX_RECORD_SECS: f64 = 60.0;
/// Bound the amount of text a model can send to the TTS provider in one call.
/// This keeps provider cost, latency, and speaker occupancy predictable.
const MAX_SPEAK_CHARS: usize = 4_000;

/// Speak / play / record audio and control system volume / mute.
///
/// `record` captures through the shared input pipeline (same engine/STT as
/// user voice input) and returns the transcription.
/// `play` plays a `.wav` via WinMM `PlaySoundW`; `speak` synthesizes WAV audio
/// through the configured TTS client and plays it synchronously.
pub struct AudioTool {
    /// Shared capture/STT pipeline. `None` in headless/test contexts where
    /// recording is unavailable; the `record` operation then fails cleanly.
    pipeline: Option<Arc<InputPipeline>>,
    /// Shared TTS client. `None` means TTS is disabled or failed to initialize.
    tts: Option<Arc<dyn TtsClient>>,
    /// Injectable playback boundary keeps the tool testable without a speaker.
    playback: Arc<dyn AudioPlayback>,
}

/// Blocking speaker boundary used by the `speak` operation.
pub trait AudioPlayback: Send + Sync {
    fn play_wav(&self, data: &[u8]) -> anyhow::Result<()>;
}

struct SystemAudioPlayback;

impl AudioPlayback for SystemAudioPlayback {
    fn play_wav(&self, data: &[u8]) -> anyhow::Result<()> {
        imp::play_wav_bytes(data)
    }
}

/// Audio operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioOperation {
    Record,
    Play,
    Speak,
    VolumeGet,
    VolumeSet,
    MuteGet,
    MuteSet,
}

/// Typed parameters for `AudioTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `AudioTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct AudioParams {
    /// What to do with audio.
    pub operation: AudioOperation,
    /// Path to a `.wav` file for `play`.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Text to synthesize and play for `speak`.
    #[serde(default)]
    pub text: Option<String>,
    /// Recording duration in seconds (default 10, max 60).
    #[serde(default)]
    pub duration: Option<f64>,
    /// Master volume scalar in `[0.0, 1.0]` for `volume_set`.
    #[serde(default)]
    pub volume: Option<f64>,
    /// Mute state for `mute_set`.
    #[serde(default)]
    pub muted: Option<bool>,
}

impl AudioTool {
    pub fn new(pipeline: Option<Arc<InputPipeline>>) -> Self {
        Self::with_tts(pipeline, None)
    }

    pub fn with_tts(pipeline: Option<Arc<InputPipeline>>, tts: Option<Arc<dyn TtsClient>>) -> Self {
        Self {
            pipeline,
            tts,
            playback: Arc::new(SystemAudioPlayback),
        }
    }

    #[cfg(test)]
    fn with_playback(mut self, playback: Arc<dyn AudioPlayback>) -> Self {
        self.playback = playback;
        self
    }

    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: AudioParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        match params.operation {
            AudioOperation::Record => {
                let result = self.record(&params, cancel).await?;
                Ok(add_operation(result, "record"))
            }
            AudioOperation::Play => {
                let result = self.play(&params).await?;
                Ok(add_operation(result, "play"))
            }
            AudioOperation::Speak => {
                let result = self.speak(&params, cancel).await?;
                Ok(add_operation(result, "speak"))
            }
            AudioOperation::VolumeGet => {
                let volume = tokio::task::spawn_blocking(imp::get_volume).await??;
                Ok(ToolResult::ok(
                    serde_json::json!({ "operation": "volume_get", "volume": volume }),
                ))
            }
            AudioOperation::VolumeSet => {
                let level = params.volume.ok_or_else(|| {
                    anyhow::anyhow!("audio tool: volume (0.0–1.0) is required for volume_set")
                })?;
                let clamped = level.clamp(0.0, 1.0) as f32;
                tokio::task::spawn_blocking(move || imp::set_volume(clamped)).await??;
                Ok(ToolResult::ok(serde_json::json!({
                    "operation": "volume_set",
                    "volume": clamped,
                    "set": true,
                })))
            }
            AudioOperation::MuteGet => {
                let muted = tokio::task::spawn_blocking(imp::get_mute).await??;
                Ok(ToolResult::ok(
                    serde_json::json!({ "operation": "mute_get", "muted": muted }),
                ))
            }
            AudioOperation::MuteSet => {
                let muted = params.muted.ok_or_else(|| {
                    anyhow::anyhow!("audio tool: muted (boolean) is required for mute_set")
                })?;
                tokio::task::spawn_blocking(move || imp::set_mute(muted)).await??;
                Ok(ToolResult::ok(serde_json::json!({
                    "operation": "mute_set",
                    "muted": muted,
                    "set": true,
                })))
            }
        }
    }
}

fn add_operation(mut result: ToolResult, operation: &str) -> ToolResult {
    if let Some(object) = result.output.as_object_mut() {
        object.insert("operation".into(), Value::String(operation.into()));
    }
    result
}

#[async_trait]
impl Tool for AudioTool {
    fn name(&self) -> String {
        "audio".into()
    }

    fn description(&self) -> String {
        "Speak or play audio, record audio, and control system volume/mute: `speak` \
         uses the configured TTS provider to say text aloud when the user needs \
         an audible response; `record` captures \
         through the microphone and returns an STT transcript; `play` plays a \
         `.wav` file; \
         `volume_get`/`volume_set` (0.0–1.0) and `mute_get`/`mute_set` control \
         the default playback endpoint. Use `notify` for visual/system alerts, \
         not for speech."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("record") | Some("volume_set") | Some("mute_set") => RiskLevel::Medium,
            Some("play") | Some("speak") | Some("volume_get") | Some("mute_get") => RiskLevel::Low,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["play", "speak", "record", "volume_get", "volume_set", "mute_get", "mute_set"] },
                "file_path": { "type": "string" },
                "text": { "type": "string" },
                "duration": { "type": "number" },
                "volume": { "type": "number" },
                "muted": { "type": "boolean" }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "record" },
                        "duration": { "type": "number", "minimum": 1, "maximum": 60, "description": "Recording duration in seconds (default 10)" }
                    },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "play" },
                        "file_path": { "type": "string", "minLength": 1, "description": "Path to a .wav file" }
                    },
                    "required": ["operation", "file_path"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "speak" },
                        "text": { "type": "string", "minLength": 1, "maxLength": 4000, "description": "Text to synthesize and play aloud" }
                    },
                    "required": ["operation", "text"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "volume_get" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "volume_set" },
                        "volume": { "type": "number", "minimum": 0, "maximum": 1 }
                    },
                    "required": ["operation", "volume"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "mute_get" } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "mute_set" },
                        "muted": { "type": "boolean" }
                    },
                    "required": ["operation", "muted"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `AudioParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<AudioParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

impl AudioTool {
    /// Capture for `duration` seconds via the shared input pipeline, run STT
    /// and return the transcript. Never disturbs the user-facing recording
    /// UI: the pipeline's timed mode skips VAD auto-stop and handler
    /// notifications, and a user recording in flight is reported as an error.
    async fn record(
        &self,
        params: &AudioParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let Some(pipeline) = &self.pipeline else {
            return Err(anyhow::anyhow!(
                "audio tool: recording is unavailable in this context"
            ));
        };
        if !pipeline.recording_configured().await {
            return Err(anyhow::anyhow!(
                "audio tool: STT is not configured; enable an STT provider to record audio"
            ));
        }
        let duration = params
            .duration
            .unwrap_or(DEFAULT_RECORD_SECS)
            .clamp(1.0, MAX_RECORD_SECS);
        match pipeline.get_state().await {
            haven_input::RecordingState::Recording => {
                return Err(anyhow::anyhow!(
                    "audio tool: a recording is already in progress, try again later"
                ));
            }
            haven_input::RecordingState::Processing => {
                return Err(anyhow::anyhow!(
                    "audio tool: the previous recording is still processing, try again shortly"
                ));
            }
            haven_input::RecordingState::Pending => {}
        }

        let mut result = tokio::select! {
            r = pipeline.record_for(Duration::from_secs_f64(duration)) => r.map_err(|e| {
                anyhow::anyhow!("audio tool: recording failed: {e}")
            })?,
            _ = cancel.cancelled() => {
                // Release the microphone promptly; the partial capture is
                // discarded (nothing was delivered to the agent).
                let _ = pipeline.stop_capture().await;
                return Err(anyhow::anyhow!("audio tool: recording cancelled"));
            }
        };
        pipeline.transcribe(&mut result).await;

        if let Some(text) = result.transcript.filter(|t| !t.trim().is_empty()) {
            return Ok(ToolResult::ok(serde_json::json!({
                "transcript": text,
                "duration_ms": result.duration_ms,
            })));
        }
        let detail = result
            .transcript_error
            .unwrap_or_else(|| "no speech detected in the recording".into());
        Err(anyhow::anyhow!("audio tool: {detail}"))
    }

    async fn play(&self, params: &AudioParams) -> anyhow::Result<ToolResult> {
        let path = params
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow::anyhow!("audio tool: file_path (.wav) is required for play"))?;

        let lower = path.to_ascii_lowercase();
        if !lower.ends_with(".wav") {
            return Err(anyhow::anyhow!(
                "audio tool: play currently supports only .wav files (got '{}')",
                path
            ));
        }
        if !std::path::Path::new(path).is_file() {
            return Err(anyhow::anyhow!("audio tool: file not found: {}", path));
        }

        let path_owned = path.to_string();
        tokio::task::spawn_blocking(move || imp::play_wav(&path_owned)).await??;
        Ok(ToolResult::ok(serde_json::json!({
            "played": path,
            "format": "wav",
        })))
    }

    async fn speak(
        &self,
        params: &AudioParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let text = params
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| anyhow::anyhow!("audio tool: text is required for speak"))?;
        let character_count = text.chars().count();
        if character_count > MAX_SPEAK_CHARS {
            anyhow::bail!(
                "audio tool: speak text is too long (maximum {} characters)",
                MAX_SPEAK_CHARS
            );
        }
        let Some(tts) = &self.tts else {
            anyhow::bail!(
                "audio tool: TTS is not configured; choose a TTS-capable provider in Settings"
            );
        };
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let wav = tokio::select! {
            result = tts.synthesize_wav(text) => result
                .map_err(|e| anyhow::anyhow!("audio tool: TTS synthesis failed: {e}"))?,
            _ = cancel.cancelled() => anyhow::bail!("audio tool: speech synthesis cancelled"),
        };
        if cancel.is_cancelled() {
            anyhow::bail!("audio tool: speech playback cancelled");
        }

        let playback = self.playback.clone();
        tokio::task::spawn_blocking(move || playback.play_wav(&wav)).await??;
        Ok(ToolResult::ok(serde_json::json!({
            "spoken": true,
            "characters": character_count,
            "format": "wav",
            "delivered_to": ["speakers"],
        })))
    }
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{
        IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, PlaySoundW, SND_ASYNC, SND_FILENAME,
        SND_MEMORY, SND_SYNC, eConsole, eRender,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::core::{BOOL, PCWSTR};

    fn get_endpoint_volume() -> anyhow::Result<IAudioEndpointVolume> {
        // RPC_E_CHANGED_MODE (0x80010106) is fine if COM is already initialized
        // on this thread with a different apartment model.
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)? };

        let device: IMMDevice = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole)? };

        let ep: IAudioEndpointVolume = unsafe { device.Activate(CLSCTX_INPROC_SERVER, None)? };

        Ok(ep)
    }

    pub fn get_volume() -> anyhow::Result<f32> {
        let ep = get_endpoint_volume()?;
        let level: f32 = unsafe { ep.GetMasterVolumeLevelScalar()? };
        Ok(level)
    }

    pub fn set_volume(level: f32) -> anyhow::Result<()> {
        let level = level.clamp(0.0, 1.0);
        let ep = get_endpoint_volume()?;
        unsafe {
            ep.SetMasterVolumeLevelScalar(level, std::ptr::null())?;
        }
        Ok(())
    }

    pub fn get_mute() -> anyhow::Result<bool> {
        let ep = get_endpoint_volume()?;
        let muted: BOOL = unsafe { ep.GetMute()? };
        Ok(muted.as_bool())
    }

    pub fn set_mute(muted: bool) -> anyhow::Result<()> {
        let ep = get_endpoint_volume()?;
        unsafe {
            ep.SetMute(muted, std::ptr::null())?;
        }
        Ok(())
    }

    pub fn play_wav(path: &str) -> anyhow::Result<()> {
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let ok = unsafe { PlaySoundW(PCWSTR(wide.as_ptr()), None, SND_FILENAME | SND_ASYNC) };
        if !ok.as_bool() {
            anyhow::bail!("PlaySoundW failed for '{}'", path);
        }
        Ok(())
    }

    pub fn play_wav_bytes(data: &[u8]) -> anyhow::Result<()> {
        if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
            anyhow::bail!("audio playback requires a valid WAV payload");
        }
        // SND_SYNC keeps the borrowed provider buffer alive until WinMM has
        // finished reading and playing it. This also makes one `speak` call
        // complete only after the audible response has been delivered.
        let ok = unsafe {
            PlaySoundW(
                PCWSTR(data.as_ptr() as *const u16),
                None,
                SND_MEMORY | SND_SYNC,
            )
        };
        if !ok.as_bool() {
            anyhow::bail!("PlaySoundW failed for synthesized audio");
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn get_volume() -> anyhow::Result<f32> {
        Ok(0.0)
    }

    pub fn set_volume(_level: f32) -> anyhow::Result<()> {
        anyhow::bail!("audio control requires Windows")
    }

    pub fn get_mute() -> anyhow::Result<bool> {
        Ok(false)
    }

    pub fn set_mute(_muted: bool) -> anyhow::Result<()> {
        anyhow::bail!("audio control requires Windows")
    }

    pub fn play_wav(_path: &str) -> anyhow::Result<()> {
        anyhow::bail!("audio playback requires Windows")
    }

    pub fn play_wav_bytes(_data: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("audio playback requires Windows")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_audio_tool_name() {
        assert_eq!(AudioTool::new(None).name(), "audio");
    }

    #[test]
    fn test_audio_tool_description() {
        let desc = AudioTool::new(None).description();
        assert!(desc.contains("audio"));
        assert!(desc.contains("volume"));
    }

    #[test]
    fn test_audio_tool_risk_level() {
        let tool = AudioTool::new(None);
        assert_eq!(
            tool.risk_level(&json!({"operation": "play"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "record"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "volume_set"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "volume_get"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "mute_set"})),
            RiskLevel::Medium
        );
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::Low);
    }

    #[test]
    fn test_audio_tool_input_schema() {
        let schema = AudioTool::new(None).input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        for expected in [
            "play",
            "speak",
            "record",
            "volume_get",
            "volume_set",
            "mute_get",
            "mute_set",
        ] {
            assert!(ops.contains(&expected), "missing op {expected}");
        }
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"operation"));
        assert!(schema["properties"]["file_path"]["type"].as_str().is_some());
        assert!(schema["properties"]["text"]["type"].as_str().is_some());
        assert!(schema["properties"]["duration"]["type"].as_str().is_some());
        assert!(schema["properties"]["volume"]["type"].as_str().is_some());
        assert!(schema["properties"]["muted"]["type"].as_str().is_some());
        assert_eq!(schema["oneOf"].as_array().unwrap().len(), 7);
    }

    #[tokio::test]
    async fn test_audio_execute_play_requires_wav() {
        let result = AudioTool::new(None)
            .execute(
                json!({"operation": "play", "file_path": "x.mp3"}),
                CancellationToken::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains(".wav"));
    }

    #[tokio::test]
    async fn test_audio_execute_play_rejects_removed_text_param() {
        let result = AudioTool::new(None)
            .execute(
                json!({"operation": "play", "text": "hello"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_audio_execute_record_without_pipeline() {
        let result = AudioTool::new(None)
            .execute(json!({"operation": "record"}), CancellationToken::new())
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn test_audio_execute_cancelled_record_without_pipeline() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = AudioTool::new(None)
            .execute(json!({"operation": "record"}), cancel)
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("cancelled") || err.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn test_audio_execute_unknown_operation() {
        let result = AudioTool::new(None)
            .execute(json!({"operation": "record_x"}), CancellationToken::new())
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unknown variant `record_x`"));
    }

    #[tokio::test]
    async fn test_audio_native_entry_lands_in_run() {
        let result = AudioTool::new(None)
            .run(
                AudioParams {
                    operation: AudioOperation::Record,
                    file_path: None,
                    text: None,
                    duration: None,
                    volume: None,
                    muted: None,
                },
                CancellationToken::new(),
            )
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn test_audio_volume_get_set_roundtrip() {
        let tool = AudioTool::new(None);
        let get = tool
            .execute(json!({"operation": "volume_get"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(get.success);
        let vol = get.output["volume"].as_f64().unwrap();
        assert!((0.0..=1.0).contains(&vol));

        let set = tool
            .execute(
                json!({"operation": "volume_set", "volume": vol}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(set.success);
        assert_eq!(set.output["set"], true);
    }

    #[tokio::test]
    async fn test_audio_mute_get() {
        let result = AudioTool::new(None)
            .execute(json!({"operation": "mute_get"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["muted"].is_boolean());
    }

    #[tokio::test]
    async fn test_audio_volume_set_requires_value() {
        let err = AudioTool::new(None)
            .execute(json!({"operation": "volume_set"}), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("volume"));
    }

    #[tokio::test]
    async fn test_audio_mute_set_requires_value() {
        let err = AudioTool::new(None)
            .execute(json!({"operation": "mute_set"}), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("muted"));
    }

    struct MockTts;

    #[async_trait]
    impl TtsClient for MockTts {
        async fn synthesize(&self, _text: &str) -> anyhow::Result<Vec<u8>> {
            Ok(b"encoded".to_vec())
        }

        async fn synthesize_wav(&self, _text: &str) -> anyhow::Result<Vec<u8>> {
            Ok(b"RIFF\x24\x00\x00\x00WAVEfake".to_vec())
        }
    }

    struct MockPlayback {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl AudioPlayback for MockPlayback {
        fn play_wav(&self, data: &[u8]) -> anyhow::Result<()> {
            assert!(data.starts_with(b"RIFF"));
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_audio_speak_synthesizes_and_plays_wav() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool = AudioTool::with_tts(None, Some(Arc::new(MockTts))).with_playback(Arc::new(
            MockPlayback {
                calls: calls.clone(),
            },
        ));
        let result = tool
            .execute(
                serde_json::json!({"operation": "speak", "text": "你好"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["operation"], "speak");
        assert_eq!(result.output["spoken"], true);
        assert_eq!(result.output["characters"], 2);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_audio_speak_requires_tts_configuration() {
        let err = AudioTool::new(None)
            .execute(
                serde_json::json!({"operation": "speak", "text": "hello"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("TTS"));
    }

    #[tokio::test]
    async fn test_audio_speak_rejects_empty_or_oversized_text() {
        let tool = AudioTool::with_tts(None, Some(Arc::new(MockTts)));
        let empty = tool
            .execute(
                serde_json::json!({"operation": "speak", "text": "  "}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(empty.to_string().contains("text"));

        let too_long = "x".repeat(MAX_SPEAK_CHARS + 1);
        let long = tool
            .execute(
                serde_json::json!({"operation": "speak", "text": too_long}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(long.to_string().contains("too long"));
    }
}
