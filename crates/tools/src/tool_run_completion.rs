//! Completion transport shared by background and scheduled tool_runs.
//!
//! This module owns the transient broadcast stream and scheduled-fire
//! in-process recovery claims. ToolRunService retains ToolRun state,
//! persistence, timers, and lifecycle transitions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use haven_common::ToolRunStatus;
use haven_common::tool_run_lease::ToolRunLease;
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};

use crate::tool_run_service::ToolRunService;
use crate::tool_run_types::ScheduledToolRunFired;

const TOOL_RUN_COMPLETION_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const SCHEDULED_FIRE_LEASE: Duration = Duration::from_secs(15 * 60);
const TOOL_RUN_COMPLETION_CHANNEL_CAPACITY: usize = 256;

/// A background ToolRun that has reached a terminal state, surfaced to a
/// consumer so the owning session receives it without status polling.
#[derive(Clone, Debug)]
pub struct BackgroundToolRunCompletion {
    pub tool_run_id: String,
    /// Stable identity of the terminal result. It remains the same when the
    /// broadcast is replayed or the owning session queue retries delivery.
    pub tool_run_result_id: String,
    pub session_id: Option<String>,
    /// Canonical terminal lifecycle status.
    pub status: ToolRunStatus,
    /// The ToolRun's status JSON (same shape `status()` returns for terminal
    /// states), carrying the output/error payload.
    pub status_json: Value,
}

/// A scheduled tool ToolRun that has reached a completed or failed state.
/// Scheduled firing/execution stays on its own owner path; only the terminal
/// result delivery is shared with background ToolRun results.
#[derive(Clone, Debug)]
pub struct ScheduledToolRunResultCompletion {
    pub tool_run_id: String,
    pub tool_run_result_id: String,
    pub session_id: Option<String>,
    pub status: ToolRunStatus,
    pub status_json: Value,
}

/// One completion stream for every ToolRun kind.
#[derive(Clone, Debug)]
pub enum ToolRunCompletion {
    Background(BackgroundToolRunCompletion),
    ScheduledResult(ScheduledToolRunResultCompletion),
    Scheduled(ScheduledToolRunFired),
}

/// Receiver for the unified ToolRun completion stream.
pub struct ToolRunCompletionReceiver {
    rx: broadcast::Receiver<ToolRunCompletion>,
    /// Scheduled fire claims are shared by all receivers. Terminal result
    /// claims remain durable in the ToolRunStore outbox.
    pending_scheduled_fires: Arc<RwLock<HashMap<String, ScheduledToolRunFired>>>,
    tool_run_leases: Arc<RwLock<HashMap<String, ToolRunLease<Instant>>>>,
}

/// Shared owner of the transient completion broadcast and in-process claims.
pub(crate) struct ToolRunCompletionBus {
    tx: broadcast::Sender<ToolRunCompletion>,
    pending_scheduled_fires: Arc<RwLock<HashMap<String, ScheduledToolRunFired>>>,
    tool_run_leases: Arc<RwLock<HashMap<String, ToolRunLease<Instant>>>>,
}

