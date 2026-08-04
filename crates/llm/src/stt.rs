//! Speech-to-text (STT) capability, unified with the chat/vision adapters.
//!
//! Every transcription implementation lives here next to the `LlmClient`
//! adapters, and [`build_stt_client`] is the single dispatch entry point
//! (the STT counterpart of `adapters::adapter_for`). Providers:
//! - `none`: no client
//! - `mcp`: route through an MCP server exposing `stt.transcribe`
//! - `llm`: transcribe via the `audio_model` LLM endpoint (see
//!   [`LlmSttAdapter`])
//! - `openai` / `groq`: OpenAI-Whisper-compatible `/audio/transcriptions`
//! - `gemini`: Google Gemini `generateContent` audio transcription
//! - `deepgram`: Deepgram REST `/v1/listen`
//! - `assemblyai`: AssemblyAI `/v2/transcript` job polling

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use haven_common::config::SttConfig;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::types::{ContentPart, LlmMessage, LlmRole};
use crate::LlmRouter;

/// Trait for speech-to-text conversion.
/// Implementations receive WAV bytes and return transcribed text.
#[async_trait]
pub trait SttClient: Send + Sync {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String>;
}

/// Outcome of an MCP tool invocation, decoupled from the tools crate's
/// `ToolResult` so the llm crate (which cannot depend on tools) can build
/// `McpSttClient` against a generic caller.
pub struct McpToolOutcome {
    pub success: bool,
    pub error: Option<String>,
    pub output: Value,
}

/// Generic MCP tool invocation surface implemented by the tools crate's
/// `McpManager`. Keeps `McpSttClient` dependency-free.
#[async_trait]
pub trait McpToolCaller: Send + Sync {
    async fn invoke_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpToolOutcome>;
}

/// Build the STT client for a given config. Returns `None` when the configured
/// provider does not need a client (e.g. `none`), and returns an error for an
/// unknown provider id. `mcp` is the MCP caller injected by the app layer
/// (required only for the `mcp` provider).
pub fn build_stt_client(
    router: Arc<LlmRouter>,
    mcp: Option<Arc<dyn McpToolCaller>>,
    cfg: &SttConfig,
) -> Result<Option<Box<dyn SttClient>>> {
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let client: Box<dyn SttClient> = match cfg.provider.as_str() {
        "none" => return Ok(None),
        "mcp" => {
            let server = cfg.mcp_server.clone().ok_or_else(|| {
                anyhow::anyhow!("STT provider is 'mcp' but no mcp_server is configured")
            })?;
            let caller = mcp.ok_or_else(|| {
                anyhow::anyhow!("STT provider is 'mcp' but no MCP caller is available")
            })?;
            Box::new(McpSttClient::new(caller, &server, cfg.timeout_secs))
        }
        "llm" => Box::new(LlmSttAdapter::new(router)),
        "openai" => Box::new(OpenAiWhisperClient::new(
            cfg,
            cfg.base_url.clone(),
            timeout,
        )),
        "groq" => Box::new(OpenAiWhisperClient::new(
            cfg,
            "https://api.groq.com/openai/v1".into(),
            timeout,
        )),
        "gemini" => Box::new(GeminiSttClient::new(cfg, timeout)),
        "deepgram" => Box::new(DeepgramSttClient::new(cfg, timeout)),
        "assemblyai" => Box::new(AssemblyAiSttClient::new(cfg, timeout)),
        other => anyhow::bail!("unknown STT provider: {}", other),
    };
    Ok(Some(client))
}

/// STT client that routes transcription through an MCP server.
/// Calls the `stt.transcribe` tool with base64-encoded WAV audio.
pub struct McpSttClient {
    caller: Arc<dyn McpToolCaller>,
    server_name: String,
    timeout: Duration,
}

impl McpSttClient {
    pub fn new(caller: Arc<dyn McpToolCaller>, server_name: &str, timeout_secs: u64) -> Self {
        Self {
            caller,
            server_name: server_name.into(),
            timeout: Duration::from_secs(timeout_secs),
        }
    }
}

