//! Session lifecycle owned by [`SessionSupervisor`].

use super::*;
use haven_common::retry::{BackoffPolicy, RecoveryDecision, RecoveryPolicy, RecoverySignal};
use std::time::{Duration, Instant};

const PENDING_SESSION_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const PENDING_SESSION_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(30);

impl SessionSupervisor {
    pub async fn create_session(self: &Arc<Self>, input: &str) -> anyhow::Result<SessionInfo> {
        self.create_session_with_summary(input, input).await
    }

    pub async fn create_session_with_summary(
        self: &Arc<Self>,
        input: &str,
        summary: &str,
    ) -> anyhow::Result<SessionInfo> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        let record = self.store.create_session(input).await?;
        let mut info = SessionInfo::from_db_record(&record);
        info.summary = summary.to_string();
        self.install_actor(info.clone()).await?;
        self.enqueue_pending(&info.id).await;
        self.wake_dispatcher();
        Ok(info)
    }

    pub(super) async fn persist_status(
        store: &SessionStore,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        let retry_policy = RecoveryPolicy::new(
            Some(3),
            None,
            BackoffPolicy::new(Duration::from_millis(10), 1, Duration::from_millis(10)),
        );
        let mut completed_attempts = 0u32;
        loop {
            match store.update_session_status(session_id, status).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    completed_attempts = completed_attempts.saturating_add(1);
                    match retry_policy.decide(
                        completed_attempts,
                        RecoverySignal::Retryable { retry_after: None },
                        Instant::now(),
                        0,
                    ) {
                        RecoveryDecision::Retry { delay, .. } => {
                            tokio::time::sleep(delay).await;
                        }
                        RecoveryDecision::Stop { .. } => return Err(error),
                    }
                }
            }
        }
    }

    pub async fn end_session(&self, session_id: &str) -> anyhow::Result<SessionStatus> {
        self.end_session_with_cascade(session_id, true).await
    }

    async fn end_session_with_cascade(
        &self,
        session_id: &str,
        cascade: bool,
    ) -> anyhow::Result<SessionStatus> {
        // Close concurrent loads/resumes while an actorless end persists its
        // terminal status and cleans up durable tool_runs.
        let closing = self
            .begin_session_closing(session_id, SessionClosingMode::EndPreparing { cascade })
            .await?;
        self.end_session_inner(session_id, cascade, &closing).await
    }

    pub async fn interrupt_session(&self, session_id: &str) -> anyhow::Result<bool> {
        let status = self
            .get_active_session_status(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found", session_id))?;
        match status {
            SessionStatus::Running => {
                self.cancel_direct_session_run_admission_waiters(session_id)
                    .await;
                let cancel = self.cancellation_token(session_id).await;
                // Cancellation is a control-plane request. Do not wait for a
                // provider/tool to cooperate here: the UI must regain its
                // controls even when the active tool is slow or stuck.
                cancel.cancel();
                if let Some(actor) = self.actor_for(session_id).await {
                    let _ = actor.cancel_session().await;
                }
                self.update_session_status(session_id, SessionStatus::Paused)
                    .await?;
                Ok(self.get_active_session_status(session_id).await == Some(SessionStatus::Paused))
            }
            SessionStatus::Pending => {
                self.update_session_status(session_id, SessionStatus::Paused)
                    .await?;
                Ok(self.get_active_session_status(session_id).await == Some(SessionStatus::Paused))
            }
            SessionStatus::Paused => Ok(false),
            SessionStatus::Completed | SessionStatus::Error => Err(anyhow::anyhow!(
                "session '{}' is not running (current: {})",
                session_id,
                status.as_str()
            )),
        }
    }

    async fn end_session_inner(
        &self,
        session_id: &str,
        cascade: bool,
        closing: &SessionClosingGuard,
    ) -> anyhow::Result<SessionStatus> {
        // Confirmation resolution can claim a scheduled ToolRun before it
        // consumes the owner-local request. Serialize that two-part decision
        // with lifecycle cleanup; resolution entry points also reject owners
        // whose session is marked closing while this guard is held.
        let resolution = self.confirmation_resolution_gate.clone().lock_owned().await;
        let Some(actor) = self.actor_for(session_id).await else {
            // Preserve the established idempotent no-op behavior for unknown
            // sessions. Existing callers may end a stale selection while its
            // durable row has already been removed.
            let record = self.store.session_record(session_id)?;
            if record.as_ref().is_some_and(|record| {
                record.status != SessionStatus::Paused && record.status != SessionStatus::Completed
            }) {
                Self::persist_status(&self.store, session_id, SessionStatus::Paused).await?;
            }
            if let Err(error) = self.cancel_session_tool_runs_checked(session_id).await {
                if record
                    .as_ref()
                    .is_some_and(|record| record.status != SessionStatus::Completed)
                {
                    self.emit_event(SessionSupervisorEvent::SessionEndPaused {
                        session_id: session_id.to_string(),
                    });
                }
                return Err(error);
            }
            drop(resolution);
            if record
                .as_ref()
                .is_some_and(|record| record.status != SessionStatus::Completed)
                && let Err(error) =
                    Self::persist_status(&self.store, session_id, SessionStatus::Completed).await
            {
                self.emit_event(SessionSupervisorEvent::SessionEndPaused {
                    session_id: session_id.to_string(),
                });
                return Err(error);
            }
            self.finish_ended_session(session_id, cascade).await;
            return Ok(SessionStatus::Completed);
        };
        self.cancel_direct_session_run_admission_waiters(session_id)
            .await;
        let status = actor
            .snapshot()
            .await
            .map(|session| session.status)
            .ok_or_else(|| anyhow::anyhow!("session actor '{}' has stopped", session_id))?;
        if status != SessionStatus::Paused && status != SessionStatus::Completed {
            // Persist the retryable state before signalling the run. If this
            // write fails, the actor and its run remain untouched and the user
            // can retry without inheriting a dead actor.
            actor.transition(SessionStatus::Paused, true).await?;
        }
        // End one run without cancelling the actor lifetime. A failed ToolRun
        // cleanup leaves this actor available for Continue or another end
        // attempt, and the next accepted run receives a fresh child token.
        actor.run_cancellation_token().cancel();
        if let Err(error) = self.cancel_session_tool_runs_checked(session_id).await {
            if status != SessionStatus::Completed {
                self.emit_end_paused_for_retry(session_id, Some(&actor))
                    .await;
            }
            return Err(error);
        }
        // Keep the closing marker installed while the confirmation gate is
        // released. A resolver that arrives before Completed is written will
        // observe the marker and decline to change owner state. Releasing here
        // also lets a cascading child end acquire the same gate.
        drop(resolution);
        // Marking the session terminal is immediate. If the run is still
        // active, terminal cleanup and partial promotion are deferred to the
        // dispatcher run-exit edge, which keeps late tool output isolated from
        // the next session without blocking the user's control action.
        if let Err(error) = self
            .complete_resident_session_end(session_id, &actor, closing, cascade)
            .await
        {
            if self.get_session_status(session_id).await == Some(SessionStatus::Paused) {
                self.emit_end_paused_for_retry(session_id, Some(&actor))
                    .await;
            }
            return Err(error);
        }
        Ok(SessionStatus::Completed)
    }

    async fn complete_resident_session_end(
        &self,
        session_id: &str,
        expected_actor: &actor::SessionActorHandle,
        closing: &SessionClosingGuard,
        cascade: bool,
    ) -> anyhow::Result<()> {
        let actor = {
            let _lifecycle = self.lifecycle_guard().await;
            self.ensure_lifecycle_open()?;
            let Some(actor) = self.actor_for(session_id).await else {
                anyhow::bail!("session actor '{}' has stopped", session_id);
            };
            if !actor.same_instance(expected_actor) {
                anyhow::bail!("session actor '{}' changed during end", session_id);
            }
            actor.transition(SessionStatus::Completed, true).await?;
            anyhow::ensure!(
                closing.mark_end_committed(),
                "session '{}' lost its end closing marker",
                session_id
            );
            self.cancel_direct_session_run_admission_waiters(session_id)
                .await;
            self.dequeue_pending(session_id).await;
            actor
        };

        // End has now committed Completed and published its handoff phase.
        // If the run has already exited, this caller claims cleanup; otherwise
        // the run-exit owner may take over while the marker remains installed.
        if !actor.is_running().await {
            self.finish_idle_terminal_state(session_id, &actor, false, Some(cascade))
                .await;
        }
        Ok(())
    }

    async fn emit_end_paused_for_retry(
        &self,
        session_id: &str,
        actor: Option<&actor::SessionActorHandle>,
    ) {
        if let Some(actor) = actor
            && let Err(error) = actor
                .set_waiting_reason(Some(haven_common::SessionWaitingReason::EndIncomplete))
                .await
        {
            tracing::warn!(
                session_id,
                error = %error,
                "failed to mark paused session as awaiting end retry"
            );
        }
        self.emit_event(SessionSupervisorEvent::SessionEndPaused {
            session_id: session_id.to_string(),
        });
    }

    pub(super) async fn finish_ended_session(&self, session_id: &str, cascade: bool) {
        Self::unregister_from_inbox(session_id);
        self.unregister_session_tool_overlay(session_id).await;
        self.scheduled_confirms
            .lock()
            .await
            .retain(|_, request| request.session_id.as_deref() != Some(session_id));
        if cascade && self.may_have_children(session_id).await {
            Box::pin(self.cascade_end_children(session_id)).await;
        }
        self.clear_has_children(session_id).await;
        self.authorization.clear_session_trust(session_id).await;
    }

    fn unregister_from_inbox(session_id: &str) {
        let session_id = session_id.to_string();
        let log_id = session_id.clone();
        tokio::spawn(async move {
            let messaging = haven_messaging::MessagingService::default_root();
            let result = tokio::task::spawn_blocking(move || {
                let entry = messaging
                    .list_agents()?
                    .into_iter()
                    .find(|agent| agent.name == session_id);
                if entry.as_ref().is_some_and(|agent| agent.parent.is_some()) {
                    messaging.mark_offline(&session_id)
                } else {
                    messaging.unregister(&session_id)
                }
            })
            .await;
            if let Ok(Err(error)) = result {
                tracing::debug!(session_id = %log_id, error = %error, "messaging registry cleanup failed");
            }
        });
    }

    async fn cascade_end_children(&self, parent_session_id: &str) {
        let parent = parent_session_id.to_string();
        let descendants = match tokio::task::spawn_blocking({
            let parent = parent.clone();
            move || -> anyhow::Result<Vec<String>> {
                let messaging = haven_messaging::MessagingService::default_root();
                let children = messaging.list_descendants(&parent)?;
                for child in &children {
                    let _ = messaging.deliver_system_notice(
                        &parent,
                        child,
                        "Parent session ended; stop work and finish this delegated task.",
                    );
                }
                Ok(children)
            }
        })
        .await
        {
            Ok(Ok(children)) => children,
            Ok(Err(error)) => {
                tracing::error!(parent_session_id = %parent, error = %error, "failed to enumerate descendants");
                return;
            }
            Err(error) => {
                tracing::error!(parent_session_id = %parent, error = %error, "descendant enumeration worker failed");
                return;
            }
        };
        for child_id in descendants {
            if child_id == parent {
                continue;
            }
            let title = self
                .get_session(&child_id)
                .await
                .map(|session| session.title.unwrap_or(session.input))
                .unwrap_or_default();
            if self
                .end_session_with_cascade(&child_id, false)
                .await
                .is_ok()
            {
                self.emit_event(SessionSupervisorEvent::CascadeCompleted {
                    session_id: child_id,
                    title,
                });
            }
        }
    }

    pub async fn remove_session(&self, session_id: &str) -> anyhow::Result<()> {
        let _closing = self
            .begin_session_closing(session_id, SessionClosingMode::Destructive)
            .await?;
        async {
            self.quiesce_session(session_id).await?;
            let _lifecycle = self.lifecycle_guard().await;
            self.remove_session_locked(session_id).await
        }
        .await
    }

    /// Cancel and join a run without holding the registry gate. A live ReAct
    /// handler may need that gate to finish a child-session operation.
    async fn quiesce_session(&self, session_id: &str) -> anyhow::Result<()> {
        self.cancel_direct_session_run_admission_waiters(session_id)
            .await;
        if let Some(actor) = self.actor_for(session_id).await {
            actor.cancel_actor();
            self.dequeue_pending(session_id).await;
            if let Err(error) = self.cancel_session_tool_runs_checked(session_id).await {
                tracing::warn!(session_id = %session_id, "initial session ToolRun cleanup failed while quiescing; retrying after run exit: {error}");
            }
            self.await_run_finished(session_id).await?;
            // Join closes ToolRun admission from the actor run, so this final
            // pass also catches a durable scheduled row whose caller was
            // cancelled between its SQLite commit and board publication.
            self.cancel_session_tool_runs_checked(session_id).await?;
        } else {
            self.cancel_session_tool_runs_checked(session_id).await?;
        }
        Ok(())
    }

    async fn remove_session_locked(&self, session_id: &str) -> anyhow::Result<()> {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.clear_runtime().await;
        }
        self.dequeue_pending(session_id).await;
        self.unregister_session_tool_overlay(session_id).await;
        self.authorization.clear_session_trust(session_id).await;
        self.scheduled_confirms
            .lock()
            .await
            .retain(|_, request| request.session_id.as_deref() != Some(session_id));
        self.remove_actor_locked(session_id).await;
        Ok(())
    }

    pub async fn update_session_title(&self, session_id: &str, title: &str) {
        if let Some(actor) = self.actor_for(session_id).await {
            let _ = actor
                .send(actor::ActorCommand::UpdateTitle {
                    title: title.to_string(),
                })
                .await;
        }
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let actors = self
            .actors
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut sessions = Vec::with_capacity(actors.len());
        for actor in actors {
            if let Some(mut session) = actor.snapshot().await
                && !session.status.is_terminal()
            {
                session.waiting_reason = self.waiting_reason(&session.id).await;
                sessions.push(session);
            }
        }
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        sessions
    }

    pub async fn clear_all_sessions(&self) -> anyhow::Result<()> {
        let _block = self.begin_lifecycle_block()?;
        async {
            self.quiesce_all_sessions(false).await?;
            let _lifecycle = self.lifecycle_guard().await;
            self.clear_all_sessions_locked().await
        }
        .await
    }

    /// Clear the in-memory session actors during normal application shutdown.
    ///
    /// A session-owned scheduled ToolRun is durable work, not a child process of
    /// the actor.  It must remain `waiting` so ToolRunService can restore it on
    /// the next startup.  Explicit session deletion/end still uses the regular
    /// quiesce path and cancels all owned tool_runs.
    pub async fn clear_all_sessions_for_shutdown(&self) -> anyhow::Result<()> {
        let _block = self.begin_lifecycle_block()?;
        async {
            self.quiesce_all_sessions(true).await?;
            let _lifecycle = self.lifecycle_guard().await;
            self.clear_all_sessions_locked().await
        }
        .await
    }

    async fn quiesce_all_sessions(&self, preserve_scheduled: bool) -> anyhow::Result<()> {
        let actors = self
            .actors
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for actor in &actors {
            self.cancel_direct_session_run_admission_waiters(&actor.id)
                .await;
            actor.cancel_actor();
            if preserve_scheduled {
                self.cancel_session_background_tool_runs(&actor.id).await;
            } else {
                self.cancel_session_tool_runs(&actor.id).await;
            }
            self.dequeue_pending(&actor.id).await;
        }
        for actor in &actors {
            self.await_run_finished(&actor.id).await?;
        }
        Ok(())
    }

    async fn clear_all_sessions_locked(&self) -> anyhow::Result<()> {
        let actors = self
            .actors
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for actor in &actors {
            actor.clear_runtime().await;
        }
        self.authorization.clear_all_trust().await;
        self.actors.lock().await.clear();
        self.terminal_cleanup_cascade_overrides
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
        self.pending_queue.lock().await.clear();
        self.scheduled_confirms.lock().await.clear();
        self.direct_session_run_admission_waiters
            .lock()
            .await
            .clear();
        Ok(())
    }

    /// Remove one session from both the actor registry and durable storage
    /// under one lifecycle gate. This is the only deletion entry point for
    /// app commands; it prevents ensure/load from reinstalling a stale actor
    /// between the in-memory quiesce and the SQL delete.
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        let _closing = self
            .begin_session_closing(session_id, SessionClosingMode::Destructive)
            .await?;
        async {
            self.quiesce_session(session_id).await?;
            let _lifecycle = self.lifecycle_guard().await;
            self.partials.forget_session(session_id).await;
            self.remove_session_locked(session_id).await?;
            self.store.delete_session(session_id).await?;
            // Once the durable owner is gone, release its process-local asset
            // lease so reference-based media cleanup can reclaim unshared
            // files. The deletion must succeed first; a failed DB delete keeps
            // the lease intact.
            self.release_managed_assets_for_session(session_id);
            Ok(())
        }
        .await
    }

    /// Expire sessions through the same quiesce/delete path as an explicit
    /// user deletion. This releases live actors, session grants, MCP overlays,
    /// and managed-asset leases before the host reconciles attachment files.
    pub async fn delete_old_sessions(&self, retention_days: u32) -> anyhow::Result<usize> {
        if retention_days == 0 {
            return Ok(0);
        }
        self.ensure_lifecycle_open()?;
        let candidates = self.store.old_session_ids(retention_days).await?;
        let mut deleted = 0;
        for session_id in candidates {
            match self.delete_session(&session_id).await {
                Ok(()) => deleted += 1,
                Err(error) => {
                    // A separate explicit delete may have won after the
                    // candidate snapshot. Treat a now-absent row as already
                    // expired, but do not hide a live-session failure.
                    match self.store.load_session_record(&session_id).await {
                        Ok(None) => {
                            self.release_managed_assets_for_session(&session_id);
                        }
                        Ok(Some(_)) => {
                            return Err(
                                error.context(format!("failed to expire session {session_id}"))
                            );
                        }
                        Err(read_error) => {
                            return Err(error.context(format!(
                                "failed to expire session {session_id}; checking whether it still exists also failed: {read_error}"
                            )));
                        }
                    }
                }
            }
        }
        Ok(deleted)
    }

    /// Quiesce the working set and clear durable history while holding the
    /// same gate used by session creation/loading/deletion.
    pub async fn clear_sessions_and_delete(&self) -> anyhow::Result<Vec<String>> {
        let _block = self.begin_lifecycle_block()?;
        async {
            self.quiesce_all_sessions(false).await?;
            let _lifecycle = self.lifecycle_guard().await;
            let session_ids = self
                .store
                .all_session_ids_cancellable(tokio_util::sync::CancellationToken::new())
                .await?;
            // The actor registry is only the resident working set. Include
            // durable sessions that were never loaded before clearing their
            // rows, while ToolRunService remains the ToolRun-state owner.
            for session_id in &session_ids {
                self.cancel_session_tool_runs_checked(session_id).await?;
            }
            self.clear_all_sessions_locked().await?;
            self.partials.forget_all_sessions().await;
            self.store.clear_sessions().await?;
            // Release only after the durable purge succeeds. Shared paths are
            // still protected by any remaining message reference and can be
            // reclaimed by the host cleanup pass.
            for session_id in &session_ids {
                self.release_managed_assets_for_session(session_id);
            }
            Ok(session_ids)
        }
        .await
    }

    pub(crate) fn ensure_lifecycle_open(&self) -> anyhow::Result<()> {
        if self
            .lifecycle_blocked
            .load(std::sync::atomic::Ordering::Acquire)
        {
            anyhow::bail!("session lifecycle is quiescing; retry after history cleanup")
        }
        Ok(())
    }

    pub(crate) fn begin_lifecycle_block(&self) -> anyhow::Result<LifecycleBlockGuard> {
        self.lifecycle_blocked
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .map(|_| LifecycleBlockGuard {
                blocked: self.lifecycle_blocked.clone(),
                dispatch_tx: self.dispatch_tx.clone(),
            })
            .map_err(|_| anyhow::anyhow!("session lifecycle cleanup is already in progress"))
    }

    pub(crate) async fn begin_session_closing(
        &self,
        session_id: &str,
        mode: SessionClosingMode,
    ) -> anyhow::Result<SessionClosingGuard> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        if self
            .terminal_cleanup_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(session_id)
        {
            anyhow::bail!(
                "session '{}' terminal cleanup is already in progress",
                session_id
            );
        }
        {
            let mut closing_sessions = self
                .closing_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if closing_sessions.contains_key(session_id) {
                anyhow::bail!("session '{}' is already closing", session_id);
            }
            closing_sessions.insert(session_id.to_string(), mode);
        }
        let closing = SessionClosingGuard {
            sessions: self.closing_sessions.clone(),
            cascade_overrides: self.terminal_cleanup_cascade_overrides.clone(),
            retry_queue: self.terminal_cleanup_retry_queue.clone(),
            session_id: session_id.to_string(),
        };
        self.cancel_direct_session_run_admission_waiters(session_id)
            .await;
        Ok(closing)
    }

    pub async fn subscribe_status(&self, session_id: &str) -> watch::Receiver<SessionStatus> {
        self.actor_for(session_id)
            .await
            .map(|actor| actor.status())
            .unwrap_or_else(|| watch::channel(SessionStatus::Pending).1)
    }

    pub async fn update_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<bool> {
        self.update_session_status_inner(session_id, None, status, true)
            .await
    }

    /// Update a status only when the actor is still in `expected`.
    ///
    /// The check and transition are serialized inside the session actor. This
    /// is required for wake-up paths that first observe `Paused`: a dispatcher
    /// may claim the session before the wake request reaches the actor, and a
    /// stale unconditional `Running -> Pending` transition would make the
    /// active ReAct loop violate its run-entry invariant.
    pub async fn update_session_status_if(
        &self,
        session_id: &str,
        expected: SessionStatus,
        status: SessionStatus,
    ) -> anyhow::Result<bool> {
        self.update_session_status_inner(session_id, Some(expected), status, true)
            .await
    }

    /// Commit a run failure from the dispatcher and publish its typed event
    /// while lifecycle admission is serialized with end/Continue/rollback.
    /// ReAct may already have conditionally set Error before returning; that
    /// state is accepted only while no close operation has started.
    pub(crate) async fn commit_dispatcher_run_error(
        &self,
        session_id: &str,
        reason: String,
    ) -> anyhow::Result<bool> {
        let _lifecycle = self.lifecycle_guard().await;
        if self.ensure_lifecycle_open().is_err() || self.is_session_closing(session_id) {
            return Ok(false);
        }

        let changed = self
            .update_session_status_if(session_id, SessionStatus::Running, SessionStatus::Error)
            .await?;
        let is_error =
            changed || self.get_session_status(session_id).await == Some(SessionStatus::Error);
        if is_error {
            self.emit_event(SessionSupervisorEvent::SessionError {
                session_id: session_id.to_string(),
                reason,
            });
        }
        Ok(is_error)
    }

    /// Commit a direct-run error only while a run is still active. The caller
    /// holds the lifecycle gate through event publication so end cannot
    /// publish a later pause before the matching Agent event.
    pub(crate) async fn mark_run_failed_if_active(&self, session_id: &str) -> anyhow::Result<bool> {
        self.update_session_status_if(session_id, SessionStatus::Running, SessionStatus::Error)
            .await
    }

    pub async fn update_session_status_memory_only(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<bool> {
        if status.is_terminal() {
            anyhow::bail!("memory-only session updates cannot commit a terminal status");
        }
        self.update_session_status_inner(session_id, None, status, false)
            .await
    }

    async fn update_session_status_inner(
        &self,
        session_id: &str,
        expected: Option<SessionStatus>,
        status: SessionStatus,
        persist: bool,
    ) -> anyhow::Result<bool> {
        let Some(actor) = self.actor_for(session_id).await else {
            return Ok(false);
        };
        let transition = match expected {
            Some(expected) => actor.transition_if(expected, status, persist).await?,
            None => actor.transition(status, persist).await?,
        };
        if transition.pending {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
        if !transition.changed {
            return Ok(false);
        }
        if transition.terminal {
            self.cancel_direct_session_run_admission_waiters(session_id)
                .await;
            self.dequeue_pending(session_id).await;
            // A terminal status can be requested while the run is still
            // unwinding. Defer cleanup until `unmark_running` observes the
            // actual run exit; this is what makes end/stop responsive without
            // allowing the old run to race a newly opened session.
            if !actor.is_running().await {
                self.finish_idle_terminal_state(session_id, &actor, false, None)
                    .await;
            }
        }
        Ok(true)
    }

    /// Finish terminal cleanup only after claiming the session under the
    /// lifecycle gate. The claim remains active while cleanup runs outside
    /// that gate because cascading child cleanup may acquire it recursively.
    async fn finish_idle_terminal_state(
        &self,
        session_id: &str,
        expected_actor: &actor::SessionActorHandle,
        remove_error_actor: bool,
        cascade: Option<bool>,
    ) -> bool {
        let cleanup = {
            let _lifecycle = self.lifecycle_guard().await;
            let Some(actor) = self.actor_for(session_id).await else {
                return false;
            };
            if !actor.same_instance(expected_actor) || actor.is_running().await {
                return false;
            }
            let Some(snapshot) = actor.snapshot().await else {
                return false;
            };
            if !snapshot.status.is_terminal() {
                return false;
            }
            self.begin_terminal_cleanup_locked(session_id)
                .map(|cleanup| {
                    let cascade = cascade.unwrap_or_else(|| {
                        self.terminal_cleanup_cascade_locked(session_id)
                            .unwrap_or(true)
                    });
                    (cleanup, cascade)
                })
        };
        let Some((cleanup, cascade)) = cleanup else {
            return false;
        };
        let mut cleanup = cleanup;
        cleanup.set_retry_policy(TerminalCleanupRetry {
            cascade: Some(cascade),
            remove_error_actor,
        });

        self.finish_terminal_cleanup(
            session_id,
            expected_actor,
            cleanup,
            remove_error_actor,
            cascade,
        )
        .await;
        true
    }

    async fn finish_terminal_cleanup(
        &self,
        session_id: &str,
        expected_actor: &actor::SessionActorHandle,
        mut cleanup: TerminalCleanupGuard,
        remove_error_actor: bool,
        cascade: bool,
    ) {
        self.dequeue_pending(session_id).await;
        if let Err(error) = self.partials.promote(session_id).await {
            tracing::warn!(session_id, error = %error, "failed to promote session partial");
        }
        self.cleanup_session_maps(session_id).await;
        self.finish_ended_session(session_id, cascade).await;

        let _lifecycle = self.lifecycle_guard().await;
        let Some(actor) = self.actor_for(session_id).await else {
            cleanup.mark_complete();
            drop(cleanup);
            return;
        };
        let Some(snapshot) = actor.snapshot().await else {
            cleanup.mark_complete();
            drop(cleanup);
            return;
        };
        let status = snapshot.status;
        if actor.same_instance(expected_actor)
            && !actor.is_running().await
            && (status == SessionStatus::Completed
                || (remove_error_actor && status == SessionStatus::Error))
        {
            self.remove_actor_locked(session_id).await;
        }
        cleanup.mark_complete();
        drop(cleanup);
    }

    /// Reconcile the actor's live state after the run bit has been cleared.
    /// `RunFinished` used to carry Pending/terminal booleans captured before a
    /// concurrent Continue or rollback could commit; all decisions here use a
    /// fresh snapshot under lifecycle admission.
    pub(crate) async fn reconcile_run_exit(
        &self,
        session_id: &str,
        expected_actor: &actor::SessionActorHandle,
    ) {
        self.reconcile_run_exit_with_retry(
            session_id,
            expected_actor,
            TerminalCleanupRetry {
                cascade: None,
                remove_error_actor: true,
            },
        )
        .await;
    }

    pub(crate) async fn retry_terminal_cleanup(
        &self,
        session_id: &str,
        retry: TerminalCleanupRetry,
    ) {
        let Some(actor) = self.actor_for(session_id).await else {
            return;
        };
        self.reconcile_run_exit_with_retry(session_id, &actor, retry)
            .await;
    }

    async fn reconcile_run_exit_with_retry(
        &self,
        session_id: &str,
        expected_actor: &actor::SessionActorHandle,
        retry: TerminalCleanupRetry,
    ) {
        enum Decision {
            Pending,
            Cleanup(TerminalCleanupGuard, bool, bool),
            None,
        }

        let decision = {
            let _lifecycle = self.lifecycle_guard().await;
            let Some(actor) = self.actor_for(session_id).await else {
                return;
            };
            if !actor.same_instance(expected_actor) || actor.is_running().await {
                return;
            }
            let Some(snapshot) = actor.snapshot().await else {
                return;
            };
            let status = snapshot.status;
            if status == SessionStatus::Pending && !self.is_session_closing(session_id) {
                self.enqueue_pending(session_id).await;
                Decision::Pending
            } else if status.is_terminal() {
                match (
                    retry
                        .cascade
                        .or_else(|| self.terminal_cleanup_cascade_locked(session_id)),
                    self.begin_terminal_cleanup_locked(session_id),
                ) {
                    (Some(cascade), Some(mut cleanup)) => {
                        cleanup.set_retry_policy(TerminalCleanupRetry {
                            cascade: Some(cascade),
                            remove_error_actor: retry.remove_error_actor,
                        });
                        Decision::Cleanup(cleanup, cascade, retry.remove_error_actor)
                    }
                    _ => Decision::None,
                }
            } else {
                Decision::None
            }
        };

        match decision {
            Decision::Pending => self.wake_dispatcher(),
            Decision::Cleanup(cleanup, cascade, remove_error_actor) => {
                self.finish_terminal_cleanup(
                    session_id,
                    expected_actor,
                    cleanup,
                    remove_error_actor,
                    cascade,
                )
                .await;
            }
            Decision::None => {}
        }
    }

    pub async fn cleanup_session_maps(&self, session_id: &str) {
        self.partials.forget_session(session_id).await;
        self.emit_event(SessionSupervisorEvent::SessionCleanup {
            session_id: session_id.to_string(),
        });
        if self
            .get_active_session_status(session_id)
            .await
            .is_none_or(|status| status.is_terminal())
        {
            self.release_managed_assets_for_session(session_id);
        }
    }

    pub async fn get_session(&self, session_id: &str) -> Option<SessionInfo> {
        let mut session = self.actor_for(session_id).await?.snapshot().await?;
        session.waiting_reason = self.waiting_reason(session_id).await;
        (!session.status.is_terminal()).then_some(session)
    }

    /// Return the derived reason a paused session is waiting. The actor-owned
    /// value covers explicit interruption and pause boundaries; the fallback
    /// inspection makes recovery/list projections accurate after a restart.
    pub async fn waiting_reason(&self, session_id: &str) -> Option<SessionWaitingReason> {
        let actor = self.actor_for(session_id).await?;
        let session = actor.snapshot().await?;
        if session.status != SessionStatus::Paused {
            return None;
        }
        if session.waiting_reason.is_some() {
            return session.waiting_reason;
        }

        let interactions = actor.interactions(None, true).await;
        if interactions
            .iter()
            .any(|request| request.kind == crate::interaction::InteractionKind::Ask)
        {
            return Some(SessionWaitingReason::Ask);
        }
        if interactions
            .iter()
            .any(|request| request.kind == crate::interaction::InteractionKind::Confirm)
        {
            return Some(SessionWaitingReason::Confirmation);
        }
        if self
            .scheduled_confirms
            .lock()
            .await
            .values()
            .any(|request| {
                request.session_id.as_deref() == Some(session_id)
                    && request.status == crate::interaction::InteractionStatus::Pending
            })
        {
            return Some(SessionWaitingReason::ScheduledConfirmation);
        }

        for tool_run in self.tool_runs.list_for_session_views(session_id).await {
            if !tool_run.status.is_live() {
                continue;
            }
            return match tool_run.kind {
                haven_tools::ToolRunKind::Scheduled => Some(SessionWaitingReason::ScheduledTask),
                haven_tools::ToolRunKind::Background => Some(SessionWaitingReason::BackgroundTask),
            };
        }
        Some(SessionWaitingReason::UserInput)
    }

    /// Set the non-durable reason attached to the current paused projection.
    /// A non-paused actor ignores the value, keeping the field derived from
    /// lifecycle state rather than allowing stale reason data to leak.
    pub async fn set_waiting_reason(
        &self,
        session_id: &str,
        reason: Option<SessionWaitingReason>,
    ) -> anyhow::Result<()> {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.set_waiting_reason(reason).await?;
        }
        Ok(())
    }

    pub async fn ensure_session_loaded(self: &Arc<Self>, session_id: &str) -> anyhow::Result<()> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_session_loaded_locked(session_id).await
    }

    pub(crate) async fn ensure_session_loaded_locked(
        self: &Arc<Self>,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.ensure_lifecycle_open()?;
        if self.is_session_closing(session_id) {
            anyhow::bail!("session '{}' is closing; retry after deletion", session_id);
        }
        if self.actor_for(session_id).await.is_some() {
            if self
                .get_session_status(session_id)
                .await
                .is_some_and(SessionStatus::is_terminal)
            {
                // Error sessions can retain an idle actor for Continue. The
                // terminal edge cleared its process-local grants, so restore
                // the durable per-session set before reopening or continuing.
                self.restore_session_authorization_grants_for_locked(session_id)
                    .await?;
            }
            return Ok(());
        }
        let record = self
            .store
            .session_record(session_id)?
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found in database", session_id))?;
        self.install_actor(SessionInfo::from_db_record(&record))
            .await?;
        Ok(())
    }

    /// Retry the complete durable pending-session read after a prior batch
    /// failure. Per-session actor replay failures are intentionally contained
    /// by `load_pending_sessions` and therefore never repeat the whole batch.
    pub(crate) async fn recover_pending_sessions_with_retry(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
        mut completed_failures: u32,
    ) -> Option<usize> {
        let backoff = BackoffPolicy::new(
            PENDING_SESSION_RECOVERY_INITIAL_BACKOFF,
            2,
            PENDING_SESSION_RECOVERY_MAX_BACKOFF,
        );

        loop {
            if cancellation.is_cancelled() {
                return None;
            }
            if completed_failures > 0 {
                #[cfg(test)]
                self.pending_session_recovery_backoff_started.notify_one();
                let delay = backoff.delay_after(completed_failures, None, 0);
                tokio::select! {
                    _ = cancellation.cancelled() => return None,
                    _ = tokio::time::sleep(delay) => {}
                }
            }
            if cancellation.is_cancelled() {
                return None;
            }

            match self.load_pending_sessions().await {
                Ok(loaded) => return Some(loaded),
                Err(error) => {
                    completed_failures = completed_failures.saturating_add(1);
                    let next_delay = backoff.delay_after(completed_failures, None, 0);
                    tracing::warn!(
                        %error,
                        attempt = completed_failures,
                        retry_delay = ?next_delay,
                        "pending session batch recovery failed; retry scheduled"
                    );
                }
            }
        }
    }

    pub async fn load_pending_sessions(self: &Arc<Self>) -> anyhow::Result<usize> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;

        #[cfg(test)]
        {
            self.pending_session_recovery_attempts
                .fetch_add(1, Ordering::SeqCst);
            if self
                .pending_session_recovery_failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                anyhow::bail!("injected pending session batch read failure");
            }
        }

        let pending = self.store.pending_session_records()?;
        let mut loaded = 0;
        let mut found_pending_actor = false;
        for record in pending {
            if self.is_session_closing(&record.id) {
                continue;
            }
            let actor = match self.actor_for(&record.id).await {
                Some(actor) => actor,
                None => match self
                    .install_actor(SessionInfo::from_db_record(&record))
                    .await
                {
                    Ok(actor) => {
                        loaded += 1;
                        actor
                    }
                    Err(error) => {
                        tracing::warn!(
                            session_id = %record.id,
                            %error,
                            "failed to restore pending session; retry requires a later load attempt"
                        );
                        continue;
                    }
                },
            };
            if actor
                .snapshot()
                .await
                .is_some_and(|snapshot| snapshot.status == SessionStatus::Pending)
            {
                self.enqueue_pending(&record.id).await;
                found_pending_actor = true;
            }
        }
        if found_pending_actor {
            self.wake_dispatcher();
        }
        Ok(loaded)
    }

    pub async fn get_active_session_status(&self, session_id: &str) -> Option<SessionStatus> {
        let status = self.get_session_status(session_id).await?;
        (!status.is_terminal()).then_some(status)
    }

    /// Return the actor's exact status, including terminal states. The active
    /// status view intentionally hides Completed because it means the session
    /// is no longer in the working set; lifecycle operations such as reopen and
    /// ingress cleanup still need the terminal value to avoid resurrecting it.
    pub async fn get_session_status(&self, session_id: &str) -> Option<SessionStatus> {
        self.actor_for(session_id)
            .await?
            .snapshot()
            .await
            .map(|session| session.status)
    }

    pub async fn session_is_live(&self, session_id: &str) -> bool {
        self.get_active_session_status(session_id).await.is_some()
    }
    pub(crate) fn session_store(&self) -> haven_memory::SessionStore {
        self.store.clone()
    }

    /// Return the live ToolRun capability needed by Agent background
    /// consumers. This is intentionally narrower than exposing ToolServices.
    pub(crate) fn tool_run_service(&self) -> Arc<ToolRunService> {
        self.tool_runs.clone()
    }

    #[cfg(test)]
    pub(crate) fn tool_catalog_for_test(&self) -> Arc<dyn crate::react::ToolCatalogPort> {
        self.tool_catalog.clone()
    }

    pub async fn cancel_session_tool_runs(&self, session_id: &str) {
        if let Err(error) = self
            .tool_runs
            .cancel_owned_by_session_checked(session_id)
            .await
        {
            tracing::warn!(session_id = %session_id, "failed to completely cancel session-owned tool_runs: {error}");
        }
    }

    /// Fail closed for destructive lifecycle paths when a durable scheduled
    /// cancellation cannot be confirmed. The caller must preserve the
    /// durable session so cleanup can be retried.
    pub async fn cancel_session_tool_runs_checked(&self, session_id: &str) -> anyhow::Result<()> {
        self.tool_runs
            .cancel_owned_by_session_checked(session_id)
            .await
    }

    pub async fn cancel_session_background_tool_runs(&self, session_id: &str) {
        self.tool_runs
            .cancel_owned_background_by_session(session_id)
            .await;
    }

    pub async fn fail_pending_tool_run_steps(&self, session_id: &str, observation: &str) {
        let _ = self
            .store
            .fail_pending_tool_run_steps(session_id, observation)
            .await;
    }
}
