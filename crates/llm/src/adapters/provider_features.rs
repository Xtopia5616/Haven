//! Provider-specific wire feature decisions shared by adapters.
//!
//! These helpers translate Haven's endpoint metadata and reasoning settings
//! into provider-neutral decisions. They do not build requests or own
//! provider response parsing.

use haven_common::config::ModelEndpoint;
use serde_json::Value;

/// Lowercased `provider` + `base_url` + `model_name` haystack used to detect
/// vendor-specific extras (DeepSeek thinking, Kimi `thinking.type`, etc.) even
/// when the endpoint is behind a gateway that sets `provider: "openai"`.
pub(crate) fn vendor_haystack(endpoint: &ModelEndpoint) -> String {
    [&endpoint.provider, &endpoint.base_url, &endpoint.model_name]
        .map(String::as_str)
        .join(" ")
        .to_ascii_lowercase()
}

pub(crate) fn is_deepseek(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("deepseek")
}

pub(crate) fn is_kimi_or_moonshot(endpoint: &ModelEndpoint) -> bool {
    let hay = vendor_haystack(endpoint);
    hay.contains("kimi") || hay.contains("moonshot")
}

pub(crate) fn is_openrouter(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("openrouter")
}

/// True when the configured effort means "turn thinking off"
/// (`none` / `off` / `disabled`). Used by DeepSeek chat (`thinking.type`) and
/// Responses (`reasoning.effort: "none"`), and by Kimi `thinking.type`.
pub(crate) fn is_thinking_disabled(effort: &str) -> bool {
    matches!(
        effort.trim().to_ascii_lowercase().as_str(),
        "none" | "off" | "disabled"
    )
}

/// Map Haven UI `reasoning_effort` (`low`/`medium`/`high`) onto DeepSeek's
/// accepted effort values. DeepSeek docs: medium/high/xhigh → high; max → max;
/// none disables thinking (Responses) / pairs with `thinking.type=disabled`.
pub(crate) fn map_deepseek_effort(effort: &str) -> &'static str {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "medium" | "high" | "xhigh" => "high",
        "max" => "max",
        "none" | "off" | "disabled" => "none",
        _ => "high",
    }
}

/// Vendor chat-completions extras derived from `reasoning_effort` + vendor
/// detection. Returns `(thinking_object, reasoning_effort_to_send)`.
pub(crate) fn chat_thinking_extras(endpoint: &ModelEndpoint) -> (Option<Value>, Option<String>) {
    let effort = endpoint
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if is_deepseek(endpoint) {
        return match effort {
            None => (None, None),
            Some(e) if is_thinking_disabled(e) => {
                (Some(serde_json::json!({"type": "disabled"})), None)
            }
            Some(e) => (
                Some(serde_json::json!({"type": "enabled"})),
                Some(map_deepseek_effort(e).to_string()),
            ),
        };
    }

    if is_kimi_or_moonshot(endpoint) {
        return kimi_chat_thinking_extras(&endpoint.model_name, effort);
    }

    // OpenAI / other chat providers: never send disable tokens as
    // `reasoning_effort` (rejected by the API).
    match effort {
        None => (None, None),
        Some(e) if is_thinking_disabled(e) => (None, None),
        Some(e) => (None, Some(e.to_string())),
    }
}

fn kimi_chat_thinking_extras(
    model_name: &str,
    effort: Option<&str>,
) -> (Option<Value>, Option<String>) {
    let model = model_name.to_ascii_lowercase();
    if model.contains("kimi-k3") || model.split(['/', '-', '_']).any(|p| p == "k3") {
        let mapped = effort.map(|e| {
            if is_thinking_disabled(e) {
                return None;
            }
            Some(
                match e.to_ascii_lowercase().as_str() {
                    "low" => "low",
                    "max" => "max",
                    _ => "high",
                }
                .to_string(),
            )
        });
        return (None, mapped.flatten());
    }
    if model.contains("k2.7") {
        return (None, None);
    }
    let supports_keep = model.contains("k2.6")
        || !(model.contains("k2.5") || model.contains("k2.7") || model.contains("kimi-k3"));

    match effort {
        None => (None, None),
        Some(e) if is_thinking_disabled(e) => (Some(serde_json::json!({"type": "disabled"})), None),
        Some(_) => {
            let thinking = if supports_keep {
                serde_json::json!({"type": "enabled", "keep": "all"})
            } else {
                serde_json::json!({"type": "enabled"})
            };
            (Some(thinking), None)
        }
    }
}

/// Responses-API `reasoning` object from `reasoning_effort`.
pub(crate) fn responses_reasoning_config(endpoint: &ModelEndpoint) -> Option<Value> {
    let effort = endpoint
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    if is_deepseek(endpoint) {
        return Some(serde_json::json!({ "effort": map_deepseek_effort(effort) }));
    }
    if is_thinking_disabled(effort) {
        return None;
    }
    Some(serde_json::json!({ "effort": effort }))
}

