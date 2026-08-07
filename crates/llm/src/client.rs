use async_trait::async_trait;
use futures_util::Stream;
use std::pin::Pin;
use std::time::Duration;

use crate::types::{Embedding, LlmError, LlmMessage, LlmResponse, StreamChunk, ToolDefinition};

/// Unified interface implemented by every provider adapter. Adapters convert
/// the provider's native wire protocol to/from the provider-neutral
/// `LlmMessage` / `LlmResponse` / `StreamChunk` types (see `adapters/`).
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Human-readable wire protocol style of this adapter (e.g.
    /// "openai-chat", "anthropic", "gemini"), used for logging and dispatch
    /// assertions. Defaults to "unknown"; adapters override it.
    fn style(&self) -> &'static str {
        "unknown"
    }

    async fn chat(&self, messages: Vec<LlmMessage>) -> Result<LlmResponse, LlmError>;

    async fn chat_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        _tools: Vec<ToolDefinition>,
    ) -> Result<LlmResponse, LlmError> {
        self.chat(messages).await
    }

    async fn chat_stream(
        &self,
        messages: Vec<LlmMessage>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError>;

    async fn chat_stream_with_tools(
        &self,
        messages: Vec<LlmMessage>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, LlmError>> + Send>>, LlmError> {
        let resp = self.chat_with_tools(messages, tools).await?;
        let chunk = StreamChunk {
            text: Some(resp.text.clone()),
            tool_calls: resp.tool_calls.clone(),
            finish_reason: resp.finish_reason,
            usage: Some(resp.usage.clone()),
            model: resp.model.clone(),
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
        };
        let final_chunk = StreamChunk {
            text: None,
            tool_calls: Vec::new(),
            finish_reason: None,
            usage: None,
            model: None,
            reasoning: None,
            web_search: None,
            web_search_calls: Vec::new(),
        };
        Ok(Box::pin(futures_util::stream::iter(vec![
            Ok(chunk),
            Ok(final_chunk),
        ])))
    }

    /// Embed a batch of texts into vectors via the provider's embeddings API.
    /// Only adapters that speak an embeddings wire protocol implement this;
    /// the default reports an unsupported error so chat-only endpoints
    /// (anthropic, gemini) degrade gracefully when routed to this slot.
    async fn embed(&self, _input: Vec<String>) -> Result<Embedding, LlmError> {
        Err(LlmError::RequestFailed(
            "embeddings not supported by this adapter".into(),
        ))
    }

    async fn health_check(&self) -> Result<(), LlmError>;
}

// ---------------------------------------------------------------------------
// HTTP status code → LlmError mapping (§2.2)
// ---------------------------------------------------------------------------

/// Try to extract a human-readable error message from various provider error
/// JSON schemas: `{"error":{"message":"..."}}`, `{"message":"..."}`,
/// `{"detail":"..."}`, or fall back to the raw body.
fn extract_error_body(body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        // OpenAI: {"error": {"message": "...", "code": "..."}}
        if let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return msg.to_string();
        }
        // Generic: {"message": "..."}
        if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        // Generic: {"detail": "..."}
        if let Some(msg) = val.get("detail").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        // Generic: {"error": "..."} (flat)
        if let Some(msg) = val.get("error").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        // Fallback: pretty-print the first 500 chars of JSON
        let s = serde_json::to_string(&val).unwrap_or_default();
        if s.len() <= 500 {
            return s;
        }
        return s[..500].to_string();
    }
    body.to_string()
}

/// Shared HTTP status → `LlmError` mapping used by all adapters.
pub(crate) fn http_status_to_error(
    status: reqwest::StatusCode,
    body: &str,
    retry_after: Option<Duration>,
) -> LlmError {
    let err_body = extract_error_body(body);
    match status.as_u16() {
        401 | 403 => LlmError::Auth(format!("{}: {}", status, err_body)),
        429 => LlmError::RateLimit { retry_after },
        400 => {
            if err_body.contains("context_length")
                || err_body.contains("maximum context")
                || err_body.contains("context length")
            {
                LlmError::ContextLengthExceeded
            } else if err_body.contains("content_filter") {
                LlmError::ContentFilter
            } else if err_body.contains("billing")
                || err_body.contains("insufficient_quota")
                || err_body.contains("quota")
            {
                LlmError::Billing(err_body)
            } else {
                LlmError::RequestFailed(format!("{}: {}", status, err_body))
            }
        }
        s if s >= 500 => LlmError::ServerError(format!("{}: {}", status, err_body)),
        _ => LlmError::RequestFailed(format!("{}: {}", status, err_body)),
    }
}

