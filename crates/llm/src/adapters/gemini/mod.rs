use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    LineMode, WebSearchMode, build_client, build_headers, empty_chunk, health_check_request,
    normalize_web_search_call_item, resolve_web_search_mode, send_request, spawn_line_reader,
    stream_header_timeout,
};
use crate::client::LlmClient;
use haven_common::CapabilityProfile;
#[cfg(test)]
use haven_common::CapabilitySupport;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    CacheAccounting, CacheDiagnostics, Embedding, FinishReason, LlmError, LlmResponse, StreamChunk,
    SttResult, ToolDefinition, Usage,
};
use base64::Engine;
use haven_common::config::ModelEndpoint;
#[cfg(test)]
use haven_common::prompts::SESSION_CONTEXT_FENCE_START;
use haven_common::prompts::{STT_SYSTEM_PROMPT, split_system_prompt_cache_sections};

mod features;
mod mapping;
mod request;
mod response;
mod stream;
mod wire;

#[allow(unused_imports)]
pub(super) use features::*;
#[allow(unused_imports)]
pub(super) use mapping::*;
#[allow(unused_imports)]
pub(super) use response::*;
#[allow(unused_imports)]
pub(super) use stream::*;
#[allow(unused_imports)]
pub(super) use wire::*;

/// Google Gemini API adapter (`generateContent` / `streamGenerateContent`).
pub struct GeminiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    web_search_mode: WebSearchMode,
}

impl GeminiAdapter {
    pub(super) fn try_new(endpoint: ModelEndpoint) -> Result<Self, LlmError> {
        let client = build_client(&endpoint)?;
        let web_search_mode = resolve_web_search_mode(&endpoint);
        Ok(Self {
            endpoint,
            client,
            web_search_mode,
        })
    }

    #[cfg(test)]
    pub(super) fn new(endpoint: ModelEndpoint) -> Self {
        Self::try_new(endpoint).expect("valid test endpoint")
    }

    pub(super) fn build_headers(&self) -> Result<HeaderMap, LlmError> {
        build_headers(&self.endpoint, "x-goog-api-key", false)
    }

    pub(super) async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, None).await
    }

    pub(super) async fn chat_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let cache_diagnostics = body.cache_diagnostics.clone();
        let url = self.generate_url();
        tracing::debug!("POST {} (model: {})", url, body.contents.len());
        tracing::debug!(
            "POST {} request body: {} chars",
            url,
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body);
        // §2.9: per-request timeout for non-streaming
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let raw: Value =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let web_search_calls = Self::web_search_calls_from_grounding(&raw);
        let json: GeminiResponse =
            serde_json::from_value(raw).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model_version.clone();
        let mut parsed = self.parse_response_with_cache(json, model, cache_diagnostics)?;
        parsed.web_search_calls = web_search_calls;
        Ok(parsed)
    }
}

#[async_trait]
impl LlmClient for GeminiAdapter {
    fn style(&self) -> &'static str {
        "gemini"
    }

    fn capability_profile(&self) -> CapabilityProfile {
        Self::wire_capability_profile()
    }

    async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, Vec::new()).await
    }

    async fn chat_with_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, Vec::new(), max_output_tokens)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools).await
    }

    async fn chat_with_tools_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, max_output_tokens)
            .await
    }

    async fn chat_stream(
        &self,
        messages: Vec<CanonicalMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner(messages, Vec::new()).await
    }

    async fn chat_stream_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens(messages, Vec::new(), max_output_tokens)
            .await
    }

    async fn chat_stream_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner(messages, tools).await
    }

    async fn chat_stream_with_tools_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens(messages, tools, max_output_tokens)
            .await
    }

    async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
        if input.is_empty() {
            return Ok(Embedding {
                vectors: Vec::new(),
                model: Some(self.endpoint.model_name.clone()),
                usage: Usage::default(),
            });
        }
        let model_resource = format!("models/{}", self.embed_model_id());
        let requests: Vec<Value> = input
            .iter()
            .map(|text| {
                json!({
                    "model": model_resource,
                    "content": { "parts": [{ "text": text }] }
                })
            })
            .collect();
        let body = json!({ "requests": requests });
        let url = self.embed_url();
        tracing::debug!("POST {} (embed model: {})", url, self.endpoint.model_name);
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body);
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;
        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        parse_gemini_embed_response(&txt, input.len(), &self.endpoint.model_name)
    }

    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult, LlmError> {
        let data = base64::engine::general_purpose::STANDARD.encode(wav_data);
        let body = json!({
            "contents": [{
                "role": "user",
                "parts": [
                    { "text": STT_SYSTEM_PROMPT },
                    {
                        "inline_data": {
                            "mime_type": "audio/wav",
                            "data": data
                        }
                    }
                ]
            }]
        });
        let url = self.generate_url();
        tracing::debug!("POST {} (stt model: {})", url, self.endpoint.model_name);
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body);
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;
        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let json: Value =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let text = json["candidates"][0]["content"]["parts"]
            .as_array()
            .and_then(|parts| parts.iter().find_map(|p| p["text"].as_str()))
            .ok_or_else(|| LlmError::InvalidResponse("Gemini STT response missing text".into()))?
            .trim()
            .to_string();
        Ok(SttResult {
            text,
            confidence: None,
            usage: None,
            model: None,
        })
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        health_check_request(
            &self.client,
            &self.models_url(),
            self.build_headers()?,
            self.endpoint.timeout_secs,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