#[async_trait]
impl SttClient for McpSttClient {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let audio_b64 = base64::engine::general_purpose::STANDARD.encode(wav_data);

        let input = serde_json::json!({
            "audio": audio_b64,
        });

        let cancel = CancellationToken::new();

        let result = tokio::time::timeout(self.timeout, async {
            self.caller
                .invoke_tool(&self.server_name, "stt.transcribe", input, cancel)
                .await
        })
        .await
        .map_err(|_| anyhow::anyhow!("STT transcription timed out after {:?}", self.timeout))??;

        if !result.success {
            let msg = result.error.unwrap_or_else(|| "unknown error".into());
            anyhow::bail!("STT transcription failed: {}", msg);
        }

        let text = result.output["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("STT response missing 'text' field"))?
            .to_string();

        Ok(text)
    }
}

/// Fallback STT adapter that transcribes audio through the `audio_model` LLM
/// endpoint. Sends base64 WAV audio as an `input_audio` content part; the
/// model's text reply is returned verbatim as the transcription.
pub struct LlmSttAdapter {
    router: Arc<LlmRouter>,
}

impl LlmSttAdapter {
    pub fn new(router: Arc<LlmRouter>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl SttClient for LlmSttAdapter {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let role = match self.router.stt_role().await {
            Some(role) => role,
            None => {
                return Err(anyhow::anyhow!(
                    "audio_model endpoint is not configured; configure it in Settings -> LLM to use LLM-based transcription"
                ));
            }
        };
        let data = base64::engine::general_purpose::STANDARD.encode(wav_data);
        let messages = vec![
            LlmMessage {
                role: LlmRole::System,
                content: vec![ContentPart::text(
                    "You are a speech-to-text engine. Transcribe the audio verbatim in the speaker's language. Output only the transcription text, no commentary.",
                )],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::Audio {
                    content_type: "input_audio".into(),
                    media_type: "audio/wav".into(),
                    data,
                }],
                tool_call_id: None,
                tool_calls: None,
                reasoning: None,
            },
        ];

        // Streaming aggregated path: avoids the endpoint's non-streaming
        // `timeout_secs` (default 7s), which is too short for transcription of
        // longer recordings. The router-level `max_total_duration_secs` still
        // applies as an overall deadline.
        let resp = match self
            .router
            .chat_stream_with_tools_aggregated(role, messages, Vec::new(), |_| {})
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // The `input_audio` content part is OpenAI-specific. Many
                // third-party OpenAI-compatible providers (or non-audio
                // OpenAI models) reject it with a 400 like
                // "Only text and image_url are supported." Surface a setup
                // hint instead of the raw upstream error so the user knows
                // to either pick an audio-capable model or switch STT
                // provider.
                let msg = e.to_string();
                if msg.contains("input_audio")
                    || msg.contains("Only text and image_url are supported")
                {
                    return Err(anyhow::anyhow!(
                        "audio_model endpoint does not support audio input ({msg}). \
                         Configure audio_model to a model that accepts audio \
                         (e.g. openai/gpt-4o-audio-preview), or set STT Provider \
                         to an MCP server that exposes `stt.transcribe`."
                    ));
                }
                return Err(anyhow::Error::from(e));
            }
        };
        let text = resp.text.trim().to_string();
        // Empty transcription (silence / too-short clip) is not an error:
        // the caller treats it as "no speech" and skips the message.
        Ok(text)
    }
}

// ---------------------------------------------------------------------------
// HTTP STT / ASR adapters
// ---------------------------------------------------------------------------

/// Build an HTTP client with a transport timeout for cloud STT providers.
fn stt_http_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default()
}

