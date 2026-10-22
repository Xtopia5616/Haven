//! CPAL capture backend.
//!
//! The recording path runs on a plain CPAL input stream (WASAPI shared mode
//! on Windows). CPAL invokes the data callback on a dedicated high-priority
//! thread with whatever sample format was negotiated (f32/i16/i32/u16/...);
//! the callback converts it to mono 16 kHz f32, resamples it, and pushes it
//! into the ring buffer. `has_signal` is flipped as soon as a real
//! (non-digital-silence) sample is seen, and stream errors (e.g. device
//! loss) flip `stream_failed` so the recording loop can stop early instead of
//! draining a dead ring.
//!
//! The backend is created and driven exclusively by the engine thread (never
//! moved across threads), so the `!Send` CPAL stream is safe here. Unlike the
//! previous hand-rolled WASAPI exclusive-mode backend, no device format
//! negotiation is needed: CPAL picks a shared-mode format (usually the 48 kHz
//! stereo mix format) and the callback converts.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SampleRate, SizedSample};

use super::resample::Resampler;
use super::ring::RingBuffer;

/// Pipeline target format: mono 16 kHz f32.
pub(crate) const TARGET_SAMPLE_RATE: u32 = 16000;
/// Anything above this amplitude counts as "the data path is alive". A
/// stream whose samples never exceed this is treated as digital silence.
/// Set below a quiet room's typical noise floor (≈ -60..-90 dBFS) so the
/// silent-capture abort only fires on genuinely dead streams.
const SIGNAL_FLOOR: f32 = 1e-5;
/// Fixed-memory aggregate metrics shared with the capture engine. Updates
/// happen once per callback and never log or allocate on CPAL's audio thread.
#[derive(Default)]
pub(super) struct CaptureMetrics {
    callback_count: AtomicU64,
    callback_total_ns: AtomicU64,
    callback_max_ns: AtomicU64,
    lock_wait_count: AtomicU64,
    lock_wait_total_ns: AtomicU64,
    lock_wait_max_ns: AtomicU64,
    dropped_samples: AtomicU64,
    poisoned_locks: AtomicU64,
}

#[derive(Clone, Copy, Default)]
pub(super) struct CaptureMetricsSnapshot {
    pub(super) callback_count: u64,
    pub(super) callback_total_ns: u64,
    pub(super) callback_max_ns: u64,
    pub(super) lock_wait_count: u64,
    pub(super) lock_wait_total_ns: u64,
    pub(super) lock_wait_max_ns: u64,
    pub(super) dropped_samples: u64,
    pub(super) poisoned_locks: u64,
}

impl CaptureMetrics {
    pub(super) fn reset(&self) {
        self.callback_count.store(0, Ordering::Relaxed);
        self.callback_total_ns.store(0, Ordering::Relaxed);
        self.callback_max_ns.store(0, Ordering::Relaxed);
        self.lock_wait_count.store(0, Ordering::Relaxed);
        self.lock_wait_total_ns.store(0, Ordering::Relaxed);
        self.lock_wait_max_ns.store(0, Ordering::Relaxed);
        self.dropped_samples.store(0, Ordering::Relaxed);
        self.poisoned_locks.store(0, Ordering::Relaxed);
    }

    fn observe_callback(&self, elapsed: Duration) {
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.callback_count.fetch_add(1, Ordering::Relaxed);
        self.callback_total_ns.fetch_add(nanos, Ordering::Relaxed);
        self.callback_max_ns.fetch_max(nanos, Ordering::Relaxed);
    }

    pub(super) fn observe_lock_wait(&self, elapsed: Duration) {
        let nanos = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        self.lock_wait_count.fetch_add(1, Ordering::Relaxed);
        self.lock_wait_total_ns.fetch_add(nanos, Ordering::Relaxed);
        self.lock_wait_max_ns.fetch_max(nanos, Ordering::Relaxed);
    }

    fn observe_dropped_samples(&self, count: usize) {
        self.dropped_samples
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
    }

