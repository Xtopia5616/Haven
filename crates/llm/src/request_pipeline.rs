//! Provider-neutral request policy shared by every router request path.
//!
//! The router owns endpoint selection and health state; this module owns the
//! immutable retry/timeout policy snapshot and its execution semantics. Keeping
//! those concerns together prevents chat, tool, embedding, and streaming code
//! from unpacking the same configuration differently.

use std::future::Future;
use std::time::Duration;

use haven_common::config::RouterConfig;
use tokio_util::sync::CancellationToken;

use crate::client::with_retry;
use crate::types::LlmError;

/// Retry settings captured for one request or one streaming endpoint attempt.
///
/// A request must use one snapshot for all of its attempts. Settings changes
/// rebuild the router for new requests, while an in-flight request keeps the
/// policy it started with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RetryPolicy {
    pub(crate) max_retries: u32,
    pub(crate) base_secs: u64,
    pub(crate) factor: u32,
    pub(crate) max_secs: u64,
    pub(crate) jitter: f32,
}

/// Complete provider-neutral policy for one router request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RequestPolicy {
    pub(crate) retry: RetryPolicy,
    pub(crate) total_timeout_secs: u64,
}

impl RequestPolicy {
    /// Policy for the configured primary endpoint.
    pub(crate) fn primary(config: &RouterConfig) -> Self {
        Self {
            retry: RetryPolicy {
                max_retries: config.retry_max_retries,
                base_secs: config.retry_base_secs,
                factor: config.retry_factor,
                max_secs: config.retry_max_secs,
                jitter: config.retry_jitter,
            },
            total_timeout_secs: config.max_total_duration_secs,
        }
    }
}

/// Execute one provider operation using a captured retry policy.
pub(crate) async fn execute_with_retry<T, F, Fut>(
    policy: RetryPolicy,
    cancel: Option<&CancellationToken>,
    f: F,
) -> Result<T, LlmError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, LlmError>>,
{
    with_retry(
        policy.max_retries,
        policy.base_secs,
        policy.factor,
        policy.max_secs,
        policy.jitter,
        cancel,
        f,
    )
    .await
}

/// Bound the total duration of one logical request, including its retries.
pub(crate) async fn execute_with_timeout<T, F, Fut>(
    timeout_secs: u64,
    operation: &str,
    f: F,
) -> Result<T, LlmError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, LlmError>>,
{
    match tokio::time::timeout(Duration::from_secs(timeout_secs), f()).await {
        Ok(result) => result,
        Err(_) => Err(LlmError::Timeout(format!(
            "{operation} total timeout after {timeout_secs}s"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_snapshots_the_retry_budget() {
        let config = RouterConfig {
            max_total_duration_secs: 77,
            retry_max_retries: 2,
            retry_base_secs: 3,
            retry_factor: 2,
            retry_max_secs: 19,
            retry_jitter: 0.25,
            ..RouterConfig::default()
        };

        assert_eq!(
            RequestPolicy::primary(&config),
            RequestPolicy {
                retry: RetryPolicy {
                    max_retries: 2,
                    base_secs: 3,
                    factor: 2,
                    max_secs: 19,
                    jitter: 0.25,
                },
                total_timeout_secs: 77,
            }
        );
    }

    #[tokio::test]
    async fn timeout_reports_the_logical_operation_name() {
        let result = execute_with_timeout(0, "embedding", || async {
            std::future::pending::<Result<(), LlmError>>().await
        })
        .await;

        assert!(matches!(
            result,
            Err(LlmError::Timeout(message))
                if message == "embedding total timeout after 0s"
        ));
    }
}
