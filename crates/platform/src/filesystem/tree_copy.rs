use super::{
    DirectoryCapability, DirectoryEntryKind, is_cap_link_or_reparse_point,
    reject_relative_reparse_components, same_file_identity,
};
use cap_std::fs::Dir;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

const MAX_TREE_COPY_ENTRIES: usize = 100_000;
const MAX_TREE_COPY_DEPTH: usize = 64;
const MAX_TREE_COPY_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_TREE_COPY_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Caller-selected limits for copying a directory snapshot.
///
/// Values are checked against hard platform limits so an untrusted caller
/// cannot turn a snapshot into an unbounded traversal or allocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryTreeCopyLimits {
    max_entries: usize,
    max_depth: usize,
    max_file_bytes: u64,
    max_total_bytes: u64,
}

impl DirectoryTreeCopyLimits {
    pub fn new(
        max_entries: usize,
        max_depth: usize,
        max_file_bytes: u64,
        max_total_bytes: u64,
    ) -> io::Result<Self> {
        if max_entries == 0
            || max_entries > MAX_TREE_COPY_ENTRIES
            || max_depth == 0
            || max_depth > MAX_TREE_COPY_DEPTH
            || max_file_bytes == 0
            || max_file_bytes > MAX_TREE_COPY_FILE_BYTES
            || max_total_bytes == 0
            || max_total_bytes > MAX_TREE_COPY_TOTAL_BYTES
        {
            return Err(invalid_input(
                "directory snapshot limits are outside the supported range",
            ));
        }
        Ok(Self {
            max_entries,
            max_depth,
            max_file_bytes,
            max_total_bytes,
        })
    }
}

/// Bounded counts returned after a complete directory snapshot copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DirectoryTreeCopySummary {
    pub entry_count: usize,
    pub file_count: usize,
    pub directory_count: usize,
    pub byte_count: u64,
}

impl DirectoryCapability {
    /// Copy this directory into an empty destination capability.
    ///
    /// Both roots must be disjoint, and the destination must be exclusively
    /// owned for the duration of the copy. Traversal rejects links, reparse
    /// points, and non-regular entries. File reads and destination writes stay
    /// anchored to the opened directory capabilities. Limits apply to all
    /// entries and bytes actually copied, not only metadata estimates.
    ///
    /// This is a bounded best-effort snapshot: callers must prevent concurrent
    /// source mutation if they require point-in-time consistency. Cancellation
    /// and errors can leave a partial destination; callers must discard that
    /// entire destination rather than merge it into another workspace.
    pub fn copy_tree_to(
        &self,
        destination_root: &DirectoryCapability,
        limits: DirectoryTreeCopyLimits,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeCopySummary> {
        reject_overlapping_roots(self, destination_root)?;
        reject_non_empty_destination(destination_root)?;

        let mut summary = DirectoryTreeCopySummary::default();
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
        let source_root = self.directory.try_clone()?.into_std_file();
        let destination = destination_root.directory.try_clone()?.into_std_file();
        if same_file_identity(&source_root, &destination)? {
            return Err(invalid_input(
                "source and destination identify the same directory",
            ));
        }
        copy_directory(
            self,
            &self.directory,
            destination_root,
            &PathBuf::new(),
            0,
            &limits,
            &mut summary,
            &mut buffer,
            &mut is_cancelled,
        )?;
        Ok(summary)
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_directory(
    source_root: &DirectoryCapability,
    source_directory: &Dir,
    destination_root: &DirectoryCapability,
    relative_directory: &Path,
    depth: usize,
    limits: &DirectoryTreeCopyLimits,
    summary: &mut DirectoryTreeCopySummary,
    buffer: &mut [u8],
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    if is_cancelled() {
        return Err(cancelled());
    }
    let source_handle = source_directory.try_clone()?.into_std_file();
    let destination_handle = destination_root.directory.try_clone()?.into_std_file();
    if same_file_identity(&source_handle, &destination_handle)? {
        return Err(invalid_input(
            "destination became nested inside the source during snapshot copy",
        ));
    }

    for entry in source_directory.entries()? {
        if is_cancelled() {
            return Err(cancelled());
        }
        let entry = entry?;
        let name = entry.file_name();
        if !is_single_normal_component(&name) {
            return Err(invalid_data("directory contains an invalid entry name"));
        }
        summary.entry_count = summary
            .entry_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("directory entry count overflowed"))?;
        if summary.entry_count > limits.max_entries {
            return Err(invalid_data("directory snapshot exceeds its entry limit"));
        }
        let mut relative = relative_directory.to_path_buf();
        relative.push(&name);
        let metadata = entry.metadata()?;
        let kind = if is_cap_link_or_reparse_point(&metadata) {
            DirectoryEntryKind::ReparsePoint
        } else if metadata.is_dir() {
            DirectoryEntryKind::Directory
        } else if metadata.is_file() {
            DirectoryEntryKind::File
        } else {
            DirectoryEntryKind::Other
        };

        match kind {
            DirectoryEntryKind::Directory => {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("directory depth overflowed"))?;
                if child_depth > limits.max_depth {
                    return Err(invalid_data("directory snapshot exceeds its depth limit"));
                }
                reject_relative_reparse_components(&source_root.directory, &relative, true)?;
                destination_root.directory.create_dir(&relative)?;
                summary.directory_count = summary
                    .directory_count
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("directory count overflowed"))?;
                let child = source_root.directory.open_dir(&relative)?;
                copy_directory(
                    source_root,
                    &child,
                    destination_root,
                    &relative,
                    child_depth,
                    limits,
                    summary,
                    buffer,
                    is_cancelled,
                )?;
            }
            DirectoryEntryKind::File => {
                copy_file(
                    source_root,
                    destination_root,
                    &relative,
                    limits,
                    summary,
                    buffer,
                    is_cancelled,
                )?;
            }
            DirectoryEntryKind::ReparsePoint => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "directory snapshot contains a reparse point",
                ));
            }
            DirectoryEntryKind::Other => {
                return Err(invalid_data(
                    "directory snapshot contains a non-regular entry",
                ));
            }
        }
    }
    Ok(())
}

