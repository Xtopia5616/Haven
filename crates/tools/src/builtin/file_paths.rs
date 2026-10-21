use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use super::file_classification::{FileClassificationKind, classify_file_by_extension};
use super::{FilesOperation, MAX_INSPECT_HASH_BYTES};
use crate::execution_workspace::ExecutionWorkspaceSnapshot;
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
/// destination in one filesystem operation. The optional hash is an
/// optimistic guard against edits based on stale content and is rechecked
/// after staging, just before the destination is replaced.
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
    if let Some(expected) = expected_hash {
        verify_expected_hash(path, expected).await?;
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
    tokio::fs::create_dir_all(parent).await?;
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
        if let Some(expected) = expected_hash {
            verify_expected_hash(path, expected).await?;
        }
        let replace_existing =
            !expected_hash.is_some_and(|expected| expected.trim().eq_ignore_ascii_case("missing"));
        replace_file(&temporary, path, replace_existing).await
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

async fn verify_expected_hash(path: &Path, expected: &str) -> anyhow::Result<()> {
    let current_hash = match tokio::fs::File::open(path).await {
        Ok(file) => {
            let size = file.metadata().await?.len();
            if size > MAX_INSPECT_HASH_BYTES {
                anyhow::bail!(
                    "cannot verify expected_hash for a file larger than {} bytes",
                    MAX_INSPECT_HASH_BYTES
                );
            }
            let mut bytes = Vec::with_capacity(size as usize);
            file.take(MAX_INSPECT_HASH_BYTES.saturating_add(1))
                .read_to_end(&mut bytes)
                .await?;
            if bytes.len() as u64 > MAX_INSPECT_HASH_BYTES {
                anyhow::bail!(
                    "cannot verify expected_hash for a file larger than {} bytes",
                    MAX_INSPECT_HASH_BYTES
                );
            }
            Some(sha256_bytes(&bytes))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let matches = match current_hash.as_deref() {
        Some(actual) => expected_hash_matches(expected, actual),
        None => expected.trim().eq_ignore_ascii_case("missing"),
    };
    if !matches {
        anyhow::bail!("write compare failed: expected_hash does not match the current file");
    }
    Ok(())
}

async fn replace_file(
    temporary: &Path,
    destination: &Path,
    replace_existing: bool,
) -> anyhow::Result<()> {
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
        let flags = MOVEFILE_WRITE_THROUGH
            | if replace_existing {
                MOVEFILE_REPLACE_EXISTING
            } else {
                0
            };
        let ok = unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), flags) };
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
        if replace_existing {
            tokio::fs::rename(temporary, destination).await?;
        } else {
            tokio::fs::hard_link(temporary, destination).await?;
            let _ = tokio::fs::remove_file(temporary).await;
        }
        Ok(())
    }
}

pub(super) async fn inspect_open_file(
    path: &str,
    mut file: tokio::fs::File,
    max_hash_bytes: u64,
) -> anyhow::Result<ToolResult> {
    let metadata = file.metadata().await?;
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
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut bytes).await?;
        encoding = Some(haven_common::encoding::decode_with_encoding(&bytes).encoding);
        hash = Some(sha256_bytes(&bytes));
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

pub(super) fn missing_file(path: &str) -> ToolResult {
    ToolResult::ok(serde_json::json!({
        "exists": false,
        "file_type": "missing",
        "path": path,
        "hash": null,
        "encoding": null,
    }))
}

pub(super) fn sanitize_path(path: &str) -> anyhow::Result<String> {
    let normalized = Path::new(path).components().collect::<std::path::PathBuf>();
    if normalized
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        anyhow::bail!("path traversal detected");
    }
    Ok(normalized.to_string_lossy().to_string())
}

/// Resolve relative file paths against the same immutable root used by the
/// authorization gateway. Explicit absolute paths are preserved for the live
/// sandbox policy to accept or reject.
pub(super) fn resolve_workspace_path(
    workspace: &ExecutionWorkspaceSnapshot,
    path: &str,
) -> anyhow::Result<String> {
    let sanitized = sanitize_path(path)?;
    if sanitized.trim().is_empty() {
        anyhow::bail!("path is required");
    }
    let path = Path::new(&sanitized);
    if path.is_absolute() {
        return Ok(sanitized);
    }
    let resolved = workspace
        .root
        .as_ref()
        .map(|root| root.join(path))
        .ok_or_else(|| anyhow::anyhow!("execution workspace is unavailable for relative paths"))?;
    Ok(resolved.to_string_lossy().into_owned())
}

/// NUL byte in the first sample bytes is a strong binary indicator.
pub(super) fn looks_like_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    sample.contains(&0)
}

pub(super) fn binary_result(path: &str, size: u64) -> ToolResult {
    let classification = classify_file_by_extension(path);
    let hint = match classification.file_kind {
        FileClassificationKind::Pdf => "PDF file. Its content cannot be read directly as text.",
        FileClassificationKind::Archive => {
            "Archive file (zip/tar/...). Extract it with the shell tool to inspect contents."
        }
        FileClassificationKind::Office => {
            "Office document. Its binary format cannot be read as text."
        }
        FileClassificationKind::Audio => {
            "Audio file. Read it to request a bounded transcript, or use media.play to play it."
        }
        FileClassificationKind::Video => "Video file. It cannot be read as text.",
        FileClassificationKind::Executable => "Executable/binary file. It cannot be read as text.",
        FileClassificationKind::Image | FileClassificationKind::Unknown => {
            "Binary file. Use search(mode=content) to locate text, or read specific parts with offset/limit."
        }
    };
    ToolResult::ok(serde_json::json!({
        "binary": true,
        "path": path,
        "size": size,
        "file_type": classification.file_kind.as_str(),
        "mime": classification.mime_type,
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

#[cfg(test)]
mod atomic_replace_tests {
    use super::replace_file;

    #[tokio::test]
    async fn missing_file_write_does_not_replace_a_destination_created_before_commit() {
        let temp = tempfile::tempdir().unwrap();
        let staged = temp.path().join("staged.tmp");
        let destination = temp.path().join("output.txt");
        tokio::fs::write(&staged, b"agent content").await.unwrap();
        tokio::fs::write(&destination, b"external content")
            .await
            .unwrap();

        assert!(replace_file(&staged, &destination, false).await.is_err());
        assert_eq!(
            tokio::fs::read(destination).await.unwrap(),
            b"external content"
        );
    }
}
