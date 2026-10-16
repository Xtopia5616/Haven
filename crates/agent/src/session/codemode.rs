//! Agent-owned authorization bridge for nested CodeMode tool calls.
//!
//! A script can name only tools from its resolved per-run allowlist. Every
//! child is independently authorized and then sent through the ordinary
//! proof-carrying ToolExecutionPort. Confirm-required operations fail closed:
//! the current ReAct confirmation lifecycle cannot resume an in-memory JS
//! continuation after a pause or process restart.

use crate::session::tool_ports::{
    ToolAuthorizationPort, ToolExecutionContext, ToolExecutionPort,
    sandbox_output_review_matches_step,
};
use async_trait::async_trait;
use haven_common::types::is_canonical_id;
use haven_tools::{AuthorizationDecision, CodeModeCallResult, CodeModeSubcallPort};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub(super) struct AuthorizedCodeModeSubcallPort {
    session_id: String,
    allowed_tool_names: BTreeSet<String>,
    authorization: Arc<dyn ToolAuthorizationPort>,
    execution: Arc<dyn ToolExecutionPort>,
    sandbox_output_review_step_ids: Mutex<Vec<String>>,
}

impl AuthorizedCodeModeSubcallPort {
    pub(super) fn new(
        session_id: &str,
        allowed_tool_names: impl IntoIterator<Item = String>,
        authorization: Arc<dyn ToolAuthorizationPort>,
        execution: Arc<dyn ToolExecutionPort>,
    ) -> Self {
        Self {
            session_id: session_id.to_owned(),
            allowed_tool_names: allowed_tool_names.into_iter().collect(),
            authorization,
            execution,
            sandbox_output_review_step_ids: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl CodeModeSubcallPort for AuthorizedCodeModeSubcallPort {
    async fn call(
        &self,
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
        let step_id = haven_common::types::new_id("step");

        let request = self
            .authorization
            .authorization_request_for_codemode_subcall(Some(&self.session_id), tool_name, &input)
            .await?;
        let authorization = self.authorization.authorize_for_execution(&request).await;
        let proof = match authorization.decision {
            AuthorizationDecision::AutoApproved => authorization
                .execution_proof
                .ok_or_else(|| "authorization owner did not issue an execution proof".to_owned())?,
            AuthorizationDecision::Blocked { reason, .. } => {
                return Err(format!("CodeMode child operation is blocked: {reason}"));
            }
            AuthorizationDecision::RequiresConfirmation { .. } => {
                return Err(format!(
                    "operation '{tool_name}' requires its own user confirmation; issue it as a Legacy tool call"
                ));
            }
        };

        let context = ToolExecutionContext {
            session_id: Some(self.session_id.clone()),
            tool_name: tool_name.to_owned(),
            input,
            cancel,
            step_id: Some(step_id.clone()),
            authorization_proof: Some(proof),
        };
        let result = self
            .execution
            .execute_codemode_subcall(context)
            .await
            .map_err(|error| error.to_string())?;

        if sandbox_output_review_matches_step(&result, &step_id) {
            let mut step_ids = self
                .sandbox_output_review_step_ids
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !step_ids.contains(&step_id)
                && step_ids.len() < haven_common::tools::MAX_SANDBOX_OUTPUT_REVIEW_STEP_IDS
            {
                step_ids.push(step_id);
            }
        }

        Ok(CodeModeCallResult {
            success: result.success,
            output: result.output,
            error: result.error,
            truncated: result.truncated,
        })
    }

    fn sandbox_output_review_step_ids(&self) -> Vec<String> {
        self.sandbox_output_review_step_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
