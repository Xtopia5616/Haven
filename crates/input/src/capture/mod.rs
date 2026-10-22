//! Capture engine: a single long-lived thread that owns the CPAL capture
//! backend and the ring buffer, and serves the recording loop's commands.
//!
//! **Capture path** — a CPAL input stream (WASAPI shared mode on Windows) is
//! opened on `Start` and torn down on `Stop`. The audio callback converts
//! whatever sample format the device delivers to mono 16 kHz f32, pushes it
//! into the ring, and flips `has_signal` as soon as real audio is seen.
//!
//! **Consumption** — the ring is mutex-protected and shared with the
//! recording loop, which drains currently buffered samples directly
//! (`CaptureEngineHandle::drain_buffered_into`)
//! with no engine round-trip. The capture engine thread handles start/stop
//! commands and runs the silent-capture check on its poll cadence while recording.
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

use backend::{CaptureMetrics, CaptureSignals, CpalBackend};
use ring::RingBuffer;

mod backend;
mod resample;
mod ring;

pub(super) use backend::TARGET_SAMPLE_RATE;

/// Upper bound for a command round-trip. The engine answers in microseconds;
/// the timeout only guards against a dead engine thread.
const CMD_TIMEOUT: Duration = Duration::from_secs(2);
/// Fallback command-wait for the engine while a recording's silence check is
/// pending but `started_at` is unset (defensive; the start path always sets
/// both together). Real waits sleep until the check deadline instead.
const MONITOR_INTERVAL: Duration = Duration::from_millis(20);
/// How much of a fresh recording is observed before deciding the capture is
/// delivering pure digital silence and aborting with an error. Deliberately
/// generous: WASAPI streams start with a silence transient, the effects APO
/// can gate quiet input to digital zero, and users take a beat to start
/// speaking after pressing record.
const SILENCE_CHECK_DELAY: Duration = Duration::from_millis(3000);
/// Ring capacity is derived from `context_limits.input_ring_buffer_secs`
/// (default 20 seconds of 16 kHz mono) at capture-engine spawn time.
const DEFAULT_RING_CAPACITY: usize = TARGET_SAMPLE_RATE as usize * 20;

enum CaptureEngineCommand {
    Start(tokio::sync::oneshot::Sender<Result<()>>),
    StopAndDrain(tokio::sync::oneshot::Sender<Vec<f32>>),
    StopAndClear,
    Shutdown,
}

/// Client-side handle to the capture engine thread.
#[derive(Clone)]
pub struct CaptureEngineHandle {
    cmd_tx: mpsc::Sender<CaptureEngineCommand>,
    thread: Arc<StdMutex<Option<std::thread::JoinHandle<()>>>>,
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

impl CaptureEngineHandle {
    /// Open the capture stream and clear the ring.
    pub async fn start(&self) -> Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self.cmd_tx.send(CaptureEngineCommand::Start(tx)).is_err() {
            return Err(anyhow!("capture engine is gone"));
        }
        match tokio::time::timeout(CMD_TIMEOUT, rx).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(e))) => Err(e),
            _ => Err(anyhow!("capture engine did not respond to start")),
        }
    }

    /// Drain directly into reusable caller-owned storage. Capacity growth
    /// happens before the final ring lock is acquired, keeping allocation
    /// and sample copying out of the callback's critical section.
    pub fn drain_buffered_into(&self, out: &mut Vec<f32>) {
        drain_ring_into(&self.ring, out);
    }

    /// Stop the capture stream (releasing the device), drain the ring and
    /// report the final tail.
    pub async fn stop_and_drain(&self) -> Result<Vec<f32>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(CaptureEngineCommand::StopAndDrain(tx))
            .is_err()
        {
            return Err(anyhow!("capture engine is gone while stopping"));
        }
        let data = match tokio::time::timeout(CMD_TIMEOUT, rx).await {
            Ok(Ok(data)) => data,
            Ok(Err(_)) => {
                return Err(anyhow!("capture engine dropped the stop reply"));
            }
            Err(_) => {
                return Err(anyhow!(
                    "capture engine did not reply within {:?}",
                    CMD_TIMEOUT
                ));
            }
        };
        Ok(data)
    }

    /// Cancel path: stop the capture stream and drop the ring contents.
    pub fn stop_and_clear(&self) {
        let _ = self.cmd_tx.send(CaptureEngineCommand::StopAndClear);
    }

    /// Stop the engine thread and wait for its stream teardown to finish.
    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.cmd_tx.send(CaptureEngineCommand::Shutdown);
        let Some(thread) = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("capture engine thread lock poisoned; recovering state");
                poisoned.into_inner()
            })
            .take()
        else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || thread.join())
            .await
            .map_err(|error| anyhow!("capture engine join task failed: {error}"))?
            .map_err(|_| anyhow!("capture engine thread panicked"))
    }
}

