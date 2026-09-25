//! Pure retry decisions for action terminal persistence repair.
//!
//! This policy does not execute a job, access ActionStore, or own a timer. The
//! caller supplies the current monotonic instant and remains responsible for
//! sleeping, cancellation, persistence, and terminal arbitration.

use std::time::Duration;
use tokio::time::Instant;

const TERMINAL_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(1);
const TERMINAL_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ActionPersistenceRetryPolicy {
    /// `None` preserves the current unbounded repair behavior. No action-level
    /// execution deadline is currently configured by background or scheduled
    /// actions.
    retry_deadline: Option<Instant>,
    /// Maximum number of policy-level attempts, including the initial attempt.
    /// `None` preserves the current unbounded repair behavior.
    max_attempts: Option<u32>,
    initial_delay: Duration,
    max_delay: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetrySignal {
    /// The caller classifies the persistence error; current ActionStore errors
    /// retain the existing retry-all behavior.
    Failure { retryable: bool },
    /// The retry worker itself was cancelled by service shutdown.
    Cancelled,
    /// A competing terminal transition won or the durable row is no longer live.
    Terminal,
    /// The durable terminal transition committed.
    Succeeded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryStopReason {
    NonRetryableFailure,
    Cancelled,
    Terminal,
    Succeeded,
    RetryDeadlineElapsed,
    AttemptBudgetExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryDecision {
    Retry { next_attempt: u32, delay: Duration },
    Stop { reason: RetryStopReason },
}

impl ActionPersistenceRetryPolicy {
    /// The existing terminal repair workers retry indefinitely, with a capped
    /// exponential delay and no action-level deadline.
    pub(crate) const fn terminal_persistence() -> Self {
        Self {
            retry_deadline: None,
            max_attempts: None,
            initial_delay: TERMINAL_RETRY_INITIAL_DELAY,
            max_delay: TERMINAL_RETRY_MAX_DELAY,
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
        let stop_reason = match signal {
            RetrySignal::Failure { retryable: false } => Some(RetryStopReason::NonRetryableFailure),
            RetrySignal::Cancelled => Some(RetryStopReason::Cancelled),
            RetrySignal::Terminal => Some(RetryStopReason::Terminal),
            RetrySignal::Succeeded => Some(RetryStopReason::Succeeded),
            RetrySignal::Failure { retryable: true } => None,
        };
        if let Some(reason) = stop_reason {
            return RetryDecision::Stop { reason };
        }

        let Some(next_attempt) = completed_attempts.checked_add(1) else {
            return RetryDecision::Stop {
                reason: RetryStopReason::AttemptBudgetExhausted,
            };
        };
        if let Err(reason) = self.can_start_attempt(next_attempt, now) {
            return RetryDecision::Stop { reason };
        }

        RetryDecision::Retry {
            next_attempt,
            delay: self.retry_delay(completed_attempts),
        }
    }

    /// Recheck immediately before a scheduled retry begins so a caller that
    /// later supplies a deadline cannot start a retry after it has elapsed.
    pub(crate) fn can_start_attempt(
        self,
        attempt: u32,
        now: Instant,
    ) -> Result<(), RetryStopReason> {
        if self.retry_deadline.is_some_and(|deadline| now >= deadline) {
            return Err(RetryStopReason::RetryDeadlineElapsed);
        }
        if self
            .max_attempts
            .is_some_and(|max_attempts| attempt > max_attempts)
        {
            return Err(RetryStopReason::AttemptBudgetExhausted);
        }
        Ok(())
    }

    fn retry_delay(self, completed_attempts: u32) -> Duration {
        let exponent = completed_attempts.saturating_sub(1).min(u32::BITS - 1);
        self.initial_delay
            .checked_mul(1u32 << exponent)
            .unwrap_or(self.max_delay)
            .min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(
        deadline: Option<Instant>,
        max_attempts: Option<u32>,
    ) -> ActionPersistenceRetryPolicy {
        ActionPersistenceRetryPolicy {
            retry_deadline: deadline,
            max_attempts,
            ..ActionPersistenceRetryPolicy::terminal_persistence()
        }
    }

    #[test]
    fn terminal_persistence_has_no_deadline_or_retry_budget() {
        let policy = ActionPersistenceRetryPolicy::terminal_persistence();
        assert_eq!(policy.retry_deadline, None);
        assert_eq!(policy.max_attempts, None);
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
        let policy = ActionPersistenceRetryPolicy::terminal_persistence();
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
}
