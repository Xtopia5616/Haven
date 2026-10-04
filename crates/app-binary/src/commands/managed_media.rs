//! App-owned upload and managed-media file lifecycle.

use haven_memory::SessionStore;

/// Root folder for user-uploaded files. Lives under the agent's default Temp
/// working directory so the file tool can read uploads with the same access
/// the agent already has for its own scripts.
fn uploads_root() -> std::path::PathBuf {
    haven_common::default_work_dir().join("uploads")
}

/// Replace characters that are illegal in Windows file names (and path
/// traversal hazards) so an uploaded name cannot escape its batch directory.
/// Falls back to a random name for empty / "." / ".." / reserved device names
/// (CON, PRN, AUX, NUL, COM1–9, LPT1–9, incl. `NUL.txt` forms — writing to
/// those opens the device and silently discards the bytes) and caps the
/// length on a char boundary so full paths stay short without panicking.
fn sanitize_filename(name: &str) -> String {
    let mut clean: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0' | '\n' | '\r' => '_',
            c => c,
        })
        .collect();
    // Windows strips trailing dots/spaces at the filesystem layer; drop them
    // here so `foo.` and `foo` can't silently collide (and overwrite) on disk.
    while clean.ends_with(['.', ' ']) {
        clean.pop();
    }
    let stem = clean
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        s if (s.starts_with("COM") || s.starts_with("LPT")) && s.len() == 4 => {
            s.as_bytes()[3].is_ascii_digit()
        }
        _ => false,
    };
    if clean.trim().is_empty() || clean == "." || clean == ".." || reserved {
        clean = haven_common::types::new_id("file");
    }
    // `String::truncate` panics when the index is not a char boundary; pop
    // whole chars instead (CJK/emoji names are common).
    while clean.len() > 120 {
        clean.pop();
    }
    clean
}

/// Write ordinary file attachments to disk under `uploads/<batch>/` and return
/// them with `path` set. `data` is cleared afterwards — the bytes live on
/// disk, keeping the persisted message and DB storage slim. Inline image and
/// audio media pass through because the active chat model consumes their
/// base64 payload directly.
pub(crate) async fn persist_file_attachments(
    attachments: Vec<haven_common::types::MessageAttachment>,
    max_total_bytes: u64,
    registry: haven_tools::ManagedAssetRegistry,
    session_id: Option<String>,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    persist_file_attachments_to_with_limit_and_registry(
        uploads_root(),
        attachments,
        max_total_bytes,
        Some((registry, session_id)),
    )
    .await
}

