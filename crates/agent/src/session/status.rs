//! Session lifecycle owned by [`SessionSupervisor`].

use super::*;

impl SessionSupervisor {
    pub async fn create_session(&self, input: &str) -> anyhow::Result<SessionInfo> {
        self.create_session_with_summary(input, input).await
    }

    pub async fn create_session_with_summary(
        &self,
        input: &str,
        summary: &str,
    ) -> anyhow::Result<SessionInfo> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        let record = self.db.create_session(input, input)?;
        let mut info = SessionInfo::from_db_record(&record);
        info.summary = summary.to_string();
        self.install_actor(info.clone()).await;
        self.enqueue_pending(&info.id).await;
        self.wake_dispatcher();
        Ok(info)
    }

    pub(super) async fn persist_status(
        db: &Arc<Database>,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        let mut last_error = None;
        for attempt in 0..3 {
            let db = db.clone();
            let session_id = session_id.to_string();
            match db
                .run_blocking(move |db| db.update_session_status(&session_id, status))
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("status persist failed")))
    }

    pub async fn end_session(&self, session_id: &str) -> anyhow::Result<SessionStatus> {
        self.end_session_inner(session_id, true).await
    }

    pub async fn interrupt_session(&self, session_id: &str) -> anyhow::Result<bool> {
        let status = self
            .get_session_state(session_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found", session_id))?;
        match status {
            SessionStatus::Running => {
                self.cancel_direct_waiters(session_id).await;
                let cancel = self.cancellation_token(session_id).await;
                // The UI command must not report success while a cancelled
                // tool/turn can still publish output or release the run slot.
                cancel.cancel();
                self.await_run_finished(session_id).await?;
                self.update_session_status(session_id, SessionStatus::Paused)
                    .await?;
                Ok(self.get_session_state(session_id).await == Some(SessionStatus::Paused))
            }
            SessionStatus::Pending => {
                self.update_session_status(session_id, SessionStatus::Paused)
                    .await?;
                Ok(self.get_session_state(session_id).await == Some(SessionStatus::Paused))
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
    ) -> anyhow::Result<SessionStatus> {
        let Some(actor) = self.actor_for(session_id).await else {
            self.cancel_direct_waiters(session_id).await;
            Self::persist_status(&self.db, session_id, SessionStatus::Completed).await?;
            self.finish_ended_session(session_id, cascade).await;
            return Ok(SessionStatus::Completed);
        };
        self.cancel_direct_waiters(session_id).await;
        actor.cancel().cancel();
        self.cancel_session_actions(session_id).await;
        // Keep terminal cleanup behind the run-exit fence. Otherwise the
        // frontend can clear the session while the old handler is still alive,
        // and a late tool result can race with the next session.
        self.await_run_finished(session_id).await?;
        if let Err(error) = self.partials.promote(session_id).await {
            tracing::warn!(session_id = %session_id, error = %error, "failed to promote session partial");
        }
        self.update_session_status(session_id, SessionStatus::Completed)
            .await?;
        Ok(SessionStatus::Completed)
    }

    async fn finish_ended_session(&self, session_id: &str, cascade: bool) {
        Self::unregister_from_inbox(session_id);
        self.tools.unregister_session(session_id).await;
        self.scheduled_confirms
            .lock()
            .await
            .retain(|request| request.session_id != session_id);
        if cascade && self.may_have_children(session_id).await {
            Box::pin(self.cascade_end_children(session_id)).await;
        }
        self.clear_has_children(session_id).await;
        self.tools
            .authorization()
            .clear_session_trust(session_id)
            .await;
    }

    fn unregister_from_inbox(session_id: &str) {
        let session_id = session_id.to_string();
        let log_id = session_id.clone();
        tokio::spawn(async move {
            let messaging = haven_tools::MessagingService::default_root();
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
                let messaging = haven_tools::MessagingService::default_root();
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
            if self.end_session_inner(&child_id, false).await.is_ok() {
                self.emit_event(SessionEvent::CascadeCompleted {
                    session_id: child_id,
                    title,
                });
            }
        }
    }

    pub async fn remove_session(&self, session_id: &str) -> anyhow::Result<()> {
        let _closing = self.begin_session_closing(session_id).await?;
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
        self.cancel_direct_waiters(session_id).await;
        if let Some(actor) = self.actor_for(session_id).await {
            actor.cancel().cancel();
            self.cancel_session_actions(session_id).await;
            self.dequeue_pending(session_id).await;
            self.await_run_finished(session_id).await?;
        }
        Ok(())
    }

    async fn remove_session_locked(&self, session_id: &str) -> anyhow::Result<()> {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.clear_runtime().await;
        }
        self.dequeue_pending(session_id).await;
        self.tools.unregister_session(session_id).await;
        self.tools
            .authorization()
            .clear_session_trust(session_id)
            .await;
        self.scheduled_confirms
            .lock()
            .await
            .retain(|request| request.session_id != session_id);
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
            if let Some(session) = actor.snapshot().await
                && !session.status.is_terminal()
            {
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
    /// A session-owned scheduled action is durable work, not a child process of
    /// the actor.  It must remain `waiting` so ActionService can restore it on
    /// the next startup.  Explicit session deletion/end still uses the regular
    /// quiesce path and cancels all owned actions.
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
            self.cancel_direct_waiters(&actor.id).await;
            actor.cancel().cancel();
            if preserve_scheduled {
                self.cancel_session_background_actions(&actor.id).await;
            } else {
                self.cancel_session_actions(&actor.id).await;
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
        self.tools.authorization().clear_all_trust().await;
        self.actors.lock().await.clear();
        self.pending_queue.lock().await.clear();
        self.scheduled_confirms.lock().await.clear();
        self.direct_run_waiters.lock().await.clear();
        Ok(())
    }

    /// Remove one session from both the actor registry and durable storage
    /// under one lifecycle gate. This is the only deletion entry point for
    /// app commands; it prevents ensure/load from reinstalling a stale actor
    /// between the in-memory quiesce and the SQL delete.
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        let _closing = self.begin_session_closing(session_id).await?;
        async {
            self.quiesce_session(session_id).await?;
            let _lifecycle = self.lifecycle_guard().await;
            self.remove_session_locked(session_id).await?;
            let db = self.db.clone();
            let session_id = session_id.to_string();
            db.run_blocking(move |db| db.delete_session(&session_id))
                .await
        }
        .await
    }

    /// Quiesce the working set and clear durable history while holding the
    /// same gate used by session creation/loading/deletion.
    pub async fn clear_sessions_and_delete(&self) -> anyhow::Result<usize> {
        let _block = self.begin_lifecycle_block()?;
        async {
            self.quiesce_all_sessions(false).await?;
            let _lifecycle = self.lifecycle_guard().await;
            self.clear_all_sessions_locked().await?;
            self.db.clone().run_blocking(|db| db.clear_sessions()).await
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
    ) -> anyhow::Result<SessionClosingGuard> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        let inserted = self
            .closing_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id.to_string());
        if !inserted {
            anyhow::bail!("session '{}' is already closing", session_id);
        }
        let closing = SessionClosingGuard {
            sessions: self.closing_sessions.clone(),
            session_id: session_id.to_string(),
        };
        self.cancel_direct_waiters(session_id).await;
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
        self.update_session_status_inner(session_id, status, true)
            .await
    }
    pub async fn update_session_status_memory_only(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<bool> {
        self.update_session_status_inner(session_id, status, false)
            .await
    }

    async fn update_session_status_inner(
        &self,
        session_id: &str,
        status: SessionStatus,
        persist: bool,
    ) -> anyhow::Result<bool> {
        let Some(actor) = self.actor_for(session_id).await else {
            return Ok(false);
        };
        let transition = actor.transition(status, persist).await?;
        if transition.pending {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
        if !transition.changed {
            return Ok(false);
        }
        if transition.terminal {
            self.cancel_direct_waiters(session_id).await;
            self.dequeue_pending(session_id).await;
            self.finish_ended_session(session_id, true).await;
            // Completed sessions are explicitly ended and leave the working
            // set. Error is retryable: keep an idle actor when the transition
            // happens outside the dispatcher so `continue_session` can inspect
            // and resume it without rebuilding a second runtime owner.
            if status == SessionStatus::Completed && !actor.is_running().await {
                let _lifecycle = self.lifecycle_guard().await;
                self.remove_actor_locked(session_id).await;
            }
        }
        Ok(true)
    }

    pub async fn cleanup_session_maps(&self, session_id: &str) {
        self.emit_event(SessionEvent::SessionCleanup {
            session_id: session_id.to_string(),
        });
        if self
            .get_session_state(session_id)
            .await
            .is_none_or(|status| status.is_terminal())
        {
            self.tools.release_managed_assets_for_session(session_id);
        }
    }

    pub async fn get_session(&self, session_id: &str) -> Option<SessionInfo> {
        let session = self.actor_for(session_id).await?.snapshot().await?;
        (!session.status.is_terminal()).then_some(session)
    }

    pub async fn ensure_session_loaded(&self, session_id: &str) -> anyhow::Result<()> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_session_loaded_locked(session_id).await
    }

    pub(crate) async fn ensure_session_loaded_locked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        self.ensure_lifecycle_open()?;
        if self.is_session_closing(session_id) {
            anyhow::bail!("session '{}' is closing; retry after deletion", session_id);
        }
        if self.actor_for(session_id).await.is_some() {
            return Ok(());
        }
        let record = self
            .db
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found in database", session_id))?;
        self.install_actor(SessionInfo::from_db_record(&record))
            .await;
        Ok(())
    }

    pub async fn load_pending_sessions(&self) -> anyhow::Result<usize> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        let pending = self
            .db
            .search_sessions_filtered(None, Some("pending"), None, None, -1, 0)?;
        let mut loaded = 0;
        for record in pending {
            if self.is_session_closing(&record.id) {
                continue;
            }
            if self.actor_for(&record.id).await.is_none() {
                self.install_actor(SessionInfo::from_db_record(&record))
                    .await;
                self.enqueue_pending(&record.id).await;
                loaded += 1;
            }
        }
        if loaded > 0 {
            self.wake_dispatcher();
        }
        Ok(loaded)
    }

    pub async fn get_session_state(&self, session_id: &str) -> Option<SessionStatus> {
        let status = self.get_session_status(session_id).await?;
        (!status.is_terminal()).then_some(status)
    }

    /// Return the actor's exact status, including terminal states. The
    /// compatibility `get_session_state` API intentionally hides Completed
    /// because it means the session is no longer in the active working set;
    /// lifecycle operations such as reopen and ingress cleanup still need the
    /// terminal value to avoid accidentally resurrecting it.
    pub async fn get_session_status(&self, session_id: &str) -> Option<SessionStatus> {
        self.actor_for(session_id)
            .await?
            .snapshot()
            .await
            .map(|session| session.status)
    }

    pub async fn session_is_live(&self, session_id: &str) -> bool {
        self.get_session_state(session_id).await.is_some()
    }
    pub fn get_tools(&self) -> Arc<ToolsManager> {
        self.tools.clone()
    }
    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    pub async fn cancel_session_actions(&self, session_id: &str) {
        self.tools
            .action_service()
            .cancel_owned_by_session(session_id)
            .await;
    }

    pub async fn cancel_session_background_actions(&self, session_id: &str) {
        self.tools
            .action_service()
            .cancel_owned_background_by_session(session_id)
            .await;
    }

    pub async fn fail_pending_action_steps(&self, session_id: &str, observation: &str) {
        let session_id = session_id.to_string();
        let observation = observation.to_string();
        let _ = self
            .db
            .run_blocking(move |db| db.fail_pending_action_steps(&session_id, &observation))
            .await;
    }
}
