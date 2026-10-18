//! Agent-owned authorization bridge for declarative Workflow child calls.
//!
//! The workflow plan contains only names and JSON inputs. This bridge binds
//! those names to one session's immutable allowlist, obtains an independent
//! proof for each child, and delegates to the normal Tools execution gateway.

use crate::session::nested_execution::{NestedCallCompletion, NestedExecutionJournal};
use crate::session::tool_ports::{
    AgentToolRunPort, ToolAuthorizationPort, ToolExecutionContext, ToolExecutionPort,
    ToolObservationPort, sandbox_output_review_matches_step,
};
use async_trait::async_trait;
use haven_common::{tool_run::ScheduleMode, types::is_canonical_id};
use haven_tools::{AuthorizationDecision, WorkflowSubcallPort, WorkflowSubcallResult};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use tokio_util::sync::CancellationToken;

pub(super) struct AuthorizedWorkflowSubcallConfig {
    pub(super) session_id: String,
    pub(super) allowed_tool_names: Vec<String>,
    pub(super) authorization: Arc<dyn ToolAuthorizationPort>,
    pub(super) execution: Arc<dyn ToolExecutionPort>,
    pub(super) observations: Arc<dyn ToolObservationPort>,
    pub(super) tool_runs: Arc<dyn AgentToolRunPort>,
    pub(super) journal: Option<Arc<NestedExecutionJournal>>,
    pub(super) resume_confirmation: Option<haven_tools::NestedConfirmationRequest>,
}

pub(super) struct AuthorizedWorkflowSubcallPort {
    session_id: String,
    allowed_tool_names: BTreeSet<String>,
    authorization: Arc<dyn ToolAuthorizationPort>,
    execution: Arc<dyn ToolExecutionPort>,
    observations: Arc<dyn ToolObservationPort>,
    tool_runs: Arc<dyn AgentToolRunPort>,
    journal: Option<Arc<NestedExecutionJournal>>,
    resume_confirmation: Option<haven_tools::NestedConfirmationRequest>,
    pending_confirmation: Mutex<Option<haven_tools::NestedConfirmationRequest>>,
    next_call_index: AtomicU32,
}

impl AuthorizedWorkflowSubcallPort {
    pub(super) fn new(config: AuthorizedWorkflowSubcallConfig) -> Self {
        Self {
            session_id: config.session_id,
            allowed_tool_names: config.allowed_tool_names.into_iter().collect(),
            authorization: config.authorization,
            execution: config.execution,
            observations: config.observations,
            tool_runs: config.tool_runs,
            journal: config.journal,
            resume_confirmation: config.resume_confirmation,
            pending_confirmation: Mutex::new(None),
            next_call_index: AtomicU32::new(0),
        }
    }
}

#[async_trait]
impl WorkflowSubcallPort for AuthorizedWorkflowSubcallPort {
    async fn schedule_run(
        &self,
        mut input: Value,
        due_at: Option<String>,
    ) -> Result<String, String> {
        if !is_canonical_id(&self.session_id, "ses") {
            return Err("Workflow session context is invalid".into());
        }
        let object = input
            .as_object_mut()
            .ok_or_else(|| "Workflow input must be an object".to_owned())?;
        object.insert("execution_mode".into(), Value::String("inline".into()));
        object.remove("run_at");
        let delay_secs = due_at.is_none().then_some(1);
        self.tool_runs
            .schedule_workflow_run(haven_tools::ScheduledToolRunSpec {
                due_at,
                delay_secs,
                watch_tool_run_id: None,
                title: "Workflow".into(),
                body: "Run a Workflow".into(),
                mode: ScheduleMode::Tool,
                session_id: Some(self.session_id.clone()),
                tool_name: Some(haven_tools::WORKFLOW_RUN_TOOL_NAME.into()),
                tool_args: Some(input),
                prompt: None,
            })
            .await
            .map_err(|error| format!("Workflow ToolRun could not be scheduled: {error}"))
    }

