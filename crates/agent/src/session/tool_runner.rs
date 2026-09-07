//! Tool execution, safety-gated confirms, and action-step persistence.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract). R2: confirm never
//! blocks inside a tool future — ReAct uses pause/continue; scheduled fires
//! use [`SessionExecutor::request_scheduled_confirm`].

use super::*;
use haven_memory::repositories::session_steps::ActionStepOutcome;

/// The tool may already have produced an external side effect when its final
/// action-step projection fails. Callers must surface this as an unknown
/// outcome, never as an ordinary retryable failure.
#[derive(Debug)]
pub(crate) struct ActionStepPersistenceError(anyhow::Error);

impl std::fmt::Display for ActionStepPersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tool executed but action-step persistence failed: {}",
            self.0
        )
    }
}

impl std::error::Error for ActionStepPersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

fn action_step_outcome(result: &ToolResult) -> ActionStepOutcome {
    match result.outcome {
        haven_tools::ToolExecutionOutcome::Succeeded => ActionStepOutcome::Completed,
        haven_tools::ToolExecutionOutcome::Cancelled => ActionStepOutcome::Cancelled,
        haven_tools::ToolExecutionOutcome::TimedOutUnknown => ActionStepOutcome::Unknown,
        haven_tools::ToolExecutionOutcome::Failed
        | haven_tools::ToolExecutionOutcome::TimedOutAndTerminated => ActionStepOutcome::Failed,
    }
}

struct ActionStepRequest<'a> {
    session_id: &'a str,
    tool_name: &'a str,
    input: &'a Value,
    step_num: u32,
    action_index: u32,
    tool_call_id: Option<&'a str>,
    step_id: &'a str,
}

struct ActionStepContext {
    session_id: String,
    step_number: i32,
    action_index: i32,
    tool_name: String,
    tool_input: String,
    tool_call_id: Option<String>,
    is_high_risk: bool,
    silent: bool,
    step_id: String,
}

impl ActionStepContext {
    fn new(request: ActionStepRequest<'_>, risk_level: RiskLevel) -> Self {
        Self {
            session_id: request.session_id.into(),
            step_number: request.step_num as i32,
            action_index: request.action_index as i32,
            tool_name: request.tool_name.into(),
            tool_input: request.input.to_string(),
            tool_call_id: request.tool_call_id.map(str::to_string),
            is_high_risk: risk_level != RiskLevel::Safe,
            silent: is_silent_action(request.tool_name, request.input),
            step_id: request.step_id.into(),
        }
    }

    fn ensure(&self, db: &Database, confirmed: Option<bool>) -> anyhow::Result<()> {
        db.ensure_action_step_with_identity(
            &self.session_id,
            self.step_number,
            self.action_index,
            &self.tool_name,
            &self.tool_input,
            self.tool_call_id.as_deref(),
            self.is_high_risk,
            self.silent,
            confirmed,
            &self.step_id,
        )
    }
}

