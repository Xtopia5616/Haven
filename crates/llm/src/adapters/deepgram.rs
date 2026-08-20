//! Deepgram speech-to-text adapter (`POST /v1/listen`).
//!
//! STT-only: chat / stream / embed leave the [`LlmClient`] defaults
//! (`UnsupportedCapability`). Wired through `adapter_for` when
//! `api_style` / `provider` is `deepgram`.

use async_trait::async_trait;
use futures_util::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{build_client, send_request};
use crate::client::LlmClient;
use crate::types::{LlmError, LlmResponse, StreamChunk, SttResult};
use haven_common::config::ModelEndpoint;
use haven_common::types::CanonicalMessage;

pub struct DeepgramAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
}

impl DeepgramAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let client = build_client(&endpoint);
        Self { endpoint, client }
    }

    fn model(&self) -> &str {
        if self.endpoint.model_name.is_empty() {
            "nova-3"
        } else {
            &self.endpoint.model_name
        }
    }

    /// Percent-encode a query component (RFC 3986 unreserved). Avoids raw
    /// interpolation that would let `&` / `=` inject extra query params.
    fn encode_query_component(value: &str) -> String {
        let mut out = String::with_capacity(value.len());
        for b in value.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char);
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }

    fn auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        let key = self.endpoint.api_key.trim();
        if key.is_empty() {
            return headers;
        }
        // Normalize to `Token <key>`. Strip a legacy `Deepgram ` scheme if
        // present; keep an already-correct `Token ` prefix as-is.
        let value = if let Some(rest) = key
            .strip_prefix("Token ")
            .or_else(|| key.strip_prefix("token "))
        {
            format!("Token {rest}")
        } else if let Some(rest) = key
            .strip_prefix("Deepgram ")
            .or_else(|| key.strip_prefix("deepgram "))
        {
            format!("Token {rest}")
        } else {
            format!("Token {key}")
        };
        if let Ok(v) = HeaderValue::from_str(&value) {
            headers.insert(AUTHORIZATION, v);
        }
        headers
    }
}

#[async_trait]
impl LlmClient for DeepgramAdapter {
    fn style(&self) -> &'static str {
        "deepgram"
    }

    async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        Err(LlmError::UnsupportedCapability(
            "deepgram is speech-to-text only".into(),
        ))
    }

    async fn chat_stream(
        &self,
        _messages: Vec<CanonicalMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        Err(LlmError::UnsupportedCapability(
            "deepgram is speech-to-text only".into(),
        ))
    }

    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult, LlmError> {
        let base = if self.endpoint.base_url.trim().is_empty() {
            "https://api.deepgram.com".to_string()
        } else {
            self.endpoint.base_url.trim_end_matches('/').to_string()
        };
        let model = Self::encode_query_component(self.model());
        let url = format!("{base}/v1/listen?model={model}&smart_format=true");
        tracing::debug!("POST {url}");
        let mut req = self
            .client
            .post(&url)
            .headers(self.auth_headers())
            .header(CONTENT_TYPE, "audio/wav")
            .body(wav_data.to_vec());
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;
        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let json: serde_json::Value =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let alternative = &json["results"]["channels"][0]["alternatives"][0];
        let text = alternative["transcript"]
            .as_str()
            .ok_or_else(|| LlmError::InvalidResponse("Deepgram response missing transcript".into()))?
            .trim()
            .to_string();
        let confidence = alternative["confidence"].as_f64().map(|c| c as f32);
        Ok(SttResult { text, confidence })
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        // Deepgram has no lightweight models list; a missing key is enough to
        // mark the slot unconfigured at the router layer.
        if self.endpoint.api_key.trim().is_empty() {
            return Err(LlmError::Auth("deepgram api_key not configured".into()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_headers_prefix_raw_key() {
        let ep = ModelEndpoint {
            api_key: "abc".into(),
            ..Default::default()
        };
        let headers = DeepgramAdapter::new(ep).auth_headers();
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Token abc"
        );
    }

    #[test]
    fn auth_headers_keep_existing_token_prefix() {
        let ep = ModelEndpoint {
            api_key: "Token xyz".into(),
            ..Default::default()
        };
        let headers = DeepgramAdapter::new(ep).auth_headers();
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Token xyz"
        );
    }

    #[test]
    fn encode_query_component_neutralizes_param_injection() {
        let encoded = DeepgramAdapter::encode_query_component("nova-3&callback=https://evil.test");
        assert!(!encoded.contains('&'));
        assert!(!encoded.contains('='));
        assert!(encoded.contains("%26"));
        assert!(encoded.contains("%3D"));
        assert_eq!(DeepgramAdapter::encode_query_component("nova-3"), "nova-3");
    }

    #[test]
    fn auth_headers_normalize_deepgram_scheme_prefix() {
        let ep = ModelEndpoint {
            api_key: "Deepgram dg-key".into(),
            ..Default::default()
        };
        let headers = DeepgramAdapter::new(ep).auth_headers();
        assert_eq!(
            headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
            "Token dg-key"
        );
    }
}