/// Test helper for deleting unreferenced generated upload batches. Production
/// cleanup uses the same reference-driven path after reading SessionStore.
#[cfg(test)]
pub(crate) async fn cleanup_stale_upload_batches(
    root: std::path::PathBuf,
    max_age: std::time::Duration,
) -> Result<usize, String> {
    cleanup_stale_upload_batches_with_registry(
        root,
        max_age,
        haven_tools::ManagedAssetRegistry::default(),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn cleanup_stale_upload_batches_with_registry(
    root: std::path::PathBuf,
    _max_age: std::time::Duration,
    registry: haven_tools::ManagedAssetRegistry,
) -> Result<usize, String> {
    cleanup_unreferenced_upload_batches(root, registry, Vec::new()).await
}

/// Delete committed upload batches once no durable message or active ingress /
/// session lease references any file in the batch. A known reference set is
/// mandatory: failure to read session metadata must fail closed.
#[cfg(test)]
pub(crate) async fn cleanup_unreferenced_upload_batches(
    root: std::path::PathBuf,
    registry: haven_tools::ManagedAssetRegistry,
    referenced_paths: Vec<std::path::PathBuf>,
) -> Result<usize, String> {
    let _write_guard = upload_write_lock().lock().await;
    tokio::task::spawn_blocking(move || {
        cleanup_unreferenced_upload_batches_sync(&root, &registry, &referenced_paths)
    })
    .await
    .map_err(|error| format!("上传目录清理任务失败: {error}"))?
}

/// Reconcile both managed media roots against durable session attachment
/// references. `referenced_paths` must come from SessionStore; callers must not
/// turn a failed reference read into an empty set.
pub(crate) async fn cleanup_unreferenced_managed_media(
    uploads_root: std::path::PathBuf,
    generated_root: std::path::PathBuf,
    registry: haven_tools::ManagedAssetRegistry,
    session_store: &SessionStore,
) -> Result<(usize, usize), String> {
    let referenced_paths = session_store
        .list_managed_attachment_paths()
        .await
        .map_err(|error| error.to_string());
    cleanup_unreferenced_managed_media_with_references(
        uploads_root,
        generated_root,
        registry,
        referenced_paths,
    )
    .await
}

async fn cleanup_unreferenced_managed_media_with_references(
    uploads_root: std::path::PathBuf,
    generated_root: std::path::PathBuf,
    registry: haven_tools::ManagedAssetRegistry,
    referenced_paths: Result<Vec<std::path::PathBuf>, String>,
) -> Result<(usize, usize), String> {
    let referenced_paths =
        referenced_paths.map_err(|error| format!("读取会话附件引用失败: {error}"))?;
    let upload_guard = upload_write_lock().lock().await;
    let generated_media_guard = registry.lock_generated_media_cleanup().await;
    tokio::task::spawn_blocking(move || {
        let _upload_guard = upload_guard;
        cleanup_media_roots_with_generated_guard(
            generated_media_guard,
            || {
                cleanup_unreferenced_generated_media_sync(
                    &generated_root,
                    &registry,
                    &referenced_paths,
                )
            },
            || {
                cleanup_unreferenced_upload_batches_sync(
                    &uploads_root,
                    &registry,
                    &referenced_paths,
                )
            },
        )
    })
    .await
    .map_err(|error| format!("受管媒体清理任务失败: {error}"))?
}

#[cfg(test)]
fn cleanup_media_roots(
    cleanup_generated: impl FnOnce() -> Result<usize, String>,
    cleanup_uploads: impl FnOnce() -> Result<usize, String>,
) -> Result<(usize, usize), String> {
    let generated = cleanup_generated();
    let uploads = cleanup_uploads();
    combine_media_cleanup_results(generated, uploads)
}

fn cleanup_media_roots_with_generated_guard(
    generated_media_guard: haven_tools::GeneratedMediaCleanupGuard,
    cleanup_generated: impl FnOnce() -> Result<usize, String>,
    cleanup_uploads: impl FnOnce() -> Result<usize, String>,
) -> Result<(usize, usize), String> {
    let generated = cleanup_generated();
    drop(generated_media_guard);
    let uploads = cleanup_uploads();
    combine_media_cleanup_results(generated, uploads)
}

fn combine_media_cleanup_results(
    generated: Result<usize, String>,
    uploads: Result<usize, String>,
) -> Result<(usize, usize), String> {
    match (generated, uploads) {
        (Ok(generated), Ok(uploads)) => Ok((uploads, generated)),
        (Err(error), Ok(_)) => Err(format!("清理生成媒体目录失败: {error}")),
        (Ok(_), Err(error)) => Err(format!("清理上传目录失败: {error}")),
        (Err(generated_error), Err(uploads_error)) => Err(format!(
            "清理生成媒体目录失败: {generated_error}; 清理上传目录失败: {uploads_error}"
        )),
    }
}

/// Staging directories are crash leftovers, not durable session history. They
/// therefore have their own short retention window and are cleaned even when
/// `history_retention_days` is disabled.
pub(crate) const UPLOAD_STAGING_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

pub(crate) async fn cleanup_stale_upload_staging(
    root: std::path::PathBuf,
) -> Result<usize, String> {
    cleanup_stale_upload_staging_with_age(root, UPLOAD_STAGING_MAX_AGE).await
}

async fn cleanup_stale_upload_staging_with_age(
    root: std::path::PathBuf,
    max_age: std::time::Duration,
) -> Result<usize, String> {
    let _write_guard = upload_write_lock().lock().await;
    tokio::task::spawn_blocking(move || cleanup_stale_upload_staging_sync(&root, max_age))
        .await
        .map_err(|error| format!("上传暂存目录清理任务失败: {error}"))?
}

static UPLOAD_WRITE_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

fn upload_write_lock() -> &'static tokio::sync::Mutex<()> {
    UPLOAD_WRITE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn cleanup_unreferenced_upload_batches_sync(
    root: &std::path::Path,
    registry: &haven_tools::ManagedAssetRegistry,
    referenced_paths: &[std::path::PathBuf],
) -> Result<usize, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            registry.prune_unreferenced(referenced_paths);
            registry.prune_missing();
            return Ok(0);
        }
        Err(error) => return Err(format!("读取上传目录失败: {error}")),
    };
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Ok(0);
    }
    let leased_paths = registry.leased_or_pending_paths();
    registry.prune_unreferenced(referenced_paths);
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => return Err(format!("读取上传目录失败: {error}")),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(error = %error, "跳过不可读取的上传目录项");
                continue;
            }
        };
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(error = %error, "跳过无法判断类型的上传目录项");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !metadata.is_dir() || is_link_or_reparse(&metadata) || !is_generated_upload_batch(&name)
        {
            continue;
        }
        let batch_path = entry.path();
        let has_message_reference = referenced_paths
            .iter()
            .any(|path| path_is_equal_or_child(&batch_path, path));
        let has_live_lease = leased_paths
            .iter()
            .any(|path| path_is_equal_or_child(&batch_path, path));
        if has_message_reference || has_live_lease {
            tracing::debug!(batch = %name, "保留仍被消息引用或持有活动租约的上传批次");
            continue;
        }
        match std::fs::remove_dir_all(&batch_path) {
            Ok(()) => {
                removed += 1;
                registry.prune_paths_under(&batch_path);
            }
            Err(error) => tracing::debug!(batch = %name, error = %error, "上传批次清理失败"),
        }
    }
    registry.prune_missing();
    Ok(removed)
}

fn cleanup_stale_upload_staging_sync(
    root: &std::path::Path,
    max_age: std::time::Duration,
) -> Result<usize, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取上传暂存根目录失败: {error}")),
    };
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Ok(0);
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取上传暂存目录失败: {error}")),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(error = %error, "跳过不可读取的上传暂存目录项");
                continue;
            }
        };
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(error = %error, "跳过无法判断类型的上传暂存目录项");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !metadata.is_dir()
            || is_link_or_reparse(&metadata)
            || !is_generated_upload_staging(&name)
        {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                tracing::debug!(staging = %name, error = %error, "跳过没有修改时间的上传暂存目录");
                continue;
            }
        };
        if modified.elapsed().map_or(true, |age| age <= max_age) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) => {
                tracing::debug!(staging = %name, error = %error, "上传暂存目录清理失败")
            }
        }
    }
    Ok(removed)
}

