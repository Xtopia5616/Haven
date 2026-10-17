//! Alibaba Cloud Model Studio (DashScope) search policy and wire fields.

use haven_common::config::ModelEndpoint;
use serde_json::{Value, json};

use super::{WebSearchMode, provider_features::vendor_haystack};

pub(crate) fn is_dashscope(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("dashscope")
        || endpoint
            .base_url
            .to_ascii_lowercase()
            .contains(".maas.aliyuncs.com")
}

fn model_starts_with_any(model: &str, prefixes: &[&str]) -> bool {
    let model = model.to_ascii_lowercase();
    prefixes.iter().any(|prefix| model.starts_with(prefix))
}

fn supports_chat_model(model: &str) -> bool {
    model_starts_with_any(
        model,
        &[
            "qwen3.5",
            "qwen3.6",
            "qwen3.7",
            "qwen3.8",
            "deepseek-v4",
            "qwen-max",
            "qwen-plus",
            "qwen-flash",
        ],
    )
}

fn supports_responses_model(model: &str) -> bool {
    model_starts_with_any(
        model,
        &[
            "qwen3.5-plus",
            "qwen3.5-flash",
            "qwen3.5-27b",
            "qwen3.5-35b-a3b",
            "qwen3.5-122b-a10b",
            "qwen3.5-397b-a17b",
            "qwen3.6-plus",
            "qwen3.6-flash",
            "qwen3.6-35b-a3b",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.7-flash",
            "qwen3.8-max",
            "qwen3.8-flash",
            "qwen3.8-2.4t-a95b",
            "qwen3.8-27b",
            "qwen3.8-omni-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash-0731",
            "glm-5.2",
            "kimi-k3",
        ],
    )
}

/// Return a provider-specific decision only for DashScope endpoints.
pub(crate) fn builtin_search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if !is_dashscope(endpoint) {
        return None;
    }

    Some(match api_style {
        "dashscope" => supports_chat_model(&endpoint.model_name),
        "openai-responses" => supports_responses_model(&endpoint.model_name),
        _ => false,
    })
}

/// Chat Completions supports `enable_search` and can force a search with
/// `search_options.forced_search`. Qwen 3.8 Omni additionally requires the
/// `agent` search strategy.
pub(crate) fn chat_search_enabled(mode: WebSearchMode) -> bool {
    !matches!(mode, WebSearchMode::Off)
}

pub(crate) fn responses_search_can_force(endpoint: &ModelEndpoint) -> Option<bool> {
    is_dashscope(endpoint).then_some(false)
}

pub(crate) fn chat_search_options(model: &str, mode: WebSearchMode) -> Option<Value> {
    if !chat_search_enabled(mode) {
        return None;
    }

    let mut options = serde_json::Map::new();
    if matches!(mode, WebSearchMode::Always) {
        options.insert("forced_search".into(), Value::Bool(true));
    }
    if model_starts_with_any(model, &["qwen3.5-omni", "qwen3.8-omni-flash"]) {
        options.insert("search_strategy".into(), Value::String("agent".into()));
    }
    (!options.is_empty()).then_some(Value::Object(options))
}

pub(crate) fn search_count(x_tools: &Value) -> Option<u64> {
    x_tools.pointer("/search/count").and_then(Value::as_u64)
}

pub(crate) fn search_call(count: u64) -> Value {
    json!({
        "type": "web_search_call",
        "id": "haven_search_dashscope",
        "status": "completed",
        "action": {
            "type": "search",
            "queries": [],
            "search_count": count,
            "count_semantics": "intent_detected"
        }
    })
}

pub(crate) fn chat_search_call(usage: Option<&Value>) -> Option<Value> {
    let count = usage.and_then(search_count)?;
    (count > 0).then(|| search_call(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(model: &str) -> ModelEndpoint {
        ModelEndpoint {
            provider: "dashscope".into(),
            model_name: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn capability_uses_dashscope_model_allowlists() {
        assert_eq!(
            builtin_search_available(&endpoint("qwen-plus"), "dashscope"),
            Some(true)
        );
        assert_eq!(
            builtin_search_available(&endpoint("qwen2.5-72b"), "dashscope"),
            Some(false)
        );
        assert_eq!(
            builtin_search_available(&endpoint("qwen3.8-max"), "openai-responses"),
            Some(true)
        );
        assert_eq!(
            builtin_search_available(&endpoint("glm-5.2"), "openai-responses"),
            Some(true)
        );
        assert_eq!(
            builtin_search_available(&endpoint("glm-5.2"), "dashscope"),
            Some(false)
        );
        assert_eq!(
            builtin_search_available(&endpoint("glm-5.3"), "openai-responses"),
            Some(false)
        );
        assert_eq!(
            builtin_search_available(
                &ModelEndpoint {
                    provider: "openai".into(),
                    ..endpoint("qwen3.8-max")
                },
                "openai-responses"
            ),
            None
        );
    }

    #[test]
    fn chat_search_usage_count_is_read_from_openai_compatible_usage() {
        assert_eq!(search_count(&json!({"search": {"count": 2}})), Some(2));
        assert_eq!(search_count(&json!({"x_tools": {}})), None);
    }

    #[test]
    fn chat_search_options_force_and_select_qwen_omni_strategy() {
        assert_eq!(chat_search_options("qwen-plus", WebSearchMode::Auto), None);
        assert_eq!(
            chat_search_options("qwen-plus", WebSearchMode::Always),
            Some(json!({"forced_search": true}))
        );
        assert_eq!(
            chat_search_options("qwen3.8-omni-flash", WebSearchMode::Auto),
            Some(json!({"search_strategy": "agent"}))
        );
        assert_eq!(
            chat_search_options("qwen3.5-omni-plus", WebSearchMode::Auto),
            Some(json!({"search_strategy": "agent"}))
        );
        assert_eq!(
            chat_search_options("qwen3.8-omni-flash", WebSearchMode::Always),
            Some(json!({"forced_search": true, "search_strategy": "agent"}))
        );
    }
}
