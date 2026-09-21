use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};
use std::time::{SystemTime, UNIX_EPOCH};

impl OpenAiResponsesAdapter {
    pub(super) fn wire_capability_profile() -> CapabilityProfile {
        crate::adapters::chat_capability_profile(
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
        )
    }

    /// (the endpoint's `reasoning_echo_max_chars` override wins when set).
    pub(super) const MAX_REASONING_ECHO_CHARS: usize = 3000;

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

        // Capability profiles are part of the provider surface.  A role or
        // endpoint that projects media differently must not reuse a routing
        // shard created for another representation.
        let capability_value = serde_json::to_value(Self::wire_capability_profile()).ok()?;
        hasher.update(b"capabilities\0");
        hasher.update(crate::types::stable_json_bytes(&capability_value));

        // Hash the exact provider tool projection, not the canonical
        // ToolDefinition. In particular, sanitized schemas must not select a
        // different cache shard from the wire request they produce.
        let tool_value = serde_json::to_value(Self::convert_tools(tools)).ok()?;
        hasher.update(b"tools\0");
        hasher.update(crate::types::stable_json_bytes(&tool_value));
        // Built-in web search changes the Responses tool surface and
        // tool-choice semantics, so it must select a distinct cache shard.
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

    pub(super) fn cache_diagnostics(
        messages: &[CanonicalMessage],
        key_requested: bool,
    ) -> CacheDiagnostics {
        let system_split = messages.iter().any(|message| {
            message.role == CanonicalRole::System
                && message.content.iter().any(|part| {
                    matches!(part, ContentPart::Text(text) if split_system_prompt_cache_sections(text).is_some())
                })
        });
        CacheDiagnostics::for_request(key_requested, system_split)
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

    pub(super) fn developer_input_rejected(error: &LlmError) -> bool {
        let LlmError::RequestFailed(message) = error else {
            return false;
        };
        let message = message.to_ascii_lowercase();
        message.contains("developer")
            && [
                "invalid role",
                "unsupported role",
                "unknown role",
                "allowed roles",
                "not supported",
                "not allowed",
            ]
            .iter()
            .any(|hint| message.contains(hint))
    }

    pub(super) fn requires_reasoning_echo(&self) -> bool {
        requires_reasoning_echo(&self.endpoint)
    }
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
