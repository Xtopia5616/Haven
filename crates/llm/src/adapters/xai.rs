//! xAI-specific search mappings for legacy Chat Completions and Responses.

use haven_common::config::ModelEndpoint;
use serde_json::{Value, json};

use super::{WebSearchMode, provider_features::vendor_haystack};

pub(crate) fn is_xai(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("xai")
}

pub(crate) fn responses_search_available(endpoint: &ModelEndpoint) -> Option<bool> {
    if !is_xai(endpoint) {
        return None;
    }
    Some(
        endpoint
            .model_name
            .to_ascii_lowercase()
            .starts_with("grok-4"),
    )
}

pub(crate) fn builtin_search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if !is_xai(endpoint) {
        return None;
    }

    Some(match api_style {
        // The legacy xAI protocol is explicitly the Chat Live Search adapter.
        "xai" => true,
        "openai-responses" => responses_search_available(endpoint).unwrap_or(false),
        _ => false,
    })
}

/// xAI Chat Completions `search_parameters.mode` mapping.
pub(crate) fn legacy_search_mode(mode: WebSearchMode) -> Option<&'static str> {
    match mode {
        WebSearchMode::Off => None,
        WebSearchMode::Auto => Some("auto"),
        WebSearchMode::Always => Some("on"),
    }
}

pub(crate) fn chat_search_parameters(
    endpoint: &ModelEndpoint,
    api_style: &str,
    mode: WebSearchMode,
) -> Option<Value> {
    (api_style == "xai" && is_xai(endpoint))
        .then(|| {
            legacy_search_mode(mode).map(|mode| json!({"mode": mode, "return_citations": true}))
        })
        .flatten()
}

pub(crate) fn citation_search_call(citations: Vec<String>) -> Option<Value> {
    (!citations.is_empty()).then(|| {
        json!({
            "type": "web_search_call",
            "id": "haven_search_xai",
            "status": "completed",
            "action": {"type": "search", "queries": [], "citations": citations}
        })
    })
}
