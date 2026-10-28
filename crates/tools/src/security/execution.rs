#[cfg(test)]
use super::engine::execution_proof_expiry;
use super::engine::{AuthorizationEngine, SafetyConfig, confirmation_now};
use crate::{
    StructuredToolError, ToolErrorClass, ToolErrorMetadata, ToolExecutionOutcome, ToolRetryability,
};
use haven_common::config::{MxcIsolationMode, NetworkAccessBoundary};
use haven_common::types::{NetworkPolicy, SandboxMode};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

mod process_policy;

#[cfg(windows)]
const MAX_MXC_WRITE_ROOT_INSPECTED_ENTRIES: usize = 1_000_000;

/// Short-lived side-effect capability derived from one proof-verified tool
/// attempt. Acquiring it holds a read lock on the live policy through the
/// protected operation, so a policy/workspace update either wins first and
/// rejects the stale proof or takes effect after the operation completes.
#[derive(Clone)]
pub(crate) struct ExecutionPolicyAuthorization {
    pub(super) authorization: Arc<AuthorizationEngine>,
    pub(super) tool_name: String,
    pub(super) policy_revision: u64,
    pub(super) session_execution_mode_revision: u64,
    pub(super) execution_workspace_generation: u64,
    pub(super) session_id: Option<String>,
    pub(super) execution_boundary: crate::ExecutionBoundary,
    pub(super) resource_boundary_extension: bool,
    pub(super) network_authorized: bool,
    pub(super) authorized_resources: Vec<crate::security::DeclaredResource>,
    pub(super) expires_at: u64,
    pub(super) process_revocation_signal: Arc<()>,
    pub(super) tool_settings_cancellation: Arc<CancellationToken>,
    pub(super) session_trust_cancellation: Arc<CancellationToken>,
}

/// Short-lived process capability for commands explicitly configured as
/// lifecycle hooks. Unlike a model operation, the command is user-configured;
/// the process still requires a fresh live policy snapshot at the OS spawn
/// boundary and uses MXC under WorkspaceWrite.
#[derive(Clone)]
pub(crate) struct LifecycleHookProcessStartAuthorization {
    authorization: Arc<AuthorizationEngine>,
    session_id: Option<String>,
    policy_revision: u64,
    process_boundary_revision: u64,
    execution_workspace_generation: u64,
    process_revocation_signal: Arc<()>,
    process_boundary_signal: Arc<()>,
    session_trust_cancellation: Arc<CancellationToken>,
}

pub(crate) struct LifecycleHookProcessStartRequest {
    pub(crate) command: String,
    pub(crate) arguments: Vec<String>,
    pub(crate) working_directory: std::path::PathBuf,
    pub(crate) runtime_read_roots: Vec<std::path::PathBuf>,
}

pub(super) struct LifecycleHookSessionContext {
    pub(super) session_id: Option<String>,
    pub(super) trust_cancellation: Arc<CancellationToken>,
}

pub(crate) struct EverythingProcessStartRequest {
    pub(crate) search_root: std::path::PathBuf,
    pub(crate) scratch_directory: std::path::PathBuf,
    pub(crate) runtime_read_roots: Vec<std::path::PathBuf>,
}

pub(crate) struct McpStdioProcessStartPermit {
    _permit: ExecutionPolicyPermit,
    policy: Option<haven_platform::sandboxed_process::SandboxedProcessPolicy>,
    working_directory: Option<std::path::PathBuf>,
    scratch: Option<crate::shell_runtime::ProcessScratchDirectory>,
}

impl haven_mcp::McpStdioProcessStartPermit for McpStdioProcessStartPermit {
    fn scratch_capacity_budget(&self) -> Option<haven_platform::filesystem::DirectoryBudgetLease> {
        self.scratch
            .as_ref()
            .map(crate::shell_runtime::ProcessScratchDirectory::capacity_budget)
    }
    fn sandboxed_process_policy(
        &self,
    ) -> Option<haven_platform::sandboxed_process::SandboxedProcessPolicy> {
        self.policy.clone()
    }

    fn sandboxed_working_directory(&self) -> Option<&std::path::Path> {
        self.working_directory.as_deref()
    }

    fn sandboxed_temp_directory(&self) -> Option<&std::path::Path> {
        self.scratch
            .as_ref()
            .map(crate::shell_runtime::ProcessScratchDirectory::path)
    }

    fn take_lifetime_resource(&mut self) -> Option<Box<dyn Send>> {
        self.scratch
            .take()
            .map(|scratch| Box::new(scratch) as Box<dyn Send>)
    }
}

impl LifecycleHookProcessStartAuthorization {
    pub(super) fn new(
        authorization: Arc<AuthorizationEngine>,
        session_context: LifecycleHookSessionContext,
        policy_revision: u64,
        process_boundary_revision: u64,
        execution_workspace_generation: u64,
        process_revocation_signal: Arc<()>,
        process_boundary_signal: Arc<()>,
    ) -> Self {
        Self {
            authorization,
            session_id: session_context.session_id,
            policy_revision,
            process_boundary_revision,
            execution_workspace_generation,
            process_revocation_signal,
            process_boundary_signal,
            session_trust_cancellation: session_context.trust_cancellation,
        }
    }

    pub(crate) fn execution_workspace_generation(&self) -> u64 {
        self.execution_workspace_generation
    }

    pub(crate) async fn acquire(
        &self,
        request: LifecycleHookProcessStartRequest,
    ) -> Result<ExecutionPolicyPermit, String> {
        {
            let config = self.authorization.config.read().await;
            if config.policy_revision != self.policy_revision
                || config.process_boundary_revision != self.process_boundary_revision
                || config
                    .execution_workspace_for(self.session_id.as_deref())
                    .generation()
                    != self.execution_workspace_generation
            {
                return Err("execution policy changed before lifecycle hook process start".into());
            }
        }
        let permission_request = AuthorizationEngine::runtime_permission_request(
            self.session_id.as_deref(),
            "lifecycle_hook",
            serde_json::json!({"command":request.command, "args":request.arguments, "cwd":request.working_directory}),
            crate::NetworkAccess::Opaque,
            vec![crate::DeclaredResource::LocalPath {
                path: request.working_directory.clone(),
                access: haven_common::types::LocalPathAccess::Write,
            }],
        )?;
        let proof = self
            .authorization
            .request_execution_permission(
                &permission_request,
                self.session_trust_cancellation.as_ref().clone(),
            )
            .await?;
        let context = self
            .authorization
            .execution_context_for_proof(&permission_request, &proof)
            .await?;
        let granted = context
            .clone_execution_authorization()
            .ok_or_else(|| "hook permission has no execution authority".to_owned())?;
        let config = Arc::clone(&self.authorization.config).read_owned().await;
        if config.policy_revision != self.policy_revision
            || config.process_boundary_revision != self.process_boundary_revision
            || config
                .execution_workspace_for(self.session_id.as_deref())
                .generation()
                != self.execution_workspace_generation
        {
            return Err("execution policy changed before lifecycle hook process start".into());
        }
        drop(config);
        let mut permit = granted.acquire().await?;
        match permit.sandbox_mode() {
            SandboxMode::FullAccess if permit.opaque_network_is_unrestricted() => Ok(permit),
            SandboxMode::FullAccess => {
                Err("lifecycle hook commands require Open network policy after process-network authorization in Full Access".into())
            }
            SandboxMode::WorkspaceWrite if cfg!(windows) => {
                let filesystem = permit.workspace_process_directories_for_tool(
                    "lifecycle_hook",
                    &request.working_directory,
                    &request.runtime_read_roots,
                )?;
                permit.sandboxed_process_policy =
                    Some(haven_platform::sandboxed_process::SandboxedProcessPolicy {
                        isolation_mode: permit._config.mxc_isolation_mode,
                        readwrite_paths: normalize_directory_roots(filesystem.readwrite_roots)
                            .iter()
                            .map(|root| root.root().to_path_buf())
                            .collect(),
                        readonly_paths: normalize_directory_roots(filesystem.readonly_roots)
                            .iter()
                            .map(|root| root.root().to_path_buf())
                            .collect(),
                        protected_readonly_paths: filesystem.protected_readonly_paths,
                        denied_paths: filesystem.denied_paths,
                        network_policy: permit.process_network_policy(),
                    });
                Ok(permit)
            }
            SandboxMode::WorkspaceWrite => {
                Err("Workspace Write lifecycle hooks require Windows MXC isolation".into())
            }
            SandboxMode::ReadOnly => {
                Err("lifecycle hook commands are blocked by the read-only sandbox".into())
            }
        }
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
            if self.session_trust_cancellation.is_cancelled() {
                return;
            }
            tokio::select! {
                _ = self.session_trust_cancellation.cancelled() => return,
                result = policy_changes.changed() => if result.is_err() { return; },
                result = boundary_changes.changed() => if result.is_err() { return; },
            }
        }
    }
}

