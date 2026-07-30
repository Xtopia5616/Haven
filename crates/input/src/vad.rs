use std::io::Cursor;

use anyhow::Result;
use tract_onnx::prelude::*;

const MODEL_BYTES: &[u8] = include_bytes!("../../../assets/models/silero_vad.onnx");
const FRAME_SIZE: usize = 480;
const STATE_DIM: usize = 128;
const ENERGY_THRESHOLD: f32 = 0.001;

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>;

pub struct VadEngine {
    model: Model,
    h: Tensor,
    c: Tensor,
}

impl VadEngine {
    pub fn new() -> Result<Self> {
        let model = onnx()
            .model_for_read(&mut Cursor::new(MODEL_BYTES))?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(1, FRAME_SIZE as i64)),
            )?
            .with_input_fact(1, InferenceFact::dt_shape(i64::datum_type(), tvec!(1)))?
            .with_input_fact(
                2,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(2, 1, STATE_DIM as i64)),
            )?
            .with_input_fact(
                3,
                InferenceFact::dt_shape(f32::datum_type(), tvec!(2, 1, STATE_DIM as i64)),
            )?
            .into_optimized()?
            .into_runnable()?;
        let h = Tensor::zero::<f32>(&[2, 1, STATE_DIM])?;
        let c = Tensor::zero::<f32>(&[2, 1, STATE_DIM])?;
        Ok(Self { model, h, c })
    }

    pub fn infer(&mut self, frame: &[f32]) -> f32 {
        if frame.len() < FRAME_SIZE {
            return 0.0;
        }
        let energy: f32 = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        if energy.sqrt() < ENERGY_THRESHOLD {
            return 0.0;
        }

        let input = Tensor::from_shape(&[1, FRAME_SIZE], &frame[..FRAME_SIZE]).unwrap();
        let sr = tensor1(&[16000i64]);
        let h = self.h.clone();
        let c = self.c.clone();

        let result = self
            .model
            .run(tvec!(input.into(), sr.into(), h.into(), c.into()))
            .unwrap();

        let prob = result[0].to_array_view::<f32>().unwrap()[[]];

        self.h = result[1].clone().into_tensor();
        self.c = result[2].clone().into_tensor();

        prob
    }

    pub fn reset(&mut self) {
        self.h = Tensor::zero::<f32>(&[2, 1, STATE_DIM]).unwrap();
        self.c = Tensor::zero::<f32>(&[2, 1, STATE_DIM]).unwrap();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadState {
    Silent,
    Speech,
    SilenceAfterSpeech { silent_frames: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadSignal {
    None,
    SpeechStart,
    SpeechEnd,
    AutoStop,
}

pub struct VadDetector {
    state: VadState,
    threshold: f32,
    silence_max_frames: u32,
}

impl VadDetector {
    pub fn new(threshold: f32, silence_timeout_ms: u64) -> Self {
        let silence_max_frames = (silence_timeout_ms / 30) as u32;
        Self {
            state: VadState::Silent,
            threshold,
            silence_max_frames,
        }
    }

    pub fn process(&mut self, prob: f32) -> VadSignal {
        match self.state {
            VadState::Silent => {
                if prob >= self.threshold {
                    self.state = VadState::Speech;
                    VadSignal::SpeechStart
                } else {
                    VadSignal::None
                }
            }
            VadState::Speech => {
                if prob < self.threshold {
                    self.state = VadState::SilenceAfterSpeech { silent_frames: 1 };
                    VadSignal::None
                } else {
                    VadSignal::None
                }
            }
            VadState::SilenceAfterSpeech { silent_frames } => {
                if prob >= self.threshold {
                    self.state = VadState::Speech;
                    VadSignal::SpeechStart
                } else if silent_frames >= self.silence_max_frames {
                    self.state = VadState::Silent;
                    VadSignal::AutoStop
                } else {
                    self.state = VadState::SilenceAfterSpeech {
                        silent_frames: silent_frames + 1,
                    };
                    VadSignal::None
                }
            }
        }
    }

    pub fn reset(&mut self) {
        self.state = VadState::Silent;
    }

    pub fn state(&self) -> VadState {
        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_detector_silent_to_speech() {
        let mut det = VadDetector::new(0.5, 1500);
        assert_eq!(det.state, VadState::Silent);
        assert_eq!(det.process(0.8), VadSignal::SpeechStart);
        assert_eq!(det.state, VadState::Speech);
    }

    #[test]
    fn vad_detector_speech_to_silence() {
        let mut det = VadDetector::new(0.5, 1500);
        det.process(0.8);
        assert_eq!(det.process(0.3), VadSignal::None);
        assert_eq!(det.state, VadState::SilenceAfterSpeech { silent_frames: 1 });
    }

    #[test]
    fn vad_detector_autostop() {
        let mut det = VadDetector::new(0.5, 90);
        det.process(0.8);
        assert_eq!(det.process(0.3), VadSignal::None);
        assert_eq!(det.process(0.2), VadSignal::None);
        assert_eq!(det.process(0.1), VadSignal::None);
        assert_eq!(det.process(0.05), VadSignal::AutoStop);
        assert_eq!(det.state, VadState::Silent);
    }

    #[test]
    fn vad_detector_reentry() {
        let mut det = VadDetector::new(0.5, 1500);
        det.process(0.8);
        det.process(0.3);
        assert_eq!(det.process(0.9), VadSignal::SpeechStart);
        assert_eq!(det.state, VadState::Speech);
    }

    #[test]
    fn vad_detector_reset() {
        let mut det = VadDetector::new(0.5, 1500);
        det.process(0.8);
        det.reset();
        assert_eq!(det.state, VadState::Silent);
    }

    #[test]
    fn vad_detector_low_prob_stays_silent() {
        let mut det = VadDetector::new(0.5, 1500);
        assert_eq!(det.process(0.1), VadSignal::None);
        assert_eq!(det.state, VadState::Silent);
    }

    #[test]
    fn energy_threshold_bypasses_engine() {
        let frame = vec![0.0f32; 480];
        let energy: f32 = frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32;
        assert!(energy.sqrt() < ENERGY_THRESHOLD);
    }
}
