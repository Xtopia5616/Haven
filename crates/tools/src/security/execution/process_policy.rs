use super::ExecutionPolicyPermit;
use crate::security::filesystem::{open_directory_roots, open_filesystem_paths};
use haven_common::types::SandboxMode;
use haven_platform::filesystem::DirectoryCapability;
use haven_platform::sandboxed_process::{SandboxedProcessNetworkPolicy, SandboxedProcessPolicy};
use std::path::{Path, PathBuf};

pub(crate) struct WorkspaceProcessFilesystemPolicy {
    pub(crate) readwrite_roots: Vec<DirectoryCapability>,
    pub(crate) readonly_roots: Vec<DirectoryCapability>,
    pub(crate) protected_readonly_paths: Vec<PathBuf>,
    pub(crate) denied_paths: Vec<PathBuf>,
}

impl ExecutionPolicyPermit {
    /// Compile a manager-owned MCP server with a read-only working subtree,
    /// app-owned scratch, read-only runtime dependencies, and the opaque
    /// network grant the live global policy can actually express.
    pub(crate) fn mcp_stdio_process_policy(
        &self,
        request: &haven_mcp::McpStdioProcessStartRequest,
        scratch_directory: &Path,
        allow_public_egress: bool,
    ) -> Result<(SandboxedProcessPolicy, PathBuf), String> {
        if self.sandbox_mode() != SandboxMode::WorkspaceWrite {
            return Err("MXC MCP roots require the Workspace Write sandbox".into());
        }
        let workspace_roots = self._config.workspace_write_directories(None)?;
        let configured_cwd = request
            .working_directory
            .as_deref()
            .or_else(|| workspace_roots.first().map(|root| root.root()))
            .ok_or_else(|| "MCP server has no authorized working directory".to_owned())?;
        let working_directory = DirectoryCapability::open(configured_cwd)
            .map_err(|_| "MCP server working directory is unavailable or unsafe".to_owned())?;
        if !super::roots_contain_directory(&workspace_roots, &working_directory)
            && !self.resource_boundary_extension
        {
            return Err(
                "Workspace Write MCP working directory must stay inside the workspace".into(),
            );
        }

        let settings = ["mcp_stdio", "mcp"]
            .into_iter()
            .filter_map(|name| self._config.tool_settings.get(name))
            .collect::<Vec<_>>();
        let mut path_constraints = Vec::new();
        if !self._config.file_policy.global_file_roots.is_empty() {
            path_constraints.push(open_directory_roots(
                &self._config.file_policy.global_file_roots,
                "security.resource_policy.files.path_roots",
            )?);
        }
        let mut denied_paths = self._config.file_policy.global_denied_paths.clone();
        let mut protected_paths = self._config.file_policy.global_read_only_roots.clone();
        for setting in &settings {
            let files = &setting.resource_policy.files;
            if !files.path_roots.is_empty() {
                let roots = files
                    .path_roots
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>();
                path_constraints.push(open_directory_roots(
                    &roots,
                    "MCP process resource_policy.files.path_roots",
                )?);
            }
            denied_paths.extend(files.denied_paths.iter().map(PathBuf::from));
            protected_paths.extend(files.read_only_roots.iter().map(PathBuf::from));
        }
        if !self.resource_boundary_extension
            && path_constraints
                .iter()
                .any(|constraint| !super::roots_contain_directory(constraint, &working_directory))
        {
            return Err("MCP server working directory is outside the configured file scope".into());
        }

        let denied = open_filesystem_paths(&denied_paths, "MCP process denied paths")?;
        if denied
            .iter()
            .any(|deny| deny.contains_path(working_directory.root()).unwrap_or(true))
        {
            return Err("MCP server working directory is explicitly denied".into());
        }
        let protected = open_filesystem_paths(&protected_paths, "MCP process protected paths")?;
        let scratch = DirectoryCapability::open(scratch_directory)
            .map_err(|_| "MCP process scratch directory is unavailable or unsafe".to_owned())?;
        let temp_root =
            DirectoryCapability::open(&haven_common::config::default_runtime_temp_root())
                .map_err(|_| "MCP application temp root is unavailable".to_owned())?;
        if !super::directory_root_contains(&temp_root, &scratch) {
            return Err("MCP scratch directory is outside Haven's runtime temp root".into());
        }
        if denied
            .iter()
            .any(|deny| deny.contains_path(scratch.root()).unwrap_or(true))
            || protected
                .iter()
                .any(|path| path.contains_path(scratch.root()).unwrap_or(true))
        {
            return Err("MCP scratch directory is denied or protected read-only".into());
        }
        #[cfg(windows)]
        scratch
            .ensure_no_hardlinked_files(super::MAX_MXC_WRITE_ROOT_INSPECTED_ENTRIES)
            .map_err(|_| {
                "MXC cannot safely isolate an MCP scratch directory with hard-linked files"
                    .to_owned()
            })?;

        let mut runtime_paths = request.runtime_read_roots.clone();
        runtime_paths.extend(self._config.shell_runtime_read_roots.iter().cloned());
        let mut runtime = open_directory_roots(&runtime_paths, "MCP runtime roots")?;
        runtime.retain(|root| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(root.root()).unwrap_or(true))
        });
        runtime = super::normalize_directory_roots(runtime);
        runtime.retain(|root| {
            !super::directory_root_contains(&scratch, root)
                && !super::directory_root_contains(root, &scratch)
        });

        let mut readonly_paths = vec![working_directory.root().to_path_buf()];
        readonly_paths.extend(runtime.iter().map(|root| root.root().to_path_buf()));
        readonly_paths.sort();
        readonly_paths.dedup();
        let denied_paths = denied
            .iter()
            .filter(|deny| {
                deny.contains_path(scratch.root()).unwrap_or(false)
                    || super::directory_root_contains_path(&working_directory, deny.path())
                    || runtime.iter().any(|root| {
                        super::directory_root_contains_path(root, deny.path())
                            || deny.contains_path(root.root()).unwrap_or(true)
                    })
            })
            .map(|deny| deny.path().to_path_buf())
            .collect();
        let protected_readonly_paths = protected
            .iter()
            .filter(|path| {
                super::directory_root_contains_path(&working_directory, path.path())
                    || runtime
                        .iter()
                        .any(|root| super::directory_root_contains_path(root, path.path()))
            })
            .map(|path| path.path().to_path_buf())
            .collect();

        Ok((
            SandboxedProcessPolicy {
                isolation_mode: self._config.mxc_isolation_mode,
                readwrite_paths: vec![scratch.root().to_path_buf()],
                readonly_paths,
                protected_readonly_paths,
                denied_paths,
                network_policy: SandboxedProcessNetworkPolicy {
                    allow_public_egress,
                    allow_private_network_client_server: self.network_authorized,
                    allow_host_loopback: self.network_authorized,
                },
            },
            working_directory.root().to_path_buf(),
        ))
    }

    /// Compile a filename-search helper with a private writable scratch root,
    /// the selected workspace subtree and runtime dependencies read-only, and
    /// no network access. The Everything service remains an external IPC peer.
    pub(crate) fn everything_process_policy(
        &self,
        request: &super::EverythingProcessStartRequest,
    ) -> Result<SandboxedProcessPolicy, String> {
        if !matches!(
            self.sandbox_mode(),
            SandboxMode::WorkspaceWrite | SandboxMode::ReadOnly
        ) {
            return Err("MXC Everything roots require the Workspace Write sandbox".into());
        }
        let workspace_roots = self
            ._config
            .workspace_write_directories(self.session_id.as_deref())?;
        let search_root = DirectoryCapability::open(&request.search_root)
            .map_err(|_| "Everything search root is unavailable or unsafe".to_owned())?;
        if !super::roots_contain_directory(&workspace_roots, &search_root)
            && !self.authorized_resources.iter().any(|resource| {
                matches!(resource,
                crate::DeclaredResource::LocalPath { path, .. } if path == &request.search_root)
            })
        {
            return Err("Everything search root has no declared read grant".into());
        }

        let settings = self
            ._config
            .tool_settings
            .get("files")
            .into_iter()
            .collect::<Vec<_>>();
        let mut path_constraints = Vec::new();
        if !self._config.file_policy.global_file_roots.is_empty() {
            path_constraints.push(open_directory_roots(
                &self._config.file_policy.global_file_roots,
                "security.resource_policy.files.path_roots",
            )?);
        }
        let mut denied_paths = self._config.file_policy.global_denied_paths.clone();
        let mut protected_paths = self._config.file_policy.global_read_only_roots.clone();
        for setting in &settings {
            if !setting.resource_policy.files.path_roots.is_empty() {
                let roots = setting
                    .resource_policy
                    .files
                    .path_roots
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>();
                path_constraints.push(open_directory_roots(
                    &roots,
                    "tool resource_policy.files.path_roots",
                )?);
            }
            denied_paths.extend(
                setting
                    .resource_policy
                    .files
                    .denied_paths
                    .iter()
                    .map(PathBuf::from),
            );
            protected_paths.extend(
                setting
                    .resource_policy
                    .files
                    .read_only_roots
                    .iter()
                    .map(PathBuf::from),
            );
        }
        if !self.resource_boundary_extension
            && path_constraints
                .iter()
                .any(|constraint| !super::roots_contain_directory(constraint, &search_root))
        {
            return Err("Everything search root is outside the configured file scope".into());
        }

        let denied = open_filesystem_paths(&denied_paths, "Everything denied paths")?;
        if denied
            .iter()
            .any(|deny| deny.contains_path(search_root.root()).unwrap_or(true))
        {
            return Err("Everything search root is explicitly denied".into());
        }

        let scratch = DirectoryCapability::open(&request.scratch_directory)
            .map_err(|_| "Everything scratch directory is unavailable or unsafe".to_owned())?;
        let temp_root =
            DirectoryCapability::open(&haven_common::config::default_runtime_temp_root())
                .map_err(|_| "Everything application temp root is unavailable".to_owned())?;
        if !super::directory_root_contains(&temp_root, &scratch) {
            return Err("Everything scratch directory is outside Haven's runtime temp root".into());
        }
        let protected = open_filesystem_paths(&protected_paths, "Everything protected paths")?;
        if denied
            .iter()
            .any(|deny| deny.contains_path(scratch.root()).unwrap_or(true))
            || protected
                .iter()
                .any(|path| path.contains_path(scratch.root()).unwrap_or(true))
        {
            return Err("Everything scratch directory is denied or protected read-only".into());
        }
        #[cfg(windows)]
        scratch
            .ensure_no_hardlinked_files(super::MAX_MXC_WRITE_ROOT_INSPECTED_ENTRIES)
            .map_err(|_| {
                "MXC cannot safely isolate an Everything scratch directory with hard-linked files"
                    .to_owned()
            })?;

        let mut runtime_roots = request.runtime_read_roots.clone();
        runtime_roots.extend(self._config.shell_runtime_read_roots.iter().cloned());
        let mut runtime = open_directory_roots(&runtime_roots, "Everything runtime roots")?;
        runtime.retain(|root| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(root.root()).unwrap_or(true))
        });
        let runtime = super::normalize_directory_roots(runtime);
        let mut readonly_paths = vec![search_root.root().to_path_buf()];
        readonly_paths.extend(runtime.iter().map(|root| root.root().to_path_buf()));
        readonly_paths.sort();
        readonly_paths.dedup();
        let denied_paths = denied
            .iter()
            .filter(|deny| {
                deny.contains_path(scratch.root()).unwrap_or(false)
                    || haven_platform::filesystem::FilesystemPathCapability::open(
                        search_root.root(),
                    )
                    .is_ok_and(|scope| scope.contains_path(deny.path()).unwrap_or(false))
                    || runtime.iter().any(|root| {
                        super::directory_root_contains_path(root, deny.path())
                            || deny.contains_path(root.root()).unwrap_or(true)
                    })
            })
            .map(|deny| deny.path().to_path_buf())
            .collect();

        Ok(SandboxedProcessPolicy {
            isolation_mode: self._config.mxc_isolation_mode,
            readwrite_paths: vec![scratch.root().to_path_buf()],
            readonly_paths,
            protected_readonly_paths: protected
                .iter()
                .filter(|path| {
                    haven_platform::filesystem::FilesystemPathCapability::open(search_root.root())
                        .is_ok_and(|scope| scope.contains_path(path.path()).unwrap_or(false))
                })
                .map(|path| path.path().to_path_buf())
                .collect(),
            denied_paths,
            network_policy: self.process_network_policy(),
        })
    }

    /// Compile the Skill setup process grant. Setup writes only the app-owned
    /// venv and per-call scratch; project roots are never granted as writable.
    pub(crate) fn skill_setup_process_policy(
        &self,
        request: &haven_skills::SkillProcessRequest,
        allow_public_egress: bool,
    ) -> Result<SandboxedProcessPolicy, String> {
        if self.sandbox_mode() != SandboxMode::WorkspaceWrite {
            return Err("MXC Skill setup roots require the Workspace Write sandbox".into());
        }

        let settings = self
            ._config
            .tool_settings
            .get(&self.tool_name)
            .into_iter()
            .collect::<Vec<_>>();
        let mut denied_paths = self._config.file_policy.global_denied_paths.clone();
        let mut protected_paths = self._config.file_policy.global_read_only_roots.clone();
        for setting in &settings {
            denied_paths.extend(
                setting
                    .resource_policy
                    .files
                    .denied_paths
                    .iter()
                    .map(PathBuf::from),
            );
            protected_paths.extend(
                setting
                    .resource_policy
                    .files
                    .read_only_roots
                    .iter()
                    .map(PathBuf::from),
            );
        }
        let denied = open_filesystem_paths(&denied_paths, "Skill setup denied paths")?;
        let protected = open_filesystem_paths(&protected_paths, "Skill setup protected paths")?;
        let write_paths = [
            request.venv_directory.clone(),
            request.scratch_directory.clone(),
        ];
        let readwrite_roots =
            open_directory_roots(&write_paths, "Skill setup private write roots")?;
        if readwrite_roots.len() != write_paths.len() {
            return Err("Skill setup private write roots are unavailable".into());
        }
        for writable in &readwrite_roots {
            if denied
                .iter()
                .any(|deny| deny.contains_path(writable.root()).unwrap_or(true))
            {
                return Err("Skill setup write root is explicitly denied".into());
            }
            if protected
                .iter()
                .any(|path| path.contains_path(writable.root()).unwrap_or(true))
            {
                return Err("Skill setup write root is protected read-only".into());
            }
            #[cfg(windows)]
            writable
                .ensure_no_hardlinked_files(super::MAX_MXC_WRITE_ROOT_INSPECTED_ENTRIES)
                .map_err(|_| {
                    "MXC cannot safely isolate a Skill setup directory containing hard-linked files"
                        .to_owned()
                })?;
        }

        let mut runtime_paths = request.runtime_read_roots.clone();
        if request.purpose == Some(haven_skills::SkillProcessPurpose::InstallRequirements) {
            runtime_paths.push(request.skill_root.clone());
        }
        runtime_paths.extend(self._config.shell_runtime_read_roots.iter().cloned());
        let mut readonly_roots = open_directory_roots(&runtime_paths, "Skill setup runtime roots")?;
        readonly_roots.retain(|root| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(root.root()).unwrap_or(true))
        });
        let mut readonly_roots = readonly_roots;
        readonly_roots.retain(|readable| {
            !readwrite_roots
                .iter()
                .any(|writable| super::directory_root_contains(writable, readable))
        });

        let protected_readonly_paths = protected
            .iter()
            .filter(|path| {
                !denied
                    .iter()
                    .any(|deny| deny.contains_path(path.path()).unwrap_or(true))
                    && readwrite_roots
                        .iter()
                        .chain(&readonly_roots)
                        .any(|scope| super::directory_root_contains_path(scope, path.path()))
            })
            .map(|path| path.path().to_path_buf())
            .collect::<Vec<_>>();
        let denied_paths = denied
            .iter()
            .filter(|deny| {
                readwrite_roots.iter().chain(&readonly_roots).any(|scope| {
                    super::directory_root_contains_path(scope, deny.path())
                        || deny.contains_path(scope.root()).unwrap_or(true)
                })
            })
            .map(|deny| deny.path().to_path_buf())
            .collect::<Vec<_>>();
        let scratch = DirectoryCapability::open(&request.scratch_directory)
            .map_err(|_| "Skill setup scratch directory is unavailable or unsafe".to_owned())?;
        if !super::roots_contain_directory(&readwrite_roots, &scratch) {
            return Err("Skill setup scratch directory is outside its write roots".into());
        }

        Ok(SandboxedProcessPolicy {
            isolation_mode: self._config.mxc_isolation_mode,
            readwrite_paths: super::normalize_directory_roots(readwrite_roots)
                .iter()
                .map(|root| root.root().to_path_buf())
                .collect(),
            readonly_paths: super::normalize_directory_roots(readonly_roots)
                .iter()
                .map(|root| root.root().to_path_buf())
                .collect(),
            protected_readonly_paths,
            denied_paths,
            network_policy: SandboxedProcessNetworkPolicy {
                allow_public_egress,
                allow_private_network_client_server: self.network_authorized,
                allow_host_loopback: self.network_authorized,
            },
        })
    }

    /// Compile the normal Skill script grant from the same WorkspaceWrite
    /// roots used by Shell, plus the Skill's read-only runtime dependencies.
    pub(crate) fn skill_execution_process_policy(
        &self,
        working_directory: &Path,
        runtime_read_roots: &[PathBuf],
    ) -> Result<SandboxedProcessPolicy, String> {
        let filesystem = self.workspace_process_directories_with_runtime_writes_for_tool(
            &self.tool_name,
            working_directory,
            runtime_read_roots,
            working_directory,
            &[],
        )?;
        Ok(SandboxedProcessPolicy {
            isolation_mode: self._config.mxc_isolation_mode,
            readwrite_paths: filesystem
                .readwrite_roots
                .iter()
                .map(|root| root.root().to_path_buf())
                .collect(),
            readonly_paths: filesystem
                .readonly_roots
                .iter()
                .map(|root| root.root().to_path_buf())
                .collect(),
            protected_readonly_paths: filesystem.protected_readonly_paths,
            denied_paths: filesystem.denied_paths,
            network_policy: self.process_network_policy(),
        })
    }

    /// Compile a read-only project scope and a single app-owned writable
    /// scratch directory for a WorkspaceWrite child process such as LSP.
    pub(crate) fn workspace_lsp_directories(
        &self,
        workspace_root: &Path,
        runtime_read_roots: &[PathBuf],
        scratch_directory: &Path,
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        if self.sandbox_mode() != haven_common::types::SandboxMode::WorkspaceWrite {
            return Err("MXC LSP roots require the Workspace Write sandbox".into());
        }

        let workspace_roots = self
            ._config
            .workspace_write_directories(self.session_id.as_deref())?;
        let mut path_constraints = Vec::new();
        if !self._config.file_policy.global_file_roots.is_empty() {
            path_constraints.push(open_directory_roots(
                &self._config.file_policy.global_file_roots,
                "security.resource_policy.files.path_roots",
            )?);
        }

        let lsp_settings = self
            ._config
            .tool_settings
            .get("lsp")
            .into_iter()
            .collect::<Vec<_>>();
        let mut protected_readonly_paths = self._config.file_policy.global_read_only_roots.clone();
        let mut denied_paths = self._config.file_policy.global_denied_paths.clone();
        for setting in &lsp_settings {
            let files = &setting.resource_policy.files;
            if !files.path_roots.is_empty() {
                let roots = files
                    .path_roots
                    .iter()
                    .map(PathBuf::from)
                    .collect::<Vec<_>>();
                path_constraints.push(open_directory_roots(
                    &roots,
                    "tool resource_policy.files.path_roots",
                )?);
            }
            protected_readonly_paths.extend(files.read_only_roots.iter().map(PathBuf::from));
            denied_paths.extend(files.denied_paths.iter().map(PathBuf::from));
        }

        let protected_readonly = open_filesystem_paths(
            &protected_readonly_paths,
            "resource_policy.files.read_only_roots",
        )?;
        let denied = open_filesystem_paths(&denied_paths, "resource_policy.files.denied_paths")?;

        let mut project_read = workspace_roots;
        for constraint in &path_constraints {
            project_read = super::intersect_directory_roots(&project_read, constraint);
        }
        DirectoryCapability::open(workspace_root)
            .map_err(|_| "LSP workspace is unavailable or unsafe".to_owned())?;
        if !project_read
            .iter()
            .any(|root| super::directory_root_contains_path(root, workspace_root))
        {
            return Err("LSP workspace is outside the configured process read scope".into());
        }

        let mut runtime_read = open_directory_roots(runtime_read_roots, "LSP runtime read roots")?;
        runtime_read.retain(|root| {
            !denied
                .iter()
                .any(|deny| deny.contains_path(root.root()).unwrap_or(true))
        });

        let scratch =
            open_directory_roots(&[scratch_directory.to_path_buf()], "LSP scratch directory")?
                .into_iter()
                .next()
                .ok_or_else(|| "LSP scratch directory is unavailable".to_owned())?;
        if denied
            .iter()
            .any(|deny| deny.contains_path(scratch.root()).unwrap_or(true))
            || protected_readonly
                .iter()
                .any(|path| path.contains_path(scratch.root()).unwrap_or(true))
            || runtime_read
                .iter()
                .any(|root| super::directory_root_contains_path(root, scratch.root()))
        {
            return Err("LSP scratch directory is denied or protected read-only".into());
        }

        let mut readonly_roots = project_read;
        readonly_roots.extend(runtime_read);
        readonly_roots.retain(|root| {
            !super::directory_root_contains_path(root, scratch.root())
                && !super::directory_root_contains_path(&scratch, root.root())
        });
        Ok(WorkspaceProcessFilesystemPolicy {
            readwrite_roots: vec![scratch],
            readonly_roots,
            protected_readonly_paths,
            denied_paths,
        })
    }
}
