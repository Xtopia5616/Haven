//! Capture engine: a single long-lived thread that owns the CPAL capture
//! backend and the ring buffer, and serves the recording loop's commands.
//!
//! **Capture path** — a CPAL input stream (WASAPI shared mode on Windows) is
//! opened on `Start` and torn down on `Stop`. The audio callback converts
//! whatever sample format the device delivers to mono 16 kHz f32, pushes it
//! into the ring, and flips `has_signal` as soon as real audio is seen.
//!
//! **Consumption** — the ring is mutex-protected and shared with the
//! recording loop, which drains it directly (`EngineHandle::drain_shared`)
//! with no engine round-trip. The engine thread handles start/stop commands
//! and runs the silent-capture check on its poll cadence while recording.
//!
//! **Silent-capture detection** — if the first [`SILENCE_CHECK_DELAY`] of a
//! recording is pure digital silence (the device delivered no signal at
//! all), the engine aborts the recording immediately and sets
//! `silent_abort`. The pipeline surfaces this as an error and the request is
//! never sent, instead of shipping an all-zero recording.
//!
//! Responsibilities:
//!
//! - **Lifecycle** — open/close the CPAL stream on `Start` / `Stop`. The
//!   stream runs only while recording because it claims the microphone.
//! - **Ring feeding** — the stream callback feeds the ring with mono 16 kHz
//!   f32 directly.
//! - **Fatal errors** — stream errors (device loss) arrive via CPAL's error
//!   callback, which sets `stream_failed` so the recording loop stops early
//!   (L7) instead of draining a dead ring.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use backend::{CaptureSignals, CpalBackend};

mod backend;
mod resample;
mod ring;

pub use backend::{SIGNAL_FLOOR, TARGET_SAMPLE_RATE};
pub use resample::Resampler;
pub use ring::RingBuffer;

/// Upper bound for a command round-trip. The engine answers in microseconds;
/// the timeout only guards against a dead engine thread.
const CMD_TIMEOUT: Duration = Duration::from_secs(2);
/// Engine command-channel poll cadence while a recording is active.
const MONITOR_INTERVAL: Duration = Duration::from_millis(20);
/// How much of a fresh recording is observed before deciding the capture is
/// delivering pure digital silence and aborting with an error. Deliberately
/// generous: WASAPI streams start with a silence transient, the effects APO
/// can gate quiet input to digital zero, and users take a beat to start
/// speaking after pressing record.
const SILENCE_CHECK_DELAY: Duration = Duration::from_millis(3000);
/// Ring capacity: 20 seconds of 16 kHz mono.
const RING_CAPACITY: usize = TARGET_SAMPLE_RATE as usize * 20;

enum EngineCommand {
    Start(tokio::sync::oneshot::Sender<Result<()>>),
    StopAndDrain(tokio::sync::oneshot::Sender<Vec<f32>>),
    StopAndClear,
}

/// Client-side handle to the engine thread.
#[derive(Clone)]
pub struct EngineHandle {
    cmd_tx: mpsc::Sender<EngineCommand>,
    /// Shared ring; the recording loop drains it directly (mutex-protected),
    /// so audio consumption needs no command round-trip.
    ring: Arc<StdMutex<RingBuffer>>,
    /// Set on a fatal stream error; the recording loop stops early when it
    /// observes it (instead of draining a dead ring until max duration).
    pub stream_failed: Arc<AtomicBool>,
    /// Set when the engine aborts a recording because the capture delivered
    /// pure digital silence for the opening [`SILENCE_CHECK_DELAY`]. The
    /// pipeline turns this into an error and never sends the request.
    pub silent_abort: Arc<AtomicBool>,
}

impl EngineHandle {
    /// Open the capture stream and clear the ring.
    pub async fn start(&self) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(EngineCommand::Start(tx))
            .is_err()
        {
            return Err(anyhow!("capture engine is gone"));
        }
        match tokio::time::timeout(CMD_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            _ => Err(anyhow!("capture engine did not respond to start")),
        }
    }

    /// Drain the ring directly. No engine round-trip: the ring is
    /// mutex-protected, so the recording loop consumes audio without waiting
    /// for the engine's command poll. Only the mutex is contended, and only
    /// for the duration of one copy.
    pub fn drain_shared(&self) -> Vec<f32> {
        self.ring.lock().expect("ring lock poisoned").drain()
    }

    /// Stop the capture stream (releasing the device), drain the ring and
    /// report the final tail.
    pub async fn stop_and_drain(&self) -> Result<Vec<f32>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.cmd_tx.send(EngineCommand::StopAndDrain(tx)).is_err() {
            return Ok(Vec::new());
        }
        let data = tokio::time::timeout(CMD_TIMEOUT, rx)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
        Ok(data)
    }

    /// Cancel path: stop the capture stream and drop the ring contents.
    pub fn stop_and_clear(&self) {
        let _ = self.cmd_tx.send(EngineCommand::StopAndClear);
    }
}

