use async_trait::async_trait;
use haven_common::config::default_generated_media_dir;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

use crate::{ManagedAsset, ManagedAssetRegistry, Tool, ToolConcurrency, ToolResult};

const MAX_CLIPBOARD_FILES: usize = 32;
const MAX_CLIPBOARD_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// One clipboard history entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClipboardEntry {
    pub content: String,
    pub timestamp_ms: u64,
}

/// In-memory clipboard history shared across clipboard tool instances
/// (survives catalog rebuilds). Newest entries first.
pub struct ClipboardHistory {
    entries: Mutex<VecDeque<ClipboardEntry>>,
    max_entries: usize,
}

impl ClipboardHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(VecDeque::new()),
            max_entries: max_entries.max(1),
        }
    }

    /// Record a copied/read text. Re-recording an existing entry moves it to
    /// the front (bumps its timestamp) instead of duplicating it.
    pub fn record(&self, content: String) {
        if content.is_empty() {
            return;
        }
        let timestamp_ms = now_ms();
        let mut entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(idx) = entries.iter().position(|e| e.content == content) {
            let mut entry = entries.remove(idx).unwrap();
            entry.timestamp_ms = timestamp_ms;
            entries.push_front(entry);
        } else {
            entries.push_front(ClipboardEntry {
                content,
                timestamp_ms,
            });
        }
        while entries.len() > self.max_entries {
            entries.pop_back();
        }
    }

    /// Recent entries, newest first, capped at `limit`.
    pub fn recent(&self, limit: usize) -> Vec<ClipboardEntry> {
        let entries = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        entries.iter().take(limit).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Per-entry content truncation so a history dump stays readable.
pub struct ClipboardTool {
    history: Arc<ClipboardHistory>,
    /// Output cap (chars) for clipboard content.
    max_output_chars: usize,
    /// Default `limit` for the `history` operation when the caller omits it.
    default_limit: usize,
    /// Upper clamp for the `history` operation's `limit` argument.
    max_history_limit: usize,
    /// Per-entry content truncation for history dumps.
    entry_max_chars: usize,
    /// Managed assets available for image/file clipboard transfers.
    managed_assets: ManagedAssetRegistry,
}

/// Clipboard operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardOperation {
    Read,
    Write,
    History,
}

/// Native clipboard representation to read or write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardFormat {
    Auto,
    Text,
    Html,
    Image,
    Files,
}

/// Typed parameters for `ClipboardTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `ClipboardTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ClipboardParams {
    /// Operation to perform; defaults to `read`.
    #[serde(default)]
    pub operation: Option<ClipboardOperation>,
    /// Content for the write operation.
    #[serde(default)]
    pub content: Option<String>,
    /// Entry limit for the history operation (clamped to the tool cap).
    #[serde(default)]
    pub limit: Option<u64>,
    /// Representation to read/write. `auto` prefers text and falls back to
    /// image or file-list data.
    #[serde(default)]
    pub format: Option<ClipboardFormat>,
    /// HTML payload for a rich write.
    #[serde(default)]
    pub html: Option<String>,
    /// Managed image asset to place on the native clipboard.
    #[serde(default)]
    pub asset_id: Option<String>,
    /// File paths to place on the native clipboard.
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

impl ClipboardTool {
    pub fn new(
        history: Arc<ClipboardHistory>,
        max_output_chars: usize,
        default_limit: usize,
        max_history_limit: usize,
        entry_max_chars: usize,
    ) -> Self {
        Self {
            history,
            max_output_chars,
            default_limit,
            max_history_limit,
            entry_max_chars,
            managed_assets: ManagedAssetRegistry::default(),
        }
    }

    pub(crate) fn with_managed_assets(mut self, managed_assets: ManagedAssetRegistry) -> Self {
        self.managed_assets = managed_assets;
        self
    }
}

