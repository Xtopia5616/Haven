use std::path::{Path, PathBuf};

/// Execution root owned by the Tools security boundary: the global default
/// or a session-specific override. It is separate from renderer navigation
/// state and shared by file path resolution and sandbox authorization.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionWorkspace {
    root: Option<PathBuf>,
    directory: Option<haven_platform::filesystem::DirectoryCapability>,
    generation: u64,
}

/// Read-only workspace information for prompt construction and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionWorkspaceSnapshot {
    pub root: Option<PathBuf>,
    pub generation: u64,
}

impl ExecutionWorkspace {
    pub(crate) fn discover() -> Self {
        let discovered_root = std::env::current_dir()
            .ok()
            .and_then(|current| haven_common::discover_workspace_root(&current))
            .and_then(|root| Self::open_root(&root, 1).ok());
        let workspace = discovered_root.unwrap_or_else(Self::unavailable);

        if workspace.root.is_none() {
            tracing::warn!(
                "execution workspace could not be resolved; workspace-relative file access is unavailable"
            );
        }

        workspace
    }

    #[cfg(test)]
    pub(crate) fn from_root(root: &Path) -> anyhow::Result<Self> {
        Self::open_root(root, 1)
    }

    pub(crate) fn open_root(root: &Path, generation: u64) -> anyhow::Result<Self> {
        let root = canonical_workspace_root(root)?;
        let directory = haven_platform::filesystem::DirectoryCapability::open(&root)?;
        Ok(Self {
            root: Some(directory.root().to_path_buf()),
            directory: Some(directory),
            generation,
        })
    }

    pub(crate) fn with_generation(mut self, generation: u64) -> Self {
        self.generation = generation;
        self
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn same_root(&self, other: &Self) -> bool {
        let same_path = match (&self.root, &other.root) {
            (Some(left), Some(right)) => {
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
            (None, None) => true,
            _ => false,
        };
        same_path && self.directory.is_some() == other.directory.is_some()
    }

    pub(crate) fn unavailable() -> Self {
        Self {
            root: None,
            directory: None,
            generation: 1,
        }
    }

    /// Preserve a previously selected path for diagnostics while keeping its
    /// directory capability unavailable. Session restoration uses this when
    /// a bound directory has been removed or disconnected; writes then fail
    /// closed at the directory-capability boundary.
    pub(crate) fn unavailable_for(root: &Path, generation: u64) -> Self {
        Self {
            root: Some(root.to_path_buf()),
            directory: None,
            generation,
        }
    }

    pub(crate) fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub(crate) fn directory(&self) -> Option<haven_platform::filesystem::DirectoryCapability> {
        self.directory.clone()
    }

    pub(crate) fn resolve_relative(&self, path: &Path) -> Option<PathBuf> {
        self.root.as_ref().map(|root| root.join(path))
    }

    pub(crate) fn snapshot(&self) -> ExecutionWorkspaceSnapshot {
        ExecutionWorkspaceSnapshot {
            root: self.root.clone(),
            generation: self.generation,
        }
    }
}

fn canonical_workspace_root(root: &Path) -> anyhow::Result<PathBuf> {
    let canonical = std::fs::canonicalize(root)?;
    let normalized = normalize_windows_extended_path(canonical)
        .ok_or_else(|| anyhow::anyhow!("execution workspace root is not a local absolute path"))?;
    anyhow::ensure!(
        normalized.is_absolute() && normalized.is_dir() && !is_unc_or_device_path(&normalized),
        "execution workspace root is not a local absolute directory"
    );
    Ok(normalized)
}

#[cfg(windows)]
fn normalize_windows_extended_path(path: PathBuf) -> Option<PathBuf> {
    let value = path.to_str()?;
    if let Some(rest) = value.strip_prefix("\\\\?\\") {
        if rest.starts_with("UNC\\") || rest.starts_with("unc\\") {
            return None;
        }
        Some(PathBuf::from(rest))
    } else {
        Some(path)
    }
}

#[cfg(not(windows))]
fn normalize_windows_extended_path(path: PathBuf) -> Option<PathBuf> {
    Some(path)
}

fn is_unc_or_device_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("\\\\") || value.starts_with("//")
}
