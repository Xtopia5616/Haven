use super::*;
use haven_memory::ToolStepWrite;

/// The tool may already have produced an external side effect when its final
/// ToolCall-step projection fails. Callers must surface this as an unknown
/// outcome, never as an ordinary retryable failure.
#[derive(Debug)]
pub(crate) struct ToolStepPersistenceError(pub(super) anyhow::Error);

/// Metadata resolved from the ReAct turn's immutable tool catalog.
/// Authorization and risk checks remain live; this only avoids reopening the
/// catalog for ToolCall-step bookkeeping on every tool call.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ToolStepMetadata {
    pub(crate) is_high_risk: bool,
    pub(crate) silent: bool,
}

impl std::fmt::Display for ToolStepPersistenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "tool executed but ToolCall-step persistence failed: {}",
            self.0
        )
    }
}

impl std::error::Error for ToolStepPersistenceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

pub(super) fn tool_step_outcome(result: &ToolResult) -> ToolStepOutcome {
    match result.outcome {
        haven_common::tools::ToolExecutionOutcome::Succeeded => ToolStepOutcome::Completed,
        haven_common::tools::ToolExecutionOutcome::Cancelled => ToolStepOutcome::Cancelled,
        haven_common::tools::ToolExecutionOutcome::TimedOutUnknown => ToolStepOutcome::Unknown,
        haven_common::tools::ToolExecutionOutcome::Failed
        | haven_common::tools::ToolExecutionOutcome::TimedOutAndTerminated => {
            ToolStepOutcome::Failed
        }
    }
}

pub(super) struct ToolStepRequest<'a> {
    pub(super) session_id: &'a str,
    pub(super) tool_name: &'a str,
    pub(super) input: &'a Value,
    pub(super) step_number: u32,
    pub(super) tool_index: u32,
    pub(super) tool_call_id: Option<&'a str>,
    pub(super) step_id: &'a str,
}

pub(super) struct ToolStepContext {
    pub(super) session_id: String,
    pub(super) step_number: u32,
    pub(super) tool_index: i32,
    pub(super) tool_name: String,
    pub(super) tool_input: String,
    pub(super) tool_call_id: Option<String>,
    pub(super) is_high_risk: bool,
    pub(super) silent: bool,
    pub(super) step_id: String,
}

impl ToolStepContext {
    pub(super) fn new(request: ToolStepRequest<'_>, risk_level: RiskLevel) -> Self {
        let metadata = ToolStepMetadata {
            is_high_risk: risk_level != RiskLevel::Safe,
            silent: is_silent_tool_call(request.tool_name, request.input),
        };
        Self::new_with_metadata(request, metadata)
    }

    pub(super) fn new_with_metadata(
        request: ToolStepRequest<'_>,
        metadata: ToolStepMetadata,
    ) -> Self {
        Self {
            session_id: request.session_id.into(),
            step_number: request.step_number,
            tool_index: request.tool_index as i32,
            tool_name: request.tool_name.into(),
            tool_input: request.input.to_string(),
            tool_call_id: request.tool_call_id.map(str::to_string),
            is_high_risk: metadata.is_high_risk,
            silent: metadata.silent,
            step_id: request.step_id.into(),
        }
    }