    fn observe_poisoned_lock(&self) {
        self.poisoned_locks.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn snapshot(&self) -> CaptureMetricsSnapshot {
        CaptureMetricsSnapshot {
            callback_count: self.callback_count.load(Ordering::Relaxed),
            callback_total_ns: self.callback_total_ns.load(Ordering::Relaxed),
            callback_max_ns: self.callback_max_ns.load(Ordering::Relaxed),
            lock_wait_count: self.lock_wait_count.load(Ordering::Relaxed),
            lock_wait_total_ns: self.lock_wait_total_ns.load(Ordering::Relaxed),
            lock_wait_max_ns: self.lock_wait_max_ns.load(Ordering::Relaxed),
            dropped_samples: self.dropped_samples.load(Ordering::Relaxed),
            poisoned_locks: self.poisoned_locks.load(Ordering::Relaxed),
        }
    }
}

/// Shared flags between the capture callback and the engine.
#[derive(Clone)]
pub struct CaptureSignals {
    /// Set by the data callback when any delivered sample exceeds
    /// [`SIGNAL_FLOOR`]. Reset by the engine before each start.
    pub has_signal: Arc<AtomicBool>,
    /// Set by the stream error callback (device lost, etc.). Wired to the
    /// engine's `stream_failed` flag so the recording loop stops early.
    pub stream_failed: Arc<AtomicBool>,
    pub(super) metrics: Arc<CaptureMetrics>,
}

impl CaptureSignals {
    #[cfg(test)]
    pub fn new() -> Self {
        Self {
            has_signal: Arc::new(AtomicBool::new(false)),
            stream_failed: Arc::new(AtomicBool::new(false)),
            metrics: Arc::new(CaptureMetrics::default()),
        }
    }

    pub(super) fn with_metrics(
        metrics: Arc<CaptureMetrics>,
        stream_failed: Arc<AtomicBool>,
    ) -> Self {
        Self {
            has_signal: Arc::new(AtomicBool::new(false)),
            stream_failed,
            metrics,
        }
    }
}

/// CPAL capture backend, driven by the engine thread: `start` opens the
/// stream, `stop` pauses and drops it.
pub struct CpalBackend {
    ring: Arc<StdMutex<RingBuffer>>,
    signals: CaptureSignals,
    stream: Option<cpal::Stream>,
    running: bool,
}

impl CpalBackend {
    pub fn new(ring: Arc<StdMutex<RingBuffer>>, signals: CaptureSignals) -> Result<Self> {
        Ok(Self {
            ring,
            signals,
            stream: None,
            running: false,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device available"))?;
        let (config, sample_format) = pick_config(&device)?;
        let channels = config.channels;
        let sample_rate = config.sample_rate.0;
        let stream = build_stream(
            &device,
            &config,
            sample_format,
            self.ring.clone(),
            self.signals.clone(),
        )?;
        stream.play()?;
        self.stream = Some(stream);
        self.running = true;
        tracing::info!(
            sample_rate_hz = sample_rate,
            channels,
            sample_format = %sample_format,
            "cpal capture running"
        );
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }
        // Pausing then dropping releases the device; in-flight callbacks
        // finish before the stream is torn down, so the ring stays intact.
        if let Some(stream) = self.stream.take()
            && let Err(error) = stream.pause()
        {
            tracing::warn!(
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to pause cpal capture stream during stop"
            );
        }
        self.running = false;
        tracing::debug!("cpal capture stopped");
        Ok(())
    }
}

/// Negotiate a stream config: prefer the device's default (WASAPI shared
/// mode typically offers the 48 kHz mix format), falling back to the best
/// supported range (cpal's default heuristics) with a rate near 48 kHz.
fn pick_config(device: &cpal::Device) -> Result<(cpal::StreamConfig, SampleFormat)> {
    if let Ok(default) = device.default_input_config() {
        let format = default.sample_format();
        return Ok((default.config(), format));
    }
    let ranges = device.supported_input_configs()?;
    let range = ranges
        .max_by(|a, b| a.cmp_default_heuristics(b))
        .ok_or_else(|| anyhow!("device exposes no supported input configs"))?;
    let format = range.sample_format();
    let config = range
        .try_with_sample_rate(choose_rate(&range))
        .ok_or_else(|| anyhow!("chosen rate outside supported range"))?;
    Ok((config.config(), format))
}

/// Prefer 48 kHz (the usual Windows mix rate) when the range allows it,
/// otherwise the range's maximum.
fn choose_rate(range: &cpal::SupportedStreamConfigRange) -> SampleRate {
    const PREFERRED: u32 = 48000;
    if (range.min_sample_rate().0..=range.max_sample_rate().0).contains(&PREFERRED) {
        SampleRate(PREFERRED)
    } else {
        range.max_sample_rate()
    }
}

/// Open a CPAL input stream with the negotiated format. The sample format
/// drives the typed callback; all formats are converted to f32 downstream.
fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: SampleFormat,
    ring: Arc<StdMutex<RingBuffer>>,
    signals: CaptureSignals,
) -> Result<cpal::Stream> {
    match sample_format {
        SampleFormat::F32 => build_typed::<f32>(device, config, ring, signals),
        SampleFormat::I8 => build_typed::<i8>(device, config, ring, signals),
        SampleFormat::I16 => build_typed::<i16>(device, config, ring, signals),
        SampleFormat::I32 => build_typed::<i32>(device, config, ring, signals),
        SampleFormat::I64 => build_typed::<i64>(device, config, ring, signals),
        SampleFormat::U8 => build_typed::<u8>(device, config, ring, signals),
        SampleFormat::U16 => build_typed::<u16>(device, config, ring, signals),
        SampleFormat::U32 => build_typed::<u32>(device, config, ring, signals),
        SampleFormat::U64 => build_typed::<u64>(device, config, ring, signals),
        SampleFormat::F64 => build_typed::<f64>(device, config, ring, signals),
        other => Err(anyhow!("unsupported capture sample format: {other}")),
    }
}

fn build_typed<T: SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    ring: Arc<StdMutex<RingBuffer>>,
    signals: CaptureSignals,
) -> Result<cpal::Stream>
where
    f32: FromSample<T>,
{
    let channels = config.channels;
    let sample_rate = config.sample_rate.0 as f64;
    let frames_hint = match &config.buffer_size {
        cpal::BufferSize::Fixed(frames) => *frames as usize,
        // CPAL leaves the OS period unspecified for Default. Reserve for a
        // 20 ms callback, which covers common shared-mode periods while
        // retaining the grow-on-unexpected-oversize fallback.
        cpal::BufferSize::Default => ((sample_rate as usize) / 50).max(1),
    };
    let output_hint = ((frames_hint as f64 * TARGET_SAMPLE_RATE as f64 / sample_rate).ceil()
        as usize)
        .saturating_add(2);
    let mut resampler = Resampler::with_capacity(
        sample_rate,
        TARGET_SAMPLE_RATE as f64,
        frames_hint.saturating_add(2),
    );
    // Reuse callback-owned scratch storage, initially sized from the
    // negotiated callback period. Drivers that exceed the hint may still
    // trigger a later capacity growth.
    let mut mono = Vec::with_capacity(frames_hint);
    let mut resampled = Vec::with_capacity(output_hint);
    let data_signals = signals.clone();
    let err_signals = signals;
    let stream = device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _info: &cpal::InputCallbackInfo| {
            process_chunk(
                data,
                channels,
                &mut resampler,
                &mut mono,
                &mut resampled,
                &ring,
                &data_signals,
            );
        },
        move |_err| {
            // Keep CPAL callbacks free of logging I/O. The recording loop
            // reports the stable failure to the user-facing layer.
            err_signals.stream_failed.store(true, Ordering::SeqCst);
        },
        None,
    )?;
    Ok(stream)
}

