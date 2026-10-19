//! Explicit tool admission contract for one ReAct call.
//!
//! The decision is made for a stable call identity and every call in a batch
//! must finish admission before the executor starts any of them.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::Value;

/// Pre-tool gate decision.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum BeforeToolCallDecision {
    /// Run the tool now (auto-approved or already confirmed).
    Proceed {
        receipt: Option<haven_tools::ConfirmationReceipt>,
    },
    /// Safety policy blocked the tool — emit a failed observation, do not run.
    Block { error: String },
    /// Needs user confirmation — pause the session (like ask) before running.
    NeedConfirm {
        receipt: haven_tools::ConfirmationReceipt,
    },
}

/// Stable identity of one tool call within a ReAct step. Gate decisions must
/// use this identity; tool names and JSON arguments are not unique.
#[derive(Debug, Clone)]
pub(crate) struct ToolCallIdentity {
    pub step_id: String,
    pub tool_index: u32,
    pub tool_call_id: Option<String>,
}

/// Owned inputs let pre-tool gates run concurrently without retaining borrows
/// across async calls.
pub(crate) struct BeforeToolRequest {
    pub executor: Arc<crate::session::SessionSupervisor>,
    pub session_id: String,
    pub catalog: haven_tools::ToolCatalogSnapshot,
    pub identity: ToolCallIdentity,
    pub tool_name: String,
    pub input: Value,
}

/// Admission must be implemented explicitly by every collaborator. There is
/// intentionally no default decision: an omitted security policy must fail to
/// compile instead of silently allowing tool execution.
pub(crate) trait ToolAdmission: Send + Sync {
    fn admit_tool(&self, request: BeforeToolRequest) -> BoxFuture<'static, BeforeToolCallDecision>;
}

pub(crate) type ToolAdmissionHandle = Arc<dyn ToolAdmission>;

/// Test-only collaborator that keeps loop tests inert by rejecting all tool
/// calls. Production composition uses the authorization and confirmation gate.
#[cfg(test)]
pub(crate) struct RejectAllToolAdmission;

#[cfg(test)]
impl ToolAdmission for RejectAllToolAdmission {
    fn admit_tool(
        &self,
        _request: BeforeToolRequest,
    ) -> BoxFuture<'static, BeforeToolCallDecision> {
        Box::pin(async {
            BeforeToolCallDecision::Block {
                error: "tool admission is not configured in this test".into(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn inert_admission_rejects_tool_calls() {
        let db = Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let tools = Arc::new(haven_tools::ToolsFacade::new());
        let session_id = "ses-37d95174ca2a846e8425178b7dc6ed84";
        let catalog = tools.tool_catalog_snapshot(session_id).await;
        let executor = Arc::new(crate::session::SessionSupervisor::new_for_test(
            db, tools, 1,
        ));
        let decision = RejectAllToolAdmission
            .admit_tool(BeforeToolRequest {
                executor,
                session_id: session_id.into(),
                catalog,
                identity: ToolCallIdentity {
                    step_id: "step-37d95174ca2a846e8425178b7dc6ed84".into(),
                    tool_index: 0,
                    tool_call_id: Some("provider-call".into()),
                },
                tool_name: "files.read".into(),
                input: Value::Null,
            })
            .await;

        assert!(matches!(decision, BeforeToolCallDecision::Block { .. }));
    }
}
