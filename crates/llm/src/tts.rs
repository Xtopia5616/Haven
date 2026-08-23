//! Text-to-speech (TTS) capability.
//!
//! Unified dispatch entry point: [`build_tts_client`] maps a `TtsConfig`
//! provider id to a concrete client. `provider` is either:
//! - `none` / empty: no client
//! - a name from `llm.providers`: credentials (base URL + API key) and the
//!   OpenAI-compatible vs ElevenLabs backend are taken from that provider
//! - legacy `openai` / `elevenlabs`: uses `TtsConfig.api_key` / `base_url`
//!
//! Every client returns raw audio bytes (MP3); decoding/playback is the
//! caller's job.

use anyhow::Result;
use async_trait::async_trait;
use haven_common::config::{ProviderConfig, TtsConfig, provider_config_wire_style};
use std::time::Duration;

/// Trait for text-to-speech synthesis. Implementations receive plain text
/// and return encoded audio bytes (typically MP3).
#[async_trait]
pub trait TtsClient: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>>;
}

/// Resolve TTS config against named LLM providers. Returns `None` when TTS
/// is disabled (`none` / empty). Rewrites a provider-name reference into a
/// concrete backend (`openai` / `elevenlabs`) with that provider's URL + key.
pub fn resolve_tts_config(
    cfg: &TtsConfig,
    providers: &[ProviderConfig],
) -> Result<Option<TtsConfig>> {
    let name = cfg.provider.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    // Named llm.providers win over legacy capability ids so a provider
    // named `openai` / `elevenlabs` reuses that entry's URL + key.
    if let Some(p) = providers.iter().find(|p| p.name == name) {
        let backend = tts_backend_for(p)?;
        return Ok(Some(TtsConfig {
            provider: backend.to_string(),
            api_key: p.api_key.clone(),
            base_url: p.base_url.clone(),
            model: cfg.model.clone(),
            voice: cfg.voice.clone(),
            timeout_secs: cfg.timeout_secs,
        }));
    }
    if name == "openai" || name == "elevenlabs" {
        return Ok(Some(cfg.clone()));
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
    pub fn new(cfg: &TtsConfig, timeout: Duration) -> Self {
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
        if self.api_key.is_empty() {
            anyhow::bail!("OpenAI TTS requires an api_key");
        }
        let payload = serde_json::json!({
            "model": self.model,
            "input": text,
            "voice": self.voice,
            "response_format": "mp3",
        });
        let resp = self
            .client
            .post(format!("{}/audio/speech", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI TTS request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(media_body_error("OpenAI TTS", status, &body));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI TTS response read failed: {e}"))?;
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
    pub fn new(cfg: &TtsConfig, timeout: Duration) -> Self {
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
        let resp = self
            .client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}",
                self.voice
            ))
            .header("xi-api-key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("ElevenLabs TTS request failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(media_body_error("ElevenLabs TTS", status, &body));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("ElevenLabs TTS response read failed: {e}"))?;
        Ok(bytes.to_vec())
    }
}

/// Error text extraction for media HTTP responses.
fn media_body_error(kind: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        anyhow::anyhow!("{kind} request failed: HTTP {}", status)
    } else {
        let snippet = if trimmed.len() > 300 {
            &trimmed[..300]
        } else {
            trimmed
        };
        anyhow::anyhow!("{kind} request failed (HTTP {}): {}", status, snippet)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ProviderConfig, TtsConfig};

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
    fn tts_dispatch_known_providers() {
        for provider in ["openai", "elevenlabs"] {
            let cfg = TtsConfig {
                provider: provider.into(),
                api_key: "k".into(),
                voice: "v".into(),
                ..Default::default()
            };
            assert!(
                build_tts_client(&cfg, &[]).unwrap().is_some(),
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
    fn tts_named_provider_openai_wins_over_legacy_id() {
        let providers = vec![ProviderConfig {
            name: "openai".into(),
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            api_key: "from-provider".into(),
            ..Default::default()
        }];
        let cfg = TtsConfig {
            provider: "openai".into(),
            api_key: "legacy".into(),
            base_url: "https://legacy.example/v1".into(),
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
        let cfg = TtsConfig {
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
        let cfg = TtsConfig {
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
        let cfg = TtsConfig {
            provider: "elevenlabs".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        let client = ElevenLabsTtsClient::new(&cfg, Duration::from_secs(10));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.synthesize("hi")).unwrap_err();
        assert!(err.to_string().contains("voice"));
    }

    #[test]
    fn elevenlabs_requires_key_at_call_time() {
        let cfg = TtsConfig {
            provider: "elevenlabs".into(),
            voice: "v".into(),
            ..Default::default()
        };
        let client = ElevenLabsTtsClient::new(&cfg, Duration::from_secs(10));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt.block_on(client.synthesize("hi")).unwrap_err();
        assert!(err.to_string().contains("api_key"));
    }
}
