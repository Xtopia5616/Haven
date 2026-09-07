//! FIFO dispatcher, permit map, run-exit gates, and cancellation-token registration.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract; behavior unchanged).

use super::*;
use tracing::Instrument;

impl SessionExecutor {
    /// Bump the dispatcher wake counter. Level-triggered: a bump that lands
    /// between a failed claim and the dispatcher's wait resolves `changed()`
    /// immediately, so no transition is ever lost.
    pub(super) fn wake_dispatcher(&self) {
        self.dispatch_tx.send_modify(|c| *c += 1);
    }

    /// Enqueue a session id at the tail of the FIFO dispatch queue (idempotent:
    /// a session already queued is not duplicated).
    pub(super) async fn enqueue_pending(&self, session_id: &str) {
        let mut q = self.pending_queue.lock().await;
        if !q.iter().any(|t| t == session_id) {
            q.push_back(session_id.to_string());
        }
    }

    /// Remove a session id from the FIFO dispatch queue (no-op when absent).
    pub(super) async fn dequeue_pending(&self, session_id: &str) {
        let mut q = self.pending_queue.lock().await;
        q.retain(|t| t != session_id);
    }

    /// Subscribe to dispatch wake signals (a `watch` receiver on the wake
    /// counter). The receiver resolves as soon as the counter moved past the
    /// version it has seen, so it must be created before the first claim.
    pub fn subscribe_dispatch(&self) -> watch::Receiver<u64> {
        self.dispatch_tx.subscribe()
    }

