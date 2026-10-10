//! Media generation and managed-asset registration.

use chrono::{Duration as ChronoDuration, Utc};
use haven_common::config::{GENERATED_MEDIA_RETENTION_SECS, default_generated_media_root};
use haven_common::media_detection::extension_for_mime_type;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

use crate::asset_registry::GeneratedMediaWriteGuard;
use crate::{ManagedAsset, ManagedAssetRegistry, ToolResult};
use haven_common::media::MediaRepresentationKind;

use super::{MAX_GENERATION_PROMPT_CHARS, MediaParams, MediaTool};

const MAX_GENERATED_MEDIA_BYTES: usize = 16 * 1024 * 1024;

impl MediaTool {
    pub(super) async fn generate(
        &self,
        params: MediaParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let prompt = params
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
            .map(|prompt| {
                prompt
                    .chars()
                    .take(MAX_GENERATION_PROMPT_CHARS)
                    .collect::<String>()
            })
            .ok_or_else(|| anyhow::anyhow!("prompt is required"))?;
        let Some(client) = self.image_gen_client.clone() else {
            let mut output =
                self.media_result_output(super::MediaOperation::Generate, None, None, None);
            output["available"] = json!(false);
            output["capability"] = json!("generate");
            output["reason_code"] = json!("generate_unavailable");
            output["reason"] = json!("No image-generation provider is configured.");
            return Ok(ToolResult::ok(output));
        };
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("image generation cancelled"));
        }
        let timeout_secs = self.timeout_secs.max(1);
        let image = tokio::select! {
            _ = cancel.cancelled() => {
                return Ok(ToolResult::cancelled("image generation cancelled"));
            }
            result = tokio::time::timeout(
                Duration::from_secs(timeout_secs),
                client.generate(&prompt),
            ) => result
                .map_err(|_| {
                    anyhow::anyhow!("image generation timed out after {}s", timeout_secs)
                })??,
        };
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("image generation cancelled"));
        }
        if image.data.is_empty() {
            anyhow::bail!("image generation returned empty media");
        }
        if image.data.len() > MAX_GENERATED_MEDIA_BYTES {
            anyhow::bail!("image generation output exceeds the media size limit");
        }
        if !image.media_type.starts_with("image/") {
            anyhow::bail!("image generation returned a non-image media type");
        }
        let root = default_generated_media_root();
        let extension = extension_for_mime_type(&image.media_type);
        let path = root.join(format!(
            "{}.{}",
            haven_common::types::new_id("file"),
            extension
        ));
        let write_guard = tokio::select! {
            _ = cancel.cancelled() => {
                return Ok(ToolResult::cancelled("image generation cancelled"));
            }
            guard = self.managed_assets.lock_generated_media_write() => guard,
        };
        let registry = self.managed_assets.clone();
        let session_id = params.session_id.clone();
        let media_type = image.media_type;
        let bytes = image.data;
        let cancel_for_write = cancel.clone();
        let asset = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ManagedAsset>> {
            use std::io::Write;

            if cancel_for_write.is_cancelled() {
                return Ok(None);
            }
            let mut created_file = false;
            let result = (|| -> anyhow::Result<Option<ManagedAsset>> {
                std::fs::create_dir_all(&root)?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)?;
                created_file = true;
                if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                    drop(file);
                    return Err(error.into());
                }
                drop(file);
                if cancel_for_write.is_cancelled() {
                    return Ok(None);
                }
                let size = std::fs::metadata(&path)?.len();
                if cancel_for_write.is_cancelled() {
                    return Ok(None);
                }
                let filename = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let asset = register_generated_asset(
                    &registry,
                    &write_guard,
                    session_id.as_deref(),
                    &root,
                    path.clone(),
                    Some(filename),
                    &media_type,
                    size,
                )?;
                Ok(Some(asset))
            })();
            if created_file && !matches!(&result, Ok(Some(_))) {
                let _ = std::fs::remove_file(&path);
            }
            result
        })
        .await??;
        let Some(asset) = asset else {
            return Ok(ToolResult::cancelled("image generation cancelled"));
        };
        let output = self.media_result_output(
            super::MediaOperation::Generate,
            Some(&asset),
            Some(MediaRepresentationKind::RawImage),
            None,
        );
        Ok(ToolResult::ok(output))
    }
}

/// Register a generated tool output in the same lifecycle used by generated
/// attachments. Window capture uses this helper so its asset id is usable by
/// the media tool without introducing a second storage/cleanup path.
// The explicit write permit is part of this helper's lifecycle contract.
#[allow(clippy::too_many_arguments)]
pub(crate) fn register_generated_asset(
    registry: &ManagedAssetRegistry,
    _write_guard: &GeneratedMediaWriteGuard,
    session_id: Option<&str>,
    root: &Path,
    path: std::path::PathBuf,
    filename: Option<String>,
    media_type: &str,
    size_bytes: u64,
) -> anyhow::Result<ManagedAsset> {
    let asset_id = haven_common::types::new_id("asset");
    let expires_at = Utc::now() + ChronoDuration::seconds(GENERATED_MEDIA_RETENTION_SECS as i64);
    let registered = if let Some(session_id) = session_id.filter(|id| !id.trim().is_empty()) {
        registry.register_under_root_for_session_with_metadata(
            session_id,
            root,
            asset_id.clone(),
            path.clone(),
            filename,
            media_type,
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    } else {
        registry.register_under_root_with_metadata(
            root,
            asset_id.clone(),
            path.clone(),
            filename,
            media_type.to_string(),
            None,
            Some(size_bytes),
            Some(expires_at),
        )
    };
    if !registered {
        anyhow::bail!("failed to register generated media asset");
    }
    registry
        .resolve(&asset_id)
        .ok_or_else(|| anyhow::anyhow!("generated media asset disappeared after registration"))
}
