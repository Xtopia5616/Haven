use async_trait::async_trait;
use haven_common::config::default_generated_media_root;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

use crate::asset_registry::GeneratedMediaWriteGuard;
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
    clipboard_text_reader: Arc<dyn Fn() -> anyhow::Result<String> + Send + Sync>,
    clipboard_text_writer: Arc<dyn Fn(String) -> anyhow::Result<()> + Send + Sync>,
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
#[serde(deny_unknown_fields)]
pub struct ClipboardParams {
    /// Operation to perform; defaults to `read`.
    #[serde(default)]
    pub operation: Option<ClipboardOperation>,
    /// Entry limit for the history operation (clamped to the tool cap).
    #[serde(default)]
    pub limit: Option<u64>,
    /// Representation to read. Writes require an explicit non-`auto` format.
    #[serde(default)]
    pub format: Option<ClipboardFormat>,
    /// Plain text payload for text writes, or optional plain-text fallback for
    /// HTML writes.
    #[serde(default)]
    pub text: Option<String>,
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
            clipboard_text_reader: Arc::new(read_text_clipboard),
            clipboard_text_writer: Arc::new(write_text_clipboard),
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

    #[cfg(test)]
    fn with_clipboard_text_reader(
        mut self,
        reader: impl Fn() -> anyhow::Result<String> + Send + Sync + 'static,
    ) -> Self {
        self.clipboard_text_reader = Arc::new(reader);
        self
    }

