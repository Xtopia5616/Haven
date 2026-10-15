use std::path::Path;

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
