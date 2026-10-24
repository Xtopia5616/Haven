//! Production authorization and confirmation admission for ReAct tool calls.

use futures_util::future::BoxFuture;
use haven_tools::AuthorizationDecision;
use std::sync::Arc;

use super::tool_admission::{
    BeforeToolCallDecision, BeforeToolRequest, ToolAdmission, ToolAdmissionHandle,
};

pub(crate) struct DefaultToolAdmission;

impl ToolAdmission for DefaultToolAdmission {
    fn admit_tool(&self, request: BeforeToolRequest) -> BoxFuture<'static, BeforeToolCallDecision> {
        Box::pin(async move {
            // Resume path: a prior confirm pause already recorded a decision.
            if let Some(decision) = request
                .executor
                .confirm_decision_for(
                    &request.session_id,
                    &request.identity.step_id,
                    request.identity.tool_index,
                    request.identity.tool_call_id.as_deref(),
                )
                .await
            {
                return if decision.approved {
                    BeforeToolCallDecision::Proceed {
                        receipt: decision.receipt,
                    }
                } else {
                    BeforeToolCallDecision::Block {
                        error: format!(
                            "The user declined confirmation for operation '{}'. Ask what they want to do next; any new attempt needs fresh confirmation.",
                            request.tool_name
                        ),
                    }
                };
            }

            match request
                .executor
                .check_tool_gate_with_catalog(
                    &request.session_id,
                    &request.tool_name,
                    &request.input,
                    &request.catalog,
                )
                .await
            {
                AuthorizationDecision::AutoApproved => {
                    BeforeToolCallDecision::Proceed { receipt: None }
                }
                AuthorizationDecision::Blocked { reason, .. } => BeforeToolCallDecision::Block {
                    error: format!(
                        "operation '{}' was blocked before execution by the security policy ({reason}). Explain the policy boundary and use an authorized capability, or ask the user whether they want to change that setting.",
                        request.tool_name
                    ),
                },
                AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
                    BeforeToolCallDecision::NeedConfirm { receipt }
                }
            }
        })
    }
}

pub(crate) fn default_tool_admission() -> ToolAdmissionHandle {
    Arc::new(DefaultToolAdmission)
}
