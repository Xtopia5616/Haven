//! Input pipeline: recording orchestration, VAD and transcription.
//!
//! The pipeline owns the high-level recording state machine; the actual
//! audio capture lives in [`capture`] (the capture engine thread + CPAL
//! backend).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use capture::{EngineHandle, TARGET_SAMPLE_RATE};
use haven_common::SttClient;

pub use haven_common::config::AudioConfig;

pub mod capture;
pub mod vad;
mod wav;

pub use wav::encode_wav_to_vec;

const VAD_FRAME_SAMPLES: usize = 480;
const VAD_THROTTLE_INTERVAL: Duration = Duration::from_millis(100);
const RECORDING_LOOP_INTERVAL: Duration = Duration::from_millis(30);

/// Unified input-pipeline hook surface. Both methods have no-op defaults, so
/// an implementation only overrides the hooks it needs.
#[async_trait]
pub trait InputHandler: Send + Sync {
    /// Fired (sync) on each VAD signal/state transition. May be throttled by
    /// the pipeline before delivery.
    fn on_vad_status(&self, _signal: vad::VadSignal, _state: vad::VadState) {}
    /// Fired (async) when VAD detects end-of-speech or the max recording
    /// duration is reached, before the recording result is finalized.
    async fn on_auto_stop(&self) {}
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecordingState {
    Pending,
    Recording,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecordingReason {
    Manual,
    Silence,
    MaxDuration,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct RecordingResult {
    pub pcm: Vec<f32>,
    pub reason: RecordingReason,
    pub duration_ms: u64,
    pub transcript: Option<String>,
    /// When transcription was attempted but failed, the underlying error
    /// message. Surfaced to the UI instead of a generic failure notice.
    pub transcript_error: Option<String>,
}

pub struct InputPipeline {
    config: Arc<Mutex<AudioConfig>>,
    state: Mutex<RecordingState>,
    engine: Arc<StdMutex<Option<EngineHandle>>>,
    vad_engine: Arc<StdMutex<Option<vad::VadEngine>>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    handler: Arc<StdMutex<Option<Arc<dyn InputHandler>>>>,
    cancel_token: StdMutex<Option<CancellationToken>>,
    result_rx: StdMutex<Option<tokio::sync::oneshot::Receiver<RecordingResult>>>,
    stt_client: Arc<Mutex<Option<Box<dyn SttClient>>>>,
}

impl InputPipeline {
    pub fn new() -> Self {
        let vad_detector = vad::VadDetector::new(0.5, 1500);
        Self {
            config: Arc::new(Mutex::new(AudioConfig::default())),
            state: Mutex::new(RecordingState::Pending),
            engine: Arc::new(StdMutex::new(None)),
            vad_engine: Arc::new(StdMutex::new(None)),
            vad_detector: Arc::new(Mutex::new(vad_detector)),
            handler: Arc::new(StdMutex::new(None)),
            cancel_token: StdMutex::new(None),
            result_rx: StdMutex::new(None),
            stt_client: Arc::new(Mutex::new(None)),
        }
    }
}

impl Default for InputPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl InputPipeline {
    /// Install the unified input handler.
    pub fn set_handler(&self, handler: Arc<dyn InputHandler>) {
        *self.handler.lock().expect("handler lock poisoned") = Some(handler);
    }

    /// Install or clear the STT client. `None` disables transcription
    /// (e.g. when the provider is set to `none` at runtime).
    pub async fn set_stt_client(&self, client: Option<Box<dyn SttClient>>) {
        *self.stt_client.lock().await = client;
    }

    /// Start the capture engine at app startup so the first recording pays no
    /// engine-spawn latency. No capture stream is opened here (the microphone
    /// is only claimed while recording); `start_recording` opens the stream
    /// itself. The VAD model is also preloaded here (off the async runtime
    /// thread) so the first recording does not stall on ONNX graph
    /// compilation.
    pub async fn prewarm(&self) {
        let mut guard = self.engine.lock().expect("engine lock poisoned");
        if guard.is_some() {
            return;
        }
        match capture::spawn_engine() {
            Ok(h) => {
                *guard = Some(h);
                tracing::debug!("audio capture engine prewarmed");
            }
            Err(e) => {
                tracing::warn!("audio capture engine prewarm failed: {e}");
            }
        }
        drop(guard);

        let vad_engine = self.vad_engine.clone();
        tokio::spawn(async move {
            let loaded = tokio::task::spawn_blocking(vad::VadEngine::new).await;
            match loaded {
                Ok(Ok(e)) => {
                    *vad_engine.lock().expect("vad_engine lock poisoned") = Some(e);
                    tracing::debug!("VAD engine preloaded");
                }
                Ok(Err(err)) => tracing::warn!("VAD engine preload failed: {err}"),
                Err(_) => tracing::warn!("VAD engine preload task panicked"),
            }
        });
    }

    pub async fn get_vad_state(&self) -> vad::VadState {
        self.vad_detector.lock().await.state()
    }

    pub async fn start_recording(&self) -> Result<()> {
        {
            let mut state = self.state.lock().await;
            if *state != RecordingState::Pending {
                return Err(anyhow!("already recording or processing"));
            }
            *state = RecordingState::Recording;
        }

        // Ensure the engine is running (prewarm may have been skipped).
        let handle = {
            let existing = self.engine.lock().expect("engine lock poisoned").clone();
            match existing {
                Some(h) => h,
                None => match capture::spawn_engine() {
                    Ok(h) => {
                        *self.engine.lock().expect("engine lock poisoned") = Some(h.clone());
                        h
                    }
                    Err(e) => {
                        *self.state.lock().await = RecordingState::Pending;
                        return Err(e);
                    }
                },
            }
        };

        // Open the capture stream (see `capture` docs).
        if let Err(e) = handle.start().await {
            *self.state.lock().await = RecordingState::Pending;
            return Err(e);
        }

        {
            self.vad_detector.lock().await.reset();
            // Reuse the prewarmed VAD engine; only load on demand (first
            // recording after a failed prewarm). The model stays resident
            // across recordings — graph compilation is the slow part, and a
            // fresh recording only needs the recurrent state reset.
            let needs_load = {
                let guard = self.vad_engine.lock().expect("vad_engine lock poisoned");
                guard.is_none()
            };
            if needs_load {
                let loaded = tokio::task::spawn_blocking(vad::VadEngine::new).await;
                let mut eng_guard = self.vad_engine.lock().expect("vad_engine lock poisoned");
                match loaded {
                    Ok(Ok(e)) => *eng_guard = Some(e),
                    Ok(Err(err)) => {
                        tracing::warn!("VAD engine init failed, VAD disabled: {err}");
                        *eng_guard = None;
                    }
                    Err(_) => {
                        tracing::warn!("VAD engine init task panicked, VAD disabled");
                        *eng_guard = None;
                    }
                }
                if let Some(e) = eng_guard.as_mut() {
                    e.reset();
                }
            } else {
                let mut eng_guard = self.vad_engine.lock().expect("vad_engine lock poisoned");
                if let Some(e) = eng_guard.as_mut() {
                    e.reset();
                }
            }
        }

        let cancel = CancellationToken::new();
        *self
            .cancel_token
            .lock()
            .expect("cancel_token lock poisoned") = Some(cancel.clone());

        let (tx, rx) = tokio::sync::oneshot::channel();
        *self.result_rx.lock().expect("result_rx lock poisoned") = Some(rx);

        let loop_data = LoopData {
            config: self.config.clone(),
            engine: self.engine.clone(),
            vad_engine: self.vad_engine.clone(),
            vad_detector: self.vad_detector.clone(),
            handler: self.handler.clone(),
            failed: handle.stream_failed.clone(),
            silent_abort: handle.silent_abort.clone(),
        };
        tokio::spawn(async move {
            let result = Self::recording_loop(loop_data, cancel).await;
            let _ = tx.send(result);
        });

        tracing::debug!("Recording started (cpal capture + VAD loop)");
        Ok(())
    }

    async fn recording_loop(data: LoopData, cancel: CancellationToken) -> RecordingResult {
        let start = std::time::Instant::now();
        let max_duration = {
            let config = data.config.lock().await;
            Duration::from_secs(config.max_duration_secs)
        };

        let capture_handle = {
            let guard = data.engine.lock().expect("engine lock poisoned");
            guard.as_ref().cloned()
        };

        let mut accumulated_pcm: Vec<f32> = Vec::new();
        let mut vad_partial: Vec<f32> = Vec::new();
        let mut last_vad_status = std::time::Instant::now();

        loop {
            if cancel.is_cancelled() {
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                    transcript: None,
                    transcript_error: None,
                };
            }

            // Fatal stream error (device lost): stop early instead of
            // recording silence until max_duration.
            if data.failed.load(Ordering::SeqCst) {
                tracing::warn!("audio capture stream failed; stopping recording early");
                let elapsed = start.elapsed();
                let h = data.handler.lock().expect("handler lock poisoned").clone();
                if let Some(h) = h {
                    tokio::spawn(async move {
                        h.on_auto_stop().await;
                    });
                }
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                    transcript: None,
                    transcript_error: None,
                };
            }

            // The engine aborted the recording: the capture delivered pure
            // digital silence for the opening window. Stop immediately — the
            // transcribe step reports the error and the request is never sent.
            if data.silent_abort.load(Ordering::SeqCst) {
                tracing::warn!("recording aborted: capture delivered no signal");
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                    transcript: None,
                    transcript_error: None,
                };
            }

            if start.elapsed() >= max_duration {
                let h = data.handler.lock().expect("handler lock poisoned").clone();
                if let Some(h) = h {
                    tokio::spawn(async move {
                        h.on_auto_stop().await;
                    });
                }
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::MaxDuration,
                    duration_ms: elapsed.as_millis() as u64,
                    transcript: None,
                    transcript_error: None,
                };
            }

            let new_data = match &capture_handle {
                Some(handle) => handle.drain_shared(),
                None => Vec::new(),
            };

            if !new_data.is_empty() {
                if accumulated_pcm.is_empty() && vad_partial.is_empty() {
                    tracing::debug!(
                        "recording_loop: first audio chunk ({} samples)",
                        new_data.len()
                    );
                }
                accumulated_pcm.extend_from_slice(&new_data);

                let mut vad_input = Vec::new();
                std::mem::swap(&mut vad_partial, &mut vad_input);
                vad_input.extend_from_slice(&new_data);

                let mut offset = 0;
                while offset + VAD_FRAME_SAMPLES <= vad_input.len() {
                    // Cancellation wins over VAD latency: each inference runs
                    // on the blocking pool so a slow model (tract in debug
                    // builds) cannot stall the stop path.
                    if cancel.is_cancelled() {
                        let elapsed = start.elapsed();
                        return RecordingResult {
                            pcm: accumulated_pcm,
                            reason: RecordingReason::Manual,
                            duration_ms: elapsed.as_millis() as u64,
                            transcript: None,
                            transcript_error: None,
                        };
                    }
                    let frame = &vad_input[offset..offset + VAD_FRAME_SAMPLES];
                    offset += VAD_FRAME_SAMPLES;

                    let prob = {
                        let engine = data.vad_engine.clone();
                        let frame_owned = frame.to_vec();
                        tokio::select! {
                            res = tokio::task::spawn_blocking(move || {
                                let mut guard = engine.lock().expect("vad_engine lock poisoned");
                                match guard.as_mut() {
                                    Some(e) => e.infer(&frame_owned),
                                    None => 0.0,
                                }
                            }) => res.unwrap_or(0.0),
                            _ = cancel.cancelled() => {
                                let elapsed = start.elapsed();
                                return RecordingResult {
                                    pcm: accumulated_pcm,
                                    reason: RecordingReason::Manual,
                                    duration_ms: elapsed.as_millis() as u64,
                                    transcript: None,
                                    transcript_error: None,
                                };
                            }
                        }
                    };

                    let signal = {
                        let mut det = data.vad_detector.lock().await;
                        let signal = det.process(prob);
                        let state = det.state();
                        if last_vad_status.elapsed() >= VAD_THROTTLE_INTERVAL {
                            if let Some(h) =
                                data.handler.lock().expect("handler lock poisoned").clone()
                            {
                                h.on_vad_status(signal, state);
                            }
                            last_vad_status = std::time::Instant::now();
                        }
                        signal
                    };

                    if signal == vad::VadSignal::AutoStop {
                        let h = data.handler.lock().expect("handler lock poisoned").clone();
                        if let Some(h) = h {
                            tokio::spawn(async move {
                                h.on_auto_stop().await;
                            });
                        }
                        let elapsed = start.elapsed();
                        return RecordingResult {
                            pcm: accumulated_pcm,
                            reason: RecordingReason::Silence,
                            duration_ms: elapsed.as_millis() as u64,
                            transcript: None,
                            transcript_error: None,
                        };
                    }
                }

                if offset < vad_input.len() {
                    vad_partial = vad_input[offset..].to_vec();
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(RECORDING_LOOP_INTERVAL) => {}
                _ = cancel.cancelled() => {}
            }
        }
    }

