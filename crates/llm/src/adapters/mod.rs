pub mod anthropic;
pub mod assemblyai;
pub mod deepgram;
pub mod gemini;
pub mod openai;
pub mod openai_responses;

pub use anthropic::AnthropicAdapter;
pub use openai::OpenAiAdapter;

use futures_util::FutureExt;
use futures_util::StreamExt;
use haven_common::config::ModelEndpoint;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use std::time::Duration;
use tokio::sync::mpsc;

use crate::client::{LlmClient, http_status_to_error};
use crate::types::{LlmError, StreamChunk};

/// Resolve the wire protocol style for an endpoint. An explicit `api_style`
/// wins; otherwise the style is derived from `provider`.
pub fn api_style_for(endpoint: &ModelEndpoint) -> &str {
    if let Some(style) = &endpoint.api_style
        && !style.is_empty()
    {
        return style;
    }
    match endpoint.provider.as_str() {
        "anthropic" => "anthropic",
        "google" | "gemini" => "gemini",
        "llama" | "llama.cpp" | "llamacpp" => "llama.cpp",
        "deepgram" => "deepgram",
        "assemblyai" => "assemblyai",
        _ => "openai-chat",
    }
}

/// Build the protocol adapter for an endpoint.
///
/// Dispatch happens on the resolved `api_style` (see `api_style_for`):
/// - `openai-chat` / `llama.cpp`: OpenAI-compatible `/chat/completions`
///   (OpenAI, Ollama, vLLM, DeepSeek, llama.cpp server, and most third-party
///   gateways). Whisper-family models also implement `transcribe` via
///   `/audio/transcriptions`.
/// - `openai-responses`: OpenAI Responses API
/// - `anthropic`: Anthropic Messages API
/// - `gemini`: Google Gemini API (chat + STT via `generateContent`)
/// - `deepgram` / `assemblyai`: speech-to-text only
pub fn adapter_for(endpoint: &ModelEndpoint) -> Box<dyn LlmClient> {
    match api_style_for(endpoint) {
        "anthropic" => Box::new(anthropic::AnthropicAdapter::new(endpoint.clone())),
        "gemini" => Box::new(gemini::GeminiAdapter::new(endpoint.clone())),
        "openai-responses" => Box::new(openai_responses::OpenAiResponsesAdapter::new(
            endpoint.clone(),
        )),
        "deepgram" => Box::new(deepgram::DeepgramAdapter::new(endpoint.clone())),
        "assemblyai" => Box::new(assemblyai::AssemblyAiAdapter::new(endpoint.clone())),
        _ => Box::new(openai::OpenAiAdapter::new(endpoint.clone())),
    }
}

// ---------------------------------------------------------------------------
// Shared HTTP plumbing for every provider adapter
// ---------------------------------------------------------------------------

/// Build the reqwest client with proxy support (§2.5) and connection-pool
/// tuning (§5.5). Identical for every adapter.
pub(crate) fn build_client(endpoint: &ModelEndpoint) -> reqwest::Client {
    let mut builder = crate::client::http_client_builder();

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

    builder.build().unwrap_or_default()
}