fn copy_file(
    source_root: &DirectoryCapability,
    destination_root: &DirectoryCapability,
    relative: &Path,
    limits: &DirectoryTreeCopyLimits,
    summary: &mut DirectoryTreeCopySummary,
    buffer: &mut [u8],
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    reject_relative_reparse_components(&source_root.directory, relative, true)?;
    let mut source = source_root.open_read(relative)?;
    let before = source.metadata()?;
    if !before.is_file() {
        return Err(invalid_data(
            "snapshot source changed to a non-regular file",
        ));
    }
    if before.len() > limits.max_file_bytes {
        return Err(invalid_data(
            "file exceeds the directory snapshot file limit",
        ));
    }
    let mut copied_for_file = 0_u64;
    let mut copied_total = summary.byte_count;
    destination_root.atomic_replace_with(relative, |destination| {
        loop {
            if is_cancelled() {
                return Err(cancelled());
            }
            let bytes_read = source.read(buffer)?;
            if bytes_read == 0 {
                break;
            }
            copied_for_file = copied_for_file
                .checked_add(bytes_read as u64)
                .ok_or_else(|| invalid_data("file byte count overflowed"))?;
            copied_total = copied_total
                .checked_add(bytes_read as u64)
                .ok_or_else(|| invalid_data("directory snapshot byte count overflowed"))?;
            if copied_for_file > limits.max_file_bytes {
                return Err(invalid_data(
                    "file exceeds the directory snapshot file limit",
                ));
            }
            if copied_total > limits.max_total_bytes {
                return Err(invalid_data("directory snapshot exceeds its byte limit"));
            }
            destination.write_all(&buffer[..bytes_read])?;
        }
        let after = source.metadata()?;
        if after.len() != copied_for_file || before.len() != copied_for_file {
            return Err(invalid_data(
                "file changed size while the directory snapshot was copied",
            ));
        }
        Ok(())
    })?;
    summary.byte_count = copied_total;
    summary.file_count = summary
        .file_count
        .checked_add(1)
        .ok_or_else(|| invalid_data("file count overflowed"))?;
    Ok(())
}

fn reject_non_empty_destination(destination: &DirectoryCapability) -> io::Result<()> {
    let mut entries = destination.directory.entries()?;
    if entries.next().transpose()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "directory snapshot destination must be empty",
        ));
    }
    Ok(())
}

fn reject_overlapping_roots(
    source: &DirectoryCapability,
    destination: &DirectoryCapability,
) -> io::Result<()> {
    if path_is_within_or_equal(&source.root, &destination.root)
        || path_is_within_or_equal(&destination.root, &source.root)
    {
        return Err(invalid_input("source and destination roots overlap"));
    }
    Ok(())
}

fn path_is_within_or_equal(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        #[cfg(windows)]
        let equal = path_component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&root_component.as_os_str().to_string_lossy());
        #[cfg(not(windows))]
        let equal = path_component == root_component;
        if !equal {
            return false;
        }
    }
    true
}

