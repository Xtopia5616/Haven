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
    policy_revision: u64,
    process_boundary_revision: u64,
    execution_workspace_generation: u64,
    process_revocation_signal: Arc<()>,
    process_boundary_signal: Arc<()>,
}

pub(crate) struct LifecycleHookProcessStartRequest {
    pub(crate) working_directory: std::path::PathBuf,
    pub(crate) runtime_read_roots: Vec<std::path::PathBuf>,
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
    scratch: Option<tempfile::TempDir>,
}

impl haven_mcp::McpStdioProcessStartPermit for McpStdioProcessStartPermit {
    fn sandboxed_process_policy(
        &self,
    ) -> Option<haven_platform::sandboxed_process::SandboxedProcessPolicy> {
        self.policy.clone()
    }

    fn sandboxed_working_directory(&self) -> Option<&std::path::Path> {
        self.working_directory.as_deref()
    }

    fn sandboxed_temp_directory(&self) -> Option<&std::path::Path> {
        self.scratch.as_ref().map(tempfile::TempDir::path)
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

    pub(crate) async fn acquire(
        &self,
        request: LifecycleHookProcessStartRequest,
    ) -> Result<ExecutionPolicyPermit, String> {
        let config = Arc::clone(&self.authorization.config).read_owned().await;
        if config.policy_revision != self.policy_revision
            || config.process_boundary_revision != self.process_boundary_revision
            || config.execution_workspace.generation() != self.execution_workspace_generation
        {
            return Err("execution policy changed before lifecycle hook process start".into());
        }
        let sandbox_mode = config.sandbox_mode;
        let mut permit = ExecutionPolicyPermit {
            _config: config,
            session_id: None,
            tool_name: "lifecycle_hook".into(),
            resource_boundary_extension: false,
            authorized_resources: Vec::new(),
            sandboxed_process_policy: None,
        };
        match sandbox_mode {
            SandboxMode::FullAccess if permit.opaque_network_is_unrestricted() => Ok(permit),
            SandboxMode::FullAccess => {
                Err("lifecycle hook commands require Open network policy without domain allowlists in Full Access".into())
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
                        network_policy: Default::default(),
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
            tokio::select! {
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
    authorized_resources: Vec<crate::security::DeclaredResource>,
    sandboxed_process_policy: Option<haven_platform::sandboxed_process::SandboxedProcessPolicy>,
}

pub(super) fn opaque_network_is_unrestricted(config: &SafetyConfig, tool_name: &str) -> bool {
    if config.network.network_policy() != NetworkPolicy::Open
        || !config.network.allowed_domains.is_empty()
    {
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
            ) && setting.resource_policy.network.allowed_domains.is_empty()
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
    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self._config.sandbox_mode
    }

    pub(crate) fn network_policy(&self) -> NetworkPolicy {
        self._config.network.network_policy()
    }

    fn opaque_network_is_unrestricted(&self) -> bool {
        opaque_network_is_unrestricted(&self._config, &self.tool_name)
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
            None,
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
            None,
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
            None,
        )
    }

    pub(crate) fn workspace_git_directories(
        &self,
        working_directory: &std::path::Path,
        runtime_read_roots: &[std::path::PathBuf],
        scratch_directory: &std::path::Path,
        git_metadata_directory: &std::path::Path,
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        self.compile_workspace_process_directories(
            "git",
            working_directory,
            runtime_read_roots,
            Some(scratch_directory),
            &[],
            Some(git_metadata_directory),
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
        git_metadata_write: Option<&std::path::Path>,
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
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
            .get(tool_name)
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
        let git_metadata_identity = git_metadata_write
            .map(|path| {
                super::filesystem::open_filesystem_paths(
                    &[path.to_path_buf()],
                    "Git repository metadata",
                )
                .map(|mut paths| paths.remove(0))
            })
            .transpose()?;

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
        for constraint in &path_constraints {
            runtime_read = intersect_directory_roots(&runtime_read, constraint);
        }
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
            if self.authorized_resources.is_empty() {
                return Err("process resource extension has no bound local paths".into());
            }
            for resource in &self.authorized_resources {
                let crate::security::DeclaredResource::LocalPath { path, access } = resource else {
                    return Err(
                        "process resource extension contains an unsupported resource type".into(),
                    );
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
                && !runtime_read
                    .iter()
                    .any(|protected| directory_root_contains_path(protected, writable.root()))
        });

        if let Some(git_metadata) = &git_metadata_identity {
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
            if protected_readonly.iter().any(|protected| {
                protected
                    .contains_path(metadata_directory.root())
                    .unwrap_or(true)
                    || git_metadata.contains_path(protected.path()).unwrap_or(true)
            }) {
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

        let metadata_roots = haven_platform::filesystem::protected_metadata_paths(
            &write_scope
                .iter()
                .map(|directory| directory.root().to_path_buf())
                .collect::<Vec<_>>(),
        );
        let metadata =
            super::filesystem::open_filesystem_paths(&metadata_roots, "protected VCS metadata")?;
        let metadata = metadata
            .into_iter()
            .filter(|protected| {
                git_metadata_identity
                    .as_ref()
                    .is_none_or(|git_metadata| !git_metadata.same_object(protected))
            })
            .collect::<Vec<_>>();

        let mut protected_readonly_paths = protected_readonly
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
        protected_readonly_paths.extend(
            runtime_read
                .iter()
                .filter(|protected| {
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
    /// dependency installation may use opaque public egress only when both the
    /// global and Skill network policies are Open without domain allowlists.
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
                "Skill dependency installation requires Open network policy without domain allowlists"
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
            SandboxMode::WorkspaceWrite if cfg!(windows) => {
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
            tool_name: tool_name.to_owned(),
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
    pub(super) async fn acquire(
        &self,
        request: haven_mcp::McpStdioProcessStartRequest,
    ) -> Result<McpStdioProcessStartPermit, String> {
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
        let mut permit = ExecutionPolicyPermit {
            _config: config,
            session_id: None,
            tool_name: "mcp_stdio".into(),
            resource_boundary_extension: false,
            authorized_resources: Vec::new(),
            sandboxed_process_policy: None,
        };
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
                "MCP stdio requires Open network policy without domain allowlists in Full Access"
                    .into(),
            ),
            SandboxMode::WorkspaceWrite if cfg!(windows) => {
                let scratch_root =
                    haven_common::config::default_runtime_temp_root().join("mcp-stdio-scratch");
                std::fs::create_dir_all(&scratch_root)
                    .map_err(|_| "MCP process scratch root is unavailable".to_owned())?;
                haven_platform::filesystem::DirectoryCapability::open(&scratch_root)
                    .map_err(|_| "MCP process scratch root is unsafe".to_owned())?;
                let scratch = tempfile::Builder::new()
                    .prefix("server-")
                    .tempdir_in(&scratch_root)
                    .map_err(|_| "MCP process scratch directory could not be created".to_owned())?;
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
        let scratch = crate::shell_runtime::ProcessScratchDirectory::create_lsp().unwrap();
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
        let scratch = crate::shell_runtime::ProcessScratchDirectory::create_lsp().unwrap();
        let (authorization, _) = ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
            &workspace,
            NetworkPolicy::Deny,
        );
        let permit = authorization.acquire().await.unwrap();
        let request = haven_mcp::McpStdioProcessStartRequest {
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
        engine.config.try_write().unwrap().network.allowed_domains =
            vec!["packages.example".into()];
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
        assert!(error.contains("without domain allowlists"));
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
            shell_policy
                .protected_readonly_paths
                .contains(&git_metadata)
        );

        let git_policy = permit
            .workspace_git_directories(&workspace, &[], &scratch, &git_metadata)
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
            .global_denied_paths = vec![git_metadata.clone()];
        let permit = authorization.acquire().await.unwrap();

        let error = permit
            .workspace_git_directories(&workspace, &[], &scratch, &git_metadata)
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
            .workspace_git_directories(&workspace, &[], &scratch, &git_metadata)
            .expect_err("Git tool read-only policy must block metadata writes");
        assert!(error.contains("outside the configured write scope"));
    }

    #[tokio::test]
    async fn lifecycle_hook_start_uses_mxc_for_workspace_write_and_requires_live_policy() {
        let engine = Arc::new(AuthorizationEngine::new());
        let workspace_write = engine
            .capture_lifecycle_hook_process_start_authorization()
            .await;
        if cfg!(windows) {
            assert!(workspace_write.is_ok());
        } else {
            assert!(
                workspace_write
                    .err()
                    .expect("non-Windows Workspace Write hooks are denied")
                    .contains("Windows MXC")
            );
        }

        engine
            .set_boundaries(SandboxMode::ReadOnly, Vec::new(), NetworkPolicy::Open)
            .await;
        assert!(
            engine
                .capture_lifecycle_hook_process_start_authorization()
                .await
                .err()
                .expect("ReadOnly blocks user Hook processes")
                .contains("read-only sandbox")
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
        let request = LifecycleHookProcessStartRequest {
            working_directory: std::env::current_dir().unwrap(),
            runtime_read_roots: Vec::new(),
        };
        let permit = authorization.acquire(request).await.unwrap();
        assert!(permit.sandboxed_process_policy().is_none());
        drop(permit);

        engine.config.try_write().unwrap().network.allowed_domains = vec!["hooks.example".into()];
        let error = authorization
            .acquire(LifecycleHookProcessStartRequest {
                working_directory: std::env::current_dir().unwrap(),
                runtime_read_roots: Vec::new(),
            })
            .await
            .err()
            .expect("a live domain allowlist cannot constrain an opaque Hook process");
        assert!(error.contains("without domain allowlists"));
        assert!(
            engine
                .capture_lifecycle_hook_process_start_authorization()
                .await
                .err()
                .expect("global Hook domain allowlist blocks FullAccess")
                .contains("without domain allowlists")
        );

        {
            let mut config = engine.config.try_write().unwrap();
            config.network.allowed_domains.clear();
            config.tool_settings.insert(
                "lifecycle_hook".into(),
                haven_common::config::ToolConfig {
                    resource_policy: haven_common::config::ResourcePolicy {
                        network: haven_common::config::NetworkResourcePolicy {
                            access: haven_common::config::NetworkAccessBoundary::Restricted,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }
        assert!(
            engine
                .capture_lifecycle_hook_process_start_authorization()
                .await
                .err()
                .expect("Hook-level network restriction blocks FullAccess")
                .contains("without domain allowlists")
        );
        engine
            .config
            .try_write()
            .unwrap()
            .tool_settings
            .remove("lifecycle_hook");

        engine
            .set_boundaries(
                SandboxMode::FullAccess,
                vec![PathBuf::from("C:/workspace")],
                NetworkPolicy::Open,
            )
            .await;
        assert!(
            authorization
                .acquire(LifecycleHookProcessStartRequest {
                    working_directory: std::env::current_dir().unwrap(),
                    runtime_read_roots: Vec::new(),
                })
                .await
                .err()
                .expect("stale authorization is denied")
                .contains("policy changed")
        );
    }

    #[tokio::test]
    async fn workspace_write_hook_policy_is_scoped_to_workspace_and_offline() {
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
            .capture_lifecycle_hook_process_start_authorization()
            .await;
        if cfg!(windows) {
            let authorization = authorization.expect("Workspace Write hooks use Windows MXC");
            let permit = authorization
                .acquire(LifecycleHookProcessStartRequest {
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
            assert!(!policy.network_policy.allow_public_egress);
            assert!(!policy.network_policy.allow_private_network_client_server);
            assert!(!policy.network_policy.allow_host_loopback);
        } else {
            assert!(authorization.is_err());
        }
    }

    #[tokio::test]
    async fn workspace_write_hook_policy_respects_tool_read_only_and_path_roots() {
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
            .capture_lifecycle_hook_process_start_authorization()
            .await
            .unwrap();
        let permit = authorization
            .acquire(LifecycleHookProcessStartRequest {
                working_directory: workspace.clone(),
                runtime_read_roots: Vec::new(),
            })
            .await
            .unwrap();
        let policy = permit.sandboxed_process_policy().unwrap();
        assert!(!policy.readwrite_paths.contains(&workspace));
        assert!(policy.readonly_paths.contains(&workspace));

        drop(permit);
        {
            let mut config = engine.config.try_write().unwrap();
            config.tool_settings.remove("lifecycle_hook");
            config.global_file_roots = vec![allowed_subdirectory];
        }
        let authorization = engine
            .capture_lifecycle_hook_process_start_authorization()
            .await
            .unwrap();
        let error = match authorization
            .acquire(LifecycleHookProcessStartRequest {
                working_directory: workspace,
                runtime_read_roots: Vec::new(),
            })
            .await
        {
            Ok(_) => panic!("workspace root outside the configured path scope must fail closed"),
            Err(error) => error,
        };
        assert!(error.contains("outside the compiled MXC filesystem policy"));
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
