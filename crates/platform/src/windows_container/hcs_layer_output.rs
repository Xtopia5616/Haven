//! Bounded extraction of Haven's workspace subtree from a Windows layer
//! transport directory produced by HCS.
//!
//! HCS layer transport is not a normal directory tree. Its `Files` entries
//! contain Windows backup streams and directory metadata markers. This module
//! recognizes only that transport representation, extracts only the reserved
//! `C:\\Haven\\workspace-output` subtree, and writes through a private
//! destination capability. Any error leaves a partial destination that the
//! owning attempt must discard.

use super::hcs_process::validate_guest_path_component;
use crate::filesystem::{DirectoryCapability, DirectoryEntryKind, DirectoryTreeDiffLimits};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

const LAYER_FILES_ROOT: &str = "Files";
const WORKSPACE_COMPONENTS: [&str; 2] = ["Haven", "workspace-output"];
const DIRECTORY_METADATA_SUFFIX: &str = ".$wcidirs$";
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
const BACKUP_DATA_STREAM_ID: u32 = 1;
const BACKUP_STREAM_HEADER_BYTES: usize = 20;
const MAX_BACKUP_STREAM_NAME_BYTES: u32 = 64 * 1024;
const MAX_LAYER_SCAN_ENTRIES: usize = 100_000;
const MAX_LAYER_SCAN_DEPTH: usize = 64;
const MAX_LAYER_PATH_BYTES: usize = 4 * 1024 * 1024;
const MAX_LAYER_TRANSPORT_METADATA_BYTES: u64 = 64 * 1024 * 1024;
const MAX_BACKUP_FILE_OVERHEAD_BYTES: u64 = 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct HcsLayerOutputSummary {
    pub(super) entry_count: usize,
    pub(super) file_count: usize,
    pub(super) directory_count: usize,
    pub(super) data_bytes: u64,
}

