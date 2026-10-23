use std::path::{Path, PathBuf};

/// Return whether a path is absolute and local on the current platform.
/// Windows UNC and device paths are excluded because Haven's directory
/// capabilities and file dialogs accept only local filesystem roots.
pub fn is_absolute_local_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        !value.starts_with("\\\\") && !value.starts_with("//")
    }
    #[cfg(not(windows))]
    {
        true
    }
}

/// Normalize a canonical absolute local path into the ordinary path spelling
/// expected by config files and Windows directory capabilities.
///
/// Windows `std::fs::canonicalize` may return a verbatim drive path such as
/// `\\?\C:\workspace`. Strip that prefix only for drive paths; UNC and other
/// device paths remain rejected by `is_absolute_local_path`.
pub fn normalize_absolute_local_path(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let normalized = {
        let value = path.to_string_lossy();
        PathBuf::from(value.strip_prefix(r"\\?\").unwrap_or(value.as_ref()))
    };
    #[cfg(not(windows))]
    let normalized = path.to_path_buf();

    is_absolute_local_path(&normalized).then_some(normalized)
}

/// Return whether `candidate` is lexically equal to or beneath `root`.
///
/// This comparison does not access the filesystem, resolve symbolic links, or
/// canonicalize either path. Callers must own those checks and their root
/// policy. Windows compares lossy path text without case and requires a path
/// separator at the descendant boundary; other platforms use `Path` component
/// semantics.
pub fn path_is_equal_or_child(root: &Path, candidate: &Path) -> bool {
    #[cfg(windows)]
    {
        let root = root.to_string_lossy().to_lowercase();
        let candidate = candidate.to_string_lossy().to_lowercase();
        candidate == root
            || candidate.starts_with(&format!("{root}\\"))
            || candidate.starts_with(&format!("{root}/"))
    }
    #[cfg(not(windows))]
    {
        candidate == root || candidate.strip_prefix(root).is_ok()
    }
}

/// Return whether two paths compare equal under the current platform's path
/// semantics. This is lexical equality, not filesystem identity.
pub fn path_is_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn normalizes_verbatim_local_drive_paths_but_rejects_unc_and_device_paths() {
        assert_eq!(
            normalize_absolute_local_path(Path::new(r"\\?\C:\workspace")),
            Some(PathBuf::from(r"C:\workspace"))
        );
        assert_eq!(
            normalize_absolute_local_path(Path::new(r"\\?\UNC\server\share")),
            None
        );
        assert_eq!(
            normalize_absolute_local_path(Path::new(r"\\.\PhysicalDrive0")),
            None
        );
    }
}