#[async_trait]
impl Tool for ClipboardTool {
    fn name(&self) -> String {
        "clipboard".into()
    }
    fn description(&self) -> String {
        crate::prompts::CLIPBOARD_DESCRIPTION.into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("write") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        if input["operation"].as_str() == Some("write") {
            ToolConcurrency::Resource("clipboard".into())
        } else {
            ToolConcurrency::SharedResource("clipboard".into())
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write", "history"] },
                "format": { "type": "string", "enum": ["auto", "text", "html", "image", "files"] },
                "content": { "type": "string" },
                "html": { "type": "string" },
                "asset_id": { "type": "string", "pattern": "^asset-[0-9a-f]{32}$" },
                "files": { "type": "array", "minItems": 1, "maxItems": MAX_CLIPBOARD_FILES, "items": { "type": "string", "minLength": 1 } },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
            },
            "required": ["operation"],
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "read" }, "format": { "type": "string", "enum": ["auto", "text", "html", "image", "files"] } },
                    "required": ["operation"]
                },
                {
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "content": { "type": "string" }
                            },
                            "required": ["operation", "content"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "format": { "const": "text" },
                                "content": { "type": "string" }
                            },
                            "required": ["operation", "format", "content"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "format": { "const": "html" },
                                "content": { "type": "string" },
                                "html": { "type": "string" }
                            },
                            "required": ["operation", "format", "html"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "format": { "const": "image" },
                                "asset_id": { "type": "string", "pattern": "^asset-[0-9a-f]{32}$" }
                            },
                            "required": ["operation", "format", "asset_id"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "format": { "const": "files" },
                                "files": { "type": "array", "minItems": 1, "maxItems": MAX_CLIPBOARD_FILES, "items": { "type": "string", "minLength": 1 } }
                            },
                            "required": ["operation", "format", "files"]
                        }
                    ]
                },
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "operation": { "const": "history" },
                        "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                    },
                    "required": ["operation"]
                }
            ]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `ClipboardParams`,
    /// then land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params =
            crate::tool_contract::parse_tool_input::<ClipboardParams>(&self.name(), input)?;
        self.run(params, cancel).await
    }
}

impl ClipboardTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: ClipboardParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        match params.operation.unwrap_or(ClipboardOperation::Read) {
            ClipboardOperation::Read => {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let format = params.format.unwrap_or(ClipboardFormat::Auto);
                let read = tokio::task::spawn_blocking(move || read_clipboard(format)).await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                match read {
                    ClipboardRead::Text(text) => {
                        self.history.record(text.clone());
                        let (text, truncated) =
                            haven_common::encoding::truncate_output(&text, self.max_output_chars);
                        let mut result = serde_json::json!({"operation": "read", "format": "text", "content": text});
                        if truncated {
                            result["truncated"] = serde_json::Value::Bool(true);
                        }
                        Ok(ToolResult::from_output(result, truncated))
                    }
                    ClipboardRead::Html(html) => {
                        let (html, truncated) =
                            haven_common::encoding::truncate_output(&html, self.max_output_chars);
                        Ok(ToolResult::from_output(
                            serde_json::json!({ "operation": "read", "format": "html", "html": html }),
                            truncated,
                        ))
                    }
                    ClipboardRead::Image(image) => {
                        let asset = save_image_asset(&self.managed_assets, image)?;
                        Ok(ToolResult::ok(serde_json::json!({
                            "operation": "read", "format": "image", "asset_id": asset.asset_id,
                            "media_type": asset.media_type, "size_bytes": asset.size_bytes,
                        })))
                    }
                    ClipboardRead::Files(paths) => {
                        let assets = copy_file_assets(&self.managed_assets, paths)?;
                        Ok(ToolResult::ok(serde_json::json!({
                            "operation": "read", "format": "files",
                            "asset_ids": assets.iter().map(|asset| asset.asset_id.clone()).collect::<Vec<_>>(),
                            "files": assets.iter().map(|asset| serde_json::json!({"asset_id": asset.asset_id, "filename": asset.filename, "media_type": asset.media_type, "size_bytes": asset.size_bytes})).collect::<Vec<_>>(),
                        })))
                    }
                }
            }
            ClipboardOperation::Write => {
                let format = params.format.unwrap_or_else(|| {
                    if params.html.is_some() {
                        ClipboardFormat::Html
                    } else {
                        ClipboardFormat::Text
                    }
                });
                let content = params.content.clone();
                let html = params.html.clone();
                let asset_id = params.asset_id.clone();
                let files = params.files.clone();
                let registry = self.managed_assets.clone();
                tokio::task::spawn_blocking(move || {
                    write_clipboard(format, content, html, asset_id, files, registry)
                })
                .await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                if let Some(content) = params.content.filter(|content| !content.is_empty()) {
                    self.history.record(content);
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"operation": "write", "format": format, "written": true}),
                ))
            }
            ClipboardOperation::History => {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let limit = params
                    .limit
                    .map(|l| (l.min(self.max_history_limit as u64)) as usize)
                    .unwrap_or(self.default_limit);
                let entries = self.history.recent(limit);
                let json_entries: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        let (content, _) = haven_common::encoding::truncate_output(
                            &e.content,
                            self.entry_max_chars,
                        );
                        serde_json::json!({
                            "content": content,
                            "timestamp_ms": e.timestamp_ms,
                        })
                    })
                    .collect();
                Ok(ToolResult::ok(serde_json::json!({
                    "operation": "history",
                    "entries": json_entries,
                    "total": self.history.len(),
                })))
            }
        }
    }
}

