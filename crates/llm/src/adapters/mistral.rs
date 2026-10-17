//! Mistral Conversations API web-search adapter.

use haven_common::config::{IndependentSearchConfig, ModelEndpoint};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::types::LlmError;

pub(crate) async fn independent_search(
    endpoint: &ModelEndpoint,
    query: &str,
    cancel: CancellationToken,
) -> Result<Value, LlmError> {
    let Some(IndependentSearchConfig::MistralWebSearch { premium }) =
        endpoint.independent_search.as_ref()
    else {
        return Err(LlmError::UnsupportedCapability(
            "Mistral web search is not configured".into(),
        ));
    };
    let tool_type = if *premium {
        "web_search_premium"
    } else {
        "web_search"
    };
    let body = json!({
        "model": endpoint.model_name,
        "instructions": "Always use the web search tool to research the user's query. Return a concise answer grounded in the retrieved sources, and do not invent sources.",
        "inputs": query,
        "tools": [{"type": tool_type}],
        "store": false,
        "stream": false,
    });
    let response = super::independent_search::post_json(
        endpoint,
        &conversations_url(&endpoint.base_url),
        body,
        cancel,
    )
    .await?;
    let (answer, rows) = parse_conversation_output(&response);
    let mut result = super::independent_search::normalize_results(
        query,
        if *premium {
            "Mistral Web Search Premium"
        } else {
            "Mistral Web Search"
        },
        rows,
    );
    if let Some(answer) = answer
        && let Some(object) = result.as_object_mut()
    {
        object.insert(
            "answer".into(),
            Value::String(answer.chars().take(6_000).collect()),
        );
    }
    Ok(result)
}

fn conversations_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    if base.ends_with("/v1/conversations") {
        base.to_string()
    } else if base.ends_with("/v1") {
        format!("{base}/conversations")
    } else {
        format!("{base}/v1/conversations")
    }
}

fn parse_conversation_output(
    response: &Value,
) -> (
    Option<String>,
    Vec<super::independent_search::SearchResultRow>,
) {
    let mut answer_parts = Vec::new();
    let mut rows = Vec::new();
    if let Some(outputs) = response.get("outputs").and_then(Value::as_array) {
        for output in outputs {
            if output.get("type").and_then(Value::as_str) != Some("message.output") {
                continue;
            }
            if let Some(content) = output.get("content").and_then(Value::as_array) {
                for chunk in content {
                    match chunk.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = chunk.get("text").and_then(Value::as_str) {
                                let text = text.trim();
                                if !text.is_empty() {
                                    answer_parts.push(text.to_string());
                                }
                            }
                        }
                        Some("tool_reference") => {
                            let parsed = super::independent_search::rows_from_values(
                                [chunk.clone()],
                                "title",
                                "url",
                                &["source", "text"],
                            );
                            rows.extend(parsed);
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    let answer = (!answer_parts.is_empty()).then(|| answer_parts.join("\n\n"));
    (answer, rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn conversation_url_supports_root_and_v1_bases() {
        assert_eq!(
            conversations_url("https://api.mistral.ai"),
            "https://api.mistral.ai/v1/conversations"
        );
        assert_eq!(
            conversations_url("https://api.mistral.ai/v1/"),
            "https://api.mistral.ai/v1/conversations"
        );
    }

    #[test]
    fn projects_answer_text_and_tool_references() {
        let response = json!({"outputs":[{"type":"message.output","content":[
            {"type":"text","text":"Grounded answer."},
            {"type":"tool_reference","tool":"web_search","title":"Source","url":"https://example.com","source":"A source."}
        ]}]});
        let (answer, rows) = parse_conversation_output(&response);
        assert_eq!(answer.as_deref(), Some("Grounded answer."));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].url, "https://example.com");
    }
}