// ---------------------------------------------------------------------------
// Retry wrapper (§2.3, §5.1)
// ---------------------------------------------------------------------------

pub async fn with_retry<F, Fut, T>(
    max_retries: u32,
    base_secs: u64,
    factor: u32,
    max_secs: u64,
    jitter: f32,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    mut f: F,
) -> Result<T, LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, LlmError>>,
{
    let mut last_err = None;
    for attempt in 0..=max_retries {
        if cancel.is_some_and(|c| c.is_cancelled()) {
            return Err(LlmError::Cancelled);
        }
        tracing::debug!("llm attempt {}/{}", attempt + 1, max_retries + 1);
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = e.is_retryable();
                if !retryable || attempt == max_retries {
                    return Err(e);
                }
                // §2.3: take max(fixed_backoff, Retry-After)
                let backoff = (base_secs * (factor.pow(attempt) as u64)).min(max_secs);
                let retry_after = e.retry_after().map(|d| d.as_secs()).unwrap_or(0);
                let delay = backoff.max(retry_after);
                // §5.1: jitter
                let jitter_ms = (delay as f32 * jitter * 1000.0) as u64;
                let actual_delay = Duration::from_secs(delay) + Duration::from_millis(jitter_ms);
                tracing::debug!(
                    "llm retry {} after {:?} (error: {})",
                    attempt,
                    actual_delay,
                    e
                );
                tokio::time::sleep(actual_delay).await;
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| LlmError::Unknown("exhausted retries".into())))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FinishReason, Usage};

    #[test]
    fn http_status_maps_correctly() {
        let r = http_status_to_error(reqwest::StatusCode::TOO_MANY_REQUESTS, "rate limited", None);
        assert!(matches!(r, LlmError::RateLimit { .. }));

        let r = http_status_to_error(reqwest::StatusCode::UNAUTHORIZED, "bad key", None);
        assert!(matches!(r, LlmError::Auth(_)));

        let r = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "context_length_exceeded",
            None,
        );
        assert!(matches!(r, LlmError::ContextLengthExceeded));

        let r = http_status_to_error(reqwest::StatusCode::BAD_REQUEST, "content_filter", None);
        assert!(matches!(r, LlmError::ContentFilter));

        let r = http_status_to_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            "server error",
            None,
        );
        assert!(matches!(r, LlmError::ServerError(_)));
    }

    #[test]
    fn retry_after_extracted() {
        let e = LlmError::RateLimit {
            retry_after: Some(Duration::from_secs(10)),
        };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(10)));
        assert!(e.is_retryable());

        let e = LlmError::StreamTruncated;
        assert!(e.is_retryable());

        let e = LlmError::Auth("bad key".into());
        assert!(!e.is_retryable());
    }

    #[test]
    fn http_status_429_retains_retry_after() {
        let e = http_status_to_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "rate limited",
            Some(Duration::from_secs(30)),
        );
        assert!(matches!(e, LlmError::RateLimit { .. }));
        assert_eq!(e.retry_after(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn http_status_402_returns_request_failed() {
        let e = http_status_to_error(reqwest::StatusCode::PAYMENT_REQUIRED, "billing issue", None);
        assert!(matches!(e, LlmError::RequestFailed(_)));
        assert!(e.to_string().contains("402"));
    }

    #[test]
    fn http_status_403_returns_auth() {
        let e = http_status_to_error(reqwest::StatusCode::FORBIDDEN, "forbidden", None);
        assert!(matches!(e, LlmError::Auth(_)));
        assert!(e.to_string().contains("403"));
    }

    #[test]
    fn http_status_422_returns_request_failed() {
        let e = http_status_to_error(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "body too large",
            None,
        );
        assert!(matches!(e, LlmError::RequestFailed(_)));
        assert!(e.to_string().contains("422"));
    }

    #[test]
    fn http_status_500_returns_server_error() {
        let e = http_status_to_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom", None);
        assert!(matches!(e, LlmError::ServerError(_)));
    }

    #[test]
    fn http_status_503_returns_server_error() {
        let e = http_status_to_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "down", None);
        assert!(matches!(e, LlmError::ServerError(_)));
    }

    #[test]
    fn http_status_400_with_insufficient_quota_returns_billing() {
        let e = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "insufficient_quota for model gpt-4",
            None,
        );
        assert!(matches!(e, LlmError::Billing(_)));
    }

    #[test]
    fn http_status_400_with_context_length_returns_context_error() {
        let e = http_status_to_error(
            reqwest::StatusCode::BAD_REQUEST,
            "maximum context exceeded",
            None,
        );
        assert!(matches!(e, LlmError::ContextLengthExceeded));
    }

    #[tokio::test]
    async fn with_retry_success_first_attempt() {
        let result = with_retry(3, 1, 2, 30, 0.0, None, || async {
            Ok(LlmResponse {
                text: "ok".into(),
                tool_calls: vec![],
                finish_reason: Some(FinishReason::Stop),
                usage: Usage::default(),
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            })
        })
        .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn with_retry_non_retryable_skips_without_delay() {
        let result = with_retry(3, 1, 2, 30, 0.0, None, || async {
            Err::<LlmResponse, LlmError>(LlmError::Auth("bad key".into()))
        })
        .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LlmError::Auth(_)));
    }

    #[tokio::test]
    async fn with_retry_retryable_recovers() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let attempts = AtomicU32::new(0);
        let result = with_retry(3, 1, 2, 30, 0.0, None, || {
            let a = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if a < 2 {
                    Err(LlmError::Timeout("timeout".into()))
                } else {
                    Ok(LlmResponse {
                        text: "recovered".into(),
                        tool_calls: vec![],
                        finish_reason: Some(FinishReason::Stop),
                        usage: Usage::default(),
                        model: None,
                        reasoning: None,
                        web_search_calls: Vec::new(),
                    })
                }
            }
        })
        .await;
        assert!(result.is_ok());
        assert!(attempts.load(Ordering::SeqCst) >= 3);
    }

    #[tokio::test]
    async fn with_retry_max_retries_exhausted() {
        let result = with_retry(2, 1, 2, 5, 0.0, None, || async {
            Err::<LlmResponse, LlmError>(LlmError::Timeout("persistent timeout".into()))
        })
        .await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), LlmError::Timeout(_)));
    }

    #[test]
    fn retry_backoff_formula() {
        let base = 2u64;
        let factor = 2u32;
        let max = 30u64;
        // attempt 0: 2 * (2^0) = 2, min max → 2
        let backoff_0 = (base * (factor.pow(0) as u64)).min(max);
        assert_eq!(backoff_0, 2);
        // attempt 1: 2 * (2^1) = 4
        let backoff_1 = (base * (factor.pow(1) as u64)).min(max);
        assert_eq!(backoff_1, 4);
        // attempt 2: 2 * (2^2) = 8
        let backoff_2 = (base * (factor.pow(2) as u64)).min(max);
        assert_eq!(backoff_2, 8);
        // attempt 3: 2 * (2^3) = 16
        let backoff_3 = (base * (factor.pow(3) as u64)).min(max);
        assert_eq!(backoff_3, 16);
        // attempt 4: 2 * (2^4) = 32, capped at 30
        let backoff_4 = (base * (factor.pow(4) as u64)).min(max);
        assert_eq!(backoff_4, 30);
    }

    #[test]
    fn retry_jitter_calculation() {
        let delay = Duration::from_secs(5);
        let jitter = 0.2f32;
        let jitter_ms = (delay.as_secs() as f32 * jitter * 1000.0) as u64;
        assert_eq!(jitter_ms, 1000);
    }
}
