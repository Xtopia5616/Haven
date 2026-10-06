//! Rich file handoff from `files` into the managed media registry.

use crate::{ManagedAsset, ManagedAssetRegistry};
use std::future::Future;
use std::path::Path;

use super::super::media::{MediaOperation, classify_media, register_path_asset};
use super::file_classification::classify_by_extension;

pub(super) fn media_operation_for(asset: &ManagedAsset) -> Option<MediaOperation> {
    match classify_media(asset).0 {
        haven_common::media_detection::DetectedMediaKind::Image => Some(MediaOperation::Describe),
        haven_common::media_detection::DetectedMediaKind::Audio => Some(MediaOperation::Transcribe),
        haven_common::media_detection::DetectedMediaKind::Video => Some(MediaOperation::Inspect),
        haven_common::media_detection::DetectedMediaKind::Document => Some(MediaOperation::Extract),
        _ => None,
    }
}

/// Rich filesystem inputs become managed sources before any information is
/// requested. When the canonical source is a direct child of the generated
/// media root, this handoff shares the registry's GC gate through validation
/// and registration; external paths leave that gate available to cleanup.
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
    let generated_media_root =
        tokio::fs::canonicalize(haven_common::config::default_generated_media_dir())
            .await
            .ok();
    register_canonical_rich_path_asset(
        registry,
        session_id,
        &canonical,
        media_type,
        generated_media_root.as_deref(),
    )
    .await
}

async fn register_canonical_rich_path_asset(
    registry: &ManagedAssetRegistry,
    session_id: Option<&str>,
    canonical: &Path,
    media_type: &str,
    generated_media_root: Option<&Path>,
) -> anyhow::Result<Option<ManagedAsset>> {
    register_canonical_rich_path_asset_with_metadata(
        registry,
        session_id,
        canonical,
        media_type,
        generated_media_root,
        tokio::fs::metadata,
    )
    .await
}