    pub(super) fn into_write(self) -> ToolStepWrite {
        ToolStepWrite {
            session_id: self.session_id,
            step_number: self.step_number,
            tool_index: self.tool_index,
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
    pub(super) async fn tool_step_context(&self, request: ToolStepRequest<'_>) -> ToolStepContext {
        let risk_level = match self
            .authorization
            .risk_level(Some(request.session_id), request.tool_name, request.input)
            .await
        {
            Ok(risk_level) => risk_level,
            Err(error) => {
                tracing::warn!(
                    session_id = %request.session_id,
                    step_id = %request.step_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "could not resolve tool risk for step metadata; recording as critical"
                );
                RiskLevel::Critical
            }
        };
        ToolStepContext::new(request, risk_level)
    }

    /// Persist a pending `session_steps` row under the pre-minted `step-*` id
    /// at ToolCall-emit time — before the tool runs. Interrupted / cancelled
    /// tools never reach `execute_step`'s post-completion write, so without
    /// this the live card is the only copy and drops on every DB rebuild
    /// (Continue resync, session switch, app restart).
    pub async fn begin_tool_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        step_id: &str,
    ) -> anyhow::Result<()> {
        self.begin_tool_step_with_identity(
            session_id,
            tool_name,
            input,
            step_number,
            0,
            None,
            step_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn begin_tool_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
    ) -> anyhow::Result<()> {
        let context = self
            .tool_step_context(ToolStepRequest {
                session_id,
                tool_name,
                input,
                step_number,
                tool_index,
                tool_call_id,
                step_id,
            })
            .await;
        self.persist_pending_tool_step_context(context).await
    }

    async fn persist_pending_tool_step_context(
        &self,
        context: ToolStepContext,
    ) -> anyhow::Result<()> {
        let step_id_for_log = context.step_id.clone();
        self.store
            .ensure_tool_step(context.into_write(), None)
            .await
            .map_err(|e| {
                tracing::error!(
                    step_id = %step_id_for_log,
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "begin_tool_step failed"
                );
                anyhow::anyhow!("failed to persist pending tool intent {step_id_for_log}: {e}")
            })
    }

    /// Persist an Interrupted observation onto the pending step row (creating
    /// it if ToolCall-time begin failed). Keeps the resume badge aligned with
    /// the live Interrupted card across resume/resync.
    pub async fn finish_interrupted_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        step_id: &str,
        observation: &str,
    ) {
        self.finish_step_with_outcome(
            session_id,
            tool_name,
            input,
            step_number,
            0,
            None,
            step_id,
            observation,
            ToolStepOutcome::Failed,
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
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
        outcome: ToolStepOutcome,
    ) {
        let context = self
            .tool_step_context(ToolStepRequest {
                session_id,
                tool_name,
                input,
                step_number,
                tool_index,
                tool_call_id,
                step_id,
            })
            .await;
        self.finish_tool_step_context(context, observation, outcome)
            .await;
    }

    /// Finalize a batch ToolCall using metadata resolved from the batch's
    /// immutable catalog snapshot. The execution safety decision is still
    /// performed live by `execute_gated`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_step_with_outcome_and_metadata(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
        outcome: ToolStepOutcome,
        metadata: ToolStepMetadata,
    ) {
        let context = ToolStepContext::new_with_metadata(
            ToolStepRequest {
                session_id,
                tool_name,
                input,
                step_number,
                tool_index,
                tool_call_id,
                step_id,
            },
            metadata,
        );
        self.finish_tool_step_context(context, observation, outcome)
            .await;
    }

    async fn finish_tool_step_context(
        &self,
        context: ToolStepContext,
        observation: &str,
        outcome: ToolStepOutcome,
    ) {
        let step_id_for_log = context.step_id.clone();
        let observation = observation.to_string();
        if let Err(e) = self
            .store
            .ensure_and_finish_tool_step(context.into_write(), None, observation, outcome)
            .await
        {
            tracing::warn!(
                step_id = %step_id_for_log,
                error = %haven_common::error::sanitize_error_text(&e.to_string()),
                "finish_step_with_outcome failed"
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn finish_interrupted_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
    ) {
        self.finish_step_with_outcome(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            observation,
            ToolStepOutcome::Failed,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn finish_interrupted_step_with_identity_and_metadata(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
        observation: &str,
        metadata: ToolStepMetadata,
    ) {
        self.finish_step_with_outcome_and_metadata(
            session_id,
            tool_name,
            input,
            step_number,
            tool_index,
            tool_call_id,
            step_id,
            observation,
            ToolStepOutcome::Failed,
            metadata,
        )
        .await;
    }

    /// Move the ToolCall-emit pending row to running immediately before the
    /// tool is invoked. The ensure step keeps direct callers safe when no
    /// ToolCall event created the row first.
    pub async fn start_tool_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        step_id: &str,
    ) -> anyhow::Result<()> {
        self.start_tool_step_with_identity(
            session_id,
            tool_name,
            input,
            step_number,
            0,
            None,
            step_id,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn start_tool_step_with_identity(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_number: u32,
        tool_index: u32,
        tool_call_id: Option<&str>,
        step_id: &str,
    ) -> anyhow::Result<()> {
        let context = self
            .tool_step_context(ToolStepRequest {
                session_id,
                tool_name,
                input,
                step_number,
                tool_index,
                tool_call_id,
                step_id,
            })
            .await;
        self.start_running_tool_step_context(context).await
    }

    pub(super) async fn start_running_tool_step_context(
        &self,
        context: ToolStepContext,
    ) -> anyhow::Result<()> {
        let step_id_for_log = context.step_id.clone();
        let started = self
            .store
            .ensure_and_start_tool_step(context.into_write(), None)
            .await
            .map_err(|e| {
                tracing::error!(
                    step_id = %step_id_for_log,
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "start_tool_step failed"
                );
                anyhow::anyhow!("failed to mark tool intent running {step_id_for_log}: {e}")
            })?;
        anyhow::ensure!(
            started,
            "tool step {step_id_for_log} is already terminal; refusing to replay its side effect"
        );
        Ok(())
    }
}
