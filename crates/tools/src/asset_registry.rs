//! Process-local registry for host-owned media assets.
//!
//! The model-facing contract contains only an opaque `asset_id`. This module
//! is the trusted boundary that resolves that id to a host path for a narrow
//! read-only files operation; paths never need to enter the LLM transcript.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use haven_platform::filesystem::is_link_or_reparse_point;

/// Host-owned metadata needed to resolve a managed attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedAsset {
    pub asset_id: String,
    pub path: PathBuf,
    managed_root: PathBuf,
    pub filename: Option<String>,
    pub media_type: String,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub expires_at: Option<DateTime<Utc>>,
}

/// In-process mapping from opaque asset ids to host-owned files.
#[derive(Debug, Clone, Default)]
pub struct ManagedAssetRegistry {
    assets: Arc<RwLock<HashMap<String, ManagedAsset>>>,
    /// Assets held by a live session remain protected even before the
    /// attachment has been projected into `messages`.
    session_leases: Arc<RwLock<HashMap<String, HashSet<String>>>>,
    /// Assets registered just before a new session id is allocated. These
    /// short-lived ingress leases close the pre-session handoff window.
    pending_assets: Arc<RwLock<HashSet<String>>>,
    /// Serializes generated-media writes and GC from file creation through
    /// registry registration, and from the GC snapshot through unlink.
    generated_media_gate: Arc<tokio::sync::RwLock<()>>,
}

/// Exclusive access to the generated-media cleanup window.
///
/// The app holds this from before it snapshots leases/TTLs until its generated
/// media scan and unlink pass has finished.
#[must_use = "the cleanup guard must live through the generated-media scan and unlink pass"]
pub struct GeneratedMediaCleanupGuard {
    _guard: tokio::sync::OwnedRwLockWriteGuard<()>,
}

/// Shared access held while a producer writes and registers one file or a
/// rich-path handoff validates and registers an existing file.
#[must_use = "the shared guard must live through generated-media validation and registration"]
pub(crate) struct GeneratedMediaWriteGuard {
    _guard: tokio::sync::OwnedRwLockReadGuard<()>,
}

impl ManagedAssetRegistry {
    /// Enter the generated-media GC window. Producers that already hold a
    /// write guard finish registration first; later producers wait until the
    /// caller has completed its snapshot and unlink pass.
    pub async fn lock_generated_media_cleanup(&self) -> GeneratedMediaCleanupGuard {
        GeneratedMediaCleanupGuard {
            _guard: self.generated_media_gate.clone().write_owned().await,
        }
    }

