//! Text-to-image generation capability.
//!
//! Unified dispatch entry point: [`build_image_gen_client`] maps an
//! `ImageGenConfig` provider id to a concrete client. `provider` is either:
//! - `none` / empty: no client
//! - a name from `llm.providers`: credentials and OpenAI vs Gemini backend
//!   are taken from that provider
//! - legacy `openai` / `gemini`: uses `ImageGenConfig.api_key` / `base_url`
//!
//! Every client returns the generated image bytes (PNG/JPEG) plus its media
//! type; saving/display is the caller's job.

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use haven_common::config::{ImageGenConfig, ProviderConfig, provider_config_wire_style};
use std::time::Duration;

/// A generated image: raw bytes plus the media type the provider returned.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub media_type: String,
    pub data: Vec<u8>,
}

/// Trait for text-to-image generation.
#[async_trait]
pub trait ImageGenClient: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<GeneratedImage>;
}

/// Resolve image-gen config against named LLM providers. Returns `None` when
/// disabled. Rewrites a provider-name reference into `openai` / `gemini` with
/// that provider's URL + key.
pub fn resolve_image_gen_config(
    cfg: &ImageGenConfig,
    providers: &[ProviderConfig],
) -> Result<Option<ImageGenConfig>> {
    let name = cfg.provider.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    // Named llm.providers win over legacy capability ids so a provider
    // named `openai` / `gemini` reuses that entry's URL + key.
    if let Some(p) = providers.iter().find(|p| p.name == name) {
        let backend = image_gen_backend_for(p)?;
        let base_url = if backend == "gemini" {
            normalize_gemini_image_base(&p.base_url)
        } else {
            p.base_url.clone()
        };
        return Ok(Some(ImageGenConfig {
            provider: backend.to_string(),
            api_key: p.api_key.clone(),
            base_url,
            model: cfg.model.clone(),
            timeout_secs: cfg.timeout_secs,
        }));
    }
    if name == "openai" || name == "gemini" {
        return Ok(Some(cfg.clone()));
    }
    Err(anyhow::anyhow!(
        "image generation references unknown provider '{name}'"
    ))
}

fn image_gen_backend_for(p: &ProviderConfig) -> Result<&'static str> {
    use haven_common::config::is_openai_family_wire_style;
    let style = provider_config_wire_style(p);
    if style == "elevenlabs" || p.provider.eq_ignore_ascii_case("elevenlabs") {
        anyhow::bail!(
            "provider '{}' is TTS-only (ElevenLabs); use OpenAI-compatible or Gemini for image generation",
            p.name
        );
    }
    match style {
        "gemini" => Ok("gemini"),
        _ if is_openai_family_wire_style(style) => Ok("openai"),
        other => anyhow::bail!(
            "provider '{}' (api_style={other}) does not support image generation; use OpenAI-compatible or Gemini",
            p.name
        ),
    }
}

/// Strip a trailing `/v1beta` so Gemini image URLs stay
/// `{host}/v1beta/models/...` even when the LLM provider stores the chat
/// base URL that already includes `/v1beta`.
fn normalize_gemini_image_base(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .strip_suffix("/v1beta")
        .unwrap_or(trimmed)
        .to_string()
}

/// Build the image generation client, resolving named LLM providers when
/// `providers` is supplied. Returns `None` when disabled.
pub fn build_image_gen_client(
    cfg: &ImageGenConfig,
    providers: &[ProviderConfig],
) -> Result<Option<Box<dyn ImageGenClient>>> {
    let Some(resolved) = resolve_image_gen_config(cfg, providers)? else {
        return Ok(None);
    };
    let timeout = Duration::from_secs(resolved.timeout_secs);
    let client: Box<dyn ImageGenClient> = match resolved.provider.as_str() {
        "openai" => Box::new(OpenAiImageGenClient::new(&resolved, timeout)),
        "gemini" => Box::new(GeminiImageGenClient::new(&resolved, timeout)),
        other => anyhow::bail!("unknown image generation provider: {}", other),
    };
    Ok(Some(client))
}

fn imagegen_http_client(timeout: Duration) -> reqwest::Client {
    crate::client::http_client_builder()
        .timeout(timeout)
        .build()
        .unwrap_or_default()
}

