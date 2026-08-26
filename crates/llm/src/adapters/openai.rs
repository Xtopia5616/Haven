use async_trait::async_trait;
use futures_util::FutureExt;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::adapters::{
    WebSearchMode, build_client, build_headers, chat_thinking_extras, health_check_request,
    normalize_web_search_call_item, reasoning_tail, reasoning_text_from_thinking_blocks,
    requires_reasoning_echo, resolve_web_search_mode, send_request, stream_header_timeout,
    xai_search_mode,
};
use crate::client::LlmClient;
use haven_common::prompts::split_system_prompt_cache_boundary;
#[cfg(test)]
use haven_common::prompts::{MEMORY_FENCE_START, SESSION_CONTEXT_FENCE_START};
use haven_common::types::{CanonicalMessage, CanonicalRole, CanonicalToolCall, ContentPart};

use crate::types::{
    CacheAccounting, CacheDiagnostics, Embedding, FinishReason, LlmError, LlmResponse, StreamChunk,
    SttResult, ToolDefinition, Usage,
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
    /// Vendor thinking toggle (DeepSeek / Kimi): `{"type":"enabled|disabled"}`,
    /// optionally with Kimi `keep: "all"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// xAI Live Search (`search_parameters`). Omitted for non-xAI styles and
    /// when web search mode is `off`.
    #[serde(skip_serializing_if = "Option::is_none")]
    search_parameters: Option<Value>,
    /// Stable routing hint for OpenAI-compatible prompt caches. It does not
    /// alter the prompt; providers use it to keep matching prefixes on a cache
    /// shard. Unsupported gateways are detected and downgraded at runtime.
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip)]
    cache_diagnostics: CacheDiagnostics,
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
struct OpenAiPromptTokensDetails {
    #[serde(default)]
    cached_tokens: u32,
    #[serde(default)]
    cache_write_tokens: u32,
    #[serde(default)]
    cache_creation_tokens: u32,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiUsage {
    #[serde(default)]
    prompt_tokens: u32,
    /// Some OpenAI-compatible proxies emit Responses-style names on chat.
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
    /// OpenAI / compatible: nested cache hit count.
    #[serde(default)]
    prompt_tokens_details: Option<OpenAiPromptTokensDetails>,
    /// DeepSeek Chat Completions flat alias for cache hits.
    #[serde(default)]
    prompt_cache_hit_tokens: u32,
    /// Kimi / Moonshot top-level cache hit count.
    #[serde(default)]
    cached_tokens: u32,
    /// DeepSeek's reported normal (non-cache) input tokens.
    #[serde(default)]
    prompt_cache_miss_tokens: u32,
    #[serde(default)]
    cache_write_tokens: u32,
}

impl OpenAiUsage {
    fn prompt(&self) -> u32 {
        self.prompt_tokens.max(self.input_tokens)
    }

    fn completion(&self) -> u32 {
        self.completion_tokens.max(self.output_tokens)
    }

    fn cached(&self) -> u32 {
        super::resolve_cached_tokens(
            self.prompt_tokens_details.as_ref().map(|d| d.cached_tokens),
            self.prompt_cache_hit_tokens.max(self.cached_tokens),
        )
    }

    fn cache_created(&self) -> u32 {
        self.prompt_tokens_details
            .as_ref()
            .map(|details| {
                details
                    .cache_write_tokens
                    .max(details.cache_creation_tokens)
            })
            .unwrap_or(0)
            .max(self.cache_write_tokens)
    }

