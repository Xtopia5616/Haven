//! Input pipeline: recording orchestration, VAD, and the audio capture
//! lifecycle.
//!
//! The pipeline owns the high-level recording state machine; the actual
//! audio capture lives in [`capture`] (the capture engine thread + CPAL
//! backend). Media interpretation and provider fallback live in the
//! model-facing `haven-tools::builtin::media` runtime; this crate only owns
//! capture, VAD, and recording lifecycle. Provider selection, transcription,
//! and fallback stay outside the acquisition boundary.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use capture::{CaptureEngineHandle, TARGET_SAMPLE_RATE};
use haven_common::hooks::OnceHandler;
use haven_common::types::RecordingId;

use haven_common::config::{AudioConfig, MAX_INPUT_RING_BUFFER_SECS};
pub use hotkey::{KeyCode, KeyCombo, KeyModifier};
pub use vad::{VadSignal, VadState};

mod capture;
mod hotkey;
mod vad;
mod wav;

const VAD_THROTTLE_INTERVAL: Duration = Duration::from_millis(100);
const VAD_COMMAND_QUEUE_CAPACITY: usize = 2;
const VAD_RESPONSE_QUEUE_CAPACITY: usize = 1;
const RECORDING_LOOP_INTERVAL: Duration = Duration::from_millis(30);
const DIGITAL_SILENCE_FLOOR: f32 = 1e-4;

fn capture_error_for(mode: LoopMode, pcm: &[f32], duration_ms: u64) -> Option<String> {
    if !matches!(mode, LoopMode::Normal) || duration_ms < 200 || pcm.is_empty() {
        return None;
    }
    let rms = (pcm.iter().map(|sample| sample * sample).sum::<f32>() / pcm.len() as f32).sqrt();
    if rms < DIGITAL_SILENCE_FLOOR {
        tracing::error!("captured audio is digital silence");
        Some("麦克风没有检测到声音，请检查系统麦克风是否被静音或已禁用".into())
    } else {
        None
    }
}

fn lock_std_or_recover<'a, T>(
    lock: &'a StdMutex<T>,
    name: &'static str,
) -> std::sync::MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            lock = name,
            "input pipeline lock poisoned; recovering state"
        );
        poisoned.into_inner()
    })
}

/// Unified input-pipeline event hooks. Both methods have no-op defaults, so
/// an implementation only overrides the hooks it needs.
#[async_trait]
pub trait InputEventHandler: Send + Sync {
    /// Fired (sync) on each VAD signal/state transition. May be throttled by
    /// the pipeline before delivery.
    fn on_vad_status(&self, _signal: VadSignal, _state: VadState) {}
    /// Fired asynchronously when VAD ends a recording or reaches its maximum
    /// duration. The ID is the exact capture that produced the signal, so a
    /// delayed callback cannot stop a later recording.
    async fn on_auto_stop(&self, recording_id: RecordingId);
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
}

/// Recording flavor for the recording loop. User-input recordings (button /
/// hotkey / VAD) run VAD auto-stop and notify the input handler; agent-tool
/// (`media.record`) recordings capture a fixed window with no VAD and no
/// handler notifications, so the user-facing recording UI and shell state
/// are never disturbed by an agent-initiated recording.
#[derive(Debug, Clone, Copy)]
enum LoopMode {
    Normal,
    Timed { duration: Duration },
}

#[derive(Debug, Clone)]
pub struct RecordingResult {
    pub pcm: Vec<f32>,
    pub reason: RecordingReason,
    pub duration_ms: u64,
    /// Capture-side failure, if the device or VAD could not produce a usable
    /// recording. Provider and transcription failures are owned by the media
    /// runtime and never cross back into this acquisition result.
    pub capture_error: Option<String>,
}

impl RecordingResult {
    /// Encode this captured 16 kHz mono PCM as a 16-bit WAV.
    pub fn encode_wav(&self) -> Vec<u8> {
        wav::encode_wav_to_vec(&self.pcm, TARGET_SAMPLE_RATE, 1)
    }
}

pub struct InputPipeline {
    audio_config: Arc<Mutex<AudioConfig>>,
    state: Mutex<RecordingState>,
    /// Serializes capture transitions with worker startup and terminal shutdown.
    lifecycle: Mutex<PipelineLifecycle>,
    /// Makes concurrent shutdown callers wait for the same resource drain.
    shutdown_gate: Mutex<()>,
    engine: Arc<StdMutex<Option<CaptureEngineHandle>>>,
    /// VAD worker thread handle (spawned by `prewarm`, reused by every
    /// recording; owns the resident model).
    vad_worker: Arc<StdMutex<Option<Arc<VadWorker>>>>,
    /// Audio ring buffer size in seconds (from `context_limits`).
    ring_buffer_capacity_secs: Arc<std::sync::Mutex<usize>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    event_handler: OnceHandler<dyn InputEventHandler>,
    cancel_token: StdMutex<Option<CancellationToken>>,
    result_rx: StdMutex<Option<tokio::sync::oneshot::Receiver<RecordingResult>>>,
    auto_stop_tasks: Arc<StdMutex<tokio::task::JoinSet<()>>>,
}

#[derive(Default)]
struct PipelineLifecycle {
    closed: bool,
}