enum ClipboardRead {
    Text(String),
    Html(String),
    Image(arboard::ImageData<'static>),
    Files(Vec<PathBuf>),
}

fn read_clipboard(format: ClipboardFormat) -> anyhow::Result<ClipboardRead> {
    match format {
        ClipboardFormat::Text => {
            let mut clipboard = arboard::Clipboard::new()?;
            Ok(ClipboardRead::Text(clipboard.get_text()?))
        }
        ClipboardFormat::Html => read_html(),
        ClipboardFormat::Image => {
            let mut clipboard = arboard::Clipboard::new()?;
            Ok(ClipboardRead::Image(clipboard.get_image()?))
        }
        ClipboardFormat::Files => read_files(),
        ClipboardFormat::Auto => {
            let text_result =
                arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text());
            if let Ok(text) = text_result {
                return Ok(ClipboardRead::Text(text));
            }
            if let Ok(html) = read_html() {
                return Ok(html);
            }
            let image_result =
                arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_image());
            if let Ok(image) = image_result {
                return Ok(ClipboardRead::Image(image));
            }
            read_files()
        }
    }
}

fn write_clipboard(
    format: ClipboardFormat,
    content: Option<String>,
    html: Option<String>,
    asset_id: Option<String>,
    files: Option<Vec<String>>,
    registry: ManagedAssetRegistry,
) -> anyhow::Result<()> {
    match format {
        ClipboardFormat::Text | ClipboardFormat::Auto => {
            let content = content
                .ok_or_else(|| anyhow::anyhow!("content is required for text clipboard writes"))?;
            let mut clipboard = arboard::Clipboard::new()?;
            clipboard.set_text(content)?;
        }
        ClipboardFormat::Html => {
            let html =
                html.ok_or_else(|| anyhow::anyhow!("html is required for HTML clipboard writes"))?;
            let alt = content.unwrap_or_default();
            set_html(&html, &alt)?;
        }
        ClipboardFormat::Image => {
            let asset_id = asset_id.ok_or_else(|| {
                anyhow::anyhow!("asset_id is required for image clipboard writes")
            })?;
            let asset = registry
                .resolve(&asset_id)
                .ok_or_else(|| anyhow::anyhow!("unknown managed asset '{asset_id}'"))?;
            let decoded = image::open(&asset.path)?.to_rgba8();
            let image = arboard::ImageData {
                width: decoded.width() as usize,
                height: decoded.height() as usize,
                bytes: Cow::Owned(decoded.into_raw()),
            };
            arboard::Clipboard::new()?.set_image(image)?;
        }
        ClipboardFormat::Files => {
            let files = validate_file_paths(files.ok_or_else(|| {
                anyhow::anyhow!("files is required for file-list clipboard writes")
            })?)?;
            set_files(&files)?;
        }
    }
    Ok(())
}

