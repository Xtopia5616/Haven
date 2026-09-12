//! Provider-neutral media primitives.
//!
//! Model-facing media orchestration belongs to `haven-tools::builtin::media`.
//! Common owns media type detection; this crate only owns provider content
//! parts, the media projection, and the shared vision adapter.

pub mod multimodal;
pub mod projection;
pub mod vision;

pub use multimodal::{audio_part, image_part, video_part};
pub use projection::project_media_plan;
pub use vision::analyze_image;
