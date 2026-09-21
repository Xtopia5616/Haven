use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

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

impl GeminiAdapter {
    pub(super) fn cached_content_fingerprint(
        &self,
        system_instruction: &Value,
        tools: Option<&Vec<GeminiTool>>,
    ) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"haven-gemini-cached-content-v1\0");
        hasher.update(self.endpoint.model_name.as_bytes());
        hasher.update([0]);
        hasher.update(crate::types::stable_json_bytes(system_instruction));
        hasher.update([0]);
        let tools = tools
            .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
            .unwrap_or(Value::Null);
        hasher.update(crate::types::stable_json_bytes(&tools));
        let digest = hasher.finalize();
        let fingerprint = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!("haven-gemini-v1-{fingerprint}")
    }

    pub(super) fn cached_content_rejected(error: &LlmError) -> bool {
        let LlmError::RequestFailed(message) = error else {
            return false;
        };
        let message = message.to_ascii_lowercase();
        (message.contains("cachedcontent") || message.contains("cached content"))
            && [
                "not found",
                "expired",
                "invalid",
                "unsupported",
                "cannot be used",
                "can not be used",
                "does not exist",
            ]
            .iter()
            .any(|hint| message.contains(hint))
    }
}

pub(super) fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
