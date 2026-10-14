use std::io::{self, Read};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::file_paths::{atomic_replace, looks_like_binary, sha256_bytes};
use super::{FilesPatchEdit, MAX_INSPECT_HASH_BYTES, MAX_PATCH_EDITS, MAX_PATCH_INPUT_BYTES};
use crate::ToolResult;
use crate::security::FileWritePolicy;
use haven_platform::filesystem::DirectoryCapability;

async fn run_file_mutation<T>(
    cancel: &CancellationToken,
    mutation: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    // File mutations are cooperative: cancellation prevents a mutation from
    // starting, but cannot roll back an operation once the filesystem accepts it.
    // Return the operation's real result instead of reporting cancellation after
    // a successful side effect.
    mutation.await
}

#[derive(Debug, Clone)]
struct PatchReplacement {
    start: usize,
    end: usize,
    edit_index: usize,
}

#[derive(Debug, Clone)]
pub(super) struct PatchEditSummary {
    lines: Vec<usize>,
    matches: usize,
}

pub(super) struct PatchApplication {
    pub(super) content: String,
    pub(super) edit_summaries: Vec<PatchEditSummary>,
}

pub(super) struct MutationLimits {
    pub(super) max_read_bytes: u64,
    pub(super) max_write_bytes: u64,
}

#[derive(Clone, Copy)]
pub(super) struct MutationContext<'a> {
    pub(super) cancel: &'a CancellationToken,
    pub(super) write_policy: Option<&'a FileWritePolicy>,
}

#[derive(Clone)]
struct MutationTarget {
    directory: DirectoryCapability,
    relative_path: PathBuf,
}

async fn blocking_io<T>(
    operation: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> anyhow::Result<T>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| anyhow::anyhow!("filesystem operation failed: {error}"))?
        .map_err(Into::into)
}

async fn mutation_target(
    path: &str,
    policy: Option<&FileWritePolicy>,
) -> anyhow::Result<Option<MutationTarget>> {
    let Some(policy) = policy else {
        return Ok(None);
    };
    let policy = policy.clone();
    let path = PathBuf::from(path);
    let (directory, relative_path) = blocking_io(move || policy.resolve(&path)).await?;
    Ok(Some(MutationTarget {
        directory,
        relative_path,
    }))
}

async fn read_target_bytes(
    path: &str,
    target: Option<&MutationTarget>,
    max_bytes: u64,
) -> anyhow::Result<Vec<u8>> {
    if let Some(target) = target {
        let target = target.clone();
        return blocking_io(move || {
            let mut file = target.directory.open_read(&target.relative_path)?;
            if file.metadata()?.len() > max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("file exceeds the {max_bytes} byte read limit"),
                ));
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(max_bytes.saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max_bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("file exceeds the {max_bytes} byte read limit"),
                ));
            }
            Ok(bytes)
        })
        .await;
    }

    let file = tokio::fs::File::open(path).await?;
    if file.metadata().await?.len() > max_bytes {
        anyhow::bail!("file exceeds the {max_bytes} byte read limit");
    }
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > max_bytes {
        anyhow::bail!("file exceeds the {max_bytes} byte read limit");
    }
    Ok(bytes)
}

fn expected_hash_matches(expected: &str, actual: &str) -> bool {
    let expected = expected
        .trim()
        .strip_prefix("sha256:")
        .unwrap_or(expected.trim());
    let actual = actual.strip_prefix("sha256:").unwrap_or(actual);
    expected.eq_ignore_ascii_case(actual)
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::NotFound)
}

async fn replace_target(
    path: &str,
    target: Option<&MutationTarget>,
    bytes: &[u8],
    expected_hash: Option<&str>,
    max_write_bytes: u64,
    dry_run: bool,
) -> anyhow::Result<super::file_paths::AtomicWriteResult> {
    let size = u64::try_from(bytes.len()).map_err(|_| anyhow::anyhow!("file is too large"))?;
    if size > max_write_bytes {
        anyhow::bail!(
            "write is {} bytes, above the {} byte write limit",
            size,
            max_write_bytes
        );
    }
    let current_hash = if expected_hash.is_some() {
        match read_target_bytes(path, target, MAX_INSPECT_HASH_BYTES).await {
            Ok(current) => Some(sha256_bytes(&current)),
            Err(error) if is_not_found(&error) => None,
            Err(error) => return Err(error),
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
            anyhow::bail!("write compare failed: expected_hash does not match the current file");
        }
    }
    let hash = sha256_bytes(bytes);
    if dry_run {
        return Ok(super::file_paths::AtomicWriteResult {
            bytes: size,
            sha256: hash,
            dry_run: true,
        });
    }
    if let Some(target) = target {
        let target = target.clone();
        let bytes = bytes.to_vec();
        blocking_io(move || {
            target
                .directory
                .atomic_replace(&target.relative_path, &bytes)
        })
        .await?;
        Ok(super::file_paths::AtomicWriteResult {
            bytes: size,
            sha256: hash,
            dry_run: false,
        })
    } else {
        atomic_replace(
            Path::new(path),
            bytes,
            expected_hash,
            max_write_bytes,
            false,
        )
        .await
    }
}