fn cleanup_unreferenced_generated_media_sync(
    root: &std::path::Path,
    registry: &haven_tools::ManagedAssetRegistry,
    referenced_paths: &[std::path::PathBuf],
) -> Result<usize, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            registry.prune_unreferenced(referenced_paths);
            registry.prune_missing();
            return Ok(0);
        }
        Err(error) => return Err(format!("读取生成媒体根目录失败: {error}")),
    };
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Ok(0);
    }
    let leased_paths = registry.leased_paths();
    let transient_paths = registry.unexpired_transient_paths();
    registry.prune_unreferenced(referenced_paths);
    let entries =
        std::fs::read_dir(root).map_err(|error| format!("读取生成媒体目录失败: {error}"))?;
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(error = %error, "跳过不可读取的生成媒体目录项");
                continue;
            }
        };
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(error = %error, "跳过无法判断类型的生成媒体目录项");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !metadata.is_file() || is_link_or_reparse(&metadata) || !is_generated_media_file(&name) {
            continue;
        }
        let path_is_referenced = referenced_paths
            .iter()
            .any(|candidate| path_is_equal(candidate, &path));
        let has_live_lease = leased_paths
            .iter()
            .any(|candidate| path_is_equal(candidate, &path));
        let has_transient_ttl = transient_paths
            .iter()
            .any(|candidate| path_is_equal(candidate, &path));
        if path_is_referenced || has_live_lease || has_transient_ttl {
            tracing::debug!(file = %name, "保留仍被会话、租约或临时资产 TTL 引用的生成媒体");
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) => tracing::debug!(file = %name, error = %error, "生成媒体清理失败"),
        }
    }
    registry.prune_missing();
    Ok(removed)
}

fn is_generated_media_file(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("file-") else {
        return false;
    };
    let Some(separator) = suffix.as_bytes().get(32).copied() else {
        return false;
    };
    let (Some(id), Some(tail)) = (suffix.get(..32), suffix.get(33..)) else {
        return false;
    };
    if !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    match separator {
        b'.' => {
            !tail.is_empty()
                && tail.len() <= 16
                && tail.bytes().all(|byte| byte.is_ascii_alphanumeric())
        }
        // Clipboard copies retain the original basename after a generated ID.
        // The entry is still a direct child file under the host-owned root.
        b'-' => !tail.is_empty() && !tail.contains(['/', '\\']),
        _ => false,
    }
}

