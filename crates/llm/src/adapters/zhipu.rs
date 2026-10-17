//! Zhipu Web-Search-Pro model adapter.

use haven_common::config::{IndependentSearchConfig, ModelEndpoint};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::types::LlmError;

pub(crate) async fn independent_search(
    endpoint: &ModelEndpoint,
    query: &str,
    cancel: CancellationToken,
) -> Result<Value, LlmError> {
    if !matches!(
        endpoint.independent_search,
        Some(IndependentSearchConfig::ZhipuWebSearch)
    ) {
        return Err(LlmError::UnsupportedCapability(
            "Zhipu Web-Search-Pro is not configured".into(),
        ));
    }
    let body = json!({
        "model": "web-search-pro",
        "messages": [{"role": "user", "content": query}],
        "top_p": 0.7,
        "temperature": 0.1,
        "stream": false,
    });
    let response = super::independent_search::post_json(
        endpoint,
        &chat_completions_url(&endpoint.base_url),
        body,
        cancel,
    )
    .await?;
    Ok(super::independent_search::normalize_results(
        query,
        "智谱 Web-Search-Pro",
        parse_search_results(&response),
    ))
}

fn chat_completions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/chat/completions") {
        base.to_string()
    } else {
        format!("{base}/chat/completions")
    }
}

fn parse_search_results(response: &Value) -> Vec<super::independent_search::SearchResultRow> {
    let Some(tool_calls) = response
        .pointer("/choices/0/message/tool_calls")
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    tool_calls
        .iter()
        .filter_map(|call| call.get("search_result"))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|item| {
            let title = item.get("title").and_then(Value::as_str)?.trim();
            let url = item
                .get("link")
                .or_else(|| item.get("url"))
                .and_then(Value::as_str)?
                .trim();
            if title.is_empty() || url.is_empty() {
                return None;
            }
            let snippet = item
                .get("content")
                .or_else(|| item.get("snippet"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|content| !content.is_empty())
                .map(ToString::to_string);
            Some(super::independent_search::SearchResultRow {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_completions_url_keeps_existing_path() {
        assert_eq!(
            chat_completions_url("https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        assert_eq!(
            chat_completions_url("https://example.test/chat/completions"),
            "https://example.test/chat/completions"
        );
    }

    #[test]
    fn parses_zhipu_search_result_shape() {
        let response = json!({"choices":[{"message":{"tool_calls":[
            {"search_result":[{"title":"Source","link":"https://example.com","content":"Snippet"}]}
        ]}}]});
        let rows = parse_search_results(&response);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url, "https://example.com");
        assert_eq!(rows[0].snippet.as_deref(), Some("Snippet"));
    }
}
