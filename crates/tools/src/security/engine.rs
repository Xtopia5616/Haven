use super::contract::{
    AuthorizationCheck, AuthorizationDecision, AuthorizationExecutionBinding,
    AuthorizationExecutionProof, AuthorizationPort, AuthorizationReasonCode, AuthorizationRequest,
    ConfirmationReceipt, ExecutionAuthorizationKind, NetworkPolicyDecision,
};
#[cfg(test)]
use super::execution::execution_policy_denied;
use super::execution::{
    ExecutionPolicyAuthorization, LifecycleHookProcessStartAuthorization,
    McpStdioProcessStartAuthorization,
};
use super::filesystem::{
    FileReadPolicy, FileWritePolicy, is_unc_or_device_path, open_directory_roots,
    open_filesystem_paths, path_is_allowed, path_is_within, resolve_path_without_reparse,
};
#[cfg(test)]
use super::prompt::{LOCAL_TOOL_SECURITY_MATRIX, LocalToolSecurityCase};
use crate::execution_workspace::{ExecutionWorkspace, ExecutionWorkspaceSnapshot};
use crate::{
    ConfirmationRequirement, NetworkAccess, OperationEffect, OperationPolicy, ToolExecutionContext,
};
#[cfg(test)]
use crate::{
    DataSensitivity, StructuredToolError, ToolConcurrency, ToolErrorClass, ToolRetryability,
};
use haven_common::config::{
    FileResourcePolicy, FileWriteMode, MxcIsolationMode, NetworkAccessBoundary,
    NetworkResourcePolicy, NetworkResourcePolicySnapshot, ResourcePolicy, SecurityConfig,
    StoredPermission, ToolConfig,
};
use haven_common::types::{
    CapabilityScope, ExecutionMode, NetworkPolicy, PermissionEffect, PermissionResourceScope,
    PermissionResourceTarget, PermissionScope, RegistryAccess, RiskLevel, SandboxMode,
    is_canonical_id,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Allow and deny grants keyed by capability, each with resource boundaries.
type ScopedGrantMap = HashMap<CapabilityScope, Vec<PermissionResourceScope>>;

// Long-lived MCP transports must stop when their connection authority is removed.
fn mcp_connection_grants(grants: &ScopedGrantMap) -> ScopedGrantMap {
    grants
        .iter()
        .filter_map(|(key, scopes)| {
            let key_text = key.as_str();
            let is_mcp = key_text == "mcp"
                || key_text.starts_with("mcp.")
                || key_text == "mcp_stdio"
                || key_text.starts_with("mcp_stdio.");
            let connection_scopes = scopes
                .iter()
                .filter(|scope| {
                    matches!(
                        scope,
                        PermissionResourceScope::ProcessNetwork { .. }
                            | PermissionResourceScope::NetworkDestination { .. }
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            (is_mcp && !connection_scopes.is_empty()).then(|| (key.clone(), connection_scopes))
        })
        .collect()
}

fn mcp_connection_authority_removed(previous: &ScopedGrantMap, current: &ScopedGrantMap) -> bool {
    mcp_connection_grants(previous).iter().any(|(key, scopes)| {
        scopes
            .iter()
            .any(|scope| !current.get(key).is_some_and(|new| new.contains(scope)))
    })
}

/// Per-session allow/deny maps keyed by typed capability scope.
#[derive(Clone, Default, PartialEq, Eq)]
struct SessionGrants {
    allow: ScopedGrantMap,
    deny: ScopedGrantMap,
}

/// Combined safety config under a single RwLock so `check` reads atomically.
#[derive(Clone)]
pub(super) struct SafetyConfig {
    execution_mode: ExecutionMode,
    session_execution_modes: HashMap<String, ExecutionMode>,
    session_execution_mode_revisions: HashMap<String, u64>,
    pub(super) sandbox_mode: SandboxMode,
    pub(super) mxc_isolation_mode: MxcIsolationMode,
    writable_roots: Vec<PathBuf>,
    pub(super) global_file_roots: Vec<PathBuf>,
    pub(super) global_read_only_roots: Vec<PathBuf>,
    pub(super) global_denied_paths: Vec<PathBuf>,
    pub(super) shell_runtime_read_roots: Vec<PathBuf>,
    pub(super) network: NetworkResourcePolicy,
    /// Revision for the authoritative global network resource policy only.
    network_policy_revision: u64,
    pub(super) execution_workspace: ExecutionWorkspace,
    /// Stable working directory bindings restored from Agent session events.
    session_workspaces: HashMap<String, ExecutionWorkspace>,
    /// Monotonic process-local revision. Configuration/policy changes
    /// invalidate outstanding receipts; the grant attached to the decision
    /// that created a receipt is intentionally applied after final execution.
    pub(super) policy_revision: u64,
    /// Boundary revision for manager-owned opaque MCP stdio processes.
    /// Permission and trust changes do not affect an already-running server.
    pub(super) process_boundary_revision: u64,
    /// Permanent (Always) grants from `SecurityConfig.permissions`.
    permanent: SessionGrants,
    /// Session-scoped grants keyed by session id.
    session_grants: HashMap<String, SessionGrants>,
    /// Compiled tool settings for capability/resource enforcement.
    pub(super) tool_settings: HashMap<String, ToolConfig>,
}

impl SafetyConfig {
    pub(super) fn execution_mode_for(&self, session_id: Option<&str>) -> ExecutionMode {
        session_id
            .and_then(|session_id| self.session_execution_modes.get(session_id))
            .copied()
            .unwrap_or(self.execution_mode)
    }

    pub(super) fn execution_mode_revision_for(&self, session_id: Option<&str>) -> u64 {
        session_id
            .and_then(|session_id| self.session_execution_mode_revisions.get(session_id))
            .copied()
            .unwrap_or_default()
    }

    pub(super) fn execution_workspace_for(&self, session_id: Option<&str>) -> ExecutionWorkspace {
        session_id
            .and_then(|session_id| self.session_workspaces.get(session_id))
            .cloned()
            .unwrap_or_else(|| self.execution_workspace.clone())
    }

    pub(super) fn workspace_write_directories(
        &self,
        session_id: Option<&str>,
    ) -> Result<Vec<haven_platform::filesystem::DirectoryCapability>, String> {
        let directories = if self.writable_roots.is_empty() {
            self.execution_workspace_for(session_id)
                .directory()
                .into_iter()
                .collect()
        } else {
            open_directory_roots(&self.writable_roots, "security.writable_roots")?
        };
        if directories.is_empty() {
            return Err("Workspace Write has no execution root for MXC".into());
        }
        Ok(directories)
    }

    #[cfg(test)]
    pub(super) fn configure_test_workspace_write(
        &mut self,
        root: &Path,
        network_policy: NetworkPolicy,
    ) -> anyhow::Result<()> {
        self.sandbox_mode = SandboxMode::WorkspaceWrite;
        self.writable_roots = vec![root.to_path_buf()];
        self.execution_workspace = ExecutionWorkspace::open_root(root, 1)?;
        self.network.access = network_policy.into();
        Ok(())
    }
}

/// Central authorization engine for every foreground or background tool call.
///
/// The engine owns policy evaluation; callers never decide based on a
/// frontend-provided `confirmed` flag. Explicit resource grants precede default
/// scopes and capability refusals; structural boundaries remain mandatory.
/// Policy revisions invalidate stale proofs;
/// process revocation follows global, tool-setting or session-trust scope.
pub struct AuthorizationEngine {
    pub(super) permission_slots: tokio::sync::Semaphore,
    pub(super) permission_interaction: RwLock<Option<Arc<dyn super::PermissionInteractionPort>>>,
    pub(super) config: Arc<RwLock<SafetyConfig>>,
    pub(super) process_revocations: tokio::sync::watch::Sender<Arc<()>>,
    pub(super) process_boundary_changes: tokio::sync::watch::Sender<Arc<()>>,
    mcp_permission_changes: tokio::sync::watch::Sender<Arc<()>>,
    tool_processes: Mutex<HashMap<String, Vec<Weak<CancellationToken>>>>,
    session_processes: Mutex<HashMap<String, Vec<Weak<CancellationToken>>>>,
}

impl AuthorizationEngine {
    pub fn new() -> Self {
        Self::new_with_workspace(ExecutionWorkspace::discover())
    }

    fn new_with_workspace(execution_workspace: ExecutionWorkspace) -> Self {
        let (process_revocations, _process_revocation_receiver) =
            tokio::sync::watch::channel(Arc::new(()));
        let (process_boundary_changes, _process_boundary_change_receiver) =
            tokio::sync::watch::channel(Arc::new(()));
        let (mcp_permission_changes, _) = tokio::sync::watch::channel(Arc::new(()));
        Self {
            mcp_permission_changes,
            permission_slots: tokio::sync::Semaphore::new(32),
            permission_interaction: RwLock::new(None),
            config: Arc::new(RwLock::new(SafetyConfig {
                execution_mode: ExecutionMode::Collaborative,
                session_execution_modes: HashMap::new(),
                session_execution_mode_revisions: HashMap::new(),
                sandbox_mode: SandboxMode::WorkspaceWrite,
                mxc_isolation_mode: MxcIsolationMode::Strict,
                writable_roots: Vec::new(),
                global_file_roots: Vec::new(),
                global_read_only_roots: Vec::new(),
                global_denied_paths: Vec::new(),
                shell_runtime_read_roots: Vec::new(),
                network: NetworkResourcePolicy {
                    access: NetworkAccessBoundary::Restricted,
                },
                network_policy_revision: 0,
                execution_workspace,
                session_workspaces: HashMap::new(),
                policy_revision: 0,
                process_boundary_revision: 0,
                permanent: SessionGrants::default(),
                session_grants: HashMap::new(),
                tool_settings: HashMap::new(),
            })),
            process_revocations,
            process_boundary_changes,
            tool_processes: Mutex::new(HashMap::new()),
            session_processes: Mutex::new(HashMap::new()),
        }
    }

    fn bump_global_policy_revision(&self, cfg: &mut SafetyConfig) {
        advance_policy_revision(cfg);
        self.process_revocations.send_replace(Arc::new(()));
    }

    fn bump_process_boundary_revision(&self, cfg: &mut SafetyConfig) {
        cfg.process_boundary_revision = cfg.process_boundary_revision.saturating_add(1);
        self.process_boundary_changes.send_replace(Arc::new(()));
    }

    /// Observe only the boundary changes that require manager-owned opaque
    /// MCP processes to stop; ordinary permission and trust changes are
    /// handled by fresh authorization of each MCP tool call.
    pub(crate) fn subscribe_mcp_permission_changes(&self) -> tokio::sync::watch::Receiver<Arc<()>> {
        self.mcp_permission_changes.subscribe()
    }

    pub(crate) fn subscribe_stdio_process_changes(&self) -> tokio::sync::watch::Receiver<Arc<()>> {
        self.process_boundary_changes.subscribe()
    }

    pub(super) fn register_tool_settings_cancellation(
        &self,
        tool_name: &str,
    ) -> Arc<CancellationToken> {
        let cancellation = Arc::new(CancellationToken::new());
        let mut tool_processes = self
            .tool_processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tool_processes.retain(|_, processes| {
            processes.retain(|process| process.strong_count() > 0);
            !processes.is_empty()
        });
        for name in tool_setting_names(tool_name) {
            let processes = tool_processes.entry(name.to_string()).or_default();
            processes.push(Arc::downgrade(&cancellation));
        }
        cancellation
    }

    pub(super) fn register_session_trust_cancellation(
        &self,
        session_id: Option<&str>,
    ) -> Arc<CancellationToken> {
        let cancellation = Arc::new(CancellationToken::new());
        let Some(session_id) = session_id else {
            return cancellation;
        };
        let mut session_processes = self
            .session_processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        session_processes.retain(|_, processes| {
            processes.retain(|process| process.strong_count() > 0);
            !processes.is_empty()
        });
        session_processes
            .entry(session_id.to_string())
            .or_default()
            .push(Arc::downgrade(&cancellation));
        cancellation
    }

    fn cancel_tool_settings_processes(&self, changed_tool_names: &HashSet<String>) {
        if changed_tool_names.is_empty() {
            return;
        }
        let mut tool_processes = self
            .tool_processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for name in changed_tool_names {
            let Some(processes) = tool_processes.get_mut(name) else {
                continue;
            };
            processes.retain(|process| {
                let Some(process) = process.upgrade() else {
                    return false;
                };
                process.cancel();
                true
            });
        }
        tool_processes.retain(|_, processes| !processes.is_empty());
    }

    fn cancel_session_processes(&self, session_id: &str) {
        let mut session_processes = self
            .session_processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(processes) = session_processes.remove(session_id) {
            for process in processes {
                if let Some(process) = process.upgrade() {
                    process.cancel();
                }
            }
        }
    }

    pub(crate) async fn capture_mcp_stdio_process_start_authorization(
        self: &Arc<Self>,
    ) -> Option<Arc<McpStdioProcessStartAuthorization>> {
        let config = self.config.read().await;
        Some(Arc::new(McpStdioProcessStartAuthorization {
            authorization: Arc::clone(self),
            process_boundary_revision: config.process_boundary_revision,
            execution_workspace_generation: config.execution_workspace.generation(),
            process_boundary_signal: self.process_boundary_changes.borrow().clone(),
        }))
    }

    /// Capture the current process and network boundary for user-configured
    /// lifecycle commands. Their explicit configuration is the user consent;
    /// the runner still revalidates this snapshot immediately before spawn.
    pub(crate) async fn capture_lifecycle_hook_process_start_authorization(
        self: &Arc<Self>,
        session_id: Option<&str>,
    ) -> Result<LifecycleHookProcessStartAuthorization, String> {
        let config = self.config.read().await;
        let execution_workspace = config.execution_workspace_for(session_id);
        Ok(LifecycleHookProcessStartAuthorization::new(
            Arc::clone(self),
            super::execution::LifecycleHookSessionContext {
                session_id: session_id.map(str::to_owned),
                trust_cancellation: self.register_session_trust_cancellation(session_id),
            },
            config.policy_revision,
            config.process_boundary_revision,
            execution_workspace.generation(),
            self.process_revocations.borrow().clone(),
            self.process_boundary_changes.borrow().clone(),
        ))
    }

    pub(crate) async fn execution_workspace(&self) -> ExecutionWorkspace {
        self.config.read().await.execution_workspace.clone()
    }

    pub(crate) async fn execution_workspace_for_session(
        &self,
        session_id: &str,
    ) -> ExecutionWorkspace {
        self.config
            .read()
            .await
            .execution_workspace_for(Some(session_id))
    }

    pub(crate) async fn sandbox_mode(&self) -> SandboxMode {
        self.config.read().await.sandbox_mode
    }

    pub(crate) async fn lifecycle_hook_resource_policy(&self) -> ResourcePolicy {
        let config = self.config.read().await;
        ResourcePolicy {
            files: FileResourcePolicy {
                write_mode: match config.sandbox_mode {
                    SandboxMode::ReadOnly => FileWriteMode::ReadOnly,
                    SandboxMode::WorkspaceWrite if !config.writable_roots.is_empty() => {
                        FileWriteMode::Roots
                    }
                    SandboxMode::WorkspaceWrite => FileWriteMode::Workspace,
                    SandboxMode::FullAccess => FileWriteMode::Unrestricted,
                },
                write_roots: config
                    .writable_roots
                    .iter()
                    .map(|root| root.to_string_lossy().into_owned())
                    .collect(),
                path_roots: config
                    .global_file_roots
                    .iter()
                    .map(|root| root.to_string_lossy().into_owned())
                    .collect(),
                read_only_roots: config
                    .global_read_only_roots
                    .iter()
                    .map(|root| root.to_string_lossy().into_owned())
                    .collect(),
                denied_paths: config
                    .global_denied_paths
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
            },
            network: config.network.clone(),
        }
    }

    pub async fn execution_workspace_snapshot(&self) -> ExecutionWorkspaceSnapshot {
        self.config.read().await.execution_workspace.snapshot()
    }

    pub async fn session_execution_workspace_snapshot(
        &self,
        session_id: &str,
    ) -> ExecutionWorkspaceSnapshot {
        self.config
            .read()
            .await
            .execution_workspace_for(Some(session_id))
            .snapshot()
    }

    /// Bind a session to a workspace value. The first binding deliberately
    /// copies the current global default even when its path is equal, so later
    /// default changes cannot silently move an existing session.
    pub async fn bind_session_execution_workspace(
        &self,
        session_id: &str,
        root: Option<&Path>,
    ) -> Result<ExecutionWorkspaceSnapshot, String> {
        let mut cfg = self.config.write().await;
        let current = cfg.execution_workspace_for(Some(session_id));
        let candidate = root
            .map(|root| {
                ExecutionWorkspace::open_root(root, current.generation()).unwrap_or_else(|_| {
                    ExecutionWorkspace::unavailable_for(root, current.generation())
                })
            })
            .unwrap_or_else(ExecutionWorkspace::unavailable);
        if cfg.session_workspaces.contains_key(session_id) && current.same_root(&candidate) {
            return Ok(current.snapshot());
        }

        let generation = if cfg.session_workspaces.contains_key(session_id) {
            current
                .generation()
                .checked_add(1)
                .ok_or_else(|| "session workspace generation is exhausted".to_string())?
        } else {
            current.generation()
        };
        let workspace = match root {
            Some(root) => ExecutionWorkspace::open_root(root, generation)
                .unwrap_or_else(|_| ExecutionWorkspace::unavailable_for(root, generation)),
            None => ExecutionWorkspace::unavailable().with_generation(generation),
        };
        cfg.session_workspaces
            .insert(session_id.to_string(), workspace.clone());
        self.cancel_session_processes(session_id);
        Ok(workspace.snapshot())
    }

    pub async fn forget_session_execution_workspaces(&self, session_ids: &[String]) {
        let mut cfg = self.config.write().await;
        for session_id in session_ids {
            cfg.session_workspaces.remove(session_id);
            cfg.session_execution_modes.remove(session_id);
            cfg.session_execution_mode_revisions.remove(session_id);
            self.cancel_session_processes(session_id);
        }
    }

    /// Bind one session to its durable execution preset. A session update
    /// invalidates existing proofs and stops managed work under the old mode.
    pub async fn bind_session_execution_mode(&self, session_id: &str, mode: ExecutionMode) {
        let mut cfg = self.config.write().await;
        if cfg.session_execution_modes.get(session_id) == Some(&mode) {
            return;
        }
        cfg.session_execution_modes
            .insert(session_id.to_owned(), mode);
        let revision = cfg
            .session_execution_mode_revisions
            .entry(session_id.to_owned())
            .or_default();
        *revision = revision.saturating_add(1);
        self.cancel_session_processes(session_id);
    }

    pub async fn session_execution_mode(&self, session_id: &str) -> ExecutionMode {
        self.config
            .read()
            .await
            .execution_mode_for(Some(session_id))
    }

    pub(crate) async fn switch_execution_workspace(
        &self,
        root: &Path,
    ) -> Result<ExecutionWorkspaceSnapshot, String> {
        let candidate = ExecutionWorkspace::open_root(root, 1)
            .map_err(|_| "workspace root is not an available local directory".to_string())?;
        let mut cfg = self.config.write().await;
        if cfg.execution_workspace.same_root(&candidate) {
            return Ok(cfg.execution_workspace.snapshot());
        }
        let generation = cfg
            .execution_workspace
            .generation()
            .checked_add(1)
            .ok_or_else(|| "workspace generation is exhausted".to_string())?;
        cfg.execution_workspace = candidate.with_generation(generation);
        self.bump_process_boundary_revision(&mut cfg);
        Ok(cfg.execution_workspace.snapshot())
    }

    /// Replace the policy and permanent rules atomically. Session grants are
    /// process-local; workspace bindings remain session-owned in the Agent
    /// event stream and are restored when each session is installed.
    pub async fn apply_security(&self, security: &SecurityConfig) -> Result<(), String> {
        let candidate = match security.execution_root.as_deref() {
            Some(root) => ExecutionWorkspace::open_root(root, 1)
                .map_err(|_| "configured execution workspace is unavailable".to_string()),
            None => Ok(ExecutionWorkspace::discover()),
        };
        let mut cfg = self.config.write().await;
        let current_workspace = cfg.execution_workspace.clone();
        let (workspace, workspace_error) = match candidate {
            Ok(candidate) if current_workspace.same_root(&candidate) => {
                (current_workspace.clone(), None)
            }
            Err(_error) if current_workspace.root().is_none() => {
                // Keep an already-unavailable workspace unavailable without
                // blocking unrelated security settings from being applied.
                (current_workspace.clone(), None)
            }
            candidate => {
                if let Some(generation) = current_workspace.generation().checked_add(1) {
                    match candidate {
                        Ok(candidate) => (candidate.with_generation(generation), None),
                        Err(error) => (
                            ExecutionWorkspace::unavailable().with_generation(generation),
                            Some(error),
                        ),
                    }
                } else {
                    (
                        ExecutionWorkspace::unavailable()
                            .with_generation(current_workspace.generation()),
                        Some("execution workspace generation is exhausted".to_string()),
                    )
                }
            }
        };
        let writable_roots = security
            .resource_policy
            .files
            .write_roots
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let global_file_roots = security
            .resource_policy
            .files
            .path_roots
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let global_read_only_roots = security
            .resource_policy
            .files
            .read_only_roots
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let global_denied_paths = security
            .resource_policy
            .files
            .denied_paths
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let shell_runtime_read_roots = security
            .shell_runtime_read_roots
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let writable_roots_validation = open_directory_roots(
            &writable_roots,
            "security.resource_policy.files.write_roots",
        )
        .map(|_| ());
        let global_file_roots_validation = open_directory_roots(
            &global_file_roots,
            "security.resource_policy.files.path_roots",
        )
        .map(|_| ());
        let global_read_only_roots_validation = open_filesystem_paths(
            &global_read_only_roots,
            "security.resource_policy.files.read_only_roots",
        )
        .map(|_| ());
        let global_denied_paths_validation = validate_denied_paths(
            &global_denied_paths,
            "security.resource_policy.files.denied_paths",
        );
        let shell_runtime_read_roots_validation = open_directory_roots(
            &shell_runtime_read_roots,
            "security.shell_runtime_read_roots",
        )
        .map(|_| ());
        let network = security.resource_policy.network.clone();
        let sandbox_mode = security.resource_policy.sandbox_mode();
        let network_policy_changed = cfg.network != network;
        let process_boundaries_changed = cfg.sandbox_mode != sandbox_mode
            || !cfg.execution_workspace.same_root(&workspace)
            || cfg.mxc_isolation_mode != security.mxc_isolation_mode
            || cfg.writable_roots != writable_roots
            || cfg.global_file_roots != global_file_roots
            || cfg.global_read_only_roots != global_read_only_roots
            || cfg.global_denied_paths != global_denied_paths
            || cfg.shell_runtime_read_roots != shell_runtime_read_roots
            || cfg.network != network;
        let mut permanent = SessionGrants::default();
        for permission in &security.permissions {
            match CapabilityScope::try_new(&permission.key) {
                Ok(capability) => {
                    let scopes = if permission.effect == PermissionEffect::Allow
                        && permission.resource_scope.is_empty()
                    {
                        tracing::warn!(
                            capability = %capability,
                            "ignoring unscoped persistent allow; reauthorize with a resource scope"
                        );
                        continue;
                    } else if permission.effect == PermissionEffect::Deny {
                        Vec::new()
                    } else {
                        permission.resource_scope.clone()
                    };
                    let destination = match permission.effect {
                        PermissionEffect::Allow => &mut permanent.allow,
                        PermissionEffect::Deny => &mut permanent.deny,
                    };
                    merge_resource_scopes(destination.entry(capability).or_default(), &scopes);
                }
                Err(_error) => {
                    // Keep runtime policy fail-closed if a caller constructs
                    // SecurityConfig in memory or a future wire boundary bypasses
                    // the loader.
                    tracing::warn!("ignoring invalid persisted capability rule");
                }
            }
        }
        let policy_changed = cfg.execution_mode != security.execution_mode
            || cfg.sandbox_mode != sandbox_mode
            || cfg.mxc_isolation_mode != security.mxc_isolation_mode
            || cfg.writable_roots != writable_roots
            || cfg.global_file_roots != global_file_roots
            || cfg.global_read_only_roots != global_read_only_roots
            || cfg.global_denied_paths != global_denied_paths
            || cfg.shell_runtime_read_roots != shell_runtime_read_roots
            || cfg.network != network
            || cfg.permanent != permanent;
        cfg.execution_workspace = workspace;
        cfg.execution_mode = security.execution_mode;
        cfg.sandbox_mode = sandbox_mode;
        cfg.mxc_isolation_mode = security.mxc_isolation_mode;
        cfg.writable_roots = writable_roots;
        cfg.global_file_roots = global_file_roots;
        cfg.global_read_only_roots = global_read_only_roots;
        cfg.global_denied_paths = global_denied_paths;
        cfg.shell_runtime_read_roots = shell_runtime_read_roots;
        cfg.network = network;
        if mcp_connection_authority_removed(&cfg.permanent.allow, &permanent.allow) {
            self.mcp_permission_changes.send_replace(Arc::new(()));
        }
        cfg.permanent = permanent;
        if network_policy_changed {
            cfg.network_policy_revision = cfg.network_policy_revision.saturating_add(1);
        }
        if policy_changed {
            cfg.session_grants.clear();
        }
        if process_boundaries_changed {
            self.bump_process_boundary_revision(&mut cfg);
        }
        if policy_changed {
            self.bump_global_policy_revision(&mut cfg);
        }
        if let Err(error) = writable_roots_validation {
            tracing::warn!(error, "writable roots configuration could not be applied");
            return Err(error);
        }
        if let Err(error) = global_file_roots_validation {
            tracing::warn!(error, "global file resource roots could not be applied");
            return Err(error);
        }
        if let Err(error) = global_read_only_roots_validation {
            tracing::warn!(error, "global read-only file roots could not be applied");
            return Err(error);
        }
        if let Err(error) = global_denied_paths_validation {
            tracing::warn!(error, "global denied file paths could not be applied");
            return Err(error);
        }
        if let Err(error) = shell_runtime_read_roots_validation {
            tracing::warn!(error, "Shell runtime read roots could not be applied");
            return Err(error);
        }
        if let Some(error) = workspace_error {
            tracing::warn!(
                error,
                "execution workspace configuration could not be applied"
            );
            return Err(error);
        }
        Ok(())
    }

    /// Update the policy profile. Clears session grants.
    #[cfg(test)]
    pub(crate) async fn set_execution_mode(&self, mode: ExecutionMode) {
        let mut cfg = self.config.write().await;
        cfg.execution_mode = mode;
        cfg.session_grants.clear();
        self.bump_global_policy_revision(&mut cfg);
    }

    /// Update the technical boundaries without touching permanent grants.
    /// This is useful for a settings surface that owns the sandbox controls
    /// but must not accidentally replace the independently managed rule list.
    #[cfg(test)]
    pub(crate) async fn set_boundaries(
        &self,
        sandbox_mode: SandboxMode,
        writable_roots: Vec<PathBuf>,
        network_policy: NetworkPolicy,
    ) {
        let mut cfg = self.config.write().await;
        let mut network = cfg.network.clone();
        network.access = network_policy.into();
        let network_policy_changed = cfg.network != network;
        let process_boundaries_changed = cfg.sandbox_mode != sandbox_mode
            || cfg.writable_roots != writable_roots
            || network_policy_changed;
        cfg.sandbox_mode = sandbox_mode;
        cfg.writable_roots = writable_roots;
        cfg.network = network;
        if network_policy_changed {
            cfg.network_policy_revision = cfg.network_policy_revision.saturating_add(1);
        }
        cfg.session_grants.clear();
        if process_boundaries_changed {
            self.bump_process_boundary_revision(&mut cfg);
        }
        self.bump_global_policy_revision(&mut cfg);
    }

    /// Capture the current global network policy together with its owner
    /// revision for consumers such as MCP connection management.
    pub async fn network_resource_policy_snapshot(&self) -> NetworkResourcePolicySnapshot {
        let cfg = self.config.read().await;
        NetworkResourcePolicySnapshot::new(cfg.network_policy_revision, cfg.network.clone())
    }

    /// Refresh the live tool_settings mirror used by path/op/risk overrides.
    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        let mut cfg = self.config.write().await;
        let changed_tool_names = cfg
            .tool_settings
            .keys()
            .chain(settings.keys())
            .filter(|name| cfg.tool_settings.get(*name) != settings.get(*name))
            .cloned()
            .collect::<HashSet<_>>();
        cfg.tool_settings = settings;
        if changed_tool_names
            .iter()
            .any(|name| matches!(name.as_str(), "mcp" | "mcp_stdio"))
        {
            self.bump_process_boundary_revision(&mut cfg);
        }
        advance_policy_revision(&mut cfg);
        self.cancel_tool_settings_processes(&changed_tool_names);
    }

    /// Return a compact, non-sensitive policy summary for the model context.
    /// Permission keys and paths stay private; the model only needs to know
    /// which policy mode is active and whether explicit grants/blocks exist.
    pub async fn prompt_summary(&self) -> String {
        let cfg = self.config.read().await;
        let always_allowed = cfg.permanent.allow.len();
        let always_denied = cfg.permanent.deny.len();
        let disabled_tools = cfg.tool_settings.values().filter(|c| !c.enabled).count();
        let disabled_operations = cfg
            .tool_settings
            .values()
            .map(|c| c.disabled_operations.len())
            .sum::<usize>();
        let file_write_mode = match cfg.sandbox_mode {
            SandboxMode::ReadOnly => FileWriteMode::ReadOnly,
            SandboxMode::WorkspaceWrite if !cfg.writable_roots.is_empty() => FileWriteMode::Roots,
            SandboxMode::WorkspaceWrite => FileWriteMode::Workspace,
            SandboxMode::FullAccess => FileWriteMode::Unrestricted,
        };
        let network_access = cfg.network.access;
        format!(
            "mode={:?}; resource_policy.files.write_mode={file_write_mode:?}; resource_policy.network.access={network_access:?}; always_allowed={always_allowed}; always_denied={always_denied}; disabled_tools={disabled_tools}; disabled_operations={disabled_operations}; operation contracts decide read-only access; higher-risk operations may require confirmation",
            cfg.execution_mode
        )
    }

    /// Effective risk after optional `tool_settings.risk_override`.
    pub async fn effective_risk(&self, tool_name: &str, reported: RiskLevel) -> RiskLevel {
        let cfg = self.config.read().await;
        effective_risk_from(&cfg, tool_name, reported)
    }

    pub(crate) async fn file_read_policy(&self, tool_name: &str) -> FileReadPolicy {
        let cfg = self.config.read().await;
        file_read_policy_for_config(&cfg, tool_name)
    }

    pub(crate) async fn execution_context_for_proof(
        self: &Arc<Self>,
        request: &AuthorizationRequest,
        proof: &AuthorizationExecutionProof,
    ) -> Result<ToolExecutionContext, String> {
        self.verify_execution_proof(request, proof).await?;
        let cfg = self.config.read().await;
        if cfg.policy_revision != proof.binding.policy_revision {
            return Err("execution policy changed before capability capture".into());
        }
        let execution_workspace = cfg.execution_workspace_for(request.session_id.as_deref());
        if execution_workspace.generation() != proof.binding.workspace_generation {
            return Err("execution workspace changed before capability capture".into());
        }

        let file_write_policy = if request.tool_name.starts_with("files.")
            && matches!(request.policy.effect, OperationEffect::WorkspaceWrite)
        {
            let mut root_constraints = Vec::new();
            let mut protection_roots = Vec::new();
            let mut readonly_paths = cfg.global_read_only_roots.clone();
            let mut denied_paths = cfg.global_denied_paths.clone();
            let resource_boundary_extension = proof_resource_boundary_extension(proof);
            let (extension_roots, extension_targets) = if resource_boundary_extension {
                resource_extension_write_roots(request, &execution_workspace)?
            } else {
                (Vec::new(), Vec::new())
            };
            let ambient = match cfg.sandbox_mode {
                SandboxMode::ReadOnly if !resource_boundary_extension => {
                    return Err("file mutation requires permission".into());
                }
                SandboxMode::ReadOnly => false,
                SandboxMode::WorkspaceWrite if resource_boundary_extension => false,
                SandboxMode::WorkspaceWrite if cfg.writable_roots.is_empty() => {
                    let roots = execution_workspace
                        .directory()
                        .into_iter()
                        .collect::<Vec<_>>();
                    protection_roots
                        .extend(roots.iter().map(|directory| directory.root().to_path_buf()));
                    root_constraints.push(roots);
                    false
                }
                SandboxMode::WorkspaceWrite => {
                    protection_roots.extend(cfg.writable_roots.iter().cloned());
                    root_constraints.push(open_directory_roots(
                        &cfg.writable_roots,
                        "security.writable_roots",
                    )?);
                    false
                }
                SandboxMode::FullAccess => {
                    protection_roots.extend(execution_workspace.root().map(Path::to_path_buf));
                    true
                }
            };

            if !resource_boundary_extension && !cfg.global_file_roots.is_empty() {
                protection_roots.extend(cfg.global_file_roots.iter().cloned());
                root_constraints.push(open_directory_roots(
                    &cfg.global_file_roots,
                    "security.resource_policy.files.path_roots",
                )?);
            }

            for setting in tool_setting_names(&request.tool_name)
                .filter_map(|name| cfg.tool_settings.get(name))
            {
                match setting.resource_policy.files.write_mode {
                    _ if resource_boundary_extension => {}
                    FileWriteMode::ReadOnly => {
                        return Err("file mutation requires permission".into());
                    }
                    FileWriteMode::Workspace => {
                        let root = execution_workspace
                            .root()
                            .ok_or_else(|| {
                                "tool workspace resource policy has no execution root".to_owned()
                            })?
                            .to_path_buf();
                        protection_roots.push(root.clone());
                        root_constraints.push(open_directory_roots(
                            &[root],
                            "tool resource_policy.files.workspace",
                        )?);
                    }
                    FileWriteMode::Roots => {
                        let roots = setting
                            .resource_policy
                            .files
                            .write_roots
                            .iter()
                            .map(PathBuf::from)
                            .collect::<Vec<_>>();
                        protection_roots.extend(roots.iter().cloned());
                        root_constraints.push(open_directory_roots(
                            &roots,
                            "tool resource_policy.files.write_roots",
                        )?);
                    }
                    FileWriteMode::Inherit | FileWriteMode::Unrestricted => {}
                }
                if !resource_boundary_extension
                    && !setting.resource_policy.files.path_roots.is_empty()
                {
                    let roots = setting
                        .resource_policy
                        .files
                        .path_roots
                        .iter()
                        .map(PathBuf::from)
                        .collect::<Vec<_>>();
                    protection_roots.extend(roots.iter().cloned());
                    root_constraints.push(open_directory_roots(
                        &roots,
                        "tool resource_policy.files.path_roots",
                    )?);
                }
                readonly_paths.extend(
                    setting
                        .resource_policy
                        .files
                        .read_only_roots
                        .iter()
                        .map(PathBuf::from),
                );
                denied_paths.extend(
                    setting
                        .resource_policy
                        .files
                        .denied_paths
                        .iter()
                        .map(PathBuf::from),
                );
            }

            protection_roots.extend(
                extension_roots
                    .iter()
                    .map(|directory| directory.root().to_path_buf()),
            );
            let readonly_paths = if resource_boundary_extension {
                Vec::new()
            } else {
                open_filesystem_paths(&readonly_paths, "resource_policy.files.read_only_roots")?
            };
            let denied_paths =
                open_filesystem_paths(&denied_paths, "resource_policy.files.denied_paths")?;

            Some(FileWritePolicy {
                root_constraints,
                extension_roots,
                extension_targets,
                readonly_paths,
                denied_paths,
                deny_all: false,
                ambient,
            })
        } else {
            None
        };
        let file_read_policy = request
            .resources
            .iter()
            .any(|resource| {
                matches!(
                    resource,
                    super::DeclaredResource::LocalPath {
                        access: super::LocalPathAccess::Read,
                        ..
                    }
                )
            })
            .then(|| {
                let mut policy = file_read_policy_for_config(&cfg, &request.tool_name);
                if proof_resource_boundary_extension(proof) {
                    policy.allowed_roots.clear();
                    policy.allowed_roots.push(
                        request
                            .resources
                            .iter()
                            .filter_map(|resource| match resource {
                                super::DeclaredResource::LocalPath { path, .. } => {
                                    Some(resolve_relative_sandbox_path(
                                        path.clone(),
                                        &execution_workspace,
                                    ))
                                }
                                _ => None,
                            })
                            .collect(),
                    );
                }
                policy
            });
        let network_destination_extension = request
            .resources
            .iter()
            .find(|resource| matches!(resource, super::DeclaredResource::NetworkDestination { .. }))
            .cloned();
        let network_domain_constraints = network_destination_extension.as_ref().map(|resource| {
            let super::DeclaredResource::NetworkDestination { host, .. } = resource else {
                unreachable!()
            };
            vec![vec![host.clone()]]
        });
        let tool_settings_cancellation =
            self.register_tool_settings_cancellation(&request.tool_name);
        let session_trust_cancellation =
            self.register_session_trust_cancellation(proof.binding.session_id.as_deref());

        Ok(ToolExecutionContext {
            execution_workspace: Some(execution_workspace.clone()),
            authorized_resources: request.resources.clone(),
            file_read_policy,
            file_write_policy,
            network_domain_constraints,
            network_destination_extension,
            execution_authorization: Some(ExecutionPolicyAuthorization {
                authorization: Arc::clone(self),
                tool_name: request.tool_name.clone(),
                policy_revision: proof.binding.policy_revision,
                session_execution_mode_revision: proof.binding.session_execution_mode_revision,
                execution_workspace_generation: execution_workspace.generation(),
                session_id: proof.binding.session_id.clone(),
                execution_boundary: request.policy.execution_boundary,
                resource_boundary_extension: proof_resource_boundary_extension(proof),
                network_authorized: request.policy.network_access == NetworkAccess::Opaque
                    && request.resources.iter().any(|resource| {
                        matches!(resource, super::DeclaredResource::ProcessNetwork { .. })
                    }),
                authorized_resources: request.resources.clone(),
                expires_at: proof.binding.expires_at,
                process_revocation_signal: self.process_revocations.borrow().clone(),
                tool_settings_cancellation,
                session_trust_cancellation,
            }),
            call_session_id: None,
            call_step_id: None,
        })
    }

    /// Evaluate one typed authorization request. The operation policy and
    /// capability scope are produced by the same contract that executes the
    /// operation; the engine never reconstructs authorization from a tool
    /// name or a renderer-provided flag.
    pub async fn authorize_for_execution(
        &self,
        request: &AuthorizationRequest,
    ) -> AuthorizationCheck {
        let session_id = request.session_id.as_deref();
        let tool_name = &request.tool_name;
        let policy = &request.policy;
        let key = request.capability();
        let cfg = self.config.read().await;
        let risk = effective_risk_from(&cfg, tool_name, policy.risk_level);
        let workspace = cfg.execution_workspace_for(session_id);
        let network_resolution =
            resolve_network_policy(&cfg, tool_name, &request.resources, policy);
        let mut network_policy_decision = network_resolution.decision;

        let resource_boundary_extension = match check_policy_boundaries(
            &cfg,
            &workspace,
            tool_name,
            key,
            &request.resources,
            policy,
        ) {
            Ok(()) => false,
            Err(failure) if failure.resource_extension_allowed => {
                if network_resolution.resource_boundary_extension {
                    network_policy_decision = NetworkPolicyDecision::RequiresConfirmation;
                }
                true
            }
            Err(failure) => {
                let decision = if failure.reason_code == AuthorizationReasonCode::NetworkPolicy {
                    network_policy_decision
                } else {
                    NetworkPolicyDecision::Allowed
                };
                return AuthorizationCheck::without_proof(AuthorizationDecision::Blocked {
                    reason: failure.reason,
                    reason_code: failure.reason_code,
                })
                .with_network_policy_decision(decision);
            }
        };

        let execution_mode = cfg.execution_mode_for(session_id);
        let granted = request_has_allow(&cfg, request, &workspace);
        // Explicit authority is resolved before default resource boundaries.
        // Structural errors and disabled operations were checked above.
        let hard_confirmation =
            risk >= RiskLevel::Critical || policy.confirmation == ConfirmationRequirement::Required;
        if granted && !hard_confirmation {
            let mut check = AuthorizationCheck::auto_approved(
                request,
                cfg.policy_revision,
                cfg.execution_mode_revision_for(session_id),
                workspace.generation(),
            );
            check
                .execution_proof
                .as_mut()
                .expect("auto approval has proof")
                .binding
                .resource_boundary_extension = resource_boundary_extension;
            return check.with_network_policy_decision(NetworkPolicyDecision::Allowed);
        }
        if let Some((reason, reason_code)) = request_denial(&cfg, request, &workspace) {
            return AuthorizationCheck::without_proof(AuthorizationDecision::Blocked {
                reason,
                reason_code,
            })
            .with_network_policy_decision(network_policy_decision);
        }
        // Analyze/read-only is the default grant, not a permanent refusal to
        // request additional rights for this exact operation.
        let resource_boundary_extension = resource_boundary_extension
            || (execution_mode == ExecutionMode::Analyze && !policy.is_read_only());
        let needs_prompt = resource_boundary_extension
            || hard_confirmation
            || policy.requires_disclosure_confirmation()
            || (!policy.is_read_only()
                && (risk > RiskLevel::Safe
                    || policy.confirmation != ConfirmationRequirement::None));

        if needs_prompt
            && policy.network_access != NetworkAccess::None
            && network_policy_decision == NetworkPolicyDecision::Allowed
        {
            network_policy_decision = NetworkPolicyDecision::RequiresConfirmation;
        }

        if !needs_prompt {
            return AuthorizationCheck::auto_approved(
                request,
                cfg.policy_revision,
                cfg.execution_mode_revision_for(session_id),
                workspace.generation(),
            )
            .with_network_policy_decision(network_policy_decision);
        }

        let reason_code = if resource_boundary_extension {
            AuthorizationReasonCode::ResourceBoundaryExtension
        } else if risk >= RiskLevel::Critical {
            AuthorizationReasonCode::CriticalOperation
        } else if policy.requires_disclosure_confirmation() {
            AuthorizationReasonCode::SensitiveData
        } else {
            AuthorizationReasonCode::ApprovalPolicy
        };

        let receipt = ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: key.clone(),
            resource_boundary_extension,
            resources: request.resources.clone(),
            canonical_input_hash: canonical_authorization_hash_for_workspace(
                request,
                workspace.generation(),
                cfg.execution_mode_revision_for(session_id),
            ),
            effective_risk: risk,
            policy_revision: cfg.policy_revision,
            expires_at: confirmation_expiry(),
        };
        AuthorizationCheck::without_proof(AuthorizationDecision::RequiresConfirmation {
            capability: key.clone(),
            risk_level: risk,
            receipt,
            reason_code,
        })
        .with_network_policy_decision(network_policy_decision)
    }

    /// Compatibility-free decision helper for callers that do not execute the
    /// operation themselves. Execution callers must retain the proof returned
    /// by `authorize_for_execution` and pass it to the Tools owner.
    pub async fn authorize(&self, request: &AuthorizationRequest) -> AuthorizationDecision {
        self.authorize_for_execution(request).await.decision
    }

    /// Verify a receipt against the exact typed request that created it.
    pub async fn verify_receipt(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<(), String> {
        let session_id = request.session_id.as_deref();
        let tool_name = &request.tool_name;
        let policy = &request.policy;
        let key = request.capability();
        let cfg = self.config.read().await;
        let workspace = cfg.execution_workspace_for(session_id);
        if receipt.capability != *key {
            return Err("confirmation receipt does not match the capability scope".into());
        }
        if receipt.resources != request.resources {
            return Err("confirmation receipt does not match the declared resources".into());
        }
        if receipt.canonical_input_hash
            != canonical_authorization_hash_for_workspace(
                request,
                workspace.generation(),
                cfg.execution_mode_revision_for(session_id),
            )
        {
            return Err("confirmation receipt does not match the tool input".into());
        }
        if receipt.policy_revision != cfg.policy_revision {
            return Err("confirmation receipt was issued under an older policy".into());
        }
        if confirmation_now() >= receipt.expires_at {
            return Err("confirmation receipt has expired".into());
        }

        let risk = effective_risk_from(&cfg, tool_name, policy.risk_level);
        if risk > receipt.effective_risk {
            return Err("the operation risk increased after confirmation".into());
        }
        let mut effective_policy = policy.clone();
        effective_policy.risk_level = risk;
        match check_policy_boundaries(
            &cfg,
            &workspace,
            tool_name,
            key,
            &request.resources,
            &effective_policy,
        ) {
            Ok(())
                if receipt.resource_boundary_extension
                    && !(cfg.execution_mode_for(session_id) == ExecutionMode::Analyze
                        && !policy.is_read_only()) =>
            {
                return Err(
                    "confirmation receipt requested an unnecessary resource extension".into(),
                );
            }
            Ok(()) => {}
            Err(failure)
                if failure.resource_extension_allowed && receipt.resource_boundary_extension => {}
            Err(failure) => return Err(failure.reason),
        }
        if let Some((reason, _)) = request_denial(&cfg, request, &workspace) {
            return Err(reason);
        }
        Ok(())
    }

    /// Issue an opaque execution proof after the trusted interaction owner has
    /// accepted this exact confirmation receipt. Receipts are display/IPC
    /// records and are never executable authority on their own.
    pub async fn confirm_for_execution(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<AuthorizationExecutionProof, String> {
        self.verify_receipt(request, receipt).await?;
        let cfg = self.config.read().await;
        Ok(AuthorizationExecutionProof {
            binding: AuthorizationExecutionBinding {
                resource_boundary_extension: receipt.resource_boundary_extension,
                session_id: request.session_id.clone(),
                tool_name: request.tool_name.clone(),
                capability: request.capability().clone(),
                canonical_input_hash: canonical_authorization_hash(request),
                target_binding: request.target_binding.clone(),
                policy: request.policy.clone(),
                policy_revision: receipt.policy_revision,
                session_execution_mode_revision: cfg
                    .execution_mode_revision_for(request.session_id.as_deref()),
                workspace_generation: cfg
                    .execution_workspace_for(request.session_id.as_deref())
                    .generation(),
                expires_at: execution_proof_expiry().min(receipt.expires_at),
            },
            authorization: ExecutionAuthorizationKind::Confirmed(receipt.clone()),
        })
    }

    /// Revalidate a proof immediately before an operation attempt. Auto
    /// approvals bind the operation contract and current policy revision;
    /// confirmations retain the existing input-bound receipt semantics.
    pub async fn verify_execution_proof(
        &self,
        request: &AuthorizationRequest,
        proof: &AuthorizationExecutionProof,
    ) -> Result<(), String> {
        let binding = &proof.binding;
        if binding.session_id != request.session_id
            || binding.tool_name != request.tool_name
            || binding.capability != *request.capability()
            || binding.canonical_input_hash != canonical_authorization_hash(request)
            || binding.target_binding != request.target_binding
            || binding.policy != request.policy
        {
            return Err("execution proof does not match the operation request".into());
        }
        if confirmation_now() >= binding.expires_at {
            return Err("execution proof has expired".into());
        }
        let cfg = self.config.read().await;
        let workspace = cfg.execution_workspace_for(request.session_id.as_deref());
        if binding.workspace_generation != workspace.generation() {
            return Err("execution proof was issued for an older execution workspace".into());
        }
        if binding.session_execution_mode_revision
            != cfg.execution_mode_revision_for(request.session_id.as_deref())
        {
            return Err("execution proof was issued for an older session execution mode".into());
        }
        if let ExecutionAuthorizationKind::Confirmed(receipt) = &proof.authorization {
            if receipt.policy_revision != binding.policy_revision {
                return Err("execution proof receipt has a mismatched policy revision".into());
            }
            drop(cfg);
            return self.verify_receipt(request, receipt).await;
        }
        if binding.policy_revision != cfg.policy_revision {
            return Err("execution proof was issued under an older policy".into());
        }
        let session_id = request.session_id.as_deref();
        let tool_name = request.tool_name.as_str();
        let key = request.capability();
        let risk = effective_risk_from(&cfg, tool_name, request.policy.risk_level);
        let workspace = cfg.execution_workspace_for(session_id);
        if binding.workspace_generation != workspace.generation() {
            return Err("execution proof was issued for an older execution workspace".into());
        }
        if let Err(failure) = check_policy_boundaries(
            &cfg,
            &workspace,
            tool_name,
            key,
            &request.resources,
            &request.policy,
        ) && (!failure.resource_extension_allowed
            || !binding.resource_boundary_extension
            || !request_has_allow(&cfg, request, &workspace))
        {
            return Err(failure.reason);
        }
        if let Some((reason, _)) = request_denial(&cfg, request, &workspace) {
            return Err(reason);
        }
        if risk >= RiskLevel::Critical
            || request.policy.confirmation == ConfirmationRequirement::Required
        {
            return Err("operation now requires a fresh user confirmation".into());
        }
        Ok(())
    }

    /// Record a grant. `Once` is a no-op (caller already approved this call).
    /// `Always` updates the in-memory permanent map; the app layer must also
    /// persist to `SecurityConfig.permissions`.
    pub async fn grant(
        &self,
        session_id: Option<&str>,
        capability: CapabilityScope,
        effect: PermissionEffect,
        scope: PermissionScope,
        resource_scope: Vec<PermissionResourceScope>,
    ) {
        if matches!(scope, PermissionScope::Once) {
            return;
        }
        let resource_scope = if effect == PermissionEffect::Deny {
            Vec::new()
        } else {
            resource_scope
        };
        if effect == PermissionEffect::Allow && resource_scope.is_empty() {
            tracing::warn!(
                capability = %capability,
                "refusing to install an unscoped persistent allow"
            );
            return;
        }
        let mut cfg = self.config.write().await;
        match scope {
            PermissionScope::Always => {
                match effect {
                    PermissionEffect::Allow => {
                        cfg.permanent.deny.remove(&capability);
                    }
                    PermissionEffect::Deny => {
                        if cfg.permanent.allow.get(&capability).is_some_and(|scopes| {
                            !mcp_connection_grants(&HashMap::from([(
                                capability.clone(),
                                scopes.clone(),
                            )]))
                            .is_empty()
                        }) {
                            self.mcp_permission_changes.send_replace(Arc::new(()));
                        }
                        cfg.permanent.allow.remove(&capability);
                    }
                }
                let destination = match effect {
                    PermissionEffect::Allow => &mut cfg.permanent.allow,
                    PermissionEffect::Deny => &mut cfg.permanent.deny,
                };
                merge_resource_scopes(
                    destination.entry(capability.clone()).or_default(),
                    &resource_scope,
                );
            }
            PermissionScope::Session => {
                let Some(sid) = session_id else {
                    return;
                };
                let entry = cfg.session_grants.entry(sid.to_string()).or_default();
                let (destination, opposite) = match effect {
                    PermissionEffect::Allow => (&mut entry.allow, &mut entry.deny),
                    PermissionEffect::Deny => (&mut entry.deny, &mut entry.allow),
                };
                opposite.remove(&capability);
                merge_resource_scopes(
                    destination.entry(capability.clone()).or_default(),
                    &resource_scope,
                );
            }
            PermissionScope::Once => {}
        }
        if effect == PermissionEffect::Deny {
            match scope {
                PermissionScope::Always => self.bump_global_policy_revision(&mut cfg),
                PermissionScope::Session => {
                    advance_policy_revision(&mut cfg);
                    if let Some(session_id) = session_id {
                        self.cancel_session_processes(session_id);
                    }
                }
                PermissionScope::Once => {}
            }
        }
        // An allow is the user's decision for the receipt that is currently
        // being resolved. It must not invalidate that same receipt between
        // the resolver waking the session and the executor's final check.
        // Revocations and policy/config changes still advance the revision.
    }

    pub async fn resource_scope_for_request(
        &self,
        request: &AuthorizationRequest,
        target: PermissionResourceTarget,
    ) -> Result<Vec<PermissionResourceScope>, String> {
        if request.resources.is_empty() {
            return Err("persistent allow requires typed resource declarations".into());
        }
        let cfg = self.config.read().await;
        let workspace = cfg.execution_workspace_for(request.session_id.as_deref());
        let mut scopes = Vec::with_capacity(request.resources.len());
        for resource in &request.resources {
            let scope = match resource {
                super::DeclaredResource::ProcessNetwork { target_fingerprint } => {
                    PermissionResourceScope::ProcessNetwork {
                        target_fingerprint: target_fingerprint.clone(),
                    }
                }
                super::DeclaredResource::LocalPath { path, access } => {
                    let resolved = resolve_relative_sandbox_path(path.clone(), &workspace);
                    if is_unc_or_device_path(&resolved) {
                        return Err(
                            "persistent path grant does not support UNC or device paths".into()
                        );
                    }
                    let path = resolve_path_without_reparse(&resolved).ok_or_else(|| {
                        "persistent path grant could not safely resolve the resource".to_owned()
                    })?;
                    let path = persisted_local_resource_path(path)?;
                    let (path, recursive) = match target {
                        PermissionResourceTarget::Exact => (path, false),
                        PermissionResourceTarget::Directory => {
                            let resource_is_directory = std::fs::metadata(&path)
                                .map(|metadata| metadata.is_dir())
                                .unwrap_or(false);
                            let root = if resource_is_directory {
                                path
                            } else {
                                path.parent()
                                    .map(Path::to_path_buf)
                                    .ok_or_else(|| "path has no containing directory".to_owned())?
                            };
                            let root = resolve_path_without_reparse(&root)
                                .ok_or_else(|| "directory grant root is unavailable".to_owned())?;
                            let root = persisted_local_resource_path(root)?;
                            if !std::fs::metadata(&root)
                                .map(|metadata| metadata.is_dir())
                                .unwrap_or(false)
                            {
                                return Err(
                                    "directory grant root is not an available directory".into()
                                );
                            }
                            (root, true)
                        }
                    };
                    PermissionResourceScope::LocalPath {
                        path,
                        recursive,
                        access: *access,
                    }
                }
                super::DeclaredResource::NetworkDestination { scheme, host, port } => {
                    PermissionResourceScope::NetworkDestination {
                        scheme: scheme.to_ascii_lowercase(),
                        host: host.to_ascii_lowercase(),
                        port: *port,
                    }
                }
                super::DeclaredResource::Clipboard { access } => {
                    PermissionResourceScope::Clipboard { access: *access }
                }
                super::DeclaredResource::Desktop { access } => {
                    PermissionResourceScope::Desktop { access: *access }
                }
                super::DeclaredResource::Window {
                    window_id,
                    process_id,
                    access,
                    element_fingerprint,
                } => {
                    if !is_canonical_window_id(window_id) || *process_id == 0 {
                        return Err(
                            "persistent window grant requires a valid window identity".into()
                        );
                    }
                    if element_fingerprint
                        .as_deref()
                        .is_some_and(|value| !is_sha256_fingerprint(value))
                    {
                        return Err(
                            "persistent window grant has an invalid UI element fingerprint".into(),
                        );
                    }
                    PermissionResourceScope::Window {
                        window_id: window_id.clone(),
                        process_id: *process_id,
                        access: *access,
                        element_fingerprint: element_fingerprint.clone(),
                    }
                }
                super::DeclaredResource::Registry {
                    hive,
                    path,
                    access,
                    value_name,
                } => {
                    if target != PermissionResourceTarget::Exact {
                        return Err("registry grants require an exact resource scope".into());
                    }
                    if !matches!(hive.as_str(), "HKCU" | "HKLM" | "HKCR" | "HKU" | "HKCC")
                        || hive.contains('\\')
                        || hive.contains('\0')
                        || path.contains('\0')
                    {
                        return Err(
                            "persistent registry grant requires a canonical hive and key path"
                                .into(),
                        );
                    }
                    let value_name = match (access, value_name) {
                        (RegistryAccess::EnumerateNames, None) => None,
                        (RegistryAccess::ReadValue, Some(name))
                            if !name.is_empty() && !name.contains('\0') =>
                        {
                            Some(name.clone())
                        }
                        _ => {
                            return Err(
                                "persistent registry grant has an invalid access/value-name pair"
                                    .into(),
                            );
                        }
                    };
                    PermissionResourceScope::Registry {
                        hive: hive.clone(),
                        path: path.clone(),
                        access: *access,
                        value_name,
                    }
                }
                super::DeclaredResource::McpRemoteInvocation {
                    target_fingerprint,
                    request_fingerprint,
                } => {
                    if target != PermissionResourceTarget::Exact
                        || !is_sha256_fingerprint(target_fingerprint)
                        || !is_sha256_fingerprint(request_fingerprint)
                    {
                        return Err("persistent MCP grant requires exact canonical target and request fingerprints".into());
                    }
                    PermissionResourceScope::McpRemoteInvocation {
                        target_fingerprint: target_fingerprint.clone(),
                        request_fingerprint: request_fingerprint.clone(),
                    }
                }
                super::DeclaredResource::ManagedAsset { asset_id } => {
                    if !is_canonical_id(asset_id, "asset") {
                        return Err(
                            "persistent managed asset grant requires a canonical asset id".into(),
                        );
                    }
                    PermissionResourceScope::ManagedAsset {
                        asset_id: asset_id.clone(),
                    }
                }
            };
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        Ok(scopes)
    }

    /// Snapshot of permanent grants for the settings UI.
    pub async fn list_permanent(&self) -> Vec<StoredPermission> {
        let cfg = self.config.read().await;
        let mut out = cfg
            .permanent
            .allow
            .iter()
            .map(|(key, scopes)| StoredPermission {
                key: key.to_string(),
                effect: PermissionEffect::Allow,
                resource_scope: scopes.clone(),
            })
            .chain(
                cfg.permanent
                    .deny
                    .iter()
                    .map(|(key, scopes)| StoredPermission {
                        key: key.to_string(),
                        effect: PermissionEffect::Deny,
                        resource_scope: scopes.clone(),
                    }),
            )
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.key.cmp(&b.key));
        out
    }

    /// Remove one permanent grant. Session grants have independent lifetimes
    /// and are removed only by their owning session-level controls.
    pub async fn revoke_permanent(&self, key: &str) -> bool {
        let Ok(capability) = CapabilityScope::try_new(key) else {
            return false;
        };
        let mut cfg = self.config.write().await;
        if cfg.permanent.allow.get(&capability).is_some_and(|scopes| {
            !mcp_connection_grants(&HashMap::from([(capability.clone(), scopes.clone())]))
                .is_empty()
        }) {
            self.mcp_permission_changes.send_replace(Arc::new(()));
        }
        let removed_permanent = cfg.permanent.allow.remove(&capability).is_some()
            | cfg.permanent.deny.remove(&capability).is_some();
        if removed_permanent {
            self.bump_global_policy_revision(&mut cfg);
        }
        removed_permanent
    }

    /// Remove one exact capability grant from one session in memory.
    pub async fn revoke_session_grant(
        &self,
        session_id: &str,
        capability: &CapabilityScope,
    ) -> bool {
        let mut cfg = self.config.write().await;
        let mut removed = false;
        let keep_session = if let Some(grants) = cfg.session_grants.get_mut(session_id) {
            removed |= grants.allow.remove(capability).is_some();
            removed |= grants.deny.remove(capability).is_some();
            !grants.allow.is_empty() || !grants.deny.is_empty()
        } else {
            true
        };
        if !keep_session {
            cfg.session_grants.remove(session_id);
        }
        if removed {
            advance_policy_revision(&mut cfg);
            self.cancel_session_processes(session_id);
        }
        removed
    }

    /// Remove every permanent rule. Durable session grants have their own
    /// explicit reset command and remain in effect.
    pub async fn clear_permanent(&self) -> usize {
        let mut cfg = self.config.write().await;
        let removed = cfg.permanent.allow.len() + cfg.permanent.deny.len();
        if !mcp_connection_grants(&cfg.permanent.allow).is_empty() {
            self.mcp_permission_changes.send_replace(Arc::new(()));
        }
        cfg.permanent = SessionGrants::default();
        if removed > 0 {
            self.bump_global_policy_revision(&mut cfg);
        }
        removed
    }

    /// Drop one session's process-local grants when its actor ends or is
    /// removed. Durable rows remain until session deletion or retention.
    pub async fn clear_session_trust(&self, session_id: &str) {
        let mut cfg = self.config.write().await;
        if cfg.session_grants.remove(session_id).is_some() {
            advance_policy_revision(&mut cfg);
            self.cancel_session_processes(session_id);
        }
    }

    /// Drop every process-local session grant. Durable cleanup belongs to the
    /// SessionStore and follows explicit reset/deletion/retention operations.
    pub async fn clear_all_trust(&self) {
        let mut cfg = self.config.write().await;
        if !cfg.session_grants.is_empty() {
            let affected_sessions = cfg.session_grants.keys().cloned().collect::<Vec<_>>();
            cfg.session_grants.clear();
            advance_policy_revision(&mut cfg);
            for session_id in affected_sessions {
                self.cancel_session_processes(&session_id);
            }
        }
    }
}

#[async_trait::async_trait]
impl AuthorizationPort for AuthorizationEngine {
    async fn execution_workspace_snapshot(
        &self,
        session_id: Option<&str>,
    ) -> ExecutionWorkspaceSnapshot {
        match session_id {
            Some(session_id) => self.session_execution_workspace_snapshot(session_id).await,
            None => AuthorizationEngine::execution_workspace_snapshot(self).await,
        }
    }

    async fn bind_session_execution_workspace(
        &self,
        session_id: &str,
        root: Option<&Path>,
    ) -> Result<ExecutionWorkspaceSnapshot, String> {
        AuthorizationEngine::bind_session_execution_workspace(self, session_id, root).await
    }

    async fn forget_session_execution_workspaces(&self, session_ids: &[String]) {
        AuthorizationEngine::forget_session_execution_workspaces(self, session_ids).await
    }

    async fn bind_session_execution_mode(&self, session_id: &str, mode: ExecutionMode) {
        AuthorizationEngine::bind_session_execution_mode(self, session_id, mode).await;
    }

    async fn session_execution_mode(&self, session_id: &str) -> ExecutionMode {
        AuthorizationEngine::session_execution_mode(self, session_id).await
    }

    async fn authorize(&self, request: &AuthorizationRequest) -> AuthorizationDecision {
        AuthorizationEngine::authorize(self, request).await
    }

    async fn authorize_for_execution(&self, request: &AuthorizationRequest) -> AuthorizationCheck {
        AuthorizationEngine::authorize_for_execution(self, request).await
    }

    async fn verify_receipt(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<(), String> {
        AuthorizationEngine::verify_receipt(self, request, receipt).await
    }

    async fn confirm_for_execution(
        &self,
        request: &AuthorizationRequest,
        receipt: &ConfirmationReceipt,
    ) -> Result<AuthorizationExecutionProof, String> {
        AuthorizationEngine::confirm_for_execution(self, request, receipt).await
    }

    async fn verify_execution_proof(
        &self,
        request: &AuthorizationRequest,
        proof: &AuthorizationExecutionProof,
    ) -> Result<(), String> {
        AuthorizationEngine::verify_execution_proof(self, request, proof).await
    }

    async fn resource_scope_for_request(
        &self,
        request: &AuthorizationRequest,
        target: PermissionResourceTarget,
    ) -> Result<Vec<PermissionResourceScope>, String> {
        AuthorizationEngine::resource_scope_for_request(self, request, target).await
    }

    async fn grant(
        &self,
        session_id: Option<&str>,
        capability: CapabilityScope,
        effect: PermissionEffect,
        scope: PermissionScope,
        resource_scope: Vec<PermissionResourceScope>,
    ) {
        AuthorizationEngine::grant(self, session_id, capability, effect, scope, resource_scope)
            .await;
    }

    async fn list_permanent(&self) -> Vec<StoredPermission> {
        AuthorizationEngine::list_permanent(self).await
    }

    async fn revoke_permanent(&self, capability: &str) -> bool {
        AuthorizationEngine::revoke_permanent(self, capability).await
    }

    async fn revoke_session_grant(&self, session_id: &str, capability: &CapabilityScope) -> bool {
        AuthorizationEngine::revoke_session_grant(self, session_id, capability).await
    }

    async fn clear_permanent(&self) -> usize {
        AuthorizationEngine::clear_permanent(self).await
    }

    async fn clear_session_trust(&self, session_id: &str) {
        AuthorizationEngine::clear_session_trust(self, session_id).await;
    }

    async fn clear_all_trust(&self) {
        AuthorizationEngine::clear_all_trust(self).await;
    }

    async fn prompt_summary(&self) -> String {
        AuthorizationEngine::prompt_summary(self).await
    }
}

impl Default for AuthorizationEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn effective_risk_from(cfg: &SafetyConfig, tool_name: &str, reported: RiskLevel) -> RiskLevel {
    tool_setting_names(tool_name)
        .filter_map(|name| {
            cfg.tool_settings
                .get(name)
                .and_then(|tool| tool.risk_override)
        })
        .fold(reported, |effective, configured| {
            if configured > effective {
                configured
            } else {
                effective
            }
        })
}

fn tool_setting_names(tool_name: &str) -> impl Iterator<Item = &str> {
    std::iter::once(tool_name)
        .chain(tool_name.split_once('.').map(|(root, _)| root))
        .chain((tool_name == "mcp_stdio").then_some("mcp"))
}

fn advance_policy_revision(cfg: &mut SafetyConfig) {
    cfg.policy_revision = cfg.policy_revision.saturating_add(1);
}

pub(super) fn confirmation_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(u64::MAX)
}

fn confirmation_expiry() -> u64 {
    confirmation_now().saturating_add(5 * 60)
}

pub(super) fn execution_proof_expiry() -> u64 {
    confirmation_now().saturating_add(30)
}

fn canonical_input_hash(input: &Value) -> String {
    let digest = Sha256::digest(haven_common::json::canonical_json_bytes(input));
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn canonical_authorization_hash(request: &AuthorizationRequest) -> String {
    match &request.target_binding {
        Some(target_binding) => canonical_input_hash(&serde_json::json!({
            "input": &request.input,
            "target_binding": target_binding,
            "resources": &request.resources,
        })),
        None => canonical_input_hash(&serde_json::json!({
            "input": &request.input,
            "resources": &request.resources,
        })),
    }
}

fn canonical_authorization_hash_for_workspace(
    request: &AuthorizationRequest,
    workspace_generation: u64,
    session_execution_mode_revision: u64,
) -> String {
    canonical_input_hash(&serde_json::json!({
        "operation": canonical_authorization_hash(request),
        "session_execution_mode_revision": session_execution_mode_revision,
        "workspace_generation": workspace_generation,
    }))
}

struct NetworkPolicyResolution {
    decision: NetworkPolicyDecision,
    reason: Option<String>,
    resource_boundary_extension: bool,
}

impl NetworkPolicyResolution {
    fn allowed() -> Self {
        Self {
            decision: NetworkPolicyDecision::Allowed,
            reason: None,
            resource_boundary_extension: false,
        }
    }

    fn blocked(decision: NetworkPolicyDecision, reason: String) -> Self {
        Self {
            decision,
            reason: Some(reason),
            resource_boundary_extension: false,
        }
    }
}

fn resolve_network_policy(
    cfg: &SafetyConfig,
    tool_name: &str,
    resources: &[super::DeclaredResource],
    operation: &OperationPolicy,
) -> NetworkPolicyResolution {
    if operation.network_access == NetworkAccess::None {
        return NetworkPolicyResolution::allowed();
    }

    let settings = tool_setting_names(tool_name)
        .filter_map(|name| cfg.tool_settings.get(name))
        .collect::<Vec<_>>();
    let global_network_policy = cfg.network.network_policy();
    if global_network_policy == NetworkPolicy::Deny {
        return NetworkPolicyResolution::blocked(
            NetworkPolicyDecision::DeniedNetworking,
            format!("network access is disabled for tool '{tool_name}'"),
        );
    }
    if settings
        .iter()
        .any(|setting| setting.resource_policy.network.access == NetworkAccessBoundary::Deny)
    {
        return NetworkPolicyResolution::blocked(
            NetworkPolicyDecision::DeniedNetworking,
            format!("tool resource policy denies network access for '{tool_name}'"),
        );
    }
    let requires_consent = global_network_policy != NetworkPolicy::Open
        || settings.iter().any(|setting| {
            setting.resource_policy.network.access == NetworkAccessBoundary::Restricted
        })
        || resources.iter().any(|resource| {
            matches!(resource,
            super::DeclaredResource::NetworkDestination { host, .. }
                if haven_common::network::http_host_is_local_or_special(host))
        });
    if requires_consent {
        NetworkPolicyResolution {
            decision: NetworkPolicyDecision::RequiresConfirmation,
            reason: Some(format!("network permission is required for '{tool_name}'")),
            resource_boundary_extension: true,
        }
    } else {
        NetworkPolicyResolution::allowed()
    }
}

fn file_read_policy_for_config(cfg: &SafetyConfig, tool_name: &str) -> FileReadPolicy {
    let mut allowed_roots = Vec::new();
    if !cfg.global_file_roots.is_empty() {
        allowed_roots.push(cfg.global_file_roots.clone());
    }
    allowed_roots.extend(
        tool_setting_names(tool_name)
            .filter_map(|name| cfg.tool_settings.get(name))
            .filter(|setting| !setting.resource_policy.files.path_roots.is_empty())
            .map(|setting| {
                setting
                    .resource_policy
                    .files
                    .path_roots
                    .iter()
                    .map(PathBuf::from)
                    .collect()
            }),
    );
    let mut denied_paths = cfg.global_denied_paths.clone();
    denied_paths.extend(
        tool_setting_names(tool_name)
            .filter_map(|name| cfg.tool_settings.get(name))
            .flat_map(|setting| setting.resource_policy.files.denied_paths.iter())
            .map(PathBuf::from),
    );
    match open_filesystem_paths(&denied_paths, "resource_policy.files.denied_paths") {
        Ok(denied_paths) => FileReadPolicy {
            allowed_roots,
            denied_paths,
            deny_all: false,
        },
        Err(_) => FileReadPolicy {
            allowed_roots,
            denied_paths: Vec::new(),
            deny_all: true,
        },
    }
}

fn validate_denied_paths(paths: &[PathBuf], setting: &str) -> Result<(), String> {
    open_filesystem_paths(paths, setting).map(|_| ())
}

fn proof_resource_boundary_extension(proof: &AuthorizationExecutionProof) -> bool {
    proof.binding.resource_boundary_extension
}

fn workspace_host_process_block(
    sandbox_mode: SandboxMode,
    tool_name: &str,
    operation: &OperationPolicy,
) -> Option<String> {
    // WorkspaceWrite protects the host filesystem from local child
    // processes. In-process network adapters remain subject to the separate
    // network policy.
    (matches!(sandbox_mode, SandboxMode::WorkspaceWrite)
        && matches!(
            operation.execution_boundary,
            crate::ExecutionBoundary::HostProcess
        ))
    .then(|| format!("workspace-write sandbox cannot safely constrain host process '{tool_name}'"))
}

fn merge_resource_scopes(
    current: &mut Vec<PermissionResourceScope>,
    additions: &[PermissionResourceScope],
) {
    for scope in additions {
        if !current.contains(scope) {
            current.push(scope.clone());
        }
    }
}

/// Windows `canonicalize` returns the `\\?\` extended syntax. It is useful
/// for long-path I/O, but storing it makes later resource resolution treat a
/// persisted grant as a device path and can prevent it from matching its
/// ordinary drive-letter declaration. Persist only ordinary local absolute
/// paths; UNC and device paths remain unsupported.
fn persisted_local_resource_path(path: PathBuf) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let path = {
        let value = path.to_string_lossy();
        let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
        PathBuf::from(value)
    };

    if !path.is_absolute() || is_unc_or_device_path(&path) {
        return Err("persistent path grant requires an ordinary local absolute path".into());
    }
    Ok(path)
}

fn request_has_allow(
    cfg: &SafetyConfig,
    request: &AuthorizationRequest,
    workspace: &ExecutionWorkspace,
) -> bool {
    scoped_grant_matches(
        &cfg.permanent.allow,
        request.capability(),
        &request.resources,
        workspace,
        false,
    ) || request
        .session_id
        .as_ref()
        .and_then(|id| cfg.session_grants.get(id))
        .is_some_and(|grants| {
            scoped_grant_matches(
                &grants.allow,
                request.capability(),
                &request.resources,
                workspace,
                false,
            )
        })
}

fn request_denial(
    cfg: &SafetyConfig,
    request: &AuthorizationRequest,
    workspace: &ExecutionWorkspace,
) -> Option<(String, AuthorizationReasonCode)> {
    if request_has_allow(cfg, request, workspace) {
        return None;
    }
    if scoped_grant_matches(
        &cfg.permanent.deny,
        request.capability(),
        &request.resources,
        workspace,
        true,
    ) {
        return Some((
            format!("permanently denied: {}", request.capability()),
            AuthorizationReasonCode::PermanentDeny,
        ));
    }
    if request
        .session_id
        .as_ref()
        .and_then(|id| cfg.session_grants.get(id))
        .is_some_and(|grants| {
            scoped_grant_matches(
                &grants.deny,
                request.capability(),
                &request.resources,
                workspace,
                true,
            )
        })
    {
        return Some((
            format!("denied for this session: {}", request.capability()),
            AuthorizationReasonCode::SessionDeny,
        ));
    }
    None
}

fn scoped_grant_matches(
    grants: &ScopedGrantMap,
    key: &CapabilityScope,
    resources: &[super::DeclaredResource],
    workspace: &ExecutionWorkspace,
    allow_unscoped: bool,
) -> bool {
    key.candidates().into_iter().any(|candidate| {
        let Some(scopes) = grants.get(&candidate) else {
            return false;
        };
        if scopes.is_empty() {
            return allow_unscoped;
        }
        !resources.is_empty()
            && resources.iter().all(|resource| {
                scopes
                    .iter()
                    .any(|scope| permission_resource_scope_matches(scope, resource, workspace))
            })
    })
}

fn permission_resource_scope_matches(
    scope: &PermissionResourceScope,
    resource: &super::DeclaredResource,
    workspace: &ExecutionWorkspace,
) -> bool {
    match (scope, resource) {
        (
            PermissionResourceScope::ProcessNetwork {
                target_fingerprint: granted,
            },
            super::DeclaredResource::ProcessNetwork { target_fingerprint },
        ) => granted == target_fingerprint,

        (
            PermissionResourceScope::LocalPath {
                path: root,
                recursive,
                access: granted_access,
            },
            super::DeclaredResource::LocalPath {
                path,
                access: requested_access,
            },
        ) if granted_access == requested_access => {
            let root = resolve_relative_sandbox_path(root.clone(), workspace);
            let path = resolve_relative_sandbox_path(path.clone(), workspace);
            let (Some(root), Some(path)) = (
                resolve_path_without_reparse(&root),
                resolve_path_without_reparse(&path),
            ) else {
                return false;
            };
            if *recursive {
                path_is_within(&path, &root)
            } else {
                paths_equal(&path, &root)
            }
        }
        (
            PermissionResourceScope::NetworkDestination {
                scheme: granted_scheme,
                host: granted_host,
                port: granted_port,
            },
            super::DeclaredResource::NetworkDestination { scheme, host, port },
        ) => {
            granted_scheme.eq_ignore_ascii_case(scheme)
                && granted_host.eq_ignore_ascii_case(host)
                && granted_port == port
        }
        (
            PermissionResourceScope::Clipboard {
                access: granted_access,
            },
            super::DeclaredResource::Clipboard {
                access: requested_access,
            },
        ) => granted_access == requested_access,
        (
            PermissionResourceScope::Desktop {
                access: granted_access,
            },
            super::DeclaredResource::Desktop {
                access: requested_access,
            },
        ) => granted_access == requested_access,
        (
            PermissionResourceScope::Window {
                window_id: granted_window_id,
                process_id: granted_process_id,
                access: granted_access,
                element_fingerprint: granted_element_fingerprint,
            },
            super::DeclaredResource::Window {
                window_id,
                process_id,
                access,
                element_fingerprint,
            },
        ) => {
            granted_window_id == window_id
                && granted_process_id == process_id
                && granted_access == access
                && granted_element_fingerprint == element_fingerprint
        }
        (
            PermissionResourceScope::Registry {
                hive: granted_hive,
                path: granted_path,
                access: granted_access,
                value_name: granted_value_name,
            },
            super::DeclaredResource::Registry {
                hive,
                path,
                access,
                value_name,
            },
        ) => {
            granted_hive == hive
                && granted_path == path
                && granted_access == access
                && granted_value_name == value_name
        }
        (
            PermissionResourceScope::McpRemoteInvocation {
                target_fingerprint: granted_target_fingerprint,
                request_fingerprint: granted_request_fingerprint,
            },
            super::DeclaredResource::McpRemoteInvocation {
                target_fingerprint,
                request_fingerprint,
            },
        ) => {
            granted_target_fingerprint == target_fingerprint
                && granted_request_fingerprint == request_fingerprint
        }
        (
            PermissionResourceScope::ManagedAsset {
                asset_id: granted_asset_id,
            },
            super::DeclaredResource::ManagedAsset { asset_id },
        ) => granted_asset_id == asset_id,
        _ => false,
    }
}

fn is_canonical_window_id(value: &str) -> bool {
    let Some(hex) = value.strip_prefix("hwnd:") else {
        return false;
    };
    let Ok(raw) = usize::from_str_radix(hex, 16) else {
        return false;
    };
    raw != 0 && format!("{raw:x}") == hex
}

fn is_sha256_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn disabled_operation_block(
    settings: &HashMap<String, ToolConfig>,
    tool_name: &str,
    capability: &CapabilityScope,
) -> Option<String> {
    for name in tool_setting_names(tool_name) {
        let Some(cfg) = settings.get(name) else {
            continue;
        };
        for disabled in &cfg.disabled_operations {
            let d = disabled.trim();
            if d.is_empty() {
                continue;
            }
            // Disabled operations are matched against the canonical
            // capability identity. A bare value remains a convenient
            // operation-local shorthand (`files` + `read`), but arbitrary
            // request fields can no longer change what is disabled.
            let matches = d == tool_name
                || d == capability.as_str()
                || format!("{name}.{d}") == capability.as_str();
            if matches {
                return Some(format!(
                    "capability '{}' is disabled for tool '{tool_name}'",
                    capability
                ));
            }
        }
    }
    None
}

struct PolicyBoundaryFailure {
    reason: String,
    reason_code: AuthorizationReasonCode,
    resource_extension_allowed: bool,
}

/// Check the hard policy boundaries in their established precedence order.
/// Each authorization entry calls this at its existing point in its own
/// sequence. Callers retain their identity, expiry, revision, Plan, grant and
/// critical checks around this shared ordered boundary check.
fn check_policy_boundaries(
    cfg: &SafetyConfig,
    execution_workspace: &ExecutionWorkspace,
    tool_name: &str,
    capability: &CapabilityScope,
    resources: &[super::DeclaredResource],
    policy: &OperationPolicy,
) -> Result<(), PolicyBoundaryFailure> {
    if let Some(reason) = disabled_operation_block(&cfg.tool_settings, tool_name, capability) {
        return Err(PolicyBoundaryFailure {
            reason,
            reason_code: AuthorizationReasonCode::DisabledOperation,
            resource_extension_allowed: false,
        });
    }
    let mut denied_paths = cfg.global_denied_paths.clone();
    for setting in tool_setting_names(tool_name).filter_map(|name| cfg.tool_settings.get(name)) {
        denied_paths.extend(
            setting
                .resource_policy
                .files
                .denied_paths
                .iter()
                .map(PathBuf::from),
        );
    }
    for resource in resources {
        if let super::DeclaredResource::LocalPath { path, .. } = resource {
            let path = resolve_relative_sandbox_path(path.clone(), execution_workspace);
            if filesystem_policy_path_matches(&denied_paths, &path) {
                return Err(PolicyBoundaryFailure {
                    reason: "resource is explicitly denied".into(),
                    reason_code: AuthorizationReasonCode::SandboxBoundary,
                    resource_extension_allowed: false,
                });
            }
        }
    }
    let mut expansion =
        workspace_host_process_block(cfg.sandbox_mode, tool_name, policy).map(|reason| {
            PolicyBoundaryFailure {
                reason,
                reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
                resource_extension_allowed: true,
            }
        });
    let network = resolve_network_policy(cfg, tool_name, resources, policy);
    if let Some(reason) = network.reason {
        let failure = PolicyBoundaryFailure {
            reason,
            reason_code: if network.resource_boundary_extension {
                AuthorizationReasonCode::ResourceBoundaryExtension
            } else {
                AuthorizationReasonCode::NetworkPolicy
            },
            resource_extension_allowed: network.resource_boundary_extension,
        };
        if !failure.resource_extension_allowed {
            return Err(failure);
        }
        expansion = Some(failure);
    }
    if let Some(failure) =
        path_sandbox_block(cfg, execution_workspace, tool_name, resources, policy)
    {
        if !failure.resource_extension_allowed {
            return Err(failure);
        }
        expansion = Some(failure);
    }
    if let Some(failure) = expansion {
        return Err(failure);
    }

    Ok(())
}

fn path_sandbox_block(
    cfg: &SafetyConfig,
    execution_workspace: &ExecutionWorkspace,
    tool_name: &str,
    resources: &[super::DeclaredResource],
    policy: &OperationPolicy,
) -> Option<PolicyBoundaryFailure> {
    if matches!(cfg.sandbox_mode, SandboxMode::ReadOnly)
        && (matches!(policy.effect, OperationEffect::WorkspaceWrite)
            || policy.execution_boundary != crate::ExecutionBoundary::InProcess)
    {
        return Some(PolicyBoundaryFailure {
            reason: format!("read-only resource policy blocks mutating operation '{tool_name}'"),
            reason_code: AuthorizationReasonCode::SandboxBoundary,
            resource_extension_allowed: true,
        });
    }
    let paths = resources
        .iter()
        .filter_map(|resource| match resource {
            super::DeclaredResource::LocalPath { path, access } => Some((path, access)),
            super::DeclaredResource::NetworkDestination { .. }
            | super::DeclaredResource::Clipboard { .. }
            | super::DeclaredResource::Desktop { .. }
            | super::DeclaredResource::Window { .. }
            | super::DeclaredResource::Registry { .. }
            | super::DeclaredResource::McpRemoteInvocation { .. }
            | super::DeclaredResource::ManagedAsset { .. }
            | super::DeclaredResource::ProcessNetwork { .. } => None,
        })
        .collect::<Vec<_>>();
    if matches!(policy.effect, OperationEffect::WorkspaceWrite)
        && !paths.is_empty()
        && !paths
            .iter()
            .any(|(_, access)| **access == super::LocalPathAccess::Write)
    {
        return Some(PolicyBoundaryFailure {
            reason: format!(
                "mutating operation '{tool_name}' did not declare its writable resources"
            ),
            reason_code: AuthorizationReasonCode::SandboxBoundary,
            resource_extension_allowed: false,
        });
    }
    if paths.is_empty() {
        let has_tool_path_policy = tool_setting_names(tool_name).any(|name| {
            cfg.tool_settings.get(name).is_some_and(|setting| {
                !setting.resource_policy.files.path_roots.is_empty()
                    || !matches!(
                        setting.resource_policy.files.write_mode,
                        FileWriteMode::Inherit | FileWriteMode::Unrestricted
                    )
            })
        });
        if (matches!(cfg.sandbox_mode, SandboxMode::WorkspaceWrite)
            && matches!(policy.effect, OperationEffect::WorkspaceWrite))
            || has_tool_path_policy
        {
            return Some(PolicyBoundaryFailure {
                reason: format!("operation '{tool_name}' did not declare its local path resources"),
                reason_code: AuthorizationReasonCode::SandboxBoundary,
                resource_extension_allowed: false,
            });
        }
        return None;
    }
    for (path, access) in &paths {
        let path = resolve_relative_sandbox_path((*path).clone(), execution_workspace);
        let mut denied_paths = cfg.global_denied_paths.clone();
        for setting in tool_setting_names(tool_name).filter_map(|name| cfg.tool_settings.get(name))
        {
            denied_paths.extend(
                setting
                    .resource_policy
                    .files
                    .denied_paths
                    .iter()
                    .map(PathBuf::from),
            );
        }
        if filesystem_policy_path_matches(&denied_paths, &path) {
            return Some(PolicyBoundaryFailure {
                reason: format!("operation '{tool_name}' is blocked by a denied or protected path"),
                reason_code: AuthorizationReasonCode::SandboxBoundary,
                resource_extension_allowed: false,
            });
        }
        if **access == super::LocalPathAccess::Write {
            let mut readonly_paths = cfg.global_read_only_roots.clone();
            for setting in
                tool_setting_names(tool_name).filter_map(|name| cfg.tool_settings.get(name))
            {
                readonly_paths.extend(
                    setting
                        .resource_policy
                        .files
                        .read_only_roots
                        .iter()
                        .map(PathBuf::from),
                );
            }
            if filesystem_policy_path_matches(&readonly_paths, &path) {
                return Some(PolicyBoundaryFailure {
                    reason: format!("operation '{tool_name}' targets a read-only protected path"),
                    reason_code: AuthorizationReasonCode::SandboxBoundary,
                    resource_extension_allowed: true,
                });
            }
        }
    }
    for (path, _) in &paths {
        if !cfg.global_file_roots.is_empty() {
            let path = resolve_relative_sandbox_path((*path).clone(), execution_workspace);
            if !path_is_allowed(&path, &cfg.global_file_roots) {
                return Some(PolicyBoundaryFailure {
                    reason: format!(
                        "operation '{tool_name}' is blocked by the global file resource path policy"
                    ),
                    reason_code: AuthorizationReasonCode::SandboxBoundary,
                    resource_extension_allowed: true,
                });
            }
        }
    }
    for name in tool_setting_names(tool_name) {
        let Some(tool_cfg) = cfg.tool_settings.get(name) else {
            continue;
        };
        if tool_cfg.resource_policy.files.path_roots.is_empty() {
            continue;
        }
        let allowed: Vec<PathBuf> = tool_cfg
            .resource_policy
            .files
            .path_roots
            .iter()
            .map(PathBuf::from)
            .collect();
        for (path, _) in &paths {
            let path = resolve_relative_sandbox_path((*path).clone(), execution_workspace);
            if !path_is_allowed(&path, &allowed) {
                return Some(PolicyBoundaryFailure {
                    reason: format!(
                        "operation '{tool_name}' is blocked by its tool resource path policy"
                    ),
                    reason_code: AuthorizationReasonCode::SandboxBoundary,
                    resource_extension_allowed: true,
                });
            }
        }
    }
    if matches!(policy.effect, OperationEffect::WorkspaceWrite)
        || (tool_name == "shell"
            && paths
                .iter()
                .any(|(_, access)| **access == super::LocalPathAccess::Write))
    {
        for name in tool_setting_names(tool_name) {
            let Some(setting) = cfg.tool_settings.get(name) else {
                continue;
            };
            let allowed = match setting.resource_policy.files.write_mode {
                FileWriteMode::Inherit | FileWriteMode::Unrestricted => continue,
                FileWriteMode::ReadOnly => {
                    return Some(PolicyBoundaryFailure {
                        reason: format!(
                            "tool resource policy blocks file mutation for '{tool_name}'"
                        ),
                        reason_code: AuthorizationReasonCode::SandboxBoundary,
                        resource_extension_allowed: true,
                    });
                }
                FileWriteMode::Workspace => {
                    let Some(root) = execution_workspace.root() else {
                        return Some(PolicyBoundaryFailure {
                            reason: format!(
                                "tool resource policy has no workspace for '{tool_name}'"
                            ),
                            reason_code: AuthorizationReasonCode::SandboxBoundary,
                            resource_extension_allowed: false,
                        });
                    };
                    vec![root.to_path_buf()]
                }
                FileWriteMode::Roots => {
                    if setting.resource_policy.files.write_roots.is_empty() {
                        return Some(PolicyBoundaryFailure {
                            reason: format!(
                                "tool resource policy has no write roots for '{tool_name}'"
                            ),
                            reason_code: AuthorizationReasonCode::SandboxBoundary,
                            resource_extension_allowed: false,
                        });
                    }
                    setting
                        .resource_policy
                        .files
                        .write_roots
                        .iter()
                        .map(PathBuf::from)
                        .collect()
                }
            };
            if paths.iter().any(|(path, access)| {
                if **access != super::LocalPathAccess::Write {
                    return false;
                }
                let path = resolve_relative_sandbox_path((*path).clone(), execution_workspace);
                !path_is_allowed(&path, &allowed)
            }) {
                return Some(PolicyBoundaryFailure {
                    reason: format!("operation '{tool_name}' is blocked by its tool write scope"),
                    reason_code: AuthorizationReasonCode::SandboxBoundary,
                    resource_extension_allowed: true,
                });
            }
        }
    }
    if matches!(cfg.sandbox_mode, SandboxMode::WorkspaceWrite)
        && matches!(policy.effect, OperationEffect::WorkspaceWrite)
    {
        let allowed: Vec<PathBuf> = if cfg.writable_roots.is_empty() {
            execution_workspace
                .root()
                .map(Path::to_path_buf)
                .into_iter()
                .collect()
        } else {
            cfg.writable_roots.clone()
        };
        if allowed.is_empty() {
            return Some(PolicyBoundaryFailure {
                reason: format!(
                    "workspace resource policy has no execution root for '{tool_name}'"
                ),
                reason_code: AuthorizationReasonCode::SandboxBoundary,
                resource_extension_allowed: false,
            });
        }
        if paths.iter().any(|(path, access)| {
            if **access != super::LocalPathAccess::Write {
                return false;
            }
            let path = resolve_relative_sandbox_path((*path).clone(), execution_workspace);
            !path_is_allowed(&path, &allowed)
        }) {
            return Some(PolicyBoundaryFailure {
                reason: format!(
                    "a path for mutating operation '{tool_name}' is outside its configured write scope"
                ),
                reason_code: AuthorizationReasonCode::SandboxBoundary,
                resource_extension_allowed: true,
            });
        }
    }
    if matches!(cfg.sandbox_mode, SandboxMode::WorkspaceWrite) && tool_name == "shell" {
        let allowed: Vec<PathBuf> = if cfg.writable_roots.is_empty() {
            execution_workspace
                .root()
                .map(Path::to_path_buf)
                .into_iter()
                .collect()
        } else {
            cfg.writable_roots.clone()
        };
        if allowed.is_empty() {
            return Some(PolicyBoundaryFailure {
                reason: "workspace resource policy has no execution root for 'shell'".into(),
                reason_code: AuthorizationReasonCode::SandboxBoundary,
                resource_extension_allowed: false,
            });
        }
        if paths.iter().any(|(path, _)| {
            let path = resolve_relative_sandbox_path((*path).clone(), execution_workspace);
            !path_is_allowed(&path, &allowed)
        }) {
            return Some(PolicyBoundaryFailure {
                reason: "Shell requested a local path outside its configured Workspace Write roots"
                    .into(),
                reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
                resource_extension_allowed: true,
            });
        }
    }
    None
}

fn resolve_relative_sandbox_path(
    path: PathBuf,
    execution_workspace: &ExecutionWorkspace,
) -> PathBuf {
    if path.is_absolute() || is_unc_or_device_path(&path) {
        return path;
    }
    execution_workspace.resolve_relative(&path).unwrap_or(path)
}

fn filesystem_policy_path_matches(paths: &[PathBuf], candidate: &Path) -> bool {
    paths.iter().any(|path| {
        haven_platform::filesystem::FilesystemPathCapability::open(path)
            .map(|scope| scope.contains_path(candidate).unwrap_or(true))
            .unwrap_or(true)
    })
}

fn resource_extension_write_roots(
    request: &AuthorizationRequest,
    execution_workspace: &ExecutionWorkspace,
) -> Result<
    (
        Vec<haven_platform::filesystem::DirectoryCapability>,
        Vec<PathBuf>,
    ),
    String,
> {
    let mut roots = Vec::new();
    let mut targets = Vec::new();
    for resource in &request.resources {
        let super::DeclaredResource::LocalPath {
            path,
            access: super::LocalPathAccess::Write,
        } = resource
        else {
            continue;
        };
        let path = resolve_relative_sandbox_path(path.clone(), execution_workspace);
        if is_unc_or_device_path(&path) {
            return Err("temporary resource extension does not support UNC or device paths".into());
        }
        let target = resolve_path_without_reparse(&path)
            .ok_or_else(|| "temporary resource extension target is unavailable".to_owned())?;
        let target = persisted_local_resource_path(target)?;
        let is_existing_directory = std::fs::metadata(&target)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false);
        let root = if is_existing_directory {
            target.clone()
        } else {
            target.parent().map(Path::to_path_buf).ok_or_else(|| {
                "temporary resource extension has no containing directory".to_owned()
            })?
        };
        roots.push(persisted_local_resource_path(root)?);
        targets.push(target);
    }
    if roots.is_empty() {
        return Err("temporary resource extension has no declared write resource".into());
    }
    Ok((
        open_directory_roots(&roots, "temporary resource extension")?,
        targets,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeclaredResource, LocalPathAccess, ToolExecutionOutcome};
    use haven_common::types::permission_key;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use tokio_util::sync::CancellationToken;

    macro_rules! capability_scope {
        ($value:expr) => {
            CapabilityScope::try_new($value).expect("test capability scope is valid")
        };
    }

    fn test_config_service(
        loader: haven_common::config::ConfigLoader,
    ) -> haven_common::config::ConfigService {
        haven_common::config::ConfigService::new_with_credential_store(
            loader,
            std::sync::Arc::new(haven_common::config::InMemoryCredentialStore::default()),
        )
        .unwrap()
    }

    type ConfirmationResult = AuthorizationDecision;

    fn fixture_policy(tool_name: &str, params: &Value, risk_level: RiskLevel) -> OperationPolicy {
        let capability_key = permission_key(tool_name, params);
        let capability_key = if tool_name.starts_with("mcp::") || tool_name.starts_with("skill::") {
            crate::llm_tool_name(&capability_key)
        } else {
            capability_key
        };
        let network_access = match tool_name {
            "http" => NetworkAccess::Public,
            name if name.starts_with("mcp::")
                || name.starts_with("mcp__")
                || name.starts_with("skill::")
                || name.starts_with("skill__")
                || name.starts_with("haven.mcp.") =>
            {
                NetworkAccess::Opaque
            }
            _ => NetworkAccess::None,
        };
        let execution_boundary = if tool_name == "shell" {
            if cfg!(windows) {
                crate::ExecutionBoundary::HostProcess
            } else {
                crate::ExecutionBoundary::HostProcess
            }
        } else if tool_name.starts_with("mcp::")
            || tool_name.starts_with("mcp__")
            || tool_name.starts_with("skill::")
            || tool_name.starts_with("skill__")
            || tool_name.starts_with("haven.mcp.")
        {
            crate::ExecutionBoundary::HostProcess
        } else {
            crate::ExecutionBoundary::InProcess
        };
        OperationPolicy {
            risk_level,
            capability: CapabilityScope::try_new(capability_key).unwrap(),
            confirmation: if risk_level >= RiskLevel::Critical {
                ConfirmationRequirement::Required
            } else if risk_level == RiskLevel::Safe {
                ConfirmationRequirement::None
            } else {
                ConfirmationRequirement::SecurityPolicy
            },
            idempotency: crate::OperationIdempotency::Unknown,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: if risk_level == RiskLevel::Safe {
                OperationEffect::ReadOnly
            } else {
                OperationEffect::ExternalEffect
            },
            data_sensitivity: DataSensitivity::None,
            network_access,
            execution_boundary,
        }
    }

    async fn approved_test_proof(
        engine: &AuthorizationEngine,
        request: &AuthorizationRequest,
    ) -> AuthorizationExecutionProof {
        let check = engine.authorize_for_execution(request).await;
        match check.decision {
            AuthorizationDecision::AutoApproved => check.execution_proof.unwrap(),
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => engine
                .confirm_for_execution(request, &receipt)
                .await
                .unwrap(),
            decision => panic!("fixture operation cannot be authorized: {decision:?}"),
        }
    }

    async fn execution_authorization_for_session(
        authorization: Arc<AuthorizationEngine>,
        session_id: &str,
    ) -> ExecutionPolicyAuthorization {
        let input = json!({});
        // The session already has an `ask` deny while this test is creating
        // an unrelated process authorization to observe session revocation.
        let request = AuthorizationRequest::new(
            Some(session_id),
            "authorization.test.safe",
            input.clone(),
            fixture_policy("authorization.test.safe", &input, RiskLevel::Safe),
        );
        let proof = approved_test_proof(&authorization, &request).await;
        authorization
            .execution_context_for_proof(&request, &proof)
            .await
            .expect("the execution context should be derived from the proof")
            .execution_authorization
            .expect("the execution context should carry live authorization")
    }

    async fn authorize(
        engine: &AuthorizationEngine,
        session_id: Option<&str>,
        tool_name: &str,
        params: &Value,
        policy: &OperationPolicy,
    ) -> AuthorizationDecision {
        let request = fixture_request(session_id, tool_name, params, policy);
        engine.authorize(&request).await
    }

    /// Build the explicit resource contract used by policy-engine fixtures.
    /// Production requests get these from `Tool::authorization_resources`.
    fn fixture_request(
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
        policy: &OperationPolicy,
    ) -> AuthorizationRequest {
        let mut resources = Vec::new();
        if let Some(object) = input.as_object() {
            for (field, access) in [
                (
                    "path",
                    if matches!(policy.effect, OperationEffect::WorkspaceWrite) {
                        LocalPathAccess::Write
                    } else {
                        LocalPathAccess::Read
                    },
                ),
                ("source", LocalPathAccess::Read),
                ("destination", LocalPathAccess::Write),
                ("file_path", LocalPathAccess::Read),
            ] {
                if let Some(path) = object.get(field).and_then(Value::as_str) {
                    resources.push(DeclaredResource::LocalPath {
                        path: PathBuf::from(path),
                        access,
                    });
                }
            }
            for field in ["paths", "files"] {
                if let Some(paths) = object.get(field).and_then(Value::as_array) {
                    resources.extend(paths.iter().filter_map(Value::as_str).map(|path| {
                        DeclaredResource::LocalPath {
                            path: PathBuf::from(path),
                            access: if matches!(policy.effect, OperationEffect::WorkspaceWrite) {
                                LocalPathAccess::Write
                            } else {
                                LocalPathAccess::Read
                            },
                        }
                    }));
                }
            }
        }
        AuthorizationRequest::new(session_id, tool_name, input.clone(), policy.clone())
            .with_resources(resources)
    }

    fn resource_scope_test_policy(capability: CapabilityScope) -> OperationPolicy {
        OperationPolicy {
            risk_level: RiskLevel::Medium,
            capability,
            confirmation: ConfirmationRequirement::SecurityPolicy,
            idempotency: crate::OperationIdempotency::Idempotent,
            scope: crate::ToolOperationScope::Global,
            concurrency: ToolConcurrency::ReadOnly,
            effect: OperationEffect::ExternalEffect,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        }
    }

    fn resource_test_request(
        capability: &CapabilityScope,
        resources: Vec<DeclaredResource>,
    ) -> AuthorizationRequest {
        AuthorizationRequest::new(
            None,
            "resource.test",
            json!({}),
            resource_scope_test_policy(capability.clone()),
        )
        .with_resources(resources)
    }

    #[tokio::test]
    async fn persistent_allow_matches_only_the_confirmed_exact_path() {
        let directory = tempfile::tempdir().unwrap();
        let approved_path = directory.path().join("approved.txt");
        let sibling_path = directory.path().join("sibling.txt");
        std::fs::write(&approved_path, "approved").unwrap();
        std::fs::write(&sibling_path, "sibling").unwrap();
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("resource.test");
        let approved = resource_test_request(
            &capability,
            vec![DeclaredResource::LocalPath {
                path: approved_path.clone(),
                access: LocalPathAccess::Read,
            }],
        );
        let scope = engine
            .resource_scope_for_request(&approved, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        let PermissionResourceScope::LocalPath {
            path: saved_path, ..
        } = &scope[0]
        else {
            panic!("local path declaration must produce a local path grant")
        };
        assert!(saved_path.is_absolute());
        assert!(!is_unc_or_device_path(saved_path));
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scope,
            )
            .await;

        assert!(matches!(
            engine.authorize(&approved).await,
            AuthorizationDecision::AutoApproved
        ));
        let sibling = resource_test_request(
            &capability,
            vec![DeclaredResource::LocalPath {
                path: sibling_path,
                access: LocalPathAccess::Read,
            }],
        );
        assert!(matches!(
            engine.authorize(&sibling).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn directory_scope_covers_descendants_but_not_sibling_directories() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("workspace");
        let nested = root.join("nested");
        let sibling = directory.path().join("outside");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let approved_path = root.join("note.txt");
        let nested_path = nested.join("child.txt");
        let outside_path = sibling.join("secret.txt");
        std::fs::write(&approved_path, "approved").unwrap();
        std::fs::write(&nested_path, "nested").unwrap();
        std::fs::write(&outside_path, "outside").unwrap();
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("resource.test");
        let approved = resource_test_request(
            &capability,
            vec![DeclaredResource::LocalPath {
                path: approved_path,
                access: LocalPathAccess::Read,
            }],
        );
        let scope = engine
            .resource_scope_for_request(&approved, PermissionResourceTarget::Directory)
            .await
            .unwrap();
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scope,
            )
            .await;

        for path in [nested_path, outside_path] {
            let request = resource_test_request(
                &capability,
                vec![DeclaredResource::LocalPath {
                    path: path.clone(),
                    access: LocalPathAccess::Read,
                }],
            );
            let decision = engine.authorize(&request).await;
            if path.starts_with(&root) {
                assert!(matches!(decision, AuthorizationDecision::AutoApproved));
            } else {
                assert!(matches!(
                    decision,
                    AuthorizationDecision::RequiresConfirmation { .. }
                ));
            }
        }
    }

    #[tokio::test]
    async fn persistent_network_allow_matches_only_the_confirmed_origin() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("resource.test");
        let origin = resource_test_request(
            &capability,
            vec![DeclaredResource::NetworkDestination {
                scheme: "https".into(),
                host: "api.example.test".into(),
                port: Some(8443),
            }],
        );
        let scope = engine
            .resource_scope_for_request(&origin, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scope,
            )
            .await;
        assert!(matches!(
            engine.authorize(&origin).await,
            AuthorizationDecision::AutoApproved
        ));
        let different_origin = resource_test_request(
            &capability,
            vec![DeclaredResource::NetworkDestination {
                scheme: "https".into(),
                host: "api.example.test".into(),
                port: Some(443),
            }],
        );
        assert!(matches!(
            engine.authorize(&different_origin).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn persistent_clipboard_and_managed_asset_grants_match_exact_resources() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("clipboard.operation");
        let read = resource_test_request(
            &capability,
            vec![DeclaredResource::Clipboard {
                access: haven_common::types::ClipboardAccess::Read,
            }],
        );
        let read_scope = engine
            .resource_scope_for_request(&read, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        assert_eq!(
            read_scope,
            vec![PermissionResourceScope::Clipboard {
                access: haven_common::types::ClipboardAccess::Read,
            }]
        );
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                read_scope,
            )
            .await;
        assert!(matches!(
            engine.authorize(&read).await,
            AuthorizationDecision::AutoApproved
        ));
        let write = resource_test_request(
            &capability,
            vec![DeclaredResource::Clipboard {
                access: haven_common::types::ClipboardAccess::Write,
            }],
        );
        assert!(matches!(
            engine.authorize(&write).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        let asset_capability = capability_scope!("clipboard.asset");
        let asset_id = haven_common::types::new_id("asset");
        let asset_request = resource_test_request(
            &asset_capability,
            vec![DeclaredResource::ManagedAsset {
                asset_id: asset_id.clone(),
            }],
        );
        let asset_scope = engine
            .resource_scope_for_request(&asset_request, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        engine
            .grant(
                None,
                asset_capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                asset_scope,
            )
            .await;
        assert!(matches!(
            engine.authorize(&asset_request).await,
            AuthorizationDecision::AutoApproved
        ));
        let other_asset = resource_test_request(
            &asset_capability,
            vec![DeclaredResource::ManagedAsset {
                asset_id: haven_common::types::new_id("asset"),
            }],
        );
        assert!(matches!(
            engine.authorize(&other_asset).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn persistent_registry_grants_bind_hive_key_access_and_value_name() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("system.registry");
        let list = resource_test_request(
            &capability,
            vec![DeclaredResource::Registry {
                hive: "HKCU".into(),
                path: "Software\\Haven".into(),
                access: RegistryAccess::EnumerateNames,
                value_name: None,
            }],
        );
        let scope = engine
            .resource_scope_for_request(&list, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        assert_eq!(
            scope,
            vec![PermissionResourceScope::Registry {
                hive: "HKCU".into(),
                path: "Software\\Haven".into(),
                access: RegistryAccess::EnumerateNames,
                value_name: None,
            }]
        );
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scope,
            )
            .await;
        assert!(matches!(
            engine.authorize(&list).await,
            AuthorizationDecision::AutoApproved
        ));

        for changed in [
            DeclaredResource::Registry {
                hive: "HKLM".into(),
                path: "Software\\Haven".into(),
                access: RegistryAccess::EnumerateNames,
                value_name: None,
            },
            DeclaredResource::Registry {
                hive: "HKCU".into(),
                path: "Software\\Other".into(),
                access: RegistryAccess::EnumerateNames,
                value_name: None,
            },
            DeclaredResource::Registry {
                hive: "HKCU".into(),
                path: "Software\\Haven".into(),
                access: RegistryAccess::ReadValue,
                value_name: Some("InstallPath".into()),
            },
        ] {
            let changed_request = resource_test_request(&capability, vec![changed]);
            assert!(matches!(
                engine.authorize(&changed_request).await,
                AuthorizationDecision::RequiresConfirmation { .. }
            ));
        }

        let invalid_shape = resource_test_request(
            &capability,
            vec![DeclaredResource::Registry {
                hive: "HKCU".into(),
                path: "Software\\Haven".into(),
                access: RegistryAccess::EnumerateNames,
                value_name: Some("unexpected".into()),
            }],
        );
        assert!(
            engine
                .resource_scope_for_request(&invalid_shape, PermissionResourceTarget::Exact)
                .await
                .is_err()
        );
        assert!(
            engine
                .resource_scope_for_request(&list, PermissionResourceTarget::Directory)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn mcp_remote_invocations_require_fresh_confirmation_even_with_exact_persistent_allow() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("mcp__calendar__calendar_status");
        let mut request = resource_test_request(
            &capability,
            vec![DeclaredResource::McpRemoteInvocation {
                target_fingerprint: "a".repeat(64),
                request_fingerprint: "b".repeat(64),
            }],
        );
        request.policy = OperationPolicy::external(capability.clone(), RiskLevel::High);
        assert_eq!(
            request.policy.confirmation,
            ConfirmationRequirement::Required
        );
        let scope = engine
            .resource_scope_for_request(&request, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scope,
            )
            .await;
        assert!(
            matches!(
                engine.authorize(&request).await,
                AuthorizationDecision::RequiresConfirmation { .. }
            ),
            "a saved MCP allow must not bypass the per-call confirmation floor"
        );

        for (invalid, target) in [
            (
                DeclaredResource::McpRemoteInvocation {
                    target_fingerprint: "not-sha256".into(),
                    request_fingerprint: "b".repeat(64),
                },
                PermissionResourceTarget::Exact,
            ),
            (
                DeclaredResource::McpRemoteInvocation {
                    target_fingerprint: "a".repeat(64),
                    request_fingerprint: "b".repeat(64),
                },
                PermissionResourceTarget::Directory,
            ),
        ] {
            let invalid_request = resource_test_request(&capability, vec![invalid]);
            assert!(
                engine
                    .resource_scope_for_request(&invalid_request, target)
                    .await
                    .is_err()
            );
        }
    }

    #[tokio::test]
    async fn persistent_window_grants_bind_identity_access_and_ui_control() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("window.interact");
        let resource = DeclaredResource::Window {
            window_id: "hwnd:2a".into(),
            process_id: 42,
            access: haven_common::types::WindowAccess::Interact,
            element_fingerprint: Some("a".repeat(64)),
        };
        let request = resource_test_request(&capability, vec![resource.clone()]);
        let scopes = engine
            .resource_scope_for_request(&request, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        assert_eq!(
            scopes,
            vec![PermissionResourceScope::Window {
                window_id: "hwnd:2a".into(),
                process_id: 42,
                access: haven_common::types::WindowAccess::Interact,
                element_fingerprint: Some("a".repeat(64)),
            }]
        );
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scopes,
            )
            .await;
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::AutoApproved
        ));

        for changed in [
            DeclaredResource::Window {
                window_id: "hwnd:2b".into(),
                process_id: 42,
                access: haven_common::types::WindowAccess::Interact,
                element_fingerprint: Some("a".repeat(64)),
            },
            DeclaredResource::Window {
                window_id: "hwnd:2a".into(),
                process_id: 43,
                access: haven_common::types::WindowAccess::Interact,
                element_fingerprint: Some("a".repeat(64)),
            },
            DeclaredResource::Window {
                window_id: "hwnd:2a".into(),
                process_id: 42,
                access: haven_common::types::WindowAccess::Close,
                element_fingerprint: Some("a".repeat(64)),
            },
            DeclaredResource::Window {
                window_id: "hwnd:2a".into(),
                process_id: 42,
                access: haven_common::types::WindowAccess::Interact,
                element_fingerprint: Some("b".repeat(64)),
            },
        ] {
            let changed_request = resource_test_request(&capability, vec![changed]);
            assert!(matches!(
                engine.authorize(&changed_request).await,
                AuthorizationDecision::RequiresConfirmation { .. }
            ));
        }
    }

    #[tokio::test]
    async fn persistent_window_grants_reject_noncanonical_identities() {
        let engine = AuthorizationEngine::new();
        let capability = capability_scope!("window.read");
        let invalid = resource_test_request(
            &capability,
            vec![DeclaredResource::Window {
                window_id: "hwnd:02a".into(),
                process_id: 42,
                access: haven_common::types::WindowAccess::Read,
                element_fingerprint: None,
            }],
        );
        assert!(
            engine
                .resource_scope_for_request(&invalid, PermissionResourceTarget::Exact)
                .await
                .is_err()
        );

        let invalid_fingerprint = resource_test_request(
            &capability,
            vec![DeclaredResource::Window {
                window_id: "hwnd:2a".into(),
                process_id: 42,
                access: haven_common::types::WindowAccess::Read,
                element_fingerprint: Some("not-a-fingerprint".into()),
            }],
        );
        assert!(
            engine
                .resource_scope_for_request(&invalid_fingerprint, PermissionResourceTarget::Exact)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn persistent_desktop_grants_separate_read_from_capture() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("window.desktop");
        let read = resource_test_request(
            &capability,
            vec![DeclaredResource::Desktop {
                access: haven_common::types::DesktopAccess::Read,
            }],
        );
        let scopes = engine
            .resource_scope_for_request(&read, PermissionResourceTarget::Exact)
            .await
            .unwrap();
        assert_eq!(
            scopes,
            vec![PermissionResourceScope::Desktop {
                access: haven_common::types::DesktopAccess::Read,
            }]
        );
        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                scopes,
            )
            .await;
        assert!(matches!(
            engine.authorize(&read).await,
            AuthorizationDecision::AutoApproved
        ));
        let capture = resource_test_request(
            &capability,
            vec![DeclaredResource::Desktop {
                access: haven_common::types::DesktopAccess::Capture,
            }],
        );
        assert!(matches!(
            engine.authorize(&capture).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn persistent_allow_without_typed_resources_is_refused() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let capability = capability_scope!("resource.test");
        let request = resource_test_request(&capability, Vec::new());
        assert!(
            engine
                .resource_scope_for_request(&request, PermissionResourceTarget::Exact)
                .await
                .is_err()
        );
        engine
            .grant(
                None,
                capability,
                PermissionEffect::Allow,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn execution_proof_binds_input_and_policy_revision() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let input = serde_json::json!({"value": "approved"});
        let request = AuthorizationRequest::new(
            Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "ask",
            input,
            fixture_policy(
                "ask",
                &serde_json::json!({"value": "approved"}),
                RiskLevel::Safe,
            ),
        );
        let check = engine.authorize_for_execution(&request).await;
        assert!(matches!(
            check.decision,
            AuthorizationDecision::AutoApproved
        ));
        let proof = check
            .execution_proof
            .expect("auto-approved requests carry an execution proof");
        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_ok()
        );

        let child_input = json!({"path": "child.txt"});
        let child_request = AuthorizationRequest::new(
            request.session_id.as_deref(),
            "files.read",
            child_input.clone(),
            fixture_policy("files.read", &child_input, RiskLevel::Safe),
        );
        assert!(
            engine
                .verify_execution_proof(&child_request, &proof)
                .await
                .is_err(),
            "a parent tool proof cannot authorize a child capability"
        );

        let mut other_session_request = request.clone();
        other_session_request.session_id = Some("ses-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into());
        assert!(
            engine
                .verify_execution_proof(&other_session_request, &proof)
                .await
                .is_err(),
            "a proof cannot be replayed in another session"
        );

        let mut changed_request = request.clone();
        changed_request.input = serde_json::json!({"value": "changed"});
        assert!(
            engine
                .verify_execution_proof(&changed_request, &proof)
                .await
                .is_err(),
            "proof cannot be replayed with different input"
        );

        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_err(),
            "even an equivalent boundary update invalidates an outstanding proof"
        );

        let target_request = request
            .clone()
            .with_target_binding(Some("mcp-target:0000000000000001:0000000000000002".into()));
        let target_proof = approved_test_proof(&engine, &target_request).await;
        let changed_target = target_request
            .clone()
            .with_target_binding(Some("mcp-target:0000000000000001:0000000000000003".into()));
        assert!(
            engine
                .verify_execution_proof(&changed_target, &target_proof)
                .await
                .is_err(),
            "an authorization proof cannot be redirected to a replacement target"
        );
    }

    #[tokio::test]
    async fn execution_context_rejects_a_stale_execution_proof() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let input = json!({"path": "notes.txt", "content": "safe"});
        let mut policy = fixture_policy("files.write", &input, RiskLevel::Safe);
        policy.effect = OperationEffect::WorkspaceWrite;
        policy.confirmation = ConfirmationRequirement::None;
        let request = fixture_request(None, "files.write", &input, &policy);
        let proof = approved_test_proof(&engine, &request).await;

        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .expect("current proofs can capture execution authority");
        assert!(context.file_write_policy.is_some());
        assert!(context.execution_workspace.is_some());
        assert!(
            context
                .acquire_execution_policy_permit()
                .await
                .expect("the current policy grants process creation")
                .is_some()
        );

        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        assert!(
            engine
                .execution_context_for_proof(&request, &proof)
                .await
                .is_err(),
            "execution authority must not be captured from a stale proof"
        );
        let reason = context
            .acquire_execution_policy_permit()
            .await
            .err()
            .expect("a captured context cannot start a process after policy changes");
        let error = execution_policy_denied(reason);
        let metadata = error
            .downcast_ref::<StructuredToolError>()
            .expect("a stale spawn is reported as a structured permission failure")
            .metadata();
        assert_eq!(metadata.class, ToolErrorClass::Permission);
        assert_eq!(metadata.retryability, ToolRetryability::NotRetryable);
    }

    #[tokio::test]
    async fn process_spawn_permit_serializes_live_policy_updates() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let input = json!({"value": "spawn"});
        let request = AuthorizationRequest::new(
            None,
            "ask",
            input.clone(),
            fixture_policy("ask", &input, RiskLevel::Safe),
        );
        let proof = approved_test_proof(&engine, &request).await;
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .expect("current proof captures process-start authorization");
        let spawn_permit = context
            .acquire_execution_policy_permit()
            .await
            .expect("policy is live")
            .expect("proof-bound execution context owns a process-start permit");

        assert!(
            engine.config.try_write().is_err(),
            "policy changes cannot acquire the owner write lock during process creation"
        );
        drop(spawn_permit);
        engine
            .set_boundaries(SandboxMode::ReadOnly, Vec::new(), NetworkPolicy::Deny)
            .await;
        assert!(
            context.acquire_execution_policy_permit().await.is_err(),
            "the captured execution context is stale after the update"
        );
    }

    #[tokio::test]
    async fn policy_revision_change_notifies_running_execution_authorization() {
        let (authorization, engine) = ExecutionPolicyAuthorization::for_test_with_engine();
        let mut waiter = tokio::spawn(async move {
            authorization.wait_for_policy_revocation().await;
        });

        engine
            .grant(
                None,
                capability_scope!("shell"),
                PermissionEffect::Allow,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
        assert_eq!(engine.config.read().await.policy_revision, 0);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiter)
                .await
                .is_err(),
            "a grant must not revoke already-authorized work"
        );

        engine.set_execution_mode(ExecutionMode::Analyze).await;

        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("running authorization should observe policy revocation")
            .expect("policy watcher should not panic");
    }

    #[tokio::test]
    async fn session_trust_revocation_stops_only_processes_for_affected_sessions() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        let ask_capability = capability_scope!("ask");
        engine
            .grant(
                Some("session-one"),
                ask_capability.clone(),
                PermissionEffect::Deny,
                PermissionScope::Session,
                Vec::new(),
            )
            .await;
        engine
            .grant(
                Some("session-two"),
                ask_capability.clone(),
                PermissionEffect::Deny,
                PermissionScope::Session,
                Vec::new(),
            )
            .await;
        engine
            .grant(
                Some("session-four"),
                ask_capability.clone(),
                PermissionEffect::Deny,
                PermissionScope::Session,
                Vec::new(),
            )
            .await;

        let session_one =
            execution_authorization_for_session(Arc::clone(&engine), "session-one").await;
        let session_two =
            execution_authorization_for_session(Arc::clone(&engine), "session-two").await;
        let session_four =
            execution_authorization_for_session(Arc::clone(&engine), "session-four").await;
        let no_trust_session =
            execution_authorization_for_session(Arc::clone(&engine), "session-three").await;
        let session_two_stale_proof = session_two.clone();
        let mut session_one_waiter = tokio::spawn(async move {
            session_one.wait_for_policy_revocation().await;
        });
        let mut session_two_waiter = tokio::spawn(async move {
            session_two.wait_for_policy_revocation().await;
        });
        let mut session_four_waiter = tokio::spawn(async move {
            session_four.wait_for_policy_revocation().await;
        });
        let mut no_trust_waiter = tokio::spawn(async move {
            no_trust_session.wait_for_policy_revocation().await;
        });

        assert!(
            engine
                .revoke_session_grant("session-one", &ask_capability)
                .await,
            "the existing session grant should be removed"
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut session_one_waiter)
            .await
            .expect("the process owned by the revoked session should stop")
            .expect("session-one watcher should not panic");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                &mut session_two_waiter
            )
            .await
            .is_err(),
            "revoking one session must not stop a different session"
        );
        assert!(
            session_two_stale_proof.acquire().await.is_err(),
            "the global revision must still reject another session's old proof"
        );

        engine.clear_session_trust("session-two").await;
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut session_two_waiter)
            .await
            .expect("clearing one session's trust should stop that session's process")
            .expect("session-two watcher should not panic");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                &mut session_four_waiter
            )
            .await
            .is_err(),
            "clearing one session must not stop another session's process"
        );

        engine.clear_all_trust().await;
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut session_four_waiter)
            .await
            .expect("clear-all trust should stop processes from sessions with removed grants")
            .expect("session-four watcher should not panic");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut no_trust_waiter)
                .await
                .is_err(),
            "clear-all trust should not stop a session that had no trust grant"
        );

        engine.set_execution_mode(ExecutionMode::Analyze).await;
        tokio::time::timeout(std::time::Duration::from_secs(1), no_trust_waiter)
            .await
            .expect("global mode changes should still stop every proof-bound process")
            .expect("session-three watcher should not panic");
    }

    #[tokio::test]
    async fn tool_settings_update_does_not_publish_global_process_revocation() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        let mut process_revocations = engine.process_revocations.subscribe();
        let before_revision = engine.config.read().await.policy_revision;
        let unrelated_authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "unrelated",
        );
        let mut settings = HashMap::new();
        settings.insert(
            "shell".into(),
            ToolConfig {
                enabled: false,
                ..ToolConfig::default()
            },
        );

        engine.set_tool_settings(settings).await;

        assert_eq!(
            engine.config.read().await.policy_revision,
            before_revision + 1,
            "new execution proofs must still observe the updated tool setting"
        );
        assert!(
            unrelated_authorization.acquire().await.is_err(),
            "the policy revision must reject stale proofs even when their process is not revoked"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                process_revocations.changed()
            )
            .await
            .is_err(),
            "tool-scoped updates must not publish a global process revocation"
        );
    }

    #[tokio::test]
    async fn tool_settings_update_revokes_only_affected_tool_processes() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        let shell_config = ToolConfig::default();
        let skill_config = ToolConfig::default();
        engine
            .set_tool_settings(HashMap::from([
                ("shell".into(), shell_config.clone()),
                ("skill".into(), skill_config.clone()),
            ]))
            .await;

        let shell_authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "shell",
        );
        let skill_authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "skill.demo",
        );
        let mut shell_waiter = tokio::spawn(async move {
            shell_authorization.wait_for_policy_revocation().await;
        });
        let mut skill_waiter = tokio::spawn(async move {
            skill_authorization.wait_for_policy_revocation().await;
        });

        let changed_shell = ToolConfig {
            enabled: false,
            ..shell_config.clone()
        };
        engine
            .set_tool_settings(HashMap::from([
                ("shell".into(), changed_shell.clone()),
                ("skill".into(), skill_config.clone()),
            ]))
            .await;

        tokio::time::timeout(std::time::Duration::from_secs(1), &mut shell_waiter)
            .await
            .expect("the affected shell process should be revoked")
            .expect("shell authorization watcher should not panic");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut skill_waiter)
                .await
                .is_err(),
            "a skill process must keep running after an unrelated shell setting change"
        );

        let changed_skill = ToolConfig {
            enabled: false,
            ..skill_config
        };
        engine
            .set_tool_settings(HashMap::from([
                ("shell".into(), changed_shell),
                ("skill".into(), changed_skill),
            ]))
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), skill_waiter)
            .await
            .expect("the affected skill process should be revoked through its root setting")
            .expect("skill authorization watcher should not panic");
    }

    #[test]
    fn execution_cancellation_registry_prunes_expired_identities() {
        let engine = Arc::new(AuthorizationEngine::new());
        let expired = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "retired.tool",
        );
        drop(expired);
        let expired_session = ExecutionPolicyAuthorization::for_test_with_engine_tool_and_session(
            Arc::clone(&engine),
            "retired.session.tool",
            Some("session-retired"),
        );
        drop(expired_session);

        let _active = ExecutionPolicyAuthorization::for_test_with_engine_tool_and_session(
            Arc::clone(&engine),
            "current.tool",
            Some("session-current"),
        );

        let processes = engine
            .tool_processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !processes.contains_key("retired.tool") && !processes.contains_key("retired"),
            "inactive tool aliases should not accumulate in the registry"
        );
        drop(processes);
        let session_processes = engine
            .session_processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !session_processes.contains_key("session-retired"),
            "inactive session identities should not accumulate in the registry"
        );
    }

    #[tokio::test]
    async fn mcp_stdio_process_authorization_is_bound_to_network_policy() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let authorization = engine
            .capture_mcp_stdio_process_start_authorization()
            .await
            .expect("explicitly open network policy permits an MCP stdio snapshot");
        let permit = authorization
            .acquire(haven_mcp::McpStdioProcessStartRequest::default())
            .await
            .expect("the current open policy permits process creation");
        drop(permit);

        engine
            .set_boundaries(
                SandboxMode::FullAccess,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;

        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_err(),
            "a captured stdio start authorization must be rejected after policy revision changes"
        );
        assert!(
            engine
                .capture_mcp_stdio_process_start_authorization()
                .await
                .is_some(),
            "an unknown permission must reach the interactive start gate"
        );

        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let authorization = engine
            .capture_mcp_stdio_process_start_authorization()
            .await
            .expect("FullAccess and Open permit a fresh stdio generation");
        engine
            .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Open)
            .await;
        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_err(),
            "changing to WorkspaceWrite must revoke an existing stdio start permit"
        );
        let workspace_authorization = engine.capture_mcp_stdio_process_start_authorization().await;
        if cfg!(windows) {
            let workspace_authorization = workspace_authorization
                .expect("WorkspaceWrite on Windows permits MXC-backed MCP startup");
            let permit = workspace_authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .expect("WorkspaceWrite MCP startup compiles an isolated policy");
            assert!(
                haven_mcp::McpStdioProcessStartPermit::sandboxed_process_policy(&permit)
                    .expect("MCP has an MXC policy")
                    .network_policy
                    .allow_public_egress
            );
        } else {
            assert!(workspace_authorization.is_none());
        }
    }

    #[tokio::test]
    async fn mcp_stdio_authorization_survives_unrelated_permission_changes() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let authorization = engine
            .capture_mcp_stdio_process_start_authorization()
            .await
            .expect("open network policy permits an MCP stdio snapshot");
        let mut process_changes = engine.subscribe_stdio_process_changes();

        engine.set_execution_mode(ExecutionMode::Analyze).await;
        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_ok(),
            "permission mode changes affect tool calls, not the lifetime of an already-open stdio server"
        );

        engine.set_tool_settings(HashMap::new()).await;
        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_ok(),
            "tool settings changes must not revoke an unrelated server process"
        );

        let capability = CapabilityScope::try_new("shell").unwrap();
        engine
            .grant(
                Some("session-one"),
                capability.clone(),
                PermissionEffect::Deny,
                PermissionScope::Session,
                Vec::new(),
            )
            .await;
        assert!(
            engine
                .revoke_session_grant("session-one", &capability)
                .await
        );
        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_ok(),
            "revoking a session-scoped shell grant must not disconnect MCP servers"
        );

        let security = SecurityConfig {
            execution_mode: ExecutionMode::Unattended,
            resource_policy: ResourcePolicy {
                files: FileResourcePolicy {
                    write_mode: FileWriteMode::Unrestricted,
                    ..FileResourcePolicy::default()
                },
                network: NetworkResourcePolicy {
                    access: NetworkAccessBoundary::Open,
                    ..NetworkResourcePolicy::default()
                },
            },
            ..SecurityConfig::default()
        };
        engine.apply_security(&security).await.unwrap();
        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_ok(),
            "applying unrelated security settings without changing process boundaries must preserve the server generation"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                process_changes.changed()
            )
            .await
            .is_err(),
            "unrelated authorization changes must not notify the MCP process owner"
        );

        engine
            .set_boundaries(
                SandboxMode::FullAccess,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(1), process_changes.changed())
            .await
            .expect("network boundary changes must notify the MCP process owner")
            .expect("MCP process boundary signal remains available");
        assert!(
            authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_err(),
            "a network boundary change must still invalidate the captured server authorization"
        );

        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let workspace_authorization = engine
            .capture_mcp_stdio_process_start_authorization()
            .await
            .expect("open policy permits a fresh MCP server generation");
        let next_workspace = tempfile::tempdir().unwrap();
        engine
            .switch_execution_workspace(next_workspace.path())
            .await
            .unwrap();
        assert!(
            workspace_authorization
                .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                .await
                .is_err(),
            "workspace changes must continue to revoke captured MCP server generations"
        );
    }

    #[tokio::test]
    async fn mcp_stdio_tool_network_policy_revokes_live_process_authorization() {
        for tool_name in ["mcp_stdio", "mcp"] {
            let engine = Arc::new(AuthorizationEngine::new());
            engine
                .set_permission_interaction(Arc::new(
                    super::super::consent::ApprovingPermissionInteraction,
                ))
                .await;
            engine
                .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
                .await;
            let old_authorization = engine
                .capture_mcp_stdio_process_start_authorization()
                .await
                .expect("initial Open policy allows a Full Access server");
            let mut process_changes = engine.subscribe_stdio_process_changes();
            let settings = HashMap::from([(
                tool_name.into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        network: NetworkResourcePolicy {
                            access: NetworkAccessBoundary::Deny,
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            )]);

            engine.set_tool_settings(settings).await;

            assert!(
                old_authorization
                    .acquire(haven_mcp::McpStdioProcessStartRequest::default())
                    .await
                    .is_err(),
                "changing {tool_name} process policy must reject old start proofs"
            );
            tokio::time::timeout(std::time::Duration::from_secs(1), process_changes.changed())
                .await
                .expect("the MCP manager must be notified to stop old generations")
                .expect("the process-boundary watcher remains available");
            assert!(
                engine
                    .capture_mcp_stdio_process_start_authorization()
                    .await
                    .is_some(),
                "a snapshot may be captured; the live permission gate still refuses explicit denials"
            );
        }
    }

    #[tokio::test]
    async fn side_effect_permit_outlives_cancelled_caller_until_worker_finishes() {
        let authorization = ExecutionPolicyAuthorization::for_test();
        let engine = Arc::clone(&authorization.authorization);
        let permit = authorization.acquire().await.unwrap();
        let (worker_started_tx, worker_started_rx) = tokio::sync::oneshot::channel();
        let (release_worker_tx, release_worker_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let _ = worker_started_tx.send(());
                let _ = release_worker_rx.blocking_recv();
            })
            .await
            .unwrap();
        });
        worker_started_rx
            .await
            .expect("blocking side effect worker started with the owned permit");
        caller.abort();

        let (update_started_tx, update_started_rx) = tokio::sync::oneshot::channel();
        let update = tokio::spawn(async move {
            let _ = update_started_tx.send(());
            engine.set_execution_mode(ExecutionMode::Unattended).await;
        });
        update_started_rx
            .await
            .expect("policy update attempted to acquire the owner write lock");
        tokio::task::yield_now().await;
        assert!(
            !update.is_finished(),
            "cancelling the awaiting task must not release policy while blocking work continues"
        );

        release_worker_tx
            .send(())
            .expect("release the bounded side effect worker");
        update
            .await
            .expect("the policy update completes after the worker releases its permit");
    }

    #[tokio::test]
    async fn workspace_switch_advances_generation_and_invalidates_old_proofs() {
        let first_root = tempfile::tempdir().unwrap();
        let next_root = tempfile::tempdir().unwrap();
        let workspace = ExecutionWorkspace::from_root(first_root.path()).unwrap();
        let engine = Arc::new(AuthorizationEngine::new_with_workspace(workspace));
        engine
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        let input = json!({});
        let request = AuthorizationRequest::new(
            None,
            "ask",
            input.clone(),
            fixture_policy("ask", &input, RiskLevel::Safe),
        );
        let proof = approved_test_proof(&engine, &request).await;
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .expect("current proof captures its workspace generation");
        let before = engine.execution_workspace_snapshot().await;
        let same = engine
            .switch_execution_workspace(first_root.path())
            .await
            .expect("selecting the current root is idempotent");
        assert_eq!(same, before);
        assert!(
            engine
                .execution_context_for_proof(&request, &proof)
                .await
                .is_ok(),
            "idempotently selecting the same root does not invalidate current proofs"
        );

        let mut process_boundary_changes = engine.subscribe_stdio_process_changes();
        let after = engine
            .switch_execution_workspace(next_root.path())
            .await
            .expect("a valid local directory can become the execution workspace");
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            process_boundary_changes.changed(),
        )
        .await
        .expect("workspace switch must notify the MCP process owner")
        .expect("the process-boundary watcher remains available");
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.root.as_deref(), Some(next_root.path()));
        assert!(
            engine
                .execution_context_for_proof(&request, &proof)
                .await
                .is_err(),
            "switching the workspace invalidates outstanding execution proofs"
        );
        assert!(
            context.acquire_execution_policy_permit().await.is_err(),
            "a process-start context cannot cross workspace generations"
        );

        let invalid_root = next_root.path().join("missing");
        assert!(
            engine
                .switch_execution_workspace(&invalid_root)
                .await
                .is_err()
        );
        assert_eq!(engine.execution_workspace_snapshot().await, after);
    }

    #[tokio::test]
    async fn session_workspaces_are_isolated_and_only_revoke_the_changed_session() {
        let global_root = tempfile::tempdir().unwrap();
        let first_root = tempfile::tempdir().unwrap();
        let second_root = tempfile::tempdir().unwrap();
        let next_first_root = tempfile::tempdir().unwrap();
        let engine = AuthorizationEngine::new_with_workspace(
            ExecutionWorkspace::from_root(global_root.path()).unwrap(),
        );
        engine
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        let first_session = "ses-00000000000000000000000000000001";
        let second_session = "ses-00000000000000000000000000000002";
        engine
            .bind_session_execution_workspace(first_session, Some(first_root.path()))
            .await
            .unwrap();
        engine
            .bind_session_execution_workspace(second_session, Some(second_root.path()))
            .await
            .unwrap();

        let policy_for = || OperationPolicy {
            risk_level: RiskLevel::Safe,
            capability: CapabilityScope::try_new("files.write").unwrap(),
            confirmation: ConfirmationRequirement::None,
            idempotency: crate::OperationIdempotency::Unknown,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::WorkspaceWrite,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        let first_input = json!({ "path": first_root.path().join("note.txt") });
        let second_input = json!({ "path": second_root.path().join("note.txt") });
        let first_policy = policy_for();
        let first_request = fixture_request(
            Some(first_session),
            "files.write",
            &first_input,
            &first_policy,
        );
        let second_policy = policy_for();
        let second_request = fixture_request(
            Some(second_session),
            "files.write",
            &second_input,
            &second_policy,
        );
        let first_proof = approved_test_proof(&engine, &first_request).await;
        let second_proof = approved_test_proof(&engine, &second_request).await;
        let outside_policy = policy_for();
        let outside_first = fixture_request(
            Some(first_session),
            "files.write",
            &second_input,
            &outside_policy,
        );
        let outside_decision = engine.authorize(&outside_first).await;
        assert!(
            matches!(
                &outside_decision,
                AuthorizationDecision::RequiresConfirmation {
                    receipt,
                    reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
                    ..
                } if receipt.resource_boundary_extension
                    && receipt.resources == outside_first.resources
            ),
            "unexpected outside-workspace decision: {outside_decision:?}"
        );

        engine
            .bind_session_execution_workspace(first_session, Some(next_first_root.path()))
            .await
            .unwrap();
        assert!(
            engine
                .verify_execution_proof(&first_request, &first_proof)
                .await
                .is_err()
        );
        assert!(
            engine
                .verify_execution_proof(&second_request, &second_proof)
                .await
                .is_ok()
        );
        assert_eq!(
            engine
                .session_execution_workspace_snapshot(first_session)
                .await
                .root
                .as_deref(),
            Some(next_first_root.path())
        );
        assert_eq!(
            engine
                .session_execution_workspace_snapshot(second_session)
                .await
                .root
                .as_deref(),
            Some(second_root.path())
        );

        engine
            .apply_security(&SecurityConfig {
                execution_root: Some(next_first_root.path().to_path_buf()),
                ..SecurityConfig::default()
            })
            .await
            .unwrap();
        assert_eq!(
            engine
                .session_execution_workspace_snapshot(first_session)
                .await
                .root
                .as_deref(),
            Some(next_first_root.path()),
            "changing the default does not move a bound session"
        );
        assert!(
            engine
                .verify_execution_proof(&second_request, &second_proof)
                .await
                .is_ok(),
            "changing the default does not invalidate proofs for a bound session"
        );
    }

    #[tokio::test]
    async fn session_workspace_can_be_reopened_after_a_missing_directory_returns() {
        let default_root = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let selected_root = parent.path().join("recreated-workspace");
        let engine = AuthorizationEngine::new_with_workspace(
            ExecutionWorkspace::from_root(default_root.path()).unwrap(),
        );
        let session_id = "ses-12121212121212121212121212121212";

        let unavailable = engine
            .bind_session_execution_workspace(session_id, Some(&selected_root))
            .await
            .unwrap();
        assert_eq!(unavailable.root.as_deref(), Some(selected_root.as_path()));

        std::fs::create_dir(&selected_root).unwrap();
        let available = engine
            .bind_session_execution_workspace(session_id, Some(&selected_root))
            .await
            .unwrap();
        assert_eq!(available.root.as_deref(), Some(selected_root.as_path()));

        let input = json!({ "path": selected_root.join("note.txt") });
        let policy = OperationPolicy {
            risk_level: RiskLevel::Safe,
            capability: CapabilityScope::try_new("files.write").unwrap(),
            confirmation: ConfirmationRequirement::None,
            idempotency: crate::OperationIdempotency::Unknown,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::WorkspaceWrite,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        let request = fixture_request(Some(session_id), "files.write", &input, &policy);
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::AutoApproved
        ));
    }

    #[tokio::test]
    async fn security_configuration_persists_or_rediscovers_the_execution_root() {
        let initial_root = tempfile::tempdir().unwrap();
        let configured_root = tempfile::tempdir().unwrap();
        let engine = AuthorizationEngine::new_with_workspace(
            ExecutionWorkspace::from_root(initial_root.path()).unwrap(),
        );
        let initial = engine.execution_workspace_snapshot().await;
        let mut security = SecurityConfig {
            execution_root: Some(configured_root.path().to_path_buf()),
            ..SecurityConfig::default()
        };

        engine
            .apply_security(&security)
            .await
            .expect("configured local workspace is available");
        let configured = engine.execution_workspace_snapshot().await;
        assert_eq!(configured.generation, initial.generation + 1);
        assert_eq!(
            configured.root,
            ExecutionWorkspace::open_root(configured_root.path(), 1)
                .unwrap()
                .snapshot()
                .root
        );

        security.execution_root = None;
        engine
            .apply_security(&security)
            .await
            .expect("missing config root selects startup discovery");
        let discovered = ExecutionWorkspace::discover().snapshot();
        let reset = engine.execution_workspace_snapshot().await;
        assert_eq!(reset.generation, configured.generation + 1);
        assert_eq!(reset.root, discovered.root);
    }

    #[tokio::test]
    async fn unavailable_persisted_execution_root_fails_closed() {
        let initial_root = tempfile::tempdir().unwrap();
        let missing_root = initial_root.path().join("missing-workspace");
        let engine = AuthorizationEngine::new_with_workspace(
            ExecutionWorkspace::from_root(initial_root.path()).unwrap(),
        );
        let before = engine.execution_workspace_snapshot().await;
        let security = SecurityConfig {
            execution_root: Some(missing_root),
            ..SecurityConfig::default()
        };

        let error = engine
            .apply_security(&security)
            .await
            .expect_err("a missing persisted workspace must be rejected");
        assert!(error.contains("execution workspace is unavailable"));
        let after = engine.execution_workspace_snapshot().await;
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(after.root, None);

        let mut follow_up = security;
        follow_up.resource_policy.files.write_mode = FileWriteMode::ReadOnly;
        engine
            .apply_security(&follow_up)
            .await
            .expect("already-unavailable workspace does not block other security updates");
        assert_eq!(engine.execution_workspace_snapshot().await.root, None);
    }

    #[tokio::test]
    async fn invalid_writable_roots_are_reported_and_kept_fail_closed() {
        let initial_root = tempfile::tempdir().unwrap();
        let valid_root = tempfile::tempdir().unwrap();
        let missing_root = initial_root.path().join("missing-writable-root");
        let engine = AuthorizationEngine::new_with_workspace(
            ExecutionWorkspace::from_root(initial_root.path()).unwrap(),
        );
        let security = SecurityConfig {
            resource_policy: ResourcePolicy {
                files: FileResourcePolicy {
                    write_mode: FileWriteMode::Roots,
                    write_roots: vec![
                        valid_root.path().to_string_lossy().into_owned(),
                        missing_root.to_string_lossy().into_owned(),
                    ],
                    ..FileResourcePolicy::default()
                },
                ..ResourcePolicy::default()
            },
            ..SecurityConfig::default()
        };

        let error = engine
            .apply_security(&security)
            .await
            .expect_err("every configured writable root must open safely");
        assert!(
            error.contains("security.resource_policy.files.write_roots[1]"),
            "unexpected invalid writable-root error: {error}"
        );

        let config = engine.config.read().await;
        assert_eq!(
            config.writable_roots,
            vec![valid_root.path().to_path_buf(), missing_root]
        );
        assert!(open_directory_roots(&config.writable_roots, "security.writable_roots").is_err());
    }

    #[tokio::test]
    async fn execution_proof_rechecks_a_deny_added_after_authorization() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let session_id = "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let input = json!({});
        let request = AuthorizationRequest::new(
            Some(session_id),
            "ask",
            input.clone(),
            fixture_policy("ask", &input, RiskLevel::Safe),
        );
        let check = engine.authorize_for_execution(&request).await;
        assert!(matches!(
            check.decision,
            AuthorizationDecision::AutoApproved
        ));
        let proof = check.execution_proof.unwrap();

        engine
            .grant(
                Some(session_id),
                request.capability().clone(),
                PermissionEffect::Deny,
                PermissionScope::Session,
                Vec::new(),
            )
            .await;

        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_err(),
            "a newly installed session deny takes effect before the handler starts"
        );
    }

    #[tokio::test]
    async fn confirmed_execution_proof_is_opaque_and_revalidates_the_receipt() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let input = serde_json::json!({"path": "report.txt"});
        let mut policy = fixture_policy("files.write", &input, RiskLevel::High);
        policy.confirmation = ConfirmationRequirement::Required;
        let request = AuthorizationRequest::new(
            Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            "files.write",
            input.clone(),
            policy,
        )
        .with_target_binding(Some("mcp-target:0000000000000001:0000000000000002".into()));
        let check = engine.authorize_for_execution(&request).await;
        let AuthorizationDecision::RequiresConfirmation { receipt, .. } = check.decision else {
            panic!("the explicit confirmation policy must require user approval")
        };
        assert!(
            check.execution_proof.is_none(),
            "a display receipt is not an execution proof"
        );

        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .expect("the interaction owner can convert its accepted receipt to an opaque proof");
        assert!(
            engine
                .verify_execution_proof(&request, &proof)
                .await
                .is_ok()
        );

        let mut changed_request = request.clone();
        changed_request.policy.effect = OperationEffect::WorkspaceWrite;
        assert!(
            engine
                .verify_execution_proof(&changed_request, &proof)
                .await
                .is_err(),
            "a receipt cannot authorize a changed operation contract"
        );
        let changed_target = request
            .clone()
            .with_target_binding(Some("mcp-target:0000000000000001:0000000000000003".into()));
        assert!(
            engine
                .verify_receipt(&changed_target, &receipt)
                .await
                .is_err(),
            "a confirmation receipt cannot be redirected to a replacement target"
        );
    }

    #[tokio::test]
    async fn required_confirmation_prompts_despite_allow_and_explicit_deny_still_blocks() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let input = json!({"command": "Remove-Item -LiteralPath report.txt"});
        let mut request = fixture_request(
            None,
            "shell",
            &input,
            &fixture_policy("shell", &input, RiskLevel::Medium),
        );
        request.policy.confirmation = ConfirmationRequirement::Required;
        let capability = request.capability().clone();

        engine
            .grant(
                None,
                capability.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation {
                reason_code: AuthorizationReasonCode::ApprovalPolicy,
                ..
            }
        ));

        engine
            .grant(
                None,
                capability,
                PermissionEffect::Deny,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::Blocked {
                reason_code: AuthorizationReasonCode::PermanentDeny,
                ..
            }
        ));
    }

    // Keep threshold fixtures terse while production tests use the new policy
    // API. The wrapper only preserves the fixture's threshold semantics; all
    // decisions still run through AuthorizationEngine.
    struct ThresholdFixture {
        engine: AuthorizationEngine,
        threshold: RiskLevel,
    }
    impl ThresholdFixture {
        fn new(threshold: RiskLevel) -> Self {
            let engine = AuthorizationEngine::new();
            // Historical threshold fixtures are about confirmation floors,
            // not transport policy. Keep them on an explicitly open network
            // boundary; dedicated network tests use the real Restricted/Deny
            // defaults instead.
            let mut config = engine.config.try_write().unwrap();
            config.network.access = NetworkAccessBoundary::Open;
            config.sandbox_mode = SandboxMode::FullAccess;
            drop(config);
            Self { engine, threshold }
        }

        async fn check(
            &self,
            session_id: Option<&str>,
            tool_name: &str,
            params: &Value,
            risk_level: RiskLevel,
        ) -> ConfirmationResult {
            let policy = fixture_policy(tool_name, params, risk_level);
            let mut request = fixture_request(session_id, tool_name, params, &policy);
            if !request
                .resources
                .iter()
                .any(|resource| matches!(resource, DeclaredResource::LocalPath { .. }))
            {
                request.resources.extend(threshold_fixture_resource());
            }
            let result = self.engine.authorize(&request).await;
            if risk_level < self.threshold
                && matches!(result, ConfirmationResult::RequiresConfirmation { reason_code, .. } if reason_code != AuthorizationReasonCode::ResourceBoundaryExtension)
            {
                ConfirmationResult::AutoApproved
            } else {
                result
            }
        }

        async fn grant(
            &self,
            session_id: Option<&str>,
            capability: CapabilityScope,
            effect: PermissionEffect,
            scope: PermissionScope,
            mut resource_scope: Vec<PermissionResourceScope>,
        ) {
            if effect == PermissionEffect::Allow && resource_scope.is_empty() {
                resource_scope = threshold_fixture_resource_scope();
                let request = AuthorizationRequest::new(
                    session_id,
                    capability.as_str(),
                    json!({}),
                    fixture_policy(capability.as_str(), &json!({}), RiskLevel::High),
                );
                resource_scope.push(PermissionResourceScope::ProcessNetwork {
                    target_fingerprint: canonical_authorization_hash(&request),
                });
            }
            self.engine
                .grant(session_id, capability, effect, scope, resource_scope)
                .await;
        }
    }

    fn threshold_fixture_resource() -> Vec<DeclaredResource> {
        vec![DeclaredResource::NetworkDestination {
            scheme: "https".into(),
            host: "fixture.invalid".into(),
            port: None,
        }]
    }

    fn threshold_fixture_resource_scope() -> Vec<PermissionResourceScope> {
        vec![PermissionResourceScope::NetworkDestination {
            scheme: "https".into(),
            host: "fixture.invalid".into(),
            port: None,
        }]
    }
    impl std::ops::Deref for ThresholdFixture {
        type Target = AuthorizationEngine;

        fn deref(&self) -> &Self::Target {
            &self.engine
        }
    }
    #[tokio::test]
    async fn test_authorization_default_threshold() {
        let gw = ThresholdFixture::new(RiskLevel::Low);
        // Safe is below Low → auto approved
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn execution_modes_request_ungranted_mutations() {
        let gw = ThresholdFixture::new(RiskLevel::Safe);
        gw.set_execution_mode(ExecutionMode::Collaborative).await;
        assert!(matches!(
            gw.check(None, "tool", &json!({}), RiskLevel::Safe).await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(None, "tool", &json!({}), RiskLevel::Low).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        gw.set_execution_mode(ExecutionMode::Analyze).await;
        assert!(matches!(
            gw.check(None, "tool", &json!({}), RiskLevel::Safe).await,
            ConfirmationResult::AutoApproved
        ));
    }

    #[tokio::test]
    async fn changing_one_session_execution_mode_revokes_only_that_sessions_authority() {
        let engine = AuthorizationEngine::new();
        engine
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        let first_session = "ses-11111111111111111111111111111111";
        let second_session = "ses-22222222222222222222222222222222";
        let ask_input = json!({});
        let ask_policy = fixture_policy("ask", &ask_input, RiskLevel::Safe);
        let first_request = fixture_request(Some(first_session), "ask", &ask_input, &ask_policy);
        let second_request = fixture_request(Some(second_session), "ask", &ask_input, &ask_policy);
        let first_proof = approved_test_proof(&engine, &first_request).await;
        let second_proof = approved_test_proof(&engine, &second_request).await;

        let network_input = json!({ "url": "https://example.com/resource" });
        let network_policy = fixture_policy("http", &network_input, RiskLevel::Medium);
        let first_network_request =
            fixture_request(Some(first_session), "http", &network_input, &network_policy);
        let first_receipt = match engine.authorize(&first_network_request).await {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("public network request should require confirmation: {decision:?}"),
        };
        let second_network_request = fixture_request(
            Some(second_session),
            "http",
            &network_input,
            &network_policy,
        );
        let second_receipt = match engine.authorize(&second_network_request).await {
            AuthorizationDecision::RequiresConfirmation { receipt, .. } => receipt,
            decision => panic!("public network request should require confirmation: {decision:?}"),
        };

        let global_policy_revision = engine.config.read().await.policy_revision;
        engine
            .bind_session_execution_mode(first_session, ExecutionMode::Analyze)
            .await;
        assert_eq!(
            engine.config.read().await.policy_revision,
            global_policy_revision,
            "a session preset change does not advance the global policy revision"
        );
        assert!(
            engine
                .verify_execution_proof(&first_request, &first_proof)
                .await
                .is_err()
        );
        engine
            .verify_execution_proof(&second_request, &second_proof)
            .await
            .expect("the other session's proof remains valid");
        assert!(
            engine
                .confirm_for_execution(&first_network_request, &first_receipt)
                .await
                .is_err()
        );
        let second_network_proof = engine
            .confirm_for_execution(&second_network_request, &second_receipt)
            .await
            .expect("the other session's confirmation receipt remains valid");
        engine
            .verify_execution_proof(&second_network_request, &second_network_proof)
            .await
            .expect("the other session's confirmed proof remains valid");
    }

    #[tokio::test]
    async fn operation_contract_requests_mutation_authority_in_every_mode() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let read_policy = OperationPolicy {
            risk_level: RiskLevel::Low,
            capability: capability_scope!("files.read"),
            confirmation: ConfirmationRequirement::None,
            idempotency: crate::OperationIdempotency::Idempotent,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::ReadOnly,
            effect: OperationEffect::ReadOnly,
            data_sensitivity: DataSensitivity::UserData,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        assert!(matches!(
            authorize(&gateway, None, "files.read", &json!({}), &read_policy).await,
            ConfirmationResult::AutoApproved
        ));

        gateway.set_execution_mode(ExecutionMode::Analyze).await;
        let mut write_policy = OperationPolicy {
            risk_level: RiskLevel::Medium,
            capability: capability_scope!("files.write"),
            confirmation: ConfirmationRequirement::SecurityPolicy,
            idempotency: crate::OperationIdempotency::NonIdempotent,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::WorkspaceWrite,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        assert!(matches!(
            authorize(&gateway, None, "files.write", &json!({}), &write_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        gateway
            .set_execution_mode(ExecutionMode::Collaborative)
            .await;
        assert!(matches!(
            authorize(&gateway, None, "files.write", &json!({}), &write_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        write_policy.confirmation = ConfirmationRequirement::Required;
        gateway.set_execution_mode(ExecutionMode::Unattended).await;
        assert!(matches!(
            authorize(&gateway, None, "files.write", &json!({}), &write_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn analyze_mode_requests_additional_authority() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let write_policy = OperationPolicy {
            risk_level: RiskLevel::Medium,
            capability: capability_scope!("files.write"),
            confirmation: ConfirmationRequirement::SecurityPolicy,
            idempotency: crate::OperationIdempotency::NonIdempotent,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::WorkspaceWrite,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        gateway
            .grant(
                None,
                capability_scope!("files.write"),
                PermissionEffect::Allow,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
        gateway.set_execution_mode(ExecutionMode::Analyze).await;
        assert!(matches!(
            authorize(&gateway, None, "files.write", &json!({}), &write_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn sensitive_read_and_network_access_are_not_safe_just_because_read_only() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let sensitive_policy = OperationPolicy {
            risk_level: RiskLevel::Low,
            capability: capability_scope!("system.env.get"),
            confirmation: ConfirmationRequirement::None,
            idempotency: crate::OperationIdempotency::Idempotent,
            scope: crate::ToolOperationScope::Global,
            concurrency: ToolConcurrency::ReadOnly,
            effect: OperationEffect::ReadOnly,
            data_sensitivity: DataSensitivity::Sensitive,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        assert!(matches!(
            authorize(
                &gateway,
                None,
                "system.env.get",
                &json!({}),
                &sensitive_policy,
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        let network_policy = OperationPolicy {
            capability: capability_scope!("http"),
            network_access: NetworkAccess::Public,
            ..sensitive_policy.clone()
        };
        assert!(matches!(
            authorize(&gateway, None, "http", &json!({}), &network_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn default_network_policy_requests_network_confirmation() {
        let gateway = AuthorizationEngine::new();
        let default_security = SecurityConfig::default();
        assert_eq!(
            default_security.resource_policy.network.access,
            NetworkAccessBoundary::Restricted
        );
        gateway
            .apply_security(&default_security)
            .await
            .expect("default security uses the discovered workspace");

        let public_policy = fixture_policy("http", &json!({}), RiskLevel::Medium);
        assert!(matches!(
            authorize(&gateway, None, "http", &json!({}), &public_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        let opaque_policy = fixture_policy("mcp::remote::search", &json!({}), RiskLevel::High);
        assert!(matches!(
            authorize(
                &gateway,
                None,
                "mcp::remote::search",
                &json!({}),
                &opaque_policy,
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        let skill_policy = fixture_policy("skill__remote", &json!({}), RiskLevel::High);
        assert!(matches!(
            authorize(&gateway, None, "skill__remote", &json!({}), &skill_policy,).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        gateway
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        for (tool_name, policy) in [
            ("mcp::remote::search", opaque_policy),
            ("skill__remote", skill_policy),
        ] {
            assert!(matches!(
                authorize(&gateway, None, tool_name, &json!({}), &policy).await,
                ConfirmationResult::RequiresConfirmation { .. }
            ));
        }
    }

    #[tokio::test]
    async fn changing_network_access_updates_the_single_network_policy() {
        let engine = AuthorizationEngine::new();
        let security = SecurityConfig::default();
        engine
            .apply_security(&security)
            .await
            .expect("default security uses the discovered workspace");

        engine
            .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Deny)
            .await;
        {
            let config = engine.config.read().await;
            assert_eq!(config.network.access, NetworkAccessBoundary::Deny);
        }

        engine
            .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Open)
            .await;
        let config = engine.config.read().await;
        assert_eq!(config.network.access, NetworkAccessBoundary::Open);
    }

    #[tokio::test]
    async fn auto_edit_does_not_auto_approve_ui_schedule_or_process_effects() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        gateway
            .set_execution_mode(ExecutionMode::Collaborative)
            .await;
        gateway
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        for (tool_name, effect) in [
            ("window.focus", OperationEffect::ExternalEffect),
            ("schedule.set", OperationEffect::ExternalEffect),
            ("shell", OperationEffect::ExternalEffect),
        ] {
            let policy = OperationPolicy {
                risk_level: RiskLevel::Medium,
                capability: capability_scope!(tool_name),
                confirmation: ConfirmationRequirement::SecurityPolicy,
                idempotency: crate::OperationIdempotency::Unknown,
                scope: crate::ToolOperationScope::Session,
                concurrency: ToolConcurrency::Exclusive,
                effect,
                data_sensitivity: DataSensitivity::None,
                network_access: if tool_name == "shell" {
                    NetworkAccess::Opaque
                } else {
                    NetworkAccess::None
                },
                execution_boundary: crate::tool_contract::operation_attributes(
                    tool_name,
                    ToolConcurrency::Exclusive,
                )
                .execution_boundary,
            };
            assert!(matches!(
                authorize(&gateway, None, tool_name, &json!({}), &policy).await,
                ConfirmationResult::RequiresConfirmation { .. }
            ));
        }
    }

    #[tokio::test]
    async fn workspace_write_requests_process_and_network_authority() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        gateway
            .set_boundaries(SandboxMode::WorkspaceWrite, Vec::new(), NetworkPolicy::Open)
            .await;
        let policy = OperationPolicy::native(
            "shell",
            capability_scope!("shell"),
            RiskLevel::High,
            NetworkAccess::Opaque,
        );
        #[cfg(windows)]
        let policy = policy.with_execution_boundary(crate::ExecutionBoundary::HostProcess);
        let skill_policy =
            OperationPolicy::external(capability_scope!("skill__echo"), RiskLevel::High);
        let http_mcp_policy =
            OperationPolicy::external(capability_scope!("mcp__remote__search"), RiskLevel::High)
                .with_execution_boundary(crate::ExecutionBoundary::InProcess);
        assert!(matches!(
            authorize(&gateway, None, "shell", &json!({}), &policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            authorize(&gateway, None, "skill__echo", &json!({}), &skill_policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            authorize(
                &gateway,
                None,
                "mcp__remote__search",
                &json!({}),
                &http_mcp_policy
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        gateway
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        assert!(matches!(
            authorize(&gateway, None, "shell", &json!({}), &policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        gateway
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        assert!(matches!(
            authorize(
                &gateway,
                None,
                "mcp__remote__search",
                &json!({}),
                &http_mcp_policy
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            authorize(&gateway, None, "shell", &json!({}), &policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        gateway
            .set_boundaries(
                SandboxMode::FullAccess,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        assert!(matches!(
            authorize(&gateway, None, "shell", &json!({}), &policy).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
        gateway
            .set_boundaries(SandboxMode::FullAccess, Vec::new(), NetworkPolicy::Open)
            .await;
        assert!(matches!(
            authorize(&gateway, None, "shell", &json!({}), &policy).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn opaque_process_network_permission_names_its_real_scope() {
        let engine = Arc::new(AuthorizationEngine::new());
        let root = tempfile::tempdir().unwrap();
        engine
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                vec![root.path().to_path_buf()],
                NetworkPolicy::Restricted,
            )
            .await;
        let request = AuthorizationEngine::runtime_permission_request(
            None,
            "shell",
            json!({"command":"git status"}),
            NetworkAccess::Opaque,
            vec![DeclaredResource::LocalPath {
                path: root.path().to_path_buf(),
                access: LocalPathAccess::Write,
            }],
        )
        .unwrap();
        assert!(
            request
                .resources
                .iter()
                .any(|resource| matches!(resource, DeclaredResource::ProcessNetwork { .. }))
        );
        let proof = approved_test_proof(&engine, &request).await;
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .unwrap();
        let permit = context
            .acquire_execution_policy_permit()
            .await
            .unwrap()
            .unwrap();
        let network = permit.process_network_policy();
        assert!(
            network.allow_public_egress
                && network.allow_private_network_client_server
                && network.allow_host_loopback
        );
    }

    #[tokio::test]
    async fn network_deny_is_a_hard_boundary_for_http_and_adapters() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let security = SecurityConfig {
            resource_policy: ResourcePolicy {
                network: NetworkResourcePolicy {
                    access: NetworkAccessBoundary::Deny,
                    ..NetworkResourcePolicy::default()
                },
                ..ResourcePolicy::default()
            },
            ..SecurityConfig::default()
        };
        gateway
            .apply_security(&security)
            .await
            .expect("valid security workspace applies");
        assert!(matches!(
            gateway
                .check(
                    None,
                    "http",
                    &json!({"url": "https://example.com"}),
                    RiskLevel::High
                )
                .await,
            ConfirmationResult::Blocked { .. }
        ));
        assert!(matches!(
            gateway
                .check(None, "mcp::remote::search", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
        assert!(matches!(
            gateway
                .check(None, "mcp__remote__search", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
        assert!(matches!(
            gateway
                .check(None, "skill__echo", &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::Blocked { .. }
        ));
        assert!(matches!(
            gateway
                .check(
                    None,
                    "haven.mcp.add",
                    &json!({"operation": "add"}),
                    RiskLevel::High
                )
                .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn http_confirmation_binds_exact_origin_without_a_domain_whitelist() {
        let engine = Arc::new(AuthorizationEngine::new());
        engine
            .set_permission_interaction(Arc::new(
                super::super::consent::ApprovingPermissionInteraction,
            ))
            .await;
        engine
            .apply_security(&SecurityConfig {
                resource_policy: ResourcePolicy {
                    network: NetworkResourcePolicy {
                        access: NetworkAccessBoundary::Restricted,
                    },
                    ..ResourcePolicy::default()
                },
                ..SecurityConfig::default()
            })
            .await
            .expect("valid network policy applies");
        engine
            .set_tool_settings(HashMap::from([(
                "http".into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        network: NetworkResourcePolicy {
                            access: NetworkAccessBoundary::Inherit,
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            )]))
            .await;
        let input = json!({"url": "https://api.outside.example:8443/data?token=secret"});
        let policy = fixture_policy("http", &input, RiskLevel::Medium);
        let request = fixture_request(None, "http", &input, &policy).with_resources(vec![
            DeclaredResource::NetworkDestination {
                scheme: "https".into(),
                host: "api.outside.example".into(),
                port: Some(8443),
            },
        ]);

        let check = engine.authorize_for_execution(&request).await;
        let AuthorizationDecision::RequiresConfirmation {
            receipt,
            reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
            ..
        } = check.decision
        else {
            panic!("an HTTP origin outside the global domain list should request confirmation");
        };
        assert!(receipt.resource_boundary_extension);
        assert_eq!(receipt.resources, request.resources);

        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .expect("the exact confirmed origin should produce a proof");
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .expect("the proof should produce an HTTP execution context");
        assert_eq!(
            context.network_destination_extension(),
            request.resources.first()
        );
        assert_eq!(
            context.network_domain_constraints().unwrap(),
            &[vec!["api.outside.example".to_owned()]],
            "the execution context binds the exact approved origin"
        );

        let changed_origin = AuthorizationRequest::new(
            None,
            "http",
            json!({"url": "https://api.attacker.example:8443/data?token=secret"}),
            policy,
        )
        .with_resources(vec![DeclaredResource::NetworkDestination {
            scheme: "https".into(),
            host: "api.attacker.example".into(),
            port: Some(8443),
        }]);
        assert!(
            engine
                .verify_receipt(&changed_origin, &receipt)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn network_permission_requests_work_with_tool_defaults_and_unattended_mode() {
        let engine = AuthorizationEngine::new();
        let mut tool_settings = HashMap::new();
        tool_settings.insert(
            "http".into(),
            ToolConfig {
                resource_policy: ResourcePolicy {
                    network: NetworkResourcePolicy {
                        access: NetworkAccessBoundary::Inherit,
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        engine
            .apply_security(&SecurityConfig {
                resource_policy: ResourcePolicy {
                    network: NetworkResourcePolicy {
                        access: NetworkAccessBoundary::Restricted,
                    },
                    ..ResourcePolicy::default()
                },
                ..SecurityConfig::default()
            })
            .await
            .expect("valid global and tool network policies apply");
        engine.set_tool_settings(tool_settings).await;
        let input = json!({"url": "https://outside.example/data"});
        let policy = fixture_policy("http", &input, RiskLevel::Medium);
        let request = fixture_request(None, "http", &input, &policy).with_resources(vec![
            DeclaredResource::NetworkDestination {
                scheme: "https".into(),
                host: "outside.example".into(),
                port: Some(443),
            },
        ]);
        assert!(matches!(
            engine.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        let unattended_engine = AuthorizationEngine::new();
        unattended_engine
            .apply_security(&SecurityConfig {
                resource_policy: ResourcePolicy {
                    network: NetworkResourcePolicy {
                        access: NetworkAccessBoundary::Restricted,
                    },
                    ..ResourcePolicy::default()
                },
                ..SecurityConfig::default()
            })
            .await
            .expect("valid global network policy applies");
        unattended_engine
            .set_execution_mode(ExecutionMode::Unattended)
            .await;
        let check = unattended_engine.authorize(&request).await;
        assert!(matches!(
            check,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        let analyze_engine = AuthorizationEngine::new();
        analyze_engine
            .apply_security(&SecurityConfig {
                resource_policy: ResourcePolicy {
                    network: NetworkResourcePolicy {
                        access: NetworkAccessBoundary::Restricted,
                    },
                    ..ResourcePolicy::default()
                },
                ..SecurityConfig::default()
            })
            .await
            .expect("valid global network policy applies");
        analyze_engine
            .set_execution_mode(ExecutionMode::Analyze)
            .await;
        assert!(matches!(
            analyze_engine.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn sandbox_boundaries_request_write_authority_outside_default_roots() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let read_only = SecurityConfig {
            resource_policy: ResourcePolicy {
                files: FileResourcePolicy {
                    write_mode: FileWriteMode::ReadOnly,
                    ..FileResourcePolicy::default()
                },
                ..ResourcePolicy::default()
            },
            ..SecurityConfig::default()
        };
        gateway
            .apply_security(&read_only)
            .await
            .expect("valid read-only security applies");
        let read_only_write_params = json!({"operation": "write", "path": "notes.md"});
        let mut read_only_write_policy =
            fixture_policy("files", &read_only_write_params, RiskLevel::Medium);
        read_only_write_policy.effect = OperationEffect::WorkspaceWrite;
        assert!(matches!(
            authorize(
                &gateway,
                None,
                "files",
                &read_only_write_params,
                &read_only_write_policy,
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));

        let root = std::env::temp_dir().join("haven-permission-root");
        gateway
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                vec![root.clone()],
                NetworkPolicy::Restricted,
            )
            .await;
        let write_policy = OperationPolicy {
            risk_level: RiskLevel::Medium,
            capability: capability_scope!("files.write"),
            confirmation: ConfirmationRequirement::SecurityPolicy,
            idempotency: crate::OperationIdempotency::NonIdempotent,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::WorkspaceWrite,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::None,
            execution_boundary: crate::ExecutionBoundary::InProcess,
        };
        let outside = std::env::temp_dir().join("haven-permission-outside.txt");
        let outside_decision = authorize(
            &gateway,
            None,
            "files.write",
            &json!({"path": outside}),
            &write_policy,
        )
        .await;
        assert!(
            matches!(
                &outside_decision,
                ConfirmationResult::RequiresConfirmation {
                    receipt,
                    reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
                    ..
                } if receipt.resource_boundary_extension
                    && receipt.resources.iter().any(|resource| matches!(
                        resource,
                        DeclaredResource::LocalPath { path, access: LocalPathAccess::Write }
                            if path == &outside
                    ))
            ),
            "unexpected outside-root decision: {outside_decision:?}"
        );
        assert!(matches!(
            authorize(
                &gateway,
                None,
                "files.write",
                &json!({"path": root.join("notes.md")}),
                &write_policy,
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn confirmed_workspace_write_extension_is_bound_to_the_exact_target() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("approved.txt");
        let sibling = outside.path().join("sibling.txt");
        let engine = Arc::new(AuthorizationEngine::new_with_workspace(
            ExecutionWorkspace::from_root(workspace.path()).unwrap(),
        ));
        engine
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        let input = json!({"path": target});
        let mut policy = fixture_policy("files.write", &input, RiskLevel::Medium);
        policy.effect = OperationEffect::WorkspaceWrite;
        policy.idempotency = crate::OperationIdempotency::NonIdempotent;
        policy.network_access = NetworkAccess::None;
        let request = fixture_request(None, "files.write", &input, &policy);
        let check = engine.authorize_for_execution(&request).await;
        let AuthorizationDecision::RequiresConfirmation {
            receipt,
            reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
            ..
        } = check.decision
        else {
            panic!("outside workspace writes must request one-call boundary extension");
        };
        assert!(receipt.resource_boundary_extension);
        assert_eq!(
            receipt.resources,
            vec![DeclaredResource::LocalPath {
                path: target.clone(),
                access: LocalPathAccess::Write,
            }]
        );

        let proof = engine
            .confirm_for_execution(&request, &receipt)
            .await
            .expect("the confirmed exact request should produce a proof");
        let context = engine
            .execution_context_for_proof(&request, &proof)
            .await
            .expect("the matching proof should produce an execution context");
        let file_policy = context
            .file_write_policy
            .expect("file writes carry a proof-derived path policy");
        assert!(file_policy.resolve(&target).is_ok());
        assert_eq!(
            file_policy.resolve(&sibling).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the parent directory capability must not authorize a sibling file"
        );
    }

    #[tokio::test]
    async fn external_open_requests_confirmation_outside_workspace_roots() {
        let engine = AuthorizationEngine::new();
        let writable_root = tempfile::tempdir().unwrap();
        let target = "https://auth.openai.com/oauth/authorize?client_id=public";
        let params = json!({"target": target});
        let policy = OperationPolicy::native(
            "open_external",
            CapabilityScope::try_new(permission_key("open_external", &params)).unwrap(),
            RiskLevel::Low,
            NetworkAccess::Opaque,
        );

        for sandbox_mode in [SandboxMode::WorkspaceWrite, SandboxMode::ReadOnly] {
            engine
                .set_boundaries(
                    sandbox_mode,
                    vec![writable_root.path().to_path_buf()],
                    NetworkPolicy::Open,
                )
                .await;

            let decision = authorize(&engine, None, "open_external", &params, &policy).await;

            assert!(
                matches!(decision, AuthorizationDecision::RequiresConfirmation { .. }),
                "open_external should reach confirmation in {sandbox_mode:?}, got {decision:?}"
            );
        }
    }

    #[tokio::test]
    async fn workspace_write_defaults_to_the_captured_execution_workspace() {
        let workspace = ExecutionWorkspace::from_root(&std::env::current_dir().unwrap()).unwrap();
        let root = workspace.root().unwrap().to_path_buf();
        let outside = root
            .parent()
            .expect("workspace checkout has a parent directory")
            .join("haven-workspace-outside.md");
        let gateway = AuthorizationEngine::new_with_workspace(workspace.clone());
        gateway
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        let mut policy = fixture_policy("files.write", &json!({}), RiskLevel::Medium);
        policy.effect = OperationEffect::WorkspaceWrite;
        policy.idempotency = crate::OperationIdempotency::NonIdempotent;
        policy.network_access = NetworkAccess::None;

        let outside_decision = authorize(
            &gateway,
            None,
            "files.write",
            &json!({"path": outside}),
            &policy,
        )
        .await;
        assert!(
            matches!(
                &outside_decision,
                ConfirmationResult::RequiresConfirmation {
                    receipt,
                    reason_code: AuthorizationReasonCode::ResourceBoundaryExtension,
                    ..
                } if receipt.resource_boundary_extension
                    && receipt.resources.iter().any(|resource| matches!(
                        resource,
                        DeclaredResource::LocalPath { path, access: LocalPathAccess::Write }
                            if path == &outside
                    ))
            ),
            "unexpected outside-workspace decision: {outside_decision:?}"
        );

        let inside_path = root.join("notes.md");
        let canonical_root = resolve_path_without_reparse(workspace.root().unwrap())
            .expect("workspace root should resolve without reparse points");
        let canonical_inside = resolve_path_without_reparse(&inside_path)
            .expect("new file under workspace should resolve through its existing parent");
        assert!(
            path_is_within(&canonical_inside, &canonical_root),
            "resolved file path must be within the resolved workspace root"
        );
        let inside_decision = authorize(
            &gateway,
            None,
            "files.write",
            &json!({"path": inside_path}),
            &policy,
        )
        .await;
        assert!(
            matches!(
                &inside_decision,
                ConfirmationResult::RequiresConfirmation { .. }
            ),
            "unexpected inside-workspace decision: {inside_decision:?}"
        );

        assert!(matches!(
            authorize(
                &gateway,
                None,
                "files.write",
                &json!({"path": "notes.md"}),
                &policy,
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn workspace_write_fails_closed_without_a_workspace_or_explicit_roots() {
        let gateway = AuthorizationEngine::new_with_workspace(ExecutionWorkspace::unavailable());
        gateway
            .set_boundaries(
                SandboxMode::WorkspaceWrite,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;
        let mut policy = fixture_policy("files.write", &json!({}), RiskLevel::Medium);
        policy.effect = OperationEffect::WorkspaceWrite;
        policy.idempotency = crate::OperationIdempotency::NonIdempotent;
        policy.network_access = NetworkAccess::None;

        let decision = authorize(
            &gateway,
            None,
            "files.write",
            &json!({"path": "C:\\outside\\notes.md"}),
            &policy,
        )
        .await;
        assert!(matches!(
            decision,
            ConfirmationResult::Blocked {
                reason_code: AuthorizationReasonCode::SandboxBoundary,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn risk_override_can_raise_but_never_lower_intrinsic_risk() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let mut settings = HashMap::new();
        settings.insert(
            "system".into(),
            ToolConfig {
                risk_override: Some(RiskLevel::Safe),
                ..ToolConfig::default()
            },
        );
        gateway.set_tool_settings(settings).await;
        let decision = gateway
            .check(
                None,
                "system",
                &json!({"scope": "power", "operation": "hibernate"}),
                RiskLevel::Critical,
            )
            .await;
        let ConfirmationResult::RequiresConfirmation {
            risk_level,
            receipt,
            ..
        } = decision
        else {
            panic!("a Critical intrinsic risk must remain gated");
        };
        assert_eq!(risk_level, RiskLevel::Critical);
        assert_eq!(receipt.effective_risk, RiskLevel::Critical);
    }

    #[tokio::test]
    async fn confirmation_receipt_is_bound_to_input_and_policy_revision() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let safe_input = json!({"command": "echo safe"});
        let safe_policy = fixture_policy("shell", &safe_input, RiskLevel::High);
        let decision = gateway
            .check(None, "shell", &safe_input, RiskLevel::High)
            .await;
        let ConfirmationResult::RequiresConfirmation { receipt, .. } = decision else {
            panic!("High-risk shell call should require confirmation");
        };
        let safe_request =
            AuthorizationRequest::new(None, "shell", safe_input.clone(), safe_policy.clone())
                .with_resources(threshold_fixture_resource());
        gateway
            .verify_receipt(&safe_request, &receipt)
            .await
            .unwrap();
        let changed_input = json!({"command": "echo changed"});
        let changed_policy = fixture_policy("shell", &changed_input, RiskLevel::High);
        assert!(
            gateway
                .verify_receipt(
                    &AuthorizationRequest::new(None, "shell", changed_input, changed_policy)
                        .with_resources(threshold_fixture_resource()),
                    &receipt,
                )
                .await
                .is_err()
        );

        gateway
            .set_execution_mode(ExecutionMode::Collaborative)
            .await;
        assert!(
            gateway
                .verify_receipt(&safe_request, &receipt)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn authorization_request_and_receipt_are_bound_to_typed_capability() {
        let gateway = ThresholdFixture::new(RiskLevel::Safe);
        let input = json!({"command": "echo safe"});
        let policy = fixture_policy("shell", &input, RiskLevel::High);
        let request = AuthorizationRequest::new(None, "shell", input, policy);
        let AuthorizationDecision::RequiresConfirmation {
            receipt,
            capability,
            ..
        } = gateway.authorize(&request).await
        else {
            panic!("High-risk shell call should require confirmation");
        };
        assert_eq!(capability, request.policy.capability);
        assert_eq!(receipt.capability, request.policy.capability);

        let wrong_request = AuthorizationRequest::new(
            None,
            "shell",
            json!({"command": "echo safe"}),
            OperationPolicy {
                capability: capability_scope!("system.get_info"),
                ..request.policy.clone()
            },
        );
        assert!(
            gateway
                .verify_receipt(&wrong_request, &receipt)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn execution_proof_is_bound_to_the_execution_boundary() {
        let gateway = AuthorizationEngine::new();
        let request = AuthorizationRequest::new(
            None,
            "files.read",
            json!({}),
            fixture_policy("files.read", &json!({}), RiskLevel::Safe),
        );
        let mut check = gateway.authorize_for_execution(&request).await;
        let proof = check
            .execution_proof
            .take()
            .expect("safe operation should receive an execution proof");
        gateway
            .verify_execution_proof(&request, &proof)
            .await
            .expect("matching execution boundary should validate");

        let changed_boundary = AuthorizationRequest {
            policy: OperationPolicy {
                execution_boundary: crate::ExecutionBoundary::HostProcess,
                ..request.policy.clone()
            },
            ..request.clone()
        };
        assert!(
            gateway
                .verify_execution_proof(&changed_boundary, &proof)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn test_authorization_below_threshold_auto_approved() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        // Low is below Medium → auto approved
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Low).await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_authorization_at_threshold_requires_confirmation() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        // Medium is at the threshold → requires confirmation
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::Medium).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_authorization_above_threshold_requires_confirmation() {
        let gw = ThresholdFixture::new(RiskLevel::Low);
        // High is above Low → requires confirmation
        let result = gw.check(None, "tool1", &json!({}), RiskLevel::High).await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_authorization_session_allow_tool_key() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("tool1"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        let result = gw
            .check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_authorization_session_allow_is_per_session_and_per_tool() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("tool1"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(
                Some("ses-f54924ed1bcde71cb1d3e0e9d437e3e4"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool2",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(None, "tool1", &json!({}), RiskLevel::Medium).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_authorization_permanent_deny_blocks() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            None,
            capability_scope!("shell"),
            PermissionEffect::Deny,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        let result = gw.check(None, "shell", &json!({}), RiskLevel::Safe).await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn revoke_permanent_rejects_invalid_capability_without_mutating_rules() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            None,
            capability_scope!("files.read"),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;

        assert!(!gw.revoke_permanent("files:read").await);
        assert_eq!(gw.list_permanent().await.len(), 1);
    }

    #[tokio::test]
    async fn test_authorization_parent_key_matches_operation() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("files"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        let result = gw
            .check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn test_authorization_clear_session_trust() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("tool1"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::AutoApproved
        ));

        gw.clear_session_trust("ses-8042867cb780e3dfc14959c1fe8d87e1")
            .await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn permanent_permission_revoke_preserves_session_capability() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("tool1"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        gw.grant(
            Some("ses-f54924ed1bcde71cb1d3e0e9d437e3e4"),
            capability_scope!("tool1"),
            PermissionEffect::Deny,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::AutoApproved
        ));

        assert!(!gw.revoke_permanent("tool1").await);
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(
            gw.revoke_session_grant(
                "ses-8042867cb780e3dfc14959c1fe8d87e1",
                &CapabilityScope::try_new("tool1").unwrap()
            )
            .await
        );
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(
                Some("ses-f54924ed1bcde71cb1d3e0e9d437e3e4"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::Blocked { .. }
        ));
    }

    #[tokio::test]
    async fn clear_permanent_preserves_session_rules() {
        let gw = ThresholdFixture::new(RiskLevel::Safe);
        gw.grant(
            None,
            capability_scope!("shell"),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("files"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;

        assert_eq!(gw.clear_permanent().await, 1);
        assert!(gw.list_permanent().await.is_empty());
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "files",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(
            gw.revoke_session_grant(
                "ses-8042867cb780e3dfc14959c1fe8d87e1",
                &CapabilityScope::try_new("files").unwrap()
            )
            .await
        );
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "files",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn critical_operations_cannot_be_bypassed_by_allow_grants() {
        let gw = ThresholdFixture::new(RiskLevel::Safe);
        gw.grant(
            None,
            capability_scope!("system.hibernate"),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        assert!(matches!(
            gw.check(None, "system.hibernate", &json!({}), RiskLevel::Critical)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_authorization_set_mode_clears_session_grants() {
        let gw = ThresholdFixture::new(RiskLevel::Low);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("tool1"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::AutoApproved
        ));

        gw.set_execution_mode(ExecutionMode::Collaborative).await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Medium
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "tool1",
                &json!({}),
                RiskLevel::Low
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_authorization_disabled_operation_blocks() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                disabled_operations: vec!["delete".into()],
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[test]
    fn hard_policy_boundaries_preserve_shared_failure_precedence() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let engine = AuthorizationEngine::new();
        let mut cfg = engine.config.try_read().unwrap().clone();
        cfg.tool_settings.insert(
            "files.write".into(),
            ToolConfig {
                disabled_operations: vec!["files.write".into()],
                resource_policy: ResourcePolicy {
                    files: FileResourcePolicy {
                        path_roots: vec![allowed.path().to_string_lossy().into_owned()],
                        ..FileResourcePolicy::default()
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        let capability = CapabilityScope::try_new("files.write").unwrap();
        let policy = OperationPolicy {
            risk_level: RiskLevel::Medium,
            capability: capability.clone(),
            confirmation: ConfirmationRequirement::SecurityPolicy,
            idempotency: crate::OperationIdempotency::NonIdempotent,
            scope: crate::ToolOperationScope::Session,
            concurrency: ToolConcurrency::Exclusive,
            effect: OperationEffect::WorkspaceWrite,
            data_sensitivity: DataSensitivity::None,
            network_access: NetworkAccess::Public,
            execution_boundary: crate::ExecutionBoundary::HostProcess,
        };
        let resources = vec![crate::security::DeclaredResource::LocalPath {
            path: outside.path().join("blocked.txt"),
            access: crate::security::LocalPathAccess::Write,
        }];
        let check = |cfg: &SafetyConfig| {
            check_policy_boundaries(
                cfg,
                &cfg.execution_workspace,
                "files.write",
                &capability,
                &resources,
                &policy,
            )
            .unwrap_err()
            .reason_code
        };

        assert_eq!(
            check(&cfg),
            AuthorizationReasonCode::DisabledOperation,
            "disabled operation must win over sandbox, network and path failures"
        );
        cfg.tool_settings
            .get_mut("files.write")
            .unwrap()
            .disabled_operations
            .clear();
        assert_eq!(
            check(&cfg),
            AuthorizationReasonCode::SandboxBoundary,
            "host-process sandbox failure must win over network and path failures"
        );
        cfg.sandbox_mode = SandboxMode::FullAccess;
        cfg.network.access = NetworkAccessBoundary::Deny;
        assert_eq!(
            check(&cfg),
            AuthorizationReasonCode::NetworkPolicy,
            "network failure must win over the path allowlist"
        );
        cfg.network.access = NetworkAccessBoundary::Open;
        assert_eq!(
            check(&cfg),
            AuthorizationReasonCode::SandboxBoundary,
            "path allowlist remains the final shared hard-boundary check"
        );
    }

    #[tokio::test]
    async fn disabled_operation_matching_uses_canonical_capability_identity() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.set_tool_settings(HashMap::from([(
            "files".into(),
            ToolConfig {
                disabled_operations: vec!["read".into()],
                ..ToolConfig::default()
            },
        )]))
        .await;

        // The discriminator is intentionally inconsistent with the operation
        // view. Authorization must follow the registered capability identity,
        // not re-interpret arbitrary input fields.
        let result = gw
            .check(
                None,
                "files.read",
                &json!({"operation": "delete", "path": "notes.md"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn authorization_decisions_expose_reason_codes_without_parsing_text() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.set_tool_settings(HashMap::from([(
            "files".into(),
            ToolConfig {
                disabled_operations: vec!["read".into()],
                ..ToolConfig::default()
            },
        )]))
        .await;
        let decision = gw
            .check(
                None,
                "files.read",
                &json!({"operation": "read"}),
                RiskLevel::Low,
            )
            .await;
        let AuthorizationDecision::Blocked { reason_code, .. } = decision else {
            panic!("expected disabled capability to be blocked");
        };
        assert_eq!(reason_code, AuthorizationReasonCode::DisabledOperation);
        assert_eq!(reason_code.as_str(), "disabled_operation");
    }

    #[tokio::test]
    async fn invalid_persisted_capability_rules_fail_closed() {
        let gw = ThresholdFixture::new(RiskLevel::Safe);
        gw.apply_security(&SecurityConfig {
            permissions: vec![StoredPermission {
                key: "files:read".into(),
                effect: PermissionEffect::Allow,
                resource_scope: Vec::new(),
            }],
            ..SecurityConfig::default()
        })
        .await
        .expect("invalid grants are ignored while security applies");

        assert!(gw.list_permanent().await.is_empty());
    }

    #[tokio::test]
    async fn operation_view_inherits_aggregate_tool_safety_settings() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                disabled_operations: vec!["read".into()],
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files.read",
                &json!({"operation": "read", "path": "notes.md"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::Blocked { .. }));
    }

    #[tokio::test]
    async fn unattended_mode_requests_workspace_host_process_authority() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        let security = SecurityConfig {
            execution_mode: ExecutionMode::Unattended,
            ..SecurityConfig::default()
        };
        gw.apply_security(&security)
            .await
            .expect("valid security workspace applies");
        assert!(matches!(
            gw.check(None, "shell", &json!({}), RiskLevel::High).await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
        assert!(matches!(
            gw.check(
                None,
                "system",
                &json!({"scope":"power","operation":"hibernate"}),
                RiskLevel::Critical
            )
            .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_authorization_permanent_allow_precedes_session_deny() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            None,
            capability_scope!("files"),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("files"),
            PermissionEffect::Deny,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
    }

    #[tokio::test]
    async fn test_authorization_child_allow_precedes_parent_deny() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            None,
            capability_scope!("files"),
            PermissionEffect::Deny,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        gw.grant(
            None,
            capability_scope!("files.read"),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;

        assert!(matches!(
            gw.check(None, "files", &json!({"operation": "read"}), RiskLevel::Low)
                .await,
            ConfirmationResult::AutoApproved
        ));
    }

    #[tokio::test]
    async fn test_authorization_parent_allow_precedes_child_deny() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            None,
            capability_scope!("files"),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        gw.grant(
            None,
            capability_scope!("files.delete"),
            PermissionEffect::Deny,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;

        assert!(matches!(
            gw.check(
                None,
                "files",
                &json!({"operation": "delete"}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
    }

    #[tokio::test]
    async fn test_authorization_session_child_allow_precedes_parent_deny() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("system.power"),
            PermissionEffect::Deny,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("system.power.lock"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;

        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "system",
                &json!({"scope": "power", "operation": "lock"}),
                RiskLevel::High,
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
    }

    #[tokio::test]
    async fn test_authorization_session_allow_precedes_permanent_deny() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.grant(
            None,
            capability_scope!("shell"),
            PermissionEffect::Deny,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!("shell"),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;

        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                "shell",
                &json!({}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
    }

    #[tokio::test]
    async fn test_local_tool_security_matrix_gates_every_risk_bearing_case() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        for case in LOCAL_TOOL_SECURITY_MATRIX {
            let input = matrix_input(case);
            let result = gw
                .check(None, case.tool_name, &input, case.risk_level)
                .await;
            if case.risk_level >= RiskLevel::Medium {
                assert!(
                    matches!(result, ConfirmationResult::RequiresConfirmation { .. }),
                    "{}:{} should be gated, got {result:?}",
                    case.tool_name,
                    case.operation
                );
            } else {
                assert!(
                    matches!(result, ConfirmationResult::AutoApproved),
                    "{}:{} should be automatic, got {result:?}",
                    case.tool_name,
                    case.operation
                );
            }
        }
    }

    #[tokio::test]
    async fn permanent_deny_blocks_every_registered_operation_view() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        for case in LOCAL_TOOL_SECURITY_MATRIX {
            let input = matrix_input(case);
            let key = permission_key(case.tool_name, &input);
            gw.grant(
                None,
                capability_scope!(&key),
                PermissionEffect::Deny,
                PermissionScope::Always,
                Vec::new(),
            )
            .await;
            assert!(
                matches!(
                    gw.check(None, case.tool_name, &input, case.risk_level)
                        .await,
                    ConfirmationResult::Blocked { .. }
                ),
                "permanent deny must block {}:{}",
                case.tool_name,
                case.operation
            );
        }
    }

    /// Extract the routing fields from the operation branches of a tool
    /// schema. This intentionally inspects the provider-facing schema rather
    /// than duplicating each tool's operation list in the test.
    fn schema_route_inputs(schema: &Value) -> Vec<Value> {
        let mut local_variants = vec![serde_json::Map::new()];
        for field in ["scope", "operation"] {
            let Some(property) = schema.get("properties").and_then(|p| p.get(field)) else {
                continue;
            };
            let values = if let Some(value) = property.get("const") {
                vec![value.clone()]
            } else {
                property
                    .get("enum")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            };
            if values.is_empty() {
                continue;
            }
            let mut expanded = Vec::new();
            for variant in local_variants {
                for value in &values {
                    let mut next = variant.clone();
                    next.insert(field.to_string(), value.clone());
                    expanded.push(next);
                }
            }
            local_variants = expanded;
        }

        if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
            let mut routes = Vec::new();
            for branch in branches {
                routes.extend(schema_route_inputs(branch));
            }
            // A top-level schema often carries a broad operation enum while
            // its oneOf branches carry the authoritative route. Nested
            // oneOf branches (for example schedule.set's timing alternatives)
            // may carry no routing fields; in that case retain the local
            // const/enum route instead of producing a spurious empty route.
            if routes.iter().any(route_has_selector) {
                return dedupe_json(routes);
            }
        }
        dedupe_json(local_variants.into_iter().map(Value::Object).collect())
    }

    fn route_has_selector(value: &Value) -> bool {
        value.get("scope").is_some() || value.get("operation").is_some()
    }

    fn dedupe_json(values: Vec<Value>) -> Vec<Value> {
        let mut seen = HashSet::new();
        values
            .into_iter()
            .filter(|value| seen.insert(value.to_string()))
            .collect()
    }

    fn route_label(tool_name: &str, input: &Value) -> String {
        if tool_name == "files"
            && input["operation"].as_str() == Some("search")
            && input["mode"].as_str() == Some("content")
        {
            return "search:content".into();
        }
        if tool_name == "files.search" && input["mode"].as_str() == Some("content") {
            return "search:content".into();
        }
        if tool_name.contains('.') {
            return tool_name.to_string();
        }
        input["operation"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| match tool_name {
                "ask" => "ask".into(),
                "toolrun" => "list".into(),
                "shell" => "execute".into(),
                "http" => "request".into(),
                "notify" => "notify".into(),
                "load_skill" => "load".into(),
                "discover_tools" => "discover_tools".into(),
                "load_mcp" => "load".into(),
                other => other.to_string(),
            })
    }

    fn matrix_input(case: &LocalToolSecurityCase) -> Value {
        if case.operation == "search:content" {
            return serde_json::json!({"mode": "content"});
        }
        match case.tool_name {
            name if name.contains('.') => serde_json::json!({}),
            "ask" | "shell" | "http" | "notify" | "load_skill" | "load_mcp" => {
                serde_json::json!({})
            }
            "discover_tools" => serde_json::json!({"action": "list"}),
            _ => serde_json::json!({"operation": case.operation}),
        }
    }

    #[tokio::test]
    async fn test_builtin_registry_security_contract_covers_every_route() {
        use crate::ToolsFacade;
        use crate::builtin::AdminContext;
        use haven_common::config::ConfigLoader;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let manager = ToolsFacade::new();
        manager
            .set_admin_context(AdminContext {
                config_service: Some(Arc::new(test_config_service(loader))),
                config_apply_gate: None,
                session_store: None,
                session_working_document: None,
                memory_facts: None,
                model_media_runtime: None,
                log_path: None,
                file_logging_enabled: false,
                log_level: None,
                tool_control: None,
            })
            .await
            .unwrap();

        let mut tools = manager.registry().list().await;
        // Deferred builtins are intentionally absent from the model-facing
        // registry, but they still need the same security-contract coverage.
        // Resolve them through the control-plane lookup so this test covers
        // both the eager and lazy portions of the builtin catalog.
        let registry_names: HashSet<_> = tools.iter().map(|tool| tool.name()).collect();
        for def in manager.list_enabled_builtin_tool_definitions().await {
            if !registry_names.contains(&def.name) {
                tools.push(
                    manager
                        .get_tool(&def.name)
                        .await
                        .unwrap_or_else(|| panic!("missing deferred builtin {}", def.name)),
                );
            }
        }
        let matrix_names: HashSet<_> = LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .map(|case| case.tool_name)
            .collect();
        let registry_names: HashSet<_> = tools.iter().map(|tool| tool.name()).collect();
        assert!(
            registry_names
                .iter()
                .all(|name| matrix_names.contains(name.as_str())),
            "security matrix must cover every currently registered builtin route"
        );

        let gateway = ThresholdFixture::new(RiskLevel::Medium);
        let mut seen = HashSet::new();
        for tool in tools {
            let name = tool.name();
            let mut inputs = schema_route_inputs(&tool.input_schema());
            // `files:search` has a mode-dependent risk level. The schema
            // operation is one route, but both risk-bearing modes need a
            // contract assertion.
            if name == "files" || name == "files.search" {
                let content_search = serde_json::json!({
                    "operation": "search",
                    "mode": "content"
                });
                if !inputs.iter().any(|input| input == &content_search) {
                    inputs.push(content_search);
                }
            }
            assert!(!inputs.is_empty(), "{name} must expose a contract route");

            for input in inputs {
                let operation = route_label(&name, &input);
                let case = LOCAL_TOOL_SECURITY_MATRIX
                    .iter()
                    .find(|case| case.tool_name == name && case.operation == operation)
                    .unwrap_or_else(|| {
                        panic!("missing security matrix row for {name}:{operation}")
                    });
                let reported_risk = tool.risk_level(&input);
                assert_eq!(
                    reported_risk, case.risk_level,
                    "risk level drift for {name}:{operation}"
                );

                let key = permission_key(&name, &input);
                assert!(
                    !key.is_empty(),
                    "empty permission key for {name}:{operation}"
                );
                assert_eq!(
                    haven_common::types::permission_tool_root(&key),
                    haven_common::types::permission_tool_root(&name),
                    "permission key must remain rooted at the registered tool"
                );
                assert_eq!(
                    key,
                    permission_key(&name, &matrix_input(case)),
                    "permission key drift for {name}:{operation}"
                );

                let decision = gateway.check(None, &name, &input, reported_risk).await;
                if reported_risk >= RiskLevel::Medium {
                    assert!(
                        matches!(decision, ConfirmationResult::RequiresConfirmation { .. }),
                        "{name}:{operation} with {reported_risk:?} must be gated"
                    );
                } else {
                    assert!(
                        matches!(decision, ConfirmationResult::AutoApproved),
                        "{name}:{operation} with {reported_risk:?} must be automatic"
                    );
                }
                seen.insert((name.clone(), operation));
            }
        }

        let optional_routes = [
            ("media.describe", "media.describe"),
            ("media.run_ocr", "media.run_ocr"),
            ("media.transcribe", "media.transcribe"),
            ("media.generate", "media.generate"),
            ("media.record", "media.record"),
            ("media.speak", "media.speak"),
            ("load_skill", "load"),
            ("load_mcp", "load"),
            ("agent.list_children", "agent.list_children"),
            ("agent.read_history", "agent.read_history"),
            ("agent.acknowledge", "agent.acknowledge"),
            ("agent.get_status", "agent.get_status"),
            ("agent.wait", "agent.wait"),
            ("agent.stop", "agent.stop"),
        ];
        for case in LOCAL_TOOL_SECURITY_MATRIX {
            if !seen.contains(&(case.tool_name.to_string(), case.operation.to_string())) {
                assert!(
                    optional_routes.contains(&(case.tool_name, case.operation)),
                    "matrix row is not advertised by the builtin catalog: {}:{}",
                    case.tool_name,
                    case.operation
                );
            }
        }
    }

    fn schema_example(schema: &Value) -> Option<Value> {
        schema_example_at(schema, 0)
    }

    fn schema_example_at(schema: &Value, depth: usize) -> Option<Value> {
        if depth > 12 {
            return None;
        }
        if let Some(value) = schema.get("const") {
            return Some(value.clone());
        }
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            return values.first().cloned();
        }
        for keyword in ["oneOf", "anyOf"] {
            if schema.get("properties").is_some() {
                continue;
            }
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
                for branch in branches {
                    let candidate = schema_example_at(branch, depth + 1).or_else(|| {
                        // Operation-view branches often carry only
                        // `required`; their properties live on the parent
                        // object. Build the smallest branch example from
                        // that shared property map.
                        let required = branch.get("required")?.as_array()?;
                        let properties = schema.get("properties")?.as_object()?;
                        let mut object = serde_json::Map::new();
                        for name in required.iter().filter_map(Value::as_str) {
                            let property = properties.get(name)?;
                            object.insert(name.into(), schema_example_at(property, depth + 1)?);
                        }
                        Some(Value::Object(object))
                    });
                    let Some(candidate) = candidate else { continue };
                    let Ok(validator) = jsonschema::validator_for(schema) else {
                        continue;
                    };
                    if validator.is_valid(&candidate) {
                        return Some(candidate);
                    }
                }
                return None;
            }
        }

        let schema_type = schema
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| schema.get("properties").map(|_| "object"));
        match schema_type {
            Some("object") => {
                let mut object = serde_json::Map::new();
                let properties = schema.get("properties").and_then(Value::as_object);
                let required = schema
                    .get("required")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str);
                for name in required {
                    let property = properties?.get(name)?;
                    object.insert(name.into(), schema_example_at(property, depth + 1)?);
                }
                for keyword in ["oneOf", "anyOf"] {
                    let Some(branches) = schema.get(keyword).and_then(Value::as_array) else {
                        continue;
                    };
                    for branch in branches {
                        let mut candidate = object.clone();
                        let branch_required = branch
                            .get("required")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str);
                        for name in branch_required {
                            if candidate.contains_key(name) {
                                continue;
                            }
                            let property = properties?.get(name)?;
                            candidate.insert(name.into(), schema_example_at(property, depth + 1)?);
                        }
                        let candidate = Value::Object(candidate);
                        let Ok(validator) = jsonschema::validator_for(schema) else {
                            return None;
                        };
                        if validator.is_valid(&candidate) {
                            return Some(candidate);
                        }
                    }
                    return None;
                }
                let candidate = Value::Object(object);
                let Ok(validator) = jsonschema::validator_for(schema) else {
                    return None;
                };
                validator.is_valid(&candidate).then_some(candidate)
            }
            Some("array") => {
                let min_items = schema
                    .get("minItems")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .min(4) as usize;
                let item = schema
                    .get("items")
                    .and_then(|items| schema_example_at(items, depth + 1));
                Some(Value::Array(
                    item.into_iter().cycle().take(min_items).collect(),
                ))
            }
            Some("string") => {
                if let Some(pattern) = schema.get("pattern").and_then(Value::as_str)
                    && let Some(rest) = pattern.strip_prefix('^')
                    && let Some((prefix, suffix)) = rest.split_once("[0-9a-f]")
                {
                    let count = suffix
                        .strip_prefix("{")
                        .and_then(|value| value.split_once('}'))
                        .and_then(|(value, _)| value.parse::<usize>().ok())
                        .unwrap_or(1);
                    return Some(Value::String(format!("{}{}", prefix, "0".repeat(count))));
                }
                let min_len = schema
                    .get("minLength")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .min(32) as usize;
                let mut value = "x".repeat(min_len.max(1));
                if schema.get("format").and_then(Value::as_str) == Some("uri") {
                    value = "https://example.invalid/".into();
                }
                if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
                    if pattern.contains("msg-") {
                        value = "msg-00000000000000000000000000000000".into();
                    }
                }
                if let Some(max_len) = schema.get("maxLength").and_then(Value::as_u64) {
                    value.truncate(max_len as usize);
                }
                Some(Value::String(value))
            }
            Some("integer") => Some(json!(
                schema.get("minimum").and_then(Value::as_i64).unwrap_or(0)
            )),
            Some("number") => Some(json!(
                schema.get("minimum").and_then(Value::as_f64).unwrap_or(0.0)
            )),
            Some("boolean") => Some(Value::Bool(false)),
            Some("null") => Some(Value::Null),
            // An empty schema intentionally means any JSON value (used by
            // preference values and opaque payloads); null is the smallest
            // valid representative.
            _ => Some(Value::Null),
        }
    }

    fn required_fields_for_valid_branch(schema: &Value, input: &Value) -> Vec<String> {
        for keyword in ["oneOf", "anyOf"] {
            if let Some(branches) = schema.get(keyword).and_then(Value::as_array)
                && let Some(branch) = branches.iter().find(|branch| {
                    jsonschema::validator_for(branch)
                        .map(|validator| validator.is_valid(input))
                        .unwrap_or(false)
                })
            {
                return required_fields_for_valid_branch(branch, input);
            }
        }
        schema
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn every_registered_operation_view_rejects_missing_and_unknown_fields() {
        use crate::ToolsFacade;
        use crate::builtin::AdminContext;
        use haven_common::config::ConfigLoader;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let manager = ToolsFacade::new();
        manager
            .set_admin_context(AdminContext {
                config_service: Some(Arc::new(test_config_service(loader))),
                config_apply_gate: None,
                session_store: None,
                session_working_document: None,
                memory_facts: None,
                model_media_runtime: None,
                log_path: None,
                file_logging_enabled: false,
                log_level: None,
                tool_control: None,
            })
            .await
            .unwrap();

        let mut tools = manager.registry().list().await;
        let registry_names: HashSet<_> = tools.iter().map(|tool| tool.name()).collect();
        for def in manager.list_enabled_builtin_tool_definitions().await {
            if !registry_names.contains(&def.name) {
                tools.push(
                    manager
                        .get_tool(&def.name)
                        .await
                        .unwrap_or_else(|| panic!("missing deferred builtin {}", def.name)),
                );
            }
        }
        let mut seen = HashSet::new();
        for tool in tools.iter().filter(|tool| tool.name().contains('.')) {
            let name = tool.name();
            let schema = tool.input_schema();
            let input = schema_example(&schema)
                .unwrap_or_else(|| panic!("{name} schema has no structural example: {schema}"));
            assert!(
                tool.validate_input(&input).is_ok(),
                "{name} rejected generated valid input {input}"
            );

            let required = required_fields_for_valid_branch(&schema, &input);
            for field in required {
                let mut missing = input.as_object().cloned().unwrap_or_default();
                missing.remove(&field);
                assert!(
                    tool.validate_input(&Value::Object(missing)).is_err(),
                    "{name} accepted input missing required field {field}"
                );
            }

            let mut unknown = input.as_object().cloned().unwrap_or_default();
            unknown.insert("__unexpected_field".into(), json!(true));
            assert!(
                tool.validate_input(&Value::Object(unknown)).is_err(),
                "{name} accepted an unknown field"
            );
            seen.insert(name);
        }

        assert!(
            seen.len() >= 20,
            "expected broad operation-view coverage, got {}: {:?}",
            seen.len(),
            seen
        );
        assert!(seen.contains("haven.config.set_log_level"));
        assert!(seen.contains("files.read"));
    }

    #[tokio::test]
    async fn every_registered_operation_view_declares_replay_timeout_and_cancel_contract() {
        use crate::ToolsFacade;
        use crate::builtin::AdminContext;
        use haven_common::config::ConfigLoader;
        use std::sync::Arc;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let manager = ToolsFacade::new();
        manager
            .set_admin_context(AdminContext {
                config_service: Some(Arc::new(test_config_service(loader))),
                config_apply_gate: None,
                session_store: None,
                session_working_document: None,
                memory_facts: None,
                model_media_runtime: None,
                log_path: None,
                file_logging_enabled: false,
                log_level: None,
                tool_control: None,
            })
            .await
            .unwrap();

        let mut tools = manager.registry().list().await;
        let registry_names: HashSet<_> = tools.iter().map(|tool| tool.name()).collect();
        for def in manager.list_enabled_builtin_tool_definitions().await {
            if !registry_names.contains(&def.name) {
                tools.push(
                    manager
                        .get_tool(&def.name)
                        .await
                        .unwrap_or_else(|| panic!("missing deferred builtin {}", def.name)),
                );
            }
        }

        let mut seen = HashSet::new();
        let expected_unknown = [
            "haven.skills.create",
            "haven.mcp.add",
            "haven.mcp.update",
            "haven.mcp.remove",
            "haven.mcp.reload",
        ];
        for tool in tools.iter().filter(|tool| tool.name().contains('.')) {
            let name = tool.name();
            let input = schema_example(&tool.input_schema())
                .unwrap_or_else(|| panic!("{name} schema has no structural example"));
            assert!(
                tool.validate_input(&input).is_ok(),
                "{name} rejected {input}"
            );
            let policy = tool.operation_policy(&input);
            if expected_unknown.contains(&name.as_str()) {
                assert_eq!(
                    policy.idempotency,
                    crate::OperationIdempotency::Unknown,
                    "{name} must preserve its explicitly unknown replay semantics"
                );
            } else {
                assert_ne!(
                    policy.idempotency,
                    crate::OperationIdempotency::Unknown,
                    "{name} must declare duplicate-call semantics"
                );
            }
            assert!(
                tool.timeout_secs_for(&input) > 0,
                "{name} must declare a positive timeout"
            );
            let timeout_secs = tool.timeout_secs_for(&input);

            let cancel = CancellationToken::new();
            cancel.cancel();
            let result = tool
                .execute_with_timeout(input, cancel, timeout_secs)
                .await
                .unwrap_or_else(|error| panic!("{name} lost cancellation result: {error}"));
            assert_eq!(
                result.outcome,
                ToolExecutionOutcome::Cancelled,
                "{name} must stop before a cancelled operation runs"
            );
            assert_eq!(result.attempts, 1);
            seen.insert(name);
        }

        assert!(
            seen.len() >= 100,
            "operation-view contract coverage is too small"
        );
    }

    #[tokio::test]
    async fn test_adapter_authorization_is_shared_but_session_scoped() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        let mcp_name = crate::McpToolAdapter::qualified_name_of("calendar", "create_event");
        let skill_name = crate::skill_tool_name("calendar");

        gw.grant(
            None,
            capability_scope!(&mcp_name),
            PermissionEffect::Allow,
            PermissionScope::Always,
            Vec::new(),
        )
        .await;
        gw.grant(
            Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
            capability_scope!(&skill_name),
            PermissionEffect::Allow,
            PermissionScope::Session,
            Vec::new(),
        )
        .await;

        assert!(matches!(
            gw.check(None, &mcp_name, &json!({}), RiskLevel::High).await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(
                Some("ses-8042867cb780e3dfc14959c1fe8d87e1"),
                &skill_name,
                &json!({}),
                RiskLevel::High
            )
            .await,
            ConfirmationResult::AutoApproved
        ));
        assert!(matches!(
            gw.check(None, &skill_name, &json!({}), RiskLevel::High)
                .await,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn test_local_tool_security_matrix_covers_every_builtin_family() {
        let names: HashSet<_> = LOCAL_TOOL_SECURITY_MATRIX
            .iter()
            .map(|case| case.tool_name)
            .collect();
        for expected in [
            "media.inspect",
            "ask",
            "files.read",
            "shell",
            "system.get_info",
            "http",
            "notify",
            "agent.list",
            "load_mcp",
            "memory.search",
            "haven.diagnostics.get_status",
        ] {
            assert!(
                names.contains(expected),
                "missing matrix family: {expected}"
            );
        }
    }

    #[tokio::test]
    async fn test_path_scope_requests_source_and_destination_authority() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                resource_policy: ResourcePolicy {
                    files: FileResourcePolicy {
                        path_roots: vec![allowed.path().to_string_lossy().into_owned()],
                        ..FileResourcePolicy::default()
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        let result = gw
            .check(
                None,
                "files",
                &json!({
                    "operation": "copy",
                    "source": allowed.path().join("source.txt"),
                    "destination": outside.path().join("destination.txt"),
                }),
                RiskLevel::Medium,
            )
            .await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_path_scope_requests_media_audio_file_authority() {
        let allowed = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut settings = HashMap::new();
        settings.insert(
            "media".into(),
            ToolConfig {
                resource_policy: ResourcePolicy {
                    files: FileResourcePolicy {
                        path_roots: vec![allowed.path().to_string_lossy().into_owned()],
                        ..FileResourcePolicy::default()
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        let result = gw
            .check(
                None,
                "media",
                &json!({
                    "operation": "play",
                    "file_path": outside.path().join("outside.wav"),
                }),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_path_scope_requests_parent_access_authority() {
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                resource_policy: ResourcePolicy {
                    files: FileResourcePolicy {
                        path_roots: vec!["C:\\allowed".into()],
                        ..FileResourcePolicy::default()
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        gw.set_tool_settings(settings).await;
        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "read", "path": "C:\\allowed\\..\\Windows\\System32"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(
            result,
            ConfirmationResult::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn test_path_sandbox_resolves_relative_workspace_paths() {
        let root = haven_common::discover_workspace_root(&std::env::current_dir().unwrap())
            .expect("tests run inside the Haven workspace");
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                resource_policy: ResourcePolicy {
                    files: FileResourcePolicy {
                        path_roots: vec![root.to_string_lossy().into_owned()],
                        ..FileResourcePolicy::default()
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        let result = gw
            .check(
                None,
                "files",
                &json!({"operation": "read", "path": "docs/architecture.md"}),
                RiskLevel::Low,
            )
            .await;
        assert!(matches!(result, ConfirmationResult::AutoApproved));
    }

    #[tokio::test]
    async fn path_defaults_request_consent_and_explicit_denials_still_refuse() {
        let root = tempfile::tempdir().unwrap();
        let readonly = root.path().join("vendor");
        let denied = root.path().join("private");
        let git = root.path().join(".git");
        std::fs::create_dir(&readonly).unwrap();
        std::fs::create_dir(&denied).unwrap();
        std::fs::create_dir(&git).unwrap();
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                resource_policy: ResourcePolicy {
                    files: FileResourcePolicy {
                        write_mode: FileWriteMode::Roots,
                        write_roots: vec![root.path().to_string_lossy().into_owned()],
                        path_roots: vec![root.path().to_string_lossy().into_owned()],
                        read_only_roots: vec![readonly.to_string_lossy().into_owned()],
                        denied_paths: vec![denied.to_string_lossy().into_owned()],
                    },
                    ..ResourcePolicy::default()
                },
                ..ToolConfig::default()
            },
        );
        let gw = ThresholdFixture::new(RiskLevel::Medium);
        gw.set_tool_settings(settings).await;

        for (operation, path) in [
            ("write", readonly.join("manifest.txt")),
            ("read", denied.join("secret.txt")),
            ("write", git.join("config")),
        ] {
            let input = json!({"operation": operation, "path": path});
            let result = if operation == "write" {
                let mut policy = fixture_policy("files", &input, RiskLevel::Medium);
                policy.effect = OperationEffect::WorkspaceWrite;
                gw.engine
                    .authorize(&fixture_request(None, "files", &input, &policy))
                    .await
            } else {
                gw.check(None, "files", &input, RiskLevel::Medium).await
            };
            assert!(
                if operation == "read" {
                    matches!(result, ConfirmationResult::Blocked { .. })
                } else {
                    matches!(result, ConfirmationResult::RequiresConfirmation { .. })
                },
                "{operation} to protected policy path should block: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn workspace_shell_mxc_roots_intersect_global_and_tool_file_limits() {
        let temp = tempfile::tempdir().expect("create shell file-policy fixture");
        let workspace = temp.path().join("workspace");
        let src = workspace.join("src");
        let scoped = src.join("code");
        let protected = scoped.join("secrets");
        let git_metadata = scoped.join(".git");
        let global_protected = scoped.join("global-secrets");
        let denied = scoped.join("blocked.txt");
        let global_denied = scoped.join("global-blocked.txt");
        std::fs::create_dir_all(&scoped).expect("create narrowed workspace source directory");
        std::fs::create_dir(&protected).expect("create protected read-only directory");
        std::fs::create_dir(&git_metadata).expect("create protected VCS metadata");
        std::fs::create_dir(&global_protected).expect("create global read-only directory");
        std::fs::write(&denied, "denied").expect("create denied file");
        std::fs::write(&global_denied, "globally denied").expect("create global denied file");
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.writable_roots = vec![src.clone()];
            config.global_file_roots = vec![src.clone()];
            config.global_read_only_roots = vec![global_protected.clone()];
            config.global_denied_paths = vec![global_denied.clone()];
            config.tool_settings.insert(
                "shell".into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        files: FileResourcePolicy {
                            write_mode: FileWriteMode::Roots,
                            write_roots: vec![scoped.to_string_lossy().into_owned()],
                            path_roots: vec![scoped.to_string_lossy().into_owned()],
                            read_only_roots: vec![protected.to_string_lossy().into_owned()],
                            denied_paths: vec![denied.to_string_lossy().into_owned()],
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            );
        }

        let permit = authorization.acquire().await.expect("acquire live permit");
        let filesystem_policy = permit
            .workspace_shell_directories(&scoped, &[])
            .expect("compile the final Shell MXC policy");
        assert_eq!(
            filesystem_policy
                .readwrite_roots
                .iter()
                .map(|directory| directory.root())
                .collect::<Vec<_>>(),
            vec![scoped.as_path()],
            "global workspace, tool write_roots, and global/tool path_roots must intersect"
        );
        assert_eq!(
            filesystem_policy
                .readonly_roots
                .iter()
                .map(|directory| directory.root())
                .collect::<Vec<_>>(),
            Vec::<&std::path::Path>::new(),
            "a read scope fully covered by a write grant is redundant"
        );
        assert_eq!(
            filesystem_policy.protected_readonly_paths,
            vec![global_protected, protected]
        );
        assert_eq!(filesystem_policy.denied_paths, vec![global_denied, denied]);
    }

    #[tokio::test]
    async fn workspace_shell_tool_write_roots_narrow_writes_without_narrowing_reads() {
        let temp = tempfile::tempdir().expect("create shell write-root fixture");
        let workspace = temp.path().join("workspace");
        let src = workspace.join("src");
        std::fs::create_dir_all(&src).expect("create workspace source directory");
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.tool_settings.insert(
                "shell".into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        files: FileResourcePolicy {
                            write_mode: FileWriteMode::Roots,
                            write_roots: vec![src.to_string_lossy().into_owned()],
                            ..FileResourcePolicy::default()
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            );
        }

        let permit = authorization.acquire().await.expect("acquire live permit");
        let filesystem_policy = permit
            .workspace_shell_directories(&src, &[])
            .expect("compile the Shell write-root restriction");
        assert_eq!(
            filesystem_policy
                .readwrite_roots
                .iter()
                .map(|directory| directory.root())
                .collect::<Vec<_>>(),
            vec![src.as_path()],
            "tool write_roots must intersect the global workspace write grant"
        );
        assert_eq!(
            filesystem_policy
                .readonly_roots
                .iter()
                .map(|directory| directory.root())
                .collect::<Vec<_>>(),
            vec![workspace.as_path()],
            "write_roots alone must not accidentally narrow the read scope"
        );
    }

    #[tokio::test]
    async fn workspace_shell_tool_read_only_policy_compiles_without_write_roots() {
        let temp = tempfile::tempdir().expect("create shell read-only fixture");
        let workspace = temp.path().join("workspace");
        let src = workspace.join("src");
        std::fs::create_dir_all(&src).expect("create workspace source directory");
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.tool_settings.insert(
                "shell".into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        files: FileResourcePolicy {
                            write_mode: FileWriteMode::ReadOnly,
                            path_roots: vec![src.to_string_lossy().into_owned()],
                            ..FileResourcePolicy::default()
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            );
        }

        let permit = authorization.acquire().await.expect("acquire live permit");
        let filesystem_policy = permit
            .workspace_shell_directories(&src, &[])
            .expect("compile the read-only Shell MXC policy");
        assert!(
            filesystem_policy.readwrite_roots.is_empty(),
            "read-only tool policy must deny writes"
        );
        assert!(filesystem_policy.protected_readonly_paths.is_empty());
        assert!(filesystem_policy.denied_paths.is_empty());
        assert_eq!(
            filesystem_policy
                .readonly_roots
                .iter()
                .map(|directory| directory.root())
                .collect::<Vec<_>>(),
            vec![src.as_path()],
            "the tool path_roots still bound the read-only process scope"
        );
    }

    #[tokio::test]
    async fn workspace_shell_runtime_read_grants_survive_default_roots_and_keep_explicit_denials() {
        let temp = tempfile::tempdir().expect("create Shell runtime read-root fixture");
        let allowed = temp.path().join("allowed");
        let workspace = allowed.join("workspace");
        let toolchain = allowed.join("sdk");
        let workspace_vendor = workspace.join("vendor");
        let global_denied = toolchain.join("global-private");
        let tool_denied = toolchain.join("tool-private");
        let outside_global = temp.path().join("outside-global");
        std::fs::create_dir_all(&workspace).expect("create workspace directory");
        std::fs::create_dir_all(&workspace_vendor).expect("create protected runtime directory");
        std::fs::create_dir_all(&global_denied).expect("create global denied directory");
        std::fs::create_dir_all(&tool_denied).expect("create tool denied directory");
        std::fs::create_dir_all(&outside_global).expect("create outside directory");
        let (_authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        let mut security = SecurityConfig::default();
        security.execution_root = Some(workspace.clone());
        security.shell_runtime_read_roots = vec![
            toolchain.to_string_lossy().into_owned(),
            outside_global.to_string_lossy().into_owned(),
            workspace_vendor.to_string_lossy().into_owned(),
        ];
        security.resource_policy.files.path_roots = vec![allowed.to_string_lossy().into_owned()];
        security.resource_policy.files.denied_paths =
            vec![global_denied.to_string_lossy().into_owned()];
        engine
            .apply_security(&security)
            .await
            .expect("apply global runtime read and file boundaries");
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.tool_settings.insert(
                "shell".into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        files: FileResourcePolicy {
                            path_roots: vec![allowed.to_string_lossy().into_owned()],
                            denied_paths: vec![tool_denied.to_string_lossy().into_owned()],
                            ..FileResourcePolicy::default()
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            );
        }

        let authorization = ExecutionPolicyAuthorization::for_test_with_engine_and_tool(
            Arc::clone(&engine),
            "shell",
        );
        let permit = authorization.acquire().await.expect("acquire live permit");
        let filesystem_policy = permit
            .workspace_shell_directories(&workspace, &[])
            .expect("compile configured runtime roots through the resource policy");
        assert_eq!(
            filesystem_policy
                .readonly_roots
                .iter()
                .map(|directory| directory.root())
                .collect::<Vec<_>>(),
            vec![toolchain.as_path(), outside_global.as_path()],
            "configured SDK read grants are independent of default project scopes"
        );
        assert_eq!(
            filesystem_policy.denied_paths,
            vec![global_denied, tool_denied]
        );
        assert!(
            filesystem_policy
                .protected_readonly_paths
                .contains(&workspace_vendor),
            "a configured runtime root inside the write workspace must remain read-only"
        );
    }

    #[tokio::test]
    async fn workspace_shell_default_path_roots_do_not_remove_existing_runtime_read_grants() {
        let temp = tempfile::tempdir().expect("create Shell runtime restriction fixture");
        let workspace = temp.path().join("workspace");
        let toolchain = temp.path().join("sdk");
        std::fs::create_dir_all(&workspace).expect("create workspace directory");
        std::fs::create_dir_all(&toolchain).expect("create SDK directory");
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.shell_runtime_read_roots = vec![toolchain.clone()];
            config.tool_settings.insert(
                "shell".into(),
                ToolConfig {
                    resource_policy: ResourcePolicy {
                        files: FileResourcePolicy {
                            path_roots: vec![workspace.to_string_lossy().into_owned()],
                            ..FileResourcePolicy::default()
                        },
                        ..ResourcePolicy::default()
                    },
                    ..ToolConfig::default()
                },
            );
        }

        let permit = authorization.acquire().await.expect("acquire live permit");
        let filesystem_policy = permit
            .workspace_shell_directories(&workspace, &[])
            .expect("compile the narrowed tool path policy");
        assert!(
            filesystem_policy
                .readonly_roots
                .iter()
                .any(|directory| directory.root() == toolchain),
            "existing SDK read grants must survive a narrower default project scope"
        );
    }

    #[tokio::test]
    async fn workspace_shell_mxc_policy_rejects_a_working_directory_outside_path_roots() {
        let temp = tempfile::tempdir().expect("create shell path-boundary fixture");
        let workspace = temp.path().join("workspace");
        let allowed = workspace.join("src");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&allowed).expect("create allowed source directory");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.global_file_roots = vec![allowed];
        }

        let permit = authorization.acquire().await.expect("acquire live permit");
        let error = permit
            .workspace_shell_directories(&outside, &[])
            .expect_err("MXC must not add cwd outside the owner-compiled roots");
        assert!(error.contains("outside the compiled MXC filesystem policy"));
    }

    #[tokio::test]
    async fn workspace_shell_mxc_policy_rejects_a_denied_working_directory() {
        let temp = tempfile::tempdir().expect("create shell denied-cwd fixture");
        let workspace = temp.path().join("workspace");
        let denied = workspace.join("private");
        std::fs::create_dir_all(&denied).expect("create denied directory");
        let (authorization, engine) =
            ExecutionPolicyAuthorization::for_test_with_workspace_write_root(
                &workspace,
                NetworkPolicy::Deny,
            );
        {
            let mut config = engine
                .config
                .try_write()
                .expect("test authorization config must be idle");
            config.global_denied_paths = vec![denied.clone()];
        }

        let permit = authorization.acquire().await.expect("acquire live permit");
        let error = permit
            .workspace_shell_directories(&denied, &[])
            .expect_err("the owner must fail closed when cwd is explicitly denied");
        assert!(error.contains("working directory is explicitly denied"));
    }
    #[tokio::test]
    async fn mcp_connection_grant_revocation_signals_transport_shutdown_only_for_connections() {
        let engine = AuthorizationEngine::new();
        let mut changes = engine.subscribe_mcp_permission_changes();
        let mcp = CapabilityScope::try_new("mcp_stdio.permission").unwrap();
        engine
            .grant(
                None,
                mcp.clone(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                vec![PermissionResourceScope::ProcessNetwork {
                    target_fingerprint: "a".repeat(64),
                }],
            )
            .await;
        assert!(!changes.has_changed().unwrap());
        engine
            .grant(
                None,
                CapabilityScope::try_new("mcp.tool.read").unwrap(),
                PermissionEffect::Allow,
                PermissionScope::Always,
                vec![PermissionResourceScope::ManagedAsset {
                    asset_id: haven_common::types::new_id("asset"),
                }],
            )
            .await;
        engine.revoke_permanent("mcp.tool.read").await;
        assert!(!changes.has_changed().unwrap());
        engine.revoke_permanent(mcp.as_str()).await;
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        engine
            .grant(
                None,
                mcp,
                PermissionEffect::Allow,
                PermissionScope::Always,
                vec![PermissionResourceScope::ProcessNetwork {
                    target_fingerprint: "b".repeat(64),
                }],
            )
            .await;
        engine.clear_permanent().await;
        assert!(changes.has_changed().unwrap());
    }
}
