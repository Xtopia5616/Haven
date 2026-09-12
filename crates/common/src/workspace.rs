use std::path::{Path, PathBuf};

/// Find the repository/workspace root visible from `start`.
///
/// `AGENTS.md` and `.git` are the authoritative markers. The Cargo/UI pair is
/// retained as a useful fallback for exported source trees where the `.git`
/// directory is intentionally omitted.
pub fn discover_workspace_root(start: &Path) -> Option<PathBuf> {
    let mut cargo_candidate = None;
    for ancestor in start.ancestors() {
        if ancestor.join("AGENTS.md").is_file() || ancestor.join(".git").exists() {
            return Some(ancestor.to_path_buf());
        }
        if cargo_candidate.is_none()
            && ancestor.join("Cargo.toml").is_file()
            && ancestor.join("ui").is_dir()
        {
            cargo_candidate = Some(ancestor.to_path_buf());
        }
    }
    cargo_candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn discovers_marker_from_nested_directory() {
        let root = tempdir().unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "workspace").unwrap();
        let nested = root.path().join("crates").join("agent");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(discover_workspace_root(&nested), Some(root.path().into()));
    }
}
