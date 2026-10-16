//! Agent-owned authorization bridge for declarative Workflow child calls.
//!
//! The workflow plan contains only names and JSON inputs. This bridge binds
//! those names to one session's immutable allowlist, obtains an independent
//! proof for each child, and delegates to the normal Tools execution gateway.

use crate::session::tool_ports::{
    ToolAuthorizationPort, ToolExecutionContext, ToolExecutionPort, ToolObservationPort,
};
use async_trait::async_trait;
use haven_common::types::is_canonical_id;
use haven_tools::{AuthorizationDecision, WorkflowSubcallPort, WorkflowSubcallResult};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct AuthorizedWorkflowSubcallPort {
    session_id: String,
    allowed_tool_names: BTreeSet<String>,
    authorization: Arc<dyn ToolAuthorizationPort>,
    execution: Arc<dyn ToolExecutionPort>,
    observations: Arc<dyn ToolObservationPort>,
}

impl AuthorizedWorkflowSubcallPort {
    pub(super) fn new(
        session_id: &str,
        allowed_tool_names: impl IntoIterator<Item = String>,
        authorization: Arc<dyn ToolAuthorizationPort>,
        execution: Arc<dyn ToolExecutionPort>,
        observations: Arc<dyn ToolObservationPort>,
    ) -> Self {
        Self {
            session_id: session_id.to_owned(),
            allowed_tool_names: allowed_tool_names.into_iter().collect(),
            authorization,
            execution,
            observations,
        }
    }
}

#[async_trait]
impl WorkflowSubcallPort for AuthorizedWorkflowSubcallPort {
    async fn call(
        &self,
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
        let step_id = haven_common::types::new_id("step");

        let request = self
            .authorization
            .authorization_request_for_workflow_subcall(Some(&self.session_id), capability, &input)
            .await?;
        let authorization = self.authorization.authorize_for_execution(&request).await;
        let proof = match authorization.decision {
            AuthorizationDecision::AutoApproved => authorization
                .execution_proof
                .ok_or_else(|| "authorization owner did not issue an execution proof".to_owned())?,
            AuthorizationDecision::Blocked { reason, .. } => {
                return Err(format!("Workflow child operation is blocked: {reason}"));
            }
            AuthorizationDecision::RequiresConfirmation { .. } => {
                return Err(format!(
                    "operation '{capability}' requires its own user confirmation; issue it as a Legacy tool call"
                ));
            }
        };

        let result = self
            .execution
            .execute_workflow_subcall(ToolExecutionContext {
                session_id: Some(self.session_id.clone()),
                tool_name: capability.to_owned(),
                input,
                cancel,
                step_id: Some(step_id),
                authorization_proof: Some(proof),
            })
            .await
            .map_err(|error| error.to_string())?;
        let observation = self
            .observations
            .observation_text(capability, &result)
            .await;
        Ok(WorkflowSubcallResult {
            result,
            observation,
        })
    }
}