impl ToolRunCompletionBus {
    pub(crate) fn new() -> Self {
        let (tx, _) = broadcast::channel(TOOL_RUN_COMPLETION_CHANNEL_CAPACITY);
        Self {
            tx,
            pending_scheduled_fires: Arc::new(RwLock::new(HashMap::new())),
            tool_run_leases: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) fn subscribe(&self) -> ToolRunCompletionReceiver {
        ToolRunCompletionReceiver {
            rx: self.tx.subscribe(),
            pending_scheduled_fires: Arc::clone(&self.pending_scheduled_fires),
            tool_run_leases: Arc::clone(&self.tool_run_leases),
        }
    }

    pub(crate) fn send(
        &self,
        completion: ToolRunCompletion,
    ) -> Result<usize, Box<broadcast::error::SendError<ToolRunCompletion>>> {
        self.tx.send(completion).map_err(Box::new)
    }

    pub(crate) async fn retain_scheduled_fire(&self, fired: ScheduledToolRunFired) {
        self.pending_scheduled_fires
            .write()
            .await
            .insert(fired.tool_run_id.clone(), fired);
    }

    /// Claim a retained fire for recovery by a receiver created after its
    /// transient broadcast was missed. The claim map lock always precedes the
    /// pending-fire map lock, matching `claim_scheduled_fire` and removal.
    pub(crate) async fn pending_scheduled_fire(&self) -> Option<ScheduledToolRunFired> {
        let ids = self
            .pending_scheduled_fires
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for tool_run_id in ids {
            if let Some(fired) = claim_scheduled_fire(
                &self.pending_scheduled_fires,
                &self.tool_run_leases,
                &tool_run_id,
            )
            .await
            {
                return Some(fired);
            }
        }
        None
    }

    /// Clear a scheduled recovery item after the owning ToolRun completes or
    /// when a no-consumer fire is rolled back. Keep the claim -> pending lock
    /// order used by claims so simultaneous receivers cannot invert locks.
    pub(crate) async fn clear_scheduled_fire(&self, tool_run_id: &str) {
        self.release_tool_run_lease(tool_run_id, tool_run_id).await;
        self.pending_scheduled_fires
            .write()
            .await
            .remove(tool_run_id);
    }

    async fn release_tool_run_lease(&self, tool_run_id: &str, claim_token: &str) -> bool {
        let mut leases = self.tool_run_leases.write().await;
        if !leases
            .get_mut(tool_run_id)
            .is_some_and(|lease| lease.invalidate_for(claim_token))
        {
            return false;
        }
        leases.remove(tool_run_id);
        true
    }

    #[cfg(test)]
    pub(crate) async fn has_pending_scheduled_fire(&self, tool_run_id: &str) -> bool {
        self.pending_scheduled_fires
            .read()
            .await
            .contains_key(tool_run_id)
    }

    #[cfg(test)]
    pub(crate) async fn has_scheduled_fire_claim(&self, tool_run_id: &str) -> bool {
        self.tool_run_leases.read().await.contains_key(tool_run_id)
    }

    #[cfg(test)]
    pub(crate) async fn has_tool_run_lease(&self, tool_run_id: &str) -> bool {
        self.tool_run_leases.read().await.contains_key(tool_run_id)
    }
}

async fn claim_scheduled_fire(
    pending_scheduled_fires: &RwLock<HashMap<String, ScheduledToolRunFired>>,
    tool_run_leases: &RwLock<HashMap<String, ToolRunLease<Instant>>>,
    tool_run_id: &str,
) -> Option<ScheduledToolRunFired> {
    // Claim and lookup use the same lock order everywhere. This makes the
    // claim check atomic from the perspective of concurrent receivers while
    // allowing an abandoned consumer to be recovered after the lease expires.
    let mut leases = tool_run_leases.write().await;
    let now = Instant::now();
    let lease = ToolRunLease::try_claim(
        leases.get(tool_run_id),
        tool_run_id,
        &now,
        now + SCHEDULED_FIRE_LEASE,
    )?;
    let fired = pending_scheduled_fires
        .read()
        .await
        .get(tool_run_id)
        .cloned()?;
    leases.insert(tool_run_id.to_string(), lease);
    Some(fired)
}

impl ToolRunCompletionReceiver {
    pub async fn recv(&mut self) -> Option<ToolRunCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(ToolRunCompletion::Scheduled(fired)) => {
                    if let Some(fired) = claim_scheduled_fire(
                        &self.pending_scheduled_fires,
                        &self.tool_run_leases,
                        &fired.tool_run_id,
                    )
                    .await
                    {
                        return Some(ToolRunCompletion::Scheduled(fired));
                    }
                }
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ToolRun completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Receive background completions without claiming scheduled fires. This
    /// narrow receiver remains useful to callers that do not consume scheduled
    /// tool results.
    pub async fn recv_background(&mut self) -> Option<ToolRunCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(ToolRunCompletion::Background(completion)) => {
                    return Some(ToolRunCompletion::Background(completion));
                }
                Ok(ToolRunCompletion::ScheduledResult(_) | ToolRunCompletion::Scheduled(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ToolRun completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Receive an ToolRun result from either the transient broadcast or
    /// the durable outbox. The outbox is checked after every broadcast lag and
    /// on a bounded interval so a completion that was never published still
    /// wakes the owning session. Delivery claims expire if the consumer dies.
    pub async fn recv_tool_run_result_with_recovery(
        &mut self,
        service: &ToolRunService,
    ) -> Option<ToolRunCompletion> {
        let mut reconcile = tokio::time::interval(TOOL_RUN_COMPLETION_RECONCILE_INTERVAL);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Reconcile once immediately at startup; subsequent checks are
        // bounded by the interval while the transient channel remains the
        // fast path for newly completed tool_runs.
        reconcile.tick().await;
        loop {
            if let Some(completion) = service.claim_pending_tool_run_result().await {
                return Some(completion);
            }
            match tokio::select! {
                result = self.rx.recv() => result,
                _ = reconcile.tick() => continue,
            } {
                Ok(ToolRunCompletion::Background(completion)) => {
                    return Some(ToolRunCompletion::Background(completion));
                }
                Ok(ToolRunCompletion::ScheduledResult(completion)) => {
                    return Some(ToolRunCompletion::ScheduledResult(completion));
                }
                Ok(ToolRunCompletion::Scheduled(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "ToolRun result receiver lagged; reconciling durable outbox"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return service.claim_pending_tool_run_result().await;
                }
            }
        }
    }

    /// Receive a scheduled trigger with recovery for broadcast lag. The
    /// scheduled ToolRun remains in an in-memory unacknowledged set until the
    /// actual work is acknowledged, so a lagged receiver can replay it rather
    /// than silently losing the trigger.
    pub async fn recv_scheduled_with_recovery(
        &mut self,
        service: &ToolRunService,
    ) -> Option<ToolRunCompletion> {
        loop {
            // A fire can have been retained after a send with no consumer. A
            // receiver created later must drain that recovery source before
            // waiting on the transient broadcast channel.
            if let Some(fired) = service.pending_scheduled_fire().await {
                return Some(ToolRunCompletion::Scheduled(fired));
            }
            match self.rx.recv().await {
                Ok(ToolRunCompletion::Scheduled(fired)) => {
                    if let Some(fired) = claim_scheduled_fire(
                        &self.pending_scheduled_fires,
                        &self.tool_run_leases,
                        &fired.tool_run_id,
                    )
                    .await
                    {
                        return Some(ToolRunCompletion::Scheduled(fired));
                    }
                }
                Ok(ToolRunCompletion::Background(_))
                | Ok(ToolRunCompletion::ScheduledResult(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ToolRun completion receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduled_fire(tool_run_id: &str) -> ScheduledToolRunFired {
        ScheduledToolRunFired {
            tool_run_id: tool_run_id.into(),
            title: "Scheduled".into(),
            body: "fire".into(),
            mode: crate::tool_run_types::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        }
    }

    #[tokio::test]
    async fn scheduled_fire_claim_is_shared_and_deduplicates_receivers() {
        let bus = ToolRunCompletionBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        let fired = scheduled_fire("toolrun-shared-claim");
        bus.retain_scheduled_fire(fired.clone()).await;
        bus.send(ToolRunCompletion::Scheduled(fired)).unwrap();

        assert!(matches!(
            first.recv().await,
            Some(ToolRunCompletion::Scheduled(_))
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), second.recv())
                .await
                .is_err()
        );
        assert!(bus.has_scheduled_fire_claim("toolrun-shared-claim").await);
    }

    #[tokio::test]
    async fn expired_scheduled_fire_claim_can_be_reclaimed() {
        let bus = ToolRunCompletionBus::new();
        let fired = scheduled_fire("toolrun-expired-claim");
        bus.retain_scheduled_fire(fired.clone()).await;

        assert!(
            claim_scheduled_fire(
                &bus.pending_scheduled_fires,
                &bus.tool_run_leases,
                &fired.tool_run_id,
            )
            .await
            .is_some()
        );
        bus.tool_run_leases.write().await.insert(
            fired.tool_run_id.clone(),
            ToolRunLease::new(
                fired.tool_run_id.clone(),
                Instant::now() - Duration::from_secs(1),
            ),
        );

        let recovered = claim_scheduled_fire(
            &bus.pending_scheduled_fires,
            &bus.tool_run_leases,
            &fired.tool_run_id,
        )
        .await
        .expect("expired lease must permit recovery");
        assert_eq!(recovered.tool_run_id, fired.tool_run_id);
    }

    #[tokio::test]
    async fn scheduled_claim_release_checks_token_and_terminal_clear_invalidates_it() {
        let bus = ToolRunCompletionBus::new();
        let fired = scheduled_fire("toolrun-release-claim");
        bus.retain_scheduled_fire(fired.clone()).await;
        assert!(
            claim_scheduled_fire(
                &bus.pending_scheduled_fires,
                &bus.tool_run_leases,
                &fired.tool_run_id,
            )
            .await
            .is_some()
        );

        assert!(
            !bus.release_tool_run_lease(&fired.tool_run_id, "toolrun-wrong-token")
                .await
        );
        assert!(bus.has_tool_run_lease(&fired.tool_run_id).await);

        // Scheduled completion/cancellation uses this clear path. Once
        // terminal, both the lease and its recoverable fire are invalidated.
        bus.clear_scheduled_fire(&fired.tool_run_id).await;
        assert!(!bus.has_scheduled_fire_claim(&fired.tool_run_id).await);
        assert!(!bus.has_pending_scheduled_fire(&fired.tool_run_id).await);
        assert!(bus.pending_scheduled_fire().await.is_none());
    }

    #[tokio::test]
    async fn background_completion_passes_through_without_scheduled_claim() {
        let bus = ToolRunCompletionBus::new();
        let mut receiver = bus.subscribe();
        let completion = BackgroundToolRunCompletion {
            tool_run_id: "toolrun-background".into(),
            tool_run_result_id: "toolrun-background".into(),
            session_id: Some("ses-background".into()),
            status: ToolRunStatus::Completed,
            status_json: serde_json::json!({"status": "completed"}),
        };
        bus.send(ToolRunCompletion::Background(completion.clone()))
            .unwrap();

        let received = receiver.recv_background().await.unwrap();
        let ToolRunCompletion::Background(received) = received else {
            panic!("background receiver must pass through background completions");
        };
        assert_eq!(received.tool_run_result_id, completion.tool_run_result_id);
        assert!(bus.tool_run_leases.read().await.is_empty());
    }

    #[tokio::test]
    async fn lagged_receiver_continues_and_closed_receiver_returns_none() {
        let bus = ToolRunCompletionBus::new();
        let mut receiver = bus.subscribe();
        for index in 0..=TOOL_RUN_COMPLETION_CHANNEL_CAPACITY {
            bus.send(ToolRunCompletion::Background(BackgroundToolRunCompletion {
                tool_run_id: format!("toolrun-noise-{index}"),
                tool_run_result_id: format!("toolrun-noise-{index}"),
                session_id: None,
                status: ToolRunStatus::Completed,
                status_json: serde_json::json!({"status": "completed"}),
            }))
            .unwrap();
        }

        let received = receiver.recv().await.unwrap();
        let ToolRunCompletion::Background(received) = received else {
            panic!("lag recovery should return a retained event");
        };
        assert_eq!(received.tool_run_id, "toolrun-noise-1");

        drop(bus);
        while receiver.recv().await.is_some() {}
        assert!(receiver.recv().await.is_none());
    }
}
