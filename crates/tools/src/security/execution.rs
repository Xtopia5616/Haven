#[cfg(test)]
use super::engine::execution_proof_expiry;
use super::engine::{AuthorizationEngine, SafetyConfig, confirmation_now};
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
    pub(super) session_id: Option<String>,
    pub(super) execution_boundary: crate::ExecutionBoundary,
    pub(super) expires_at: u64,
    pub(super) process_revocation_signal: Arc<()>,
    pub(super) tool_settings_cancellation: Arc<CancellationToken>,
    pub(super) session_trust_cancellation: Arc<CancellationToken>,
}

/// Short-lived process capability for commands explicitly configured as
/// lifecycle hooks. Unlike a model operation, the command is user-configured;
/// this capability still requires FullAccess, open network, and a fresh live
/// policy snapshot at the OS spawn boundary.
#[derive(Clone)]
pub(crate) struct LifecycleHookProcessStartAuthorization {
    authorization: Arc<AuthorizationEngine>,
    policy_revision: u64,
    process_boundary_revision: u64,
    execution_workspace_generation: u64,
    process_revocation_signal: Arc<()>,
    process_boundary_signal: Arc<()>,
}

impl LifecycleHookProcessStartAuthorization {
    pub(super) fn new(
        authorization: Arc<AuthorizationEngine>,
        policy_revision: u64,
        process_boundary_revision: u64,
        execution_workspace_generation: u64,
        process_revocation_signal: Arc<()>,
        process_boundary_signal: Arc<()>,
    ) -> Self {
        Self {
            authorization,
            policy_revision,
            process_boundary_revision,
            execution_workspace_generation,
            process_revocation_signal,
            process_boundary_signal,
        }
    }

    pub(crate) fn execution_workspace_generation(&self) -> u64 {
        self.execution_workspace_generation
    }

    pub(crate) async fn acquire(&self) -> Result<ExecutionPolicyPermit, String> {
        let config = Arc::clone(&self.authorization.config).read_owned().await;
        if config.policy_revision != self.policy_revision
            || config.process_boundary_revision != self.process_boundary_revision
            || config.execution_workspace.generation() != self.execution_workspace_generation
        {
            return Err("execution policy changed before lifecycle hook process start".into());
        }
        if config.sandbox_mode != SandboxMode::FullAccess {
            return Err("lifecycle hook commands require the Full Access sandbox".into());
        }
        if config.network_policy != NetworkPolicy::Open {
            return Err("lifecycle hook commands require Open network policy".into());
        }
        Ok(ExecutionPolicyPermit {
            _config: config,
            session_id: None,
        })
    }

    /// Wait for any policy or process-boundary change so the caller can stop
    /// the hook process tree immediately instead of letting an old command
    /// outlive the boundary under which it started.
    pub(crate) async fn wait_for_revocation(&self) {
        let mut policy_changes = self.authorization.process_revocations.subscribe();
        let mut boundary_changes = self.authorization.process_boundary_changes.subscribe();
        loop {
            let policy_changed = !Arc::ptr_eq(
                &policy_changes.borrow_and_update(),
                &self.process_revocation_signal,
            );
            let boundary_changed = !Arc::ptr_eq(
                &boundary_changes.borrow_and_update(),
                &self.process_boundary_signal,
            );
            if policy_changed || boundary_changed {
                return;
            }
            tokio::select! {
                result = policy_changes.changed() => if result.is_err() { return; },
                result = boundary_changes.changed() => if result.is_err() { return; },
            }
        }
    }
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
    session_id: Option<String>,
}

impl ExecutionPolicyPermit {
    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self._config.sandbox_mode
    }

    pub(crate) fn network_policy(&self) -> NetworkPolicy {
        self._config.network_policy
    }

    /// Resolve the exact write roots that WorkspaceWrite grants to a
    /// sandboxed host process. Paths are canonicalized again at the spawn
    /// boundary and an unavailable root fails closed.
    pub(crate) fn workspace_write_directories(
        &self,
    ) -> Result<Vec<haven_platform::filesystem::DirectoryCapability>, String> {
        if self._config.sandbox_mode != SandboxMode::WorkspaceWrite {
            return Err("MXC process roots require the Workspace Write sandbox".into());
        }
        self._config
            .workspace_write_directories(self.session_id.as_deref())
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
        if config
            .execution_workspace_for(self.session_id.as_deref())
            .generation()
            != self.execution_workspace_generation
        {
            return Err("execution workspace changed before the side effect".into());
        }
        Ok(ExecutionPolicyPermit {
            _config: config,
            session_id: self.session_id.clone(),
        })
    }

    /// LSP servers are user-configured host executables and may read workspace
    /// files or perform their own network requests. Do not treat Job Object
    /// containment as a filesystem or network sandbox: only start them from a
    /// proof-bound call in FullAccess mode, with network policy handled by
    /// the operation's opaque-network authorization.
    pub(crate) async fn acquire_lsp_process_start_permit(
        &self,
    ) -> Result<ExecutionPolicyPermit, String> {
        self.acquire_configured_process_start_permit("configured LSP servers")
            .await
    }

    pub(crate) async fn acquire_everything_process_start_permit(
        &self,
    ) -> Result<ExecutionPolicyPermit, String> {
        self.acquire_configured_process_start_permit("Everything CLI")
            .await
    }

    async fn acquire_configured_process_start_permit(
        &self,
        process_name: &str,
    ) -> Result<ExecutionPolicyPermit, String> {
        if confirmation_now() >= self.expires_at {
            return Err(format!(
                "{process_name} execution proof expired before process start"
            ));
        }
        let config = Arc::clone(&self.authorization.config).read_owned().await;
        if confirmation_now() >= self.expires_at {
            return Err(format!(
                "{process_name} execution proof expired before process start"
            ));
        }
        if config.policy_revision != self.policy_revision {
            return Err(format!(
                "execution policy changed before {process_name} process start"
            ));
        }
        if config
            .execution_workspace_for(self.session_id.as_deref())
            .generation()
            != self.execution_workspace_generation
        {
            return Err(format!(
                "execution workspace changed before {process_name} process start"
            ));
        }
        if config.sandbox_mode != SandboxMode::FullAccess {
            return Err(format!("{process_name} requires the Full Access sandbox"));
        }
        if matches!(
            config.network_policy,
            NetworkPolicy::Deny | NetworkPolicy::Restricted
        ) {
            return Err(format!(
                "{process_name} requires Open or user-approved network access"
            ));
        }
        Ok(ExecutionPolicyPermit {
            _config: config,
            session_id: self.session_id.clone(),
        })
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
    pub(crate) fn for_test_with_workspace_write_root(
        root: &std::path::Path,
        network_policy: NetworkPolicy,
    ) -> (Self, Arc<AuthorizationEngine>) {
        let authorization = Arc::new(AuthorizationEngine::new());
        {
            let mut config = authorization
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config
                .configure_test_workspace_write(root, network_policy)
                .expect("test workspace root must be available");
        }
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
            session_id: session_id.map(ToOwned::to_owned),
            expires_at: execution_proof_expiry(),
            process_revocation_signal,
            tool_settings_cancellation,
            session_trust_cancellation,
        }
    }
}