fn save_image_asset(
    registry: &ManagedAssetRegistry,
    image: arboard::ImageData<'static>,
) -> anyhow::Result<ManagedAsset> {
    let root = default_generated_media_dir();
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.png", haven_common::types::new_id("file")));
    let rgba = image::RgbaImage::from_raw(
        image.width as u32,
        image.height as u32,
        image.bytes.into_owned(),
    )
    .ok_or_else(|| anyhow::anyhow!("clipboard image dimensions do not match pixel data"))?;
    let mut file = std::fs::File::create(&path)?;
    image::DynamicImage::ImageRgba8(rgba).write_to(&mut file, image::ImageFormat::Png)?;
    super::media::register_generated_asset(
        registry,
        None,
        &root,
        path,
        Some("clipboard.png".into()),
        "image/png",
        file.metadata()?.len(),
    )
}

fn copy_file_assets(
    registry: &ManagedAssetRegistry,
    paths: Vec<PathBuf>,
) -> anyhow::Result<Vec<ManagedAsset>> {
    let paths = validate_file_paths(
        paths
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
    )?;
    let root = default_generated_media_dir();
    std::fs::create_dir_all(&root)?;
    let mut assets = Vec::with_capacity(paths.len());
    for source in paths {
        let metadata = std::fs::metadata(&source)?;
        if metadata.len() > MAX_CLIPBOARD_FILE_BYTES {
            anyhow::bail!(
                "clipboard file exceeds {} byte limit",
                MAX_CLIPBOARD_FILE_BYTES
            );
        }
        let filename = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("clipboard.bin");
        let destination = root.join(format!(
            "{}-{}",
            haven_common::types::new_id("file"),
            filename
        ));
        std::fs::copy(&source, &destination)?;
        let media_type = mime_from_path(&source);
        assets.push(super::media::register_generated_asset(
            registry,
            None,
            &root,
            destination,
            Some(filename.to_string()),
            media_type,
            metadata.len(),
        )?);
    }
    Ok(assets)
}

fn validate_file_paths(paths: Vec<String>) -> anyhow::Result<Vec<PathBuf>> {
    if paths.is_empty() || paths.len() > MAX_CLIPBOARD_FILES {
        anyhow::bail!("files must contain 1..={MAX_CLIPBOARD_FILES} entries");
    }
    paths
        .into_iter()
        .map(|path| {
            let path = PathBuf::from(path);
            let metadata = std::fs::metadata(&path)?;
            if !metadata.is_file() {
                anyhow::bail!("clipboard path is not a file: {}", path.display());
            }
            Ok(path)
        })
        .collect()
}

fn mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "txt" | "md" | "csv" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(windows)]
fn read_html() -> anyhow::Result<ClipboardRead> {
    use clipboard_win::formats::Html;
    let html = clipboard_win::get_clipboard(
        Html::new().ok_or_else(|| anyhow::anyhow!("HTML clipboard format is unavailable"))?,
    )?;
    Ok(ClipboardRead::Html(html))
}

#[cfg(not(windows))]
fn read_html() -> anyhow::Result<ClipboardRead> {
    anyhow::bail!("HTML clipboard access is only available on Windows")
}

#[cfg(windows)]
fn read_files() -> anyhow::Result<ClipboardRead> {
    use clipboard_win::formats::FileList;
    Ok(ClipboardRead::Files(clipboard_win::get_clipboard(
        FileList,
    )?))
}

#[cfg(not(windows))]
fn read_files() -> anyhow::Result<ClipboardRead> {
    anyhow::bail!("file-list clipboard access is only available on Windows")
}

#[cfg(windows)]
fn set_html(html: &str, alt: &str) -> anyhow::Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_html(html, Some(alt))?;
    Ok(())
}

#[cfg(not(windows))]
fn set_html(_html: &str, _alt: &str) -> anyhow::Result<()> {
    anyhow::bail!("HTML clipboard access is only available on Windows")
}

#[cfg(windows)]
fn set_files(files: &[PathBuf]) -> anyhow::Result<()> {
    use clipboard_win::{Setter, formats::FileList};
    let _clipboard = clipboard_win::Clipboard::new_attempts(10)?;
    let values: Vec<String> = files
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    FileList.write_clipboard(values.as_slice())?;
    Ok(())
}