/// Extract the final workspace tree from an HCS writable-layer transport.
///
/// `destination` must be an empty, attempt-private capability and must be
/// discarded on any error. The writable layer's `Files` subtree is a Windows
/// backup-stream transport format, so file contents are copied only from one
/// validated `BACKUP_DATA` stream. Tombstones and all entries outside the
/// workspace subtree are ignored; the workspace root is reserved and absent
/// from the trusted image, so its current layer contents represent the full
/// output tree after the fixed seed step.
pub(super) fn extract_workspace_output(
    transport: &DirectoryCapability,
    destination: &DirectoryCapability,
    limits: DirectoryTreeDiffLimits,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<HcsLayerOutputSummary> {
    check_cancelled(&mut is_cancelled)?;
    ensure_disjoint_roots(transport, destination)?;
    if !destination.entries(1)?.is_empty() {
        return Err(invalid_input(
            "HCS workspace output destination must be empty",
        ));
    }

    let files_root = match transport.entries_at(Path::new(LAYER_FILES_ROOT), MAX_LAYER_SCAN_ENTRIES)
    {
        Ok(_) => PathBuf::from(LAYER_FILES_ROOT),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(invalid_data("HCS transport is missing its Files tree"));
        }
        Err(error) => return Err(error),
    };
    let scan_limits = limits.scan();
    let mut summary = HcsLayerOutputSummary::default();
    let mut visited_transport_entries = 0usize;
    let mut total_path_bytes = 0usize;
    let mut transport_file_bytes = 0u64;
    // NTFS paths are case-insensitive by default. Reject file/directory
    // collisions before creating a destination entry. A directory may be
    // represented both by its physical directory and its `.$wcidirs$` record.
    let mut logical_entries = BTreeMap::<String, DirectoryEntryKind>::new();
    let mut pending = vec![PathBuf::new()];

    while let Some(relative_directory) = pending.pop() {
        check_cancelled(&mut is_cancelled)?;
        let directory = if relative_directory.as_os_str().is_empty() {
            transport.entries_at(&files_root, MAX_LAYER_SCAN_ENTRIES)?
        } else {
            transport.entries_at(
                &files_root.join(&relative_directory),
                MAX_LAYER_SCAN_ENTRIES,
            )?
        };
        for entry in directory {
            check_cancelled(&mut is_cancelled)?;
            visited_transport_entries = visited_transport_entries
                .checked_add(1)
                .ok_or_else(|| invalid_data("HCS layer entry count overflowed"))?;
            if visited_transport_entries > MAX_LAYER_SCAN_ENTRIES {
                return Err(invalid_data(
                    "HCS layer transport exceeds its entry scan limit",
                ));
            }
            let name = entry.name.to_str().ok_or_else(|| {
                invalid_data("HCS layer transport contains a non-Unicode filename")
            })?;
            let transport_path = relative_directory.join(name);
            let has_directory_suffix = transport_path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(DIRECTORY_METADATA_SUFFIX));
            let stripped_marker_path = if has_directory_suffix {
                strip_directory_metadata_suffix(&transport_path)
            } else {
                None
            };
            let stripped_mapping = stripped_marker_path
                .as_deref()
                .map(map_workspace_path)
                .transpose()?;
            let raw_mapping = map_workspace_path(&transport_path)?;
            let candidate_mapping = stripped_mapping
                .as_ref()
                .filter(|mapping| !matches!(mapping, WorkspacePath::Outside))
                .unwrap_or(&raw_mapping);
            if matches!(candidate_mapping, WorkspacePath::Outside)
                && matches!(
                    entry.kind,
                    DirectoryEntryKind::File | DirectoryEntryKind::Other
                )
            {
                continue;
            }
            let source_path = files_root.join(&transport_path);
            let mut source = if entry.kind == DirectoryEntryKind::File {
                Some(transport.open_read(&source_path)?)
            } else {
                None
            };
            let attributes = source.as_mut().map(read_file_attributes).transpose()?;
            if let Some(attributes) = attributes {
                reject_unsafe_attributes(attributes)?;
                let source_size = source
                    .as_ref()
                    .expect("file entry has an opened transport file")
                    .metadata()?
                    .len();
                transport_file_bytes = transport_file_bytes
                    .checked_add(source_size)
                    .ok_or_else(|| invalid_data("HCS transport byte count overflowed"))?;
                let maximum_transport_bytes = scan_limits
                    .max_total_bytes()
                    .checked_add(MAX_LAYER_TRANSPORT_METADATA_BYTES)
                    .ok_or_else(|| invalid_data("HCS transport byte limit overflowed"))?;
                if transport_file_bytes > maximum_transport_bytes {
                    return Err(invalid_data(
                        "HCS layer workspace transport exceeds its byte scan limit",
                    ));
                }
            }
            let is_directory_marker = has_directory_suffix
                && attributes.is_some_and(|value| value & FILE_ATTRIBUTE_DIRECTORY != 0);
            let logical_transport_path = if is_directory_marker {
                stripped_marker_path.expect("directory marker path was checked")
            } else {
                transport_path.clone()
            };
            let mapping = map_workspace_path(&logical_transport_path)?;
            let depth = logical_transport_path.components().count();
            if !matches!(mapping, WorkspacePath::Outside) && depth > MAX_LAYER_SCAN_DEPTH {
                return Err(invalid_data("HCS layer path exceeds its depth limit"));
            }

            match entry.kind {
                DirectoryEntryKind::ReparsePoint => {
                    if matches!(
                        candidate_mapping,
                        WorkspacePath::Ancestor | WorkspacePath::Root | WorkspacePath::Entry(_)
                    ) {
                        return Err(permission_denied(
                            "HCS workspace output contains a reparse point",
                        ));
                    }
                }
                DirectoryEntryKind::Directory => {
                    if matches!(&mapping, WorkspacePath::Root | WorkspacePath::Entry(_)) {
                        let output_relative = match &mapping {
                            WorkspacePath::Entry(path) => path.clone(),
                            WorkspacePath::Root | WorkspacePath::Ancestor => PathBuf::new(),
                            WorkspacePath::Outside => unreachable!(),
                        };
                        if !output_relative.as_os_str().is_empty() {
                            let is_new = record_logical_entry(
                                &mut logical_entries,
                                &output_relative,
                                DirectoryEntryKind::Directory,
                            )?;
                            if is_new {
                                record_path_bytes(
                                    &mut total_path_bytes,
                                    output_relative.as_os_str().len(),
                                )?;
                            }
                            if is_new && summary.directory_count >= scan_limits.max_entries() {
                                return Err(invalid_data(
                                    "HCS workspace output exceeds its directory limit",
                                ));
                            }
                            if is_new {
                                summary.directory_count += 1;
                            }
                            destination.create_dir_all(&output_relative)?;
                        }
                    }
                    if matches!(
                        &mapping,
                        WorkspacePath::Ancestor | WorkspacePath::Root | WorkspacePath::Entry(_)
                    ) {
                        pending.push(transport_path);
                    }
                }
                DirectoryEntryKind::File => {
                    let output_relative = match mapping {
                        WorkspacePath::Root => PathBuf::new(),
                        WorkspacePath::Entry(path) => path,
                        WorkspacePath::Outside | WorkspacePath::Ancestor => continue,
                    };
                    let mut source = source
                        .take()
                        .ok_or_else(|| invalid_data("HCS layer file handle is missing"))?;
                    let attributes = attributes
                        .ok_or_else(|| invalid_data("HCS layer file metadata is missing"))?;
                    let is_directory_record = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                    if is_directory_record != is_directory_marker {
                        return Err(invalid_data(
                            "HCS layer directory metadata does not match its transport path",
                        ));
                    }
                    if is_directory_record {
                        if !output_relative.as_os_str().is_empty() {
                            let is_new = record_logical_entry(
                                &mut logical_entries,
                                &output_relative,
                                DirectoryEntryKind::Directory,
                            )?;
                            if is_new {
                                record_path_bytes(
                                    &mut total_path_bytes,
                                    output_relative.as_os_str().len(),
                                )?;
                            }
                            if is_new && summary.directory_count >= scan_limits.max_entries() {
                                return Err(invalid_data(
                                    "HCS workspace output exceeds its directory limit",
                                ));
                            }
                            if is_new {
                                summary.directory_count += 1;
                            }
                            destination.create_dir_all(&output_relative)?;
                        }
                        validate_empty_directory_backup_stream(&mut source, &mut is_cancelled)?;
                    } else {
                        if output_relative.as_os_str().is_empty() {
                            return Err(invalid_data(
                                "HCS layer output file aliases the workspace root",
                            ));
                        }
                        validate_output_relative_path(&output_relative)?;
                        let is_new = record_logical_entry(
                            &mut logical_entries,
                            &output_relative,
                            DirectoryEntryKind::File,
                        )?;
                        if is_new {
                            record_path_bytes(
                                &mut total_path_bytes,
                                output_relative.as_os_str().len(),
                            )?;
                        }
                        if summary.file_count >= scan_limits.max_entries() {
                            return Err(invalid_data(
                                "HCS workspace output exceeds its file limit",
                            ));
                        }
                        let mut destination_file = destination.create_new_file(&output_relative)?;
                        let remaining_total = scan_limits
                            .max_total_bytes()
                            .checked_sub(summary.data_bytes)
                            .ok_or_else(|| invalid_data("HCS output byte count overflowed"))?;
                        let maximum_file_bytes = scan_limits.max_file_bytes().min(remaining_total);
                        let maximum_transport_file_bytes = scan_limits
                            .max_file_bytes()
                            .checked_add(MAX_BACKUP_FILE_OVERHEAD_BYTES)
                            .ok_or_else(|| invalid_data("HCS backup file limit overflowed"))?;
                        if source.metadata()?.len() > maximum_transport_file_bytes {
                            return Err(invalid_data(
                                "HCS layer file transport exceeds its per-file limit",
                            ));
                        }
                        let copied = copy_backup_data_stream(
                            &mut source,
                            &mut destination_file,
                            maximum_file_bytes,
                            &mut is_cancelled,
                        )?;
                        summary.data_bytes = summary
                            .data_bytes
                            .checked_add(copied)
                            .ok_or_else(|| invalid_data("HCS output byte count overflowed"))?;
                        if summary.data_bytes > scan_limits.max_total_bytes() {
                            return Err(invalid_data(
                                "HCS workspace output exceeds its byte limit",
                            ));
                        }
                        destination_file.flush()?;
                        if is_new {
                            summary.file_count += 1;
                        }
                    }
                }
                DirectoryEntryKind::Other => {
                    if matches!(
                        candidate_mapping,
                        WorkspacePath::Ancestor | WorkspacePath::Root | WorkspacePath::Entry(_)
                    ) {
                        return Err(permission_denied(
                            "HCS workspace output contains a special filesystem entry",
                        ));
                    }
                }
            }
        }
    }

    summary.entry_count = summary
        .file_count
        .checked_add(summary.directory_count)
        .ok_or_else(|| invalid_data("HCS output entry count overflowed"))?;
    if summary.entry_count > scan_limits.max_entries() {
        return Err(invalid_data("HCS workspace output exceeds its entry limit"));
    }

    Ok(summary)
}