impl SessionExecutor {
    async fn action_step_context(&self, request: ActionStepRequest<'_>) -> ActionStepContext {
        let risk_level = self
            .tools
            .get_risk_level(Some(request.session_id), request.tool_name, request.input)
            .await;
        ActionStepContext::new(request, risk_level)
    }

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
    ) -> anyhow::Result<()> {
        self.begin_action_step_with_identity(
            session_id, tool_name, input, step_num, 0, None, step_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn begin_action_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
    ) -> anyhow::Result<()> {
        let context = self
            .action_step_context(ActionStepRequest {
                session_id,
                tool_name,
                input,
                step_num,
                action_index,
                tool_call_id,
                step_id,
            })
            .await;
        let step_id_for_log = context.step_id.clone();
        self.db
            .run_blocking(move |db| context.ensure(db, None))
            .await
            .map_err(|e| {
                tracing::error!(
                    "begin_action_step failed for step {}: {}",
                    step_id_for_log,
                    e
                );
                anyhow::anyhow!("failed to persist pending tool intent {step_id_for_log}: {e}")
            })
    }

    /// Persist an Interrupted observation onto the pending step row (creating
    /// it if Action-time begin failed). Keeps the resume badge aligned with
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
        self.finish_step_with_outcome(
            session_id,
            tool_name,
            input,
            step_num,
            0,
            None,
            step_id,
            observation,
            ActionStepOutcome::Failed,
        )
        .await;
    }

    /// Finalize a step that did not produce a normal ToolResult. Queued calls
    /// are `Cancelled`; calls that may have crossed an external side-effect
    /// boundary are `Unknown` and must not be retried automatically.
    #[allow(clippy::too_many_arguments)]
    pub async fn finish_step_with_outcome(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
        outcome: ActionStepOutcome,
    ) {
        let context = self
            .action_step_context(ActionStepRequest {
                session_id,
                tool_name,
                input,
                step_num,
                action_index,
                tool_call_id,
                step_id,
            })
            .await;
        let step_id_for_log = context.step_id.clone();
        let observation = observation.to_string();
        if let Err(e) = self
            .db
            .run_blocking(move |db| {
                context.ensure(db, None)?;
                db.finish_action_step(&context.step_id, &observation, outcome)
            })
            .await
        {
            tracing::warn!(
                "finish_step_with_outcome failed for step {}: {}",
                step_id_for_log,
                e
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_interrupted_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
    ) {
        self.finish_step_with_outcome(
            session_id,
            tool_name,
            input,
            step_num,
            action_index,
            tool_call_id,
            step_id,
            observation,
            ActionStepOutcome::Failed,
        )
        .await;
    }

    /// Move the Action-emit pending row to running immediately before the
    /// tool is invoked. The ensure fallback keeps direct test/caller paths
    /// safe when no Action event created the row first.
    pub async fn start_action_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        step_id: &str,
    ) -> anyhow::Result<()> {
        self.start_action_step_with_identity(
            session_id, tool_name, input, step_num, 0, None, step_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_action_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
    ) -> anyhow::Result<()> {
        let context = self
            .action_step_context(ActionStepRequest {
                session_id,
                tool_name,
                input,
                step_num,
                action_index,
                tool_call_id,
                step_id,
            })
            .await;
        let step_id_for_log = context.step_id.clone();
        self.db
            .run_blocking(move |db| {
                context.ensure(db, None)?;
                db.start_action_step(&context.step_id)
            })
            .await
            .map(|_| ())
            .map_err(|e| {
                tracing::error!(
                    "start_action_step failed for step {}: {}",
                    step_id_for_log,
                    e
                );
                anyhow::anyhow!("failed to mark tool intent running {step_id_for_log}: {e}")
            })
    }

    pub async fn tool_concurrency(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> haven_tools::ToolConcurrency {
        self.tools
            .get_concurrency(Some(session_id), tool_name, input)
            .await
    }

    pub async fn tool_idempotency(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> haven_tools::OperationIdempotency {
        self.tools
            .get_idempotency(Some(session_id), tool_name, input)
            .await
    }

    pub async fn tool_operation_scope(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
    ) -> haven_tools::ToolOperationScope {
        self.tools
            .get_operation_scope(Some(session_id), tool_name, input)
            .await
    }

    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.tools.observation_text(tool_name, result).await
    }

    /// Execute a tool step. `step_id` is the pre-minted `step-*` id the frontend's
    /// live tool card already uses; the persisted step row reuses it so the live
    /// card and the resume badge are one entity. The pending row is normally
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
        self.execute_step_with_identity(session_id, tool_name, input, step_num, 0, None, step_id)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_num,
            action_index,
            tool_call_id,
            step_id,
            None,
        )
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
        self.execute_step_preconfirmed_with_identity(
            session_id,
            tool_name,
            input,
            step_num,
            0,
            None,
            step_id,
            pre_confirmed,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step_preconfirmed_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        pre_confirmed: bool,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_num,
            action_index,
            tool_call_id,
            step_id,
            Some(pre_confirmed),
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_step_inner(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        pre_confirmed: Option<bool>,
    ) -> anyhow::Result<ToolResult> {
        let tool_call_id = tool_call_id.map(str::to_string);
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
                self.finish_interrupted_step_with_identity(
                    session_id,
                    tool_name,
                    &input,
                    step_num,
                    action_index,
                    tool_call_id.as_deref(),
                    step_id,
                    &err,
                )
                .await;
                return Err(anyhow::anyhow!(err));
            }
        }

        let cancel = self.cancellation_token(session_id).await;
        self.start_action_step_with_identity(
            session_id,
            tool_name,
            &input,
            step_num,
            action_index,
            tool_call_id.as_deref(),
            step_id,
        )
        .await?;
        let gated = match self
            .execute_gated(
                Some(session_id),
                tool_name,
                input.clone(),
                cancel.clone(),
                pre_confirmed,
                Some(step_id),
            )
            .await
        {
            Ok(gated) => gated,
            Err(e) => {
                // Pending row was created at Action emit; record the failure
                // so resume/resync does not rebuild an empty tool badge.
                self.finish_step_with_outcome(
                    session_id,
                    tool_name,
                    &input,
                    step_num,
                    action_index,
                    tool_call_id.as_deref(),
                    step_id,
                    &e.to_string(),
                    if cancel.is_cancelled() {
                        ActionStepOutcome::Unknown
                    } else {
                        ActionStepOutcome::Failed
                    },
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
            tool = %tool_name,
            success = result.success,
            attempts = result.attempts,
            outcome = ?result.outcome,
            "execute_step result"
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
        // Apply session-local registrations only after the terminal/rollback
        // fence above. A tool can finish successfully just as its session is
        // ended; registering its skill/MCP overlay after that point would
        // leak tools into a dead session and let a late result mutate state.
        for reg in &registrations {
            match reg {
                haven_tools::ToolRegistration::Skill(name) => {
                    self.tools
                        .register_skill_for_session(session_id, name)
                        .await;
                }
                haven_tools::ToolRegistration::McpServer(name) => {
                    self.tools
                        .register_mcp_for_session(session_id, name, None)
                        .await;
                }
                haven_tools::ToolRegistration::Action(_) => {}
            }
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
        let obs = self.tools.observation_text(tool_name, &result).await;
        let step_outcome = action_step_outcome(&result);
        let persist_step_id = step_id.to_string();
        let tool_name_owned = tool_name.to_string();
        // The in-memory StepInfo reuses the persisted step row's id so the
        // live session state and the resume history reference the same step.
        if let Some(entry) = self.sessions.lock().await.get(session_id).cloned() {
            let mut session = entry.lock().await;
            session.steps.push(StepInfo {
                id: persist_step_id.clone(),
                step_number,
                tool_name: tool_name_owned.clone(),
                input: input.clone(),
                output: Some(result.output.clone()),
                status: step_outcome.as_str().into(),
                risk_level,
                confirmed,
            });
            session.updated_at = chrono::Utc::now().to_rfc3339();
        }
        // Row was normally created at Action emit; ensure + complete covers
        // direct execute_step callers (tests) and races where begin failed.
        let action_step = ActionStepContext::new(
            ActionStepRequest {
                session_id,
                tool_name,
                input: &input,
                step_num,
                action_index,
                tool_call_id: tool_call_id.as_deref(),
                step_id,
            },
            risk_level,
        );
        self.db
            .run_blocking(move |db| {
                action_step.ensure(db, confirmed)?;
                db.finish_action_step(&action_step.step_id, &obs, step_outcome)
            })
            .await
            .map_err(|error| anyhow::Error::new(ActionStepPersistenceError(error)))?;
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
        step_id: Option<&str>,
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
            ConfirmationResult::Blocked { reason } => {
                return Ok(ToolExecution {
                    result: ToolResult {
                        success: false,
                        output: Value::Null,
                        error: Some(format!(
                            "operation '{tool_name}' is blocked by the security policy ({reason}). Do NOT retry it — ask the user what to do instead or choose a different approach."
                        )),
                        truncated: false,
                        outcome: haven_tools::ToolExecutionOutcome::Failed,
                        attempts: 1,
                        signals: haven_tools::ToolSignals::default(),
                    },
                    risk_level,
                    confirmed: Some(false),
                });
            }
            ConfirmationResult::RequiresConfirmation { .. } => {
                // R2 / Phase 5 E3: never block inside the tool future.
                // Callers must pre-decide via pause-confirm (`Some(true|false)`)
                // or `request_scheduled_confirm`. Missing pre_confirmed fails closed.
                match pre_confirmed {
                    Some(true) => {
                        confirmed = Some(true);
                    }
                    Some(false) | None => {
                        if pre_confirmed.is_none() {
                            tracing::warn!(
                                tool = %tool_name,
                                session = %session_id.unwrap_or("action"),
                                "execute_gated RequiresConfirmation without pre_confirmed; rejecting (R2 fail-closed)"
                            );
                        }
                        return Ok(ToolExecution {
                            result: ToolResult {
                                success: false,
                                output: Value::Null,
                                error: Some(format!(
                                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                    tool_name
                                )),
                                truncated: false,
                                outcome: haven_tools::ToolExecutionOutcome::Cancelled,
                                attempts: 1,
                                signals: haven_tools::ToolSignals::default(),
                            },
                            risk_level,
                            confirmed: Some(false),
                        });
                    }
                }
            }
        }
        let result = self
            .tools
            .execute_tool_with_step(session_id, tool_name, input, cancel, step_id)
            .await?;
        Ok(ToolExecution {
            result,
            risk_level,
            confirmed,
        })
    }

    /// Queue a scheduled-tool confirmation without blocking the fired-action
    /// consumer (R2). Emits `confirm:requested` and stores pending args; a
    /// later [`Self::resolve_confirmation`] (or the timeout task) executes or
    /// skips. Returns `None` when no confirm channel is wired (fail closed).
    pub async fn request_scheduled_confirm(
        self: &Arc<Self>,
        session_id: Option<&str>,
        tool_name: &str,
        tool_args: Value,
        risk_level: RiskLevel,
        title: &str,
    ) -> Option<haven_common::types::ConfirmId> {
        let step_id: haven_common::types::ConfirmId = haven_common::types::new_id("conf").into();
        let tid = session_id.unwrap_or("action").to_string();
        if self.on_confirm_request.snap().is_none() {
            tracing::info!(
                "scheduled confirmation for tool '{}' on session {} rejected: no confirmation channel wired",
                tool_name,
                tid
            );
            return None;
        }
        self.scheduled_confirms.lock().await.insert(
            step_id.clone(),
            ScheduledConfirmPending {
                session_id: session_id.map(str::to_string),
                tool_name: tool_name.to_string(),
                tool_args: tool_args.clone(),
                title: title.to_string(),
            },
        );
        if let Some(cb) = self.on_confirm_request.snap() {
            cb(
                step_id.clone(),
                tid.clone(),
                tool_name.to_string(),
                risk_level,
                tool_args,
                None,
                0,
                None,
            );
        }
        // Absolute fail-closed timer for closed/crashed UI. Interactive
        // countdown starts when the dialog is shown (frontend), so queued
        // confirms are not starved by arrival-time deadlines.
        let executor = Arc::clone(self);
        let timeout_id = step_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SCHEDULED_CONFIRM_ABSOLUTE_TIMEOUT).await;
            if executor
                .scheduled_confirms
                .lock()
                .await
                .contains_key(&timeout_id)
            {
                tracing::warn!(
                    "scheduled confirmation {} timed out after {:?}; treating as rejected",
                    timeout_id,
                    SCHEDULED_CONFIRM_ABSOLUTE_TIMEOUT
                );
                let _ = executor.resolve_confirmation(&timeout_id, false).await;
            }
        });
        Some(step_id)
    }

    /// Resolve a pending safety-gateway confirmation and return enough context
    /// for the app layer to record a permission grant (tool + session).
    ///
    /// Handles (1) scheduled-tool pending (R2 — execute/skip asynchronously)
    /// and (2) ReAct pause-confirm (`awaiting_confirm`). An unknown id is stale.
    pub async fn resolve_confirmation(
        self: &Arc<Self>,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        // Scheduled tool path: non-blocking request → resolve later.
        if let Some(pending) = self.scheduled_confirms.lock().await.remove(step_id) {
            let resolution = crate::session::ConfirmResolution {
                session_id: pending.session_id.clone(),
                tool_name: pending.tool_name.clone(),
                tool_input: pending.tool_args.clone(),
            };
            let executor = Arc::clone(self);
            tokio::spawn(async move {
                executor.finish_scheduled_confirm(pending, confirmed).await;
            });
            return Ok(Some(resolution));
        }
        // Phase 5 / E3: pause-based confirm — record decision and wake when
        // every pending gated tool in the batch has been answered.
        if let Some(resolution) = self.resolve_confirm_pause(step_id, confirmed).await? {
            return Ok(Some(resolution));
        }
        Ok(None)
    }

    async fn finish_scheduled_confirm(&self, pending: ScheduledConfirmPending, confirmed: bool) {
        if confirmed
            && let Some(session_id) = pending.session_id.as_deref()
            && !self.session_is_live(session_id).await
        {
            if let Some(cb) = self.on_scheduled_confirm_outcome.snap() {
                cb(
                    pending.title,
                    format!(
                        "Scheduled tool '{}' was NOT executed: its session is no longer active.",
                        pending.tool_name
                    ),
                );
            }
            return;
        }
        let tool_name = pending.tool_name;
        let title = pending.title;
        if !confirmed {
            if let Some(cb) = self.on_scheduled_confirm_outcome.snap() {
                cb(
                    title,
                    format!(
                        "Scheduled tool '{tool_name}' was NOT executed: \
                         confirmation was declined or timed out."
                    ),
                );
            }
            return;
        }
        let outcome = self
            .execute_gated(
                pending.session_id.as_deref(),
                &tool_name,
                pending.tool_args,
                CancellationToken::new(),
                Some(true),
                None,
            )
            .await;
        let summary_chars = self
            .notification_summary_chars
            .load(std::sync::atomic::Ordering::Relaxed);
        let body = match outcome {
            Ok(g) => {
                let summary = crate::truncate_notification(&g.result.summary_text(), summary_chars);
                format!("schedule tool '{tool_name}':\n{summary}")
            }
            Err(e) => format!("schedule tool '{tool_name}' failed: {e}"),
        };
        if let Some(cb) = self.on_scheduled_confirm_outcome.snap() {
            cb(title, body);
        }
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
        step_id: &str,
        action_index: u32,
        tool_call_id: Option<&str>,
    ) -> Option<bool> {
        let guard = self.awaiting_confirm.lock().await;
        let pending = guard.get(session_id)?;
        pending
            .tools
            .iter()
            .find(|t| {
                t.step_id == step_id
                    && t.action_index == action_index
                    && t.tool_call_id == tool_call_id.unwrap_or_default()
            })
            .and_then(|t| t.decision)
    }
}