impl InputPipeline {
    pub fn new() -> Self {
        let vad_detector = vad::VadDetector::new(0.5, 1500);
        Self {
            audio_config: Arc::new(Mutex::new(AudioConfig::default())),
            state: Mutex::new(RecordingState::Pending),
            lifecycle: Mutex::new(PipelineLifecycle::default()),
            shutdown_gate: Mutex::new(()),
            engine: Arc::new(StdMutex::new(None)),
            vad_worker: Arc::new(StdMutex::new(None)),
            ring_buffer_capacity_secs: Arc::new(std::sync::Mutex::new(20)),
            vad_detector: Arc::new(Mutex::new(vad_detector)),
            event_handler: OnceHandler::new(),
            cancel_token: StdMutex::new(None),
            result_rx: StdMutex::new(None),
            auto_stop_tasks: Arc::new(StdMutex::new(tokio::task::JoinSet::new())),
        }
    }
}

impl Default for InputPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl InputPipeline {
    /// Install the input event handler. May only be installed once; a second
    /// install is ignored and logged (the handler never changes at runtime).
    pub fn install_event_handler(&self, event_handler: Arc<dyn InputEventHandler>) {
        self.event_handler.set(event_handler);
    }

    /// Set the audio ring capacity used when the capture engine is next spawned.
    /// An already-running capture engine retains its current capacity. Zero
    /// keeps the capture engine's default-capacity behavior.
    pub fn set_ring_buffer_capacity_secs(&self, capacity_secs: usize) -> Result<()> {
        if capacity_secs > MAX_INPUT_RING_BUFFER_SECS {
            return Err(anyhow!(
                "input ring buffer capacity must not exceed {MAX_INPUT_RING_BUFFER_SECS} seconds"
            ));
        }
        *lock_std_or_recover(&self.ring_buffer_capacity_secs, "ring_buffer_capacity_secs") =
            capacity_secs;
        Ok(())
    }

    /// Start the capture engine at app startup so the first recording pays no
    /// engine-spawn latency. No capture stream is opened here (the microphone
    /// is only claimed while recording); `start_capture` opens the stream
    /// itself. The VAD worker thread is also spawned here (off the async
    /// runtime): the model loads on it in the background, so the first
    /// recording does not stall on ONNX graph compilation — its first
    /// inferences just queue behind the load.
    pub async fn prewarm(&self) {
        let lifecycle = self.lifecycle.lock().await;
        if lifecycle.closed {
            tracing::debug!("ignoring input pipeline prewarm after shutdown");
            return;
        }
        {
            let mut guard = lock_std_or_recover(&self.engine, "engine");
            if guard.is_none() {
                let ring_capacity_secs = *lock_std_or_recover(
                    &self.ring_buffer_capacity_secs,
                    "ring_buffer_capacity_secs",
                );
                match capture::spawn_engine(ring_capacity_secs) {
                    Ok(h) => {
                        *guard = Some(h);
                        tracing::debug!("audio capture engine prewarmed");
                    }
                    Err(_error) => {
                        tracing::warn!(
                            failure_kind = "audio_capture_prewarm",
                            "audio capture engine prewarm failed"
                        );
                    }
                }
            }
        }
        if self.ensure_vad_worker().is_err() {
            tracing::error!(failure_kind = "vad_worker_prewarm", "VAD prewarm failed");
        }
    }

    /// Spawn the VAD worker thread on first use (from `prewarm` or the first
    /// recording). The engine loads on the worker in the background. A failed
    /// spawn is returned to the recording caller instead of silently disabling
    /// VAD.
    fn ensure_vad_worker(&self) -> Result<Arc<VadWorker>> {
        let mut guard = lock_std_or_recover(&self.vad_worker, "vad_worker");
        if guard.is_none() {
            let worker = VadWorker::spawn()
                .map_err(|error| anyhow!("failed to spawn VAD worker: {error}"))?;
            tracing::debug!("VAD worker spawned");
            *guard = Some(worker);
        }
        guard
            .clone()
            .ok_or_else(|| anyhow!("VAD worker is unavailable"))
    }

    /// Current state of the speech-activity detector.
    #[cfg(test)]
    async fn vad_state(&self) -> VadState {
        self.vad_detector.lock().await.state()
    }

    /// Start user-facing capture with VAD auto-stop and the supplied identity.
    /// The identity is installed before opening the microphone so every
    /// auto-stop callback can refer to the capture that produced it.
    pub async fn start_capture(&self, recording_id: RecordingId) -> Result<()> {
        self.start_inner(LoopMode::Normal, Some(recording_id)).await
    }

    /// Capture audio for a fixed `duration`, returning the recorded PCM.
    /// Used by the agent `media.record`: no VAD auto-stop, no handler
    /// notifications, and the capture is capped by the configured
    /// `max_duration_secs` so a runaway tool call cannot monopolize the
    /// microphone. Fails when a user recording is already in flight.
    pub async fn capture_for(&self, duration: Duration) -> Result<RecordingResult> {
        let max = Duration::from_secs(self.audio_config.lock().await.max_duration_secs.max(1));
        let duration = duration.min(max);
        self.start_inner(LoopMode::Timed { duration }, None).await?;
        // The loop ends by itself at `duration`; `stop_capture` closes the
        // stream and returns whatever accumulated (works whether or not the
        // loop already finished).
        tokio::time::sleep(duration).await;
        self.stop_capture().await
    }

