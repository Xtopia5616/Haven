//! Shared HTTP transport for provider adapters.
//!
//! Provider modules own URL and payload mapping. This module owns the common
//! reqwest client, authentication/header policy, response-status conversion,
//! streaming header bound, and health-check behavior.

use std::time::Duration;

use haven_common::config::ModelEndpoint;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};

use crate::client::http_status_to_error;
use crate::types::LlmError;

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
    if super::is_openrouter(endpoint) {
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
/// configures no `timeout_streaming_secs`.
pub(crate) const STREAM_HEADER_TIMEOUT_SECS: u64 = 60;

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
