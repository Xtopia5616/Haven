//! Tool execution, safety-gated confirms, and action-step persistence.
//!
//! Split from `session.rs` (Phase 7 / A3 mechanical extract). R2: confirm never
//! blocks inside a tool future — ReAct uses pause/continue; scheduled fires
//! use [`SessionSupervisor::request_scheduled_confirm`].

use super::*;
use haven_memory::repositories::session_steps::{ActionStepOutcome, ActionStepWrite};
use tracing::Instrument;

pub(super) fn confirmation_expiry_delay(expires_at: Option<&str>) -> Option<std::time::Duration> {
    let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at?)
        .ok()?
        .with_timezone(&chrono::Utc);
    let remaining = expires_at.signed_duration_since(chrono::Utc::now());
    Some(remaining.to_std().unwrap_or_default())
}

fn interaction_owner_matches_request(
    owner: &crate::interaction::InteractionOwner,
    request: &crate::interaction::InteractionRequest,
) -> bool {
    match (owner, &request.details) {
        (
            crate::interaction::InteractionOwner::Session { session_id },
            crate::interaction::InteractionDetails::Confirm { .. },
        ) => request.session_id.as_deref() == Some(session_id.as_str()),
        (
            crate::interaction::InteractionOwner::ScheduledAction { action_id },
            crate::interaction::InteractionDetails::ScheduledConfirm {
                action_id: request_action_id,
                ..
            },
        ) => action_id == request_action_id,
        _ => false,
    }
}

fn scheduled_request_matches_route(
    request: &crate::interaction::InteractionRequest,
    action_id: &str,
    request_id: &haven_common::types::ConfirmId,
) -> bool {
    request.id == request_id.as_str()
        && request.status == crate::interaction::InteractionStatus::Pending
        && matches!(
            &request.details,
            crate::interaction::InteractionDetails::ScheduledConfirm {
                action_id: request_action_id,
                ..
            } if request_action_id == action_id
        )
}

#[derive(Clone, Copy)]
enum ScheduledConfirmDeadlineDecision {
    /// Determine expiry when the request is committed as terminal.
    CheckAtResolution,
    /// The owner gate already accepted this decision before the deadline.
    AcceptedBeforeDeadline,
    /// An owner timer or failed authorization explicitly expired the request.
    Expire,
}

fn scheduled_confirmation_is_expired(
    request: &crate::interaction::InteractionRequest,
    decision: ScheduledConfirmDeadlineDecision,
) -> bool {
    match decision {
        ScheduledConfirmDeadlineDecision::CheckAtResolution => request
            .pending_permission_deadline()
            .map(|deadline| deadline <= chrono::Utc::now())
            .unwrap_or(true),
        ScheduledConfirmDeadlineDecision::AcceptedBeforeDeadline => false,
        ScheduledConfirmDeadlineDecision::Expire => true,
    }
}

/// The tool may already have produced an external side effect when its final
/// action-step projection fails. Callers must surface this as an unknown
/// outcome, never as an ordinary retryable failure.
#[derive(Debug)]
pub(crate) struct ActionStepPersistenceError(anyhow::Error);

/// Metadata resolved from the ReAct turn's immutable tool catalog.
/// Authorization and risk checks remain live; this only avoids reopening the
/// catalog for action-step bookkeeping on every tool call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ActionStepMetadata {
    pub(crate) is_high_risk: bool,
    pub(crate) silent: bool,
}

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
        let metadata = ActionStepMetadata {
            is_high_risk: risk_level != RiskLevel::Safe,
            silent: is_silent_action(request.tool_name, request.input),
        };
        Self::new_with_metadata(request, metadata)
    }

    fn new_with_metadata(request: ActionStepRequest<'_>, metadata: ActionStepMetadata) -> Self {
        Self {
            session_id: request.session_id.into(),
            step_number: request.step_num as i32,
            action_index: request.action_index as i32,
            tool_name: request.tool_name.into(),
            tool_input: request.input.to_string(),
            tool_call_id: request.tool_call_id.map(str::to_string),
            is_high_risk: metadata.is_high_risk,
            silent: metadata.silent,
            step_id: request.step_id.into(),
        }
    }

    fn into_write(self) -> ActionStepWrite {
        ActionStepWrite {
            session_id: self.session_id,
            step_number: self.step_number,
            action_index: self.action_index,
            tool_name: self.tool_name,
            tool_input: self.tool_input,
            tool_call_id: self.tool_call_id,
            is_high_risk: self.is_high_risk,
            silent: self.silent,
            step_id: self.step_id,
        }
    }
}

impl SessionSupervisor {
    async fn action_step_context(&self, request: ActionStepRequest<'_>) -> ActionStepContext {
        let risk_level = self
            .tool_authorization
            .risk_level(Some(request.session_id), request.tool_name, request.input)
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
        self.persist_pending_action_step_context(context).await
    }

    async fn persist_pending_action_step_context(
        &self,
        context: ActionStepContext,
    ) -> anyhow::Result<()> {
        let step_id_for_log = context.step_id.clone();
        self.store
            .ensure_action_step(context.into_write(), None)
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
        self.finish_action_step_context(context, observation, outcome)
            .await;
    }