/// Error text extraction for STT HTTP responses, so upstream error bodies
/// surface as helpful messages instead of raw HTTP status numbers.
fn stt_error_body(status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        anyhow::anyhow!("STT request failed: HTTP {}", status)
    } else {
        let snippet = if trimmed.len() > 300 {
            &trimmed[..300]
        } else {
            trimmed
        };
        anyhow::anyhow!("STT request failed (HTTP {}): {}", status, snippet)
    }
}

/// Transcribe via an OpenAI-Whisper-compatible `/audio/transcriptions`
/// multipart endpoint. This is the wire format spoken by OpenAI, Groq,
/// Together, Deepgram's OpenAI-compatible endpoint, and local Whisper-style
/// servers (whisper.cpp via WhisperServer, LM Studio, etc.).
pub struct OpenAiWhisperClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiWhisperClient {
    pub fn new(cfg: &SttConfig, base_url: String, timeout: Duration) -> Self {
        let client = stt_http_client(timeout);
        let base_url = if base_url.trim().is_empty() {
            "https://api.openai.com/v1".to_string()
        } else {
            base_url.trim_end_matches('/').to_string()
        };
        Self {
            client,
            base_url,
            api_key: cfg.api_key.clone(),
            model: if cfg.model.is_empty() {
                "whisper-1".into()
            } else {
                cfg.model.clone()
            },
        }
    }
}

#[async_trait]
impl SttClient for OpenAiWhisperClient {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let form = reqwest::multipart::Form::new()
            .part(
                "file",
                reqwest::multipart::Part::bytes(wav_data.to_vec())
                    .file_name("audio.wav")
                    .mime_str("audio/wav")?,
            )
            .text("model", self.model.clone())
            .text("response_format", "json");

        let url = format!("{}/audio/transcriptions", self.base_url);
        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Whisper STT request failed: {}", e))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .replace('\n', " ");
        if !status.is_success() {
            return Err(stt_error_body(status, &body));
        }

        // Whisper returns `{ "text": "..." }`. An empty transcription
        // (silence / too-short clip) is not an error — the caller treats it
        // as "no speech" and skips the message.
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Whisper STT returned invalid JSON: {}", e))?;
        let text = json["text"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Whisper STT response missing 'text' field"))?
            .trim()
            .to_string();
        Ok(text)
    }
}

/// Transcribe via Deepgram REST (`POST /v1/listen`). Sends raw audio bytes
/// with the configured model (default `nova-2`).
pub struct DeepgramSttClient {
    client: reqwest::Client,
    api_key: String,
    model: String,
}

impl DeepgramSttClient {
    pub fn new(cfg: &SttConfig, timeout: Duration) -> Self {
        let client = stt_http_client(timeout);
        let api_key = cfg
            .api_key
            .strip_prefix("Deepgram ")
            .map(str::to_string)
            .unwrap_or_else(|| {
                if cfg.api_key.is_empty() {
                    String::new()
                } else {
                    format!("Token {}", cfg.api_key)
                }
            });
        Self {
            client,
            api_key,
            model: if cfg.model.is_empty() {
                "nova-3".into()
            } else {
                cfg.model.clone()
            },
        }
    }
}

#[async_trait]
impl SttClient for DeepgramSttClient {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let url = format!(
            "https://api.deepgram.com/v1/listen?model={}&smart_format=true",
            self.model
        );
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, format!("Token {}", self.api_key))
            .header(reqwest::header::CONTENT_TYPE, "audio/wav")
            .body(wav_data.to_vec())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Deepgram STT request failed: {}", e))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .replace('\n', " ");
        if !status.is_success() {
            return Err(stt_error_body(status, &body));
        }

        // Deepgram nests text under `results.channels[0].alternatives[0].transcript`.
        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Deepgram STT returned invalid JSON: {}", e))?;
        let text = json["results"]["channels"][0]["alternatives"][0]["transcript"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Deepgram STT response missing transcript"))?
            .trim()
            .to_string();
        Ok(text)
    }
}

