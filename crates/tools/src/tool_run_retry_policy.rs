//! ToolRun-specific classification over the shared pure recovery policy.
//!
//! ToolRunService still owns terminal arbitration, persistence, sleeping, and
//! shutdown. This adapter only translates ToolRun outcomes into shared signals.

use std::time::Duration;

use haven_common::retry::{BackoffPolicy, RecoveryPolicy, RecoverySignal, RetryJitter};
pub(crate) use haven_common::retry::{
    RecoveryDecision as RetryDecision, RecoveryStopReason as RetryStopReason,
};
use tokio::time::Instant;

const TERMINAL_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const TERMINAL_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const INLINE_STORE_RETRY_ATTEMPTS: u32 = 3;
const INLINE_STORE_RETRY_DELAY: Duration = Duration::from_millis(50);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ToolRunPersistenceRetryPolicy {
    inner: RecoveryPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ToolRunStoreRetryPolicy {
    inner: RecoveryPolicy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetrySignal {
    /// ToolRunStore persistence errors retain the existing retry-all behavior.
    Failure { retryable: bool },
    /// The retry worker itself was cancelled by service shutdown.
    Cancelled,
    /// A competing terminal transition won or the durable row is no longer live.
    Terminal,
    /// The durable terminal transition committed.
    Succeeded,
}

impl ToolRunPersistenceRetryPolicy {
    /// The existing terminal repair workers retry indefinitely, with a capped
    /// exponential delay and no ToolRun-level deadline.
    pub(crate) const fn terminal_persistence() -> Self {
        Self {
            inner: RecoveryPolicy::new(
                None,
                None,
                BackoffPolicy::new(TERMINAL_RETRY_INITIAL_DELAY, 2, TERMINAL_RETRY_MAX_DELAY)
                    .with_jitter(RetryJitter::None),
            ),
        }
    }

    /// `completed_attempts` counts policy-level persistence calls. A scheduled
    /// call may itself include three existing short store attempts; those
    /// remain outside this counter.
    pub(crate) fn decide(
        self,
        completed_attempts: u32,
        signal: RetrySignal,
        now: Instant,
    ) -> RetryDecision {
        self.inner
            .decide(completed_attempts, signal.into(), now.into(), 0)
    }

    /// Recheck immediately before a scheduled retry begins so a caller that
    /// later supplies a deadline cannot start a retry after it has elapsed.
    pub(crate) fn can_start_attempt(
        self,
        attempt: u32,
        now: Instant,
    ) -> Result<(), RetryStopReason> {
        self.inner.can_start_attempt(attempt, now.into())
    }
}

impl ToolRunStoreRetryPolicy {
    /// Short inline retries used by ToolRun store transitions. The durable
    /// recovery worker, when present, remains owned by ToolRunService.
    pub(crate) const fn inline_store() -> Self {
        Self {
            inner: RecoveryPolicy::new(
                Some(INLINE_STORE_RETRY_ATTEMPTS),
                None,
                BackoffPolicy::new(INLINE_STORE_RETRY_DELAY, 1, INLINE_STORE_RETRY_DELAY),
            ),
        }
    }

    pub(crate) const fn max_attempts(self) -> u32 {
        INLINE_STORE_RETRY_ATTEMPTS
    }

    pub(crate) fn decide(self, completed_attempts: u32, retryable: bool) -> RetryDecision {
        let signal = if retryable {
            RecoverySignal::Retryable { retry_after: None }
        } else {
            RecoverySignal::PermanentFailure
        };
        self.inner
            .decide(completed_attempts, signal, std::time::Instant::now(), 0)
    }
}

impl From<RetrySignal> for RecoverySignal {
    fn from(signal: RetrySignal) -> Self {
        match signal {
            RetrySignal::Failure { retryable: true } => Self::Retryable { retry_after: None },
            RetrySignal::Failure { retryable: false } => Self::PermanentFailure,
            RetrySignal::Cancelled => Self::Cancelled,
            RetrySignal::Terminal => Self::Terminal,
            RetrySignal::Succeeded => Self::Succeeded,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(
        deadline: Option<Instant>,
        max_attempts: Option<u32>,
    ) -> ToolRunPersistenceRetryPolicy {
        ToolRunPersistenceRetryPolicy {
            inner: RecoveryPolicy::new(
                max_attempts,
                deadline.map(Into::into),
                BackoffPolicy::new(TERMINAL_RETRY_INITIAL_DELAY, 2, TERMINAL_RETRY_MAX_DELAY),
            ),
        }
    }

    #[test]
    fn terminal_persistence_has_no_deadline_or_retry_budget() {
        let policy = ToolRunPersistenceRetryPolicy::terminal_persistence();
        assert_eq!(
            policy.decide(1, RetrySignal::Failure { retryable: true }, Instant::now()),
            RetryDecision::Retry {
                next_attempt: 2,
                delay: Duration::from_secs(1),
            }
        );
    }

    #[test]
    fn elapsed_deadline_stops_retry_at_the_deadline_boundary() {
        let now = Instant::now();
        let policy = policy(Some(now), None);
        assert_eq!(
            policy.decide(1, RetrySignal::Failure { retryable: true }, now),
            RetryDecision::Stop {
                reason: RetryStopReason::RetryDeadlineElapsed,
            }
        );
    }

    #[test]
    fn non_retryable_failure_and_exhausted_budget_stop() {
        let now = Instant::now();
        assert_eq!(
            policy(None, None).decide(1, RetrySignal::Failure { retryable: false }, now),
            RetryDecision::Stop {
                reason: RetryStopReason::NonRetryableFailure,
            }
        );
        assert_eq!(
            policy(None, Some(3)).decide(3, RetrySignal::Failure { retryable: true }, now),
            RetryDecision::Stop {
                reason: RetryStopReason::AttemptBudgetExhausted,
            }
        );
    }

    #[test]
    fn cancellation_terminal_and_success_win_over_retry_or_deadline() {
        let now = Instant::now();
        let policy = policy(Some(now), Some(0));
        for (signal, reason) in [
            (RetrySignal::Cancelled, RetryStopReason::Cancelled),
            (RetrySignal::Terminal, RetryStopReason::Terminal),
            (RetrySignal::Succeeded, RetryStopReason::Succeeded),
        ] {
            assert_eq!(
                policy.decide(0, signal, now),
                RetryDecision::Stop { reason }
            );
        }
    }

    #[test]
    fn retry_attempt_and_backoff_are_exponential_then_capped() {
        let policy = ToolRunPersistenceRetryPolicy::terminal_persistence();
        let now = Instant::now();
        for (completed_attempts, next_attempt, delay) in [
            (1, 2, 1),
            (2, 3, 2),
            (3, 4, 4),
            (5, 6, 16),
            (6, 7, 30),
            (20, 21, 30),
        ] {
            assert_eq!(
                policy.decide(
                    completed_attempts,
                    RetrySignal::Failure { retryable: true },
                    now
                ),
                RetryDecision::Retry {
                    next_attempt,
                    delay: Duration::from_secs(delay),
                }
            );
        }
    }

    #[test]
    fn inline_store_policy_uses_three_total_attempts_and_fixed_delay() {
        let policy = ToolRunStoreRetryPolicy::inline_store();
        assert_eq!(policy.max_attempts(), 3);
        assert_eq!(
            policy.decide(1, true),
            RetryDecision::Retry {
                next_attempt: 2,
                delay: Duration::from_millis(50),
            }
        );
        assert_eq!(
            policy.decide(2, true),
            RetryDecision::Retry {
                next_attempt: 3,
                delay: Duration::from_millis(50),
            }
        );
        assert_eq!(
            policy.decide(3, true),
            RetryDecision::Stop {
                reason: RetryStopReason::AttemptBudgetExhausted,
            }
        );
        assert_eq!(
            policy.decide(1, false),
            RetryDecision::Stop {
                reason: RetryStopReason::NonRetryableFailure,
            }
        );
    }
}
