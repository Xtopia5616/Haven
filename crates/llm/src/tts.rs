//! Text-to-speech (TTS) capability.
//!
//! Unified dispatch entry point: [`build_tts_client`] maps a `TtsConfig`
//! provider id to a concrete client. `provider` is either:
//! - `none` / empty: no client
//! - a name from `llm.providers`: credentials (base URL + API key) and the
//!   OpenAI-compatible vs ElevenLabs backend are taken from that provider
//!
//! Clients expose both the provider's encoded output and a PCM/WAV path for
//! the local speaker. The latter keeps playback inside the app boundary: the
//! Windows audio adapter only needs to understand RIFF/WAVE and does not
//! need a bundled media decoder.

use anyhow::Result;
use async_trait::async_trait;
use haven_common::config::{ProviderConfig, TtsConfig, provider_config_wire_style};
use std::time::Duration;

/// Trait for text-to-speech synthesis. Implementations receive plain text
/// and return encoded audio bytes (typically MP3).
#[async_trait]
pub trait TtsClient: Send + Sync {
    /// Synthesize encoded audio for an attachment or other media consumer.
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>>;

    /// Synthesize a PCM/WAV payload suitable for local playback.
    ///
    /// Providers that only implement the encoded path get a safe error rather
    /// than sending compressed bytes to the Windows `PlaySoundW` adapter.
    async fn synthesize_wav(&self, text: &str) -> Result<Vec<u8>> {
        let bytes = self.synthesize(text).await?;
        if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE") {
            Ok(bytes)
        } else {
            anyhow::bail!("TTS provider did not return WAV audio for local playback")
        }
    }
}

/// Runtime-only TTS configuration resolved from a named `llm.providers`
/// entry. Credentials and endpoint details intentionally do not live in the
/// persisted `TtsConfig`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedTtsConfig {
    pub provider: String,
    pub api_key: String,
    pub model: String,
    pub voice: String,
    pub base_url: String,
    pub timeout_secs: u64,
}

/// Resolve TTS config against named LLM providers. Returns `None` when TTS
/// is disabled (`none` / empty). Rewrites a provider-name reference into a
/// concrete backend (`openai` / `elevenlabs`) with that provider's URL + key.
pub fn resolve_tts_config(
    cfg: &TtsConfig,
    providers: &[ProviderConfig],
) -> Result<Option<ResolvedTtsConfig>> {
    let name = cfg.provider.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if let Some(p) = providers.iter().find(|p| p.name == name) {
        let backend = tts_backend_for(p)?;
        return Ok(Some(ResolvedTtsConfig {
            provider: backend.to_string(),
            api_key: p.api_key.clone(),
            base_url: p.base_url.clone(),
            model: cfg.model.clone(),
            voice: cfg.voice.clone(),
            timeout_secs: cfg.timeout_secs,
        }));
    }
    Err(anyhow::anyhow!("TTS references unknown provider '{name}'"))
}

fn tts_backend_for(p: &ProviderConfig) -> Result<&'static str> {
    use haven_common::config::is_openai_family_wire_style;
    let style = provider_config_wire_style(p);
    if style == "elevenlabs" || p.provider.eq_ignore_ascii_case("elevenlabs") {
        return Ok("elevenlabs");
    }
    if is_openai_family_wire_style(style) {
        return Ok("openai");
    }
    anyhow::bail!(
        "provider '{}' (api_style={style}) does not support TTS; use OpenAI-compatible or ElevenLabs",
        p.name
    )
}

/// Build the TTS client for a given config, resolving named LLM providers
/// when `providers` is supplied. Returns `None` when disabled.
pub fn build_tts_client(
    cfg: &TtsConfig,
    providers: &[ProviderConfig],
) -> Result<Option<Box<dyn TtsClient>>> {
    let Some(resolved) = resolve_tts_config(cfg, providers)? else {
        return Ok(None);
    };
    let timeout = Duration::from_secs(resolved.timeout_secs);
    let client: Box<dyn TtsClient> = match resolved.provider.as_str() {
        "openai" => Box::new(OpenAiTtsClient::new(&resolved, timeout)),
        "elevenlabs" => Box::new(ElevenLabsTtsClient::new(&resolved, timeout)),
        other => anyhow::bail!("unknown TTS provider: {}", other),
    };
    Ok(Some(client))
}

fn tts_http_client(timeout: Duration) -> reqwest::Client {
    crate::client::http_client_builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default()
}

/// OpenAI `/v1/audio/speech` client. `base_url` defaults to the OpenAI host
/// so self-hosted OpenAI-compatible gateways can be used.
pub struct OpenAiTtsClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    voice: String,
}

impl OpenAiTtsClient {
    pub fn new(cfg: &ResolvedTtsConfig, timeout: Duration) -> Self {
        Self {
            client: tts_http_client(timeout),
            base_url: if cfg.base_url.trim().is_empty() {
                "https://api.openai.com/v1".to_string()
            } else {
                cfg.base_url.trim_end_matches('/').to_string()
            },
            api_key: cfg.api_key.clone(),
            model: if cfg.model.is_empty() {
                "tts-1".to_string()
            } else {
                cfg.model.clone()
            },
            voice: if cfg.voice.is_empty() {
                "alloy".to_string()
            } else {
                cfg.voice.clone()
            },
        }
    }
}

