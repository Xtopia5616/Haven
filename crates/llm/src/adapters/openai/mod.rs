use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::adapters::{
    LineMode, MAX_JSON_RESPONSE_BYTES, WebSearchMode, build_client, build_headers,
    chat_thinking_extras, health_check_request, is_deepseek, normalize_web_search_call_item,
    read_text_bounded, reasoning_tail, reasoning_text_from_thinking_blocks,
    requires_reasoning_echo, resolve_web_search_mode, send_request, spawn_line_reader,
    stream_header_timeout, xai_search_mode,
};
use crate::client::LlmClient;
use haven_common::CapabilityProfile;
#[cfg(test)]
use haven_common::CapabilitySupport;
use haven_common::prompts::split_system_prompt_cache_sections;
#[cfg(test)]
use haven_common::prompts::{MEMORY_FENCE_START, SESSION_CONTEXT_FENCE_START};
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    CacheAccounting, CacheDiagnostics, Embedding, FinishReason, LlmError, LlmResponse, StreamChunk,
    SttResult, ToolDefinition, Usage,
};
use haven_common::config::ModelEndpoint;

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

/// OpenAI-compatible chat adapter: the common wire format spoken by OpenAI,
/// Ollama, vLLM, DeepSeek, llama.cpp, xAI Grok, and most third-party gateways.
///
/// When constructed via [`Self::new_with_style`] with `"xai"`, request bodies
/// may include xAI Live Search `search_parameters` driven by the endpoint's
/// `web_search` mode (`off` | `auto` | `always`).
pub struct OpenAiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    /// Reported `LlmClient::style()` — `"openai-chat"` (default) or `"xai"`.
    style: &'static str,
    web_search_mode: WebSearchMode,
    /// Whether this endpoint accepts OpenAI's optional `prompt_cache_key`.
    /// A gateway rejection is remembered for this adapter so we only pay one
    /// compatibility retry instead of failing every request.
    prompt_cache_key_state: AtomicU8,
}

const PROMPT_CACHE_KEY_UNKNOWN: u8 = 0;
const PROMPT_CACHE_KEY_ENABLED: u8 = 1;
const PROMPT_CACHE_KEY_UNSUPPORTED: u8 = 2;

impl OpenAiAdapter {
    pub fn try_new(endpoint: ModelEndpoint) -> Result<Self, LlmError> {
        Self::try_new_with_style(endpoint, "openai-chat")
    }

    pub fn try_new_with_style(
        endpoint: ModelEndpoint,
        style: &'static str,
    ) -> Result<Self, LlmError> {
        let client = build_client(&endpoint)?;
        let web_search_mode = resolve_web_search_mode(&endpoint);
        Ok(Self {
            endpoint,
            client,
            style,
            web_search_mode,
            prompt_cache_key_state: AtomicU8::new(PROMPT_CACHE_KEY_UNKNOWN),
        })
    }

    #[cfg(test)]
    pub(super) fn new(endpoint: ModelEndpoint) -> Self {
        Self::try_new(endpoint).expect("valid test endpoint")
    }

    #[cfg(test)]
    pub(super) fn new_with_style(endpoint: ModelEndpoint, style: &'static str) -> Self {
        Self::try_new_with_style(endpoint, style).expect("valid test endpoint")
    }

    pub(super) fn build_headers(&self) -> Result<HeaderMap, LlmError> {
        build_headers(&self.endpoint, "Authorization", true)
    }
}

#[async_trait]
impl LlmClient for OpenAiAdapter {
    fn style(&self) -> &'static str {
        self.style
    }

    fn capability_profile(&self) -> CapabilityProfile {
        Self::wire_capability_profile()
    }

    async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, Vec::new(), false).await
    }

    async fn chat_with_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, Vec::new(), false, max_output_tokens)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools, false).await
    }

    async fn chat_with_tools_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, false, max_output_tokens)
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

    async fn chat_stream_with_tools_output_cap_shared(
        &self,
        messages: Arc<[CanonicalMessage]>,
        tools: Arc<[ToolDefinition]>,
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens_shared(
            messages.as_ref(),
            tools.as_ref(),
            max_output_tokens,
        )
        .await
    }

    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult, LlmError> {
        // Native `/audio/transcriptions` only for dedicated ASR models.
        // Multimodal chat models (e.g. gpt-4o-audio-preview) return
        // Unsupported so the router can fall back to chat + `input_audio`.
        if !is_whisper_model(&self.endpoint.model_name) {
            return Err(LlmError::UnsupportedCapability(format!(
                "model '{}' is not a native /audio/transcriptions model",
                self.endpoint.model_name
            )));
        }
        let form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(wav_data.to_vec())
                    .file_name("audio.wav")
                    .mime_str("audio/wav")
                    .map_err(|e| LlmError::RequestFailed(e.to_string()))?,
            )
            .text("model", self.endpoint.model_name.clone())
            .text("response_format", "json");

        let url = format!(
            "{}/audio/transcriptions",
            self.endpoint.base_url.trim_end_matches('/')
        );
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            model = %self.endpoint.model_name,
            request_kind = "transcription",
            "POST provider endpoint"
        );
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .multipart(form);
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;
        let txt = read_text_bounded(resp, MAX_JSON_RESPONSE_BYTES).await?;
        let json: serde_json::Value =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let text = json["text"]
            .as_str()
            .ok_or_else(|| LlmError::InvalidResponse("Whisper response missing 'text'".into()))?
            .trim()
            .to_string();
        Ok(SttResult {
            text,
            confidence: None,
            usage: None,
            model: None,
        })
    }

    async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
        super::openai_compatible_embed(
            &self.client,
            self.build_headers()?,
            &super::openai_embeddings_url(&self.endpoint.base_url, false),
            &self.endpoint.model_name,
            self.endpoint.timeout_secs,
            input,
        )
        .await
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        let url = format!("{}/models", self.endpoint.base_url.trim_end_matches('/'));
        health_check_request(
            &self.client,
            &url,
            self.build_headers()?,
            self.endpoint.timeout_secs,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