/// DeepSeek's Responses API keeps thinking-mode effort in a separate
/// `output_config` object. `reasoning.effort` is still required to toggle the
/// mode (and is retained by `responses_reasoning_config`).
pub(crate) fn responses_output_config(endpoint: &ModelEndpoint) -> Option<Value> {
    let effort = endpoint
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    if !is_deepseek(endpoint) || is_thinking_disabled(effort) {
        return None;
    }
    Some(serde_json::json!({
        "effort": map_deepseek_effort(effort)
    }))
}

/// True when the endpoint's thinking mode requires the assistant's reasoning
/// to be echoed back on every request that carries tool-call history
/// (chat-completions: `reasoning_content`; Responses compat: `reasoning_text`).
/// The affected APIs validate PRESENCE of the field, not its content — an
/// empty echo passes — so a tool-call turn on which the model skipped thinking
/// still needs the item injected. Providers in this class: DeepSeek
/// (thinking mode), Moonshot/Kimi K2.x+ (thinking on by default for the plain
/// `kimi-k2.6` model id), and MiMo. Matched by provider hint, base URL and
/// model name so a proxied/gatewayed endpoint is caught too.
pub(crate) fn requires_reasoning_echo(endpoint: &ModelEndpoint) -> bool {
    const REASONING_ECHO_PROVIDERS: [&str; 4] = ["deepseek", "kimi", "moonshot", "mimo"];
    let hay = vendor_haystack(endpoint);
    REASONING_ECHO_PROVIDERS.iter().any(|p| hay.contains(p))
}

/// Reconstruct the plain reasoning text from raw Anthropic `thinking` blocks.
/// Mirrors the anthropic adapter's `reasoning` assembly exactly (concatenation
/// of the `thinking` fields of `type == "thinking"` blocks, in order; redacted
/// data is skipped). Lets OpenAI-compatible adapters echo reasoning when the
/// canonical carries only the raw echo-capable blocks (the agent drops the
/// redundant `reasoning` copy on Anthropic messages).
pub(crate) fn reasoning_text_from_thinking_blocks(blocks: &[Value]) -> String {
    let mut out = String::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("thinking")
            && let Some(text) = block.get("thinking").and_then(Value::as_str)
        {
            out.push_str(text);
        }
    }
    out
}

