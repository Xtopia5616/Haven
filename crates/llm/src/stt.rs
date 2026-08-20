//! Speech-to-text (STT) capability, unified with the chat/vision adapters.
//!
//! Transcription implementations live on [`LlmClient::transcribe`] (OpenAI
//! Whisper, Gemini, Deepgram, AssemblyAI). [`build_stt_client`] is the
//! consumer-facing factory (the STT counterpart of `adapters::adapter_for`):
//! - `none`: no client
//! - `mcp`: route through an MCP server exposing `stt.transcribe`
//! - `llm`: transcribe via the `audio_model` / default LLM endpoint (native
//!   `transcribe` first, multimodal chat fallback)
//! - `openai` / `groq` / `gemini` / `deepgram` / `assemblyai`: synthesize a
//!   [`ModelEndpoint`] from [`SttConfig`] and dispatch through `adapter_for`

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use haven_common::config::{ModelEndpoint, SttConfig};
use haven_common::prompts::STT_SYSTEM_PROMPT;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::LlmRouter;
use crate::adapters::adapter_for;
use crate::client::LlmClient;
use crate::types::{LlmError, SttResult};
use haven_common::types::{CanonicalMessage, CanonicalRole, ContentPart};

/// Trait for speech-to-text conversion.
/// Implementations receive WAV bytes and return the transcript.
#[async_trait]
pub trait SttClient: Send + Sync {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult>;
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
/// (required only for the `mcp` provider). Dedicated cloud providers are
/// dispatched through the same [`adapter_for`] path as chat endpoints.
pub fn build_stt_client(
    router: Arc<LlmRouter>,
    mcp: Option<Arc<dyn McpToolCaller>>,
    cfg: &SttConfig,
) -> Result<Option<Box<dyn SttClient>>> {
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
        "openai" | "groq" | "gemini" | "deepgram" | "assemblyai" => {
            let endpoint = endpoint_from_stt_config(cfg);
            Box::new(LlmClientSttBridge {
                client: adapter_for(&endpoint),
            })
        }
        other => anyhow::bail!("unknown STT provider: {}", other),
    };
    Ok(Some(client))
}

/// Map `[media.stt]` into a [`ModelEndpoint`] so dedicated STT providers share
/// the same adapter dispatch as chat/vision roles.
pub fn endpoint_from_stt_config(cfg: &SttConfig) -> ModelEndpoint {
    let (provider, api_style, default_base, default_model) = match cfg.provider.as_str() {
        "groq" => (
            "groq",
            Some("openai-chat".into()),
            "https://api.groq.com/openai/v1",
            "whisper-large-v3-turbo",
        ),
        "gemini" => (
            "gemini",
            Some("gemini".into()),
            "https://generativelanguage.googleapis.com/v1beta",
            "gemini-2.5-flash",
        ),
        "deepgram" => ("deepgram", Some("deepgram".into()), "https://api.deepgram.com", "nova-3"),
        "assemblyai" => (
            "assemblyai",
            Some("assemblyai".into()),
            "https://api.assemblyai.com",
            "assemblyai_default",
        ),
        // openai + anything else Whisper-compatible
        _ => (
            "openai",
            Some("openai-chat".into()),
            "https://api.openai.com/v1",
            "whisper-1",
        ),
    };
    let base_url = if cfg.base_url.trim().is_empty() {
        default_base.to_string()
    } else {
        cfg.base_url.trim_end_matches('/').to_string()
    };
    let model_name = if cfg.model.trim().is_empty() {
        default_model.to_string()
    } else {
        cfg.model.clone()
    };
    ModelEndpoint {
        provider: provider.into(),
        api_style,
        base_url,
        api_key: cfg.api_key.clone(),
        model_name,
        timeout_secs: cfg.timeout_secs,
        ..Default::default()
    }
}

/// Thin bridge: dedicated STT providers built via [`adapter_for`] still expose
/// the consumer-facing [`SttClient`] trait used by input / MediaGateway.
struct LlmClientSttBridge {
    client: Box<dyn LlmClient>,
}

#[async_trait]
impl SttClient for LlmClientSttBridge {
    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult> {
        Ok(self.client.transcribe(wav_data).await?)
    }
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
    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult> {
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
        let confidence = result.output["confidence"].as_f64().map(|c| c as f32);
        Ok(SttResult { text, confidence })
    }
}

/// STT adapter that transcribes audio through the router's STT role
/// (`audio_model` / default). Prefers native [`LlmClient::transcribe`]; falls
/// back to multimodal chat with an `input_audio` content part when the
/// endpoint adapter does not implement STT (e.g. gpt-4o-audio-preview).
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
    async fn transcribe(&self, wav_data: &[u8]) -> Result<SttResult> {
        Ok(self.router.transcribe_audio(wav_data).await?)
    }
}