/// Current process-boundary snapshot for a manager-owned MCP stdio client
/// generation. Unlike a tool proof it is not permission to call a tool; it
/// only permits a child process to start under the active FullAccess or
/// WorkspaceWrite process policy.
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
    tool_name: String,
    resource_boundary_extension: bool,
    network_authorized: bool,
    authorized_resources: Vec<crate::security::DeclaredResource>,
    sandboxed_process_policy: Option<haven_platform::sandboxed_process::SandboxedProcessPolicy>,
}

pub(super) fn opaque_network_is_unrestricted(config: &SafetyConfig, tool_name: &str) -> bool {
    if config.network.network_policy() != NetworkPolicy::Open {
        return false;
    }
    let settings = if tool_name == "mcp_stdio" {
        [Some("mcp_stdio"), Some("mcp")]
    } else {
        [Some(tool_name), None]
    };
    settings.into_iter().flatten().all(|name| {
        config.tool_settings.get(name).is_none_or(|setting| {
            !matches!(
                setting.resource_policy.network.access,
                NetworkAccessBoundary::Deny | NetworkAccessBoundary::Restricted
            )
        })
    })
}

#[derive(Debug)]
pub(crate) struct WorkspaceProcessFilesystemPolicy {
    pub(crate) readwrite_roots: Vec<haven_platform::filesystem::DirectoryCapability>,
    pub(crate) readonly_roots: Vec<haven_platform::filesystem::DirectoryCapability>,
    pub(crate) protected_readonly_paths: Vec<std::path::PathBuf>,
    pub(crate) denied_paths: Vec<std::path::PathBuf>,
}

impl ExecutionPolicyPermit {
    pub(crate) fn authorizes_git_desktop_handles(&self) -> bool {
        self.authorized_resources.iter().any(|resource| {
            matches!(
                resource,
                crate::DeclaredResource::Desktop {
                    access: haven_common::types::DesktopAccess::ProcessHandles
                }
            )
        })
    }