    /// Adjust the session concurrency ceiling at runtime (settings save). The
    /// semaphore permit count is updated by the delta:
    /// - Raising: `add_permits` grows the cap immediately; queued sessions start
    ///   as soon as a permit is free.
    /// - Lowering: unused permits are reclaimed best-effort. Permits held by
    ///   in-flight sessions cannot be revoked (they finish and release naturally),
    ///   so the effective concurrency may stay above the new target until the
    ///   current sessions complete — never forcibly cancelled.
    pub fn set_max_concurrent(&self, new_max: usize) {
        let new_max = new_max.max(1);
        let cur = self
            .max_concurrent
            .load(std::sync::atomic::Ordering::Relaxed);
        if new_max == cur {
            return;
        }
        if new_max > cur {
            self.semaphore.add_permits(new_max - cur);
        } else {
            let mut reclaimed = 0usize;
            while reclaimed < cur - new_max {
                match self.semaphore.clone().try_acquire_owned() {
                    Ok(p) => {
                        // `forget` (not `drop`): dropping a permit returns it
                        // to the semaphore, which would make the reclaim loop
                        // a no-op and let the ceiling drift (a later raise
                        // would then overshoot by the stale delta).
                        p.forget();
                        reclaimed += 1;
                    }
                    Err(_) => break,
                }
            }
            if reclaimed < cur - new_max {
                tracing::warn!(
                    "set_max_concurrent: reclaimed {}/{} permits; in-flight sessions keep \
                     the effective concurrency above the new target until they finish",
                    reclaimed,
                    cur - new_max
                );
            }
        }
        self.max_concurrent
            .store(new_max, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("session concurrency ceiling: {} -> {}", cur, new_max);
    }

    /// Spawn the background dispatcher. Whenever a semaphore permit is free
    /// and a `Pending` session exists, the dispatcher calls `handler(session_id)`.
    /// The handler must perform the ReAct loop and finalize the session status.
    pub fn start_dispatcher(self: Arc<Self>, handler: RunHandler) {
        self.start_dispatcher_inner(handler, true);
    }

    /// Spawn the dispatcher without reloading durable pending sessions yet.
    ///
    /// This is used during desktop cold start: a new session can run while the
    /// MCP/Skills catalog is still warming, but sessions left pending by a
    /// previous process are deliberately reloaded only after that catalog is
    /// ready. That keeps startup recovery's tool visibility deterministic
    /// without putting a multi-second gate in front of a fresh conversation.
    pub fn start_dispatcher_without_recovery(self: Arc<Self>, handler: RunHandler) {
        self.start_dispatcher_inner(handler, false);
    }

    fn start_dispatcher_inner(self: Arc<Self>, handler: RunHandler, recover_pending: bool) {
        let exec = self.clone();
        tokio::spawn(async move {
            // Pick up sessions that were still Pending when the app stopped so
            // queued work survives a restart instead of being stranded in
            // the DB (the in-memory working set is empty on a fresh start).
            if recover_pending {
                let reloaded = match exec.load_pending_sessions().await {
                    Ok(count) => count,
                    Err(error) => {
                        tracing::error!(
                            error = %error,
                            "dispatcher startup aborted: pending sessions could not be loaded"
                        );
                        return;
                    }
                };
                if reloaded > 0 {
                    tracing::info!(
                        "dispatcher reloaded {} pending session(s) from previous run",
                        reloaded
                    );
                }
            } else {
                tracing::debug!(
                    "dispatcher started without pending-session recovery; recovery is deferred"
                );
            }
            // Subscribe BEFORE the first claim so a Pending transition that
            // lands between a failed claim and the wait below is never lost:
            // `changed()` resolves immediately when the counter moved.
            let mut dispatch_rx = exec.subscribe_dispatch();
            let mut log_counter: u64 = 0;
            loop {
                log_counter += 1;
                if log_counter.is_multiple_of(DISPATCH_LOG_INTERVAL) {
                    tracing::debug!("dispatcher heartbeat (iter {})", log_counter);
                }
                let permit = match exec.semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::error!("session semaphore closed");
                        return;
                    }
                };

                let session_id = exec.try_claim_pending().await;
                let Some(session_id) = session_id else {
                    drop(permit);
                    // Wait for the next Pending transition, then re-claim.
                    let _ = dispatch_rx.changed().await;
                    continue;
                };

                // Register the permit so pause/cancel can release it.
                {
                    let mut permits = exec.session_permits.lock().await;
                    permits.insert(session_id.clone(), permit);
                }
                // Create a cancellation token for this session. Use entry() so a
                // token already created (and possibly cancelled) by end_session
                // during the claim window is never clobbered with a fresh one.
                {
                    let mut cancels = exec.session_cancellations.lock().await;
                    cancels
                        .entry(session_id.clone())
                        .or_insert_with(CancellationToken::new);
                }

                let exec_inner = exec.clone();
                let handler_inner = handler.clone();
                tracing::info!(session_id = %session_id, "dispatcher spawning handler");
                // Run the handler on a nested session so a panic in the ReAct
                // loop is contained: the JoinHandle turns it into an Err and
                // the cleanup below still runs. Without this, a panicked
                // handler would skip the Error marking and unmark_running,
                // leaving the session stuck in Running (memory + DB) forever.
                //
                // The handler runs inside a ses-level span so every log line
                // emitted by the ReAct loop (agent, compactor, title,
                // inference) carries the session_id even when the call site does
                // not name it — parallel sessions stay distinguishable in logs.
                let session_span = tracing::info_span!("run_session", session_id = %session_id);
                tokio::spawn(async move {
                    let result =
                        tokio::spawn(handler_inner(session_id.clone()).instrument(session_span))
                            .await;
                    let failed = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(format!("handler failed: {}", e)),
                        Err(join_err) if join_err.is_panic() => {
                            Some(format!("handler panicked: {}", join_err))
                        }
                        Err(join_err) => Some(format!("handler aborted: {}", join_err)),
                    };
                    if let Some(reason) = failed {
                        tracing::error!(session_id = %session_id, "dispatcher session {} {}", session_id, reason);
                        if let Err(error) = exec_inner
                            .update_session_status(&session_id, SessionStatus::Error)
                            .await
                        {
                            tracing::error!(
                                session_id = %session_id,
                                error = %error,
                                "dispatcher failed to persist terminal error status"
                            );
                        }
                        // The ReAct loop errored out: kill any background actions
                        // the session spawned so their children cannot leak.
                        exec_inner.cancel_session_actions(&session_id).await;
                        // Panic/abort can leave Action-emit `pending` step rows
                        // with no observation — finalize them as unknown so resume does
                        // not rebuild blank tool badges.
                        exec_inner
                            .fail_pending_action_steps(
                                &session_id,
                                &format!("Session ended before tool finished: {reason}"),
                            )
                            .await;
                        // The ReAct loop never emitted a terminal event for
                        // this failure (panic bypasses its error path), so
                        // surface it through the wired callback — otherwise
                        // the UI keeps the session in its busy set and the chip
                        // would stay stuck on "waiting" forever.
                        if let Some(cb) = exec_inner.on_session_error.snap() {
                            cb(session_id.clone(), reason);
                        }
                    }
                    exec_inner.unmark_running(&session_id).await;
                });
            }
        });
    }

    /// Claim the oldest `Pending` session from the FIFO dispatch queue, flip it
    /// to `Running` (memory + DB) and insert it into the running set. Returns
    /// the claimed session id, or `None` if nothing is dispatchable.
    ///
    /// Stale queue entries are skipped without re-queuing: a session whose
    /// status moved away from Pending (paused, cancelled, ended) or whose
    /// handler is still in `running_sessions` (claim-window race: only the
    /// dispatcher inserts into that set, and a re-claim would be a
    /// double-dispatch) must not be started again. Pause is exit-based
    /// (Phase 2 / C1): the handler does not park on a status watcher.
    ///
    /// The status flip happens under the session's own entry lock (never under
    /// the map lock), so a slow transition of another session cannot block the
    /// claim. The DB write precedes the memory flip: on a persistent DB
    /// failure the claim is aborted before memory and the running set diverge,
    /// keeping the memory/DB error policy consistent with `update_session_status`.
    /// The session is re-queued at the tail so it is not lost.
    pub(crate) async fn try_claim_pending(&self) -> Option<String> {
        loop {
            let session_id = {
                let mut q = self.pending_queue.lock().await;
                match q.pop_front() {
                    Some(id) => id,
                    None => return None,
                }
            };
            let entry = { self.sessions.lock().await.get(&session_id).cloned() };
            let Some(entry) = entry else {
                // Session removed between enqueue and claim (end_session /
                // remove_session / terminal cleanup): stale queue entry.
                continue;
            };
            let mut session = entry.lock().await;
            if session.status != SessionStatus::Pending {
                // Status moved (paused, ended, errored) while queued: no
                // longer dispatchable, and the transition path already
                // re-enqueued it if it became Pending again.
                continue;
            }
            // Deletion / history clearing marks the token cancelled before it
            // removes the working-set entry. A queued id can already have
            // been popped by this dispatcher, so check the tombstone here as
            // well as removing it from the queue.
            if self
                .session_cancellations
                .lock()
                .await
                .get(&session_id)
                .is_some_and(CancellationToken::is_cancelled)
            {
                continue;
            }
            // The `running_sessions` check prevents double-dispatch during the
            // claim→spawn window. Pause is exit-based (Phase 2 / C1): a paused
            // handler returns, `unmark_running` drops the set entry, and only
            // then can Pending be claimed again. Inserts come from this claim
            // path **or** `begin_direct_run` (direct `run_session_from_id`);
            // promotion to Running under the session lock happens before
            // either insert, so check-then-insert cannot double-dispatch.
            if self.running_sessions.lock().await.contains(&session_id) {
                continue;
            }
            if let Err(e) = Self::persist_status(&self.db, &session_id, "running").await {
                tracing::error!(
                    "try_claim_pending: DB persist failed for session {}; re-queuing it: {}",
                    session_id,
                    e
                );
                self.pending_queue.lock().await.push_back(session_id);
                return None;
            }
            session.status = SessionStatus::Running;
            session.updated_at = chrono::Utc::now().to_rfc3339();
            self.running_sessions
                .lock()
                .await
                .insert(session_id.clone());
            // Gate is live for the whole claim→spawn→exit lifetime so
            // rollback can await exit even during the claim→spawn window.
            let (tx, rx) = oneshot::channel();
            self.run_exit
                .lock()
                .await
                .insert(session_id.clone(), RunExitGate { tx, rx: Some(rx) });
            tracing::debug!("try_claim_pending: claimed session {}", session_id);
            return Some(session_id);
        }
    }

    /// Remove a session from the running set. Terminal status updates are
    /// performed by the handler / agent loop. Also removes terminal-status
    /// sessions from the in-memory list so `try_claim_pending` only counts
    /// active (Pending / Running) sessions.
    async fn unmark_running(&self, session_id: &str) {
        self.cleanup_session_maps(session_id).await;
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return;
        };
        let status = entry.lock().await.status.clone();
        if status == SessionStatus::Error || status == SessionStatus::Completed {
            tracing::debug!(
                "session {} unmark_running: {:?}, removing from list",
                session_id,
                status
            );
            self.dequeue_pending(session_id).await;
            self.sessions.lock().await.remove(session_id);
        } else {
            // The handler has exited (unmark_running runs after the handler
            // future completes). A session left Pending here is claimable again
            // — re-enqueue it, or it would strand forever: the FIFO claim
            // consumed its queue entry when it skipped it while the handler
            // was still alive, and no later Pending transition re-queues it.
            // The alive-handler case is safe: a session whose handler is truly
            // still running is claimed only after `running_sessions` re-check.
            if status == SessionStatus::Pending {
                self.enqueue_pending(session_id).await;
                // The dispatcher may be parked on its wake channel after a
                // failed claim; re-queueing alone would not wake it.
                self.wake_dispatcher();
            }
            tracing::debug!(
                "session {} unmark_running: {:?}, keeping in list",
                session_id,
                status
            );
        }
    }

    pub async fn running_count(&self) -> usize {
        self.running_sessions.lock().await.len()
    }

    /// Current `session.max_concurrent` ceiling (may temporarily be exceeded
    /// while in-flight sessions finish after a lowering).
    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Return a list of currently running session IDs.
    pub async fn running_actions_list(&self) -> Vec<String> {
        self.running_sessions.lock().await.iter().cloned().collect()
    }

    /// True while the dispatcher still holds the run slot for `session_id`
    /// (claim→spawn through `unmark_running`). Status may already be
    /// `Paused*` during pause-write unwind — R6 rollback/continue must join
    /// on this flag, not only on `SessionStatus::Running`.
    pub async fn is_run_in_flight(&self, session_id: &str) -> bool {
        self.running_sessions.lock().await.contains(session_id)
    }

    /// Register a run slot for a direct `run_session_from_id` caller (tests /
    /// continue without dispatcher claim) so R6 rollback can
    /// `await_run_finished` the same way as the claim→spawn path.
    ///
    /// Returns `true` when this call inserted the slot (caller must
    /// [`Self::end_direct_run`]). Returns `false` when the dispatcher already
    /// holds the id — do **not** end in that case (`unmark_running` owns it).
    pub async fn begin_direct_run(&self, session_id: &str) -> bool {
        let mut running = self.running_sessions.lock().await;
        if running.contains(session_id) {
            return false;
        }
        running.insert(session_id.to_string());
        drop(running);
        let (tx, rx) = oneshot::channel();
        self.run_exit
            .lock()
            .await
            .insert(session_id.to_string(), RunExitGate { tx, rx: Some(rx) });
        // Ensure a cancel token exists for rollback/end_session.
        let mut cancels = self.session_cancellations.lock().await;
        cancels
            .entry(session_id.to_string())
            .or_insert_with(CancellationToken::new);
        true
    }

    /// Release a direct-run slot previously inserted by [`Self::begin_direct_run`]
    /// (when that call returned `true`). Signals `run_exit` so rollback's
    /// `await_run_finished` unblocks.
    ///
    /// Mirrors [`Self::unmark_running`]'s Pending re-queue: ask with a
    /// pre-queued answer may flip to Pending while the handler is still
    /// alive; `try_claim_pending` can skip the id while the slot is held, so
    /// cleanup must re-enqueue + wake or the session strands forever.
    pub async fn end_direct_run(&self, session_id: &str) {
        self.cleanup_session_maps(session_id).await;
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return;
        };
        let status = entry.lock().await.status.clone();
        if status == SessionStatus::Pending {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
    }

    /// Wait until the dispatcher run handler for `session_id` has fully
    /// exited (running slot released via `unmark_running`). No-op when the
    /// session is not in an in-flight run. Prefer this over polling
    /// [`Self::running_actions_list`]: the oneshot is signaled exactly when
    /// cleanup runs, so rollback vs in-flight tools has a deterministic order.
    ///
    /// A generous timeout is only a last-resort safety net so a wedged
    /// handler cannot hang rollback forever; the normal path is a true join.
    pub async fn await_run_finished(&self, session_id: &str) {
        let rx = {
            let mut map = self.run_exit.lock().await;
            match map.get_mut(session_id) {
                Some(gate) => gate.rx.take(),
                None => return,
            }
        };
        let Some(rx) = rx else {
            // Another waiter already took the receiver; fall back to observing
            // the running set with the same timeout as the oneshot path.
            let deadline = tokio::time::Instant::now() + RUN_EXIT_WAIT_TIMEOUT;
            while self.running_sessions.lock().await.contains(session_id) {
                if tokio::time::Instant::now() >= deadline {
                    tracing::warn!(
                        "await_run_finished {}: running-set fallback timed out after {:?}; proceeding with restore",
                        session_id,
                        RUN_EXIT_WAIT_TIMEOUT
                    );
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            return;
        };
        match tokio::time::timeout(RUN_EXIT_WAIT_TIMEOUT, rx).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                // Sender dropped without send (e.g. clear_all_sessions) —
                // treat as finished.
            }
            Err(_) => {
                tracing::warn!(
                    "await_run_finished {}: handler did not exit within {:?}; proceeding with restore (late step writes are guarded by execute_step)",
                    session_id,
                    RUN_EXIT_WAIT_TIMEOUT
                );
            }
        }
    }

    /// Return (and, on first call, register) the cancellation token for a
    /// session. Register-on-miss mirrors `end_session`/the dispatcher's `entry()`
    /// pattern: a caller that cancels a directly-run session (tests, or code that
    /// runs the handler without the dispatcher claim path) must observe the
    /// same token the loop watches, otherwise `cancel()` would fire on a
    /// default token nobody listens to. `entry()` never clobbers an existing
    /// (possibly already-cancelled) token.
    pub async fn cancellation_token(&self, session_id: &str) -> CancellationToken {
        self.session_cancellations
            .lock()
            .await
            .entry(session_id.to_string())
            .or_insert_with(CancellationToken::new)
            .clone()
    }
}
