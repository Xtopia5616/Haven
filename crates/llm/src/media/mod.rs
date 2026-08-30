//! Multimodal media gateway (merged from `haven-gateway`, formerly owned by
//! the input crate).
//!
//! Owns the routing pipeline over the [`crate::LlmRouter`] and the dedicated
//! media clients (STT / OCR / TTS / image generation):
//!
//! - [gateway::MediaGateway::process_attachment] — for a binary attachment:
//!   detect modality, classify intent, run the coverage action. Extraction
//!   actions run through the dedicated provider with a confidence gate; a
//!   result below `min_confidence` (or an error / empty result) falls back to
//!   the main model, which is called directly with the media as a content part.
//! - [gateway::MediaGateway::process_generate] — pure-text generate requests
//!   (TTS / text-to-image), saving the generated file under the app data
//!   media directory.
//!
//! Everything is in-process: there is no separate HTTP service, the agent
//! calls these methods while building the user message.

pub mod coverage;
pub mod gateway;
pub mod intent;
pub mod modality;
pub mod multimodal;

pub use coverage::{CoverageAction, MediaDecision, coverage_for, coverage_for_generate};
pub use gateway::{AttachmentOutcome, GenerateOutcome, MediaGateway};
pub use intent::{GenerateKind, Intent, detect_generate_kind, detect_intent};
pub use modality::{Modality, detect_media_type, detect_modality, extension_for_media_type};
pub use multimodal::{audio_part_from_bytes, image_part, image_part_from_bytes};
