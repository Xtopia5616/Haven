//! Shared, pure recovery policy decisions.
//!
//! The caller owns error classification, sleeping, cancellation, persistence,
//! queue acknowledgement, and side effects. This module only decides whether
//! another attempt is allowed and computes its delay.

use std::time::{Duration, Instant};

/// Exponential backoff shape shared by retry owners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BackoffPolicy {
    pub initial_delay: Duration,
    pub factor: u32,
    pub max_delay: Duration,
    pub jitter: RetryJitter,
}

impl BackoffPolicy {
    pub const fn new(initial_delay: Duration, factor: u32, max_delay: Duration) -> Self {
        Self {
            initial_delay,
            factor,
            max_delay,
            jitter: RetryJitter::None,
        }
    }

    pub const fn with_jitter(mut self, jitter: RetryJitter) -> Self {
        self.jitter = jitter;
        self
    }

    /// Compute the delay after `completed_attempts` failures. The first
    /// failure uses `initial_delay`; provider `Retry-After` is never capped by
    /// the local backoff maximum.
    pub fn delay_after(
        self,
        completed_attempts: u32,
        retry_after: Option<Duration>,
        jitter_sample: u32,
    ) -> Duration {
        let exponent = completed_attempts.saturating_sub(1);
        let multiplier = self.factor.max(1).saturating_pow(exponent);
        let local_backoff = self
            .initial_delay
            .checked_mul(multiplier)
            .unwrap_or(self.max_delay)
            .min(self.max_delay);
        let jitter_sample = f64::from(jitter_sample) / f64::from(u32::MAX);
        let jittered = match self.jitter {
            RetryJitter::None => local_backoff,
            RetryJitter::SymmetricFraction(fraction) => {
                if local_backoff.is_zero() {
                    local_backoff
                } else {
                    let fraction = f64::from(fraction.clamp(0.0, 1.0));
                    let spread = 1.0 + ((jitter_sample * 2.0 - 1.0) * fraction);
                    local_backoff.mul_f64(spread.max(0.0)).min(self.max_delay)
                }
            }
            RetryJitter::Additive(max_jitter) => {
                let jitter = max_jitter.mul_f64(jitter_sample);
                local_backoff.saturating_add(jitter).min(self.max_delay)
            }
        };

        jittered.max(retry_after.unwrap_or_default())
    }
}

/// Jitter applied to a locally computed delay. Callers supply a stable or
/// random sample in the inclusive range `0..=u32::MAX`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RetryJitter {
    None,
    /// Symmetric spread around the local delay as a fraction from 0 to 1.
    SymmetricFraction(f32),
    /// Add a non-negative amount up to this duration.
    Additive(Duration),
}

/// Complete retry budget and stop conditions for one recovery owner.
///
/// `max_attempts` includes the original attempt. `None` means no attempt
/// budget; this does not create or own a job lifecycle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecoveryPolicy {
    pub max_attempts: Option<u32>,
    pub deadline: Option<Instant>,
    pub backoff: BackoffPolicy,
}

impl RecoveryPolicy {
    pub const fn new(
        max_attempts: Option<u32>,
        deadline: Option<Instant>,
        backoff: BackoffPolicy,
    ) -> Self {
        Self {
            max_attempts,
            deadline,
            backoff,
        }
    }

    /// Decide whether to schedule another attempt after the current attempt
    /// has finished.
    pub fn decide(
        self,
        completed_attempts: u32,
        signal: RecoverySignal,
        now: Instant,
        jitter_sample: u32,
    ) -> RecoveryDecision {
        let retry_after = match signal {
            RecoverySignal::Retryable { retry_after } => retry_after,
            RecoverySignal::PermanentFailure => {
                return RecoveryDecision::Stop {
                    reason: RecoveryStopReason::NonRetryableFailure,
                };
            }
            RecoverySignal::OutcomeUnknown => {
                return RecoveryDecision::Stop {
                    reason: RecoveryStopReason::OutcomeUnknown,
                };
            }
            RecoverySignal::Cancelled => {
                return RecoveryDecision::Stop {
                    reason: RecoveryStopReason::Cancelled,
                };
            }
            RecoverySignal::Succeeded => {
                return RecoveryDecision::Stop {
                    reason: RecoveryStopReason::Succeeded,
                };
            }
            RecoverySignal::Terminal => {
                return RecoveryDecision::Stop {
                    reason: RecoveryStopReason::Terminal,
                };
            }
        };

        let Some(next_attempt) = completed_attempts.checked_add(1) else {
            return RecoveryDecision::Stop {
                reason: RecoveryStopReason::AttemptBudgetExhausted,
            };
        };
        if let Err(reason) = self.can_start_attempt(next_attempt, now) {
            return RecoveryDecision::Stop { reason };
        }

        RecoveryDecision::Retry {
            next_attempt,
            delay: self
                .backoff
                .delay_after(completed_attempts, retry_after, jitter_sample),
        }
    }

