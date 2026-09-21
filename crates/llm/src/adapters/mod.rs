pub mod anthropic;
pub mod assemblyai;
pub mod capabilities;
pub mod deepgram;
mod embedding;
pub mod gemini;
pub mod openai;
pub mod openai_responses;
mod provider_features;
mod stream;
mod transport;
mod web_search;

pub use anthropic::AnthropicAdapter;
pub use capabilities::{
    WebSearchMode, is_known_api_style, is_openai_family_wire_style, is_stt_only_style,
    is_tts_only_style, normalize_api_style, parse_web_search_mode, resolve_web_search_mode,
    supports_builtin_web_search, xai_search_mode,
};
pub use openai::OpenAiAdapter;

use crate::client::LlmClient;
use crate::types::LlmError;
use haven_common::config::ModelEndpoint;
use haven_common::media::{CapabilityProfile, CapabilitySupport};
use haven_common::prompts::COMPACTED_SUMMARY_PREFIX;
use haven_common::types::{ContentPart, InjectSource};

pub(crate) use embedding::{openai_compatible_embed, openai_embeddings_url};
pub(crate) use provider_features::{
    chat_thinking_extras, is_deepseek, is_openrouter, reasoning_tail,
    reasoning_text_from_thinking_blocks, requires_reasoning_echo, responses_output_config,
    responses_reasoning_config,
};
pub(crate) use stream::{LineMode, empty_chunk, spawn_line_reader};
pub(crate) use transport::{
    MAX_AUDIO_RESPONSE_BYTES, MAX_JSON_RESPONSE_BYTES, MAX_OCR_RESPONSE_BYTES, build_client,
    build_headers, health_check_request, read_bytes_bounded, read_json_bounded, read_text_bounded,
    send_request, stream_header_timeout,
};
pub use web_search::web_search_result_of;
pub(crate) use web_search::{normalize_web_search_call_item, upsert_web_search_call};

/// Capabilities guaranteed by a chat wire protocol. Model-specific limits
/// remain unknown; this helper only records representations the adapter can
/// serialize without inferring a model name.
pub(crate) fn chat_capability_profile(
    image: CapabilitySupport,
    audio: CapabilitySupport,
) -> CapabilityProfile {
    CapabilityProfile {
        image,
        audio,
        tools: CapabilitySupport::Supported,
        ..CapabilityProfile::default()
    }
}

/// Return a stable, non-sensitive marker for the latest compaction root in a
/// canonical transcript.  Compaction deliberately preserves the early
/// anchor, but inserts a summary before the recent suffix; that changes the
/// provider prefix after the anchor.  OpenAI routing keys use this marker to
/// start a fresh cache shard while remaining stable for subsequent turns.
pub(crate) fn prompt_cache_compaction_marker(
    messages: &[haven_common::types::CanonicalMessage],
) -> Option<Vec<u8>> {
    let summary = messages.iter().rev().find(|message| {
        message.role == haven_common::types::CanonicalRole::Assistant
            && message.content.iter().any(|part| {
                matches!(part, ContentPart::Text(text) if text.starts_with(COMPACTED_SUMMARY_PREFIX))
            })
    })?;
    let value = serde_json::to_value(summary).ok()?;
    Some(crate::types::stable_json_bytes(&value))
}

/// Describe only the raw media surface of a provider-visible request.  Media
/// bytes are intentionally excluded: a new image should extend the prompt,
/// not force a new routing shard, while switching between raw media and a
/// text fallback must not reuse an incompatible prefix.
pub(crate) fn prompt_cache_media_marker(
    messages: &[haven_common::types::CanonicalMessage],
) -> Option<Vec<u8>> {
    let mut surface = Vec::new();
    for message in messages {
        for part in &message.content {
            let (kind, content_type, media_type) = match part {
                ContentPart::Image {
                    content_type,
                    media_type,
                    ..
                } => ("image", content_type, media_type),
                ContentPart::Audio {
                    content_type,
                    media_type,
                    ..
                } => ("audio", content_type, media_type),
                ContentPart::Video {
                    content_type,
                    media_type,
                    ..
                } => ("video", content_type, media_type),
                ContentPart::Text(_) => continue,
            };
            surface.push(serde_json::json!({
                "kind": kind,
                "content_type": content_type,
                "media_type": media_type,
            }));
        }
    }
    (!surface.is_empty()).then(|| crate::types::stable_json_bytes(&serde_json::json!(surface)))
}