    #[cfg(test)]
    fn with_clipboard_text_writer(
        mut self,
        writer: impl Fn(String) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.clipboard_text_writer = Arc::new(writer);
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
                "text": { "type": "string" },
                "html": { "type": "string" },
                "asset_id": { "type": "string", "pattern": "^asset-[0-9a-f]{32}$" },
                "files": { "type": "array", "minItems": 1, "maxItems": MAX_CLIPBOARD_FILES, "items": { "type": "string", "minLength": 1 } },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
            },
            "oneOf": [
                {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": { "operation": { "const": "read" }, "format": { "type": "string", "enum": ["auto", "text", "html", "image", "files"] } },
                    "required": ["operation"]
                },
                {
                    "type": "object",
                    "properties": { "operation": { "const": "write" } },
                    "required": ["operation"],
                    "oneOf": [
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "format": { "const": "text" },
                                "text": { "type": "string" }
                            },
                            "required": ["operation", "format", "text"]
                        },
                        {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "operation": { "const": "write" },
                                "format": { "const": "html" },
                                "html": { "type": "string" },
                                "text": { "type": "string", "description": "Optional plain-text fallback for the HTML clipboard representation." }
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
                let read = if format == ClipboardFormat::Text {
                    let read_text = Arc::clone(&self.clipboard_text_reader);
                    tokio::task::spawn_blocking(move || read_text().map(ClipboardRead::Text))
                        .await??
                } else {
                    tokio::task::spawn_blocking(move || read_clipboard(format)).await??
                };

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                match read {
                    ClipboardRead::Text(text) => {
                        self.history.record(text.clone());
                        let output =
                            haven_common::encoding::truncate_output(&text, self.max_output_chars);
                        let result = serde_json::json!({"operation": "read", "format": "text", "content": output.text});
                        Ok(ToolResult::from_output(result, output.truncated))
                    }
                    ClipboardRead::Html(html) => {
                        let output =
                            haven_common::encoding::truncate_output(&html, self.max_output_chars);
                        Ok(ToolResult::from_output(
                            serde_json::json!({ "operation": "read", "format": "html", "html": output.text }),
                            output.truncated,
                        ))
                    }
                    ClipboardRead::Image(image) => {
                        let write_guard = tokio::select! {
                            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                            guard = self.managed_assets.lock_generated_media_write() => guard,
                        };
                        let registry = self.managed_assets.clone();
                        let cancel_for_save = cancel.clone();
                        let asset = tokio::task::spawn_blocking(move || {
                            save_image_asset(&registry, &write_guard, image, &cancel_for_save)
                        })
                        .await??;
                        Ok(ToolResult::ok(serde_json::json!({
                            "operation": "read", "format": "image", "asset_id": asset.asset_id,
                            "media_type": asset.media_type, "size_bytes": asset.size_bytes,
                        })))
                    }
                    ClipboardRead::Files(paths) => {
                        let assets = copy_file_assets(&self.managed_assets, paths, &cancel).await?;
                        Ok(ToolResult::ok(serde_json::json!({
                            "operation": "read", "format": "files",
                            "asset_ids": assets.iter().map(|asset| asset.asset_id.clone()).collect::<Vec<_>>(),
                            "files": assets.iter().map(|asset| serde_json::json!({"asset_id": asset.asset_id, "filename": asset.filename, "media_type": asset.media_type, "size_bytes": asset.size_bytes})).collect::<Vec<_>>(),
                        })))
                    }
                }
            }
            ClipboardOperation::Write => {
                let format = validate_write_params(&params)?;
                let text = params.text.clone();
                let html = params.html.clone();
                let asset_id = params.asset_id.clone();
                let files = params.files.clone();
                let registry = self.managed_assets.clone();
                let text_writer = Arc::clone(&self.clipboard_text_writer);
                tokio::task::spawn_blocking(move || {
                    if format == ClipboardFormat::Text {
                        let text = text.ok_or_else(|| {
                            anyhow::anyhow!("text is required for text clipboard writes")
                        })?;
                        text_writer(text)
                    } else {
                        write_clipboard(format, text, html, asset_id, files, registry)
                    }
                })
                .await??;

                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                if let Some(text) = params.text.filter(|text| !text.is_empty()) {
                    self.history.record(text);
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"operation": "write", "format": format, "written": true}),
                ))
            }
            ClipboardOperation::History => {
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                let history = Arc::clone(&self.history);
                let read_text = Arc::clone(&self.clipboard_text_reader);
                let current_text = tokio::task::spawn_blocking(move || read_text()).await;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                match current_text {
                    Ok(Ok(text)) => history.record(text),
                    Ok(Err(error)) => {
                        tracing::debug!(
                            error = %error,
                            "Could not sample current clipboard text for history"
                        );
                    }
                    Err(error) => {
                        tracing::debug!(
                            error = %error,
                            "Clipboard history snapshot task failed"
                        );
                    }
                }
                let limit = params
                    .limit
                    .map(|l| (l.min(self.max_history_limit as u64)) as usize)
                    .unwrap_or(self.default_limit);
                let entries = self.history.recent(limit);
                let json_entries: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        let output = haven_common::encoding::truncate_output(
                            &e.content,
                            self.entry_max_chars,
                        );
                        serde_json::json!({
                            "content": output.text,
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

fn validate_write_params(params: &ClipboardParams) -> anyhow::Result<ClipboardFormat> {
    let format = params
        .format
        .ok_or_else(|| anyhow::anyhow!("format is required for clipboard writes"))?;
    let has_text = params.text.is_some();
    let has_html = params.html.is_some();
    let has_asset = params.asset_id.is_some();
    let has_files = params.files.is_some();
    let has_limit = params.limit.is_some();

    match format {
        ClipboardFormat::Text
            if has_text && !has_html && !has_asset && !has_files && !has_limit =>
        {
            Ok(format)
        }
        ClipboardFormat::Html if has_html && !has_asset && !has_files && !has_limit => Ok(format),
        ClipboardFormat::Image
            if has_asset && !has_text && !has_html && !has_files && !has_limit =>
        {
            Ok(format)
        }
        ClipboardFormat::Files
            if has_files && !has_text && !has_html && !has_asset && !has_limit =>
        {
            Ok(format)
        }
        ClipboardFormat::Auto => anyhow::bail!("auto format is not supported for clipboard writes"),
        ClipboardFormat::Text => anyhow::bail!("text writes require only the text payload"),
        ClipboardFormat::Html => {
            anyhow::bail!("HTML writes require html and allow optional text fallback")
        }
        ClipboardFormat::Image => anyhow::bail!("image writes require only asset_id"),
        ClipboardFormat::Files => anyhow::bail!("file-list writes require only files"),
    }
}

enum ClipboardRead {
    Text(String),
    Html(String),
    Image(arboard::ImageData<'static>),
    Files(Vec<PathBuf>),
}

fn read_text_clipboard() -> anyhow::Result<String> {
    let mut clipboard = arboard::Clipboard::new()?;
    normalize_text_clipboard_result(clipboard.get_text())
}

fn normalize_text_clipboard_result(
    result: Result<String, arboard::Error>,
) -> anyhow::Result<String> {
    match result {
        Ok(text) => Ok(text),
        // Windows reports ERROR_NOT_FOUND when CF_UNICODETEXT is absent;
        // arboard normalizes that to ContentNotAvailable. An empty text read
        // is the useful clipboard contract when no text is available.
        Err(arboard::Error::ContentNotAvailable) => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn read_clipboard(format: ClipboardFormat) -> anyhow::Result<ClipboardRead> {
    match format {
        ClipboardFormat::Text => {
            let mut clipboard = arboard::Clipboard::new()?;
            Ok(ClipboardRead::Text(normalize_text_clipboard_result(
                clipboard.get_text(),
            )?))
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
            match read_files() {
                Ok(files) => Ok(files),
                Err(error) if is_missing_file_list_format(&error) => {
                    Ok(ClipboardRead::Text(String::new()))
                }
                Err(error) => Err(error),
            }
        }
    }
}

#[cfg(windows)]
fn is_missing_file_list_format(error: &anyhow::Error) -> bool {
    const ERROR_NOT_FOUND: i32 = 1168;
    error.chain().any(|cause| {
        cause
            .downcast_ref::<clipboard_win::ErrorCode>()
            .is_some_and(|error| error.raw_code() == ERROR_NOT_FOUND)
    })
}

#[cfg(not(windows))]
fn is_missing_file_list_format(_error: &anyhow::Error) -> bool {
    false
}

fn write_clipboard(
    format: ClipboardFormat,
    text: Option<String>,
    html: Option<String>,
    asset_id: Option<String>,
    files: Option<Vec<String>>,
    registry: ManagedAssetRegistry,
) -> anyhow::Result<()> {
    match format {
        ClipboardFormat::Text | ClipboardFormat::Auto => {
            let text =
                text.ok_or_else(|| anyhow::anyhow!("text is required for text clipboard writes"))?;
            write_text_clipboard(text)?;
        }
        ClipboardFormat::Html => {
            let html =
                html.ok_or_else(|| anyhow::anyhow!("html is required for HTML clipboard writes"))?;
            set_html(&html, text.as_deref().unwrap_or_default())?;
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

fn write_text_clipboard(text: String) -> anyhow::Result<()> {
    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text)?;
    Ok(())
}

fn save_image_asset(
    registry: &ManagedAssetRegistry,
    _write_guard: &GeneratedMediaWriteGuard,
    image: arboard::ImageData<'static>,
    cancel: &CancellationToken,
) -> anyhow::Result<ManagedAsset> {
    let root = default_generated_media_root();
    std::fs::create_dir_all(&root)?;
    let path = root.join(format!("{}.png", haven_common::types::new_id("file")));
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let mut created_file = false;
    let result = (|| -> anyhow::Result<ManagedAsset> {
        let rgba = image::RgbaImage::from_raw(
            image.width as u32,
            image.height as u32,
            image.bytes.into_owned(),
        )
        .ok_or_else(|| anyhow::anyhow!("clipboard image dimensions do not match pixel data"))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        created_file = true;
        image::DynamicImage::ImageRgba8(rgba).write_to(&mut file, image::ImageFormat::Png)?;
        use std::io::Write;
        file.flush()?;
        drop(file);
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let size = std::fs::metadata(&path)?.len();
        super::media::register_generated_asset(
            registry,
            _write_guard,
            None,
            &root,
            path.clone(),
            Some("clipboard.png".into()),
            "image/png",
            size,
        )
    })();
    if created_file && result.is_err() {
        let _ = std::fs::remove_file(&path);
    }
    result
}

async fn copy_file_assets(
    registry: &ManagedAssetRegistry,
    paths: Vec<PathBuf>,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<ManagedAsset>> {
    let paths: Vec<String> = paths
        .into_iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let cancel_for_validation = cancel.clone();
    let validated = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<_>> {
        if cancel_for_validation.is_cancelled() {
            return Ok(None);
        }
        let paths =
            validate_file_paths_cancellable(paths, || cancel_for_validation.is_cancelled())?;
        if cancel_for_validation.is_cancelled() {
            return Ok(None);
        }
        let root = default_generated_media_root();
        std::fs::create_dir_all(&root)?;
        Ok(Some((paths, root)))
    })
    .await??;
    let Some((paths, root)) = validated else {
        anyhow::bail!("cancelled");
    };
    let mut assets = Vec::with_capacity(paths.len());
    for source in paths {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let source_for_metadata = source.clone();
        let metadata =
            tokio::task::spawn_blocking(move || std::fs::metadata(source_for_metadata)).await??;
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
        let media_type = mime_from_path(&source);
        let write_guard = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
            guard = registry.lock_generated_media_write() => guard,
        };
        let registry = registry.clone();
        let root = root.clone();
        let filename = filename.to_string();
        let source_for_copy = source.clone();
        let destination_for_copy = destination.clone();
        let cancel_for_copy = cancel.clone();
        let asset = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<ManagedAsset>> {
            use std::io::{Read, Write};

            if cancel_for_copy.is_cancelled() {
                return Ok(None);
            }
            let mut created_file = false;
            let result = (|| -> anyhow::Result<Option<ManagedAsset>> {
                let mut source_file = std::fs::File::open(&source_for_copy)?;
                let source_metadata = source_file.metadata()?;
                if source_metadata.len() > MAX_CLIPBOARD_FILE_BYTES {
                    anyhow::bail!(
                        "clipboard file exceeds {} byte limit",
                        MAX_CLIPBOARD_FILE_BYTES
                    );
                }
                let mut destination_file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&destination_for_copy)?;
                created_file = true;
                let mut copied_bytes = 0_u64;
                let mut buffer = [0_u8; 64 * 1024];
                loop {
                    if cancel_for_copy.is_cancelled() {
                        return Ok(None);
                    }
                    let read = source_file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    copied_bytes += read as u64;
                    if copied_bytes > MAX_CLIPBOARD_FILE_BYTES {
                        anyhow::bail!(
                            "clipboard file exceeds {} byte limit",
                            MAX_CLIPBOARD_FILE_BYTES
                        );
                    }
                    if cancel_for_copy.is_cancelled() {
                        return Ok(None);
                    }
                    destination_file.write_all(&buffer[..read])?;
                }
                destination_file.flush()?;
                drop(destination_file);
                if cancel_for_copy.is_cancelled() {
                    return Ok(None);
                }
                std::fs::set_permissions(&destination_for_copy, source_metadata.permissions())?;
                let asset = super::media::register_generated_asset(
                    &registry,
                    &write_guard,
                    None,
                    &root,
                    destination_for_copy.clone(),
                    Some(filename),
                    media_type,
                    copied_bytes,
                )?;
                Ok(Some(asset))
            })();
            if created_file && !matches!(&result, Ok(Some(_))) {
                let _ = std::fs::remove_file(&destination_for_copy);
            }
            result
        })
        .await??;
        let asset = asset.ok_or_else(|| anyhow::anyhow!("cancelled"))?;
        assets.push(asset);
    }
    Ok(assets)
}

fn validate_file_paths(paths: Vec<String>) -> anyhow::Result<Vec<PathBuf>> {
    validate_file_paths_cancellable(paths, || false)
}

fn validate_file_paths_cancellable(
    paths: Vec<String>,
    mut is_cancelled: impl FnMut() -> bool,
) -> anyhow::Result<Vec<PathBuf>> {
    if paths.is_empty() || paths.len() > MAX_CLIPBOARD_FILES {
        anyhow::bail!("files must contain 1..={MAX_CLIPBOARD_FILES} entries");
    }
    let mut validated = Vec::with_capacity(paths.len());
    for path in paths {
        if is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let path = PathBuf::from(path);
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() {
            anyhow::bail!("clipboard path is not a file: {}", path.display());
        }
        validated.push(path);
    }
    Ok(validated)
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
            .with_clipboard_text_reader(|| anyhow::bail!("clipboard access is disabled in tests"))
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

        assert!(schema["properties"]["text"].is_object());
        assert!(schema["properties"]["content"].is_null());
        let write_formats = schema["oneOf"][1]["oneOf"].as_array().unwrap();
        let formats: Vec<&str> = write_formats
            .iter()
            .map(|branch| branch["properties"]["format"]["const"].as_str().unwrap())
            .collect();
        assert_eq!(formats, ["text", "html", "image", "files"]);
        assert_eq!(
            write_formats[1]["required"],
            json!(["operation", "format", "html"])
        );
        assert!(
            write_formats[1]["properties"]["text"]["description"]
                .as_str()
                .unwrap()
                .contains("fallback")
        );
    }

    #[tokio::test]
    #[ignore = "requires an interactive desktop clipboard provider"]
    async fn test_clipboard_write_read_roundtrip() {
        let text = format!("haven-clipboard-test-{}", std::process::id());
        let write = test_tool()
            .execute(
                json!({"operation": "write", "format": "text", "text": text.clone()}),
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
        assert_eq!(read.output["content"], text);
    }

    #[tokio::test]
    async fn test_programmatic_text_write_read_is_visible_in_history() {
        let clipboard = Arc::new(Mutex::new(String::new()));
        let reader_clipboard = Arc::clone(&clipboard);
        let writer_clipboard = Arc::clone(&clipboard);
        let tool = test_tool()
            .with_clipboard_text_reader(move || {
                Ok(reader_clipboard
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone())
            })
            .with_clipboard_text_writer(move |text| {
                *writer_clipboard.lock().unwrap_or_else(|p| p.into_inner()) = text;
                Ok(())
            });

        let write = tool
            .execute(
                json!({"operation": "write", "format": "text", "text": "program text"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(write.success);

        let read = tool
            .execute(
                json!({"operation": "read", "format": "text"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(read.success);
        assert_eq!(read.output["content"], "program text");

        let history = tool
            .execute(json!({"operation": "history"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(history.output["total"], 1);
        assert_eq!(history.output["entries"][0]["content"], "program text");
    }

    #[tokio::test]
    async fn test_clipboard_write_rejects_ambiguous_payloads() {
        let tool = test_tool();
        for input in [
            json!({"operation": "write", "text": "hello"}),
            json!({"operation": "write", "content": "legacy"}),
            json!({"operation": "write", "format": "text", "text": "hello", "html": "<b>hello</b>"}),
            json!({"operation": "write", "format": "html", "text": "fallback"}),
        ] {
            assert!(tool.execute(input, CancellationToken::new()).await.is_err());
        }
    }

    #[test]
    fn test_clipboard_write_format_matches_one_payload() {
        let params = |format, text, html, asset_id, files| ClipboardParams {
            operation: Some(ClipboardOperation::Write),
            limit: None,
            format,
            text,
            html,
            asset_id,
            files,
        };
        assert_eq!(
            validate_write_params(&params(
                Some(ClipboardFormat::Text),
                Some("hello".into()),
                None,
                None,
                None
            ))
            .unwrap(),
            ClipboardFormat::Text
        );
        assert_eq!(
            validate_write_params(&params(
                Some(ClipboardFormat::Html),
                Some("plain".into()),
                Some("<b>rich</b>".into()),
                None,
                None
            ))
            .unwrap(),
            ClipboardFormat::Html
        );
        assert!(
            validate_write_params(&params(None, Some("hello".into()), None, None, None)).is_err()
        );
        assert!(
            validate_write_params(&params(
                Some(ClipboardFormat::Auto),
                Some("hello".into()),
                None,
                None,
                None
            ))
            .is_err()
        );
        assert!(
            validate_write_params(&params(
                Some(ClipboardFormat::Text),
                Some("hello".into()),
                Some("<b>hello</b>".into()),
                None,
                None
            ))
            .is_err()
        );
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

    #[test]
    fn test_text_read_maps_unavailable_content_to_empty() {
        assert_eq!(
            normalize_text_clipboard_result(Err(arboard::Error::ContentNotAvailable)).unwrap(),
            ""
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_auto_read_maps_missing_file_list_to_empty() {
        let error = anyhow::Error::new(clipboard_win::ErrorCode::new_system(1168));
        assert!(is_missing_file_list_format(&error));

        let unrelated = anyhow::Error::new(clipboard_win::ErrorCode::new_system(5));
        assert!(!is_missing_file_list_format(&unrelated));
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
    async fn test_history_operation_records_current_clipboard_text() {
        let tool = test_tool().with_clipboard_text_reader(|| Ok("program text".into()));

        let result = tool
            .execute(json!({"operation": "history"}), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.output["total"], 1);
        assert_eq!(result.output["entries"][0]["content"], "program text");
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
                    limit: Some(10),
                    format: None,
                    text: None,
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
