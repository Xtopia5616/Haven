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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SampleRate, SizedSample};

use super::resample::Resampler;
use super::ring::RingBuffer;

/// Pipeline target format: mono 16 kHz f32.
pub const TARGET_SAMPLE_RATE: u32 = 16000;
/// Anything above this amplitude counts as "the data path is alive". A
/// stream whose samples never exceed this is treated as digital silence.
/// Set below a quiet room's typical noise floor (≈ -60..-90 dBFS) so the
/// silent-capture abort only fires on genuinely dead streams.
pub const SIGNAL_FLOOR: f32 = 1e-5;
/// Emit a debug-level amplitude report every N callbacks (~10 ms each).
const DIAG_INTERVAL: u32 = 250;

/// Per-stream diagnostic counter for the audio callback: logs a rolling
/// peak amplitude so silent-capture and "no speech at the head" issues can
/// be diagnosed from logs instead of by listening to saved WAVs.
struct CallbackDiag {
    callbacks: u32,
    window_peak: f32,
    poison_logged: bool,
}

impl CallbackDiag {
    fn new() -> Self {
        Self {
            callbacks: 0,
            window_peak: 0.0,
            poison_logged: false,
        }
    }

    fn observe(&mut self, mono: &[f32]) {
        self.callbacks += 1;
        for &s in mono {
            let a = s.abs();
            if a > self.window_peak {
                self.window_peak = a;
            }
        }
        if self.callbacks.is_multiple_of(DIAG_INTERVAL) {
            tracing::debug!(
                "capture callback: {} chunks, window peak {:.6}",
                self.callbacks,
                self.window_peak
            );
            self.window_peak = 0.0;
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
}

impl CaptureSignals {
    pub fn new() -> Self {
        Self {
            has_signal: Arc::new(AtomicBool::new(false)),
            stream_failed: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// CPAL capture backend, driven by the engine thread: `start` opens the
/// stream, `stop` pauses and drops it.
pub struct CpalBackend {
    ring: Arc<StdMutex<RingBuffer>>,
    signals: CaptureSignals,
    device_name: String,
    stream: Option<cpal::Stream>,
    running: bool,
}

impl CpalBackend {
    pub fn new(ring: Arc<StdMutex<RingBuffer>>, signals: CaptureSignals) -> Result<Self> {
        let device_name = cpal::default_host()
            .default_input_device()
            .and_then(|d| d.name().ok())
            .unwrap_or_else(|| "default input".into());
        Ok(Self {
            ring,
            signals,
            device_name,
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
            "cpal capture running on {} ({} Hz, {} ch, {})",
            self.device_name,
            sample_rate,
            channels,
            sample_format
        );
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }
        // Pausing then dropping releases the device; in-flight callbacks
        // finish before the stream is torn down, so the ring stays intact.
        if let Some(stream) = self.stream.take() {
            let _ = stream.pause();
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
    let mut resampler = Resampler::new(sample_rate, TARGET_SAMPLE_RATE as f64);
    let mut diag = CallbackDiag::new();
    // Scratch buffers owned by the callback closure: the real-time thread
    // must not heap-allocate per 10 ms chunk, so both the mono mixdown and
    // the resampled output are reused across callbacks.
    let mut mono = Vec::new();
    let mut resampled = Vec::new();
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
                &mut diag,
            );
        },
        move |err| {
            tracing::error!("capture stream error: {err}");
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
#[allow(clippy::too_many_arguments)]
fn process_chunk<T: Sample>(
    data: &[T],
    channels: u16,
    resampler: &mut Resampler,
    mono: &mut Vec<f32>,
    resampled: &mut Vec<f32>,
    ring: &Arc<StdMutex<RingBuffer>>,
    signals: &CaptureSignals,
    diag: &mut CallbackDiag,
) where
    f32: FromSample<T>,
{
    let ch = channels.max(1) as usize;
    let frames = data.len() / ch;
    mono.clear();
    mono.reserve(frames);
    for frame in data.chunks_exact(ch) {
        let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
        mono.push(sum / ch as f32);
    }
    diag.observe(&*mono);
    resampler.process_into(mono, resampled);
    if resampled.iter().any(|s| s.abs() > SIGNAL_FLOOR) {
        signals.has_signal.store(true, Ordering::SeqCst);
    }
    if let Ok(mut ring) = ring.lock() {
        ring.push(resampled.as_slice());
    } else if !diag.poison_logged {
        // The engine thread panicked and poisoned the ring; audio cannot be
        // delivered anyway. Log once per stream, not every 10 ms callback.
        diag.poison_logged = true;
        tracing::error!("capture ring lock poisoned; dropping audio chunks");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let ring = Arc::new(StdMutex::new(RingBuffer::new(1024)));
        let signals = CaptureSignals::new();
        let mut resampler = Resampler::new(16000.0, 16000.0);
        let mut diag = CallbackDiag::new();
        let mut mono = Vec::new();
        let mut resampled = Vec::new();
        process_chunk(
            &[0.5f32, 0.3, 0.1, 0.9],
            2,
            &mut resampler,
            &mut mono,
            &mut resampled,
            &ring,
            &signals,
            &mut diag,
        );
        let mono = ring.lock().unwrap().drain();
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.4).abs() < 1e-6);
        assert!((mono[1] - 0.5).abs() < 1e-6);
        assert!(signals.has_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn process_chunk_i16_converts_and_detects_signal() {
        let ring = Arc::new(StdMutex::new(RingBuffer::new(1024)));
        let signals = CaptureSignals::new();
        let mut resampler = Resampler::new(16000.0, 16000.0);
        let mut diag = CallbackDiag::new();
        let mut mono = Vec::new();
        let mut resampled = Vec::new();
        process_chunk(
            &[i16::MAX, i16::MIN],
            1,
            &mut resampler,
            &mut mono,
            &mut resampled,
            &ring,
            &signals,
            &mut diag,
        );
        let mono = ring.lock().unwrap().drain();
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 32767.0 / 32768.0).abs() < 1e-5);
        assert!((mono[1] + 1.0).abs() < 1e-5);
        assert!(signals.has_signal.load(Ordering::SeqCst));
    }

    #[test]
    fn process_chunk_silence_does_not_set_signal() {
        let ring = Arc::new(StdMutex::new(RingBuffer::new(1024)));
        let signals = CaptureSignals::new();
        let mut resampler = Resampler::new(16000.0, 16000.0);
        let mut diag = CallbackDiag::new();
        let mut mono = Vec::new();
        let mut resampled = Vec::new();
        process_chunk(
            &[0.0f32, 0.0, 0.0, 0.0],
            2,
            &mut resampler,
            &mut mono,
            &mut resampled,
            &ring,
            &signals,
            &mut diag,
        );
        assert!(!signals.has_signal.load(Ordering::SeqCst));
        assert_eq!(ring.lock().unwrap().len(), 2);
    }

    #[test]
    fn diag_tracks_rolling_peak() {
        let mut diag = CallbackDiag::new();
        diag.observe(&[0.0, 0.3]);
        diag.observe(&[-0.9, 0.1]);
        assert_eq!(diag.callbacks, 2);
        assert!((diag.window_peak - 0.9).abs() < 1e-6);
    }
}
