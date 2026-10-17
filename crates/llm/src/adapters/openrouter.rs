//! OpenRouter-specific Chat Completions and Responses server-tool support.

use haven_common::config::ModelEndpoint;
use serde_json::{Value, json};

use super::{WebSearchMode, provider_features::vendor_haystack, web_search};

pub(crate) fn is_openrouter(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("openrouter")
}

/// OpenRouter exposes its server search tool on both Chat Completions and
/// Responses. It is model-agnostic and falls back to a search engine when the
/// selected model has no native search implementation.
pub(crate) fn builtin_search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if !is_openrouter(endpoint) {
        return None;
    }
    Some(matches!(api_style, "openrouter" | "openai-responses"))
}

/// OpenRouter's server tool is model-decided. The provider does not document
/// a `tool_choice` form that forces only this server-side tool, so `always`
/// exposes the tool just like `auto`.
pub(crate) fn chat_search_tool_type(mode: WebSearchMode) -> Option<&'static str> {
    (!matches!(mode, WebSearchMode::Off)).then_some("openrouter:web_search")
}

pub(crate) fn chat_search_tool(
    endpoint: &ModelEndpoint,
    api_style: &str,
    mode: WebSearchMode,
) -> Option<&'static str> {
    (builtin_search_available(endpoint, api_style) == Some(true))
        .then(|| chat_search_tool_type(mode))
        .flatten()
}

pub(crate) fn responses_search_tool_type(endpoint: &ModelEndpoint) -> Option<&'static str> {
    is_openrouter(endpoint).then_some("openrouter:web_search")
}

pub(crate) fn responses_search_can_force(endpoint: &ModelEndpoint) -> Option<bool> {
    is_openrouter(endpoint).then_some(false)
}

pub(crate) fn citations_from_annotations(annotations: &[Value]) -> Vec<Value> {
    web_search::citations_from_annotations(annotations)
}

pub(crate) fn search_count(server_tool_use: &Value) -> Option<u64> {
    server_tool_use
        .pointer("/web_search_requests")
        .and_then(Value::as_u64)
}

fn search_call_with_usage(citations: Vec<Value>, count: Option<u64>) -> Option<Value> {
    if citations.is_empty() && count.is_none_or(|count| count == 0) {
        return None;
    }
    let mut action = json!({"type": "search", "queries": [], "citations": citations});
    if let Some(count) = count {
        action["search_count"] = json!(count);
    }
    Some(json!({
        "type": "web_search_call",
        "id": "haven_search_openrouter",
        "status": "completed",
        "action": action
    }))
}

pub(crate) fn chat_search_call(
    endpoint: &ModelEndpoint,
    api_style: &str,
    annotations: &[Value],
    server_tool_use: Option<&Value>,
) -> Option<Value> {
    if builtin_search_available(endpoint, api_style) != Some(true) {
        return None;
    }
    let citations = citations_from_annotations(annotations);
    let count = server_tool_use.and_then(search_count);
    search_call_with_usage(citations, count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint() -> ModelEndpoint {
        ModelEndpoint {
            provider: "openrouter".into(),
            model_name: "openai/gpt-5.5".into(),
            ..Default::default()
        }
    }

    #[test]
    fn chat_search_usage_and_annotations_are_projected_to_one_call() {
        let annotations = [json!({
            "type": "url_citation",
            "url_citation": {
                "title": "OpenRouter docs",
                "url": "https://openrouter.ai/docs",
                "content": "Server tools guide"
            }
        })];
        let usage = json!({"web_search_requests": 2});
        let call = chat_search_call(&endpoint(), "openrouter", &annotations, Some(&usage))
            .expect("search call");
        assert_eq!(call["id"], "haven_search_openrouter");
        assert_eq!(call["action"]["search_count"], 2);
        assert_eq!(
            call["action"]["citations"][0]["url"],
            "https://openrouter.ai/docs"
        );
    }
}
