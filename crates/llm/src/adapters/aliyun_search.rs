//! Alibaba Cloud AI Search Open Platform's direct Web Search API.

use haven_common::config::{
    AliyunSearchContentType, AliyunSearchTimeRange, AliyunSearchWay, IndependentSearchConfig,
    ModelEndpoint,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::types::LlmError;

const SERVICE_ID: &str = "ops-web-search-001";

pub(crate) async fn independent_search(
    endpoint: &ModelEndpoint,
    query: &str,
    filters: &crate::types::IndependentSearchFilters,
    cancel: CancellationToken,
) -> Result<Value, LlmError> {
    let Some(IndependentSearchConfig::AliyunAiSearch {
        endpoint: search_endpoint,
        workspace_name,
        top_k,
        query_rewrite,
        content_type,
        way,
        time_range,
        api_key,
        ..
    }) = endpoint.independent_search.as_ref()
    else {
        return Err(LlmError::UnsupportedCapability(
            "Alibaba AI Search is not configured".into(),
        ));
    };
    if api_key.is_empty() {
        return Err(LlmError::Configuration(
            "Alibaba AI Search API key is not configured".into(),
        ));
    }

    let url = search_url(search_endpoint, workspace_name);
    let effective_top_k = filters.max_results.unwrap_or(*top_k);
    let effective_time_range = filters
        .recency
        .and_then(recency_time_range)
        .unwrap_or(*time_range);
    let mut body = json!({
        "query": query,
        "query_rewrite": query_rewrite,
        "top_k": effective_top_k,
        "content_type": content_type_value(*content_type),
        "way": way_value(*way),
    });
    if effective_time_range != AliyunSearchTimeRange::NoLimit {
        body["time_range"] = json!(time_range_value(effective_time_range));
    }

    let mut search_endpoint = endpoint.clone();
    search_endpoint.api_key.clone_from(api_key);
    search_endpoint.auth_header_name = "Authorization".into();
    search_endpoint.auth_header_prefix = "Bearer".into();
    let response =
        super::independent_search::post_json(&search_endpoint, &url, body, cancel).await?;
    if response
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| !code.is_empty())
    {
        return Err(LlmError::InvalidResponse(
            "Alibaba AI Search returned an unsuccessful response".into(),
        ));
    }
    let rows = super::independent_search::rows_from_values(
        response
            .get("result")
            .and_then(|result| result.get("search_result"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .cloned(),
        "title",
        "link",
        &["content", "snippet"],
    );
    Ok(super::independent_search::normalize_results(
        query,
        "Alibaba AI Search",
        rows,
    ))
}

fn search_url(host: &str, workspace_name: &str) -> String {
    format!(
        "{}/v3/openapi/workspaces/{workspace_name}/web-search/{SERVICE_ID}",
        host.trim_end_matches('/')
    )
}

fn content_type_value(value: AliyunSearchContentType) -> &'static str {
    match value {
        AliyunSearchContentType::Snippet => "snippet",
        AliyunSearchContentType::Summary => "summary",
        AliyunSearchContentType::MainText => "mainText",
    }
}

fn way_value(value: AliyunSearchWay) -> &'static str {
    match value {
        AliyunSearchWay::Lite => "lite",
        AliyunSearchWay::Pro => "pro",
        AliyunSearchWay::ProFetch => "pro-fetch",
    }
}

fn time_range_value(value: AliyunSearchTimeRange) -> &'static str {
    match value {
        AliyunSearchTimeRange::Day => "day",
        AliyunSearchTimeRange::Week => "week",
        AliyunSearchTimeRange::Month => "month",
        AliyunSearchTimeRange::Year => "year",
        AliyunSearchTimeRange::NoLimit => "no_limit",
    }
}

fn recency_time_range(value: crate::types::WebSearchRecency) -> Option<AliyunSearchTimeRange> {
    match value {
        crate::types::WebSearchRecency::Hour => None,
        crate::types::WebSearchRecency::Day => Some(AliyunSearchTimeRange::Day),
        crate::types::WebSearchRecency::Week => Some(AliyunSearchTimeRange::Week),
        crate::types::WebSearchRecency::Month => Some(AliyunSearchTimeRange::Month),
        crate::types::WebSearchRecency::Year => Some(AliyunSearchTimeRange::Year),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_documented_content_modes_and_time_ranges() {
        assert_eq!(
            content_type_value(AliyunSearchContentType::MainText),
            "mainText"
        );
        assert_eq!(way_value(AliyunSearchWay::ProFetch), "pro-fetch");
        assert_eq!(time_range_value(AliyunSearchTimeRange::Week), "week");
    }

    #[test]
    fn constructs_workspace_scoped_builtin_service_endpoint() {
        assert_eq!(
            search_url("https://opensearch.example", "default"),
            "https://opensearch.example/v3/openapi/workspaces/default/web-search/ops-web-search-001"
        );
    }
}