    /// Match the Git cwd against the write resource bound to this proof.
    pub(crate) fn authorizes_git_directory(
        &self,
        directory: &haven_platform::filesystem::DirectoryCapability,
    ) -> bool {
        let workspace = self
            ._config
            .execution_workspace_for(self.session_id.as_deref());
        self.authorized_resources.iter().any(|resource| {
            let crate::DeclaredResource::LocalPath {
                path,
                access: haven_common::types::LocalPathAccess::Write,
            } = resource
            else {
                return false;
            };
            let path = if path.is_absolute() {
                Some(path.clone())
            } else {
                workspace.resolve_relative(path)
            };
            path.and_then(|path| haven_platform::filesystem::DirectoryCapability::open(&path).ok())
                .is_some_and(|authorized| authorized.root() == directory.root())
        })
    }

    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        if self.resource_boundary_extension
            && self._config.file_policy.sandbox_mode() == SandboxMode::ReadOnly
        {
            SandboxMode::WorkspaceWrite
        } else {
            self._config.file_policy.sandbox_mode()
        }
    }

    pub(crate) fn network_policy(&self) -> NetworkPolicy {
        if self.network_authorized {
            NetworkPolicy::Open
        } else {
            self._config.network.network_policy()
        }
    }

    pub(crate) fn process_network_policy(
        &self,
    ) -> haven_platform::sandboxed_process::SandboxedProcessNetworkPolicy {
        haven_platform::sandboxed_process::SandboxedProcessNetworkPolicy {
            allow_public_egress: self.network_authorized,
            allow_private_network_client_server: self.network_authorized,
            allow_host_loopback: self.network_authorized,
        }
    }

    fn opaque_network_is_unrestricted(&self) -> bool {
        self.network_authorized || opaque_network_is_unrestricted(&self._config, &self.tool_name)
    }

    pub(crate) fn mxc_isolation_mode(&self) -> MxcIsolationMode {
        self._config.mxc_isolation_mode
    }

    pub(crate) fn sandboxed_process_policy(
        &self,
    ) -> Option<&haven_platform::sandboxed_process::SandboxedProcessPolicy> {
        self.sandboxed_process_policy.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn workspace_shell_directories(
        &self,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        self.compile_workspace_process_directories(
            "shell",
            working_directory,
            runtime_read_roots,
            None,
            &[],
            &[],
        )
    }

    pub(crate) fn workspace_shell_directories_with_runtime_writes(
        &self,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
        scratch_directory: &std::path::Path,
        cache_directories: &[std::path::PathBuf],
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        self.workspace_process_directories_with_runtime_writes_for_tool(
            "shell",
            working_directory,
            runtime_read_roots,
            scratch_directory,
            cache_directories,
        )
    }

    pub(crate) fn workspace_process_directories_with_runtime_writes_for_tool(
        &self,
        tool_name: &str,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
        scratch_directory: &std::path::Path,
        cache_directories: &[std::path::PathBuf],
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        self.compile_workspace_process_directories(
            tool_name,
            working_directory,
            runtime_read_roots,
            Some(scratch_directory),
            cache_directories,
            &[],
        )
    }

    pub(crate) fn workspace_process_directories_for_tool(
        &self,
        tool_name: &str,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        self.compile_workspace_process_directories(
            tool_name,
            working_directory,
            runtime_read_roots,
            None,
            &[],
            &[],
        )
    }

    pub(crate) fn workspace_git_directories(
        &self,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
        scratch_directory: &std::path::Path,
        git_metadata_directories: &[std::path::PathBuf],
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        self.compile_workspace_process_directories(
            "git",
            working_directory,
            runtime_read_roots,
            Some(scratch_directory),
            &[],
            git_metadata_directories,
        )
    }

    /// Compile the final read and write roots for a Workspace Write process
    /// invocation from the live global policy, tool policy, and any
    /// proof-bound resource extension. Roots deliberately name directories,
    /// since MXC ProcessContainer enforces directory ACLs.
    fn compile_workspace_process_directories(
        &self,
        tool_name: &str,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
        scratch_directory: Option<&std::path::Path>,
        cache_directories: &[std::path::PathBuf],
        git_metadata_write: &[std::path::PathBuf],
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        if self.sandbox_mode() != SandboxMode::WorkspaceWrite {
            return Err("MXC process roots require the Workspace Write sandbox".into());
        }
        let workspace_writable = self
            ._config
            .workspace_write_directories(self.session_id.as_deref())?;

        let mut path_constraints = Vec::new();
        if !self._config.file_policy.global_file_roots.is_empty() {
            path_constraints.push(super::filesystem::open_directory_roots(
                &self._config.file_policy.global_file_roots,
                "security.resource_policy.files.path_roots",
            )?);
        }
        let tool_settings = self
            ._config
            .tool_settings
            .get(tool_name)
            .into_iter()
            .collect::<Vec<_>>();
        let mut protected_readonly_paths = self._config.file_policy.global_read_only_roots.clone();
        let mut denied_paths = self._config.file_policy.global_denied_paths.clone();
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
        let git_metadata_identity = super::filesystem::open_filesystem_paths(
            git_metadata_write,
            "Git repository metadata",
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
        let mut write_scope = if self._config.file_policy.sandbox_mode() == SandboxMode::ReadOnly {
            Vec::new()
        } else {
            workspace_writable
        };
        let tool_write_constraint = tool_write_constraint.transpose()?;
        if let Some(tool_write_constraint) = &tool_write_constraint {
            write_scope = intersect_directory_roots(&write_scope, tool_write_constraint);
        }
        for constraint in &path_constraints {
            read_scope = intersect_directory_roots(&read_scope, constraint);
            write_scope = intersect_directory_roots(&write_scope, constraint);
        }

        // Runtime dependencies (Windows, command search entries, and user
        // configured SDK/interpreter roots) are read grants, not a bypass of
        // the resource policy. Compile them through every global/tool path
        // constraint before they are included in the MXC request.
        let mut runtime_roots = runtime_read_roots.to_vec();
        runtime_roots.extend(self._config.shell_runtime_read_roots.iter().cloned());
        let mut runtime_read = super::filesystem::open_directory_roots(
            &runtime_roots,
            "security.shell_runtime_read_roots",
        )?;
        runtime_read.retain(|scope| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(scope.root()).unwrap_or(true))
        });

        let tool_is_read_only = tool_settings.iter().any(|setting| {
            setting.resource_policy.files.write_mode
                == haven_common::config::FileWriteMode::ReadOnly
        });
        let mut readonly = read_scope.clone();
        readonly.extend(runtime_read.iter().cloned());
        let mut extension_writable = Vec::new();
        let mut extension_readonly = Vec::new();
        if self.resource_boundary_extension {
            for resource in &self.authorized_resources {
                let crate::security::DeclaredResource::LocalPath { path, access } = resource else {
                    continue;
                };
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    self._config
                        .execution_workspace_for(self.session_id.as_deref())
                        .resolve_relative(path)
                        .ok_or_else(|| {
                            "relative process resource extension has no execution workspace"
                                .to_owned()
                        })?
                };
                let directory = haven_platform::filesystem::DirectoryCapability::open(&path)
                    .map_err(|error| {
                        format!(
                            "process resource extension directory is unavailable or unsafe: {}",
                            haven_common::error::sanitize_error_text(&error.to_string())
                        )
                    })?;
                match access {
                    haven_common::types::LocalPathAccess::Read => {
                        extension_readonly.push(directory)
                    }
                    haven_common::types::LocalPathAccess::Write => {
                        extension_writable.push(directory)
                    }
                }
            }
            if !extension_writable.is_empty() {
                write_scope = extension_writable;
            }
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
            self.resource_boundary_extension && self.authorized_resources.iter().any(|resource| matches!(resource,
                crate::DeclaredResource::LocalPath { path, access: haven_common::types::LocalPathAccess::Write }
                if directory_root_contains_path(writable, path))) || (!protected_readonly
                .iter()
                .any(|protected| protected.contains_path(writable.root()).unwrap_or(true))
                && !runtime_read
                    .iter()
                    .any(|protected| directory_root_contains_path(protected, writable.root())))
        });

        for git_metadata in &git_metadata_identity {
            let metadata_directory =
                haven_platform::filesystem::DirectoryCapability::open(git_metadata.path())
                    .map_err(|_| "Git repository metadata is unavailable or unsafe".to_owned())?;
            if !roots_contain_directory(&write_scope, &metadata_directory) {
                return Err("Git repository metadata is outside the configured write scope".into());
            }
            if denied.iter().any(|deny| {
                deny.contains_path(metadata_directory.root())
                    .unwrap_or(true)
                    || git_metadata.contains_path(deny.path()).unwrap_or(true)
            }) {
                return Err("Git repository metadata overlaps a denied path".into());
            }
            if !(self.resource_boundary_extension
                && self.authorizes_git_directory(&metadata_directory))
                && protected_readonly.iter().any(|protected| {
                    protected
                        .contains_path(metadata_directory.root())
                        .unwrap_or(true)
                        || git_metadata.contains_path(protected.path()).unwrap_or(true)
                })
            {
                return Err("Git repository metadata is explicitly protected read-only".into());
            }
        }

        // Shell-owned runtime files are independent grants. They are not
        // selected from or widened to the first user writable root, and
        // explicit deny/protected-read-only policy still takes precedence.
        if let Some(scratch_directory) = scratch_directory {
            let scratch = super::filesystem::open_directory_roots(
                &[scratch_directory.to_path_buf()],
                "process per-call scratch directory",
            )?;
            let scratch = scratch
                .into_iter()
                .next()
                .ok_or_else(|| "Shell per-call scratch directory is unavailable".to_owned())?;
            if denied
                .iter()
                .any(|deny| deny.contains_path(scratch.root()).unwrap_or(true))
            {
                return Err(
                    "process scratch directory is explicitly denied by the compiled MXC policy"
                        .into(),
                );
            }
            if protected_readonly
                .iter()
                .any(|protected| protected.contains_path(scratch.root()).unwrap_or(true))
                || runtime_read
                    .iter()
                    .any(|protected| directory_root_contains_path(protected, scratch.root()))
            {
                return Err(
                    "process scratch directory is protected read-only by the compiled MXC policy"
                        .into(),
                );
            }
            write_scope.push(scratch);
        }
        if !tool_is_read_only && !cache_directories.is_empty() {
            let cache_roots = super::filesystem::open_directory_roots(
                cache_directories,
                "Shell package cache directories",
            )?;
            for cache_root in cache_roots {
                let explicitly_denied = denied
                    .iter()
                    .any(|deny| deny.contains_path(cache_root.root()).unwrap_or(true));
                let protected_read_only = protected_readonly
                    .iter()
                    .any(|protected| protected.contains_path(cache_root.root()).unwrap_or(true))
                    || runtime_read.iter().any(|protected| {
                        directory_root_contains_path(protected, cache_root.root())
                    });
                if !explicitly_denied && !protected_read_only {
                    write_scope.push(cache_root);
                }
            }
        }

        // A read grant contained by a write grant adds no read access. Keep
        // only broader read-only roots; explicit protected subpaths are
        // carried separately so they remain read-only below a write root.
        readonly.retain(|readable| {
            !write_scope
                .iter()
                .any(|writable| directory_root_contains(writable, readable))
        });

        let mut protected_readonly_paths = protected_readonly
            .iter()
            .filter(|protected| {
                if self.resource_boundary_extension
                    && write_scope.iter().any(|root| {
                        directory_root_contains_path(root, protected.path())
                            || protected.contains_path(root.root()).unwrap_or(false)
                    })
                {
                    return false;
                }
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
        protected_readonly_paths.extend(
            runtime_read
                .iter()
                .filter(|protected| {
                    if self.resource_boundary_extension
                        && write_scope
                            .iter()
                            .any(|root| directory_root_contains_path(root, protected.root()))
                    {
                        return false;
                    }
                    !denied
                        .iter()
                        .any(|deny| deny.contains_path(protected.root()).unwrap_or(true))
                        && read_scope
                            .iter()
                            .chain(&write_scope)
                            .any(|scope| directory_root_contains_path(scope, protected.root()))
                })
                .map(|protected| protected.root().to_path_buf()),
        );
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
                "process working directory is unavailable or unsafe: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            )
        })?;
        if !roots_contain_directory(&write_scope, &working_directory)
            && !roots_contain_directory(&readonly, &working_directory)
        {
            return Err(
                "process working directory is outside the compiled MXC filesystem policy".into(),
            );
        }
        if denied
            .iter()
            .any(|deny| deny.contains_path(working_directory.root()).unwrap_or(true))
        {
            return Err(
                "process working directory is explicitly denied by the compiled MXC policy".into(),
            );
        }

        let write_scope = normalize_directory_roots(write_scope);
        #[cfg(windows)]
        for writable in &write_scope {
            writable
                .ensure_no_hardlinked_files(MAX_MXC_WRITE_ROOT_INSPECTED_ENTRIES)
                .map_err(|error| {
                    tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "refusing MXC Shell because writable hard-link safety could not be verified"
                    );
                    "MXC cannot safely isolate a writable directory containing hard-linked files or exceeding the inspection limit".to_owned()
                })?;
        }

        Ok(WorkspaceProcessFilesystemPolicy {
            readwrite_roots: write_scope,
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
    pub(crate) fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
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
            tool_name: self.tool_name.clone(),
            resource_boundary_extension: self.resource_boundary_extension,
            network_authorized: self.network_authorized,
            authorized_resources: self.authorized_resources.clone(),
            sandboxed_process_policy: None,
        })
    }

    /// LSP servers are user-configured host executables and may read workspace
    /// files or perform their own network requests. WorkspaceWrite may start
    /// them only on Windows, where the LSP adapter supplies read-only project
    /// roots, app-owned scratch, and an offline MXC network policy.
    pub(crate) async fn acquire_lsp_process_start_permit(
        &self,
    ) -> Result<ExecutionPolicyPermit, String> {
        let permit = self.acquire().await?;
        match permit.sandbox_mode() {
            SandboxMode::FullAccess if permit.network_policy() == NetworkPolicy::Open => Ok(permit),
            SandboxMode::FullAccess => Err(
                "configured LSP servers require the Open network policy in Full Access mode".into(),
            ),
            SandboxMode::WorkspaceWrite if cfg!(windows) => Ok(permit),
            SandboxMode::WorkspaceWrite => Err(
                "configured LSP servers require Windows MXC isolation in Workspace Write mode"
                    .into(),
            ),
            SandboxMode::ReadOnly => {
                Err("configured LSP servers are blocked by the read-only sandbox".into())
            }
        }
    }

    /// Acquire a live Skill process grant. Environment creation is offline;
    /// dependency installation requires the opaque network scope approved with
    /// the operation, or an explicitly open policy.
    pub(crate) async fn acquire_skill_process_start_permit(
        &self,
        request: haven_skills::SkillProcessRequest,
    ) -> Result<ExecutionPolicyPermit, String> {
        let purpose = request
            .purpose
            .ok_or_else(|| "Skill process purpose is required".to_owned())?;
        let mut permit = self.acquire().await?;
        let install_network = purpose == haven_skills::SkillProcessPurpose::InstallRequirements;
        if install_network && !permit.opaque_network_is_unrestricted() {
            return Err(
                "Skill dependency installation requires Open network policy after process-network authorization"
                    .into(),
            );
        }
        match permit.sandbox_mode() {
            SandboxMode::FullAccess => Ok(permit),
            SandboxMode::WorkspaceWrite if cfg!(windows) => {
                let policy = if purpose == haven_skills::SkillProcessPurpose::Execute {
                    permit.skill_execution_process_policy(
                        &request.scratch_directory,
                        &request.runtime_read_roots,
                    )?
                } else {
                    permit.skill_setup_process_policy(&request, install_network)?
                };
                permit.sandboxed_process_policy = Some(policy);
                Ok(permit)
            }
            SandboxMode::WorkspaceWrite => {
                Err("Workspace Write Skill processes require Windows MXC isolation".into())
            }
            SandboxMode::ReadOnly => {
                Err("Skill processes are blocked by the read-only sandbox".into())
            }
        }
    }

    pub(crate) async fn acquire_git_process_start_permit(
        &self,
        network_required: bool,
    ) -> Result<ExecutionPolicyPermit, String> {
        let permit = self.acquire().await?;
        match permit.sandbox_mode() {
            SandboxMode::FullAccess if !network_required => Ok(permit),
            SandboxMode::FullAccess if permit.network_policy() == NetworkPolicy::Open => Ok(permit),
            SandboxMode::FullAccess => Err(
                "network-capable Git operations require Open network policy in Full Access mode"
                    .into(),
            ),
            SandboxMode::WorkspaceWrite if cfg!(windows) => Ok(permit),
            SandboxMode::WorkspaceWrite => {
                Err("Git operations require Windows MXC isolation in Workspace Write mode".into())
            }
            SandboxMode::ReadOnly => {
                Err("Git operations are blocked by the read-only sandbox".into())
            }
        }
    }

    pub(crate) async fn acquire_everything_process_start_permit(
        &self,
        request: EverythingProcessStartRequest,
    ) -> Result<ExecutionPolicyPermit, String> {
        let mut permit = self.acquire().await?;
        if permit.sandbox_mode() == SandboxMode::FullAccess
            && !permit.opaque_network_is_unrestricted()
        {
            drop(permit);
            permit = self
                .authorization
                .runtime_process_permit(
                    self.session_id.as_deref(),
                    &self.tool_name,
                    serde_json::json!({"service":"Everything", "search_root":request.search_root}),
                    vec![crate::DeclaredResource::LocalPath {
                        path: request.search_root.clone(),
                        access: haven_common::types::LocalPathAccess::Read,
                    }],
                    self.session_trust_cancellation.as_ref().clone(),
                )
                .await?;
        }
        match permit.sandbox_mode() {
            SandboxMode::FullAccess => {
                if matches!(
                    permit.network_policy(),
                    NetworkPolicy::Deny | NetworkPolicy::Restricted
                ) {
                    return Err("Everything CLI requires Open network access in Full Access".into());
                }
                Ok(permit)
            }
            SandboxMode::WorkspaceWrite | SandboxMode::ReadOnly if cfg!(windows) => {
                let policy = permit.everything_process_policy(&request)?;
                permit.sandboxed_process_policy = Some(policy);
                Ok(permit)
            }
            SandboxMode::WorkspaceWrite => {
                Err("Workspace Write Everything CLI requires Windows MXC isolation".into())
            }
            SandboxMode::ReadOnly => {
                Err("Everything CLI is blocked by the read-only sandbox".into())
            }
        }
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
            .file_policy
            .write_mode = match sandbox_mode {
            SandboxMode::ReadOnly => haven_common::config::FileWriteMode::ReadOnly,
            SandboxMode::WorkspaceWrite => haven_common::config::FileWriteMode::Workspace,
            SandboxMode::FullAccess => haven_common::config::FileWriteMode::Unrestricted,
        };
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
        if authorization
            .permission_interaction
            .try_read()
            .unwrap()
            .is_none()
        {
            *authorization.permission_interaction.try_write().unwrap() =
                Some(Arc::new(super::consent::ApprovingPermissionInteraction));
        }
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
            tool_name: tool_name.to_owned(),
            policy_revision,
            session_execution_mode_revision,
            execution_workspace_generation,
            execution_boundary: crate::ExecutionBoundary::HostProcess,
            session_id: session_id.map(ToOwned::to_owned),
            resource_boundary_extension: false,
            network_authorized: false,
            authorized_resources: Vec::new(),
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
        request: haven_mcp::McpStdioProcessStartRequest,
    ) -> Result<McpStdioProcessStartPermit, String> {
        let cwd = {
            let config = self.authorization.config.read().await;
            if config.process_boundary_revision != self.process_boundary_revision
                || config.execution_workspace.generation() != self.execution_workspace_generation
            {
                return Err("MCP process boundaries changed before process start".into());
            }
            request
                .working_directory
                .clone()
                .or_else(|| {
                    config
                        .execution_workspace
                        .root()
                        .map(std::path::Path::to_path_buf)
                })
                .ok_or_else(|| "MCP server has no working directory".to_owned())?
        };
        let permission_request = AuthorizationEngine::runtime_permission_request(
            None,
            "mcp_stdio",
            serde_json::json!({"command_fingerprint":request.command_fingerprint, "cwd":cwd}),
            crate::NetworkAccess::Opaque,
            vec![crate::DeclaredResource::LocalPath {
                path: cwd,
                access: haven_common::types::LocalPathAccess::Read,
            }],
        )?;
        let proof = self
            .authorization
            .request_execution_permission(&permission_request, request.cancellation.clone())
            .await?;
        let context = self
            .authorization
            .execution_context_for_proof(&permission_request, &proof)
            .await?;
        let granted = context
            .clone_execution_authorization()
            .ok_or_else(|| "MCP permission has no execution authority".to_owned())?;
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
        drop(config);
        let mut permit = granted.acquire().await?;
        match permit.sandbox_mode() {
            SandboxMode::FullAccess if permit.opaque_network_is_unrestricted() => {
                Ok(McpStdioProcessStartPermit {
                    _permit: permit,
                    policy: None,
                    working_directory: None,
                    scratch: None,
                })
            }
            SandboxMode::FullAccess => Err(
                "MCP stdio requires Open network policy after process-network authorization in Full Access"
                    .into(),
            ),
            SandboxMode::WorkspaceWrite if cfg!(windows) => {
                let scratch = tokio::select! {
                    biased;
                    _ = request.cancellation.cancelled() => {
                        return Err("MCP process start was cancelled while waiting for scratch capacity".into());
                    }
                    scratch = crate::shell_runtime::ProcessScratchDirectory::create_mcp() => {
                        scratch.map_err(|_| "MCP process scratch directory is unavailable".to_owned())?
                    }
                };
                let allow_public_egress = permit.opaque_network_is_unrestricted();
                let (policy, working_directory) = permit.mcp_stdio_process_policy(
                    &request,
                    scratch.path(),
                    allow_public_egress,
                )?;
                permit.sandboxed_process_policy = Some(policy.clone());
                Ok(McpStdioProcessStartPermit {
                    _permit: permit,
                    policy: Some(policy),
                    working_directory: Some(working_directory),
                    scratch: Some(scratch),
                })
            }
            SandboxMode::WorkspaceWrite => {
                Err("Workspace Write MCP stdio requires Windows MXC isolation".into())
            }
            SandboxMode::ReadOnly => {
                Err("MCP stdio servers are blocked by the read-only sandbox".into())
            }
        }
    }
}

