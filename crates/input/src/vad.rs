use std::io::Cursor;

use anyhow::Result;
use tract_onnx::prelude::*;

const MODEL_BYTES: &[u8] = include_bytes!("../../../assets/models/silero_vad.onnx");
const FRAME_SIZE: usize = 480;
const STATE_DIM: usize = 128;
const ENERGY_THRESHOLD: f32 = 0.001;

/// Root-mean-square energy of a frame.
pub fn frame_energy(frame: &[f32]) -> f32 {
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
}

/// Whether a frame carries enough energy to be worth running the model.
/// Frames below [`ENERGY_THRESHOLD`] are treated as silence; this lets the
/// recording loop skip the model round-trip for quiet audio (the common case).
pub fn frame_has_energy(frame: &[f32]) -> bool {
    frame_energy(frame) >= ENERGY_THRESHOLD
}

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, TypedModel>;

/// Silero VAD inference engine. The bundled model is the **v5** variant,
/// whose interface is:
///   inputs:  [0] audio `[batch, seq]` f32
///            [1] sr    `[1]` i64 (16000)
///            [2] state `[2, batch, 128]` f32 (recurrent state)
///   outputs: [0] prob  `[batch, 1]` f32
///            [1] state `[2, batch, 128]` f32 (new recurrent state)
///
/// Earlier code assumed the legacy 4-input (input/h/c/sr) variant; setting
/// a 4th input fact on this 3-input model panicked tract ("index out of
/// bounds: len 3, index 3") and, because the panic crossed the non-unwinding
/// global-hotkey C callback, aborted the whole process.
pub struct VadEngine {
    model: Model,
    state: Tensor,
}

impl VadEngine {
    pub fn new() -> Result<Self> {
        let model = onnx()
            .model_for_read(&mut Cursor::new(MODEL_BYTES))?
            // The ONNX reader already supplies input facts with a symbolic
            // `batch` dim. Do NOT override them with concrete values: the v5
            // model's Pad op shape inference ("Impossible to unify Sym(batch)
            // with Val(1)") chokes when batch is pinned at graph-build time.
            // We keep batch symbolic and pass batch=1 tensors at run time.
            .into_optimized()?
            .into_runnable()?;
        let state = Tensor::zero::<f32>(&[2, 1, STATE_DIM])?;
        Ok(Self { model, state })
    }

    pub fn infer(&mut self, frame: &[f32]) -> f32 {
        if frame.len() < FRAME_SIZE {
            return 0.0;
        }
        if !frame_has_energy(frame) {
            return 0.0;
        }

        let input = Tensor::from_shape(&[1, FRAME_SIZE], &frame[..FRAME_SIZE]).unwrap();
        let sr = tensor0(16000i64);
        let state = self.state.clone();

        let result = self
            .model
            .run(tvec!(input.into(), sr.into(), state.into()))
            .unwrap();

        let prob = result[0]
            .to_array_view::<f32>()
            .unwrap()
            .iter()
            .copied()
            .next()
            .unwrap_or(0.0);

        self.state = result[1].clone().into_tensor();

        prob
    }

    pub fn reset(&mut self) {
        self.state = Tensor::zero::<f32>(&[2, 1, STATE_DIM]).unwrap();
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
    /// VAD probability at or above which a frame counts as speech. Wired from
    /// `AudioConfig.vad_threshold` via `InputPipeline::update_config`.
    pub(crate) threshold: f32,
    /// Consecutive silent frames (30 ms each) after speech that trigger an
    /// auto-stop. Derived from `AudioConfig.silence_timeout_ms`.
    pub(crate) silence_max_frames: u32,
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

    /// End-to-end: load the bundled v5 model and run a frame through it. A
    /// zero-energy frame short-circuits (returns 0.0 without touching the
    /// model), so feed a non-trivial signal to exercise the actual tract
    /// inference path and confirm the 3-input / 2-output interface is correct.
    #[test]
    fn vad_engine_loads_and_infers() {
        let mut engine = VadEngine::new().expect("model loads");
        // A mid-amplitude sine-ish frame well above the energy threshold.
        let frame: Vec<f32> = (0..FRAME_SIZE)
            .map(|i| 0.3 * (i as f32 * 0.1).sin())
            .collect();
        let prob = engine.infer(&frame);
        assert!((0.0..=1.0).contains(&prob), "prob out of range: {prob}");
        // A second inference reuses the updated state without panic.
        let prob2 = engine.infer(&frame);
        assert!((0.0..=1.0).contains(&prob2));
        engine.reset();
        let prob3 = engine.infer(&frame);
        assert!((0.0..=1.0).contains(&prob3));
    }
}