    /// Recheck a scheduled attempt immediately before it starts. Callers that
    /// sleep between `decide` and execution use this to honor a deadline that
    /// may have elapsed during the wait.
    pub fn can_start_attempt(self, attempt: u32, now: Instant) -> Result<(), RecoveryStopReason> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Err(RecoveryStopReason::RetryDeadlineElapsed);
        }
        if self
            .max_attempts
            .is_some_and(|max_attempts| attempt > max_attempts)
        {
            return Err(RecoveryStopReason::AttemptBudgetExhausted);
        }
        Ok(())
    }
}

/// Outcome classification made by the domain that owns the operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverySignal {
    Retryable {
        retry_after: Option<Duration>,
    },
    PermanentFailure,
    /// The operation may have happened, so replay could duplicate a side effect.
    OutcomeUnknown,
    Cancelled,
    Succeeded,
    /// A competing owner completed or superseded this operation.
    Terminal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryStopReason {
    NonRetryableFailure,
    OutcomeUnknown,
    Cancelled,
    Succeeded,
    Terminal,
    RetryDeadlineElapsed,
    AttemptBudgetExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryDecision {
    Retry { next_attempt: u32, delay: Duration },
    Stop { reason: RecoveryStopReason },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(max_attempts: Option<u32>, backoff: BackoffPolicy) -> RecoveryPolicy {
        RecoveryPolicy::new(max_attempts, None, backoff)
    }

    #[test]
    fn unbounded_backoff_doubles_then_caps() {
        let policy = policy(
            None,
            BackoffPolicy::new(Duration::from_secs(1), 2, Duration::from_secs(30)),
        );
        let now = Instant::now();
        let delays: Vec<_> = (1..=7)
            .map(|completed| {
                match policy.decide(
                    completed,
                    RecoverySignal::Retryable { retry_after: None },
                    now,
                    u32::MAX / 2,
                ) {
                    RecoveryDecision::Retry { delay, .. } => delay,
                    decision => panic!("expected retry, got {decision:?}"),
                }
            })
            .collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 30, 30].map(Duration::from_secs));
    }

    #[test]
    fn bounded_policy_counts_initial_attempt_and_honors_retry_after() {
        let policy = policy(
            Some(3),
            BackoffPolicy::new(Duration::from_secs(2), 2, Duration::from_secs(10)),
        );
        let now = Instant::now();
        assert_eq!(
            policy.decide(
                1,
                RecoverySignal::Retryable {
                    retry_after: Some(Duration::from_secs(20)),
                },
                now,
                0,
            ),
            RecoveryDecision::Retry {
                next_attempt: 2,
                delay: Duration::from_secs(20),
            }
        );
        assert_eq!(
            policy.decide(3, RecoverySignal::Retryable { retry_after: None }, now, 0,),
            RecoveryDecision::Stop {
                reason: RecoveryStopReason::AttemptBudgetExhausted,
            }
        );
    }

    #[test]
    fn stop_signals_are_not_replayed() {
        let policy = policy(
            None,
            BackoffPolicy::new(Duration::from_secs(1), 2, Duration::from_secs(30)),
        );
        let now = Instant::now();
        for (signal, reason) in [
            (
                RecoverySignal::PermanentFailure,
                RecoveryStopReason::NonRetryableFailure,
            ),
            (
                RecoverySignal::OutcomeUnknown,
                RecoveryStopReason::OutcomeUnknown,
            ),
            (RecoverySignal::Cancelled, RecoveryStopReason::Cancelled),
            (RecoverySignal::Succeeded, RecoveryStopReason::Succeeded),
            (RecoverySignal::Terminal, RecoveryStopReason::Terminal),
        ] {
            assert_eq!(
                policy.decide(1, signal, now, 0),
                RecoveryDecision::Stop { reason }
            );
        }
    }

    #[test]
    fn deadline_is_checked_before_scheduling_a_retry() {
        let now = Instant::now();
        let policy = RecoveryPolicy::new(
            None,
            Some(now),
            BackoffPolicy::new(Duration::from_secs(1), 2, Duration::from_secs(30)),
        );
        assert_eq!(
            policy.decide(1, RecoverySignal::Retryable { retry_after: None }, now, 0,),
            RecoveryDecision::Stop {
                reason: RecoveryStopReason::RetryDeadlineElapsed,
            }
        );
    }

    #[test]
    fn jitter_modes_are_bounded_by_local_cap() {
        let symmetric = BackoffPolicy::new(Duration::from_secs(10), 2, Duration::from_secs(10))
            .with_jitter(RetryJitter::SymmetricFraction(0.25));
        assert_eq!(
            symmetric.delay_after(1, None, 0),
            Duration::from_millis(7_500)
        );
        assert_eq!(
            symmetric.delay_after(1, None, u32::MAX),
            Duration::from_secs(10)
        );

        let additive = BackoffPolicy::new(Duration::from_secs(29), 2, Duration::from_secs(30))
            .with_jitter(RetryJitter::Additive(Duration::from_secs(1)));
        assert_eq!(
            additive.delay_after(1, None, u32::MAX),
            Duration::from_secs(30)
        );
    }
}