    /// Reserve the generated-media lifecycle while a producer creates and
    /// registers one file, or while a rich-path handoff validates and
    /// registers an existing file that may be reclaimed by generated-media
    /// cleanup. Kept crate-private so the registry remains the owner of this
    /// coordination.
    pub(crate) async fn lock_generated_media_write(&self) -> GeneratedMediaWriteGuard {
        GeneratedMediaWriteGuard {
            _guard: self.generated_media_gate.clone().read_owned().await,
        }
    }

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
        self.register_under_root_with_metadata(
            root,
            asset_id,
            path,
            filename,
            media_type.into(),
            None,
            None,
            None,
        )
    }

    /// Register a host-produced asset with integrity and expiry metadata.
    ///
    /// This is the non-session variant for short-lived tool outputs. Callers
    /// that have a session should prefer
    /// [`Self::register_under_root_for_session_with_metadata`] so cleanup
    /// cannot reclaim the file while the ReAct run is still using it.
    #[allow(clippy::too_many_arguments)]
    pub fn register_under_root_with_metadata(
        &self,
        root: &Path,
        asset_id: String,
        path: PathBuf,
        filename: Option<String>,
        media_type: String,
        sha256: Option<String>,
        size_bytes: Option<u64>,
        expires_at: Option<DateTime<Utc>>,
    ) -> bool {
        if asset_id.trim().is_empty()
            || path.as_os_str().is_empty()
            || !is_safe_managed_file(root, &path)
        {
            return false;
        }
        let asset = ManagedAsset {
            asset_id: asset_id.clone(),
            path,
            managed_root: root.to_path_buf(),
            filename,
            media_type,
            sha256,
            size_bytes,
            expires_at,
        };
        self.assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(asset_id, asset);
        true
    }

    /// Register an asset and hold it for the lifetime of a session. The
    /// session lease closes the GC window between event persistence and the
    /// materialized `messages.ui_metadata` projection.
    pub fn register_under_root_for_session(
        &self,
        session_id: &str,
        root: &Path,
        asset_id: impl Into<String>,
        path: PathBuf,
        filename: Option<String>,
        media_type: impl Into<String>,
    ) -> bool {
        if session_id.trim().is_empty() {
            return false;
        }
        let asset_id = asset_id.into();
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(asset_id.clone());
        if !self.register_under_root_with_metadata(
            root,
            asset_id.clone(),
            path,
            filename,
            media_type.into(),
            None,
            None,
            None,
        ) {
            self.release_pending(&asset_id);
            return false;
        }
        self.bind_pending_to_session(session_id, &asset_id)
    }

    /// Register generated media with its integrity metadata and hold it for
    /// the lifetime of a session. Session-owned assets follow their session's
    /// durable attachment references; the independent transient-asset TTL
    /// only applies when no session owns the asset.
    #[allow(clippy::too_many_arguments)]
    pub fn register_under_root_for_session_with_metadata(
        &self,
        session_id: &str,
        root: &Path,
        asset_id: impl Into<String>,
        path: PathBuf,
        filename: Option<String>,
        media_type: impl Into<String>,
        sha256: Option<String>,
        size_bytes: Option<u64>,
        _expires_at: Option<DateTime<Utc>>,
    ) -> bool {
        if session_id.trim().is_empty() {
            return false;
        }
        let asset_id = asset_id.into();
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(asset_id.clone());
        if !self.register_under_root_with_metadata(
            root,
            asset_id.clone(),
            path,
            filename,
            media_type.into(),
            sha256,
            size_bytes,
            // Once an asset belongs to a session, expiry is governed by the
            // session's retention/deletion lifecycle. `expires_at` remains
            // available for detached, runtime-only media registrations.
            None,
        ) {
            self.release_pending(&asset_id);
            return false;
        }
        self.bind_pending_to_session(session_id, &asset_id)
    }

    /// Register an asset while ingress is creating a new session. The caller
    /// must bind or release this pending lease when ingress returns.
    pub fn register_under_root_pending(
        &self,
        root: &Path,
        asset_id: impl Into<String>,
        path: PathBuf,
        filename: Option<String>,
        media_type: impl Into<String>,
    ) -> bool {
        let asset_id = asset_id.into();
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(asset_id.clone());
        if !self.register_under_root(root, asset_id.clone(), path, filename, media_type) {
            self.release_pending(&asset_id);
            return false;
        }
        true
    }

    /// Associate an already registered asset with a live session.
    pub fn lease_for_session(&self, session_id: &str, asset_id: &str) -> bool {
        if session_id.trim().is_empty() || asset_id.trim().is_empty() || !self.contains(asset_id) {
            return false;
        }
        self.session_leases
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(session_id.to_string())
            .or_default()
            .insert(asset_id.to_string());
        true
    }

    /// Convert a pending ingress lease into a session lease.
    pub fn bind_pending_to_session(&self, session_id: &str, asset_id: &str) -> bool {
        if !self.lease_for_session(session_id, asset_id) {
            return false;
        }
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(asset_id);
        true
    }

    /// Move one already leased asset when ingress replaces a stale active
    /// session id with the actual newly-created session id.
    pub fn transfer_session_lease(
        &self,
        from_session_id: &str,
        to_session_id: &str,
        asset_id: &str,
    ) -> bool {
        if from_session_id.trim().is_empty()
            || to_session_id.trim().is_empty()
            || asset_id.trim().is_empty()
            || !self.contains(asset_id)
        {
            return false;
        }
        if from_session_id == to_session_id {
            return self.lease_for_session(to_session_id, asset_id);
        }
        let mut leases = self
            .session_leases
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(from_assets) = leases.get_mut(from_session_id) else {
            return false;
        };
        if !from_assets.remove(asset_id) {
            return false;
        }
        if from_assets.is_empty() {
            leases.remove(from_session_id);
        }
        leases
            .entry(to_session_id.to_string())
            .or_default()
            .insert(asset_id.to_string());
        true
    }

    /// Release a pending ingress lease after session creation failed.
    pub fn release_pending(&self, asset_id: &str) -> bool {
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(asset_id)
    }

    /// Release every asset lease owned by a terminal session.
    pub fn release_session(&self, session_id: &str) -> usize {
        self.session_leases
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
            .map_or(0, |assets| assets.len())
    }

    /// Return paths protected specifically by active session leases.
    pub fn leased_paths(&self) -> Vec<PathBuf> {
        let leased_ids: HashSet<String> = self
            .session_leases
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .flat_map(|assets| assets.iter().cloned())
            .collect();
        let assets = self
            .assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        leased_ids
            .iter()
            .filter_map(|asset_id| assets.get(asset_id).map(|asset| asset.path.clone()))
            .collect()
    }

    /// Return paths protected by either a live session lease or a pending
    /// ingress lease. Cleanup must include pending uploads while their new
    /// session id is being allocated.
    pub fn leased_or_pending_paths(&self) -> Vec<PathBuf> {
        let mut leased_ids: HashSet<String> = self
            .session_leases
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .flat_map(|assets| assets.iter().cloned())
            .collect();
        leased_ids.extend(
            self.pending_assets
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .cloned(),
        );
        let assets = self
            .assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        leased_ids
            .iter()
            .filter_map(|asset_id| assets.get(asset_id).map(|asset| asset.path.clone()))
            .collect()
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
        let managed_root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        self.assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                asset_id.clone(),
                ManagedAsset {
                    asset_id,
                    path,
                    managed_root,
                    filename,
                    media_type: media_type.into(),
                    sha256: None,
                    size_bytes: None,
                    expires_at: None,
                },
            );
    }

    pub fn resolve(&self, asset_id: &str) -> Option<ManagedAsset> {
        let asset = self
            .assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(asset_id)
            .cloned()?;
        if asset
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            return None;
        }
        Some(asset)
    }

    /// Re-run the managed-root and reparse-point checks immediately before a
    /// read. Registration is intentionally not treated as permanent proof:
    /// a file or parent directory may be replaced after the asset enters the
    /// registry.
    pub fn revalidate(&self, asset: &ManagedAsset) -> bool {
        self.resolve(&asset.asset_id)
            .is_some_and(|current| current == *asset)
            && is_safe_managed_file(&asset.managed_root, &asset.path)
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

    /// Return unleased runtime-only assets whose explicit TTL is still live.
    /// Session-owned assets have no TTL and are retained by their durable
    /// message reference or active lease instead.
    pub fn unexpired_transient_paths(&self) -> Vec<PathBuf> {
        let now = Utc::now();
        let leased_ids: HashSet<String> = self
            .session_leases
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .flat_map(|assets| assets.iter().cloned())
            .collect();
        self.assets
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(asset_id, asset)| {
                !leased_ids.contains(*asset_id)
                    && asset.expires_at.is_some_and(|expires_at| expires_at > now)
            })
            .map(|(_, asset)| asset.path.clone())
            .collect()
    }

    /// Remove registry entries whose files no longer exist or are no longer
    /// regular files. This bounds the process-local map after retention GC.
    pub fn prune_missing(&self) -> usize {
        let mut assets = self
            .assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed: HashSet<String> = assets
            .iter()
            .filter_map(|(asset_id, asset)| {
                let exists = std::fs::symlink_metadata(&asset.path)
                    .map(|metadata| metadata.is_file() && !is_link_or_reparse_point(&metadata))
                    .unwrap_or(false);
                (!exists).then_some(asset_id.clone())
            })
            .collect();
        assets.retain(|asset_id, _| !removed.contains(asset_id));
        drop(assets);
        self.remove_leases_for_assets(&removed);
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|asset_id| !removed.contains(asset_id));
        removed.len()
    }

    fn remove_leases_for_assets(&self, removed: &HashSet<String>) {
        if removed.is_empty() {
            return;
        }
        let mut leases = self
            .session_leases
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        leases.retain(|_, assets| {
            assets.retain(|asset_id| !removed.contains(asset_id));
            !assets.is_empty()
        });
    }

    /// Remove entries that point into a successfully deleted managed batch.
    pub fn prune_paths_under(&self, root: &Path) -> usize {
        let mut assets = self
            .assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let removed: HashSet<String> = assets
            .iter()
            .filter_map(|(asset_id, asset)| {
                haven_common::path::path_is_equal_or_child(root, &asset.path)
                    .then_some(asset_id.clone())
            })
            .collect();
        assets.retain(|asset_id, _| !removed.contains(asset_id));
        drop(assets);
        self.remove_leases_for_assets(&removed);
        self.pending_assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|asset_id| !removed.contains(asset_id));
        removed.len()
    }

    /// Remove registry entries that are no longer referenced by persisted
    /// messages or an active/pending lease. This lets retention GC distinguish
    /// in-flight assets from entries whose durable owners were deleted.
    pub fn prune_unreferenced(&self, referenced_paths: &[PathBuf]) -> usize {
        let mut leased_ids: HashSet<String> = self
            .session_leases
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .flat_map(|assets| assets.iter().cloned())
            .collect();
        leased_ids.extend(
            self.pending_assets
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .cloned(),
        );
        let mut assets = self
            .assets
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let before = assets.len();
        assets.retain(|asset_id, asset| {
            leased_ids.contains(asset_id)
                || referenced_paths
                    .iter()
                    .any(|path| haven_common::path::path_is_equal(path, &asset.path))
        });
        before.saturating_sub(assets.len())
    }
}

