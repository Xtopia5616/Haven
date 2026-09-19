//! FIFO dispatcher and run-lifecycle coordination.

use super::*;
use std::sync::Mutex as StdMutex;
use tokio::sync::Notify;
use tracing::Instrument;

/// Explicit run admission state. A Tokio semaphore cannot safely represent a
/// limit that is lowered while all permits are held: permits returned by old
/// runs can make the later limit larger than configured. Tracking active runs
/// directly makes resize semantics exact.
pub(super) struct RunAdmission {
    state: StdMutex<AdmissionState>,
    notify: Notify,
}

struct AdmissionState {
    limit: usize,
    active: usize,
}

pub(super) struct RunPermit {
    admission: Arc<RunAdmission>,
}

impl RunAdmission {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            state: StdMutex::new(AdmissionState {
                limit: limit.max(1),
                active: 0,
            }),
            notify: Notify::new(),
        }
    }

    pub(super) async fn acquire(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
    ) -> Option<RunPermit> {
        loop {
            // Register before checking the state so a release/resize cannot
            // notify between the check and awaiting the notification.
            let notified = self.notify.notified();
            if cancellation.is_cancelled() {
                return None;
            }
            if self.try_take() {
                return Some(RunPermit {
                    admission: self.clone(),
                });
            }
            tokio::select! {
                _ = cancellation.cancelled() => return None,
                _ = notified => {}
            }
        }
    }

    #[cfg(test)]
    pub(super) fn try_acquire(self: &Arc<Self>) -> Option<RunPermit> {
        self.try_take().then(|| RunPermit {
            admission: self.clone(),
        })
    }

    fn try_take(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.active >= state.limit {
            return false;
        }
        state.active += 1;
        true
    }

    pub(super) fn set_limit(&self, limit: usize) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .limit = limit.max(1);
        self.notify.notify_waiters();
    }

    pub(super) fn limit(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .limit
    }

    fn release(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active = state.active.saturating_sub(1);
        drop(state);
        self.notify.notify_one();
    }
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        self.admission.release();
    }
}

/// A direct (non-dispatcher) run owns an admission permit for its lifetime.
/// Dispatcher-owned runs never create this value because their actor is
/// already marked running.
pub(crate) struct DirectRunLease {
    pub(crate) actor: actor::SessionActorHandle,
    _permit: RunPermit,
}

impl SessionSupervisor {
    pub(super) fn wake_dispatcher(&self) {
        self.dispatch_tx.send_modify(|counter| *counter += 1);
    }

    pub(super) async fn enqueue_pending(&self, session_id: &str) {
        let mut queue = self.pending_queue.lock().await;
        if !queue.iter().any(|id| id == session_id) {
            queue.push_back(session_id.to_string());
        }
    }

    pub(super) async fn dequeue_pending(&self, session_id: &str) {
        self.pending_queue
            .lock()
            .await
            .retain(|id| id != session_id);
    }

    pub fn subscribe_dispatch(&self) -> watch::Receiver<u64> {
        self.dispatch_tx.subscribe()
    }

    pub fn set_max_concurrent(&self, new_max: usize) {
        self.admission.set_limit(new_max);
    }

    pub fn start_dispatcher(self: Arc<Self>, handler: RunHandler) {
        self.start_dispatcher_with_cancellation(handler, CancellationToken::new());
    }

    pub fn start_dispatcher_without_recovery(self: Arc<Self>, handler: RunHandler) {
        self.start_dispatcher_without_recovery_with_cancellation(handler, CancellationToken::new());
    }

    pub fn start_dispatcher_with_cancellation(
        self: Arc<Self>,
        handler: RunHandler,
        cancellation: CancellationToken,
    ) {
        self.start_dispatcher_inner(RunEngine::new(handler), true, cancellation);
    }

    pub fn start_dispatcher_without_recovery_with_cancellation(
        self: Arc<Self>,
        handler: RunHandler,
        cancellation: CancellationToken,
    ) {
        self.start_dispatcher_inner(RunEngine::new(handler), false, cancellation);
    }

