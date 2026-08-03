//! WASAPI exclusive-mode capture backend.
//!
//! Exclusive mode talks directly to the device, bypassing the Windows audio
//! effects chain (enhancement APOs) whose bugs intermittently deliver digital
//! silence — e.g. the Elevoc/ELEVO AI noise-suppression APO found on many
//! laptops (including the Intel SST array on this project's dev machine).
//!
//! The backend is created and driven exclusively by the engine thread
//! (never moved across threads), so it does not need to be `Send`. The
//! engine calls [`ExclusiveBackend::pull`] periodically to feed the ring.
//!
//! **Why not shared mode?** A shared-mode (cpal) stream that has run once in
//! a process poisons exclusive-mode capture for the rest of that process:
//! subsequent exclusive streams initialize successfully but deliver digital
//! silence (verified on the Intel SST array). Shared mode is therefore not
//! used at all for now; it can be reintroduced later as a separate backend
//! with a per-process "shared was used → keep using shared" guard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Result, anyhow};

use super::resample::{Resampler, downmix};
use super::ring::RingBuffer;

/// Pipeline target format: mono 16 kHz f32.
pub const TARGET_SAMPLE_RATE: u32 = 16000;
/// Anything above this amplitude counts as "the data path is alive". A
/// stream whose samples never exceed this is treated as digital silence.
/// Set below a quiet room's typical noise floor (≈ -60..-90 dBFS) so the
/// silent-capture abort only fires on genuinely dead streams.
pub const SIGNAL_FLOOR: f32 = 1e-5;

/// Shared flags between the backend's data path and the engine.
#[derive(Clone)]
pub struct CaptureSignals {
    /// Set by the data path when any delivered sample exceeds
    /// [`SIGNAL_FLOOR`]. Reset by the engine before each start.
    pub has_signal: Arc<AtomicBool>,
}

impl CaptureSignals {
    pub fn new() -> Self {
        Self {
            has_signal: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// WASAPI exclusive-mode capture, driven by the engine thread via `pull`.
pub struct ExclusiveBackend {
    ring: Arc<StdMutex<RingBuffer>>,
    signals: CaptureSignals,
    device_name: String,
    client: Option<wasapi::AudioClient>,
    capture: Option<wasapi::AudioCaptureClient>,
    fmt: Option<wasapi::WaveFormat>,
    buffer: Vec<u8>,
    resampler: Option<Resampler>,
    running: bool,
}

impl ExclusiveBackend {
    pub fn new(
        ring: Arc<StdMutex<RingBuffer>>,
        signals: CaptureSignals,
    ) -> Result<Self> {
        let device_name = default_capture_device_name()?;
        Ok(Self {
            ring,
            signals,
            device_name,
            client: None,
            capture: None,
            fmt: None,
            buffer: Vec::new(),
            resampler: None,
            running: false,
        })
    }

    /// Try to initialize an exclusive stream with each candidate format
    /// until one is accepted by the device.
    fn negotiate(
        device: &wasapi::Device,
    ) -> Result<(wasapi::AudioClient, wasapi::AudioCaptureClient, wasapi::WaveFormat)> {
        let (_default_period, min_period) =
            device.get_iaudioclient()?.get_device_period()?;

        let mut candidates: Vec<wasapi::WaveFormat> = Vec::new();
        if let Ok(fmt) = device.get_device_format() {
            candidates.push(fmt);
        }
        if let Ok(fmt) = device.get_iaudioclient()?.get_mixformat() {
            candidates.push(fmt);
        }
        for (bits, sample_type, rate, ch) in [
            (32usize, wasapi::SampleType::Float, 48000usize, 2usize),
            (32usize, wasapi::SampleType::Int, 48000, 2),
            (16usize, wasapi::SampleType::Int, 48000, 2),
            (16usize, wasapi::SampleType::Int, 44100, 2),
            (16usize, wasapi::SampleType::Int, 16000, 1),
        ] {
            candidates.push(wasapi::WaveFormat::new(
                bits,
                bits,
                &sample_type,
                rate,
                ch,
                None,
            ));
        }

        // De-duplicate by (rate, channels, bits).
        let mut seen = std::collections::HashSet::new();
        let mut last_err: Option<String> = None;
        for fmt in candidates {
            let key = (
                fmt.get_samplespersec(),
                fmt.get_nchannels(),
                fmt.get_bitspersample(),
            );
            if !seen.insert(key) {
                continue;
            }
            let mut client = match device.get_iaudioclient() {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("get_iaudioclient failed: {e}");
                    continue;
                }
            };
            // Request a large exclusive buffer (100 ms): in exclusive mode
            // the device overwrites data that the client does not consume in
            // time, so a buffer smaller than the engine's poll interval
            // (~20-40 ms) would silently drop audio. A large buffer turns
            // poll jitter into harmless latency instead of loss.
            const EXCLUSIVE_BUFFER_HNS: i64 = 1_000_000; // 100 ms
            let mode = wasapi::StreamMode::PollingExclusive {
                buffer_duration_hns: EXCLUSIVE_BUFFER_HNS,
                period_hns: min_period,
            };
            match client.initialize_client(&fmt, &wasapi::Direction::Capture, &mode) {
                Ok(()) => {
                    let capture = client.get_audiocaptureclient()?;
                    tracing::info!(
                        "exclusive capture initialized: {} Hz, {} ch, {} bits ({:?})",
                        fmt.get_samplespersec(),
                        fmt.get_nchannels(),
                        fmt.get_bitspersample(),
                        fmt.get_subformat()
                    );
                    return Ok((client, capture, fmt));
                }
                Err(e) => {
                    last_err = Some(format!("{fmt:?}: {e}"));
                    tracing::debug!("exclusive init failed for candidate: {e}");
                }
            }
        }
        Err(anyhow!(
            "no device-supported exclusive format (last error: {})",
            last_err.unwrap_or_else(|| "no candidates".into())
        ))
    }

    pub fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        let _ = wasapi::initialize_mta();
        let enumerator = wasapi::DeviceEnumerator::new()?;
        let device = enumerator.get_default_device(&wasapi::Direction::Capture)?;
        let (client, capture, fmt) = Self::negotiate(&device)?;
        let buffer_frames = client.get_buffer_size()?;
        client.start_stream()?;

        let blockalign = fmt.get_blockalign() as usize;
        self.resampler = Some(Resampler::new(
            fmt.get_samplespersec() as f64,
            TARGET_SAMPLE_RATE as f64,
        ));
        // Read buffer large enough to cover the engine's poll interval
        // (~20 ms) plus slack: a too-small buffer would drop audio between
        // polls in exclusive mode (data arrives in continuous bursts).
        let min_frames = fmt.get_samplespersec() / 10; // 100 ms
        self.buffer = vec![
            0u8;
            buffer_frames.max(min_frames) as usize * blockalign
        ];
        self.client = Some(client);
        self.capture = Some(capture);
        self.fmt = Some(fmt.clone());
        self.running = true;
        tracing::info!(
            "exclusive capture running on {} ({} Hz, {} ch, {} bits)",
            self.device_name,
            fmt.get_samplespersec(),
            fmt.get_nchannels(),
            fmt.get_bitspersample()
        );
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }
        if let Some(client) = self.client.as_ref() {
            let _ = client.stop_stream();
        }
        self.client = None;
        self.capture = None;
        self.fmt = None;
        self.buffer.clear();
        self.resampler = None;
        self.running = false;
        tracing::debug!("exclusive capture stopped");
        Ok(())
    }

