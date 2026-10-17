#[cfg(test)]
use super::execution_proof_expiry;
use super::{AuthorizationEngine, SafetyConfig, confirmation_now};
use crate::{
    StructuredToolError, ToolErrorClass, ToolErrorMetadata, ToolExecutionOutcome, ToolRetryability,
};
use haven_common::types::{NetworkPolicy, SandboxMode};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Short-lived side-effect capability derived from one proof-verified tool
/// attempt. Acquiring it holds a read lock on the live policy through the
/// protected operation, so a policy/workspace update either wins first and
/// rejects the stale proof or takes effect after the operation completes.
#[derive(Clone)]
pub(crate) struct ExecutionPolicyAuthorization {
    pub(super) authorization: Arc<AuthorizationEngine>,
    pub(super) policy_revision: u64,
    pub(super) execution_workspace_generation: u64,
    pub(super) execution_boundary: crate::ExecutionBoundary,
    pub(super) expires_at: u64,
    pub(super) process_revocation_signal: Arc<()>,
    pub(super) tool_settings_cancellation: Arc<CancellationToken>,
    pub(super) session_trust_cancellation: Arc<CancellationToken>,
}

/// Current process-boundary snapshot for a manager-owned MCP stdio client
/// generation. Unlike a tool proof it is not permission to call a tool; it
/// only permits a child process to start while sandbox mode is FullAccess
/// and opaque network access remains explicitly open.
#[derive(Clone)]
pub(crate) struct McpStdioProcessStartAuthorization {
    pub(super) authorization: Arc<AuthorizationEngine>,
    pub(super) process_boundary_revision: u64,
    pub(super) execution_workspace_generation: u64,
    pub(super) process_boundary_signal: Arc<()>,
}

pub(crate) struct ExecutionPolicyPermit {
    _config: tokio::sync::OwnedRwLockReadGuard<SafetyConfig>,
}

impl ExecutionPolicyPermit {
    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self._config.sandbox_mode
    }
}

pub(crate) fn execution_policy_denied(reason: String) -> anyhow::Error {
    anyhow::Error::new(StructuredToolError::new(
        reason,
        ToolErrorMetadata {
            class: ToolErrorClass::Permission,
            outcome: ToolExecutionOutcome::Failed,
            retryability: ToolRetryability::NotRetryable,
        },
    ))
}

impl ExecutionPolicyAuthorization {
    pub(crate) fn policy_stamp(&self) -> (u64, u64) {
        (self.policy_revision, self.execution_workspace_generation)
    }

    pub(crate) fn execution_boundary(&self) -> crate::ExecutionBoundary {
        self.execution_boundary
    }

    pub(crate) async fn acquire(&self) -> Result<ExecutionPolicyPermit, String> {
        if confirmation_now() >= self.expires_at {
            return Err("execution proof expired before the side effect".into());
        }
        let config = Arc::clone(&self.authorization.config).read_owned().await;
        if confirmation_now() >= self.expires_at {
            return Err("execution proof expired before the side effect".into());
        }
        if config.policy_revision != self.policy_revision {
            return Err("execution policy changed before the side effect".into());
        }
        if config.execution_workspace.generation() != self.execution_workspace_generation {
            return Err("execution workspace changed before the side effect".into());
        }
        Ok(ExecutionPolicyPermit { _config: config })
    }