#[async_trait]
impl TtsClient for OpenAiTtsClient {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        self.request_audio(text, "mp3").await
    }

    async fn synthesize_wav(&self, text: &str) -> Result<Vec<u8>> {
        self.request_audio(text, "wav").await
    }
}

impl OpenAiTtsClient {
    async fn request_audio(&self, text: &str, response_format: &str) -> Result<Vec<u8>> {
        if self.api_key.is_empty() {
            anyhow::bail!("OpenAI TTS requires an api_key");
        }
        let payload = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": self.voice,
            "response_format": response_format,
        });
        let resp = self
            .client
            .post(format!("{}/audio/speech", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "OpenAI TTS request failed: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                )
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.map_err(|e| {
                anyhow::anyhow!(
                    "OpenAI TTS error response read failed: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                )
            })?;
            return Err(media_body_error("OpenAI TTS", status, &body));
        }
        let bytes = resp.bytes().await.map_err(|e| {
            anyhow::anyhow!(
                "OpenAI TTS response read failed: {}",
                haven_common::error::sanitize_error_text(&e.to_string())
            )
        })?;
        Ok(bytes.to_vec())
    }
}

/// ElevenLabs `/v1/text-to-speech/{voice_id}` client.
pub struct ElevenLabsTtsClient {
    client: reqwest::Client,
    api_key: String,
    model: Option<String>,
    voice: String,
}

impl ElevenLabsTtsClient {
    pub fn new(cfg: &ResolvedTtsConfig, timeout: Duration) -> Self {
        Self {
            client: tts_http_client(timeout),
            api_key: cfg.api_key.clone(),
            model: if cfg.model.is_empty() {
                None
            } else {
                Some(cfg.model.clone())
            },
            voice: cfg.voice.clone(),
        }
    }
}

#[async_trait]
impl TtsClient for ElevenLabsTtsClient {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        self.request_audio(text, None).await
    }

    async fn synthesize_wav(&self, text: &str) -> Result<Vec<u8>> {
        // ElevenLabs exposes raw signed 16-bit PCM as a stable low-level
        // format. Wrap it in a standard WAV container for WinMM playback.
        let pcm = self.request_audio(text, Some("pcm_16000")).await?;
        Ok(pcm_to_wav(&pcm, 16_000, 1))
    }
}

impl ElevenLabsTtsClient {
    async fn request_audio(&self, text: &str, output_format: Option<&str>) -> Result<Vec<u8>> {
        if self.api_key.is_empty() {
            anyhow::bail!("ElevenLabs TTS requires an api_key");
        }
        if self.voice.is_empty() {
            anyhow::bail!("ElevenLabs TTS requires a voice id");
        }
        let mut payload = serde_json::json!({ "text": text });
        if let Some(model) = &self.model {
            payload["model_id"] = serde_json::Value::String(model.clone());
        }
        let mut url = format!("https://api.elevenlabs.io/v1/text-to-speech/{}", self.voice);
        if let Some(format) = output_format {
            url.push_str("?output_format=");
            url.push_str(format);
        }
        let resp = self
            .client
            .post(url)
            .header("xi-api-key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "ElevenLabs TTS request failed: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                )
            })?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.map_err(|e| {
                anyhow::anyhow!(
                    "ElevenLabs TTS error response read failed: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                )
            })?;
            return Err(media_body_error("ElevenLabs TTS", status, &body));
        }
        let bytes = resp.bytes().await.map_err(|e| {
            anyhow::anyhow!(
                "ElevenLabs TTS response read failed: {}",
                haven_common::error::sanitize_error_text(&e.to_string())
            )
        })?;
        Ok(bytes.to_vec())
    }
}

/// Wrap signed little-endian 16-bit PCM in a canonical RIFF/WAVE container.
/// This is intentionally small and deterministic because provider output is
/// already decoded PCM; no general-purpose media decoder belongs in the
/// tool crate.
fn pcm_to_wav(pcm: &[u8], sample_rate: u32, channels: u16) -> Vec<u8> {
    let block_align = channels.saturating_mul(2);
    let byte_rate = sample_rate.saturating_mul(block_align as u32);
    let data_len = pcm.len().min(u32::MAX as usize) as u32;
    let riff_len = 36u32.saturating_add(data_len);
    let mut wav = Vec::with_capacity(44usize.saturating_add(data_len as usize));
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm[..data_len as usize]);
    wav
}