#[derive(Debug)]
enum WorkspacePath {
    Outside,
    Ancestor,
    Root,
    Entry(PathBuf),
}

fn map_workspace_path(path: &Path) -> io::Result<WorkspacePath> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid_data("HCS layer path is not valid Unicode")),
            _ => Err(invalid_data(
                "HCS layer path is not relative and normalized",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;
    for (index, expected) in WORKSPACE_COMPONENTS.iter().enumerate() {
        let Some(component) = components.get(index) else {
            return Ok(if index > 0 {
                WorkspacePath::Ancestor
            } else {
                WorkspacePath::Outside
            });
        };
        if !component.eq_ignore_ascii_case(expected) {
            return Ok(WorkspacePath::Outside);
        }
    }
    if components.len() == WORKSPACE_COMPONENTS.len() {
        return Ok(WorkspacePath::Root);
    }
    let mut relative = PathBuf::new();
    for component in &components[WORKSPACE_COMPONENTS.len()..] {
        validate_guest_path_component(component)?;
        relative.push(component);
    }
    Ok(WorkspacePath::Entry(relative))
}

fn strip_directory_metadata_suffix(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_str()?;
    let stripped = name.strip_suffix(DIRECTORY_METADATA_SUFFIX)?;
    let mut result = path.to_path_buf();
    result.set_file_name(stripped);
    Some(result)
}

fn validate_output_relative_path(path: &Path) -> io::Result<()> {
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(invalid_data(
                "HCS output path is not relative and normalized",
            ));
        };
        let name = name
            .to_str()
            .ok_or_else(|| invalid_data("HCS output path is not valid Unicode"))?;
        validate_guest_path_component(name)?;
    }
    Ok(())
}

