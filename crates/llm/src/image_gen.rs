//! Text-to-image generation capability.
//!
//! Unified dispatch entry point: [`build_image_gen_client`] maps an
//! `ImageGenConfig` provider connection name to a concrete client.
//! `provider_name` is either:
//! - `none` / empty: no client
//! - a name from `llm.providers`: credentials and OpenAI vs Gemini backend
//!   are taken from that provider
//!
//! Every client returns the generated image bytes (PNG/JPEG) plus its media
//! type; saving/display is the caller's job.

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use futures_util::TryStreamExt;
use haven_common::config::{ImageGenConfig, ProviderConfig, provider_config_wire_style};
use haven_common::network::{self, MAX_HTTP_REDIRECT_HOPS, ResolvedHttpDestination};
use std::time::Duration;

/// Bound the provider response before JSON parsing or base64 decoding. The
/// decoded generated-media cap is enforced separately by the media tool.
pub const MAX_IMAGE_GEN_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// A generated image: raw bytes plus the media type the provider returned.
#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub media_type: String,
    pub data: Vec<u8>,
}

/// Runtime-only image generation configuration resolved from a named
/// `llm.providers` entry. Credentials and endpoint details intentionally do
/// not live in the persisted `ImageGenConfig`.
#[derive(Debug, Clone, Default)]
struct ResolvedImageGenConfig {
    provider: String,
    api_key: String,
    model: String,
    base_url: String,
    timeout_secs: u64,
}

/// Trait for text-to-image generation.
#[async_trait]
pub trait ImageGenClient: Send + Sync {
    async fn generate(&self, prompt: &str) -> Result<GeneratedImage>;
}

/// Resolve image-gen config against named LLM providers. Returns `None` when
/// disabled. Rewrites a provider connection name into `openai` / `gemini` with
/// that provider's URL + key.
fn resolve_image_gen_config(
    cfg: &ImageGenConfig,
    providers: &[ProviderConfig],
) -> Result<Option<ResolvedImageGenConfig>> {
    let name = cfg.provider_name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    if let Some(p) = providers.iter().find(|p| p.name == name) {
        let backend = image_gen_backend_for(p)?;
        let base_url = if backend == "gemini" {
            normalize_gemini_image_base(&p.base_url)
        } else {
            p.base_url.clone()
        };
        return Ok(Some(ResolvedImageGenConfig {
            provider: backend.to_string(),
            api_key: p.api_key.clone(),
            base_url,
            model: cfg.model.clone(),
            timeout_secs: cfg.timeout_secs,
        }));
    }
    Err(anyhow::anyhow!(
        "image generation references unknown provider '{name}'"
    ))
}

