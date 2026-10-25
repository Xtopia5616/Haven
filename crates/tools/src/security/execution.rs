#[cfg(test)]
use super::engine::execution_proof_expiry;
use super::engine::{AuthorizationEngine, SafetyConfig, confirmation_now};
use crate::{
    StructuredToolError, ToolErrorClass, ToolErrorMetadata, ToolExecutionOutcome, ToolRetryability,
};
use haven_common::config::MxcIsolationMode;
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
    pub(super) session_execution_mode_revision: u64,
    pub(super) execution_workspace_generation: u64,
    pub(super) session_id: Option<String>,
    pub(super) execution_boundary: crate::ExecutionBoundary,
    pub(super) resource_boundary_extension: bool,
    pub(super) authorized_resources: Vec<crate::security::DeclaredResource>,
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
            resource_boundary_extension: false,
            authorized_resources: Vec::new(),
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
    resource_boundary_extension: bool,
    authorized_resources: Vec<crate::security::DeclaredResource>,
}

#[derive(Debug)]
pub(crate) struct WorkspaceShellFilesystemPolicy {
    pub(crate) readwrite_roots: Vec<haven_platform::filesystem::DirectoryCapability>,
    pub(crate) readonly_roots: Vec<haven_platform::filesystem::DirectoryCapability>,
    pub(crate) protected_readonly_paths: Vec<std::path::PathBuf>,
    pub(crate) denied_paths: Vec<std::path::PathBuf>,
}

