//! Input pipeline: recording orchestration, VAD and transcription.
//!
//! The pipeline owns the high-level recording state machine; the actual
//! audio capture lives in [`capture`] (the capture engine thread + CPAL
//! backend).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use capture::{EngineHandle, TARGET_SAMPLE_RATE};
use haven_llm::SttClient;

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

impl Default for RecordingResult {
    fn default() -> Self {
        Self {
            pcm: Vec::new(),
            reason: RecordingReason::Manual,
            duration_ms: 0,
            transcript: None,
            transcript_error: None,
        }
    }
}

pub struct InputPipeline {
    config: Arc<Mutex<AudioConfig>>,
    state: Mutex<RecordingState>,
    engine: Arc<StdMutex<Option<EngineHandle>>>,
    /// VAD worker thread handle (spawned by `prewarm`, reused by every
    /// recording; owns the resident model).
    vad_worker: Arc<StdMutex<Option<Arc<VadWorker>>>>,
    /// Audio ring buffer size in seconds (from `context_limits`).
    ring_buffer_secs: Arc<std::sync::Mutex<usize>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    handler: OnceLock<Arc<dyn InputHandler>>,
    cancel_token: StdMutex<Option<CancellationToken>>,
    result_rx: StdMutex<Option<tokio::sync::oneshot::Receiver<RecordingResult>>>,
    stt_client: Arc<Mutex<Option<Arc<dyn SttClient>>>>,
}

impl InputPipeline {
    pub fn new() -> Self {
        let vad_detector = vad::VadDetector::new(0.5, 1500);
        Self {
            config: Arc::new(Mutex::new(AudioConfig::default())),
            state: Mutex::new(RecordingState::Pending),
            engine: Arc::new(StdMutex::new(None)),
            vad_worker: Arc::new(StdMutex::new(None)),
            ring_buffer_secs: Arc::new(std::sync::Mutex::new(20)),
            vad_detector: Arc::new(Mutex::new(vad_detector)),
            handler: OnceLock::new(),
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
    /// Install the unified input handler. May only be installed once; a second
    /// install panics (the handler never changes at runtime).
    pub fn set_handler(&self, handler: Arc<dyn InputHandler>) {
        if self.handler.set(handler).is_err() {
            panic!("input handler already installed");
        }
    }

    /// Replace the unified context limits (audio ring buffer size).
    pub fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.ring_buffer_secs.lock().unwrap() = limits.input_ring_buffer_secs;
    }

    /// Install or clear the STT client. `None` disables transcription
    /// (e.g. when the provider is set to `none` at runtime).
    pub async fn set_stt_client(&self, client: Option<Arc<dyn SttClient>>) {
        *self.stt_client.lock().await = client;
    }

    /// Whether speech-to-text is configured (an STT client is installed).
    /// Used by callers that should only record when transcription can
    /// actually produce a transcript (e.g. the wake hotkey).
    pub async fn recording_configured(&self) -> bool {
        self.stt_client.lock().await.is_some()
    }

    /// Start the capture engine at app startup so the first recording pays no
    /// engine-spawn latency. No capture stream is opened here (the microphone
    /// is only claimed while recording); `start_recording` opens the stream
    /// itself. The VAD worker thread is also spawned here (off the async
    /// runtime): the model loads on it in the background, so the first
    /// recording does not stall on ONNX graph compilation — its first
    /// inferences just queue behind the load.
    pub async fn prewarm(&self) {
        {
            let mut guard = self.engine.lock().expect("engine lock poisoned");
            if guard.is_none() {
                let ring_secs = *self.ring_buffer_secs.lock().unwrap();
                match capture::spawn_engine(ring_secs) {
                    Ok(h) => {
                        *guard = Some(h);
                        tracing::debug!("audio capture engine prewarmed");
                    }
                    Err(e) => {
                        tracing::warn!("audio capture engine prewarm failed: {e}");
                    }
                }
            }
        }
        self.ensure_vad_worker();
    }

    /// Spawn the VAD worker thread on first use (from `prewarm` or the first
    /// recording). The engine loads on the worker in the background; a failed
    /// spawn leaves VAD disabled for this process.
    fn ensure_vad_worker(&self) -> Option<Arc<VadWorker>> {
        let mut guard = self.vad_worker.lock().expect("vad_worker lock poisoned");
        if guard.is_none() {
            match VadWorker::spawn() {
                Ok(w) => {
                    tracing::debug!("VAD worker spawned");
                    *guard = Some(w.clone());
                }
                Err(e) => tracing::warn!("VAD worker spawn failed, VAD disabled: {e}"),
            }
        }
        guard.clone()
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
                None => {
                    let ring_secs = *self.ring_buffer_secs.lock().unwrap();
                    match capture::spawn_engine(ring_secs) {
                        Ok(h) => {
                            *self.engine.lock().expect("engine lock poisoned") = Some(h.clone());
                            h
                        }
                        Err(e) => {
                            *self.state.lock().await = RecordingState::Pending;
                            return Err(e);
                        }
                    }
                }
            }
        };