    /// Finalize a batch action using metadata resolved from the batch's
    /// immutable catalog snapshot. The execution safety decision is still
    /// performed live by `execute_gated`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_step_with_outcome_and_metadata(
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
        metadata: ActionStepMetadata,
    ) {
        let context = ActionStepContext::new_with_metadata(
            ActionStepRequest {
                session_id,
                tool_name,
                input,
                step_num,
                action_index,
                tool_call_id,
                step_id,
            },
            metadata,
        );
        self.finish_action_step_context(context, observation, outcome)
            .await;
    }

    async fn finish_action_step_context(
        &self,
        context: ActionStepContext,
        observation: &str,
        outcome: ActionStepOutcome,
    ) {
        let step_id_for_log = context.step_id.clone();
        let observation = observation.to_string();
        if let Err(e) = self
            .store
            .ensure_and_finish_action_step(context.into_write(), None, observation, outcome)
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_interrupted_step_with_identity_and_metadata(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
        metadata: ActionStepMetadata,
    ) {
        self.finish_step_with_outcome_and_metadata(
            session_id,
            tool_name,
            input,
            step_num,
            action_index,
            tool_call_id,
            step_id,
            observation,
            ActionStepOutcome::Failed,
            metadata,
        )
        .await;
    }

    /// Move the Action-emit pending row to running immediately before the
    /// tool is invoked. The ensure step keeps direct callers safe when no
    /// Action event created the row first.
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
        self.start_running_action_step_context(context).await
    }

    async fn start_running_action_step_context(
        &self,
        context: ActionStepContext,
    ) -> anyhow::Result<()> {
        let step_id_for_log = context.step_id.clone();
        self.store
            .ensure_and_start_action_step(context.into_write(), None)
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

    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        self.observation_port
            .observation_text(tool_name, result)
            .await
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
            None,
            None,
        )
        .await
    }

    /// Metadata-preserving variant used by a ReAct turn. The caller supplies
    /// the turn-scoped cancellation token so a deadline can stop a tool even
    /// though the session itself remains live for recovery.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_step_with_identity_and_metadata_and_cancel(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        metadata: ActionStepMetadata,
        cancel: tokio_util::sync::CancellationToken,
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
            Some(metadata),
            Some(cancel),
        )
        .await
    }

    /// Like [`execute_step`], but skips the blocking confirm wait when
    /// `receipt` is supplied only when the authorization engine approved the
    /// exact invocation earlier (Phase 5 / E3 resume after pause-confirm).
    pub async fn execute_step_preconfirmed(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        step_id: &str,
        receipt: haven_tools::ConfirmationReceipt,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_preconfirmed_with_identity(
            session_id, tool_name, input, step_num, 0, None, step_id, receipt,
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
        receipt: haven_tools::ConfirmationReceipt,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_num,
            action_index,
            tool_call_id,
            step_id,
            Some(receipt),
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn execute_step_preconfirmed_with_identity_and_metadata_and_cancel(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        action_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        receipt: haven_tools::ConfirmationReceipt,
        metadata: ActionStepMetadata,
        cancel: tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.execute_step_inner(
            session_id,
            tool_name,
            input,
            step_num,
            action_index,
            tool_call_id,
            step_id,
            Some(receipt),
            Some(metadata),
            Some(cancel),
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
        receipt: Option<haven_tools::ConfirmationReceipt>,
        action_step_metadata: Option<ActionStepMetadata>,
        cancel_override: Option<tokio_util::sync::CancellationToken>,
    ) -> anyhow::Result<ToolResult> {
        let tool_call_id = tool_call_id.map(str::to_string);
        tracing::debug!(
            "execute_step: session={} tool={} input_fields={} input_chars={} receipt={}",
            session_id,
            tool_name,
            input.as_object().map(|object| object.len()).unwrap_or(0),
            serde_json::to_string(&input)
                .map(|serialized| serialized.len())
                .unwrap_or(0),
            receipt.is_some(),
        );
        {
            let actor = self.actor_for(session_id).await;
            // Phase 7 / E4: only Running may execute tools. Missing session
            // (end_session already removed the entry) must fail closed — never
            // treat absence as permission to run.
            let refuse = match actor {
                None => Some(format!(
                    "execute_step: session {} not in working set; refusing to execute tool '{}'",
                    session_id, tool_name
                )),
                Some(actor) => {
                    let prev = actor.snapshot().await.map(|session| session.status);
                    let Some(prev) = prev else {
                        return Err(anyhow::anyhow!(
                            "execute_step: session {} actor stopped",
                            session_id
                        ));
                    };
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
                if let Some(metadata) = action_step_metadata {
                    self.finish_interrupted_step_with_identity_and_metadata(
                        session_id,
                        tool_name,
                        &input,
                        step_num,
                        action_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &err,
                        metadata,
                    )
                    .await;
                } else {
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
                }
                return Err(anyhow::anyhow!(err));
            }
        }

        let cancel = match cancel_override {
            Some(cancel) => cancel,
            None => self.cancellation_token(session_id).await,
        };
        let action_step_request = ActionStepRequest {
            session_id,
            tool_name,
            input: &input,
            step_num,
            action_index,
            tool_call_id: tool_call_id.as_deref(),
            step_id,
        };
        let action_step_context = match action_step_metadata {
            Some(metadata) => ActionStepContext::new_with_metadata(action_step_request, metadata),
            None => self.action_step_context(action_step_request).await,
        };
        self.start_running_action_step_context(action_step_context)
            .await?;
        let gated = match self
            .execute_gated(
                Some(session_id),
                tool_name,
                input.clone(),
                cancel.clone(),
                receipt,
                Some(step_id),
            )
            .await
        {
            Ok(gated) => gated,
            Err(e) => {
                // Pending row was created at Action emit; record the failure
                // so resume/resync does not rebuild an empty tool badge.
                let outcome = if cancel.is_cancelled() {
                    ActionStepOutcome::Unknown
                } else {
                    ActionStepOutcome::Failed
                };
                if let Some(metadata) = action_step_metadata {
                    self.finish_step_with_outcome_and_metadata(
                        session_id,
                        tool_name,
                        &input,
                        step_num,
                        action_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &e.to_string(),
                        outcome,
                        metadata,
                    )
                    .await;
                } else {
                    self.finish_step_with_outcome(
                        session_id,
                        tool_name,
                        &input,
                        step_num,
                        action_index,
                        tool_call_id.as_deref(),
                        step_id,
                        &e.to_string(),
                        outcome,
                    )
                    .await;
                }
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

        // Apply the tool's declared per-session side effects (MCP adapter
        // registration) instead of name-matching loader tools here —
        // a new tool with a side effect declares it via `Tool::registrations`
        // and nothing in this executor needs to change. Background-action
        // bindings are applied after the running-set guard below (a action
        // spawned in a concurrently-rolled-back step must not attach past
        // the cleanup sweep). `registrations` is extracted ONCE: calling it
        // twice could yield divergent results for stateful tools, and the
        // variant split below is explicit rather than silently partitioned.
        let registrations = if result.success {
            self.execution
                .registrations(session_id, tool_name, &result.output)
                .await
        } else {
            Vec::new()
        };
        let step_number = step_num as i32;
        // Guard against rollback/cancel: if the session has been removed from the
        // running set while the tool was executing (e.g. rollback_session marked
        // it Error and restored a snapshot), skip persisting step records that
        // would otherwise corrupt the restored state.
        if !self.is_run_in_flight(session_id).await {
            tracing::warn!(
                "execute_step: session {} left running set during tool execution; skipping step record",
                session_id
            );
            return Ok(result);
        }
        // Apply session-local registrations only after the terminal/rollback
        // fence above. A tool can finish successfully just as its session is
        // ended; registering its MCP overlay after that point would
        // leak tools into a dead session and let a late result mutate state.
        for reg in &registrations {
            match reg {
                haven_tools::ToolRegistration::McpServer(name) => {
                    self.session_tool_overlay_port
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
                self.actions.attach_session(action_id, session_id).await;
            }
        }
        let obs = self
            .observation_port
            .observation_text(tool_name, &result)
            .await;
        let step_outcome = action_step_outcome(&result);
        let persist_step_id = step_id.to_string();
        let tool_name_owned = tool_name.to_string();
        // The in-memory StepInfo reuses the persisted step row's id so the
        // live session state and the resume history reference the same step.
        if let Some(actor) = self.actor_for(session_id).await {
            actor
                .record_step(StepInfo {
                    id: persist_step_id.clone(),
                    step_number,
                    tool_name: tool_name_owned.clone(),
                    input: input.clone(),
                    output: Some(result.output.clone()),
                    status: step_outcome.as_str().into(),
                    risk_level,
                    confirmed,
                })
                .await;
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
        self.store
            .ensure_and_finish_action_step(action_step.into_write(), confirmed, obs, step_outcome)
            .await
            .map_err(|error| anyhow::Error::new(ActionStepPersistenceError(error)))?;
        Ok(result)
    }

    /// Execute a tool through the safety gateway. The tool's risk level is
    /// checked against the configured threshold BEFORE anything runs; an
    /// operation at/above the threshold blocks on the user's confirmation
    /// (`interaction:requested` event + `resolve_confirmation`), and is aborted
    /// when the user declines or the session is cancelled. Returns a failed
    /// `ToolResult` for declined operations so the ReAct loop sees a normal
    /// tool failure the model can react to.
    pub async fn execute_gated(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        receipt: Option<haven_tools::ConfirmationReceipt>,
        step_id: Option<&str>,
    ) -> anyhow::Result<ToolExecution> {
        let risk_level = self
            .tool_authorization
            .risk_level(session_id, tool_name, &input)
            .await;
        let authorization_request = self
            .tool_authorization
            .authorization_request(session_id, tool_name, &input)
            .await;
        let mut confirmed: Option<bool> = None;
        if let Some(receipt) = receipt.as_ref()
            && let Err(reason) = self
                .authorization
                .verify_receipt(&authorization_request, receipt)
                .await
        {
            tracing::warn!(
                tool = %tool_name,
                session_id = ?session_id,
                reason_len = reason.chars().count(),
                "confirmation receipt rejected; refusing to execute"
            );
            return Ok(ToolExecution {
                result: ToolResult {
                    success: false,
                    output: Value::Null,
                    error: Some(format!(
                        "The confirmation for operation '{}' is no longer valid ({reason}). The operation was not executed.",
                        tool_name
                    )),
                    error_class: Some(haven_tools::ToolErrorClass::Permission),
                    retryability: haven_tools::ToolRetryability::NotRetryable,
                    truncated: false,
                    outcome: haven_tools::ToolExecutionOutcome::Cancelled,
                    attempts: 1,
                    signals: haven_tools::ToolSignals::default(),
                    llm_usage: Vec::new(),
                },
                risk_level,
                confirmed: Some(false),
            });
        }
        match self.authorization.authorize(&authorization_request).await {
            AuthorizationDecision::AutoApproved => {}
            AuthorizationDecision::Blocked { reason, .. } => {
                return Ok(ToolExecution {
                    result: ToolResult {
                        success: false,
                        output: Value::Null,
                        error: Some(format!(
                            "operation '{tool_name}' is blocked by the security policy ({reason}). Do NOT retry it — ask the user what to do instead or choose a different approach."
                        )),
                        error_class: Some(haven_tools::ToolErrorClass::Permission),
                        retryability: haven_tools::ToolRetryability::NotRetryable,
                        truncated: false,
                        outcome: haven_tools::ToolExecutionOutcome::Failed,
                        attempts: 1,
                        signals: haven_tools::ToolSignals::default(),
                        llm_usage: Vec::new(),
                    },
                    risk_level,
                    confirmed: Some(false),
                });
            }
            AuthorizationDecision::RequiresConfirmation { .. } => {
                // R2 / Phase 5 E3: never block inside the tool future.
                // Callers must pre-decide via pause-confirm or
                // `request_scheduled_confirm`. Missing receipt fails closed.
                match receipt {
                    Some(_) => {
                        confirmed = Some(true);
                    }
                    None => {
                        tracing::warn!(
                            tool = %tool_name,
                            session_id = ?session_id,
                            "execute_gated RequiresConfirmation without a receipt; rejecting (R2 fail-closed)"
                        );
                        return Ok(ToolExecution {
                            result: ToolResult {
                                success: false,
                                output: Value::Null,
                                error: Some(format!(
                                    "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                    tool_name
                                )),
                                error_class: Some(haven_tools::ToolErrorClass::Permission),
                                retryability: haven_tools::ToolRetryability::NotRetryable,
                                truncated: false,
                                outcome: haven_tools::ToolExecutionOutcome::Cancelled,
                                attempts: 1,
                                signals: haven_tools::ToolSignals::default(),
                                llm_usage: Vec::new(),
                            },
                            risk_level,
                            confirmed: Some(false),
                        });
                    }
                }
            }
        }
        let result = self
            .execution
            .execute(ToolExecutionContext {
                session_id: session_id.map(str::to_owned),
                tool_name: tool_name.to_owned(),
                input,
                cancel,
                step_id: step_id.map(str::to_owned),
            })
            .await?;
        Ok(ToolExecution {
            result,
            risk_level,
            confirmed,
        })
    }

    async fn scheduled_authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> haven_tools::AuthorizationRequest {
        self.tool_authorization
            .authorization_request(session_id, tool_name, input)
            .await
    }

    /// Authorize a scheduled tool invocation through the supervisor's live
    /// authorization service. Confirmation queuing and execution remain with
    /// their existing scheduled-action callers.
    pub(crate) async fn authorize_scheduled_tool(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> haven_tools::AuthorizationDecision {
        let request = self
            .scheduled_authorization_request(session_id, tool_name, input)
            .await;
        self.authorization.authorize(&request).await
    }

    /// Queue a scheduled-tool confirmation without blocking the fired-action
    /// consumer (R2). Stores the canonical interaction request and emits it
    /// through the supervisor event stream; a later owner-routed resolve or
    /// expiry executes or skips the action. Returns `None` when the action
    /// already owns a pending confirmation (fail closed).
    pub async fn request_scheduled_confirm(
        self: &Arc<Self>,
        action_id: &str,
        session_id: Option<&str>,
        tool_name: &str,
        tool_args: Value,
        receipt: haven_tools::ConfirmationReceipt,
        title: &str,
    ) -> Option<haven_common::types::ConfirmId> {
        let step_id = receipt.confirmation_id.clone();
        let request = crate::interaction::InteractionRequest::scheduled_confirm(
            action_id.to_string(),
            session_id,
            tool_name.to_string(),
            tool_args,
            receipt,
            title.to_string(),
        );
        if let Err(error) = request.validate_new_pending_permission() {
            tracing::warn!(%action_id, request_id = %step_id, %error, "scheduled confirmation has no valid future deadline");
            return None;
        }
        let Some(expiry_delay) = confirmation_expiry_delay(request.expires_at.as_deref()) else {
            tracing::warn!(%action_id, request_id = %step_id, "scheduled confirmation deadline cannot be scheduled");
            return None;
        };
        let action_id = action_id.to_string();
        {
            let mut scheduled_confirms = self.scheduled_confirms.lock().await;
            if scheduled_confirms.contains_key(&action_id) {
                tracing::warn!(%action_id, "scheduled action already owns a pending confirmation");
                return None;
            }
            scheduled_confirms.insert(action_id.clone(), request.clone());
        }
        self.emit_event(SessionEvent::InteractionRequested {
            envelope: Box::new(crate::interaction::InteractionEnvelope {
                owner: crate::interaction::InteractionOwner::ScheduledAction {
                    action_id: action_id.clone(),
                },
                request,
            }),
        });
        // The canonical request deadline is the hard lifetime for this
        // approval, including when the renderer is closed or crashed.
        let executor = Arc::clone(self);
        let timeout_id = step_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(expiry_delay).await;
            if executor
                .scheduled_confirmation_request(&action_id, &timeout_id)
                .await
                .is_some()
            {
                tracing::warn!(
                    action_id = %action_id,
                    request_id = %timeout_id,
                    "scheduled confirmation timed out after {:?}; treating as rejected",
                    expiry_delay
                );
                let mut retry_delay = std::time::Duration::from_secs(1);
                loop {
                    match executor
                        .expire_scheduled_confirmation(&action_id, &timeout_id)
                        .await
                    {
                        Ok(_) => break,
                        Err(error) => {
                            tracing::warn!(
                                action_id = %action_id,
                                request_id = %timeout_id,
                                error = %error,
                                "failed to expire scheduled confirmation; retrying"
                            );
                            tokio::time::sleep(retry_delay).await;
                            retry_delay = retry_delay
                                .saturating_mul(2)
                                .min(std::time::Duration::from_secs(30));
                        }
                    }
                }
            }
        });
        Some(step_id)
    }

    pub(super) async fn scheduled_confirmation_request(
        &self,
        action_id: &str,
        request_id: &haven_common::types::ConfirmId,
    ) -> Option<crate::interaction::InteractionRequest> {
        self.scheduled_confirms
            .lock()
            .await
            .get(action_id)
            .filter(|request| scheduled_request_matches_route(request, action_id, request_id))
            .cloned()
    }

    pub async fn pending_confirmation_capability_for_owner(
        &self,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
    ) -> Option<haven_common::types::CapabilityScope> {
        let request = match owner {
            crate::interaction::InteractionOwner::ScheduledAction { action_id } => {
                self.scheduled_confirmation_request(action_id, request_id)
                    .await?
            }
            crate::interaction::InteractionOwner::AppCommand => return None,
            crate::interaction::InteractionOwner::Session { session_id } => {
                self.pending_session_confirmation_request(session_id, request_id)
                    .await?
            }
        };
        if !interaction_owner_matches_request(owner, &request) {
            return None;
        }
        match &request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm { receipt, .. } => {
                Some(receipt.capability.clone())
            }
            crate::interaction::InteractionDetails::Confirm {
                receipt,
                tool_name,
                tool_input,
                ..
            } => {
                if let Some(receipt) = receipt {
                    Some(receipt.capability.clone())
                } else {
                    let session_id = request.session_id.as_deref()?;
                    Some(
                        self.tool_authorization
                            .authorization_request(Some(session_id), tool_name, tool_input)
                            .await
                            .policy
                            .capability,
                    )
                }
            }
            crate::interaction::InteractionDetails::Ask { .. } => None,
        }
    }

    pub(super) async fn pending_session_confirmation_request(
        &self,
        session_id: &str,
        step_id: &haven_common::types::ConfirmId,
    ) -> Option<crate::interaction::InteractionRequest> {
        let actor = self.actor_for(session_id).await?;
        actor
            .interactions(Some(crate::interaction::InteractionKind::Confirm), true)
            .await
            .into_iter()
            .find(|request| request.id == step_id.as_str())
    }

    /// Persist an explicit session grant before resolving a pending
    /// confirmation. Resolving the interaction can wake a paused ReAct actor
    /// (or spawn a scheduled operation), so the durable decision must exist
    /// before that wake edge. The owner route selects either the session actor
    /// or the scheduled-action registry; UI-only confirmations are handled by
    /// the app command because their typed action payload is app-owned.
    pub async fn resolve_confirmation_with_session_grant_for_owner(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
        target: haven_common::types::PermissionTarget,
        effect: haven_common::types::PermissionEffect,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        self.resolve_confirmation_with_session_grant_inner(owner, request_id, target, effect)
            .await
    }

    async fn resolve_confirmation_with_session_grant_inner(
        self: &Arc<Self>,
        expected_owner: &crate::interaction::InteractionOwner,
        step_id: &haven_common::types::ConfirmId,
        target: haven_common::types::PermissionTarget,
        effect: haven_common::types::PermissionEffect,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        let request = match expected_owner {
            crate::interaction::InteractionOwner::ScheduledAction { action_id } => {
                self.scheduled_confirmation_request(action_id, step_id)
                    .await
            }
            crate::interaction::InteractionOwner::AppCommand => None,
            crate::interaction::InteractionOwner::Session { session_id } => {
                self.pending_session_confirmation_request(session_id, step_id)
                    .await
            }
        };
        let Some(request) = request else {
            return Ok(None);
        };
        if self.request_session_is_closing(expected_owner, &request) {
            return Ok(None);
        }
        if !interaction_owner_matches_request(expected_owner, &request) {
            return Ok(None);
        }
        if request
            .pending_permission_deadline()
            .map(|deadline| deadline <= chrono::Utc::now())
            .unwrap_or(true)
        {
            if let Some(action_id) = match expected_owner {
                crate::interaction::InteractionOwner::ScheduledAction { action_id } => {
                    Some(action_id.as_str())
                }
                _ => None,
            } {
                return self
                    .resolve_scheduled_confirmation_locked(
                        action_id,
                        step_id,
                        false,
                        ScheduledConfirmDeadlineDecision::Expire,
                    )
                    .await;
            }
            let crate::interaction::InteractionOwner::Session { session_id } = expected_owner
            else {
                return Ok(None);
            };
            return self
                .resolve_confirmation_locked(session_id, step_id, false, true)
                .await;
        }
        let scheduled_action_id = match &request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm { action_id, .. } => {
                Some(action_id.clone())
            }
            _ => None,
        };

        let (tool_name, tool_input, receipt) = match &request.details {
            crate::interaction::InteractionDetails::Confirm {
                tool_name,
                tool_input,
                receipt,
                ..
            } => (tool_name, tool_input, receipt.as_ref()),
            crate::interaction::InteractionDetails::ScheduledConfirm {
                tool_name,
                tool_input,
                receipt,
                ..
            } => (tool_name, tool_input, Some(receipt)),
            _ => return Ok(None),
        };
        let session_id = request
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("session scope requires an owning conversation"))?;

        let authorization_request = self
            .tool_authorization
            .authorization_request(Some(session_id), tool_name, tool_input)
            .await;
        let capability = receipt
            .map(|receipt| receipt.capability.clone())
            .unwrap_or_else(|| authorization_request.policy.capability.clone());
        let key = capability.target(target).ok_or_else(|| {
            anyhow::anyhow!("permission target is broader than the pending capability")
        })?;

        // For an allow, verify the exact receipt before making a durable trust
        // change. Denials do not execute the operation, but persist before the
        // wake so a retry cannot race past the user's session-scoped denial.
        if matches!(effect, haven_common::types::PermissionEffect::Allow) {
            let receipt = receipt.ok_or_else(|| {
                anyhow::anyhow!("confirmation is missing its authorization receipt")
            })?;
            if let Err(reason) = self
                .authorization
                .verify_receipt(&authorization_request, receipt)
                .await
            {
                // A stale approval must not leave its session paused forever.
                // Consume the request as a denial (without installing a grant)
                // before returning the validation error to the renderer.
                if let Some(action_id) = &scheduled_action_id {
                    self.resolve_scheduled_confirmation_locked(
                        action_id,
                        step_id,
                        false,
                        ScheduledConfirmDeadlineDecision::Expire,
                    )
                    .await?;
                } else {
                    let crate::interaction::InteractionOwner::Session { session_id } =
                        expected_owner
                    else {
                        return Ok(None);
                    };
                    self.resolve_confirmation_locked(session_id, step_id, false, true)
                        .await?;
                }
                anyhow::bail!("confirmation request can no longer be executed: {reason}");
            }
        }
        if let Some(action_id) = scheduled_action_id.as_deref()
            && !self
                .actions
                .claim_scheduled_execution(action_id, step_id.as_str())
                .await?
        {
            // Consume and dismiss the request as stale after cancellation won;
            // this path deliberately does not persist the requested session
            // grant.
            self.resolve_scheduled_confirmation_locked(
                action_id,
                step_id,
                true,
                ScheduledConfirmDeadlineDecision::CheckAtResolution,
            )
            .await?;
            return Ok(None);
        }
        if let Err(error) = self
            .grant_session_permission(session_id, key, target, effect)
            .await
        {
            if let Some(action_id) = scheduled_action_id.as_deref()
                && let Err(release_error) = self
                    .actions
                    .release_scheduled_execution_claim(action_id, step_id.as_str())
                    .await
            {
                tracing::warn!(
                    %action_id,
                    request_id = %step_id,
                    error = %release_error,
                    "failed to release scheduled confirmation claim after grant persistence failed"
                );
            }
            return Err(error);
        }

        let confirmed = matches!(effect, haven_common::types::PermissionEffect::Allow);
        if let Some(action_id) = scheduled_action_id {
            self.resolve_scheduled_confirmation_locked(
                &action_id,
                step_id,
                confirmed,
                ScheduledConfirmDeadlineDecision::AcceptedBeforeDeadline,
            )
            .await
        } else {
            let crate::interaction::InteractionOwner::Session { session_id } = expected_owner
            else {
                return Ok(None);
            };
            self.resolve_confirmation_locked(session_id, step_id, confirmed, false)
                .await
        }
    }

    pub async fn resolve_confirmation_for_owner(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        self.resolve_confirmation_for_owner_locked(owner, request_id, confirmed, false)
            .await
    }

    /// Expire one confirmation through its declared owner. Unlike the user
    /// resolve command, expiry is initiated only by the owner timer.
    pub async fn expire_confirmation_for_owner(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        self.resolve_confirmation_for_owner_locked(owner, request_id, false, true)
            .await
    }

    async fn resolve_confirmation_for_owner_locked(
        self: &Arc<Self>,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
        confirmed: bool,
        expire: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        if self.owner_session_is_closing(owner, request_id).await {
            if expire {
                anyhow::bail!(
                    "session is closing; retry confirmation expiry after lifecycle cleanup"
                );
            }
            return Ok(None);
        }
        match owner {
            crate::interaction::InteractionOwner::ScheduledAction { action_id } => {
                self.resolve_scheduled_confirmation_locked(
                    action_id,
                    request_id,
                    confirmed,
                    if expire {
                        ScheduledConfirmDeadlineDecision::Expire
                    } else {
                        ScheduledConfirmDeadlineDecision::CheckAtResolution
                    },
                )
                .await
            }
            crate::interaction::InteractionOwner::AppCommand => Ok(None),
            crate::interaction::InteractionOwner::Session { session_id } => {
                let Some(request) = self
                    .pending_session_confirmation_request(session_id, request_id)
                    .await
                else {
                    return Ok(None);
                };
                if !interaction_owner_matches_request(owner, &request) {
                    return Ok(None);
                }
                let deadline_elapsed = request
                    .pending_permission_deadline()
                    .map(|deadline| deadline <= chrono::Utc::now())
                    .unwrap_or(true);
                self.resolve_confirmation_locked(
                    session_id,
                    request_id,
                    confirmed,
                    expire || deadline_elapsed,
                )
                .await
            }
        }
    }

    pub async fn expire_scheduled_confirmation(
        self: &Arc<Self>,
        action_id: &str,
        request_id: &haven_common::types::ConfirmId,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let _resolution = self.confirmation_resolution_gate.lock().await;
        let owner = crate::interaction::InteractionOwner::ScheduledAction {
            action_id: action_id.to_string(),
        };
        if self.owner_session_is_closing(&owner, request_id).await {
            anyhow::bail!(
                "session is closing; retry scheduled confirmation expiry after lifecycle cleanup"
            );
        }
        self.resolve_scheduled_confirmation_locked(
            action_id,
            request_id,
            false,
            ScheduledConfirmDeadlineDecision::Expire,
        )
        .await
    }

    async fn owner_session_is_closing(
        &self,
        owner: &crate::interaction::InteractionOwner,
        request_id: &haven_common::types::ConfirmId,
    ) -> bool {
        match owner {
            crate::interaction::InteractionOwner::Session { session_id } => {
                self.is_session_closing(session_id)
            }
            crate::interaction::InteractionOwner::ScheduledAction { action_id } => self
                .scheduled_confirmation_request(action_id, request_id)
                .await
                .is_some_and(|request| self.request_session_is_closing(owner, &request)),
            crate::interaction::InteractionOwner::AppCommand => false,
        }
    }

    fn request_session_is_closing(
        &self,
        owner: &crate::interaction::InteractionOwner,
        request: &crate::interaction::InteractionRequest,
    ) -> bool {
        let Some(session_id) = request.session_id.as_deref() else {
            return false;
        };
        match owner {
            crate::interaction::InteractionOwner::Session {
                session_id: owner_id,
            } => owner_id == session_id && self.is_session_closing(owner_id),
            crate::interaction::InteractionOwner::ScheduledAction { .. } => {
                self.is_session_closing(session_id)
            }
            crate::interaction::InteractionOwner::AppCommand => false,
        }
    }

    async fn resolve_confirmation_locked(
        self: &Arc<Self>,
        session_id: &str,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
        expired: bool,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        // Phase 5 / E3: pause-based confirm — record decision and wake when
        // every pending gated tool in the batch has been answered.
        if let Some(request) = self
            .resolve_interaction(
                session_id,
                step_id.as_str(),
                Value::Bool(confirmed),
                expired,
            )
            .await?
        {
            let status = request.status;
            let (session_id, tool_name, tool_input) = match request.details {
                crate::interaction::InteractionDetails::Confirm {
                    tool_name,
                    tool_input,
                    ..
                } => (request.session_id, tool_name, tool_input),
                _ => (None, String::new(), Value::Null),
            };
            return Ok(Some(crate::session::ConfirmResolution {
                session_id,
                tool_name,
                tool_input,
                status,
            }));
        }
        Ok(None)
    }

    async fn resolve_scheduled_confirmation_locked(
        self: &Arc<Self>,
        action_id: &str,
        request_id: &haven_common::types::ConfirmId,
        confirmed: bool,
        deadline_decision: ScheduledConfirmDeadlineDecision,
    ) -> anyhow::Result<Option<crate::session::ConfirmResolution>> {
        let Some(pending_request) = self
            .scheduled_confirmation_request(action_id, request_id)
            .await
        else {
            return Ok(None);
        };
        let (request_action_id, tool_name, tool_input) = match &pending_request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm {
                action_id,
                tool_name,
                tool_input,
                ..
            } => (action_id.clone(), tool_name.clone(), tool_input.clone()),
            _ => return Ok(None),
        };
        if request_action_id != action_id {
            return Ok(None);
        }
        let expired = scheduled_confirmation_is_expired(&pending_request, deadline_decision);

        // An approval must win a durable claim against cancellation before it
        // can consume the request, persist a session grant, or start a tool.
        // If cancel already committed, dismiss the stale prompt without any
        // authorization or execution side effect.
        if confirmed
            && !expired
            && !self
                .actions
                .claim_scheduled_execution(action_id, request_id.as_str())
                .await?
        {
            let cancelled = {
                let mut scheduled_confirms = self.scheduled_confirms.lock().await;
                let Some(request) = scheduled_confirms.get(action_id).filter(|request| {
                    scheduled_request_matches_route(request, action_id, request_id)
                }) else {
                    return Ok(None);
                };
                let mut request = request.clone();
                scheduled_confirms.remove(action_id);
                request.cancel();
                request
            };
            self.emit_event(crate::session::SessionEvent::InteractionRequested {
                envelope: Box::new(crate::interaction::InteractionEnvelope {
                    owner: crate::interaction::InteractionOwner::ScheduledAction {
                        action_id: action_id.to_string(),
                    },
                    request: cancelled,
                }),
            });
            return Ok(None);
        }

        let Some(mut request) = ({
            let mut scheduled_confirms = self.scheduled_confirms.lock().await;
            if !scheduled_confirms.get(action_id).is_some_and(|request| {
                scheduled_request_matches_route(request, action_id, request_id)
            }) {
                return Ok(None);
            }
            scheduled_confirms.remove(action_id)
        }) else {
            if confirmed {
                self.actions
                    .release_scheduled_execution_claim(action_id, request_id.as_str())
                    .await?;
            }
            return Ok(None);
        };
        let session_id = request.session_id.clone();
        if expired {
            let _ = request.expire();
        } else {
            let _ = request.resolve(Value::Bool(confirmed));
        }
        self.emit_event(crate::session::SessionEvent::InteractionRequested {
            envelope: Box::new(crate::interaction::InteractionEnvelope {
                owner: crate::interaction::InteractionOwner::ScheduledAction {
                    action_id: action_id.to_string(),
                },
                request: request.clone(),
            }),
        });
        let resolution = crate::session::ConfirmResolution {
            session_id,
            tool_name,
            tool_input,
            status: request.status,
        };
        let executor = Arc::clone(self);
        tokio::spawn(async move {
            executor
                .finish_scheduled_confirm(request, confirmed, expired)
                .await;
        });
        Ok(Some(resolution))
    }

    async fn finish_scheduled_confirm(
        &self,
        request: crate::interaction::InteractionRequest,
        confirmed: bool,
        expired: bool,
    ) {
        let (action_id, session_id, tool_name, tool_args, receipt, title) = match request.details {
            crate::interaction::InteractionDetails::ScheduledConfirm {
                action_id,
                tool_name,
                tool_input,
                receipt,
                title,
            } => (
                action_id,
                request.session_id.clone(),
                tool_name,
                tool_input,
                receipt,
                title,
            ),
            _ => return,
        };
        let action_service = self.actions.clone();
        if confirmed
            && let Some(live_session_id) = session_id.as_deref()
            && !self.session_is_live(live_session_id).await
        {
            self.emit_event(SessionEvent::ScheduledConfirmOutcome {
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                title,
                body: format!(
                    "Scheduled tool '{}' was NOT executed: its session is no longer active.",
                    tool_name
                ),
            });
            let _ = action_service
                .fail_scheduled(&action_id, "关联会话已结束或不存在")
                .await;
            return;
        }
        if !confirmed {
            let reason = if expired {
                "confirmation timed out"
            } else {
                "confirmation was declined"
            };
            self.emit_event(SessionEvent::ScheduledConfirmOutcome {
                action_id: action_id.clone(),
                session_id: session_id.clone(),
                title,
                body: format!("Scheduled tool '{tool_name}' was NOT executed: {reason}."),
            });
            let _ = action_service
                .fail_scheduled(
                    &action_id,
                    if expired {
                        "确认超时"
                    } else {
                        "确认被拒绝"
                    },
                )
                .await;
            return;
        }
        let outcome = self
            .execute_gated(
                session_id.as_deref(),
                &tool_name,
                tool_args,
                CancellationToken::new(),
                Some(receipt),
                None,
            )
            .instrument(tracing::info_span!(
                "scheduled_action_confirmation_execution",
                action_id = %action_id,
                session_id = ?session_id
            ))
            .await;
        let summary_chars = self
            .notification_summary_chars
            .load(std::sync::atomic::Ordering::Relaxed);
        let succeeded = outcome.is_ok();
        let mut result_summary = None;
        let body = match outcome {
            Ok(g) => {
                let summary = crate::truncate_notification(&g.result.summary_text(), summary_chars);
                result_summary = Some(summary.clone());
                format!("schedule tool '{tool_name}':\n{summary}")
            }
            Err(e) => format!("schedule tool '{tool_name}' failed: {e}"),
        };
        if succeeded {
            if let Some(result) = result_summary.as_deref() {
                let _ = action_service
                    .complete_scheduled_with_result(&action_id, result)
                    .await;
            } else {
                let _ = action_service.complete_scheduled(&action_id).await;
            }
        } else {
            let failure_summary = crate::truncate_notification(&body, summary_chars);
            let _ = action_service
                .fail_scheduled(&action_id, &failure_summary)
                .await;
        }
        self.emit_event(SessionEvent::ScheduledConfirmOutcome {
            action_id,
            session_id,
            title,
            body,
        });
    }

    /// Check authorization using the turn's immutable catalog snapshot. The
    /// authorization engine remains live so grants, denies and policy
    /// revisions are never cached across the safety boundary.
    pub async fn check_tool_gate_with_catalog(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        catalog: &haven_tools::ToolCatalogSnapshot,
    ) -> haven_tools::AuthorizationDecision {
        let authorization_request = self.tool_authorization.authorization_request_from_catalog(
            catalog,
            Some(session_id),
            tool_name,
            input,
        );
        self.authorization.authorize(&authorization_request).await
    }

    /// Resume decision for a gated tool after a confirm pause (Phase 5 / E3).
    /// Returns the recorded decision and its exact authorization receipt.
    /// Siblings that were carried behind a confirm barrier have no receipt and
    /// are therefore rechecked normally on resume.
    pub async fn confirm_decision_for(
        &self,
        session_id: &str,
        step_id: &str,
        action_index: u32,
        tool_call_id: Option<&str>,
    ) -> Option<(bool, Option<haven_tools::ConfirmationReceipt>)> {
        self.interaction_requests(session_id)
            .await
            .into_iter()
            .filter(|request| request.kind == crate::interaction::InteractionKind::Confirm)
            .find_map(|request| {
                let decision = request.decision();
                match request.details {
                    crate::interaction::InteractionDetails::Confirm {
                        step_id: request_step_id,
                        action_index: request_action_index,
                        tool_call_id: request_tool_call_id,
                        receipt,
                        ..
                    } if request_step_id == step_id
                        && request_action_index == action_index
                        && request_tool_call_id == tool_call_id.unwrap_or_default() =>
                    {
                        decision.map(|decision| (decision, receipt))
                    }
                    _ => None,
                }
            })
    }
}

#[cfg(test)]
mod scheduled_authorization_tests {
    use super::*;
    use haven_common::types::PermissionMode;
    use haven_memory::Database;
    use haven_tools::{AuthorizationDecision, AuthorizationReasonCode};
    use serde_json::json;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct PolicyTestTool {
        name: String,
        risk_level: RiskLevel,
        on_execute: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    #[async_trait::async_trait]
    impl haven_tools::Tool for PolicyTestTool {
        fn name(&self) -> String {
            self.name.clone()
        }

        fn description(&self) -> String {
            "test-only authorization policy".into()
        }

        fn risk_level(&self, _input: &Value) -> RiskLevel {
            self.risk_level
        }

        async fn execute(
            &self,
            _input: Value,
            _cancel: CancellationToken,
        ) -> anyhow::Result<haven_tools::ToolResult> {
            if let Some(on_execute) = &self.on_execute {
                on_execute();
                return Ok(haven_tools::ToolResult::ok(json!({"ok": true})));
            }
            unreachable!("scheduled authorization tests never execute a tool")
        }

        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
    }

    fn test_supervisor() -> (
        Arc<SessionSupervisor>,
        Arc<haven_tools::ToolsManager>,
        Arc<Database>,
        tempfile::TempDir,
    ) {
        let tools = Arc::new(haven_tools::ToolsManager::new());
        let directory = tempfile::tempdir().unwrap();
        let database =
            Arc::new(Database::open(&directory.path().join("authorization.db")).unwrap());
        let supervisor = Arc::new(SessionSupervisor::new_for_test(
            database.clone(),
            tools.clone(),
            1,
        ));
        (supervisor, tools, database, directory)
    }

    fn future_receipt(
        confirmation_id: haven_common::types::ConfirmId,
        capability: &str,
    ) -> haven_tools::ConfirmationReceipt {
        haven_tools::ConfirmationReceipt {
            confirmation_id,
            capability: haven_common::types::CapabilityScope::try_new(capability).unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: RiskLevel::High,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        }
    }

    #[test]
    fn accepted_scheduled_decision_keeps_its_deadline_verdict_after_persistence() {
        let mut request = crate::interaction::InteractionRequest::scheduled_confirm(
            "act-1234567890abcdef1234567890abcdef".into(),
            None,
            "files.write".into(),
            json!({}),
            future_receipt(haven_common::types::new_id("conf").into(), "files.write"),
            "scheduled".into(),
        );
        request.expires_at = Some((chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339());

        assert!(scheduled_confirmation_is_expired(
            &request,
            ScheduledConfirmDeadlineDecision::CheckAtResolution
        ));
        assert!(!scheduled_confirmation_is_expired(
            &request,
            ScheduledConfirmDeadlineDecision::AcceptedBeforeDeadline
        ));
        assert!(scheduled_confirmation_is_expired(
            &request,
            ScheduledConfirmDeadlineDecision::Expire
        ));
    }

    async fn running_scheduled_action(
        supervisor: &SessionSupervisor,
        session_id: Option<&str>,
        tool_name: &str,
    ) -> String {
        let _receiver = supervisor
            .actions
            .take_action_receiver()
            .expect("scheduled receiver");
        let action_id = supervisor
            .actions
            .set(haven_tools::ScheduledActionSpec {
                due_at: Some((chrono::Utc::now() + chrono::Duration::seconds(3)).to_rfc3339()),
                delay_secs: None,
                watch_action_id: None,
                title: "confirmation arbitration".into(),
                body: "test scheduled execution owner".into(),
                mode: haven_tools::ScheduleMode::Tool,
                session_id: session_id.map(str::to_owned),
                tool_name: Some(tool_name.to_string()),
                tool_args: Some(json!({})),
                prompt: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            loop {
                if supervisor.actions.status_view(&action_id).await.status()
                    == Some(haven_common::ActionStatus::Running)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("scheduled action should enter Running before confirmation");
        action_id
    }

    #[tokio::test]
    async fn scheduled_authorization_preserves_request_and_decision_behavior() {
        let (supervisor, tools, _database, _directory) = test_supervisor();
        let session_id = "ses-00000000000000000000000000000001";
        let tool_name = "scheduled.critical";
        let input = json!({"target": "recording"});
        tools
            .register_for_session(
                session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::Critical,
                    on_execute: None,
                }),
            )
            .await;

        let request = supervisor
            .scheduled_authorization_request(Some(session_id), tool_name, &input)
            .await;
        assert_eq!(request.session_id.as_deref(), Some(session_id));
        assert_eq!(request.tool_name, tool_name);
        assert_eq!(request.input, input);
        assert_eq!(request.policy.risk_level, RiskLevel::Critical);
        assert_eq!(
            request.policy.capability.to_string(),
            haven_common::types::permission_key(tool_name, &input)
        );
        assert_eq!(
            request.policy.confirmation,
            haven_tools::ConfirmationRequirement::Required
        );

        assert!(matches!(
            supervisor
                .authorize_scheduled_tool(Some(session_id), tool_name, &input)
                .await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        let safe_tool_name = "scheduled.safe";
        tools
            .register_for_session(
                session_id,
                Arc::new(PolicyTestTool {
                    name: safe_tool_name.into(),
                    risk_level: RiskLevel::Safe,
                    on_execute: None,
                }),
            )
            .await;
        assert!(matches!(
            supervisor
                .authorize_scheduled_tool(Some(session_id), safe_tool_name, &json!({}))
                .await,
            AuthorizationDecision::AutoApproved
        ));

        supervisor
            .authorization
            .set_permission_mode(PermissionMode::Plan)
            .await;
        assert!(matches!(
            supervisor
                .authorize_scheduled_tool(Some(session_id), tool_name, &input)
                .await,
            AuthorizationDecision::Blocked {
                reason_code: AuthorizationReasonCode::PlanMode,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn session_grant_is_durable_before_scheduled_confirmation_wakes_tool() {
        let (supervisor, tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("grant before confirm wake")
            .await
            .unwrap();
        let tool_name = "scheduled.high";
        let input = json!({"target": "recording"});
        let observed_durable_grant = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = observed_durable_grant.clone();
        let db = database.clone();
        let session_id = session.id.clone();
        let observed_session_id = session_id.clone();
        let expected_capability = haven_common::types::CapabilityScope::try_new(
            haven_common::types::permission_key(tool_name, &input),
        )
        .unwrap();
        tools
            .register_for_session(
                &session_id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        let grants = db
                            .session_authorization_grants(&observed_session_id)
                            .unwrap();
                        observed.store(
                            grants.iter().any(|grant| {
                                grant.capability == expected_capability
                                    && grant.effect == PermissionEffect::Allow
                            }),
                            std::sync::atomic::Ordering::SeqCst,
                        );
                    })),
                }),
            )
            .await;

        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session_id), tool_name, &input)
            .await;
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let action_id = running_scheduled_action(&supervisor, Some(&session_id), tool_name).await;
        let confirmation_id = supervisor
            .request_scheduled_confirm(
                &action_id,
                Some(&session_id),
                tool_name,
                input,
                receipt,
                "scheduled confirmation",
            )
            .await
            .unwrap();
        let owner = crate::interaction::InteractionOwner::ScheduledAction {
            action_id: action_id.clone(),
        };
        let wrong_owner = crate::interaction::InteractionOwner::ScheduledAction {
            action_id: haven_common::types::new_id("act"),
        };
        assert!(
            supervisor
                .resolve_confirmation_for_owner(&wrong_owner, &confirmation_id, true)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            supervisor
                .scheduled_confirmation_request(&action_id, &confirmation_id,)
                .await
                .is_some()
        );
        let wrong_request_id: haven_common::types::ConfirmId =
            haven_common::types::new_id("conf").into();
        assert!(
            supervisor
                .resolve_confirmation_for_owner(&owner, &wrong_request_id, true)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            supervisor
                .scheduled_confirmation_request(&action_id, &confirmation_id,)
                .await
                .is_some()
        );

        assert!(
            supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &owner,
                    &confirmation_id,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
                .unwrap()
                .is_some()
        );

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !observed_durable_grant.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("scheduled tool should observe its durable grant before execution");
    }

    #[tokio::test]
    async fn session_confirmation_route_uses_only_the_declared_actor() {
        let (supervisor, _tools, _database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("owner-routed confirmation")
            .await
            .unwrap();
        let other_session = supervisor
            .create_session("different confirmation owner")
            .await
            .unwrap();
        let request = crate::interaction::InteractionRequest::confirm(
            &session.id,
            1,
            "files.write".into(),
            json!({"path": "notes.txt"}),
            "call-owner-route".into(),
            "step-owner-route".into(),
            0,
            RiskLevel::High,
            Some(future_receipt(
                haven_common::types::new_id("conf").into(),
                "files.write",
            )),
        );
        supervisor
            .request_interaction(request.clone())
            .await
            .unwrap();
        let other_request = crate::interaction::InteractionRequest::confirm(
            &other_session.id,
            1,
            "files.write".into(),
            json!({"path": "other-notes.txt"}),
            "call-other-owner-route".into(),
            "step-other-owner-route".into(),
            0,
            RiskLevel::High,
            Some(future_receipt(
                haven_common::types::new_id("conf").into(),
                "files.write",
            )),
        );
        supervisor
            .request_interaction(other_request.clone())
            .await
            .unwrap();
        let request_id: haven_common::types::ConfirmId = request.id.clone().into();
        let other_request_id: haven_common::types::ConfirmId = other_request.id.clone().into();
        let wrong_owner = crate::interaction::InteractionOwner::Session {
            session_id: other_session.id.clone(),
        };

        assert!(
            supervisor
                .resolve_confirmation_for_owner(&wrong_owner, &request_id, true)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.id, &request_id)
                .await
                .is_some()
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&other_session.id, &other_request_id)
                .await
                .is_some()
        );

        let owner = crate::interaction::InteractionOwner::Session {
            session_id: session.id.clone(),
        };
        let resolved = supervisor
            .resolve_confirmation_for_owner(&owner, &request_id, false)
            .await
            .unwrap()
            .expect("the request must resolve through its owning actor");
        assert_eq!(resolved.session_id.as_deref(), Some(session.id.as_str()));
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.id, &request_id)
                .await
                .is_none()
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&other_session.id, &other_request_id)
                .await
                .is_some(),
            "resolving one actor must not consume another actor's confirmation"
        );
    }

    #[tokio::test]
    async fn session_grant_survives_resolve_append_failure_and_same_request_retry() {
        let (supervisor, tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("session grant resolve append retry")
            .await
            .unwrap();
        let tool_name = "files.write";
        let input = json!({"path": "notes.txt"});
        tools
            .register_for_session(
                &session.id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: None,
                }),
            )
            .await;

        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session.id), tool_name, &input)
            .await;
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let request_id = receipt.confirmation_id.clone();
        let request = crate::interaction::InteractionRequest::confirm(
            &session.id,
            1,
            tool_name.into(),
            input,
            "call-grant-resolve-retry".into(),
            request_id.to_string(),
            0,
            RiskLevel::High,
            Some(receipt),
        );
        supervisor
            .request_confirm_batch(&session.id, vec![request])
            .await
            .unwrap();
        assert_eq!(
            supervisor.get_active_session_status(&session.id).await,
            Some(haven_common::SessionStatus::Paused)
        );

        database
            .conn()
            .execute_batch(
                "CREATE TRIGGER reject_confirmation_resolve_event
                 BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'interaction_resolved'
                 BEGIN SELECT RAISE(ABORT, 'test confirmation resolve append failure'); END;",
            )
            .unwrap();
        let owner = crate::interaction::InteractionOwner::Session {
            session_id: session.id.clone(),
        };
        let error = supervisor
            .resolve_confirmation_with_session_grant_for_owner(
                &owner,
                &request_id,
                haven_common::types::PermissionTarget::Operation,
                PermissionEffect::Allow,
            )
            .await
            .expect_err("a failed durable decision append must be retryable");
        assert!(format!("{error:#}").contains("test confirmation resolve append failure"));

        let grants = database.session_authorization_grants(&session.id).unwrap();
        assert_eq!(grants.len(), 1, "the approved grant remains durable");
        assert_eq!(grants[0].effect, PermissionEffect::Allow);
        assert!(matches!(
            supervisor
                .authorization
                .authorize(&authorization_request)
                .await,
            AuthorizationDecision::AutoApproved
        ));
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.id, &request_id)
                .await
                .is_some(),
            "the original actor must retain the pending request after append failure"
        );
        assert_eq!(
            supervisor.get_active_session_status(&session.id).await,
            Some(haven_common::SessionStatus::Paused),
            "append failure must not wake the paused session"
        );
        let events = supervisor
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_REQUESTED_EVENT_TYPE)
                .count(),
            1
        );
        assert!(
            events
                .iter()
                .all(|event| event.event_type != haven_memory::INTERACTION_RESOLVED_EVENT_TYPE),
            "the failed append must not leave a partial decision event"
        );

        database
            .conn()
            .execute_batch("DROP TRIGGER reject_confirmation_resolve_event;")
            .unwrap();
        assert!(
            supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &owner,
                    &request_id,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
                .unwrap()
                .is_some(),
            "the same request must resolve after the durable store recovers"
        );
        assert!(
            supervisor
                .pending_session_confirmation_request(&session.id, &request_id)
                .await
                .is_none()
        );
        assert_eq!(
            supervisor.get_active_session_status(&session.id).await,
            Some(haven_common::SessionStatus::Pending)
        );
        assert_eq!(
            database
                .session_authorization_grants(&session.id)
                .unwrap()
                .len(),
            1,
            "retry must not duplicate the already durable grant"
        );
        let events = supervisor
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_RESOLVED_EVENT_TYPE)
                .count(),
            1,
            "a successful retry must append exactly one decision event"
        );
    }

    #[tokio::test]
    async fn scheduled_resolve_and_expiry_share_a_single_terminal_claim() {
        let (supervisor, _tools, _database, _directory) = test_supervisor();
        let action_id = running_scheduled_action(&supervisor, None, "files.write").await;
        let confirmation_id: haven_common::types::ConfirmId =
            haven_common::types::new_id("conf").into();
        supervisor
            .request_scheduled_confirm(
                &action_id,
                None,
                "files.write",
                json!({"path": "notes.txt"}),
                future_receipt(confirmation_id.clone(), "files.write"),
                "scheduled confirmation",
            )
            .await
            .unwrap();
        let owner = crate::interaction::InteractionOwner::ScheduledAction {
            action_id: action_id.clone(),
        };

        let resolve_supervisor = supervisor.clone();
        let resolve_owner = owner.clone();
        let resolve_id = confirmation_id.clone();
        let resolve = tokio::spawn(async move {
            resolve_supervisor
                .resolve_confirmation_for_owner(&resolve_owner, &resolve_id, true)
                .await
                .unwrap()
        });
        let expiry_supervisor = supervisor.clone();
        let expiry_id = confirmation_id.clone();
        let expiry_action_id = action_id.clone();
        let expiry = tokio::spawn(async move {
            expiry_supervisor
                .expire_scheduled_confirmation(&expiry_action_id, &expiry_id)
                .await
                .unwrap()
        });

        let outcomes = [resolve.await.unwrap(), expiry.await.unwrap()];
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_some()).count(),
            1
        );
        assert!(
            supervisor
                .scheduled_confirmation_request(&action_id, &confirmation_id)
                .await
                .is_none()
        );
        assert!(
            supervisor
                .resolve_confirmation_for_owner(&owner, &confirmation_id, true)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn scheduled_cancel_winning_confirmation_prevents_grant_and_execution() {
        let (supervisor, tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("cancel wins scheduled confirmation")
            .await
            .unwrap();
        let tool_name = "scheduled.critical";
        let input = json!({"target": "recording"});
        let tool_executed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let executed = Arc::clone(&tool_executed);
        tools
            .register_for_session(
                &session.id,
                Arc::new(PolicyTestTool {
                    name: tool_name.into(),
                    risk_level: RiskLevel::High,
                    on_execute: Some(Arc::new(move || {
                        executed.store(true, std::sync::atomic::Ordering::SeqCst);
                    })),
                }),
            )
            .await;
        let authorization_request = supervisor
            .scheduled_authorization_request(Some(&session.id), tool_name, &input)
            .await;
        let receipt = match supervisor
            .authorization
            .authorize(&authorization_request)
            .await
        {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("expected confirmation, got {decision:?}"),
        };
        let action_id = running_scheduled_action(&supervisor, Some(&session.id), tool_name).await;
        let confirmation_id = supervisor
            .request_scheduled_confirm(
                &action_id,
                Some(&session.id),
                tool_name,
                input,
                receipt,
                "scheduled confirmation",
            )
            .await
            .unwrap();
        let owner = crate::interaction::InteractionOwner::ScheduledAction {
            action_id: action_id.clone(),
        };

        assert!(supervisor.actions.cancel(&action_id).await);
        assert!(
            supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &owner,
                    &confirmation_id,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            database
                .session_authorization_grants(&session.id)
                .unwrap()
                .is_empty()
        );
        assert!(!tool_executed.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            supervisor
                .scheduled_confirmation_request(&action_id, &confirmation_id)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_grant_loses_to_an_already_queued_once_rejection_without_stale_row() {
        let (supervisor, _tools, database, _directory) = test_supervisor();
        let session = supervisor
            .create_session("serialized confirmation decisions")
            .await
            .unwrap();
        let confirmation_id: haven_common::types::ConfirmId =
            haven_common::types::new_id("conf").into();
        supervisor
            .request_scheduled_confirm(
                "act-00000000000000000000000000000002",
                Some(&session.id),
                "files.write",
                json!({"path": "notes.txt"}),
                future_receipt(confirmation_id.clone(), "files.write"),
                "scheduled confirmation",
            )
            .await
            .unwrap();

        // Hold the shared resolver gate, then queue a one-shot rejection
        // before a duplicate Session-Allow click. FIFO serialization must let
        // the rejection consume the pending request before the grant path can
        // inspect or persist it.
        let gate = supervisor.confirmation_resolution_gate.clone();
        let held_gate = gate.lock().await;
        let owner = crate::interaction::InteractionOwner::ScheduledAction {
            action_id: "act-00000000000000000000000000000002".into(),
        };
        let once_supervisor = supervisor.clone();
        let once_id = confirmation_id.clone();
        let (once_started_tx, once_started_rx) = tokio::sync::oneshot::channel();
        let once = tokio::spawn(async move {
            let _ = once_started_tx.send(());
            once_supervisor
                .resolve_confirmation_for_owner(&owner, &once_id, false)
                .await
        });
        once_started_rx.await.unwrap();

        let session_supervisor = supervisor.clone();
        let session_id_for_call = confirmation_id.clone();
        let (session_started_tx, session_started_rx) = tokio::sync::oneshot::channel();
        let session_grant = tokio::spawn(async move {
            let _ = session_started_tx.send(());
            session_supervisor
                .resolve_confirmation_with_session_grant_for_owner(
                    &crate::interaction::InteractionOwner::ScheduledAction {
                        action_id: "act-00000000000000000000000000000002".into(),
                    },
                    &session_id_for_call,
                    haven_common::types::PermissionTarget::Operation,
                    PermissionEffect::Allow,
                )
                .await
        });
        session_started_rx.await.unwrap();
        drop(held_gate);

        assert!(once.await.unwrap().unwrap().is_some());
        assert!(session_grant.await.unwrap().unwrap().is_none());
        assert!(
            database
                .session_authorization_grants(&session.id)
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod action_step_persistence_tests {
    use super::*;
    use haven_memory::Database;
    use serde_json::json;

    #[tokio::test]
    async fn action_step_lifecycle_persists_identity_through_session_store() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let supervisor = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsManager::new()),
            1,
        ));
        let session = supervisor
            .create_session("action step store port")
            .await
            .unwrap();
        let step_id = "step-tool-runner-port";
        let input = json!({"path": "notes.txt", "silent": true});

        supervisor
            .begin_action_step_with_identity(
                &session.id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                step_id,
            )
            .await
            .unwrap();

        let pending = db.get_session_steps(&session.id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, step_id);
        assert_eq!(pending[0].step_number, 5);
        assert_eq!(pending[0].action_index, 3);
        assert_eq!(pending[0].action_tool.as_deref(), Some("files.read"));
        assert_eq!(pending[0].tool_call_id.as_deref(), Some("provider-call-5"));
        assert!(pending[0].silent);
        assert!(!pending[0].is_high_risk);
        assert_eq!(pending[0].status, "pending");

        supervisor
            .start_action_step_with_identity(
                &session.id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                step_id,
            )
            .await
            .unwrap();
        supervisor
            .finish_step_with_outcome(
                &session.id,
                "files.read",
                &input,
                5,
                3,
                Some("provider-call-5"),
                step_id,
                "cancelled during execution",
                ActionStepOutcome::Cancelled,
            )
            .await;

        let finished = db.get_session_steps(&session.id).unwrap();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].status, "cancelled");
        assert_eq!(
            finished[0].observation.as_deref(),
            Some("cancelled during execution")
        );
        assert!(finished[0].started_at.is_some());
        assert!(finished[0].completed_at.is_some());
    }
}