fn record_logical_entry(
    entries: &mut BTreeMap<String, DirectoryEntryKind>,
    path: &Path,
    kind: DirectoryEntryKind,
) -> io::Result<bool> {
    let key = path.to_string_lossy().to_lowercase();
    match entries.get(&key) {
        Some(existing) if *existing != kind => Err(invalid_data(
            "HCS workspace output has a file and directory at the same path",
        )),
        Some(_) if kind == DirectoryEntryKind::Directory => Ok(false),
        Some(_) => Err(invalid_data(
            "HCS workspace output contains duplicate file paths",
        )),
        None => {
            entries.insert(key, kind);
            Ok(true)
        }
    }
}

fn record_path_bytes(total: &mut usize, bytes: usize) -> io::Result<()> {
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| invalid_data("HCS output path byte count overflowed"))?;
    if *total > MAX_LAYER_PATH_BYTES {
        return Err(invalid_data("HCS workspace output exceeds its path limit"));
    }
    Ok(())
}

fn read_file_attributes(source: &mut File) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    source.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn reject_unsafe_attributes(attributes: u32) -> io::Result<()> {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(permission_denied(
            "HCS workspace output contains a reparse point",
        ));
    }
    Ok(())
}

fn validate_empty_directory_backup_stream(
    source: &mut File,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    let (data_bytes, _) = parse_backup_streams(source, None, 0, is_cancelled)?;
    if data_bytes != 0 {
        return Err(invalid_data(
            "HCS layer directory metadata contains unexpected data",
        ));
    }
    Ok(())
}

