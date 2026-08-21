//! Tool execution, safety-gated confirm waits, and action-step persistence.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract; behavior unchanged).

use super::*;

impl SessionExecutor {
    /// Persist a pending `session_steps` row under the pre-minted `step-*` id
    /// at Action-emit time — before the tool runs. Interrupted / cancelled
    /// tools never reach `execute_step`'s post-completion write, so without
    /// this the live card is the only copy and drops on every DB rebuild
    /// (Continue resync, session switch, app restart).
    pub async fn begin_action_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        step_id: &str,
    ) {
        let risk_level = self
            .tools
            .get_risk_level(Some(session_id), tool_name, input)
            .await;
        let silent = is_silent_action(tool_name, input);
        let step_number = step_num as i32;
        let session_id = session_id.to_string();
        let tool_name = tool_name.to_string();
        let tool_input = input.to_string();
        let step_id_owned = step_id.to_string();
        if let Err(e) = self
            .db
            .run_blocking(move |db| {
                db.ensure_action_step(
                    &session_id,
                    step_number,
                    &tool_name,
                    &tool_input,
                    risk_level != RiskLevel::Safe,
                    silent,
                    None,
                    &step_id_owned,
                )
            })
            .await
        {
            tracing::warn!("begin_action_step failed for step {}: {}", step_id, e);
        }
    }

    /// Persist an Interrupted observation onto the pending step row (creating
    /// it if Action-time begin failed). Keeps the review badge aligned with
    /// the live Interrupted card across resume/resync.
    pub async fn finish_interrupted_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        step_id: &str,
        observation: &str,
    ) {
        let risk_level = self
            .tools
            .get_risk_level(Some(session_id), tool_name, input)
            .await;
        let silent = is_silent_action(tool_name, input);
        let step_number = step_num as i32;
        let session_id = session_id.to_string();
        let tool_name = tool_name.to_string();
        let tool_input = input.to_string();
        let step_id_owned = step_id.to_string();
        let observation = observation.to_string();
        if let Err(e) = self
            .db
            .run_blocking(move |db| {
                db.ensure_action_step(
                    &session_id,
                    step_number,
                    &tool_name,
                    &tool_input,
                    risk_level != RiskLevel::Safe,
                    silent,
                    None,
                    &step_id_owned,
                )?;
                db.complete_action_step(&step_id_owned, &observation, false)
            })
            .await
        {
            tracing::warn!(
                "finish_interrupted_step failed for step {}: {}",
                step_id,
                e
            );
        }
    }

    /// Execute a tool step. `step_id` is the pre-minted `step-*` id the frontend's
    /// live tool card already uses; the persisted step row reuses it so the live
    /// card and the review badge are one entity. The pending row is normally
    /// created by [`Self::begin_action_step`] at Action emit; this method
    /// ensures + completes it after the tool finishes.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        step_id: &str,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(session_id, tool_name, input, step_num, step_id, None)
            .await
    }

    /// Like [`execute_step`], but skips the blocking confirm wait when
    /// `pre_confirmed` is set (Phase 5 / E3 resume after pause-confirm).
    pub async fn execute_step_preconfirmed(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        step_id: &str,
        pre_confirmed: bool,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_num,
            step_id,
            Some(pre_confirmed),
        )
        .await
    }

    async fn execute_step_inner(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        step_id: &str,
        pre_confirmed: Option<bool>,
    ) -> anyhow::Result<ToolResult> {
        tracing::debug!(
            "execute_step: session={} tool={} input={:?} pre_confirmed={:?}",
            session_id,
            tool_name,
            input,
            pre_confirmed
        );
        {
            let entry = { self.sessions.lock().await.get(session_id).cloned() };
            // Phase 7 / E4: only Running may execute tools. Missing session
            // (end_session already removed the entry) must fail closed — never
            // treat absence as permission to run.
            let refuse = match entry {
                None => Some(format!(
                    "execute_step: session {} not in working set; refusing to execute tool '{}'",
                    session_id, tool_name
                )),
                Some(entry) => {
                    let session = entry.lock().await;
                    let prev = session.status.clone();
                    if !matches!(prev, SessionStatus::Running) {
                        Some(format!(
                            "execute_step: session {} is {}; refusing to execute tool '{}'",
                            session_id,
                            prev.as_str(),
                            tool_name
                        ))
                    } else {
                        None
                    }
                }
            };
            if let Some(err) = refuse {
                self.finish_interrupted_step(
                    session_id,
                    tool_name,
                    &input,
                    step_num,
                    step_id,
                    &err,
                )
                .await;
                return Err(anyhow::anyhow!(err));
            }
        }

        let cancel = self.cancellation_token(session_id).await;
        let gated = match self
            .execute_gated(
                Some(session_id),
                tool_name,
                input.clone(),
                cancel,
                pre_confirmed,
            )
            .await
        {
            Ok(gated) => gated,
            Err(e) => {
                // Pending row was created at Action emit; record the failure
                // so resume/resync does not rebuild an empty tool badge.
                self.finish_interrupted_step(
                    session_id,
                    tool_name,
                    &input,
                    step_num,
                    step_id,
                    &e.to_string(),
                )
                .await;
                return Err(e);
            }
        };
        let ToolExecution {
            result,
            risk_level,
            confirmed,
        } = gated;
        tracing::info!(
            "execute_step result: tool={} success={}",
            tool_name,
            result.success
        );

        // Apply the tool's declared per-session side effects (skill/MCP adapter
        // registration) instead of name-matching load_skill/load_mcp here —
        // a new tool with a side effect declares it via `Tool::registrations`
        // and nothing in this executor needs to change. Background-action
        // bindings are applied after the running-set guard below (a action
        // spawned in a concurrently-rolled-back step must not attach past
        // the cleanup sweep). `registrations` is extracted ONCE: calling it
        // twice could yield divergent results for stateful tools, and the
        // variant split below is explicit rather than silently partitioned.
        let registrations = if result.success {
            self.tools
                .get_tool_for_session(Some(session_id), tool_name)
                .await
                .map(|t| t.registrations(&result.output))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for reg in &registrations {
            match reg {
                haven_tools::ToolRegistration::Skill(name) => {
                    self.tools
                        .register_skill_for_session(session_id, name)
                        .await;
                }
                haven_tools::ToolRegistration::McpServer(name) => {
                    self.tools.register_mcp_for_session(session_id, name).await;
                }
                // Action is applied after the running-set guard.
                haven_tools::ToolRegistration::Action(_) => {}
            }
        }
        let step_number = step_num as i32;
        // Guard against rollback/cancel: if the session has been removed from the
        // running set while the tool was executing (e.g. rollback_session marked
        // it Error and restored a snapshot), skip persisting step records that
        // would otherwise corrupt the restored state.
        if !self.running_sessions.lock().await.contains(session_id) {
            tracing::warn!(
                "execute_step: session {} left running set during tool execution; skipping step record",
                session_id
            );
            return Ok(result);
        }
        // Tie a background action to its session so end/rollback can clean it up.
        // Applied only AFTER the running-set guard above passed (a rollback
        // racing this step may have removed the session); the registrations were
        // extracted once, before the guard.
        for reg in &registrations {
            if let haven_tools::ToolRegistration::Action(action_id) = reg {
                self.tools
                    .background_actions
                    .attach_session(action_id, session_id)
                    .await;
            }
        }
        let obs = result.summary_text();
        let success = result.success;
        let persist_step_id = step_id.to_string();
        let tool_name_owned = tool_name.to_string();
        // The in-memory StepInfo reuses the persisted step row's id so the
        // live session state and the review history reference the same step.
        if let Some(entry) = self.sessions.lock().await.get(session_id).cloned() {
            let mut session = entry.lock().await;
            session.steps.push(StepInfo {
                id: persist_step_id.clone(),
                step_number,
                tool_name: tool_name_owned.clone(),
                input: input.clone(),
                output: Some(result.output.clone()),
                status: if success {
                    "completed".into()
                } else {
                    "failed".into()
                },
                risk_level,
                confirmed,
            });
            session.updated_at = chrono::Utc::now().to_rfc3339();
        }
        // Row was normally created at Action emit; ensure + complete covers
        // direct execute_step callers (tests) and races where begin failed.
        let session_id_owned = session_id.to_string();
        let tool_input = input.to_string();
        let silent = is_silent_action(tool_name, &input);
        self.db
            .run_blocking(move |db| {
                db.ensure_action_step(
                    &session_id_owned,
                    step_number,
                    &tool_name_owned,
                    &tool_input,
                    risk_level != RiskLevel::Safe,
                    silent,
                    confirmed,
                    &persist_step_id,
                )?;
                db.complete_action_step(&persist_step_id, &obs, success)
            })
            .await?;
        Ok(result)
    }

    /// Execute a tool through the safety gateway. The tool's risk level is
    /// checked against the configured threshold BEFORE anything runs; an
    /// operation at/above the threshold blocks on the user's confirmation
    /// (`confirm:requested` event + `resolve_confirmation`), and is aborted
    /// when the user declines or the session is cancelled. Returns a failed
    /// `ToolResult` for declined operations so the ReAct loop sees a normal
    /// tool failure the model can react to.
    pub async fn execute_gated(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        pre_confirmed: Option<bool>,
    ) -> anyhow::Result<ToolExecution> {
        let risk_level = self
            .tools
            .get_risk_level(session_id, tool_name, &input)
            .await;
        let mut confirmed: Option<bool> = None;
        match self
            .tools
            .safety_gateway
            .check(session_id, tool_name, &input, risk_level)
            .await
        {
            ConfirmationResult::AutoApproved => {}
            ConfirmationResult::Blocked => {
                return Ok(ToolExecution {
                    result: ToolResult {
                        success: false,
                        output: Value::Null,
                        error: Some(format!(
                            "operation '{}' is blocked by the security policy. Do NOT retry it — ask the user what to do instead or choose a different approach.",
                            tool_name
                        )),
                        truncated: false,
                        signals: haven_tools::ToolSignals::default(),
                    },
                    risk_level,
                    confirmed: Some(false),
                });
            }
            ConfirmationResult::RequiresConfirmation { .. } => {
                // Phase 5 / E3: pause-confirm resume supplies pre_confirmed so
                // we never block inside the tool future. Scheduled / headless
                // callers still use the bounded await_confirmation path.
                match pre_confirmed {
                    Some(true) => {
                        confirmed = Some(true);
                    }
                    Some(false) => {
                        return Ok(ToolExecution {
                            result: ToolResult {
                                success: false,
                                output: Value::Null,
                                error: Some(format!(
                                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                    tool_name
                                )),
                                truncated: false,
                                signals: haven_tools::ToolSignals::default(),
                            },
                            risk_level,
                            confirmed: Some(false),
                        });
                    }
                    None => {
                        if !self
                            .await_confirmation(session_id, tool_name, risk_level)
                            .await
                        {
                            return Ok(ToolExecution {
                                result: ToolResult {
                                    success: false,
                                    output: Value::Null,
                                    error: Some(format!(
                                        "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                        tool_name
                                    )),
                                    truncated: false,
                                    signals: haven_tools::ToolSignals::default(),
                                },
                                risk_level,
                                confirmed: Some(false),
                            });
                        }
                        confirmed = Some(true);
                    }
                }
            }
        }
        let result = self
            .tools
            .execute_tool(session_id, tool_name, input, cancel)
            .await?;
        Ok(ToolExecution {
            result,
            risk_level,
            confirmed,
        })
    }

    /// Request user confirmation for a safety-gated tool call and wait for
    /// the answer. Emits `confirm:requested` through the wired callback and
    /// blocks until `resolve_confirmation` resolves the generated step id, or
    /// the session's cancellation token fires (end/rollback/stop). Returns
    /// `true` when the user approved.
    async fn await_confirmation(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        risk_level: RiskLevel,
    ) -> bool {
        let step_id: haven_common::types::ConfirmId = haven_common::types::new_id("conf").into();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.confirm_waits.lock().await.insert(
            step_id.clone(),
            ConfirmWait {
                risk_level,
                session_id: session_id.map(str::to_string),
                tx,
            },
        );
        let tid = session_id.unwrap_or("action").to_string();
        // No confirmation callback wired (unit tests, degraded startup):
        // there is no UI that could ever answer — fail closed so the tool
        // never runs without approval, instead of blocking the session forever.
        if self.on_confirm_request.snap().is_none() {
            self.confirm_waits.lock().await.remove(&step_id);
            tracing::info!(
                "confirmation for tool '{}' on session {} rejected: no confirmation channel wired",
                tool_name,
                tid
            );
            return false;
        }
        if let Some(cb) = self.on_confirm_request.snap() {
            cb(
                step_id.clone(),
                tid.clone(),
                tool_name.to_string(),
                risk_level,
            );
        }
        let cancel = self.cancellation_token(&tid).await;
        let decision = tokio::select! {
            r = rx => r.ok(),
            _ = cancel.cancelled() => None,
            // Bounded, fail-closed fallback: an unanswered confirmation (e.g.
            // the app window is closed when a scheduled action fires) must
            // not wedge the session — or the sequential scheduled-action consumer —
            // forever.
            _ = tokio::time::sleep(CONFIRM_WAIT_TIMEOUT) => {
                tracing::warn!(
                    "confirmation for tool '{}' on session {} timed out after {:?}; treating as rejected",
                    tool_name,
                    tid,
                    CONFIRM_WAIT_TIMEOUT
                );
                None
            }
        };
        self.confirm_waits.lock().await.remove(&step_id);
        match decision {
            Some(true) => true,
            Some(false) | None => {
                tracing::info!(
                    "confirmation for tool '{}' on session {} not approved (answer={:?})",
                    tool_name,
                    tid,
                    decision
                );
                false
            }
        }
    }

    /// Resolve a pending safety-gateway confirmation and return the risk level
    /// and the owning session id, so the caller can trust the level for the
    /// right conversation. The approval/denial itself is persisted on the real
    /// `session_steps` row when `execute_step` completes the pending step (via
    /// the `confirmed` returned by `execute_gated`); this method only unblocks
    /// the ReAct loop waiting on the oneshot. Every step id handed here comes
    /// from a `confirm:requested` payload, which is only emitted by
    /// `await_confirmation` / pause-confirm request — so an id not present in
    /// `confirm_waits` (and not in `awaiting_confirm`) is stale.
    pub async fn resolve_confirmation(
        &self,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> anyhow::Result<Option<(RiskLevel, Option<String>)>> {
        // Legacy / scheduled path: oneshot wait inside execute_gated.
        if let Some(wait) = self.confirm_waits.lock().await.remove(step_id) {
            let level = wait.risk_level;
            let session_id = wait.session_id;
            let _ = wait.tx.send(confirmed);
            return Ok(Some((level, session_id)));
        }
        // Phase 5 / E3: pause-based confirm — record decision and wake when
        // every pending gated tool in the batch has been answered.
        if let Some((level, session_id)) = self
            .resolve_confirm_pause(step_id, confirmed)
            .await
        {
            return Ok(Some((level, Some(session_id))));
        }
        Ok(None)
    }

    /// Safety-gateway pre-check used by `LoopHooks::before_tool` (Phase 5 / E3).
    /// Does not block — `RequiresConfirmation` means the batch should pause.
    pub async fn check_tool_gate(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> ConfirmationResult {
        let risk_level = self
            .tools
            .get_risk_level(Some(session_id), tool_name, input)
            .await;
        self.tools
            .safety_gateway
            .check(Some(session_id), tool_name, input, risk_level)
            .await
    }

    /// Resume decision for a gated tool after a confirm pause (Phase 5 / E3).
    /// `Some(true)` = approved, `Some(false)` = declined, `None` = not in a
    /// confirm-continuation batch.
    pub async fn confirm_decision_for(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> Option<bool> {
        let guard = self.awaiting_confirm.lock().await;
        let pending = guard.get(session_id)?;
        pending
            .tools
            .iter()
            .find(|t| t.tool_name == tool_name && t.tool_input == *input)
            .and_then(|t| t.decision)
    }
}
