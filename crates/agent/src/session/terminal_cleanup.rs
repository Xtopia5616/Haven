//! Process-local terminal cleanup retries. Pending policy is authoritative;
//! Notify is only a coalesced wake-up edge, and FIFO identity order prevents
//! a repeatedly failing session from starving the rest of the backlog.

use super::{SessionSupervisor, TerminalCleanupRetry};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

pub(super) const CLEANUP_BATCH_SIZE: usize = 32;
const CLEANUP_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_RETRY_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Default)]
struct PendingCleanup {
    policies: HashMap<String, TerminalCleanupRetry>,
    order: VecDeque<String>,
}

#[derive(Default)]
pub(crate) struct TerminalCleanupRetryQueue {
    pending: Mutex<PendingCleanup>,
    wake: Notify,
}

impl TerminalCleanupRetryQueue {
    pub(super) fn enqueue(&self, session_id: &str, retry: TerminalCleanupRetry) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = pending.policies.get_mut(session_id) {
            if existing.cascade.is_none() && retry.cascade.is_some() {
                existing.cascade = retry.cascade;
            }
            existing.remove_error_actor |= retry.remove_error_actor;
        } else {
            pending.policies.insert(session_id.to_string(), retry);
            pending.order.push_back(session_id.to_string());
        }
        drop(pending);
        self.wake.notify_one();
    }

    fn pop(self: &Arc<Self>) -> Option<CleanupRetryLease> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session_id = pending.order.pop_front()?;
        let retry = pending
            .policies
            .remove(&session_id)
            .expect("cleanup policy and FIFO identity must stay aligned");
        Some(CleanupRetryLease {
            queue: self.clone(),
            session_id,
            retry: Some(retry),
        })
    }

    pub(super) fn pending_count(&self) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .policies
            .len()
    }

    #[cfg(test)]
    pub(super) fn pending_retry(&self, session_id: &str) -> Option<TerminalCleanupRetry> {
        self.pending
            .lock()
            .unwrap()
            .policies
            .get(session_id)
            .copied()
    }
}

/// Own policy as soon as it leaves the map, including cancellation before
/// reconcile_run_exit has had a chance to establish its cleanup guard.
struct CleanupRetryLease {
    queue: Arc<TerminalCleanupRetryQueue>,
    session_id: String,
    retry: Option<TerminalCleanupRetry>,
}

impl Drop for CleanupRetryLease {
    fn drop(&mut self) {
        if let Some(retry) = self.retry {
            self.queue.enqueue(&self.session_id, retry);
        }
    }
}

impl SessionSupervisor {
    /// Runs only in the dispatcher's owned cleanup JoinSet. The final-drain
    /// signal is sent after every run wrapper has exited; it interrupts normal
    /// processing and retries each identity pending at that boundary once.
    pub(super) async fn run_terminal_cleanup_worker(
        self: Arc<Self>,
        final_drain: CancellationToken,
    ) {
        let queue = &self.terminal_cleanup_retry_queue;
        'normal: loop {
            if final_drain.is_cancelled() {
                break;
            }
            if queue.pending_count() == 0 {
                tokio::select! {
                    biased;
                    _ = final_drain.cancelled() => break,
                    _ = queue.wake.notified() => {}
                }
                continue;
            }
            for _ in 0..CLEANUP_BATCH_SIZE {
                let retry_required = tokio::select! {
                    biased;
                    _ = final_drain.cancelled() => break 'normal,
                    result = self.attempt_terminal_cleanup_retry() => result,
                };
                match retry_required {
                    None => break,
                    Some(false) => {}
                    Some(true) => {
                        // A failed identity was reinserted at the FIFO tail.
                        // Back off and end this batch rather than spinning on
                        // the same item when it is the only pending identity.
                        tokio::select! {
                            biased;
                            _ = final_drain.cancelled() => break 'normal,
                            _ = tokio::time::sleep(CLEANUP_RETRY_BACKOFF) => {}
                        }
                        break;
                    }
                }
            }
            tokio::task::yield_now().await;
        }

        // Only this worker pops entries. New enqueues and failed attempts go
        // behind these initial FIFO identities, so count bounds a single final
        // round without cloning or draining an unbounded backlog into a Vec.
        let final_count = queue.pending_count();
        for index in 0..final_count {
            if self.attempt_terminal_cleanup_retry().await.is_none() {
                break;
            }
            if (index + 1) % CLEANUP_BATCH_SIZE == 0 {
                tokio::task::yield_now().await;
            }
        }
    }

    /// None means idle; Some(true) means incomplete cleanup or a deadline.
    async fn attempt_terminal_cleanup_retry(&self) -> Option<bool> {
        let mut lease = self.terminal_cleanup_retry_queue.pop()?;
        let retry = lease
            .retry
            .expect("active cleanup lease must retain policy");
        match tokio::time::timeout(
            CLEANUP_ATTEMPT_TIMEOUT,
            self.retry_terminal_cleanup(&lease.session_id, retry),
        )
        .await
        {
            Ok(completed) => {
                // An incomplete cleanup has already requeued its guard policy.
                // Do not duplicate it, but apply the same backoff as a timeout.
                lease.retry = None;
                Some(!completed)
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %lease.session_id,
                    timeout = ?CLEANUP_ATTEMPT_TIMEOUT,
                    "terminal session cleanup retry timed out; retained for retry"
                );
                Some(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(cascade: Option<bool>, remove_error_actor: bool) -> TerminalCleanupRetry {
        TerminalCleanupRetry {
            cascade,
            remove_error_actor,
        }
    }

    #[test]
    fn repeated_wakeups_merge_policy_and_keep_one_fifo_identity() {
        let queue = Arc::new(TerminalCleanupRetryQueue::default());
        let first = haven_common::types::new_id("ses");
        let second = haven_common::types::new_id("ses");
        queue.enqueue(&first, policy(None, false));
        queue.enqueue(&second, policy(Some(true), false));
        for _ in 0..10_000 {
            queue.enqueue(&first, policy(Some(false), true));
        }
        assert_eq!(queue.pending_count(), 2);
        let mut lease = queue.pop().unwrap();
        assert_eq!(lease.session_id, first);
        assert_eq!(lease.retry, Some(policy(Some(false), true)));
        lease.retry = None;
        let mut lease = queue.pop().unwrap();
        assert_eq!(lease.session_id, second);
        lease.retry = None;
        assert!(queue.pop().is_none());
    }

    #[test]
    fn cancelled_pop_restores_policy_at_tail_without_starving_other_sessions() {
        let queue = Arc::new(TerminalCleanupRetryQueue::default());
        let first = haven_common::types::new_id("ses");
        let second = haven_common::types::new_id("ses");
        queue.enqueue(&first, policy(Some(false), true));
        queue.enqueue(&second, policy(Some(true), false));
        drop(queue.pop().unwrap());
        let mut lease = queue.pop().unwrap();
        assert_eq!(lease.session_id, second);
        lease.retry = None;
        let lease = queue.pop().unwrap();
        assert_eq!(lease.session_id, first);
        assert_eq!(lease.retry, Some(policy(Some(false), true)));
        queue.enqueue(&first, policy(None, false));
        drop(lease);
        assert_eq!(queue.pending_retry(&first), Some(policy(Some(false), true)));
        assert_eq!(queue.pending_count(), 1);
    }
}