fn copy_backup_data_stream(
    source: &mut File,
    destination: &mut File,
    maximum_data_bytes: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<u64> {
    let (data_bytes, stream_count) =
        parse_backup_streams(source, Some(destination), maximum_data_bytes, is_cancelled)?;
    if stream_count == 0 {
        return Err(invalid_data("HCS layer file has no backup data stream"));
    }
    Ok(data_bytes)
}

fn parse_backup_streams(
    source: &mut File,
    mut destination: Option<&mut File>,
    maximum_data_bytes: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<(u64, usize)> {
    let mut data_bytes = 0u64;
    let mut data_stream_count = 0usize;
    let mut stream_count = 0usize;
    loop {
        check_cancelled(is_cancelled)?;
        let mut header = [0u8; BACKUP_STREAM_HEADER_BYTES];
        if !read_header(source, &mut header)? {
            break;
        }
        stream_count = stream_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("HCS backup stream count overflowed"))?;
        if stream_count > 16 {
            return Err(invalid_data("HCS layer file has too many backup streams"));
        }
        let stream_id = u32::from_le_bytes(header[0..4].try_into().expect("header field"));
        let stream_size = i64::from_le_bytes(header[8..16].try_into().expect("header field"));
        let stream_name_bytes =
            u32::from_le_bytes(header[16..20].try_into().expect("header field"));
        if stream_size < 0
            || stream_name_bytes > MAX_BACKUP_STREAM_NAME_BYTES
            || stream_name_bytes % 2 != 0
        {
            return Err(invalid_data("HCS backup stream header is invalid"));
        }
        let mut stream_name = vec![0u8; stream_name_bytes as usize];
        source.read_exact(&mut stream_name)?;
        let stream_size = stream_size as u64;
        if stream_id == BACKUP_DATA_STREAM_ID {
            if !stream_name.is_empty() {
                return Err(invalid_data(
                    "HCS main backup data stream has an unexpected name",
                ));
            }
            data_stream_count += 1;
            if data_stream_count != 1 {
                return Err(invalid_data(
                    "HCS layer file has multiple backup data streams",
                ));
            }
            if stream_size > maximum_data_bytes {
                return Err(invalid_data("HCS layer file exceeds its byte limit"));
            }
            data_bytes = stream_size;
            if let Some(destination) = destination.as_deref_mut() {
                copy_exact(source, destination, stream_size, is_cancelled)?;
            } else {
                skip_exact(source, stream_size, is_cancelled)?;
            }
        } else {
            skip_exact(source, stream_size, is_cancelled)?;
        }
    }
    Ok((data_bytes, data_stream_count))
}

fn read_header(
    source: &mut File,
    header: &mut [u8; BACKUP_STREAM_HEADER_BYTES],
) -> io::Result<bool> {
    let mut read = 0;
    while read < header.len() {
        match source.read(&mut header[read..])? {
            0 if read == 0 => return Ok(false),
            0 => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated HCS backup stream header",
                ));
            }
            count => read += count,
        }
    }
    Ok(true)
}

fn copy_exact(
    source: &mut File,
    destination: &mut File,
    bytes: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    let mut remaining = bytes;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        check_cancelled(is_cancelled)?;
        let chunk_bytes = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded backup stream chunk fits usize");
        let read = source.read(&mut buffer[..chunk_bytes])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated HCS backup data stream",
            ));
        }
        destination.write_all(&buffer[..read])?;
        remaining -= read as u64;
    }
    Ok(())
}

