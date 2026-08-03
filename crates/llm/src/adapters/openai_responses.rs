use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{LineMode, spawn_line_reader};
use crate::client::{LlmClient, http_status_to_error};
use crate::types::{
    ContentPart, FinishReason, LlmError, LlmMessage, LlmResponse, LlmRole, StreamChunk, ToolCall,
    ToolDefinition, Usage,
};
use haven_common::config::ModelEndpoint;

// ---------------------------------------------------------------------------
// OpenAI Responses API request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    tool_type: String,
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Serialize)]
struct ResponsesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    input: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ResponsesTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesItem {
    #[serde(rename = "type")]
    item_type: Option<String>,
    #[serde(default)]
    content: Vec<ResponsesContentPart>,
    #[serde(default)]
    call_id: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ResponsesContentPart {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ResponsesUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Vec<ResponsesItem>,
    usage: Option<ResponsesUsage>,
    model: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

// Streaming SSE events (https://platform.openai.com/docs/api-reference/responses)
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ResponsesStreamEvent {
    #[serde(rename = "response.output_text.delta")]
    OutputTextDelta { delta: Option<String> },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgsDelta {
        item_id: Option<String>,
        delta: Option<String>,
    },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { item: Option<ResponsesItem> },
    #[serde(rename = "response.completed")]
    Completed {
        response: Option<ResponsesStreamResponse>,
    },
    #[serde(rename = "response.failed")]
    Failed {
        response: Option<ResponsesStreamResponse>,
    },
    #[serde(rename = "error")]
    Error { message: Option<String> },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ResponsesStreamResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    usage: Option<ResponsesUsage>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    error: Option<Value>,
}

/// OpenAI Responses API adapter (`/v1/responses`), for GPT-5 and other
/// models that only ship on the Responses protocol.
pub struct OpenAiResponsesAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
}

impl OpenAiResponsesAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let mut builder = reqwest::Client::builder();

        // §2.5: proxy support
        if let Some(ref proxy_url) = endpoint.proxy_url
            && let Ok(proxy) = reqwest::Proxy::all(proxy_url)
        {
            if let Some(ref no_proxy) = endpoint.no_proxy {
                let proxy = proxy.no_proxy(reqwest::NoProxy::from_string(no_proxy));
                builder = builder.proxy(proxy);
            } else {
                builder = builder.proxy(proxy);
            }
        }

        // §5.5: connection pool tuning
        builder = builder
            .pool_max_idle_per_host(5)
            .pool_idle_timeout(Duration::from_secs(90));

