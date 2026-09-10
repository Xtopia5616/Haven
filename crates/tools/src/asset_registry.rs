//! Process-local registry for host-owned media assets.
//!
//! The model-facing contract contains only an opaque `asset_id`. This module
//! is the trusted boundary that resolves that id to a host path for a narrow
//! read-only files operation; paths never need to enter the LLM transcript.

use std::collections::HashMap;
use std::path::PathBuf;
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
    /// Register a host-created asset. Renderer-supplied ids are filtered at
    /// the upload validation boundary before this method is called.
    pub fn register(
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_roundtrips_host_metadata_without_normalizing_the_id() {
        let registry = ManagedAssetRegistry::default();
        registry.register(
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
}