    fn start_dispatcher_inner(
        self: Arc<Self>,
        engine: RunEngine,
        recover_pending: bool,
        cancellation: CancellationToken,
    ) {
        if self
            .dispatcher_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            tracing::warn!("session dispatcher start ignored: already running");
            return;
        }
        tokio::spawn(async move {
            if recover_pending {
                match self.load_pending_sessions().await {
                    Ok(count) if count > 0 => tracing::info!(count, "reloaded pending sessions"),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(%error, "pending session recovery failed");
                        return;
                    }
                }
            }
            let mut wake_rx = self.subscribe_dispatch();
            loop {
                let Some(permit) = self.admission.acquire(&cancellation).await else {
                    return;
                };
                let Some(session_id) = self.try_claim_pending().await else {
                    drop(permit);
                    tokio::select! {
                        _ = cancellation.cancelled() => return,
                        result = wake_rx.changed() => {
                            if result.is_err() {
                                return;
                            }
                        }
                    }
                    continue;
                };
                let supervisor = self.clone();
                let runner = engine.clone();
                let span = tracing::info_span!("run_session", session_id = %session_id);
                tokio::spawn(async move {
                    let result =
                        tokio::spawn(runner.run(session_id.clone()).instrument(span)).await;
                    if let Err(reason) = handler_error(result) {
                        tracing::error!(session_id = %session_id, %reason, "session run failed");
                        let _ = supervisor
                            .update_session_status(&session_id, SessionStatus::Error)
                            .await;
                        supervisor
                            .fail_pending_action_steps(
                                &session_id,
                                &format!("Session ended before tool finished: {reason}"),
                            )
                            .await;
                        supervisor.emit_event(SessionEvent::SessionError {
                            session_id: session_id.clone(),
                            reason,
                        });
                    }
                    supervisor.unmark_running(&session_id).await;
                    drop(permit);
                });
            }
        });
    }

    pub(crate) async fn try_claim_pending(&self) -> Option<String> {
        if self
            .lifecycle_blocked
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return None;
        }
        loop {
            let session_id = self.pending_queue.lock().await.pop_front()?;
            if self.is_session_closing(&session_id).await {
                continue;
            }
            let Some(actor) = self.actor_for(&session_id).await else {
                continue;
            };
            match actor.claim_run().await {
                Ok(claim) if claim.accepted => return Some(session_id),
                Ok(_) => continue,
                Err(error) => {
                    tracing::error!(session_id = %session_id, %error, "failed to claim session");
                    self.enqueue_pending(&session_id).await;
                    return None;
                }
            }
        }
    }

    async fn unmark_running(&self, session_id: &str) {
        let Some(actor) = self.actor_for(session_id).await else {
            return;
        };
        let Some(run) = actor.finish_run().await else {
            return;
        };
        if run.terminal {
            self.dequeue_pending(session_id).await;
            self.cleanup_session_maps(session_id).await;
            self.remove_actor(session_id).await;
        } else if run.pending {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
    }

    pub async fn running_count(&self) -> usize {
        let actors = self
            .actors
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut count = 0;
        for actor in actors {
            if actor.is_running().await {
                count += 1;
            }
        }
        count
    }

    pub fn max_concurrent(&self) -> usize {
        self.admission.limit()
    }

    pub async fn running_actions_list(&self) -> Vec<String> {
        let actors = self
            .actors
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut ids = Vec::new();
        for actor in actors {
            if actor.is_running().await {
                ids.push(actor.id.clone());
            }
        }
        ids
    }

    pub async fn is_run_in_flight(&self, session_id: &str) -> bool {
        match self.actor_for(session_id).await {
            Some(actor) => actor.is_running().await,
            None => false,
        }
    }

    pub(crate) async fn begin_direct_run(&self, session_id: &str) -> Option<DirectRunLease> {
        let actor = self.actor_for(session_id).await?;
        // A dispatcher-owned run already holds admission and the actor run
        // bit. Direct callers must not acquire a second permit or gate it.
        if actor.is_running().await {
            return None;
        }
        let waiter_cancel = CancellationToken::new();
        let waiter_id;
        {
            // Register the cancellation before waiting for admission. Delete
            // linearizes its closing marker under this same gate, so it can
            // always wake a direct resume that is blocked on capacity.
            let _lifecycle = self.lifecycle_guard().await;
            if self.ensure_lifecycle_open().is_err()
                || self.is_session_closing(session_id).await
                || self.actor_for(session_id).await.is_none()
                || actor.is_running().await
            {
                return None;
            }
            waiter_id = self
                .register_direct_waiter(session_id, waiter_cancel.clone())
                .await;
        }
        let permit = self.admission.acquire(&waiter_cancel).await;
        self.unregister_direct_waiter(session_id, waiter_id).await;
        let permit = permit?;
        // The actor may have been quiesced and removed while waiting for a
        // permit. Re-check the registry under the same lifecycle gate used by
        // deletion/loading before changing the actor's run bit; otherwise a
        // direct resume could start an orphaned actor after its DB row was
        // deleted.
        let _lifecycle = self.lifecycle_guard().await;
        if self.ensure_lifecycle_open().is_err()
            || self.is_session_closing(session_id).await
            || self.actor_for(session_id).await.is_none()
            || actor.is_running().await
        {
            drop(permit);
            return None;
        }
        if actor.begin_direct_run().await {
            Some(DirectRunLease {
                actor,
                _permit: permit,
            })
        } else {
            drop(permit);
            None
        }
    }

    pub async fn end_direct_run(&self, session_id: &str) {
        let Some(actor) = self.actor_for(session_id).await else {
            return;
        };
        if let Some(run) = actor.finish_run().await
            && run.pending
        {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
    }

    pub async fn await_run_finished(&self, session_id: &str) -> anyhow::Result<()> {
        let Some(actor) = self.actor_for(session_id).await else {
            return Ok(());
        };
        let mut state = actor.run_state();
        tokio::time::timeout(RUN_EXIT_WAIT_TIMEOUT, async {
            loop {
                // The actor is the source of truth. The watch channel is only
                // the wake-up edge; checking the actor after every wake also
                // covers the small window where a waiter subscribes before
                // the claim transition is published to the watch receiver.
                if !actor.is_running().await {
                    break;
                }
                if state.changed().await.is_err() {
                    // The handle normally keeps the sender alive. If the
                    // actor has stopped, one final mailbox read decides
                    // whether the run really ended.
                    if !actor.is_running().await {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "session '{}' did not exit within {:?}; lifecycle mutation aborted",
                session_id,
                RUN_EXIT_WAIT_TIMEOUT
            )
        })
    }

    pub async fn cancellation_token(&self, session_id: &str) -> CancellationToken {
        self.actor_for(session_id)
            .await
            .map(|actor| actor.cancel())
            .unwrap_or_default()
    }
}

fn handler_error(
    result: Result<Result<(), anyhow::Error>, tokio::task::JoinError>,
) -> Result<(), String> {
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(format!("handler failed: {error}")),
        Err(error) if error.is_panic() => Err(format!("handler panicked: {error}")),
        Err(error) => Err(format!("handler aborted: {error}")),
    }
}
