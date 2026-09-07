use async_trait::async_trait;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::pin::Pin;
use std::time::Duration;

use crate::adapters::{
    LineMode, WebSearchMode, build_client, build_headers, empty_chunk, health_check_request,
    normalize_web_search_call_item, resolve_web_search_mode, send_request, spawn_line_reader,
    stream_header_timeout,
};
use crate::client::LlmClient;
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    CacheAccounting, CacheDiagnostics, Embedding, FinishReason, LlmError, LlmResponse, StreamChunk,
    SttResult, ToolDefinition, Usage,
};
use base64::Engine;
use haven_common::config::ModelEndpoint;
#[cfg(test)]
use haven_common::prompts::SESSION_CONTEXT_FENCE_START;
use haven_common::prompts::{STT_SYSTEM_PROMPT, split_system_prompt_cache_boundary};

// ---------------------------------------------------------------------------
// Gemini generateContent request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(rename = "inlineData", skip_serializing_if = "Option::is_none")]
    inline_data: Option<Value>,
    #[serde(rename = "functionCall", skip_serializing_if = "Option::is_none")]
    function_call: Option<Value>,
    #[serde(rename = "functionResponse", skip_serializing_if = "Option::is_none")]
    function_response: Option<Value>,
    /// Opaque Gemini thought signature that must be echoed on the same Part
    /// in a later stateless request.
    #[serde(rename = "thoughtSignature", skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
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

/// Gemini tools are a heterogeneous list: function declarations and built-in
/// tools such as `google_search` grounding share the same `tools[]` array.
#[derive(Debug, Serialize)]
#[serde(untagged)]
enum GeminiTool {
    Functions {
        #[serde(rename = "functionDeclarations")]
        function_declarations: Vec<GeminiFunctionDeclaration>,
    },
    GoogleSearch {
        #[serde(rename = "googleSearch")]
        google_search: Value,
    },
}

#[derive(Debug, Serialize)]
struct GeminiGenerationConfig {
    temperature: f32,
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: u32,
    #[serde(rename = "topP", skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(rename = "topK", skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    #[serde(rename = "stopSequences", skip_serializing_if = "Option::is_none")]
    stop_sequences: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    system_instruction: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
    #[serde(rename = "generationConfig", skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip)]
    cache_diagnostics: CacheDiagnostics,
}

// Response types: `text` and `function_call` parts, plus usage metadata.
#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata", alias = "usage_metadata", default)]
    usage_metadata: Option<GeminiUsage>,
    #[serde(rename = "modelVersion", alias = "model_version", default)]
    model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiResponseContent>,
    #[serde(alias = "finishReason")]
    #[serde(alias = "finish_reason")]
    finish_reason: Option<String>,
    #[serde(default, alias = "groundingMetadata")]
    grounding_metadata: Option<Value>,
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
    #[serde(rename = "functionCall", alias = "function_call", default)]
    function_call: Option<GeminiFunctionCall>,
    /// Gemini thinking-mode marker: parts carrying `"thought": true` hold the
    /// model's internal reasoning and MUST NOT be shown as assistant text.
    #[serde(default)]
    thought: Option<bool>,
    /// Opaque signature returned by Gemini for thought-bearing parts and
    /// function calls. It must be echoed verbatim in the next request.
    #[serde(rename = "thoughtSignature", alias = "thought_signature", default)]
    thought_signature: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    #[serde(default)]
    id: Option<String>,
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
    #[serde(default, alias = "cachedContentTokenCount")]
    cached_tokens: u32,
    #[serde(default, alias = "thoughtsTokenCount")]
    thoughts_tokens: u32,
    #[serde(default, alias = "toolUsePromptTokenCount")]
    tool_use_prompt_tokens: u32,
}

impl GeminiUsage {
    fn to_usage(&self, model_name: Option<String>) -> Usage {
        let completion = self.candidates_tokens.saturating_add(self.thoughts_tokens);
        let mut prompt = self.prompt_tokens;
        let tool_use = self.tool_use_prompt_tokens;
        if tool_use > 0 {
            let folded = prompt.saturating_add(completion);
            if self.total_tokens == folded.saturating_add(tool_use) {
                prompt = prompt.saturating_add(tool_use);
            }
        }
        Usage::from_counts_with_accounting(
            prompt,
            completion,
            self.total_tokens,
            self.cached_tokens,
            0,
            CacheAccounting::Inclusive,
            model_name,
        )
    }
}