fn is_generated_upload_batch(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("file-") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_generated_upload_staging(name: &str) -> bool {
    name.strip_prefix(".file-")
        .and_then(|value| value.strip_suffix(".tmp"))
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(test)]
async fn persist_file_attachments_to(
    root: std::path::PathBuf,
    attachments: Vec<haven_common::types::MessageAttachment>,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    persist_file_attachments_to_with_limit(root, attachments, DEFAULT_MAX_UPLOAD_TOTAL_BYTES).await
}

#[cfg(test)]
const DEFAULT_MAX_UPLOAD_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

struct UploadBatchGuard {
    path: std::path::PathBuf,
    committed: bool,
}

impl Drop for UploadBatchGuard {
    fn drop(&mut self) {
        if !self.committed {
            // This also runs when an in-flight upload task is cancelled. The
            // staging directory is private to this operation, so a best-
            // effort synchronous cleanup is preferable to leaving bytes
            // behind until the next retention pass.
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
async fn persist_file_attachments_to_with_limit(
    root: std::path::PathBuf,
    attachments: Vec<haven_common::types::MessageAttachment>,
    max_total_bytes: u64,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    persist_file_attachments_to_with_limit_and_registry(root, attachments, max_total_bytes, None)
        .await
}

async fn persist_file_attachments_to_with_limit_and_registry(
    root: std::path::PathBuf,
    attachments: Vec<haven_common::types::MessageAttachment>,
    max_total_bytes: u64,
    registry: Option<(haven_tools::ManagedAssetRegistry, Option<String>)>,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    use base64::Engine as _;

    let mut files = Vec::new();
    for mut att in attachments {
        // Clear renderer-only metadata here as a second defense, not only in
        // the Tauri validation command.
        att.path = None;
        // Asset identity is host-owned; never allow the renderer to alias a
        // previously registered managed asset.
        if att.asset_id.is_none() {
            att.asset_id = Some(haven_common::types::new_id("asset"));
        }
        // All binary inputs now become managed assets.  Inline base64 is an
        // ingress-only transport representation; keeping it in messages and
        // snapshots made the same bytes live in multiple authorities.
        files.push(att);
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }

    // Serialize quota accounting and staging-directory commits so concurrent
    // transcript submissions cannot each observe the same free capacity.
    let _write_guard = upload_write_lock().lock().await;

    let existing_bytes = tokio::task::spawn_blocking({
        let root = root.clone();
        move || upload_tree_size(&root)
    })
    .await
    .map_err(|error| format!("计算上传目录容量失败: {error}"))??;
    let batch_id = haven_common::types::new_id("file");
    let staging_dir = root.join(format!(".{batch_id}.tmp"));
    let batch_dir = root.join(&batch_id);
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("创建上传目录失败: {e}"))?;
    let root_metadata = tokio::fs::symlink_metadata(&root)
        .await
        .map_err(|e| format!("读取上传目录元数据失败: {e}"))?;
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Err("上传目录不能是符号链接或重解析点".to_string());
    }
    tokio::fs::create_dir(&staging_dir)
        .await
        .map_err(|e| format!("创建上传临时目录失败: {e}"))?;
    let mut guard = UploadBatchGuard {
        path: staging_dir.clone(),
        committed: false,
    };

    let mut used_names = std::collections::HashSet::new();
    let mut persisted = Vec::with_capacity(files.len());
    let mut total_bytes = existing_bytes;
    for mut att in files {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&att.data)
            .map_err(|_| "附件数据不是有效的 base64".to_string())?;
        let decoded_len = bytes.len() as u64;
        if total_bytes.saturating_add(decoded_len) > max_total_bytes {
            return Err(format!(
                "上传目录超过 {}MB 总容量上限",
                max_total_bytes / 1024 / 1024
            ));
        }
        total_bytes = total_bytes.saturating_add(decoded_len);
        let base_name = att
            .filename
            .as_deref()
            .map(sanitize_filename)
            .unwrap_or_else(|| haven_common::types::new_id("file"));
        // Keep the extension for readability but dedupe collisions so two
        // same-named uploads in one batch never overwrite each other.
        let mut name = base_name.clone();
        let mut n = 2;
        while !used_names.insert(filename_collision_key(&name)) {
            let stem = std::path::Path::new(&base_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&base_name)
                .to_string();
            let ext = std::path::Path::new(&base_name)
                .extension()
                .and_then(|s| s.to_str())
                .map(|e| format!(".{e}"))
                .unwrap_or_default();
            name = format!("{stem}_{n}{ext}");
            n += 1;
        }
        let file_path = staging_dir.join(&name);
        tokio::fs::write(&file_path, bytes)
            .await
            .map_err(|e| format!("保存附件失败: {e}"))?;
        att.path = Some(batch_dir.join(&name).to_string_lossy().into_owned());
        // Keep the decoded transport data in the returned in-memory value so
        // the ReAct media projection can still reference the just-persisted
        // asset while the model-facing media tool performs OCR/STT.
        // `messages.ui_metadata` strips it at the DB boundary and snapshots
        // use `MediaInput::for_snapshot`, so this is not a second durable
        // authority.
        persisted.push(att);
    }
    tokio::fs::rename(&staging_dir, &batch_dir)
        .await
        .map_err(|e| format!("提交上传批次失败: {e}"))?;
    // Register every committed file while still holding the upload write lock.
    // A session lease (or pending ingress lease before a new session gets its
    // id) closes the interval between atomic batch rename and transcript
    // projection.
    if let Some((registry, session_id)) = registry {
        for attachment in &persisted {
            let (Some(asset_id), Some(path)) = (&attachment.asset_id, &attachment.path) else {
                continue;
            };
            let registered = if let Some(session_id) = session_id.as_deref() {
                registry.register_under_root_for_session(
                    session_id,
                    &root,
                    asset_id.clone(),
                    std::path::PathBuf::from(path),
                    attachment.filename.clone(),
                    attachment.media_type.clone(),
                )
            } else {
                registry.register_under_root_pending(
                    &root,
                    asset_id.clone(),
                    std::path::PathBuf::from(path),
                    attachment.filename.clone(),
                    attachment.media_type.clone(),
                )
            };
            if !registered {
                let _ = std::fs::remove_dir_all(&batch_dir);
                registry.prune_missing();
                return Err("无法注册受管附件".into());
            }
        }
    }
    guard.committed = true;
    Ok(persisted)
}

fn filename_collision_key(name: &str) -> String {
    #[cfg(windows)]
    {
        name.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        name.to_string()
    }
}

fn upload_tree_size(path: &std::path::Path) -> Result<u64, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取上传目录元数据失败: {error}")),
    };
    if is_link_or_reparse(&metadata) {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path).map_err(|error| format!("读取上传目录失败: {error}"))?
    {
        let entry = entry.map_err(|error| format!("读取上传目录项失败: {error}"))?;
        total = total.saturating_add(upload_tree_size(&entry.path())?);
    }
    Ok(total)
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn path_is_equal_or_child(root: &std::path::Path, candidate: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let root = root.to_string_lossy().to_lowercase();
        let candidate = candidate.to_string_lossy().to_lowercase();
        candidate == root
            || candidate.starts_with(&format!("{root}\\"))
            || candidate.starts_with(&format!("{root}/"))
    }
    #[cfg(not(windows))]
    {
        candidate == root || candidate.strip_prefix(root).is_ok()
    }
}

