use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    LineMode, build_client, build_headers, empty_chunk, health_check_request,
    normalize_web_search_call_item, reasoning_tail, reasoning_text_from_thinking_blocks,
    requires_reasoning_echo, send_request, spawn_line_reader, stream_header_timeout,
    upsert_web_search_call,
};
use crate::client::LlmClient;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    FinishReason, LlmError, LlmResponse, StreamChunk, ToolDefinition, Usage, WebSearchPhase,
};
use haven_common::config::ModelEndpoint;

// ---------------------------------------------------------------------------
// OpenAI Responses API request / response types
// ---------------------------------------------------------------------------

/// Web search mode for the provider's built-in search tool. Selected via the
/// endpoint's `web_search` config field or the `HAVEN_WEB_SEARCH` environment
/// variable (`off` | `auto` | `always`). Unconfigured defaults to `off`:
/// web search is opt-in and never changes the request shape on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchMode {
    /// Never expose the web_search tool: request shape identical to before.
    Off,
    /// Expose `{"type": "web_search"}` with `tool_choice: "auto"` — the model
    /// decides whether the question needs real-time information.
    Auto,
    /// Force a search on every request via
    /// `tool_choice: {"type": "web_search"}`.
    Always,
}

/// Parse a web search mode value (`off` | `auto` | `always`, case-insensitive).
/// Unset/empty/unrecognized values fall back to `Off` — web search only
/// activates through an explicit `off`/`auto`/`always` choice.
pub fn parse_web_search_mode(value: Option<&str>) -> WebSearchMode {
    match value
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "auto" => WebSearchMode::Auto,
        "always" | "required" | "on" | "1" | "true" => WebSearchMode::Always,
        _ => WebSearchMode::Off,
    }
}

fn web_search_mode_from_env() -> WebSearchMode {
    parse_web_search_mode(std::env::var("HAVEN_WEB_SEARCH").ok().as_deref())
}

/// Resolve the effective web search mode for an endpoint: the endpoint's
/// `web_search` config field wins, then an explicitly set `HAVEN_WEB_SEARCH`
/// environment variable, then the default (`off`).
fn resolve_web_search_mode(endpoint: &ModelEndpoint) -> WebSearchMode {
    match endpoint.web_search.as_deref() {
        Some(v) if !v.trim().is_empty() => parse_web_search_mode(Some(v)),
        _ => web_search_mode_from_env(),
    }
}