/// Chat + `input_audio` fallback used when the selected role's adapter does
/// not implement native STT. Shared by [`LlmRouter::transcribe_audio`].
pub(crate) async fn transcribe_via_chat(
    router: &LlmRouter,
    role: crate::EndpointRole,
    wav_data: &[u8],
) -> Result<SttResult, LlmError> {
    let data = base64::engine::general_purpose::STANDARD.encode(wav_data);
    let messages = vec![
        CanonicalMessage::system(vec![ContentPart::text(STT_SYSTEM_PROMPT)]),
        CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::Audio {
                content_type: "input_audio".into(),
                media_type: "audio/wav".into(),
                data,
            }],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
        },
    ];

    let resp = match router
        .chat_stream_with_tools_aggregated(role, &messages, &[], |_| {})
        .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("input_audio")
                || msg.contains("Only text and image_url are supported")
            {
                return Err(LlmError::RequestFailed(format!(
                    "audio_model endpoint does not support audio input ({msg}). \
                     Configure audio_model to a model that accepts audio \
                     (e.g. openai/gpt-4o-audio-preview), or set STT Provider \
                     to an MCP server that exposes `stt.transcribe`."
                )));
            }
            return Err(e);
        }
    };
    Ok(SttResult {
        text: resp.text.trim().to_string(),
        confidence: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures_util::Stream;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::EndpointRole;
    use crate::client::LlmClient;
    use crate::types::{LlmError, LlmResponse, StreamChunk};

    struct MockSttClient {
        response: String,
        call_count: AtomicU64,
    }

    #[async_trait]
    impl SttClient for MockSttClient {
        async fn transcribe(&self, _wav_data: &[u8]) -> Result<SttResult> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(SttResult {
                text: self.response.clone(),
                confidence: None,
            })
        }
    }

    #[tokio::test]
    async fn test_mock_stt_returns_text() {
        let client = MockSttClient {
            response: "hello world".into(),
            call_count: AtomicU64::new(0),
        };
        let wav = vec![0u8; 44];
        let result = client.transcribe(&wav).await.unwrap();
        assert_eq!(result.text, "hello world");
        assert!(result.confidence.is_none());
        assert_eq!(client.call_count.load(Ordering::SeqCst), 1);
    }

    struct MockLlm {
        text: String,
        calls: AtomicU64,
    }

    #[async_trait]
    impl LlmClient for MockLlm {
        async fn chat(&self, messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
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
            _messages: Vec<CanonicalMessage>,
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
        let result = adapter.transcribe(&[0u8; 44]).await.unwrap();
        assert_eq!(result.text, "你好世界");
        assert!(result.confidence.is_none());
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_unconfigured_errors() {
        let router = mock_router("ignored");
        let adapter = LlmSttAdapter::new(router);
        let err = adapter.transcribe(&[0u8; 44]).await.unwrap_err();
        assert!(err.to_string().contains("audio_model"));
    }

    #[tokio::test]
    async fn test_llm_stt_adapter_uses_default_model_when_routing_disabled() {
        let router = mock_router("走默认模型的转写");
        router.force_routing_flags(false, true).await;
        let adapter = LlmSttAdapter::new(router);
        let result = adapter.transcribe(&[0u8; 44]).await.unwrap();
        assert_eq!(result.text, "走默认模型的转写");
    }

    struct MockLlmErr {
        err: LlmError,
    }

    #[async_trait]
    impl LlmClient for MockLlmErr {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(self.err.clone())
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
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
        let err = adapter
            .transcribe(&[0u8; 44])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("does not support audio input"),
            "expected rewritten hint, got: {err}"
        );
        assert!(
            err.contains("gpt-4o-audio-preview"),
            "expected model suggestion, got: {err}"
        );
    }

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
            assert!(
                client.is_some(),
                "provider {provider} should yield a client"
            );
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
    fn endpoint_from_stt_config_defaults() {
        let cfg = SttConfig {
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        let ep = endpoint_from_stt_config(&cfg);
        assert_eq!(ep.base_url, "https://gateway.example/v1");
        assert_eq!(ep.model_name, "whisper-1");
        assert_eq!(ep.api_style.as_deref(), Some("openai-chat"));

        let groq = endpoint_from_stt_config(&SttConfig {
            provider: "groq".into(),
            api_key: "k".into(),
            ..Default::default()
        });
        assert_eq!(groq.base_url, "https://api.groq.com/openai/v1");
        assert!(groq.model_name.contains("whisper"));

        let gemini = endpoint_from_stt_config(&SttConfig {
            provider: "gemini".into(),
            api_key: "k".into(),
            ..Default::default()
        });
        assert_eq!(
            gemini.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(gemini.model_name, "gemini-2.5-flash");
        assert_eq!(gemini.api_style.as_deref(), Some("gemini"));

        let deepgram = endpoint_from_stt_config(&SttConfig {
            provider: "deepgram".into(),
            api_key: "dg".into(),
            ..Default::default()
        });
        assert_eq!(deepgram.api_style.as_deref(), Some("deepgram"));
        assert_eq!(deepgram.model_name, "nova-3");

        let assembly = endpoint_from_stt_config(&SttConfig {
            provider: "assemblyai".into(),
            api_key: "aa".into(),
            model: "universal-2".into(),
            base_url: "https://api.eu.assemblyai.com".into(),
            ..Default::default()
        });
        assert_eq!(assembly.base_url, "https://api.eu.assemblyai.com");
        assert_eq!(assembly.model_name, "universal-2");
        assert_eq!(assembly.api_style.as_deref(), Some("assemblyai"));
    }
}
