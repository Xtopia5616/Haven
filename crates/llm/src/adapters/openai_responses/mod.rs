use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::adapters::{
    LineMode, MAX_JSON_RESPONSE_BYTES, WebSearchMode, build_client, build_headers, empty_chunk,
    health_check_request, is_deepseek, normalize_web_search_call_item, read_text_bounded,
    reasoning_tail, reasoning_text_from_thinking_blocks, requires_reasoning_echo,
    resolve_web_search_mode, responses_output_config, responses_reasoning_config, send_request,
    spawn_line_reader, stream_header_timeout, upsert_web_search_call, web_search_result_of,
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
    ToolDefinition, Usage, WebSearchPhase, WebSearchUpdate,
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

/// OpenAI Responses API adapter (`/v1/responses`), for GPT-5 and other
/// models that only ship on the Responses protocol.
pub struct OpenAiResponsesAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    web_search_mode: WebSearchMode,
    prompt_cache_key_state: AtomicU8,
    developer_input_state: AtomicU8,
}

const PROMPT_CACHE_KEY_UNKNOWN: u8 = 0;
const PROMPT_CACHE_KEY_ENABLED: u8 = 1;
const PROMPT_CACHE_KEY_UNSUPPORTED: u8 = 2;
const DEVELOPER_INPUT_UNKNOWN: u8 = 0;
const DEVELOPER_INPUT_UNSUPPORTED: u8 = 1;

impl OpenAiResponsesAdapter {
    pub(super) fn try_new(endpoint: ModelEndpoint) -> Result<Self, LlmError> {
        let client = build_client(&endpoint)?;
        let web_search_mode = resolve_web_search_mode(&endpoint);
        Ok(Self {
            endpoint,
            client,
            web_search_mode,
            prompt_cache_key_state: AtomicU8::new(PROMPT_CACHE_KEY_UNKNOWN),
            developer_input_state: AtomicU8::new(DEVELOPER_INPUT_UNKNOWN),
        })
    }

    #[cfg(test)]
    pub(super) fn new(endpoint: ModelEndpoint) -> Self {
        Self::try_new(endpoint).expect("valid test endpoint")
    }

    pub(super) fn build_headers(&self) -> Result<HeaderMap, LlmError> {
        build_headers(&self.endpoint, "Authorization", true)
    }
}

#[async_trait]
impl LlmClient for OpenAiResponsesAdapter {
    fn style(&self) -> &'static str {
        "openai-responses"
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

    async fn chat_stream_with_tools_output_cap_shared_guidance(
        &self,
        messages: Arc<[CanonicalMessage]>,
        tools: Arc<[ToolDefinition]>,
        guidance: String,
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens_shared_guidance(
            messages.as_ref(),
            tools.as_ref(),
            Some(&guidance),
            max_output_tokens,
        )
        .await
    }

    async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
        // Chat uses `/v1/responses`; embeddings stay on the OpenAI-compatible
        // `/v1/embeddings` path (OpenAI, DeepSeek, and most gateways).
        super::openai_compatible_embed(
            &self.client,
            self.build_headers()?,
            &super::openai_embeddings_url(&self.endpoint.base_url, true),
            &self.endpoint.model_name,
            self.endpoint.timeout_secs,
            input,
        )
        .await
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        let base = self.endpoint.base_url.trim_end_matches('/');
        let url = if base.ends_with("/v1") {
            format!("{}/models", base)
        } else {
            format!("{}/v1/models", base)
        };
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
