use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};

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
        tools: &[LlmToolDefinition],
        web_search_mode: WebSearchMode,
    ) -> Option<String> {
        // OpenAI documents this field for its Chat Completions API. Other
        // OpenAI-compatible gateways must opt in through their own documented
        // adapter behavior instead of receiving this extension by default.
        if self.style != "openai-chat" || !self.endpoint.provider.eq_ignore_ascii_case("openai") {
            return None;
        }

        if !self.prompt_cache_key_support.should_attach() {
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

        // Keep capability changes (especially media representation support)
        // from sharing a routing shard with an incompatible wire surface.
        let capability_value = serde_json::to_value(Self::wire_capability_profile()).ok()?;
        hasher.update(b"capabilities\0");
        hasher.update(haven_common::json::canonical_json_bytes(&capability_value));

        // Tool schemas are part of the provider cache key. Changing a loaded
        // MCP/Skill therefore gets a new routing key rather than contaminating
        // the old cache shard.
        // Hash the exact provider tool projection, not the canonical
        // LlmToolDefinition. This keeps the routing key aligned with the wire
        // schema after recursive JSON canonicalization.
        let tool_names = self.tool_name_map(messages, tools);
        let tool_value =
            serde_json::to_value(Self::convert_tools_ref_with_names(tools, &tool_names)).ok()?;
        hasher.update(b"tools\0");
        hasher.update(haven_common::json::canonical_json_bytes(&tool_value));

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

    /// xAI Chat Completions uses a conversation-affinity header rather than
    /// OpenAI's `prompt_cache_key` body field. Derive it from the stable
    /// system prefix and the first user message so it survives appended turns,
    /// tool-surface changes, and memory refreshes without exposing prompt text.
    pub(super) fn xai_conversation_id(&self, messages: &[CanonicalMessage]) -> Option<String> {
        if self.style != "xai" || !self.endpoint.provider.eq_ignore_ascii_case("xai") {
            return None;
        }

        let system = messages
            .iter()
            .find(|message| message.role == CanonicalRole::System)?;
        let first_user = messages
            .iter()
            .find(|message| message.role == CanonicalRole::User)?;
        let mut hasher = Sha256::new();
        hasher.update(b"haven-xai-conversation-v1\0");
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

        if let Some(message_id) = &first_user.id {
            hasher.update(b"initial-user-id\0");
            hasher.update(message_id.as_bytes());
            hasher.update([0]);
        }
        hasher.update(b"initial-user-text\0");
        for part in &first_user.content {
            if let ContentPart::Text(text) = part {
                hasher.update(text.as_bytes());
                hasher.update([0]);
            }
        }
        if let Some(media) =
            crate::adapters::prompt_cache_media_marker(std::slice::from_ref(first_user))
        {
            hasher.update(b"initial-user-media\0");
            hasher.update(media);
        }

        let digest = hasher.finalize();
        let fingerprint = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Some(format!("haven-conv-v1-{fingerprint}"))
    }
}
pub(crate) fn is_whisper_model(model: &str) -> bool {
    let n = model.to_ascii_lowercase();
    n.contains("whisper")
        || n.contains("transcribe")
        || n.contains("sensevoice")
        || n.contains("asr")
}
