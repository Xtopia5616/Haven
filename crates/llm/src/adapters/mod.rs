pub mod anthropic;
pub mod assemblyai;
pub mod capabilities;
pub mod deepgram;
pub mod gemini;
pub mod openai;
pub mod openai_responses;

pub use anthropic::AnthropicAdapter;
pub use capabilities::{
    WebSearchMode, api_style_from_provider, is_known_api_style, is_openai_family_wire_style,
    is_stt_only_style, is_tts_only_style, normalize_api_style, parse_web_search_mode,
    resolve_web_search_mode, supports_builtin_web_search, xai_search_mode,
};
pub use openai::OpenAiAdapter;

use futures_util::FutureExt;
use futures_util::StreamExt;
use haven_common::config::ModelEndpoint;
use haven_common::types::{ContentPart, InjectSource};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;

use crate::client::{LlmClient, http_status_to_error};
use crate::types::{Embedding, LlmError, StreamChunk, Usage};

/// Phase 8 / B3: apply wire-only inject prefix to user content parts.
///
/// When `source.needs_wire_prefix()`, prepends `"{prefix}: "` to the first
/// text part (or inserts a text part when content is image/audio-only).
/// Skips when already prefixed (defensive) or when `ActionResult`.
pub(crate) fn apply_wire_inject_prefix(
    source: Option<InjectSource>,
    mut content: Vec<ContentPart>,
) -> Vec<ContentPart> {
    let Some(src) = source.filter(|s| s.needs_wire_prefix()) else {
        return content;
    };
    let rendered = format!("{}: ", src.render_prefix());
    if let Some(part) = content
        .iter_mut()
        .find(|p| matches!(p, ContentPart::Text(_)))
    {
        if let ContentPart::Text(text) = part
            && !text.starts_with(&rendered)
        {
            text.insert_str(0, &rendered);
        }
    } else if !content.is_empty() {
        content.insert(0, ContentPart::Text(rendered));
    }
    content
}

/// Resolve the wire protocol style for an endpoint. An explicit `api_style`
/// wins (after [`normalize_api_style`]); otherwise the style is derived from
/// `provider` via [`api_style_from_provider`].
pub fn api_style_for(endpoint: &ModelEndpoint) -> &'static str {
    if let Some(style) = &endpoint.api_style
        && !style.is_empty()
    {
        if !is_known_api_style(style) {
            tracing::warn!(
                api_style = %style,
                "unknown api_style; falling back to openai-chat"
            );
        }
        return normalize_api_style(style);
    }
    api_style_from_provider(&endpoint.provider)
}