fn is_single_normal_component(name: &std::ffi::OsStr) -> bool {
    let mut components = Path::new(name).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn cancelled() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "directory snapshot was cancelled",
    )
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::{DirectoryTreeCopyLimits, DirectoryTreeCopySummary};
    use crate::filesystem::DirectoryCapability;
    use std::io;

    fn limits(
        entries: usize,
        depth: usize,
        file_bytes: u64,
        total_bytes: u64,
    ) -> DirectoryTreeCopyLimits {
        DirectoryTreeCopyLimits::new(entries, depth, file_bytes, total_bytes).unwrap()
    }

    #[test]
    fn copies_a_bounded_nested_tree_through_capabilities() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir_all(source_path.join("nested/deeper")).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        std::fs::write(source_path.join("root.txt"), b"root").unwrap();
        std::fs::write(source_path.join("nested/deeper/data.bin"), b"payload").unwrap();
        let source = DirectoryCapability::open(&source_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();

        let summary = source
            .copy_tree_to(&destination, limits(10, 4, 32, 64), || false)
            .unwrap();

        assert_eq!(
            summary,
            DirectoryTreeCopySummary {
                entry_count: 4,
                file_count: 2,
                directory_count: 2,
                byte_count: 11,
            }
        );
        assert_eq!(
            std::fs::read(destination_path.join("root.txt")).unwrap(),
            b"root"
        );
        assert_eq!(
            std::fs::read(destination_path.join("nested/deeper/data.bin")).unwrap(),
            b"payload"
        );
    }

    #[test]
    fn rejects_non_empty_and_overlapping_destinations() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        std::fs::write(destination_path.join("existing"), b"keep").unwrap();
        let source = DirectoryCapability::open(&source_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();
        assert_eq!(
            source
                .copy_tree_to(&destination, limits(10, 2, 32, 64), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );

        let nested_path = source_path.join("nested");
        std::fs::create_dir(&nested_path).unwrap();
        let nested = DirectoryCapability::open(&nested_path).unwrap();
        assert_eq!(
            source
                .copy_tree_to(&nested, limits(10, 2, 32, 64), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn enforces_entry_depth_per_file_and_total_byte_limits() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir_all(source_path.join("nested/deeper")).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        std::fs::write(source_path.join("nested/deeper/file"), b"12345").unwrap();
        let source = DirectoryCapability::open(&source_path).unwrap();

        let per_file = DirectoryCapability::open(&destination_path).unwrap();
        assert_eq!(
            source
                .copy_tree_to(&per_file, limits(10, 3, 4, 100), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        drop(per_file);
        let _ = std::fs::remove_dir_all(&destination_path);
        std::fs::create_dir(&destination_path).unwrap();

        let total = DirectoryCapability::open(&destination_path).unwrap();
        assert_eq!(
            source
                .copy_tree_to(&total, limits(10, 3, 8, 4), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        drop(total);
        let _ = std::fs::remove_dir_all(&destination_path);
        std::fs::create_dir(&destination_path).unwrap();

        let entries = DirectoryCapability::open(&destination_path).unwrap();
        assert_eq!(
            source
                .copy_tree_to(&entries, limits(2, 3, 8, 100), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn cancellation_discards_the_in_progress_file_but_reports_partial_tree() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        std::fs::write(source_path.join("large.bin"), vec![7_u8; 256 * 1024]).unwrap();
        let source = DirectoryCapability::open(&source_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();
        let mut checks = 0;

        assert_eq!(
            source
                .copy_tree_to(&destination, limits(10, 2, 512 * 1024, 512 * 1024), || {
                    checks += 1;
                    checks >= 4
                })
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!destination_path.join("large.bin").exists());
        assert_eq!(std::fs::read_dir(destination_path).unwrap().count(), 0);
    }

    #[test]
    fn rejects_reparse_entries_without_following_them() {
        let temp = tempfile::tempdir().unwrap();
        let source_path = temp.path().join("source");
        let destination_path = temp.path().join("destination");
        let outside_path = temp.path().join("outside");
        std::fs::create_dir(&source_path).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        std::fs::create_dir(&outside_path).unwrap();
        std::fs::write(outside_path.join("keep"), b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_path, source_path.join("link")).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&outside_path, source_path.join("link")).is_err() {
            return;
        }
        let source = DirectoryCapability::open(&source_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();

        assert_eq!(
            source
                .copy_tree_to(&destination, limits(10, 2, 32, 64), || false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            std::fs::read(outside_path.join("keep")).unwrap(),
            b"outside"
        );
    }

    #[test]
    fn validates_hard_limit_configuration() {
        assert!(DirectoryTreeCopyLimits::new(0, 1, 1, 1).is_err());
        assert!(DirectoryTreeCopyLimits::new(1, 65, 1, 1).is_err());
        assert!(DirectoryTreeCopyLimits::new(1, 1, 1, 4 * 1024 * 1024 * 1024 + 1).is_err());
    }

    #[test]
    fn destination_root_must_not_be_the_source_root() {
        let temp = tempfile::tempdir().unwrap();
        let source = DirectoryCapability::open(temp.path()).unwrap();
        let destination = DirectoryCapability::open(temp.path()).unwrap();
        assert!(
            source
                .copy_tree_to(&destination, limits(10, 2, 32, 64), || false)
                .is_err()
        );
    }
}
