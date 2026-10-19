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
                            "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
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
                        "operation '{}' is blocked by the security policy ({reason}). Do NOT retry it — ask the user what to do instead or choose a different approach.",
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