/// Spawn the capture engine thread.
pub fn spawn_engine() -> Result<EngineHandle> {
    let ring = Arc::new(StdMutex::new(RingBuffer::new(RING_CAPACITY)));
    let stream_failed = Arc::new(AtomicBool::new(false));
    let silent_abort = Arc::new(AtomicBool::new(false));

    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>();
    let handle = EngineHandle {
        cmd_tx: cmd_tx.clone(),
        ring: ring.clone(),
        stream_failed: stream_failed.clone(),
        silent_abort: silent_abort.clone(),
    };

    std::thread::Builder::new()
        .name("haven-audio-engine".into())
        .spawn(move || {
            let mut engine = Engine {
                ring,
                backend: None,
                signals: CaptureSignals::new(),
                out_failed: stream_failed,
                out_silent_abort: silent_abort,
                recording: false,
                started_at: None,
                silent_checked: false,
            };
            loop {
                // Poll while recording (the silent-capture check runs on the
                // poll cadence); block indefinitely while idle so an idle
                // engine does not wake 50x/sec and keep the CPU out of low
                // power states.
                let cmd = if engine.recording {
                    match cmd_rx.recv_timeout(MONITOR_INTERVAL) {
                        Ok(cmd) => Some(cmd),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            engine.teardown();
                            return;
                        }
                    }
                } else {
                    match cmd_rx.recv() {
                        Ok(cmd) => Some(cmd),
                        Err(_) => {
                            engine.teardown();
                            return;
                        }
                    }
                };
                match cmd {
                    Some(EngineCommand::Start(reply)) => {
                        engine.cmd_start(reply);
                    }
                    Some(EngineCommand::StopAndDrain(tx)) => {
                        let data = engine.cmd_stop_and_drain();
                        let _ = tx.send(data);
                    }
                    Some(EngineCommand::StopAndClear) => {
                        engine.cmd_stop_and_clear();
                    }
                    None => {
                        engine.poll_timeout();
                    }
                }
            }
        })
        .map_err(|e| anyhow!("failed to spawn capture engine: {e}"))?;

    Ok(handle)
}

struct Engine {
    ring: Arc<StdMutex<RingBuffer>>,
    backend: Option<CpalBackend>,
    signals: CaptureSignals,
    /// Stable; exposed on the handle (L7).
    out_failed: Arc<AtomicBool>,
    /// Stable; exposed on the handle (silent-capture abort).
    out_silent_abort: Arc<AtomicBool>,
    recording: bool,
    started_at: Option<Instant>,
    silent_checked: bool,
}

impl Engine {
    fn cmd_start(&mut self, reply: tokio::sync::oneshot::Sender<Result<()>>) {
        // Release any leftover session before opening a new one.
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
        }
        self.signals = Self::new_signals(&self.out_failed);
        self.ring.lock().expect("ring lock poisoned").clear();
        self.out_failed.store(false, Ordering::SeqCst);
        self.out_silent_abort.store(false, Ordering::SeqCst);

        let result = CpalBackend::new(self.ring.clone(), self.signals.clone())
            .and_then(|mut b| b.start().map(|_| b));
        match result {
            Ok(backend) => {
                self.backend = Some(backend);
                self.recording = true;
                self.started_at = Some(Instant::now());
                self.silent_checked = false;
                tracing::info!("recording started via cpal capture");
                let _ = reply.send(Ok(()));
            }
            Err(e) => {
                tracing::error!("cpal capture start failed: {e:#}");
                self.recording = false;
                let _ = reply.send(Err(e));
            }
        }
    }

    fn cmd_stop_and_drain(&mut self) -> Vec<f32> {
        // Release the stream immediately (it claims the microphone).
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
        }
        self.recording = false;
        self.started_at = None;
        self.out_failed.store(false, Ordering::SeqCst);
        self.ring.lock().expect("ring lock poisoned").drain()
    }

    fn cmd_stop_and_clear(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
        }
        self.recording = false;
        self.started_at = None;
        self.out_failed.store(false, Ordering::SeqCst);
        self.ring.lock().expect("ring lock poisoned").clear();
    }

    /// Background work between commands: run the silent-capture check. Audio
    /// delivery is callback-driven, so nothing is pulled here.
    fn poll_timeout(&mut self) {
        if self.recording
            && !self.silent_checked
            && let Some(started) = self.started_at
            && started.elapsed() >= SILENCE_CHECK_DELAY
        {
            self.silent_checked = true;
            if !self.signals.has_signal.load(Ordering::SeqCst) {
                tracing::error!(
                    "capture delivered no signal in the first {}ms; aborting recording",
                    SILENCE_CHECK_DELAY.as_millis()
                );
                self.out_silent_abort.store(true, Ordering::SeqCst);
                self.recording = false;
                self.started_at = None;
                if let Some(mut backend) = self.backend.take() {
                    let _ = backend.stop();
                }
            }
        }
    }

    fn teardown(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
        }
    }

    fn new_signals(out_failed: &Arc<AtomicBool>) -> CaptureSignals {
        CaptureSignals {
            has_signal: Arc::new(AtomicBool::new(false)),
            stream_failed: out_failed.clone(),
        }
    }
}
