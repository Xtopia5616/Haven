use haven_common::config::default_generated_media_dir;
use haven_input::InputPipeline;
use haven_llm::TtsClient;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use super::media::{MediaParams, register_generated_asset};
use crate::{ManagedAsset, ManagedAssetRegistry};

/// Default capture window when the LLM omits `duration`.
const DEFAULT_RECORD_SECS: f64 = 10.0;
/// Hard cap: the microphone belongs to the user first, and a runaway tool
/// call must not monopolize it (the pipeline's own `max_duration_secs`
/// config still applies as a final bound).
const MAX_RECORD_SECS: f64 = 60.0;
/// Bound the amount of text a model can send to the TTS provider in one call.
/// This keeps provider cost, latency, and speaker occupancy predictable.
const MAX_SPEAK_CHARS: usize = 4_000;

/// Host-side audio device runtime used by the public `media` tool.
///
/// The model-facing contract is owned by `MediaTool`; this type keeps the
/// platform/device boundary isolated so merging the public operations does not
/// merge Windows audio plumbing into managed-asset orchestration.
pub(crate) struct AudioRuntime {
    /// Shared capture/STT pipeline. `None` in headless/test contexts where
    /// recording is unavailable; the `record` operation then fails cleanly.
    pipeline: Option<Arc<InputPipeline>>,
    /// Shared TTS client. `None` means TTS is disabled or failed to initialize.
    tts: Option<Arc<dyn TtsClient>>,
    /// Injectable playback boundary keeps the tool testable without a speaker.
    playback: Arc<dyn AudioPlayback>,
    /// Host-owned registry used by `record` to return a reusable asset.
    managed_assets: ManagedAssetRegistry,
    /// Dedicated generated-media root for recordings.
    capture_root: PathBuf,
    /// Whether microphone capture is wired in this runtime.
    record_available: bool,
}

#[derive(Debug)]
pub(crate) struct RecordedAudio {
    pub asset: ManagedAsset,
    pub duration_ms: u64,
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

impl AudioRuntime {
    #[cfg(test)]
    pub(crate) fn new(pipeline: Option<Arc<InputPipeline>>) -> Self {
        Self::with_tts(pipeline, None)
    }

    pub(crate) fn with_tts(
        pipeline: Option<Arc<InputPipeline>>,
        tts: Option<Arc<dyn TtsClient>>,
    ) -> Self {
        let record_available = pipeline.is_some();
        Self {
            pipeline,
            tts,
            playback: Arc::new(SystemAudioPlayback),
            managed_assets: ManagedAssetRegistry::default(),
            capture_root: default_generated_media_dir(),
            record_available,
        }
    }

    pub(crate) fn with_managed_assets(mut self, managed_assets: ManagedAssetRegistry) -> Self {
        self.managed_assets = managed_assets;
        self
    }

    pub(crate) fn with_capabilities(mut self, record_available: bool) -> Self {
        self.record_available = record_available;
        self
    }

    pub(crate) fn record_available(&self) -> bool {
        self.record_available
    }

    pub(crate) fn tts_available(&self) -> bool {
        self.tts.is_some()
    }

    #[cfg(test)]
    fn with_playback(mut self, playback: Arc<dyn AudioPlayback>) -> Self {
        self.playback = playback;
        self
    }

    /// Capture for `duration` seconds via the shared input pipeline and return
    /// the registered WAV asset. Never disturbs the user-facing recording UI:
    /// the pipeline's timed mode skips VAD auto-stop and handler notifications,
    /// and a user recording in flight is reported as an error.
    pub(crate) async fn record_asset(
        &self,
        params: &MediaParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<RecordedAudio> {
        let Some(pipeline) = &self.pipeline else {
            return Err(anyhow::anyhow!(
                "media record: recording is unavailable in this context"
            ));
        };
        if !self.record_available {
            return Err(anyhow::anyhow!(
                "media record: recording is unavailable in this context"
            ));
        }
        let duration = params
            .duration
            .unwrap_or(DEFAULT_RECORD_SECS)
            .clamp(1.0, MAX_RECORD_SECS);
        match pipeline.get_state().await {
            haven_input::RecordingState::Recording => {
                return Err(anyhow::anyhow!(
                    "media record: a recording is already in progress, try again later"
                ));
            }
            haven_input::RecordingState::Processing => {
                return Err(anyhow::anyhow!(
                    "media record: the previous recording is still processing, try again shortly"
                ));
            }
            haven_input::RecordingState::Pending => {}
        }

        let result = tokio::select! {
            r = pipeline.record_for(Duration::from_secs_f64(duration)) => r.map_err(|e| {
                anyhow::anyhow!("media record: recording failed: {e}")
            })?,
            _ = cancel.cancelled() => {
                // Release the microphone promptly; the partial capture is
                // discarded (nothing was delivered to the agent).
                let _ = pipeline.stop_capture().await;
                return Err(anyhow::anyhow!("media record: recording cancelled"));
            }
        };
        // Recording is a producer. Persist the captured WAV before the shared
        // media runtime attempts STT so a failed or unconfigured transcription
        // still leaves a reusable asset for a later `media.transcribe` call.
        let wav = pipeline.encode_wav(&result.pcm).await?;
        tokio::fs::create_dir_all(&self.capture_root).await?;
        let path = self
            .capture_root
            .join(format!("{}.wav", haven_common::types::new_id("file")));
        if let Err(error) = tokio::fs::write(&path, &wav).await {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error.into());
        }
        let asset = match register_generated_asset(
            &self.managed_assets,
            params.session_id.as_deref(),
            &self.capture_root,
            path.clone(),
            Some("recording.wav".into()),
            "audio/wav",
            wav.len() as u64,
        ) {
            Ok(asset) => asset,
            Err(error) => {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(error);
            }
        };

        Ok(RecordedAudio {
            asset,
            duration_ms: result.duration_ms,
        })
    }

