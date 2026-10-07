//! FIFO dispatcher and run-lifecycle coordination.

use super::*;
use std::sync::Mutex as StdMutex;
use tokio::sync::Notify;
use tracing::Instrument;

fn session_run_error_reason(error: &anyhow::Error) -> String {
    crate::sqlite_storage_failure_message(error)
        .map(str::to_owned)
        .unwrap_or_else(|| error.to_string())
}

/// Explicit run admission state. A Tokio semaphore cannot safely represent a
/// limit that is lowered while all permits are held: permits returned by old
/// runs can make the later limit larger than configured. Tracking active runs
/// directly makes resize semantics exact.
pub(super) struct SessionRunAdmission {
    state: StdMutex<AdmissionState>,
    notify: Notify,
}

struct AdmissionState {
    limit: usize,
    active: usize,
}

pub(super) struct SessionRunPermit {
    admission: Arc<SessionRunAdmission>,
}

impl SessionRunAdmission {
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
    ) -> Option<SessionRunPermit> {
        loop {
            // Register before checking the state so a release/resize cannot
            // notify between the check and awaiting the notification.
            let notified = self.notify.notified();
            if cancellation.is_cancelled() {
                return None;
            }
            if self.try_take() {
                return Some(SessionRunPermit {
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
    pub(super) fn try_acquire(self: &Arc<Self>) -> Option<SessionRunPermit> {
        self.try_take().then(|| SessionRunPermit {
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

impl Drop for SessionRunPermit {
    fn drop(&mut self) {
        self.admission.release();
    }
}

/// A direct (non-dispatcher) run owns an admission permit for its lifetime.
/// Dispatcher-owned runs never create this value because their actor is
/// already marked running.
pub(crate) struct DirectSessionRunLease {
    pub(crate) actor: actor::SessionActorHandle,
    executor: Arc<super::SessionSupervisor>,
    session_id: String,
    lease_id: usize,
    permit: Option<SessionRunPermit>,
    finished: bool,
}

impl DirectSessionRunLease {
    pub(crate) async fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.executor
            .end_direct_session_run(&self.session_id, &self.actor)
            .await;
        self.finished = true;
        self.executor
            .release_direct_session_run_lease(&self.session_id, self.lease_id);
        self.permit.take();
    }
}

impl Drop for DirectSessionRunLease {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.actor.run_cancellation_token().cancel();
        let executor = self.executor.clone();
        let actor = self.actor.clone();
        let session_id = self.session_id.clone();
        let lease_id = self.lease_id;
        let permit = self.permit.take();
        tokio::spawn(async move {
            actor.await_react_loop_finished().await;
            executor
                .end_cancelled_direct_session_run(&session_id, &actor)
                .await;
            executor.release_direct_session_run_lease(&session_id, lease_id);
            drop(permit);
        });
    }
}

struct DirectSessionRunAdmissionWaiterGuard {
    executor: Arc<super::SessionSupervisor>,
    session_id: String,
    waiter_id: Option<usize>,
}

impl DirectSessionRunAdmissionWaiterGuard {
    async fn unregister(&mut self) {
        if let Some(waiter_id) = self.waiter_id {
            self.executor
                .unregister_direct_session_run_admission_waiter(&self.session_id, waiter_id)
                .await;
            self.waiter_id = None;
        }
    }
}

impl Drop for DirectSessionRunAdmissionWaiterGuard {
    fn drop(&mut self) {
        let Some(waiter_id) = self.waiter_id.take() else {
            return;
        };
        let executor = self.executor.clone();
        let session_id = self.session_id.clone();
        tokio::spawn(async move {
            executor
                .unregister_direct_session_run_admission_waiter(&session_id, waiter_id)
                .await;
        });
    }
}

impl SessionSupervisor {
    pub(super) fn wake_dispatcher(&self) {
        self.dispatch_tx.send_modify(|counter| *counter += 1);
    }

    fn direct_session_run_lease_active(&self, session_id: &str) -> bool {
        self.direct_session_run_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(session_id)
    }

    fn reserve_direct_session_run_lease(&self, session_id: &str) -> Option<usize> {
        let mut leases = self
            .direct_session_run_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if leases.contains_key(session_id) {
            return None;
        }
        let lease_id = self
            .direct_session_run_lease_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        leases.insert(session_id.to_string(), lease_id);
        Some(lease_id)
    }

    fn release_direct_session_run_lease(&self, session_id: &str, lease_id: usize) {
        let released = {
            let mut leases = self
                .direct_session_run_leases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if leases.get(session_id) == Some(&lease_id) {
                leases.remove(session_id);
                true
            } else {
                false
            }
        };
        if released {
            self.wake_dispatcher();
        }
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

    pub fn start_dispatcher(self: Arc<Self>, handler: SessionRunHandler) {
        self.start_dispatcher_with_cancellation(handler, CancellationToken::new());
    }

    pub fn start_dispatcher_without_recovery(self: Arc<Self>, handler: SessionRunHandler) {
        self.start_dispatcher_without_recovery_with_cancellation(handler, CancellationToken::new());
    }

    pub fn start_dispatcher_with_cancellation(
        self: Arc<Self>,
        handler: SessionRunHandler,
        cancellation: CancellationToken,
    ) {
        self.start_dispatcher_inner(SessionRunEngine::new(handler), true, cancellation);
    }

    pub fn start_dispatcher_without_recovery_with_cancellation(
        self: Arc<Self>,
        handler: SessionRunHandler,
        cancellation: CancellationToken,
    ) {
        self.start_dispatcher_inner(SessionRunEngine::new(handler), false, cancellation);
    }

    fn start_dispatcher_inner(
        self: Arc<Self>,
        engine: SessionRunEngine,
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
        let retry_rx = self
            .terminal_cleanup_retry_rx
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(mut retry_rx) = retry_rx else {
            tracing::error!("session dispatcher terminal cleanup retry receiver is unavailable");
            return;
        };
        tokio::spawn(async move {
            if recover_pending {
                match self
                    .recover_pending_sessions_with_retry(&cancellation, 0)
                    .await
                {
                    Some(count) if count > 0 => {
                        tracing::info!(count, "reloaded pending sessions")
                    }
                    Some(_) => {}
                    None => return,
                }
            }
            let mut wake_rx = self.subscribe_dispatch();
            loop {
                let permit = tokio::select! {
                    _ = cancellation.cancelled() => return,
                    retry_id = retry_rx.recv() => {
                        let Some(session_id) = retry_id else {
                            return;
                        };
                        if let Some(retry) = self.terminal_cleanup_retry_queue.take(&session_id) {
                            self.retry_terminal_cleanup(&session_id, retry).await;
                        }
                        continue;
                    }
                    permit = self.admission.acquire(&cancellation) => match permit {
                        Some(permit) => permit,
                        None => return,
                    },
                };
                let Some(session_id) = self.try_claim_pending().await else {
                    drop(permit);
                    tokio::select! {
                        _ = cancellation.cancelled() => return,
                        retry_id = retry_rx.recv() => {
                            let Some(session_id) = retry_id else {
                                return;
                            };
                            if let Some(retry) = self.terminal_cleanup_retry_queue.take(&session_id) {
                                self.retry_terminal_cleanup(&session_id, retry).await;
                            }
                        }
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
                    let result = async {
                        let actor = supervisor.actor_for(&session_id).await.ok_or_else(|| {
                            anyhow::anyhow!("session actor disappeared before run")
                        })?;
                        actor.run(runner).await
                    }
                    .instrument(span)
                    .await;
                    if let Err(error) = result {
                        let reason = session_run_error_reason(&error);
                        // ReAct can already have conditionally moved this run
                        // to Error before returning. The supervisor serializes
                        // confirmation and typed event publication with end;
                        // a late error after closing is suppressed.
                        let marked_error = match supervisor
                            .commit_dispatcher_run_error(&session_id, reason.clone())
                            .await
                        {
                            Ok(changed) => changed,
                            Err(status_error) => {
                                tracing::error!(
                                    session_id = %session_id,
                                    error = %status_error,
                                    "failed to persist session error status"
                                );
                                false
                            }
                        };
                        if marked_error {
                            let failure =
                                haven_memory::SessionStore::sqlite_storage_write_failure(&error);
                            if let Some(failure) = failure {
                                tracing::error!(
                                    session_id = %session_id,
                                    ?failure,
                                    "session run failed while writing SQLite storage"
                                );
                            } else {
                                tracing::error!(
                                    session_id = %session_id,
                                    error = %reason,
                                    "session run failed"
                                );
                            }
                            supervisor
                                .fail_pending_tool_run_steps(
                                    &session_id,
                                    &format!("Session ended before tool finished: {reason}"),
                                )
                                .await;
                        } else {
                            tracing::debug!(
                                session_id = %session_id,
                                "preserving session lifecycle status after run exit"
                            );
                        }
                    }
                    supervisor.unmark_running(&session_id).await;
                    drop(permit);
                });
            }
        });
    }

    pub(crate) async fn try_claim_pending(&self) -> Option<String> {
        // Claiming is a lifecycle admission, not just a queue operation. Keep
        // the registry gate across the queue scan + closing check + actor
        // claim so delete/clear cannot remove an actor between its check and
        // run-bit transition. A direct-run lease may outlive the actor's run
        // bit while exit reconciliation finishes, so rotate those entries to
        // the back instead of blocking other sessions that could run now.
        let _lifecycle = self.lifecycle_guard().await;
        if self.ensure_lifecycle_open().is_err() {
            return None;
        }
        let scan_count = self.pending_queue.lock().await.len();
        for _ in 0..scan_count {
            let session_id = self.pending_queue.lock().await.pop_front()?;
            if self.is_session_closing(&session_id) {
                continue;
            }
            if self.direct_session_run_lease_active(&session_id) {
                self.enqueue_pending(&session_id).await;
                continue;
            }
            let Some(actor) = self.actor_for(&session_id).await else {
                continue;
            };
            match actor.claim_session_run().await {
                Ok(claim) if claim.accepted => return Some(session_id),
                Ok(_) => continue,
                Err(error) => {
                    tracing::error!(session_id = %session_id, %error, "failed to claim session");
                    self.enqueue_pending(&session_id).await;
                    return None;
                }
            }
        }
        None
    }

    async fn unmark_running(&self, session_id: &str) {
        let Some(actor) = self.actor_for(session_id).await else {
            return;
        };
        let Some(()) = actor.finish_run().await else {
            return;
        };
        self.reconcile_run_exit(session_id, &actor).await;
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

    pub async fn running_tool_runs_list(&self) -> Vec<String> {
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

    pub(crate) async fn begin_direct_session_run(
        self: &Arc<Self>,
        session_id: &str,
    ) -> Option<DirectSessionRunLease> {
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
            let current_actor = self.actor_for(session_id).await;
            if self.ensure_lifecycle_open().is_err()
                || self.is_session_closing(session_id)
                || self.direct_session_run_lease_active(session_id)
                || !current_actor
                    .as_ref()
                    .is_some_and(|current| current.same_instance(&actor))
                || actor.is_running().await
            {
                return None;
            }
            waiter_id = self
                .register_direct_session_run_admission_waiter(session_id, waiter_cancel.clone())
                .await;
        }
        let mut waiter = DirectSessionRunAdmissionWaiterGuard {
            executor: self.clone(),
            session_id: session_id.to_string(),
            waiter_id: Some(waiter_id),
        };
        let permit = self.admission.acquire(&waiter_cancel).await;
        waiter.unregister().await;
        let permit = permit?;
        // The actor may have been quiesced and removed while waiting for a
        // permit. Re-check the registry under the same lifecycle gate used by
        // deletion/loading before changing the actor's run bit; otherwise a
        // direct resume could start an orphaned actor after its DB row was
        // deleted.
        let _lifecycle = self.lifecycle_guard().await;
        let current_actor = self.actor_for(session_id).await;
        if self.ensure_lifecycle_open().is_err()
            || self.is_session_closing(session_id)
            || self.direct_session_run_lease_active(session_id)
            || !current_actor
                .as_ref()
                .is_some_and(|current| current.same_instance(&actor))
            || actor.is_running().await
        {
            drop(permit);
            return None;
        }
        // Public direct runs can intentionally resume a paused actor, but the
        // run must become Running before it starts. That gives error handling
        // one unambiguous expected state and keeps an end racing this admission
        // serialized by the lifecycle gate above.
        let status = actor.snapshot().await?.status;
        if matches!(status, SessionStatus::Completed | SessionStatus::Error) {
            drop(permit);
            return None;
        }
        let Some(lease_id) = self.reserve_direct_session_run_lease(session_id) else {
            drop(permit);
            return None;
        };
        // Own the permit and a cancellation fallback before any await can
        // commit Running or set the actor's run bit. If this admission future
        // is dropped after either commit but before returning to its caller,
        // the lease cancels and reconciles the exact actor instance.
        let lease = DirectSessionRunLease {
            actor: actor.clone(),
            executor: self.clone(),
            session_id: session_id.to_string(),
            lease_id,
            permit: Some(permit),
            finished: false,
        };
        match status {
            SessionStatus::Running => {}
            expected @ (SessionStatus::Pending | SessionStatus::Paused) => {
                match self
                    .update_session_status_if(session_id, expected, SessionStatus::Running)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return None,
                    Err(error) => {
                        tracing::warn!(
                            session_id,
                            %error,
                            "failed to persist direct-run admission status"
                        );
                        return None;
                    }
                }
            }
            SessionStatus::Completed | SessionStatus::Error => {
                unreachable!("terminal direct-run status was rejected before creating its lease")
            }
        }
        if actor.begin_direct_session_run().await {
            Some(lease)
        } else {
            None
        }
    }

    pub(crate) async fn end_direct_session_run(
        &self,
        session_id: &str,
        actor: &actor::SessionActorHandle,
    ) {
        let finished = {
            // Finish and direct-run admission share the lifecycle gate. If the
            // run bit were released first through the actor side channel, a
            // second direct run could claim this actor before the old FinishRun
            // mailbox command and have its bit cleared by stale cleanup.
            let _lifecycle = self.lifecycle_guard().await;
            actor.release_run_now();
            actor.finish_run().await.is_some()
        };
        if finished {
            self.reconcile_run_exit(session_id, actor).await;
        }
    }

    async fn end_cancelled_direct_session_run(
        &self,
        session_id: &str,
        expected_actor: &actor::SessionActorHandle,
    ) {
        let finished = {
            let _lifecycle = self.lifecycle_guard().await;
            let Some(actor) = self.actor_for(session_id).await else {
                return;
            };
            if !actor.same_instance(expected_actor) {
                return;
            }
            actor.release_run_now();
            let finished = actor.finish_run().await.is_some();
            if actor
                .snapshot()
                .await
                .is_some_and(|snapshot| snapshot.status == SessionStatus::Running)
            {
                match self
                    .update_session_status_if(
                        session_id,
                        SessionStatus::Running,
                        SessionStatus::Paused,
                    )
                    .await
                {
                    Ok(true) => self.emit_event(SessionSupervisorEvent::SessionRunPaused {
                        session_id: session_id.to_string(),
                    }),
                    Ok(false) => {}
                    Err(error) => tracing::warn!(
                        session_id,
                        %error,
                        "failed to pause a cancelled direct run"
                    ),
                }
            }
            finished
        };
        if finished {
            self.reconcile_run_exit(session_id, expected_actor).await;
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
            .map(|actor| actor.run_cancellation_token())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod storage_error_tests {
    use super::session_run_error_reason;
    use haven_memory::{Database, SessionCommitted, SessionStore};
    use std::sync::Arc;

    #[test]
    fn full_database_error_gives_recovery_guidance_without_database_details() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("storage-full-feedback").unwrap();
        let store = SessionStore::new(db.clone());
        let page_count: i64 = db
            .conn()
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        db.conn()
            .pragma_update(None, "max_page_count", page_count)
            .unwrap();

        let payload = format!(
            r#"{{"type":"transcript","text":"{}"}}"#,
            "x".repeat(1024 * 1024)
        );
        let mut committed = SessionCommitted::transcript(payload, 1, 1);
        committed.project_assistant_message(haven_common::types::new_id("msg"), "saved", None);
        let error = store
            .commit_transcript(&session.id, &committed)
            .unwrap_err();

        let reason = session_run_error_reason(&error);
        assert!(reason.contains("数据库所在磁盘空间不足"));
        assert!(reason.contains("释放该磁盘空间"));
        assert!(reason.contains("继续生成"));
        assert!(reason.contains("删除会话不保证缩小 SQLite 文件"));
        assert!(!reason.contains("storage-full-feedback"));
    }
}