impl haven_mcp::McpStdioProcessStartAuthorization for McpStdioProcessStartAuthorization {
    fn authorize_http_destination<'a>(
        &'a self,
        raw_url: &'a str,
        cancel: CancellationToken,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = anyhow::Result<haven_common::network::ResolvedHttpDestination>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.authorization
                .request_network_destination(None, "mcp", raw_url, cancel)
                .await
                .map_err(anyhow::Error::msg)
        })
    }
    fn acquire_process_start_permit(
        &self,
        request: haven_mcp::McpStdioProcessStartRequest,
    ) -> haven_mcp::McpStdioProcessStartPermitFuture<'_> {
        Box::pin(async move {
            let permit = self
                .acquire(request)
                .await
                .map_err(execution_policy_denied)?;
            Ok(Box::new(permit) as Box<dyn haven_mcp::McpStdioProcessStartPermit>)
        })
    }
}

impl haven_skills::ProcessStartAuthorization for ExecutionPolicyAuthorization {
    async fn acquire_process_start_permit(
        &self,
        request: haven_skills::SkillProcessRequest,
    ) -> anyhow::Result<Box<dyn haven_skills::ProcessStartPermit>> {
        let permit =
            ExecutionPolicyAuthorization::acquire_skill_process_start_permit(self, request)
                .await
                .map_err(execution_policy_denied)?;
        Ok(Box::new(permit))
    }
}

