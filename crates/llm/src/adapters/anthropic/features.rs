use super::*;
use haven_common::{CapabilityProfile, CapabilitySupport};

impl AnthropicAdapter {
    pub(super) fn wire_capability_profile() -> CapabilityProfile {
        crate::adapters::chat_capability_profile(
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
        )
    }

    pub(super) fn validate_provider_content(messages: &[CanonicalMessage]) -> Result<(), LlmError> {
        if messages
            .iter()
            .flat_map(|message| &message.content)
            .any(|part| matches!(part, ContentPart::Audio { .. }))
        {
            return Err(LlmError::UnsupportedCapability(
                "Anthropic Messages API does not support audio input; configure an audio-capable model or STT provider".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn thinking_config(
        max_tokens: u32,
        model_name: &str,
        effort: Option<&str>,
    ) -> (Option<Value>, Option<Value>) {
        let Some(effort) = effort.map(str::trim).filter(|value| !value.is_empty()) else {
            return (None, None);
        };
        let effort = effort.to_ascii_lowercase();
        let model = model_name.to_ascii_lowercase();
        let adaptive = [
            "4-6",
            "4.6",
            "4-7",
            "4.7",
            "4-8",
            "4.8",
            "opus-5",
            "sonnet-5",
            "fable-5",
            "mythos-5",
            "mythos-preview",
        ]
        .iter()
        .any(|part| model.contains(part));
        let manual = ["3-7", "3.7", "4-5", "4.5"]
            .iter()
            .any(|part| model.contains(part));

        if matches!(effort.as_str(), "none" | "off" | "disabled") {
            return if adaptive || manual {
                (Some(json!({"type": "disabled"})), None)
            } else {
                (None, None)
            };
        }
        if adaptive {
            return (
                Some(json!({"type": "adaptive"})),
                Some(json!({"effort": effort})),
            );
        }
        if manual && max_tokens > 1024 {
            let ratio = match effort.as_str() {
                "low" => 0.25,
                "medium" => 0.40,
                "max" => 0.80,
                _ => 0.60,
            };
            let budget = ((max_tokens as f32 * ratio).round() as u32)
                .max(1024)
                .min(max_tokens - 1);
            return (
                Some(json!({"type": "enabled", "budget_tokens": budget})),
                None,
            );
        }
        (None, None)
    }

    pub(super) fn apply_tools_cache_breakpoint(tools: &mut [Value]) {
        let stable_index = tools
            .iter()
            .rposition(|tool| tool.get("type").is_none())
            .or_else(|| tools.len().checked_sub(1));
        if let Some(last) = stable_index.and_then(|index| tools.get_mut(index))
            && let Some(obj) = last.as_object_mut()
        {
            obj.insert("cache_control".into(), json!({"type": "ephemeral"}));
        }
    }

    pub(super) fn apply_messages_cache_breakpoint(messages: &mut [AnthropicMessage]) {
        if messages.len() < 2 {
            return;
        }

        let Some(latest) = messages.last() else {
            return;
        };
        let is_latest_user_turn = latest.role == "user";
        if !is_latest_user_turn {
            return;
        }
        // A tool result is the newest mutable input. The preceding assistant
        // tool-use message remains part of the reusable conversation prefix.
        let Some(prefix_last) = messages.get_mut(messages.len() - 2) else {
            return;
        };
        let Some(blocks) = prefix_last.content.as_array_mut() else {
            return;
        };
        let Some(last) = blocks.last_mut() else {
            return;
        };
        if let Some(obj) = last.as_object_mut() {
            obj.insert("cache_control".into(), json!({"type": "ephemeral"}));
        }
    }

    pub(super) fn system_with_cache_control(system: Option<String>) -> Option<Value> {
        let text = system.filter(|s| !s.is_empty())?;
        if let Some((stable, session, memory)) = split_system_prompt_cache_sections(&text) {
            let mut blocks = Vec::with_capacity(3);
            if !stable.is_empty() {
                blocks.push(json!({
                    "type": "text",
                    "text": stable,
                    "cache_control": {"type": "ephemeral"}
                }));
            }
            if !session.is_empty() {
                blocks.push(json!({
                    "type": "text",
                    "text": session
                }));
            }
            if !memory.is_empty() {
                blocks.push(json!({
                    "type": "text",
                    "text": memory
                }));
            }
            if !blocks.is_empty() {
                return Some(Value::Array(blocks));
            }
        }
        Some(json!([{
            "type": "text",
            "text": text,
            "cache_control": {"type": "ephemeral"}
        }]))
    }

    pub(super) fn cache_diagnostics(messages: &[CanonicalMessage]) -> CacheDiagnostics {
        let system_split = messages.iter().any(|message| {
            message.role == CanonicalRole::System
                && message.content.iter().any(|part| {
                    matches!(part, ContentPart::Text(text) if split_system_prompt_cache_sections(text).is_some())
                })
        });
        CacheDiagnostics::for_provider_cache(system_split)
    }
}
