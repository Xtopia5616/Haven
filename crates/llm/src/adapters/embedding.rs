//! OpenAI-compatible embedding transport and response normalization.
//!
//! Provider adapters decide which endpoint and URL shape to use. This module
//! owns only the shared request/response contract for OpenAI-compatible
//! embedding endpoints.

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::types::{Embedding, LlmError, Usage};

use super::transport::send_request;

#[derive(Debug, Serialize)]
struct OpenAiEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedItem {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiEmbedUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedItem>,
    usage: Option<OpenAiEmbedUsage>,
    model: Option<String>,
}

/// OpenAI-compatible embeddings URL.
///
/// Chat Completions adapters concatenate `/embeddings` onto `base_url` as-is
/// (typically already ends in `/v1`). Responses adapters pass `ensure_v1`
/// so a host-only base (`https://api.deepseek.com`) still hits `/v1/embeddings`.
pub(crate) fn openai_embeddings_url(base_url: &str, ensure_v1: bool) -> String {
    let base = base_url.trim_end_matches('/');
    if ensure_v1 && !(base.ends_with("/v1") || base.ends_with("/v1beta")) {
        format!("{base}/v1/embeddings")
    } else {
        format!("{base}/embeddings")
    }
}

pub(crate) fn parse_openai_embed_response(
    body: &str,
    requested: usize,
    requested_model: &str,
) -> Result<Embedding, LlmError> {
    let json: OpenAiEmbedResponse =
        serde_json::from_str(body).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let mut items = json.data;
    if items.is_empty() {
        return Err(LlmError::InvalidResponse(
            "embeddings response missing data".into(),
        ));
    }
    if items.len() != requested {
        return Err(LlmError::InvalidResponse(format!(
            "embeddings count mismatch: requested {requested}, got {}",
            items.len()
        )));
    }
    items.sort_by_key(|item| item.index);
    let vectors: Vec<Vec<f32>> = items.into_iter().map(|item| item.embedding).collect();
    let model = json.model.clone().or(Some(requested_model.to_string()));
    let usage = json
        .usage
        .map(|u| {
            Usage::from_counts(
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                0,
                0,
                model.clone(),
            )
        })
        .unwrap_or_default();
    Ok(Embedding {
        vectors,
        model,
        usage,
    })
}

pub(crate) async fn openai_compatible_embed(
    client: &reqwest::Client,
    headers: HeaderMap,
    url: &str,
    model: &str,
    timeout_secs: u64,
    input: Vec<String>,
) -> Result<Embedding, LlmError> {
    if input.is_empty() {
        return Ok(Embedding {
            vectors: Vec::new(),
            model: Some(model.to_string()),
            usage: Usage::default(),
        });
    }
    let requested = input.len();
    let body = OpenAiEmbedRequest {
        model: model.to_string(),
        input,
    };
    tracing::debug!("POST {url} (model: {model})");
    tracing::debug!(
        "POST {url} request body: {} chars",
        serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
    );
    let mut req = client.post(url).headers(headers).json(&body);
    req = req.timeout(Duration::from_secs(timeout_secs));
    let resp = send_request(req, None).await?;
    let txt = resp
        .text()
        .await
        .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    tracing::trace!("POST {url} response body: {} chars", txt.len());
    parse_openai_embed_response(&txt, requested, model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_embeddings_url_chat_keeps_base() {
        assert_eq!(
            openai_embeddings_url("https://api.openai.com/v1", false),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            openai_embeddings_url("http://127.0.0.1:11434/v1/", false),
            "http://127.0.0.1:11434/v1/embeddings"
        );
    }

    #[test]
    fn openai_embeddings_url_responses_adds_v1() {
        assert_eq!(
            openai_embeddings_url("https://api.deepseek.com", true),
            "https://api.deepseek.com/v1/embeddings"
        );
        assert_eq!(
            openai_embeddings_url("https://api.openai.com/v1", true),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn parse_openai_embed_response_sorts_by_index() {
        let body = r#"{
            "data": [
                {"embedding": [3.0], "index": 1},
                {"embedding": [1.0, 2.0], "index": 0}
            ],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 4, "total_tokens": 4}
        }"#;
        let emb = parse_openai_embed_response(body, 2, "requested-model").unwrap();
        assert_eq!(emb.vectors, vec![vec![1.0, 2.0], vec![3.0]]);
        assert_eq!(emb.model.as_deref(), Some("text-embedding-3-small"));
        assert_eq!(emb.usage.prompt_tokens, 4);
    }

    #[test]
    fn parse_openai_embed_response_rejects_count_mismatch() {
        let body = r#"{"data":[{"embedding":[0.1],"index":0}]}"#;
        let err = parse_openai_embed_response(body, 2, "m").unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(msg) if msg.contains("count mismatch")));
    }
}