#[cfg(test)]
mod interaction_owner_route_tests {
    use super::*;
    use serde_json::json;

    fn future_receipt(capability: &str) -> haven_tools::ConfirmationReceipt {
        haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::new_id("conf").into(),
            capability: haven_common::types::CapabilityScope::try_new(capability).unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: haven_common::types::RiskLevel::High,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        }
    }

    #[test]
    fn owner_must_match_the_pending_request_kind_and_route_key() {
        let session_id = "ses-1234567890abcdef1234567890abcdef";
        let session_request = crate::interaction::InteractionRequest::confirm(
            session_id,
            1,
            "files.write".into(),
            json!({"path": "notes.txt"}),
            "call-session".into(),
            "step-session".into(),
            0,
            haven_common::types::RiskLevel::High,
            Some(future_receipt("files.write")),
        );
        let session_owner = crate::interaction::InteractionOwner::Session {
            session_id: session_id.into(),
        };
        assert!(interaction_owner_matches_request(
            &session_owner,
            &session_request
        ));
        assert!(!interaction_owner_matches_request(
            &crate::interaction::InteractionOwner::Session {
                session_id: "ses-other".into()
            },
            &session_request
        ));
        assert!(!interaction_owner_matches_request(
            &crate::interaction::InteractionOwner::AppCommand,
            &session_request
        ));

        let action_id = "act-1234567890abcdef1234567890abcdef";
        let receipt = future_receipt("files.write");
        let scheduled_request = crate::interaction::InteractionRequest::scheduled_confirm(
            action_id.into(),
            Some(session_id),
            "files.write".into(),
            json!({"path": "notes.txt"}),
            receipt,
            "scheduled".into(),
        );
        assert!(interaction_owner_matches_request(
            &crate::interaction::InteractionOwner::ScheduledAction {
                action_id: action_id.into()
            },
            &scheduled_request
        ));
        assert!(!interaction_owner_matches_request(
            &session_owner,
            &scheduled_request
        ));
    }
}