    pub(crate) async fn play(&self, params: &MediaParams) -> anyhow::Result<()> {
        let path = params
            .file_path
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow::anyhow!("media play: file_path (.wav) is required"))?;

        let lower = path.to_ascii_lowercase();
        if !lower.ends_with(".wav") {
            return Err(anyhow::anyhow!(
                "media play: only .wav files are supported (got '{}')",
                path
            ));
        }
        if !std::path::Path::new(path).is_file() {
            return Err(anyhow::anyhow!("media play: file not found: {}", path));
        }

        let path_owned = path.to_string();
        tokio::task::spawn_blocking(move || imp::play_wav(&path_owned)).await??;
        Ok(())
    }

    pub(crate) async fn speak(
        &self,
        params: &MediaParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<usize> {
        let text = params
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| anyhow::anyhow!("media speak: text is required"))?;
        let character_count = text.chars().count();
        if character_count > MAX_SPEAK_CHARS {
            anyhow::bail!(
                "media speak: text is too long (maximum {} characters)",
                MAX_SPEAK_CHARS
            );
        }
        let Some(tts) = &self.tts else {
            anyhow::bail!(
                "media speak: TTS is not configured; choose a TTS-capable provider in Settings"
            );
        };
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let wav = tokio::select! {
            result = tts.synthesize_wav(text) => result
                .map_err(|e| anyhow::anyhow!("media speak: TTS synthesis failed: {e}"))?,
            _ = cancel.cancelled() => anyhow::bail!("media speak: speech synthesis cancelled"),
        };
        if cancel.is_cancelled() {
            anyhow::bail!("media speak: speech playback cancelled");
        }

        let playback = self.playback.clone();
        tokio::task::spawn_blocking(move || playback.play_wav(&wav)).await??;
        Ok(character_count)
    }

    pub(crate) async fn volume_get(&self) -> anyhow::Result<f32> {
        tokio::task::spawn_blocking(imp::get_volume).await?
    }

    pub(crate) async fn volume_set(&self, volume: f64) -> anyhow::Result<f32> {
        let clamped = volume.clamp(0.0, 1.0) as f32;
        tokio::task::spawn_blocking(move || imp::set_volume(clamped)).await??;
        Ok(clamped)
    }

    pub(crate) async fn mute_get(&self) -> anyhow::Result<bool> {
        tokio::task::spawn_blocking(imp::get_mute).await?
    }

    pub(crate) async fn mute_set(&self, muted: bool) -> anyhow::Result<()> {
        tokio::task::spawn_blocking(move || imp::set_mute(muted)).await??;
        Ok(())
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
    use super::super::media::MediaOperation;
    use super::*;
    use async_trait::async_trait;

    fn params(operation: MediaOperation) -> MediaParams {
        MediaParams {
            operation,
            asset_id: None,
            focus: None,
            prompt: None,
            page_index: None,
            file_path: None,
            text: None,
            duration: None,
            volume: None,
            muted: None,
            session_id: None,
        }
    }

    #[tokio::test]
    async fn runtime_rejects_non_wav_playback() {
        let mut params = params(MediaOperation::Play);
        params.file_path = Some("x.mp3".into());
        let err = AudioRuntime::new(None).play(&params).await.unwrap_err();
        assert!(err.to_string().contains(".wav"));
    }

    #[tokio::test]
    async fn runtime_rejects_recording_without_pipeline() {
        let err = AudioRuntime::new(None)
            .record_asset(&params(MediaOperation::Record), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unavailable"));
    }

    #[tokio::test]
    async fn runtime_rejects_cancelled_speech() {
        let mut params = params(MediaOperation::Speak);
        params.text = Some("hello".into());
        let cancel = CancellationToken::new();
        cancel.cancel();
        let err = AudioRuntime::with_tts(None, Some(Arc::new(MockTts)))
            .speak(&params, cancel)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn runtime_speak_synthesizes_and_plays_wav() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runtime = AudioRuntime::with_tts(None, Some(Arc::new(MockTts))).with_playback(
            Arc::new(MockPlayback {
                calls: calls.clone(),
            }),
        );
        let mut params = params(MediaOperation::Speak);
        params.text = Some("你好".into());
        let characters = runtime
            .speak(&params, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(characters, 2);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn runtime_speech_requires_tts_configuration() {
        let mut params = params(MediaOperation::Speak);
        params.text = Some("hello".into());
        let err = AudioRuntime::new(None)
            .speak(&params, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("TTS"));
    }

    #[tokio::test]
    async fn runtime_speech_rejects_empty_or_oversized_text() {
        let runtime = AudioRuntime::with_tts(None, Some(Arc::new(MockTts)));
        let mut empty = params(MediaOperation::Speak);
        empty.text = Some("  ".into());
        let err = runtime
            .speak(&empty, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("text"));

        let mut too_long = params(MediaOperation::Speak);
        too_long.text = Some("x".repeat(MAX_SPEAK_CHARS + 1));
        let err = runtime
            .speak(&too_long, CancellationToken::new())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too long"));
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
}