async fn register_canonical_rich_path_asset_with_metadata<F, Fut>(
    registry: &ManagedAssetRegistry,
    session_id: Option<&str>,
    canonical: &Path,
    media_type: &str,
    generated_media_root: Option<&Path>,
    metadata_for: F,
) -> anyhow::Result<Option<ManagedAsset>>
where
    F: FnOnce(std::path::PathBuf) -> Fut,
    Fut: Future<Output = std::io::Result<std::fs::Metadata>>,
{
    // The app cleaner only reclaims direct children of its generated-media
    // root. Resolve the source first so aliases into that root take the same
    // permit, while ordinary external files do not hold up generated-media GC.
    let _generated_media_guard = if canonical.parent() == generated_media_root {
        Some(registry.lock_generated_media_write().await)
    } else {
        None
    };
    let metadata = metadata_for(canonical.to_path_buf()).await?;
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
        canonical,
        media_type,
        filename,
        metadata.len(),
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::task::Poll;
    use tempfile::TempDir;
    use tokio::sync::oneshot;

    fn generated_media_file(root: &Path) -> PathBuf {
        root.join(format!("{}.png", haven_common::types::new_id("file")))
    }

    async fn canonical_file(path: &Path) -> (PathBuf, PathBuf) {
        let canonical = tokio::fs::canonicalize(path).await.unwrap();
        let root = canonical.parent().unwrap().to_path_buf();
        (canonical, root)
    }

    #[tokio::test]
    async fn rich_path_handoff_waits_for_cleanup_before_registering() {
        let temp = TempDir::new().unwrap();
        let path = generated_media_file(temp.path());
        tokio::fs::write(&path, b"image").await.unwrap();
        let (canonical, root) = canonical_file(&path).await;
        let registry = ManagedAssetRegistry::default();
        let session_id = haven_common::types::new_id("ses");
        let (cleanup_entered_tx, cleanup_entered_rx) = oneshot::channel();
        let (allow_cleanup_tx, allow_cleanup_rx) = oneshot::channel();
        let (validation_started_tx, mut validation_started_rx) = oneshot::channel();
        let cleaner_registry = registry.clone();
        let cleanup_path = canonical.clone();
        let cleanup = tokio::spawn(async move {
            let _cleanup_guard = cleaner_registry.lock_generated_media_cleanup().await;
            cleanup_entered_tx.send(()).unwrap();
            allow_cleanup_rx.await.unwrap();
            let cleanup_snapshot = cleaner_registry.leased_paths();
            if !cleanup_snapshot
                .iter()
                .any(|leased| leased == &cleanup_path)
            {
                tokio::fs::remove_file(&cleanup_path).await.unwrap();
            }
        });
        cleanup_entered_rx.await.unwrap();

        let mut handoff = Box::pin(register_canonical_rich_path_asset_with_metadata(
            &registry,
            Some(&session_id),
            &canonical,
            "image/png",
            Some(&root),
            move |path| {
                validation_started_tx.send(()).unwrap();
                tokio::fs::metadata(path)
            },
        ));
        let waited_for_cleanup = std::future::poll_fn(|context| {
            Poll::Ready(handoff.as_mut().poll(context).is_pending())
        })
        .await;
        assert!(
            waited_for_cleanup,
            "handoff must wait at the shared registry gate"
        );
        assert!(
            matches!(
                validation_started_rx.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "handoff must not validate while cleanup owns the gate"
        );
        allow_cleanup_tx.send(()).unwrap();
        cleanup.await.unwrap();

        let result = handoff.await;
        validation_started_rx.await.unwrap();
        assert!(result.is_err(), "removed files cannot be registered");
        assert!(registry.leased_paths().is_empty());
    }

    #[tokio::test]
    async fn rich_path_handoff_registers_before_cleanup_snapshots_leases() {
        let temp = TempDir::new().unwrap();
        let path = generated_media_file(temp.path());
        tokio::fs::write(&path, b"image").await.unwrap();
        let (canonical, root) = canonical_file(&path).await;
        let registry = ManagedAssetRegistry::default();
        let session_id = haven_common::types::new_id("ses");
        let producer_guard = registry.lock_generated_media_write().await;

        let asset = register_canonical_rich_path_asset(
            &registry,
            Some(&session_id),
            &canonical,
            "image/png",
            Some(&root),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(asset.path, canonical);

        let (cleanup_started_tx, cleanup_started_rx) = oneshot::channel();
        let cleaner_registry = registry.clone();
        let cleanup_path = canonical.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_started_tx.send(()).unwrap();
            let _cleanup_guard = cleaner_registry.lock_generated_media_cleanup().await;
            let leased = cleaner_registry.leased_paths();
            if leased.iter().any(|path| path == &cleanup_path) {
                Ok::<bool, std::io::Error>(true)
            } else {
                tokio::fs::remove_file(&cleanup_path).await.map(|()| false)
            }
        });
        cleanup_started_rx.await.unwrap();
        assert!(
            canonical.exists(),
            "cleaner waits while registration is protected"
        );

        drop(producer_guard);
        assert!(
            cleanup.await.unwrap().unwrap(),
            "the session lease protects the file"
        );
        assert!(canonical.exists());
    }

    #[tokio::test]
    async fn external_rich_path_does_not_wait_for_generated_media_cleanup() {
        let generated_root = TempDir::new().unwrap();
        let external_root = TempDir::new().unwrap();
        let path = external_root.path().join("report.png");
        tokio::fs::write(&path, b"image").await.unwrap();
        let canonical = tokio::fs::canonicalize(&path).await.unwrap();
        let generated_root = tokio::fs::canonicalize(generated_root.path())
            .await
            .unwrap();
        let registry = ManagedAssetRegistry::default();
        let cleanup_guard = registry.lock_generated_media_cleanup().await;
        let (validation_started_tx, mut validation_started_rx) = oneshot::channel();
        let mut handoff = Box::pin(register_canonical_rich_path_asset_with_metadata(
            &registry,
            None,
            &canonical,
            "image/png",
            Some(&generated_root),
            move |path| {
                validation_started_tx.send(()).unwrap();
                tokio::fs::metadata(path)
            },
        ));
        let first_poll =
            std::future::poll_fn(|context| Poll::Ready(handoff.as_mut().poll(context))).await;
        assert!(
            validation_started_rx.try_recv().is_ok(),
            "external file validation should proceed while generated-media GC owns its gate"
        );
        drop(cleanup_guard);

        let result = match first_poll {
            Poll::Ready(result) => result,
            Poll::Pending => handoff.await,
        };
        assert!(result.unwrap().is_some());
        assert!(registry.unexpired_transient_paths().contains(&canonical));
    }
}