/// Adapter returned when endpoint construction fails. Keeping the failure in
/// the client preserves the router's existing factory API while ensuring the
/// first operation reports the actual configuration error instead of silently
/// using reqwest's default client.
struct UnavailableLlmClient {
    style: &'static str,
    error: LlmError,
}

#[async_trait::async_trait]
impl LlmClient for UnavailableLlmClient {
    fn style(&self) -> &'static str {
        self.style
    }

    async fn chat(
        &self,
        _messages: Vec<haven_common::types::CanonicalMessage>,
    ) -> Result<crate::types::LlmResponse, LlmError> {
        Err(self.error.clone())
    }

    async fn chat_stream(
        &self,
        _messages: Vec<haven_common::types::CanonicalMessage>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures_util::Stream<Item = Result<crate::types::StreamChunk, LlmError>> + Send,
            >,
        >,
        LlmError,
    > {
        Err(self.error.clone())
    }

    async fn chat_stream_with_tools_output_cap_shared(
        &self,
        _messages: std::sync::Arc<[haven_common::types::CanonicalMessage]>,
        _tools: std::sync::Arc<[crate::types::ToolDefinition]>,
        _max_output_tokens: Option<u32>,
    ) -> Result<
        std::pin::Pin<
            Box<
                dyn futures_util::Stream<Item = Result<crate::types::StreamChunk, LlmError>> + Send,
            >,
        >,
        LlmError,
    > {
        Err(self.error.clone())
    }

    async fn embed(&self, _input: Vec<String>) -> Result<crate::types::Embedding, LlmError> {
        Err(self.error.clone())
    }

    async fn transcribe(&self, _wav_data: &[u8]) -> Result<crate::types::SttResult, LlmError> {
        Err(self.error.clone())
    }

    async fn health_check(&self) -> Result<(), LlmError> {
        Err(self.error.clone())
    }
}

fn unavailable(style: &'static str, error: LlmError) -> Box<dyn LlmClient> {
    tracing::error!(adapter = style, error = %error, "LLM adapter construction failed");
    Box::new(UnavailableLlmClient { style, error })
}

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

/// Resolve the wire protocol style for an endpoint. An explicit canonical
/// `api_style` wins; an omitted style uses the neutral OpenAI-compatible
/// protocol. Unknown values remain invalid and are never remapped.
pub fn api_style_for(endpoint: &ModelEndpoint) -> &'static str {
    if let Some(style) = &endpoint.api_style
        && !style.trim().is_empty()
    {
        return normalize_api_style(style);
    }
    "openai-chat"
}

/// Build the protocol adapter for an endpoint.
///
/// Dispatch happens on the resolved canonical `api_style` (see
/// `api_style_for`):
/// - `openai-chat` / `llama.cpp`: OpenAI-compatible `/chat/completions`
///   (OpenAI, Ollama, vLLM, DeepSeek chat, llama.cpp server, and most
///   third-party gateways). Whisper-family models also implement `transcribe`
///   via `/audio/transcriptions`. Embeddings use `/embeddings`.
/// - `xai`: same OpenAI chat adapter with xAI Live Search `search_parameters`
///   (embeddings still `/embeddings`)
/// - `openai-responses`: OpenAI Responses API
///   (`/v1/responses`), including DeepSeek thinking + built-in `web_search`.
///   Embeddings still use the OpenAI-compatible `/v1/embeddings` path — chat
///   wire style does not apply to that endpoint.
/// - `anthropic`: Anthropic Messages API (+ optional server `web_search`);
///   no embeddings API
/// - `gemini`: Google Gemini API (+ optional `google_search` grounding);
///   embeddings via `batchEmbedContents`
/// - `deepgram` / `assemblyai`: speech-to-text only
pub fn adapter_for(endpoint: &ModelEndpoint) -> Box<dyn LlmClient> {
    let style = api_style_for(endpoint);
    if style == "invalid" {
        return unavailable(
            "invalid",
            LlmError::Configuration(format!(
                "unsupported api_style '{}'; choose a canonical wire protocol",
                endpoint.api_style.as_deref().unwrap_or_default()
            )),
        );
    }
    match style {
        "anthropic" => anthropic::AnthropicAdapter::try_new(endpoint.clone())
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("anthropic", error)),
        "gemini" => gemini::GeminiAdapter::try_new(endpoint.clone())
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("gemini", error)),
        "openai-responses" => openai_responses::OpenAiResponsesAdapter::try_new(endpoint.clone())
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("openai-responses", error)),
        "deepgram" => deepgram::DeepgramAdapter::try_new(endpoint.clone())
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("deepgram", error)),
        "assemblyai" => assemblyai::AssemblyAiAdapter::try_new(endpoint.clone())
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("assemblyai", error)),
        "xai" => openai::OpenAiAdapter::try_new_with_style(endpoint.clone(), "xai")
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("xai", error)),
        _ => openai::OpenAiAdapter::try_new(endpoint.clone())
            .map(|adapter| Box::new(adapter) as Box<dyn LlmClient>)
            .unwrap_or_else(|error| unavailable("openai-chat", error)),
    }
}