/// Transcribe via AssemblyAI: upload raw audio to `/v2/upload`, create a job
/// at `/v2/transcript`, then poll `GET /v2/transcript/{id}` until the status
/// reaches `completed`. AssemblyAI authenticates with the raw API key in the
/// `authorization` header (no `Bearer` prefix).
pub struct AssemblyAiSttClient {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    poll_interval: Duration,
}

impl AssemblyAiSttClient {
    pub fn new(cfg: &SttConfig, timeout: Duration) -> Self {
        let client = stt_http_client(timeout);
        let base_url = if cfg.base_url.is_empty() {
            "https://api.assemblyai.com".to_string()
        } else {
            cfg.base_url.trim_end_matches('/').to_string()
        };
        Self {
            client,
            api_key: cfg.api_key.clone(),
            base_url,
            model: cfg.model.clone(),
            poll_interval: Duration::from_secs(3),
        }
    }

    fn auth(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if !self.api_key.is_empty()
            && let Ok(v) = reqwest::header::HeaderValue::from_str(&self.api_key)
        {
            headers.insert("authorization", v);
        }
        headers
    }
}

#[async_trait]
impl SttClient for AssemblyAiSttClient {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let upload_url = self
            .client
            .post(format!("{}/v2/upload", self.base_url))
            .headers(self.auth())
            .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
            .body(wav_data.to_vec())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("AssemblyAI upload failed: {}", e))?;
        let upload_status = upload_url.status();
        let upload_body = upload_url
            .text()
            .await
            .unwrap_or_default()
            .replace('\n', " ");
        if !upload_status.is_success() {
            return Err(stt_error_body(upload_status, &upload_body));
        }
        let upload_json: serde_json::Value = serde_json::from_str(&upload_body)?;
        let audio_url = upload_json["upload_url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("AssemblyAI upload missing 'upload_url'"))?;

        // Submit the transcription job (speech_model mirrors the model picked
        // in the settings UI; empty means the provider default).
        let mut create_json = serde_json::json!({ "audio_url": audio_url });
        if !self.model.is_empty() && self.model != "assemblyai_default" {
            create_json["speech_model"] = serde_json::json!(self.model);
        }
        let create_resp = self
            .client
            .post(format!("{}/v2/transcript", self.base_url))
            .headers(self.auth())
            .json(&create_json)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("AssemblyAI job creation failed: {}", e))?;
        let create_status = create_resp.status();
        let create_body = create_resp
            .text()
            .await
            .unwrap_or_default()
            .replace('\n', " ");
        if !create_status.is_success() {
            return Err(stt_error_body(create_status, &create_body));
        }
        let create: serde_json::Value = serde_json::from_str(&create_body)?;
        let job_id = create["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("AssemblyAI response missing transcript id"))?;

        // Poll until completed (or errored).
        let job_url = format!("{}/v2/transcript/{}", self.base_url, job_id);
        loop {
            let poll_resp = self
                .client
                .get(&job_url)
                .headers(self.auth())
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("AssemblyAI polling failed: {}", e))?;
            let poll_status = poll_resp.status();
            let poll_body = poll_resp
                .text()
                .await
                .unwrap_or_default()
                .replace('\n', " ");
            if !poll_status.is_success() {
                return Err(stt_error_body(poll_status, &poll_body));
            }
            let job: serde_json::Value = serde_json::from_str(&poll_body)?;
            let status = job["status"].as_str().unwrap_or("");
            match status {
                "completed" => {
                    let text = job["text"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    return Ok(text);
                }
                "error" => {
                    let err = job["error"].as_str().unwrap_or("unknown error");
                    anyhow::bail!("AssemblyAI transcription error: {}", err);
                }
                _ => tokio::time::sleep(self.poll_interval).await,
            }
        }
    }
}

