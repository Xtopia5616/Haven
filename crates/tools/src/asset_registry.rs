//! Process-local registry for host-owned media assets.
//!
//! The model-facing contract contains only an opaque `asset_id`. This module
//! is the trusted boundary that resolves that id to a host path for a narrow
//! read-only files operation; paths never need to enter the LLM transcript.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Host-owned metadata needed to resolve a managed attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedAsset {
    pub asset_id: String,
    pub path: PathBuf,
    pub filename: Option<String>,
    pub media_type: String,
}

/// In-process mapping from opaque asset ids to host-owned files.
#[derive(Debug, Clone, Default)]
pub struct ManagedAssetRegistry {
    assets: Arc<RwLock<HashMap<String, ManagedAsset>>>,
}

impl ManagedAssetRegistry {
    /// Register a host-created asset after re-validating its path.
    ///
    /// The caller must supply the dedicated managed-assets root. This second
    /// validation is intentional: the upload ingress is not the only caller
    /// that can reach this process-local registry, and a path that escapes the
    /// root must never become resolvable through `files(asset_id)`.
    pub fn register_under_root(
        &self,
        root: &Path,
        asset_id: impl Into<String>,
        path: PathBuf,
        filename: Option<String>,
        media_type: impl Into<String>,
    ) -> bool {
        let asset_id = asset_id.into();
        if asset_id.trim().is_empty()
            || path.as_os_str().is_empty()
            || !is_safe_managed_file(root, &path)
        {
            return false;
        }
        let asset = ManagedAsset {
            asset_id: asset_id.clone(),
            path,
            filename,
            media_type: media_type.into(),
        };
        self.assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(asset_id, asset);
        true
    }

    #[cfg(test)]
    pub(crate) fn register_for_test(
        &self,
        asset_id: impl Into<String>,
        path: PathBuf,
        filename: Option<String>,
        media_type: impl Into<String>,
    ) {
        let asset_id = asset_id.into();
        if asset_id.trim().is_empty() || path.as_os_str().is_empty() {
            return;
        }
        self.assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                asset_id.clone(),
                ManagedAsset {
                    asset_id,
                    path,
                    filename,
                    media_type: media_type.into(),
                },
            );
    }

    pub fn resolve(&self, asset_id: &str) -> Option<ManagedAsset> {
        self.assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(asset_id)
            .cloned()
    }

    pub fn contains(&self, asset_id: &str) -> bool {
        self.assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(asset_id)
    }

    /// Return a snapshot of paths currently protected from retention cleanup.
    pub fn protected_paths(&self) -> Vec<PathBuf> {
        self.assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .map(|asset| asset.path.clone())
            .collect()
    }

    /// Remove registry entries whose files no longer exist or are no longer
    /// regular files. This bounds the process-local map after retention GC.
    pub fn prune_missing(&self) -> usize {
        let mut assets = self
            .assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = assets.len();
        assets.retain(|_, asset| {
            std::fs::symlink_metadata(&asset.path)
                .map(|metadata| metadata.is_file() && !is_link_or_reparse(&metadata))
                .unwrap_or(false)
        });
        before.saturating_sub(assets.len())
    }

    /// Remove entries that point into a successfully deleted managed batch.
    pub fn prune_paths_under(&self, root: &Path) -> usize {
        let mut assets = self
            .assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = assets.len();
        assets.retain(|_, asset| !path_is_equal_or_child(root, &asset.path));
        before.saturating_sub(assets.len())
    }

    /// Remove registry entries that are no longer referenced by persisted
    /// messages. This lets retention GC distinguish active/history-backed
    /// assets from old rows that were already deleted from the database.
    pub fn prune_unreferenced(&self, referenced_paths: &[PathBuf]) -> usize {
        let mut assets = self
            .assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = assets.len();
        assets.retain(|_, asset| {
            referenced_paths
                .iter()
                .any(|path| path_is_equal(path, &asset.path))
        });
        before.saturating_sub(assets.len())
    }
}

fn is_safe_managed_file(root: &Path, path: &Path) -> bool {
    let Ok(root_metadata) = std::fs::symlink_metadata(root) else {
        return false;
    };
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return false;
    }
    let Ok(path_metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !path_metadata.is_file() || is_link_or_reparse(&path_metadata) {
        return false;
    }

    let Ok(canonical_root) = root.canonicalize() else {
        return false;
    };
    let Ok(canonical_path) = path.canonicalize() else {
        return false;
    };
    if canonical_path == canonical_root || !canonical_path.starts_with(&canonical_root) {
        return false;
    }

    // Walk the lexical path as well as the canonical path. This rejects a
    // symlink/reparse point introduced after the initial canonicalization and
    // closes the check-then-use race for the normal host-created path shape.
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if matches!(
            component,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        ) {
            return false;
        }
        current.push(component);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        if is_link_or_reparse(&metadata) {
            return false;
        }
    }
    true
}

fn path_is_equal_or_child(root: &Path, candidate: &Path) -> bool {
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

fn path_is_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn registry_roundtrips_host_metadata_without_normalizing_the_id() {
        let registry = ManagedAssetRegistry::default();
        registry.register_for_test(
            "asset-test",
            PathBuf::from(r"C:\uploads\report.pdf"),
            Some("report.pdf".into()),
            "application/pdf",
        );

        let asset = registry.resolve("asset-test").expect("registered asset");
        assert_eq!(asset.path, PathBuf::from(r"C:\uploads\report.pdf"));
        assert_eq!(asset.filename.as_deref(), Some("report.pdf"));
        assert!(registry.contains("asset-test"));
        assert!(!registry.contains("asset-other"));
    }

    #[test]
    fn register_under_root_rejects_paths_outside_the_managed_root() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let path = outside.path().join("secret.txt");
        std::fs::write(&path, "secret").unwrap();
        let registry = ManagedAssetRegistry::default();

        assert!(!registry.register_under_root(
            root.path(),
            "asset-outside",
            path,
            Some("secret.txt".into()),
            "text/plain",
        ));
        assert!(!registry.contains("asset-outside"));
    }

    #[test]
    fn prune_missing_removes_deleted_assets() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("gone.txt");
        std::fs::write(&path, "gone").unwrap();
        let registry = ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            root.path(),
            "asset-gone",
            path.clone(),
            Some("gone.txt".into()),
            "text/plain",
        ));
        std::fs::remove_file(path).unwrap();

        assert_eq!(registry.prune_missing(), 1);
        assert!(!registry.contains("asset-gone"));
    }

    #[test]
    fn prune_unreferenced_removes_assets_deleted_from_history() {
        let root = TempDir::new().unwrap();
        let keep = root.path().join("keep.txt");
        let old = root.path().join("old.txt");
        std::fs::write(&keep, "keep").unwrap();
        std::fs::write(&old, "old").unwrap();
        let registry = ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            root.path(),
            "asset-keep",
            keep.clone(),
            Some("keep.txt".into()),
            "text/plain",
        ));
        assert!(registry.register_under_root(
            root.path(),
            "asset-old",
            old,
            Some("old.txt".into()),
            "text/plain",
        ));

        assert_eq!(registry.prune_unreferenced(&[keep]), 1);
        assert!(registry.contains("asset-keep"));
        assert!(!registry.contains("asset-old"));
    }
}