pub(super) async fn write(
    path: &str,
    content: &str,
    expected_hash: Option<&str>,
    max_write_bytes: u64,
    dry_run: bool,
    cancel: CancellationToken,
    policy: Option<&FileWritePolicy>,
) -> anyhow::Result<ToolResult> {
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let target = mutation_target(path, policy).await?;
    let write = replace_target(
        path,
        target.as_ref(),
        content.as_bytes(),
        expected_hash,
        max_write_bytes,
        dry_run,
    )
    .await?;
    Ok(ToolResult::ok(serde_json::json!({
        "written": !write.dry_run,
        "dry_run": write.dry_run,
        "bytes": write.bytes,
        "sha256": write.sha256,
        "path": path
    })))
}

pub(super) async fn create_dir(
    path: &str,
    context: MutationContext<'_>,
) -> anyhow::Result<ToolResult> {
    run_file_mutation(context.cancel, async {
        if let Some(target) = mutation_target(path, context.write_policy).await? {
            blocking_io(move || target.directory.create_dir_all(&target.relative_path)).await?;
        } else {
            tokio::fs::create_dir_all(path).await?;
        }
        Ok(ToolResult::ok(
            serde_json::json!({"created": true, "path": path}),
        ))
    })
    .await
}

pub(super) async fn edit(
    path: &str,
    old: Option<&str>,
    new: &str,
    expected_hash: Option<&str>,
    limits: MutationLimits,
    dry_run: bool,
    context: MutationContext<'_>,
) -> anyhow::Result<ToolResult> {
    let old = old.ok_or_else(|| anyhow::anyhow!("'old_string' is required for edit operation"))?;
    let target = mutation_target(path, context.write_policy).await?;
    let bytes = read_target_bytes(path, target.as_ref(), limits.max_read_bytes)
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::InvalidData)
                || error.to_string().contains("byte read limit")
            {
                anyhow::anyhow!(
                    "file is above the {} byte edit limit. Locate the text with search(mode=content) and rewrite the file in smaller pieces.",
                    limits.max_read_bytes
                )
            } else {
                error
            }
        })?;
    let content = haven_common::encoding::decode_lossy(&bytes);
    let positions: Vec<usize> = content.match_indices(old).map(|(i, _)| i).collect();
    if positions.is_empty() {
        anyhow::bail!("old_string not found in the target file");
    }
    if positions.len() > 1 {
        let lines: Vec<usize> = positions
            .iter()
            .map(|&p| content[..p].matches('\n').count() + 1)
            .collect();
        let snippet = |pos: usize| -> String {
            let start = pos.saturating_sub(40);
            let end = (pos + old.len() + 40).min(content.len());
            let mut s = String::new();
            if start > 0 {
                s.push('…');
            }
            s.push_str(&content[start..end]);
            if end < content.len() {
                s.push('…');
            }
            s
        };
        let matches: Vec<serde_json::Value> = lines
            .iter()
            .zip(positions.iter())
            .map(|(&l, &p)| serde_json::json!({"line": l, "snippet": snippet(p)}))
            .collect();
        return Ok(ToolResult {
            success: true,
            output: serde_json::json!({
                "warning": format!("old_string appears {} times; provide more context in old_string to disambiguate", positions.len()),
                "matches": matches,
            }),
            error: None,
            error_class: None,
            retryability: crate::ToolRetryability::Unknown,
            truncated: false,
            outcome: crate::ToolExecutionOutcome::Succeeded,
            attempts: 1,
            signals: crate::tool_contract::ToolSignals::default(),
            llm_usage: Vec::new(),
        });
    }
    let result = content.replace(old, new);
    if context.cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let write = replace_target(
        path,
        target.as_ref(),
        result.as_bytes(),
        expected_hash,
        limits.max_write_bytes,
        dry_run,
    )
    .await?;
    let line = content[..positions[0]].matches('\n').count() + 1;
    Ok(ToolResult::ok(serde_json::json!({
        "edited": !write.dry_run,
        "dry_run": write.dry_run,
        "bytes": write.bytes,
        "sha256": write.sha256,
        "path": path,
        "line": line
    })))
}