#[derive(Debug, Serialize)]
struct ResponsesTool {
    #[serde(rename = "type")]
    tool_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parameters: Option<Value>,
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
    tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponsesItem {
    #[serde(rename = "type")]
    item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    content: Vec<ResponsesContentPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    arguments: Option<String>,
    /// Fields not covered by the named members (e.g. a `web_search_call`
    /// item's `status`). Kept so output items can be round-tripped back into
    /// the next request's input verbatim.
    #[serde(flatten)]
    extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ResponsesContentPart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
    /// DeepSeek's thinking-mode compat layer streams the assistant's
    /// reasoning via this event. It must be accumulated and echoed back in
    /// the next request's input (see `convert_input`), or the provider
    /// rejects tool-call history with a 400 ("The `reasoning_text` in the
    /// thinking mode must be passed back to the API.").
    #[serde(rename = "response.reasoning_text.delta")]
    ReasoningTextDelta { delta: Option<String> },
    #[serde(rename = "response.function_call_arguments.delta")]
    FunctionCallArgsDelta {
        item_id: Option<String>,
        delta: Option<String>,
    },
    #[serde(rename = "response.output_item.added")]
    OutputItemAdded { item: Option<ResponsesItem> },
    /// DeepSeek streams the complete output item here: for a
    /// `web_search_call` this is the ONLY event that carries the full
    /// payload (`action` with `queries`, `status: "completed"`) — the
    /// `output_item.added` skeleton and the `web_search_call.*` status
    /// events lack it, and echoing the bare skeleton back 400s ("missing
    /// field `action`"). The full item must replace the skeleton (by id)
    /// before the round-trip.
    #[serde(rename = "response.output_item.done")]
    OutputItemDone { item: Option<ResponsesItem> },
    #[serde(rename = "response.web_search_call.in_progress")]
    WebSearchInProgress,
    #[serde(rename = "response.web_search_call.searching")]
    WebSearchSearching,
    #[serde(rename = "response.web_search_call.completed")]
    WebSearchCompleted { item: Option<ResponsesItem> },
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
    web_search_mode: WebSearchMode,
}

impl OpenAiResponsesAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let client = build_client(&endpoint);
        let web_search_mode = resolve_web_search_mode(&endpoint);
        Self {
            endpoint,
            client,
            web_search_mode,
        }
    }

    fn build_headers(&self) -> HeaderMap {
        build_headers(&self.endpoint, "Authorization", true)
    }

    fn responses_url(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{}/responses", base)
        } else {
            format!("{}/v1/responses", base)
        }
    }

    /// True when the endpoint is in the reasoning-echo class (DeepSeek /
    /// Kimi / MiMo), whose thinking mode requires the assistant's reasoning to
    /// be echoed back on every request carrying tool-call history (see
    /// `convert_input`).
    fn requires_reasoning_echo(&self) -> bool {
        requires_reasoning_echo(&self.endpoint)
    }

    /// Cap for the per-turn reasoning echo in `convert_input`. Full reasoning
    /// (10k+ chars per turn) makes request bodies balloon and providers stall
    /// mid-inference; the tail of each turn preserves the conclusions.
    /// The live value comes from `context_limits.reasoning_echo_max_chars`
    /// (the endpoint's `reasoning_echo_max_chars` override wins when set).
    const MAX_REASONING_ECHO_CHARS: usize = 3000;

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
    ///
    /// `requires_reasoning_echo` is set for endpoints whose Responses-compat
    /// layer demands the assistant's reasoning_text be echoed back on every
    /// request carrying tool-call history (DeepSeek thinking mode). DeepSeek
    /// validates presence, not content, so a tool-call turn on which the
    /// model skipped thinking still needs an (empty) reasoning item injected.
    fn convert_input(
        msgs: Vec<CanonicalMessage>,
        max_reasoning_echo_chars: usize,
        requires_reasoning_echo: bool,
    ) -> (Vec<Value>, Option<String>) {
        let mut instructions: Vec<String> = Vec::new();
        let mut items: Vec<Value> = Vec::new();
        for m in msgs {
            match m.role {
                CanonicalRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            instructions.push(t.clone());
                        }
                    }
                }
                CanonicalRole::User => {
                    let content = Self::content_to_parts(&m.content);
                    if !content.is_empty() {
                        items.push(json!({"role": "user", "content": content}));
                    }
                }
                CanonicalRole::Assistant => {
                    let text = Self::text_content(&m.content);
                    // DeepSeek's thinking-mode Responses compat layer REQUIRES
                    // the reasoning_text of previous assistant turns to be
                    // passed back whenever the input carries tool-call history;
                    // omitting it returns 400 ("The `reasoning_text` in the
                    // thinking mode must be passed back to the API.") or — on
                    // the streaming path — a silent empty/truncated stream.
                    // `reasoning` was persisted on the assistant message for
                    // exactly this purpose. Anthropic messages carry the text
                    // only as raw `thinking_blocks` (the agent drops the
                    // redundant `reasoning` copy), so it is reconstructed
                    // before the echo.
                    let reasoning = m
                        .reasoning
                        .as_deref()
                        .map(str::trim)
                        .filter(|r| !r.is_empty())
                        .map(str::to_string)
                        .or_else(|| {
                            let t = reasoning_text_from_thinking_blocks(&m.thinking_blocks);
                            let t = t.trim();
                            (!t.is_empty()).then(|| t.to_string())
                        });
                    //
                    // The echo is capped: full reasoning (10k+ chars per turn
                    // is routine) makes the request body balloon to 150-200KB,
                    // and providers then stall/truncate the stream mid-inference
                    // (observed as repeated empty responses on large contexts).
                    // Keeping the TAIL of each turn's reasoning preserves the
                    // conclusions while bounding the request; the provider
                    // validates presence, not length.
                    //
                    // The reasoning item comes FIRST in the turn, before the
                    // assistant message, matching the order DeepSeek's own
                    // response output emits a turn (reasoning → message →
                    // function_call). The text goes into an OpenAI-style
                    // content-part array (`[{"type":"reasoning_text","text":…}]`):
                    // DeepSeek's compat layer deserializes the `content` of a
                    // `reasoning` input item as a SEQUENCE of parts — a plain
                    // string 400s with "invalid type: string, expected a
                    // sequence" (verified against the live API).
                    //
                    // The echo is emitted only for endpoints that demand it
                    // (DeepSeek/Kimi/MiMo): the array form is
                    // compat-layer-specific, and other providers (official
                    // OpenAI Responses) neither require a reasoning input item
                    // nor accept this shape.
                    if requires_reasoning_echo {
                        if let Some(r) = reasoning {
                            items.push(json!({
                                "type": "reasoning",
                                "content": [{
                                    "type": "reasoning_text",
                                    "text": reasoning_tail(r, max_reasoning_echo_chars)
                                }]
                            }));
                        } else if m.tool_calls.as_ref().is_some_and(|c| !c.is_empty())
                            || !m.web_search_calls.is_empty()
                        {
                            // Best-effort presence echo for a tool-call /
                            // web-search turn on which the model produced no
                            // reasoning_text (it may skip thinking for a turn).
                            // NOTE (live-API verified): an empty reasoning item
                            // does NOT satisfy DeepSeek — it still 400s with
                            // "The `reasoning_text` in the thinking mode must be
                            // passed back" — so this injection only avoids a
                            // missing-item shape; a truly reasoning-less tool
                            // turn cannot be round-tripped and is the provider's
                            // constraint, not ours. Shape stays the array form
                            // (`reasoning_text` parts), never a plain string.
                            items.push(json!({
                                "type": "reasoning",
                                "content": [{"type": "reasoning_text", "text": ""}]
                            }));
                        }
                    }
                    if !text.is_empty() {
                        items.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        }));
                    }
                    // `web_search_call` items are passed back verbatim: the
                    // server restores the search context from them. Never
                    // parsed or rewritten (deepseek docs: 原样回传) — except
                    // that the `action` discriminator is filled when the
                    // captured skeleton lacks it (DeepSeek rejects an
                    // `action`-less item with a 400).
                    for ws in &m.web_search_calls {
                        items.push(normalize_web_search_call_item(ws.clone()));
                    }
                    if let Some(calls) = m.tool_calls {
                        for tc in calls {
                            items.push(json!({
                                "type": "function_call",
                                "call_id": tc.id,
                                "name": tc.name,
                                "arguments": tc.args_to_wire()
                            }));
                        }
                    }
                }
                CanonicalRole::Tool => {
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

    fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<Value> {
        tools
            .into_iter()
            .map(|t| {
                // Defense in depth: `ToolDefinition::from` already sanitizes,
                // but direct constructors / cache hits may still carry Null.
                let parameters = crate::types::sanitize_tool_parameters(t.function.parameters);
                serde_json::to_value(ResponsesTool {
                    tool_type: t.tool_type,
                    name: Some(t.function.name),
                    description: Some(t.function.description),
                    parameters: Some(parameters),
                })
                .unwrap_or_default()
            })
            .collect()
    }

    fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> ResponsesRequest {
        self.build_request_body_with_mode(messages, tools, stream, self.web_search_mode)
    }

    /// Request-body construction with an explicit web search mode. The mode
    /// is a parameter so tests can pin it without touching process-global
    /// environment variables.
    fn build_request_body_with_mode(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
        web_search_mode: WebSearchMode,
    ) -> ResponsesRequest {
        let (input, instructions) = Self::convert_input(
            messages,
            self.endpoint
                .reasoning_echo_max_chars
                .unwrap_or(Self::MAX_REASONING_ECHO_CHARS),
            self.requires_reasoning_echo(),
        );
        let mut tools_json = Self::convert_tools(tools);
        // `tool_choice` semantics: `None` (no tools at all), string
        // `"auto"`, or a specific tool object like
        // `{"type": "web_search"}` for forced search.
        let tool_choice: Option<Value> = match web_search_mode {
            WebSearchMode::Off => {
                if tools_json.is_empty() {
                    None
                } else {
                    Some(json!("auto"))
                }
            }
            WebSearchMode::Auto => {
                tools_json.push(json!({"type": "web_search"}));
                Some(json!("auto"))
            }
            WebSearchMode::Always => {
                tools_json.push(json!({"type": "web_search"}));
                Some(json!({"type": "web_search"}))
            }
        };
        ResponsesRequest {
            model: self.endpoint.model_name.clone(),
            instructions,
            input,
            max_output_tokens: Some(self.endpoint.max_tokens),
            // o-series models reject temperature != 1; skip it when unset-ish.
            temperature: (self.endpoint.temperature != 1.0).then_some(self.endpoint.temperature),
            stream,
            tools: if tools_json.is_empty() {
                None
            } else {
                Some(tools_json)
            },
            tool_choice,
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
        let mut web_search_calls = Vec::new();
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
                        tool_calls.push(CanonicalToolCall {
                            id: item.call_id.or(item.id).unwrap_or_default(),
                            name,
                            arguments: item
                                .arguments
                                .map(|a| CanonicalToolCall::from_wire_args(&a))
                                .unwrap_or(Value::Null),
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
                // Server-side web search (DeepSeek built-in): not a local
                // tool. Keep the raw item so it can be passed back verbatim
                // in the next request's input (with the `action`
                // discriminator normalized in).
                Some("web_search_call") => {
                    web_search_calls.push(normalize_web_search_call_item(
                        serde_json::to_value(&item).unwrap_or_default(),
                    ));
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
            web_search_calls,
            thinking_blocks: Vec::new(),
        })
    }

    async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body(messages, tools, stream);
        let url = self.responses_url();
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
        let json: ResponsesResponse =
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
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = send_request(
            req,
            stream_header_timeout(self.endpoint.timeout_streaming_secs),
        )
        .await?;

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseDataOnly);

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<String>,
            done: bool,
            /// Function calls accumulated per item id; flushed in the final
            /// chunk. Tuple: (lookup key for argument deltas, resolved call
            /// id, name, raw argument JSON fragments parsed at flush time).
            tool_calls: Vec<(String, String, String, String)>,
            accumulated_text: String,
            last_model: Option<String>,
            finish_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_completed: bool,
            /// Raw `web_search_call` items seen while streaming; flushed in
            /// the final chunk for round-tripping into the next request.
            web_search_calls: Vec<Value>,
        }

        let empty_chunk = empty_chunk;

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
                web_search_calls: Vec::new(),
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
                                tool_calls: state
                                    .tool_calls
                                    .drain(..)
                                    .map(|(_, id, name, args)| CanonicalToolCall {
                                        id,
                                        name,
                                        arguments: CanonicalToolCall::from_wire_args(&args),
                                    })
                                    .collect(),
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: state.web_search_calls.drain(..).collect(),
                                thinking_blocks: Vec::new(),
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
                    Ok(ResponsesStreamEvent::ReasoningTextDelta { delta }) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        // Forward each reasoning delta live so the UI can render
                        // thinking while it streams (matches the chat-completions
                        // adapter's per-delta `reasoning_content`); the router
                        // aggregates the deltas into the final response for the
                        // provider echo-back (`convert_input`).
                        if let Some(d) = delta {
                            chunk.reasoning = Some(d);
                        }
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::FunctionCallArgsDelta { item_id, delta }) => {
                        if let (Some(id), Some(d)) = (item_id, delta)
                            && let Some((_, _, _, args)) = state
                                .tool_calls
                                .iter_mut()
                                .find(|(tid, _, _, _)| tid == &id)
                        {
                            args.push_str(&d);
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::OutputItemAdded { item }) => {
                        if let Some(item) = item
                            && let Some(item_type) = item.item_type.as_deref()
                        {
                            match item_type {
                                "function_call" => {
                                    if let Some(id) = item.id.clone() {
                                        state.tool_calls.push((
                                            id,
                                            item.call_id.or(item.id).unwrap_or_default(),
                                            item.name.unwrap_or_default(),
                                            item.arguments.unwrap_or_default(),
                                        ));
                                    }
                                }
                                "web_search_call" => {
                                    upsert_web_search_call(
                                        &mut state.web_search_calls,
                                        normalize_web_search_call_item(
                                            serde_json::to_value(&item).unwrap_or_default(),
                                        ),
                                    );
                                }
                                _ => {}
                            }
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::OutputItemDone { item }) => {
                        if let Some(item) = item
                            && let Some(item_type) = item.item_type.as_deref()
                        {
                            // The authoritative `web_search_call` payload
                            // (`action`/`queries`): replace the in-progress
                            // skeleton captured from `output_item.added`, or
                            // record the item when no skeleton arrived.
                            if item_type == "web_search_call" {
                                upsert_web_search_call(
                                    &mut state.web_search_calls,
                                    normalize_web_search_call_item(
                                        serde_json::to_value(&item).unwrap_or_default(),
                                    ),
                                );
                            }
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::WebSearchInProgress) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        chunk.web_search = Some(WebSearchPhase::InProgress);
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::WebSearchSearching) => {
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        chunk.web_search = Some(WebSearchPhase::Searching);
                        Some((Ok(chunk), state))
                    }
                    Ok(ResponsesStreamEvent::WebSearchCompleted { item }) => {
                        if let Some(item) = item
                            && item.item_type.as_deref() == Some("web_search_call")
                        {
                            upsert_web_search_call(
                                &mut state.web_search_calls,
                                normalize_web_search_call_item(
                                    serde_json::to_value(&item).unwrap_or_default(),
                                ),
                            );
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        chunk.web_search = Some(WebSearchPhase::Completed);
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
                                tool_calls: state
                                    .tool_calls
                                    .drain(..)
                                    .map(|(_, id, name, args)| CanonicalToolCall {
                                        id,
                                        name,
                                        arguments: CanonicalToolCall::from_wire_args(&args),
                                    })
                                    .collect(),
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: state.web_search_calls.drain(..).collect(),
                                thinking_blocks: Vec::new(),
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
        let base = self.endpoint.base_url.trim_end_matches('/');
        let url = if base.ends_with("/v1") {
            format!("{}/models", base)
        } else {
            format!("{}/v1/models", base)
        };
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

    #[test]
    fn stream_event_parses_reasoning_text_delta() {
        // DeepSeek streams thinking-mode reasoning via this event; it must be
        // parsed (not fall through to Other) so it can be echoed back.
        let ev: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.reasoning_text.delta","content_index":0,"delta":"We need","item_id":"rs_1","output_index":0,"sequence_number":4}"#,
        )
        .unwrap();
        match ev {
            ResponsesStreamEvent::ReasoningTextDelta { delta } => {
                assert_eq!(delta.as_deref(), Some("We need"));
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

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
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("you are helpful")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hello")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (items, instructions) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
        assert_eq!(instructions.as_deref(), Some("you are helpful"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["role"], "user");
        assert_eq!(items[0]["content"][0]["type"], "input_text");
        assert_eq!(items[0]["content"][0]["text"], "hello");
    }

    #[test]
    fn convert_input_function_call_and_output() {
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("let me check")],
                tool_call_id: None,
                tool_calls: Some(vec![CanonicalToolCall {
                    id: "call_1".into(),
                    name: "file".into(),
                    arguments: serde_json::json!({"operation": "read"}),
                }]),
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("result body")],
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
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
    fn convert_input_echoes_reasoning_for_thinking_mode() {
        // DeepSeek's thinking-mode compat layer rejects tool-call history
        // without the assistant's reasoning_text passed back (400). The
        // reasoning item must be emitted before the function_call item, in
        // the same position the provider produced it (reasoning → message →
        // function_call).
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("let me check")],
                tool_call_id: None,
                tool_calls: Some(vec![CanonicalToolCall {
                    id: "call_1".into(),
                    name: "file".into(),
                    arguments: serde_json::json!({"operation": "read"}),
                }]),
                reasoning: Some("  I should read the file first.  ".into()),
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("result body")],
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs.clone(),
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            true,
        );
        assert_eq!(items.len(), 4);
        // The reasoning item leads the turn, carrying an OpenAI-style
        // content-part array (DeepSeek's compat layer deserializes the
        // `content` of a `reasoning` input item as a SEQUENCE of parts; a
        // plain string 400s with "invalid type: string, expected a sequence"),
        // then the message, then the tool call.
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["content"][0]["type"], "reasoning_text");
        assert_eq!(
            items[0]["content"][0]["text"],
            "I should read the file first."
        );
        assert_eq!(items[1]["role"], "assistant");
        assert_eq!(items[1]["content"][0]["type"], "output_text");
        assert_eq!(items[2]["type"], "function_call");
        assert_eq!(items[3]["type"], "function_call_output");
        // Providers without the echo requirement get NO reasoning item: the
        // plain-text form is DeepSeek-compat-specific, and other APIs neither
        // require nor accept it.
        let (no_echo, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
        assert_eq!(no_echo.len(), 3);
        assert_eq!(no_echo[0]["role"], "assistant");
        assert_eq!(no_echo[1]["type"], "function_call");
        assert_eq!(no_echo[2]["type"], "function_call_output");
    }

    #[test]
    fn convert_input_synthesizes_empty_reasoning_for_tool_turns_when_echo_required() {
        // DeepSeek thinking mode validates PRESENCE of the reasoning item,
        // not content: a tool-call turn on which the model skipped thinking
        // (reasoning absent) must still echo an (empty) reasoning item, or
        // the next request 400s. Only the DeepSeek echo is synthesized.
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("let me check")],
                tool_call_id: None,
                tool_calls: Some(vec![CanonicalToolCall {
                    id: "call_1".into(),
                    name: "file".into(),
                    arguments: serde_json::json!({"operation": "read"}),
                }]),
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("result body")],
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (with_echo, _) = OpenAiResponsesAdapter::convert_input(
            msgs.clone(),
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            true,
        );
        assert_eq!(with_echo.len(), 4);
        assert_eq!(with_echo[0]["type"], "reasoning");
        assert_eq!(with_echo[0]["content"][0]["type"], "reasoning_text");
        assert_eq!(with_echo[0]["content"][0]["text"], "");
        assert_eq!(with_echo[1]["role"], "assistant");
        assert_eq!(with_echo[2]["type"], "function_call");
        assert_eq!(with_echo[3]["type"], "function_call_output");
        // Without the echo requirement the reasoning item must not appear.
        let (no_echo, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
        assert_eq!(no_echo.len(), 3);
        assert_eq!(no_echo[0]["role"], "assistant");
        assert_eq!(no_echo[1]["type"], "function_call");
    }

    #[test]
    fn convert_input_truncates_oversized_reasoning_to_tail() {
        // Full reasoning echo (10k+ chars per turn) balloons the request body
        // and providers stall/truncate mid-inference. Oversized reasoning must
        // keep its TAIL (the conclusions), trimmed of whitespace.
        let long = format!(
            "{}END-MARKER",
            "thinking step. ".repeat(OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS + 500)
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
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            true,
        );
        assert_eq!(items.len(), 2);
        let echoed = items[0]["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            echoed.len(),
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS
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
    }

    #[test]
    fn convert_input_skips_blank_reasoning() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("hi")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: Some("   ".into()),
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["role"], "assistant");
    }

    #[test]
    fn convert_input_image_and_audio() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::User,
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
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
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
        let body = client.build_request_body_with_mode(
            vec![CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            }],
            Vec::new(),
            true,
            WebSearchMode::Off,
        );
        assert_eq!(body.model, "gpt-5");
        assert_eq!(body.max_output_tokens, Some(2048));
        assert_eq!(body.temperature, Some(0.4));
        assert!(body.stream);
        assert!(body.tools.is_none());
        assert!(body.tool_choice.is_none());
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
        let body = client.build_request_body_with_mode(vec![], tools, false, WebSearchMode::Off);
        let tools = body.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(body.tool_choice, Some(serde_json::json!("auto")));
    }

    #[test]
    fn web_search_mode_shapes_the_request() {
        let ep = ModelEndpoint::default();
        let client = OpenAiResponsesAdapter::new(ep);
        let defs = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "file".into(),
                description: "files".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];

        // auto: web_search tool appended, model decides via tool_choice auto.
        let body =
            client.build_request_body_with_mode(vec![], defs.clone(), false, WebSearchMode::Auto);
        let tools = body.tools.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[1], serde_json::json!({"type": "web_search"}));
        assert_eq!(body.tool_choice, Some(serde_json::json!("auto")));

        // always: search forced via a specific tool choice.
        let body =
            client.build_request_body_with_mode(vec![], defs.clone(), false, WebSearchMode::Always);
        let tools = body.tools.unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(
            body.tool_choice,
            Some(serde_json::json!({"type": "web_search"}))
        );

        // auto with no function tools still exposes the search tool.
        let body =
            client.build_request_body_with_mode(vec![], Vec::new(), false, WebSearchMode::Auto);
        let tools = body.tools.unwrap();
        assert_eq!(tools, vec![serde_json::json!({"type": "web_search"})]);

        // off with no function tools: request identical to pre-feature shape.
        let body =
            client.build_request_body_with_mode(vec![], Vec::new(), false, WebSearchMode::Off);
        assert!(body.tools.is_none());
        assert!(body.tool_choice.is_none());
    }

    #[test]
    fn parse_web_search_mode_maps_env_values() {
        // Unset / unrecognized values default to Off (web search is opt-in).
        assert_eq!(parse_web_search_mode(None), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("bogus")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("off")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("OFF")), WebSearchMode::Off);
        assert_eq!(parse_web_search_mode(Some("auto")), WebSearchMode::Auto);
        assert_eq!(parse_web_search_mode(Some("Auto")), WebSearchMode::Auto);
        assert_eq!(parse_web_search_mode(Some("always")), WebSearchMode::Always);
        assert_eq!(parse_web_search_mode(Some("Always")), WebSearchMode::Always);
    }

    #[test]
    fn convert_input_supplies_missing_web_search_call_action() {
        // DeepSeek rejects an `action`-less web_search_call input item with a
        // 400 ("missing field `action`"): the stream's output_item.added
        // skeleton only carries type/id/status, so the fallback fills the
        // internally-tagged-enum action object (search variant + queries).
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("let me search")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: vec![serde_json::json!({
                "type": "web_search_call",
                "id": "ws_1",
                "status": "in_progress"
            })],
            thinking_blocks: Vec::new(),
        }];
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(
            items[1],
            serde_json::json!({
                "type": "web_search_call",
                "id": "ws_1",
                "status": "in_progress",
                "action": {"type": "search", "queries": []}
            })
        );
    }

    #[test]
    fn convert_input_round_trips_complete_web_search_call_verbatim() {
        // An item that already carries a well-formed object `action`
        // (e.g. a completed payload) is passed back untouched.
        let ws = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {"type": "open_page", "url": "https://example.com"},
            "query": "foo"
        });
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("let me search")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: vec![ws.clone()],
            thinking_blocks: Vec::new(),
        }];
        let (items, _) = OpenAiResponsesAdapter::convert_input(
            msgs,
            OpenAiResponsesAdapter::MAX_REASONING_ECHO_CHARS,
            false,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[1], ws);
    }

    #[test]
    fn parse_response_collects_web_search_calls() {
        let json = ResponsesResponse {
            status: Some("completed".into()),
            output: vec![
                ResponsesItem {
                    item_type: Some("web_search_call".into()),
                    content: vec![],
                    call_id: None,
                    id: Some("ws_1".into()),
                    name: None,
                    arguments: None,
                    extra: serde_json::json!({"status": "completed"})
                        .as_object()
                        .unwrap()
                        .clone(),
                },
                ResponsesItem {
                    item_type: Some("message".into()),
                    content: vec![ResponsesContentPart {
                        text: Some("found it".into()),
                    }],
                    call_id: None,
                    id: Some("msg_1".into()),
                    name: None,
                    arguments: None,
                    extra: Default::default(),
                },
            ],
            usage: None,
            model: Some("deepseek-v4-flash".into()),
            error: None,
        };
        let ep = ModelEndpoint::default();
        let client = OpenAiResponsesAdapter::new(ep);
        let resp = client
            .parse_response(json, Some("deepseek-v4-flash".into()))
            .unwrap();
        assert_eq!(resp.text, "found it");
        assert!(resp.tool_calls.is_empty());
        assert_eq!(resp.web_search_calls.len(), 1);
        let item = &resp.web_search_calls[0];
        assert_eq!(item["type"], "web_search_call");
        assert_eq!(item["id"], "ws_1");
        assert_eq!(item["status"], "completed");
        assert_eq!(
            item["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
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
                    extra: Default::default(),
                },
                ResponsesItem {
                    item_type: Some("function_call".into()),
                    content: vec![],
                    call_id: Some("call_1".into()),
                    id: Some("fc_1".into()),
                    name: Some("file".into()),
                    arguments: Some(r#"{"operation":"read"}"#.into()),
                    extra: Default::default(),
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
    fn stream_web_search_events_parse() {
        let in_progress: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.web_search_call.in_progress","item_id":"ws_1"}"#,
        )
        .unwrap();
        assert!(matches!(
            in_progress,
            ResponsesStreamEvent::WebSearchInProgress
        ));

        let searching: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.web_search_call.searching","item_id":"ws_1"}"#,
        )
        .unwrap();
        assert!(matches!(
            searching,
            ResponsesStreamEvent::WebSearchSearching
        ));

        let completed: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.web_search_call.completed","item_id":"ws_1","item":{"type":"web_search_call","id":"ws_1","status":"completed"}}"#,
        )
        .unwrap();
        if let ResponsesStreamEvent::WebSearchCompleted { item: Some(item) } = completed {
            assert_eq!(item.item_type.as_deref(), Some("web_search_call"));
            assert_eq!(item.id.as_deref(), Some("ws_1"));
            assert_eq!(
                item.extra.get("status").and_then(|v| v.as_str()),
                Some("completed")
            );
        } else {
            panic!("expected web_search_call.completed with item");
        }

        // The raw item serializes back verbatim (round-trip input fidelity).
        let raw = serde_json::to_value(item_of(&completed_parse())).unwrap();
        assert_eq!(
            raw,
            serde_json::json!({"type": "web_search_call", "id": "ws_1", "status": "completed"})
        );

        // `output_item.done` carries the FULL web_search_call payload
        // (action + queries) that the skeleton lacks; this is the event the
        // round-trip must keep.
        let done: ResponsesStreamEvent = serde_json::from_str(
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","queries":["capital of France","ws_call_id=ws_1"]}}}"#,
        )
        .unwrap();
        if let ResponsesStreamEvent::OutputItemDone { item: Some(item) } = done {
            assert_eq!(item.item_type.as_deref(), Some("web_search_call"));
            assert_eq!(item.id.as_deref(), Some("ws_1"));
            let action = item.extra.get("action").unwrap();
            assert_eq!(action["type"], "search");
            assert_eq!(action["queries"][0], "capital of France");
        } else {
            panic!("expected output_item.done with web_search_call item");
        }
    }

    fn completed_parse() -> ResponsesStreamEvent {
        serde_json::from_str(
            r#"{"type":"response.web_search_call.completed","item":{"type":"web_search_call","id":"ws_1","status":"completed"}}"#,
        )
        .unwrap()
    }

    fn item_of(event: &ResponsesStreamEvent) -> &ResponsesItem {
        match event {
            ResponsesStreamEvent::WebSearchCompleted { item } => item.as_ref().unwrap(),
            _ => panic!("expected WebSearchCompleted"),
        }
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
