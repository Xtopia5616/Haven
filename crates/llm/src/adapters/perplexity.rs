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
    let Some(config @ IndependentSearchConfig::PerplexitySearch { .. }) =
        endpoint.independent_search.as_ref()
    else {
        return Err(LlmError::UnsupportedCapability(
            "Perplexity Search is not configured".into(),
        ));
    };
    let body = search_request_body(query, config)?;
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

fn search_request_body(query: &str, config: &IndependentSearchConfig) -> Result<Value, LlmError> {
    let IndependentSearchConfig::PerplexitySearch {
        max_results,
        search_type,
        recency_filter,
        country,
        max_tokens,
        max_tokens_per_page,
        search_language_filter,
        search_domain_filter,
        last_updated_after_filter,
        last_updated_before_filter,
        search_after_date_filter,
        search_before_date_filter,
    } = config
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
    if let Some(country) = country
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        body["country"] = json!(country.to_ascii_uppercase());
    }
    if let Some(max_tokens) = max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    if let Some(max_tokens_per_page) = max_tokens_per_page {
        body["max_tokens_per_page"] = json!(max_tokens_per_page);
    }
    if !search_language_filter.is_empty() {
        body["search_language_filter"] = json!(search_language_filter);
    }
    if !search_domain_filter.is_empty() {
        body["search_domain_filter"] = json!(search_domain_filter);
    }
    for (key, value) in [
        ("last_updated_after_filter", last_updated_after_filter),
        ("last_updated_before_filter", last_updated_before_filter),
        ("search_after_date_filter", search_after_date_filter),
        ("search_before_date_filter", search_before_date_filter),
    ] {
        if let Some(value) = value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            body[key] = json!(value);
        }
    }
    Ok(body)
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

    #[test]
    fn includes_configured_filters_in_the_search_api_body() {
        let config = IndependentSearchConfig::PerplexitySearch {
            max_results: 8,
            search_type: PerplexitySearchType::Fast,
            recency_filter: Some(PerplexitySearchRecency::Week),
            country: Some("us".into()),
            max_tokens: Some(20_000),
            max_tokens_per_page: Some(2_000),
            search_language_filter: vec!["en".into(), "zh".into()],
            search_domain_filter: vec!["wikipedia.org".into(), "-example.com".into()],
            last_updated_after_filter: Some("01/01/2026".into()),
            last_updated_before_filter: None,
            search_after_date_filter: None,
            search_before_date_filter: Some("12/31/2026".into()),
        };
        let body = search_request_body("latest API changes", &config).unwrap();
        assert_eq!(body["country"], "US");
        assert_eq!(body["max_tokens"], 20_000);
        assert_eq!(body["max_tokens_per_page"], 2_000);
        assert_eq!(body["search_language_filter"], json!(["en", "zh"]));
        assert_eq!(
            body["search_domain_filter"],
            json!(["wikipedia.org", "-example.com"])
        );
        assert_eq!(body["search_recency_filter"], "week");
        assert_eq!(body["last_updated_after_filter"], "01/01/2026");
        assert_eq!(body["search_before_date_filter"], "12/31/2026");
        assert!(body.get("last_updated_before_filter").is_none());
    }
}