    pub async fn cancel_recording(&self) -> Result<()> {
        // Capture the cancel_token and result_rx belonging to the current
        // recording BEFORE setting state to Pending (see stop_capture_inner
        // for the same ordering rationale).
        let token;
        let rx;
        {
            let mut state = self.state.lock().await;
            if *state != RecordingState::Recording && *state != RecordingState::Processing {
                return Ok(());
            }
            token = self
                .cancel_token
                .lock()
                .expect("cancel_token lock poisoned")
                .take();
            rx = self
                .result_rx
                .lock()
                .expect("result_rx lock poisoned")
                .take();
            *state = RecordingState::Pending;
        }

        if let Some(token) = token {
            token.cancel();
        }

        if let Some(rx) = rx {
            let _ = rx.await;
        }

        if let Some(ref handle) = *self.engine.lock().expect("engine lock poisoned") {
            handle.stop_and_clear();
        }
        // The VAD engine stays resident across recordings (state was reset at
        // the next start); releasing it here would re-pay ONNX graph
        // compilation on the next recording.
        self.vad_detector.lock().await.reset();

        tracing::debug!("Recording cancelled");
        Ok(())
    }

    /// Stop the audio capture and return the captured PCM. Runs no STT and
    /// leaves `transcript`/`transcript_error` unset.
    pub async fn stop_capture(&self) -> Result<RecordingResult> {
        let result = self.stop_capture_inner().await?;
        *self.state.lock().await = RecordingState::Pending;
        Ok(result)
    }