/// Shared JSON request headers plus provider auth.
///
/// `default_header` / `default_uses_prefix` describe the provider's default
/// auth scheme, used when the endpoint does not customize
/// `auth_header_name`/`auth_header_prefix`:
/// - Anthropic: `x-api-key: <key>` (no prefix)
/// - Gemini: `x-goog-api-key: <key>` (no prefix)
/// - OpenAI-style: `Authorization: Bearer <key>` (prefix)
///
/// A customized scheme always wins and sends `<prefix> <key>` under the
/// custom header name (§2.15).
pub(crate) fn build_headers(
    endpoint: &ModelEndpoint,
    default_header: &str,
    default_uses_prefix: bool,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if endpoint.api_key.is_empty() {
        return headers;
    }
    let customized =
        endpoint.auth_header_name != "Authorization" || endpoint.auth_header_prefix != "Bearer";
    if customized {
        // Match `auth_value` in discover_models: empty prefix means the raw
        // key (Anthropic / Gemini / AssemblyAI), not `" <key>"`.
        let auth = if endpoint.auth_header_prefix.is_empty() {
            endpoint.api_key.clone()
        } else {
            format!("{} {}", endpoint.auth_header_prefix, endpoint.api_key)
        };
        if let Ok(v) = HeaderValue::from_str(&auth) {
            let name = endpoint
                .auth_header_name
                .parse::<reqwest::header::HeaderName>()
                .unwrap_or(reqwest::header::AUTHORIZATION);
            headers.insert(name, v);
        }
    } else {
        let value = if default_uses_prefix {
            format!("Bearer {}", endpoint.api_key)
        } else {
            endpoint.api_key.clone()
        };
        if let Ok(v) = HeaderValue::from_str(&value)
            && let Ok(name) = default_header.parse::<reqwest::header::HeaderName>()
        {
            headers.insert(name, v);
        }
    }
    headers
}

/// Default budget for the streaming response-HEADER wait when the endpoint
/// configures no `timeout_streaming_secs`. The `req.send()` phase (connect,
/// request upload, and the wait for response headers) otherwise has no
/// HTTP-level bound: a provider that accepts the connection but never returns
/// headers stalls silently until the router's total-duration timeout
/// (minutes), looking like a frozen session with no log output. Bounds ONLY
/// the header wait — once the response resolves, the body stream is governed
/// by the router's per-chunk idle timeouts, so a legitimately long generation
/// is never cut off by this cap (matching the router's first-chunk grace).
const STREAM_HEADER_TIMEOUT_SECS: u64 = 60;

/// Streaming header-wait budget for adapters: when the endpoint configured
/// `timeout_streaming_secs`, the request already carries a total-duration
/// `.timeout()` (covering the header wait), so no separate bound is needed;
/// otherwise `Some(...)` bounds the header wait via
/// [`STREAM_HEADER_TIMEOUT_SECS`].
pub(crate) fn stream_header_timeout(timeout_streaming_secs: Option<u64>) -> Option<Duration> {
    timeout_streaming_secs
        .is_none()
        .then_some(Duration::from_secs(STREAM_HEADER_TIMEOUT_SECS))
}

/// Send a prepared request and turn non-success statuses into a structured
/// `LlmError`, extracting `Retry-After` (§2.3) before consuming the body.
/// Returns the response for the caller to parse on success.
///
/// `header_timeout`, when `Some`, additionally bounds `req.send()` (waiting
/// for the response headers) so a provider that accepts the connection but
/// never responds cannot stall the caller indefinitely. It deliberately does
/// NOT bound the body stream — that is the router's per-chunk idle timeout's
/// job, and a total-duration cap would truncate legitimately long streams.
pub(crate) async fn send_request(
    req: reqwest::RequestBuilder,
    header_timeout: Option<Duration>,
) -> Result<reqwest::Response, LlmError> {
    let resp = match header_timeout {
        Some(d) => tokio::time::timeout(d, req.send())
            .await
            .map_err(|_| {
                LlmError::Timeout(format!(
                    "timed out waiting for the stream response headers after {}s",
                    d.as_secs()
                ))
            })?
            .map_err(LlmError::from)?,
        None => req.send().await.map_err(LlmError::from)?,
    };
    if resp.status().is_success() {
        return Ok(resp);
    }
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
    Err(http_status_to_error(status, &txt, retry_after))
}