fn image_gen_backend_for(p: &ProviderConfig) -> Result<&'static str> {
    use haven_common::config::{is_openai_family_wire_style, provider_media_capabilities};
    let style = provider_config_wire_style(p);
    if style == "elevenlabs" || p.provider.eq_ignore_ascii_case("elevenlabs") {
        anyhow::bail!(
            "provider '{}' is TTS-only (ElevenLabs); use OpenAI-compatible or Gemini for image generation",
            p.name
        );
    }
    match style {
        "gemini" if provider_media_capabilities(p).image_gen => Ok("gemini"),
        _ if is_openai_family_wire_style(style) && provider_media_capabilities(p).image_gen => {
            Ok("openai")
        }
        other => anyhow::bail!(
            "provider '{}' (api_style={other}) does not declare an image-generation endpoint; enable it only if its service implements the configured route",
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
    let trimmed = haven_common::error::sanitize_error_text(body);
    if trimmed.is_empty() {
        anyhow::anyhow!("{kind} request failed: HTTP {}", status)
    } else {
        anyhow::anyhow!("{kind} request failed (HTTP {}): {}", status, trimmed)
    }
}

async fn response_bytes_bounded(resp: reqwest::Response, operation: &str) -> Result<Vec<u8>> {
    if resp
        .content_length()
        .is_some_and(|length| length > MAX_IMAGE_GEN_RESPONSE_BYTES as u64)
    {
        anyhow::bail!(
            "{operation} response exceeds the {} byte limit",
            MAX_IMAGE_GEN_RESPONSE_BYTES
        );
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream
        .try_next()
        .await
        .map_err(|e| anyhow::anyhow!("{operation} response read failed: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_IMAGE_GEN_RESPONSE_BYTES {
            anyhow::bail!(
                "{operation} response exceeds the {} byte limit",
                MAX_IMAGE_GEN_RESPONSE_BYTES
            );
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn response_text_bounded(resp: reqwest::Response, operation: &str) -> Result<String> {
    let body = response_bytes_bounded(resp, operation).await?;
    String::from_utf8(body)
        .map_err(|e| anyhow::anyhow!("{operation} response was not valid UTF-8: {e}"))
}

/// Extract the generated image from an OpenAI `/v1/images/generations`
/// response. Handles both `b64_json` (default for gpt-image-1) and `url`
/// (dall-e-3 default) data items.
async fn openai_image_from_response(body: &str, timeout: Duration) -> Result<GeneratedImage> {
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
        let resp = fetch_public_image_url(url, timeout).await?;
        let status = resp.status();
        if !status.is_success() {
            let body = response_text_bounded(resp, "OpenAI image error response").await?;
            return Err(imagegen_body_error("OpenAI image fetch", status, &body));
        }
        let media_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .filter(|value| value.starts_with("image/"))
            .unwrap_or("image/png")
            .to_string();
        let bytes = response_bytes_bounded(resp, "OpenAI image fetch").await?;
        return Ok(GeneratedImage {
            media_type,
            data: bytes,
        });
    }
    anyhow::bail!("OpenAI image response item has neither 'b64_json' nor 'url'")
}

async fn fetch_public_image_url(url: &str, timeout: Duration) -> Result<reqwest::Response> {
    fetch_public_image_url_inner(url, timeout, false).await
}

async fn fetch_public_image_url_inner(
    raw_url: &str,
    timeout: Duration,
    allow_loopback_for_test: bool,
) -> Result<reqwest::Response> {
    let mut current = reqwest::Url::parse(raw_url)
        .map_err(|error| anyhow::anyhow!("invalid OpenAI image URL: {error}"))?;
    let mut redirect_hops = 0usize;
    loop {
        let destination =
            resolve_image_fetch_destination(current.as_str(), allow_loopback_for_test).await?;
        let mut builder = reqwest::Client::builder()
            .timeout(timeout)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none());
        if let Some(address) = destination.pinned_address
            && let Some(host) = destination.url.host_str()
        {
            builder = builder.resolve(host, address);
        }
        let client = builder.build()?;
        let response = client
            .get(destination.url.clone())
            .send()
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "OpenAI image URL fetch failed: {}",
                    haven_common::error::sanitize_error_text(&error.to_string())
                )
            })?;
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .filter(|_| response.status().is_redirection());
        let Some(location) = location else {
            return Ok(response);
        };
        redirect_hops += 1;
        if redirect_hops > MAX_HTTP_REDIRECT_HOPS {
            anyhow::bail!("OpenAI image URL redirect limit exceeded");
        }
        current = destination.url.join(&location)?;
        // Each loop validates and pins the new destination before connecting.
        // The fetch is unauthenticated and cookie-free, so public CDN redirects
        // can safely cross origins.
        drop(response);
    }
}

async fn resolve_image_fetch_destination(
    raw_url: &str,
    allow_loopback_for_test: bool,
) -> Result<ResolvedHttpDestination> {
    #[cfg(test)]
    if allow_loopback_for_test {
        return Ok(network::resolve_http_destination_for_test(raw_url, &[]).await?);
    }
    #[cfg(not(test))]
    let _ = allow_loopback_for_test;
    network::resolve_http_destination(raw_url, &[]).await
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
struct OpenAiImageGenClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    image_fetch_timeout: Duration,
}

