//! DeepSeek-specific search capability checks.

use haven_common::config::ModelEndpoint;

use super::{openrouter, provider_features::is_deepseek};

/// DeepSeek's direct Responses API exposes `web_search` only on V4 Pro.
/// `None` means this is not a DeepSeek-owned endpoint, so another adapter
/// policy (or the generic Responses contract) can decide.
pub(crate) fn builtin_search_available(endpoint: &ModelEndpoint, api_style: &str) -> Option<bool> {
    if openrouter::is_openrouter(endpoint) || !is_deepseek(endpoint) {
        return None;
    }

    Some(api_style == "openai-responses" && supports_responses_model(&endpoint.model_name))
}

fn supports_responses_model(model: &str) -> bool {
    model.eq_ignore_ascii_case("deepseek-v4-pro")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(model: &str) -> ModelEndpoint {
        ModelEndpoint {
            provider: "deepseek".into(),
            model_name: model.into(),
            ..Default::default()
        }
    }

    #[test]
    fn only_direct_v4_pro_responses_supports_search() {
        assert_eq!(
            builtin_search_available(&endpoint("deepseek-v4-pro"), "openai-responses"),
            Some(true)
        );
        assert_eq!(
            builtin_search_available(&endpoint("deepseek-v4-flash"), "openai-responses"),
            Some(false)
        );
        assert_eq!(
            builtin_search_available(&endpoint("deepseek-v4-pro-0813"), "openai-responses"),
            Some(false)
        );
        assert_eq!(
            builtin_search_available(&endpoint("deepseek-v4-pro"), "openai-chat"),
            Some(false)
        );
    }
}