/// Transcribe via the Google Gemini API (`generateContent` with inline
/// audio). Sends base64 WAV audio as an `inline_data` part and asks the model
/// to return a verbatim transcript.
pub struct GeminiSttClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl GeminiSttClient {
    pub fn new(cfg: &SttConfig, timeout: Duration) -> Self {
        let client = stt_http_client(timeout);
        let base_url = if cfg.base_url.is_empty() {
            "https://generativelanguage.googleapis.com/v1beta".to_string()
        } else {
            cfg.base_url.trim_end_matches('/').to_string()
        };
        Self {
            client,
            base_url,
            api_key: cfg.api_key.clone(),
            model: if cfg.model.is_empty() {
                "gemini-2.5-flash".into()
            } else {
                cfg.model.clone()
            },
        }
    }
}

#[async_trait]
impl SttClient for GeminiSttClient {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<String> {
        let data = base64::engine::general_purpose::STANDARD.encode(wav_data);
        let body = serde_json::json!({
            "contents": [{
                "parts": [
                    {
                        "text": "You are a speech-to-text engine. Transcribe the audio verbatim in the speaker's language. Output only the transcription text, no commentary."
                    },
                    {
                        "inline_data": {
                            "mime_type": "audio/wav",
                            "data": data
                        }
                    }
                ]
            }]
        });

        let url = format!(
            "{}/models/{}:generateContent",
            self.base_url, self.model
        );
        let resp = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Gemini STT request failed: {}", e))?;

        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .replace('\n', " ");
        if !status.is_success() {
            return Err(stt_error_body(status, &body));
        }

        let json: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Gemini STT returned invalid JSON: {}", e))?;
        let text = json["candidates"][0]["content"]["parts"]
            .as_array()
            .and_then(|parts| parts.iter().find_map(|p| p["text"].as_str()))
            .ok_or_else(|| anyhow::anyhow!("Gemini STT response missing text part"))?
            .trim()
            .to_string();
        // Empty transcription (silence / too-short clip) is not an error.
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::Stream;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::client::LlmClient;
    use crate::types::{LlmError, LlmResponse, StreamChunk};
    use crate::EndpointRole;

    struct MockSttClient {
        response: String,
        call_count: AtomicU64,
    }

    #[async_trait]
    impl SttClient for MockSttClient {
        async fn transcribe(&self, _wav_data: &[u8]) -> Result<String> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_mock_stt_returns_text() {
        let client = MockSttClient {
            response: "hello world".into(),
            call_count: AtomicU64::new(0),
        };
        let wav = vec![0u8; 44]; // minimal WAV header
        let result = client.transcribe(&wav).await.unwrap();
        assert_eq!(result, "hello world");
        assert_eq!(client.call_count.load(Ordering::SeqCst), 1);
    }

    struct MockLlm {
        text: String,
        calls: AtomicU64,
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, messages: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // The audio payload must be carried in an Audio content part.
            let has_audio = messages.iter().any(|m| {
                m.content
                    .iter()
                    .any(|p| matches!(p, ContentPart::Audio { .. }))
            });
            if !has_audio {
                return Err(LlmError::ServerError("no audio part".into()));
            }
            Ok(LlmResponse {
                text: self.text.clone(),
                ..Default::default()
            })
        }

