use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};
use tokio::io::AsyncWriteExt;

use super::file_classification::classify_by_extension;
use super::{FilesOperation, MAX_INSPECT_HASH_BYTES};
use crate::{ManagedAsset, ToolResult};

pub(super) fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

fn expected_hash_matches(expected: &str, actual: &str) -> bool {
    let expected = expected
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(expected.trim());
    let actual = actual.strip_prefix("sha256:").unwrap_or(actual);
    expected.eq_ignore_ascii_case(actual)
}

pub(super) struct AtomicWriteResult {
    pub(super) bytes: u64,
    pub(super) sha256: String,
    pub(super) dry_run: bool,
}

/// Write a complete replacement beside the destination and then replace the
/// destination in one filesystem operation. The optional hash is a compare-
/// and-swap guard against edits based on stale content.
pub(super) async fn atomic_replace(
    path: &Path,
    bytes: &[u8],
    expected_hash: Option<&str>,
    max_write_bytes: u64,
    dry_run: bool,
) -> anyhow::Result<AtomicWriteResult> {
    let size = u64::try_from(bytes.len()).map_err(|_| anyhow::anyhow!("file is too large"))?;
    if size > max_write_bytes {
        anyhow::bail!(
            "write is {} bytes, above the {} byte write limit",
            size,
            max_write_bytes
        );
    }
    let current_hash = if expected_hash.is_some() {
        match tokio::fs::metadata(path).await {
            Ok(metadata) => {
                if metadata.len() > MAX_INSPECT_HASH_BYTES {
                    anyhow::bail!(
                        "cannot verify expected_hash for a file larger than {} bytes",
                        MAX_INSPECT_HASH_BYTES
                    );
                }
                Some(sha256_bytes(&tokio::fs::read(path).await?))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    if let Some(expected) = expected_hash {
        let matches = match current_hash.as_deref() {
            Some(actual) => expected_hash_matches(expected, actual),
            None => expected.trim().eq_ignore_ascii_case("missing"),
        };
        if !matches {
            anyhow::bail!(
                "write compare failed for '{}': expected_hash does not match the current file",
                path.display()
            );
        }
    }
    let sha256 = sha256_bytes(bytes);
    if dry_run {
        return Ok(AtomicWriteResult {
            bytes: size,
            sha256,
            dry_run: true,
        });
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".{}.tmp", haven_common::types::new_id("file")));
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        replace_file(&temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result?;
    Ok(AtomicWriteResult {
        bytes: size,
        sha256,
        dry_run: false,
    })
}

async fn replace_file(temporary: &Path, destination: &Path) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        };
        let from: Vec<u16> = temporary
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let to: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok == 0 {
            anyhow::bail!(
                "atomic file replacement failed: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        tokio::fs::rename(temporary, destination).await?;
        Ok(())
    }
}

pub(super) async fn inspect_file(
    path: &Path,
    include_hash: bool,
    max_hash_bytes: u64,
) -> anyhow::Result<ToolResult> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ToolResult::ok(serde_json::json!({
                "exists": false,
                "file_type": "missing",
                "path": path,
                "hash": null,
                "encoding": null,
            })));
        }
        Err(error) => return Err(error.into()),
    };
    let file_type = if metadata.is_file() {
        "file"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "other"
    };
    let modified_at = metadata
        .modified()
        .ok()
        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339());
    let mut hash = None;
    let mut encoding = None;
    if metadata.is_file()
        && metadata.len() <= max_hash_bytes
        && metadata.len() <= MAX_INSPECT_HASH_BYTES
    {
        let bytes = tokio::fs::read(path).await?;
        encoding = Some(haven_common::encoding::decode_with_encoding(&bytes).encoding);
        if include_hash {
            hash = Some(sha256_bytes(&bytes));
        }
    } else if include_hash && metadata.is_file() {
        anyhow::bail!(
            "file is too large to hash safely (limit {} bytes)",
            max_hash_bytes.min(MAX_INSPECT_HASH_BYTES)
        );
    }
    Ok(ToolResult::ok(serde_json::json!({
        "exists": true,
        "file_type": file_type,
        "path": path,
        "size": metadata.len(),
        "mtime": modified_at,
        "hash": hash,
        "encoding": encoding,
    })))
}

pub(super) fn sanitize_path(path: &str) -> anyhow::Result<String> {
    let normalized = Path::new(path).components().collect::<std::path::PathBuf>();
    if normalized
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        anyhow::bail!("path traversal detected: '{}'", path);
    }
    Ok(normalized.to_string_lossy().to_string())
}

