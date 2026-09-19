use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};

impl GeminiAdapter {
    pub(super) fn wire_capability_profile() -> CapabilityProfile {
        let mut profile = crate::adapters::chat_capability_profile(
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
        );
        // Gemini's generateContent wire accepts inline video data. The
        // planner therefore may select RawVideo without a still/keyframe
        // fallback or provider-name inference.
        profile.video = CapabilitySupport::Supported;
        profile
    }
}

impl GeminiAdapter {}