#[cfg(not(windows))]
fn set_files(_files: &[PathBuf]) -> anyhow::Result<()> {
    anyhow::bail!("file-list clipboard access is only available on Windows")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use serde_json::json;

    fn test_tool() -> ClipboardTool {
        ClipboardTool::new(Arc::new(ClipboardHistory::new(10)), 20_000, 10, 100, 2000)
    }

    #[test]
    fn test_clipboard_tool_name() {
        assert_eq!(test_tool().name(), "clipboard");
    }

    #[test]
    fn test_clipboard_tool_description() {
        assert!(test_tool().description().contains("clipboard"));
    }

    #[test]
    fn test_clipboard_tool_risk_level() {
        assert_eq!(
            test_tool().risk_level(&json!({"operation": "write"})),
            RiskLevel::Medium
        );
        assert_eq!(
            test_tool().risk_level(&json!({"operation": "read"})),
            RiskLevel::Low
        );
        assert_eq!(
            test_tool().risk_level(&json!({"operation": "history"})),
            RiskLevel::Low
        );
    }

    #[test]
    fn test_clipboard_tool_input_schema() {
        let schema = test_tool().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let enum_vals = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
        assert!(ops.contains(&"history"));
    }

    #[tokio::test]
    #[ignore = "requires an interactive desktop clipboard provider"]
    async fn test_clipboard_write_read_roundtrip() {
        let content = format!("haven-clipboard-test-{}", std::process::id());
        let write = test_tool()
            .execute(
                json!({"operation": "write", "content": content.clone()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(write.success);
        assert_eq!(write.output["written"], true);

        let read = test_tool()
            .execute(json!({"operation": "read"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(read.success);
        assert_eq!(read.output["content"], content);
    }

    #[tokio::test]
    async fn test_clipboard_write_requires_content() {
        let result = test_tool()
            .execute(json!({"operation": "write"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clipboard_unknown_operation() {
        let result = test_tool()
            .execute(json!({"operation": "bogus"}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_clipboard_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = test_tool()
            .execute(json!({"operation": "read"}), cancel)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_history_records_newest_first() {
        let history = ClipboardHistory::new(10);
        assert!(history.is_empty());
        history.record("first".into());
        history.record("second".into());
        let recent = history.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].content, "second");
        assert_eq!(recent[1].content, "first");
    }

    #[test]
    fn test_history_dedupes_most_recent() {
        let history = ClipboardHistory::new(10);
        history.record("a".into());
        history.record("b".into());
        history.record("a".into());
        let recent = history.recent(10);
        assert_eq!(recent.len(), 2, "re-copying 'a' must not duplicate it");
        assert_eq!(recent[0].content, "a");
        assert_eq!(recent[1].content, "b");
    }

    #[test]
    fn test_history_caps_entries() {
        let history = ClipboardHistory::new(3);
        for i in 0..10 {
            history.record(format!("item-{}", i));
        }
        let recent = history.recent(100);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].content, "item-9");
        assert_eq!(recent[2].content, "item-7");
    }

    #[test]
    fn test_history_ignores_empty() {
        let history = ClipboardHistory::new(10);
        history.record(String::new());
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn test_history_operation_returns_recorded_entries() {
        let tool = test_tool();
        tool.history.record("alpha".into());
        tool.history.record("beta".into());

        let result = tool
            .execute(json!({"operation": "history"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        let entries = result.output["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["content"], "beta");
        assert_eq!(entries[1]["content"], "alpha");
        assert_eq!(result.output["total"], 2);
        assert!(entries[0]["timestamp_ms"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn test_history_operation_respects_limit() {
        let tool = test_tool();
        for i in 0..5 {
            tool.history.record(format!("item-{}", i));
        }
        let result = tool
            .execute(
                json!({"operation": "history", "limit": 2}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let entries = result.output["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["content"], "item-4");
        assert_eq!(result.output["total"], 5);
    }

    #[tokio::test]
    async fn test_clipboard_native_entry_lands_in_run() {
        let tool = test_tool();
        tool.history.record("native".into());
        let result = tool
            .run(
                ClipboardParams {
                    operation: Some(ClipboardOperation::History),
                    content: None,
                    limit: Some(10),
                    format: None,
                    html: None,
                    asset_id: None,
                    files: None,
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let entries = result.output["entries"].as_array().unwrap();
        assert_eq!(entries[0]["content"], "native");
    }
}
