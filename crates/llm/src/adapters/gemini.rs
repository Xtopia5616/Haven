use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    LineMode, build_client, build_headers, empty_chunk, health_check_request, send_request,
    spawn_line_reader, stream_header_timeout,
};
use crate::client::LlmClient;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    FinishReason, LlmError, LlmResponse, StreamChunk, SttResult, ToolDefinition, Usage,
};
use base64::Engine;
use haven_common::config::ModelEndpoint;
use haven_common::prompts::STT_SYSTEM_PROMPT;

// ---------------------------------------------------------------------------
// Gemini generateContent request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<Value>,
}

#[derive(Debug, Serialize)]
struct GeminiContent {
    role: String,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Serialize)]
struct GeminiTool {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig {
    temperature: f32,
    max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

// Response types: `text` and `function_call` parts, plus usage metadata.
#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(default)]
    usage_metadata: Option<GeminiUsage>,
    #[serde(default)]
    model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiResponseContent>,
    #[serde(alias = "finishReason")]
    #[serde(alias = "finish_reason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponseContent {
    #[serde(default)]
    parts: Vec<GeminiResponsePart>,
}

#[derive(Debug, Deserialize)]
struct GeminiResponsePart {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    function_call: Option<GeminiFunctionCall>,
    /// Gemini thinking-mode marker: parts carrying `"thought": true` hold the
    /// model's internal reasoning and MUST NOT be shown as assistant text.
    #[serde(default)]
    thought: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
struct GeminiUsage {
    #[serde(default, alias = "promptTokenCount")]
    prompt_tokens: u32,
    #[serde(default, alias = "candidatesTokenCount")]
    candidates_tokens: u32,
    #[serde(default, alias = "totalTokenCount")]
    total_tokens: u32,
}

/// Google Gemini API adapter (`generateContent` / `streamGenerateContent`).
pub struct GeminiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
}

impl GeminiAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        let client = build_client(&endpoint);
        Self { endpoint, client }
    }

    /// Gemini authenticates with `x-goog-api-key`. If the user customized
    /// `auth_header_name`/`auth_header_prefix`, respect the custom scheme
    /// instead (for gateways that expect `Authorization: Bearer …`).
    fn build_headers(&self) -> HeaderMap {
        build_headers(&self.endpoint, "x-goog-api-key", false)
    }

    /// API base, tolerating base_urls that already carry `/v1beta` (or `/v1`).
    fn api_base(&self) -> String {
        let base = self.endpoint.base_url.trim_end_matches('/');
        if base.ends_with("/v1beta") || base.ends_with("/v1") {
            base.to_string()
        } else {
            format!("{}/v1beta", base)
        }
    }

    fn generate_url(&self) -> String {
        format!(
            "{}/models/{}:generateContent",
            self.api_base(),
            self.endpoint.model_name
        )
    }

    fn stream_generate_url(&self) -> String {
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.api_base(),
            self.endpoint.model_name
        )
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.api_base())
    }

    /// Convert provider-neutral messages into Gemini contents. System prompts
    /// are extracted into the top-level `systemInstruction`; tool results map
    /// to `functionResponse` parts; assistant tool calls to `functionCall`
    /// parts.
    fn convert_contents(msgs: Vec<CanonicalMessage>) -> (Vec<GeminiContent>, Option<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<GeminiContent> = Vec::new();
        // Gemini's `functionResponse.name` must match the `functionCall.name`
        // of the original call (call ids are generated locally and never sent
        // to the API). Track the id -> function name mapping from assistant
        // tool calls so tool results reference the function name.
        let mut call_id_to_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        // Call ids in the order the latest assistant DECLARED them. Gemini
        // pairs each `functionResponse` with the `functionCall` of the same
        // name by position, so parallel calls to the SAME tool must have
        // their results emitted in declaration order — the canonical holds
        // them in completion order, which could swap them.
        let mut declared_order: Vec<String> = Vec::new();
        // Consecutive tool results buffered until the next non-tool message
        // (or end of input), then flushed in declaration order.
        let mut pending_tool_results: Vec<(String, String)> = Vec::new();
        for m in msgs {
            match m.role {
                CanonicalRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            system_parts.push(t.clone());
                        }
                    }
                }
                CanonicalRole::User | CanonicalRole::Tool => {
                    let is_tool_result =
                        matches!(m.role, CanonicalRole::Tool) || m.tool_call_id.is_some();
                    if is_tool_result {
                        let call_id = m.tool_call_id.unwrap_or_default();
                        let text = Self::text_content(&m.content);
                        pending_tool_results.push((call_id, text));
                    } else {
                        Self::flush_pending_tool_results(
                            &mut out,
                            &mut pending_tool_results,
                            &declared_order,
                            &call_id_to_name,
                        );
                        let parts = Self::content_to_parts(&m.content);
                        if parts.is_empty() {
                            continue;
                        }
                        out.push(GeminiContent {
                            role: "user".into(),
                            parts,
                        });
                    }
                }
                CanonicalRole::Assistant => {
                    Self::flush_pending_tool_results(
                        &mut out,
                        &mut pending_tool_results,
                        &declared_order,
                        &call_id_to_name,
                    );
                    let mut parts = Self::content_to_parts(&m.content);
                    if let Some(calls) = &m.tool_calls {
                        declared_order.clear();
                        for tc in calls {
                            call_id_to_name.insert(tc.id.clone(), tc.name.clone());
                            declared_order.push(tc.id.clone());
                            parts.push(GeminiPart {
                                text: None,
                                inline_data: None,
                                function_call: Some(json!({
                                    "name": tc.name,
                                    "args": tc.arguments
                                })),
                                function_response: None,
                            });
                        }
                    }
                    if parts.is_empty() {
                        continue;
                    }
                    out.push(GeminiContent {
                        role: "model".into(),
                        parts,
                    });
                }
            }
        }
        Self::flush_pending_tool_results(
            &mut out,
            &mut pending_tool_results,
            &declared_order,
            &call_id_to_name,
        );
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(json!({"parts": [{"text": system_parts.join("\n\n")}]}))
        };
        (out, system)
    }

    /// Emit buffered tool results as `functionResponse` user contents, ordered
    /// by the assistant's DECLARATION order. Gemini pairs each
    /// `functionResponse` with the `functionCall` of the same name by
    /// position, so parallel calls to the same tool must have their results
    /// emitted in declaration order — the canonical holds them in completion
    /// order, which would swap them. Results whose call id was never declared
    /// (orphans) keep their arrival order at the end.
    fn flush_pending_tool_results(
        out: &mut Vec<GeminiContent>,
        pending: &mut Vec<(String, String)>,
        declared_order: &[String],
        call_id_to_name: &std::collections::HashMap<String, String>,
    ) {
        if pending.is_empty() {
            return;
        }
        let mut items: Vec<(usize, (String, String))> = pending
            .drain(..)
            .map(|(call_id, text)| {
                let pos = declared_order
                    .iter()
                    .position(|id| *id == call_id)
                    .unwrap_or(usize::MAX);
                (pos, (call_id, text))
            })
            .collect();
        // Stable sort: unknown call ids keep their arrival order at the end.
        items.sort_by_key(|(pos, _)| *pos);
        for (_, (call_id, text)) in items {
            let name = call_id_to_name
                .get(&call_id)
                .cloned()
                .unwrap_or_else(|| call_id.clone());
            out.push(GeminiContent {
                role: "user".into(),
                parts: vec![GeminiPart {
                    text: None,
                    inline_data: None,
                    function_call: None,
                    function_response: Some(json!({
                        "name": name,
                        "response": {"result": text}
                    })),
                }],
            });
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

    fn content_to_parts(parts: &[ContentPart]) -> Vec<GeminiPart> {
        parts
            .iter()
            .map(|p| match p {
                ContentPart::Text(t) => GeminiPart {
                    text: Some(t.clone()),
                    inline_data: None,
                    function_call: None,
                    function_response: None,
                },
                ContentPart::Image {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mime_type": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                },
                ContentPart::Audio {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mime_type": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                },
            })
            .collect()
    }

    fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<GeminiTool> {
        tools
            .into_iter()
            .map(|t| GeminiTool {
                function_declarations: vec![GeminiFunctionDeclaration {
                    name: t.function.name,
                    description: t.function.description,
                    parameters: t.function.parameters,
                }],
            })
            .collect()
    }

    fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        _stream: bool,
    ) -> GeminiRequest {
        let (contents, system_instruction) = Self::convert_contents(messages);
        let has_tools = !tools.is_empty();
        GeminiRequest {
            contents,
            system_instruction,
            tools: if has_tools {
                Some(Self::convert_tools(tools))
            } else {
                None
            },
            generation_config: Some(GeminiGenerationConfig {
                temperature: self.endpoint.temperature,
                max_output_tokens: self.endpoint.max_tokens,
                top_p: self.endpoint.top_p,
                top_k: self.endpoint.top_k,
                stop_sequences: self.endpoint.stop.clone(),
            }),
        }
    }

    fn finish_reason_of(s: &str) -> Option<FinishReason> {
        match s {
            "STOP" => Some(FinishReason::Stop),
            "MAX_TOKENS" => Some(FinishReason::Length),
            "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" => {
                Some(FinishReason::ContentFilter)
            }
            "MALFORMED_FUNCTION_CALL" => Some(FinishReason::ToolCalls),
            _ => FinishReason::from_openai(&s.to_lowercase()),
        }
    }

    fn parse_response(
        &self,
        json: GeminiResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut finish_reason = None;
        if let Some(candidate) = json.candidates.and_then(|c| c.into_iter().next()) {
            finish_reason = candidate
                .finish_reason
                .as_deref()
                .and_then(Self::finish_reason_of);
            if let Some(content) = candidate.content {
                for part in content.parts {
                    if part.thought == Some(true) {
                        // Thinking-mode parts are internal reasoning, not
                        // assistant output: route to `reasoning` (displayed as
                        // a thought bubble), never into the visible answer.
                        if let Some(t) = part.text {
                            reasoning.push_str(&t);
                        }
                    } else if let Some(t) = part.text {
                        text.push_str(&t);
                    }
                    if let Some(fc) = part.function_call
                        && let Some(name) = fc.name
                    {
                        tool_calls.push(CanonicalToolCall {
                            id: format!("call_{}", tool_calls.len()),
                            name,
                            arguments: fc.args.unwrap_or_default(),
                        });
                    }
                }
            }
        }
        let usage = json
            .usage_metadata
            .map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.candidates_tokens,
                total_tokens: u.total_tokens,
                model_name: model.clone(),
                cost: None,
            })
            .unwrap_or_default();
        Ok(LlmResponse {
            text,
            tool_calls,
            finish_reason,
            usage,
            model: model.or_else(|| Some(self.endpoint.model_name.clone())),
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        })
    }

    async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body(messages, tools, false);
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
        let json: GeminiResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model_version.clone();
        self.parse_response(json, model)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body(messages, tools, true);
        let url = self.stream_generate_url();
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
        // `:streamGenerateContent?alt=sse` returns SSE frames; gateways that
        // ignore `alt=sse` fall back to raw JSON lines — both are handled.
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseOrRaw);

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<String>,
            done: bool,
            /// Accumulated text per part index (deltas are emitted as suffixes).
            /// Tracks EVERY part (including `thought: true` reasoning parts) so
            /// prefix-stripping stays aligned on the part index.
            part_texts: Vec<String>,
            /// Accumulated reasoning per thinking part index (emitted as
            /// reasoning deltas, mirroring the text delta logic).
            reasoning_parts: Vec<String>,
            /// Accumulated tool calls per functionCall part index.
            tool_calls_acc: Vec<CanonicalToolCall>,
            accumulated_text: String,
            last_model: Option<String>,
            finish_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_finish: bool,
        }

        let empty_chunk = empty_chunk;

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                part_texts: Vec::new(),
                reasoning_parts: Vec::new(),
                tool_calls_acc: Vec::new(),
                accumulated_text: String::new(),
                last_model: None,
                finish_reason: None,
                usage: None,
                saw_finish: false,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(d) => d,
                    None => {
                        let chunk = if !state.saw_finish && !state.accumulated_text.is_empty() {
                            Err(LlmError::StreamTruncated)
                        } else {
                            Ok(StreamChunk {
                                text: None,
                                // Flush accumulated tool calls like the OpenAI
                                // adapter: per-delta chunks carry none, the
                                // final chunk carries all merged calls.
                                tool_calls: std::mem::take(&mut state.tool_calls_acc),
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: Vec::new(),
                                thinking_blocks: Vec::new(),
                            })
                        };
                        state.done = true;
                        return Some((chunk, state));
                    }
                };
                let parsed: Result<GeminiResponse, _> = serde_json::from_str(&data);
                match parsed {
                    Ok(resp) => {
                        if let Some(m) = &resp.model_version {
                            state.last_model = Some(m.clone());
                        }
                        if let Some(u) = resp.usage_metadata {
                            state.usage = Some(Usage {
                                prompt_tokens: u.prompt_tokens,
                                completion_tokens: u.candidates_tokens,
                                total_tokens: u.total_tokens,
                                model_name: state.last_model.clone(),
                                cost: None,
                            });
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(candidate) = resp.candidates.and_then(|c| c.into_iter().next())
                        {
                            if let Some(sr) = candidate.finish_reason.as_deref() {
                                state.saw_finish = true;
                                state.finish_reason = Self::finish_reason_of(sr);
                            }
                            if let Some(content) = candidate.content {
                                for (idx, part) in content.parts.into_iter().enumerate() {
                                    if let Some(t) = part.text {
                                        // Previous text is always a prefix of
                                        // the new text; emit only the delta.
                                        let delta = match state.part_texts.get(idx) {
                                            Some(prev) => {
                                                t.strip_prefix(prev).unwrap_or(&t).to_string()
                                            }
                                            None => t.clone(),
                                        };
                                        if state.part_texts.len() <= idx {
                                            state.part_texts.push(t);
                                        } else {
                                            state.part_texts[idx] = t;
                                        }
                                        if delta.is_empty() {
                                            continue;
                                        }
                                        if part.thought == Some(true) {
                                            // Thinking-mode part: reasoning
                                            // delta, never visible assistant
                                            // text. Mirror the text-delta
                                            // suffix logic per part index.
                                            let prev = state.reasoning_parts.get(idx);
                                            let rdelta = match prev {
                                                Some(prev) => {
                                                    let full = state.part_texts[idx].clone();
                                                    full.strip_prefix(prev)
                                                        .unwrap_or(&delta)
                                                        .to_string()
                                                }
                                                None => delta.clone(),
                                            };
                                            if !rdelta.is_empty() {
                                                if state.reasoning_parts.len() <= idx {
                                                    state
                                                        .reasoning_parts
                                                        .push(state.part_texts[idx].clone());
                                                } else {
                                                    state.reasoning_parts[idx] =
                                                        state.part_texts[idx].clone();
                                                }
                                                let r =
                                                    chunk.reasoning.get_or_insert_with(String::new);
                                                r.push_str(&rdelta);
                                            }
                                        } else {
                                            state.accumulated_text.push_str(&delta);
                                            let c = chunk.text.get_or_insert_with(String::new);
                                            c.push_str(&delta);
                                        }
                                    }
                                    if let Some(fc) = part.function_call
                                        && let Some(name) = fc.name
                                    {
                                        while state.tool_calls_acc.len() <= idx {
                                            state.tool_calls_acc.push(CanonicalToolCall {
                                                id: format!("call_{}", state.tool_calls_acc.len()),
                                                name: String::new(),
                                                arguments: Value::Null,
                                            });
                                        }
                                        state.tool_calls_acc[idx].name = name;
                                        if let Some(args) = fc.args {
                                            state.tool_calls_acc[idx].arguments = args;
                                        }
                                    }
                                }
                            }
                        }
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
impl LlmClient for GeminiAdapter {
    fn style(&self) -> &'static str {
        "gemini"
    }

    async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, Vec::new()).await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools).await
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
            .headers(self.build_headers())
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
        })
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
    fn build_headers_uses_goog_api_key_by_default() {
        let ep = ModelEndpoint {
            api_key: "AIza-test".into(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        let headers = client.build_headers();
        assert_eq!(
            headers.get("x-goog-api-key").unwrap().to_str().unwrap(),
            "AIza-test"
        );
        assert!(!headers.contains_key("authorization"));
    }

    #[test]
    fn build_headers_respects_custom_auth_scheme() {
        let ep = ModelEndpoint {
            api_key: "key".into(),
            auth_header_name: "Authorization".into(),
            auth_header_prefix: "Bearer".into(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        assert!(client.build_headers().get("x-goog-api-key").is_some());

        let ep = ModelEndpoint {
            api_key: "key".into(),
            auth_header_name: "X-Gateway-Key".into(),
            auth_header_prefix: String::new(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        assert!(client.build_headers().get("x-gateway-key").is_some());
        assert!(client.build_headers().get("x-goog-api-key").is_none());
    }

    #[test]
    fn api_base_handles_v1beta_suffix() {
        let ep = ModelEndpoint {
            base_url: "https://generativelanguage.googleapis.com".into(),
            model_name: "gemini-2.5-flash".into(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        assert_eq!(
            client.generate_url(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent"
        );
        assert_eq!(
            client.stream_generate_url(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        );

        let ep = ModelEndpoint {
            base_url: "https://host/v1beta".into(),
            model_name: "gemini-2.5-flash".into(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        assert_eq!(
            client.generate_url(),
            "https://host/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[test]
    fn convert_contents_extracts_system_instruction() {
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::System,
                content: vec![ContentPart::text("be concise")],
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
        let (contents, system) = GeminiAdapter::convert_contents(msgs);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        assert_eq!(contents[0].parts[0].text.as_deref(), Some("hi"));
        let sys = system.unwrap();
        assert_eq!(sys["parts"][0]["text"], "be concise");
    }

    #[test]
    fn convert_contents_tool_result_function_response() {
        // Without a preceding assistant declaration the call id is the only
        // name available; use it as the fallback.
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text("result body")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "user");
        let fr = contents[0].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr["name"], "call_1");
        assert_eq!(fr["response"]["result"], "result body");
    }

    #[test]
    fn convert_contents_tool_result_uses_function_name_of_matching_call() {
        // Gemini requires functionResponse.name to match the original
        // functionCall.name; the local call id (call_N) must never leak
        // into the response name. Build the assistant declaration first,
        // then the tool result referencing the same call id.
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("checking")],
                tool_call_id: None,
                tool_calls: Some(vec![CanonicalToolCall {
                    id: "call_0".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "/tmp"}),
                }]),
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("file contents")],
                tool_call_id: Some("call_0".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        assert_eq!(contents.len(), 2);
        let fr = contents[1].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr["name"], "read_file");
        assert_eq!(fr["response"]["result"], "file contents");
        // The assistant's functionCall keeps the function name (never the id).
        let fc = contents[0].parts[1].function_call.as_ref().unwrap();
        assert_eq!(fc["name"], "read_file");
    }

    #[test]
    fn convert_contents_parallel_same_tool_results_follow_declaration_order() {
        // Two parallel calls to the SAME tool: Gemini pairs functionResponse
        // parts with functionCall parts by name + position, so results must
        // be emitted in DECLARATION order even when the canonical holds them
        // in completion order (c2 finished first).
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("doing")],
                tool_call_id: None,
                tool_calls: Some(vec![
                    CanonicalToolCall {
                        id: "c1".into(),
                        name: "shell".into(),
                        arguments: serde_json::json!({"cmd": "echo one"}),
                    },
                    CanonicalToolCall {
                        id: "c2".into(),
                        name: "shell".into(),
                        arguments: serde_json::json!({"cmd": "echo two"}),
                    },
                ]),
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out-two")],
                tool_call_id: Some("c2".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out-one")],
                tool_call_id: Some("c1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        assert_eq!(contents.len(), 3);
        // First result in the emitted stream belongs to c1 (the first
        // declared call), even though c2 completed first.
        let fr1 = contents[1].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr1["name"], "shell");
        assert_eq!(fr1["response"]["result"], "out-one");
        let fr2 = contents[2].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr2["response"]["result"], "out-two");
    }

    #[test]
    fn convert_contents_multiple_tool_results_map_their_own_call_names() {
        let msgs = vec![
            CanonicalMessage {
                role: CanonicalRole::Assistant,
                content: vec![ContentPart::text("doing")],
                tool_call_id: None,
                tool_calls: Some(vec![
                    CanonicalToolCall {
                        id: "c1".into(),
                        name: "shell".into(),
                        arguments: serde_json::json!({}),
                    },
                    CanonicalToolCall {
                        id: "c2".into(),
                        name: "ask".into(),
                        arguments: serde_json::json!({}),
                    },
                ]),
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out1")],
                tool_call_id: Some("c1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out2")],
                tool_call_id: Some("c2".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            },
        ];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        let fr1 = contents[1].parts[0].function_response.as_ref().unwrap();
        let fr2 = contents[2].parts[0].function_response.as_ref().unwrap();
        assert_eq!(fr1["name"], "shell");
        assert_eq!(fr2["name"], "ask");
    }

    #[test]
    fn convert_contents_assistant_function_call() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("checking")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "call_2".into(),
                name: "file".into(),
                arguments: serde_json::json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        }];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "model");
        let fc = contents[0].parts[1].function_call.as_ref().unwrap();
        assert_eq!(fc["name"], "file");
        assert_eq!(fc["args"]["operation"], "read");
    }

    #[test]
    fn convert_contents_image_and_audio_inline_data() {
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
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        let inline = contents[0].parts[0].inline_data.as_ref().unwrap();
        assert_eq!(inline["mime_type"], "image/png");
        assert_eq!(inline["data"], "aGVsbG8=");
        let inline = contents[0].parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline["mime_type"], "audio/wav");
        assert_eq!(inline["data"], "d3d3");
    }

    #[test]
    fn parse_response_thought_parts_route_to_reasoning_not_text() {
        // Gemini 2.5 thinking mode returns `"thought": true` parts. They must
        // never leak into the visible assistant text; they surface as reasoning.
        let json = GeminiResponse {
            candidates: Some(vec![GeminiCandidate {
                content: Some(GeminiResponseContent {
                    parts: vec![
                        GeminiResponsePart {
                            text: Some("I should read the file first.".into()),
                            function_call: None,
                            thought: Some(true),
                        },
                        GeminiResponsePart {
                            text: Some("Final answer.".into()),
                            function_call: None,
                            thought: Some(false),
                        },
                    ],
                }),
                finish_reason: Some("STOP".into()),
            }]),
            usage_metadata: None,
            model_version: None,
        };
        let client = GeminiAdapter::new(ModelEndpoint::default());
        let resp = client.parse_response(json, None).unwrap();
        assert_eq!(resp.text, "Final answer.");
        assert_eq!(
            resp.reasoning.as_deref(),
            Some("I should read the file first.")
        );
    }

    #[test]
    fn build_request_body_with_tools_and_config() {
        let ep = ModelEndpoint {
            model_name: "gemini-2.5-flash".into(),
            max_tokens: 2048,
            temperature: 0.2,
            top_p: Some(0.9),
            top_k: Some(40),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "search".into(),
                description: "search the web".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let body = client.build_request_body(vec![], tools, false);
        let gtools = body.tools.unwrap();
        assert_eq!(gtools[0].function_declarations[0].name, "search");
        let cfg = body.generation_config.unwrap();
        assert_eq!(cfg.max_output_tokens, 2048);
        assert_eq!(cfg.temperature, 0.2);
        assert_eq!(cfg.top_p, Some(0.9));
        assert_eq!(cfg.top_k, Some(40));
    }

    #[test]
    fn finish_reason_mapping() {
        assert_eq!(
            GeminiAdapter::finish_reason_of("STOP"),
            Some(FinishReason::Stop)
        );
        assert_eq!(
            GeminiAdapter::finish_reason_of("MAX_TOKENS"),
            Some(FinishReason::Length)
        );
        assert_eq!(
            GeminiAdapter::finish_reason_of("SAFETY"),
            Some(FinishReason::ContentFilter)
        );
        assert_eq!(
            GeminiAdapter::finish_reason_of("RECITATION"),
            Some(FinishReason::ContentFilter)
        );
        assert_eq!(
            GeminiAdapter::finish_reason_of("MALFORMED_FUNCTION_CALL"),
            Some(FinishReason::ToolCalls)
        );
    }

    #[test]
    fn parse_response_text_tool_call_usage() {
        let json = GeminiResponse {
            candidates: Some(vec![GeminiCandidate {
                content: Some(GeminiResponseContent {
                    parts: vec![
                        GeminiResponsePart {
                            text: Some("checking".into()),
                            function_call: None,
                            thought: None,
                        },
                        GeminiResponsePart {
                            text: None,
                            function_call: Some(GeminiFunctionCall {
                                name: Some("file".into()),
                                args: Some(json!({"operation": "read"})),
                            }),
                            thought: None,
                        },
                    ],
                }),
                finish_reason: Some("STOP".into()),
            }]),
            usage_metadata: Some(GeminiUsage {
                prompt_tokens: 10,
                candidates_tokens: 5,
                total_tokens: 15,
            }),
            model_version: Some("gemini-2.5-flash".into()),
        };
        let ep = ModelEndpoint::default();
        let client = GeminiAdapter::new(ep);
        let resp = client
            .parse_response(json, Some("gemini-2.5-flash".into()))
            .unwrap();
        assert_eq!(resp.text, "checking");
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "file");
        assert_eq!(resp.tool_calls[0].arguments["operation"], "read");
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage.total_tokens, 15);
        assert_eq!(resp.model.as_deref(), Some("gemini-2.5-flash"));
    }
}
