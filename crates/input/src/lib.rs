use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, mpsc};
use std::time::Duration;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample as CpalSample, SampleFormat, Stream, StreamConfig};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use haven_common::SttClient;

pub use haven_common::config::AudioConfig;

pub mod vad;

/// Unified input-pipeline hook surface, replacing the former separate
/// `VadCallback` and `AutoStopCallback` function-pointer fields on
/// `InputPipeline`. Naming mirrors `ShellHandler`. Both methods have no-op
/// defaults, so an implementation only overrides the hooks it needs.
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

const TARGET_SAMPLE_RATE: u32 = 16000;
const RING_CAPACITY: usize = TARGET_SAMPLE_RATE as usize * 5;
const VAD_FRAME_SAMPLES: usize = 480;
const VAD_THROTTLE_INTERVAL: Duration = Duration::from_millis(100);
const RECORDING_LOOP_INTERVAL: Duration = Duration::from_millis(30);
/// Upper bound for a capture-thread command round-trip. The thread answers in
/// microseconds; the timeout only guards against a dead capture thread.
const CAPTURE_CMD_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the capture thread polls the command channel while idle. Keeps
/// command latency bounded (well under CAPTURE_CMD_TIMEOUT) while letting the
/// thread observe recoverable CPAL stream errors and rebuild the stream.
const STREAM_MONITOR_INTERVAL: Duration = Duration::from_millis(50);
/// Max consecutive stream rebuilds before giving up on a recoverable error.
/// A successful rebuild resets the counter; hitting the cap ends the
/// recording early instead of spinning forever.
const MAX_STREAM_RESTARTS: u32 = 3;

pub struct RingBuffer {
    buf: Vec<f32>,
    head: usize,
    len: usize,
    cap: usize,
}

impl RingBuffer {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: vec![0.0f32; cap],
            head: 0,
            len: 0,
            cap,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.buf[self.head] = s;
            self.head = (self.head + 1) % self.cap;
            if self.len < self.cap {
                self.len += 1;
            }
        }
    }

    pub fn drain(&mut self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.len);
        if self.len == 0 {
            return out;
        }
        let tail_start = if self.len < self.cap { 0 } else { self.head };
        for i in 0..self.len {
            out.push(self.buf[(tail_start + i) % self.cap]);
        }
        self.len = 0;
        out
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.head = 0;
    }
}

struct Resampler {
    leftover: VecDeque<f32>,
    position: f64,
    ratio: f64,
}

impl Resampler {
    fn new(src_sr: f64, target_sr: f64) -> Self {
        Self {
            leftover: VecDeque::new(),
            position: 0.0,
            ratio: src_sr / target_sr,
        }
    }

    fn process(&mut self, mono: &[f32]) -> Vec<f32> {
        self.leftover.extend(mono);
        let mut out = Vec::new();
        while (self.position + self.ratio * 0.5) < self.leftover.len() as f64 {
            let i = self.position.floor() as usize;
            let frac = (self.position - i as f64) as f32;
            let s0 = self.leftover[i];
            let s1 = self.leftover.get(i + 1).copied().unwrap_or(s0);
            out.push(s0 + (s1 - s0) * frac);
            self.position += self.ratio;
        }
        let consumed = self.position.floor() as usize;
        if consumed > 0 {
            for _ in 0..consumed.min(self.leftover.len()) {
                self.leftover.pop_front();
            }
            self.position -= consumed as f64;
        }
        out
    }
}

fn downmix(data: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return data.to_vec();
    }
    let frames = data.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for f in 0..frames {
        let sum: f32 = (0..channels).map(|c| data[f * channels + c]).sum();
        mono.push(sum / channels as f32);
    }
    mono
}

/// Fatal stream errors end the recording early (L7). Everything else —
/// backend-specific glitches like shared-mode conflicts or transient
/// underruns — is treated as recoverable: the capture thread rebuilds the
/// stream instead of aborting the session.
fn is_fatal_stream_error(err: &cpal::StreamError) -> bool {
    matches!(err, cpal::StreamError::DeviceNotAvailable)
}

enum CaptureCmd {
    Drain(tokio::sync::oneshot::Sender<Vec<f32>>),
    StopAndDrain(tokio::sync::oneshot::Sender<Vec<f32>>),
    StopAndClear,
}

