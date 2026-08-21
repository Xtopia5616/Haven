//! Session lifecycle, status machine, and working-set helpers.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract; behavior unchanged).

use super::*;

impl SessionExecutor {
    pub async fn create_session(&self, input: &str) -> anyhow::Result<SessionInfo> {
        self.create_session_with_summary(input, input).await
    }

    pub async fn create_session_with_summary(
        &self,
        input: &str,
        summary: &str,
    ) -> anyhow::Result<SessionInfo> {
        let record = self.db.create_session(input, input)?;
        let mut session = SessionInfo::from_db_record(&record);
        // The DB record was created with `input` as its transcript, but the
        // caller may have a distinct classifier-generated summary — overlay
        // it after construction so we keep the constructor single-purpose.
        session.summary = summary.into();
        let mut sessions = self.sessions.lock().await;
        sessions.insert(session.id.clone(), Arc::new(Mutex::new(session.clone())));

        // FIFO dispatch: queue the session before waking so the dispatcher's
        // first claim finds it at the tail, in submission order.
        self.enqueue_pending(&session.id).await;

        // Wake the dispatcher so it picks up this Pending session immediately.
        self.wake_dispatcher();
        Ok(session)
    }

    /// Remove the session from the cross-session messaging registry (graceful
    /// shutdown: `agents_list` no longer shows it). Mailboxes and archives
    /// are kept, so late messages remain deliverable (reported offline) and
    /// the history survives a later re-registration. Fire-and-forget.
    fn unregister_from_inbox(session_id: &str) {
        let sid = session_id.to_string();
        let sid_err = sid.clone();
        tokio::spawn(async move {
            let bus = haven_tools::inbox::InboxBus::default_root();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || bus.unregister(&sid)).await {
                tracing::debug!("messaging unregister failed for {sid_err}: {e}");
            }
        });
    }

    /// Persist a session status to the DB with a small number of retries. SQLite
    /// writes through the blocking pool can transiently fail with SQLITE_BUSY;
    /// a short retry turns that into extra latency instead of a diverged
    /// memory/DB state. Returns the last failure after exhausting the retries.
    pub(super) async fn persist_status(
        db: &Arc<Database>,
        session_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let mut last_err = None;
        for attempt in 0..3 {
            let db = db.clone();
            let tid = session_id.to_string();
            let st = status.to_string();
            match db
                .run_blocking(move |db| db.update_session_status(&tid, &st))
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("status persist failed")))
    }

    /// End a session. Since the user explicitly asked to end it, the session is
    /// always marked as Completed —regardless of whether it was still
    /// Running (forced stop) or Paused (naturally finished). Clean up
    /// resources either way. Called from the frontend "结束任务" button.
    pub async fn end_session(&self, session_id: &str) -> anyhow::Result<SessionStatus> {
        // Cancel the running token first to interrupt any active ReAct loop.
        // Ensure a real token exists even when the dispatcher hasn't created
        // one yet (race window between try_claim_pending and token insertion);
        // otherwise cancel() would fire on a default token nobody observes.
        let cancel = {
            let mut cancels = self.session_cancellations.lock().await;
            cancels
                .entry(session_id.to_string())
                .or_insert_with(CancellationToken::new)
                .clone()
        };
        cancel.cancel();
        // Kill any background actions the session spawned; they would otherwise
        // keep running (and leak child processes) after the session is gone.
        self.cancel_session_actions(session_id).await;
        // Promote checkpointed stream text into history (skip when a real
        // message already supersedes it). Runs BEFORE the session is torn down;
        // the PartialStore's generation bump also invalidates any in-flight
        // checkpoint so it cannot re-create the row afterwards.
        if let Err(e) = self.partials.promote(session_id).await {
            tracing::warn!(
                "end_session: failed to promote partial reply for session {}: {}",
                session_id,
                e
            );
        }
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            // Session not in memory (e.g. after restart) —end it regardless of
            // its DB state; the user asked to finish it.
            if let Err(e) = Self::persist_status(&self.db, session_id, "completed").await {
                tracing::error!(
                    "end_session: DB persist failed for session {}: {}",
                    session_id,
                    e
                );
                return Err(e);
            }
            return Ok(SessionStatus::Completed);
        };
        {
            let mut session = entry.lock().await;
            if let Err(e) = Self::persist_status(&self.db, session_id, "completed").await {
                tracing::error!(
                    "end_session: DB persist failed for session {}: {}",
                    session_id,
                    e
                );
                return Err(e);
            }
            session.status = SessionStatus::Completed;
            session.updated_at = chrono::Utc::now().to_rfc3339();
        }
        // Wake any ReAct-loop status waiter before tearing down the rest of
        // the per-session state.
        if let Some(tx) = self.status_tx.lock().await.remove(session_id) {
            let _ = tx.send(SessionStatus::Completed);
        }
        self.dequeue_pending(session_id).await;
        // F1 fence: if a handler is still marked running, leave
        // `running_sessions` + permit + cancel token for that handler's
        // post-exit `unmark_running`. Clearing them here would let the
        // dispatcher reclaim the same id while the cancelled handler is
        // still unwinding, and the late `unmark_running` would then drop
        // the *new* claim's permit.
        let still_running = self.running_sessions.lock().await.contains(session_id);
        if !still_running {
            self.cleanup_session_maps(session_id).await;
        }
        self.sessions.lock().await.remove(session_id);
        Self::unregister_from_inbox(session_id);
        // The conversation is over — its trusted risk levels must not outlive
        // it (a later conversation must ask again).
        self.tools
            .safety_gateway
            .clear_session_trust(session_id)
            .await;
        Ok(SessionStatus::Completed)
    }

    /// Remove a session entirely from the in-memory state.
    /// This does NOT delete from DB —the caller handles that.
    /// Succeeds even if the session is not in memory (e.g. after restart).
    pub async fn remove_session(&self, session_id: &str) {
        self.tools
            .safety_gateway
            .clear_session_trust(session_id)
            .await;
        self.cancel_session_actions(session_id).await;
        self.sessions.lock().await.remove(session_id);
        self.dequeue_pending(session_id).await;
        self.cleanup_session_maps(session_id).await;
        self.status_tx.lock().await.remove(session_id);
        self.action_completions.lock().await.remove(session_id);
        self.awaiting_answer.lock().await.remove(session_id);
        self.awaiting_confirm.lock().await.remove(session_id);
    }

    pub async fn update_session_title(&self, session_id: &str, title: &str) {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        if let Some(entry) = entry {
            entry.lock().await.title = Some(title.into());
        }
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let entries: Vec<Arc<Mutex<SessionInfo>>> =
            self.sessions.lock().await.values().cloned().collect();
        let mut sessions: Vec<SessionInfo> = Vec::with_capacity(entries.len());
        for entry in entries {
            sessions.push(entry.lock().await.clone());
        }
        // Preserve the insertion-order semantics of the former Vec storage
        // (the map itself is unordered).
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        sessions
    }

    /// Remove all sessions from memory and clean up running state.
    /// Used when the user clears history —the DB is already wiped.
    pub async fn clear_all_sessions(&self) {
        self.tools.safety_gateway.clear_all_trust().await;
        self.sessions.lock().await.clear();
        self.pending_queue.lock().await.clear();
        self.running_sessions.lock().await.clear();
        self.session_permits.lock().await.clear();
        self.session_cancellations.lock().await.clear();
        // Drop gates so any `await_run_finished` waiter unblocks (recv Err).
        self.run_exit.lock().await.clear();
        self.status_tx.lock().await.clear();
        self.action_completions.lock().await.clear();
        self.awaiting_answer.lock().await.clear();
        self.awaiting_confirm.lock().await.clear();
    }

    /// Subscribe to a session's status changes. Level-triggered: the receiver
    /// holds the CURRENT status, so a transition that happened before the
    /// subscription is visible immediately, and `changed()` resolves as soon
    /// as the status moves after the receiver's last observed value. Callers
    /// must re-read the authoritative state after waking (the watch value is
    /// a hint, not a lock-free source of truth).
    pub async fn subscribe_status(&self, session_id: &str) -> watch::Receiver<SessionStatus> {
        // Initial value: the session's current status so a receiver created
        // after a transition observes it; Pending when the session is absent
        // (the caller re-checks state after waking anyway).
        let initial = {
            let entry = self.sessions.lock().await.get(session_id).cloned();
            match entry {
                Some(e) => e.lock().await.status.clone(),
                None => SessionStatus::Pending,
            }
        };
        self.status_tx
            .lock()
            .await
            .entry(session_id.to_string())
            .or_insert_with(|| watch::channel(initial).0)
            .subscribe()
    }

    /// Transition a session's status through the centralized state machine.
    ///
    /// Ordering guarantees (all under the session's own entry lock, so
    /// transitions of different sessions never serialize on a global lock):
    /// 1. The transition is validated against `can_transition`; illegal
    ///    transitions (e.g. mutating a terminal state) are rejected with a
    ///    warning and leave the state untouched.
    /// 2. The DB write happens BEFORE the memory flip, with a short retry, so
    ///    a persistent DB failure aborts the transition with memory/DB
    ///    consistent (the DB is the source of truth across restarts).
    /// 3. The status watcher is notified and, for Pending transitions, the
    ///    dispatcher is woken — outside the entry lock.
    /// 4. Terminal transitions run cleanup (maps, per-session tools, working
    ///    set) after the wake so a waiter observing the terminal status
    ///    always sees the session still resolvable.
    pub async fn update_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        self.update_session_status_inner(session_id, status, true).await
    }

    /// Transition a session's status in MEMORY ONLY, without persisting it to
    /// the DB. Used by the history-review reopen flow: a merely VIEWED
    /// completed/errored session must be made resumable for the current run
    /// (Paused) without resurrecting it in the DB — otherwise the ended
    /// conversation would be auto-restored on every app start and shown as an
    /// active conversation everywhere. Once the user actually continues it,
    /// the normal transitions persist again.
    pub async fn update_session_status_memory_only(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        self.update_session_status_inner(session_id, status, false).await
    }

    async fn update_session_status_inner(
        &self,
        session_id: &str,
        status: SessionStatus,
        persist: bool,
    ) -> anyhow::Result<()> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            // Session not in memory (e.g. already removed): no-op, matching the
            // historical behavior of silently succeeding.
            return Ok(());
        };
        let mut session = entry.lock().await;
        let old_status = session.status.clone();
        if old_status == status {
            // Same-status refresh: still wake the dispatcher so a session
            // re-registered as Pending (e.g. `create_session_with_first_message`)
            // is picked up even though its status did not change.
            if status == SessionStatus::Pending {
                self.enqueue_pending(session_id).await;
                self.wake_dispatcher();
            }
            return Ok(());
        }
        if !Self::can_transition(&old_status, &status) {
            tracing::warn!(
                "update_session_status: rejected illegal transition session={} {:?} -> {:?}",
                session_id,
                old_status,
                status
            );
            return Ok(());
        }
        if persist
            && let Err(e) = Self::persist_status(&self.db, session_id, status.as_str()).await
        {
            tracing::error!(
                "update_session_status: DB persist failed for session {}; transition {:?} -> {:?} aborted: {}",
                session_id,
                old_status,
                status,
                e
            );
            return Err(e);
        }
        session.status = status.clone();
        session.updated_at = chrono::Utc::now().to_rfc3339();
        tracing::info!(
            "update_session_status: session={} {} -> {}",
            session_id,
            old_status.as_str(),
            status.as_str()
        );
        let is_pending = status == SessionStatus::Pending;
        let is_terminal = status.is_terminal();
        drop(session);
        drop(entry);
        // Level-triggered wake: send on the existing watcher channel (or
        // lazily create one) so the ReAct loop's pause-wait resolves.
        let tx = {
            let mut map = self.status_tx.lock().await;
            map.entry(session_id.to_string())
                .or_insert_with(|| watch::channel(status.clone()).0)
                .clone()
        };
        let _ = tx.send(status.clone());
        if is_pending {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
        if is_terminal {
            self.dequeue_pending(session_id).await;
            self.cleanup_session_maps(session_id).await;
            self.tools.unregister_session(session_id).await;
            Self::unregister_from_inbox(session_id);
            // The conversation ended — drop its trusted risk levels too (the
            // ReAct loop / dispatcher-panic path reaches terminal status
            // through here, not `end_session`, so this must happen on every
            // terminal transition or the per-session trust map leaks).
            self.tools
                .safety_gateway
                .clear_session_trust(session_id)
                .await;
            if let Some(tx) = self.status_tx.lock().await.remove(session_id) {
                let _ = tx.send(status);
            }
            self.sessions.lock().await.remove(session_id);
        }
        Ok(())
    }

    /// Centralized transition validation. Only transitions reachable from
    /// real call sites are allowed; anything else (notably any mutation of a
    /// terminal state except the explicit reopen/continue flows) is a bug and
    /// is rejected.
    fn can_transition(from: &SessionStatus, to: &SessionStatus) -> bool {
        use SessionStatus::*;
        match (from, to) {
            // Claim by the dispatcher.
            (Pending, Running) => true,
            // Park / finish a queued session without dispatching it.
            (Pending, Paused)
            | (Pending, PausedAwaitingAnswer)
            | (Pending, PausedAwaitingConfirm) => true,
            (Pending, Completed) | (Pending, Error) => true,
            // Pause for a user reply / confirm / scheduling / budget checkpoint.
            (Running, Paused)
            | (Running, PausedAwaitingAnswer)
            | (Running, PausedAwaitingConfirm) => true,
            // Immediate resume: the ask/confirm was answered in the same turn
            // (pause_turn → Pending while the handler is still alive).
            (Running, Pending) => true,
            // Natural completion / failure.
            (Running, Completed) | (Running, Error) => true,
            // Resume paths (user message, action completion, continue flow).
            (Paused, Pending)
            | (PausedAwaitingAnswer, Pending)
            | (PausedAwaitingConfirm, Pending) => true,
            // Re-pause with an answer / confirm requirement.
            (Paused, PausedAwaitingAnswer) | (Paused, PausedAwaitingConfirm) => true,
            // Phase 7 / E4: Paused* → Running is illegal. Only the dispatcher
            // claim path (Pending → Running) starts a run; tools must not
            // force-resume a paused session.
            // Finish / fail a paused session (end_session's own path also exists,
            // but explicit transitions are kept valid).
            (Paused, Completed) | (Paused, Error) => true,
            (PausedAwaitingAnswer, Completed) | (PausedAwaitingAnswer, Error) => true,
            (PausedAwaitingConfirm, Completed) | (PausedAwaitingConfirm, Error) => true,
            // User-driven exceptions: reopen a finished session for review
            // (history flow), retry an errored session from its snapshot.
            (Completed, Paused) | (Error, Paused) => true,
            (Error, Pending) => true,
            _ => false,
        }
    }

    /// Remove `session_id` from the per-session maps (`running_sessions`,
    /// `session_permits`, `session_cancellations`, `run_exit`). Centralizes
    /// the cleanup that used to be copy-pasted at every cleanup site.
    /// Signaling `run_exit` here makes [`Self::await_run_finished`] resolve
    /// exactly when the running slot is released.
    /// Does NOT touch `sessions` (working set) or `status_tx` — those have
    /// ordering-sensitive callers (`update_session_status`, `unmark_running`)
    /// that need to remain in the lock-order path.
    pub async fn cleanup_session_maps(&self, session_id: &str) {
        self.running_sessions.lock().await.remove(session_id);
        self.session_permits.lock().await.remove(session_id);
        self.session_cancellations.lock().await.remove(session_id);
        if let Some(gate) = self.run_exit.lock().await.remove(session_id) {
            let _ = gate.tx.send(());
        }
    }

    /// Look up an in-memory `SessionInfo` by id (O(1), per-session lock only).
    pub async fn get_session(&self, session_id: &str) -> Option<SessionInfo> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        Some(entry?.lock().await.clone())
    }

    /// Load a session from the database into the in-memory list if it is not
    /// already there (e.g. after an app restart). Used by `process_input`
    /// so that follow-up messages can reach sessions that were paused before
    /// the restart and never re-entered the executor's working set.
    pub async fn ensure_session_loaded(&self, session_id: &str) -> anyhow::Result<()> {
        {
            let sessions = self.sessions.lock().await;
            if sessions.contains_key(session_id) {
                return Ok(());
            }
        }
        let record = self
            .db
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found in database", session_id))?;
        let session = SessionInfo::from_db_record(&record);
        let mut sessions = self.sessions.lock().await;
        // Re-check: another thread may have inserted this session between the
        // check above and the DB query.
        if !sessions.contains_key(session_id) {
            sessions.insert(session_id.to_string(), Arc::new(Mutex::new(session)));
        }
        Ok(())
    }

    /// Reload sessions that are still `Pending` in the database into the
    /// in-memory working set and wake the dispatcher. Called at dispatcher
    /// startup so queued work from a previous run is picked up after an app
    /// restart. Returns the number of sessions reloaded.
    pub async fn load_pending_sessions(&self) -> usize {
        let pending =
            match self
                .db
                .search_sessions_filtered(None, Some("pending"), None, None, -1, 0)
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("load_pending_sessions: pending-session query failed: {}", e);
                    Vec::new()
                }
            };
        let mut loaded = 0;
        let mut queued = Vec::new();
        {
            let mut sessions = self.sessions.lock().await;
            for record in pending {
                if sessions.contains_key(&record.id) {
                    continue;
                }
                // Force Pending: this loader only ever rehydrates sessions
                // whose DB status is already "pending" (the SQL filter
                // guarantees that), so the override is a no-op but keeps
                // the invariant explicit at the call site.
                let mut info = SessionInfo::from_db_record(&record);
                info.status = SessionStatus::Pending;
                sessions.insert(record.id.clone(), Arc::new(Mutex::new(info)));
                queued.push(record.id);
                loaded += 1;
            }
        }
        // FIFO: enqueue after releasing the working-set lock (the queue lock
        // is never held across the map lock to keep the order acyclic).
        for id in queued {
            self.enqueue_pending(&id).await;
        }
        if loaded > 0 {
            self.wake_dispatcher();
        }
        loaded
    }

    /// Current in-memory status of a session, or `None` when the session is not in
    /// the working set (removed on terminal cleanup / `end_session` / restart).
    /// Deliberately does NOT conflate "absent" with `Error`: callers that
    /// previously probed for `Error` to detect removal must check for `None`.
    pub async fn get_session_state(&self, session_id: &str) -> Option<SessionStatus> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        Some(entry?.lock().await.status.clone())
    }

    pub fn get_tools(&self) -> Arc<ToolsManager> {
        self.tools.clone()
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Cancel and drop all background actions owned by a session, and cancel its
    /// pending scheduled_actions. Called when the session ends, is removed, or is
    /// rolled back so child processes cannot leak past their session and no
    /// scheduled action fires against a session that no longer exists.
    pub async fn cancel_session_actions(&self, session_id: &str) {
        self.tools
            .background_actions
            .cancel_for_session(session_id)
            .await;
        self.tools
            .scheduled_actions
            .cancel_for_session(session_id)
            .await;
    }

    /// Fail every still-`pending` action step for a session (handler panic /
    /// abort after [`Self::begin_action_step`]).
    pub async fn fail_pending_action_steps(&self, session_id: &str, observation: &str) {
        let session_id = session_id.to_string();
        let observation = observation.to_string();
        if let Err(e) = self
            .db
            .run_blocking(move |db| db.fail_pending_action_steps(&session_id, &observation))
            .await
        {
            tracing::warn!("fail_pending_action_steps failed: {e}");
        }
    }
}
