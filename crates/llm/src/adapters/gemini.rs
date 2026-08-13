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
    spawn_line_reader,
};
use crate::client::LlmClient;
use crate::types::{
    ContentPart, FinishReason, LlmError, LlmMessage, LlmResponse, LlmRole, StreamChunk, ToolCall,
    ToolDefinition, Usage,
};
use haven_common::config::ModelEndpoint;

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
    fn convert_contents(msgs: Vec<LlmMessage>) -> (Vec<GeminiContent>, Option<Value>) {
        let mut system_parts: Vec<String> = Vec::new();
        let mut out: Vec<GeminiContent> = Vec::new();
        // Gemini's `functionResponse.name` must match the `functionCall.name`
        // of the original call (call ids are generated locally and never sent
        // to the API). Track the id -> function name mapping from assistant
        // tool calls so tool results reference the function name.
        let mut call_id_to_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for m in msgs {
            match m.role {
                LlmRole::System => {
                    for p in &m.content {
                        if let ContentPart::Text(t) = p {
                            system_parts.push(t.clone());
                        }
                    }
                }
                LlmRole::User | LlmRole::Tool => {
                    let is_tool_result =
                        matches!(m.role, LlmRole::Tool) || m.tool_call_id.is_some();
                    let mut parts = Vec::new();
                    if is_tool_result {
                        let call_id = m.tool_call_id.unwrap_or_default();
                        let name = call_id_to_name
                            .get(&call_id)
                            .cloned()
                            .unwrap_or_else(|| call_id.clone());
                        let text = Self::text_content(&m.content);
                        parts.push(GeminiPart {
                            text: None,
                            inline_data: None,
                            function_call: None,
                            function_response: Some(json!({
                                "name": name,
                                "response": {"result": text}
                            })),
                        });
                    } else {
                        parts.extend(Self::content_to_parts(&m.content));
                    }
                    if parts.is_empty() {
                        continue;
                    }
                    out.push(GeminiContent {
                        role: "user".into(),
                        parts,
                    });
                }
                LlmRole::Assistant => {
                    let mut parts = Self::content_to_parts(&m.content);
                    if let Some(calls) = &m.tool_calls {
                        for tc in calls {
                            call_id_to_name.insert(tc.id.clone(), tc.name.clone());
                            let args = serde_json::from_str(&tc.arguments).unwrap_or_else(|_| {
                                tracing::warn!(
                                    "tool call '{}' arguments are not valid JSON: {}",
                                    tc.name,
                                    tc.arguments
                                );
                                json!({})
                            });
                            parts.push(GeminiPart {
                                text: None,
                                inline_data: None,
                                function_call: Some(json!({
                                    "name": tc.name,
                                    "args": args
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
        let system = if system_parts.is_empty() {
            None
        } else {
            Some(json!({"parts": [{"text": system_parts.join("\n\n")}]}))
        };
        (out, system)
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
        messages: Vec<LlmMessage>,
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
        let mut tool_calls = Vec::new();
        let mut finish_reason = None;
        if let Some(candidate) = json.candidates.and_then(|c| c.into_iter().next()) {
            finish_reason = candidate
                .finish_reason
                .as_deref()
                .and_then(Self::finish_reason_of);
            if let Some(content) = candidate.content {
                for part in content.parts {
                    if let Some(t) = part.text {
                        text.push_str(&t);
                    }
                    if let Some(fc) = part.function_call
                        && let Some(name) = fc.name
                    {
                        tool_calls.push(ToolCall {
                            id: format!("call_{}", tool_calls.len()),
                            name,
                            arguments: fc
                                .args
                                .map(|v| serde_json::to_string(&v).unwrap_or_default())
                                .unwrap_or_default(),
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
            reasoning: None,
            web_search_calls: Vec::new(),
        })
    }

    async fn chat_inner(
        &self,
        messages: Vec<LlmMessage>,
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
        let resp = send_request(req).await?;

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
        messages: Vec<LlmMessage>,
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
        if let Some(timeout) = self.endpoint.timeout_streaming_secs {
            req = req.timeout(Duration::from_secs(timeout));
        }
        let resp = send_request(req).await?;

        use tokio::sync::mpsc;

        let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
        // `:streamGenerateContent?alt=sse` returns SSE frames; gateways that
        // ignore `alt=sse` fall back to raw JSON lines — both are handled.
        spawn_line_reader(resp.bytes_stream(), chunk_tx, LineMode::SseOrRaw);

        struct UnfoldState {
            rx: mpsc::UnboundedReceiver<String>,
            done: bool,
            /// Accumulated text per part index (deltas are emitted as suffixes).
            text_parts: Vec<String>,
            /// Accumulated tool calls per functionCall part index.
            tool_calls_acc: Vec<ToolCall>,
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
                text_parts: Vec::new(),
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
                                        state.accumulated_text.push_str(&t);
                                        // Previous text is always a prefix of
                                        // the new text; emit only the delta.
                                        let delta = if let Some(prev) = state.text_parts.get(idx) {
                                            t.strip_prefix(prev).unwrap_or(&t).to_string()
                                        } else {
                                            t.clone()
                                        };
                                        if state.text_parts.len() <= idx {
                                            state.text_parts.push(t);
                                        } else {
                                            state.text_parts[idx] = t;
                                        }
                                        if !delta.is_empty() {
                                            if chunk.text.is_none() {
                                                chunk.text = Some(String::new());
                                            }
                                            chunk.text.as_mut().unwrap().push_str(&delta);
                                        }
                                    }
                                    if let Some(fc) = part.function_call
                                        && let Some(name) = fc.name
                                    {
                                        while state.tool_calls_acc.len() <= idx {
                                            state.tool_calls_acc.push(ToolCall {
                                                id: format!("call_{}", state.tool_calls_acc.len()),
                                                name: String::new(),
                                                arguments: String::new(),
                                            });
                                        }
                                        state.tool_calls_acc[idx].name = name;
                                        if let Some(args) = fc.args {
                                            state.tool_calls_acc[idx].arguments =
                                                serde_json::to_string(&args).unwrap_or_default();
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

    async fn chat(&self, messages: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, Vec::new()).await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools).await
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
            LlmMessage {
                role: LlmRole::System,
                content: vec![ContentPart::text("be concise")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
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
        let msgs = vec![LlmMessage {
            role: LlmRole::Tool,
            content: vec![ContentPart::text("result body")],
            tool_call_id: Some("call_1".into()),
            tool_calls: None,
            reasoning: None,
            web_search_calls: Vec::new(),
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
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![ContentPart::text("checking")],
                tool_call_id: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_0".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"/tmp"}"#.into(),
                }]),
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::Tool,
                content: vec![ContentPart::text("file contents")],
                tool_call_id: Some("call_0".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
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
    fn convert_contents_multiple_tool_results_map_their_own_call_names() {
        let msgs = vec![
            LlmMessage {
                role: LlmRole::Assistant,
                content: vec![ContentPart::text("doing")],
                tool_call_id: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "shell".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "c2".into(),
                        name: "ask".into(),
                        arguments: "{}".into(),
                    },
                ]),
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::Tool,
                content: vec![ContentPart::text("out1")],
                tool_call_id: Some("c1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            },
            LlmMessage {
                role: LlmRole::Tool,
                content: vec![ContentPart::text("out2")],
                tool_call_id: Some("c2".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
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
        let msgs = vec![LlmMessage {
            role: LlmRole::Assistant,
            content: vec![ContentPart::text("checking")],
            tool_call_id: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_2".into(),
                name: "file".into(),
                arguments: r#"{"operation":"read"}"#.into(),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
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
            web_search_calls: Vec::new(),
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
                        },
                        GeminiResponsePart {
                            text: None,
                            function_call: Some(GeminiFunctionCall {
                                name: Some("file".into()),
                                args: Some(json!({"operation": "read"})),
                            }),
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
        assert!(resp.tool_calls[0].arguments.contains("\"read\""));
        assert_eq!(resp.finish_reason, Some(FinishReason::Stop));
        assert_eq!(resp.usage.total_tokens, 15);
        assert_eq!(resp.model.as_deref(), Some("gemini-2.5-flash"));
    }
}