#[derive(Clone)]
struct AudioCaptureHandle {
    cmd_tx: mpsc::Sender<CaptureCmd>,
}

impl AudioCaptureHandle {
    /// Ask the capture thread to drain the ring buffer. Async: the previous
    /// `blocking_recv()` panicked ("Cannot block the current thread from
    /// within a runtime") because the recording loop runs inside the tokio
    /// runtime. Bounded by CAPTURE_CMD_TIMEOUT so a dead capture thread can
    /// never hang the loop or a stop request.
    async fn drain_ring_buffer(&self) -> Vec<f32> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.cmd_tx.send(CaptureCmd::Drain(tx)).is_err() {
            return Vec::new();
        }
        tokio::time::timeout(CAPTURE_CMD_TIMEOUT, rx)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default()
    }

    async fn stop_and_drain(&self) -> Result<Vec<f32>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.cmd_tx.send(CaptureCmd::StopAndDrain(tx)).is_err() {
            return Ok(Vec::new());
        }
        let data = tokio::time::timeout(CAPTURE_CMD_TIMEOUT, rx)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        Ok(data)
    }

    fn stop_and_clear(&self) -> Result<()> {
        let _ = self.cmd_tx.send(CaptureCmd::StopAndClear);
        Ok(())
    }
}

fn spawn_capture_thread(
    ring: Arc<StdMutex<RingBuffer>>,
    resampler: Arc<StdMutex<Resampler>>,
    failed: Arc<AtomicBool>,
    stream_error: Arc<AtomicBool>,
) -> Result<mpsc::Sender<CaptureCmd>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device found"))?;
    let config = device.default_input_config()?;
    let channels = config.channels() as u16;
    let stream_config = StreamConfig {
        channels: config.channels(),
        sample_rate: config.sample_rate(),
        buffer_size: cpal::BufferSize::Default,
    };

    let ring_for_build = ring.clone();
    let resampler_for_build = resampler.clone();
    let build_failed = failed.clone();
    let build_stream_error = stream_error.clone();
    let build = move |sf: SampleFormat| -> Result<Stream> {
        // Error callback built per attempt: it must be Clone-able per sample
        // format arm, and it distinguishes fatal (device lost) from
        // recoverable (transient backend) errors. Recoverable errors set
        // `stream_error`, which the capture thread observes between commands
        // and responds to by rebuilding the stream — a transient glitch no
        // longer aborts the recording (L7 only fires for fatal failures).
        let err_fn = {
            let failed = build_failed.clone();
            let stream_error = build_stream_error.clone();
            move |err: cpal::StreamError| {
                if is_fatal_stream_error(&err) {
                    // The device was unplugged/disabled: the stream cannot
                    // recover, mark the recording as failed so the loop stops
                    // early instead of recording silence until max_duration.
                    tracing::error!("CPAL device lost: {err}");
                    failed.store(true, Ordering::SeqCst);
                } else {
                    // Backend-specific errors (shared-mode conflicts,
                    // transient underruns, …) can be recoverable; the capture
                    // thread will try to rebuild the stream.
                    tracing::error!("CPAL stream error (recoverable): {err}");
                    stream_error.store(true, Ordering::SeqCst);
                }
            }
        };
        match sf {
            SampleFormat::F32 => {
                let r = ring_for_build.clone();
                let rs = resampler_for_build.clone();
                device.build_input_stream::<f32, _, _>(
                    &stream_config,
                    move |data: &[f32], _: &cpal::InputCallbackInfo| {
                        let mono = if channels > 1 {
                            downmix(data, channels as usize)
                        } else {
                            data.to_vec()
                        };
                        let processed = rs.lock().expect("resampler lock poisoned").process(&mono);
                        r.lock()
                            .expect("ring buffer lock poisoned")
                            .push(&processed);
                    },
                    err_fn.clone(),
                    None,
                )
            }
            SampleFormat::I16 => {
                let r = ring_for_build.clone();
                let rs = resampler_for_build.clone();
                device.build_input_stream::<i16, _, _>(
                    &stream_config,
                    move |data: &[i16], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> =
                            data.iter().map(|s| s.to_sample::<f32>()).collect();
                        let mono = if channels > 1 {
                            downmix(&f32_data, channels as usize)
                        } else {
                            f32_data
                        };
                        let processed = rs.lock().expect("resampler lock poisoned").process(&mono);
                        r.lock()
                            .expect("ring buffer lock poisoned")
                            .push(&processed);
                    },
                    err_fn.clone(),
                    None,
                )
            }
            SampleFormat::U16 => {
                let r = ring_for_build.clone();
                let rs = resampler_for_build.clone();
                device.build_input_stream::<u16, _, _>(
                    &stream_config,
                    move |data: &[u16], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> =
                            data.iter().map(|s| s.to_sample::<f32>()).collect();
                        let mono = if channels > 1 {
                            downmix(&f32_data, channels as usize)
                        } else {
                            f32_data
                        };
                        let processed = rs.lock().expect("resampler lock poisoned").process(&mono);
                        r.lock()
                            .expect("ring buffer lock poisoned")
                            .push(&processed);
                    },
                    err_fn.clone(),
                    None,
                )
            }
            SampleFormat::I32 => {
                let r = ring_for_build.clone();
                let rs = resampler_for_build.clone();
                device.build_input_stream::<i32, _, _>(
                    &stream_config,
                    move |data: &[i32], _: &cpal::InputCallbackInfo| {
                        let f32_data: Vec<f32> =
                            data.iter().map(|s| s.to_sample::<f32>()).collect();
                        let mono = if channels > 1 {
                            downmix(&f32_data, channels as usize)
                        } else {
                            f32_data
                        };
                        let processed = rs.lock().expect("resampler lock poisoned").process(&mono);
                        r.lock()
                            .expect("ring buffer lock poisoned")
                            .push(&processed);
                    },
                    err_fn.clone(),
                    None,
                )
            }
            other => anyhow::bail!("unsupported sample format {other:?}"),
        }
        .map_err(|e| anyhow!("build_input_stream failed: {e}"))
    };

    let sample_format = config.sample_format();
    let (cmd_tx, cmd_rx) = mpsc::channel::<CaptureCmd>();

    std::thread::Builder::new()
        .name("haven-audio-capture".into())
        .spawn(move || {
            let stream = build(sample_format);
            let stream = match stream {
                Ok(s) => {
                    if let Err(e) = s.play() {
                        tracing::error!("CPAL stream play failed: {e}");
                        failed.store(true, Ordering::SeqCst);
                        return;
                    }
                    Some(s)
                }
                Err(e) => {
                    tracing::error!("CPAL stream build failed: {e}");
                    failed.store(true, Ordering::SeqCst);
                    return;
                }
            };
            let stream = StdMutex::new(stream);

            // Command loop with a bounded poll: commands are served within
            // STREAM_MONITOR_INTERVAL, and in between the thread watches for
            // recoverable CPAL errors (`stream_error`) and rebuilds the
            // stream, up to MAX_STREAM_RESTARTS consecutive failures. The
            // thread exits when every command sender is dropped (channel
            // disconnect) — dropping the stream and stopping capture. There
            // is intentionally no `Shutdown` command / Drop side-effect:
            // killing the thread at loop exit used to race `stop_and_drain`
            // and silently drop the trailing audio.
            let mut restart_count: u32 = 0;
            loop {
                match cmd_rx.recv_timeout(STREAM_MONITOR_INTERVAL) {
                    Ok(cmd) => match cmd {
                        CaptureCmd::Drain(tx) => {
                            let data = ring.lock().expect("ring buffer lock poisoned").drain();
                            let _ = tx.send(data);
                        }
                        CaptureCmd::StopAndDrain(tx) => {
                            let mut guard = stream.lock().expect("capture stream lock poisoned");
                            if let Some(s) = guard.take() {
                                drop(s);
                            }
                            drop(guard);
                            let data = ring.lock().expect("ring buffer lock poisoned").drain();
                            failed.store(false, Ordering::SeqCst);
                            stream_error.store(false, Ordering::SeqCst);
                            let _ = tx.send(data);
                        }
                        CaptureCmd::StopAndClear => {
                            let mut guard = stream.lock().expect("capture stream lock poisoned");
                            if let Some(s) = guard.take() {
                                drop(s);
                            }
                            drop(guard);
                            ring.lock().expect("ring buffer lock poisoned").clear();
                            failed.store(false, Ordering::SeqCst);
                            stream_error.store(false, Ordering::SeqCst);
                        }
                    },
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !stream_error.swap(false, Ordering::SeqCst) {
                            continue;
                        }
                        restart_count += 1;
                        if restart_count > MAX_STREAM_RESTARTS {
                            tracing::error!("CPAL stream failed {restart_count} times; giving up");
                            failed.store(true, Ordering::SeqCst);
                            let mut guard = stream.lock().expect("capture stream lock poisoned");
                            *guard = None;
                            continue;
                        }
                        tracing::warn!("CPAL stream error; rebuilding stream");
                        let mut guard = stream.lock().expect("capture stream lock poisoned");
                        *guard = None;
                        drop(guard);
                        match build(sample_format) {
                            Ok(s) => match s.play() {
                                Ok(()) => {
                                    *stream.lock().expect("capture stream lock poisoned") = Some(s);
                                    restart_count = 0;
                                    tracing::info!("CPAL stream rebuilt");
                                }
                                Err(e) => {
                                    tracing::error!("CPAL stream rebuild play failed: {e}");
                                }
                            },
                            Err(e) => {
                                tracing::error!("CPAL stream rebuild failed: {e}");
                            }
                        }
                    }
                }
            }
        })?;

    Ok(cmd_tx)
}

