//! Text-to-speech (TTS) capability.
//!
//! Unified dispatch entry point: [`build_tts_client`] maps a `TtsConfig`
//! provider id to a concrete client. Providers:
//! - `none`: no client
//! - `openai`: OpenAI `/v1/audio/speech` (tts-1 / tts-1-hd / gpt-4o-mini-tts)
//! - `elevenlabs`: ElevenLabs `/v1/text-to-speech/{voice_id}`
//!
//! Every client returns raw audio bytes (MP3); decoding/playback is the
//! caller's job.

use anyhow::Result;
use async_trait::async_trait;
use haven_common::config::TtsConfig;
use std::time::Duration;

/// Trait for text-to-speech synthesis. Implementations receive plain text
/// and return encoded audio bytes (typically MP3).
#[async_trait]
pub trait TtsClient: Send + Sync {
    async fn synthesize(&self, text: &str) -> Result<Vec<u8>>;
}

/// Build the TTS client for a given config. Returns `None` when the
/// configured provider is `none`, and an error for an unknown provider id.
pub fn build_tts_client(cfg: &TtsConfig) -> Result<Option<Box<dyn TtsClient>>> {
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let client: Box<dyn TtsClient> = match cfg.provider.as_str() {
        "none" => return Ok(None),
        "openai" => Box::new(OpenAiTtsClient::new(cfg, timeout)),
        "elevenlabs" => Box::new(ElevenLabsTtsClient::new(cfg, timeout)),
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
    use haven_common::config::TtsConfig;

    #[test]
    fn tts_default_cfg_dispatches_none() {
        assert!(build_tts_client(&TtsConfig::default()).unwrap().is_none());
    }

    #[test]
    fn tts_unknown_provider_errors() {
        let cfg = TtsConfig {
            provider: "nope".into(),
            ..Default::default()
        };
        let err = build_tts_client(&cfg).err().expect("expected error");
        assert!(err.to_string().contains("unknown TTS provider"));
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
                build_tts_client(&cfg).unwrap().is_some(),
                "provider {provider} should build"
            );
        }
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