/// Shared health check: GET the models URL and classify the status.
pub(crate) async fn health_check_request(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
    timeout_secs: u64,
) -> Result<(), LlmError> {
    let resp = client
        .get(url)
        .headers(headers)
        .timeout(Duration::from_secs(timeout_secs.min(7)))
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

/// DeepSeek's web-search round-trip: a `web_search_call` item captured from
/// the stream is echoed back verbatim into the next request's `input`.
/// DeepSeek's Responses-compat layer deserializes the echoed item against a
/// strict schema: the `action` field is an internally tagged enum
/// (`WebSearchAction`) with variants `search` / `open_page` / `find_in_page`,
/// and the `search` variant requires a `queries` string array. The
/// `output_item.added` skeleton (only `type`/`id`/`status`) and the
/// `web_search_call.*` status events lack `action` — echoing a bare skeleton
/// 400s ("missing field `action`") — so the full `output_item.done` payload
/// must be captured instead (see the adapter). As a last resort, fill the
/// action when absent or malformed with `{"type": "search", "queries": []}`
/// (verified accepted by DeepSeek); items that already carry a well-formed
/// object `action` — e.g. an `output_item.done` payload — pass through
/// untouched.
pub(crate) fn normalize_web_search_call_item(item: serde_json::Value) -> serde_json::Value {
    let mut item = item;
    if !item.is_object() {
        return item;
    }
    let has_valid_action = item.get("action").is_some_and(|a| a.is_object());
    if !has_valid_action {
        item["action"] = serde_json::json!({"type": "search", "queries": []});
    }
    item
}

/// True when the endpoint's thinking mode requires the assistant's reasoning
/// to be echoed back on every request that carries tool-call history
/// (chat-completions: `reasoning_content`; Responses compat: `reasoning_text`).
/// The affected APIs validate PRESENCE of the field, not its content — an
/// empty echo passes — so a tool-call turn on which the model skipped thinking
/// still needs the item injected. Providers in this class: DeepSeek
/// (thinking mode), Moonshot/Kimi K2.x+ (thinking on by default for the plain
/// `kimi-k2.6` model id), and MiMo. Matched by provider hint, base URL and
/// model name so a proxied/gatewayed endpoint is caught too.
pub(crate) fn requires_reasoning_echo(endpoint: &ModelEndpoint) -> bool {
    const REASONING_ECHO_PROVIDERS: [&str; 4] = ["deepseek", "kimi", "moonshot", "mimo"];
    let hay = [&endpoint.provider, &endpoint.base_url, &endpoint.model_name]
        .map(String::as_str)
        .join(" ")
        .to_ascii_lowercase();
    REASONING_ECHO_PROVIDERS.iter().any(|p| hay.contains(p))
}

/// Reconstruct the plain reasoning text from raw Anthropic `thinking` blocks.
/// Mirrors the anthropic adapter's `reasoning` assembly exactly (concatenation
/// of the `thinking` fields of `type == "thinking"` blocks, in order; redacted
/// data is skipped). Lets OpenAI-compatible adapters echo reasoning when the
/// canonical carries only the raw echo-capable blocks (the agent drops the
/// redundant `reasoning` copy on Anthropic messages).
pub(crate) fn reasoning_text_from_thinking_blocks(blocks: &[serde_json::Value]) -> String {
    let mut out = String::new();
    for b in blocks {
        if b.get("type").and_then(serde_json::Value::as_str) == Some("thinking")
            && let Some(t) = b.get("thinking").and_then(serde_json::Value::as_str)
        {
            out.push_str(t);
        }
    }
    out
}

/// Keep the TAIL of `text` bounded to `cap` characters. Full reasoning
/// (10k+ chars per turn) balloons request bodies and providers stall or
/// truncate mid-inference; the tail preserves the turn's conclusions.
/// Returns the input unchanged (no allocation) when it already fits, and
/// the cap counts CHARS (not bytes) so multi-byte text is never cut mid-codepoint.
/// Shared by the chat-completions (`reasoning_content`) and Responses
/// (`reasoning` item) adapters so the two echoes cannot drift apart.
pub(crate) fn reasoning_tail(text: String, cap: usize) -> String {
    let chars = text.chars().count();
    if chars <= cap {
        text
    } else {
        let skip = chars - cap;
        let start = text
            .char_indices()
            .nth(skip)
            .map(|(i, _)| i)
            .unwrap_or(text.len());
        text[start..].to_string()
    }
}

/// Insert a captured `web_search_call` item into `calls`, replacing any
/// earlier item with the same `id`. The `output_item.added` skeleton arrives
/// first; a later `web_search_call.completed` payload — when the provider
/// sends one — is the authoritative version. Both must never be echoed into
/// the next request's input as duplicates.
pub(crate) fn upsert_web_search_call(calls: &mut Vec<serde_json::Value>, item: serde_json::Value) {
    let id = item.get("id").and_then(serde_json::Value::as_str);
    if let Some(id) = id
        && let Some(pos) = calls
            .iter()
            .position(|c| c.get("id").and_then(serde_json::Value::as_str) == Some(id))
    {
        calls[pos] = item;
    } else {
        calls.push(item);
    }
}

/// An empty `StreamChunk` — the "no payload" baseline emitted by every
/// adapter's stream unfolding.
pub(crate) fn empty_chunk() -> StreamChunk {
    StreamChunk {
        text: None,
        tool_calls: Vec::new(),
        finish_reason: None,
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Shared streaming response reader
// ---------------------------------------------------------------------------

/// How the shared line reader should interpret each line of the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineMode {
    /// Forward only SSE `data: …` payloads; ignore `event:` and comment lines
    /// (Anthropic / OpenAI Responses-style SSE).
    SseDataOnly,
    /// Forward `data: …` payloads, treating any other line as raw JSON
    /// (OpenAI-style SSE with non-standard providers; also tolerates Gemini
    /// gateways that ignore `alt=sse` and return NDJSON).
    SseOrRaw,
}

/// Spawn a session that reads an HTTP response byte stream line-by-line and
/// forwards parsed payloads on `tx`. Handles SSE (`data: …`) and raw-JSON-lines
/// formats in one pass; the interpretation is selected via `mode`.
pub(crate) fn spawn_line_reader<S>(
    byte_stream: S,
    tx: mpsc::UnboundedSender<String>,
    mode: LineMode,
) where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(async {
            let mut buf = String::new();
            tokio::pin!(byte_stream);
            loop {
                let chunk = byte_stream.next().await;
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
                            match mode {
                                LineMode::SseDataOnly => {
                                    if let Some(payload) = line.strip_prefix("data: ") {
                                        let payload = payload.trim().to_string();
                                        if payload.is_empty() || payload == "[DONE]" {
                                            continue;
                                        }
                                        tracing::trace!("stream payload: {} chars", payload.len());
                                        if tx.send(payload).is_err() {
                                            return;
                                        }
                                    }
                                    // `event: …` lines carry no payload; skip.
                                }
                                LineMode::SseOrRaw => {
                                    let payload = if let Some(p) = line.strip_prefix("data: ") {
                                        p.trim().to_string()
                                    } else {
                                        line
                                    };
                                    if payload == "[DONE]" || payload.is_empty() {
                                        continue;
                                    }
                                    tracing::trace!("stream payload: {} chars", payload.len());
                                    if tx.send(payload).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(_)) | None => {
                        // Flush any remaining buffered data before EOF.
                        let remaining = buf.trim().to_string();
                        if !remaining.is_empty() && remaining != "[DONE]" {
                            match mode {
                                LineMode::SseDataOnly => {
                                    if let Some(payload) = remaining.strip_prefix("data: ") {
                                        let payload = payload.trim().to_string();
                                        if !payload.is_empty() && payload != "[DONE]" {
                                            tracing::trace!(
                                                "stream flush: {} chars",
                                                payload.len()
                                            );
                                            let _ = tx.send(payload);
                                        }
                                    }
                                }
                                LineMode::SseOrRaw => {
                                    tracing::trace!("stream flush: {} chars", remaining.len());
                                    let _ = tx.send(remaining);
                                }
                            }
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
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_header_timeout_bounds_only_unconfigured_endpoints() {
        // Endpoint configured with a streaming timeout: the request carries
        // its own total-duration `.timeout()`, so no separate header bound.
        assert_eq!(stream_header_timeout(Some(120)), None);
        // Unconfigured: the fallback header-wait budget applies so a silent
        // provider stall surfaces instead of hanging the caller.
        assert_eq!(
            stream_header_timeout(None),
            Some(Duration::from_secs(STREAM_HEADER_TIMEOUT_SECS))
        );
    }

    #[test]
    fn requires_reasoning_echo_covers_reasoning_echo_providers() {
        // DeepSeek (both native and proxied).
        let deepseek = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model_name: "deepseek-v4-flash".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&deepseek));
        let proxied = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://gateway.example.com/v1".into(),
            model_name: "deepseek-reasoner".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&proxied));
        // Moonshot / Kimi K2.x (thinking on by default).
        let kimi = ModelEndpoint {
            provider: "moonshot".into(),
            base_url: "https://api.moonshot.ai/v1".into(),
            model_name: "kimi-k2.6".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&kimi));
        let kimi_cn = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://api.moonshot.cn/v1".into(),
            model_name: "kimi-k2.7-code".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&kimi_cn));
        // MiMo.
        let mimo = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://platform.xiaomimimo.com/v1".into(),
            model_name: "MiMo-7B-RL".into(),
            ..Default::default()
        };
        assert!(requires_reasoning_echo(&mimo));
        // Providers without the requirement must not be padded.
        let openai = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            model_name: "gpt-5".into(),
            ..Default::default()
        };
        assert!(!requires_reasoning_echo(&openai));
        let zhipu = ModelEndpoint {
            provider: "zhipu".into(),
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            model_name: "glm-5".into(),
            ..Default::default()
        };
        assert!(!requires_reasoning_echo(&zhipu));
    }

    #[test]
    fn reasoning_text_from_thinking_blocks_concatenates_thinking_only() {
        let blocks = vec![
            serde_json::json!({"type": "thinking", "thinking": "first part", "signature": "s1"}),
            serde_json::json!({"type": "text", "text": "visible"}),
            serde_json::json!({"type": "thinking", "thinking": "second part"}),
            serde_json::json!({"type": "redacted_thinking", "data": "redacted"}),
        ];
        // Mirrors the anthropic adapter's `reasoning` assembly: only
        // `type == "thinking"` text, concatenated in order, redacted skipped.
        assert_eq!(
            reasoning_text_from_thinking_blocks(&blocks),
            "first partsecond part"
        );
    }

    #[test]
    fn reasoning_text_from_thinking_blocks_empty_for_no_thinking() {
        assert_eq!(
            reasoning_text_from_thinking_blocks(&[serde_json::json!({"type": "text"})]),
            ""
        );
        assert_eq!(reasoning_text_from_thinking_blocks(&[]), "");
    }

    #[test]
    fn api_style_explicit_wins() {
        let ep = ModelEndpoint {
            provider: "openai".into(),
            api_style: Some("anthropic".into()),
            ..Default::default()
        };
        assert_eq!(api_style_for(&ep), "anthropic");
    }

    #[test]
    fn api_style_derived_from_provider() {
        let anthropic = ModelEndpoint {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&anthropic), "anthropic");
        let google = ModelEndpoint {
            provider: "google".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&google), "gemini");
        let gemini = ModelEndpoint {
            provider: "gemini".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&gemini), "gemini");
        let openai = ModelEndpoint {
            provider: "openai".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&openai), "openai-chat");
        let unknown = ModelEndpoint {
            provider: "ollama".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&unknown), "openai-chat");
        let llama = ModelEndpoint {
            provider: "llama.cpp".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&llama), "llama.cpp");
        let llama_alias = ModelEndpoint {
            provider: "llama".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&llama_alias), "llama.cpp");
        let llamacpp = ModelEndpoint {
            provider: "llamacpp".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&llamacpp), "llama.cpp");
        let deepgram = ModelEndpoint {
            provider: "deepgram".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&deepgram), "deepgram");
        let assemblyai = ModelEndpoint {
            provider: "assemblyai".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&assemblyai), "assemblyai");
    }

    #[test]
    fn adapter_for_dispatches_by_style() {
        let anthropic = ModelEndpoint {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&anthropic).style(), "anthropic");
        let gemini = ModelEndpoint {
            provider: "google".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&gemini).style(), "gemini");
        let responses = ModelEndpoint {
            api_style: Some("openai-responses".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&responses).style(), "openai-responses");
        let openai = ModelEndpoint::default();
        assert_eq!(adapter_for(&openai).style(), "openai-chat");
        // llama.cpp speaks the OpenAI-compatible wire protocol and is served by
        // the same adapter, so its reported style matches openai-chat.
        let llama = ModelEndpoint {
            provider: "llama.cpp".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&llama).style(), "openai-chat");
        let deepgram = ModelEndpoint {
            provider: "deepgram".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&deepgram).style(), "deepgram");
        let assemblyai = ModelEndpoint {
            provider: "assemblyai".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&assemblyai).style(), "assemblyai");
    }

    #[test]
    fn normalize_web_search_call_item_fills_missing_action() {
        let skeleton = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress"
        });
        let out = normalize_web_search_call_item(skeleton);
        // `action` is an internally tagged enum object; the `search` variant
        // requires a `queries` array (verified against DeepSeek).
        assert_eq!(
            out["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
        assert_eq!(out["type"], "web_search_call");
        assert_eq!(out["id"], "ws_1");
        assert_eq!(out["status"], "in_progress");
    }

    #[test]
    fn normalize_web_search_call_item_replaces_malformed_string_action() {
        // A previous buggy fill wrote a bare string; DeepSeek rejects it
        // ("invalid type: string, expected internally tagged enum
        // WebSearchAction"), so it is replaced with the object form.
        let skeleton = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "in_progress",
            "action": "web_search"
        });
        let out = normalize_web_search_call_item(skeleton);
        assert_eq!(
            out["action"],
            serde_json::json!({"type": "search", "queries": []})
        );
    }

    #[test]
    fn normalize_web_search_call_item_keeps_existing_action() {
        let complete = serde_json::json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {"type": "open_page", "url": "https://example.com"},
            "query": "foo"
        });
        let out = normalize_web_search_call_item(complete.clone());
        assert_eq!(out, complete);
    }

    #[test]
    fn normalize_web_search_call_item_skips_non_objects() {
        assert_eq!(
            normalize_web_search_call_item(serde_json::Value::Null),
            serde_json::Value::Null
        );
    }

    #[test]
    fn upsert_web_search_call_replaces_same_id_and_appends_new() {
        use serde_json::json;
        let mut calls =
            vec![json!({"type": "web_search_call", "id": "ws_1", "status": "in_progress"})];
        // The completed payload replaces the in-progress skeleton by id.
        upsert_web_search_call(
            &mut calls,
            json!({"type": "web_search_call", "id": "ws_1", "status": "completed", "action": {"type": "search", "queries": ["capital of France"]}}),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["status"], "completed");
        assert_eq!(
            calls[0]["action"],
            json!({"type": "search", "queries": ["capital of France"]})
        );
        // A different id is appended.
        upsert_web_search_call(
            &mut calls,
            json!({"type": "web_search_call", "id": "ws_2", "status": "in_progress"}),
        );
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1]["id"], "ws_2");
    }

    #[tokio::test]
    async fn line_reader_sse_data_only_skips_event_lines() {
        use futures_util::stream;

        let body = "event: message_start\ndata: {\"a\":1}\n\n: comment\n\
                    event: content_block_delta\ndata: {\"b\":2}\n";
        let stream = stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(body))]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_line_reader(stream, tx, LineMode::SseDataOnly);
        let mut got = Vec::new();
        while let Some(p) = rx.recv().await {
            got.push(p);
        }
        assert_eq!(got, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }
}