pub(super) async fn patch(
    path: &str,
    edits: Option<&[FilesPatchEdit]>,
    expected_hash: Option<&str>,
    max_read_bytes: u64,
    max_write_bytes: u64,
    dry_run: bool,
    context: MutationContext<'_>,
) -> anyhow::Result<ToolResult> {
    let edits = edits.ok_or_else(|| anyhow::anyhow!("'edits' is required for patch operation"))?;
    let target = mutation_target(path, context.write_policy).await?;
    let bytes = read_target_bytes(path, target.as_ref(), max_read_bytes)
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::InvalidData)
                || error.to_string().contains("byte read limit")
            {
                anyhow::anyhow!(
                    "file is above the {} byte patch limit. Locate the text with search(mode=content) and rewrite the file in smaller pieces.",
                    max_read_bytes
                )
            } else {
                error
            }
        })?;
    if context.cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let decoded = haven_common::encoding::decode_with_encoding(&bytes);
    if looks_like_binary(&bytes) && !matches!(decoded.encoding, "utf-16le" | "utf-16be") {
        anyhow::bail!("file encoding is unsupported for patching; patch only supports text files");
    }
    let patch = apply_patch_edits(
        &decoded.text,
        edits,
        max_write_bytes,
        context.cancel.clone(),
    )?;
    if context.cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let encoded = encode_patched_text(&patch.content, decoded.encoding)?;
    let write = replace_target(
        path,
        target.as_ref(),
        &encoded,
        expected_hash,
        max_write_bytes,
        dry_run,
    )
    .await?;
    Ok(ToolResult::ok(serde_json::json!({
        "patched": !write.dry_run,
        "dry_run": write.dry_run,
        "bytes": write.bytes,
        "sha256": write.sha256,
        "path": path,
        "edits": patch.edit_summaries.len(),
        "replacements": patch.edit_summaries
            .into_iter()
            .enumerate()
            .map(|(index, summary)| serde_json::json!({
                "edit": index,
                "matches": summary.matches,
                "lines": summary.lines,
            }))
            .collect::<Vec<_>>(),
    })))
}

pub(super) async fn copy(
    path: &str,
    destination: &str,
    context: MutationContext<'_>,
) -> anyhow::Result<ToolResult> {
    run_file_mutation(context.cancel, async {
        let source_target = mutation_target(path, context.write_policy).await?;
        let destination_target = mutation_target(destination, context.write_policy).await?;
        match (source_target, destination_target) {
            (Some(source), Some(destination)) => {
                blocking_io(move || {
                    source.directory.copy_file_to(
                        &source.relative_path,
                        &destination.directory,
                        &destination.relative_path,
                    )
                })
                .await?;
            }
            (None, None) => {
                tokio::fs::copy(path, destination).await?;
            }
            _ => anyhow::bail!("file copy requires authorized source and destination roots"),
        }
        Ok(ToolResult::ok(
            serde_json::json!({"copied": true, "from": path, "to": destination}),
        ))
    })
    .await
}

pub(super) async fn move_file(
    path: &str,
    destination: &str,
    context: MutationContext<'_>,
) -> anyhow::Result<ToolResult> {
    run_file_mutation(context.cancel, async {
        let source_target = mutation_target(path, context.write_policy).await?;
        let destination_target = mutation_target(destination, context.write_policy).await?;
        match (source_target, destination_target) {
            (Some(source), Some(destination)) => {
                let source_for_rename = source.clone();
                let destination_for_rename = destination.clone();
                match blocking_io(move || {
                    source_for_rename.directory.rename_to(
                        &source_for_rename.relative_path,
                        &destination_for_rename.directory,
                        &destination_for_rename.relative_path,
                    )
                })
                .await
                {
                    Ok(()) => {}
                    Err(error) if is_cross_device(&error) => {
                        let source_for_copy = source.clone();
                        let destination_for_copy = destination.clone();
                        blocking_io(move || {
                            source_for_copy.directory.copy_file_to(
                                &source_for_copy.relative_path,
                                &destination_for_copy.directory,
                                &destination_for_copy.relative_path,
                            )
                        })
                        .await?;
                        blocking_io(move || source.directory.remove_file(&source.relative_path))
                            .await?;
                    }
                    Err(error) => return Err(error),
                }
            }
            (None, None) => match tokio::fs::rename(path, destination).await {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                    tokio::fs::copy(path, destination).await?;
                    tokio::fs::remove_file(path).await?;
                }
                Err(error) => return Err(error.into()),
            },
            _ => anyhow::bail!("file move requires authorized source and destination roots"),
        }
        Ok(ToolResult::ok(
            serde_json::json!({"moved": true, "from": path, "to": destination}),
        ))
    })
    .await
}