        async fn chat_stream(
            &self,
            _messages: Vec<LlmMessage>,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError>
        {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn mock_router(text: &str) -> Arc<LlmRouter> {
        let client: Arc<dyn LlmClient> = Arc::new(MockLlm {
            text: text.into(),
            calls: AtomicU64::new(0),
        });
        Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ))
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_transcribes_audio() {
        let router = mock_router("你好世界");
        router
            .force_role_configured(EndpointRole::AudioModel, true)
            .await;
        let adapter = LlmSttAdapter::new(router);
        let text = adapter.transcribe(&[0u8; 44]).await.unwrap();
        assert_eq!(text, "你好世界");
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_unconfigured_errors() {
        // Default (empty-key) config: audio_model not configured.
        let router = mock_router("ignored");
        let adapter = LlmSttAdapter::new(router);
        let err = adapter.transcribe(&[0u8; 44]).await.unwrap_err();
        assert!(err.to_string().contains("audio_model"));
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_uses_default_model_when_routing_disabled() {
        // `stt_use_audio_model = false`: transcription goes to the default
        // model, and the missing audio_model endpoint is not an error.
        let router = mock_router("走默认模型的转写");
        router.force_routing_flags(false, true).await;
        let adapter = LlmSttAdapter::new(router);
        let text = adapter.transcribe(&[0u8; 44]).await.unwrap();
        assert_eq!(text, "走默认模型的转写");
    }

    struct MockLlmErr {
        err: LlmError,
    }

    #[async_trait]
    impl LlmClient for MockLlmErr {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            Err(self.err.clone())
        }
        async fn chat_stream(
            &self,
            _: Vec<LlmMessage>,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError>
        {
            Ok(Box::pin(futures_util::stream::empty()))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_rewrites_unsupported_audio_input_error() {
        // Simulate a third-party provider that rejects `input_audio` with the
        // canonical OpenAI 400 error body. The adapter must rewrite it into a
        // setup hint instead of leaking the raw upstream message verbatim.
        let err_body = "400 Bad Request: Only text and image_url are supported.";
        let client: Arc<dyn LlmClient> = Arc::new(MockLlmErr {
            err: LlmError::RequestFailed(err_body.into()),
        });
        let router = Arc::new(LlmRouter::new_with_clients(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ));
        router
            .force_role_configured(EndpointRole::AudioModel, true)
            .await;
        let adapter = LlmSttAdapter::new(router);
        let err = adapter.transcribe(&[0u8; 44]).await.unwrap_err().to_string();
        assert!(
            err.contains("does not support audio input"),
            "expected rewritten hint, got: {err}"
        );
        assert!(
            err.contains("gpt-4o-audio-preview"),
            "expected model suggestion, got: {err}"
        );
    }

    /// No-op MCP caller for dispatch tests (only the `mcp` provider's
    /// missing-server error path is exercised, so the caller never fires).
    struct NoopMcpCaller;

    #[async_trait]
    impl McpToolCaller for NoopMcpCaller {
        async fn invoke_tool(
            &self,
            _server_name: &str,
            _tool_name: &str,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<McpToolOutcome> {
            Ok(McpToolOutcome {
                success: true,
                error: None,
                output: serde_json::json!({ "text": "mock" }),
            })
        }
    }

    fn test_stt_cfg(provider: &str) -> SttConfig {
        SttConfig {
            provider: provider.into(),
            api_key: "test-key".into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn build_stt_client_dispaches_by_provider() {
        // `none` yields no client; `mcp` without a server errors; cloud
        // providers yield a client.
        let cfg = SttConfig {
            provider: "none".into(),
            ..Default::default()
        };
        let router = mock_router("unused");
        let mcp: Arc<dyn McpToolCaller> = Arc::new(NoopMcpCaller);
        let client = build_stt_client(router.clone(), Some(mcp.clone()), &cfg).unwrap();
        assert!(client.is_none());

        let cfg = test_stt_cfg("mcp");
        let err = build_stt_client(router.clone(), Some(mcp.clone()), &cfg)
            .err()
            .expect("expected mcp without server to fail");
        assert!(err.to_string().contains("mcp_server"));

        let cfg = SttConfig {
            provider: "mcp".into(),
            mcp_server: Some("svc".into()),
            ..Default::default()
        };
        let err = build_stt_client(router.clone(), None, &cfg)
            .err()
            .expect("expected mcp without caller to fail");
        assert!(err.to_string().contains("MCP caller"));

        for provider in ["openai", "groq", "gemini", "deepgram", "assemblyai"] {
            let cfg = test_stt_cfg(provider);
            let client = build_stt_client(router.clone(), Some(mcp.clone()), &cfg).unwrap();
            assert!(client.is_some(), "provider {provider} should yield a client");
        }
    }

    #[test]
    fn build_stt_client_unknown_provider_errors() {
        let cfg = test_stt_cfg("not-a-provider");
        let err = build_stt_client(mock_router("unused"), None, &cfg)
            .err()
            .expect("expected unknown provider to fail");
        assert!(err.to_string().contains("unknown STT provider"));
    }

    #[test]
    fn openai_whisper_hosts_and_model_defaults() {
        // OpenAI provider defaults model to whisper-1; base_url override wins.
        let cfg = SttConfig {
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        let c = OpenAiWhisperClient::new(&cfg, cfg.base_url.clone(), Duration::from_secs(30));
        assert_eq!(c.base_url, "https://gateway.example/v1");
        assert_eq!(c.model, "whisper-1");

        let cfg_no_model = SttConfig {
            provider: "groq".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        let c = OpenAiWhisperClient::new(
            &cfg_no_model,
            "https://api.groq.com/openai/v1".into(),
            Duration::from_secs(30),
        );
        assert_eq!(c.model, "whisper-1");

        // An empty base URL falls back to the OpenAI host.
        let c_empty = OpenAiWhisperClient::new(&cfg_no_model, String::new(), Duration::from_secs(30));
        assert_eq!(c_empty.base_url, "https://api.openai.com/v1");
        assert_eq!(c_empty.model, "whisper-1");
    }

    #[test]
    fn deepgram_api_key_normalization() {
        // A bare key gains the `Token ` scheme prefix.
        let cfg = SttConfig {
            provider: "deepgram".into(),
            api_key: "dg-key".into(),
            ..Default::default()
        };
        let c = DeepgramSttClient::new(&cfg, Duration::from_secs(30));
        assert_eq!(c.api_key, "Token dg-key");

        // An already-prefixed key is passed through unchanged.
        let cfg_full = SttConfig {
            provider: "deepgram".into(),
            api_key: "Deepgram dg-key".into(),
            ..Default::default()
        };
        let c = DeepgramSttClient::new(&cfg_full, Duration::from_secs(30));
        assert_eq!(c.api_key, "dg-key");
        assert_eq!(c.model, "nova-3");
    }

    #[test]
    fn gemini_defaults_and_base_url() {
        // Default model + host; base_url override wins.
        let cfg = SttConfig {
            provider: "gemini".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        let c = GeminiSttClient::new(&cfg, Duration::from_secs(30));
        assert_eq!(c.base_url, "https://generativelanguage.googleapis.com/v1beta");
        assert_eq!(c.model, "gemini-2.5-flash");

        let cfg_override = SttConfig {
            provider: "gemini".into(),
            api_key: "k".into(),
            base_url: "https://gateway.example/v1beta".into(),
            model: "gemini-3.6-flash".into(),
            ..Default::default()
        };
        let c = GeminiSttClient::new(&cfg_override, Duration::from_secs(30));
        assert_eq!(c.base_url, "https://gateway.example/v1beta");
        assert_eq!(c.model, "gemini-3.6-flash");
    }

    #[test]
    fn assemblyai_base_url_defaults_and_auth() {
        let cfg = SttConfig {
            provider: "assemblyai".into(),
            api_key: "aa-key".into(),
            model: "universal-2".into(),
            ..Default::default()
        };
        let c = AssemblyAiSttClient::new(&cfg, Duration::from_secs(30));
        assert_eq!(c.base_url, "https://api.assemblyai.com");
        assert_eq!(c.model, "universal-2");
        let headers = c.auth();
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "aa-key",
            "AssemblyAI uses the raw key with no Bearer prefix"
        );

        let cfg_eu = SttConfig {
            provider: "assemblyai".into(),
            api_key: "k".into(),
            base_url: "https://api.eu.assemblyai.com".into(),
            ..Default::default()
        };
        let c = AssemblyAiSttClient::new(&cfg_eu, Duration::from_secs(30));
        assert_eq!(c.base_url, "https://api.eu.assemblyai.com");
    }
}