fn is_safe_managed_file(root: &Path, path: &Path) -> bool {
    let Ok(root_metadata) = std::fs::symlink_metadata(root) else {
        return false;
    };
    if !root_metadata.is_dir() || is_link_or_reparse_point(&root_metadata) {
        return false;
    }
    let Ok(path_metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !path_metadata.is_file() || is_link_or_reparse_point(&path_metadata) {
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
        if is_link_or_reparse_point(&metadata) {
            return false;
        }
    }
    true
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

    #[test]
    fn active_session_lease_survives_missing_message_projection() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("active.txt");
        std::fs::write(&path, "active").unwrap();
        let registry = ManagedAssetRegistry::default();

        assert!(registry.register_under_root_for_session(
            "ses-active",
            root.path(),
            "asset-active",
            path.clone(),
            Some("active.txt".into()),
            "text/plain",
        ));
        assert_eq!(registry.prune_unreferenced(&[]), 0);
        assert_eq!(registry.leased_paths(), vec![path.clone()]);

        assert_eq!(registry.release_session("ses-active"), 1);
        assert_eq!(registry.prune_unreferenced(&[]), 1);
        assert!(!registry.contains("asset-active"));
    }

    #[test]
    fn pruning_deleted_batch_also_releases_session_lease() {
        let root = TempDir::new().unwrap();
        let batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let path = batch.join("active.txt");
        std::fs::create_dir_all(&batch).unwrap();
        std::fs::write(&path, "active").unwrap();
        let registry = ManagedAssetRegistry::default();

        assert!(registry.register_under_root_for_session(
            "ses-active",
            root.path(),
            "asset-active",
            path,
            Some("active.txt".into()),
            "text/plain",
        ));
        assert_eq!(registry.prune_paths_under(&batch), 1);
        assert!(registry.leased_paths().is_empty());
        assert_eq!(registry.release_session("ses-active"), 0);
    }

    #[tokio::test]
    async fn generated_media_gc_waits_for_write_and_registration() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("file-generated.png");
        let registry = ManagedAssetRegistry::default();
        let write_guard = registry.lock_generated_media_write().await;
        std::fs::write(&path, b"complete image").unwrap();

        let cleaner_registry = registry.clone();
        let cleaner_path = path.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let cleaner = tokio::spawn(async move {
            started_tx.send(()).unwrap();
            let _cleanup_guard = cleaner_registry.lock_generated_media_cleanup().await;
            cleaner_registry
                .leased_paths()
                .iter()
                .any(|leased_path| haven_common::path::path_is_equal(leased_path, &cleaner_path))
        });
        started_rx.await.unwrap();
        assert!(
            !cleaner.is_finished(),
            "GC must wait for producer registration"
        );

        assert!(registry.register_under_root_for_session(
            "ses-generated",
            root.path(),
            "asset-generated",
            path.clone(),
            Some("generated.png".into()),
            "image/png",
        ));
        drop(write_guard);

        assert!(cleaner.await.unwrap());
        assert!(path.exists());
    }

    #[tokio::test]
    async fn aborted_producer_keeps_gc_outside_active_blocking_write() {
        use std::io::Write;

        let root = TempDir::new().unwrap();
        let path = root.path().join("file-cancelled-producer.png");
        let producer_registry = ManagedAssetRegistry::default();
        let worker_registry = producer_registry.clone();
        let worker_root = root.path().to_path_buf();
        let worker_path = path.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let producer = tokio::spawn(async move {
            let write_guard = worker_registry.lock_generated_media_write().await;
            tokio::task::spawn_blocking(move || {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&worker_path)
                    .unwrap();
                file.write_all(b"partial").unwrap();
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                file.write_all(b" complete").unwrap();
                drop(file);
                assert!(worker_registry.register_under_root_for_session(
                    "ses-cancelled-producer",
                    &worker_root,
                    "asset-cancelled-producer",
                    worker_path.clone(),
                    Some("generated.png".into()),
                    "image/png",
                ));
                drop(write_guard);
            })
            .await
            .unwrap();
        });
        entered_rx.await.unwrap();
        producer.abort();
        assert!(producer.await.unwrap_err().is_cancelled());

        let cleaner_registry = producer_registry.clone();
        let (cleaner_started_tx, cleaner_started_rx) = tokio::sync::oneshot::channel();
        let cleaner = tokio::spawn(async move {
            cleaner_started_tx.send(()).unwrap();
            let _cleanup_guard = cleaner_registry.lock_generated_media_cleanup().await;
        });
        cleaner_started_rx.await.unwrap();
        assert!(
            !cleaner.is_finished(),
            "GC must wait for the detached writer"
        );

        release_tx.send(()).unwrap();
        cleaner.await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"partial complete");
        assert!(producer_registry.contains("asset-cancelled-producer"));
    }

    #[tokio::test]
    async fn aborted_gc_keeps_producer_outside_active_blocking_unlink() {
        use std::io::Write;

        let root = TempDir::new().unwrap();
        let stale_path = root.path().join("file-stale.png");
        let fresh_path = root.path().join("file-fresh.png");
        std::fs::write(&stale_path, b"stale").unwrap();
        let registry = ManagedAssetRegistry::default();
        let cleanup_registry = registry.clone();
        let stale_path_for_cleanup = stale_path.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cleaner = tokio::spawn(async move {
            let cleanup_guard = cleanup_registry.lock_generated_media_cleanup().await;
            tokio::task::spawn_blocking(move || {
                let _cleanup_guard = cleanup_guard;
                std::fs::remove_file(&stale_path_for_cleanup).unwrap();
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            })
            .await
            .unwrap();
        });
        entered_rx.await.unwrap();
        cleaner.abort();
        assert!(cleaner.await.unwrap_err().is_cancelled());

        let producer_registry = registry.clone();
        let producer_root = root.path().to_path_buf();
        let fresh_path_for_producer = fresh_path.clone();
        let (producer_started_tx, producer_started_rx) = tokio::sync::oneshot::channel();
        let producer = tokio::spawn(async move {
            producer_started_tx.send(()).unwrap();
            let write_guard = producer_registry.lock_generated_media_write().await;
            let mut file = std::fs::File::create(&fresh_path_for_producer).unwrap();
            file.write_all(b"fresh").unwrap();
            assert!(producer_registry.register_under_root_for_session(
                "ses-after-gc",
                &producer_root,
                "asset-after-gc",
                fresh_path_for_producer.clone(),
                Some("fresh.png".into()),
                "image/png",
            ));
            drop(write_guard);
        });
        producer_started_rx.await.unwrap();
        assert!(
            !producer.is_finished(),
            "producer must wait for the active GC unlink"
        );

        release_tx.send(()).unwrap();
        producer.await.unwrap();
        assert!(!stale_path.exists());
        assert_eq!(std::fs::read(&fresh_path).unwrap(), b"fresh");
    }
}