/// Convert a callback chunk to mono f32, resample it to
/// [`TARGET_SAMPLE_RATE`], detect signal, and push it into the ring. The
/// mutex is held for the shortest possible window (a single push). `mono`
/// and `resampled` are caller-owned scratch buffers reused across callbacks.
/// The chunk is scanned exactly once: peak and signal-floor detection fold
/// into the mixdown loop instead of extra passes over the data.
fn process_chunk<T: Sample>(
    data: &[T],
    channels: u16,
    resampler: &mut Resampler,
    mono: &mut Vec<f32>,
    resampled: &mut Vec<f32>,
    ring: &Arc<StdMutex<RingBuffer>>,
    signals: &CaptureSignals,
) where
    f32: FromSample<T>,
{
    let callback_started = Instant::now();
    let ch = channels.max(1) as usize;
    let frames = data.len() / ch;
    mono.clear();
    mono.reserve(frames);
    // Peak of the chunk's mono mixdown drives the signal-floor check without
    // a second scan. It is never recorded in diagnostics.
    let mut peak = 0.0f32;
    for frame in data.chunks_exact(ch) {
        let mut sum = 0.0f32;
        for &s in frame {
            let v = s.to_sample::<f32>();
            let a = v.abs();
            if a > peak {
                peak = a;
            }
            sum += v;
        }
        mono.push(sum / ch as f32);
    }
    resampler.resample_into(mono, resampled);
    // Checked on the source mixdown: if any source sample exceeds the floor,
    // the resampled stream carries signal too (interpolation of the peak's
    // neighbors stays far above the -80 dBFS floor), and scanning here saves
    // a second pass over the data on the real-time thread.
    if peak > SIGNAL_FLOOR {
        signals.has_signal.store(true, Ordering::SeqCst);
    }
    let lock_started = Instant::now();
    let ring_guard = ring.lock();
    let lock_wait = lock_started.elapsed();
    match ring_guard {
        Ok(mut ring) => {
            let dropped = ring.push(resampled.as_slice());
            drop(ring);
            signals.metrics.observe_lock_wait(lock_wait);
            signals.metrics.observe_dropped_samples(dropped);
        }
        Err(poisoned) => {
            // The engine thread panicked and poisoned the ring. Recover the
            // guard for teardown, but also stop the recording instead of
            // silently continuing with a stream whose samples are discarded.
            drop(poisoned);
            signals.metrics.observe_lock_wait(lock_wait);
            signals.metrics.observe_poisoned_lock();
            signals.metrics.observe_dropped_samples(resampled.len());
            signals.stream_failed.store(true, Ordering::SeqCst);
        }
    }
    signals.metrics.observe_callback(callback_started.elapsed());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture for `process_chunk` tests: ring, signals, resampler, and the
    /// output buffers, all freshly constructed.
    struct ChunkFixture {
        ring: Arc<StdMutex<RingBuffer>>,
        signals: CaptureSignals,
        resampler: Resampler,
        mono: Vec<f32>,
        resampled: Vec<f32>,
    }

    impl ChunkFixture {
        fn new() -> Self {
            Self {
                ring: Arc::new(StdMutex::new(RingBuffer::new(1024))),
                signals: CaptureSignals::new(),
                resampler: Resampler::new(16000.0, 16000.0),
                mono: Vec::new(),
                resampled: Vec::new(),
            }
        }
    }

    #[test]
    fn signal_floor_detects_digital_silence() {
        assert!(0.0f32.abs() <= SIGNAL_FLOOR);
        assert!(5e-6f32.abs() <= SIGNAL_FLOOR);
        assert!(2e-5f32.abs() > SIGNAL_FLOOR);
    }

    #[test]
    fn choose_rate_prefers_48000_in_range() {
        let range = cpal::SupportedStreamConfigRange::new(
            2,
            SampleRate(44100),
            SampleRate(96000),
            cpal::SupportedBufferSize::Range {
                min: 256,
                max: 4096,
            },
            SampleFormat::F32,
        );
        assert_eq!(choose_rate(&range), SampleRate(48000));
    }

    #[test]
    fn choose_rate_clamps_outside_range() {
        let range = cpal::SupportedStreamConfigRange::new(
            1,
            SampleRate(8000),
            SampleRate(16000),
            cpal::SupportedBufferSize::Unknown,
            SampleFormat::I16,
        );
        assert_eq!(choose_rate(&range), SampleRate(16000));
    }

    #[test]
    fn process_chunk_stereo_f32_downmixes_to_mono() {
        let mut f = ChunkFixture::new();
        process_chunk(
            &[0.5f32, 0.3, 0.1, 0.9],
            2,
            &mut f.resampler,
            &mut f.mono,
            &mut f.resampled,
            &f.ring,
            &f.signals,
        );
        let mono = f.ring.lock().unwrap().drain();
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.4).abs() < 1e-6);
        assert!((mono[1] - 0.5).abs() < 1e-6);
        assert!(f.signals.has_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn process_chunk_i16_converts_and_detects_signal() {
        let mut f = ChunkFixture::new();
        process_chunk(
            &[i16::MAX, i16::MIN],
            1,
            &mut f.resampler,
            &mut f.mono,
            &mut f.resampled,
            &f.ring,
            &f.signals,
        );
        let mono = f.ring.lock().unwrap().drain();
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 32767.0 / 32768.0).abs() < 1e-5);
        assert!((mono[1] + 1.0).abs() < 1e-5);
        assert!(f.signals.has_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn process_chunk_silence_does_not_set_signal() {
        let mut f = ChunkFixture::new();
        process_chunk(
            &[0.0f32, 0.0, 0.0, 0.0],
            2,
            &mut f.resampler,
            &mut f.mono,
            &mut f.resampled,
            &f.ring,
            &f.signals,
        );
        assert!(!f.signals.has_signal.load(Ordering::SeqCst));
        assert_eq!(f.ring.lock().unwrap().len(), 2);
    }

    #[test]
    fn metrics_track_callback_count_without_sample_values() {
        let metrics = CaptureMetrics::default();
        metrics.observe_callback(Duration::from_micros(2));
        metrics.observe_callback(Duration::from_micros(3));
        assert_eq!(metrics.snapshot().callback_count, 2);
    }
}