pub struct InputPipeline {
    config: Arc<Mutex<AudioConfig>>,
    state: Mutex<RecordingState>,
    capture: Arc<StdMutex<Option<AudioCaptureHandle>>>,
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
            capture: Arc::new(StdMutex::new(None)),
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
    /// Install the unified input handler (replaces former
    /// `set_vad_status_callback` + `set_on_auto_stop`).
    pub fn set_handler(&self, handler: Arc<dyn InputHandler>) {
        *self.handler.lock().expect("handler lock poisoned") = Some(handler);
    }

    /// Install or clear the STT client. `None` disables transcription
    /// (e.g. when the provider is set to `none` at runtime).
    pub async fn set_stt_client(&self, client: Option<Box<dyn SttClient>>) {
        *self.stt_client.lock().await = client;
    }

    pub async fn process_vad_frame(&self, frame: &[f32]) -> vad::VadSignal {
        let prob = {
            let mut guard = self.vad_engine.lock().expect("vad_engine lock poisoned");
            let engine: Option<&mut vad::VadEngine> = guard.as_mut();
            match engine {
                Some(e) => e.infer(frame),
                None => return vad::VadSignal::None,
            }
        };
        let (signal, state) = {
            let mut det = self.vad_detector.lock().await;
            let signal = det.process(prob);
            let state = det.state();
            (signal, state)
        };
        if let Some(ref h) = *self.handler.lock().expect("handler lock poisoned") {
            h.on_vad_status(signal, state);
        }
        signal
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

        let ring = Arc::new(StdMutex::new(RingBuffer::new(RING_CAPACITY)));
        let failed = Arc::new(AtomicBool::new(false));
        let stream_error = Arc::new(AtomicBool::new(false));
        let resampler = Arc::new(StdMutex::new(Resampler::new(
            cpal::default_host()
                .default_input_device()
                .and_then(|d| d.default_input_config().ok())
                .map(|c| c.sample_rate().0 as f64)
                .unwrap_or(TARGET_SAMPLE_RATE as f64),
            TARGET_SAMPLE_RATE as f64,
        )));

        let cmd_tx = match spawn_capture_thread(ring, resampler, failed.clone(), stream_error) {
            Ok(tx) => tx,
            Err(e) => {
                // Roll back the state we just set, otherwise a failed start
                // leaves the pipeline stuck in `Recording` and every later
                // stop attempt fails with "not recording".
                *self.state.lock().await = RecordingState::Pending;
                return Err(e);
            }
        };

        let capture_handle = AudioCaptureHandle { cmd_tx };
        *self.capture.lock().expect("capture lock poisoned") = Some(capture_handle);

        {
            self.vad_detector.lock().await.reset();
            let mut eng_guard = self.vad_engine.lock().expect("vad_engine lock poisoned");
            match vad::VadEngine::new() {
                Ok(e) => *eng_guard = Some(e),
                Err(err) => {
                    tracing::warn!("VAD engine init failed, VAD disabled: {err}");
                    *eng_guard = None;
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
            capture: self.capture.clone(),
            vad_engine: self.vad_engine.clone(),
            vad_detector: self.vad_detector.clone(),
            handler: self.handler.clone(),
            failed: failed.clone(),
        };
        tokio::spawn(async move {
            let result = Self::recording_loop(loop_data, cancel).await;
            let _ = tx.send(result);
        });

        tracing::debug!("Recording started with CPAL + VAD background loop");
        Ok(())
    }

    async fn recording_loop(data: LoopData, cancel: CancellationToken) -> RecordingResult {
        let start = std::time::Instant::now();
        let max_duration = {
            let config = data.config.lock().await;
            Duration::from_secs(config.max_duration_secs)
        };

        // Clone the capture handle ONCE for the whole session. Clones are
        // side-effect free (there is no Drop impl sending Shutdown anymore):
        // the capture thread stays alive until every sender is dropped,
        // which is exactly what lets `stop_recording` call `stop_and_drain`
        // afterwards and capture the trailing audio between the loop's final
        // drain and the actual stream teardown.
        let capture_handle = {
            let guard = data.capture.lock().expect("capture lock poisoned");
            guard.as_ref().cloned()
        };

        let mut accumulated_pcm: Vec<f32> = Vec::new();
        let mut vad_partial: Vec<f32> = Vec::new();
        let mut last_vad_status = std::time::Instant::now();

        loop {
            if cancel.is_cancelled() {
                Self::final_drain(&mut accumulated_pcm, &capture_handle).await;
                let elapsed = start.elapsed();
                return RecordingResult {
                    pcm: accumulated_pcm,
                    reason: RecordingReason::Manual,
                    duration_ms: elapsed.as_millis() as u64,
                    transcript: None,
                    transcript_error: None,
                };
            }

            // L7: the CPAL capture stream errored (e.g. device unplugged).
            // Stop now and keep whatever was captured so far instead of
            // silently recording silence until max_duration.
            if data.failed.load(Ordering::SeqCst) {
                tracing::warn!("audio capture stream failed; stopping recording early");
                Self::final_drain(&mut accumulated_pcm, &capture_handle).await;
                let elapsed = start.elapsed();
                let h = data.handler.lock().expect("handler lock poisoned").clone();
                if let Some(h) = h {
                    // Fire-and-forget: on_auto_stop drives `stop_recording`,
                    // which awaits the loop's result channel. Awaiting it here
                    // would deadlock (the loop can't return while blocked).
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

            if start.elapsed() >= max_duration {
                Self::final_drain(&mut accumulated_pcm, &capture_handle).await;
                let h = data.handler.lock().expect("handler lock poisoned").clone();
                if let Some(h) = h {
                    // Fire-and-forget: on_auto_stop drives `stop_recording`,
                    // which awaits the loop's result channel. Awaiting it here
                    // would deadlock (the loop can't return while blocked).
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
                Some(capture) => capture.drain_ring_buffer().await,
                None => Vec::new(),
            };

            if !new_data.is_empty() {
                accumulated_pcm.extend_from_slice(&new_data);

                let mut vad_input = Vec::new();
                std::mem::swap(&mut vad_partial, &mut vad_input);
                vad_input.extend_from_slice(&new_data);

                let mut offset = 0;
                while offset + VAD_FRAME_SAMPLES <= vad_input.len() {
                    let frame = &vad_input[offset..offset + VAD_FRAME_SAMPLES];
                    offset += VAD_FRAME_SAMPLES;

                    let prob = {
                        let mut eng_guard =
                            data.vad_engine.lock().expect("vad_engine lock poisoned");
                        match eng_guard.as_mut() {
                            Some(engine) => engine.infer(frame),
                            None => 0.0,
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
                        Self::final_drain(&mut accumulated_pcm, &capture_handle).await;
                        let h = data.handler.lock().expect("handler lock poisoned").clone();
                        if let Some(h) = h {
                            // Fire-and-forget: on_auto_stop drives
                            // `stop_recording`, which awaits the loop's result
                            // channel. Awaiting it here would deadlock (the
                            // loop can't return while blocked).
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

            // Poll cadence is RECORDING_LOOP_INTERVAL during active recording,
            // but we must break out of the sleep the instant the user (or an
            // auto-stop path) cancels the recording — otherwise the UI keeps
            // the red "recording" overlay up for up to one full poll interval
            // (~30 ms) before the loop notices and returns. Race the sleep
            // against the cancel token so stop feels instant.
            tokio::select! {
                _ = tokio::time::sleep(RECORDING_LOOP_INTERVAL) => {}
                _ = cancel.cancelled() => {}
            }
        }
    }

    async fn final_drain(accum: &mut Vec<f32>, capture_handle: &Option<AudioCaptureHandle>) {
        // No more sleeping here: the 50 ms `FINAL_DRAIN_TIMEOUT` delay used
        // to be the dominant source of "click stop, wait a beat before the
        // overlay disappears" latency. Trailing audio is already captured by
        // the `stop_and_drain` call that `stop_recording` makes right after
        // this loop returns, so this drain is just an opportunistic grab of
        // whatever the capture thread pushed between the loop's last
        // `drain_ring_buffer` and the cancel signal — it doesn't need to
        // wait for it.
        if let Some(capture) = capture_handle {
            let remaining = capture.drain_ring_buffer().await;
            accum.extend_from_slice(&remaining);
        }
    }

    pub async fn cancel_recording(&self) -> Result<()> {
        // Capture the cancel_token and result_rx belonging to the current
        // recording BEFORE setting state to Pending. If we set Pending first
        // and released the lock, a concurrent start_recording would see
        // Pending, create fresh token/rx, and cancel_recording would then
        // steal the new session's resources.
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

        if let Some(ref capture) = *self.capture.lock().expect("capture lock poisoned") {
            let _ = capture.stop_and_clear();
        }
        *self.capture.lock().expect("capture lock poisoned") = None;
        *self.vad_engine.lock().expect("vad_engine lock poisoned") = None;
        self.vad_detector.lock().await.reset();

        tracing::debug!("Recording cancelled");
        Ok(())
    }

    /// Stop the audio capture and return the captured PCM. Runs no STT and
    /// leaves `transcript`/`transcript_error` unset. Splitting capture from
    /// STT lets callers (Tauri commands, VAD auto-stop) emit
    /// `recording:stopped` to the UI as soon as the recording has actually
    /// ended, instead of making the user wait through STT and the agent
    /// ReAct loop before the UI flips out of "recording" state.
    pub async fn stop_capture(&self) -> Result<RecordingResult> {
        let result = self.stop_capture_inner().await?;
        // Set Pending immediately so the UI sees the recording as ended
        // (state machine is back at Pending → a new start_recording is free
        // to run) and so the cancelled recording loop's STT does not race a
        // fresh session.
        *self.state.lock().await = RecordingState::Pending;
        Ok(result)
    }

    /// Run STT on a previously-captured result, mutating `transcript` /
    /// `transcript_error` in place. Safe to call after `stop_capture`.
    pub async fn transcribe(&self, result: &mut RecordingResult) {
        if result.pcm.is_empty() {
            return;
        }
        let stt_guard = self.stt_client.lock().await;
        if let Some(ref client) = *stt_guard {
            // `result.pcm` is always the resampled mono stream at
            // TARGET_SAMPLE_RATE; encode it with its true format so the
            // WAV header matches the data even if AudioConfig.sample_rate
            // / channels were configured differently.
            let wav = encode_wav_to_vec(&result.pcm, TARGET_SAMPLE_RATE, 1);
            match client.transcribe(&wav).await {
                Ok(text) => {
                    // Empty transcription (silence / too-short clip) means
                    // "no speech": leave both fields unset so the caller
                    // skips the message instead of submitting blank input.
                    if !text.trim().is_empty() {
                        result.transcript = Some(text);
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
    /// inline. New callers should prefer `stop_capture` + `transcribe` so
    /// the UI can be notified that the recording has ended before STT
    /// begins.
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

        let result = {
            let rx = self
                .result_rx
                .lock()
                .expect("result_rx lock poisoned")
                .take();
            match rx {
                Some(rx) => match rx.await {
                    Ok(mut inner) => {
                        let capture = {
                            let guard = self.capture.lock().expect("capture lock poisoned");
                            guard.as_ref().cloned()
                        };
                        if let Some(capture) = capture {
                            let remaining = capture.stop_and_drain().await?;
                            if !remaining.is_empty() {
                                inner.pcm.extend_from_slice(&remaining);
                                if inner.duration_ms == 0 {
                                    inner.duration_ms =
                                        (inner.pcm.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64;
                                }
                            }
                        }
                        inner
                    }
                    Err(_) => {
                        let capture = {
                            let guard = self.capture.lock().expect("capture lock poisoned");
                            guard.as_ref().cloned()
                        };
                        let pcm = match capture {
                            Some(capture) => capture.stop_and_drain().await?,
                            None => Vec::new(),
                        };
                        let duration_ms = if !pcm.is_empty() {
                            (pcm.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64
                        } else {
                            0
                        };
                        RecordingResult {
                            pcm,
                            reason: RecordingReason::Manual,
                            duration_ms,
                            transcript: None,
                            transcript_error: None,
                        }
                    }
                },
                None => {
                    let capture = {
                        let guard = self.capture.lock().expect("capture lock poisoned");
                        guard.as_ref().cloned()
                    };
                    let pcm = match capture {
                        Some(capture) => capture.stop_and_drain().await?,
                        None => Vec::new(),
                    };
                    let duration_ms = if !pcm.is_empty() {
                        (pcm.len() as u64 * 1000) / TARGET_SAMPLE_RATE as u64
                    } else {
                        0
                    };
                    RecordingResult {
                        pcm,
                        reason: RecordingReason::Manual,
                        duration_ms,
                        transcript: None,
                        transcript_error: None,
                    }
                }
            }
        };

        *self.capture.lock().expect("capture lock poisoned") = None;
        *self.vad_engine.lock().expect("vad_engine lock poisoned") = None;
        self.vad_detector.lock().await.reset();

        Ok(result)
    }

    pub async fn encode_wav(&self, pcm_data: &[f32]) -> Result<Vec<u8>> {
        let config = self.config.lock().await;
        Ok(encode_wav_to_vec(
            pcm_data,
            config.sample_rate,
            config.channels,
        ))
    }

    pub async fn get_state(&self) -> RecordingState {
        self.state.lock().await.clone()
    }

    pub async fn update_config(&self, config: AudioConfig) {
        *self.config.lock().await = config;
    }
}

fn encode_wav_to_vec(pcm_data: &[f32], sample_rate: u32, channels: u16) -> Vec<u8> {
    let data_size = pcm_data.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_size);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_size as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&(channels * 2).to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());

    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_size as u32).to_le_bytes());
    for &sample in pcm_data {
        let clamped = sample.clamp(-1.0, 1.0);
        let int_sample = (clamped * 32767.0) as i16;
        wav.extend_from_slice(&int_sample.to_le_bytes());
    }

    wav
}

struct LoopData {
    config: Arc<Mutex<AudioConfig>>,
    capture: Arc<StdMutex<Option<AudioCaptureHandle>>>,
    vad_engine: Arc<StdMutex<Option<vad::VadEngine>>>,
    vad_detector: Arc<Mutex<vad::VadDetector>>,
    handler: Arc<StdMutex<Option<Arc<dyn InputHandler>>>>,
    /// Set by the CPAL stream error callback. The recording loop must observe
    /// it (L7) instead of draining a dead ring buffer until max_duration.
    failed: Arc<AtomicBool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_push_and_drain() {
        let mut rb = RingBuffer::new(10);
        rb.push(&[1.0, 2.0, 3.0]);
        assert_eq!(rb.len(), 3);
        let data = rb.drain();
        assert_eq!(data, vec![1.0, 2.0, 3.0]);
        assert_eq!(rb.len(), 0);
    }

    #[test]
    fn ring_buffer_overwrite() {
        let mut rb = RingBuffer::new(5);
        rb.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(rb.len(), 5);
        rb.push(&[6.0, 7.0]);
        assert_eq!(rb.len(), 5);
        let data = rb.drain();
        assert_eq!(data, vec![3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn ring_buffer_clear() {
        let mut rb = RingBuffer::new(10);
        rb.push(&[1.0, 2.0]);
        rb.clear();
        assert_eq!(rb.len(), 0);
        assert!(rb.drain().is_empty());
    }

    #[test]
    fn downmix_stereo_to_mono() {
        let stereo = vec![0.5, 0.3, 0.1, 0.9];
        let mono = downmix(&stereo, 2);
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.4).abs() < 1e-6);
        assert!((mono[1] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn fatal_stream_error_classification() {
        assert!(is_fatal_stream_error(
            &cpal::StreamError::DeviceNotAvailable
        ));
        let transient = cpal::StreamError::BackendSpecific {
            err: cpal::BackendSpecificError {
                description: "shared mode conflict".into(),
            },
        };
        assert!(!is_fatal_stream_error(&transient));
    }

    #[test]
    fn resampler_identity() {
        let mut rs = Resampler::new(16000.0, 16000.0);
        let input = vec![0.0, 0.5, 1.0, 0.5, 0.0];
        let out = rs.process(&input);
        assert_eq!(out.len(), input.len());
        for (a, b) in out.iter().zip(input.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn resampler_downsample() {
        let mut rs = Resampler::new(44100.0, 16000.0);
        let input: Vec<f32> = (0..44100)
            .map(|i| (i as f32 / 44100.0 * std::f32::consts::PI * 2.0).sin())
            .collect();
        let out = rs.process(&input);
        assert!(out.len() < input.len());
        assert!((out.len() as i32 - 16000).abs() <= 1);
    }

    #[test]
    fn encode_wav_produces_valid_header() {
        let pipeline = InputPipeline::new();
        let pcm = vec![0.0f32, 0.5, -0.5, 1.0, -1.0];
        let wav = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(pipeline.encode_wav(&pcm))
            .unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let data_size = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_size as usize, pcm.len() * 2);
    }

    #[test]
    fn recording_reason_serde() {
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

    // --- InputPipeline tests (no hardware required) ---

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
        // Verify that encode_wav uses the new sample_rate/channels
        let pcm = vec![0.0f32];
        let wav = pipeline.encode_wav(&pcm).await.unwrap();
        // Channels (offset 22) should be 2
        let channels = u16::from_le_bytes([wav[22], wav[23]]);
        assert_eq!(channels, 2);
        // Sample rate (offset 24) should be 44100
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 44100);
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
        // Clearing with None disables transcription again.
        pipeline.set_stt_client(None).await;
        assert!(pipeline.stt_client.lock().await.is_none());
    }

    #[tokio::test]
    async fn test_set_on_auto_stop() {
        struct StopHandler;
        #[async_trait]
        impl InputHandler for StopHandler {
            async fn on_auto_stop(&self) {}
        }
        let pipeline = InputPipeline::new();
        pipeline.set_handler(Arc::new(StopHandler));
    }

    #[test]
    fn test_set_vad_status_callback() {
        struct VadHandler;
        #[async_trait]
        impl InputHandler for VadHandler {
            fn on_vad_status(&self, _signal: vad::VadSignal, _state: vad::VadState) {}
        }
        let pipeline = InputPipeline::new();
        pipeline.set_handler(Arc::new(VadHandler));
    }

    #[tokio::test]
    async fn test_process_vad_frame_no_engine_returns_none() {
        let pipeline = InputPipeline::new();
        let frame = vec![0.0f32; 480];
        let signal = pipeline.process_vad_frame(&frame).await;
        assert_eq!(signal, vad::VadSignal::None);
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

    // --- Final drain path: no capture handle produces no pcm ---

    #[test]
    fn test_recording_result_empty_pcm_duration_zero() {
        let result = RecordingResult {
            pcm: vec![],
            reason: RecordingReason::Cancel,
            duration_ms: 0,
            transcript: None,
            transcript_error: None,
        };
        assert!(result.pcm.is_empty());
        assert_eq!(result.duration_ms, 0);
    }

    #[tokio::test]
    async fn test_transcribe_skips_empty_text() {
        // An empty transcription (silence / too-short clip) must leave both
        // fields unset instead of surfacing as an error or blank input.
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
