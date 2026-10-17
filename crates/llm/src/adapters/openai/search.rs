//! OpenAI Chat Completions web-search model support.

use haven_common::config::ModelEndpoint;
use serde_json::{Value, json};

use crate::adapters::{WebSearchMode, web_search};

pub(crate) fn search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if api_style != "openai-chat" || !endpoint.provider.eq_ignore_ascii_case("openai") {
        return None;
    }
    Some(endpoint.model_name.eq_ignore_ascii_case("gpt-5-search-api"))
}

pub(crate) fn search_options(
    endpoint: &ModelEndpoint,
    api_style: &str,
    mode: WebSearchMode,
) -> Option<Value> {
    if search_available(endpoint, api_style) != Some(true) || matches!(mode, WebSearchMode::Off) {
        return None;
    }

    // Chat Completions' dedicated search model always searches. The option is
    // the switch that selects that path; `auto` and `always` therefore map to
    // the same request shape.
    Some(json!({}))
}

pub(crate) fn search_call(
    endpoint: &ModelEndpoint,
    api_style: &str,
    annotations: &[Value],
) -> Option<Value> {
    if search_available(endpoint, api_style) != Some(true) {
        return None;
    }
    let citations = web_search::citations_from_annotations(annotations);
    (!citations.is_empty()).then(|| {
        json!({
            "type": "web_search_call",
            "id": "haven_search_openai",
            "status": "completed",
            "action": {"type": "search", "queries": [], "citations": citations}
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(model: &str) -> ModelEndpoint {
        ModelEndpoint {
            provider: "openai".into(),
            model_name: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn only_current_openai_search_model_gets_chat_search_options() {
        assert_eq!(
            search_options(
                &endpoint("gpt-5-search-api"),
                "openai-chat",
                WebSearchMode::Auto
            ),
            Some(json!({}))
        );
        assert_eq!(
            search_options(
                &endpoint("gpt-5-search-api"),
                "openai-chat",
                WebSearchMode::Off
            ),
            None
        );
        assert_eq!(
            search_options(
                &endpoint("gpt-4o-search-preview"),
                "openai-chat",
                WebSearchMode::Auto
            ),
            None
        );
        assert_eq!(
            search_options(
                &endpoint("gpt-5-search-api"),
                "openai-responses",
                WebSearchMode::Auto
            ),
            None
        );
        assert_eq!(
            search_options(
                &ModelEndpoint {
                    provider: "azure".into(),
                    ..endpoint("gpt-5-search-api")
                },
                "openai-chat",
                WebSearchMode::Auto
            ),
            None
        );
    }
}