    /// Run STT on a previously-captured result, mutating `transcript` /
    /// `transcript_error` in place. Safe to call after `stop_capture`.
    pub async fn transcribe(&self, result: &mut RecordingResult) {
        if result.pcm.is_empty() {
            return;
        }
        // Diagnostic: report how much audio was captured and whether the
        // opening seconds actually carry signal.
        let head_secs = 5u64.min(result.duration_ms / 1000 + 1);
        let head_samples = (head_secs as usize * TARGET_SAMPLE_RATE as usize).min(result.pcm.len());
        let head = &result.pcm[..head_samples];
        let rms = (head.iter().map(|s| s * s).sum::<f32>() / head.len() as f32).sqrt();
        tracing::info!(
            "captured audio: {:.1}s, {} samples, head({}s) RMS={:.4}",
            result.duration_ms as f64 / 1000.0,
            result.pcm.len(),
            head_secs,
            rms
        );

        // Silent-capture guard: if the whole recording is below the signal
        // floor, the microphone delivered nothing (muted / disabled / dead
        // effects chain). Surface an explicit error so the caller shows it
        // and does NOT submit a request — an all-zero clip must never
        // silently vanish or be transcribed. The threshold is set above a
        // quiet room's noise floor (~-80 dBFS) so only true silence trips it.
        if result.duration_ms >= 200 {
            let total_rms = (result.pcm.iter().map(|s| s * s).sum::<f32>()
                / result.pcm.len() as f32)
                .sqrt();
            if total_rms < 1e-4 {
                let msg = "麦克风没有检测到声音，请检查系统麦克风是否被静音或已禁用".to_string();
                tracing::error!("captured audio is digital silence (RMS={total_rms:.6}): {msg}");
                result.transcript_error = Some(msg);
                return;
            }
        }

        let stt_guard = self.stt_client.lock().await;
        if let Some(ref client) = *stt_guard {
            // `result.pcm` is always the resampled mono stream at
            // TARGET_SAMPLE_RATE.
            let wav = encode_wav_to_vec(&result.pcm, TARGET_SAMPLE_RATE, 1);

            match client.transcribe(&wav).await {
                Ok(text) => {
                    if !text.trim().is_empty() {
                        result.transcript = Some(text);
                    } else {
                        tracing::warn!(
                            "STT returned an empty transcription ({}s of audio); skipping — no speech detected",
                            result.duration_ms / 1000
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!("STT transcription failed: {}", e);
                    result.transcript_error = Some(e.to_string());
                }
            }
        } else {
            result.transcript_error = Some(
                "未配置 STT 服务（设置 → STT Provider 选择 MCP Server 或 LLM Adapter）".into(),
            );
        }
    }

    /// Backwards-compatible single-call stop: captures audio and runs STT
    /// inline. New callers should prefer `stop_capture` + `transcribe`.
    pub async fn stop_recording(&self) -> Result<RecordingResult> {
        let mut result = self.stop_capture().await?;
        self.transcribe(&mut result).await;
        tracing::debug!(
            "Recording stopped, {} samples, reason={:?}, transcript={}",
            result.pcm.len(),
            result.reason,
            result.transcript.is_some(),
        );
        Ok(result)
    }

    async fn stop_capture_inner(&self) -> Result<RecordingResult> {
        let mut state = self.state.lock().await;
        let prev_state = std::mem::replace(&mut *state, RecordingState::Processing);
        if prev_state != RecordingState::Recording {
            *state = prev_state;
            return Err(anyhow!("not recording"));
        }
        drop(state);

        let token = self
            .cancel_token
            .lock()
            .expect("cancel_token lock poisoned")
            .take();
        if let Some(token) = token {
            token.cancel();
        }

        let mut result = {
            let rx = self
                .result_rx
                .lock()
                .expect("result_rx lock poisoned")
                .take();
            match rx {
                Some(rx) => match rx.await {
                    Ok(inner) => inner,
                    Err(_) => RecordingResult {
                        pcm: Vec::new(),
                        reason: RecordingReason::Manual,
                        duration_ms: 0,
                        transcript: None,
                        transcript_error: None,
                    },
                },
                None => RecordingResult {
                    pcm: Vec::new(),
                    reason: RecordingReason::Manual,
                    duration_ms: 0,
                    transcript: None,
                    transcript_error: None,
                },
            }
        };

        // Capture the tail that accumulated between the loop's last drain and
        // the stream teardown.
        let handle = self.engine.lock().expect("engine lock poisoned").clone();
        if let Some(handle) = handle {
            let remaining = handle.stop_and_drain().await?;
            if !remaining.is_empty() {
                result.pcm.extend_from_slice(&remaining);
                result.duration_ms =
                    (result.pcm.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64;
            }
        }

        // The VAD engine stays resident across recordings (state was reset at
        // the next start); releasing it here would re-pay ONNX graph
        // compilation on the next recording.
        self.vad_detector.lock().await.reset();

        Ok(result)
    }

    /// Encode pipeline PCM as WAV (16 kHz mono 16-bit).
    pub async fn encode_wav(&self, pcm_data: &[f32]) -> Result<Vec<u8>> {
        Ok(encode_wav_to_vec(pcm_data, TARGET_SAMPLE_RATE, 1))
    }

    pub async fn get_state(&self) -> RecordingState {
        self.state.lock().await.clone()
    }

    /// Apply a new audio configuration: `max_duration_secs` is read by the
    /// recording loop; `silence_timeout_ms` and `vad_threshold` are
    /// propagated to the VAD detector.
    pub async fn update_config(&self, config: AudioConfig) {
        let vad_threshold = config.vad_threshold;
        let silence_timeout_ms = config.silence_timeout_ms;
        *self.config.lock().await = config;
        *self.vad_detector.lock().await = vad::VadDetector::new(vad_threshold, silence_timeout_ms);
    }
}

struct LoopData {
    config: Arc<Mutex<AudioConfig>>,
    engine: Arc<StdMutex<Option<EngineHandle>>>,
    vad_engine: Arc<StdMutex<Option<vad::VadEngine>>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    handler: Arc<StdMutex<Option<Arc<dyn InputHandler>>>>,
    failed: Arc<AtomicBool>,
    silent_abort: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- InputPipeline basics (no hardware required) ---

    #[tokio::test]
    async fn test_input_pipeline_new() {
        let _pipeline = InputPipeline::new();
    }

    #[tokio::test]
    async fn test_input_pipeline_default() {
        let _pipeline = InputPipeline::default();
    }

    #[tokio::test]
    async fn test_input_pipeline_initial_state() {
        let pipeline = InputPipeline::new();
        let state = pipeline.get_state().await;
        assert_eq!(state, RecordingState::Pending);
    }

    #[tokio::test]
    async fn test_update_config_changes_state() {
        let pipeline = InputPipeline::new();
        let config = AudioConfig {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            max_duration_secs: 30,
            silence_timeout_ms: 2000,
            vad_threshold: 0.3,
        };
        pipeline.update_config(config).await;
        let vad_threshold = pipeline.vad_detector.lock().await.threshold;
        let silence_frames = pipeline.vad_detector.lock().await.silence_max_frames;
        assert_eq!(vad_threshold, 0.3);
        assert_eq!(silence_frames, 2000 / 30);
        let pcm = vec![0.0f32];
        let wav = pipeline.encode_wav(&pcm).await.unwrap();
        let channels = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(channels, 1);
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16000);
    }

    #[tokio::test]
    async fn test_set_stt_client() {
        struct DummySttClient;
        #[async_trait::async_trait]
        impl SttClient for DummySttClient {
            async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<String> {
                Ok("dummy".into())
            }
        }
        let pipeline = InputPipeline::new();
        pipeline.set_stt_client(Some(Box::new(DummySttClient))).await;
        assert!(pipeline.stt_client.lock().await.is_some());
        pipeline.set_stt_client(None).await;
        assert!(pipeline.stt_client.lock().await.is_none());
    }

    #[tokio::test]
    async fn test_set_handler() {
        struct StopHandler;
        #[async_trait]
        impl InputHandler for StopHandler {
            async fn on_auto_stop(&self) {}
        }
        let pipeline = InputPipeline::new();
        pipeline.set_handler(Arc::new(StopHandler));
        assert!(pipeline.handler.lock().expect("lock").is_some());
    }

    #[tokio::test]
    async fn test_get_vad_state_default() {
        let pipeline = InputPipeline::new();
        let state = pipeline.get_vad_state().await;
        assert_eq!(state, vad::VadState::Silent);
    }

    // --- RecordingState / RecordingReason ---

    #[test]
    fn test_recording_state_debug() {
        let states = vec![
            (RecordingState::Pending, "Pending"),
            (RecordingState::Recording, "Recording"),
            (RecordingState::Processing, "Processing"),
        ];
        for (state, label) in states {
            assert_eq!(format!("{:?}", state), label);
        }
    }

    #[test]
    fn test_recording_reason_debug() {
        let reasons = vec![
            (RecordingReason::Manual, "Manual"),
            (RecordingReason::Silence, "Silence"),
            (RecordingReason::MaxDuration, "MaxDuration"),
            (RecordingReason::Cancel, "Cancel"),
        ];
        for (reason, label) in reasons {
            assert_eq!(format!("{:?}", reason), label);
        }
    }

    #[test]
    fn test_recording_state_serde() {
        let states = [
            RecordingState::Pending,
            RecordingState::Recording,
            RecordingState::Processing,
        ];
        for s in &states {
            let json = serde_json::to_string(s).unwrap();
            let back: RecordingState = serde_json::from_str(&json).unwrap();
            assert_eq!(*s, back);
        }
    }

    #[test]
    fn test_recording_reason_serde() {
        let reasons = [
            RecordingReason::Manual,
            RecordingReason::Silence,
            RecordingReason::MaxDuration,
            RecordingReason::Cancel,
        ];
        for r in &reasons {
            let json = serde_json::to_string(r).unwrap();
            let back: RecordingReason = serde_json::from_str(&json).unwrap();
            assert_eq!(*r, back);
        }
    }

    #[test]
    fn test_recording_result_defaults() {
        let result = RecordingResult {
            pcm: vec![0.1, -0.2],
            reason: RecordingReason::Manual,
            duration_ms: 0,
            transcript: None,
            transcript_error: None,
        };
        assert_eq!(result.pcm.len(), 2);
        assert_eq!(result.reason, RecordingReason::Manual);
        assert!(result.transcript.is_none());
    }

    #[tokio::test]
    async fn test_transcribe_skips_empty_text() {
        struct EmptySttClient;
        #[async_trait::async_trait]
        impl SttClient for EmptySttClient {
            async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<String> {
                Ok("   ".into())
            }
        }
        let pipeline = InputPipeline::new();
        pipeline.set_stt_client(Some(Box::new(EmptySttClient))).await;
        let mut result = RecordingResult {
            pcm: vec![0.0; 160],
            reason: RecordingReason::Manual,
            duration_ms: 10,
            transcript: None,
            transcript_error: None,
        };
        pipeline.transcribe(&mut result).await;
        assert!(result.transcript.is_none());
        assert!(result.transcript_error.is_none());
    }
}
