//! Rich file handoff from `files` into the managed media registry.

use crate::{ManagedAsset, ManagedAssetRegistry};

use super::super::media::{MediaOperation, classify_media, register_path_asset};
use super::file_classification::classify_by_extension;

pub(super) fn media_operation_for(asset: &ManagedAsset) -> Option<MediaOperation> {
    match classify_media(asset).0 {
        haven_common::media_detection::MediaType::Image => Some(MediaOperation::Describe),
        haven_common::media_detection::MediaType::Audio => Some(MediaOperation::Transcribe),
        haven_common::media_detection::MediaType::Video => Some(MediaOperation::Inspect),
        haven_common::media_detection::MediaType::Document => Some(MediaOperation::Extract),
        _ => None,
    }
}

/// Rich filesystem inputs become managed sources before any information is
/// requested. The path was validated by the caller; this module owns only the
/// handoff and common asset registration.
pub(super) async fn register_rich_path_asset(
    registry: &ManagedAssetRegistry,
    session_id: Option<&str>,
    path: &str,
) -> anyhow::Result<Option<ManagedAsset>> {
    let (kind, media_type) = classify_by_extension(path);
    if !matches!(kind, "image" | "audio" | "video" | "pdf" | "office") {
        return Ok(None);
    }
    let canonical = tokio::fs::canonicalize(path).await?;
    let metadata = tokio::fs::metadata(&canonical).await?;
    if !metadata.is_file() {
        return Ok(None);
    }
    let filename = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned);
    Ok(Some(register_path_asset(
        registry,
        session_id,
        &canonical,
        media_type,
        filename,
        metadata.len(),
    )?))
}
