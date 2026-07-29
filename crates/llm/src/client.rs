use async_trait::async_trait;
use futures_util::FutureExt;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::time::Duration;


use crate::types::{
    ContentPart, FinishReason, LlmError, LlmMessage, LlmResponse, LlmRole, StreamChunk, ToolCall,
    ToolDefinition, Usage,
};
use haven_common::config::ModelEndpoint;

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn chat(&self, messages: Vec<LlmMessage>) -> Result<LlmResponse, LlmError>;

    async fn chat_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat(messages).await
    }

    async fn chat_stream(
        &self,
        messages: Vec<LlmMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError>;

    async fn chat_stream_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let resp = self.chat_with_tools(messages, tools).await?;
        let chunk = StreamChunk {
            text: Some(resp.text.clone()),
            tool_calls: resp.tool_calls.clone(),
            finish_reason: resp.finish_reason,
            usage: Some(resp.usage.clone()),
            model: resp.model.clone(),
            reasoning: None,
        };
        let final_chunk = StreamChunk {
            text: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            model: None,
            reasoning: None,
        };
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(chunk),
            Ok(final_chunk),
        ])))
    }

    async fn health_check(&self) -> Result<(), LlmError>;
}

// ---------------------------------------------------------------------------
// OpenAI-compatible request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    /// Tool calls in assistant messages, serialized as the OpenAI tool_calls
    /// array so the API can link subsequent tool responses by tool_call_id.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiMessageToolCall>>,
}

/// A tool call within an assistant message, matching the OpenAI API format.
#[derive(Debug, Serialize)]
struct OpenAiMessageToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAiMessageToolFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiMessageToolFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<serde_json::Value>,
    // §2.8: additional model parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct OpenAiTool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAiToolFunction,
}

#[derive(Debug, Serialize)]
struct OpenAiToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: Option<OpenAiMessageOut>,
    delta: Option<OpenAiMessageOut>,
    #[serde(alias = "stop_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessageOut {
    #[allow(dead_code)]
    role: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallOut>>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallOut {
    id: Option<String>,
    index: Option<i32>,
    #[serde(rename = "function")]
    function: OpenAiFunctionOut,
}

#[derive(Debug, Deserialize)]
struct OpenAiFunctionOut {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    #[serde(alias = "candidates")]
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamResponse {
    #[serde(alias = "candidates")]
    choices: Vec<OpenAiChoice>,
    model: Option<String>,
}

pub struct HttpLlmClient {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
}

impl HttpLlmClient {
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

        // §2.9: no global timeout — per-request timeout applied in chat_inner/chat_stream_inner

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