/// Build the protocol adapter for an endpoint.
///
/// Dispatch happens on the resolved + normalized `api_style`
/// (see `api_style_for` / [`normalize_api_style`]):
/// - `openai-chat` / `llama.cpp`: OpenAI-compatible `/chat/completions`
///   (OpenAI, Ollama, vLLM, DeepSeek chat, llama.cpp server, and most
///   third-party gateways). Whisper-family models also implement `transcribe`
///   via `/audio/transcriptions`. Embeddings use `/embeddings`.
/// - `xai`: same OpenAI chat adapter with xAI Live Search `search_parameters`
///   (embeddings still `/embeddings`)
/// - `openai-responses` (+ alias `deepseek-responses`): OpenAI Responses API
///   (`/v1/responses`), including DeepSeek thinking + built-in `web_search`.
///   Embeddings still use the OpenAI-compatible `/v1/embeddings` path — chat
///   wire style does not apply to that endpoint.
/// - `anthropic`: Anthropic Messages API (+ optional server `web_search`);
///   no embeddings API
/// - `gemini`: Google Gemini API (+ optional `google_search` grounding);
///   embeddings via `batchEmbedContents`
/// - `deepgram` / `assemblyai`: speech-to-text only
pub fn adapter_for(endpoint: &ModelEndpoint) -> Box<dyn LlmClient> {
    match normalize_api_style(api_style_for(endpoint)) {
        "anthropic" => Box::new(anthropic::AnthropicAdapter::new(endpoint.clone())),
        "gemini" => Box::new(gemini::GeminiAdapter::new(endpoint.clone())),
        "openai-responses" => Box::new(openai_responses::OpenAiResponsesAdapter::new(
            endpoint.clone(),
        )),
        "deepgram" => Box::new(deepgram::DeepgramAdapter::new(endpoint.clone())),
        "assemblyai" => Box::new(assemblyai::AssemblyAiAdapter::new(endpoint.clone())),
        "xai" => Box::new(openai::OpenAiAdapter::new_with_style(
            endpoint.clone(),
            "xai",
        )),
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
    apply_vendor_request_headers(endpoint, &mut headers);
    headers
}

/// Vendor-specific request headers that are not part of the auth scheme
/// (e.g. OpenRouter attribution). Applied after auth so callers share one path.
fn apply_vendor_request_headers(endpoint: &ModelEndpoint, headers: &mut HeaderMap) {
    if is_openrouter(endpoint) {
        // OpenRouter ranks apps by these optional headers.
        if let Ok(v) = HeaderValue::from_str("Haven") {
            headers.insert("X-Title", v);
        }
        if let Ok(v) = HeaderValue::from_str("https://haven.app") {
            headers.insert("HTTP-Referer", v);
        }
    }
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

#[derive(Debug, Serialize)]
struct OpenAiEmbedRequest {
    model: String,
    input: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedItem {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiEmbedUsage {
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    total_tokens: u32,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbedResponse {
    data: Vec<OpenAiEmbedItem>,
    usage: Option<OpenAiEmbedUsage>,
    model: Option<String>,
}

/// OpenAI-compatible embeddings URL.
///
/// Chat Completions adapters concatenate `/embeddings` onto `base_url` as-is
/// (typically already ends in `/v1`). Responses adapters pass `ensure_v1`
/// so a host-only base (`https://api.deepseek.com`) still hits `/v1/embeddings`.
pub(crate) fn openai_embeddings_url(base_url: &str, ensure_v1: bool) -> String {
    let base = base_url.trim_end_matches('/');
    if ensure_v1 && !(base.ends_with("/v1") || base.ends_with("/v1beta")) {
        format!("{base}/v1/embeddings")
    } else {
        format!("{base}/embeddings")
    }
}

pub(crate) fn parse_openai_embed_response(
    body: &str,
    requested: usize,
    fallback_model: &str,
) -> Result<Embedding, LlmError> {
    let json: OpenAiEmbedResponse =
        serde_json::from_str(body).map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    let mut items = json.data;
    if items.is_empty() {
        return Err(LlmError::InvalidResponse(
            "embeddings response missing data".into(),
        ));
    }
    if items.len() != requested {
        return Err(LlmError::InvalidResponse(format!(
            "embeddings count mismatch: requested {requested}, got {}",
            items.len()
        )));
    }
    items.sort_by_key(|item| item.index);
    let vectors: Vec<Vec<f32>> = items.into_iter().map(|item| item.embedding).collect();
    let model = json.model.clone().or(Some(fallback_model.to_string()));
    let usage = json
        .usage
        .map(|u| {
            Usage::from_counts(
                u.prompt_tokens,
                u.completion_tokens,
                u.total_tokens,
                0,
                0,
                model.clone(),
            )
        })
        .unwrap_or_default();
    Ok(Embedding {
        vectors,
        model,
        usage,
    })
}

pub(crate) async fn openai_compatible_embed(
    client: &reqwest::Client,
    headers: HeaderMap,
    url: &str,
    model: &str,
    timeout_secs: u64,
    input: Vec<String>,
) -> Result<Embedding, LlmError> {
    if input.is_empty() {
        return Ok(Embedding {
            vectors: Vec::new(),
            model: Some(model.to_string()),
            usage: Usage::default(),
        });
    }
    let requested = input.len();
    let body = OpenAiEmbedRequest {
        model: model.to_string(),
        input,
    };
    tracing::debug!("POST {url} (model: {model})");
    tracing::debug!(
        "POST {url} request body: {} chars",
        serde_json::to_string(&body).map(|s| s.len()).unwrap_or(0)
    );
    let mut req = client.post(url).headers(headers).json(&body);
    req = req.timeout(Duration::from_secs(timeout_secs));
    let resp = send_request(req, None).await?;
    let txt = resp
        .text()
        .await
        .map_err(|e| LlmError::InvalidResponse(e.to_string()))?;
    tracing::trace!("POST {url} response body: {} chars", txt.len());
    parse_openai_embed_response(&txt, requested, model)
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
/// Resolve prompt-cache hit tokens from nested details (`cached_tokens`) and
/// optional flat aliases (e.g. DeepSeek `prompt_cache_hit_tokens`). Prefer the
/// larger value so either reporting shape wins without double-counting.
pub(crate) fn resolve_cached_tokens(nested: Option<u32>, flat_alias: u32) -> u32 {
    nested.unwrap_or(0).max(flat_alias)
}

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

/// Normalize one raw citation entry into `{title, url, snippet}`. Accepts
/// objects with `title`/`url`/`snippet` (OpenAI / DeepSeek `citations`,
/// Anthropic result rows) and plain URL strings (xAI Live Search citations).
fn web_search_citation_of(raw: &serde_json::Value) -> Option<serde_json::Value> {
    match raw {
        serde_json::Value::String(url) if !url.is_empty() => {
            Some(serde_json::json!({"title": url, "url": url, "snippet": ""}))
        }
        serde_json::Value::Object(o) => {
            let title = o.get("title").and_then(Value::as_str).unwrap_or_default();
            let url = o.get("url").and_then(Value::as_str).unwrap_or_default();
            let snippet = o
                .get("snippet")
                .and_then(Value::as_str)
                .or_else(|| o.get("content").and_then(Value::as_str))
                .unwrap_or_default()
                .chars()
                .take(240)
                .collect::<String>();
            if title.is_empty() && url.is_empty() {
                return None;
            }
            Some(serde_json::json!({"title": title, "url": url, "snippet": snippet}))
        }
        _ => None,
    }
}

fn web_search_collect(src: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match src {
        serde_json::Value::Array(items) => {
            for it in items {
                if let Some(c) = web_search_citation_of(it) {
                    out.push(c);
                }
            }
        }
        serde_json::Value::Object(o) => {
            for key in ["results", "citations", "web_search_results"] {
                if let Some(arr) = o.get(key).and_then(Value::as_array) {
                    for it in arr {
                        if let Some(c) = web_search_citation_of(it) {
                            out.push(c);
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Extract the tool return of a provider built-in web search from a
/// `web_search_call` item: a compact `{queries, results}` payload where each
/// result is `{title, url, snippet}`. Reads `action.search.citations` /
/// `action.search.results` (OpenAI / DeepSeek / xAI), `action.result`
/// (Anthropic `web_search_tool_result` blocks) and flat `citations` arrays
/// (xAI Live Search). Returns `None` when the item carries no usable content
/// (e.g. a bare Gemini grounding skeleton or an in-progress call), so the
/// UI card falls back to the status label.
pub fn web_search_result_of(item: &serde_json::Value) -> Option<serde_json::Value> {
    let action = item.as_object()?.get("action")?.as_object()?;
    let mut queries: Vec<String> = Vec::new();
    if let Some(qs) = action.get("queries").and_then(Value::as_array) {
        queries.extend(qs.iter().filter_map(Value::as_str).map(str::to_string));
    }
    let mut results: Vec<serde_json::Value> = Vec::new();
    for key in ["citations", "results", "result"] {
        if let Some(src) = action.get(key) {
            web_search_collect(src, &mut results);
        }
    }
    // Anthropic web_search_tool_result puts the payload in `result.query`.
    if queries.is_empty()
        && let Some(q) = action
            .get("result")
            .and_then(Value::as_object)
            .and_then(|r| r.get("query"))
            .and_then(Value::as_str)
    {
        queries.push(q.to_string());
    }
    if queries.is_empty() && results.is_empty() {
        return None;
    }
    Some(serde_json::json!({"queries": queries, "results": results}))
}

/// Lowercased `provider` + `base_url` + `model_name` haystack used to detect
/// vendor-specific extras (DeepSeek thinking, Kimi `thinking.type`, etc.) even
/// when the endpoint is behind a gateway that sets `provider: "openai"`.
pub(crate) fn vendor_haystack(endpoint: &ModelEndpoint) -> String {
    [&endpoint.provider, &endpoint.base_url, &endpoint.model_name]
        .map(String::as_str)
        .join(" ")
        .to_ascii_lowercase()
}

pub(crate) fn is_deepseek(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("deepseek")
}

pub(crate) fn is_kimi_or_moonshot(endpoint: &ModelEndpoint) -> bool {
    let hay = vendor_haystack(endpoint);
    hay.contains("kimi") || hay.contains("moonshot")
}

pub(crate) fn is_openrouter(endpoint: &ModelEndpoint) -> bool {
    vendor_haystack(endpoint).contains("openrouter")
}

/// True when the configured effort means "turn thinking off"
/// (`none` / `off` / `disabled`). Used by DeepSeek chat (`thinking.type`) and
/// Responses (`reasoning.effort: "none"`), and by Kimi `thinking.type`.
pub(crate) fn is_thinking_disabled(effort: &str) -> bool {
    matches!(
        effort.trim().to_ascii_lowercase().as_str(),
        "none" | "off" | "disabled"
    )
}

/// Map Haven UI `reasoning_effort` (`low`/`medium`/`high`) onto DeepSeek's
/// accepted effort values. DeepSeek docs: medium/high/xhigh → high; max → max;
/// none disables thinking (Responses) / pairs with `thinking.type=disabled`.
pub(crate) fn map_deepseek_effort(effort: &str) -> &'static str {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => "low",
        "medium" | "high" | "xhigh" => "high",
        "max" => "max",
        "none" | "off" | "disabled" => "none",
        _ => "high",
    }
}

/// Vendor chat-completions extras derived from `reasoning_effort` + vendor
/// detection. Returns `(thinking_object, reasoning_effort_to_send)`.
pub(crate) fn chat_thinking_extras(
    endpoint: &ModelEndpoint,
) -> (Option<serde_json::Value>, Option<String>) {
    let effort = endpoint
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if is_deepseek(endpoint) {
        return match effort {
            None => (None, None),
            Some(e) if is_thinking_disabled(e) => {
                (Some(serde_json::json!({"type": "disabled"})), None)
            }
            Some(e) => (
                Some(serde_json::json!({"type": "enabled"})),
                Some(map_deepseek_effort(e).to_string()),
            ),
        };
    }

    if is_kimi_or_moonshot(endpoint) {
        return kimi_chat_thinking_extras(&endpoint.model_name, effort);
    }

    // OpenAI / other chat providers: never send disable tokens as
    // `reasoning_effort` (rejected by the API).
    match effort {
        None => (None, None),
        Some(e) if is_thinking_disabled(e) => (None, None),
        Some(e) => (None, Some(e.to_string())),
    }
}

fn kimi_chat_thinking_extras(
    model_name: &str,
    effort: Option<&str>,
) -> (Option<serde_json::Value>, Option<String>) {
    let model = model_name.to_ascii_lowercase();
    if model.contains("kimi-k3") || model.split(['/', '-', '_']).any(|p| p == "k3") {
        let mapped = effort.map(|e| {
            if is_thinking_disabled(e) {
                return None;
            }
            Some(
                match e.to_ascii_lowercase().as_str() {
                    "low" => "low",
                    "max" => "max",
                    _ => "high",
                }
                .to_string(),
            )
        });
        return (None, mapped.flatten());
    }
    if model.contains("k2.7") {
        return (None, None);
    }
    let supports_keep = model.contains("k2.6")
        || !(model.contains("k2.5") || model.contains("k2.7") || model.contains("kimi-k3"));

    match effort {
        None => (None, None),
        Some(e) if is_thinking_disabled(e) => (Some(serde_json::json!({"type": "disabled"})), None),
        Some(_) => {
            let thinking = if supports_keep {
                serde_json::json!({"type": "enabled", "keep": "all"})
            } else {
                serde_json::json!({"type": "enabled"})
            };
            (Some(thinking), None)
        }
    }
}

/// Responses-API `reasoning` object from `reasoning_effort`.
pub(crate) fn responses_reasoning_config(endpoint: &ModelEndpoint) -> Option<serde_json::Value> {
    let effort = endpoint
        .reasoning_effort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;

    if is_deepseek(endpoint) {
        return Some(serde_json::json!({ "effort": map_deepseek_effort(effort) }));
    }
    if is_thinking_disabled(effort) {
        return None;
    }
    Some(serde_json::json!({ "effort": effort }))
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
    let hay = vendor_haystack(endpoint);
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
fn web_search_item_rank(item: &serde_json::Value) -> u8 {
    let mut rank = 0u8;
    if item.get("action").is_some_and(|a| a.is_object()) {
        rank += 1;
    }
    if web_search_result_of(item).is_some() {
        rank += 2;
    }
    rank
}

pub(crate) fn upsert_web_search_call(calls: &mut Vec<serde_json::Value>, item: serde_json::Value) {
    let id = item.get("id").and_then(serde_json::Value::as_str);
    if let Some(id) = id
        && let Some(pos) = calls
            .iter()
            .position(|c| c.get("id").and_then(serde_json::Value::as_str) == Some(id))
    {
        if web_search_item_rank(&item) >= web_search_item_rank(&calls[pos]) {
            calls[pos] = item;
        }
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
pub(crate) async fn serve_once(status_line: &str, content_type: &str, body: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let body = body.to_string();
    let status = status_line.to_string();
    let content_type = content_type.to_string();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        loop {
            let n = sock.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..header_end]);
                let content_length = head
                    .lines()
                    .find_map(|l| {
                        let lower = l.to_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if buf.len() >= header_end + 4 + content_length {
                    break;
                }
            }
        }
        let resp = format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            status,
            content_type,
            body.len(),
            body
        );
        let _ = sock.write_all(resp.as_bytes()).await;
    });
    format!("http://{addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_cached_tokens_prefers_larger_of_nested_and_flat() {
        assert_eq!(resolve_cached_tokens(None, 0), 0);
        assert_eq!(resolve_cached_tokens(Some(80), 0), 80);
        assert_eq!(resolve_cached_tokens(None, 70), 70);
        assert_eq!(resolve_cached_tokens(Some(80), 70), 80);
        assert_eq!(resolve_cached_tokens(Some(60), 90), 90);
    }

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
    fn openrouter_gets_attribution_headers() {
        let ep = ModelEndpoint {
            provider: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "sk-test".into(),
            ..Default::default()
        };
        assert!(is_openrouter(&ep));
        let headers = build_headers(&ep, "Authorization", true);
        assert_eq!(
            headers.get("X-Title").and_then(|v| v.to_str().ok()),
            Some("Haven")
        );
        assert_eq!(
            headers.get("HTTP-Referer").and_then(|v| v.to_str().ok()),
            Some("https://haven.app")
        );
        let plain = ModelEndpoint {
            provider: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: "sk-test".into(),
            ..Default::default()
        };
        assert!(!is_openrouter(&plain));
        assert!(
            build_headers(&plain, "Authorization", true)
                .get("X-Title")
                .is_none()
        );
    }

    #[test]
    fn map_deepseek_effort_follows_official_mapping() {
        assert_eq!(map_deepseek_effort("low"), "low");
        assert_eq!(map_deepseek_effort("medium"), "high");
        assert_eq!(map_deepseek_effort("high"), "high");
        assert_eq!(map_deepseek_effort("xhigh"), "high");
        assert_eq!(map_deepseek_effort("max"), "max");
        assert_eq!(map_deepseek_effort("none"), "none");
        assert_eq!(map_deepseek_effort("off"), "none");
    }

    #[test]
    fn chat_thinking_extras_deepseek_toggle_and_effort() {
        let base = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model_name: "deepseek-v4-pro".into(),
            ..Default::default()
        };
        let (thinking, effort) = chat_thinking_extras(&base);
        assert!(thinking.is_none());
        assert!(effort.is_none());

        let enabled = ModelEndpoint {
            reasoning_effort: Some("medium".into()),
            ..base.clone()
        };
        let (thinking, effort) = chat_thinking_extras(&enabled);
        assert_eq!(thinking, Some(serde_json::json!({"type": "enabled"})));
        assert_eq!(effort.as_deref(), Some("high"));

        let disabled = ModelEndpoint {
            reasoning_effort: Some("off".into()),
            ..base
        };
        let (thinking, effort) = chat_thinking_extras(&disabled);
        assert_eq!(thinking, Some(serde_json::json!({"type": "disabled"})));
        assert!(effort.is_none());
    }

    #[test]
    fn chat_thinking_extras_kimi_type_and_keep() {
        let k26 = ModelEndpoint {
            provider: "moonshot".into(),
            base_url: "https://api.moonshot.cn/v1".into(),
            model_name: "kimi-k2.6".into(),
            reasoning_effort: Some("high".into()),
            ..Default::default()
        };
        let (thinking, effort) = chat_thinking_extras(&k26);
        assert_eq!(
            thinking,
            Some(serde_json::json!({"type": "enabled", "keep": "all"}))
        );
        assert!(effort.is_none());

        let k25 = ModelEndpoint {
            model_name: "kimi-k2.5".into(),
            reasoning_effort: Some("low".into()),
            ..k26.clone()
        };
        let (thinking, effort) = chat_thinking_extras(&k25);
        assert_eq!(thinking, Some(serde_json::json!({"type": "enabled"})));
        assert!(effort.is_none());

        let k27 = ModelEndpoint {
            model_name: "kimi-k2.7-code".into(),
            reasoning_effort: Some("high".into()),
            ..k26.clone()
        };
        let (thinking, effort) = chat_thinking_extras(&k27);
        assert!(thinking.is_none());
        assert!(effort.is_none());

        let k3 = ModelEndpoint {
            model_name: "kimi-k3".into(),
            reasoning_effort: Some("medium".into()),
            ..k26
        };
        let (thinking, effort) = chat_thinking_extras(&k3);
        assert!(thinking.is_none());
        assert_eq!(effort.as_deref(), Some("high"));
    }

    #[test]
    fn responses_reasoning_config_openai_and_deepseek() {
        let openai = ModelEndpoint {
            provider: "openai".into(),
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        assert_eq!(
            responses_reasoning_config(&openai),
            Some(serde_json::json!({"effort": "medium"}))
        );
        let openai_off = ModelEndpoint {
            reasoning_effort: Some("off".into()),
            ..openai
        };
        assert!(responses_reasoning_config(&openai_off).is_none());

        let deepseek = ModelEndpoint {
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com".into(),
            model_name: "deepseek-v4-flash".into(),
            reasoning_effort: Some("medium".into()),
            ..Default::default()
        };
        assert_eq!(
            responses_reasoning_config(&deepseek),
            Some(serde_json::json!({"effort": "high"}))
        );
        let off = ModelEndpoint {
            reasoning_effort: Some("none".into()),
            ..deepseek
        };
        assert_eq!(
            responses_reasoning_config(&off),
            Some(serde_json::json!({"effort": "none"}))
        );
        assert!(responses_reasoning_config(&ModelEndpoint::default()).is_none());
    }

    #[test]
    fn chat_thinking_extras_openai_off_omits_effort() {
        let ep = ModelEndpoint {
            provider: "openai".into(),
            reasoning_effort: Some("off".into()),
            ..Default::default()
        };
        let (thinking, effort) = chat_thinking_extras(&ep);
        assert!(thinking.is_none());
        assert!(effort.is_none());
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
        let deepseek_alias = ModelEndpoint {
            api_style: Some("deepseek-responses".into()),
            provider: "deepseek".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&deepseek_alias), "openai-responses");
        assert_eq!(adapter_for(&deepseek_alias).style(), "openai-responses");
        let openai = ModelEndpoint::default();
        assert_eq!(adapter_for(&openai).style(), "openai-chat");
        // llama.cpp speaks the OpenAI-compatible wire protocol and is served by
        // the same adapter, so its reported style matches openai-chat.
        let llama = ModelEndpoint {
            provider: "llama.cpp".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&llama).style(), "openai-chat");
        let xai = ModelEndpoint {
            api_style: Some("xai".into()),
            provider: "xai".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&xai).style(), "xai");
        let grok_provider = ModelEndpoint {
            provider: "grok".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&grok_provider), "xai");
        assert_eq!(adapter_for(&grok_provider).style(), "xai");
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
        assert!(supports_builtin_web_search("openai-responses"));
        assert!(supports_builtin_web_search("xai"));
        assert!(!supports_builtin_web_search("openai-chat"));
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
    fn web_search_result_of_reads_deepseek_citations() {
        use serde_json::json;
        let item = json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": ["capital of France"],
                "citations": [
                    {"id": "c1", "title": "Paris — Wikipedia", "url": "https://en.wikipedia.org/wiki/Paris", "snippet": "Paris is the capital of France.", "source": "wikipedia"},
                    {"id": "c2", "title": "France", "url": "https://example.com/france"}
                ]
            }
        });
        let result = web_search_result_of(&item).expect("result payload");
        assert_eq!(result["queries"], json!(["capital of France"]));
        assert_eq!(result["results"][0]["title"], "Paris — Wikipedia");
        assert_eq!(
            result["results"][0]["url"],
            "https://en.wikipedia.org/wiki/Paris"
        );
        assert_eq!(result["results"][1]["snippet"], "");
    }

    #[test]
    fn web_search_result_of_accepts_flat_string_citations() {
        use serde_json::json;
        // xAI Live Search folds top-level URL citations into the item.
        let item = json!({
            "type": "web_search_call",
            "id": "xai_citations",
            "status": "completed",
            "action": {"type": "search", "queries": [], "citations": ["https://a.com", "https://b.com"]}
        });
        let result = web_search_result_of(&item).expect("result payload");
        assert_eq!(result["results"][0]["url"], "https://a.com");
        assert_eq!(result["results"][0]["title"], "https://a.com");
    }

    #[test]
    fn web_search_result_of_reads_anthropic_result_payload() {
        use serde_json::json;
        let item = json!({
            "type": "web_search_call",
            "id": "ws_result_3",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": [],
                "result": {
                    "query": "best laptop 2026",
                    "results": [
                        {"title": "Top Laptops", "url": "https://reviews.example/laptops", "content": "long content…"}
                    ]
                }
            }
        });
        let result = web_search_result_of(&item).expect("result payload");
        assert_eq!(result["queries"], json!(["best laptop 2026"]));
        assert_eq!(result["results"][0]["title"], "Top Laptops");
        assert_eq!(result["results"][0]["snippet"], "long content…");
    }

    #[test]
    fn web_search_result_of_empty_item_returns_none() {
        use serde_json::json;
        // Bare skeleton / Gemini grounding query-only item: no return value.
        assert!(
            web_search_result_of(
                &json!({"type": "web_search_call", "id": "ws_1", "status": "in_progress"})
            )
            .is_none()
        );
        assert!(web_search_result_of(&json!({"type": "web_search_call", "id": "gemini_grounding", "status": "completed", "action": {"type": "search", "queries": ["foo"]}})).is_some());
        assert!(web_search_result_of(&json!("nope")).is_none());
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

    #[test]
    fn upsert_web_search_call_keeps_rich_item_over_skeleton() {
        use serde_json::json;
        let mut calls = vec![json!({
            "type": "web_search_call",
            "id": "ws_1",
            "status": "completed",
            "action": {
                "type": "search",
                "queries": ["capital of France"],
                "citations": [{"title": "Paris", "url": "https://ex"}]
            }
        })];
        upsert_web_search_call(
            &mut calls,
            json!({"type": "web_search_call", "id": "ws_1", "status": "completed"}),
        );
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["action"]["citations"][0]["url"], "https://ex");
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

    #[test]
    fn openai_embeddings_url_chat_keeps_base() {
        assert_eq!(
            openai_embeddings_url("https://api.openai.com/v1", false),
            "https://api.openai.com/v1/embeddings"
        );
        assert_eq!(
            openai_embeddings_url("http://127.0.0.1:11434/v1/", false),
            "http://127.0.0.1:11434/v1/embeddings"
        );
    }

    #[test]
    fn openai_embeddings_url_responses_adds_v1() {
        assert_eq!(
            openai_embeddings_url("https://api.deepseek.com", true),
            "https://api.deepseek.com/v1/embeddings"
        );
        assert_eq!(
            openai_embeddings_url("https://api.openai.com/v1", true),
            "https://api.openai.com/v1/embeddings"
        );
    }

    #[test]
    fn parse_openai_embed_response_sorts_by_index() {
        let body = r#"{
            "data": [
                {"embedding": [3.0], "index": 1},
                {"embedding": [1.0, 2.0], "index": 0}
            ],
            "model": "text-embedding-3-small",
            "usage": {"prompt_tokens": 4, "total_tokens": 4}
        }"#;
        let emb = parse_openai_embed_response(body, 2, "fallback").unwrap();
        assert_eq!(emb.vectors, vec![vec![1.0, 2.0], vec![3.0]]);
        assert_eq!(emb.model.as_deref(), Some("text-embedding-3-small"));
        assert_eq!(emb.usage.prompt_tokens, 4);
    }

    #[test]
    fn parse_openai_embed_response_rejects_count_mismatch() {
        let body = r#"{"data":[{"embedding":[0.1],"index":0}]}"#;
        let err = parse_openai_embed_response(body, 2, "m").unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(msg) if msg.contains("count mismatch")));
    }

    #[tokio::test]
    async fn openai_responses_adapter_embeds_via_embeddings_endpoint() {
        let url = super::serve_once(
            "200 OK",
            "application/json",
            r#"{"data":[{"embedding":[0.1,0.2],"index":0}],"model":"text-embedding-3-small","usage":{"prompt_tokens":2,"total_tokens":2}}"#,
        )
        .await;
        let ep = ModelEndpoint {
            api_style: Some("openai-responses".into()),
            base_url: url,
            model_name: "text-embedding-3-small".into(),
            timeout_secs: 5,
            api_key: "sk".into(),
            ..Default::default()
        };
        let emb = adapter_for(&ep).embed(vec!["hello".into()]).await.unwrap();
        assert_eq!(emb.vectors, vec![vec![0.1, 0.2]]);
        assert_eq!(emb.model.as_deref(), Some("text-embedding-3-small"));
    }

    #[tokio::test]
    async fn openai_responses_embed_empty_input_skips_http() {
        let ep = ModelEndpoint {
            api_style: Some("openai-responses".into()),
            model_name: "emb".into(),
            ..Default::default()
        };
        let emb = adapter_for(&ep).embed(Vec::new()).await.unwrap();
        assert!(emb.vectors.is_empty());
        assert_eq!(emb.model.as_deref(), Some("emb"));
    }

    #[tokio::test]
    async fn anthropic_embed_stays_unsupported() {
        let ep = ModelEndpoint {
            provider: "anthropic".into(),
            model_name: "claude".into(),
            ..Default::default()
        };
        let err = adapter_for(&ep).embed(vec!["x".into()]).await.unwrap_err();
        assert!(err.is_unsupported());
    }

    #[tokio::test]
    async fn gemini_adapter_embeds_via_batch_embed_contents() {
        let url = super::serve_once(
            "200 OK",
            "application/json",
            r#"{"embeddings":[{"values":[0.5,0.6]}]}"#,
        )
        .await;
        let ep = ModelEndpoint {
            provider: "gemini".into(),
            base_url: url,
            model_name: "text-embedding-004".into(),
            timeout_secs: 5,
            api_key: "AIza".into(),
            ..Default::default()
        };
        let emb = adapter_for(&ep).embed(vec!["hello".into()]).await.unwrap();
        assert_eq!(emb.vectors, vec![vec![0.5, 0.6]]);
        assert_eq!(emb.model.as_deref(), Some("text-embedding-004"));
    }
}