fn imagegen_body_error(kind: &str, status: reqwest::StatusCode, body: &str) -> anyhow::Error {
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

/// Extract the generated image from an OpenAI `/v1/images/generations`
/// response. Handles both `b64_json` (default for gpt-image-1) and `url`
/// (dall-e-3 default) data items.
async fn openai_image_from_response(
    client: &reqwest::Client,
    body: &str,
) -> Result<GeneratedImage> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid OpenAI image response: {e}"))?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| anyhow::anyhow!("OpenAI image response missing 'data'"))?;
    if let Some(b64) = data.get("b64_json").and_then(|b| b.as_str()) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow::anyhow!("OpenAI image b64 decode failed: {e}"))?;
        return Ok(GeneratedImage {
            media_type: data
                .get("content_type")
                .and_then(|c| c.as_str())
                .unwrap_or("image/png")
                .to_string(),
            data: bytes,
        });
    }
    if let Some(url) = data.get("url").and_then(|u| u.as_str()) {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI image url fetch failed: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(imagegen_body_error("OpenAI image fetch", status, &body));
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI image fetch read failed: {e}"))?;
        return Ok(GeneratedImage {
            media_type: "image/png".into(),
            data: bytes.to_vec(),
        });
    }
    anyhow::bail!("OpenAI image response item has neither 'b64_json' nor 'url'")
}

/// Extract the generated image from a Gemini `generateContent` response.
/// The image arrives as an `inlineData` part (`mimeType` + base64 `data`).
fn gemini_image_from_response(body: &str) -> Result<GeneratedImage> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("invalid Gemini image response: {e}"))?;
    let parts = v
        .get("candidates")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array())
        .ok_or_else(|| anyhow::anyhow!("Gemini image response missing candidate parts"))?;
    for part in parts {
        let Some(inline) = part.get("inlineData") else {
            continue;
        };
        let Some(b64) = inline.get("data").and_then(|d| d.as_str()) else {
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| anyhow::anyhow!("Gemini image b64 decode failed: {e}"))?;
        return Ok(GeneratedImage {
            media_type: inline
                .get("mimeType")
                .and_then(|m| m.as_str())
                .unwrap_or("image/png")
                .to_string(),
            data: bytes,
        });
    }
    anyhow::bail!("Gemini response contained no inline image data")
}

/// OpenAI `/v1/images/generations` client.
pub struct OpenAiImageGenClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiImageGenClient {
    pub fn new(cfg: &ImageGenConfig, timeout: Duration) -> Self {
        Self {
            client: imagegen_http_client(timeout),
            base_url: if cfg.base_url.trim().is_empty() {
                "https://api.openai.com/v1".to_string()
            } else {
                cfg.base_url.trim_end_matches('/').to_string()
            },
            api_key: cfg.api_key.clone(),
            model: if cfg.model.is_empty() {
                "gpt-image-1".to_string()
            } else {
                cfg.model.clone()
            },
        }
    }
}

#[async_trait]
impl ImageGenClient for OpenAiImageGenClient {
    async fn generate(&self, prompt: &str) -> Result<GeneratedImage> {
        if self.api_key.is_empty() {
            anyhow::bail!("OpenAI image generation requires an api_key");
        }
        // `response_format` is deliberately omitted: gpt-image-1 rejects it
        // and always returns b64_json, while dall-e-3 defaults to a URL
        // (handled by `openai_image_from_response`).
        let payload = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "n": 1,
            "size": "1024x1024",
        });
        let resp = self
            .client
            .post(format!("{}/images/generations", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("OpenAI image request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        if !status.is_success() {
            return Err(imagegen_body_error("OpenAI image", status, &body));
        }
        openai_image_from_response(&self.client, &body).await
    }
}

/// Google Gemini `generateContent` image-generation client. Requests the
/// image modality via `responseModalities` and reads the `inlineData` part
/// from the response.
pub struct GeminiImageGenClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl GeminiImageGenClient {
    pub fn new(cfg: &ImageGenConfig, timeout: Duration) -> Self {
        Self {
            client: imagegen_http_client(timeout),
            base_url: if cfg.base_url.trim().is_empty() {
                "https://generativelanguage.googleapis.com".to_string()
            } else {
                cfg.base_url.trim_end_matches('/').to_string()
            },
            api_key: cfg.api_key.clone(),
            model: if cfg.model.is_empty() {
                "gemini-2.5-flash-image".to_string()
            } else {
                cfg.model.clone()
            },
        }
    }
}