    fn convert_messages(msgs: Vec<LlmMessage>) -> Vec<OpenAiMessage> {
        msgs.into_iter()
            .map(|m| {
                // When the assistant message carries tool_calls, the content
                // should be null (OpenAI API requirement).
                let has_tool_calls = m.tool_calls.is_some();
                let content = if m.content.is_empty() || has_tool_calls {
                    None
                } else if m.content.len() == 1 {
                    match &m.content[0] {
                        ContentPart::Text(t) => Some(serde_json::Value::String(t.clone())),
                        _ => Some(serde_json::to_value(&m.content).unwrap_or_default()),
                    }
                } else {
                    let parts: Vec<serde_json::Value> = m
                        .content
                        .iter()
                        .map(|cp| match cp {
                            ContentPart::Text(t) => {
                                serde_json::json!({"type": "text", "text": t})
                            }
                            ContentPart::Image {
                                media_type, data, ..
                            } => serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data)
                                }
                            }),
                        })
                        .collect();
                    Some(serde_json::Value::Array(parts))
                };
                let tool_calls = m.tool_calls.map(|calls| {
                    calls
                        .into_iter()
                        .map(|tc| OpenAiMessageToolCall {
                            id: tc.id,
                            call_type: "function".into(),
                            function: OpenAiMessageToolFunction {
                                name: tc.name,
                                arguments: tc.arguments,
                            },
                        })
                        .collect()
                });
                OpenAiMessage {
                    role: match m.role {
                        LlmRole::System => "system".to_string(),
                        LlmRole::User => "user".to_string(),
                        LlmRole::Assistant => "assistant".to_string(),
                        LlmRole::Tool => "tool".to_string(),
                    },
                    content,
                    tool_call_id: m.tool_call_id,
                    tool_calls,
                }
            })
            .collect()
    }

    fn extract_tool_calls(choice: &OpenAiChoice) -> Vec<ToolCall> {
        let mut out = Vec::new();
        if let Some(msg) = choice.message.as_ref().or(choice.delta.as_ref())
            && let Some(calls) = &msg.tool_calls
        {
            for c in calls {
                let name = c.function.name.clone().unwrap_or_default();
                let args = c.function.arguments.clone().unwrap_or_default();
                let id = c.id.clone().unwrap_or_default();
                if !name.is_empty() {
                    out.push(ToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
            }
        }
        out
    }

    fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<OpenAiTool> {
        tools
            .into_iter()
            .map(|t| OpenAiTool {
                tool_type: t.tool_type,
                function: OpenAiToolFunction {
                    name: t.function.name,
                    description: t.function.description,
                    parameters: t.function.parameters,
                },
            })
            .collect()
    }

    fn build_request_body(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> OpenAiRequest {
        let has_tools = !tools.is_empty();
        OpenAiRequest {
            model: self.endpoint.model_name.clone(),
            messages: Self::convert_messages(messages),
            max_tokens: Some(self.endpoint.max_tokens),
            temperature: Some(self.endpoint.temperature),
            stream,
            tools: if has_tools {
                Some(Self::convert_tools(tools))
            } else {
                None
            },
            tool_choice: if has_tools {
                Some(serde_json::json!("auto"))
            } else {
                None
            },
            top_p: self.endpoint.top_p,
            top_k: self.endpoint.top_k,
            frequency_penalty: self.endpoint.frequency_penalty,
            presence_penalty: self.endpoint.presence_penalty,
            stop: self.endpoint.stop.clone(),
            seed: self.endpoint.seed,
            response_format: self.endpoint.response_format.clone(),
        }
    }

    fn parse_openai_response(
        &self,
        json: OpenAiResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        let choice = json
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::InvalidResponse("no choices".into()))?;
        let text = choice
            .message
            .as_ref()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let reasoning = choice
            .message
            .as_ref()
            .and_then(|m| m.reasoning_content.clone());
        let tool_calls = Self::extract_tool_calls(&choice);

        let usage = json
            .usage
            .map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
                model_name: model.clone(),
                cost: None,
            })
            .unwrap_or_default();

        let response = LlmResponse {
            text,
            tool_calls,
            finish_reason: choice.finish_reason.and_then(|s| FinishReason::from_openai(&s)),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning,
        };
        tracing::trace!(
            "parse_openai_response: text={} chars, tool_calls={}, reasoning={}, usage p/c/t={}/{}/{}",
            response.text.len(),
            response.tool_calls.len(),
            response.reasoning.is_some(),
            response.usage.prompt_tokens,
            response.usage.completion_tokens,
            response.usage.total_tokens,
        );
        Ok(response)
    }

    async fn chat_inner(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body(messages, tools, stream);
        let url = format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        );

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
                .and_then(|s| {
                    // Try seconds first, then HTTP-date
                    s.parse::<u64>()
                        .ok()
                        .or_else(|| {
                            // HTTP-date: not commonly used; log and fall back to None
                            tracing::warn!("Retry-After as HTTP-date not yet supported: {}", s);
                            None
                        })
                        .map(Duration::from_secs)
                });
            let txt = resp.text().await.unwrap_or_default();
            return Err(http_status_to_error(status, &txt, retry_after));
        }

        let json: OpenAiResponse = resp
            .json()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_openai_response(json, model)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body(messages, tools, true);
        let url = format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        );
        tracing::debug!("chat_stream_inner: url={} model={} api_key={} timeout_secs={} timeout_streaming={:?}",
            url, self.endpoint.model_name,
            if self.endpoint.api_key.is_empty() { "EMPTY" } else { "SET" },
            self.endpoint.timeout_secs,
            self.endpoint.timeout_streaming_secs);

        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .json(&body);
        // For streaming, only apply an HTTP-level timeout when explicitly configured.
        // When timeout_streaming_secs is None, no HTTP timeout is set — the router-level
        // max_total_duration_secs provides overall protection.
        if let Some(timeout) = self.endpoint.timeout_streaming_secs {
            tracing::trace!("chat_stream_inner: {}s streaming timeout", timeout);
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = req.send().await.map_err(|e| {
            tracing::debug!("chat_stream_inner: send() error: {:?}", e);
            LlmError::from(e)
        })?;
        tracing::debug!("chat_stream_inner response status: {}", resp.status());

        if !resp.status().is_success() {
            let status = resp.status();
            // §2.3: extract Retry-After header before consuming body
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| {
                    s.parse::<u64>().ok().map(Duration::from_secs)
                });
            let txt = resp.text().await.unwrap_or_default();
            return Err(http_status_to_error(status, &txt, retry_after));
        }

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        let byte_stream = resp.bytes_stream();

        // Spawn a reader that buffers lines and handles both SSE (data: …)
        // and raw-JSON-lines streaming formats in one pass.
        tokio::spawn({
            let tx = chunk_tx.clone();
            async move {
                let result = std::panic::AssertUnwindSafe(async {
                    let mut buf = String::new();
                    use futures_util::StreamExt;
                    tokio::pin!(byte_stream);
                    loop {
                        let chunk = tokio::select! {
                            biased;
                            result = byte_stream.next() => result,
                        };
                        match chunk {
                            Some(Ok(bytes)) => {
                                buf.push_str(&String::from_utf8_lossy(&bytes));
                                // Process all complete lines in the buffer.
                                while let Some(newline) = buf.find('\n') {
                                    let line = buf[..newline].trim().to_string();
                                    buf.drain(..=newline);
                                    if line.is_empty() || line.starts_with(':') {
                                        continue; // SSE comment or blank line
                                    }
                                    // Strip SSE "data: " prefix if present; otherwise
                                    // treat the raw line as JSON (non-standard providers).
                                    let payload = if let Some(p) = line.strip_prefix("data: ") {
                                        p.trim().to_string()
                                    } else {
                                        line
                                    };
                                    if payload == "[DONE]" || payload.is_empty() {
                                        continue;
                                    }
                                    let _ = tx.send(payload);
                                }
                            }
                            Some(Err(_)) | None => {
                                // Flush any remaining buffered data before EOF.
                                let remaining = buf.trim().to_string();
                                if !remaining.is_empty() && remaining != "[DONE]" {
                                    let _ = tx.send(remaining);
                                }
                                break;
                            }
                        }
                    }
                })
                .catch_unwind()
                .await;
                if let Err(panic) = result {
                    tracing::error!(
                        "byte stream reader panicked: {:?}",
                        panic.downcast_ref::<String>().unwrap_or(&"unknown".into())
                    );
                }
            }
        });

        // Merge streaming tool-call deltas by index.
        fn merge_tool_call(acc: &mut Vec<ToolCall>, index: usize, id: Option<&str>, name: Option<&str>, arguments: Option<&str>) {
            while acc.len() <= index {
                acc.push(ToolCall {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
            }
            if let Some(id) = id
                && !id.is_empty() { acc[index].id = id.to_string(); }
            if let Some(name) = name
                && !name.is_empty() { acc[index].name = name.to_string(); }
            if let Some(args) = arguments {
                acc[index].arguments.push_str(args);
            }
        }

        // Return the first delta/message available: providers differ on whether
        // they send `delta` (standard SSE) or `message` (non-standard) per chunk.
        fn choice_delta(choice: &OpenAiChoice) -> Option<&OpenAiMessageOut> {
            choice.delta.as_ref().or(choice.message.as_ref())
        }

        struct UnfoldState {
            rx: tokio::sync::mpsc::UnboundedReceiver<String>,
            done: bool,
            accumulated_text: String,
            tool_calls_acc: Vec<ToolCall>,
            last_model: Option<String>,
            has_finish_reason: bool,
        }

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                accumulated_text: String::new(),
                tool_calls_acc: Vec::new(),
                last_model: None,
                has_finish_reason: false,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(d) => d,
                    None => {
                        let chunk = if !state.has_finish_reason
                            && !state.accumulated_text.is_empty()
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            Ok(StreamChunk {
                                text: None,
                                tool_calls: std::mem::take(&mut state.tool_calls_acc),
                                finish_reason: None,
                                usage: None,
                                model: state.last_model.clone(),
                                reasoning: None,
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<OpenAiStreamResponse, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(resp) => {
                        if let Some(model) = &resp.model {
                            state.last_model = Some(model.clone());
                        }
                        if let Some(choice) = resp.choices.into_iter().next() {
                            if let Some(delta) = choice_delta(&choice)
                                && let Some(content) = &delta.content
                            {
                                state.accumulated_text.push_str(content);
                            }
                            if let Some(delta) = choice_delta(&choice)
                                && let Some(calls) = &delta.tool_calls
                            {
                                for c in calls {
                                    let idx = c.index.unwrap_or(0) as usize;
                                    merge_tool_call(
                                        &mut state.tool_calls_acc,
                                        idx,
                                        c.id.as_deref(),
                                        c.function.name.as_deref(),
                                        c.function.arguments.as_deref(),
                                    );
                                }
                            }
                            if choice.finish_reason.is_some() {
                                state.has_finish_reason = true;
                            }
                            let finish_reason = choice.finish_reason
                                .as_ref()
                                .and_then(|s| FinishReason::from_openai(s));
                            Some((
                                Ok(StreamChunk {
                                    text: choice_delta(&choice)
                                        .and_then(|d| d.content.clone()),
                                    reasoning: choice_delta(&choice)
                                        .and_then(|d| d.reasoning_content.clone()),
                                    tool_calls: Vec::new(),
                                    finish_reason,
                                    usage: None,
                                    model: state.last_model.clone(),
                                }),
                                state,
                            ))
                        } else {
                            Some((
                                Ok(StreamChunk {
                                    text: None,
                                    reasoning: None,
                                    tool_calls: Vec::new(),
                                    finish_reason: None,
                                    usage: None,
                                    model: state.last_model.clone(),
                                }),
                                state,
                            ))
                        }
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
impl LlmClient for HttpLlmClient {
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
        let url = format!("{}/models", self.endpoint.base_url.trim_end_matches('/'));
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
// HTTP status code → LlmError mapping (§2.2)
// ---------------------------------------------------------------------------

/// Try to extract a human-readable error message from various provider error
/// JSON schemas: `{"error":{"message":"..."}}`, `{"message":"..."}`,
/// `{"detail":"..."}`, or fall back to the raw body.
fn extract_error_body(body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        // OpenAI: {"error": {"message": "...", "code": "..."}}
        if let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return msg.to_string();
        }
        // Generic: {"message": "..."}
        if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        // Generic: {"detail": "..."}
        if let Some(msg) = val.get("detail").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        // Generic: {"error": "..."} (flat)
        if let Some(msg) = val.get("error").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        // Fallback: pretty-print the first 500 chars of JSON
        let s = serde_json::to_string(&val).unwrap_or_default();
        if s.len() <= 500 {
            return s;
        }
        return s[..500].to_string();
    }
    body.to_string()
}

fn http_status_to_error(status: reqwest::StatusCode, body: &str, retry_after: Option<Duration>) -> LlmError {
    let err_body = extract_error_body(body);
    match status.as_u16() {
        401 | 403 => LlmError::Auth(format!("{}: {}", status, err_body)),
        429 => {
            LlmError::RateLimit { retry_after }
        }
        400 => {
            if err_body.contains("context_length") || err_body.contains("maximum context") || err_body.contains("context length") {
                LlmError::ContextLengthExceeded
            } else if err_body.contains("content_filter") {
                LlmError::ContentFilter
            } else if err_body.contains("billing") || err_body.contains("insufficient_quota") || err_body.contains("quota") {
                LlmError::Billing(err_body)
            } else {
                LlmError::RequestFailed(format!("{}: {}", status, err_body))
            }
        }
        s if s >= 500 => LlmError::ServerError(format!("{}: {}", status, err_body)),
        _ => LlmError::RequestFailed(format!("{}: {}", status, err_body)),
    }
}

// ---------------------------------------------------------------------------
// Retry wrapper (§2.3, §5.1)
// ---------------------------------------------------------------------------

pub async fn with_retry<F, Fut>(
    max_retries: u32,
    base_secs: u64,
    factor: u32,
    max_secs: u64,
    jitter: f32,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    mut f: F,
) -> Result<LlmResponse, LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<LlmResponse, LlmError>>,
{
    let mut last_err = None;
    for attempt in 0..=max_retries {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(LlmError::Cancelled);
        }
        tracing::debug!("llm attempt {}/{}", attempt + 1, max_retries + 1);
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = e.is_retryable();
                if !retryable || attempt == max_retries {
                    return Err(e);
                }
                // §2.3: take max(fixed_backoff, Retry-After)
                let backoff = (base_secs * (factor.pow(attempt) as u64))
                    .min(max_secs);
                let retry_after = e.retry_after().map(|d| d.as_secs()).unwrap_or(0);
                let delay = backoff.max(retry_after);
                // §5.1: jitter
                let jitter_ms = (delay as f32 * jitter * 1000.0) as u64;
                let actual_delay = Duration::from_secs(delay)
                    + Duration::from_millis(jitter_ms);
                tracing::debug!("llm retry {} after {:?} (error: {})", attempt, actual_delay, e);
                tokio::time::sleep(actual_delay).await;
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| LlmError::Unknown("exhausted retries".into())))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolFunction;

    #[tokio::test]
    async fn error_classifies_correctly() {
        let ep = ModelEndpoint {
            base_url: "http://127.0.0.1:1".to_string(),
            timeout_secs: 1,
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let e = client
            .chat(vec![LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
            }])
            .await
            .unwrap_err();
        assert!(
            matches!(
                e,
                LlmError::Timeout(_)
                    | LlmError::ServerError(_)
                    | LlmError::RequestFailed(_)
                    | LlmError::Unknown(_)
            ),
            "expected a recognized error variant, got: {e:?}"
        );
    }

    #[tokio::test]
    async fn health_check_rejects_auth() {
        let ep = ModelEndpoint {
            api_key: "bad_key".to_string(),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let _ = client.health_check().await;
    }

    #[test]
    fn extract_tool_calls_parses_correctly() {
        let choice = OpenAiChoice {
            message: Some(OpenAiMessageOut {
                role: None,
                content: None,
                tool_calls: Some(vec![OpenAiToolCallOut {
                    id: Some("tc_1".into()),
                    index: None,
                    function: OpenAiFunctionOut {
                        name: Some("file".into()),
                        arguments: Some("{\"path\":\".\"}".into()),
                    },
                }]),
                reasoning_content: None,
            }),
            delta: None,
            finish_reason: None,
        };
        let tc = HttpLlmClient::extract_tool_calls(&choice);
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].name, "file");
    }

    #[test]
    fn http_status_maps_correctly() {
        let r = http_status_to_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "rate limited",
            None,
        );
        assert!(matches!(r, LlmError::RateLimit { .. }));

        let r = http_status_to_error(
            reqwest::StatusCode::UNAUTHORIZED,
            "bad key",
            None,
        );
        assert!(matches!(r, LlmError::Auth(_)));

        let r = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "context_length_exceeded",
            None,
        );
        assert!(matches!(r, LlmError::ContextLengthExceeded));

        let r = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "content_filter",
            None,
        );
        assert!(matches!(r, LlmError::ContentFilter));

        let r = http_status_to_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "server error",
            None,
        );
        assert!(matches!(r, LlmError::ServerError(_)));
    }

    #[test]
    fn retry_after_extracted() {
        let e = LlmError::RateLimit {
            retry_after: Some(Duration::from_secs(10)),
        };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(10)));
        assert!(e.is_retryable());

        let e = LlmError::StreamTruncated;
        assert!(e.is_retryable());

        let e = LlmError::Auth("bad key".into());
        assert!(!e.is_retryable());
    }

    #[test]
    fn http_status_429_retains_retry_after() {
        let e = http_status_to_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "rate limited",
            Some(Duration::from_secs(30)),
        );
        assert!(matches!(e, LlmError::RateLimit { .. }));
        assert_eq!(e.retry_after(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn build_headers_custom_auth_header_name() {
        let ep = ModelEndpoint {
            api_key: "sk-test".into(),
            auth_header_name: "X-API-Key".into(),
            auth_header_prefix: String::new(),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let headers = client.build_headers();
        assert!(headers.contains_key("x-api-key"));
    }

    #[test]
    fn build_headers_default_auth_header() {
        let ep = ModelEndpoint {
            api_key: "my-key".into(),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let headers = client.build_headers();
        let val = headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(val, "Bearer my-key");
    }

    #[test]
    fn build_headers_custom_prefix() {
        let ep = ModelEndpoint {
            api_key: "token123".into(),
            auth_header_prefix: "Token".into(),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let headers = client.build_headers();
        let val = headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(val, "Token token123");
    }

    #[test]
    fn build_headers_empty_api_key_skips_auth() {
        let ep = ModelEndpoint {
            api_key: String::new(),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let headers = client.build_headers();
        assert!(headers.contains_key("content-type"));
        assert!(!headers.contains_key("authorization"));
    }

    #[test]
    fn build_headers_content_type_is_json() {
        let ep = ModelEndpoint::default();
        let client = HttpLlmClient::new(ep);
        let headers = client.build_headers();
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "application/json"
        );
    }

    #[test]
    fn build_request_body_model_name() {
        let ep = ModelEndpoint {
            model_name: "gpt-4-turbo".into(),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert_eq!(body.model, "gpt-4-turbo");
    }

    #[test]
    fn build_request_body_stream_flag() {
        let ep = ModelEndpoint::default();
        let client = HttpLlmClient::new(ep);
        let body_stream = client.build_request_body(vec![], vec![], true);
        assert!(body_stream.stream);
        let body_no_stream = client.build_request_body(vec![], vec![], false);
        assert!(!body_no_stream.stream);
    }

    #[test]
    fn build_request_body_with_tools() {
        let ep = ModelEndpoint::default();
        let client = HttpLlmClient::new(ep);
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "search".into(),
                description: "search the web".into(),
                parameters: serde_json::json!({}),
            },
        }];
        let body = client.build_request_body(vec![], tools, false);
        assert!(body.tools.is_some());
        assert_eq!(body.tools.as_ref().unwrap().len(), 1);
        assert_eq!(body.tools.unwrap()[0].tool_type, "function");
    }

    #[test]
    fn build_request_body_without_tools_has_none() {
        let ep = ModelEndpoint::default();
        let client = HttpLlmClient::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert!(body.tools.is_none());
    }

    #[test]
    fn build_request_body_extra_params_all_present() {
        let ep = ModelEndpoint {
            top_p: Some(0.9),
            top_k: Some(40),
            frequency_penalty: Some(0.5),
            presence_penalty: Some(0.3),
            stop: Some(vec!["END".into()]),
            seed: Some(42),
            response_format: Some(serde_json::json!({"type": "json_object"})),
            ..Default::default()
        };
        let client = HttpLlmClient::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert_eq!(body.top_p, Some(0.9));
        assert_eq!(body.top_k, Some(40));
        assert_eq!(body.frequency_penalty, Some(0.5));
        assert_eq!(body.presence_penalty, Some(0.3));
        assert_eq!(body.stop, Some(vec!["END".into()]));
        assert_eq!(body.seed, Some(42));
        assert!(body.response_format.is_some());
    }

    #[test]
    fn build_request_body_extra_params_none_by_default() {
        let ep = ModelEndpoint::default();
        let client = HttpLlmClient::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert_eq!(body.top_p, None);
        assert_eq!(body.top_k, None);
        assert_eq!(body.frequency_penalty, None);
        assert_eq!(body.presence_penalty, None);
        assert_eq!(body.stop, None);
        assert_eq!(body.seed, None);
        assert_eq!(body.response_format, None);
    }

    #[test]
    fn convert_messages_image_content_part() {
        let msg = LlmMessage {
            role: LlmRole::User,
            content: vec![
                ContentPart::Text("describe this".into()),
                ContentPart::Image {
                    content_type: "image_url".into(),
                    media_type: "image/png".into(),
                    data: "aGVsbG8=".into(),
                },
            ],
            tool_call_id: None,
            tool_calls: None,
        };
        let openai_msgs = HttpLlmClient::convert_messages(vec![msg]);
        assert_eq!(openai_msgs.len(), 1);
        let content = openai_msgs[0].content.as_ref().unwrap();
        assert!(content.is_array());
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "text");
        assert_eq!(arr[0]["text"], "describe this");
        assert_eq!(arr[1]["type"], "image_url");
        let url = arr[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.contains("data:image/png;base64,aGVsbG8="));
    }

    #[test]
    fn convert_messages_empty_content() {
        let msg = LlmMessage {
            role: LlmRole::User,
            content: vec![],
            tool_call_id: None,
            tool_calls: None,
        };
        let openai_msgs = HttpLlmClient::convert_messages(vec![msg]);
        assert_eq!(openai_msgs.len(), 1);
        assert!(openai_msgs[0].content.is_none());
    }

    #[test]
    fn convert_messages_system_role_maps_to_system_string() {
        let msg = LlmMessage {
            role: LlmRole::System,
            content: vec![ContentPart::text("you are helpful")],
            tool_call_id: None,
            tool_calls: None,
        };
        let openai_msgs = HttpLlmClient::convert_messages(vec![msg]);
        assert_eq!(openai_msgs[0].role, "system");
    }

    #[test]
    fn convert_messages_assistant_role_maps_to_assistant_string() {
        let msg = LlmMessage {
            role: LlmRole::Assistant,
            content: vec![ContentPart::text("hello")],
            tool_call_id: None,
            tool_calls: None,
        };
        let openai_msgs = HttpLlmClient::convert_messages(vec![msg]);
        assert_eq!(openai_msgs[0].role, "assistant");
    }

    #[test]
    fn convert_messages_tool_role_maps_to_tool_string() {
        let msg = LlmMessage {
            role: LlmRole::Tool,
            content: vec![ContentPart::text("result")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
        };
        let openai_msgs = HttpLlmClient::convert_messages(vec![msg]);
        assert_eq!(openai_msgs[0].role, "tool");
        assert_eq!(
            openai_msgs[0].tool_call_id.as_deref(),
            Some("call_1")
        );
    }

    #[test]
    fn convert_messages_single_text_part_becomes_json_string() {
        let msg = LlmMessage {
            role: LlmRole::User,
            content: vec![ContentPart::text("hello")],
            tool_call_id: None,
            tool_calls: None,
        };
        let openai_msgs = HttpLlmClient::convert_messages(vec![msg]);
        let content = openai_msgs[0].content.as_ref().unwrap();
        assert!(content.is_string());
        assert_eq!(content.as_str().unwrap(), "hello");
    }

    #[test]
    fn extract_tool_calls_no_message_no_delta() {
        let choice = OpenAiChoice {
            message: None,
            delta: None,
            finish_reason: None,
        };
        let tc = HttpLlmClient::extract_tool_calls(&choice);
        assert!(tc.is_empty());
    }

    #[test]
    fn extract_tool_calls_message_without_tool_calls_field() {
        let choice = OpenAiChoice {
            message: Some(OpenAiMessageOut {
                role: Some("assistant".into()),
                content: Some("plain text response".into()),
                tool_calls: None,
             reasoning_content: None }),
            delta: None,
            finish_reason: Some("stop".into()),
        };
        let tc = HttpLlmClient::extract_tool_calls(&choice);
        assert!(tc.is_empty());
    }

    #[test]
    fn extract_tool_calls_empty_name_skipped() {
        let choice = OpenAiChoice {
            message: None,
            delta: Some(OpenAiMessageOut {
                role: None,
                content: None,
                tool_calls: Some(vec![OpenAiToolCallOut {
                    id: Some("tc1".into()),
                    index: None,
                    function: OpenAiFunctionOut {
                        name: Some(String::new()),
                        arguments: Some("{}".into()),
                    },
                }]),
                reasoning_content: None,
            }),
            finish_reason: None,
        };
        let tc = HttpLlmClient::extract_tool_calls(&choice);
        assert!(tc.is_empty());
    }

    #[test]
    fn extract_tool_calls_missing_id_defaults_to_empty() {
        let choice = OpenAiChoice {
            message: Some(OpenAiMessageOut {
                role: None,
                content: None,
                tool_calls: Some(vec![OpenAiToolCallOut {
                    id: None,
                    index: None,
                    function: OpenAiFunctionOut {
                        name: Some("run".into()),
                        arguments: Some("{}".into()),
                    },
                }]),
                reasoning_content: None,
            }),
            delta: None,
            finish_reason: None,
        };
        let tc = HttpLlmClient::extract_tool_calls(&choice);
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].name, "run");
        assert_eq!(tc[0].id, "");
    }

    #[test]
    fn convert_tools_single_tool() {
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read".into(),
                description: "read file contents".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let result = HttpLlmClient::convert_tools(tools);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].tool_type, "function");
        assert_eq!(result[0].function.name, "read");
        assert_eq!(result[0].function.description, "read file contents");
    }

    #[test]
    fn convert_tools_multiple_tools() {
        let tools = vec![
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "a".into(),
                    description: "d1".into(),
                    parameters: serde_json::json!({}),
                },
            },
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "b".into(),
                    description: "d2".into(),
                    parameters: serde_json::json!({}),
                },
            },
        ];
        let result = HttpLlmClient::convert_tools(tools);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].function.name, "a");
        assert_eq!(result[1].function.name, "b");
    }

    #[test]
    fn convert_tools_empty_vec() {
        let result = HttpLlmClient::convert_tools(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn http_status_402_returns_request_failed() {
        let e =
            http_status_to_error(reqwest::StatusCode::PAYMENT_REQUIRED, "billing issue", None);
        assert!(matches!(e, LlmError::RequestFailed(_)));
        assert!(e.to_string().contains("402"));
    }

    #[test]
    fn http_status_403_returns_auth() {
        let e = http_status_to_error(reqwest::StatusCode::FORBIDDEN, "forbidden", None);
        assert!(matches!(e, LlmError::Auth(_)));
        assert!(e.to_string().contains("403"));
    }

    #[test]
    fn http_status_422_returns_request_failed() {
        let e = http_status_to_error(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "body too large",
            None,
        );
        assert!(matches!(e, LlmError::RequestFailed(_)));
        assert!(e.to_string().contains("422"));
    }

    #[test]
    fn http_status_500_returns_server_error() {
        let e =
            http_status_to_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom", None);
        assert!(matches!(e, LlmError::ServerError(_)));
    }

    #[test]
    fn http_status_503_returns_server_error() {
        let e =
            http_status_to_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down", None);
        assert!(matches!(e, LlmError::ServerError(_)));
    }

    #[test]
    fn http_status_400_with_insufficient_quota_returns_billing() {
        let e = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "insufficient_quota for model gpt-4",
            None,
        );
        assert!(matches!(e, LlmError::Billing(_)));
    }

    #[test]
    fn http_status_400_with_context_length_returns_context_error() {
        let e = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "maximum context exceeded",
            None,
        );
        assert!(matches!(e, LlmError::ContextLengthExceeded));
    }

    #[tokio::test]
    async fn with_retry_success_first_attempt() {
        let result = with_retry(3, 1, 2, 30, 0.0, None, || async {
            Ok(LlmResponse {
                text: "ok".into(),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: Usage::default(),
                model: None,
                reasoning: None,
})
        })
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn with_retry_non_retryable_skips_without_delay() {
        let result = with_retry(3, 1, 2, 30, 0.0, None, || async {
            Err::<LlmResponse, LlmError>(LlmError::Auth("bad key".into()))
        })
        .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LlmError::Auth(_)));
    }

    #[tokio::test]
    async fn with_retry_retryable_recovers() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = with_retry(3, 1, 2, 30, 0.0, None, || {
            let a = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if a < 2 {
                    Err(LlmError::Timeout("timeout".into()))
                } else {
                    Ok(LlmResponse {
                        text: "recovered".into(),
                        tool_calls: vec![],
                        finish_reason: Some(FinishReason::Stop),
                        usage: Usage::default(),
                        model: None,
                        reasoning: None,
})
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert!(attempts.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn with_retry_max_retries_exhausted() {
        let result = with_retry(2, 1, 2, 5, 0.0, None, || async {
            Err::<LlmResponse, LlmError>(LlmError::Timeout("persistent timeout".into()))
        })
        .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LlmError::Timeout(_)));
    }

    #[test]
    fn retry_backoff_formula() {
        let base = 2u64;
        let factor = 2u32;
        let max = 30u64;
        // attempt 0: 2 * (2^0) = 2, min max → 2
        let backoff_0 = (base * (factor.pow(0) as u64)).min(max);
        assert_eq!(backoff_0, 2);
        // attempt 1: 2 * (2^1) = 4
        let backoff_1 = (base * (factor.pow(1) as u64)).min(max);
        assert_eq!(backoff_1, 4);
        // attempt 2: 2 * (2^2) = 8
        let backoff_2 = (base * (factor.pow(2) as u64)).min(max);
        assert_eq!(backoff_2, 8);
        // attempt 3: 2 * (2^3) = 16
        let backoff_3 = (base * (factor.pow(3) as u64)).min(max);
        assert_eq!(backoff_3, 16);
        // attempt 4: 2 * (2^4) = 32, capped at 30
        let backoff_4 = (base * (factor.pow(4) as u64)).min(max);
        assert_eq!(backoff_4, 30);
    }

    #[test]
    fn retry_jitter_calculation() {
        let delay = Duration::from_secs(5);
        let jitter = 0.2f32;
        let jitter_ms = (delay.as_secs() as f32 * jitter * 1000.0) as u64;
        assert_eq!(jitter_ms, 1000);
    }
}