    fn to_usage(&self, model_name: Option<String>) -> Usage {
        let mut usage = Usage::from_counts_with_accounting(
            self.prompt(),
            self.completion(),
            self.total_tokens,
            self.cached(),
            self.cache_created(),
            CacheAccounting::Inclusive,
            model_name,
        );
        usage.cache_miss_tokens = self.prompt_cache_miss_tokens.max(usage.cache_miss_tokens());
        usage
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    #[serde(alias = "candidates")]
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
    model: Option<String>,
    /// xAI Live Search citation URLs (top-level on the final response).
    #[serde(default)]
    citations: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamResponse {
    #[serde(alias = "candidates")]
    choices: Vec<OpenAiChoice>,
    usage: Option<OpenAiUsage>,
    model: Option<String>,
    #[serde(default)]
    citations: Vec<String>,
}

/// True when `model` is a dedicated ASR id that speaks
/// `/audio/transcriptions` (OpenAI `whisper-1` / `gpt-4o-transcribe*`, Groq
/// `whisper-large-v3*`, SiliconFlow `FunAudioLLM/SenseVoiceSmall` /
/// `TeleAI/TeleSpeechASR`, local whisper.cpp aliases, …). Multimodal
/// chat-audio models (e.g. `gpt-4o-audio-preview`) return false so the
/// router can fall back to chat + `input_audio`.
pub(crate) fn is_whisper_model(model: &str) -> bool {
    let n = model.to_ascii_lowercase();
    n.contains("whisper")
        || n.contains("transcribe")
        || n.contains("sensevoice")
        || n.contains("asr")
}

/// OpenAI-compatible chat adapter: the common wire format spoken by OpenAI,
/// Ollama, vLLM, DeepSeek, llama.cpp, xAI Grok, and most third-party gateways.
///
/// When constructed via [`Self::new_with_style`] with `"xai"`, request bodies
/// may include xAI Live Search `search_parameters` driven by the endpoint's
/// `web_search` mode (`off` | `auto` | `always`).
pub struct OpenAiAdapter {
    endpoint: ModelEndpoint,
    client: reqwest::Client,
    /// Reported `LlmClient::style()` — `"openai-chat"` (default) or `"xai"`.
    style: &'static str,
    web_search_mode: WebSearchMode,
    /// Whether this endpoint accepts OpenAI's optional `prompt_cache_key`.
    /// A gateway rejection is remembered for this adapter so we only pay one
    /// compatibility retry instead of failing every request.
    prompt_cache_key_state: AtomicU8,
}

const PROMPT_CACHE_KEY_UNKNOWN: u8 = 0;
const PROMPT_CACHE_KEY_ENABLED: u8 = 1;
const PROMPT_CACHE_KEY_UNSUPPORTED: u8 = 2;

impl OpenAiAdapter {
    pub fn new(endpoint: ModelEndpoint) -> Self {
        Self::new_with_style(endpoint, "openai-chat")
    }

    pub fn new_with_style(endpoint: ModelEndpoint, style: &'static str) -> Self {
        let client = build_client(&endpoint);
        let web_search_mode = resolve_web_search_mode(&endpoint);
        Self {
            endpoint,
            client,
            style,
            web_search_mode,
            prompt_cache_key_state: AtomicU8::new(PROMPT_CACHE_KEY_UNKNOWN),
        }
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

    /// Derive a compact, deterministic cache routing key from the stable part
    /// of a ReAct conversation. The dynamic session-context suffix is
    /// intentionally excluded so identical agent instructions and tool schemas
    /// share a provider cache shard across sessions.
    fn prompt_cache_key(
        &self,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
    ) -> Option<String> {
        if self.prompt_cache_key_state.load(Ordering::Relaxed) == PROMPT_CACHE_KEY_UNSUPPORTED {
            return None;
        }

        let system = messages
            .iter()
            .find(|message| message.role == CanonicalRole::System)?;
        let mut hasher = Sha256::new();
        hasher.update(b"haven-prompt-cache-v1\0");
        hasher.update(self.endpoint.model_name.as_bytes());
        hasher.update([0]);

        let mut has_stable_system = false;
        for part in &system.content {
            if let ContentPart::Text(text) = part {
                let stable = split_system_prompt_cache_boundary(text)
                    .map(|(stable, _)| stable)
                    .unwrap_or(text);
                if !stable.trim().is_empty() {
                    hasher.update(stable.as_bytes());
                    hasher.update([0]);
                    has_stable_system = true;
                }
            }
        }
        if !has_stable_system {
            return None;
        }

        // Tool schemas are part of the provider cache key. Changing a loaded
        // MCP/Skill therefore gets a new routing key rather than contaminating
        // the old cache shard.
        let tool_bytes = serde_json::to_vec(tools).ok()?;
        hasher.update(tool_bytes);

        let digest = hasher.finalize();
        let fingerprint = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Some(format!("haven-v1-{fingerprint}"))
    }

    fn prompt_cache_key_rejected(error: &LlmError) -> bool {
        let LlmError::RequestFailed(message) = error else {
            return false;
        };
        let message = message.to_ascii_lowercase();
        message.contains("prompt_cache_key")
            && [
                "unknown",
                "unsupported",
                "unrecognized",
                "extra field",
                "extra fields",
                "additional propert",
                "not allowed",
                "unexpected",
                "invalid parameter",
            ]
            .iter()
            .any(|hint| message.contains(hint))
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
            .map(|mut m| {
                if m.role == CanonicalRole::User {
                    m.content = crate::adapters::apply_wire_inject_prefix(m.source, m.content);
                }
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
                    // Skip synthetic xAI citation markers — Live Search is
                    // driven by `search_parameters`, not call round-trip.
                    web_search_call: m
                        .web_search_calls
                        .into_iter()
                        .filter(|c| c.get("id").and_then(Value::as_str) != Some("xai_citations"))
                        .map(normalize_web_search_call_item)
                        .collect(),
                }
            })
            .collect()
    }

    /// Keep the stable system prefix byte-identical when cross-session memory
    /// refreshes. Canonical state remains one message; only the OpenAI wire
    /// representation receives the second, volatile system segment.
    fn split_system_memory(mut messages: Vec<CanonicalMessage>) -> (Vec<CanonicalMessage>, bool) {
        let Some(index) = messages
            .iter()
            .position(|message| message.role == CanonicalRole::System)
        else {
            return (messages, false);
        };
        let Some(ContentPart::Text(text)) = messages[index].content.first() else {
            return (messages, false);
        };
        let Some((stable, volatile)) = split_system_prompt_cache_boundary(text) else {
            return (messages, false);
        };
        if stable.trim().is_empty() || volatile.is_empty() {
            return (messages, false);
        }
        let stable = stable.to_string();
        let volatile = volatile.to_string();
        messages[index].content = vec![ContentPart::text(stable)];
        messages.insert(
            index + 1,
            CanonicalMessage::system(vec![ContentPart::text(volatile.to_string())]),
        );
        (messages, true)
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
        self.build_request_body_with_mode(messages, tools, stream, self.web_search_mode)
    }

    /// Request-body construction with an explicit web search mode (tests pin
    /// the mode without touching process-global env vars).
    fn build_request_body_with_mode(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
        web_search_mode: WebSearchMode,
    ) -> OpenAiRequest {
        let has_tools = !tools.is_empty();
        let prompt_cache_key = self.prompt_cache_key(&messages, &tools);
        let (messages, system_split) = Self::split_system_memory(messages);
        let cache_diagnostics =
            CacheDiagnostics::for_request(prompt_cache_key.is_some(), system_split);
        let (thinking, reasoning_effort) = chat_thinking_extras(&self.endpoint);
        let omit_temperature = reasoning_effort.is_some() || thinking.is_some();
        let search_parameters = if self.style == "xai" {
            xai_search_mode(web_search_mode).map(|mode| {
                serde_json::json!({
                    "mode": mode,
                    "return_citations": true,
                })
            })
        } else {
            None
        };
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
            // Reasoning / thinking modes reject or ignore non-default
            // temperature. Omit whenever effort or vendor thinking is pinned.
            temperature: (!omit_temperature).then_some(self.endpoint.temperature),
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
            reasoning_effort,
            thinking,
            stream_options: if stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
            search_parameters,
            prompt_cache_key,
            cache_diagnostics,
        }
    }

    fn parse_openai_response(
        &self,
        json: OpenAiResponse,
        model: Option<String>,
        cache_diagnostics: CacheDiagnostics,
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
        let mut web_search_calls: Vec<Value> = choice
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
        // xAI returns citation URLs at the top level; fold them into the
        // canonical web_search_calls list so multi-turn echo / UI cards work.
        if !json.citations.is_empty() {
            web_search_calls.push(normalize_web_search_call_item(serde_json::json!({
                "type": "web_search_call",
                "id": "xai_citations",
                "status": "completed",
                "action": {
                    "type": "search",
                    "queries": [],
                    "citations": json.citations,
                },
            })));
        }

        let usage = json
            .usage
            .map(|u| {
                let mut usage = u.to_usage(model.clone());
                usage.cache_miss_tokens = usage.cache_miss_tokens();
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

    async fn send_chat_request(
        &self,
        url: &str,
        body: &mut OpenAiRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        match self.send_chat_request_once(url, body, stream).await {
            Ok(response) => {
                if body.prompt_cache_key.is_some() {
                    let _ = self.prompt_cache_key_state.compare_exchange(
                        PROMPT_CACHE_KEY_UNKNOWN,
                        PROMPT_CACHE_KEY_ENABLED,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    );
                }
                Ok(response)
            }
            Err(error)
                if body.prompt_cache_key.is_some() && Self::prompt_cache_key_rejected(&error) =>
            {
                self.prompt_cache_key_state
                    .store(PROMPT_CACHE_KEY_UNSUPPORTED, Ordering::Relaxed);
                body.prompt_cache_key = None;
                body.cache_diagnostics.key_requested = false;
                body.cache_diagnostics.downgraded = true;
                body.cache_diagnostics.mode = if body.cache_diagnostics.system_split {
                    "split".into()
                } else {
                    "off".into()
                };
                tracing::warn!(
                    endpoint = %self.endpoint.base_url,
                    "endpoint rejected prompt_cache_key; disabled cache routing hint for this adapter"
                );
                self.send_chat_request_once(url, body, stream).await
            }
            Err(error) => Err(error),
        }
    }

    async fn send_chat_request_once(
        &self,
        url: &str,
        body: &OpenAiRequest,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let mut req = self
            .client
            .post(url)
            .headers(self.build_headers())
            .json(body);
        if stream {
            if let Some(timeout) = self.endpoint.timeout_streaming_secs {
                tracing::trace!("chat_stream_inner: {}s streaming timeout", timeout);
                req = req.timeout(Duration::from_secs(timeout));
            }
        } else {
            req = req.timeout(Duration::from_secs(self.endpoint.timeout_secs));
        }
        send_request(
            req,
            if stream {
                stream_header_timeout(self.endpoint.timeout_streaming_secs)
            } else {
                None
            },
        )
        .await
    }

    async fn chat_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
        stream: bool,
    ) -> Result<LlmResponse, LlmError> {
        let mut body = self.build_request_body(messages, tools, stream);
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
        let resp = self.send_chat_request(&url, &mut body, stream).await?;

        let txt = resp
            .text()
            .await
            .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        tracing::trace!("POST {} response body: {} chars", url, txt.len());
        let json: OpenAiResponse =
            serde_json::from_str(&txt).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
        let model = json.model.clone();
        self.parse_openai_response(json, model, body.cache_diagnostics)
    }

    async fn chat_stream_inner(
        &self,
        messages: Vec<CanonicalMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let mut body = self.build_request_body(messages, tools, true);
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
        let resp = self
            .send_chat_request(&url, &mut body, true)
            .await
            .map_err(|e| {
                tracing::debug!("chat_stream_inner: send() error: {:?}", e);
                e
            })?;
        tracing::debug!("chat_stream_inner response status: {}", resp.status());
        let cache_diagnostics = body.cache_diagnostics;

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
            cache_diagnostics: CacheDiagnostics,
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
                cache_diagnostics,
            },
            move |mut state| async move {
                if state.done {
                    return None;
                }
                let data = match state.rx.recv().await {
                    Some(d) => d,
                    None => {
                        // Interrupted mid-tool-call (no finish_reason): empty
                        // args after a name, structural-only repair, or
                        // mid-string JSON must not flush as executable calls.
                        let unfinished_tools =
                            state.tool_calls_acc.iter().any(|(_, name, args)| {
                                CanonicalToolCall::stream_tool_args_unfinished(name, args)
                            });
                        let chunk = if !state.has_finish_reason
                            && (!state.accumulated_text.is_empty() || unfinished_tools)
                        {
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
                            let mut usage = u.to_usage(state.last_model.clone());
                            usage.cache_miss_tokens = usage.cache_miss_tokens();
                            usage.cache_diagnostics = Some(
                                state
                                    .cache_diagnostics
                                    .clone()
                                    .with_provider_usage(usage.cached_tokens),
                            );
                            state.usage = Some(usage);
                        }
                        if !resp.citations.is_empty() {
                            crate::adapters::upsert_web_search_call(
                                &mut state.web_search_acc,
                                normalize_web_search_call_item(serde_json::json!({
                                    "type": "web_search_call",
                                    "id": "xai_citations",
                                    "status": "completed",
                                    "action": {
                                        "type": "search",
                                        "queries": [],
                                        "citations": resp.citations,
                                    },
                                })),
                            );
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
        self.style
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
        // Native `/audio/transcriptions` only for dedicated ASR models.
        // Multimodal chat models (e.g. gpt-4o-audio-preview) return
        // Unsupported so the router can fall back to chat + `input_audio`.
        if !is_whisper_model(&self.endpoint.model_name) {
            return Err(LlmError::UnsupportedCapability(format!(
                "model '{}' is not a native /audio/transcriptions model",
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
        super::openai_compatible_embed(
            &self.client,
            self.build_headers(),
            &super::openai_embeddings_url(&self.endpoint.base_url, false),
            &self.endpoint.model_name,
            self.endpoint.timeout_secs,
            input,
        )
        .await
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
                source: None,
                id: None,
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
            citations: Vec::new(),
        };
        let ep = ModelEndpoint::default();
        let adapter = OpenAiAdapter::new(ep);
        let resp = adapter
            .parse_openai_response(json, None, CacheDiagnostics::default())
            .unwrap();
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
        assert_eq!(
            headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-test"
        );
    }

    #[test]
    fn is_whisper_model_covers_transcribe_ids() {
        assert!(is_whisper_model("whisper-1"));
        assert!(is_whisper_model("gpt-4o-transcribe"));
        assert!(is_whisper_model("gpt-4o-mini-transcribe"));
        assert!(is_whisper_model("FunAudioLLM/SenseVoiceSmall"));
        assert!(is_whisper_model("TeleAI/TeleSpeechASR"));
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
    fn prompt_cache_key_is_stable_across_memory_refreshes() {
        let client = OpenAiAdapter::new(ModelEndpoint {
            model_name: "gpt-test".into(),
            ..Default::default()
        });
        let stable = "You are Haven.\nCurrent session: investigate cache\n";
        let first_system = CanonicalMessage::system(vec![ContentPart::text(format!(
            "{stable}{MEMORY_FENCE_START}first recalled fact"
        ))]);
        let refreshed_system = CanonicalMessage::system(vec![ContentPart::text(format!(
            "{stable}{MEMORY_FENCE_START}refreshed recalled fact"
        ))]);
        let user = CanonicalMessage::user_text("continue");
        let tools = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read".into(),
                description: "read a file".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
        }];

        let first = client
            .build_request_body(vec![first_system, user.clone()], tools.clone(), false)
            .prompt_cache_key;
        let refreshed = client
            .build_request_body(vec![refreshed_system, user], tools, false)
            .prompt_cache_key;

        assert!(first.is_some());
        assert_eq!(first, refreshed);
    }

    #[test]
    fn build_request_splits_memory_after_stable_system_prefix() {
        let client = OpenAiAdapter::new(ModelEndpoint::default());
        let body = client.build_request_body(
            vec![
                CanonicalMessage::system(vec![ContentPart::text(format!(
                    "stable instructions\n{MEMORY_FENCE_START}volatile fact"
                ))]),
                CanonicalMessage::user_text("session anchor"),
            ],
            Vec::new(),
            false,
        );

        assert!(body.cache_diagnostics.system_split);
        assert_eq!(body.messages.len(), 3);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(
            body.messages[0].content,
            Some(Value::String("stable instructions\n".into()))
        );
        assert_eq!(body.messages[1].role, "system");
        assert_eq!(
            body.messages[1].content,
            Some(Value::String(format!("{MEMORY_FENCE_START}volatile fact")))
        );
        assert_eq!(body.messages[2].role, "user");
    }

    #[test]
    fn prompt_cache_key_is_shared_across_dynamic_sessions() {
        let client = OpenAiAdapter::new(ModelEndpoint::default());
        let stable = "stable system";
        let first = client
            .build_request_body(
                vec![
                    CanonicalMessage::system(vec![ContentPart::text(format!(
                        "{stable}{SESSION_CONTEXT_FENCE_START}Current session: first"
                    ))]),
                    CanonicalMessage::user_text("first session"),
                ],
                Vec::new(),
                false,
            )
            .prompt_cache_key
            .unwrap();
        let second = client
            .build_request_body(
                vec![
                    CanonicalMessage::system(vec![ContentPart::text(format!(
                        "{stable}{SESSION_CONTEXT_FENCE_START}Current session: second"
                    ))]),
                    CanonicalMessage::user_text("second session"),
                ],
                Vec::new(),
                false,
            )
            .prompt_cache_key
            .unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn prompt_cache_key_changes_when_tools_change_or_is_unsupported() {
        let client = OpenAiAdapter::new(ModelEndpoint::default());
        let system = CanonicalMessage::system(vec![ContentPart::text("stable system")]);
        let user = CanonicalMessage::user_text("session anchor");
        let one_tool = vec![ToolDefinition {
            tool_type: "function".into(),
            function: ToolFunction {
                name: "read".into(),
                description: "read a file".into(),
                parameters: serde_json::json!({"type":"object"}),
            },
        }];
        let two_tools = vec![
            one_tool[0].clone(),
            ToolDefinition {
                tool_type: "function".into(),
                function: ToolFunction {
                    name: "write".into(),
                    description: "write a file".into(),
                    parameters: serde_json::json!({"type":"object"}),
                },
            },
        ];

        let first = client
            .build_request_body(vec![system.clone(), user.clone()], one_tool, false)
            .prompt_cache_key
            .unwrap();
        let changed = client
            .build_request_body(vec![system.clone(), user.clone()], two_tools, false)
            .prompt_cache_key
            .unwrap();
        assert_ne!(first, changed);

        client
            .prompt_cache_key_state
            .store(PROMPT_CACHE_KEY_UNSUPPORTED, Ordering::Relaxed);
        assert!(
            client
                .build_request_body(vec![system, user], Vec::new(), false)
                .prompt_cache_key
                .is_none()
        );
    }

    #[test]
    fn prompt_cache_key_rejection_detection_is_specific() {
        assert!(OpenAiAdapter::prompt_cache_key_rejected(
            &LlmError::RequestFailed("400: Unknown parameter: prompt_cache_key".into())
        ));
        assert!(OpenAiAdapter::prompt_cache_key_rejected(
            &LlmError::RequestFailed("invalid parameter 'prompt_cache_key'".into())
        ));
        assert!(OpenAiAdapter::prompt_cache_key_rejected(
            &LlmError::RequestFailed(
                "Additional properties are not allowed ('prompt_cache_key' was unexpected)".into()
            )
        ));
        assert!(!OpenAiAdapter::prompt_cache_key_rejected(
            &LlmError::RequestFailed("400: maximum context length exceeded".into())
        ));
    }

    #[tokio::test]
    async fn rejected_prompt_cache_key_retries_without_key_and_disables_it() {
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_keys = Arc::new(Mutex::new(Vec::new()));
        let seen_keys_server = Arc::clone(&seen_keys);
        let server = tokio::spawn(async move {
            for request_number in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                let header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                let body = String::from_utf8_lossy(&buf[header_end + 4..]);
                seen_keys_server
                    .lock()
                    .unwrap()
                    .push(body.contains("prompt_cache_key"));
                let (status, response) = if request_number == 0 {
                    (
                        "400 Bad Request",
                        r#"{"error":{"message":"Unknown parameter: prompt_cache_key"}}"#,
                    )
                } else {
                    (
                        "200 OK",
                        r#"{"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":1,"total_tokens":11},"model":"gpt-test"}"#,
                    )
                };
                let wire = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                socket.write_all(wire.as_bytes()).await.unwrap();
            }
        });

        let client = OpenAiAdapter::new(ModelEndpoint {
            base_url: format!("http://{addr}"),
            model_name: "gpt-test".into(),
            ..Default::default()
        });
        let messages = vec![
            CanonicalMessage::system(vec![ContentPart::text("stable system")]),
            CanonicalMessage::user_text("session anchor"),
        ];
        let response = client.chat(messages.clone()).await.unwrap();
        assert_eq!(response.text, "ok");
        assert!(
            client
                .build_request_body(messages, Vec::new(), false)
                .prompt_cache_key
                .is_none()
        );
        server.await.unwrap();
        assert_eq!(*seen_keys.lock().unwrap(), vec![true, false]);
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
    fn xai_search_parameters_follow_web_search_mode() {
        let ep = ModelEndpoint {
            api_style: Some("xai".into()),
            provider: "xai".into(),
            base_url: "https://api.x.ai/v1".into(),
            model_name: "grok-3".into(),
            web_search: Some("auto".into()),
            ..Default::default()
        };
        let client = OpenAiAdapter::new_with_style(ep, "xai");
        assert_eq!(client.style(), "xai");
        let auto = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Auto);
        assert_eq!(
            auto.search_parameters,
            Some(serde_json::json!({"mode": "auto", "return_citations": true}))
        );
        let always =
            client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Always);
        assert_eq!(
            always.search_parameters,
            Some(serde_json::json!({"mode": "on", "return_citations": true}))
        );
        let off = client.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Off);
        assert!(off.search_parameters.is_none());
        // Non-xAI style never injects search_parameters.
        let chat = OpenAiAdapter::new(ModelEndpoint::default());
        let body = chat.build_request_body_with_mode(vec![], vec![], false, WebSearchMode::Always);
        assert!(body.search_parameters.is_none());
    }

    #[test]
    fn build_request_body_deepseek_thinking_enabled_maps_medium() {
        let ep = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model_name: "deepseek-v4-pro".into(),
            temperature: 0.7,
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
        assert_eq!(body.thinking, Some(serde_json::json!({"type": "enabled"})));
        assert_eq!(body.reasoning_effort.as_deref(), Some("high"));
        assert!(body.temperature.is_none());
    }

    #[test]
    fn build_request_body_deepseek_thinking_disabled() {
        let ep = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            model_name: "deepseek-chat".into(),
            reasoning_effort: Some("disabled".into()),
            ..Default::default()
        };
        let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
        assert_eq!(body.thinking, Some(serde_json::json!({"type": "disabled"})));
        assert!(body.reasoning_effort.is_none());
    }

    #[test]
    fn build_request_body_kimi_thinking_keep_all() {
        let ep = ModelEndpoint {
            provider: "moonshot".into(),
            base_url: "https://api.moonshot.ai/v1".into(),
            model_name: "kimi-k2.6".into(),
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
        assert_eq!(
            body.thinking,
            Some(serde_json::json!({"type": "enabled", "keep": "all"}))
        );
        assert!(body.reasoning_effort.is_none());
    }

    #[test]
    fn build_request_body_openai_passes_reasoning_effort_without_thinking() {
        let ep = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model_name: "o3".into(),
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let body = OpenAiAdapter::new(ep).build_request_body(vec![], vec![], false);
        assert!(body.thinking.is_none());
        assert_eq!(body.reasoning_effort.as_deref(), Some("high"));
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
            source: None,
            id: None,
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
        assert_eq!(usage.prompt(), 10);
        assert_eq!(usage.completion(), 5);
        assert_eq!(usage.total_tokens, 15);
        assert_eq!(usage.cached(), 0);
    }

    #[test]
    fn usage_parses_prompt_tokens_details_cached_tokens() {
        let json = r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":80}}"#;
        let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.cached(), 80);
    }

    #[test]
    fn usage_parses_cache_write_and_miss_tokens() {
        let json = r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_tokens_details":{"cached_tokens":70,"cache_write_tokens":10},"prompt_cache_miss_tokens":20}"#;
        let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
        let normalized = usage.to_usage(None);
        assert_eq!(normalized.cached_tokens, 70);
        assert_eq!(normalized.cache_creation_tokens, 10);
        assert_eq!(normalized.cache_miss_tokens(), 20);
    }

    #[test]
    fn response_without_usage_keeps_cache_outcome_unknown() {
        let adapter = OpenAiAdapter::new(ModelEndpoint::default());
        let response = adapter
            .parse_openai_response(
                OpenAiResponse {
                    choices: vec![OpenAiChoice {
                        message: Some(OpenAiMessageOut {
                            role: Some("assistant".into()),
                            content: Some("ok".into()),
                            tool_calls: None,
                            reasoning_content: None,
                            web_search_call: Vec::new(),
                        }),
                        delta: None,
                        finish_reason: Some("stop".into()),
                    }],
                    usage: None,
                    model: None,
                    citations: Vec::new(),
                },
                None,
                CacheDiagnostics::for_request(true, true),
            )
            .unwrap();
        assert_eq!(response.usage.cache_diagnostics.unwrap().outcome, "unknown");
    }

    #[test]
    fn usage_parses_deepseek_prompt_cache_hit_tokens() {
        let json = r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"prompt_cache_hit_tokens":70}"#;
        let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.cached(), 70);
    }

    #[test]
    fn usage_parses_kimi_top_level_cached_tokens() {
        let json =
            r#"{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105,"cached_tokens":60}"#;
        let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.cached(), 60);
    }

    #[test]
    fn usage_parses_input_output_token_aliases() {
        let json = r#"{"input_tokens":40,"output_tokens":8}"#;
        let usage: OpenAiUsage = serde_json::from_str(json).unwrap();
        let canon = usage.to_usage(None);
        assert_eq!(canon.prompt_tokens, 40);
        assert_eq!(canon.completion_tokens, 8);
        assert_eq!(canon.total_tokens, 48);
    }

    #[test]
    fn stream_response_usage_absent_parses_fine() {
        let json = r#"{"id":"c1","choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#;
        let resp: OpenAiStreamResponse = serde_json::from_str(json).unwrap();
        assert!(resp.usage.is_none());
        assert!(!resp.choices.is_empty());
    }
}
