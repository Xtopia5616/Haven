use super::*;

#[derive(Debug)]
pub(crate) struct BackgroundShellRequest<'a> {
    pub(crate) command: &'a str,
    pub(crate) shell: &'a str,
    pub(crate) max_chars: usize,
    pub(crate) cwd: Option<std::path::PathBuf>,
    pub(crate) session_id: Option<&'a str>,
    pub(crate) source_step_id: Option<&'a str>,
}

impl ToolRunService {
    /// Spawn a shell command as a background ToolRun. Returns the ToolRun ID; the
    /// command keeps running after this function returns. `cwd` overrides the
    /// default Temp working directory when provided.
    pub async fn spawn_shell(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
    ) -> anyhow::Result<String> {
        self.spawn_shell_for_session(command, shell, max_chars, cwd, None)
            .await
    }

    /// Spawn a background ToolRun with its owner bound before the process is
    /// published. Agent calls should use this variant so session shutdown can
    /// cancel a process even if it exits during the tool-result projection.
    pub async fn spawn_shell_for_session(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
        session_id: Option<&str>,
    ) -> anyhow::Result<String> {
        self.spawn_shell_for_session_with_source(command, shell, max_chars, cwd, session_id, None)
            .await
    }

    /// Spawn a background ToolRun linked to the Agent tool step that created it.
    /// The optional source identity is persisted before the child starts so the
    /// relation survives fast completion and restart reconciliation.
    pub(crate) async fn spawn_shell_for_session_with_source(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
        session_id: Option<&str>,
        source_step_id: Option<&str>,
    ) -> anyhow::Result<String> {
        let request = BackgroundShellRequest {
            command,
            shell,
            max_chars,
            cwd,
            session_id,
            source_step_id,
        };
        let cancel = CancellationToken::new();
        self.spawn_shell_for_session_with_source_and_cancel(request, &cancel)
            .await
    }

