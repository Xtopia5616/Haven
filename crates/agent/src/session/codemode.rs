//! Agent-owned authorization bridge for nested CodeMode tool calls.
//!
//! A script can name only tools from its resolved per-run allowlist. Every
//! child is independently authorized and then sent through the ordinary
//! proof-carrying ToolExecutionPort. Confirm-required operations fail closed:
//! the current ReAct confirmation lifecycle cannot resume an in-memory JS
//! continuation after a pause or process restart.

use crate::session::nested_execution::{NestedCallCompletion, NestedExecutionJournal};
use crate::session::tool_ports::{
    ToolAuthorizationPort, ToolExecutionContext, ToolExecutionPort,
    sandbox_output_review_matches_step,
};
use async_trait::async_trait;
use haven_common::types::is_canonical_id;
use haven_tools::{AuthorizationDecision, CodeModeCallResult, CodeModeSubcallPort};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU32, Ordering},
};
use tokio_util::sync::CancellationToken;

pub(super) struct AuthorizedCodeModeSubcallPort {
    session_id: String,
    allowed_tool_names: BTreeSet<String>,
    authorization: Arc<dyn ToolAuthorizationPort>,
    execution: Arc<dyn ToolExecutionPort>,
    journal: Option<Arc<NestedExecutionJournal>>,
    resume_confirmation: Option<haven_tools::NestedConfirmationRequest>,
    pending_confirmation: Mutex<Option<haven_tools::NestedConfirmationRequest>>,
    next_call_index: AtomicU32,
    sandbox_output_review_step_ids: Mutex<Vec<String>>,
}

impl AuthorizedCodeModeSubcallPort {
    pub(super) fn new(
        session_id: &str,
        allowed_tool_names: impl IntoIterator<Item = String>,
        authorization: Arc<dyn ToolAuthorizationPort>,
        execution: Arc<dyn ToolExecutionPort>,
        journal: Option<Arc<NestedExecutionJournal>>,
        resume_confirmation: Option<haven_tools::NestedConfirmationRequest>,
    ) -> Self {
        Self {
            session_id: session_id.to_owned(),
            allowed_tool_names: allowed_tool_names.into_iter().collect(),
            authorization,
            execution,
            journal,
            resume_confirmation,
            pending_confirmation: Mutex::new(None),
            next_call_index: AtomicU32::new(0),
            sandbox_output_review_step_ids: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl CodeModeSubcallPort for AuthorizedCodeModeSubcallPort {
    async fn call(
        &self,
        call_index: u32,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> Result<CodeModeCallResult, String> {
        if !is_canonical_id(&self.session_id, "ses") {
            return Err("CodeMode session context is invalid".into());
        }
        if !self.allowed_tool_names.contains(tool_name) {
            return Err("tool is not in this CodeMode run's allowlist".into());
        }
        if !input.is_object() {
            return Err("CodeMode child input must be a JSON object".into());
        }
        if cancel.is_cancelled() {
            return Err("CodeMode child call was cancelled".into());
        }
        let expected_call_index = self.next_call_index.fetch_add(1, Ordering::Relaxed);
        if call_index != expected_call_index {
            return Err("CodeMode child call sequence is invalid".into());
        }
        if self
            .pending_confirmation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
        {
            return Err("CodeMode is paused for a nested confirmation".into());
        }
        let resumed = self.resume_confirmation.as_ref();
        if resumed.is_some_and(|entry| entry.call_index > call_index) && self.journal.is_none() {
            return Err("CodeMode confirmation replay has no durable call journal".into());
        }
        let proposed_step_id =
            if let Some(resumed) = resumed.filter(|entry| entry.call_index == call_index) {
                if resumed.tool_name != tool_name || resumed.input != input {
                    return Err("CodeMode replay diverged before its pending confirmation".into());
                }
                resumed.step_id.clone()
            } else {
                haven_common::types::new_id("step")
            };
        let replay = match &self.journal {
            Some(journal) => match journal
                .prepare_call(call_index, &proposed_step_id, tool_name, &input)
                .await?
            {
                Ok(step_id) => {
                    if resumed.is_some_and(|entry| entry.call_index != call_index)
                        && call_index < resumed.map_or(0, |entry| entry.call_index)
                    {
                        return Err("CodeMode replay skipped its pending confirmation call".into());
                    }
                    (step_id, None)
                }
                Err(completed) => (completed.child_step_id.clone(), Some(completed)),
            },
            None => (proposed_step_id, None),
        };
        let (step_id, completed) = replay;

        let request = self
            .authorization
            .authorization_request_for_codemode_subcall(Some(&self.session_id), tool_name, &input)
            .await?;
        let proof = if let Some(resumed) = resumed.filter(|entry| entry.call_index == call_index) {
            self.authorization
                .confirm_for_execution(&request, &resumed.receipt)
                .await
                .map_err(|reason| {
                    format!("CodeMode child confirmation is no longer valid: {reason}")
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
                    return Err(format!("CodeMode child operation is blocked: {reason}"));
                }
                AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
                    *self
                        .pending_confirmation
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(haven_tools::NestedConfirmationRequest {
                            call_index,
                            step_id: step_id.clone(),
                            tool_name: tool_name.to_owned(),
                            input: input.clone(),
                            receipt,
                        });
                    return Err(format!(
                        "operation '{tool_name}' requires its own user confirmation"
                    ));
                }
            }
        };

        if let Some(completed) = completed {
            let result = completed.result;
            if let Some(review_step_id) = completed.sandbox_output_review_step_id
                && sandbox_output_review_matches_step(&result, &review_step_id)
            {
                let mut step_ids = self
                    .sandbox_output_review_step_ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !step_ids.contains(&review_step_id)
                    && step_ids.len() < haven_common::tools::MAX_SANDBOX_OUTPUT_REVIEW_STEP_IDS
                {
                    step_ids.push(review_step_id);
                }
            }
            return Ok(CodeModeCallResult {
                success: result.success,
                output: result.output,
                error: result.error,
                truncated: result.truncated,
            });
        }

        if let Some(journal) = &self.journal {
            journal
                .start_call(call_index, &step_id, tool_name, &input)
                .await?;
        }

        let context = ToolExecutionContext {
            session_id: Some(self.session_id.clone()),
            tool_name: tool_name.to_owned(),
            input: input.clone(),
            cancel,
            step_id: Some(step_id.clone()),
            authorization_proof: Some(proof),
        };
        let result = self
            .execution
            .execute_codemode_subcall(context)
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
                "CodeMode child operation '{tool_name}' has an unknown outcome; it was not replayed"
            ));
        }

        if sandbox_output_review_matches_step(&result, &step_id) {
            let mut step_ids = self
                .sandbox_output_review_step_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !step_ids.contains(&step_id)
                && step_ids.len() < haven_common::tools::MAX_SANDBOX_OUTPUT_REVIEW_STEP_IDS
            {
                step_ids.push(step_id.clone());
            }
        }

        if let Some(journal) = &self.journal {
            journal
                .finish_call(NestedCallCompletion {
                    call_index,
                    child_step_id: &step_id,
                    tool_name,
                    input: &input,
                    result: &result,
                    sandbox_output_review_step_id: None,
                })
                .await?;
        }

        Ok(CodeModeCallResult {
            success: result.success,
            output: result.output,
            error: result.error,
            truncated: result.truncated,
        })
    }

    fn pending_confirmation(&self) -> Option<haven_tools::NestedConfirmationRequest> {
        self.pending_confirmation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn sandbox_output_review_step_ids(&self) -> Vec<String> {
        self.sandbox_output_review_step_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