impl OpenAiImageGenClient {
    fn new(cfg: &ResolvedImageGenConfig, timeout: Duration) -> Self {
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
            image_fetch_timeout: timeout,
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
            .map_err(|e| {
                anyhow::anyhow!(
                    "OpenAI image request failed: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                )
            })?;
        let status = resp.status();
        let body = response_text_bounded(resp, "OpenAI image response").await?;
        if !status.is_success() {
            return Err(imagegen_body_error("OpenAI image", status, &body));
        }
        openai_image_from_response(&body, self.image_fetch_timeout).await
    }
}

/// Google Gemini `generateContent` image-generation client. Requests the
/// image modality via `responseModalities` and reads the `inlineData` part
/// from the response.
struct GeminiImageGenClient {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl GeminiImageGenClient {
    fn new(cfg: &ResolvedImageGenConfig, timeout: Duration) -> Self {
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
            "{}/v1beta/models/{}:generateContent",
            self.base_url, self.model
        );
        let payload = serde_json::json!({
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {"responseModalities": ["IMAGE", "TEXT"]}
        });
        let resp = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Gemini image request failed: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                )
            })?;
        let status = resp.status();
        let body = response_text_bounded(resp, "Gemini image response").await?;
        if !status.is_success() {
            return Err(imagegen_body_error("Gemini image", status, &body));
        }
        gemini_image_from_response(&body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ImageGenConfig, ProviderConfig, ProviderMediaCapabilities};

    fn resolved_image_gen(provider: &str) -> ResolvedImageGenConfig {
        ResolvedImageGenConfig {
            provider: provider.into(),
            ..Default::default()
        }
    }

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
            provider_name: "nope".into(),
            ..Default::default()
        };
        let err = build_image_gen_client(&cfg, &[])
            .err()
            .expect("expected error");
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn imagegen_rejects_unconfigured_provider_name() {
        let cfg = ImageGenConfig {
            provider_name: "openai".into(),
            ..Default::default()
        };
        let err = resolve_image_gen_config(&cfg, &[]).unwrap_err();
        assert!(err.to_string().contains("unknown provider"));
    }

    #[test]
    fn imagegen_fails_closed_for_generic_openai_compatible_provider() {
        let provider = ProviderConfig {
            name: "custom".into(),
            provider: "openai-compatible".into(),
            api_style: Some("openai-chat".into()),
            base_url: "https://gateway.example/v1".into(),
            api_key: "k".into(),
            ..Default::default()
        };
        assert!(image_gen_backend_for(&provider).is_err());
    }

    #[test]
    fn imagegen_dispatch_known_providers() {
        for provider in ["openai", "gemini"] {
            let cfg = ImageGenConfig {
                provider_name: provider.into(),
                ..Default::default()
            };
            let providers = vec![ProviderConfig {
                name: provider.into(),
                provider: provider.into(),
                api_style: Some(
                    if provider == "gemini" {
                        "gemini"
                    } else {
                        "openai-chat"
                    }
                    .into(),
                ),
                base_url: if provider == "gemini" {
                    "https://generativelanguage.googleapis.com/v1beta".into()
                } else {
                    "https://api.openai.com/v1".into()
                },
                api_key: "k".into(),
                media_capabilities: ProviderMediaCapabilities {
                    image_gen: true,
                    ..Default::default()
                },
                ..Default::default()
            }];
            assert!(
                build_image_gen_client(&cfg, &providers).unwrap().is_some(),
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
            media_capabilities: ProviderMediaCapabilities {
                image_gen: true,
                ..Default::default()
            },
            ..Default::default()
        }];
        let cfg = ImageGenConfig {
            provider_name: "oai".into(),
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
            media_capabilities: ProviderMediaCapabilities {
                image_gen: true,
                ..Default::default()
            },
            ..Default::default()
        }];
        let cfg = ImageGenConfig {
            provider_name: "g".into(),
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
        let cfg = resolved_image_gen("openai");
        let client = OpenAiImageGenClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.model, "gpt-image-1");
        assert_eq!(client.base_url, "https://api.openai.com/v1");
    }

    #[test]
    fn gemini_imagegen_defaults() {
        let cfg = resolved_image_gen("gemini");
        let client = GeminiImageGenClient::new(&cfg, Duration::from_secs(10));
        assert_eq!(client.model, "gemini-2.5-flash-image");
        assert_eq!(client.base_url, "https://generativelanguage.googleapis.com");
    }

    #[test]
    fn openai_image_response_b64_json() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let body = format!(
            r#"{{"data": [{{"b64_json": "{}", "content_type": "image/jpeg"}}]}}"#,
            base64::engine::general_purpose::STANDARD.encode(b"fake-image")
        );
        let img = rt
            .block_on(openai_image_from_response(&body, Duration::from_secs(5)))
            .unwrap();
        assert_eq!(img.media_type, "image/jpeg");
        assert_eq!(img.data, b"fake-image");
    }

    #[test]
    fn openai_image_response_missing_data_errors() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(openai_image_from_response(
                r#"{"error": "x"}"#,
                Duration::from_secs(5),
            ))
            .unwrap_err();
        assert!(err.to_string().contains("missing 'data'"));
    }

    #[tokio::test]
    async fn openai_image_url_redirect_rejects_metadata_destination() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                }
                socket
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
            })
            .await
            .expect("HTTP fixture did not receive the initial request");
        });

        let result = fetch_public_image_url_inner(
            &format!("http://{address}/image"),
            Duration::from_secs(5),
            true,
        )
        .await;
        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(error.to_string().contains("blocked"), "{error:#}");
        server.await.unwrap();
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