/// Error text extraction for media HTTP responses.
fn media_body_error(kind: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let trimmed = haven_common::error::sanitize_error_text(body);
    if trimmed.is_empty() {
        anyhow::anyhow!("{kind} request failed: HTTP {}", status)
    } else {
        anyhow::anyhow!("{kind} request failed (HTTP {}): {}", status, trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ProviderConfig, TtsConfig};

    fn resolved_tts(provider: &str, api_key: &str) -> ResolvedTtsConfig {
        ResolvedTtsConfig {
            provider: provider.into(),
            api_key: api_key.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tts_default_cfg_dispatches_none() {
        assert!(
            build_tts_client(&TtsConfig::default(), &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn tts_unknown_provider_errors() {
        let cfg = TtsConfig {
            provider: "nope".into(),
            ..Default::default()
        };
        let err = build_tts_client(&cfg, &[]).err().expect("expected error");
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn tts_rejects_unconfigured_provider_name() {
        let cfg = TtsConfig {
            provider: "openai".into(),
            ..Default::default()
        };
        let err = resolve_tts_config(&cfg, &[]).unwrap_err();
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn tts_dispatch_known_providers() {
        for provider in ["openai", "elevenlabs"] {
            let cfg = TtsConfig {
                provider: provider.into(),
                voice: "v".into(),
                ..Default::default()
            };
            let providers = vec![ProviderConfig {
                name: provider.into(),
                provider: provider.into(),
                api_key: "k".into(),
                ..Default::default()
            }];
            assert!(
                build_tts_client(&cfg, &providers).unwrap().is_some(),
                "provider {provider} should build"
            );
        }
    }

    #[test]
    fn tts_resolves_named_llm_provider() {
        let providers = vec![ProviderConfig {
            name: "my-openai".into(),
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            api_key: "secret".into(),
            ..Default::default()
        }];
        let cfg = TtsConfig {
            provider: "my-openai".into(),
            model: "tts-1-hd".into(),
            voice: "nova".into(),
            ..Default::default()
        };
        let resolved = resolve_tts_config(&cfg, &providers)
            .unwrap()
            .expect("resolved");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.api_key, "secret");
        assert_eq!(resolved.base_url, "https://gateway.example/v1");
        assert_eq!(resolved.model, "tts-1-hd");
        assert_eq!(resolved.voice, "nova");
        assert!(build_tts_client(&cfg, &providers).unwrap().is_some());
    }

    #[test]
    fn tts_provider_named_openai_uses_named_credentials() {
        let providers = vec![ProviderConfig {
            name: "openai".into(),
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            api_key: "from-provider".into(),
            ..Default::default()
        }];
        let cfg = TtsConfig {
            provider: "openai".into(),
            ..Default::default()
        };
        let resolved = resolve_tts_config(&cfg, &providers)
            .unwrap()
            .expect("resolved");
        assert_eq!(resolved.api_key, "from-provider");
        assert_eq!(resolved.base_url, "https://gateway.example/v1");
    }

    #[test]
    fn tts_rejects_unsupported_provider_style() {
        let providers = vec![ProviderConfig {
            name: "claude".into(),
            provider: "anthropic".into(),
            api_style: Some("anthropic".into()),
            base_url: "https://api.anthropic.com".into(),
            api_key: "k".into(),
            ..Default::default()
        }];
        let cfg = TtsConfig {
            provider: "claude".into(),
            ..Default::default()
        };
        let err = resolve_tts_config(&cfg, &providers)
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("does not support TTS"));
    }

    #[test]
    fn openai_tts_defaults_model_voice_and_base_url() {
        let cfg = ResolvedTtsConfig {
            provider: "openai".into(),
            ..Default::default()
        };
        let client = OpenAiTtsClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.model, "tts-1");
        assert_eq!(client.voice, "alloy");
        assert_eq!(client.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn openai_tts_honors_custom_voice_model_base_url() {
        let cfg = ResolvedTtsConfig {
            provider: "openai".into(),
            model: "gpt-4o-mini-tts".into(),
            voice: "nova".into(),
            base_url: "https://gateway.example/v1/".into(),
            ..Default::default()
        };
        let client = OpenAiTtsClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.model, "gpt-4o-mini-tts");
        assert_eq!(client.voice, "nova");
        assert_eq!(client.base_url, "https://gateway.example/v1");
    }

    #[test]
    fn elevenlabs_requires_voice_at_call_time() {
        let cfg = resolved_tts("elevenlabs", "k");
        let client = ElevenLabsTtsClient::new(&cfg, Duration::from_secs(10));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.synthesize("hi")).unwrap_err();
        assert!(err.to_string().contains("voice"));
    }

    #[test]
    fn elevenlabs_requires_key_at_call_time() {
        let mut cfg = resolved_tts("elevenlabs", "");
        cfg.voice = "v".into();
        let client = ElevenLabsTtsClient::new(&cfg, Duration::from_secs(10));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.synthesize("hi")).unwrap_err();
        assert!(err.to_string().contains("api_key"));
    }

    #[test]
    fn pcm_to_wav_writes_a_playable_header() {
        let wav = pcm_to_wav(&[0, 0, 255, 127], 16_000, 1);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &[0, 0, 255, 127]);
    }
}