/// Drain the ring into reusable storage. The short first lock snapshots the
/// required capacity; if the producer fills more before the second lock,
/// retry after reserving outside the mutex. The final copy is at most two
/// contiguous slices and cannot allocate while holding the ring lock.
fn drain_ring_into(ring: &Arc<StdMutex<RingBuffer>>, out: &mut Vec<f32>) {
    out.clear();
    loop {
        let needed = ring
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("capture ring lock poisoned; recovering state");
                poisoned.into_inner()
            })
            .buffered_len();
        if out.capacity() < needed {
            out.reserve(needed);
        }

        let mut ring = ring.lock().unwrap_or_else(|poisoned| {
            tracing::error!("capture ring lock poisoned; recovering state");
            poisoned.into_inner()
        });
        if out.capacity() < ring.buffered_len() {
            drop(ring);
            continue;
        }
        ring.drain_into(out);
        return;
    }
}

/// Spawn the capture engine thread. `ring_capacity_secs` (from
/// `context_limits.input_ring_buffer_secs`) sets the audio ring size.
pub fn spawn_engine(ring_capacity_secs: usize) -> Result<CaptureEngineHandle> {
    let capacity = if ring_capacity_secs == 0 {
        DEFAULT_RING_CAPACITY
    } else {
        TARGET_SAMPLE_RATE as usize * ring_capacity_secs
    };
    let ring = Arc::new(StdMutex::new(RingBuffer::new(capacity)));
    let metrics = Arc::new(CaptureMetrics::default());
    let stream_failed = Arc::new(AtomicBool::new(false));
    let silent_abort = Arc::new(AtomicBool::new(false));

    let (cmd_tx, cmd_rx) = mpsc::channel::<CaptureEngineCommand>();
    let engine_ring = ring.clone();
    let engine_metrics = metrics.clone();
    let engine_stream_failed = stream_failed.clone();
    let engine_silent_abort = silent_abort.clone();
    let thread = std::thread::Builder::new()
        .name("haven-audio-engine".into())
        .spawn(move || {
            let mut engine = CaptureEngine {
                ring: engine_ring,
                backend: None,
                signals: CaptureSignals::with_metrics(
                    engine_metrics.clone(),
                    engine_stream_failed.clone(),
                ),
                metrics: engine_metrics,
                out_failed: engine_stream_failed,
                out_silent_abort: engine_silent_abort,
                recording: false,
                started_at: None,
                silent_checked: false,
                diagnostics_logged: false,
            };
            loop {
                // Sleep only while the silent-capture check is still
                // pending, and only until its deadline; after the check has
                // run the engine blocks indefinitely (commands wake it). An
                // idle or recording engine therefore never wakes 50x/sec and
                // keeps the CPU out of low power states. Commands are still
                // answered immediately: `recv_timeout` returns as soon as a
                // message arrives.
                let cmd = if engine.recording && !engine.silent_checked {
                    let remaining = match engine.started_at {
                        Some(started) => (started + SILENCE_CHECK_DELAY)
                            .saturating_duration_since(Instant::now()),
                        None => MONITOR_INTERVAL,
                    };
                    match cmd_rx.recv_timeout(remaining) {
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
                    Some(CaptureEngineCommand::Start(reply)) => {
                        engine.cmd_start(reply);
                    }
                    Some(CaptureEngineCommand::StopAndDrain(tx)) => {
                        let data = engine.cmd_stop_and_drain();
                        let _ = tx.send(data);
                    }
                    Some(CaptureEngineCommand::StopAndClear) => {
                        engine.cmd_stop_and_clear();
                    }
                    Some(CaptureEngineCommand::Shutdown) => {
                        engine.teardown();
                        return;
                    }
                    None => {
                        engine.poll_timeout();
                    }
                }
            }
        })
        .map_err(|e| anyhow!("failed to spawn capture engine: {e}"))?;

    let handle = CaptureEngineHandle {
        cmd_tx,
        thread: Arc::new(StdMutex::new(Some(thread))),
        ring,
        stream_failed,
        silent_abort,
    };

    Ok(handle)
}

struct CaptureEngine {
    ring: Arc<StdMutex<RingBuffer>>,
    metrics: Arc<CaptureMetrics>,
    backend: Option<CpalBackend>,
    signals: CaptureSignals,
    /// Stable; exposed on the handle (L7).
    out_failed: Arc<AtomicBool>,
    /// Stable; exposed on the handle (silent-capture abort).
    out_silent_abort: Arc<AtomicBool>,
    recording: bool,
    started_at: Option<Instant>,
    silent_checked: bool,
    diagnostics_logged: bool,
}

impl CaptureEngine {
    fn cmd_start(&mut self, reply: tokio::sync::oneshot::Sender<Result<()>>) {
        // Release any leftover session before opening a new one.
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
            self.log_capture_diagnostics();
        }
        self.metrics.reset();
        self.diagnostics_logged = false;
        self.signals = Self::new_signals(&self.out_failed, &self.metrics);
        self.ring
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("capture ring lock poisoned; recovering state");
                poisoned.into_inner()
            })
            .clear();
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
                tracing::error!(
                    error = %haven_common::error::sanitize_error_text(&format!("{e:#}")),
                    "cpal capture start failed"
                );
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
        let mut out = Vec::new();
        drain_ring_into(&self.ring, &mut out);
        self.log_capture_diagnostics();
        out
    }

    fn cmd_stop_and_clear(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
        }
        self.log_capture_diagnostics();
        self.recording = false;
        self.started_at = None;
        self.out_failed.store(false, Ordering::SeqCst);
        self.ring
            .lock()
            .unwrap_or_else(|poisoned| {
                tracing::error!("capture ring lock poisoned; recovering state");
                poisoned.into_inner()
            })
            .clear();
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
                self.log_capture_diagnostics();
            }
        }
    }

    fn teardown(&mut self) {
        if let Some(mut backend) = self.backend.take() {
            let _ = backend.stop();
        }
        self.log_capture_diagnostics();
    }

    fn new_signals(out_failed: &Arc<AtomicBool>, metrics: &Arc<CaptureMetrics>) -> CaptureSignals {
        CaptureSignals::with_metrics(metrics.clone(), out_failed.clone())
    }

    fn log_capture_diagnostics(&mut self) {
        if self.diagnostics_logged {
            return;
        }
        let snapshot = self.metrics.snapshot();
        if snapshot.callback_count == 0 {
            return;
        }
        self.diagnostics_logged = true;
        let callback_avg_us = snapshot.callback_total_ns / snapshot.callback_count / 1000;
        let lock_wait_avg_us = snapshot
            .lock_wait_total_ns
            .checked_div(snapshot.lock_wait_count)
            .unwrap_or_default()
            / 1000;
        tracing::debug!(
            callback_count = snapshot.callback_count,
            callback_avg_duration_us = callback_avg_us,
            callback_max_duration_us = snapshot.callback_max_ns / 1000,
            callback_lock_wait_count = snapshot.lock_wait_count,
            callback_lock_wait_avg_us = lock_wait_avg_us,
            callback_lock_wait_max_us = snapshot.lock_wait_max_ns / 1000,
            dropped_sample_count = snapshot.dropped_samples,
            poisoned_lock_count = snapshot.poisoned_locks,
            "capture diagnostics"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_joins_the_capture_engine_thread_without_opening_a_device() {
        let engine = spawn_engine(0).unwrap();

        engine.shutdown().await.unwrap();
        engine.shutdown().await.unwrap();

        assert!(
            engine
                .thread
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_none()
        );
        assert!(engine.cmd_tx.send(CaptureEngineCommand::Shutdown).is_err());
    }
}