fn skip_exact(
    source: &mut File,
    bytes: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    let mut remaining = bytes;
    let mut buffer = [0u8; COPY_BUFFER_BYTES];
    while remaining > 0 {
        check_cancelled(is_cancelled)?;
        let chunk_bytes = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded backup stream chunk fits usize");
        let read = source.read(&mut buffer[..chunk_bytes])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated HCS backup stream payload",
            ));
        }
        remaining -= read as u64;
    }
    Ok(())
}

fn ensure_disjoint_roots(
    left: &DirectoryCapability,
    right: &DirectoryCapability,
) -> io::Result<()> {
    let left_root = normalize_path(left.root());
    let right_root = normalize_path(right.root());
    if left_root == right_root
        || left_root.starts_with(&right_root)
        || right_root.starts_with(&left_root)
    {
        return Err(permission_denied(
            "HCS transport and output roots must be disjoint",
        ));
    }
    Ok(())
}

fn normalize_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> io::Result<()> {
    if is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "HCS layer output extraction was cancelled",
        ));
    }
    Ok(())
}

fn invalid_input(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::DirectoryTreeCopyLimits;
    use std::fs;
    use std::path::Path;

    fn limits(max_file_bytes: u64, max_total_bytes: u64) -> DirectoryTreeDiffLimits {
        let scan = DirectoryTreeCopyLimits::new(128, 16, max_file_bytes, max_total_bytes)
            .expect("bounded scan limits");
        DirectoryTreeDiffLimits::new(scan, 128, max_file_bytes, max_total_bytes)
            .expect("bounded diff limits")
    }

    fn roots() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
        (
            tempfile::tempdir().expect("transport root"),
            tempfile::tempdir().expect("destination root"),
            tempfile::tempdir().expect("fixture root"),
        )
    }

    fn backup_stream(attributes: u32, content: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&attributes.to_le_bytes());
        bytes.extend_from_slice(&BACKUP_DATA_STREAM_ID.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(content.len() as i64).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(content);
        bytes
    }

    fn write_layer_file(root: &Path, relative: &str, attributes: u32, content: &[u8]) {
        let path = root.join(LAYER_FILES_ROOT).join(relative);
        fs::create_dir_all(path.parent().unwrap()).expect("transport parent");
        fs::write(path, backup_stream(attributes, content)).expect("layer transport file");
    }

    #[test]
    fn extracts_only_reserved_workspace_files_and_ignores_other_layer_content() {
        let (transport_temp, destination_temp, _) = roots();
        write_layer_file(
            transport_temp.path(),
            r"Haven\workspace-output.$wcidirs$",
            FILE_ATTRIBUTE_DIRECTORY,
            &[],
        );
        write_layer_file(
            transport_temp.path(),
            r"Haven\workspace-output\src\main.rs",
            0x20,
            b"fn main() {}",
        );
        write_layer_file(
            transport_temp.path(),
            r"Haven\workspace-output\odd.$wcidirs$",
            0x20,
            b"literal",
        );
        write_layer_file(
            transport_temp.path(),
            r"Haven\unrelated.txt",
            0x20,
            b"ignore",
        );
        let transport = DirectoryCapability::open(transport_temp.path()).unwrap();
        let destination = DirectoryCapability::open(destination_temp.path()).unwrap();
        let summary =
            extract_workspace_output(&transport, &destination, limits(1024, 4096), || false)
                .expect("workspace subtree extracts");

        assert_eq!(summary.file_count, 2);
        assert_eq!(summary.directory_count, 1);
        assert_eq!(summary.entry_count, 3);
        assert_eq!(summary.data_bytes, 19);
        assert_eq!(
            fs::read(destination_temp.path().join("src").join("main.rs")).unwrap(),
            b"fn main() {}"
        );
        assert_eq!(
            fs::read(destination_temp.path().join("odd.$wcidirs$")).unwrap(),
            b"literal"
        );
        assert!(!destination_temp.path().join("unrelated.txt").exists());
    }

    #[test]
    fn rejects_reparse_attributes_before_copying_file_bytes() {
        let (transport_temp, destination_temp, _) = roots();
        write_layer_file(
            transport_temp.path(),
            r"Haven\workspace-output\escape.txt",
            FILE_ATTRIBUTE_REPARSE_POINT,
            b"link data",
        );
        let transport = DirectoryCapability::open(transport_temp.path()).unwrap();
        let destination = DirectoryCapability::open(destination_temp.path()).unwrap();
        assert_eq!(
            extract_workspace_output(&transport, &destination, limits(1024, 4096), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn rejects_corrupt_backup_streams_and_excess_file_bytes() {
        let (transport_temp, destination_temp, _) = roots();
        let path = transport_temp
            .path()
            .join(LAYER_FILES_ROOT)
            .join("Haven")
            .join("workspace-output")
            .join("large.bin");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, backup_stream(0, b"too large")).unwrap();
        let transport = DirectoryCapability::open(transport_temp.path()).unwrap();
        let destination = DirectoryCapability::open(destination_temp.path()).unwrap();
        assert_eq!(
            extract_workspace_output(&transport, &destination, limits(4, 8), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        fs::write(&path, [0u8; 6]).unwrap();
        let destination_temp_2 = tempfile::tempdir().unwrap();
        let destination_2 = DirectoryCapability::open(destination_temp_2.path()).unwrap();
        assert_eq!(
            extract_workspace_output(&transport, &destination_2, limits(1024, 4096), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn output_components_reject_windows_aliases_and_alternate_streams() {
        for path in [
            Path::new("CON"),
            Path::new("bad:name"),
            Path::new("trailing."),
        ] {
            assert!(validate_output_relative_path(path).is_err());
        }
        assert!(map_workspace_path(Path::new(r"Haven\workspace-output\..\outside")).is_err());
    }

    #[test]
    fn cancellation_and_nonempty_destination_fail_closed() {
        let (transport_temp, destination_temp, _) = roots();
        let transport = DirectoryCapability::open(transport_temp.path()).unwrap();
        let destination = DirectoryCapability::open(destination_temp.path()).unwrap();
        assert_eq!(
            extract_workspace_output(&transport, &destination, limits(1024, 4096), || true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        fs::write(destination_temp.path().join("existing"), b"x").unwrap();
        assert_eq!(
            extract_workspace_output(&transport, &destination, limits(1024, 4096), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn cancellation_is_checked_while_copying_large_backup_streams() {
        let (transport_temp, destination_temp, _) = roots();
        let data = vec![b'x'; COPY_BUFFER_BYTES * 2];
        write_layer_file(
            transport_temp.path(),
            r"Haven\workspace-output\large.bin",
            0x20,
            &data,
        );
        let transport = DirectoryCapability::open(transport_temp.path()).unwrap();
        let destination = DirectoryCapability::open(destination_temp.path()).unwrap();
        let mut checks = 0usize;
        let error = extract_workspace_output(
            &transport,
            &destination,
            limits(1024 * 1024, 1024 * 1024),
            || {
                checks += 1;
                checks >= 10
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(checks >= 10);
        assert!(
            fs::metadata(destination_temp.path().join("large.bin"))
                .unwrap()
                .len()
                < data.len() as u64
        );
    }

    #[test]
    fn unknown_transport_layout_fails_closed() {
        let (transport_temp, destination_temp, _) = roots();
        fs::create_dir(transport_temp.path().join("unknown-layout")).unwrap();
        let transport = DirectoryCapability::open(transport_temp.path()).unwrap();
        let destination = DirectoryCapability::open(destination_temp.path()).unwrap();
        assert_eq!(
            extract_workspace_output(&transport, &destination, limits(1024, 4096), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
