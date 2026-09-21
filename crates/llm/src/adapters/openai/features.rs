use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};
use std::time::{SystemTime, UNIX_EPOCH};

impl OpenAiAdapter {
    pub(super) fn wire_capability_profile() -> CapabilityProfile {
        crate::adapters::chat_capability_profile(
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
        )
    }

    /// `reasoning_echo_max_chars` override wins when set).
    pub(super) const MAX_REASONING_ECHO_CHARS: usize = 3000;

    pub(super) fn requires_reasoning_echo(&self) -> bool {
        requires_reasoning_echo(&self.endpoint)
    }

    pub(super) fn prompt_cache_key(
        &self,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        web_search_mode: WebSearchMode,
    ) -> Option<String> {
        if self.prompt_cache_key_state.load(Ordering::Relaxed) == PROMPT_CACHE_KEY_UNSUPPORTED {
            let retry_at = self.prompt_cache_key_retry_at.load(Ordering::Relaxed);
            if retry_at == 0 || current_epoch_seconds() < retry_at {
                return None;
            }
        }

        let system = messages
            .iter()
            .find(|message| message.role == CanonicalRole::System)?;
        let mut hasher = Sha256::new();
        hasher.update(b"haven-prompt-cache-v1\0");
        hasher.update(self.endpoint.model_name.as_bytes());
        hasher.update([0]);

        let mut has_stable_system = false;
        for part in &system.content {
            if let ContentPart::Text(text) = part {
                let stable = split_system_prompt_cache_sections(text)
                    .map(|(stable, _, _)| stable)
                    .unwrap_or(text);
                if !stable.trim().is_empty() {
                    hasher.update(stable.as_bytes());
                    hasher.update([0]);
                    has_stable_system = true;
                }
            }
        }
        if !has_stable_system {
            return None;
        }

        // Keep capability changes (especially media representation support)
        // from sharing a routing shard with an incompatible wire surface.
        let capability_value = serde_json::to_value(Self::wire_capability_profile()).ok()?;
        hasher.update(b"capabilities\0");
        hasher.update(crate::types::stable_json_bytes(&capability_value));

        // Tool schemas are part of the provider cache key. Changing a loaded
        // MCP/Skill therefore gets a new routing key rather than contaminating
        // the old cache shard.
        // Hash the exact provider tool projection, not the canonical
        // ToolDefinition. This keeps the routing key aligned with the wire
        // schema after recursive JSON canonicalization.
        let tool_value = serde_json::to_value(Self::convert_tools_ref(tools)).ok()?;
        hasher.update(b"tools\0");
        hasher.update(crate::types::stable_json_bytes(&tool_value));

        // xAI's search_parameters and other OpenAI-compatible gateways may
        // vary their runtime capability surface by this mode.  Keep the
        // identity aligned with the request builder even when a gateway does
        // not expose a server-side search tool.
        hasher.update(b"web-search\0");
        hasher.update([match web_search_mode {
            WebSearchMode::Off => 0,
            WebSearchMode::Auto => 1,
            WebSearchMode::Always => 2,
        }]);

        if let Some(marker) = crate::adapters::prompt_cache_media_marker(messages) {
            hasher.update(b"media\0");
            hasher.update(marker);
        }

        if let Some(marker) = crate::adapters::prompt_cache_compaction_marker(messages) {
            hasher.update(b"compaction\0");
            hasher.update(marker);
        }

        let digest = hasher.finalize();
        let fingerprint = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Some(format!("haven-v1-{fingerprint}"))
    }

    pub(super) fn prompt_cache_key_rejected(error: &LlmError) -> bool {
        let LlmError::RequestFailed(message) = error else {
            return false;
        };
        let message = message.to_ascii_lowercase();
        message.contains("prompt_cache_key")
            && [
                "unknown",
                "unsupported",
                "unrecognized",
                "extra field",
                "extra fields",
                "additional propert",
                "not allowed",
                "unexpected",
                "invalid parameter",
            ]
            .iter()
            .any(|hint| message.contains(hint))
    }

    pub(super) fn remember_prompt_cache_key_rejection(&self) {
        self.prompt_cache_key_state
            .store(PROMPT_CACHE_KEY_UNSUPPORTED, Ordering::Relaxed);
        self.prompt_cache_key_retry_at.store(
            current_epoch_seconds().saturating_add(PROMPT_CACHE_KEY_REPROBE_SECS),
            Ordering::Relaxed,
        );
    }

    pub(super) fn remember_prompt_cache_key_success(&self) {
        self.prompt_cache_key_retry_at.store(0, Ordering::Relaxed);
        self.prompt_cache_key_state
            .store(PROMPT_CACHE_KEY_ENABLED, Ordering::Relaxed);
    }
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub(crate) fn is_whisper_model(model: &str) -> bool {
    let n = model.to_ascii_lowercase();
    n.contains("whisper")
        || n.contains("transcribe")
        || n.contains("sensevoice")
        || n.contains("asr")
}
