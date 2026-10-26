use super::*;

#[cfg(test)]
pub(crate) struct BackgroundShellRequest<'a> {
    pub(crate) command: &'a str,
    pub(crate) shell: &'a str,
    pub(crate) max_chars: usize,
    pub(crate) cwd: Option<std::path::PathBuf>,
    pub(crate) session_id: Option<&'a str>,
    pub(crate) source_step_id: Option<&'a str>,
    pub(crate) execution_authorization: Option<crate::security::ExecutionPolicyAuthorization>,
}

impl ToolRunService {
    /// Start an authorized non-shell tool invocation as a detached background
    /// ToolRun. The target is executed by the caller's Tools facade closure, so
    /// schema validation, authorization proof checks, retries, and circuit
    /// breaking remain on the ordinary tool path.
    pub(crate) async fn spawn_tool_invocation(
        self: &Arc<Self>,
        tool_name: String,
        source_step_id: Option<String>,
        admission_cancel: &CancellationToken,
        invocation: crate::ToolRunInvocation,
    ) -> anyhow::Result<String> {
        const MAX_RESULT_CHARS: usize = 100_000;

        if tool_name.trim().is_empty() || tool_name.len() > 128 {
            anyhow::bail!("background tool name is invalid");
        }
        if admission_cancel.is_cancelled() {
            anyhow::bail!("background ToolRun admission was cancelled");
        }
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("ToolRun service is shutting down");
        }