/// Keep the TAIL of `text` bounded to `cap` characters. Full reasoning
/// (10k+ chars per turn) balloons request bodies and providers stall or
/// truncate mid-inference; the tail preserves the turn's conclusions.
/// Returns the input unchanged (no allocation) when it already fits, and
/// the cap counts CHARS (not bytes) so multi-byte text is never cut mid-codepoint.
/// Shared by the chat-completions (`reasoning_content`) and Responses
/// (`reasoning` item) adapters so the two echoes cannot drift apart.
pub(crate) fn reasoning_tail(text: String, cap: usize) -> String {
    let chars = text.chars().count();
    if chars <= cap {
        text
    } else {
        let skip = chars - cap;
        let start = text
            .char_indices()
            .nth(skip)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        text[start..].to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_reasoning_echo_covers_reasoning_echo_providers() {
        let deepseek = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model_name: "deepseek-v4-flash".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&deepseek));
        let proxied = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://gateway.example.com/v1".into(),
            model_name: "deepseek-reasoner".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&proxied));
        let kimi = ModelEndpoint {
            provider: "moonshot".into(),
            base_url: "https://api.moonshot.ai/v1".into(),
            model_name: "kimi-k2.6".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&kimi));
        let kimi_cn = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://api.moonshot.cn/v1".into(),
            model_name: "kimi-k2.7-code".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&kimi_cn));
        let mimo = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://platform.xiaomimimo.com/v1".into(),
            model_name: "MiMo-7B-RL".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&mimo));
        let openai = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model_name: "gpt-5".into(),
            ..Default::default()
        };
        assert!(!requires_reasoning_echo(&openai));
        let zhipu = ModelEndpoint {
            provider: "zhipu".into(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            model_name: "glm-5".into(),
            ..Default::default()
        };
        assert!(!requires_reasoning_echo(&zhipu));
    }

    #[test]
    fn map_deepseek_effort_follows_official_mapping() {
        assert_eq!(map_deepseek_effort("low"), "low");
        assert_eq!(map_deepseek_effort("medium"), "high");
        assert_eq!(map_deepseek_effort("high"), "high");
        assert_eq!(map_deepseek_effort("xhigh"), "high");
        assert_eq!(map_deepseek_effort("max"), "max");
        assert_eq!(map_deepseek_effort("none"), "none");
        assert_eq!(map_deepseek_effort("off"), "none");
    }

    #[test]
    fn chat_thinking_extras_deepseek_toggle_and_effort() {
        let base = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model_name: "deepseek-v4-pro".into(),
            ..Default::default()
        };
        let (thinking, effort) = chat_thinking_extras(&base);
        assert!(thinking.is_none());
        assert!(effort.is_none());

        let enabled = ModelEndpoint {
            reasoning_effort: Some("medium".into()),
            ..base.clone()
        };
        let (thinking, effort) = chat_thinking_extras(&enabled);
        assert_eq!(thinking, Some(serde_json::json!({"type": "enabled"})));
        assert_eq!(effort.as_deref(), Some("high"));

        let disabled = ModelEndpoint {
            reasoning_effort: Some("off".into()),
            ..base
        };
        let (thinking, effort) = chat_thinking_extras(&disabled);
        assert_eq!(thinking, Some(serde_json::json!({"type": "disabled"})));
        assert!(effort.is_none());
    }

    #[test]
    fn chat_thinking_extras_kimi_type_and_keep() {
        let k26 = ModelEndpoint {
            provider: "moonshot".into(),
            base_url: "https://api.moonshot.cn/v1".into(),
            model_name: "kimi-k2.6".into(),
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let (thinking, effort) = chat_thinking_extras(&k26);
        assert_eq!(
            thinking,
            Some(serde_json::json!({"type": "enabled", "keep": "all"}))
        );
        assert!(effort.is_none());

        let k25 = ModelEndpoint {
            model_name: "kimi-k2.5".into(),
            reasoning_effort: Some("low".into()),
            ..k26.clone()
        };
        let (thinking, effort) = chat_thinking_extras(&k25);
        assert_eq!(thinking, Some(serde_json::json!({"type": "enabled"})));
        assert!(effort.is_none());

        let k27 = ModelEndpoint {
            model_name: "kimi-k2.7-code".into(),
            reasoning_effort: Some("high".into()),
            ..k26.clone()
        };
        let (thinking, effort) = chat_thinking_extras(&k27);
        assert!(thinking.is_none());
        assert!(effort.is_none());

        let k3 = ModelEndpoint {
            model_name: "kimi-k3".into(),
            reasoning_effort: Some("medium".into()),
            ..k26
        };
        let (thinking, effort) = chat_thinking_extras(&k3);
        assert!(thinking.is_none());
        assert_eq!(effort.as_deref(), Some("high"));
    }

    #[test]
    fn responses_reasoning_config_openai_and_deepseek() {
        let openai = ModelEndpoint {
            provider: "openai".into(),
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        assert_eq!(
            responses_reasoning_config(&openai),
            Some(serde_json::json!({"effort": "medium"}))
        );
        let openai_off = ModelEndpoint {
            reasoning_effort: Some("off".into()),
            ..openai
        };
        assert!(responses_reasoning_config(&openai_off).is_none());

        let deepseek = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model_name: "deepseek-v4-flash".into(),
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        assert_eq!(
            responses_reasoning_config(&deepseek),
            Some(serde_json::json!({"effort": "high"}))
        );
        let off = ModelEndpoint {
            reasoning_effort: Some("none".into()),
            ..deepseek
        };
        assert_eq!(
            responses_reasoning_config(&off),
            Some(serde_json::json!({"effort": "none"}))
        );
        assert!(responses_reasoning_config(&ModelEndpoint::default()).is_none());
    }

    #[test]
    fn chat_thinking_extras_openai_off_omits_effort() {
        let endpoint = ModelEndpoint {
            provider: "openai".into(),
            reasoning_effort: Some("off".into()),
            ..Default::default()
        };
        let (thinking, effort) = chat_thinking_extras(&endpoint);
        assert!(thinking.is_none());
        assert!(effort.is_none());
    }

    #[test]
    fn reasoning_text_from_thinking_blocks_concatenates_thinking_only() {
        let blocks = vec![
            serde_json::json!({"type": "thinking", "thinking": "first part", "signature": "s1"}),
            serde_json::json!({"type": "text", "text": "visible"}),
            serde_json::json!({"type": "thinking", "thinking": "second part"}),
            serde_json::json!({"type": "redacted_thinking", "data": "redacted"}),
        ];
        assert_eq!(
            reasoning_text_from_thinking_blocks(&blocks),
            "first partsecond part"
        );
    }

    #[test]
    fn reasoning_text_from_thinking_blocks_empty_for_no_thinking() {
        assert_eq!(
            reasoning_text_from_thinking_blocks(&[serde_json::json!({"type": "text"})]),
            ""
        );
        assert_eq!(reasoning_text_from_thinking_blocks(&[]), "");
    }
}
