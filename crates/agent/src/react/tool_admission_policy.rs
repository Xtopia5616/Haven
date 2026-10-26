//! Production authorization and confirmation admission for ReAct tool calls.

use futures_util::future::BoxFuture;
use haven_common::hooks::{LifecycleHookDecision, LifecycleHookEvent, LifecycleHookPayload};
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
                    if request.cancel.is_cancelled() {
                        return BeforeToolCallDecision::Block {
                            error: format!(
                                "operation '{}' was cancelled before its confirmation prompt",
                                request.tool_name
                            ),
                        };
                    }
                    let mut payload =
                        LifecycleHookPayload::new(LifecycleHookEvent::PermissionRequest);
                    payload.session_id = Some(request.session_id.clone());
                    payload.run_id = Some(request.run_id);
                    payload.step_number = Some(request.step_number);
                    payload.tool_name = Some(request.tool_name.clone());
                    payload.tool_input = Some(request.input.clone());
                    match request
                        .lifecycle_hooks
                        .run(payload, request.cancel.clone())
                        .await
                    {
                        LifecycleHookDecision::Allow => {
                            // Allow only reaches the regular Haven confirmation flow.
                            BeforeToolCallDecision::NeedConfirm { receipt }
                        }
                        LifecycleHookDecision::Deny { reason } => BeforeToolCallDecision::Block {
                            error: format!(
                                "operation '{}' was blocked by the PermissionRequest hook ({reason}); explain the block and ask what the user wants to do next",
                                request.tool_name
                            ),
                        },
                    }
                }
            }
        })
    }
}

pub(crate) fn default_tool_admission() -> ToolAdmissionHandle {
    Arc::new(DefaultToolAdmission)
}