impl haven_skills::ProcessStartPermit for ExecutionPolicyPermit {
    fn sandboxed_process_policy(
        &self,
    ) -> Option<haven_platform::sandboxed_process::SandboxedProcessPolicy> {
        self.sandboxed_process_policy.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        EverythingProcessStartRequest, ExecutionPolicyAuthorization,
        LifecycleHookProcessStartRequest,
    };
    use crate::security::AuthorizationEngine;
    use haven_common::config::MxcIsolationMode;
    use haven_common::types::{NetworkPolicy, SandboxMode};
    use haven_skills::{SkillProcessPurpose, SkillProcessRequest};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn execution_policy_permit_carries_mxc_isolation_choice() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
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
    async fn git_confirmed_metadata_write_overrides_default_read_only_scope() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let metadata = workspace.join(".git");
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(&metadata).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let (_, engine) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        engine
            .config
            .try_write()
            .unwrap()
            .file_policy
            .global_read_only_roots = vec![metadata.clone()];
        let resources = [&workspace, &metadata]
            .into_iter()
            .map(|path| crate::DeclaredResource::LocalPath {
                path: path.clone(),
                access: haven_common::types::LocalPathAccess::Write,
            })
            .collect();
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "git",
            serde_json::json!({}),
            crate::NetworkAccess::None,
            resources,
        )
        .unwrap();
        let check = engine.authorize_for_execution(&request).await;
        let crate::security::AuthorizationDecision::RequiresConfirmation { receipt, .. } =
            check.decision
        else {
            panic!("protected Git metadata needs confirmation");
        };
        assert!(receipt.resource_boundary_extension);
        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .unwrap();
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .unwrap();
        let authorization = context.clone_execution_authorization().unwrap();
        let permit = authorization.acquire().await.unwrap();
        let policy = permit
            .workspace_git_directories(&workspace, &[], &scratch, std::slice::from_ref(&metadata))
            .unwrap();
        assert!(!policy.protected_readonly_paths.contains(&metadata));
    }

    #[tokio::test]
    async fn git_linked_directory_permission_decline_or_cancel_yields_no_proof() {
        struct Decline;
        #[async_trait::async_trait]
        impl crate::PermissionInteractionPort for Decline {
            async fn request_permission(
                &self,
                _: crate::AuthorizationRequest,
                _: crate::ConfirmationReceipt,
                _: CancellationToken,
            ) -> Result<crate::ConfirmationReceipt, String> {
                Err("declined".into())
            }
        }
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("external");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&external).unwrap();
        let (_, engine) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        engine.set_permission_interaction(Arc::new(Decline)).await;
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "git",
            serde_json::json!({}),
            crate::NetworkAccess::None,
            vec![crate::DeclaredResource::LocalPath {
                path: external,
                access: haven_common::types::LocalPathAccess::Write,
            }],
        )
        .unwrap();
        assert!(
            engine
                .request_execution_permission(&request, CancellationToken::new())
                .await
                .is_err()
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(
            engine
                .request_execution_permission(&request, cancel)
                .await
                .is_err()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires installed Git and a Windows host with MXC support"]
    async fn git_linked_worktree_runs_after_confirmed_resources() {
        use crate::Tool;
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let repository = root.path().join("repository");
        let worktree = root.path().join("worktree");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&repository).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(&repository)
                .args(args)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "NUL")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
        git(&[
            "worktree",
            "add",
            "-b",
            "test-worktree",
            worktree.to_str().unwrap(),
        ]);
        let (_, engine) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let tool = crate::builtin::git::GitTool::new(Arc::clone(&engine), 8000);
        let input = serde_json::json!({"subcommand":"status", "cwd": worktree});
        let contract = tool.authorization_contract(&input).await.unwrap();
        let resources =
            crate::builtin::git::authorization_resources_for_workspace(&input, &workspace).unwrap();
        let request = crate::AuthorizationRequest::new(None, "git", input.clone(), contract.policy)
            .with_resources(resources);
        let check = engine.authorize_for_execution(&request).await;
        let crate::security::AuthorizationDecision::RequiresConfirmation { receipt, .. } =
            check.decision
        else {
            panic!("linked Git requires confirmation");
        };
        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .unwrap();
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .unwrap();
        let marker = worktree.join(".git");
        let original_marker = std::fs::read(&marker).unwrap();
        let replacement = root.path().join("replacement-metadata");
        std::fs::create_dir_all(&replacement).unwrap();
        std::fs::write(&marker, format!("gitdir: {}\n", replacement.display())).unwrap();
        let changed = tool
            .execute_with_authorization_context(
                input.clone(),
                CancellationToken::new(),
                None,
                &context,
            )
            .await
            .unwrap_err();
        assert!(changed.to_string().contains("changed after authorization"));
        std::fs::write(&marker, original_marker).unwrap();
        let result = tool
            .execute_with_authorization_context(input, CancellationToken::new(), None, &context)
            .await
            .unwrap();
        assert!(result.success, "Git status should succeed: {result:?}");
    }

    #[tokio::test]
    async fn git_linked_metadata_and_worktree_use_one_confirmed_scope() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let worktree = root.path().join("worktree");
        let common = root.path().join("repository/.git");
        let metadata = common.join("worktrees/linked");
        let scratch = root.path().join("scratch");
        for path in [&workspace, &worktree, &metadata, &scratch] {
            std::fs::create_dir_all(path).unwrap();
        }
        let (_, engine) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let resources = [&worktree, &metadata, &common]
            .into_iter()
            .map(|path| crate::DeclaredResource::LocalPath {
                path: path.clone(),
                access: haven_common::types::LocalPathAccess::Write,
            })
            .collect();
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "git",
            serde_json::json!({"worktree": worktree, "metadata": metadata, "common": common}),
            crate::NetworkAccess::None,
            resources,
        )
        .unwrap();
        let check = engine.authorize_for_execution(&request).await;
        let crate::security::AuthorizationDecision::RequiresConfirmation { receipt, .. } =
            check.decision
        else {
            panic!("linked worktree must request permission");
        };
        assert_eq!(receipt.resources, request.resources);
        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .unwrap();
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .unwrap();
        let authorization = context.clone_execution_authorization().unwrap();
        let permit = authorization.acquire().await.unwrap();
        let policy = permit
            .workspace_git_directories(
                &worktree,
                &[],
                &scratch,
                &[metadata.clone(), common.clone()],
            )
            .unwrap();
        assert!(
            policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == worktree)
        );
        assert!(
            policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == common)
        );
        assert!(
            !policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == workspace)
        );
    }

    #[tokio::test]
    async fn git_external_resource_confirmation_reaches_process_policy() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("external");
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(external.join(".git")).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let (_, engine) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "git",
            serde_json::json!({"cwd": external}),
            crate::NetworkAccess::None,
            vec![crate::DeclaredResource::LocalPath {
                path: external.clone(),
                access: haven_common::types::LocalPathAccess::Write,
            }],
        )
        .unwrap();
        let check = engine.authorize_for_execution(&request).await;
        let crate::security::AuthorizationDecision::RequiresConfirmation { receipt, .. } =
            check.decision
        else {
            panic!("external Git directory must request permission");
        };
        assert_eq!(receipt.resources, request.resources);
        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .unwrap();
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .unwrap();
        let authorization = context.clone_execution_authorization().unwrap();
        let permit = authorization.acquire().await.unwrap();
        let directory = haven_platform::filesystem::DirectoryCapability::open(&external).unwrap();
        assert!(permit.authorizes_git_directory(&directory));
        let policy = permit
            .workspace_git_directories(&external, &[], &scratch, &[external.join(".git")])
            .unwrap();
        assert!(
            policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == directory.root())
        );
    }

    #[tokio::test]
    async fn git_desktop_handles_require_the_corresponding_proof_resource() {
        let (mut authorization, _) = ExecutionPolicyAuthorization::for_test_with_engine();
        let permit = authorization.acquire().await.unwrap();
        assert!(!permit.authorizes_git_desktop_handles());
        drop(permit);
        authorization.authorized_resources = vec![crate::DeclaredResource::Desktop {
            access: haven_common::types::DesktopAccess::Capture,
        }];
        let permit = authorization.acquire().await.unwrap();
        assert!(!permit.authorizes_git_desktop_handles());
        drop(permit);
        authorization.authorized_resources = vec![crate::DeclaredResource::Desktop {
            access: haven_common::types::DesktopAccess::ProcessHandles,
        }];
        let permit = authorization.acquire().await.unwrap();
        assert!(permit.authorizes_git_desktop_handles());
    }

    #[tokio::test]
    async fn git_external_directory_must_match_proof_write_resource() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("external");
        let other = root.path().join("other");
        for path in [&workspace, &external, &other] {
            std::fs::create_dir_all(path).unwrap();
        }
        let (mut authorization, _) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        authorization.resource_boundary_extension = true;
        authorization.authorized_resources = vec![crate::DeclaredResource::LocalPath {
            path: external.clone(),
            access: haven_common::types::LocalPathAccess::Write,
        }];
        let permit = authorization.acquire().await.unwrap();
        let selected = haven_platform::filesystem::DirectoryCapability::open(&external).unwrap();
        let unrelated = haven_platform::filesystem::DirectoryCapability::open(&other).unwrap();
        assert!(permit.authorizes_git_directory(&selected));
        assert!(!permit.authorizes_git_directory(&unrelated));
        std::fs::create_dir_all(external.join(".git")).unwrap();
        let scratch = root.path().join("scratch");
        std::fs::create_dir_all(&scratch).unwrap();
        let policy = permit
            .workspace_git_directories(&external, &[], &scratch, &[external.join(".git")])
            .unwrap();
        assert!(
            policy
                .readwrite_roots
                .iter()
                .any(|path| path.root() == selected.root())
        );
        assert!(
            !policy
                .readwrite_roots
                .iter()
                .any(|path| path.root() == unrelated.root())
        );
        drop(permit);
        authorization.authorized_resources = vec![crate::DeclaredResource::LocalPath {
            path: external,
            access: haven_common::types::LocalPathAccess::Read,
        }];
        let permit = authorization.acquire().await.unwrap();
        assert!(!permit.authorizes_git_directory(&selected));
    }

    #[tokio::test]
    async fn lsp_process_roots_keep_project_readonly_and_scratch_writable() {
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let scratch = root.path().join("runtime/lsp-call");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let (authorization, _) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let permit = authorization.acquire().await.unwrap();

        let policy = permit
            .workspace_lsp_directories(&workspace, &[], &scratch)
            .expect("workspace-write LSP policy compiles");
        assert_eq!(policy.readwrite_roots.len(), 1);
        assert_eq!(policy.readwrite_roots[0].root(), scratch);
        assert!(
            policy
                .readonly_roots
                .iter()
                .any(|root| root.root() == workspace)
        );
        assert!(
            !policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == workspace)
        );
    }

    #[tokio::test]
    async fn workspace_everything_policy_is_readonly_offline_and_root_scoped() {
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let search = workspace.join("project");
        std::fs::create_dir_all(&search).unwrap();
        let scratch = crate::shell_runtime::ProcessScratchDirectory::create_lsp()
            .await
            .unwrap();
        let (authorization, _) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Open,
        );
        let permit = authorization.acquire().await.unwrap();
        let policy = permit
            .everything_process_policy(&EverythingProcessStartRequest {
                search_root: search.clone(),
                scratch_directory: scratch.path().to_path_buf(),
                runtime_read_roots: Vec::new(),
            })
            .expect("Everything policy compiles for a workspace subtree");
        assert_eq!(policy.readwrite_paths, vec![scratch.path().to_path_buf()]);
        assert!(policy.readonly_paths.contains(&search));
        assert!(!policy.readonly_paths.contains(&workspace));
        assert!(!policy.network_policy.allow_public_egress);
        assert!(!policy.network_policy.allow_private_network_client_server);
        assert!(!policy.network_policy.allow_host_loopback);

        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(
            permit
                .everything_process_policy(&EverythingProcessStartRequest {
                    search_root: outside,
                    scratch_directory: scratch.path().to_path_buf(),
                    runtime_read_roots: Vec::new(),
                })
                .is_err(),
            "the search provider cannot widen the configured workspace read scope"
        );
    }

    #[tokio::test]
    async fn workspace_mcp_policy_uses_readonly_cwd_private_scratch_and_network_policy() {
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let cwd = workspace.join("server");
        std::fs::create_dir_all(&cwd).unwrap();
        let scratch = crate::shell_runtime::ProcessScratchDirectory::create_lsp()
            .await
            .unwrap();
        let (authorization, _) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let permit = authorization.acquire().await.unwrap();
        let request = haven_mcp::McpStdioProcessStartRequest {
            cancellation: CancellationToken::new(),
            command_fingerprint: "test-server".into(),
            working_directory: Some(cwd.clone()),
            runtime_read_roots: Vec::new(),
        };
        let (policy, selected_cwd) = permit
            .mcp_stdio_process_policy(&request, scratch.path(), false)
            .expect("MCP policy compiles under Workspace Write");
        assert_eq!(selected_cwd, cwd);
        assert_eq!(policy.readwrite_paths, vec![scratch.path().to_path_buf()]);
        assert!(policy.readonly_paths.contains(&selected_cwd));
        assert!(!policy.readonly_paths.contains(&workspace));
        assert!(!policy.network_policy.allow_public_egress);
        assert!(!policy.network_policy.allow_private_network_client_server);
        assert!(!policy.network_policy.allow_host_loopback);

        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        assert!(
            permit
                .mcp_stdio_process_policy(
                    &haven_mcp::McpStdioProcessStartRequest {
                        cancellation: CancellationToken::new(),
                        command_fingerprint: "test-server".into(),
                        working_directory: Some(outside),
                        runtime_read_roots: Vec::new(),
                    },
                    scratch.path(),
                    false,
                )
                .is_err(),
            "an MCP server cwd outside the workspace must be rejected"
        );
    }

    #[tokio::test]
    async fn workspace_skill_setup_policy_only_writes_private_roots_and_stays_offline() {
        if !cfg!(windows) {
            return;
        }
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let skill = root.path().join("skills/demo");
        let venv = root.path().join("venvs/demo");
        let scratch = root.path().join("runtime/skill-setup");
        let runtime = root.path().join("runtime/python");
        for path in [&workspace, &skill, &venv, &scratch, &runtime] {
            std::fs::create_dir_all(path).unwrap();
        }
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .config
            .try_write()
            .unwrap()
            .configure_test_workspace_write(&workspace, NetworkPolicy::Deny)
            .unwrap();
        let authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "skill__demo",
        );
        let permit = authorization
            .acquire_skill_process_start_permit(SkillProcessRequest {
                purpose: Some(SkillProcessPurpose::CreateEnvironment),
                skill_root: skill,
                venv_directory: venv.clone(),
                scratch_directory: scratch.clone(),
                runtime_read_roots: vec![runtime],
            })
            .await
            .expect("venv creation is allowed offline in Workspace Write");
        let policy = permit
            .sandboxed_process_policy()
            .expect("Workspace Write returns an MXC policy");
        assert!(policy.readwrite_paths.contains(&venv));
        assert!(policy.readwrite_paths.contains(&scratch));
        assert!(!policy.readwrite_paths.contains(&workspace));
        assert!(!policy.network_policy.allow_public_egress);
        assert!(!policy.network_policy.allow_private_network_client_server);
        assert!(!policy.network_policy.allow_host_loopback);
    }

    #[tokio::test]
    async fn workspace_skill_dependency_install_needs_unrestricted_open_network() {
        if !cfg!(windows) {
            return;
        }
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let skill = root.path().join("skills/demo");
        let venv = root.path().join("venvs/demo");
        let scratch = root.path().join("runtime/skill-setup");
        let runtime = root.path().join("runtime/python");
        for path in [&workspace, &skill, &venv, &scratch, &runtime] {
            std::fs::create_dir_all(path).unwrap();
        }
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        {
            let mut config = engine.config.try_write().unwrap();
            config
                .configure_test_workspace_write(&workspace, NetworkPolicy::Open)
                .unwrap();
        }
        let authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "skill__demo",
        );
        let permit = authorization
            .acquire_skill_process_start_permit(SkillProcessRequest {
                purpose: Some(SkillProcessPurpose::InstallRequirements),
                skill_root: skill.clone(),
                venv_directory: venv.clone(),
                scratch_directory: scratch.clone(),
                runtime_read_roots: vec![runtime.clone()],
            })
            .await
            .expect("Open policy without domain restrictions permits pip");
        let policy = permit.sandboxed_process_policy().unwrap();
        assert!(policy.network_policy.allow_public_egress);
        assert!(!policy.network_policy.allow_private_network_client_server);
        assert!(!policy.network_policy.allow_host_loopback);

        drop(permit);
        engine.config.try_write().unwrap().network.access =
            haven_common::config::NetworkAccessBoundary::Restricted;
        let authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "skill__demo",
        );
        let error = match authorization
            .acquire_skill_process_start_permit(SkillProcessRequest {
                purpose: Some(SkillProcessPurpose::InstallRequirements),
                skill_root: skill,
                venv_directory: venv,
                scratch_directory: scratch,
                runtime_read_roots: vec![runtime],
            })
            .await
        {
            Ok(_) => panic!("MXC cannot enforce domain allowlists for pip"),
            Err(error) => error,
        };
        assert!(error.contains("after process-network authorization"));
    }

    #[tokio::test]
    async fn git_process_can_write_only_the_selected_repository_metadata() {
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let git_metadata = workspace.join(".git");
        let scratch = root.path().join("runtime/git-call");
        std::fs::create_dir_all(&git_metadata).unwrap();
        std::fs::write(
            git_metadata.join("config"),
            "[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let (authorization, _) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let permit = authorization.acquire().await.unwrap();

        let shell_policy = permit
            .workspace_shell_directories_with_runtime_writes(&workspace, &[], &scratch, &[])
            .expect("ordinary Shell keeps Git metadata protected");
        assert!(
            !shell_policy
                .protected_readonly_paths
                .contains(&git_metadata)
        );

        let git_policy = permit
            .workspace_git_directories(
                &workspace,
                &[],
                &scratch,
                std::slice::from_ref(&git_metadata),
            )
            .expect("scoped Git policy allows its repository metadata");
        assert!(
            git_policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == workspace)
        );
        assert!(
            git_policy
                .readwrite_roots
                .iter()
                .any(|root| root.root() == scratch)
        );
        assert!(!git_policy.protected_readonly_paths.contains(&git_metadata));
    }

    #[tokio::test]
    async fn git_process_cannot_override_explicit_metadata_denials() {
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let git_metadata = workspace.join(".git");
        let scratch = root.path().join("runtime/git-call");
        std::fs::create_dir_all(&git_metadata).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        engine
            .config
            .try_write()
            .expect("test config must be idle")
            .file_policy
            .global_denied_paths = vec![git_metadata.clone()];
        let permit = authorization.acquire().await.unwrap();

        let error = permit
            .workspace_git_directories(
                &workspace,
                &[],
                &scratch,
                std::slice::from_ref(&git_metadata),
            )
            .expect_err("explicit Git metadata denial is authoritative");
        assert!(error.contains("denied path"));
    }

    #[tokio::test]
    async fn git_process_honors_its_own_read_only_tool_policy() {
        let root = tempfile::tempdir().expect("temporary root is available");
        let workspace = root.path().join("workspace");
        let git_metadata = workspace.join(".git");
        let scratch = root.path().join("runtime/git-call");
        std::fs::create_dir_all(&git_metadata).unwrap();
        std::fs::create_dir_all(&scratch).unwrap();
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        engine
            .config
            .try_write()
            .expect("test config must be idle")
            .tool_settings
            .insert(
                "git".into(),
                haven_common::config::ToolConfig {
                    resource_policy: haven_common::config::ResourcePolicy {
                        files: haven_common::config::FileResourcePolicy {
                            write_mode: haven_common::config::FileWriteMode::ReadOnly,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        let permit = authorization.acquire().await.unwrap();

        let error = permit
            .workspace_git_directories(
                &workspace,
                &[],
                &scratch,
                std::slice::from_ref(&git_metadata),
            )
            .expect_err("Git tool read-only policy must block metadata writes");
        assert!(error.contains("outside the configured write scope"));
    }

    #[tokio::test]
    async fn lifecycle_hook_start_requests_permissions_and_rejects_stale_snapshots() {
        if !cfg!(windows) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .switch_execution_workspace(root.path())
            .await
            .unwrap();
        engine
            .set_boundaries(SandboxMode::ReadOnly, Vec::new(), NetworkPolicy::Restricted)
            .await;
        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization(None)
            .await
            .unwrap();
        let permit = authorization
            .acquire(LifecycleHookProcessStartRequest {
                command: "fixture".into(),
                arguments: Vec::new(),
                working_directory: root.path().to_path_buf(),
                runtime_read_roots: Vec::new(),
            })
            .await
            .unwrap();
        let policy = permit.sandboxed_process_policy().unwrap();
        assert!(policy.readwrite_paths.contains(&root.path().to_path_buf()));
        assert!(policy.network_policy.allow_host_loopback);
        drop(permit);
        engine
            .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Open)
            .await;
        assert!(
            authorization
                .acquire(LifecycleHookProcessStartRequest {
                    command: "fixture".into(),
                    arguments: Vec::new(),
                    working_directory: root.path().to_path_buf(),
                    runtime_read_roots: Vec::new()
                })
                .await
                .err()
                .unwrap()
                .contains("policy changed")
        );
    }

    #[tokio::test]
    async fn workspace_write_hook_policy_uses_explicit_directory_and_network_consent() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let runtime = temp.path().join("runtime");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let (_tool_authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Restricted,
            );
        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization(None)
            .await;
        if cfg!(windows) {
            let authorization = authorization.expect("Workspace Write hooks use Windows MXC");
            let permit = authorization
                .acquire(LifecycleHookProcessStartRequest {
                    command: "fixture".into(),
                    arguments: Vec::new(),
                    working_directory: workspace.clone(),
                    runtime_read_roots: vec![runtime.clone()],
                })
                .await
                .unwrap();
            let policy = permit
                .sandboxed_process_policy()
                .expect("Workspace Write hook has an MXC policy");
            assert!(policy.readwrite_paths.contains(&workspace));
            assert!(!policy.readwrite_paths.contains(&runtime));
            assert!(policy.readonly_paths.contains(&runtime));
            assert!(policy.network_policy.allow_public_egress);
            assert!(policy.network_policy.allow_private_network_client_server);
            assert!(policy.network_policy.allow_host_loopback);
        } else {
            assert!(authorization.is_err());
        }
    }

    #[tokio::test]
    async fn lifecycle_hook_authorization_uses_and_tracks_the_session_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let global_workspace = temp.path().join("global");
        let first_session_workspace = temp.path().join("first-session");
        let second_session_workspace = temp.path().join("second-session");
        for path in [
            &global_workspace,
            &first_session_workspace,
            &second_session_workspace,
        ] {
            std::fs::create_dir_all(path).unwrap();
        }

        let session_id = "ses-00000000000000000000000000000001";
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .switch_execution_workspace(&global_workspace)
            .await
            .unwrap();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        engine
            .bind_session_execution_workspace(session_id, Some(&first_session_workspace))
            .await
            .unwrap();

        let workspace = engine.execution_workspace_for_session(session_id).await;
        assert_eq!(workspace.root(), Some(first_session_workspace.as_path()));
        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization(Some(session_id))
            .await
            .unwrap();
        assert_eq!(
            authorization.execution_workspace_generation(),
            workspace.generation()
        );
        authorization
            .acquire(LifecycleHookProcessStartRequest {
                command: "fixture".into(),
                arguments: Vec::new(),
                working_directory: first_session_workspace.clone(),
                runtime_read_roots: Vec::new(),
            })
            .await
            .unwrap();

        engine
            .bind_session_execution_workspace(session_id, Some(&second_session_workspace))
            .await
            .unwrap();
        assert!(
            authorization
                .acquire(LifecycleHookProcessStartRequest {
                    command: "fixture".into(),
                    arguments: Vec::new(),
                    working_directory: first_session_workspace,
                    runtime_read_roots: Vec::new(),
                })
                .await
                .err()
                .expect("the previous session workspace is stale")
                .contains("policy changed")
        );
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            authorization.wait_for_revocation(),
        )
        .await
        .expect("changing a session workspace revokes its running Hook");
    }

    #[tokio::test]
    async fn workspace_write_hook_consent_can_extend_default_readonly_and_path_roots() {
        if !cfg!(windows) {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let allowed_subdirectory = workspace.join("allowed");
        std::fs::create_dir_all(&allowed_subdirectory).unwrap();
        let (_tool_authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Open,
            );

        engine.config.try_write().unwrap().tool_settings.insert(
            "lifecycle_hook".into(),
            haven_common::config::ToolConfig {
                resource_policy: haven_common::config::ResourcePolicy {
                    files: haven_common::config::FileResourcePolicy {
                        write_mode: haven_common::config::FileWriteMode::ReadOnly,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization(None)
            .await
            .unwrap();
        let permit = authorization
            .acquire(LifecycleHookProcessStartRequest {
                command: "fixture".into(),
                arguments: Vec::new(),
                working_directory: workspace.clone(),
                runtime_read_roots: Vec::new(),
            })
            .await
            .unwrap();
        let policy = permit.sandboxed_process_policy().unwrap();
        assert!(policy.readwrite_paths.contains(&workspace));

        drop(permit);
        {
            let mut config = engine.config.try_write().unwrap();
            config.tool_settings.remove("lifecycle_hook");
            config.file_policy.global_file_roots = vec![allowed_subdirectory];
        }
        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization(None)
            .await
            .unwrap();
        let permit = authorization
            .acquire(LifecycleHookProcessStartRequest {
                command: "fixture".into(),
                arguments: Vec::new(),
                working_directory: workspace.clone(),
                runtime_read_roots: Vec::new(),
            })
            .await
            .expect("approved directory scope extends default path roots");
        assert!(
            permit
                .sandboxed_process_policy()
                .unwrap()
                .readwrite_paths
                .contains(&workspace)
        );
    }

    #[tokio::test]
    async fn lsp_start_permit_uses_mxc_in_workspace_write_and_open_network_in_full_access() {
        let (authorization, _engine) =
            ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                SandboxMode::WorkspaceWrite,
            );
        let workspace_write = authorization.acquire_lsp_process_start_permit().await;
        if cfg!(windows) {
            assert!(workspace_write.is_ok());
        } else {
            assert!(
                workspace_write
                    .err()
                    .expect("workspace-write without Windows MXC is denied")
                    .contains("Windows MXC")
            );
        }

        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_engine_and_sandbox_mode(
                SandboxMode::FullAccess,
            );
        for network_policy in [NetworkPolicy::Open] {
            engine
                .config
                .try_write()
                .expect("test authorization config must be idle")
                .network
                .access = network_policy.into();
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
                .network
                .access = network_policy.into();
            assert!(
                authorization
                    .acquire_lsp_process_start_permit()
                    .await
                    .err()
                    .expect("restricted network must be denied")
                    .contains("Open network policy")
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