fn path_is_equal(left: &std::path::Path, right: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(media_type: &str, data: &str) -> haven_common::types::MessageAttachment {
        haven_common::types::MessageAttachment::new(media_type, data)
    }

    #[cfg(windows)]
    fn create_directory_reparse_point(
        target: &std::path::Path,
        link: &std::path::Path,
    ) -> std::io::Result<()> {
        let quote_path =
            |path: &std::path::Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
        let command = format!(
            "$ErrorActionPreference = 'Stop'; New-Item -ItemType Junction -Path {} -Target {} | Out-Null",
            quote_path(link),
            quote_path(target)
        );
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &command])
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!(
                "{} {}",
                String::from_utf8_lossy(&output.stderr).trim(),
                String::from_utf8_lossy(&output.stdout).trim()
            )))
        }
    }

    #[test]
    fn test_generated_upload_batch_name_is_strict() {
        assert!(is_generated_upload_batch(
            "file-0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_generated_upload_batch("file-user-created"));
        assert!(!is_generated_upload_batch(
            "file-0123456789abcdef0123456789abcdeg"
        ));
        assert!(!is_generated_upload_batch("uploads-file-0123456789abcdef"));
    }

    #[test]
    fn test_generated_media_file_name_is_strict() {
        assert!(is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef.png"
        ));
        assert!(is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef-report.pdf"
        ));
        assert!(!is_generated_media_file("file-user-created.png"));
        assert!(!is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef-"
        ));
        assert!(!is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef-../outside.txt"
        ));
        assert!(!is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef.png.tmp"
        ));
        assert!(!is_generated_media_file(
            "file-0123456789abcdef0123456789abcdeg.png"
        ));
    }

    #[tokio::test]
    async fn test_generated_media_cleanup_reclaims_clipboard_copy_names() {
        let upload_root = tempfile::TempDir::new().unwrap();
        let generated_root = tempfile::TempDir::new().unwrap();
        let clipboard_copy = generated_root
            .path()
            .join("file-0123456789abcdef0123456789abcdef-report.pdf");
        tokio::fs::write(&clipboard_copy, b"clipboard copy")
            .await
            .unwrap();

        assert_eq!(
            cleanup_unreferenced_managed_media_with_references(
                upload_root.path().to_path_buf(),
                generated_root.path().to_path_buf(),
                haven_tools::ManagedAssetRegistry::default(),
                Ok(Vec::new()),
            )
            .await
            .unwrap(),
            (0, 1)
        );
        assert!(!clipboard_copy.exists());
    }

    #[tokio::test]
    async fn test_reference_read_failure_leaves_both_media_roots_untouched() {
        let upload_root = tempfile::TempDir::new().unwrap();
        let generated_root = tempfile::TempDir::new().unwrap();
        let batch = upload_root
            .path()
            .join("file-0123456789abcdef0123456789abcdef");
        let generated = generated_root
            .path()
            .join("file-fedcba9876543210fedcba9876543210.png");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(batch.join("attachment.txt"), b"upload")
            .await
            .unwrap();
        tokio::fs::write(&generated, b"generated").await.unwrap();

        let error = cleanup_unreferenced_managed_media_with_references(
            upload_root.path().to_path_buf(),
            generated_root.path().to_path_buf(),
            haven_tools::ManagedAssetRegistry::default(),
            Err("injected SessionStore read failure".to_string()),
        )
        .await
        .unwrap_err();

        assert!(error.contains("SessionStore read failure"));
        assert!(batch.exists());
        assert!(generated.exists());
    }

    #[test]
    fn test_cleanup_attempts_upload_root_after_generated_root_failure() {
        let mut uploads_attempted = false;

        let error = cleanup_media_roots(
            || Err("generated root is unreadable".into()),
            || {
                uploads_attempted = true;
                Ok(1)
            },
        )
        .unwrap_err();

        assert!(uploads_attempted);
        assert!(error.contains("generated root is unreadable"));
    }

    #[tokio::test]
    async fn test_cancelled_gc_keeps_registry_gate_until_blocking_generated_sweep_ends() {
        let registry = haven_tools::ManagedAssetRegistry::default();
        let generated_guard = registry.lock_generated_media_cleanup().await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let cleaner = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                cleanup_media_roots_with_generated_guard(
                    generated_guard,
                    || {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(0)
                    },
                    || Ok(0),
                )
                .unwrap();
            })
            .await
            .unwrap();
        });
        entered_rx.await.unwrap();
        cleaner.abort();
        assert!(cleaner.await.unwrap_err().is_cancelled());

        let waiting_registry = registry.clone();
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        let waiting_cleaner = tokio::spawn(async move {
            waiting_tx.send(()).unwrap();
            let _guard = waiting_registry.lock_generated_media_cleanup().await;
        });
        waiting_rx.await.unwrap();
        assert!(
            !waiting_cleaner.is_finished(),
            "the blocking generated sweep still owns the cleanup gate"
        );

        release_tx.send(()).unwrap();
        waiting_cleaner.await.unwrap();
    }

    #[tokio::test]
    async fn test_cleanup_stale_upload_batches_only_removes_generated_dirs() {
        let root = tempfile::TempDir::new().unwrap();
        let stale = root.path().join("file-0123456789abcdef0123456789abcdef");
        let unrelated = root.path().join("file-user-created");
        tokio::fs::create_dir_all(&stale).await.unwrap();
        tokio::fs::create_dir_all(&unrelated).await.unwrap();
        let removed =
            cleanup_stale_upload_batches(root.path().to_path_buf(), std::time::Duration::ZERO)
                .await
                .unwrap();
        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(unrelated.exists());
    }

    #[tokio::test]
    async fn test_cleanup_stale_upload_staging_is_independent_from_batch_retention() {
        let root = tempfile::TempDir::new().unwrap();
        let stale = root
            .path()
            .join(".file-0123456789abcdef0123456789abcdef.tmp");
        let unrelated = root.path().join("file-user-created");
        tokio::fs::create_dir_all(&stale).await.unwrap();
        tokio::fs::create_dir_all(&unrelated).await.unwrap();

        let removed = cleanup_stale_upload_staging_with_age(
            root.path().to_path_buf(),
            std::time::Duration::ZERO,
        )
        .await
        .unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(unrelated.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_staging_cleanup_rejects_symlink_root() {
        let target = tempfile::TempDir::new().unwrap();
        let link_parent = tempfile::TempDir::new().unwrap();
        let staging = target
            .path()
            .join(".file-0123456789abcdef0123456789abcdef.tmp");
        tokio::fs::create_dir_all(&staging).await.unwrap();
        let link = link_parent.path().join("uploads");
        std::os::unix::fs::symlink(target.path(), &link).unwrap();

        assert_eq!(
            cleanup_stale_upload_staging_with_age(link, std::time::Duration::ZERO)
                .await
                .unwrap(),
            0
        );
        assert!(staging.exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_staging_cleanup_rejects_reparse_root() {
        let target = tempfile::TempDir::new().unwrap();
        let link_parent = tempfile::TempDir::new().unwrap();
        let staging = target
            .path()
            .join(".file-0123456789abcdef0123456789abcdef.tmp");
        tokio::fs::create_dir_all(&staging).await.unwrap();
        let link = link_parent.path().join("uploads");
        create_directory_reparse_point(target.path(), &link).unwrap();

        assert_eq!(
            cleanup_stale_upload_staging_with_age(link, std::time::Duration::ZERO)
                .await
                .unwrap(),
            0
        );
        assert!(staging.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_upload_cleanup_rejects_symlink_batch_entry() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let batch = outside.path().join("owned");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(batch.join("keep.txt"), b"keep")
            .await
            .unwrap();
        let link = root.path().join("file-0123456789abcdef0123456789abcdef");
        std::os::unix::fs::symlink(&batch, &link).unwrap();

        assert_eq!(
            cleanup_unreferenced_upload_batches(
                root.path().to_path_buf(),
                haven_tools::ManagedAssetRegistry::default(),
                Vec::new(),
            )
            .await
            .unwrap(),
            0
        );
        assert!(link.exists());
        assert!(batch.join("keep.txt").exists());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_upload_cleanup_rejects_reparse_batch_entry() {
        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        let batch = outside.path().join("owned");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(batch.join("keep.txt"), b"keep")
            .await
            .unwrap();
        let link = root.path().join("file-0123456789abcdef0123456789abcdef");
        create_directory_reparse_point(&batch, &link).unwrap();

        assert_eq!(
            cleanup_unreferenced_upload_batches(
                root.path().to_path_buf(),
                haven_tools::ManagedAssetRegistry::default(),
                Vec::new(),
            )
            .await
            .unwrap(),
            0
        );
        assert!(link.exists());
        assert!(batch.join("keep.txt").exists());
    }

    #[tokio::test]
    async fn test_generated_media_follows_session_reference_and_lease() {
        let upload_root = tempfile::TempDir::new().unwrap();
        let generated_root = tempfile::TempDir::new().unwrap();
        let file = generated_root
            .path()
            .join("file-0123456789abcdef0123456789abcdef.png");
        tokio::fs::write(&file, b"png-bytes").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root_for_session_with_metadata(
            "ses-active",
            generated_root.path(),
            "asset-generated",
            file.clone(),
            Some("generated.png".into()),
            "image/png",
            Some("hash".into()),
            Some(9),
            Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        ));
        assert!(registry.resolve("asset-generated").is_some());
        assert!(
            registry
                .resolve("asset-generated")
                .unwrap()
                .expires_at
                .is_none()
        );

        assert_eq!(
            cleanup_unreferenced_managed_media_with_references(
                upload_root.path().to_path_buf(),
                generated_root.path().to_path_buf(),
                registry.clone(),
                Ok(vec![file.clone()]),
            )
            .await
            .unwrap(),
            (0, 0)
        );
        assert!(file.exists());

        registry.release_session("ses-active");
        assert_eq!(
            cleanup_unreferenced_managed_media_with_references(
                upload_root.path().to_path_buf(),
                generated_root.path().to_path_buf(),
                registry.clone(),
                Ok(vec![file.clone()]),
            )
            .await
            .unwrap(),
            (0, 0)
        );
        assert!(file.exists(), "durable shared reference outlives the lease");

        assert_eq!(
            cleanup_unreferenced_managed_media_with_references(
                upload_root.path().to_path_buf(),
                generated_root.path().to_path_buf(),
                registry,
                Ok(Vec::new()),
            )
            .await
            .unwrap(),
            (0, 1)
        );
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn test_generated_cleanup_keeps_live_transient_ttl_and_removes_unowned_files() {
        let upload_root = tempfile::TempDir::new().unwrap();
        let generated_root = tempfile::TempDir::new().unwrap();
        let transient = generated_root
            .path()
            .join("file-0123456789abcdef0123456789abcdef.png");
        let orphan = generated_root
            .path()
            .join("file-fedcba9876543210fedcba9876543210.png");
        tokio::fs::write(&transient, b"transient").await.unwrap();
        tokio::fs::write(&orphan, b"orphan").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root_with_metadata(
            generated_root.path(),
            "asset-transient".to_string(),
            transient.clone(),
            Some("transient.png".into()),
            "image/png".to_string(),
            None,
            Some(9),
            Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        ));

        assert_eq!(
            cleanup_unreferenced_managed_media_with_references(
                upload_root.path().to_path_buf(),
                generated_root.path().to_path_buf(),
                registry,
                Ok(Vec::new()),
            )
            .await
            .unwrap(),
            (0, 1)
        );
        assert!(transient.exists());
        assert!(!orphan.exists());
    }

    #[tokio::test]
    async fn test_cleanup_missing_upload_root_is_idempotent() {
        let root = tempfile::TempDir::new().unwrap();
        let missing = root.path().join("uploads");

        let first = cleanup_stale_upload_batches(missing.clone(), std::time::Duration::ZERO)
            .await
            .unwrap();
        let second = cleanup_stale_upload_batches(missing, std::time::Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(first, 0);
        assert_eq!(second, 0);
    }

    #[tokio::test]
    async fn test_persist_file_attachments_writes_every_binary_asset() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut file = att(
            "application/pdf",
            &base64::engine::general_purpose::STANDARD.encode(b"hello pdf"),
        );
        file.filename = Some("报告.pdf".into());
        let img = att("image/png", "aGVsbG8=");

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![file, img])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);

        let saved = out.iter().find(|a| !a.is_image()).unwrap();
        assert!(saved.asset_id.as_deref().unwrap().starts_with("asset-"));
        assert_eq!(
            saved.data,
            base64::engine::general_purpose::STANDARD.encode(b"hello pdf")
        );
        let path = saved.path.as_ref().unwrap();
        assert!(
            path.ends_with("报告.pdf") || path.contains("报告"),
            "keeps the original name"
        );
        let on_disk = std::fs::read(path).unwrap();
        assert_eq!(on_disk, b"hello pdf");

        let image = out.iter().find(|a| a.is_image()).unwrap();
        assert!(image.asset_id.as_deref().unwrap().starts_with("asset-"));
        assert_eq!(image.data, "aGVsbG8=", "gateway keeps a transient payload");
        assert!(image.path.is_some());
    }

    #[tokio::test]
    async fn test_upload_pending_lease_closes_commit_to_session_binding_window() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        let mut attachment = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"pending"),
        );
        attachment.filename = Some("pending.txt".into());
        let persisted = persist_file_attachments_to_with_limit_and_registry(
            root.path().to_path_buf(),
            vec![attachment],
            1024,
            Some((registry.clone(), None)),
        )
        .await
        .unwrap();
        let asset_id = persisted[0].asset_id.as_deref().unwrap();
        let path = std::path::PathBuf::from(persisted[0].path.as_deref().unwrap());
        let batch = path.parent().unwrap().to_path_buf();

        assert!(registry.protected_paths().contains(&path));
        assert_eq!(
            cleanup_unreferenced_upload_batches(
                root.path().to_path_buf(),
                registry.clone(),
                Vec::new(),
            )
            .await
            .unwrap(),
            0
        );
        assert!(path.exists());

        registry.release_pending(asset_id);
        assert_eq!(
            cleanup_unreferenced_upload_batches(root.path().to_path_buf(), registry, Vec::new(),)
                .await
                .unwrap(),
            1
        );
        assert!(!batch.exists());
    }

    #[tokio::test]
    async fn test_upload_and_cleanup_concurrency_preserves_pending_lease() {
        use base64::Engine as _;

        let root = tempfile::TempDir::new().unwrap();
        let generated_root = tempfile::TempDir::new().unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        let mut attachment = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"pending"),
        );
        attachment.filename = Some("pending.txt".into());

        let (uploaded, cleanup) = tokio::join!(
            persist_file_attachments_to_with_limit_and_registry(
                root.path().to_path_buf(),
                vec![attachment],
                1024,
                Some((registry.clone(), None)),
            ),
            cleanup_unreferenced_managed_media_with_references(
                root.path().to_path_buf(),
                generated_root.path().to_path_buf(),
                registry.clone(),
                Ok(Vec::new()),
            ),
        );

        let uploaded = uploaded.unwrap();
        cleanup.unwrap();
        let path = std::path::PathBuf::from(uploaded[0].path.as_deref().unwrap());
        assert!(path.exists());
        assert!(registry.protected_paths().contains(&path));
    }

    #[tokio::test]
    async fn test_persist_file_attachments_manages_audio_assets() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut audio = att("audio/wav", "UklGRg==");
        audio.filename = Some("voice.wav".into());

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![audio])
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].is_audio());
        assert!(out[0].asset_id.as_deref().unwrap().starts_with("asset-"));
        assert_eq!(out[0].data, "UklGRg==");
        assert!(out[0].path.is_some());
    }

    #[tokio::test]
    async fn test_persist_file_attachments_dedupes_collisions() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut a = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"one"),
        );
        a.filename = Some("same.txt".into());
        let mut b = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"two"),
        );
        b.filename = Some("same.txt".into());

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![a, b])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        let paths: Vec<_> = out.iter().map(|f| f.path.as_deref().unwrap()).collect();
        assert_ne!(paths[0], paths[1], "colliding names must not overwrite");
        assert!(
            paths[0].ends_with("same.txt") && paths[1].ends_with("same_2.txt")
                || paths[1].ends_with("same.txt") && paths[0].ends_with("same_2.txt")
        );
    }

    #[tokio::test]
    async fn test_persist_file_attachments_rolls_back_failed_batch() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut valid = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"valid"),
        );
        valid.filename = Some("valid.txt".into());
        let mut invalid = att("text/plain", "not-base64");
        invalid.filename = Some("invalid.txt".into());

        let result =
            persist_file_attachments_to(tmp.path().to_path_buf(), vec![valid, invalid]).await;

        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn test_persist_file_attachments_enforces_total_upload_quota() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut file = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"12345"),
        );
        file.filename = Some("quota.txt".into());

        let result =
            persist_file_attachments_to_with_limit(tmp.path().to_path_buf(), vec![file], 4).await;

        assert!(result.unwrap_err().contains("总容量"));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn test_concurrent_uploads_share_the_total_quota() {
        use base64::Engine as _;

        let root = tempfile::TempDir::new().unwrap();
        let payload = base64::engine::general_purpose::STANDARD.encode(b"123");
        let mut first = att("text/plain", &payload);
        first.filename = Some("first.txt".into());
        let mut second = att("text/plain", &payload);
        second.filename = Some("second.txt".into());

        let (first, second) = tokio::join!(
            persist_file_attachments_to_with_limit(root.path().to_path_buf(), vec![first], 5,),
            persist_file_attachments_to_with_limit(root.path().to_path_buf(), vec![second], 5,),
        );

        assert_ne!(first.is_ok(), second.is_ok());
        assert_eq!(upload_tree_size(root.path()).unwrap(), 3);
    }

    #[tokio::test]
    async fn test_cleanup_preserves_durable_reference_and_prunes_deleted_entry() {
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let file = batch.join("keep.txt");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(&file, "keep").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            root.path(),
            "asset-live",
            file.clone(),
            Some("keep.txt".into()),
            "text/plain",
        ));

        let removed = cleanup_unreferenced_upload_batches(
            root.path().to_path_buf(),
            registry.clone(),
            vec![file.clone()],
        )
        .await
        .unwrap();
        assert_eq!(removed, 0);
        assert!(file.exists());
        assert!(registry.contains("asset-live"));

        tokio::fs::remove_dir_all(&batch).await.unwrap();
        assert_eq!(registry.prune_missing(), 1);
        assert!(!registry.contains("asset-live"));
    }

    #[tokio::test]
    async fn test_cleanup_preserves_active_session_lease_without_message_reference() {
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let file = batch.join("active.txt");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(&file, "active").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root_for_session(
            "ses-active",
            root.path(),
            "asset-live",
            file.clone(),
            Some("active.txt".into()),
            "text/plain",
        ));

        let removed = cleanup_unreferenced_upload_batches(
            root.path().to_path_buf(),
            registry.clone(),
            Vec::new(),
        )
        .await
        .unwrap();
        assert_eq!(removed, 0);
        assert!(file.exists());

        registry.release_session("ses-active");
        let removed =
            cleanup_unreferenced_upload_batches(root.path().to_path_buf(), registry, Vec::new())
                .await
                .unwrap();
        assert_eq!(removed, 1);
        assert!(!batch.exists());
    }

    #[tokio::test]
    async fn test_cleanup_removes_registered_assets_deleted_from_history() {
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let keep_batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let old_batch = root.path().join("file-fedcba9876543210fedcba9876543210");
        let keep_file = keep_batch.join("keep.txt");
        let old_file = old_batch.join("old.txt");
        tokio::fs::create_dir_all(&keep_batch).await.unwrap();
        tokio::fs::create_dir_all(&old_batch).await.unwrap();
        tokio::fs::write(&keep_file, "keep").await.unwrap();
        tokio::fs::write(&old_file, "old").await.unwrap();

        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            root.path(),
            "asset-keep",
            keep_file.clone(),
            Some("keep.txt".into()),
            "text/plain",
        ));
        assert!(registry.register_under_root(
            root.path(),
            "asset-old",
            old_file,
            Some("old.txt".into()),
            "text/plain",
        ));

        let removed = cleanup_unreferenced_upload_batches(
            root.path().to_path_buf(),
            registry.clone(),
            vec![keep_file],
        )
        .await
        .unwrap();

        assert_eq!(removed, 1);
        assert!(keep_batch.exists());
        assert!(!old_batch.exists());
        assert!(registry.contains("asset-keep"));
        assert!(!registry.contains("asset-old"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_persist_file_attachments_dedupes_case_insensitive_windows_names() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut upper = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"upper"),
        );
        upper.filename = Some("A.txt".into());
        let mut lower = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"lower"),
        );
        lower.filename = Some("a.txt".into());

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![upper, lower])
            .await
            .unwrap();
        let names: Vec<_> = out
            .iter()
            .map(|attachment| {
                std::path::Path::new(attachment.path.as_deref().unwrap())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_lowercase()
            })
            .collect();
        assert!(names.contains(&"a.txt".into()));
        assert!(names.contains(&"a_2.txt".into()));
    }

    #[test]
    fn test_sanitize_filename_blocks_path_traversal() {
        assert_eq!(sanitize_filename("a/b\\c:d"), "a_b_c_d");
        let traversal = sanitize_filename("..");
        assert_ne!(traversal, "..");
        assert!(!traversal.contains('/') && !traversal.contains('\\'));
        let named = sanitize_filename("a");
        assert_eq!(named, "a");
    }
}
