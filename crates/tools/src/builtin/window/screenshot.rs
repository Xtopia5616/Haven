use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::WindowParams;
use super::WindowTool;
use super::platform;
use crate::builtin::media::{MediaOperation, MediaParams, register_generated_asset};
use crate::{ManagedAsset, ToolResult};

pub(super) struct ManagedCapture {
    pub(super) asset: ManagedAsset,
    pub(super) width: u64,
    pub(super) height: u64,
    pub(super) format: String,
}

impl WindowTool {
    pub(super) async fn observe(
        &self,
        params: WindowParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let window_id = params.window_id.clone();
        let title = params.title.clone();
        let elements = tokio::task::spawn_blocking(move || {
            platform::enumerate_ui_tree(window_id.as_deref(), title.as_deref())
        })
        .await??;
        let count = elements.len();
        let resolved_window_id = elements
            .first()
            .and_then(|element| element.get("window_id"))
            .cloned()
            .or_else(|| params.window_id.clone().map(Value::String));
        let resolved_title = elements
            .first()
            .and_then(|element| element.get("window_title"))
            .cloned()
            .or_else(|| params.title.clone().map(Value::String));
        let capture = self
            .capture_screen(params.session_id.as_deref(), cancel.clone())
            .await?;
        let media_tool = self
            .media_tool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("media runtime is not wired"))?;
        let screenshot = media_tool.media_result_output_named(
            "screenshot",
            Some(&capture.asset),
            Some(haven_common::media::MediaRepresentationKind::ManagedFileRef),
            None,
        );
        let mut output = serde_json::Map::new();
        if let Some(asset_id) = screenshot.get("asset_id") {
            output.insert("asset_id".into(), asset_id.clone());
        }
        if let Some(notes) = screenshot.get("notes") {
            output.insert("notes".into(), notes.clone());
        }
        output.insert("operation".into(), serde_json::json!("observe"));
        output.insert(
            "window".into(),
            serde_json::json!({
                "window_id": resolved_window_id,
                "title": resolved_title,
                "pid": params.pid,
            }),
        );
        output.insert("elements".into(), serde_json::json!(elements));
        output.insert("count".into(), serde_json::json!(count));
        output.insert("screenshot".into(), screenshot);
        let mut output = Value::Object(output);
        if params.ocr.unwrap_or(false) {
            let ocr = media_tool
                .run(
                    MediaParams {
                        operation: MediaOperation::Ocr,
                        asset_id: Some(capture.asset.asset_id.clone()),
                        focus: None,
                        prompt: None,
                        page_index: None,
                        file_path: None,
                        text: None,
                        duration: None,
                        volume: None,
                        muted: None,
                        session_id: params.session_id,
                    },
                    cancel,
                )
                .await?;
            output["ocr"] = ocr.output;
        }
        Ok(ToolResult::ok(output))
    }

    pub(super) async fn capture_screen(
        &self,
        session_id: Option<&str>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ManagedCapture> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let write_guard = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            guard = self.managed_assets.lock_generated_media_write() => guard,
        };
        let registry = self.managed_assets.clone();
        let session_id = session_id.map(str::to_owned);
        let root = self.capture_root.clone();
        let path = root.join(format!("{}.png", haven_common::types::new_id("file")));
        let cleanup_path = path.clone();
        let cancel_for_capture = cancel.clone();
        let capture_result =
            tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ManagedCapture>> {
                if cancel_for_capture.is_cancelled() {
                    return Ok(None);
                }
                std::fs::create_dir_all(&root)?;
                let mut capture_started = false;
                let result = (|| -> anyhow::Result<Option<ManagedCapture>> {
                    if cancel_for_capture.is_cancelled() {
                        return Ok(None);
                    }
                    capture_started = true;
                    let shot = platform::capture_screen(path.clone())?;
                    if cancel_for_capture.is_cancelled() {
                        return Ok(None);
                    }
                    let (Some(width), Some(height), Some(format)) = (
                        shot.get("width").and_then(Value::as_u64),
                        shot.get("height").and_then(Value::as_u64),
                        shot.get("format").and_then(Value::as_str),
                    ) else {
                        anyhow::bail!("screenshot dimensions or format missing");
                    };
                    if width == 0 || height == 0 || format.trim().is_empty() {
                        anyhow::bail!("screenshot dimensions or format are invalid");
                    }
                    let size = std::fs::metadata(&path)?.len();
                    if cancel_for_capture.is_cancelled() {
                        return Ok(None);
                    }
                    let asset = register_generated_asset(
                        &registry,
                        &write_guard,
                        session_id.as_deref(),
                        &root,
                        path.clone(),
                        Some("screenshot.png".into()),
                        "image/png",
                        size,
                    )?;
                    Ok(Some(ManagedCapture {
                        asset,
                        width,
                        height,
                        format: format.to_string(),
                    }))
                })();
                if capture_started && !matches!(&result, Ok(Some(_))) {
                    let _ = std::fs::remove_file(&path);
                }
                result
            })
            .await;
        let capture = match capture_result {
            Ok(result) => result?,
            Err(error) => {
                // A panic in the blocking closure can occur after the capture
                // started. Its guard is dropped only after that work stops.
                // Remove any partial output after join completion.
                let _ = std::fs::remove_file(cleanup_path);
                return Err(error.into());
            }
        };
        capture.ok_or_else(|| anyhow::anyhow!("cancelled"))
    }

    pub(super) async fn ocr(
        &self,
        session_id: Option<&str>,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        // OCR is a thin producer + consumer convenience operation. The
        // screenshot is registered first, then all bytes/capability handling
        // is delegated to the canonical media tool.
        let capture = self.capture_screen(session_id, cancel.clone()).await?;
        self.media_tool
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("media runtime is not wired"))?
            .run(
                MediaParams {
                    operation: MediaOperation::Ocr,
                    asset_id: Some(capture.asset.asset_id),
                    focus: None,
                    prompt: None,
                    page_index: None,
                    file_path: None,
                    text: None,
                    duration: None,
                    volume: None,
                    muted: None,
                    session_id: session_id.map(str::to_owned),
                },
                cancel,
            )
            .await
    }
}
