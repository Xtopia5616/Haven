//! Completion transport shared by background and scheduled actions.
//!
//! This module owns the transient broadcast stream and scheduled-fire
//! in-process recovery claims. ActionService retains action state,
//! persistence, timers, and lifecycle transitions.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use haven_common::ActionStatus;
use haven_common::action_lease::ActionLease;
use serde_json::Value;
use tokio::sync::{RwLock, broadcast};

use crate::action_service::ActionService;

const ACTION_COMPLETION_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const SCHEDULED_FIRE_LEASE: Duration = Duration::from_secs(15 * 60);
const ACTION_COMPLETION_CHANNEL_CAPACITY: usize = 256;

/// A background action that has reached a terminal state, surfaced to a consumer
/// (the agent layer) so the owning session can be auto-notified of the result
/// instead of the model having to poll `status`.
#[derive(Clone, Debug)]
pub struct BackgroundActionCompletion {
    pub action_id: String,
    /// Stable identity of the terminal result. It remains the same when the
    /// broadcast is replayed or the owning session queue retries delivery.
    pub action_result_id: String,
    pub session_id: Option<String>,
    /// Canonical terminal lifecycle status.
    pub status: ActionStatus,
    /// The action's status JSON (same shape `status()` returns for terminal
    /// states), carrying the output/error payload.
    pub status_json: Value,
}

/// A scheduled action that reached its durable `Waiting -> Running` trigger
/// transition. The agent must acknowledge the actual work with
/// [`ActionService::complete_scheduled`] or [`ActionService::fail_scheduled`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct ScheduledActionFired {
    pub action_id: String,
    pub title: String,
    pub body: String,
    pub mode: crate::builtin::scheduled_action::ScheduleMode,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<Value>,
    pub prompt: Option<String>,
}

/// One completion stream for every action kind.
#[derive(Clone, Debug)]
pub enum ActionCompletion {
    Background(BackgroundActionCompletion),
    Scheduled(ScheduledActionFired),
}

/// Receiver for the unified action completion stream.
pub struct ActionCompletionReceiver {
    rx: broadcast::Receiver<ActionCompletion>,
    /// Scheduled fire claims are shared by all receivers. Background
    /// completion claims remain durable in the ActionStore outbox.
    pending_scheduled_fires: Arc<RwLock<HashMap<String, ScheduledActionFired>>>,
    action_leases: Arc<RwLock<HashMap<String, ActionLease<Instant>>>>,
}

/// Shared owner of the transient completion broadcast and in-process claims.
pub(crate) struct ActionCompletionBus {
    tx: broadcast::Sender<ActionCompletion>,
    pending_scheduled_fires: Arc<RwLock<HashMap<String, ScheduledActionFired>>>,
    action_leases: Arc<RwLock<HashMap<String, ActionLease<Instant>>>>,
}