#[async_trait]
impl ImageGenClient for GeminiImageGenClient {
    async fn generate(&self, prompt: &str) -> Result<GeneratedImage> {
        if self.api_key.is_empty() {
            anyhow::bail!("Gemini image generation requires an api_key");
        }
        let url = format!(
            "{}/v1beta/models/{}:generateContent?key={}",
            self.base_url, self.model, self.api_key
        );
        let payload = serde_json::json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {"responseModalities": ["IMAGE", "TEXT"]}
        });
        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Gemini image request failed: {e}"))?;
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_default()
            .split_whitespace()
            .collect::<String>();
        if !status.is_success() {
            return Err(imagegen_body_error("Gemini image", status, &body));
        }
        gemini_image_from_response(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ImageGenConfig, ProviderConfig};

    #[test]
    fn imagegen_default_cfg_dispatches_none() {
        assert!(
            build_image_gen_client(&ImageGenConfig::default(), &[])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn imagegen_unknown_provider_errors() {
        let cfg = ImageGenConfig {
            provider: "nope".into(),
            ..Default::default()
        };
        let err = build_image_gen_client(&cfg, &[])
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn imagegen_dispatch_known_providers() {
        for provider in ["openai", "gemini"] {
            let cfg = ImageGenConfig {
                provider: provider.into(),
                api_key: "k".into(),
                ..Default::default()
            };
            assert!(
                build_image_gen_client(&cfg, &[]).unwrap().is_some(),
                "provider {provider} should build"
            );
        }
    }

    #[test]
    fn imagegen_resolves_named_openai_provider() {
        let providers = vec![ProviderConfig {
            name: "oai".into(),
            provider: "openai".into(),
            base_url: "https://gateway.example/v1".into(),
            api_key: "secret".into(),
            ..Default::default()
        }];
        let cfg = ImageGenConfig {
            provider: "oai".into(),
            model: "gpt-image-1".into(),
            ..Default::default()
        };
        let resolved = resolve_image_gen_config(&cfg, &providers)
            .unwrap()
            .expect("resolved");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.api_key, "secret");
        assert_eq!(resolved.base_url, "https://gateway.example/v1");
    }

    #[test]
    fn imagegen_strips_v1beta_from_gemini_provider_url() {
        let providers = vec![ProviderConfig {
            name: "g".into(),
            provider: "gemini".into(),
            api_style: Some("gemini".into()),
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            api_key: "k".into(),
            ..Default::default()
        }];
        let cfg = ImageGenConfig {
            provider: "g".into(),
            ..Default::default()
        };
        let resolved = resolve_image_gen_config(&cfg, &providers)
            .unwrap()
            .expect("resolved");
        assert_eq!(resolved.provider, "gemini");
        assert_eq!(
            resolved.base_url,
            "https://generativelanguage.googleapis.com"
        );
    }

    #[test]
    fn openai_imagegen_defaults() {
        let cfg = ImageGenConfig {
            provider: "openai".into(),
            ..Default::default()
        };
        let client = OpenAiImageGenClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.model, "gpt-image-1");
        assert_eq!(client.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn gemini_imagegen_defaults() {
        let cfg = ImageGenConfig {
            provider: "gemini".into(),
            ..Default::default()
        };
        let client = GeminiImageGenClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.model, "gemini-2.5-flash-image");
        assert_eq!(client.base_url, "https://generativelanguage.googleapis.com");
    }

    #[test]
    fn openai_image_response_b64_json() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::new();
        let body = format!(
            r#"{{"data": [{{"b64_json": "{}", "content_type": "image/jpeg"}}]}}"#,
            base64::engine::general_purpose::STANDARD.encode(b"fake-image")
        );
        let img = rt
            .block_on(openai_image_from_response(&client, &body))
            .unwrap();
        assert_eq!(img.media_type, "image/jpeg");
        assert_eq!(img.data, b"fake-image");
    }

    #[test]
    fn openai_image_response_missing_data_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let client = reqwest::Client::new();
        let err = rt
            .block_on(openai_image_from_response(&client, r#"{"error": "x"}"#))
            .unwrap_err();
        assert!(err.to_string().contains("missing 'data'"));
    }

    #[test]
    fn gemini_image_response_inline_data() {
        let body = format!(
            r#"{{
                "candidates": [{{
                    "content": {{
                        "parts": [
                            {{"text": "here you go"}},
                            {{"inlineData": {{"mimeType": "image/png", "data": "{}"}}}}
                        ]
                    }}
                }}]
            }}"#,
            base64::engine::general_purpose::STANDARD.encode(b"png-bytes")
        );
        let img = gemini_image_from_response(&body).unwrap();
        assert_eq!(img.media_type, "image/png");
        assert_eq!(img.data, b"png-bytes");
    }

    #[test]
    fn gemini_image_response_no_image_errors() {
        let body = r#"{"candidates": [{"content": {"parts": [{"text": "sorry"}]}}]}"#;
        let err = gemini_image_from_response(body).unwrap_err();
        assert!(err.to_string().contains("no inline image"));
    }
}