        // Open the capture stream (see `capture` docs).
        if let Err(e) = handle.start().await {
            *self.state.lock().await = RecordingState::Pending;
            return Err(e);
        }

        {
            self.vad_detector.lock().await.reset();
            // Reuse the resident VAD worker; the model stays loaded across
            // recordings — graph compilation is the slow part, and a fresh
            // recording only needs the recurrent state reset, queued before
            // any inference on the worker's serialized command channel.
            if let Some(w) = self.ensure_vad_worker() {
                w.reset();
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
            engine: handle.clone(),
            vad_worker: self
                .vad_worker
                .lock()
                .expect("vad_worker lock poisoned")
                .clone(),
            vad_detector: self.vad_detector.clone(),
            handler: self.handler.get().cloned(),
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
                notify_auto_stop(&data.handler);
                let elapsed = start.elapsed();
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
                notify_auto_stop(&data.handler);
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::MaxDuration,
                    duration_ms: elapsed.as_millis() as u64,
                    transcript: None,
                    transcript_error: None,
                };
            }

            let new_data = data.engine.drain_shared();

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

                    // Run the model on the dedicated VAD worker thread instead
                    // of spawning a blocking task per frame (and locking the
                    // engine). Frames below the energy floor skip the
                    // round-trip entirely — they are silence by definition.
                    let prob = match &data.vad_worker {
                        Some(w) if vad::frame_has_energy(frame) => {
                            let frame_owned = frame.to_vec();
                            tokio::select! {
                                p = w.infer(frame_owned) => p,
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
                        }
                        _ => 0.0,
                    };

                    let (signal, state) = {
                        let mut det = data.vad_detector.lock().await;
                        let signal = det.process(prob);
                        let state = det.state();
                        (signal, state)
                    };

                    // Notify outside the detector lock: the hook may emit
                    // events and must not stall VAD inference.
                    if last_vad_status.elapsed() >= VAD_THROTTLE_INTERVAL {
                        if let Some(h) = &data.handler {
                            h.on_vad_status(signal, state);
                        }
                        last_vad_status = std::time::Instant::now();
                    }

                    if signal == vad::VadSignal::AutoStop {
                        notify_auto_stop(&data.handler);
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
            let total_rms =
                (result.pcm.iter().map(|s| s * s).sum::<f32>() / result.pcm.len() as f32).sqrt();
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
                Some(rx) => rx.await.unwrap_or(RecordingResult::default()),
                None => RecordingResult::default(),
            }
        };