impl ActionCompletionBus {
    pub(crate) fn new() -> Self {
        let (tx, _) = broadcast::channel(ACTION_COMPLETION_CHANNEL_CAPACITY);
        Self {
            tx,
            pending_scheduled_fires: Arc::new(RwLock::new(HashMap::new())),
            action_leases: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub(crate) fn subscribe(&self) -> ActionCompletionReceiver {
        ActionCompletionReceiver {
            rx: self.tx.subscribe(),
            pending_scheduled_fires: Arc::clone(&self.pending_scheduled_fires),
            action_leases: Arc::clone(&self.action_leases),
        }
    }

    pub(crate) fn send(
        &self,
        completion: ActionCompletion,
    ) -> Result<usize, Box<broadcast::error::SendError<ActionCompletion>>> {
        self.tx.send(completion).map_err(Box::new)
    }

    pub(crate) async fn retain_scheduled_fire(&self, fired: ScheduledActionFired) {
        self.pending_scheduled_fires
            .write()
            .await
            .insert(fired.action_id.clone(), fired);
    }

    /// Claim a retained fire for recovery by a receiver created after its
    /// transient broadcast was missed. The claim map lock always precedes the
    /// pending-fire map lock, matching `claim_scheduled_fire` and removal.
    pub(crate) async fn pending_scheduled_fire(&self) -> Option<ScheduledActionFired> {
        let ids = self
            .pending_scheduled_fires
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for action_id in ids {
            if let Some(fired) = claim_scheduled_fire(
                &self.pending_scheduled_fires,
                &self.action_leases,
                &action_id,
            )
            .await
            {
                return Some(fired);
            }
        }
        None
    }

    /// Clear a scheduled recovery item after the owning action completes or
    /// when a no-consumer fire is rolled back. Keep the claim -> pending lock
    /// order used by claims so simultaneous receivers cannot invert locks.
    pub(crate) async fn clear_scheduled_fire(&self, action_id: &str) {
        self.release_action_lease(action_id, action_id).await;
        self.pending_scheduled_fires.write().await.remove(action_id);
    }

    async fn release_action_lease(&self, action_id: &str, claim_token: &str) -> bool {
        let mut leases = self.action_leases.write().await;
        if !leases
            .get_mut(action_id)
            .is_some_and(|lease| lease.invalidate_for(claim_token))
        {
            return false;
        }
        leases.remove(action_id);
        true
    }

    #[cfg(test)]
    pub(crate) async fn has_pending_scheduled_fire(&self, action_id: &str) -> bool {
        self.pending_scheduled_fires
            .read()
            .await
            .contains_key(action_id)
    }

    #[cfg(test)]
    pub(crate) async fn has_scheduled_fire_claim(&self, action_id: &str) -> bool {
        self.action_leases.read().await.contains_key(action_id)
    }

    #[cfg(test)]
    pub(crate) async fn has_action_lease(&self, action_id: &str) -> bool {
        self.action_leases.read().await.contains_key(action_id)
    }
}

async fn claim_scheduled_fire(
    pending_scheduled_fires: &RwLock<HashMap<String, ScheduledActionFired>>,
    action_leases: &RwLock<HashMap<String, ActionLease<Instant>>>,
    action_id: &str,
) -> Option<ScheduledActionFired> {
    // Claim and lookup use the same lock order everywhere. This makes the
    // claim check atomic from the perspective of concurrent receivers while
    // allowing an abandoned consumer to be recovered after the lease expires.
    let mut leases = action_leases.write().await;
    let now = Instant::now();
    let lease = ActionLease::try_claim(
        leases.get(action_id),
        action_id,
        &now,
        now + SCHEDULED_FIRE_LEASE,
    )?;
    let fired = pending_scheduled_fires
        .read()
        .await
        .get(action_id)
        .cloned()?;
    leases.insert(action_id.to_string(), lease);
    Some(fired)
}

impl ActionCompletionReceiver {
    pub async fn recv(&mut self) -> Option<ActionCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(ActionCompletion::Scheduled(fired)) => {
                    if let Some(fired) = claim_scheduled_fire(
                        &self.pending_scheduled_fires,
                        &self.action_leases,
                        &fired.action_id,
                    )
                    .await
                    {
                        return Some(ActionCompletion::Scheduled(fired));
                    }
                }
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "action completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Receive background completions without claiming scheduled fires. The
    /// agent's background consumer uses this so a second receiver cannot steal
    /// a scheduled trigger before the dedicated scheduled consumer sees it.
    pub async fn recv_background(&mut self) -> Option<ActionCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(ActionCompletion::Background(completion)) => {
                    return Some(ActionCompletion::Background(completion));
                }
                Ok(ActionCompletion::Scheduled(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "action completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Receive a background completion from either the transient broadcast or
    /// the durable outbox. The outbox is checked after every broadcast lag and
    /// on a bounded interval so a completion that was never published still
    /// wakes the owning session. Delivery claims expire if the consumer dies.
    pub async fn recv_background_with_recovery(
        &mut self,
        service: &ActionService,
    ) -> Option<ActionCompletion> {
        let mut reconcile = tokio::time::interval(ACTION_COMPLETION_RECONCILE_INTERVAL);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Reconcile once immediately at startup; subsequent checks are
        // bounded by the interval while the transient channel remains the
        // fast path for newly completed actions.
        reconcile.tick().await;
        loop {
            if let Some(completion) = service.claim_pending_background_completion().await {
                return Some(ActionCompletion::Background(completion));
            }
            match tokio::select! {
                result = self.rx.recv() => result,
                _ = reconcile.tick() => continue,
            } {
                Ok(ActionCompletion::Background(completion)) => {
                    return Some(ActionCompletion::Background(completion));
                }
                Ok(ActionCompletion::Scheduled(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "action completion receiver lagged; reconciling durable outbox"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return service
                        .claim_pending_background_completion()
                        .await
                        .map(ActionCompletion::Background);
                }
            }
        }
    }

    /// Receive a scheduled trigger with recovery for broadcast lag. The
    /// scheduled action remains in an in-memory unacknowledged set until the
    /// actual work is acknowledged, so a lagged receiver can replay it rather
    /// than silently losing the trigger.
    pub async fn recv_scheduled_with_recovery(
        &mut self,
        service: &ActionService,
    ) -> Option<ActionCompletion> {
        loop {
            // A fire can have been retained after a send with no consumer. A
            // receiver created later must drain that recovery source before
            // waiting on the transient broadcast channel.
            if let Some(fired) = service.pending_scheduled_fire().await {
                return Some(ActionCompletion::Scheduled(fired));
            }
            match self.rx.recv().await {
                Ok(ActionCompletion::Scheduled(fired)) => {
                    if let Some(fired) = claim_scheduled_fire(
                        &self.pending_scheduled_fires,
                        &self.action_leases,
                        &fired.action_id,
                    )
                    .await
                    {
                        return Some(ActionCompletion::Scheduled(fired));
                    }
                }
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "action completion receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduled_fire(action_id: &str) -> ScheduledActionFired {
        ScheduledActionFired {
            action_id: action_id.into(),
            title: "Scheduled".into(),
            body: "fire".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        }
    }

    #[tokio::test]
    async fn scheduled_fire_claim_is_shared_and_deduplicates_receivers() {
        let bus = ActionCompletionBus::new();
        let mut first = bus.subscribe();
        let mut second = bus.subscribe();
        let fired = scheduled_fire("act-shared-claim");
        bus.retain_scheduled_fire(fired.clone()).await;
        bus.send(ActionCompletion::Scheduled(fired)).unwrap();

        assert!(matches!(
            first.recv().await,
            Some(ActionCompletion::Scheduled(_))
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(25), second.recv())
                .await
                .is_err()
        );
        assert!(bus.has_scheduled_fire_claim("act-shared-claim").await);
    }

    #[tokio::test]
    async fn expired_scheduled_fire_claim_can_be_reclaimed() {
        let bus = ActionCompletionBus::new();
        let fired = scheduled_fire("act-expired-claim");
        bus.retain_scheduled_fire(fired.clone()).await;

        assert!(
            claim_scheduled_fire(
                &bus.pending_scheduled_fires,
                &bus.action_leases,
                &fired.action_id,
            )
            .await
            .is_some()
        );
        bus.action_leases.write().await.insert(
            fired.action_id.clone(),
            ActionLease::new(
                fired.action_id.clone(),
                Instant::now() - Duration::from_secs(1),
            ),
        );

        let recovered = claim_scheduled_fire(
            &bus.pending_scheduled_fires,
            &bus.action_leases,
            &fired.action_id,
        )
        .await
        .expect("expired lease must permit recovery");
        assert_eq!(recovered.action_id, fired.action_id);
    }

    #[tokio::test]
    async fn scheduled_claim_release_checks_token_and_terminal_clear_invalidates_it() {
        let bus = ActionCompletionBus::new();
        let fired = scheduled_fire("act-release-claim");
        bus.retain_scheduled_fire(fired.clone()).await;
        assert!(
            claim_scheduled_fire(
                &bus.pending_scheduled_fires,
                &bus.action_leases,
                &fired.action_id,
            )
            .await
            .is_some()
        );

        assert!(
            !bus.release_action_lease(&fired.action_id, "act-wrong-token")
                .await
        );
        assert!(bus.has_action_lease(&fired.action_id).await);

        // Scheduled completion/cancellation uses this clear path. Once
        // terminal, both the lease and its recoverable fire are invalidated.
        bus.clear_scheduled_fire(&fired.action_id).await;
        assert!(!bus.has_scheduled_fire_claim(&fired.action_id).await);
        assert!(!bus.has_pending_scheduled_fire(&fired.action_id).await);
        assert!(bus.pending_scheduled_fire().await.is_none());
    }

    #[tokio::test]
    async fn background_completion_passes_through_without_scheduled_claim() {
        let bus = ActionCompletionBus::new();
        let mut receiver = bus.subscribe();
        let completion = BackgroundActionCompletion {
            action_id: "act-background".into(),
            action_result_id: "act-background".into(),
            session_id: Some("ses-background".into()),
            status: ActionStatus::Completed,
            status_json: serde_json::json!({"status": "completed"}),
        };
        bus.send(ActionCompletion::Background(completion.clone()))
            .unwrap();

        let received = receiver.recv_background().await.unwrap();
        let ActionCompletion::Background(received) = received else {
            panic!("background receiver must pass through background completions");
        };
        assert_eq!(received.action_result_id, completion.action_result_id);
        assert!(bus.action_leases.read().await.is_empty());
    }

    #[tokio::test]
    async fn lagged_receiver_continues_and_closed_receiver_returns_none() {
        let bus = ActionCompletionBus::new();
        let mut receiver = bus.subscribe();
        for index in 0..=ACTION_COMPLETION_CHANNEL_CAPACITY {
            bus.send(ActionCompletion::Background(BackgroundActionCompletion {
                action_id: format!("act-noise-{index}"),
                action_result_id: format!("act-noise-{index}"),
                session_id: None,
                status: ActionStatus::Completed,
                status_json: serde_json::json!({"status": "completed"}),
            }))
            .unwrap();
        }

        let received = receiver.recv().await.unwrap();
        let ActionCompletion::Background(received) = received else {
            panic!("lag recovery should return a retained event");
        };
        assert_eq!(received.action_id, "act-noise-1");

        drop(bus);
        while receiver.recv().await.is_some() {}
        assert!(receiver.recv().await.is_none());
    }
}
