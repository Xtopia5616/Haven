use async_trait::async_trait;
use futures_util::FutureExt;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    build_client, build_headers, health_check_request, normalize_web_search_call_item,
    reasoning_tail, reasoning_text_from_thinking_blocks, requires_reasoning_echo, send_request,
    stream_header_timeout,
};
use crate::client::LlmClient;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    Embedding, FinishReason, LlmError, LlmResponse, StreamChunk, SttResult, ToolDefinition, Usage,
};
use haven_common::config::ModelEndpoint;

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
    /// DeepSeek et al. require the reasoning_content of prior assistant
    /// turns to be echoed back in the request for thinking-mode conversations.
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<String>,
    /// DeepSeek web search: the provider's built-in search tool output must
    /// be passed back verbatim in the next request's assistant message so the
    /// server restores the search context (stateless chat API). Never parsed
    /// or rewritten.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    web_search_call: Vec<serde_json::Value>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize)]
struct StreamOptions {
    include_usage: bool,
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
    /// DeepSeek's built-in web search output (`web_search_call` items).
    /// Accumulated and echoed back verbatim in the next request.
    #[serde(default)]
    web_search_call: Vec<serde_json::Value>,
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

#[derive(Debug, Serialize)]
struct OpenAiEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedItem {
    embedding: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedItem>,
    usage: Option<OpenAiUsage>,
    model: Option<String>,
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
    usage: Option<OpenAiUsage>,
    model: Option<String>,
}

/// True when `model` is a dedicated ASR id that speaks
/// `/audio/transcriptions` (OpenAI `whisper-1` / `gpt-4o-transcribe*`, Groq
/// `whisper-large-v3*`, local whisper.cpp aliases, …). Multimodal chat-audio
/// models (e.g. `gpt-4o-audio-preview`) return false so the router can fall
/// back to chat + `input_audio`.
pub(crate) fn is_whisper_model(model: &str) -> bool {
    let n = model.to_ascii_lowercase();
    n.contains("whisper") || n.contains("transcribe")
}

/// OpenAI-compatible chat adapter: the common wire format spoken by OpenAI,
/// Ollama, vLLM, DeepSeek, and most third-party gateways.
pub struct OpenAiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
}