/// Google Gemini API adapter (`generateContent` / `streamGenerateContent`).
pub struct GeminiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    web_search_mode: WebSearchMode,
}

impl GeminiAdapter {
    pub fn try_new(endpoint: ModelEndpoint) -> Result<Self, LlmError> {
        let client = build_client(&endpoint)?;
        let web_search_mode = resolve_web_search_mode(&endpoint);
        Ok(Self {
            endpoint,
            client,
            web_search_mode,
        })
    }

    #[cfg(test)]
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self::try_new(endpoint).expect("valid test endpoint")
    }

    /// Gemini authenticates with `x-goog-api-key`. If the user customized
    /// `auth_header_name`/`auth_header_prefix`, respect the custom scheme
    /// instead (for gateways that expect `Authorization: Bearer …`).
    fn build_headers(&self) -> Result<HeaderMap, LlmError> {
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
            self.model_id()
        )
    }

    fn stream_generate_url(&self) -> String {
        format!(
            "{}/models/{}:streamGenerateContent?alt=sse",
            self.api_base(),
            self.model_id()
        )
    }

    fn model_id(&self) -> &str {
        self.endpoint.model_name.trim_start_matches("models/")
    }

    fn models_url(&self) -> String {
        format!("{}/models", self.api_base())
    }

    fn embed_model_id(&self) -> &str {
        self.endpoint.model_name.trim_start_matches("models/")
    }

    fn embed_url(&self) -> String {
        format!(
            "{}/models/{}:batchEmbedContents",
            self.api_base(),
            self.embed_model_id()
        )
    }

    const THOUGHT_SIGNATURE_TYPE: &'static str = "gemini_thought_signature";

    fn thought_signature_marker(part_type: &str, name: Option<&str>, signature: &str) -> Value {
        let mut marker = json!({
            "type": Self::THOUGHT_SIGNATURE_TYPE,
            "part_type": part_type,
            "signature": signature,
        });
        if let Some(name) = name {
            marker["name"] = json!(name);
        }
        marker
    }

    fn take_thought_signature(
        blocks: &[Value],
        used: &mut Vec<usize>,
        part_type: &str,
        name: Option<&str>,
    ) -> Option<String> {
        let index = blocks.iter().enumerate().find_map(|(idx, block)| {
            if used.contains(&idx)
                || block.get("type").and_then(Value::as_str) != Some(Self::THOUGHT_SIGNATURE_TYPE)
                || block.get("part_type").and_then(Value::as_str) != Some(part_type)
            {
                return None;
            }
            let marker_name = block.get("name").and_then(Value::as_str);
            if name.is_some() && marker_name != name {
                return None;
            }
            block.get("signature").and_then(Value::as_str).map(|_| idx)
        })?;
        used.push(index);
        blocks[index]
            .get("signature")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn capture_thought_signature(
        blocks: &mut Vec<Value>,
        part: &GeminiResponsePart,
        function_name: Option<&str>,
    ) {
        if let Some(signature) = part.thought_signature.as_deref() {
            let part_type = if function_name.is_some() {
                "function_call"
            } else {
                "content"
            };
            let marker = Self::thought_signature_marker(part_type, function_name, signature);
            if !blocks.iter().any(|existing| existing == &marker) {
                blocks.push(marker);
            }
        }
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
        for mut m in msgs {
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
                        if m.role == CanonicalRole::User {
                            m.content =
                                crate::adapters::apply_wire_inject_prefix(m.source, m.content);
                        }
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
                    let mut used_signatures = Vec::new();
                    if let Some(signature) = Self::take_thought_signature(
                        &m.thinking_blocks,
                        &mut used_signatures,
                        "content",
                        None,
                    ) && let Some(part) = parts.last_mut()
                    {
                        part.thought_signature = Some(signature);
                    }
                    if let Some(calls) = &m.tool_calls {
                        declared_order.clear();
                        for tc in calls {
                            call_id_to_name.insert(tc.id.clone(), tc.name.clone());
                            declared_order.push(tc.id.clone());
                            let thought_signature = Self::take_thought_signature(
                                &m.thinking_blocks,
                                &mut used_signatures,
                                "function_call",
                                Some(&tc.name),
                            );
                            parts.push(GeminiPart {
                                text: None,
                                inline_data: None,
                                function_call: Some(json!({
                                    "id": tc.id,
                                    "name": tc.name,
                                    "args": tc.arguments
                                })),
                                function_response: None,
                                thought_signature,
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
            let text = system_parts.join("\n\n");
            let parts = if let Some((stable, dynamic)) = split_system_prompt_cache_boundary(&text) {
                vec![json!({"text": stable}), json!({"text": dynamic})]
            } else {
                vec![json!({"text": text})]
            };
            Some(json!({"parts": parts}))
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
                    function_response: Some({
                        let mut response = json!({
                            "name": name,
                            "response": {"result": text}
                        });
                        if !call_id.is_empty() {
                            response["id"] = json!(call_id);
                        }
                        response
                    }),
                    thought_signature: None,
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
                    thought_signature: None,
                },
                ContentPart::Image {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mimeType": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                },
                ContentPart::Audio {
                    media_type, data, ..
                } => GeminiPart {
                    text: None,
                    inline_data: Some(json!({
                        "mimeType": media_type,
                        "data": data
                    })),
                    function_call: None,
                    function_response: None,
                    thought_signature: None,
                },
            })
            .collect()
    }

    fn convert_tools(tools: Vec<ToolDefinition>) -> Vec<GeminiTool> {
        tools
            .into_iter()
            .map(|t| GeminiTool::Functions {
                function_declarations: vec![GeminiFunctionDeclaration {
                    name: t.function.name,
                    description: t.function.description,
                    parameters: crate::types::project_tool_parameters_for_gemini(
                        t.function.parameters,
                    ),
                }],
            })
            .collect()
    }

    #[cfg(test)]
    fn build_request_body(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        _stream: bool,
    ) -> GeminiRequest {
        self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            self.web_search_mode,
            self.endpoint.max_tokens,
        )
    }

    #[cfg(test)]
    fn build_request_body_with_mode(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        web_search_mode: WebSearchMode,
    ) -> GeminiRequest {
        self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            web_search_mode,
            self.endpoint.max_tokens,
        )
    }

    fn build_request_body_with_mode_and_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        web_search_mode: WebSearchMode,
        max_output_tokens: u32,
    ) -> GeminiRequest {
        let system_split = messages.iter().any(|message| {
            message.role == CanonicalRole::System
                && message.content.iter().any(|part| {
                    matches!(part, ContentPart::Text(text) if split_system_prompt_cache_boundary(text).is_some())
                })
        });
        let (contents, system_instruction) = Self::convert_contents(messages);
        let mut tools_json = Self::convert_tools(tools);
        // Gemini grounding: append `{"google_search": {}}`. Auto and Always
        // both expose the tool (Gemini has no forced-search tool_choice
        // equivalent for google_search); Always still opts the model in.
        if !matches!(web_search_mode, WebSearchMode::Off) {
            tools_json.push(GeminiTool::GoogleSearch {
                google_search: json!({}),
            });
        }
        GeminiRequest {
            contents,
            system_instruction,
            tools: if tools_json.is_empty() {
                None
            } else {
                Some(tools_json)
            },
            generation_config: Some(GeminiGenerationConfig {
                temperature: self.endpoint.temperature,
                max_output_tokens,
                top_p: self.endpoint.top_p,
                top_k: self.endpoint.top_k,
                stop_sequences: self.endpoint.stop.clone(),
            }),
            cache_diagnostics: CacheDiagnostics::for_provider_cache(system_split),
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

    #[cfg(test)]
    fn parse_response(
        &self,
        json: GeminiResponse,
        model: Option<String>,
    ) -> Result<LlmResponse, LlmError> {
        self.parse_response_with_cache(json, model, CacheDiagnostics::default())
    }

    fn parse_response_with_cache(
        &self,
        json: GeminiResponse,
        model: Option<String>,
        cache_diagnostics: CacheDiagnostics,
    ) -> Result<LlmResponse, LlmError> {
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        let mut thinking_blocks = Vec::new();
        let mut finish_reason = None;
        if let Some(candidate) = json.candidates.and_then(|c| c.into_iter().next()) {
            finish_reason = candidate
                .finish_reason
                .as_deref()
                .and_then(Self::finish_reason_of);
            if let Some(content) = candidate.content {
                for part in content.parts {
                    let function_name = part
                        .function_call
                        .as_ref()
                        .and_then(|fc| fc.name.as_deref())
                        .map(str::to_string);
                    Self::capture_thought_signature(
                        &mut thinking_blocks,
                        &part,
                        function_name.as_deref(),
                    );
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
                            id: fc
                                .id
                                .unwrap_or_else(|| format!("call_{}", tool_calls.len())),
                            name,
                            arguments: fc.args.unwrap_or_default(),
                        });
                    }
                }
            }
        }
        let usage = json
            .usage_metadata
            .as_ref()
            .map(|u| {
                let mut usage = u.to_usage(model.clone());
                usage.cache_diagnostics = Some(
                    cache_diagnostics
                        .clone()
                        .with_provider_usage(usage.cached_tokens),
                );
                usage
            })
            .unwrap_or_else(|| Usage {
                cache_diagnostics: Some(cache_diagnostics),
                ..Default::default()
            });
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
            thinking_blocks,
        })
    }

    /// Fold Gemini `groundingMetadata` into a compact `web_search_call` (queries
    /// only — never the full grounding blob, which balloons transcript size).
    fn web_search_calls_from_metadata(meta: &Value) -> Vec<Value> {
        let queries = meta
            .get("webSearchQueries")
            .or_else(|| meta.get("web_search_queries"))
            .cloned()
            .unwrap_or_else(|| json!([]));
        vec![normalize_web_search_call_item(json!({
            "type": "web_search_call",
            "id": "gemini_grounding",
            "status": "completed",
            "action": {"type": "search", "queries": queries},
        }))]
    }

    fn web_search_calls_from_grounding(raw: &Value) -> Vec<Value> {
        let Some(meta) = raw
            .pointer("/candidates/0/groundingMetadata")
            .or_else(|| raw.pointer("/candidates/0/grounding_metadata"))
        else {
            return Vec::new();
        };
        Self::web_search_calls_from_metadata(meta)
    }

    async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, None).await
    }

    async fn chat_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        let body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let cache_diagnostics = body.cache_diagnostics.clone();
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
            .headers(self.build_headers()?)
            .json(&body);
        // §2.9: per-request timeout for non-streaming
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let raw: Value =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let web_search_calls = Self::web_search_calls_from_grounding(&raw);
        let json: GeminiResponse =
            serde_json::from_value(raw).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model_version.clone();
        let mut parsed = self.parse_response_with_cache(json, model, cache_diagnostics)?;
        parsed.web_search_calls = web_search_calls;
        Ok(parsed)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        self.chat_stream_inner_with_max_tokens(messages, tools, None)
            .await
    }

    async fn chat_stream_inner_with_max_tokens(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let body = self.build_request_body_with_mode_and_max_tokens(
            messages,
            tools,
            self.web_search_mode,
            max_output_tokens.unwrap_or(self.endpoint.max_tokens),
        );
        let cache_diagnostics = body.cache_diagnostics.clone();
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
            .headers(self.build_headers()?)
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
            rx: mpsc::UnboundedReceiver<Result<String, LlmError>>,
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
            /// Gemini thought signatures captured from response Parts. These
            /// are opaque provider state and are echoed on the next turn.
            thinking_blocks: Vec<Value>,
            accumulated_text: String,
            last_model: Option<String>,
            finish_reason: Option<FinishReason>,
            usage: Option<Usage>,
            saw_finish: bool,
            web_search_calls: Vec<Value>,
            cache_diagnostics: CacheDiagnostics,
        }

        let empty_chunk = empty_chunk;

        let mapped = futures_util::stream::unfold(
            UnfoldState {
                rx: chunk_rx,
                done: false,
                part_texts: Vec::new(),
                reasoning_parts: Vec::new(),
                tool_calls_acc: Vec::new(),
                thinking_blocks: Vec::new(),
                accumulated_text: String::new(),
                last_model: None,
                finish_reason: None,
                usage: None,
                saw_finish: false,
                web_search_calls: Vec::new(),
                cache_diagnostics,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(Ok(d)) => d,
                    Some(Err(error)) => {
                        state.done = true;
                        return Some((Err(error), state));
                    }
                    None => {
                        // Gemini delivers args as already-parsed JSON; a name
                        // with Null args and no finish means the stream died
                        // before arguments arrived. On a clean finish, omitted
                        // args mean `{}` (empty-parameter tools) — not Null.
                        let unfinished_tools = state
                            .tool_calls_acc
                            .iter()
                            .any(|tc| !tc.name.is_empty() && tc.arguments.is_null());
                        let chunk = if !state.saw_finish
                            && (!state.accumulated_text.is_empty() || unfinished_tools)
                        {
                            Err(LlmError::StreamTruncated)
                        } else {
                            let tool_calls = std::mem::take(&mut state.tool_calls_acc)
                                .into_iter()
                                .filter(|tc| !tc.name.is_empty())
                                .map(|mut tc| {
                                    if tc.arguments.is_null() {
                                        tc.arguments = serde_json::json!({});
                                    }
                                    tc
                                })
                                .collect();
                            Ok(StreamChunk {
                                text: None,
                                // Flush accumulated tool calls like the OpenAI
                                // adapter: per-delta chunks carry none, the
                                // final chunk carries all merged calls.
                                tool_calls,
                                finish_reason: state.finish_reason,
                                usage: state.usage.take(),
                                model: state.last_model.clone(),
                                reasoning: None,
                                web_search: None,
                                web_search_calls: std::mem::take(&mut state.web_search_calls),
                                thinking_blocks: std::mem::take(&mut state.thinking_blocks),
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
                            let mut usage = u.to_usage(state.last_model.clone());
                            usage.cache_diagnostics = Some(
                                state
                                    .cache_diagnostics
                                    .clone()
                                    .with_provider_usage(usage.cached_tokens),
                            );
                            state.usage = Some(usage);
                        }
                        let mut chunk = empty_chunk();
                        chunk.model = state.last_model.clone();
                        if let Some(candidate) = resp.candidates.and_then(|c| c.into_iter().next())
                        {
                            if let Some(sr) = candidate.finish_reason.as_deref() {
                                state.saw_finish = true;
                                state.finish_reason = Self::finish_reason_of(sr);
                            }
                            if let Some(meta) = candidate.grounding_metadata.as_ref() {
                                let calls = Self::web_search_calls_from_metadata(meta);
                                if !calls.is_empty() {
                                    state.web_search_calls = calls;
                                }
                            }
                            if let Some(content) = candidate.content {
                                for (idx, part) in content.parts.into_iter().enumerate() {
                                    let function_name = part
                                        .function_call
                                        .as_ref()
                                        .and_then(|fc| fc.name.as_deref())
                                        .map(str::to_string);
                                    Self::capture_thought_signature(
                                        &mut state.thinking_blocks,
                                        &part,
                                        function_name.as_deref(),
                                    );
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
                                        if !delta.is_empty() && part.thought == Some(true) {
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
                                        } else if !delta.is_empty() {
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
                                        if let Some(id) = fc.id {
                                            state.tool_calls_acc[idx].id = id;
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

    async fn chat_with_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, Vec::new(), max_output_tokens)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner(messages, tools).await
    }

    async fn chat_with_tools_output_cap(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        max_output_tokens: Option<u32>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat_inner_with_max_tokens(messages, tools, max_output_tokens)
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

    async fn embed(&self, input: Vec<String>) -> Result<Embedding, LlmError> {
        if input.is_empty() {
            return Ok(Embedding {
                vectors: Vec::new(),
                model: Some(self.endpoint.model_name.clone()),
                usage: Usage::default(),
            });
        }
        let model_resource = format!("models/{}", self.embed_model_id());
        let requests: Vec<Value> = input
            .iter()
            .map(|text| {
                json!({
                    "model": model_resource,
                    "content": { "parts": [{ "text": text }] }
                })
            })
            .collect();
        let body = json!({ "requests": requests });
        let url = self.embed_url();
        tracing::debug!("POST {} (embed model: {})", url, self.endpoint.model_name);
        let mut req = self
            .client
            .post(&url)
            .headers(self.build_headers()?)
            .json(&body);
        req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        let resp = send_request(req, None).await?;
        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        parse_gemini_embed_response(&txt, input.len(), &self.endpoint.model_name)
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
            .headers(self.build_headers()?)
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
            self.build_headers()?,
            self.endpoint.timeout_secs,
        )
        .await
    }
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedValues {
    values: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct GeminiEmbedResponse {
    #[serde(default)]
    embeddings: Vec<GeminiEmbedValues>,
    #[serde(default)]
    embedding: Option<GeminiEmbedValues>,
}

fn parse_gemini_embed_response(
    body: &str,
    requested: usize,
    requested_model: &str,
) -> Result<Embedding, LlmError> {
    let json: GeminiEmbedResponse =
        serde_json::from_str(body).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let mut vectors: Vec<Vec<f32>> = json
        .embeddings
        .into_iter()
        .map(|item| item.values)
        .collect();
    if vectors.is_empty()
        && let Some(single) = json.embedding
    {
        vectors.push(single.values);
    }
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
    Ok(Embedding {
        vectors,
        model: Some(requested_model.to_string()),
        usage: Usage::default(),
    })
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
        let headers = client.build_headers().unwrap();
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
        assert!(
            client
                .build_headers()
                .unwrap()
                .get("x-goog-api-key")
                .is_some()
        );

        let ep = ModelEndpoint {
            api_key: "key".into(),
            auth_header_name: "X-Gateway-Key".into(),
            auth_header_prefix: String::new(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        assert!(
            client
                .build_headers()
                .unwrap()
                .get("x-gateway-key")
                .is_some()
        );
        assert!(
            client
                .build_headers()
                .unwrap()
                .get("x-goog-api-key")
                .is_none()
        );
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
    fn embed_url_strips_models_prefix() {
        let ep = ModelEndpoint {
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            model_name: "models/text-embedding-004".into(),
            ..Default::default()
        };
        let client = GeminiAdapter::new(ep);
        assert_eq!(
            client.embed_url(),
            "https://generativelanguage.googleapis.com/v1beta/models/text-embedding-004:batchEmbedContents"
        );
    }

    #[test]
    fn parse_gemini_embed_response_batch_and_single() {
        let batch = r#"{"embeddings":[{"values":[0.1,0.2]},{"values":[0.3]}]}"#;
        let emb = parse_gemini_embed_response(batch, 2, "text-embedding-004").unwrap();
        assert_eq!(emb.vectors, vec![vec![0.1, 0.2], vec![0.3]]);

        let single = r#"{"embedding":{"values":[1.0,2.0]}}"#;
        let emb = parse_gemini_embed_response(single, 1, "m").unwrap();
        assert_eq!(emb.vectors, vec![vec![1.0, 2.0]]);
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
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::User,
                content: vec![ContentPart::text("hi")],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
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
    fn convert_contents_separates_dynamic_system_context() {
        let system = format!(
            "stable instructions{SESSION_CONTEXT_FENCE_START}Current session: inspect cache"
        );
        let (_, system) = GeminiAdapter::convert_contents(vec![CanonicalMessage::system(vec![
            ContentPart::text(system),
        ])]);

        let system = system.unwrap();
        assert_eq!(system["parts"].as_array().unwrap().len(), 2);
        assert_eq!(system["parts"][0]["text"], "stable instructions");
        assert_eq!(
            system["parts"][1]["text"],
            format!("{SESSION_CONTEXT_FENCE_START}Current session: inspect cache")
        );
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
            source: None,
            id: None,
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
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("file contents")],
                tool_call_id: Some("call_0".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
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
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out-two")],
                tool_call_id: Some("c2".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out-one")],
                tool_call_id: Some("c1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
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
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out1")],
                tool_call_id: Some("c1".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
            },
            CanonicalMessage {
                role: CanonicalRole::Tool,
                content: vec![ContentPart::text("out2")],
                tool_call_id: Some("c2".into()),
                tool_calls: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
                source: None,
                id: None,
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
            source: None,
            id: None,
        }];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0].role, "model");
        let fc = contents[0].parts[1].function_call.as_ref().unwrap();
        assert_eq!(fc["name"], "file");
        assert_eq!(fc["args"]["operation"], "read");
    }

    #[test]
    fn convert_contents_echoes_function_call_thought_signature() {
        let msgs = vec![CanonicalMessage {
            role: CanonicalRole::Assistant,
            content: vec![ContentPart::text("checking")],
            tool_call_id: None,
            tool_calls: Some(vec![CanonicalToolCall {
                id: "fc_1".into(),
                name: "file".into(),
                arguments: json!({"operation": "read"}),
            }]),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: vec![json!({
                "type": "gemini_thought_signature",
                "part_type": "function_call",
                "name": "file",
                "signature": "sig_1"
            })],
            source: None,
            id: None,
        }];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        let wire = serde_json::to_value(&contents[0]).unwrap();
        assert_eq!(wire["parts"][1]["functionCall"]["id"], "fc_1");
        assert_eq!(wire["parts"][1]["thoughtSignature"], "sig_1");
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
            source: None,
            id: None,
        }];
        let (contents, _) = GeminiAdapter::convert_contents(msgs);
        let inline = contents[0].parts[0].inline_data.as_ref().unwrap();
        assert_eq!(inline["mimeType"], "image/png");
        assert_eq!(inline["data"], "aGVsbG8=");
        let inline = contents[0].parts[1].inline_data.as_ref().unwrap();
        assert_eq!(inline["mimeType"], "audio/wav");
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
                            thought_signature: None,
                        },
                        GeminiResponsePart {
                            text: Some("Final answer.".into()),
                            function_call: None,
                            thought: Some(false),
                            thought_signature: None,
                        },
                    ],
                }),
                finish_reason: Some("STOP".into()),
                grounding_metadata: None,
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
        match &gtools[0] {
            GeminiTool::Functions {
                function_declarations,
            } => assert_eq!(function_declarations[0].name, "search"),
            GeminiTool::GoogleSearch { .. } => panic!("expected function tool"),
        }
        let cfg = body.generation_config.unwrap();
        assert_eq!(cfg.max_output_tokens, 2048);
        assert_eq!(cfg.temperature, 0.2);
        assert_eq!(cfg.top_p, Some(0.9));
        assert_eq!(cfg.top_k, Some(40));
    }

    #[test]
    fn convert_tools_projects_gemini_schema_subset() {
        let tools = GeminiAdapter::convert_tools(vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "schedule".into(),
                description: "schedule an action".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "operation": { "type": "string", "enum": ["list", "set"] },
                        "delay_secs": { "type": "integer", "minimum": 1 }
                    },
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": { "operation": { "const": "list" } }
                        },
                        {
                            "type": "object",
                            "properties": { "operation": { "const": "set" } }
                        }
                    ]
                }),
            },
        }]);

        let GeminiTool::Functions {
            function_declarations,
        } = &tools[0]
        else {
            panic!("expected function declaration");
        };
        let parameters = &function_declarations[0].parameters;
        assert_eq!(parameters["type"], "object");
        assert!(parameters.get("oneOf").is_none());
        assert_eq!(
            parameters["properties"]["operation"]["enum"],
            json!(["list", "set"])
        );
        assert!(
            parameters["properties"]["delay_secs"]
                .get("minimum")
                .is_none()
        );
    }

    #[test]
    fn google_search_tool_injected_when_web_search_on() {
        let client = GeminiAdapter::new(ModelEndpoint::default());
        let body = client.build_request_body_with_mode(vec![], vec![], WebSearchMode::Auto);
        let tools = body.tools.expect("google_search present");
        assert!(matches!(tools[0], GeminiTool::GoogleSearch { .. }));
        let off = client.build_request_body_with_mode(vec![], vec![], WebSearchMode::Off);
        assert!(off.tools.is_none());
    }

    #[test]
    fn grounding_metadata_keeps_queries_only() {
        let raw = json!({
            "candidates": [{
                "groundingMetadata": {
                    "webSearchQueries": ["haven voice"],
                    "groundingChunks": [{"huge": "blob".repeat(100)}],
                }
            }]
        });
        let calls = GeminiAdapter::web_search_calls_from_grounding(&raw);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["action"]["queries"], json!(["haven voice"]));
        assert!(calls[0]["action"].get("grounding_metadata").is_none());
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
                            thought_signature: None,
                        },
                        GeminiResponsePart {
                            text: None,
                            function_call: Some(GeminiFunctionCall {
                                id: None,
                                name: Some("file".into()),
                                args: Some(json!({"operation": "read"})),
                            }),
                            thought: None,
                            thought_signature: None,
                        },
                    ],
                }),
                finish_reason: Some("STOP".into()),
                grounding_metadata: None,
            }]),
            usage_metadata: Some(GeminiUsage {
                prompt_tokens: 10,
                candidates_tokens: 5,
                total_tokens: 15,
                ..Default::default()
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

    #[test]
    fn parse_response_captures_official_function_call_signature_and_id() {
        let raw = json!({
            "candidates": [{
                "content": {"parts": [{
                    "functionCall": {
                        "id": "fc_1",
                        "name": "file",
                        "args": {"operation": "read"}
                    },
                    "thoughtSignature": "sig_1"
                }]},
                "finishReason": "STOP"
            }],
            "usageMetadata": {"promptTokenCount": 2, "candidatesTokenCount": 3, "totalTokenCount": 5},
            "modelVersion": "gemini-3.7-flash"
        });
        let response: GeminiResponse = serde_json::from_value(raw).unwrap();
        let client = GeminiAdapter::new(ModelEndpoint::default());
        let parsed = client
            .parse_response(response, Some("gemini-3.7-flash".into()))
            .unwrap();
        assert_eq!(parsed.tool_calls[0].id, "fc_1");
        assert_eq!(parsed.thinking_blocks[0]["signature"], "sig_1");
        assert_eq!(parsed.model.as_deref(), Some("gemini-3.7-flash"));
    }

    #[test]
    fn serialized_request_uses_gemini_rest_wire_names() {
        let client = GeminiAdapter::new(ModelEndpoint {
            model_name: "gemini-3.7-flash".into(),
            ..Default::default()
        });
        let body = client.build_request_body(
            vec![CanonicalMessage::user_text("hello")],
            vec![ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "file".into(),
                    description: "read a file".into(),
                    parameters: json!({"type": "object"}),
                },
            }],
            false,
        );
        let wire = serde_json::to_value(body).unwrap();
        assert!(wire["generationConfig"]["maxOutputTokens"].is_number());
        assert!(wire["generationConfig"].get("max_output_tokens").is_none());
        assert!(wire["tools"][0]["functionDeclarations"].is_array());
        assert!(wire["tools"][0].get("function_declarations").is_none());
        assert!(wire["systemInstruction"].is_null());
    }

    #[test]
    fn generate_url_strips_models_prefix() {
        let client = GeminiAdapter::new(ModelEndpoint {
            model_name: "models/gemini-3.7-flash".into(),
            ..Default::default()
        });
        assert!(
            client
                .generate_url()
                .ends_with("/models/gemini-3.7-flash:generateContent")
        );
    }

    #[test]
    fn usage_folds_thoughts_and_tool_use_into_counts() {
        let u = GeminiUsage {
            prompt_tokens: 100,
            candidates_tokens: 20,
            thoughts_tokens: 80,
            tool_use_prompt_tokens: 15,
            total_tokens: 215,
            cached_tokens: 40,
        };
        let usage = u.to_usage(None);
        assert_eq!(usage.prompt_tokens, 115);
        assert_eq!(usage.completion_tokens, 100);
        assert_eq!(usage.total_tokens, 215);
        assert_eq!(usage.cached_tokens, 40);
        assert!(!usage.cache_exclusive_of_prompt());
        assert_eq!(usage.context_tokens(), 115);
    }
}
