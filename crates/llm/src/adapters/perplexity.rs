//! Perplexity Search API request and response handling.

use haven_common::config::{
    IndependentSearchConfig, ModelEndpoint, PerplexitySearchRecency, PerplexitySearchType,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::types::LlmError;

pub(crate) async fn independent_search(
    endpoint: &ModelEndpoint,
    query: &str,
    cancel: CancellationToken,
) -> Result<Value, LlmError> {
    let Some(IndependentSearchConfig::PerplexitySearch {
        max_results,
        search_type,
        recency_filter,
    }) = endpoint.independent_search.as_ref()
    else {
        return Err(LlmError::UnsupportedCapability(
            "Perplexity Search is not configured".into(),
        ));
    };
    let mut body = json!({
        "query": query,
        "max_results": max_results,
        "search_type": search_type_value(*search_type),
    });
    if let Some(recency) = recency_filter {
        body["search_recency_filter"] = json!(recency_value(*recency));
    }
    let response = super::independent_search::post_json(
        endpoint,
        &search_api_url(&endpoint.base_url),
        body,
        cancel,
    )
    .await?;
    let rows = super::independent_search::rows_from_values(
        response
            .get("results")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned(),
        "title",
        "url",
        &["snippet"],
    );
    Ok(super::independent_search::normalize_results(
        query,
        "Perplexity Search",
        rows,
    ))
}

fn search_api_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/search") {
        base.to_string()
    } else {
        format!("{}/search", base.strip_suffix("/v1").unwrap_or(base))
    }
}

fn search_type_value(value: PerplexitySearchType) -> &'static str {
    match value {
        PerplexitySearchType::Web => "web",
        PerplexitySearchType::Fast => "fast",
        PerplexitySearchType::People => "people",
    }
}

fn recency_value(value: PerplexitySearchRecency) -> &'static str {
    match value {
        PerplexitySearchRecency::Hour => "hour",
        PerplexitySearchRecency::Day => "day",
        PerplexitySearchRecency::Week => "week",
        PerplexitySearchRecency::Month => "month",
        PerplexitySearchRecency::Year => "year",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_api_url_uses_root_search_endpoint() {
        assert_eq!(
            search_api_url("https://api.perplexity.ai"),
            "https://api.perplexity.ai/search"
        );
        assert_eq!(
            search_api_url("https://api.perplexity.ai/v1/"),
            "https://api.perplexity.ai/search"
        );
        assert_eq!(
            search_api_url("https://proxy.test/search"),
            "https://proxy.test/search"
        );
    }

    #[test]
    fn maps_only_documented_search_filters() {
        assert_eq!(search_type_value(PerplexitySearchType::Fast), "fast");
        assert_eq!(search_type_value(PerplexitySearchType::People), "people");
        assert_eq!(recency_value(PerplexitySearchRecency::Month), "month");
    }
}