impl OpenAiAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let client = build_client(&endpoint);
        Self { endpoint, client }
    }

    fn build_headers(&self) -> HeaderMap {
        build_headers(&self.endpoint, "Authorization", true)
    }

    /// True when the endpoint's thinking mode requires the assistant's
    /// reasoning to be echoed back on every request that carries tool-call
    /// history (DeepSeek / Kimi / MiMo: `reasoning_content`).
    fn requires_reasoning_echo(&self) -> bool {
        requires_reasoning_echo(&self.endpoint)
    }

    /// `requires_reasoning_echo` is set for endpoints whose thinking mode
    /// demands the assistant's reasoning be echoed back on every request that
    /// carries tool-call history (DeepSeek: `reasoning_content`). DeepSeek
    /// validates presence, not content, so a tool-call turn on which the
    /// model skipped thinking needs an empty `reasoning_content` injected.
    ///
    /// Cap for the per-turn reasoning echo. Full reasoning (10k+ chars per
    /// turn is routine) balloons request bodies and providers stall or
    /// truncate mid-inference (same failure mode documented in the Responses
    /// adapter); keeping the TAIL of each turn's reasoning preserves the
    /// conclusions. The live value comes from
    /// `context_limits.reasoning_echo_max_chars` (the endpoint's
    /// `reasoning_echo_max_chars` override wins when set).
    const MAX_REASONING_ECHO_CHARS: usize = 3000;

    fn convert_messages(
        msgs: Vec<CanonicalMessage>,
        requires_reasoning_echo: bool,
        reasoning_echo_max_chars: usize,
    ) -> Vec<OpenAiMessage> {
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
                        ContentPart::Image {
                            media_type, data, ..
                        } => Some(serde_json::json!([{
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", media_type, data)
                            }
                        }])),
                        ContentPart::Audio {
                            media_type, data, ..
                        } => Some(serde_json::json!([{
                            "type": "input_audio",
                            "input_audio": {
                                "format": media_type.rsplit('/').next().unwrap_or("wav"),
                                "data": data
                            }
                        }])),
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
                            ContentPart::Audio {
                                media_type, data, ..
                            } => serde_json::json!({
                                "type": "input_audio",
                                "input_audio": {
                                    "format": media_type.rsplit('/').next().unwrap_or("wav"),
                                    "data": data
                                }
                            }),
                        })
                        .collect();
                    Some(serde_json::Value::Array(parts))
                };
                // Anthropic messages carry the thinking text only as raw
                // `thinking_blocks` (the agent drops the redundant `reasoning`
                // copy); reconstruct it so the reasoning echo still applies.
                let reasoning = m.reasoning.or_else(|| {
                    let t = reasoning_text_from_thinking_blocks(&m.thinking_blocks);
                    (!t.is_empty()).then_some(t)
                });
                // Cap the echo to its tail (the conclusions), mirroring the
                // Responses adapter: unbounded reasoning (10k+ chars per turn)
                // balloons the request body and stalls/truncates the provider's
                // stream mid-inference. The provider validates presence, not
                // length, so the trimmed tail round-trips fine.
                let reasoning = reasoning.map(|r| reasoning_tail(r, reasoning_echo_max_chars));
                // DeepSeek thinking mode validates PRESENCE of
                // `reasoning_content`, not its content: a tool-call / web-search
                // turn on which the model skipped thinking must still carry the
                // field (empty is accepted) or the next request 400s.
                let requires_reasoning_pad = requires_reasoning_echo
                    && reasoning.as_ref().is_none_or(|r| r.trim().is_empty())
                    && (m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
                        || !m.web_search_calls.is_empty());
                let tool_calls = m.tool_calls.map(|calls| {
                    calls
                        .into_iter()
                        .map(|tc| {
                            let args = tc.args_to_wire();
                            OpenAiMessageToolCall {
                                id: tc.id,
                                call_type: "function".into(),
                                function: OpenAiMessageToolFunction {
                                    name: tc.name,
                                    arguments: args,
                                },
                            }
                        })
                        .collect()
                });
                OpenAiMessage {
                    role: match m.role {
                        CanonicalRole::System => "system".to_string(),
                        CanonicalRole::User => "user".to_string(),
                        CanonicalRole::Assistant => "assistant".to_string(),
                        CanonicalRole::Tool => "tool".to_string(),
                    },
                    content,
                    tool_call_id: m.tool_call_id,
                    tool_calls,
                    reasoning_content: if requires_reasoning_pad {
                        Some(String::new())
                    } else {
                        reasoning
                    },
                    // `web_search_call` items are echoed back for the
                    // stateless chat API to restore the search context, with
                    // the `action` discriminator filled when the captured
                    // skeleton lacks it (DeepSeek 400s otherwise).
                    web_search_call: m
                        .web_search_calls
                        .into_iter()
                        .map(normalize_web_search_call_item)
                        .collect(),
                }
            })
            .collect()
    }

    fn extract_tool_calls(choice: &OpenAiChoice) -> Vec<CanonicalToolCall> {
        let mut out = Vec::new();
        if let Some(msg) = choice.message.as_ref().or(choice.delta.as_ref())
            && let Some(calls) = &msg.tool_calls
        {
            for c in calls {
                let name = c.function.name.clone().unwrap_or_default();
                let args = c.function.arguments.clone().unwrap_or_default();
                let id = c.id.clone().unwrap_or_default();
                if !name.is_empty() {
                    out.push(CanonicalToolCall {
                        id,
                        name,
                        arguments: CanonicalToolCall::from_wire_args(&args),
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
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> OpenAiRequest {
        let has_tools = !tools.is_empty();
        OpenAiRequest {
            model: self.endpoint.model_name.clone(),
            messages: Self::convert_messages(
                messages,
                self.requires_reasoning_echo(),
                self.endpoint
                    .reasoning_echo_max_chars
                    .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
            ),
            max_tokens: Some(self.endpoint.max_tokens),
            // Reasoning models (o1/o3-family, reasoning_effort configured)
            // reject a non-default temperature; the Responses adapter skips
            // it for the same reason. Omit it whenever the endpoint pins a
            // reasoning effort — the provider's default (1.0) applies.
            temperature: self
                .endpoint
                .reasoning_effort
                .is_none()
                .then_some(self.endpoint.temperature),
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
            reasoning_effort: self.endpoint.reasoning_effort.clone(),
            stream_options: if stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
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
        let web_search_calls = choice
            .message
            .as_ref()
            .map(|m| {
                m.web_search_call
                    .iter()
                    .cloned()
                    .map(normalize_web_search_call_item)
                    .collect()
            })
            .unwrap_or_default();

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
            finish_reason: choice
                .finish_reason
                .and_then(|s| FinishReason::from_openai(&s)),
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning,
            web_search_calls,
            thinking_blocks: Vec::new(),
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
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body(messages, tools, stream);
        let url = format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        );

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
        let resp = send_request(req, None).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let json: OpenAiResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_openai_response(json, model)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body(messages, tools, true);
        let url = format!(
            "{}/chat/completions",
            self.endpoint.base_url.trim_end_matches('/')
        );
        tracing::debug!(
            "chat_stream_inner: url={} model={} api_key={} timeout_secs={} timeout_streaming={:?}",
            url,
            self.endpoint.model_name,
            if self.endpoint.api_key.is_empty() {
                "EMPTY"
            } else {
                "SET"
            },
            self.endpoint.timeout_secs,
            self.endpoint.timeout_streaming_secs
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
        // When timeout_streaming_secs is None, `stream_header_timeout` bounds the
        // response-header wait (a provider that accepts the connection but never
        // responds would otherwise stall silently until the router-level
        // max_total_duration_secs) while leaving the body stream to the router's
        // per-chunk idle timeouts.
        if let Some(timeout) = self.endpoint.timeout_streaming_secs {
            tracing::trace!("chat_stream_inner: {}s streaming timeout", timeout);
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = send_request(
            req,
            stream_header_timeout(self.endpoint.timeout_streaming_secs),
        )
        .await
        .map_err(|e| {
            tracing::debug!("chat_stream_inner: send() error: {:?}", e);
            e
        })?;
        tracing::debug!("chat_stream_inner response status: {}", resp.status());

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
                                    tracing::trace!(
                                        "openai stream payload: {} chars",
                                        payload.len()
                                    );
                                    // If the receiver was dropped (consumer cancelled
                                    // or stream abandoned), stop reading the HTTP
                                    // response body to avoid wasting bandwidth/CPU.
                                    if tx.send(payload).is_err() {
                                        return;
                                    }
                                }
                            }
                            Some(Err(_)) | None => {
                                // Flush any remaining buffered data before EOF.
                                let remaining = buf.trim().to_string();
                                if !remaining.is_empty() && remaining != "[DONE]" {
                                    tracing::trace!(
                                        "openai stream flush: {} chars",
                                        remaining.len()
                                    );
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

        // Merge streaming tool-call deltas by index. Arguments arrive as
        // incremental JSON fragments, so they accumulate as a raw string and
        // are parsed once at flush time.
        fn merge_tool_call(
            acc: &mut Vec<(String, String, String)>,
            index: usize,
            id: Option<&str>,
            name: Option<&str>,
            arguments: Option<&str>,
        ) {
            while acc.len() <= index {
                acc.push((String::new(), String::new(), String::new()));
            }
            if let Some(id) = id
                && !id.is_empty()
            {
                acc[index].0 = id.to_string();
            }
            if let Some(name) = name
                && !name.is_empty()
            {
                acc[index].1 = name.to_string();
            }
            if let Some(args) = arguments {
                acc[index].2.push_str(args);
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
            tool_calls_acc: Vec<(String, String, String)>,
            web_search_acc: Vec<serde_json::Value>,
            last_model: Option<String>,
            has_finish_reason: bool,
            usage: Option<Usage>,
        }

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                accumulated_text: String::new(),
                tool_calls_acc: Vec::new(),
                web_search_acc: Vec::new(),
                last_model: None,
                has_finish_reason: false,
                usage: None,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(d) => d,
                    None => {
                        let chunk =
                            if !state.has_finish_reason && !state.accumulated_text.is_empty() {
                                Err(LlmError::StreamTruncated)
                            } else {
                                Ok(StreamChunk {
                                    text: None,
                                    tool_calls: std::mem::take(&mut state.tool_calls_acc)
                                        .into_iter()
                                        .map(|(id, name, args)| CanonicalToolCall {
                                            id,
                                            name,
                                            arguments: CanonicalToolCall::from_wire_args(&args),
                                        })
                                        .collect(),
                                    finish_reason: None,
                                    usage: state.usage.take(),
                                    model: state.last_model.clone(),
                                    reasoning: None,
                                    web_search: None,
                                    web_search_calls: std::mem::take(&mut state.web_search_acc),
                                    thinking_blocks: Vec::new(),
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
                        if let Some(u) = resp.usage {
                            state.usage = Some(Usage {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.completion_tokens,
                                total_tokens: u.total_tokens,
                                model_name: state.last_model.clone(),
                                cost: None,
                            });
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
                            // DeepSeek's built-in web search: accumulate the
                            // `web_search_call` items so they can be echoed
                            // back verbatim on the next request.
                            if let Some(delta) = choice_delta(&choice)
                                && !delta.web_search_call.is_empty()
                            {
                                state
                                    .web_search_acc
                                    .extend(delta.web_search_call.iter().cloned());
                            }
                            if choice.finish_reason.is_some() {
                                state.has_finish_reason = true;
                            }
                            let finish_reason = choice
                                .finish_reason
                                .as_ref()
                                .and_then(|s| FinishReason::from_openai(s));
                            Some((
                                Ok(StreamChunk {
                                    text: choice_delta(&choice).and_then(|d| d.content.clone()),
                                    reasoning: choice_delta(&choice)
                                        .and_then(|d| d.reasoning_content.clone()),
                                    tool_calls: Vec::new(),
                                    finish_reason,
                                    usage: None,
                                    model: state.last_model.clone(),
                                    web_search: None,
                                    web_search_calls: Vec::new(),
                                    thinking_blocks: Vec::new(),
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
                                    usage: state.usage.take(),
                                    model: state.last_model.clone(),
                                    web_search: None,
                                    web_search_calls: Vec::new(),
                                    thinking_blocks: Vec::new(),
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
impl LlmClient for OpenAiAdapter {
    fn style(&self) -> &'static str {
        "openai-chat"
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

    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult, LlmError> {
        // Native `/audio/transcriptions` only for Whisper-family models.
        // Multimodal chat models (e.g. gpt-4o-audio-preview) return
        // Unsupported so the router can fall back to chat + `input_audio`.
        if !is_whisper_model(&self.endpoint.model_name) {
            return Err(LlmError::UnsupportedCapability(format!(
                "model '{}' is not a Whisper transcription model",
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
        tracing::debug!("POST {} (model: {})", url, self.endpoint.model_name);
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers())
            .multipart(form);
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;
        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
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
        })
    }

    async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
        if input.is_empty() {
            return Ok(Embedding {
                vectors: Vec::new(),
                model: Some(self.endpoint.model_name.clone()),
                usage: Usage::default(),
            });
        }
        let url = format!(
            "{}/embeddings",
            self.endpoint.base_url.trim_end_matches('/')
        );
        let body = OpenAiEmbedRequest {
            model: self.endpoint.model_name.clone(),
            input,
        };
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
        let resp = send_request(req, None).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let json: OpenAiEmbedResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let requested = body.input.len();
        let vectors: Vec<Vec<f32>> = json.data.into_iter().map(|item| item.embedding).collect();
        if vectors.is_empty() {
            return Err(LlmError::InvalidResponse(
                "embeddings response missing data".into(),
            ));
        }
        if vectors.len() != requested {
            return Err(LlmError::InvalidResponse(format!(
                "embeddings count mismatch: requested {requested}, got {}",
                vectors.len()
            )));
        }
        let model = json
            .model
            .clone()
            .or(Some(self.endpoint.model_name.clone()));
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
        Ok(Embedding {
            vectors,
            model,
            usage,
        })
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        let url = format!("{}/models", self.endpoint.base_url.trim_end_matches('/'));
        health_check_request(
            &self.client,
            &url,
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

    #[tokio::test]
    async fn error_classifies_correctly() {
        let ep = ModelEndpoint {
            base_url: "http://127.0.0.1:1".to_string(),
            timeout_secs: 1,
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let e = client
            .chat(vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
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
        let client = OpenAiAdapter::new(ep);
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
                web_search_call: Vec::new(),
            }),
            delta: None,
            finish_reason: None,
        };
        let tc = OpenAiAdapter::extract_tool_calls(&choice);
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].name, "file");
    }

    #[test]
    fn parse_openai_response_collects_web_search_calls() {
        // DeepSeek's built-in web search returns `web_search_call` items in
        // the assistant message. They must be collected so the next request
        // can echo them back verbatim (stateless chat API).
        let ws = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed"
        });
        let choice = OpenAiChoice {
            message: Some(OpenAiMessageOut {
                role: Some("assistant".into()),
                content: Some("searched".into()),
                tool_calls: None,
                reasoning_content: None,
                web_search_call: vec![ws.clone()],
            }),
            delta: None,
            finish_reason: Some("stop".into()),
        };
        let json = OpenAiResponse {
            choices: vec![choice],
            usage: None,
            model: Some("deepseek".into()),
        };
        let ep = ModelEndpoint::default();
        let adapter = OpenAiAdapter::new(ep);
        let resp = adapter.parse_openai_response(json, None).unwrap();
        assert_eq!(resp.web_search_calls.len(), 1);
        assert_eq!(resp.web_search_calls[0]["type"], "web_search_call");
        assert_eq!(
            resp.web_search_calls[0]["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
    }

    #[test]
    fn convert_messages_echoes_web_search_calls_verbatim() {
        // A complete item (with `action`) is echoed back untouched.
        let ws = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_9",
            "status": "completed",
            "action": {"type": "search", "queries": ["capital of France"]},
            "query": "foo"
        });
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("searched")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: vec![ws.clone()],
            thinking_blocks: Vec::new(),
        }];
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(msgs, Vec::new(), false);
        let out = body.messages[0].web_search_call.first().cloned().unwrap();
        assert_eq!(out, ws);
    }

    #[test]
    fn convert_messages_derives_reasoning_from_thinking_blocks() {
        // Anthropic messages carry the thinking text only as raw
        // `thinking_blocks` (the agent drops the redundant `reasoning` copy);
        // the reasoning echo must still work when such a message is sent to an
        // OpenAI-compatible reasoning-echo provider.
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("checked")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_1".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: vec![
                serde_json::json!({"type": "thinking", "thinking": "let me check", "signature": "s1"}),
                serde_json::json!({"type": "redacted_thinking", "data": "redacted"}),
            ],
        }];
        let ep = ModelEndpoint {
            provider: "deepseek".into(),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(msgs, Vec::new(), false);
        assert_eq!(
            body.messages[0].reasoning_content.as_deref(),
            Some("let me check")
        );
    }

    #[test]
    fn convert_messages_truncates_oversized_reasoning_to_tail() {
        // Unbounded reasoning echo (10k+ chars per turn) balloons the request
        // body and stalls/truncates the provider's stream mid-inference (the
        // same failure mode the Responses adapter documents). The echo must
        // keep the TAIL of the reasoning (the conclusions), bounded by the
        // cap — and the cap must come from the endpoint's
        // `reasoning_echo_max_chars` override.
        let long = format!(
            "{}END-MARKER",
            "thinking step. ".repeat(OpenAiAdapter::MAX_REASONING_ECHO_CHARS + 500)
        );
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("ok")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: Some(long.clone()),
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(msgs, Vec::new(), false);
        let echoed = body.messages[0].reasoning_content.as_deref().unwrap();
        assert_eq!(
            echoed.chars().count(),
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS
        );
        assert!(
            echoed.ends_with("END-MARKER"),
            "the tail (conclusions) must be preserved, got: ...{}",
            &echoed[echoed.len().saturating_sub(40)..]
        );
        assert!(
            !echoed.starts_with("thinking step. "),
            "the head must be trimmed"
        );
        // A custom per-endpoint cap wins over the default.
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("ok")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: Some(long),
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let ep = ModelEndpoint {
            reasoning_echo_max_chars: Some(64),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(msgs, Vec::new(), false);
        let echoed = body.messages[0].reasoning_content.as_deref().unwrap();
        assert_eq!(echoed.chars().count(), 64);
        assert!(echoed.ends_with("END-MARKER"));
    }

    #[test]
    fn convert_messages_supplies_missing_web_search_call_action() {
        // The in-progress skeleton captured from the stream lacks `action`;
        // echoing it back as-is 400s on DeepSeek, so the field is filled.
        let ws = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_9",
            "status": "in_progress"
        });
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("searched")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: vec![ws.clone()],
            thinking_blocks: Vec::new(),
        }];
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(msgs, Vec::new(), false);
        let out = body.messages[0].web_search_call.first().cloned().unwrap();
        assert_eq!(out["type"], "web_search_call");
        assert_eq!(out["id"], "ws_9");
        assert_eq!(out["status"], "in_progress");
        assert_eq!(
            out["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
    }

    #[test]
    fn build_headers_custom_auth_header_name() {
        let ep = ModelEndpoint {
            api_key: "sk-test".into(),
            auth_header_name: "X-API-Key".into(),
            auth_header_prefix: String::new(),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let headers = client.build_headers();
        assert!(headers.contains_key("x-api-key"));
        // Empty prefix must send the raw key — never `" sk-test"`.
        assert_eq!(headers.get("x-api-key").unwrap().to_str().unwrap(), "sk-test");
    }

    #[test]
    fn is_whisper_model_covers_transcribe_ids() {
        assert!(is_whisper_model("whisper-1"));
        assert!(is_whisper_model("gpt-4o-transcribe"));
        assert!(is_whisper_model("gpt-4o-mini-transcribe"));
        assert!(!is_whisper_model("gpt-4o-audio-preview"));
        assert!(!is_whisper_model("gpt-4o"));
    }

    #[test]
    fn build_headers_default_auth_header() {
        let ep = ModelEndpoint {
            api_key: "my-key".into(),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let headers = client.build_headers();
        let val = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(val, "Bearer my-key");
    }

    #[test]
    fn build_headers_custom_prefix() {
        let ep = ModelEndpoint {
            api_key: "token123".into(),
            auth_header_prefix: "Token".into(),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let headers = client.build_headers();
        let val = headers.get("authorization").unwrap().to_str().unwrap();
        assert_eq!(val, "Token token123");
    }

    #[test]
    fn build_headers_empty_api_key_skips_auth() {
        let ep = ModelEndpoint {
            api_key: String::new(),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let headers = client.build_headers();
        assert!(headers.contains_key("content-type"));
        assert!(!headers.contains_key("authorization"));
    }

    #[test]
    fn build_headers_content_type_is_json() {
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
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
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert_eq!(body.model, "gpt-4-turbo");
    }

    #[test]
    fn build_request_body_stream_flag() {
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
        let body_stream = client.build_request_body(vec![], vec![], true);
        assert!(body_stream.stream);
        let body_no_stream = client.build_request_body(vec![], vec![], false);
        assert!(!body_no_stream.stream);
    }

    #[test]
    fn build_request_body_stream_options_requests_usage() {
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
        let body_stream = client.build_request_body(vec![], vec![], true);
        let opts = body_stream
            .stream_options
            .expect("stream request must ask for usage");
        assert!(opts.include_usage);
        let body_no_stream = client.build_request_body(vec![], vec![], false);
        assert!(body_no_stream.stream_options.is_none());
    }

    #[test]
    fn build_request_body_with_tools() {
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
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
    fn build_request_body_omits_temperature_for_reasoning_effort_models() {
        // o1/o3-family models reject a non-default temperature; when a
        // reasoning_effort is pinned the temperature field must be omitted
        // (provider default 1.0 applies).
        let ep = ModelEndpoint {
            temperature: 0.7,
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert!(body.temperature.is_none());
        assert_eq!(body.reasoning_effort.as_deref(), Some("high"));
        // Without reasoning_effort the configured temperature is sent.
        let ep = ModelEndpoint {
            temperature: 0.7,
            reasoning_effort: None,
            ..Default::default()
        };
        let client = OpenAiAdapter::new(ep);
        let body = client.build_request_body(vec![], vec![], false);
        assert_eq!(body.temperature, Some(0.7));
    }

    #[test]
    fn build_request_body_without_tools_has_none() {
        let ep = ModelEndpoint::default();
        let client = OpenAiAdapter::new(ep);
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
        let client = OpenAiAdapter::new(ep);
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
        let client = OpenAiAdapter::new(ep);
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
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
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
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
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
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        assert_eq!(openai_msgs.len(), 1);
        assert!(openai_msgs[0].content.is_none());
    }

    #[test]
    fn convert_messages_system_role_maps_to_system_string() {
        let msg = CanonicalMessage {
            role: CanonicalRole::System,
            content: vec![ContentPart::text("you are helpful")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        assert_eq!(openai_msgs[0].role, "system");
    }

    #[test]
    fn convert_messages_assistant_role_maps_to_assistant_string() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("hello")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        assert_eq!(openai_msgs[0].role, "assistant");
    }

    #[test]
    fn convert_messages_tool_role_maps_to_tool_string() {
        let msg = CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("result")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        assert_eq!(openai_msgs[0].role, "tool");
        assert_eq!(openai_msgs[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn convert_messages_single_text_part_becomes_json_string() {
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::text("hello")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        let content = openai_msgs[0].content.as_ref().unwrap();
        assert!(content.is_string());
        assert_eq!(content.as_str().unwrap(), "hello");
    }

    #[test]
    fn convert_messages_single_audio_part_nested_input_audio() {
        let msg = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::Audio {
                content_type: "input_audio".into(),
                media_type: "audio/wav".into(),
                data: "aGVsbG8=".into(),
            }],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        let content = openai_msgs[0].content.as_ref().unwrap();
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "input_audio");
        assert_eq!(arr[0]["input_audio"]["format"], "wav");
        assert_eq!(arr[0]["input_audio"]["data"], "aGVsbG8=");
    }

    #[test]
    fn convert_messages_single_image_part_nested_image_url() {
        let msg = CanonicalMessage {
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
            thinking_blocks: Vec::new(),
        };
        let openai_msgs = OpenAiAdapter::convert_messages(
            vec![msg],
            false,
            OpenAiAdapter::MAX_REASONING_ECHO_CHARS,
        );
        let content = openai_msgs[0].content.as_ref().unwrap();
        let arr = content.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "image_url");
        let url = arr[0]["image_url"]["url"].as_str().unwrap();
        assert!(url.contains("data:image/png;base64,aGVsbG8="));
    }

    #[test]
    fn extract_tool_calls_no_message_no_delta() {
        let choice = OpenAiChoice {
            message: None,
            delta: None,
            finish_reason: None,
        };
        let tc = OpenAiAdapter::extract_tool_calls(&choice);
        assert!(tc.is_empty());
    }

    #[test]
    fn extract_tool_calls_message_without_tool_calls_field() {
        let choice = OpenAiChoice {
            message: Some(OpenAiMessageOut {
                role: Some("assistant".into()),
                content: Some("plain text response".into()),
                tool_calls: None,
                reasoning_content: None,
                web_search_call: Vec::new(),
            }),
            delta: None,
            finish_reason: Some("stop".into()),
        };
        let tc = OpenAiAdapter::extract_tool_calls(&choice);
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
                web_search_call: Vec::new(),
            }),
            finish_reason: None,
        };
        let tc = OpenAiAdapter::extract_tool_calls(&choice);
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
                web_search_call: Vec::new(),
            }),
            delta: None,
            finish_reason: None,
        };
        let tc = OpenAiAdapter::extract_tool_calls(&choice);
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
        let result = OpenAiAdapter::convert_tools(tools);
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
        let result = OpenAiAdapter::convert_tools(tools);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].function.name, "a");
        assert_eq!(result[1].function.name, "b");
    }

    #[test]
    fn convert_tools_empty_vec() {
        let result = OpenAiAdapter::convert_tools(vec![]);
        assert!(result.is_empty());
    }

    #[test]
    fn stream_response_parses_usage_from_final_chunk() {
        let json = r#"{"id":"c1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15},"model":"gpt-5"}"#;
        let resp: OpenAiStreamResponse = serde_json::from_str(json).unwrap();
        let usage = resp.usage.expect("final chunk must carry usage");
        assert_eq!(usage.prompt_tokens, 10);
        assert_eq!(usage.completion_tokens, 5);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn stream_response_usage_absent_parses_fine() {
        let json = r#"{"id":"c1","choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#;
        let resp: OpenAiStreamResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
        assert!(!resp.choices.is_empty());
    }
}
