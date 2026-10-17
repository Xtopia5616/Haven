//! Groq-specific search policy for its OpenAI-compatible Responses API.

use haven_common::config::ModelEndpoint;

use super::{WebSearchMode, provider_features::vendor_haystack, web_search};

pub(crate) fn is_groq(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("groq")
}

fn supports_browser_search_model(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "openai/gpt-oss-20b" | "openai/gpt-oss-120b" | "openai/gpt-oss-safeguard-20b"
    )
}

fn supports_responses_browser_search_model(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "openai/gpt-oss-20b" | "openai/gpt-oss-120b"
    )
}

pub(crate) fn chat_search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if !is_groq(endpoint) || api_style != "openai-chat" {
        return None;
    }

    Some(supports_browser_search_model(&endpoint.model_name) && endpoint.response_format.is_none())
}

pub(crate) fn builtin_search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if !is_groq(endpoint) {
        return None;
    }

    Some(match api_style {
        "openai-chat" => chat_search_available(endpoint, api_style).unwrap_or(false),
        "openai-responses" => responses_search_available(endpoint).unwrap_or(false),
        _ => false,
    })
}

pub(crate) fn chat_search_tool_type(mode: WebSearchMode) -> Option<&'static str> {
    (!matches!(mode, WebSearchMode::Off)).then_some("browser_search")
}

pub(crate) fn chat_search_tool(
    endpoint: &ModelEndpoint,
    api_style: &str,
    mode: WebSearchMode,
) -> Option<&'static str> {
    (chat_search_available(endpoint, api_style) == Some(true))
        .then(|| chat_search_tool_type(mode))
        .flatten()
}

pub(crate) fn chat_search_is_required(
    endpoint: &ModelEndpoint,
    api_style: &str,
    mode: WebSearchMode,
    has_function_tools: bool,
) -> bool {
    chat_search_available(endpoint, api_style) == Some(true)
        && matches!(mode, WebSearchMode::Always)
        && !has_function_tools
}

pub(crate) fn search_call(annotations: &[serde_json::Value]) -> Option<serde_json::Value> {
    let citations = web_search::citations_from_annotations(annotations);
    (!citations.is_empty()).then(|| {
        serde_json::json!({
            "type": "web_search_call",
            "id": "haven_search_groq",
            "status": "completed",
            "action": {"type": "search", "queries": [], "citations": citations}
        })
    })
}

/// Groq documents browser search for GPT-OSS 20B/120B Responses requests;
/// structured outputs cannot be combined with browser search.
pub(crate) fn responses_search_available(endpoint: &ModelEndpoint) -> Option<bool> {
    if !is_groq(endpoint) {
        return None;
    }

    let model = endpoint.model_name.to_ascii_lowercase();
    Some(supports_responses_browser_search_model(&model) && endpoint.response_format.is_none())
}

pub(crate) fn responses_search_tool_type(endpoint: &ModelEndpoint) -> Option<&'static str> {
    is_groq(endpoint).then_some("browser_search")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(model: &str) -> ModelEndpoint {
        ModelEndpoint {
            provider: "groq".into(),
            model_name: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn browser_search_supports_current_groq_models_and_rejects_structured_output() {
        assert_eq!(
            chat_search_available(&endpoint("openai/gpt-oss-20b"), "openai-chat"),
            Some(true)
        );
        assert_eq!(
            chat_search_available(&endpoint("openai/gpt-oss-safeguard-20b"), "openai-chat"),
            Some(true)
        );
        assert_eq!(
            chat_search_available(&endpoint("llama-3.3-70b-versatile"), "openai-chat"),
            Some(false)
        );
        assert_eq!(
            chat_search_available(
                &ModelEndpoint {
                    response_format: Some(serde_json::json!({"type":"json_object"})),
                    ..endpoint("openai/gpt-oss-20b")
                },
                "openai-chat"
            ),
            Some(false)
        );
    }

    #[test]
    fn responses_search_uses_its_narrower_model_allowlist() {
        assert_eq!(
            responses_search_available(&endpoint("openai/gpt-oss-20b")),
            Some(true)
        );
        assert_eq!(
            responses_search_available(&endpoint("openai/gpt-oss-safeguard-20b")),
            Some(false)
        );
    }
}
