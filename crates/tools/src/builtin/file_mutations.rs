use diffy::patch_set::{FileOperation, ParseOptions, PatchKind, PatchSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use super::file_paths::{atomic_replace, looks_like_binary, sha256_bytes};
use super::{
    FilesPatchEdit, MAX_INSPECT_HASH_BYTES, MAX_PATCH_EDITS, MAX_PATCH_INPUT_BYTES,
    MAX_PATCH_MATCHES,
};
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
    pub(super) unified_hunks: Option<usize>,
}

#[derive(Clone, Copy)]
pub(super) struct MutationContext<'a> {
    pub(super) cancel: &'a CancellationToken,
    pub(super) write_policy: Option<&'a FileWritePolicy>,
}

pub(super) struct PatchRequest<'a> {
    pub(super) edits: Option<&'a [FilesPatchEdit]>,
    pub(super) unified_diff: Option<&'a str>,
    pub(super) expected_hash: Option<&'a str>,
    pub(super) dry_run: bool,
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

fn patch_write_hash<'a>(explicit: Option<&'a str>, source_hash: &'a str) -> &'a str {
    explicit.unwrap_or(source_hash)
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
    let expected_hash = expected_hash.ok_or_else(|| {
        anyhow::anyhow!(
            "write requires expected_hash from files.inspect, or 'missing' for a new path"
        )
    })?;
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let target = mutation_target(path, policy).await?;
    let write = replace_target(
        path,
        target.as_ref(),
        content.as_bytes(),
        Some(expected_hash),
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

pub(super) async fn patch(
    path: &str,
    request: PatchRequest<'_>,
    max_read_bytes: u64,
    max_write_bytes: u64,
    context: MutationContext<'_>,
) -> anyhow::Result<ToolResult> {
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
    let source_hash = sha256_bytes(&bytes);
    let decoded = haven_common::encoding::decode_with_encoding(&bytes);
    if looks_like_binary(&bytes) && !matches!(decoded.encoding, "utf-16le" | "utf-16be") {
        anyhow::bail!("file encoding is unsupported for patching; patch only supports text files");
    }
    let patch = match (request.edits, request.unified_diff) {
        (Some(edits), None) => apply_patch_edits(
            &decoded.text,
            edits,
            max_write_bytes,
            context.cancel.clone(),
        )?,
        (None, Some(unified_diff)) => apply_unified_diff(
            &decoded.text,
            unified_diff,
            max_write_bytes,
            context.cancel.clone(),
        )?,
        _ => anyhow::bail!("provide exactly one of 'edits' or 'unified_diff'"),
    };
    if context.cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let encoded = encode_patched_text(&patch.content, decoded.encoding)?;
    let write = replace_target(
        path,
        target.as_ref(),
        &encoded,
        Some(patch_write_hash(request.expected_hash, &source_hash)),
        max_write_bytes,
        request.dry_run,
    )
    .await?;
    Ok(ToolResult::ok(serde_json::json!({
        "patched": !write.dry_run,
        "dry_run": write.dry_run,
        "bytes": write.bytes,
        "sha256": write.sha256,
        "path": path,
        "format": if patch.unified_hunks.is_some() { "unified_diff" } else { "edits" },
        "edits": patch.edit_summaries.len(),
        "hunks": patch.unified_hunks,
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
        if edit.expected_matches == Some(0) {
            anyhow::bail!(
                "patch edit {} expected_matches must be at least 1",
                edit_index
            );
        }

        let positions: Vec<usize> = content
            .match_indices(&edit.old_string)
            .map(|(position, _)| position)
            .take(MAX_PATCH_MATCHES + 1)
            .collect();
        if positions.len() > MAX_PATCH_MATCHES {
            anyhow::bail!(
                "patch edit {} found more than {} matches; add more surrounding context to target a smaller set",
                edit_index,
                MAX_PATCH_MATCHES
            );
        }
        let expected_matches = edit.expected_matches.unwrap_or(if edit.replace_all {
            positions.len().max(1)
        } else {
            1
        });
        if positions.is_empty() || positions.len() != expected_matches {
            let candidates = candidate_snippets(content, &edit.old_string, &positions);
            anyhow::bail!(
                "patch edit {} expected {} matches, found {}; candidate context:\n{}\nRe-read these lines and include more surrounding text in old_string. Set replace_all=true only when every matching occurrence should change.",
                edit_index,
                expected_matches,
                positions.len(),
                candidates
            );
        }

        let lines = line_numbers_for_positions(content, &positions);
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
        unified_hunks: None,
    })
}

fn candidate_snippets(content: &str, old_string: &str, exact_positions: &[usize]) -> String {
    if content.is_empty() {
        return "The file is empty; the file may have changed since it was inspected.".into();
    }

    let mut candidates = line_numbers_for_positions(content, exact_positions);
    if candidates.is_empty() {
        let anchor = old_string
            .lines()
            .map(str::trim)
            .filter(|line| line.chars().count() >= 3)
            .max_by_key(|line| line.chars().count());
        let old_words = old_string
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .filter(|word| word.chars().count() >= 3)
            .collect::<std::collections::HashSet<_>>();
        let mut ranked = Vec::<(usize, usize)>::new();
        for (line_index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let anchor_score = anchor
                .filter(|anchor| {
                    trimmed.contains(*anchor)
                        || (trimmed.chars().count() >= 8 && anchor.contains(trimmed))
                })
                .map(|anchor| anchor.chars().count().min(100));
            let score = anchor_score.unwrap_or_else(|| {
                trimmed
                    .split(|character: char| !character.is_alphanumeric() && character != '_')
                    .filter(|word| old_words.contains(word))
                    .count()
            });
            if score > 0 && (ranked.len() < 3 || score > ranked[2].0) {
                ranked.push((score, line_index + 1));
                ranked.sort_unstable_by(|(left_score, left_line), (right_score, right_line)| {
                    right_score
                        .cmp(left_score)
                        .then_with(|| left_line.cmp(right_line))
                });
                ranked.truncate(3);
            }
        }
        candidates = ranked.into_iter().map(|(_, line)| line).collect();
    }
    candidates.sort_unstable();
    candidates.dedup();
    candidates.truncate(3);

    if candidates.is_empty() {
        return "No similar line was found; the file may have changed. Re-read the target before retrying.".into();
    }

    let mut snippets = vec![Vec::new(); candidates.len()];
    let mut first_candidate = 0;
    for (line_index, line) in content.lines().enumerate() {
        let line_number = line_index + 1;
        while first_candidate < candidates.len()
            && candidates[first_candidate].saturating_add(1) < line_number
        {
            first_candidate += 1;
        }
        for candidate_index in first_candidate..candidates.len() {
            if candidates[candidate_index] > line_number.saturating_add(1) {
                break;
            }
            let text = line.chars().take(240).collect::<String>();
            snippets[candidate_index].push(format!("{line_number}: {text}"));
        }
    }
    snippets
        .into_iter()
        .filter(|snippet| !snippet.is_empty())
        .map(|snippet| snippet.join("\n"))
        .collect::<Vec<_>>()
        .join("\n---\n")
}

fn line_numbers_for_positions(content: &str, positions: &[usize]) -> Vec<usize> {
    let mut line_number = 1usize;
    let mut cursor = 0usize;
    positions
        .iter()
        .map(|&position| {
            line_number += content[cursor..position]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count();
            cursor = position;
            line_number
        })
        .collect()
}

pub(super) fn apply_unified_diff(
    content: &str,
    unified_diff: &str,
    max_result_bytes: u64,
    cancel: CancellationToken,
) -> anyhow::Result<PatchApplication> {
    if unified_diff.trim().is_empty() {
        anyhow::bail!("'unified_diff' must not be empty");
    }
    if unified_diff.len() > MAX_PATCH_INPUT_BYTES {
        anyhow::bail!(
            "unified diff is {} bytes, above the {} byte patch input limit",
            unified_diff.len(),
            MAX_PATCH_INPUT_BYTES
        );
    }
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    let is_git_diff = unified_diff.lines().any(|line| {
        line.starts_with("diff --git ")
            || line.starts_with("diff --cc ")
            || line.starts_with("diff --combined ")
    });
    let parse_options = if is_git_diff {
        ParseOptions::gitdiff()
    } else {
        ParseOptions::unidiff()
    };
    let mut patches = PatchSet::parse(unified_diff, parse_options)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| anyhow::anyhow!("invalid unified diff: {error}"))?;
    if patches.len() != 1 {
        anyhow::bail!(
            "unified_diff must contain exactly one file patch; found {}",
            patches.len()
        );
    }
    let parsed = patches
        .pop()
        .ok_or_else(|| anyhow::anyhow!("unified_diff did not contain a file patch"))?;
    match parsed.operation() {
        // The explicit `path` argument is authoritative for the target. Unified
        // diff headers often use different old/new labels, so do not resolve or
        // write either header path.
        FileOperation::Modify { .. } => {}
        FileOperation::Create(_) => anyhow::bail!("unified diff file creation is not supported"),
        FileOperation::Delete(_) => anyhow::bail!("unified diff file deletion is not supported"),
        FileOperation::Rename { .. } => anyhow::bail!("unified diff renames are not supported"),
        FileOperation::Copy { .. } => anyhow::bail!("unified diff copies are not supported"),
    }
    if parsed.old_mode().is_some() || parsed.new_mode().is_some() {
        anyhow::bail!("unified diff file mode changes are not supported");
    }

    let text_patch = match parsed.patch() {
        PatchKind::Text(patch) => patch,
        PatchKind::Binary(_) => anyhow::bail!("binary unified diffs are not supported"),
    };
    let hunk_count = text_patch.hunks().len();
    if hunk_count == 0 {
        anyhow::bail!("unified diff must include at least one text hunk");
    }
    let result = diffy::apply(content, text_patch).map_err(|_| {
        anyhow::anyhow!(
            "unified diff did not match the current file; no changes were made. Re-read the relevant lines and retry with current context."
        )
    })?;
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    if u64::try_from(result.len()).unwrap_or(u64::MAX) > max_result_bytes {
        anyhow::bail!(
            "patched file would be {} bytes, above the {} byte write limit",
            result.len(),
            max_result_bytes
        );
    }
    Ok(PatchApplication {
        content: result,
        edit_summaries: Vec::new(),
        unified_hunks: Some(hunk_count),
    })
}

#[cfg(test)]
mod patch_concurrency_tests {
    use super::*;

    #[tokio::test]
    async fn omitted_expected_hash_guards_the_snapshot_read_by_patch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("concurrent.txt");
        tokio::fs::write(&path, b"snapshot").await.unwrap();

        let source = read_target_bytes(path.to_str().unwrap(), None, 1024)
            .await
            .unwrap();
        let source_hash = sha256_bytes(&source);
        let guard = patch_write_hash(None, &source_hash);
        tokio::fs::write(&path, b"raced change").await.unwrap();

        let result = replace_target(
            path.to_str().unwrap(),
            None,
            b"patched",
            Some(guard),
            1024,
            false,
        )
        .await;
        let error = match result {
            Ok(_) => panic!("the concurrent modification should be rejected"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("expected_hash"));
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"raced change");
    }
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