    /// Spawn from a live tool execution that can be cancelled by its session.
    /// The token is checked while waiting for the shared admission gate and
    /// again after acquisition. If admission won first, session cleanup waits
    /// for the gate and cancels the published ToolRun; if end won first, the
    /// abandoned run cannot start a new ToolRun after cleanup releases the gate.
    pub(crate) async fn spawn_shell_for_session_with_source_and_cancel(
        self: &Arc<Self>,
        request: BackgroundShellRequest<'_>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<String> {
        let BackgroundShellRequest {
            command,
            shell,
            max_chars,
            cwd,
            session_id,
            source_step_id,
        } = request;
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("ToolRun service is shutting down");
        }
        if command.trim().is_empty() {
            anyhow::bail!("command is required");
        }
        if cancel.is_cancelled() {
            anyhow::bail!("background ToolRun admission was cancelled");
        }
        // Unpredictable ToolRun ID: a sequential counter would let any
        // session's agent enumerate and read other sessions' background outputs
        // through status (which is RiskLevel::Safe).
        let id = haven_common::types::new_id("toolrun");
        let started_at = chrono::Utc::now().to_rfc3339();
        let (kill_tx, kill_rx) = oneshot::channel();
        let tail = self.output_port.new_tail().await;
        let emit_interval = *self.tool_run_output_emit_interval.read().await;
        let terminal_ttl = *self.tool_run_terminal_ttl.read().await;
        let max_tool_runs = *self.max_tool_runs.read().await;
        let mut spawn_gate = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                anyhow::bail!("background ToolRun admission was cancelled");
            }
            guard = self.spawn_gate.clone().lock_owned() => guard,
        };
        if cancel.is_cancelled() {
            anyhow::bail!("background ToolRun admission was cancelled");
        }
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("ToolRun service is shutting down");
        }
        {
            let mut tool_runs = self.tool_runs.write().await;
            // Reap terminal entries first: their results were already
            // delivered via the completion channel, so they must not occupy
            // the cap forever (64 lifetime tool_runs would otherwise brick the
            // feature for long-lived sessions). Terminal entries older than
            // the configured terminal ToolRun TTL are dropped the same way (the
            // UI panel and the persisted log files remain the record after
            // that).
            tool_runs.retain(|_, e| !terminal_entry_stale(e, terminal_ttl));
            let running = tool_runs
                .values()
                .filter(|e| matches!(e.state, ToolRunState::Running { .. }))
                .count();
            if running >= max_tool_runs {
                anyhow::bail!(
                    "too many running background tool_runs (limit {})",
                    max_tool_runs
                );
            }
        }

        // Persist before publishing the ToolRun to the in-memory board or
        // starting a process. A failed database write therefore cannot leave a
        // process that restore_after_restart does not know how to clean up.
        if let Some(store) = self.tool_run_store.read().await.clone() {
            // A caller can be dropped while SQLite's blocking worker is still
            // writing. Let an owned admission worker retain the mutation gate
            // until the write finishes; session cleanup then waits for it and
            // reconciles any durable row that was committed before the caller
            // was cancelled.
            let persist_gate = spawn_gate;
            let persist_id = id.clone();
            let persist_session_id = session_id.map(str::to_owned);
            let persist_command = command.to_string();
            let persist_started_at = started_at.clone();
            let persist_source_step_id = source_step_id.map(str::to_owned);
            let persisted = tokio::spawn(async move {
                let result = store
                    .save_background_tool_run_with_source(
                        persist_id,
                        persist_session_id,
                        persist_command,
                        persist_started_at,
                        persist_source_step_id,
                    )
                    .await;
                (persist_gate, result)
            })
            .await
            .map_err(|error| {
                anyhow::anyhow!("background ToolRun persistence worker failed: {error}")
            })?;
            let (gate, result) = persisted;
            spawn_gate = gate;
            if let Err(error) = result {
                tracing::warn!(tool_run_id = %id, "failed to persist ToolRun spawn: {error}");
                return Err(error);
            }
        }

        self.tool_runs.write().await.insert(
            id.clone(),
            ToolRunEntry {
                kind: ToolRunKind::Background,
                session_id: session_id.map(str::to_owned),
                source_step_id: source_step_id.map(str::to_owned),
                state: ToolRunState::Running {
                    started_at: started_at.clone(),
                },
                kill: Some(kill_tx),
                tail: Some(tail.clone()),
                command: command.to_string(),
                shell: shell.to_string(),
                scheduled: None,
            },
        );

        let mut std_cmd = build_shell_command(shell, command);
        if let Some(cwd) = cwd {
            std_cmd.current_dir(cwd);
        }

        let containment = match haven_platform::process_containment::ProcessContainment::new() {
            Ok(containment) => containment,
            Err(error) => {
                self.rollback_background_registration(&id).await;
                return Err(error.into());
            }
        };
        let mut child_cmd = tokio::process::Command::from(std_cmd);
        #[cfg(windows)]
        let windows_creation_flags = crate::CREATE_NO_WINDOW;
        #[cfg(not(windows))]
        let windows_creation_flags = 0;
        containment.prepare_command(child_cmd.as_std_mut(), windows_creation_flags);
        let mut child = match child_cmd.kill_on_drop(true).spawn() {
            Ok(c) => c,
            Err(e) => {
                // Spawn failed: remove the entry so the ToolRun is not left
                // dangling as "running".
                self.rollback_background_registration(&id).await;
                return Err(e.into());
            }
        };
        let Some(pid) = child.id() else {
            self.rollback_background_registration(&id).await;
            return Err(anyhow::anyhow!(
                "background shell child did not expose a process id"
            ));
        };
        #[cfg(windows)]
        let attach_result = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("background child process handle is unavailable"))
            .and_then(|handle| containment.attach_and_resume(pid, handle));
        #[cfg(not(windows))]
        let attach_result = containment.attach_and_resume(pid, ());
        if let Err(error) = attach_result {
            let _ = child.kill().await;
            self.rollback_background_registration(&id).await;
            return Err(anyhow::anyhow!(
                "failed to attach background shell to process containment: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            ));
        }

        let me = self.clone();
        let tool_run_id = id.clone();
        let shell_owned = shell.to_string();
        let command_owned = command.to_string();
        let mut created = ToolRunLifecyclePayload::new(
            ToolRunKind::Background,
            tool_run_id.clone(),
            ToolRunLifecycleState::Running {
                started_at: started_at.clone(),
            },
        );
        created.session_id = session_id.map(str::to_owned);
        created.source_step_id = source_step_id.map(str::to_owned);
        self.emit(ToolRunLifecycleEvent::Created(created));
        // The direct child pid is captured before `run` moves `child`; on
        // Windows, cancelling must kill the whole process tree, not just the
        // cmd.exe/powershell.exe wrapper.
        let child_pid = child.id();
        // The ToolRun runner outlives its spawner: give it a ToolRun-level span so
        // every log line emitted while the ToolRun runs/cancels (output-log
        // writes, completion) carries the ToolRun ID — parallel background ToolRuns
        // stay distinguishable in logs.
        let tool_run_span = tracing::info_span!("background_tool_run", tool_run_id = %tool_run_id);
        let runner_tail = tail.clone();
        let emit_tool_run_id = tool_run_id.clone();
        let shutdown_token = self.shutdown_token.clone();
        tokio::spawn(async move {
            // Keep the Job Object alive for the entire ToolRun. Its
            // kill-on-close flag then cleans up descendants on cancellation
            // or application shutdown.
            let _containment = containment;
            // The ToolRun outlives this session: when `run` is dropped (kill signal
            // received), kill_on_drop terminates the child.
            let max_collect = collect_byte_cap(max_chars);
            let stdout_tail = runner_tail.clone();
            let stderr_tail = runner_tail.clone();
            let stdout_fut = read_stream_text_capped(
                child.stdout.take(),
                max_collect,
                Some(stdout_tail),
            );
            let stderr_fut = read_stream_text_capped(
                child.stderr.take(),
                max_collect,
                Some(stderr_tail),
            );
            let run = async {
                let (stdout, stderr) = tokio::join!(stdout_fut, stderr_fut);
                let status = child.wait().await;
                let truncated = stdout.overflowed || stderr.overflowed;
                let mut combined = stdout.text;
                if !stderr.text.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr.text);
                }
                // Strip PowerShell's NativeCommandError/CLIXML formatting so
                // the payload carries the real message, not the noise.
                combined = sanitize_shell_output(&combined, &shell_owned);
                let exit_code = status.as_ref().ok().and_then(|s| s.code());
                let success = matches!(status, Ok(s) if s.success());
                (combined, success, exit_code, truncated)
            };
            tokio::pin!(run);
            tokio::select! {
                _ = kill_rx => {
                    // Dropping `run` drops the pipes and the child
                    // (kill_on_drop), terminating the command.
                    if let Some(pid) = child_pid {
                        kill_process_tree(pid).await;
                    }
                    me.mark_cancelled(&tool_run_id, &started_at).await;
                }
                _ = shutdown_token.cancelled() => {
                    if let Some(pid) = child_pid {
                        kill_process_tree(pid).await;
                    }
                    me.mark_cancelled(&tool_run_id, &started_at).await;
                }
                (combined, success, exit_code, truncated) = &mut run => {
                    me.mark_finished(&tool_run_id, &started_at, &shell_owned, &command_owned, combined, success, exit_code, truncated).await;
                }
            }
        }.instrument(tool_run_span));

        // Live-output preview: emit `tool_run:output` when the bounded tail
        // changes (by value — length alone freezes once the window is full).
        let emit_me = self.clone();
        let emit_tail = tail;
        let shutdown_token = self.shutdown_token.clone();
        tokio::spawn(async move {
            let mut last_output = ToolRunTailSnapshot::default();
            loop {
                tokio::select! {
                    _ = shutdown_token.cancelled() => return,
                    _ = tokio::time::sleep(emit_interval) => {}
                }
                let source_step_id = match emit_me.status_view(&emit_tool_run_id).await {
                    ToolRunStatusView::Background {
                        state: ToolRunStateView::Running { .. },
                        source_step_id,
                        ..
                    } => source_step_id,
                    ToolRunStatusView::Scheduled {
                        state: ToolRunStateView::Running { .. },
                        ..
                    } => None,
                    _ => return,
                };
                if emit_tail.snapshot_if_changed(&mut last_output) {
                    emit_me.emit(ToolRunLifecycleEvent::Output(ToolRunOutputPayload {
                        tool_run_id: emit_tool_run_id.clone(),
                        source_step_id,
                        output: last_output.as_str().to_string(),
                    }));
                }
            }
        });

        drop(spawn_gate);
        Ok(id)
    }

    /// Associate a ToolRun with its owning session. Called by the session executor
    /// after a background tool call so `cancel_for_session` can clean it up.
    ///
    /// If completion committed before binding, the transactional outbox owner
    /// update makes the pending result recoverable for this session, including
    /// when the no-owner completion was already acknowledged. Binding never
    /// republishes a terminal completion in persistent mode.
    pub async fn attach_session(&self, tool_run_id: &str, session_id: &str) {
        let _mutation = self.spawn_gate.lock().await;
        {
            let tool_runs = self.tool_runs.read().await;
            let Some(entry) = tool_runs.get(tool_run_id) else {
                return;
            };
            if entry.kind != ToolRunKind::Background {
                return;
            }
            if let Some(existing) = entry.session_id.as_deref() {
                if existing == session_id {
                    return;
                }
                tracing::warn!(
                    tool_run_id,
                    existing_session_id = existing,
                    requested_session_id = session_id,
                    "refusing to rebind background ToolRun to another session"
                );
                return;
            }
        }
        // Record the owning session in the persisted row too, so terminal
        // history and any undelivered completion keep their owner (spawn rows
        // start with session_id NULL). Do not update memory if the transaction
        // fails; otherwise the runtime could claim a binding the outbox lacks.
        let tool_run_store = self.tool_run_store.read().await.clone();
        if let Some(store) = &tool_run_store
            && let Err(e) = store
                .bind_background_tool_run_session(tool_run_id.to_string(), session_id.to_string())
                .await
        {
            tracing::warn!(
                tool_run_id,
                "failed to persist ToolRun session binding: {e}"
            );
            return;
        }
        let (terminal_state, source_step_id) = {
            let mut tool_runs = self.tool_runs.write().await;
            let Some(entry) = tool_runs.get_mut(tool_run_id) else {
                return;
            };
            if entry.kind != ToolRunKind::Background || entry.session_id.is_some() {
                return;
            }
            entry.session_id = Some(session_id.to_string());
            (
                entry.state.is_terminal().then(|| entry.state.clone()),
                entry.source_step_id.clone(),
            )
        };
        self.emit(ToolRunLifecycleEvent::Updated(
            ToolRunLifecycleUpdate::SessionAttached(ToolRunSessionAttachedPayload {
                tool_run_id: tool_run_id.to_string(),
                session_id: session_id.to_string(),
                source_step_id: source_step_id.clone(),
            }),
        ));
        // Headless mode has no durable outbox to recover a completion that was
        // first published without an owner. Re-notify only with the newly
        // bound owner; persistent mode relies on the updated outbox row.
        if tool_run_store.is_none()
            && let Some(state) = terminal_state
        {
            self.publish_background_completion(
                tool_run_id,
                state,
                Some(session_id.to_string()),
                source_step_id,
            );
        }
    }

    /// Cancel and drop every background ToolRun owned by `session_id`.
    ///
    /// Running tool_runs are killed, marked cancelled, persisted, and surfaced to
    /// the UI via `tool_run:finished` before leaving the board — otherwise the
    /// titlebar panel keeps a ghost "running" row that cannot be stopped.
    pub async fn cancel_owned_background_by_session(self: &Arc<Self>, session_id: &str) {
        let _mutation = self.spawn_gate.lock().await;
        self.cancel_owned_background_by_session_locked(session_id)
            .await;
    }

    /// Cancel background tool_runs while the caller holds `spawn_gate`.
    ///
    /// Explicit session cleanup owns the gate across both background and
    /// scheduled tool_runs so an admission cannot publish an ToolRun after its
    /// owner cleanup has already taken its snapshot. Persistence remains
    /// best-effort for background work; terminal write failures keep using the
    /// existing bounded retry path.
    pub(super) async fn cancel_owned_background_by_session_locked(
        self: &Arc<Self>,
        session_id: &str,
    ) {
        let service = Arc::clone(self);
        let owner = session_id.to_string();
        let selection = self
            .cancel_owned_live_tool_runs(session_id, ToolRunKind::Background, move |id| {
                let service = Arc::clone(&service);
                let owner = owner.clone();
                async move {
                    service.cancel_owned_background_tool_run(&id, &owner).await;
                }
            })
            .await;
        self.drop_owned_terminal_background_tool_runs(&selection.terminal_ids, session_id)
            .await;

        // A tool can be cancelled after its durable INSERT began but before
        // it publishes a board entry or launches the child. Admission keeps
        // `spawn_gate` through that SQLite worker, so this durable read runs
        // after any such write and can cancel rows that never reached memory.
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return;
        };
        match store
            .list_tool_runs_for_session(session_id.to_string(), Some("background".to_string()))
            .await
        {
            Ok(rows) => {
                for row in rows
                    .into_iter()
                    .filter(|row| row.kind == "background" && row.status == ToolRunStatus::Running)
                {
                    let missing = {
                        let mut tool_runs = self.tool_runs.write().await;
                        if tool_runs.contains_key(&row.id) {
                            false
                        } else {
                            tool_runs.insert(
                                row.id.clone(),
                                ToolRunEntry {
                                    kind: ToolRunKind::Background,
                                    session_id: Some(session_id.to_string()),
                                    source_step_id: row.source_step_id.clone(),
                                    state: ToolRunState::Running {
                                        started_at: row
                                            .started_at
                                            .clone()
                                            .unwrap_or_else(|| row.created_at.clone()),
                                    },
                                    kill: None,
                                    tail: None,
                                    command: row.command.clone().unwrap_or_default(),
                                    shell: String::new(),
                                    scheduled: None,
                                },
                            );
                            true
                        }
                    };
                    if missing {
                        self.cancel_owned_background_tool_run(&row.id, session_id)
                            .await;
                    }
                }
            }
            Err(error) => tracing::warn!(
                session_id,
                %error,
                "failed to reconcile durable session-owned background tool_runs"
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn mark_finished(
        self: &Arc<Self>,
        id: &str,
        started_at: &str,
        shell: &str,
        command: &str,
        combined: String,
        success: bool,
        exit_code: Option<i32>,
        truncated: bool,
    ) {
        let next = if success {
            ToolRunStatus::Completed
        } else {
            ToolRunStatus::Failed
        };
        let running = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && can_claim_terminal(entry.state.status(), next, TerminalSource::Live)
            })
        };
        if !running {
            return;
        }
        let state = {
            if success {
                TerminalTimestamps::now(started_at).build(TerminalPayload::Completed {
                    output: combined.clone(),
                    exit_code,
                    truncated,
                    // When the collected output was capped, the log file keeps
                    // the full transcript for inspection.
                    log_path: truncated.then(|| {
                        write_output_log("tool-run-logs", id, &combined)
                            .to_string_lossy()
                            .into_owned()
                    }),
                })
            } else {
                // The failure payload must not drown the model (or the user) in
                // progress-bar spam: `error` keeps the sanitized output for full
                // inspection, `error_reason` carries a short tail of the most
                // likely error lines plus a Windows-trap hint when one matches.
                // The full output always lands in a log file so the root cause
                // is recoverable even when the summary misses it.
                let diagnosed = append_windows_diagnostics(shell, command, &combined);
                TerminalTimestamps::now(started_at).build(TerminalPayload::Failed {
                    error: combined.clone(),
                    error_reason: summarize_error(&diagnosed, 1200),
                    log_path: Some(
                        write_output_log("tool-run-logs", id, &combined)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    exit_code,
                })
            }
        };
        debug_assert_eq!(state.status(), next);
        match self.try_commit_background_terminal(id, &state, false).await {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(tool_run_id = %id, "failed to persist ToolRun result: {error}");
                self.retry_background_terminal_persistence(id, state, false)
                    .await;
            }
        }
    }

    pub(super) async fn mark_cancelled(self: &Arc<Self>, id: &str, started_at: &str) {
        let running = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && can_claim_terminal(
                        entry.state.status(),
                        ToolRunStatus::Cancelled,
                        TerminalSource::Live,
                    )
            })
        };
        if !running {
            return;
        }
        let state = TerminalTimestamps::now(started_at).build(TerminalPayload::Cancelled);
        match self.try_commit_background_terminal(id, &state, false).await {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(tool_run_id = %id, "failed to persist ToolRun cancellation: {error}");
                self.retry_background_terminal_persistence(id, state, false)
                    .await;
            }
        }
    }

    async fn cancel_owned_background_tool_run(self: &Arc<Self>, id: &str, session_id: &str) {
        let started_at = {
            let mut tool_runs = self.tool_runs.write().await;
            let Some(entry) = tool_runs.get_mut(id) else {
                return;
            };
            if entry.kind != ToolRunKind::Background
                || entry.session_id.as_deref() != Some(session_id)
            {
                return;
            }
            if let Some(tx) = entry.kill.take() {
                let _ = tx.send(());
            }
            match &entry.state {
                ToolRunState::Running { started_at } => Some(started_at.clone()),
                ToolRunState::Completed { .. }
                | ToolRunState::Failed { .. }
                | ToolRunState::Cancelled { .. }
                | ToolRunState::Waiting => None,
            }
        };
        let Some(started_at) = started_at else {
            // If the ToolRun finished after selection, it already published its
            // terminal event. Cleanup only drops the board entry.
            let mut tool_runs = self.tool_runs.write().await;
            if tool_runs.get(id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && entry.session_id.as_deref() == Some(session_id)
                    && entry.state.is_terminal()
            }) {
                tool_runs.remove(id);
            }
            return;
        };

        let state = TerminalTimestamps::now(started_at).build(TerminalPayload::Cancelled);
        if let Err(error) = self.try_commit_background_terminal(id, &state, true).await {
            tracing::warn!(
                tool_run_id = %id,
                "failed to persist session cleanup cancellation: {error}"
            );
            self.retry_background_terminal_persistence(id, state, true)
                .await;
        }
    }

    async fn drop_owned_terminal_background_tool_runs(&self, ids: &[String], session_id: &str) {
        let mut tool_runs = self.tool_runs.write().await;
        for id in ids {
            if tool_runs.get(id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && entry.session_id.as_deref() == Some(session_id)
                    && entry.state.is_terminal()
            }) {
                tool_runs.remove(id);
            }
        }
    }
}
