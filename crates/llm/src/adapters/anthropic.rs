use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    LineMode, build_client, build_headers, empty_chunk, health_check_request, send_request,
    spawn_line_reader,
};
use crate::client::LlmClient;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{FinishReason, LlmError, LlmResponse, StreamChunk, ToolDefinition, Usage};
use haven_common::config::ModelEndpoint;

// ---------------------------------------------------------------------------
// Anthropic Messages API request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct AnthropicMessage {
    role: String,
    content: Value,
}

#[derive(Debug, Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<AnthropicTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
    stream: bool,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<AnthropicResponseBlock>,
    #[serde(alias = "stop_reason")]
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponseBlock {
    #[serde(rename = "type")]
    block_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
struct AnthropicUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
}

// Streaming SSE events (https://docs.anthropic.com/en/api/messages-streaming)
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicStreamEvent {
    #[serde(rename = "message_start")]
    MessageStart {
        message: AnthropicStreamStartMessage,
    },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: usize,
        content_block: AnthropicStreamBlock,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta {
        index: usize,
        delta: AnthropicStreamDelta,
    },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: usize },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: AnthropicStreamDeltaMeta,
        usage: Option<AnthropicUsage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "error")]
    Error { error: AnthropicStreamError },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamStartMessage {
    model: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamBlock {
    #[serde(rename = "type")]
    block_type: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicStreamDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamDeltaMeta {
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicStreamError {
    #[serde(rename = "type")]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Anthropic Messages API adapter for Claude models.
pub struct AnthropicAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
}

impl AnthropicAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let client = build_client(&endpoint);
        Self { endpoint, client }
    }

    /// Anthropic authenticates with `x-api-key` (no Bearer prefix). If the
    /// user customized `auth_header_name`/`auth_header_prefix`, respect the
    /// custom scheme instead (for proxies that expect `Authorization: Bearer …`).
    fn build_headers(&self) -> HeaderMap {
        let mut headers = build_headers(&self.endpoint, "x-api-key", false);
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        headers
    }

    fn messages_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/messages", base)
        } else {
            format!("{}/v1/messages", base)
        }
    }

    fn models_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/models", base)
        } else {
            format!("{}/v1/models", base)
        }
    }

    /// Concatenate the text of a message's content parts (used for tool results).
    fn text_content(parts: &[ContentPart]) -> String {
        parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn content_to_blocks(parts: &[ContentPart]) -> Vec<Value> {
        let mut blocks = Vec::new();
        for p in parts {
            match p {
                ContentPart::Text(t) => {
                    blocks.push(json!({"type": "text", "text": t}));
                }
                ContentPart::Image {
                    media_type, data, ..
                } => {
                    blocks.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data
                        }
                    }));
                }
                ContentPart::Audio { .. } => {
                    tracing::warn!(
                        "Anthropic Messages API does not support audio input; dropping audio part"
                    );
                }
            }
        }
        blocks
    }

    /// Convert provider-neutral messages into Anthropic messages, extracting
    /// system prompts into the top-level `system` field (the API has no system
    /// role) and tool results into `tool_result` content blocks.
    fn convert_messages(msgs: Vec<CanonicalMessage>) -> (Vec<AnthropicMessage>, Option<String>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<AnthropicMessage> = Vec::new();
        for m in msgs {
            match m.role {
                CanonicalRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            system_parts.push(t.clone());
                        }
                    }
                }
                CanonicalRole::User => {
                    if m.tool_call_id.is_some() {
                        out.push(AnthropicMessage {
                            role: "user".into(),
                            content: json!([{
                                "type": "tool_result",
                                "tool_use_id": m.tool_call_id.unwrap_or_default(),
                                "content": Self::text_content(&m.content)
                            }]),
                        });
                    } else {
                        let blocks = Self::content_to_blocks(&m.content);
                        if blocks.is_empty() {
                            continue;
                        }
                        out.push(AnthropicMessage {
                            role: "user".into(),
                            content: Value::Array(blocks),
                        });
                    }
                }
                CanonicalRole::Assistant => {
                    let mut blocks = Self::content_to_blocks(&m.content);
                    if let Some(calls) = m.tool_calls {
                        for tc in calls {
                            blocks.push(json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.arguments
                            }));
                        }
                    }
                    if blocks.is_empty() {
                        continue;
                    }
                    out.push(AnthropicMessage {
                        role: "assistant".into(),
                        content: Value::Array(blocks),
                    });
                }
                CanonicalRole::Tool => {
                    out.push(AnthropicMessage {
                        role: "user".into(),
                        content: json!([{
                            "type": "tool_result",
                            "tool_use_id": m.tool_call_id.unwrap_or_default(),
                            "content": Self::text_content(&m.content)
                        }]),
                    });
                }
            }
        }
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        };
        (out, system)
    }

    fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<AnthropicTool> {
        tools
            .into_iter()
            .map(|t| AnthropicTool {
                name: t.function.name,
                description: t.function.description,
                input_schema: t.function.parameters,
            })
            .collect()
    }

    fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> AnthropicRequest {
        let (messages, system) = Self::convert_messages(messages);
        let has_tools = !tools.is_empty();
        AnthropicRequest {
            model: self.endpoint.model_name.clone(),
            max_tokens: self.endpoint.max_tokens,
            messages,
            system,
            temperature: Some(self.endpoint.temperature),
            top_p: self.endpoint.top_p,
            top_k: self.endpoint.top_k,
            stop_sequences: self.endpoint.stop.clone(),
            tools: if has_tools {
                Some(Self::convert_tools(tools))
            } else {
                None
            },
            tool_choice: if has_tools {
                Some(json!({"type": "auto"}))
            } else {
                None
            },
            stream,
        }
    }

    fn parse_response(
        &self,
        json: AnthropicResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        for block in json.content {
            match block.block_type.as_deref() {
                Some("text") => {
                    if let Some(t) = block.text {
                        text.push_str(&t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.thinking {
                        reasoning.push_str(&t);
                    }
                }
                Some("tool_use") => {
                    if let Some(name) = block.name {
                        tool_calls.push(CanonicalToolCall {
                            id: block.id.unwrap_or_default(),
                            name,
                            arguments: block.input.unwrap_or_default(),
                        });
                    }
                }
                _ => {}
            }
        }
        let usage = json
            .usage
            .map(|u| Usage {
                prompt_tokens: u.input_tokens,
                completion_tokens: u.output_tokens,
                total_tokens: u.input_tokens + u.output_tokens,
                model_name: model.clone(),
                cost: None,
            })
            .unwrap_or_default();
        Ok(LlmResponse {
            text,
            tool_calls,
            finish_reason: json
                .stop_reason
                .as_deref()
                .and_then(FinishReason::from_openai),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            web_search_calls: Vec::new(),
        })
    }

    async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body(messages, tools, stream);
        let url = self.messages_url();
        tracing::debug!("POST {} (model: {})", url, body.model);
        tracing::debug!(
            "POST {} request body: {} chars",
            url,
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&body);
        // §2.9: per-request timeout for non-streaming
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let json: AnthropicResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_response(json, model)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body(messages, tools, true);
        let url = self.messages_url();
        tracing::debug!(
            "chat_stream_inner: url={} model={} api_key={}",
            url,
            self.endpoint.model_name,
            if self.endpoint.api_key.is_empty() {
                "EMPTY"
            } else {
                "SET"
            },
        );
        tracing::trace!(
            "chat_stream_inner: POST {} request body: {} chars",
            url,
            serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
        );

        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&body);
        // For streaming, only apply an HTTP-level timeout when explicitly configured.
        if let Some(timeout) = self.endpoint.timeout_streaming_secs {
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = send_request(req).await?;

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseDataOnly);

        struct BlockState {
            kind: BlockKind,
            tool_id: String,
            tool_name: String,
            tool_input: String,
        }

        #[derive(PartialEq)]
        enum BlockKind {
            Text,
            Thinking,
            ToolUse,
        }

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<String>,
            done: bool,
            /// Per-content-block streaming state, indexed by Anthropic block index.
            blocks: Vec<BlockState>,
            accumulated_text: String,
            last_model: Option<String>,
            stop_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_message_stop: bool,
        }

        let empty_chunk = empty_chunk;

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                blocks: Vec::new(),
                accumulated_text: String::new(),
                last_model: None,
                stop_reason: None,
                usage: None,
                saw_message_stop: false,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(d) => d,
                    None => {
                        let chunk = if !state.saw_message_stop && !state.accumulated_text.is_empty()
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: Vec::new(),
                                finish_reason: state.stop_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: Vec::new(),
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<AnthropicStreamEvent, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(AnthropicStreamEvent::MessageStart { message }) => {
                        if let Some(m) = &message.model {
                            state.last_model = Some(m.clone());
                        }
                        if let Some(u) = message.usage {
                            state.usage = Some(Usage {
                                prompt_tokens: u.input_tokens,
                                completion_tokens: u.output_tokens,
                                total_tokens: u.input_tokens + u.output_tokens,
                                model_name: state.last_model.clone(),
                                cost: None,
                            });
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::ContentBlockStart {
                        index,
                        content_block,
                    }) => {
                        while state.blocks.len() <= index {
                            state.blocks.push(BlockState {
                                kind: BlockKind::Text,
                                tool_id: String::new(),
                                tool_name: String::new(),
                                tool_input: String::new(),
                            });
                        }
                        {
                            let block = &mut state.blocks[index];
                            match content_block.block_type.as_deref() {
                                Some("tool_use") => {
                                    block.kind = BlockKind::ToolUse;
                                    block.tool_id = content_block.id.unwrap_or_default();
                                    block.tool_name = content_block.name.unwrap_or_default();
                                    block.tool_input = String::new();
                                    // Some gateways send the full input in the
                                    // start event instead of `{}` + deltas.
                                    if let Some(input) = content_block.input {
                                        let s = serde_json::to_string(&input).unwrap_or_default();
                                        if s != "{}" {
                                            block.tool_input = s;
                                        }
                                    }
                                }
                                Some("thinking") => block.kind = BlockKind::Thinking,
                                _ => block.kind = BlockKind::Text,
                            }
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::ContentBlockDelta { index, delta }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        match delta {
                            AnthropicStreamDelta::TextDelta { text } => {
                                state.accumulated_text.push_str(&text);
                                chunk.text = Some(text);
                            }
                            AnthropicStreamDelta::ThinkingDelta { thinking } => {
                                chunk.reasoning = Some(thinking);
                            }
                            AnthropicStreamDelta::InputJsonDelta { partial_json } => {
                                if let Some(block) = state.blocks.get_mut(index) {
                                    block.tool_input.push_str(&partial_json);
                                }
                            }
                            AnthropicStreamDelta::Other => {}
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::ContentBlockStop { index }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(block) = state.blocks.get(index)
                            && block.kind == BlockKind::ToolUse
                        {
                            chunk.tool_calls.push(CanonicalToolCall {
                                id: block.tool_id.clone(),
                                name: block.tool_name.clone(),
                                arguments: CanonicalToolCall::from_wire_args(&block.tool_input),
                            });
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::MessageDelta { delta, usage }) => {
                        if let Some(sr) = delta.stop_reason.as_deref() {
                            state.stop_reason = FinishReason::from_openai(sr);
                        }
                        if let (Some(u), Some(existing)) = (usage, state.usage.as_mut()) {
                            existing.completion_tokens = u.output_tokens;
                            existing.total_tokens = existing.prompt_tokens + u.output_tokens;
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::MessageStop) => {
                        state.saw_message_stop = true;
                        state.done = true;
                        Some((
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: Vec::new(),
                                finish_reason: state.stop_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: Vec::new(),
                            }),
                            state,
                        ))
                    }
                    Ok(AnthropicStreamEvent::Ping) | Ok(AnthropicStreamEvent::Other) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(AnthropicStreamEvent::Error { error }) => {
                        let msg = error.message.unwrap_or_default();
                        state.done = true;
                        let err = match error.error_type.as_deref() {
                            Some("overloaded_error") | Some("api_error") => {
                                LlmError::ServerError(msg)
                            }
                            Some("rate_limit_error") => LlmError::RateLimit { retry_after: None },
                            _ => LlmError::RequestFailed(msg),
                        };
                        Some((Err(err), state))
                    }
                    Err(e) => Some((
                        Err(LlmError::InvalidResponse(format!("parse error: {}", e))),
                        state,
                    )),
                }
            },
        )
        .fuse();

        Ok(Box::pin(mapped))
    }
}