/// Resolve a relative model path against the detected repository root. The
/// shell/files tools still use the shared Temp directory as their fallback;
/// explicit absolute paths and managed asset paths are never rewritten.
pub(super) fn resolve_workspace_path(path: &str) -> anyhow::Result<String> {
    let sanitized = sanitize_path(path)?;
    if sanitized.trim().is_empty() {
        anyhow::bail!("path is required");
    }
    let path = Path::new(&sanitized);
    if path.is_absolute() {
        return Ok(sanitized);
    }
    let current = std::env::current_dir().unwrap_or_default();
    Ok(haven_common::discover_workspace_root(&current)
        .map(|root| root.join(path))
        .unwrap_or_else(|| path.to_path_buf())
        .to_string_lossy()
        .into_owned())
}

/// NUL byte in the first sample bytes is a strong binary indicator.
pub(super) fn looks_like_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    sample.contains(&0)
}

pub(super) fn binary_result(path: &str, size: u64) -> ToolResult {
    let (kind, mime) = classify_by_extension(path);
    let hint = match kind {
        "pdf" => "PDF file. Its content cannot be read directly as text.",
        "archive" => {
            "Archive file (zip/tar/...). Extract it with the shell tool to inspect contents."
        }
        "office" => "Office document. Its binary format cannot be read as text.",
        "audio" => {
            "Audio file. Read it to request a bounded transcript, or use media.play to play it."
        }
        "video" => "Video file. It cannot be read as text.",
        "executable" => "Executable/binary file. It cannot be read as text.",
        _ => {
            "Binary file. Use search(mode=content) to locate text, or read specific parts with offset/limit."
        }
    };
    ToolResult::ok(serde_json::json!({
        "binary": true,
        "path": path,
        "size": size,
        "file_type": kind,
        "mime": mime,
        "hint": hint
    }))
}

/// Add the stable context fields shared by every structured `files` result.
///
/// The operation-specific helpers intentionally only know about their own
/// payload. Keeping this normalization at the tool boundary prevents read,
/// list, search, and mutation results from slowly acquiring incompatible
/// shapes while preserving the existing operation-specific fields.
pub(super) fn annotate_file_result(
    mut result: ToolResult,
    operation: FilesOperation,
    path: Option<&str>,
    root: Option<&str>,
) -> ToolResult {
    let truncated = result.truncated;
    if let Some(output) = result.output.as_object_mut() {
        output.insert("operation".into(), serde_json::json!(operation));
        if let Some(path) = path {
            output
                .entry("path")
                .or_insert_with(|| serde_json::json!(path));
        }
        if let Some(root) = root {
            output
                .entry("root")
                .or_insert_with(|| serde_json::json!(root));
        }
        output
            .entry("truncated")
            .or_insert_with(|| serde_json::json!(truncated));
    }
    result
}

/// Remove host paths from a result produced for a managed attachment. The
/// filesystem operation already ran on the host; its provider-facing
/// observation only needs the opaque id and safe display metadata.
pub(super) fn redact_managed_file_result(result: &mut ToolResult, asset: &ManagedAsset) {
    let Some(output) = result.output.as_object_mut() else {
        return;
    };
    for key in ["path", "root", "from", "to"] {
        output.remove(key);
    }
    output.insert("asset_id".into(), serde_json::json!(asset.asset_id));
    output
        .entry("notes")
        .or_insert_with(|| Value::String(haven_common::media::MEDIA_ASSET_NAVIGATION_NOTE.into()));
    if let Some(filename) = asset.filename.as_deref() {
        output.insert("filename".into(), serde_json::json!(filename));
    }
    prioritize_asset_fields(output);
}

/// Put the opaque asset handle and its follow-up note at the head of a
/// provider-facing result. The JSON order is not semantic, but it is useful
/// when a bounded observation keeps the prefix and drops the tail.
fn prioritize_asset_fields(output: &mut Map<String, Value>) {
    let original = std::mem::take(output);
    let mut ordered = Map::new();
    for key in ["asset_id", "notes"] {
        if let Some(value) = original.get(key) {
            ordered.insert(key.to_owned(), value.clone());
        }
    }
    for (key, value) in original {
        if !matches!(key.as_str(), "asset_id" | "notes") {
            ordered.insert(key, value);
        }
    }
    *output = ordered;
}