        let tool_run_id = haven_common::types::new_id("toolrun");
        let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (kill_tx, kill_rx) = oneshot::channel();
        let terminal_ttl = *self.tool_run_terminal_ttl.read().await;
        let max_running_background_tool_runs = *self.max_running_background_tool_runs.read().await;
        let spawn_gate = tokio::select! {
            biased;
            _ = admission_cancel.cancelled() => {
                anyhow::bail!("background ToolRun admission was cancelled");
            }
            guard = self.spawn_gate.clone().lock_owned() => guard,
        };
        if admission_cancel.is_cancelled() {
            anyhow::bail!("background ToolRun admission was cancelled");
        }
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("ToolRun service is shutting down");
        }
        {
            let mut tool_runs = self.tool_runs.write().await;
            tool_runs.retain(|_, entry| !terminal_entry_stale(entry, terminal_ttl));
            let running = tool_runs
                .values()
                .filter(|entry| matches!(entry.state, ToolRunState::Running { .. }))
                .count();
            if running >= max_running_background_tool_runs {
                anyhow::bail!(
                    "too many running background tool_runs (limit {})",
                    max_running_background_tool_runs
                );
            }
        }

        if let Some(store) = self.tool_run_store.read().await.clone() {
            let persist_gate = spawn_gate;
            let persist_tool_run_id = tool_run_id.clone();
            let persist_tool_name = tool_name.clone();
            let persist_started_at = started_at.clone();
            let persist_source_step_id = source_step_id.clone();
            let persisted = self
                .spawn_owned_task(async move {
                    let result = store
                        .persist_background_tool_run_with_metadata(
                            persist_tool_run_id,
                            persist_tool_name,
                            None,
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
            let (_gate, result) = persisted;
            result?;
        }

        self.tool_runs.write().await.insert(
            tool_run_id.clone(),
            ToolRunEntry {
                kind: ToolRunKind::Background,
                tool_name: tool_name.clone(),
                session_id: None,
                source_step_id: source_step_id.clone(),
                state: ToolRunState::Running {
                    started_at: started_at.clone(),
                },
                kill: Some(kill_tx),
                #[cfg(test)]
                tail: None,
                #[cfg(test)]
                command: String::new(),
                #[cfg(test)]
                shell: String::new(),
            },
        );
        let mut created = ToolRunLifecyclePayload::new(
            ToolRunKind::Background,
            tool_run_id.clone(),
            tool_name.clone(),
            ToolRunLifecycleState::Running {
                started_at: started_at.clone(),
            },
        );
        created.source_step_id = source_step_id.clone();
        self.emit(ToolRunLifecycleEvent::Created(created));

        let service = Arc::clone(self);
        let task_tool_run_id = tool_run_id.clone();
        let task_started_at = started_at.clone();
        let task_cancel = CancellationToken::new();
        let invoke = invocation(task_cancel.clone());
        let shutdown_token = self.shutdown_token.clone();
        self.spawn_owned_task(async move {
            tokio::pin!(invoke);
            tokio::select! {
                biased;
                _ = kill_rx => {
                    task_cancel.cancel();
                    service.mark_cancelled(&task_tool_run_id, &task_started_at).await;
                }
                _ = shutdown_token.cancelled() => {
                    task_cancel.cancel();
                    service.mark_cancelled(&task_tool_run_id, &task_started_at).await;
                }
                result = &mut invoke => {
                    let (text, success) = match result {
                        Ok(result) => {
                            let serialized = serde_json::json!({
                                "output": result.output,
                                "error": result.error,
                            }).to_string();
                            (serialized, result.success)
                        }
                        Err(error) => (
                            haven_common::error::sanitize_error_text(&error.to_string()),
                            false,
                        ),
                    };
                    let truncated = text.chars().count() > MAX_RESULT_CHARS;
                    let combined = text.chars().take(MAX_RESULT_CHARS).collect();
                    service
                        .mark_finished(
                            &task_tool_run_id,
                            &task_started_at,
                            combined,
                            success,
                            truncated,
                        )
                        .await;
                }
            }
        });
        Ok(tool_run_id)
    }

    /// Spawn a shell command as a background ToolRun. Returns the ToolRun ID; the
    /// command keeps running after this function returns. `cwd` overrides the
    /// default Temp working directory when provided.
    #[cfg(test)]
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
    #[cfg(test)]
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
    #[cfg(test)]
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
            execution_authorization: None,
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
    #[cfg(test)]
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
            execution_authorization,
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
        let tool_run_id = haven_common::types::new_id("toolrun");
        let started_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let (kill_tx, kill_rx) = oneshot::channel();
        let tail = self.output_port.new_tail().await;
        let terminal_ttl = *self.tool_run_terminal_ttl.read().await;
        let max_running_background_tool_runs = *self.max_running_background_tool_runs.read().await;
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
            if running >= max_running_background_tool_runs {
                anyhow::bail!(
                    "too many running background tool_runs (limit {})",
                    max_running_background_tool_runs
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
            let persist_tool_run_id = tool_run_id.clone();
            let persist_session_id = session_id.map(str::to_owned);
            let persist_command = command.to_string();
            let persist_started_at = started_at.clone();
            let persist_source_step_id = source_step_id.map(str::to_owned);
            let persisted = self
                .spawn_owned_task(async move {
                    let result = store
                        .persist_background_tool_run_with_source(
                            persist_tool_run_id,
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
                tracing::warn!(
                    tool_run_id = %tool_run_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to persist ToolRun spawn"
                );
                return Err(error);
            }
        }

        self.tool_runs.write().await.insert(
            tool_run_id.clone(),
            ToolRunEntry {
                kind: ToolRunKind::Background,
                tool_name: "shell".into(),
                session_id: session_id.map(str::to_owned),
                source_step_id: source_step_id.map(str::to_owned),
                state: ToolRunState::Running {
                    started_at: started_at.clone(),
                },
                kill: Some(kill_tx),
                tail: Some(tail.clone()),
                command: command.to_string(),
                shell: shell.to_string(),
            },
        );

        let mut std_cmd = build_shell_command(shell, command);
        if let Some(cwd) = cwd {
            std_cmd.current_dir(cwd);
        }

        let containment = match haven_platform::process_containment::ProcessContainment::new() {
            Ok(containment) => containment,
            Err(error) => {
                self.rollback_background_registration(&tool_run_id).await;
                return Err(error.into());
            }
        };
        let mut child_cmd = tokio::process::Command::from(std_cmd);
        #[cfg(windows)]
        let windows_creation_flags = crate::CREATE_NO_WINDOW;
        #[cfg(not(windows))]
        let windows_creation_flags = 0;
        containment.prepare_command(child_cmd.as_std_mut(), windows_creation_flags);
        let execution_permit = match execution_authorization.as_ref() {
            Some(authorization) => match authorization.acquire().await {
                Ok(permit) => Some(permit),
                Err(error) => {
                    self.rollback_background_registration(&tool_run_id).await;
                    return Err(crate::security::execution_policy_denied(error));
                }
            },
            None => None,
        };
        let child_spawn_result = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            child_cmd.kill_on_drop(true).spawn()
        };
        let mut child = match child_spawn_result {
            Ok(c) => c,
            Err(e) => {
                drop(execution_permit);
                // Spawn failed: remove the entry so the ToolRun is not left
                // dangling as "running".
                self.rollback_background_registration(&tool_run_id).await;
                return Err(e.into());
            }
        };
        let Some(pid) = child.id() else {
            drop(execution_permit);
            self.rollback_background_registration(&tool_run_id).await;
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
            drop(execution_permit);
            let _ = child.kill().await;
            self.rollback_background_registration(&tool_run_id).await;
            return Err(anyhow::anyhow!(
                "failed to attach background shell to process containment: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            ));
        }
        drop(execution_permit);

        let me = self.clone();
        let runner_tool_run_id = tool_run_id.clone();
        let shell_owned = shell.to_string();
        let mut created = ToolRunLifecyclePayload::new(
            ToolRunKind::Background,
            runner_tool_run_id.clone(),
            "shell",
            ToolRunLifecycleState::Running {
                started_at: started_at.clone(),
            },
        );
        created.session_id = session_id.map(str::to_owned);
        created.source_step_id = source_step_id.map(str::to_owned);
        self.emit(ToolRunLifecycleEvent::Created(created));
        // The ToolRun runner outlives its spawner: give it a ToolRun-level span so
        // every log line emitted while the ToolRun runs/cancels (output-log
        // writes, completion) carries the ToolRun ID — parallel background ToolRuns
        // stay distinguishable in logs.
        let tool_run_span = tracing::info_span!(
            "background_tool_run",
            tool_run_id = %runner_tool_run_id
        );
        let runner_tail = tail.clone();
        let shutdown_token = self.shutdown_token.clone();
        self.spawn_owned_task(
            async move {
                // Keep the Job Object alive for the entire ToolRun. Its
                // kill-on-close flag then cleans up descendants on cancellation
                // or application shutdown.
                let _containment = containment;
                // The ToolRun outlives this session: when `run` is dropped (kill signal
                // received), kill_on_drop terminates the child.
                let max_collect = collect_byte_cap(max_chars);
                let stdout_tail = runner_tail.clone();
                let stderr_tail = runner_tail.clone();
                let stdout_fut =
                    read_stream_text_capped(child.stdout.take(), max_collect, Some(stdout_tail));
                let stderr_fut =
                    read_stream_text_capped(child.stderr.take(), max_collect, Some(stderr_tail));
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
                    let success = matches!(status, Ok(s) if s.success());
                    (combined, success, truncated)
                };
                let policy_revoked = async {
                    match execution_authorization {
                        Some(authorization) => authorization.wait_for_policy_revocation().await,
                        None => std::future::pending::<()>().await,
                    }
                };
                tokio::pin!(policy_revoked);
                let completion = {
                    tokio::pin!(run);
                    tokio::select! {
                        _ = kill_rx => None,
                        _ = shutdown_token.cancelled() => None,
                        _ = &mut policy_revoked => None,
                        result = &mut run => Some(result),
                    }
                };
                if let Some((combined, success, truncated)) = completion {
                    me.mark_finished(
                        &runner_tool_run_id,
                        &started_at,
                        combined,
                        success,
                        truncated,
                    )
                    .await;
                } else {
                    // The child is already attached to this ToolRun's Job
                    // Object. Kill the shell handle, then close the object so
                    // Windows terminates every contained descendant without
                    // launching an unbounded PATH-resolved helper process.
                    let _ = child.kill().await;
                    drop(_containment);
                    me.mark_cancelled(&runner_tool_run_id, &started_at).await;
                }
            }
            .instrument(tool_run_span),
        );

        drop(spawn_gate);
        Ok(tool_run_id)
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
        // history and any undelivered completion keep their owner. Some
        // ToolRuns enter this path without an owner; do not update memory if
        // the transaction fails or the runtime could claim a binding the
        // outbox lacks.
        let tool_run_store = self.tool_run_store.read().await.clone();
        if let Some(store) = &tool_run_store
            && let Err(e) = store
                .bind_background_tool_run_session(tool_run_id.to_string(), session_id.to_string())
                .await
        {
            tracing::warn!(
                tool_run_id,
                error = %haven_common::error::sanitize_error_text(&e.to_string()),
                "failed to persist ToolRun session binding"
            );
            return;
        }
        let (terminal_state, status, source_step_id, entry_tool_name) = {
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
                entry.state.status(),
                entry.source_step_id.clone(),
                entry.tool_name.clone(),
            )
        };
        self.emit(ToolRunLifecycleEvent::Updated(
            ToolRunLifecycleUpdate::SessionAttached(ToolRunSessionAttachedPayload {
                tool_run_id: tool_run_id.to_string(),
                tool_name: entry_tool_name.clone(),
                session_id: session_id.to_string(),
                source_step_id: source_step_id.clone(),
                status: Some(status),
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
                entry_tool_name,
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
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        self.cancel_owned_background_by_session_locked(session_id)
            .await;
    }

    /// Cancel background tool_runs while the caller holds `spawn_gate`.
    ///
    /// Session cleanup owns the gate so an admission cannot publish a
    /// background ToolRun after its owner cleanup has taken its snapshot.
    /// Persistence remains best-effort for background work; terminal write failures keep using the
    /// existing bounded retry path.
    pub(super) async fn cancel_owned_background_by_session_locked(
        self: &Arc<Self>,
        session_id: &str,
    ) {
        let service = Arc::clone(self);
        let owner = session_id.to_string();
        let selection = self
            .cancel_owned_live_tool_runs(session_id, move |tool_run_id| {
                let service = Arc::clone(&service);
                let owner = owner.clone();
                async move {
                    service
                        .cancel_owned_background_tool_run(&tool_run_id, &owner)
                        .await;
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
                        if tool_runs.contains_key(&row.tool_run_id) {
                            false
                        } else {
                            tool_runs.insert(
                                row.tool_run_id.clone(),
                                ToolRunEntry {
                                    kind: ToolRunKind::Background,
                                    tool_name: row.tool_name.clone(),
                                    session_id: Some(session_id.to_string()),
                                    source_step_id: row.source_step_id.clone(),
                                    state: ToolRunState::Running {
                                        started_at: row
                                            .started_at
                                            .clone()
                                            .unwrap_or_else(|| row.created_at.clone()),
                                    },
                                    kill: None,
                                    #[cfg(test)]
                                    tail: None,
                                    #[cfg(test)]
                                    command: String::new(),
                                    #[cfg(test)]
                                    shell: String::new(),
                                },
                            );
                            true
                        }
                    };
                    if missing {
                        self.cancel_owned_background_tool_run(&row.tool_run_id, session_id)
                            .await;
                    }
                }
            }
            Err(error) => tracing::warn!(
                session_id,
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to reconcile durable session-owned background tool_runs"
            ),
        }
    }

    pub(super) async fn mark_finished(
        self: &Arc<Self>,
        tool_run_id: &str,
        started_at: &str,
        combined: String,
        success: bool,
        truncated: bool,
    ) {
        let next = if success {
            ToolRunStatus::Completed
        } else {
            ToolRunStatus::Failed
        };
        let running = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(tool_run_id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && can_claim_terminal(entry.state.status(), next)
            })
        };
        if !running {
            return;
        }
        let state = {
            if success {
                TerminalTimestamps::now(started_at).build(TerminalPayload::Completed {
                    output: combined.clone(),
                    truncated,
                    // When the collected output was capped, the log file keeps
                    // the full transcript for inspection.
                    log_path: truncated.then(|| {
                        write_output_log("tool-run-logs", tool_run_id, &combined)
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
                TerminalTimestamps::now(started_at).build(TerminalPayload::Failed {
                    error: combined.clone(),
                    error_reason: summarize_error(&combined, 1200),
                    log_path: Some(
                        write_output_log("tool-run-logs", tool_run_id, &combined)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                })
            }
        };
        debug_assert_eq!(state.status(), next);
        match self
            .try_commit_background_terminal(tool_run_id, &state, false)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tool_run_id = %tool_run_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to persist ToolRun result"
                );
                self.retry_background_terminal_persistence(tool_run_id, state, false)
                    .await;
            }
        }
    }

    pub(super) async fn mark_cancelled(self: &Arc<Self>, tool_run_id: &str, started_at: &str) {
        let running = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(tool_run_id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && can_claim_terminal(entry.state.status(), ToolRunStatus::Cancelled)
            })
        };
        if !running {
            return;
        }
        let state = TerminalTimestamps::now(started_at).build(TerminalPayload::Cancelled);
        match self
            .try_commit_background_terminal(tool_run_id, &state, false)
            .await
        {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    tool_run_id = %tool_run_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to persist ToolRun cancellation"
                );
                self.retry_background_terminal_persistence(tool_run_id, state, false)
                    .await;
            }
        }
    }

    async fn cancel_owned_background_tool_run(
        self: &Arc<Self>,
        tool_run_id: &str,
        session_id: &str,
    ) {
        let started_at = {
            let mut tool_runs = self.tool_runs.write().await;
            let Some(entry) = tool_runs.get_mut(tool_run_id) else {
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
                | ToolRunState::Cancelled { .. } => None,
            }
        };
        let Some(started_at) = started_at else {
            // If the ToolRun finished after selection, it already published its
            // terminal event. Cleanup only drops the board entry.
            let mut tool_runs = self.tool_runs.write().await;
            if tool_runs.get(tool_run_id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && entry.session_id.as_deref() == Some(session_id)
                    && entry.state.is_terminal()
            }) {
                tool_runs.remove(tool_run_id);
            }
            return;
        };

        let state = TerminalTimestamps::now(started_at).build(TerminalPayload::Cancelled);
        if let Err(error) = self
            .try_commit_background_terminal(tool_run_id, &state, true)
            .await
        {
            tracing::warn!(
                tool_run_id = %tool_run_id,
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to persist session cleanup cancellation"
            );
            self.retry_background_terminal_persistence(tool_run_id, state, true)
                .await;
        }
    }

    async fn drop_owned_terminal_background_tool_runs(&self, ids: &[String], session_id: &str) {
        let mut tool_runs = self.tool_runs.write().await;
        for tool_run_id in ids {
            if tool_runs.get(tool_run_id).is_some_and(|entry| {
                entry.kind == ToolRunKind::Background
                    && entry.session_id.as_deref() == Some(session_id)
                    && entry.state.is_terminal()
            }) {
                tool_runs.remove(tool_run_id);
            }
        }
    }
}