    async fn start_inner(&self, mode: LoopMode, recording_id: Option<RecordingId>) -> Result<()> {
        let lifecycle = self.lifecycle.lock().await;
        if lifecycle.closed {
            return Err(anyhow!("input pipeline is shut down"));
        }
        {
            let mut state = self.state.lock().await;
            if *state != RecordingState::Pending {
                return Err(anyhow!("already recording or processing"));
            }
            *state = RecordingState::Recording;
        }

        // Ensure the engine is running (prewarm may have been skipped).
        let handle = {
            let existing = lock_std_or_recover(&self.engine, "engine").clone();
            match existing {
                Some(h) => h,
                None => {
                    let ring_capacity_secs = *lock_std_or_recover(
                        &self.ring_buffer_capacity_secs,
                        "ring_buffer_capacity_secs",
                    );
                    match capture::spawn_engine(ring_capacity_secs) {
                        Ok(h) => {
                            *lock_std_or_recover(&self.engine, "engine") = Some(h.clone());
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

        let vad_worker = if matches!(mode, LoopMode::Normal) {
            match self.ensure_vad_worker() {
                Ok(worker) => Some(worker),
                Err(error) => {
                    handle.stop_and_clear();
                    *self.state.lock().await = RecordingState::Pending;
                    return Err(error);
                }
            }
        } else {
            None
        };

        {
            self.vad_detector.lock().await.reset();
            // Reuse the resident VAD worker; the model stays loaded across
            // recordings — graph compilation is the slow part, and a fresh
            // recording only needs the recurrent state reset, queued before
            // any inference on the worker's serialized command channel.
            if let Some(w) = &vad_worker
                && let Err(error) = w.reset().await
            {
                handle.stop_and_clear();
                *self.state.lock().await = RecordingState::Pending;
                return Err(error);
            }
        }

        let cancel = CancellationToken::new();
        *lock_std_or_recover(&self.cancel_token, "cancel_token") = Some(cancel.clone());

        let (tx, rx) = tokio::sync::oneshot::channel();
        *lock_std_or_recover(&self.result_rx, "result_rx") = Some(rx);

        let loop_data = LoopData {
            audio_config: self.audio_config.clone(),
            engine: handle.clone(),
            vad_worker,
            vad_detector: self.vad_detector.clone(),
            event_handler: self.event_handler.snap(),
            recording_id,
            auto_stop_tasks: self.auto_stop_tasks.clone(),
            failed: handle.stream_failed.clone(),
            silent_abort: handle.silent_abort.clone(),
            mode,
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
        let max_duration = match data.mode {
            LoopMode::Normal => {
                let audio_config = data.audio_config.lock().await;
                Duration::from_secs(audio_config.max_duration_secs)
            }
            LoopMode::Timed { duration } => duration,
        };

        // Pre-reserve the full recording up front (max_duration × sample
        // rate): the loop appends a 30 ms chunk per tick, and growing from an
        // empty Vec would reallocate ~10 times and copy up to 2× the final
        // size over the recording.
        let mut accumulated_pcm: Vec<f32> =
            Vec::with_capacity(max_duration.as_secs() as usize * TARGET_SAMPLE_RATE as usize);
        let mut new_data = Vec::new();
        let mut vad_partial: Vec<f32> = Vec::new();
        let mut last_vad_status = std::time::Instant::now();

        loop {
            if cancel.is_cancelled() {
                let elapsed = start.elapsed();
                let capture_error =
                    capture_error_for(data.mode, &accumulated_pcm, elapsed.as_millis() as u64);
                return RecordingResult {
                    capture_error,
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                };
            }

            // Fatal stream error (device lost): stop early instead of
            // recording silence until max_duration.
            if data.failed.load(Ordering::SeqCst) {
                tracing::warn!("audio capture stream failed; stopping recording early");
                // Timed (agent-tool) recordings must not notify the handler:
                // that path steals the stop, emits UI events and submits the
                // transcript as a user voice message.
                if matches!(data.mode, LoopMode::Normal) {
                    notify_auto_stop(
                        &data.event_handler,
                        &data.recording_id,
                        &data.auto_stop_tasks,
                    );
                }
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                    capture_error: Some("录音设备发生错误，请检查麦克风连接后重试".into()),
                };
            }

            // The engine aborted the recording: the capture delivered pure
            // digital silence for the opening window. Stop immediately — the
            // capture result reports the error and no provider is contacted.
            if data.silent_abort.load(Ordering::SeqCst) {
                tracing::warn!("recording aborted: capture delivered no signal");
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                    capture_error: Some(
                        "录音没有收到麦克风信号，请检查系统麦克风是否被静音或已禁用".into(),
                    ),
                };
            }

            if start.elapsed() >= max_duration {
                // Timed (agent-tool) recordings must not notify the handler:
                // that path stops the shell recording and emits UI events.
                if matches!(data.mode, LoopMode::Normal) {
                    notify_auto_stop(
                        &data.event_handler,
                        &data.recording_id,
                        &data.auto_stop_tasks,
                    );
                }
                let elapsed = start.elapsed();
                let capture_error =
                    capture_error_for(data.mode, &accumulated_pcm, elapsed.as_millis() as u64);
                return RecordingResult {
                    capture_error,
                    pcm: accumulated_pcm,
                    reason: RecordingReason::MaxDuration,
                    duration_ms: elapsed.as_millis() as u64,
                };
            }

            data.engine.drain_buffered_into(&mut new_data);

            if !new_data.is_empty() {
                if accumulated_pcm.is_empty() && vad_partial.is_empty() {
                    tracing::debug!(
                        "recording_loop: first audio chunk ({} samples)",
                        new_data.len()
                    );
                }
                accumulated_pcm.extend_from_slice(&new_data);

                // Timed recordings skip VAD entirely: fixed-window capture,
                // no auto-stop, no handler notifications.
                if matches!(data.mode, LoopMode::Normal) {
                    let mut vad_input = Vec::new();
                    std::mem::swap(&mut vad_partial, &mut vad_input);
                    vad_input.extend_from_slice(&new_data);

                    let mut offset = 0;
                    while offset + vad::FRAME_SIZE <= vad_input.len() {
                        // Cancellation wins over VAD latency: each inference runs
                        // on the blocking pool so a slow model (tract in debug
                        // builds) cannot stall the stop path.
                        if cancel.is_cancelled() {
                            let elapsed = start.elapsed();
                            let capture_error = capture_error_for(
                                data.mode,
                                &accumulated_pcm,
                                elapsed.as_millis() as u64,
                            );
                            return RecordingResult {
                                capture_error,
                                pcm: accumulated_pcm,
                                reason: RecordingReason::Manual,
                                duration_ms: elapsed.as_millis() as u64,
                            };
                        }
                        let frame = &vad_input[offset..offset + vad::FRAME_SIZE];
                        offset += vad::FRAME_SIZE;

                        // Run the model on the dedicated VAD worker thread instead
                        // of spawning a blocking session per frame (and locking the
                        // engine). Frames below the energy floor skip the
                        // round-trip entirely — they are silence by definition.
                        let probability_result = match &data.vad_worker {
                            Some(w) if vad::frame_has_energy(frame) => {
                                let frame_owned = frame.to_vec();
                                tokio::select! {
                                    probability = w.infer_speech_probability(frame_owned) => probability,
                                    _ = cancel.cancelled() => {
                                        let elapsed = start.elapsed();
                                        let capture_error = capture_error_for(
                                            data.mode,
                                            &accumulated_pcm,
                                            elapsed.as_millis() as u64,
                                        );
                                        return RecordingResult {
                                            capture_error,
                                            pcm: accumulated_pcm,
                                            reason: RecordingReason::Manual,
                                            duration_ms: elapsed.as_millis() as u64,
                                        };
                                    }
                                }
                            }
                            _ => Ok(0.0),
                        };
                        let speech_probability = match probability_result {
                            Ok(probability) => probability,
                            Err(error) => {
                                tracing::error!(
                                    failure_kind = "vad_worker",
                                    "VAD worker failed; stopping recording"
                                );
                                let elapsed = start.elapsed();
                                return recording_result_for_vad_worker_failure(
                                    data.mode,
                                    &data.event_handler,
                                    &data.recording_id,
                                    &data.auto_stop_tasks,
                                    accumulated_pcm,
                                    elapsed.as_millis() as u64,
                                    error.to_string(),
                                );
                            }
                        };

                        let (signal, state) = {
                            let mut det = data.vad_detector.lock().await;
                            let signal = det.observe_probability(speech_probability);
                            let state = det.state();
                            (signal, state)
                        };

                        // Notify outside the detector lock: the hook may emit
                        // events and must not stall VAD inference.
                        if last_vad_status.elapsed() >= VAD_THROTTLE_INTERVAL {
                            if let Some(handler) = &data.event_handler {
                                handler.on_vad_status(signal, state);
                            }
                            last_vad_status = std::time::Instant::now();
                        }

                        if signal == vad::VadSignal::AutoStop {
                            notify_auto_stop(
                                &data.event_handler,
                                &data.recording_id,
                                &data.auto_stop_tasks,
                            );
                            let elapsed = start.elapsed();
                            let capture_error = capture_error_for(
                                data.mode,
                                &accumulated_pcm,
                                elapsed.as_millis() as u64,
                            );
                            return RecordingResult {
                                capture_error,
                                pcm: accumulated_pcm,
                                reason: RecordingReason::Silence,
                                duration_ms: elapsed.as_millis() as u64,
                            };
                        }
                    }

                    if offset < vad_input.len() {
                        vad_partial = vad_input[offset..].to_vec();
                    }
                }
            }

            tokio::select! {
                _ = tokio::time::sleep(RECORDING_LOOP_INTERVAL) => {}
                _ = cancel.cancelled() => {}
            }
        }
    }

    /// Cancel an active capture and discard its buffered audio.
    pub async fn cancel_capture(&self) -> Result<()> {
        let lifecycle = self.lifecycle.lock().await;
        if lifecycle.closed {
            return Ok(());
        }
        self.cancel_capture_inner().await
    }

    async fn cancel_capture_inner(&self) -> Result<()> {
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
            token = lock_std_or_recover(&self.cancel_token, "cancel_token").take();
            rx = lock_std_or_recover(&self.result_rx, "result_rx").take();
            *state = RecordingState::Pending;
        }

        if let Some(token) = token {
            token.cancel();
        }

        if let Some(rx) = rx {
            let _ = rx.await;
        }

        if let Some(ref handle) = *lock_std_or_recover(&self.engine, "engine") {
            handle.stop_and_clear();
        }
        // The VAD engine stays resident across recordings (state was reset at
        // the next start); releasing it here would re-pay ONNX graph
        // compilation on the next recording.
        self.vad_detector.lock().await.reset();

        tracing::debug!("Recording cancelled");
        Ok(())
    }

    /// Stop any active capture and release resident audio workers.
    ///
    /// `ApplicationRuntime` calls this during process teardown. The VAD worker
    /// is intentionally kept resident during normal recording cancellation so
    /// the next recording remains fast; only application shutdown drops it.
    pub async fn shutdown(&self) -> Result<()> {
        let _shutdown = self.shutdown_gate.lock().await;
        let (engine, vad_worker) = {
            let mut lifecycle = self.lifecycle.lock().await;
            if lifecycle.closed {
                return Ok(());
            }
            lifecycle.closed = true;

            self.cancel_capture_inner().await?;
            self.vad_detector.lock().await.reset();
            (
                lock_std_or_recover(&self.engine, "engine").take(),
                lock_std_or_recover(&self.vad_worker, "vad_worker").take(),
            )
        };

        // The closed state prevents new owners from starting after this gate
        // is released. Release it before joining callbacks: a callback may be
        // finishing an auto-stop through the same pipeline.
        let mut failures = Vec::new();
        if let Some(handle) = engine
            && let Err(error) = handle.shutdown().await
        {
            failures.push(format!("capture engine shutdown failed: {error}"));
        }
        if let Some(worker) = vad_worker
            && let Err(error) = worker.shutdown().await
        {
            failures.push(format!("VAD worker shutdown failed: {error}"));
        }
        self.join_auto_stop_tasks().await;
        if !failures.is_empty() {
            return Err(anyhow!(failures.join("; ")));
        }
        tracing::debug!("input pipeline shut down");
        Ok(())
    }

    async fn join_auto_stop_tasks(&self) {
        let mut tasks = std::mem::take(&mut *lock_std_or_recover(
            &self.auto_stop_tasks,
            "auto_stop_tasks",
        ));
        let joined = tokio::time::timeout(Duration::from_secs(5), async {
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(
                        task_panicked = error.is_panic(),
                        task_cancelled = error.is_cancelled(),
                        "input auto-stop callback failed during shutdown"
                    );
                }
            }
        })
        .await;
        if joined.is_err() {
            tracing::warn!("input auto-stop callbacks did not finish before shutdown deadline");
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
    }

    /// Stop the audio capture and return the captured PCM. No provider or
    /// transcription work is performed at this boundary.
    pub async fn stop_capture(&self) -> Result<RecordingResult> {
        let lifecycle = self.lifecycle.lock().await;
        if lifecycle.closed {
            return Err(anyhow!("input pipeline is shut down"));
        }
        let result = self.stop_capture_inner().await;
        *self.state.lock().await = RecordingState::Pending;
        result
    }

    async fn stop_capture_inner(&self) -> Result<RecordingResult> {
        let mut state = self.state.lock().await;
        let prev_state = std::mem::replace(&mut *state, RecordingState::Processing);
        if prev_state != RecordingState::Recording {
            *state = prev_state;
            return Err(anyhow!("not recording"));
        }
        drop(state);

        let token = lock_std_or_recover(&self.cancel_token, "cancel_token").take();
        if let Some(token) = token {
            token.cancel();
        }

        let mut result = {
            let rx = lock_std_or_recover(&self.result_rx, "result_rx").take();
            match rx {
                Some(rx) => rx
                    .await
                    .map_err(|_| anyhow!("recording loop terminated without a result"))?,
                None => return Err(anyhow!("recording result channel was unavailable")),
            }
        };

        // Capture the tail that accumulated between the loop's last drain and
        // the stream teardown.
        let handle = lock_std_or_recover(&self.engine, "engine").clone();
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

    /// Current user-recording lifecycle state.
    pub async fn state(&self) -> RecordingState {
        self.state.lock().await.clone()
    }

    /// Apply a new audio configuration: `max_duration_secs` is read by the
    /// recording loop; `silence_timeout_ms` and `vad_threshold` are
    /// propagated to the VAD detector.
    pub async fn update_audio_config(&self, config: AudioConfig) {
        let vad_threshold = config.vad_threshold;
        let silence_timeout_ms = config.silence_timeout_ms;
        *self.audio_config.lock().await = config;
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
    cmd_tx: StdMutex<Option<tokio::sync::mpsc::Sender<VadCmd>>>,
    thread: StdMutex<Option<std::thread::JoinHandle<()>>>,
    /// Mutex-wrapped because only the recording loop consumes replies, and
    /// `recv` needs `&mut`; uncontended (single consumer), cheap.
    speech_probability_rx: Mutex<tokio::sync::mpsc::Receiver<(u64, Result<f32, String>)>>,
    /// Monotonic sequence counter shared with the worker: never reused, so a
    /// stale reply can never be mistaken for the current inference.
    seq: AtomicU64,
}

impl VadWorker {
    /// Spawn the worker thread. The engine loads on it in the background, so
    /// the first inference may be delayed but the caller never blocks.
    fn spawn() -> std::io::Result<Arc<Self>> {
        Self::spawn_with_loop(vad_worker_loop)
    }

    fn spawn_with_loop(
        worker_loop: impl FnOnce(
            tokio::sync::mpsc::Receiver<VadCmd>,
            tokio::sync::mpsc::Sender<(u64, Result<f32, String>)>,
        ) + Send
        + 'static,
    ) -> std::io::Result<Arc<Self>> {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(VAD_COMMAND_QUEUE_CAPACITY);
        let (speech_probability_tx, speech_probability_rx) =
            tokio::sync::mpsc::channel::<(u64, Result<f32, String>)>(VAD_RESPONSE_QUEUE_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("vad-worker".into())
            .spawn(move || worker_loop(cmd_rx, speech_probability_tx))?;
        Ok(Arc::new(Self {
            cmd_tx: StdMutex::new(Some(cmd_tx)),
            thread: StdMutex::new(Some(thread)),
            speech_probability_rx: Mutex::new(speech_probability_rx),
            seq: AtomicU64::new(0),
        }))
    }

    async fn reset(&self) -> Result<()> {
        self.send_command(VadCmd::Reset).await
    }

    async fn send_command(&self, command: VadCmd) -> Result<()> {
        let sender = lock_std_or_recover(&self.cmd_tx, "vad_worker_command_sender")
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("VAD worker command channel is closed"))?;
        sender
            .send(command)
            .await
            .map_err(|_| anyhow!("VAD worker command channel is closed"))
    }

    /// Run one inference and preserve worker/model failures for the recording
    /// loop. A failed VAD inference must not become a fabricated silence value.
    async fn infer_speech_probability(&self, frame: Vec<f32>) -> Result<f32> {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        self.send_command(VadCmd::Infer { seq, frame }).await?;
        let mut rx = self.speech_probability_rx.lock().await;
        loop {
            match rx.recv().await {
                Some((s, result)) if s == seq => return result.map_err(|error| anyhow!(error)),
                // Stale reply from a recording cancelled mid-inference.
                Some(_) => continue,
                None => return Err(anyhow!("VAD worker response channel is closed")),
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        // A cancelled recording may have stopped consuming responses while the
        // worker is blocked in `blocking_send`. Close the receiver first so
        // that send wakes with an error before we join the OS thread.
        self.speech_probability_rx.lock().await.close();
        lock_std_or_recover(&self.cmd_tx, "vad_worker_command_sender").take();
        let Some(thread) = lock_std_or_recover(&self.thread, "vad_worker_thread").take() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || thread.join())
            .await
            .map_err(|error| anyhow!("VAD worker join task failed: {error}"))?
            .map_err(|_| anyhow!("VAD worker thread panicked"))
    }
}

fn vad_worker_loop(
    mut cmd_rx: tokio::sync::mpsc::Receiver<VadCmd>,
    speech_probability_tx: tokio::sync::mpsc::Sender<(u64, Result<f32, String>)>,
) {
    let (mut engine, init_error) = match vad::VadEngine::new() {
        Ok(e) => (Some(e), None),
        Err(err) => {
            let message = format!("VAD engine initialization failed: {err}");
            tracing::error!(
                failure_kind = "vad_engine_initialization",
                "VAD engine initialization failed"
            );
            (None, Some(message))
        }
    };
    while let Some(cmd) = cmd_rx.blocking_recv() {
        match cmd {
            VadCmd::Infer { seq, frame } => {
                let result = match engine.as_mut() {
                    Some(engine) => {
                        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            engine.infer_speech_probability(&frame)
                        })) {
                            Ok(result) => result.map_err(|error| error.to_string()),
                            Err(_) => Err("VAD inference panicked".to_string()),
                        }
                    }
                    None => Err(init_error
                        .clone()
                        .unwrap_or_else(|| "VAD engine is unavailable".to_string())),
                };
                if result.is_err() {
                    tracing::error!(failure_kind = "vad_inference", "VAD inference failed");
                }
                if speech_probability_tx.blocking_send((seq, result)).is_err() {
                    break;
                }
            }
            VadCmd::Reset => {
                if let Some(e) = engine.as_mut()
                    && e.reset().is_err()
                {
                    tracing::error!(
                        failure_kind = "vad_reset",
                        "VAD reset failed; disabling VAD worker"
                    );
                    engine = None;
                }
            }
        }
    }
}

struct LoopData {
    audio_config: Arc<Mutex<AudioConfig>>,
    engine: CaptureEngineHandle,
    vad_worker: Option<Arc<VadWorker>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    /// Handler snapshot taken at `start_capture`; the loop never locks the
    /// pipeline's handler storage.
    event_handler: Option<Arc<dyn InputEventHandler>>,
    recording_id: Option<RecordingId>,
    auto_stop_tasks: Arc<StdMutex<tokio::task::JoinSet<()>>>,
    failed: Arc<AtomicBool>,
    silent_abort: Arc<AtomicBool>,
    mode: LoopMode,
}

/// Schedule the async auto-stop hook in the pipeline-owned task set. The
/// recording loop must return promptly because the hook stops the capture and
/// awaits this loop's result, so the hook cannot be awaited in place.
fn notify_auto_stop(
    event_handler: &Option<Arc<dyn InputEventHandler>>,
    recording_id: &Option<RecordingId>,
    auto_stop_tasks: &Arc<StdMutex<tokio::task::JoinSet<()>>>,
) {
    if let (Some(handler), Some(recording_id)) = (event_handler, recording_id) {
        let mut tasks = lock_std_or_recover(auto_stop_tasks, "auto_stop_tasks");
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(
                    task_panicked = error.is_panic(),
                    task_cancelled = error.is_cancelled(),
                    "previous input auto-stop callback failed"
                );
            }
        }
        let handler = handler.clone();
        let recording_id = recording_id.clone();
        tasks.spawn(async move {
            handler.on_auto_stop(recording_id).await;
        });
    }
}

