use std::path::{Path, PathBuf};

/// Process-scoped execution root owned by the Tools security boundary.
///
/// This is separate from any renderer navigation state. It is discovered
/// once, canonicalized, and then shared by file path resolution and sandbox
/// authorization for the lifetime of the Tools facade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionWorkspace {
    root: Option<PathBuf>,
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
        let root = std::env::current_dir()
            .ok()
            .and_then(|current| haven_common::discover_workspace_root(&current))
            .and_then(|root| canonical_workspace_root(&root).ok());

        if root.is_none() {
            tracing::warn!(
                "execution workspace could not be resolved; workspace-relative file access is unavailable"
            );
        }

        Self {
            root,
            generation: 1,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_root(root: &Path) -> anyhow::Result<Self> {
        let root = canonical_workspace_root(root)?;
        Ok(Self {
            root: Some(root),
            generation: 1,
        })
    }

    #[cfg(test)]
    pub(crate) fn unavailable() -> Self {
        Self {
            root: None,
            generation: 1,
        }
    }

    pub(crate) fn root(&self) -> Option<&Path> {
        self.root.as_deref()
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