#[async_trait]
impl LlmClient for AnthropicAdapter {
    fn style(&self) -> &'static str {
        "anthropic"
    }

    async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, Vec::new(), false).await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools, false).await
    }

    async fn chat_stream(
        &self,
        messages: Vec<CanonicalMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner(messages, Vec::new()).await
    }

    async fn chat_stream_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner(messages, tools).await
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        health_check_request(
            &self.client,
            &self.models_url(),
            self.build_headers(),
            self.endpoint.timeout_secs,
        )
        .await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolFunction;

    #[test]
    fn build_headers_uses_x_api_key_by_default() {
        let ep = ModelEndpoint {
            api_key: "sk-ant-test".into(),
            ..Default::default()
        };
        let client = AnthropicAdapter::new(ep);
        let headers = client.build_headers();
        assert_eq!(
            headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-ant-test"
        );
        assert!(!headers.contains_key("authorization"));
        assert_eq!(
            headers.get("anthropic-version").unwrap().to_str().unwrap(),
            "2023-06-01"
        );
    }

    #[test]
    fn build_headers_respects_custom_auth_scheme() {
        let ep = ModelEndpoint {
            api_key: "sk-ant-test".into(),
            auth_header_name: "Authorization".into(),
            auth_header_prefix: "Bearer".into(),
            ..Default::default()
        };
        // "Bearer" is the default prefix, so it must still use x-api-key.
        let client = AnthropicAdapter::new(ep);
        let headers = client.build_headers();
        assert!(headers.get("x-api-key").is_some());

        let ep = ModelEndpoint {
            api_key: "sk-ant-test".into(),
            auth_header_name: "X-Gateway-Key".into(),
            auth_header_prefix: String::new(),
            ..Default::default()
        };
        let client = AnthropicAdapter::new(ep);
        let headers = client.build_headers();
        assert!(headers.get("x-gateway-key").is_some());
        assert!(headers.get("x-api-key").is_none());
    }

    #[test]
    fn build_headers_empty_api_key_skips_auth() {
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let headers = client.build_headers();
        assert!(headers.contains_key("content-type"));
        assert!(headers.get("x-api-key").is_none());
    }

    #[test]
    fn messages_url_handles_v1_suffix() {
        let ep = ModelEndpoint {
            base_url: "https://api.anthropic.com".into(),
            ..Default::default()
        };
        let client = AnthropicAdapter::new(ep);
        assert_eq!(
            client.messages_url(),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(client.models_url(), "https://api.anthropic.com/v1/models");

        let ep = ModelEndpoint {
            base_url: "https://api.anthropic.com/v1".into(),
            ..Default::default()
        };
        let client = AnthropicAdapter::new(ep);
        assert_eq!(
            client.messages_url(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn convert_messages_extracts_system_and_maps_roles() {
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("you are helpful")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let (out, system) = AnthropicAdapter::convert_messages(msgs);
        assert_eq!(system.as_deref(), Some("you are helpful"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content[0]["type"], "text");
        assert_eq!(out[0].content[0]["text"], "hello");
    }

    #[test]
    fn convert_messages_multiple_system_messages_joined() {
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("part one")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("part two")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
        ];
        let (_, system) = AnthropicAdapter::convert_messages(msgs);
        assert_eq!(system.as_deref(), Some("part one\n\npart two"));
    }

    #[test]
    fn convert_messages_tool_result_block() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("result body")],
            tool_call_id: Some("toolu_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, "user");
        assert_eq!(out[0].content[0]["type"], "tool_result");
        assert_eq!(out[0].content[0]["tool_use_id"], "toolu_1");
        assert_eq!(out[0].content[0]["content"], "result body");
    }

    #[test]
    fn convert_messages_assistant_tool_use_blocks() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("let me check")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "toolu_2".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        assert_eq!(out.len(), 1);
        let content = out[0].content.as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["id"], "toolu_2");
        assert_eq!(content[1]["name"], "file");
        assert_eq!(content[1]["input"]["operation"], "read");
    }

    #[test]
    fn convert_messages_image_part_becomes_base64_source() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::Image {
                content_type: "image_url".into(),
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        let content = out[0].content.as_array().unwrap();
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "aGVsbG8=");
    }

    #[test]
    fn convert_messages_empty_user_content_skipped() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        assert!(out.is_empty());
    }

    #[test]
    fn build_request_body_fields() {
        let ep = ModelEndpoint {
            model_name: "claude-sonnet-4-20250514".into(),
            max_tokens: 4096,
            temperature: 0.3,
            ..Default::default()
        };
        let client = AnthropicAdapter::new(ep);
        let body = client.build_request_body(
            vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            }],
            Vec::new(),
            true,
        );
        assert_eq!(body.model, "claude-sonnet-4-20250514");
        assert_eq!(body.max_tokens, 4096);
        assert_eq!(body.temperature, Some(0.3));
        assert!(body.stream);
        assert!(body.tools.is_none());
        assert!(body.system.is_none());
    }

    #[test]
    fn build_request_body_with_tools() {
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "search".into(),
                description: "search the web".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let body = client.build_request_body(vec![], tools, false);
        let tools = body.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].input_schema["type"], "object");
        assert_eq!(body.tool_choice, Some(json!({"type": "auto"})));
    }

    #[test]
    fn parse_response_text_and_usage() {
        let json = AnthropicResponse {
            content: vec![AnthropicResponseBlock {
                block_type: Some("text".into()),
                text: Some("hello there".into()),
                thinking: None,
                id: None,
                name: None,
                input: None,
            }],
            stop_reason: Some("end_turn".into()),
            usage: Some(AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
            }),
            model: Some("claude-3".into()),
        };
        let ep = ModelEndpoint {
            model_name: "claude-3".into(),
            ..Default::default()
        };
        let client = AnthropicAdapter::new(ep);
        let resp = client
            .parse_response(json, Some("claude-3".into()))
            .unwrap();
        assert_eq!(resp.text, "hello there");
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage.prompt_tokens, 10);
        assert_eq!(resp.usage.completion_tokens, 5);
        assert_eq!(resp.usage.total_tokens, 15);
        assert_eq!(resp.model.as_deref(), Some("claude-3"));
    }

    #[test]
    fn parse_response_tool_use_blocks() {
        let json = AnthropicResponse {
            content: vec![
                AnthropicResponseBlock {
                    block_type: Some("text".into()),
                    text: Some("calling tool".into()),
                    thinking: None,
                    id: None,
                    name: None,
                    input: None,
                },
                AnthropicResponseBlock {
                    block_type: Some("tool_use".into()),
                    text: None,
                    thinking: None,
                    id: Some("toolu_9".into()),
                    name: Some("file".into()),
                    input: Some(json!({"operation": "read", "path": "."})),
                },
            ],
            stop_reason: Some("tool_use".into()),
            usage: None,
            model: None,
        };
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let resp = client.parse_response(json, None).unwrap();
        assert_eq!(resp.text, "calling tool");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "toolu_9");
        assert_eq!(resp.tool_calls[0].name, "file");
        assert_eq!(resp.tool_calls[0].arguments["operation"], "read");
        assert_eq!(resp.finish_reason, Some(FinishReason::ToolCalls));
    }

    #[test]
    fn parse_response_thinking_becomes_reasoning() {
        let json = AnthropicResponse {
            content: vec![
                AnthropicResponseBlock {
                    block_type: Some("thinking".into()),
                    text: None,
                    thinking: Some("inner monologue".into()),
                    id: None,
                    name: None,
                    input: None,
                },
                AnthropicResponseBlock {
                    block_type: Some("text".into()),
                    text: Some("final answer".into()),
                    thinking: None,
                    id: None,
                    name: None,
                    input: None,
                },
            ],
            stop_reason: None,
            usage: None,
            model: None,
        };
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let resp = client.parse_response(json, None).unwrap();
        assert_eq!(resp.text, "final answer");
        assert_eq!(resp.reasoning.as_deref(), Some("inner monologue"));
    }

    #[test]
    fn stream_events_parse() {
        let start: AnthropicStreamEvent =
            serde_json::from_str(r#"{"type":"message_start","message":{"model":"claude-3","usage":{"input_tokens":10,"output_tokens":0}}}"#)
                .unwrap();
        assert!(matches!(start, AnthropicStreamEvent::MessageStart { .. }));

        let block_start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
        )
        .unwrap();
        assert!(matches!(
            block_start,
            AnthropicStreamEvent::ContentBlockStart { index: 0, .. }
        ));

        let tool_start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"file","input":{}}}"#,
        )
        .unwrap();
        if let AnthropicStreamEvent::ContentBlockStart { content_block, .. } = tool_start {
            assert_eq!(content_block.name.as_deref(), Some("file"));
        } else {
            panic!("expected tool_use block start");
        }

        let delta: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}"#,
        )
        .unwrap();
        assert!(matches!(
            delta,
            AnthropicStreamEvent::ContentBlockDelta {
                delta: AnthropicStreamDelta::TextDelta { text },
                ..
            } if text == "Hi"
        ));

        let msg_delta: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":15}}"#,
        )
        .unwrap();
        assert!(matches!(
            msg_delta,
            AnthropicStreamEvent::MessageDelta { .. }
        ));

        let stop: AnthropicStreamEvent =
            serde_json::from_str(r#"{"type":"message_stop"}"#).unwrap();
        assert!(matches!(stop, AnthropicStreamEvent::MessageStop));

        let unknown: AnthropicStreamEvent =
            serde_json::from_str(r#"{"type":"future_event","x":1}"#).unwrap();
        assert!(matches!(unknown, AnthropicStreamEvent::Other));
    }
}
