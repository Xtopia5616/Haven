use std::collections::VecDeque;

/// Linear-interpolation sample-rate converter, always owned by exactly one
/// capture backend and never shared across streams (each backend constructs
/// its own instance from its negotiated source rate).
pub struct Resampler {
    leftover: VecDeque<f32>,
    position: f64,
    ratio: f64,
}

impl Resampler {
    pub fn new(src_sr: f64, target_sr: f64) -> Self {
        Self {
            leftover: VecDeque::new(),
            position: 0.0,
            ratio: src_sr / target_sr,
        }
    }

    /// Convert `mono` (source rate) into the target rate. State is carried
    /// across calls so chunk boundaries never lose samples.
    pub fn process(&mut self, mono: &[f32]) -> Vec<f32> {
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

    pub fn reset(&mut self) {
        self.leftover.clear();
        self.position = 0.0;
    }
}

/// Average interleaved multi-channel samples into a mono stream.
pub fn downmix(data: &[f32], channels: usize) -> Vec<f32> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        let mut rs = Resampler::new(16000.0, 16000.0);
        let input = vec![0.0, 0.5, 1.0, 0.5, 0.0];
        let out = rs.process(&input);
        assert_eq!(out.len(), input.len());
        for (a, b) in out.iter().zip(input.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn downsample_length() {
        let mut rs = Resampler::new(44100.0, 16000.0);
        let input: Vec<f32> = (0..44100)
            .map(|i| (i as f32 / 44100.0 * std::f32::consts::PI * 2.0).sin())
            .collect();
        let out = rs.process(&input);
        assert!(out.len() < input.len());
        assert!((out.len() as i32 - 16000).abs() <= 1);
    }

    #[test]
    fn chunked_matches_whole() {
        let mut whole = Resampler::new(48000.0, 16000.0);
        let input: Vec<f32> = (0..4800)
            .map(|i| ((i as f32 / 4800.0) * 3.0 * std::f32::consts::PI).sin())
            .collect();
        let out_whole = whole.process(&input);

        let mut chunked = Resampler::new(48000.0, 16000.0);
        let mut out_chunked = Vec::new();
        for chunk in input.chunks(480) {
            out_chunked.extend(chunked.process(chunk));
        }
        assert_eq!(out_whole.len(), out_chunked.len());
        for (a, b) in out_whole.iter().zip(out_chunked.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
    }

    #[test]
    fn downmix_stereo_to_mono() {
        let stereo = vec![0.5, 0.3, 0.1, 0.9];
        let mono = downmix(&stereo, 2);
        assert_eq!(mono.len(), 2);
        assert!((mono[0] - 0.4).abs() < 1e-6);
        assert!((mono[1] - 0.5).abs() < 1e-6);
    }
}
