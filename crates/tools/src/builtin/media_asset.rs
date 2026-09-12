//! Managed media asset registration shared by file and media producers.

use chrono::{Duration as ChronoDuration, Utc};
use haven_common::config::GENERATED_MEDIA_RETENTION_SECS;
use std::path::Path;

use crate::{ManagedAsset, ManagedAssetRegistry};

/// Register a host-selected file as a short-lived media source. The path has
/// already crossed the trusted filesystem boundary in `files`; this helper
/// only assigns the opaque asset identity and common lifecycle metadata.
pub(crate) fn register_path_asset(
    registry: &ManagedAssetRegistry,
    session_id: Option<&str>,
    path: &Path,
    media_type: &str,
    filename: Option<String>,
    size_bytes: u64,
) -> anyhow::Result<ManagedAsset> {
    let root = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("media source has no managed parent"))?;
    let asset_id = haven_common::types::new_id("asset");
    let expires_at = Utc::now() + ChronoDuration::seconds(GENERATED_MEDIA_RETENTION_SECS as i64);
    let registered = if let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) {
        registry.register_under_root_for_session_with_metadata(
            session_id,
            root,
            asset_id.clone(),
            path.to_path_buf(),
            filename,
            media_type.to_string(),
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    } else {
        registry.register_under_root_with_metadata(
            root,
            asset_id.clone(),
            path.to_path_buf(),
            filename,
            media_type.to_string(),
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    };
    if !registered {
        anyhow::bail!("failed to register media source");
    }
    registry
        .resolve(&asset_id)
        .ok_or_else(|| anyhow::anyhow!("media source disappeared after registration"))
}
