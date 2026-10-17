//! Dispatch and bounded transport for provider-owned standalone search APIs.
//! Vendor request and response protocols stay in their respective modules.

use std::time::Duration;

use haven_common::config::{IndependentSearchConfig, ModelEndpoint};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::types::LlmError;

const MAX_SEARCH_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_SEARCH_QUERY_CHARS: usize = 512;
const MAX_SEARCH_RESULT_FIELD_CHARS: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchResultRow {
    pub(crate) title: String,
    pub(crate) url: String,
    pub(crate) snippet: Option<String>,
}

pub(crate) fn independent_search_available(endpoint: &ModelEndpoint) -> bool {
    !endpoint.api_key.is_empty()
        && !endpoint.base_url.trim().is_empty()
        && endpoint.independent_search.as_ref().is_some_and(|search| {
            endpoint
                .provider
                .trim()
                .eq_ignore_ascii_case(search.provider_id())
        })
}

pub(crate) async fn independent_search(
    endpoint: &ModelEndpoint,
    query: &str,
    cancel: CancellationToken,
) -> Result<Value, LlmError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(LlmError::Configuration(
            "web search query must not be empty".into(),
        ));
    }
    if query.chars().count() > MAX_SEARCH_QUERY_CHARS {
        return Err(LlmError::Configuration(format!(
            "web search query must be at most {MAX_SEARCH_QUERY_CHARS} characters"
        )));
    }
    if !independent_search_available(endpoint) {
        return Err(LlmError::UnsupportedCapability(
            "standalone web search is not configured for the selected model".into(),
        ));
    }

    let search = endpoint
        .independent_search
        .as_ref()
        .expect("availability check requires standalone search config");
    match search {
        IndependentSearchConfig::DeepseekSearch { .. } => {
            super::deepseek::independent_search(endpoint, query, cancel).await
        }
        IndependentSearchConfig::PerplexitySearch { .. } => {
            super::perplexity::independent_search(endpoint, query, cancel).await
        }
        IndependentSearchConfig::MistralWebSearch { .. } => {
            super::mistral::independent_search(endpoint, query, cancel).await
        }
        IndependentSearchConfig::ZhipuWebSearch => {
            super::zhipu::independent_search(endpoint, query, cancel).await
        }
    }
}

pub(crate) async fn post_json(
    endpoint: &ModelEndpoint,
    url: &str,
    body: Value,
    cancel: CancellationToken,
) -> Result<Value, LlmError> {
    if cancel.is_cancelled() {
        return Err(LlmError::Cancelled);
    }
    let client = super::build_client(endpoint)?;
    let headers = super::build_headers(endpoint, "Authorization", true)?;
    // Search products may fetch and rerank remote pages, so the ordinary
    // seven-second chat default is too short. Preserve longer provider
    // overrides while giving searches a useful 30-second floor and a hard
    // two-minute cap. The Agent tool has the same outer deadline.
    let timeout = Duration::from_secs(endpoint.timeout_secs.clamp(30, 120));
    let request = client
        .post(url)
        .headers(headers)
        .timeout(timeout)
        .json(&body);
    let response = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        response = super::send_request(request, Some(timeout)) => response?,
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(LlmError::Cancelled),
        result = super::read_json_bounded(response, MAX_SEARCH_RESPONSE_BYTES) => result,
    }
}

pub(crate) fn normalize_results(
    query: &str,
    label: &str,
    rows: impl IntoIterator<Item = SearchResultRow>,
) -> Value {
    let results = rows
        .into_iter()
        .filter_map(|mut row| {
            let url = url::Url::parse(row.url.trim()).ok()?;
            if !matches!(url.scheme(), "http" | "https")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.as_str().chars().count() > MAX_SEARCH_RESULT_FIELD_CHARS
            {
                return None;
            }
            row.title = truncate(&row.title, MAX_SEARCH_RESULT_FIELD_CHARS);
            row.url = url.to_string();
            row.snippet = row
                .snippet
                .map(|snippet| truncate(&snippet, MAX_SEARCH_RESULT_FIELD_CHARS));
            Some(json!({
                "title": row.title,
                "url": row.url,
                "snippet": row.snippet,
            }))
        })
        .collect::<Vec<_>>();
    json!({
        "label": label,
        "queries": [query],
        "results": results,
    })
}

pub(crate) fn rows_from_values(
    values: impl IntoIterator<Item = Value>,
    title_key: &str,
    url_key: &str,
    snippet_keys: &[&str],
) -> Vec<SearchResultRow> {
    values
        .into_iter()
        .filter_map(|value| {
            let title = value.get(title_key)?.as_str()?.trim();
            let url = value.get(url_key)?.as_str()?.trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = snippet_keys
                .iter()
                .find_map(|key| {
                    value
                        .get(*key)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|snippet| !snippet.is_empty())
                })
                .map(ToString::to_string);
            Some(SearchResultRow {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .collect()
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizer_keeps_only_http_sources_and_bounds_fields() {
        let output = normalize_results(
            "weather",
            "Perplexity Search",
            [
                SearchResultRow {
                    title: "ok".into(),
                    url: "https://example.com/path".into(),
                    snippet: Some("fresh".into()),
                },
                SearchResultRow {
                    title: "bad".into(),
                    url: "file:///private".into(),
                    snippet: None,
                },
                SearchResultRow {
                    title: "credentials".into(),
                    url: "https://user:password@example.com/private".into(),
                    snippet: None,
                },
            ],
        );
        assert_eq!(output["label"], "Perplexity Search");
        assert_eq!(output["queries"], json!(["weather"]));
        assert_eq!(output["results"].as_array().unwrap().len(), 1);
        assert_eq!(output["results"][0]["url"], "https://example.com/path");
    }

    #[test]
    fn rows_skip_missing_required_values_and_select_first_available_snippet() {
        let rows = rows_from_values(
            [
                json!({"title":"title", "url":"https://example.com", "summary":"summary"}),
                json!({"title":"missing url"}),
            ],
            "title",
            "url",
            &["snippet", "summary"],
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].snippet.as_deref(), Some("summary"));
    }
}