    /// Wait until a global policy mutation or this tool's settings revoke the
    /// running process. Grants do not revoke already-running work.
    pub(crate) async fn wait_for_policy_revocation(&self) {
        let mut changes = self.authorization.process_revocations.subscribe();
        loop {
            if self.tool_settings_cancellation.is_cancelled() {
                return;
            }
            if self.session_trust_cancellation.is_cancelled() {
                return;
            }
            let current = changes.borrow_and_update().clone();
            if !Arc::ptr_eq(&current, &self.process_revocation_signal) {
                return;
            }
            tokio::select! {
                _ = self.tool_settings_cancellation.cancelled() => return,
                _ = self.session_trust_cancellation.cancelled() => return,
                changed = changes.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self::for_test_with_engine().0
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_engine() -> (Self, Arc<AuthorizationEngine>) {
        let authorization = Arc::new(AuthorizationEngine::new());
        let proof = Self::for_test_with_engine_and_tool(Arc::clone(&authorization), "shell");
        (proof, authorization)
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_engine_and_sandbox_mode(
        sandbox_mode: SandboxMode,
    ) -> (Self, Arc<AuthorizationEngine>) {
        let authorization = Arc::new(AuthorizationEngine::new());
        authorization
            .config
            .try_write()
            .expect("test authorization config must be idle")
            .sandbox_mode = sandbox_mode;
        let proof = Self::for_test_with_engine_and_tool(Arc::clone(&authorization), "shell");
        (proof, authorization)
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_engine_and_tool(
        authorization: Arc<AuthorizationEngine>,
        tool_name: &str,
    ) -> Self {
        Self::for_test_with_engine_tool_and_session(authorization, tool_name, None)
    }

    #[cfg(test)]
    pub(super) fn for_test_with_engine_tool_and_session(
        authorization: Arc<AuthorizationEngine>,
        tool_name: &str,
        session_id: Option<&str>,
    ) -> Self {
        let process_revocation_signal = authorization.process_revocations.borrow().clone();
        let tool_settings_cancellation =
            authorization.register_tool_settings_cancellation(tool_name);
        let session_trust_cancellation =
            authorization.register_session_trust_cancellation(session_id);
        Self {
            authorization,
            policy_revision: 0,
            execution_workspace_generation: 1,
            execution_boundary: crate::ExecutionBoundary::HostProcess,
            expires_at: execution_proof_expiry(),
            process_revocation_signal,
            tool_settings_cancellation,
            session_trust_cancellation,
        }
    }
}

impl McpStdioProcessStartAuthorization {
    pub(super) async fn acquire(
        &self,
        execution: &haven_common::config::McpStdioExecution,
    ) -> Result<ExecutionPolicyPermit, String> {
        let config = Arc::clone(&self.authorization.config).read_owned().await;
        if config.process_boundary_revision != self.process_boundary_revision
            || !Arc::ptr_eq(
                &self.authorization.process_boundary_changes.borrow().clone(),
                &self.process_boundary_signal,
            )
        {
            return Err("MCP process boundaries changed before process start".into());
        }
        if config.execution_workspace.generation() != self.execution_workspace_generation {
            return Err("execution workspace changed before the MCP process start".into());
        }
        if config.sandbox_mode == SandboxMode::WorkspaceWrite
            && matches!(
                execution,
                haven_common::config::McpStdioExecution::HostProcess
            )
        {
            return Err("host-process MCP stdio is blocked by the workspace-write sandbox".into());
        }
        if config.network_policy != NetworkPolicy::Open {
            return Err("MCP stdio requires the network policy to be Open".into());
        }
        Ok(ExecutionPolicyPermit { _config: config })
    }
}

impl haven_mcp::McpStdioProcessStartAuthorization for McpStdioProcessStartAuthorization {
    fn acquire_process_start_permit(
        &self,
        execution: haven_common::config::McpStdioExecution,
    ) -> haven_mcp::McpStdioProcessStartPermitFuture<'_> {
        Box::pin(async move {
            let permit = self
                .acquire(&execution)
                .await
                .map_err(execution_policy_denied)?;
            Ok(Box::new(permit) as Box<dyn haven_mcp::McpStdioProcessStartPermit>)
        })
    }
}

impl haven_skills::ProcessStartAuthorization for ExecutionPolicyAuthorization {
    async fn acquire_process_start_permit(
        &self,
    ) -> anyhow::Result<Box<dyn haven_skills::ProcessStartPermit>> {
        let permit = ExecutionPolicyAuthorization::acquire(self)
            .await
            .map_err(execution_policy_denied)?;
        Ok(Box::new(permit))
    }
}
