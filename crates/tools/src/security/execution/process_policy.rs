use super::ExecutionPolicyPermit;
use crate::security::filesystem::{open_directory_roots, open_filesystem_paths};
use haven_platform::filesystem::DirectoryCapability;
use std::path::{Path, PathBuf};

pub(crate) struct WorkspaceProcessFilesystemPolicy {
    pub(crate) readwrite_roots: Vec<DirectoryCapability>,
    pub(crate) readonly_roots: Vec<DirectoryCapability>,
    pub(crate) protected_readonly_paths: Vec<PathBuf>,
    pub(crate) denied_paths: Vec<PathBuf>,
}

impl ExecutionPolicyPermit {
    /// Compile a read-only project scope and a single app-owned writable
    /// scratch directory for a WorkspaceWrite child process such as LSP.
    pub(crate) fn workspace_lsp_directories(
        &self,
        workspace_root: &Path,
        runtime_read_roots: &[PathBuf],
        scratch_directory: &Path,
    ) -> Result<WorkspaceProcessFilesystemPolicy, String> {
        if self._config.sandbox_mode != haven_common::types::SandboxMode::WorkspaceWrite {
            return Err("MXC LSP roots require the Workspace Write sandbox".into());
        }

        let workspace_roots = self
            ._config
            .workspace_write_directories(self.session_id.as_deref())?;
        let mut path_constraints = Vec::new();
        if !self._config.global_file_roots.is_empty() {
            path_constraints.push(open_directory_roots(
                &self._config.global_file_roots,
                "security.resource_policy.files.path_roots",
            )?);
        }

        let lsp_settings = self
            ._config
            .tool_settings
            .get("lsp")
            .into_iter()
            .collect::<Vec<_>>();
        let mut protected_readonly_paths = self._config.global_read_only_roots.clone();
        let mut denied_paths = self._config.global_denied_paths.clone();
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
        for constraint in &path_constraints {
            runtime_read = super::intersect_directory_roots(&runtime_read, constraint);
        }
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