pub(crate) fn resolve_cached_tokens(nested: Option<u32>, flat_alias: u32) -> u32 {
    nested.unwrap_or(0).max(flat_alias)
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
    use std::time::Duration;

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
        assert_eq!(stream_header_timeout(None), Some(Duration::from_secs(60)));
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
        let headers = build_headers(&ep, "Authorization", true).unwrap();
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
                .unwrap()
                .get("X-Title")
                .is_none()
        );
    }

    #[test]
    fn invalid_transport_configuration_is_not_silently_repaired() {
        let invalid_proxy = ModelEndpoint {
            proxy_url: Some("not a proxy URL".into()),
            ..Default::default()
        };
        assert!(matches!(
            build_client(&invalid_proxy),
            Err(LlmError::Configuration(_))
        ));

        let invalid_header = ModelEndpoint {
            api_key: "key\nwith-control".into(),
            ..Default::default()
        };
        assert!(matches!(
            build_headers(&invalid_header, "Authorization", true),
            Err(LlmError::Configuration(_))
        ));
    }

    #[tokio::test]
    async fn adapter_factory_preserves_construction_failure() {
        let endpoint = ModelEndpoint {
            proxy_url: Some("not a proxy URL".into()),
            ..Default::default()
        };
        let error = adapter_for(&endpoint).health_check().await.unwrap_err();
        assert!(matches!(error, LlmError::Configuration(_)));
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
    fn api_style_without_explicit_style_uses_neutral_default() {
        let anthropic = ModelEndpoint {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&anthropic), "openai-chat");
        let google = ModelEndpoint {
            provider: "google".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&google), "openai-chat");
        let gemini = ModelEndpoint {
            provider: "gemini".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&gemini), "openai-chat");
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
        assert_eq!(api_style_for(&llama), "openai-chat");
        let deepgram = ModelEndpoint {
            provider: "deepgram".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&deepgram), "openai-chat");
        let assemblyai = ModelEndpoint {
            provider: "assemblyai".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&assemblyai), "openai-chat");
    }

    #[test]
    fn adapter_for_dispatches_by_style() {
        let anthropic = ModelEndpoint {
            provider: "anthropic".into(),
            api_style: Some("anthropic".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&anthropic).style(), "anthropic");
        let gemini = ModelEndpoint {
            provider: "google".into(),
            api_style: Some("gemini".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&gemini).style(), "gemini");
        let responses = ModelEndpoint {
            api_style: Some("openai-responses".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&responses).style(), "openai-responses");
        let invalid_style = ModelEndpoint {
            api_style: Some("deepseek-responses".into()),
            provider: "deepseek".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&invalid_style), "invalid");
        assert_eq!(adapter_for(&invalid_style).style(), "invalid");
        let openai = ModelEndpoint::default();
        assert_eq!(adapter_for(&openai).style(), "openai-chat");
        // llama.cpp speaks the OpenAI-compatible wire protocol and is served by
        // the same adapter, so its reported style matches openai-chat.
        let llama = ModelEndpoint {
            provider: "llama.cpp".into(),
            api_style: Some("llama.cpp".into()),
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
            api_style: Some("xai".into()),
            ..Default::default()
        };
        assert_eq!(api_style_for(&grok_provider), "xai");
        assert_eq!(adapter_for(&grok_provider).style(), "xai");
        let deepgram = ModelEndpoint {
            provider: "deepgram".into(),
            api_style: Some("deepgram".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&deepgram).style(), "deepgram");
        let assemblyai = ModelEndpoint {
            provider: "assemblyai".into(),
            api_style: Some("assemblyai".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&assemblyai).style(), "assemblyai");
        assert!(supports_builtin_web_search("openai-responses"));
        assert!(supports_builtin_web_search("xai"));
        assert!(!supports_builtin_web_search("openai-chat"));
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
            api_style: Some("anthropic".into()),
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
            api_style: Some("gemini".into()),
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