pub(super) async fn delete(path: &str, context: MutationContext<'_>) -> anyhow::Result<ToolResult> {
    if context.cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let kind = if let Some(target) = mutation_target(path, context.write_policy).await? {
        blocking_io(move || {
            target
                .directory
                .remove_file_or_empty_dir(&target.relative_path)
        })
        .await?
    } else {
        let path_ref = Path::new(path);
        let metadata = tokio::fs::symlink_metadata(path_ref).await?;
        if metadata.file_type().is_dir() {
            // Deliberately remove only empty directories. Recursive deletion is a
            // separate, more destructive operation and is not implied by delete.
            tokio::fs::remove_dir(path_ref).await?;
            "directory"
        } else {
            tokio::fs::remove_file(path_ref).await?;
            "file"
        }
    };
    Ok(ToolResult::ok(serde_json::json!({
        "deleted": true,
        "path": path,
        "file_type": kind
    })))
}

fn is_cross_device(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::CrossesDevices)
}

pub(super) async fn list(
    path: &str,
    max_list_entries: usize,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    let mut entries = tokio::fs::read_dir(path).await?;
    let mut names = Vec::new();
    let mut truncated = false;
    while let Some(entry) = entries.next_entry().await? {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        if names.len() >= max_list_entries {
            truncated = true;
            break;
        }
        names.push(entry.file_name().to_string_lossy().to_string());
    }
    names.sort();
    let mut result = serde_json::json!({"entries": names, "count": names.len()});
    if truncated {
        result["hint"] = serde_json::json!(format!(
            "Directory has more than {} entries; only the first {} are listed.",
            max_list_entries, max_list_entries
        ));
    }
    Ok(ToolResult::from_output(result, truncated))
}

/// Validate and apply every patch against the original in-memory content.
/// No filesystem mutation is performed here; callers can therefore treat an
/// error or cancellation as a transaction rollback.
pub(super) fn apply_patch_edits(
    content: &str,
    edits: &[FilesPatchEdit],
    max_result_bytes: u64,
    cancel: CancellationToken,
) -> anyhow::Result<PatchApplication> {
    if edits.is_empty() {
        anyhow::bail!("'edits' must contain at least one edit");
    }
    if edits.len() > MAX_PATCH_EDITS {
        anyhow::bail!(
            "too many patch edits: {}; the maximum is {}",
            edits.len(),
            MAX_PATCH_EDITS
        );
    }

    let input_limit = max_result_bytes.min(MAX_PATCH_INPUT_BYTES as u64) as usize;
    let input_bytes = edits.iter().try_fold(0usize, |total, edit| {
        total
            .checked_add(edit.old_string.len())
            .and_then(|total| total.checked_add(edit.new_string.len()))
            .ok_or_else(|| anyhow::anyhow!("patch edit input is too large"))
    })?;
    if input_bytes > input_limit {
        anyhow::bail!(
            "patch edit text is {} bytes, above the {} byte patch input limit",
            input_bytes,
            input_limit
        );
    }

    let mut replacements = Vec::new();
    let mut summaries = Vec::with_capacity(edits.len());
    for (edit_index, edit) in edits.iter().enumerate() {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        if edit.old_string.is_empty() {
            anyhow::bail!("patch edit {} has an empty old_string", edit_index);
        }
        let expected_matches = edit.expected_matches.unwrap_or(1);
        if expected_matches == 0 {
            anyhow::bail!(
                "patch edit {} expected_matches must be at least 1",
                edit_index
            );
        }

        let positions: Vec<usize> = content
            .match_indices(&edit.old_string)
            .map(|(position, _)| position)
            .collect();
        if positions.len() != expected_matches {
            anyhow::bail!(
                "patch edit {} expected {} matches, found {}",
                edit_index,
                expected_matches,
                positions.len()
            );
        }

        let lines = positions
            .iter()
            .map(|&position| content[..position].matches('\n').count() + 1)
            .collect::<Vec<_>>();
        for &position in &positions {
            replacements.push(PatchReplacement {
                start: position,
                end: position + edit.old_string.len(),
                edit_index,
            });
        }
        summaries.push(PatchEditSummary {
            lines,
            matches: positions.len(),
        });
    }

    replacements.sort_unstable_by_key(|replacement| (replacement.start, replacement.end));
    for pair in replacements.windows(2) {
        if pair[1].start < pair[0].end {
            anyhow::bail!(
                "patch edits {} and {} overlap or target the same text",
                pair[0].edit_index,
                pair[1].edit_index
            );
        }
    }

    let mut result_len = content.len();
    for replacement in &replacements {
        let edit = &edits[replacement.edit_index];
        result_len = result_len
            .checked_sub(replacement.end - replacement.start)
            .and_then(|length| length.checked_add(edit.new_string.len()))
            .ok_or_else(|| anyhow::anyhow!("patched file size overflowed"))?;
    }
    if u64::try_from(result_len).unwrap_or(u64::MAX) > max_result_bytes {
        anyhow::bail!(
            "patched file would be {} bytes, above the {} byte patch limit",
            result_len,
            max_result_bytes
        );
    }

    let mut result = String::with_capacity(result_len);
    let mut cursor = 0;
    for replacement in replacements {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        result.push_str(&content[cursor..replacement.start]);
        result.push_str(&edits[replacement.edit_index].new_string);
        cursor = replacement.end;
    }
    result.push_str(&content[cursor..]);
    Ok(PatchApplication {
        content: result,
        edit_summaries: summaries,
    })
}

