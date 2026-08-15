//! Text-to-image generation capability.
//!
//! Unified dispatch entry point: [`build_image_gen_client`] maps an
//! `ImageGenConfig` provider id to a concrete client. Providers:
//! - `none`: no client
//! - `openai`: OpenAI `/v1/images/generations` (gpt-image-1 / dall-e-3)
//! - `gemini`: Google Gemini `generateContent` (image modality)
//!
//! Every client returns the generated image bytes (PNG/JPEG) plus its media
//! type; saving/display is the caller's job.

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use haven_common::config::ImageGenConfig;
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

/// Build the image generation client for a given config. Returns `None`
/// when the configured provider is `none`, and an error for an unknown
/// provider id.
pub fn build_image_gen_client(cfg: &ImageGenConfig) -> Result<Option<Box<dyn ImageGenClient>>> {
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let client: Box<dyn ImageGenClient> = match cfg.provider.as_str() {
        "none" => return Ok(None),
        "openai" => Box::new(OpenAiImageGenClient::new(cfg, timeout)),
        "gemini" => Box::new(GeminiImageGenClient::new(cfg, timeout)),
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
    use haven_common::config::ImageGenConfig;

    #[test]
    fn imagegen_default_cfg_dispatches_none() {
        assert!(
            build_image_gen_client(&ImageGenConfig::default())
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
        let err = build_image_gen_client(&cfg).err().expect("expected error");
        assert!(
            err.to_string()
                .contains("unknown image generation provider")
        );
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
                build_image_gen_client(&cfg).unwrap().is_some(),
                "provider {provider} should build"
            );
        }
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
