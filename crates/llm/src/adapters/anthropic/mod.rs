use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    LineMode, MAX_JSON_RESPONSE_BYTES, WebSearchMode, build_client, build_headers, empty_chunk,
    health_check_request, normalize_web_search_call_item, read_text_bounded,
    resolve_web_search_mode, send_request, spawn_line_reader, stream_header_timeout,
};
use crate::client::LlmClient;
use haven_common::CapabilityProfile;
#[cfg(test)]
use haven_common::CapabilitySupport;
use haven_common::prompts::split_system_prompt_cache_sections;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    CacheAccounting, CacheDiagnostics, FinishReason, LlmError, LlmResponse, StreamChunk,
    ToolDefinition, Usage,
};
use haven_common::config::ModelEndpoint;

/// Anthropic server-side web search tool type id (Messages API).
const ANTHROPIC_WEB_SEARCH_TOOL_TYPE: &str = "web_search_20250305";

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

/// Anthropic Messages API adapter for Claude models.
pub struct AnthropicAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    web_search_mode: WebSearchMode,
}

impl AnthropicAdapter {
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
        let mut headers = build_headers(&self.endpoint, "x-api-key", false)?;
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        Ok(headers)
    }

    pub(super) async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, stream, None)
            .await
    }

    pub(super) async fn chat_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            stream,
            self.web_search_mode,
            max_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let url = self.messages_url();
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            model = %body.model,
            request_kind = "chat",
            "POST provider endpoint"
        );
        tracing::debug!(
            endpoint = %crate::client::endpoint_log_location(&url),
            "POST provider request body: {} chars",
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

        let txt = read_text_bounded(resp, MAX_JSON_RESPONSE_BYTES).await?;
        tracing::trace!("provider response body: {} chars", txt.len());
        let json: AnthropicResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_response_with_cache(json, model, body.cache_diagnostics)
    }
}

#[async_trait]
impl LlmClient for AnthropicAdapter {
    fn style(&self) -> &'static str {
        "anthropic"
    }

    fn capability_profile(&self) -> CapabilityProfile {
        Self::wire_capability_profile()
    }

    fn validate_content(&self, messages: &[CanonicalMessage]) -> Result<(), LlmError> {
        Self::validate_provider_content(messages)
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