        // Capture the tail that accumulated between the loop's last drain and
        // the stream teardown.
        let handle = self.engine.lock().expect("engine lock poisoned").clone();
        if let Some(handle) = handle {
            let remaining = handle.stop_and_drain().await?;
            if !remaining.is_empty() {
                result.pcm.extend_from_slice(&remaining);
                result.duration_ms = (result.pcm.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64;
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

/// Commands for the VAD worker thread, serialized through one channel so a
/// `Reset` is always applied before the following `Infer`s.
enum VadCmd {
    /// Infer speech probability for one frame; the worker replies with the
    /// echoed sequence number (stale replies from a cancelled recording are
    /// discarded by the caller).
    Infer { seq: u64, frame: Vec<f32> },
    /// Reset the model's recurrent state (between recordings).
    Reset,
}

/// Client handle to the VAD worker thread. The worker owns the model
/// exclusively, so inference needs no mutex and the model stays resident
/// across recordings (graph compilation is the slow part).
struct VadWorker {
    cmd_tx: std::sync::mpsc::Sender<VadCmd>,
    /// Mutex-wrapped because only the recording loop consumes replies, and
    /// `recv` needs `&mut`; uncontended (single consumer), cheap.
    prob_rx: Mutex<tokio::sync::mpsc::UnboundedReceiver<(u64, f32)>>,
    /// Monotonic sequence counter shared with the worker: never reused, so a
    /// stale reply can never be mistaken for the current inference.
    seq: AtomicU64,
}

impl VadWorker {
    /// Spawn the worker thread. The engine loads on it in the background, so
    /// the first inference may be delayed but the caller never blocks.
    fn spawn() -> std::io::Result<Arc<Self>> {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
        let (prob_tx, prob_rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::Builder::new()
            .name("vad-worker".into())
            .spawn(move || vad_worker_loop(cmd_rx, prob_tx))?;
        Ok(Arc::new(Self {
            cmd_tx,
            prob_rx: Mutex::new(prob_rx),
            seq: AtomicU64::new(0),
        }))
    }

    fn reset(&self) {
        let _ = self.cmd_tx.send(VadCmd::Reset);
    }

    /// Run one inference; returns 0.0 when the worker is gone (VAD disabled).
    async fn infer(&self, frame: Vec<f32>) -> f32 {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        if self.cmd_tx.send(VadCmd::Infer { seq, frame }).is_err() {
            return 0.0;
        }
        let mut rx = self.prob_rx.lock().await;
        loop {
            match rx.recv().await {
                Some((s, prob)) if s == seq => return prob,
                // Stale reply from a recording cancelled mid-inference.
                Some(_) => continue,
                None => return 0.0,
            }
        }
    }
}

fn vad_worker_loop(
    cmd_rx: std::sync::mpsc::Receiver<VadCmd>,
    prob_tx: tokio::sync::mpsc::UnboundedSender<(u64, f32)>,
) {
    let mut engine = match vad::VadEngine::new() {
        Ok(e) => Some(e),
        Err(err) => {
            tracing::warn!("VAD engine init failed, VAD disabled: {err}");
            None
        }
    };
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            VadCmd::Infer { seq, frame } => {
                // A panicking inference must not kill the worker: degrade to
                // silence for that frame instead (the reply channel closing
                // would strand the recording loop on its await).
                let prob = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match engine
                    .as_mut()
                {
                    Some(e) => e.infer(&frame),
                    None => 0.0,
                }))
                .unwrap_or(0.0);
                if prob_tx.send((seq, prob)).is_err() {
                    break;
                }
            }
            VadCmd::Reset => {
                if let Some(e) = engine.as_mut() {
                    e.reset();
                }
            }
        }
    }
}

struct LoopData {
    config: Arc<Mutex<AudioConfig>>,
    engine: EngineHandle,
    vad_worker: Option<Arc<VadWorker>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    /// Handler snapshot taken at `start_recording`; the loop never locks the
    /// pipeline's handler storage.
    handler: Option<Arc<dyn InputHandler>>,
    failed: Arc<AtomicBool>,
    silent_abort: Arc<AtomicBool>,
}

/// Fire the async auto-stop hook on a spawned task: the recording loop must
/// return promptly (the hook drives the stop path that awaits this loop's
/// result), so it can never be awaited in place.
fn notify_auto_stop(handler: &Option<Arc<dyn InputHandler>>) {
    if let Some(h) = handler {
        let h = h.clone();
        tokio::spawn(async move {
            h.on_auto_stop().await;
        });
    }
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
        pipeline
            .set_stt_client(Some(Arc::new(DummySttClient)))
            .await;
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
        assert!(pipeline.handler.get().is_some());
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
        pipeline
            .set_stt_client(Some(Arc::new(EmptySttClient)))
            .await;
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