    async fn call(
        &self,
        workflow_step_id: &str,
        capability: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<WorkflowSubcallResult, String> {
        if !is_canonical_id(&self.session_id, "ses") {
            return Err("Workflow session context is invalid".into());
        }
        if !self.allowed_tool_names.contains(capability) {
            return Err("capability is not in this Workflow run's allowlist".into());
        }
        if !capability.starts_with("mcp__") || !input.is_object() {
            return Err(
                "Workflow child must be an allowlisted MCP tool with an object input".into(),
            );
        }
        if cancel.is_cancelled() {
            return Err("Workflow child call was cancelled".into());
        }
        if workflow_step_id.is_empty() || workflow_step_id.chars().any(char::is_control) {
            return Err("Workflow step identity is invalid".into());
        }
        let call_index = self.next_call_index.fetch_add(1, Ordering::Relaxed);
        if self
            .pending_confirmation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return Err("Workflow is paused for a nested confirmation".into());
        }
        let resumed = self.resume_confirmation.as_ref();
        if resumed.is_some_and(|entry| entry.call_index > call_index) && self.journal.is_none() {
            return Err("Workflow confirmation replay has no durable call journal".into());
        }
        let proposed_step_id =
            if let Some(resumed) = resumed.filter(|entry| entry.call_index == call_index) {
                if resumed.tool_name != capability || resumed.input != input {
                    return Err("Workflow replay diverged before its pending confirmation".into());
                }
                resumed.step_id.clone()
            } else {
                haven_common::types::new_id("step")
            };
        let journal_input = json!({
            "workflow_step_id": workflow_step_id,
            "input": input,
        });
        let (step_id, completed) = match &self.journal {
            Some(journal) => match journal
                .prepare_call(call_index, &proposed_step_id, capability, &journal_input)
                .await?
            {
                Ok(step_id) => {
                    if resumed.is_some_and(|entry| entry.call_index != call_index)
                        && call_index < resumed.map_or(0, |entry| entry.call_index)
                    {
                        return Err("Workflow replay skipped its pending confirmation step".into());
                    }
                    (step_id, None)
                }
                Err(completed) => (completed.child_step_id.clone(), Some(completed)),
            },
            None => (proposed_step_id, None),
        };

        let request = self
            .authorization
            .authorization_request_for_workflow_subcall(Some(&self.session_id), capability, &input)
            .await?;
        let proof = if let Some(resumed) = resumed.filter(|entry| entry.call_index == call_index) {
            self.authorization
                .confirm_for_execution(&request, &resumed.receipt)
                .await
                .map_err(|reason| {
                    format!("Workflow child confirmation is no longer valid: {reason}")
                })?
        } else {
            let authorization = self.authorization.authorize_for_execution(&request).await;
            match authorization.decision {
                AuthorizationDecision::AutoApproved => {
                    authorization.execution_proof.ok_or_else(|| {
                        "authorization owner did not issue an execution proof".to_owned()
                    })?
                }
                AuthorizationDecision::Blocked { reason, .. } => {
                    return Err(format!("Workflow child operation is blocked: {reason}"));
                }
                AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
                    *self
                        .pending_confirmation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(haven_tools::NestedConfirmationRequest {
                            call_index,
                            step_id: step_id.clone(),
                            tool_name: capability.to_owned(),
                            input: input.clone(),
                            receipt,
                        });
                    return Err(format!(
                        "operation '{capability}' requires its own user confirmation"
                    ));
                }
            }
        };

        if let Some(completed) = completed {
            let observation = completed
                .observation
                .ok_or_else(|| "Workflow checkpoint is missing its observation".to_owned())?;
            return Ok(WorkflowSubcallResult {
                result: completed.result,
                observation,
                sandbox_output_review_step_id: completed.sandbox_output_review_step_id,
            });
        }

        if let Some(journal) = &self.journal {
            journal
                .start_call(call_index, &step_id, capability, &journal_input)
                .await?;
        }

        let result = self
            .execution
            .execute_workflow_subcall(ToolExecutionContext {
                session_id: Some(self.session_id.clone()),
                tool_name: capability.to_owned(),
                input,
                cancel,
                step_id: Some(step_id.clone()),
                authorization_proof: Some(proof),
            })
            .await
            .map_err(|error| error.to_string())?;
        if result.outcome == haven_common::tools::ToolExecutionOutcome::Cancelled
            || result.outcome == haven_common::tools::ToolExecutionOutcome::TimedOutUnknown
            || matches!(
                result.error_class,
                Some(
                    haven_common::tools::ToolErrorClass::UnknownOutcome
                        | haven_common::tools::ToolErrorClass::SideEffectMayHaveHappened
                )
            )
        {
            return Err(format!(
                "Workflow child operation '{capability}' has an unknown outcome; it was not replayed"
            ));
        }
        let has_sandbox_output_review = sandbox_output_review_matches_step(&result, &step_id);
        let observation = self
            .observations
            .observation_text(capability, &result)
            .await;
        if let Some(journal) = &self.journal {
            journal
                .complete_call(NestedCallCompletion {
                    call_index,
                    child_step_id: &step_id,
                    tool_name: capability,
                    input: &journal_input,
                    result: &result,
                    observation: Some(&observation),
                    sandbox_output_review_step_id: has_sandbox_output_review
                        .then_some(step_id.as_str()),
                })
                .await?;
        }
        Ok(WorkflowSubcallResult {
            result,
            observation,
            sandbox_output_review_step_id: has_sandbox_output_review.then_some(step_id),
        })
    }

    fn pending_confirmation(&self) -> Option<haven_tools::NestedConfirmationRequest> {
        self.pending_confirmation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
