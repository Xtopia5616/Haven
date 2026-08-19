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
    /// Thinking-block signature. Anthropic validates it against the thinking
    /// text on echo, so it must be captured and passed back verbatim.
    #[serde(default)]
    signature: Option<String>,
    /// `redacted_thinking` payload. Redacted thinking blocks must also be
    /// echoed back verbatim on tool-use turns.
    #[serde(default)]
    data: Option<String>,
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
    /// Thinking-block signature delivered on the `content_block_start` event.
    #[serde(default)]
    signature: Option<String>,
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

    /// Internal marker key embedded in `thinking_blocks` that records the
    /// original content-block order at capture time. Stripped before the
    /// echo, so Anthropic never sees it. Signature validation is unaffected
    /// by position, so the echo is free to restore the exact interleaving.
    const LAYOUT_KEY: &str = "__layout";
    /// Layout entry kind: the block lives in `thinking_blocks`.
    const LAYOUT_KIND_THINKING: u8 = 0;
    /// Layout entry kind: the block lives in `tool_calls`.
    const LAYOUT_KIND_TOOL_USE: u8 = 1;

    /// If the trailing entry of `blocks` is the internal `__layout` marker,
    /// remove it and decode the layout: one `(kind, pos, text_before)` triple
    /// per original content block, in original order (`pos` = index in the
    /// provider content array, `text_before` = char count of visible text
    /// accumulated before that block).
    fn split_layout(blocks: &mut Vec<Value>) -> Option<Vec<(u8, usize, usize)>> {
        let last = blocks.last_mut()?;
        if !last.is_object() {
            return None;
        }
        let entry = last.get(Self::LAYOUT_KEY)?.clone();
        let layout: Vec<(u8, usize, usize)> = serde_json::from_value(entry).ok()?;
        blocks.pop();
        Some(layout)
    }

    /// Rebuild an assistant message's content blocks in the original
    /// interleaved order (thinking ↔ text ↔ tool_use) from the capture-time
    /// layout. The visible text is spliced back into segments at the recorded
    /// boundaries; adjacent original text blocks merge into one segment,
    /// which is semantically identical. Returns `None` when the layout is
    /// absent or inconsistent (legacy snapshots, hand-built messages, or
    /// content rewritten downstream), letting the caller fall back to the
    /// legacy front-loaded order.
    fn rebuild_ordered_blocks(
        content: &[ContentPart],
        thinking_blocks: &[Value],
        tool_calls: Option<&[CanonicalToolCall]>,
        layout: &[(u8, usize, usize)],
    ) -> Option<Vec<Value>> {
        // Non-text parts (e.g. images) have no position in the layout; keep
        // the legacy order for such messages.
        if content.iter().any(|p| !matches!(p, ContentPart::Text(_))) {
            return None;
        }
        let text: String = content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let calls = tool_calls.unwrap_or_default();
        let text_len = text.chars().count();

        let (think_count, call_count) =
            layout
                .iter()
                .fold((0usize, 0usize), |(t, c), (kind, _, _)| {
                    if *kind == Self::LAYOUT_KIND_THINKING {
                        (t + 1, c)
                    } else {
                        (t, c + 1)
                    }
                });
        if think_count != thinking_blocks.len() || call_count != calls.len() {
            return None;
        }

        let mut out = Vec::new();
        let mut prev_pos: Option<usize> = None;
        let mut prev_tb = 0usize;
        let mut think_idx = 0usize;
        let mut call_idx = 0usize;
        for (kind, pos, tb) in layout {
            if let Some(p) = prev_pos
                && *pos <= p
            {
                return None;
            }
            if *tb < prev_tb || *tb > text_len {
                return None;
            }
            prev_pos = Some(*pos);
            if *tb > prev_tb {
                out.push(json!({
                    "type": "text",
                    "text": text
                        .chars()
                        .skip(prev_tb)
                        .take(*tb - prev_tb)
                        .collect::<String>()
                }));
            }
            match *kind {
                Self::LAYOUT_KIND_THINKING => {
                    out.push(thinking_blocks[think_idx].clone());
                    think_idx += 1;
                }
                _ => {
                    let tc = &calls[call_idx];
                    out.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments
                    }));
                    call_idx += 1;
                }
            }
            prev_tb = *tb;
        }
        if text_len > prev_tb {
            out.push(json!({
                "type": "text",
                "text": text.chars().skip(prev_tb).collect::<String>()
            }));
        }
        Some(out)
    }

    /// Legacy assistant block order: all thinking blocks first, then the
    /// visible content, then the tool calls.
    fn legacy_assistant_blocks(
        captured: Vec<Value>,
        content: &[ContentPart],
        tool_calls: Option<Vec<CanonicalToolCall>>,
    ) -> Vec<Value> {
        let mut blocks = captured;
        blocks.extend(Self::content_to_blocks(content));
        if let Some(calls) = tool_calls {
            for tc in calls {
                blocks.push(json!({
                    "type": "tool_use",
                    "id": tc.id,
                    "name": tc.name,
                    "input": tc.arguments
                }));
            }
        }
        blocks
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
                    // Echo the raw thinking blocks verbatim (text + signature):
                    // Anthropic 400s a tool-use turn that omits or rewrites
                    // them. `thinking_blocks` holds the exact
                    // `{"type":"thinking",…}` JSON captured upstream, plus an
                    // internal `__layout` marker that records each block's
                    // original position so the echo restores the exact
                    // interleaved order instead of front-loading the thinking
                    // blocks (position does not affect signature validation).
                    let calls = m.tool_calls;
                    let mut captured = m.thinking_blocks;
                    let blocks = match Self::split_layout(&mut captured) {
                        Some(layout) => {
                            match Self::rebuild_ordered_blocks(
                                &m.content,
                                &captured,
                                calls.as_deref(),
                                &layout,
                            ) {
                                Some(ordered) => ordered,
                                None => Self::legacy_assistant_blocks(captured, &m.content, calls),
                            }
                        }
                        None => Self::legacy_assistant_blocks(captured, &m.content, calls),
                    };
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
        let mut thinking_blocks = Vec::new();
        // Original position of each captured block (index in the content
        // array + visible-text char count before it) so the next tool-use
        // request can restore the exact interleaved order on echo.
        let mut layout: Vec<(u8, usize, usize)> = Vec::new();
        for (i, block) in json.content.into_iter().enumerate() {
            match block.block_type.as_deref() {
                Some("text") => {
                    if let Some(t) = block.text {
                        text.push_str(&t);
                    }
                }
                Some("thinking") => {
                    if let Some(t) = block.thinking {
                        reasoning.push_str(&t);
                        // Keep the raw thinking block (text + signature) so the
                        // next tool-use request can echo it back verbatim.
                        let mut block_json = json!({
                            "type": "thinking",
                            "thinking": t,
                        });
                        if let Some(sig) = block.signature {
                            block_json["signature"] = Value::String(sig);
                        }
                        thinking_blocks.push(block_json);
                        layout.push((Self::LAYOUT_KIND_THINKING, i, text.chars().count()));
                    }
                }
                Some("redacted_thinking") => {
                    // Redacted thinking (extended-thinking safety redaction)
                    // must be echoed back verbatim too; the data is not real
                    // thinking text, so it never feeds `reasoning`.
                    if let Some(data) = block.data {
                        thinking_blocks.push(json!({
                            "type": "redacted_thinking",
                            "data": data,
                        }));
                        layout.push((Self::LAYOUT_KIND_THINKING, i, text.chars().count()));
                    }
                }
                Some("tool_use") => {
                    if let Some(name) = block.name {
                        tool_calls.push(CanonicalToolCall {
                            id: block.id.unwrap_or_default(),
                            name,
                            arguments: block.input.unwrap_or_default(),
                        });
                        layout.push((Self::LAYOUT_KIND_TOOL_USE, i, text.chars().count()));
                    }
                }
                _ => {}
            }
        }
        if !layout.is_empty() {
            thinking_blocks.push(json!({Self::LAYOUT_KEY: layout}));
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
            thinking_blocks,
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
            thinking: String,
            thinking_signature: String,
            /// Original content-block index (for the echo layout marker).
            pos: usize,
            /// Char count of accumulated visible text when this block started
            /// (for the echo layout marker).
            text_before: usize,
        }

        #[derive(PartialEq)]
        enum BlockKind {
            Text,
            Thinking,
            RedactedThinking,
            ToolUse,
        }

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<String>,
            done: bool,
            /// Per-content-block streaming state, indexed by Anthropic block index.
            blocks: Vec<BlockState>,
            accumulated_text: String,
            /// Capture-time layout: `(kind, pos, text_before)` per content
            /// block, in order. Emitted as the trailing `__layout` marker on
            /// the final chunk so the echo can restore the interleaving.
            layout: Vec<(u8, usize, usize)>,
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
                layout: Vec::new(),
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
                            let mut final_chunk = StreamChunk {
                                text: None,
                                tool_calls: Vec::new(),
                                finish_reason: state.stop_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: Vec::new(),
                                thinking_blocks: Vec::new(),
                            };
                            if !state.layout.is_empty() {
                                final_chunk.thinking_blocks.push(json!({
                                    Self::LAYOUT_KEY: state.layout
                                }));
                            }
                            Ok(final_chunk)
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
                                thinking: String::new(),
                                thinking_signature: String::new(),
                                pos: 0,
                                text_before: 0,
                            });
                        }
                        {
                            let block = &mut state.blocks[index];
                            block.pos = index;
                            block.text_before = state.accumulated_text.chars().count();
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
                                Some("thinking") => {
                                    block.kind = BlockKind::Thinking;
                                    // The signature arrives on the start event;
                                    // without it the echo of this block would be
                                    // rejected with a 400 on the next turn.
                                    block.thinking_signature =
                                        content_block.signature.unwrap_or_default();
                                }
                                Some("redacted_thinking") => {
                                    // Redacted thinking deltas accumulate into
                                    // `block.thinking` like plain thinking; the
                                    // stop handler re-emits them as a
                                    // `redacted_thinking` block for the echo.
                                    block.kind = BlockKind::RedactedThinking;
                                }
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
                                chunk.reasoning = Some(thinking.clone());
                                if let Some(block) = state.blocks.get_mut(index) {
                                    block.thinking.push_str(&thinking);
                                }
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
                        if let Some(block) = state.blocks.get(index) {
                            match block.kind {
                                BlockKind::ToolUse => {
                                    chunk.tool_calls.push(CanonicalToolCall {
                                        id: block.tool_id.clone(),
                                        name: block.tool_name.clone(),
                                        arguments: CanonicalToolCall::from_wire_args(
                                            &block.tool_input,
                                        ),
                                    });
                                    state.layout.push((
                                        Self::LAYOUT_KIND_TOOL_USE,
                                        block.pos,
                                        block.text_before,
                                    ));
                                }
                                BlockKind::Thinking => {
                                    // Emit the completed thinking block (text +
                                    // signature) so the aggregation keeps it
                                    // verbatim for the next request's echo.
                                    if !block.thinking.is_empty() {
                                        let mut block_json = json!({
                                            "type": "thinking",
                                            "thinking": block.thinking.clone(),
                                        });
                                        if !block.thinking_signature.is_empty() {
                                            block_json["signature"] =
                                                Value::String(block.thinking_signature.clone());
                                        }
                                        chunk.thinking_blocks.push(block_json);
                                        state.layout.push((
                                            Self::LAYOUT_KIND_THINKING,
                                            block.pos,
                                            block.text_before,
                                        ));
                                    }
                                }
                                BlockKind::RedactedThinking => {
                                    // Redacted thinking must also round-trip
                                    // verbatim; the deltas held `data` chunks.
                                    if !block.thinking.is_empty() {
                                        chunk.thinking_blocks.push(json!({
                                            "type": "redacted_thinking",
                                            "data": block.thinking.clone(),
                                        }));
                                        state.layout.push((
                                            Self::LAYOUT_KIND_THINKING,
                                            block.pos,
                                            block.text_before,
                                        ));
                                    }
                                }
                                BlockKind::Text => {}
                            }
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
                        let mut final_chunk = StreamChunk {
                            text: None,
                            tool_calls: Vec::new(),
                            finish_reason: state.stop_reason,
                            usage: state.usage.take(),
                            model: state.last_model.clone(),
                            reasoning: None,
                            web_search: None,
                            web_search_calls: Vec::new(),
                            thinking_blocks: Vec::new(),
                        };
                        if !state.layout.is_empty() {
                            final_chunk.thinking_blocks.push(json!({
                                Self::LAYOUT_KEY: state.layout
                            }));
                        }
                        Some((Ok(final_chunk), state))
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
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("part two")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
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
            thinking_blocks: Vec::new(),
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
            thinking_blocks: Vec::new(),
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
    fn convert_messages_echoes_thinking_blocks_verbatim() {
        let thinking = serde_json::json!({
            "type": "thinking",
            "thinking": "let me plan this out",
            "signature": "sig_abc123"
        });
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("checking")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "toolu_3".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: Some("let me plan this out".into()),
            web_search_calls: Vec::new(),
            thinking_blocks: vec![thinking.clone()],
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        assert_eq!(out.len(), 1);
        let content = out[0].content.as_array().unwrap();
        assert_eq!(content.len(), 3);
        // The raw thinking block is echoed first, verbatim (type + text +
        // signature), ahead of the text and tool_use blocks.
        assert_eq!(content[0], thinking);
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["id"], "toolu_3");
    }

    fn resp_block(
        block_type: &str,
        text: Option<&str>,
        thinking: Option<&str>,
        id: Option<&str>,
        name: Option<&str>,
        input: Option<Value>,
        signature: Option<&str>,
    ) -> AnthropicResponseBlock {
        AnthropicResponseBlock {
            block_type: Some(block_type.into()),
            text: text.map(String::from),
            thinking: thinking.map(String::from),
            id: id.map(String::from),
            name: name.map(String::from),
            input,
            signature: signature.map(String::from),
            data: None,
        }
    }

    /// Parse a response and echo the resulting canonical message back,
    /// returning the converted content blocks.
    fn parse_and_echo(content: Vec<AnthropicResponseBlock>) -> Vec<Value> {
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let resp = client
            .parse_response(
                AnthropicResponse {
                    content,
                    stop_reason: None,
                    usage: None,
                    model: None,
                },
                None,
            )
            .unwrap();
        let msg = CanonicalMessage::assistant(
            vec![ContentPart::text(resp.text.clone())],
            Some(resp.tool_calls),
            resp.reasoning.clone(),
            Vec::new(),
            resp.thinking_blocks,
        );
        let (out, _) = AnthropicAdapter::convert_messages(vec![msg]);
        out[0].content.as_array().unwrap().clone()
    }

    #[test]
    fn convert_messages_echo_restores_interleaved_thinking_text_tool_use() {
        // Original: [thinking, text, tool_use] — the echo must restore the
        // interleaving instead of front-loading the thinking block.
        let content = parse_and_echo(vec![
            resp_block(
                "thinking",
                None,
                Some("plan"),
                None,
                None,
                None,
                Some("sig_1"),
            ),
            resp_block("text", Some("Let me check"), None, None, None, None, None),
            resp_block(
                "tool_use",
                None,
                None,
                Some("toolu_1"),
                Some("file"),
                Some(json!({"operation": "read"})),
                None,
            ),
        ]);
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["thinking"], "plan");
        assert_eq!(content[0]["signature"], "sig_1");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "Let me check");
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["id"], "toolu_1");
    }

    #[test]
    fn convert_messages_echo_restores_multi_tool_interleaving() {
        // Original: [thinking, tool_use, thinking, tool_use, text]. The final
        // text must stay after both tool calls, and each thinking block must
        // stay with its tool_use.
        let content = parse_and_echo(vec![
            resp_block(
                "thinking",
                None,
                Some("think one"),
                None,
                None,
                None,
                Some("sig_1"),
            ),
            resp_block(
                "tool_use",
                None,
                None,
                Some("toolu_1"),
                Some("file"),
                Some(json!({"op": 1})),
                None,
            ),
            resp_block(
                "thinking",
                None,
                Some("think two"),
                None,
                None,
                None,
                Some("sig_2"),
            ),
            resp_block(
                "tool_use",
                None,
                None,
                Some("toolu_2"),
                Some("file"),
                Some(json!({"op": 2})),
                None,
            ),
            resp_block("text", Some("all done"), None, None, None, None, None),
        ]);
        let types: Vec<&str> = content
            .iter()
            .map(|b| b["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            ["thinking", "tool_use", "thinking", "tool_use", "text"]
        );
        assert_eq!(content[0]["thinking"], "think one");
        assert_eq!(content[1]["id"], "toolu_1");
        assert_eq!(content[2]["thinking"], "think two");
        assert_eq!(content[3]["id"], "toolu_2");
        assert_eq!(content[4]["text"], "all done");
    }

    #[test]
    fn convert_messages_echo_restores_text_before_thinking() {
        // Original: [text, thinking, tool_use].
        let content = parse_and_echo(vec![
            resp_block("text", Some("preface"), None, None, None, None, None),
            resp_block(
                "thinking",
                None,
                Some("plan"),
                None,
                None,
                None,
                Some("sig_1"),
            ),
            resp_block(
                "tool_use",
                None,
                None,
                Some("toolu_1"),
                Some("file"),
                Some(json!({})),
                None,
            ),
        ]);
        let types: Vec<&str> = content
            .iter()
            .map(|b| b["type"].as_str().unwrap())
            .collect();
        assert_eq!(types, ["text", "thinking", "tool_use"]);
        assert_eq!(content[0]["text"], "preface");
        // The layout marker is stripped before the echo; the thinking block
        // is emitted verbatim.
        assert_eq!(content[1]["signature"], "sig_1");
        assert!(content[1].get("__layout").is_none());
    }

    #[test]
    fn convert_messages_echo_falls_back_on_inconsistent_layout() {
        // A hand-built message whose layout marker does not match the actual
        // thinking blocks must fall back to the legacy front-loaded order
        // instead of emitting a malformed echo.
        let thinking = serde_json::json!({
            "type": "thinking",
            "thinking": "plan",
            "signature": "sig_x",
        });
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("checking")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "toolu_9".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: vec![
                thinking.clone(),
                // Layout claims two thinking blocks but only one exists.
                json!({"__layout": [[0, 0, 0], [0, 1, 0]]}),
            ],
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        let content = out[0].content.as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0], thinking);
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[2]["type"], "tool_use");
        assert_eq!(content[2]["id"], "toolu_9");
    }

    #[test]
    fn convert_messages_echo_falls_back_when_layout_text_before_exceeds_text() {
        // Layout claims 10 chars of text before the thinking block, but the
        // message only carries 3 — the echo must fall back, not panic.
        let thinking = serde_json::json!({
            "type": "thinking",
            "thinking": "plan",
            "signature": "sig_x",
        });
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("abc")],
            tool_call_id: None,
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: vec![thinking.clone(), json!({"__layout": [[0, 0, 10]]})],
        }];
        let (out, _) = AnthropicAdapter::convert_messages(msgs);
        let content = out[0].content.as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0], thinking);
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "abc");
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
            thinking_blocks: Vec::new(),
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
            thinking_blocks: Vec::new(),
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
                thinking_blocks: Vec::new(),
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
                signature: None,
                data: None,
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
                    signature: None,
                    data: None,
                },
                AnthropicResponseBlock {
                    block_type: Some("tool_use".into()),
                    text: None,
                    thinking: None,
                    id: Some("toolu_9".into()),
                    name: Some("file".into()),
                    input: Some(json!({"operation": "read", "path": "."})),
                    signature: None,
                    data: None,
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
                    signature: Some("sig_1".into()),
                    data: None,
                },
                AnthropicResponseBlock {
                    block_type: Some("text".into()),
                    text: Some("final answer".into()),
                    thinking: None,
                    id: None,
                    name: None,
                    input: None,
                    signature: None,
                    data: None,
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
        // The raw thinking block (text + signature) is preserved verbatim for
        // the next request's echo, followed by the internal layout marker that
        // records its original position (block at index 0, no text before).
        assert_eq!(resp.thinking_blocks.len(), 2);
        assert_eq!(resp.thinking_blocks[0]["type"], "thinking");
        assert_eq!(resp.thinking_blocks[0]["thinking"], "inner monologue");
        assert_eq!(resp.thinking_blocks[0]["signature"], "sig_1");
        assert_eq!(resp.thinking_blocks[1], json!({"__layout": [[0, 0, 0]]}));
    }

    #[test]
    fn parse_response_thinking_block_without_signature_omits_field() {
        let json = AnthropicResponse {
            content: vec![AnthropicResponseBlock {
                block_type: Some("thinking".into()),
                text: None,
                thinking: Some("no sig".into()),
                id: None,
                name: None,
                input: None,
                signature: None,
                data: None,
            }],
            stop_reason: None,
            usage: None,
            model: None,
        };
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let resp = client.parse_response(json, None).unwrap();
        assert_eq!(resp.thinking_blocks.len(), 2);
        assert!(resp.thinking_blocks[0].get("signature").is_none());
        assert_eq!(resp.reasoning.as_deref(), Some("no sig"));
    }

    #[test]
    fn parse_response_redacted_thinking_captured_for_echo() {
        let json = AnthropicResponse {
            content: vec![
                AnthropicResponseBlock {
                    block_type: Some("redacted_thinking".into()),
                    text: None,
                    thinking: None,
                    id: None,
                    name: None,
                    input: None,
                    signature: None,
                    data: Some("base64-redacted".into()),
                },
                AnthropicResponseBlock {
                    block_type: Some("text".into()),
                    text: Some("answer".into()),
                    thinking: None,
                    id: None,
                    name: None,
                    input: None,
                    signature: None,
                    data: None,
                },
            ],
            stop_reason: None,
            usage: None,
            model: None,
        };
        let ep = ModelEndpoint::default();
        let client = AnthropicAdapter::new(ep);
        let resp = client.parse_response(json, None).unwrap();
        assert_eq!(resp.text, "answer");
        // Redacted data never leaks into visible reasoning, but the block is
        // kept verbatim so the next tool-use request can echo it back.
        assert_eq!(resp.reasoning, None);
        assert_eq!(resp.thinking_blocks.len(), 2);
        assert_eq!(resp.thinking_blocks[0]["type"], "redacted_thinking");
        assert_eq!(resp.thinking_blocks[0]["data"], "base64-redacted");
        assert_eq!(resp.thinking_blocks[1], json!({"__layout": [[0, 0, 0]]}));
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

        // The thinking block's signature arrives on `content_block_start` and
        // must be captured for the verbatim echo.
        let thinking_start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"thinking","signature":"sig_stream_1"}}"#,
        )
        .unwrap();
        if let AnthropicStreamEvent::ContentBlockStart { content_block, .. } = thinking_start {
            assert_eq!(content_block.signature.as_deref(), Some("sig_stream_1"));
        } else {
            panic!("expected thinking block start");
        }

        // `redacted_thinking` blocks stream the same `thinking_delta` data
        // chunks; the start event only marks the block type.
        let redacted_start: AnthropicStreamEvent = serde_json::from_str(
            r#"{"type":"content_block_start","index":3,"content_block":{"type":"redacted_thinking"}}"#,
        )
        .unwrap();
        if let AnthropicStreamEvent::ContentBlockStart { content_block, .. } = redacted_start {
            assert_eq!(
                content_block.block_type.as_deref(),
                Some("redacted_thinking")
            );
        } else {
            panic!("expected redacted_thinking block start");
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