    /// Feed whatever audio the device has delivered since the last poll into
    /// the ring.
    pub fn pull(&mut self) -> Result<()> {
        if !self.running {
            return Ok(());
        }
        let capture = self.capture.as_ref().expect("capture client present");
        let fmt = self.fmt.as_ref().expect("format present");
        let channels = fmt.get_nchannels() as usize;
        // Exclusive-mode GetBuffer is non-blocking and returns a short
        // device-period packet per call (0 when nothing is available yet).
        // Drain all packets accumulated since the last poll; the cap bounds
        // a stuck device (ring capacity absorbs any residual backlog).
        let mut reads = 0;
        let mut silent_packets = 0u32;
        let mut data_packets = 0u32;
        let mut max_abs_i32: i64 = 0;
        let mut max_abs_f32: f32 = 0.0;
        loop {
            let (frames, info) = capture.read_from_device(&mut self.buffer)?;
            if frames == 0 {
                break;
            }
            if info.flags.silent {
                silent_packets += 1;
            } else {
                data_packets += 1;
            }
            let n = frames as usize * channels;
            let f32_data: Vec<f32> = match fmt.get_subformat()? {
                wasapi::SampleType::Float => {
                    let samples: &[f32] = unsafe {
                        std::slice::from_raw_parts(self.buffer.as_ptr() as *const f32, n)
                    };
                    samples.to_vec()
                }
                wasapi::SampleType::Int if fmt.get_bitspersample() > 16 => {
                    let samples: &[i32] = unsafe {
                        std::slice::from_raw_parts(self.buffer.as_ptr() as *const i32, n)
                    };
                    for &s in samples {
                        let a = (s as i64).abs();
                        if a > max_abs_i32 {
                            max_abs_i32 = a;
                        }
                    }
                    samples.iter().map(|s| *s as f32 / 2147483648.0).collect()
                }
                wasapi::SampleType::Int => {
                    let samples: &[i16] = unsafe {
                        std::slice::from_raw_parts(self.buffer.as_ptr() as *const i16, n)
                    };
                    samples.iter().map(|s| *s as f32 / 32768.0).collect()
                }
            };
            let mono = downmix(&f32_data, channels);
            for &s in &mono {
                let a = s.abs();
                if a > max_abs_f32 {
                    max_abs_f32 = a;
                }
            }
            let processed = self
                .resampler
                .as_mut()
                .expect("resampler present")
                .process(&mono);
            if processed.iter().any(|s| s.abs() > SIGNAL_FLOOR) {
                self.signals.has_signal.store(true, Ordering::SeqCst);
            }
            self.ring.lock().expect("ring lock poisoned").push(&processed);
            reads += 1;
            if reads >= 64 {
                break;
            }
        }
        if reads > 0 {
            tracing::debug!(
                "pull: reads={} data={} silent={} max_abs_i32={} max_abs_f32={:.6}",
                reads, data_packets, silent_packets, max_abs_i32, max_abs_f32
            );
        }
        Ok(())
    }
}

fn default_capture_device_name() -> Result<String> {
    let enumerator = wasapi::DeviceEnumerator::new()?;
    let device = enumerator.get_default_device(&wasapi::Direction::Capture)?;
    Ok(device.get_friendlyname().unwrap_or_else(|_| "default input".into()))
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
}