impl ExecutionPolicyPermit {
    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self._config.sandbox_mode
    }

    pub(crate) fn network_policy(&self) -> NetworkPolicy {
        self._config.network_policy
    }

    pub(crate) fn mxc_isolation_mode(&self) -> MxcIsolationMode {
        self._config.mxc_isolation_mode
    }

    /// Compile the final read and write roots for this Workspace Write Shell
    /// invocation from the live global policy, Shell policy, and any
    /// proof-bound resource extension. Roots deliberately name directories,
    /// since MXC ProcessContainer enforces directory ACLs.
    pub(crate) fn workspace_shell_directories(
        &self,
        working_directory: &std::path::Path,
    ) -> Result<WorkspaceShellFilesystemPolicy, String> {
        if self._config.sandbox_mode != SandboxMode::WorkspaceWrite {
            return Err("MXC process roots require the Workspace Write sandbox".into());
        }
        let workspace_writable = self
            ._config
            .workspace_write_directories(self.session_id.as_deref())?;

        let mut path_constraints = Vec::new();
        if !self._config.global_file_roots.is_empty() {
            path_constraints.push(super::filesystem::open_directory_roots(
                &self._config.global_file_roots,
                "security.resource_policy.files.path_roots",
            )?);
        }
        let tool_settings = self
            ._config
            .tool_settings
            .get("shell")
            .into_iter()
            .collect::<Vec<_>>();
        let mut protected_readonly_paths = self._config.global_read_only_roots.clone();
        let mut denied_paths = self._config.global_denied_paths.clone();
        for setting in &tool_settings {
            if !setting.resource_policy.files.path_roots.is_empty() {
                let roots = setting
                    .resource_policy
                    .files
                    .path_roots
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect::<Vec<_>>();
                path_constraints.push(super::filesystem::open_directory_roots(
                    &roots,
                    "tool resource_policy.files.path_roots",
                )?);
            }
            protected_readonly_paths.extend(
                setting
                    .resource_policy
                    .files
                    .read_only_roots
                    .iter()
                    .map(std::path::PathBuf::from),
            );
            denied_paths.extend(
                setting
                    .resource_policy
                    .files
                    .denied_paths
                    .iter()
                    .map(std::path::PathBuf::from),
            );
        }
        let protected_readonly = super::filesystem::open_filesystem_paths(
            &protected_readonly_paths,
            "resource_policy.files.read_only_roots",
        )?;
        let denied = super::filesystem::open_filesystem_paths(
            &denied_paths,
            "resource_policy.files.denied_paths",
        )?;

        let tool_write_constraint: Option<
            Result<Vec<haven_platform::filesystem::DirectoryCapability>, String>,
        > = tool_settings.iter().find_map(|setting| {
            match setting.resource_policy.files.write_mode {
                haven_common::config::FileWriteMode::ReadOnly => Some(Ok(Vec::new())),
                haven_common::config::FileWriteMode::Workspace => {
                    let workspace = self
                        ._config
                        .execution_workspace_for(self.session_id.as_deref());
                    let Some(root) = workspace.root() else {
                        return Some(Err(
                            "tool workspace resource policy has no execution root".to_owned()
                        ));
                    };
                    Some(super::filesystem::open_directory_roots(
                        &[root.to_path_buf()],
                        "tool resource_policy.files.workspace",
                    ))
                }
                haven_common::config::FileWriteMode::Roots => {
                    let roots = setting
                        .resource_policy
                        .files
                        .write_roots
                        .iter()
                        .map(std::path::PathBuf::from)
                        .collect::<Vec<_>>();
                    Some(super::filesystem::open_directory_roots(
                        &roots,
                        "tool resource_policy.files.write_roots",
                    ))
                }
                haven_common::config::FileWriteMode::Inherit
                | haven_common::config::FileWriteMode::Unrestricted => None,
            }
        });

        let mut read_scope = workspace_writable.clone();
        let mut write_scope = workspace_writable;
        let tool_write_constraint = tool_write_constraint.transpose()?;
        if let Some(tool_write_constraint) = &tool_write_constraint {
            write_scope = intersect_directory_roots(&write_scope, tool_write_constraint);
        }
        for constraint in &path_constraints {
            read_scope = intersect_directory_roots(&read_scope, constraint);
            write_scope = intersect_directory_roots(&write_scope, constraint);
        }

        let tool_is_read_only = tool_settings.iter().any(|setting| {
            setting.resource_policy.files.write_mode
                == haven_common::config::FileWriteMode::ReadOnly
        });
        let mut readonly = read_scope.clone();
        let mut extension_writable = Vec::new();
        let mut extension_readonly = Vec::new();
        if self.resource_boundary_extension {
            if self.authorized_resources.is_empty() {
                return Err("Shell resource extension has no bound local paths".into());
            }
            for resource in &self.authorized_resources {
                let crate::security::DeclaredResource::LocalPath { path, access } = resource else {
                    return Err(
                        "Shell resource extension contains an unsupported resource type".into(),
                    );
                };
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    self._config
                        .execution_workspace_for(self.session_id.as_deref())
                        .resolve_relative(path)
                        .ok_or_else(|| {
                            "relative Shell resource extension has no execution workspace"
                                .to_owned()
                        })?
                };
                let directory = haven_platform::filesystem::DirectoryCapability::open(&path)
                    .map_err(|error| {
                        format!(
                            "Shell resource extension directory is unavailable or unsafe: {}",
                            haven_common::error::sanitize_error_text(&error.to_string())
                        )
                    })?;
                match access {
                    haven_common::types::LocalPathAccess::Read => {
                        extension_readonly.push(directory)
                    }
                    haven_common::types::LocalPathAccess::Write if !tool_is_read_only => {
                        extension_writable.push(directory)
                    }
                    haven_common::types::LocalPathAccess::Write => {
                        return Err("tool resource policy is read-only".into());
                    }
                }
            }
            for constraint in &path_constraints {
                extension_writable = intersect_directory_roots(&extension_writable, constraint);
                extension_readonly = intersect_directory_roots(&extension_readonly, constraint);
            }
            if let Some(tool_write_constraint) = &tool_write_constraint {
                extension_writable =
                    intersect_directory_roots(&extension_writable, tool_write_constraint);
            }
            write_scope.extend(extension_writable);
            readonly.extend(extension_readonly);
        }

        // Denied ancestors remove their entire contained grant; denied
        // descendants remain explicit so MXC can apply native deny precedence.
        read_scope.retain(|scope| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(scope.root()).unwrap_or(true))
        });
        write_scope.retain(|scope| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(scope.root()).unwrap_or(true))
        });
        readonly.retain(|scope| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(scope.root()).unwrap_or(true))
        });

        // A protected read-only ancestor turns the entire contained write
        // root into read-only. Removing that write grant here keeps the owner
        // policy authoritative even when `path_roots` narrowed the read scope
        // below the configured protected root.
        write_scope.retain(|writable| {
            !protected_readonly
                .iter()
                .any(|protected| protected.contains_path(writable.root()).unwrap_or(true))
        });

        // A read grant contained by a write grant adds no read access. Keep
        // only broader read-only roots; explicit protected subpaths are
        // carried separately so they remain read-only below a write root.
        readonly.retain(|readable| {
            !write_scope
                .iter()
                .any(|writable| directory_root_contains(writable, readable))
        });

        let metadata_roots = haven_platform::filesystem::protected_metadata_paths(
            &write_scope
                .iter()
                .map(|directory| directory.root().to_path_buf())
                .collect::<Vec<_>>(),
        );
        let metadata =
            super::filesystem::open_filesystem_paths(&metadata_roots, "protected VCS metadata")?;

        let protected_readonly_paths = protected_readonly
            .iter()
            .chain(&metadata)
            .filter(|protected| {
                !denied
                    .iter()
                    .any(|deny| deny.contains_path(protected.path()).unwrap_or(true))
                    && read_scope
                        .iter()
                        .chain(&write_scope)
                        .any(|scope| directory_root_contains_path(scope, protected.path()))
            })
            .map(|protected| protected.path().to_path_buf())
            .collect::<Vec<_>>();
        let denied_paths = denied
            .iter()
            .filter(|deny| {
                readonly.iter().chain(&write_scope).any(|scope| {
                    directory_root_contains_path(scope, deny.path())
                        || deny.contains_path(scope.root()).unwrap_or(true)
                })
            })
            .map(|deny| deny.path().to_path_buf())
            .collect::<Vec<_>>();

        let working_directory = haven_platform::filesystem::DirectoryCapability::open(
            working_directory,
        )
        .map_err(|error| {
            format!(
                "Shell working directory is unavailable or unsafe: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            )
        })?;
        if !roots_contain_directory(&write_scope, &working_directory)
            && !roots_contain_directory(&readonly, &working_directory)
        {
            return Err(
                "Shell working directory is outside the compiled MXC filesystem policy".into(),
            );
        }
        if denied
            .iter()
            .any(|deny| deny.contains_path(working_directory.root()).unwrap_or(true))
        {
            return Err(
                "Shell working directory is explicitly denied by the compiled MXC policy".into(),
            );
        }

        Ok(WorkspaceShellFilesystemPolicy {
            readwrite_roots: normalize_directory_roots(write_scope),
            readonly_roots: normalize_directory_roots(readonly),
            protected_readonly_paths,
            denied_paths,
        })
    }
}