pub(super) fn encode_patched_text(text: &str, encoding: &str) -> anyhow::Result<Vec<u8>> {
    match encoding {
        "empty" | "utf-8" => Ok(text.as_bytes().to_vec()),
        "utf-8-bom" => {
            let mut bytes = Vec::with_capacity(3 + text.len());
            bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            bytes.extend_from_slice(text.as_bytes());
            Ok(bytes)
        }
        "utf-16le" => {
            let mut bytes = Vec::with_capacity(2 + text.len() * 2);
            bytes.extend_from_slice(&[0xFF, 0xFE]);
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_le_bytes());
            }
            Ok(bytes)
        }
        "utf-16be" => {
            let mut bytes = Vec::with_capacity(2 + text.len() * 2);
            bytes.extend_from_slice(&[0xFE, 0xFF]);
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_be_bytes());
            }
            Ok(bytes)
        }
        "gbk" => Ok(encoding_rs::GBK.encode(text).0.into_owned()),
        other => anyhow::bail!("unsupported text encoding for patch: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{MutationContext, copy, create_dir, move_file, run_file_mutation};
    use tokio::sync::oneshot;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn cancelled_file_mutations_do_not_change_filesystem_state() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.txt");
        let copied = temp.path().join("copied.txt");
        let moved = temp.path().join("moved.txt");
        let new_dir = temp.path().join("new").join("nested");
        tokio::fs::write(&source, "keep source").await.unwrap();

        let cancel = CancellationToken::new();
        cancel.cancel();

        assert!(
            create_dir(
                new_dir.to_str().unwrap(),
                MutationContext {
                    cancel: &cancel,
                    write_policy: None,
                },
            )
            .await
            .is_err()
        );
        assert!(
            copy(
                source.to_str().unwrap(),
                copied.to_str().unwrap(),
                MutationContext {
                    cancel: &cancel,
                    write_policy: None,
                },
            )
            .await
            .is_err()
        );
        assert!(
            move_file(
                source.to_str().unwrap(),
                moved.to_str().unwrap(),
                MutationContext {
                    cancel: &cancel,
                    write_policy: None,
                }
            )
            .await
            .is_err()
        );

        assert!(!new_dir.exists());
        assert!(!copied.exists());
        assert!(!moved.exists());
        assert_eq!(
            tokio::fs::read_to_string(&source).await.unwrap(),
            "keep source"
        );
    }

    #[tokio::test]
    async fn cancellation_after_mutation_starts_returns_its_committed_result() {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let (continue_tx, continue_rx) = oneshot::channel();

        let task = tokio::spawn(async move {
            run_file_mutation(&task_cancel, async move {
                started_tx.send(()).unwrap();
                continue_rx
                    .await
                    .map_err(|_| anyhow::anyhow!("test gate closed"))?;
                Ok::<_, anyhow::Error>("committed")
            })
            .await
        });

        started_rx.await.unwrap();
        cancel.cancel();
        continue_tx.send(()).unwrap();

        assert_eq!(task.await.unwrap().unwrap(), "committed");
    }
}
