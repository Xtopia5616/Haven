//! DeepSeek-specific search capability checks.

use haven_common::config::{IndependentSearchConfig, ModelEndpoint};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

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

/// Call DeepSeek's standalone Search API. This remains separate from its
/// Responses `web_search` tool and uses Search & Fetch product parameters.
pub(crate) async fn independent_search(
    endpoint: &ModelEndpoint,
    query: &str,
    original_question: Option<&str>,
    filters: &crate::types::IndependentSearchFilters,
    cancel: CancellationToken,
) -> Result<Value, crate::types::LlmError> {
    let Some(IndependentSearchConfig::DeepseekSearch {
        count,
        need_content,
        pass_original_question,
        localization,
    }) = endpoint.independent_search.as_ref()
    else {
        return Err(crate::types::LlmError::UnsupportedCapability(
            "DeepSeek Search is not configured".into(),
        ));
    };
    if query.chars().count() > 128 {
        return Err(crate::types::LlmError::Configuration(
            "DeepSeek Search queries must be at most 128 characters".into(),
        ));
    }
    let body = search_request_body(
        query,
        filters.max_results.unwrap_or(*count),
        *need_content,
        *pass_original_question,
        original_question,
        localization.as_deref(),
    )?;
    let response = super::independent_search::post_json(
        endpoint,
        &search_api_url(&endpoint.base_url),
        body,
        cancel,
    )
    .await?;
    let snippet_keys: &[&str] = if *need_content {
        &["content", "summary", "snippet"]
    } else {
        &["summary", "snippet"]
    };
    let rows = super::independent_search::rows_from_values(
        response
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned(),
        "title",
        "url",
        snippet_keys,
    );
    Ok(super::independent_search::normalize_results(
        query,
        "DeepSeek Search",
        rows,
    ))
}

fn search_request_body(
    query: &str,
    count: u8,
    need_content: bool,
    pass_original_question: bool,
    original_question: Option<&str>,
    localization: Option<&str>,
) -> Result<Value, crate::types::LlmError> {
    let mut body = json!({
        "query": query,
        "count": count,
        "need_content": need_content,
    });
    if pass_original_question
        && let Some(original_question) = original_question
            .map(str::trim)
            .filter(|value| !value.is_empty())
    {
        if original_question.chars().count() > 128 {
            return Err(crate::types::LlmError::Configuration(
                "DeepSeek Search original questions must be at most 128 characters".into(),
            ));
        }
        body["ori_question"] = json!(original_question);
    }
    if let Some(localization) = localization
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body["localization"] = json!(localization);
    }
    Ok(body)
}

fn search_api_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1/tools/search") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/tools/search")
    } else {
        format!("{base}/v1/tools/search")
    }
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

    #[test]
    fn standalone_search_url_supports_host_and_v1_bases() {
        assert_eq!(
            search_api_url("https://api.deepseek.com"),
            "https://api.deepseek.com/v1/tools/search"
        );
        assert_eq!(
            search_api_url("https://api.deepseek.com/v1/"),
            "https://api.deepseek.com/v1/tools/search"
        );
    }

    #[test]
    fn sends_original_question_only_when_enabled_and_explicitly_provided() {
        let body = search_request_body("weather tomorrow", 5, false, false, Some("original"), None)
            .unwrap();
        assert!(body.get("ori_question").is_none());

        let body = search_request_body(
            "weather tomorrow",
            5,
            false,
            true,
            Some("  What is the weather tomorrow?  "),
            Some("203.0.113.7"),
        )
        .unwrap();
        assert_eq!(body["ori_question"], "What is the weather tomorrow?");
        assert_eq!(body["localization"], "203.0.113.7");
    }
}
