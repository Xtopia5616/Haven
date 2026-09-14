//! Media generation and managed-asset registration.

use chrono::{Duration as ChronoDuration, Utc};
use haven_common::config::{GENERATED_MEDIA_RETENTION_SECS, default_generated_media_dir};
use haven_common::media_detection::extension_for_media_type;
use serde_json::json;
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

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
        let root = default_generated_media_dir();
        tokio::fs::create_dir_all(&root).await?;
        if cancel.is_cancelled() {
            return Ok(ToolResult::cancelled("image generation cancelled"));
        }
        let extension = extension_for_media_type(&image.media_type);
        let path = root.join(format!(
            "{}.{}",
            haven_common::types::new_id("file"),
            extension
        ));
        let write_path = path.clone();
        let bytes = image.data;
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&write_path)?;
            if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
                drop(file);
                let _ = std::fs::remove_file(&write_path);
                return Err(error.into());
            }
            Ok(())
        })
        .await??;
        if cancel.is_cancelled() {
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(ToolResult::cancelled("image generation cancelled"));
        }
        let size = tokio::fs::metadata(&path).await?.len();
        if cancel.is_cancelled() {
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(ToolResult::cancelled("image generation cancelled"));
        }
        let asset = match register_generated_asset(
            &self.managed_assets,
            params.session_id.as_deref(),
            &root,
            path.clone(),
            Some(
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ),
            &image.media_type,
            size,
        ) {
            Ok(asset) => asset,
            Err(error) => {
                let _ = tokio::fs::remove_file(&path).await;
                return Err(error);
            }
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
pub(crate) fn register_generated_asset(
    registry: &ManagedAssetRegistry,
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