fn intersect_directory_roots(
    left: &[haven_platform::filesystem::DirectoryCapability],
    right: &[haven_platform::filesystem::DirectoryCapability],
) -> Vec<haven_platform::filesystem::DirectoryCapability> {
    let mut intersection = Vec::new();
    for left_root in left {
        for right_root in right {
            if directory_root_contains(right_root, left_root) {
                intersection.push(left_root.clone());
            } else if directory_root_contains(left_root, right_root) {
                intersection.push(right_root.clone());
            }
        }
    }
    normalize_directory_roots(intersection)
}

fn normalize_directory_roots(
    roots: Vec<haven_platform::filesystem::DirectoryCapability>,
) -> Vec<haven_platform::filesystem::DirectoryCapability> {
    let mut normalized = Vec::new();
    for candidate in roots {
        if normalized.iter().any(
            |existing: &haven_platform::filesystem::DirectoryCapability| {
                directory_root_contains(existing, &candidate)
            },
        ) {
            continue;
        }
        normalized.retain(
            |existing: &haven_platform::filesystem::DirectoryCapability| {
                !directory_root_contains(&candidate, existing)
            },
        );
        normalized.push(candidate);
    }
    normalized
}

fn roots_contain_directory(
    roots: &[haven_platform::filesystem::DirectoryCapability],
    directory: &haven_platform::filesystem::DirectoryCapability,
) -> bool {
    roots
        .iter()
        .any(|root| directory_root_contains(root, directory))
}

fn directory_root_contains(
    root: &haven_platform::filesystem::DirectoryCapability,
    candidate: &haven_platform::filesystem::DirectoryCapability,
) -> bool {
    directory_root_contains_path(root, candidate.root())
}

fn directory_root_contains_path(
    root: &haven_platform::filesystem::DirectoryCapability,
    candidate: &std::path::Path,
) -> bool {
    haven_platform::filesystem::FilesystemPathCapability::open(root.root())
        .is_ok_and(|scope| scope.contains_path(candidate).unwrap_or(true))
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
        if config.execution_mode_revision_for(self.session_id.as_deref())
            != self.session_execution_mode_revision
        {
            return Err("session execution mode changed before the side effect".into());
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
            resource_boundary_extension: self.resource_boundary_extension,
            authorized_resources: self.authorized_resources.clone(),
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
        if config.execution_mode_revision_for(self.session_id.as_deref())
            != self.session_execution_mode_revision
        {
            return Err(format!(
                "session execution mode changed before {process_name} process start"
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
            resource_boundary_extension: false,
            authorized_resources: Vec::new(),
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
        Self::for_test_with_workspace_write_root_and_mxc_mode(
            root,
            network_policy,
            MxcIsolationMode::Strict,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_test_with_workspace_write_root_and_mxc_mode(
        root: &std::path::Path,
        network_policy: NetworkPolicy,
        mxc_isolation_mode: MxcIsolationMode,
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
            config.mxc_isolation_mode = mxc_isolation_mode;
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
        let (policy_revision, session_execution_mode_revision, execution_workspace_generation) = {
            let config = authorization
                .config
                .try_read()
                .expect("test authorization config must be idle");
            (
                config.policy_revision,
                config.execution_mode_revision_for(session_id),
                config.execution_workspace_for(session_id).generation(),
            )
        };
        Self {
            authorization,
            policy_revision,
            session_execution_mode_revision,
            execution_workspace_generation,
            execution_boundary: crate::ExecutionBoundary::HostProcess,
            session_id: session_id.map(ToOwned::to_owned),
            resource_boundary_extension: false,
            authorized_resources: Vec::new(),
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
            resource_boundary_extension: false,
            authorized_resources: Vec::new(),
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
    use haven_common::config::MxcIsolationMode;
    use haven_common::types::{NetworkPolicy, SandboxMode};
    use std::path::PathBuf;
    use std::sync::Arc;

    #[tokio::test]
    async fn execution_policy_permit_carries_mxc_isolation_choice() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .config
            .try_write()
            .expect("test authorization config must be idle")
            .mxc_isolation_mode = MxcIsolationMode::Compatibility;
        let authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "shell",
        );

        let permit = authorization
            .acquire()
            .await
            .expect("live execution policy is available");
        assert_eq!(permit.mxc_isolation_mode(), MxcIsolationMode::Compatibility);
    }

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
            .set_boundaries(
                SandboxMode::FullAccess,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
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
        for network_policy in [NetworkPolicy::Open] {
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