        let client = builder.build().unwrap_or_default();
        Self { endpoint, client }
    }

    fn build_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if !self.endpoint.api_key.is_empty() {
            // §2.15: customizable auth header name and prefix
            let auth = format!(
                "{} {}",
                self.endpoint.auth_header_prefix, self.endpoint.api_key
            );
            if let Ok(v) = HeaderValue::from_str(&auth) {
                let name = self
                    .endpoint
                    .auth_header_name
                    .parse::<reqwest::header::HeaderName>()
                    .unwrap_or(AUTHORIZATION);
                headers.insert(name, v);
            }
        }
        headers
    }

    fn responses_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/responses", base)
        } else {
            format!("{}/v1/responses", base)
        }
    }

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

    fn content_to_parts(parts: &[ContentPart]) -> Vec<Value> {
        parts
            .iter()
            .map(|p| match p {
                ContentPart::Text(t) => json!({"type": "input_text", "text": t}),
                ContentPart::Image {
                    media_type, data, ..
                } => json!({
                    "type": "input_image",
                    "image_url": format!("data:{};base64,{}", media_type, data)
                }),
                ContentPart::Audio {
                    media_type, data, ..
                } => json!({
                    "type": "input_audio",
                    "input_audio": {
                        "format": media_type.rsplit('/').next().unwrap_or("wav"),
                        "data": data
                    }
                }),
            })
            .collect()
    }

    /// Convert provider-neutral messages into Responses API input items.
    /// System prompts go to the top-level `instructions` field; assistant
    /// tool calls become standalone `function_call` items; tool results
    /// become `function_call_output` items.
    fn convert_input(msgs: Vec<LlmMessage>) -> (Vec<Value>, Option<String>) {
        let mut instructions: Vec<String> = Vec::new();
        let mut items: Vec<Value> = Vec::new();
        for m in msgs {
            match m.role {
                LlmRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            instructions.push(t.clone());
                        }
                    }
                }
                LlmRole::User => {
                    let content = Self::content_to_parts(&m.content);
                    if !content.is_empty() {
                        items.push(json!({"role": "user", "content": content}));
                    }
                }
                LlmRole::Assistant => {
                    let text = Self::text_content(&m.content);
                    if !text.is_empty() {
                        items.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        }));
                    }
                    if let Some(calls) = m.tool_calls {
                        for tc in calls {
                            items.push(json!({
                                "type": "function_call",
                                "call_id": tc.id,
                                "name": tc.name,
                                "arguments": tc.arguments
                            }));
                        }
                    }
                }
                LlmRole::Tool => {
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": m.tool_call_id.unwrap_or_default(),
                        "output": Self::text_content(&m.content)
                    }));
                }
            }
        }
        let instructions = if instructions.is_empty() {
            None
        } else {
            Some(instructions.join("\n\n"))
        };
        (items, instructions)
    }

    fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<ResponsesTool> {
        tools
            .into_iter()
            .map(|t| ResponsesTool {
                tool_type: t.tool_type,
                name: t.function.name,
                description: t.function.description,
                parameters: t.function.parameters,
            })
            .collect()
    }

    fn build_request_body(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> ResponsesRequest {
        let (input, instructions) = Self::convert_input(messages);
        let has_tools = !tools.is_empty();
        ResponsesRequest {
            model: self.endpoint.model_name.clone(),
            instructions,
            input,
            max_output_tokens: Some(self.endpoint.max_tokens),
            // o-series models reject temperature != 1; skip it when unset-ish.
            temperature: (self.endpoint.temperature != 1.0).then_some(self.endpoint.temperature),
            stream,
            tools: if has_tools {
                Some(Self::convert_tools(tools))
            } else {
                None
            },
            tool_choice: if has_tools { Some("auto".into()) } else { None },
        }
    }

    fn finish_reason_of(status: &str) -> Option<FinishReason> {
        match status {
            "completed" => Some(FinishReason::Stop),
            "incomplete" => Some(FinishReason::Length),
            "cancelled" => None,
            _ => None,
        }
    }

    fn parse_response(
        &self,
        json: ResponsesResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        for item in json.output {
            match item.item_type.as_deref() {
                Some("message") => {
                    for part in item.content {
                        if let Some(t) = part.text {
                            text.push_str(&t);
                        }
                    }
                }
                Some("function_call") => {
                    if let Some(name) = item.name {
                        tool_calls.push(ToolCall {
                            id: item.call_id.or(item.id).unwrap_or_default(),
                            name,
                            arguments: item.arguments.unwrap_or_default(),
                        });
                    }
                }
                Some("reasoning") => {
                    for part in item.content {
                        if let Some(t) = part.text {
                            reasoning.push_str(&t);
                        }
                    }
                }
                _ => {}
            }
        }
        if json.status.as_deref() == Some("failed") {
            let msg = json
                .error
                .map(|e| serde_json::to_string(&e).unwrap_or_default())
                .unwrap_or_else(|| "response failed".into());
            return Err(LlmError::RequestFailed(msg));
        }
        let usage = json
            .usage
            .map(|u| Usage {
                prompt_tokens: u.input_tokens,
                completion_tokens: u.output_tokens,
                total_tokens: u.total_tokens,
                model_name: model.clone(),
                cost: None,
            })
            .unwrap_or_default();
        Ok(LlmResponse {
            text,
            tool_calls,
            finish_reason: json.status.as_deref().and_then(Self::finish_reason_of),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
        })
    }

    async fn chat_inner(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body(messages, tools, stream);
        let url = self.responses_url();
        tracing::debug!("POST {} (model: {})", url, body.model);
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&body);
        // §2.9: per-request timeout for non-streaming
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = req.send().await.map_err(LlmError::from)?;

        if !resp.status().is_success() {
            let status = resp.status();
            // §2.3: extract Retry-After header before consuming body
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok().map(Duration::from_secs));
            let txt = resp.text().await.unwrap_or_default();
            return Err(http_status_to_error(status, &txt, retry_after));
        }

        let json: ResponsesResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_response(json, model)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body(messages, tools, true);
        let url = self.responses_url();
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

        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&body);
        // For streaming, only apply an HTTP-level timeout when explicitly configured.
        if let Some(timeout) = self.endpoint.timeout_streaming_secs {
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = req.send().await.map_err(LlmError::from)?;

        if !resp.status().is_success() {
            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok().map(Duration::from_secs));
            let txt = resp.text().await.unwrap_or_default();
            return Err(http_status_to_error(status, &txt, retry_after));
        }

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseDataOnly);

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<String>,
            done: bool,
            /// Function calls accumulated per item id; flushed in the final chunk.
            tool_calls: Vec<(String, ToolCall)>,
            accumulated_text: String,
            last_model: Option<String>,
            finish_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_completed: bool,
        }

        let empty_chunk = || StreamChunk {
            text: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            model: None,
            reasoning: None,
        };

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                tool_calls: Vec::new(),
                accumulated_text: String::new(),
                last_model: None,
                finish_reason: None,
                usage: None,
                saw_completed: false,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(d) => d,
                    None => {
                        let chunk = if !state.saw_completed && !state.accumulated_text.is_empty() {
                            Err(LlmError::StreamTruncated)
                        } else {
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: state.tool_calls.drain(..).map(|(_, tc)| tc).collect(),
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<ResponsesStreamEvent, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(ResponsesStreamEvent::OutputTextDelta { delta }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(d) = delta {
                            state.accumulated_text.push_str(&d);
                            chunk.text = Some(d);
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::FunctionCallArgsDelta { item_id, delta }) => {
                        if let (Some(id), Some(d)) = (item_id, delta)
                            && let Some((_, tc)) =
                                state.tool_calls.iter_mut().find(|(tid, _)| tid == &id)
                        {
                            tc.arguments.push_str(&d);
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::OutputItemAdded { item }) => {
                        if let Some(item) = item
                            && item.item_type.as_deref() == Some("function_call")
                            && let Some(id) = item.id.clone()
                        {
                            state.tool_calls.push((
                                id,
                                ToolCall {
                                    id: item.call_id.or(item.id).unwrap_or_default(),
                                    name: item.name.unwrap_or_default(),
                                    arguments: item.arguments.unwrap_or_default(),
                                },
                            ));
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::Completed { response }) => {
                        state.saw_completed = true;
                        if let Some(resp) = response {
                            if let Some(m) = &resp.model {
                                state.last_model = Some(m.clone());
                            }
                            if let Some(u) = resp.usage {
                                state.usage = Some(Usage {
                                    prompt_tokens: u.input_tokens,
                                    completion_tokens: u.output_tokens,
                                    total_tokens: u.total_tokens,
                                    model_name: state.last_model.clone(),
                                    cost: None,
                                });
                            }
                            if let Some(status) = resp.status.as_deref() {
                                state.finish_reason = Self::finish_reason_of(status);
                            }
                        }
                        state.done = true;
                        Some((
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: state.tool_calls.drain(..).map(|(_, tc)| tc).collect(),
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                            }),
                            state,
                        ))
                    }
                    Ok(ResponsesStreamEvent::Failed { response }) => {
                        let msg = response
                            .and_then(|r| r.error)
                            .map(|e| serde_json::to_string(&e).unwrap_or_default())
                            .unwrap_or_else(|| "response failed".into());
                        state.done = true;
                        Some((Err(LlmError::RequestFailed(msg)), state))
                    }
                    Ok(ResponsesStreamEvent::Error { message, .. }) => {
                        state.done = true;
                        let msg = message.unwrap_or_else(|| "stream error".into());
                        Some((Err(LlmError::RequestFailed(msg)), state))
                    }
                    Ok(ResponsesStreamEvent::Other) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
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
impl LlmClient for OpenAiResponsesAdapter {
    fn style(&self) -> &'static str {
        "openai-responses"
    }

    async fn chat(&self, messages: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, Vec::new(), false).await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools, false).await
    }

    async fn chat_stream(
        &self,
        messages: Vec<LlmMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner(messages, Vec::new()).await
    }

    async fn chat_stream_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner(messages, tools).await
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        let base = self.endpoint.base_url.trim_end_matches('/');
        let url = if base.ends_with("/v1") {
            format!("{}/models", base)
        } else {
            format!("{}/v1/models", base)
        };
        let resp = self
            .client
            .get(&url)
            .headers(self.build_headers())
            .timeout(Duration::from_secs(self.endpoint.timeout_secs.min(7)))
            .send()
            .await
            .map_err(LlmError::from)?;
        if resp.status().is_success() {
            Ok(())
        } else if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
            Err(LlmError::Auth(format!("status {}", resp.status())))
        } else {
            Err(LlmError::ServerError(format!("status {}", resp.status())))
        }
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
    fn responses_url_handles_v1_suffix() {
        let ep = ModelEndpoint {
            base_url: "https://api.openai.com/v1".into(),
            ..Default::default()
        };
        let client = OpenAiResponsesAdapter::new(ep);
        assert_eq!(
            client.responses_url(),
            "https://api.openai.com/v1/responses"
        );

        let ep = ModelEndpoint {
            base_url: "https://api.openai.com".into(),
            ..Default::default()
        };
        let client = OpenAiResponsesAdapter::new(ep);
        assert_eq!(
            client.responses_url(),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn convert_input_extracts_instructions() {
        let msgs = vec![
            LlmMessage {
                role: LlmRole::System,
                content: vec![ContentPart::text("you are helpful")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text("hello")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            },
        ];
        let (items, instructions) = OpenAiResponsesAdapter::convert_input(msgs);
        assert_eq!(instructions.as_deref(), Some("you are helpful"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[0]["content"][0]["type"], "input_text");
        assert_eq!(items[0]["content"][0]["text"], "hello");
    }

    #[test]
    fn convert_input_function_call_and_output() {
        let msgs = vec![
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![ContentPart::text("let me check")],
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "file".into(),
                    arguments: r#"{"operation":"read"}"#.into(),
                }]),
                reasoning: None,
            },
            LlmMessage {
                role: LlmRole::Tool,
                content: vec![ContentPart::text("result body")],
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
                reasoning: None,
            },
        ];
        let (items, _) = OpenAiResponsesAdapter::convert_input(msgs);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(items[0]["content"][0]["type"], "output_text");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["call_id"], "call_1");
        assert_eq!(items[1]["name"], "file");
        assert_eq!(items[2]["type"], "function_call_output");
        assert_eq!(items[2]["call_id"], "call_1");
        assert_eq!(items[2]["output"], "result body");
    }

    #[test]
    fn convert_input_image_and_audio() {
        let msgs = vec![LlmMessage {
            role: LlmRole::User,
            content: vec![
                ContentPart::Image {
                    content_type: "image_url".into(),
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                },
                ContentPart::Audio {
                    content_type: "input_audio".into(),
                    media_type: "audio/wav".into(),
                    data: "d3d3".into(),
                },
            ],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
        }];
        let (items, _) = OpenAiResponsesAdapter::convert_input(msgs);
        assert_eq!(items[0]["content"][0]["type"], "input_image");
        assert!(
            items[0]["content"][0]["image_url"]
                .as_str()
                .unwrap()
                .contains("data:image/png;base64,aGVsbG8=")
        );
        assert_eq!(items[0]["content"][1]["type"], "input_audio");
        assert_eq!(items[0]["content"][1]["input_audio"]["format"], "wav");
    }

    #[test]
    fn build_request_body_fields() {
        let ep = ModelEndpoint {
            model_name: "gpt-5".into(),
            max_tokens: 2048,
            temperature: 0.4,
            ..Default::default()
        };
        let client = OpenAiResponsesAdapter::new(ep);
        let body = client.build_request_body(
            vec![LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            }],
            Vec::new(),
            true,
        );
        assert_eq!(body.model, "gpt-5");
        assert_eq!(body.max_output_tokens, Some(2048));
        assert_eq!(body.temperature, Some(0.4));
        assert!(body.stream);
        assert!(body.tools.is_none());
    }

    #[test]
    fn build_request_body_skips_temperature_one() {
        let ep = ModelEndpoint {
            temperature: 1.0,
            ..Default::default()
        };
        let client = OpenAiResponsesAdapter::new(ep);
        let body = client.build_request_body(vec![], Vec::new(), false);
        assert_eq!(body.temperature, None);
    }

    #[test]
    fn build_request_body_with_tools() {
        let ep = ModelEndpoint::default();
        let client = OpenAiResponsesAdapter::new(ep);
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
        assert_eq!(body.tool_choice.as_deref(), Some("auto"));
    }

    #[test]
    fn parse_response_text_tool_call_usage() {
        let json = ResponsesResponse {
            status: Some("completed".into()),
            output: vec![
                ResponsesItem {
                    item_type: Some("message".into()),
                    content: vec![ResponsesContentPart {
                        text: Some("checking".into()),
                    }],
                    call_id: None,
                    id: Some("msg_1".into()),
                    name: None,
                    arguments: None,
                },
                ResponsesItem {
                    item_type: Some("function_call".into()),
                    content: vec![],
                    call_id: Some("call_1".into()),
                    id: Some("fc_1".into()),
                    name: Some("file".into()),
                    arguments: Some(r#"{"operation":"read"}"#.into()),
                },
            ],
            usage: Some(ResponsesUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            }),
            model: Some("gpt-5".into()),
            error: None,
        };
        let ep = ModelEndpoint::default();
        let client = OpenAiResponsesAdapter::new(ep);
        let resp = client.parse_response(json, Some("gpt-5".into())).unwrap();
        assert_eq!(resp.text, "checking");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].id, "call_1");
        assert_eq!(resp.tool_calls[0].name, "file");
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage.total_tokens, 15);
        assert_eq!(resp.model.as_deref(), Some("gpt-5"));
    }

    #[test]
    fn parse_response_failed_status_errors() {
        let json = ResponsesResponse {
            status: Some("failed".into()),
            output: vec![],
            usage: None,
            model: None,
            error: Some(json!({"code": "server_error", "message": "boom"})),
        };
        let ep = ModelEndpoint::default();
        let client = OpenAiResponsesAdapter::new(ep);
        let err = client.parse_response(json, None).unwrap_err();
        assert!(matches!(err, LlmError::RequestFailed(_)));
    }

    #[test]
    fn stream_events_parse() {
        let delta: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"Hello"}"#,
        )
        .unwrap();
        assert!(matches!(
            delta,
            ResponsesStreamEvent::OutputTextDelta { delta: Some(d) } if d == "Hello"
        ));

        let item: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_item.added","output_index":0,"item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"file","arguments":""}}"#,
        )
        .unwrap();
        if let ResponsesStreamEvent::OutputItemAdded { item: Some(item) } = item {
            assert_eq!(item.name.as_deref(), Some("file"));
        } else {
            panic!("expected output_item.added");
        }

        let args: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"op"}"#,
        )
        .unwrap();
        assert!(matches!(
            args,
            ResponsesStreamEvent::FunctionCallArgsDelta { delta: Some(d), .. } if d == "{\"op"
        ));

        let completed: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.completed","response":{"status":"completed","model":"gpt-5","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
        )
        .unwrap();
        assert!(matches!(completed, ResponsesStreamEvent::Completed { .. }));

        let other: ResponsesStreamEvent =
            serde_json::from_str(r#"{"type":"response.in_progress"}"#).unwrap();
        assert!(matches!(other, ResponsesStreamEvent::Other));
    }

    #[test]
    fn build_headers_default_auth_header() {
        let ep = ModelEndpoint {
            api_key: "sk-test".into(),
            ..Default::default()
        };
        let client = OpenAiResponsesAdapter::new(ep);
        let headers = client.build_headers();
        let val = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(val, "Bearer sk-test");
    }
}
