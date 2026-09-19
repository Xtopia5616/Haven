use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};

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
            return None;
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

        // Hash the exact provider tool projection, not the canonical
        // ToolDefinition. In particular, sanitized schemas must not select a
        // different cache shard from the wire request they produce.
        let tool_value = serde_json::to_value(Self::convert_tools(tools.to_vec())).ok()?;
        hasher.update(crate::types::stable_json_bytes(&tool_value));
        // Built-in web search changes the Responses tool surface and
        // tool-choice semantics, so it must select a distinct cache shard.
        hasher.update([match web_search_mode {
            WebSearchMode::Off => 0,
            WebSearchMode::Auto => 1,
            WebSearchMode::Always => 2,
        }]);
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