fn recording_result_for_vad_worker_failure(
    mode: LoopMode,
    event_handler: &Option<Arc<dyn InputEventHandler>>,
    recording_id: &Option<RecordingId>,
    auto_stop_tasks: &Arc<StdMutex<tokio::task::JoinSet<()>>>,
    pcm: Vec<f32>,
    duration_ms: u64,
    error: String,
) -> RecordingResult {
    if matches!(mode, LoopMode::Normal) {
        notify_auto_stop(event_handler, recording_id, auto_stop_tasks);
    }
    RecordingResult {
        pcm,
        reason: RecordingReason::Manual,
        duration_ms,
        capture_error: Some(format!("VAD worker failed: {error}")),
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
    async fn test_input_pipeline_default_starts_pending() {
        let pipeline = InputPipeline::default();
        assert_eq!(pipeline.state().await, RecordingState::Pending);
    }

    #[tokio::test]
    async fn test_input_pipeline_initial_state() {
        let pipeline = InputPipeline::new();
        let state = pipeline.state().await;
        assert_eq!(state, RecordingState::Pending);
    }

    #[tokio::test]
    async fn shutdown_is_terminal_idempotent_and_disables_prewarm() {
        let pipeline = InputPipeline::new();

        pipeline.shutdown().await.unwrap();
        pipeline.shutdown().await.unwrap();
        pipeline.prewarm().await;

        assert!(pipeline.start_capture(RecordingId::new()).await.is_err());
        assert!(
            pipeline
                .capture_for(Duration::from_millis(1))
                .await
                .is_err()
        );
        assert!(lock_std_or_recover(&pipeline.engine, "engine").is_none());
        assert!(lock_std_or_recover(&pipeline.vad_worker, "vad_worker").is_none());
    }

    #[tokio::test]
    async fn shutdown_joins_prewarmed_capture_and_vad_workers() {
        let pipeline = InputPipeline::new();
        pipeline.prewarm().await;
        assert!(lock_std_or_recover(&pipeline.engine, "engine").is_some());
        assert!(lock_std_or_recover(&pipeline.vad_worker, "vad_worker").is_some());

        pipeline.shutdown().await.unwrap();

        assert!(lock_std_or_recover(&pipeline.engine, "engine").is_none());
        assert!(lock_std_or_recover(&pipeline.vad_worker, "vad_worker").is_none());
    }

    #[tokio::test]
    async fn shutdown_waits_for_an_in_flight_lifecycle_transition() {
        let pipeline = Arc::new(InputPipeline::new());
        let transition = pipeline.lifecycle.lock().await;
        let shutdown = {
            let pipeline = pipeline.clone();
            tokio::spawn(async move { pipeline.shutdown().await })
        };

        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        drop(transition);
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown should acquire the lifecycle gate")
            .unwrap()
            .unwrap();
        assert!(pipeline.lifecycle.lock().await.closed);
    }

    #[tokio::test]
    async fn shutdown_joins_owned_auto_stop_callbacks() {
        let pipeline = Arc::new(InputPipeline::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        lock_std_or_recover(&pipeline.auto_stop_tasks, "auto_stop_tasks").spawn(async move {
            let _ = started_tx.send(());
            let _ = release_rx.await;
        });

        let shutdown = {
            let pipeline = pipeline.clone();
            tokio::spawn(async move { pipeline.shutdown().await })
        };
        started_rx.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!shutdown.is_finished());
        let concurrent_shutdown = {
            let pipeline = pipeline.clone();
            tokio::spawn(async move { pipeline.shutdown().await })
        };
        tokio::task::yield_now().await;
        assert!(!concurrent_shutdown.is_finished());
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("shutdown should join the callback")
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), concurrent_shutdown)
            .await
            .expect("concurrent shutdown should wait for the same drain")
            .unwrap()
            .unwrap();
    }

    #[test]
    fn ring_buffer_capacity_uses_the_common_limit_and_keeps_previous_value_on_rejection() {
        let pipeline = InputPipeline::new();

        assert!(pipeline.set_ring_buffer_capacity_secs(0).is_ok());
        assert!(
            pipeline
                .set_ring_buffer_capacity_secs(MAX_INPUT_RING_BUFFER_SECS)
                .is_ok()
        );

        for capacity_secs in [MAX_INPUT_RING_BUFFER_SECS + 1, usize::MAX] {
            assert!(
                pipeline
                    .set_ring_buffer_capacity_secs(capacity_secs)
                    .is_err()
            );
            assert_eq!(
                *lock_std_or_recover(
                    &pipeline.ring_buffer_capacity_secs,
                    "ring_buffer_capacity_secs"
                ),
                MAX_INPUT_RING_BUFFER_SECS
            );
        }
    }

    #[tokio::test]
    async fn test_update_audio_config_changes_state() {
        let pipeline = InputPipeline::new();
        let config = AudioConfig {
            max_duration_secs: 30,
            silence_timeout_ms: 2000,
            vad_threshold: 0.3,
        };
        pipeline.update_audio_config(config).await;
        let vad_threshold = pipeline.vad_detector.lock().await.threshold;
        let silence_frames = pipeline.vad_detector.lock().await.silence_max_frames;
        assert_eq!(vad_threshold, 0.3);
        assert_eq!(silence_frames, 2000_u32.div_ceil(30));
        let pcm = vec![0.0f32];
        let wav = RecordingResult {
            pcm,
            reason: RecordingReason::Manual,
            duration_ms: 0,
            capture_error: None,
        }
        .encode_wav();
        let channels = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(channels, 1);
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16000);
        let bits_per_sample = u16::from_le_bytes([wav[34], wav[35]]);
        assert_eq!(bits_per_sample, 16);
    }

    #[tokio::test]
    async fn test_install_event_handler() {
        struct StopHandler;
        #[async_trait]
        impl InputEventHandler for StopHandler {
            async fn on_auto_stop(&self, _recording_id: RecordingId) {}
        }
        let pipeline = InputPipeline::new();
        pipeline.install_event_handler(Arc::new(StopHandler));
        assert!(pipeline.event_handler.is_installed());
    }

    #[tokio::test]
    async fn vad_worker_failure_notifies_auto_stop_and_preserves_capture_error() {
        struct AutoStopProbe(StdMutex<Option<tokio::sync::oneshot::Sender<RecordingId>>>);

        #[async_trait]
        impl InputEventHandler for AutoStopProbe {
            async fn on_auto_stop(&self, recording_id: RecordingId) {
                if let Some(sender) = self.0.lock().unwrap().take() {
                    let _ = sender.send(recording_id);
                }
            }
        }

        let (sender, receiver) = tokio::sync::oneshot::channel();
        let event_handler: Option<Arc<dyn InputEventHandler>> =
            Some(Arc::new(AutoStopProbe(StdMutex::new(Some(sender)))));
        let expected_recording_id = RecordingId::new();
        let recording_id = Some(expected_recording_id.clone());
        let auto_stop_tasks = Arc::new(StdMutex::new(tokio::task::JoinSet::new()));
        let result = recording_result_for_vad_worker_failure(
            LoopMode::Normal,
            &event_handler,
            &recording_id,
            &auto_stop_tasks,
            vec![0.1, -0.2],
            321,
            "worker channel closed".to_string(),
        );

        let callback_recording_id = tokio::time::timeout(Duration::from_secs(1), receiver)
            .await
            .expect("VAD failure should trigger auto-stop")
            .expect("auto-stop handler should receive the notification");
        assert_eq!(callback_recording_id, expected_recording_id);
        let mut tasks = std::mem::take(&mut *auto_stop_tasks.lock().unwrap());
        assert!(tasks.join_next().await.unwrap().is_ok());
        assert_eq!(result.pcm, vec![0.1, -0.2]);
        assert_eq!(result.reason, RecordingReason::Manual);
        assert_eq!(result.duration_ms, 321);
        assert_eq!(
            result.capture_error.as_deref(),
            Some("VAD worker failed: worker channel closed")
        );
    }

    #[tokio::test]
    async fn vad_worker_shutdown_joins_the_os_thread() {
        let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_exited = exited.clone();
        let worker = VadWorker::spawn_with_loop(move |mut commands, _responses| {
            while commands.blocking_recv().is_some() {}
            worker_exited.store(true, Ordering::SeqCst);
        })
        .unwrap();

        worker.shutdown().await.unwrap();
        assert!(exited.load(Ordering::SeqCst));
        assert!(lock_std_or_recover(&worker.thread, "vad_worker_thread").is_none());
    }

    #[tokio::test]
    async fn vad_worker_shutdown_unblocks_response_send_after_cancelled_inferences() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = VadWorker::spawn_with_loop(move |mut commands, responses| {
            while let Some(command) = commands.blocking_recv() {
                if let VadCmd::Infer { seq, .. } = command {
                    if seq == 0 {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                    let _ = responses.blocking_send((seq, Ok(0.5)));
                }
            }
        })
        .unwrap();

        let first_worker = Arc::clone(&worker);
        let first =
            tokio::spawn(async move { first_worker.infer_speech_probability(vec![0.1]).await });
        tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
            .await
            .unwrap();
        first.abort();
        let _ = first.await;

        worker
            .send_command(VadCmd::Infer {
                seq: 1,
                frame: vec![0.2],
            })
            .await
            .unwrap();
        release_tx.send(()).unwrap();

        tokio::time::timeout(Duration::from_secs(1), worker.shutdown())
            .await
            .expect("shutdown must release a worker blocked sending an unconsumed reply")
            .unwrap();
    }

    #[tokio::test]
    async fn test_vad_state_default() {
        let pipeline = InputPipeline::new();
        let state = pipeline.vad_state().await;
        assert_eq!(state, VadState::Silent);
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
        ];
        for r in &reasons {
            let json = serde_json::to_string(r).unwrap();
            let back: RecordingReason = serde_json::from_str(&json).unwrap();
            assert_eq!(*r, back);
        }
    }

    #[test]
    fn test_recording_result_preserves_capture_metadata() {
        let result = RecordingResult {
            pcm: vec![0.1, -0.2],
            reason: RecordingReason::Manual,
            duration_ms: 0,
            capture_error: None,
        };
        assert_eq!(result.pcm.len(), 2);
        assert_eq!(result.reason, RecordingReason::Manual);
    }

    #[test]
    fn digital_silence_is_a_capture_error_only_for_voice_ingress() {
        let silence = vec![0.0; 4_000];
        assert!(capture_error_for(LoopMode::Normal, &silence, 250).is_some());
        assert!(
            capture_error_for(
                LoopMode::Timed {
                    duration: Duration::from_secs(1)
                },
                &silence,
                250
            )
            .is_none()
        );
        assert!(capture_error_for(LoopMode::Normal, &silence, 100).is_none());
        assert!(capture_error_for(LoopMode::Normal, &[0.01, -0.01], 250).is_none());
    }
}