impl McpStdioProcessStartAuthorization {
    pub(super) async fn acquire(&self) -> Result<ExecutionPolicyPermit, String> {
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
        if config.sandbox_mode == SandboxMode::WorkspaceWrite {
            return Err(
                "MCP stdio child processes are blocked by the workspace-write sandbox".into(),
            );
        }
        if config.network_policy != NetworkPolicy::Open {
            return Err("MCP stdio requires the network policy to be Open".into());
        }
        Ok(ExecutionPolicyPermit {
            _config: config,
            session_id: None,
        })
    }
}

impl haven_mcp::McpStdioProcessStartAuthorization for McpStdioProcessStartAuthorization {
    fn acquire_process_start_permit(&self) -> haven_mcp::McpStdioProcessStartPermitFuture<'_> {
        Box::pin(async move {
            let permit = self.acquire().await.map_err(execution_policy_denied)?;
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

#[cfg(test)]
mod tests {
    use super::ExecutionPolicyAuthorization;
    use crate::security::AuthorizationEngine;
    use haven_common::types::{NetworkPolicy, SandboxMode};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn lifecycle_hook_start_requires_full_access_open_network_and_live_policy() {
        let engine = Arc::new(AuthorizationEngine::new());
        assert!(
            engine
                .capture_lifecycle_hook_process_start_authorization()
                .await
                .err()
                .expect("default workspace-write boundary is denied")
                .contains("Full Access")
        );

        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Ask)
            .await;
        assert!(
            engine
                .capture_lifecycle_hook_process_start_authorization()
                .await
                .err()
                .expect("Ask network boundary is denied")
                .contains("Open network policy")
        );

        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization()
            .await
            .unwrap();
        assert!(authorization.acquire().await.is_ok());

        engine
            .set_boundaries(
                SandboxMode::FullAccess,
                vec![PathBuf::from("C:/workspace")],
                NetworkPolicy::Open,
            )
            .await;
        assert!(
            authorization
                .acquire()
                .await
                .err()
                .expect("stale authorization is denied")
                .contains("policy changed")
        );
    }

    #[tokio::test]
    async fn lsp_start_permit_requires_full_access_and_permitted_network_policy() {
        let (authorization, _engine) =
            ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                SandboxMode::WorkspaceWrite,
            );
        assert!(
            authorization
                .acquire_lsp_process_start_permit()
                .await
                .err()
                .expect("workspace-write must be denied")
                .contains("Full Access")
        );

        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                SandboxMode::FullAccess,
            );
        for network_policy in [NetworkPolicy::Ask, NetworkPolicy::Open] {
            engine
                .config
                .try_write()
                .expect("test authorization config must be idle")
                .network_policy = network_policy;
            assert!(
                authorization
                    .acquire_lsp_process_start_permit()
                    .await
                    .is_ok()
            );
        }

        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                SandboxMode::FullAccess,
            );
        for network_policy in [NetworkPolicy::Restricted, NetworkPolicy::Deny] {
            engine
                .config
                .try_write()
                .expect("test authorization config must be idle")
                .network_policy = network_policy;
            assert!(
                authorization
                    .acquire_lsp_process_start_permit()
                    .await
                    .err()
                    .expect("restricted network must be denied")
                    .contains("network access")
            );
        }
    }

    #[tokio::test]
    async fn lsp_start_permit_rejects_a_stale_execution_proof() {
        let (mut authorization, _engine) =
            ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                SandboxMode::FullAccess,
            );
        authorization.policy_revision += 1;
        assert!(
            authorization
                .acquire_lsp_process_start_permit()
                .await
                .err()
                .expect("stale proof must be denied")
                .contains("policy changed")
        );
    }
}
