//! FIFO dispatcher and run-lifecycle coordination.

use super::*;
use tracing::Instrument;

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
        let new_max = new_max.max(1);
        let current = self.max_concurrent.load(Ordering::Relaxed);
        if current == new_max {
            return;
        }
        if new_max > current {
            self.semaphore.add_permits(new_max - current);
        } else {
            for _ in 0..current - new_max {
                if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
                    permit.forget();
                } else {
                    break;
                }
            }
        }
        self.max_concurrent.store(new_max, Ordering::Relaxed);
    }

    pub fn start_dispatcher(self: Arc<Self>, handler: RunHandler) {
        self.start_dispatcher_inner(RunEngine::new(handler), true);
    }

    pub fn start_dispatcher_without_recovery(self: Arc<Self>, handler: RunHandler) {
        self.start_dispatcher_inner(RunEngine::new(handler), false);
    }

    fn start_dispatcher_inner(self: Arc<Self>, engine: RunEngine, recover_pending: bool) {
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
                let permit = match self.semaphore.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => return,
                };
                let Some(session_id) = self.try_claim_pending().await else {
                    drop(permit);
                    let _ = wake_rx.changed().await;
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
        loop {
            let session_id = self.pending_queue.lock().await.pop_front()?;
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
        self.max_concurrent.load(Ordering::Relaxed)
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

    pub(crate) async fn begin_direct_run(
        &self,
        session_id: &str,
    ) -> Option<actor::SessionActorHandle> {
        match self.actor_for(session_id).await {
            Some(actor) if actor.begin_direct_run().await => Some(actor),
            _ => None,
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

    pub async fn await_run_finished(&self, session_id: &str) {
        let Some(actor) = self.actor_for(session_id).await else {
            return;
        };
        let mut state = actor.run_state();
        let _ = tokio::time::timeout(RUN_EXIT_WAIT_TIMEOUT, async {
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
        .await;
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
