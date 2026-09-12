//! Provider-neutral media primitives.
//!
//! Model-facing media orchestration belongs to `haven-tools::builtin::media`.
//! This crate only owns modality detection, provider content parts, the media
//! projection, and the shared vision adapter.

pub mod modality;
pub mod multimodal;
pub mod projection;
pub mod vision;

pub use modality::{
    Modality, detect_media_type, detect_media_type_with_filename, detect_modality,
    extension_for_media_type,
};
pub use multimodal::{audio_part, image_part};
pub use projection::project_media_plan;
pub use vision::analyze_image;
