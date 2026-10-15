//! Shared low-level filesystem metadata checks.

mod tree_copy;
mod tree_diff;
pub use tree_copy::{DirectoryTreeCopyLimits, DirectoryTreeCopySummary};
pub use tree_diff::{
    DirectoryTreeChange, DirectoryTreeChangeKind, DirectoryTreeDiffLimits, DirectoryTreeEntryKind,
    DirectoryTreeFileCopySummary, DirectoryTreeOutputManifest,
};

use std::ffi::OsString;
use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryEntryKind {
    Directory,
    File,
    ReparsePoint,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntryInfo {
    pub name: OsString,
    pub kind: DirectoryEntryKind,
    pub modified: Option<SystemTime>,
}

/// A directory capability opened once from a trusted policy root.
///
/// Filesystem operations use paths relative to this held directory handle.
/// This keeps a later rename or reparse-point substitution from redirecting a
/// mutation to a different directory tree. The absolute root is metadata for
/// authorization matching; it is never used as the mutation handle.
#[derive(Clone, Debug)]
pub struct DirectoryCapability {
    root: PathBuf,
    directory: Arc<cap_std::fs::Dir>,
}

impl DirectoryCapability {
    /// Open an existing local directory without following a reparse point at
    /// any component. The returned handle pins the directory object.
    pub fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || is_unc_or_device_path(path) {
            return Err(permission_denied(
                "directory root must be a local absolute path",
            ));
        }
        reject_reparse_components(path)?;

        #[cfg(windows)]
        let opened = open_directory_capability(path)?;
        #[cfg(not(windows))]
        let opened = open_for_metadata(path)?;
        let opened_metadata = opened.metadata()?;
        if !opened_metadata.is_dir() || is_link_or_reparse_point(&opened_metadata) {
            return Err(permission_denied(
                "directory root is not a regular directory",
            ));
        }

        let canonical = normalize_windows_extended_path(std::fs::canonicalize(path)?)
            .ok_or_else(|| permission_denied("directory root is not a local path"))?;
        let resolved = open_for_metadata(&canonical)?;
        if !same_file_identity(&opened, &resolved)? {
            return Err(permission_denied("directory root changed while opening"));
        }

        Ok(Self {
            root: canonical,
            directory: Arc::new(cap_std::fs::Dir::from_std_file(opened)),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Open an unrestricted directory capability from ambient process
    /// authority. Callers should use this only after FullAccess authorization.
    pub fn open_ambient(path: &Path) -> io::Result<Self> {
        let directory = cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())?;
        let root = std::fs::canonicalize(path)
            .ok()
            .and_then(normalize_windows_extended_path)
            .unwrap_or_else(|| path.to_path_buf());
        Ok(Self {
            root,
            directory: Arc::new(directory),
        })
    }

    /// For FullAccess writes, pin the nearest existing directory ancestor and
    /// return the remaining target as a relative path. Missing parent
    /// directories can then be created beneath the pinned handle.
    pub fn open_ambient_for_path(path: &Path) -> io::Result<(Self, PathBuf)> {
        if !path.is_absolute() {
            return Err(permission_denied("ambient target must be absolute"));
        }
        let mut ancestor = path
            .parent()
            .ok_or_else(|| permission_denied("ambient target has no parent"))?
            .to_path_buf();
        let mut suffix = vec![
            path.file_name()
                .ok_or_else(|| permission_denied("ambient target has no filename"))?
                .to_os_string(),
        ];
        loop {
            match std::fs::metadata(&ancestor) {
                Ok(metadata) if metadata.is_dir() => break,
                Ok(_) => {
                    return Err(permission_denied(
                        "ambient target parent is not a directory",
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let name = ancestor
                        .file_name()
                        .ok_or_else(|| permission_denied("no existing target ancestor"))?
                        .to_os_string();
                    suffix.push(name);
                    if !ancestor.pop() {
                        return Err(permission_denied("no existing target ancestor"));
                    }
                }
                Err(error) => return Err(error),
            }
        }
        suffix.reverse();
        let mut relative = PathBuf::new();
        for part in suffix {
            relative.push(part);
        }
        Ok((Self::open_ambient(&ancestor)?, relative))
    }

    /// Convert an absolute path beneath this capability root into a safe
    /// relative path. Paths with a prefix, root, or parent component fail.
    pub fn relative_path(&self, absolute: &Path) -> Option<PathBuf> {
        let absolute = normalize_windows_extended_path(absolute.to_path_buf())?;
        if !absolute.is_absolute() || is_unc_or_device_path(&absolute) {
            return None;
        }
        let root = path_components(&self.root);
        let path = path_components(&absolute);
        if path.len() <= root.len()
            || !root.iter().zip(path.iter()).all(|(left, right)| {
                #[cfg(windows)]
                {
                    left.as_os_str()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
                }
                #[cfg(not(windows))]
                {
                    left == right
                }
            })
        {
            return None;
        }
        let mut relative = PathBuf::new();
        for component in &path[root.len()..] {
            match component {
                Component::Normal(value) => relative.push(value),
                _ => return None,
            }
        }
        (!relative.as_os_str().is_empty()).then_some(relative)
    }

    pub fn open_read(&self, relative: &Path) -> io::Result<File> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, true)?;
        self.directory
            .open(&relative)
            .map(cap_std::fs::File::into_std)
    }

    pub fn open_read_write(&self, relative: &Path) -> io::Result<File> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, true)?;
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).write(true);
        self.directory
            .open_with(&relative, &options)
            .map(cap_std::fs::File::into_std)
    }

    /// Read a bounded snapshot of direct children using the held directory
    /// handle. An oversized directory fails closed instead of allocating an
    /// unbounded list.
    pub fn entries(&self, maximum: usize) -> io::Result<Vec<DirectoryEntryInfo>> {
        let mut result = Vec::new();
        for entry in self.directory.entries()? {
            let entry = entry?;
            if result.len() >= maximum {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "directory entry count exceeds its inspection limit",
                ));
            }
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
            result.push(DirectoryEntryInfo {
                name: entry.file_name(),
                kind,
                modified: metadata
                    .modified()
                    .ok()
                    .map(cap_std::time::SystemTime::into_std),
            });
        }
        Ok(result)
    }

    /// Atomically replace a file under this directory handle. Missing parent
    /// directories are created relative to the same handle. The destination's
    /// final component is replaced as a directory entry and is never followed.
    pub fn atomic_replace(&self, relative: &Path, bytes: &[u8]) -> io::Result<()> {
        self.atomic_replace_with(relative, |file| file.write_all(bytes))
    }

    fn atomic_replace_with(
        &self,
        relative: &Path,
        write_contents: impl FnOnce(&mut cap_std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        self.atomic_write_with(relative, true, true, write_contents)
    }

    /// Atomically create a new file below an already existing parent. Existing
    /// destinations are rejected, including a destination created after the
    /// initial check. This is used when promoting reviewed sandbox output so a
    /// concurrent edit is never overwritten.
    pub(crate) fn atomic_create_with_existing_parent(
        &self,
        relative: &Path,
        write_contents: impl FnOnce(&mut cap_std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        self.atomic_write_with(relative, false, false, write_contents)
    }

    fn atomic_write_with(
        &self,
        relative: &Path,
        create_parents: bool,
        replace_existing: bool,
        write_contents: impl FnOnce(&mut cap_std::fs::File) -> io::Result<()>,
    ) -> io::Result<()> {
        let relative = validate_relative_path(relative)?;
        let parent_path = relative_parent(&relative);
        reject_relative_reparse_components(&self.directory, parent_path, false)?;
        if create_parents && parent_path != Path::new(".") {
            self.directory.create_dir_all(parent_path)?;
        }
        let parent = if parent_path == Path::new(".") {
            self.directory.try_clone().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not clone the capability root: {error}"),
                )
            })?
        } else {
            self.directory.open_dir(parent_path).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not open the target parent: {error}"),
                )
            })?
        };
        reject_relative_reparse_components(&self.directory, parent_path, false)?;

        let filename = relative
            .file_name()
            .ok_or_else(|| permission_denied("file path must include a filename"))?;
        match parent.symlink_metadata(filename) {
            Ok(metadata) if is_cap_link_or_reparse_point(&metadata) || !metadata.is_file() => {
                return Err(permission_denied("destination is not a regular file"));
            }
            Ok(_) if !replace_existing => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination already exists",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let temporary = format!(".{}.tmp", haven_common::types::new_id("file"));
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{DELETE, FILE_GENERIC_WRITE};

            // SetFileInformationByHandle needs DELETE access on the staging
            // file so replacement stays relative to a verified parent handle.
            options.access_mode(FILE_GENERIC_WRITE | DELETE);
        }
        let result = (|| {
            let mut file = parent.open_with(&temporary, &options).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not create the staging file: {error}"),
                )
            })?;
            write_contents(&mut file).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not write the staging file: {error}"),
                )
            })?;
            file.flush().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not flush the staging file: {error}"),
                )
            })?;
            file.sync_all().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("could not sync the staging file: {error}"),
                )
            })?;
            #[cfg(windows)]
            {
                let file = file.into_std();
                rename_file_by_handle(
                    &file,
                    &self.root,
                    parent_path,
                    &parent,
                    filename,
                    replace_existing,
                )
                .map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!("could not commit the staged file replacement: {error}"),
                    )
                })
            }
            #[cfg(not(windows))]
            {
                drop(file);
                if replace_existing {
                    parent.rename(&temporary, &parent, filename)
                } else {
                    // A same-directory hard link gives create-if-absent
                    // semantics on platforms where rename replaces its target.
                    parent.hard_link(&temporary, &parent, filename)?;
                    let _ = parent.remove_file(&temporary);
                    Ok(())
                }
            }
        })();
        if result.is_err() {
            let _ = parent.remove_file(&temporary);
        }
        result
    }

    pub fn create_dir_all(&self, relative: &Path) -> io::Result<()> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, false)?;
        self.directory.create_dir_all(relative)
    }

    pub fn copy_file_to(
        &self,
        source: &Path,
        destination_root: &DirectoryCapability,
        destination: &Path,
    ) -> io::Result<u64> {
        let mut source = self.open_read(source)?;
        let mut copied = 0u64;
        destination_root.atomic_replace_with(destination, |target| {
            copied = std::io::copy(&mut source, target)?;
            Ok(())
        })?;
        Ok(copied)
    }

    pub fn rename_to(
        &self,
        source: &Path,
        destination_root: &DirectoryCapability,
        destination: &Path,
    ) -> io::Result<()> {
        let source = validate_relative_path(source)?;
        let destination = validate_relative_path(destination)?;
        let source_parent_path = relative_parent(&source);
        let destination_parent_path = relative_parent(&destination);
        reject_relative_reparse_components(&self.directory, &source, true)?;
        reject_relative_reparse_components(
            &destination_root.directory,
            destination_parent_path,
            false,
        )?;
        if destination_parent_path != Path::new(".") {
            destination_root
                .directory
                .create_dir_all(destination_parent_path)?;
        }
        reject_relative_reparse_components(&self.directory, &source, true)?;
        reject_relative_reparse_components(
            &destination_root.directory,
            destination_parent_path,
            false,
        )?;
        let source_parent = if source_parent_path == Path::new(".") {
            self.directory.try_clone()?
        } else {
            self.directory.open_dir(source_parent_path)?
        };
        let destination_parent = if destination_parent_path == Path::new(".") {
            destination_root.directory.try_clone()?
        } else {
            destination_root
                .directory
                .open_dir(destination_parent_path)?
        };
        source_parent.rename(
            source.file_name().expect("validated source filename"),
            &destination_parent,
            destination
                .file_name()
                .expect("validated destination filename"),
        )
    }

    pub fn remove_file(&self, relative: &Path) -> io::Result<()> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, true)?;
        self.directory.remove_file(relative)
    }

    pub fn remove_empty_dir(&self, relative: &Path) -> io::Result<()> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, true)?;
        self.directory.remove_dir(relative)
    }

    /// Recursively remove a directory through this held root handle. The
    /// target path must stay relative to the capability; cap-std removes
    /// nested entries without following symlinks or reparse points.
    pub fn remove_dir_all(&self, relative: &Path) -> io::Result<()> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, true)?;
        self.directory.remove_dir_all(relative)
    }

    pub fn remove_file_or_empty_dir(&self, relative: &Path) -> io::Result<&'static str> {
        let relative = validate_relative_path(relative)?;
        reject_relative_reparse_components(&self.directory, &relative, true)?;
        let metadata = self.directory.symlink_metadata(&relative)?;
        if metadata.is_dir() {
            self.directory.remove_dir(relative)?;
            Ok("directory")
        } else if metadata.is_file() {
            self.directory.remove_file(relative)?;
            Ok("file")
        } else {
            Err(permission_denied(
                "target is not a regular file or directory",
            ))
        }
    }
}

fn permission_denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn is_cap_link_or_reparse_point(metadata: &cap_std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

#[cfg(windows)]
fn open_directory_capability(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
        FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
    };

    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE);
    options.open(path)
}

#[cfg(windows)]
fn rename_file_by_handle(
    staged_file: &File,
    root: &Path,
    parent_path: &Path,
    capability_parent: &cap_std::fs::Dir,
    destination_name: &std::ffi::OsStr,
    replace_existing: bool,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ADD_FILE, FILE_DELETE_CHILD, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_RENAME_INFO_0,
        FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
    };

    let parent_path = if parent_path == Path::new(".") {
        root.to_path_buf()
    } else {
        root.join(parent_path)
    };
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .access_mode(
            FILE_ADD_FILE
                | FILE_DELETE_CHILD
                | FILE_LIST_DIRECTORY
                | FILE_READ_ATTRIBUTES
                | SYNCHRONIZE,
        );
    let parent_handle = options.open(&parent_path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not open the verified replacement parent: {error}"),
        )
    })?;
    let capability_parent_handle = capability_parent.try_clone()?.into_std_file();
    if !same_file_identity(&parent_handle, &capability_parent_handle).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not verify the replacement parent identity: {error}"),
        )
    })? {
        return Err(permission_denied(
            "destination parent changed while opening its directory handle",
        ));
    }

    let name: Vec<u16> = destination_name.encode_wide().collect();
    let name_bytes = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|bytes| u32::try_from(bytes).ok())
        .ok_or_else(|| permission_denied("destination filename is too long"))?;
    let name_offset = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let buffer_len = name_offset
        .checked_add(name_bytes as usize)
        .ok_or_else(|| permission_denied("destination filename is too long"))?;
    let word_count = buffer_len.div_ceil(std::mem::size_of::<u64>());
    let mut buffer = vec![0u64; word_count];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    let buffer_bytes = buffer.as_mut_ptr().cast::<u8>();
    let root_directory = parent_handle.as_raw_handle();

    // The variable-sized FILE_RENAME_INFO buffer is aligned by Vec<u64> and
    // sized through the trailing UTF-16 filename. The Win32 wrapper rejects
    // non-null RootDirectory handles on current Windows, so use the native
    // handle-relative entry point after comparing and pinning the parent.
    let status = unsafe {
        (*info).Anonymous = FILE_RENAME_INFO_0 {
            ReplaceIfExists: replace_existing,
        };
        (*info).RootDirectory = root_directory;
        (*info).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            buffer_bytes.add(name_offset).cast::<u16>(),
            name.len(),
        );
        let mut io_status_block = [0usize; 2];
        nt_set_information_file(
            staged_file.as_raw_handle(),
            io_status_block.as_mut_ptr().cast(),
            info.cast(),
            u32::try_from(buffer_len)
                .map_err(|_| permission_denied("destination filename is too long"))?,
            10, // FileRenameInformation
        )
    };
    if status < 0 {
        let code = unsafe { rtl_nt_status_to_dos_error(status) };
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    Ok(())
}

#[cfg(windows)]
#[link(name = "ntdll")]
unsafe extern "system" {
    #[link_name = "NtSetInformationFile"]
    fn nt_set_information_file(
        file: windows_sys::Win32::Foundation::HANDLE,
        io_status_block: *mut std::ffi::c_void,
        file_information: *const std::ffi::c_void,
        length: u32,
        file_information_class: i32,
    ) -> i32;

    #[link_name = "RtlNtStatusToDosError"]
    fn rtl_nt_status_to_dos_error(status: i32) -> u32;
}

fn path_components(path: &Path) -> Vec<Component<'_>> {
    path.components().collect()
}

fn relative_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn validate_relative_path(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() || is_unc_or_device_path(path) {
        return Err(permission_denied("capability path must be relative"));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(permission_denied("capability path escapes its root"));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(permission_denied("capability path is empty"));
    }
    Ok(normalized)
}

fn reject_reparse_components(path: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for (index, component) in path.components().enumerate() {
        current.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&current)?;
        if is_link_or_reparse_point(&metadata) {
            return Err(permission_denied("directory path contains a reparse point"));
        }
        if index + 1 < path.components().count() && !metadata.is_dir() {
            return Err(permission_denied("directory path contains a non-directory"));
        }
    }
    Ok(())
}

fn reject_relative_reparse_components(
    directory: &cap_std::fs::Dir,
    path: &Path,
    include_leaf: bool,
) -> io::Result<()> {
    let components: Vec<_> = path.components().collect();
    let count = if include_leaf {
        components.len()
    } else {
        components.len().saturating_sub(1)
    };
    let mut current = PathBuf::new();
    for (index, component) in components.iter().take(count).enumerate() {
        let Component::Normal(value) = component else {
            continue;
        };
        current.push(value);
        match directory.symlink_metadata(&current) {
            Ok(metadata) => {
                if is_cap_link_or_reparse_point(&metadata) {
                    return Err(permission_denied(
                        "capability path contains a reparse point",
                    ));
                }
                if index + 1 < count && !metadata.is_dir() {
                    return Err(permission_denied(
                        "capability path contains a non-directory",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn is_unc_or_device_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    value.starts_with("\\\\") || value.starts_with("//")
}

#[cfg(windows)]
fn normalize_windows_extended_path(path: PathBuf) -> Option<PathBuf> {
    let value = path.to_str()?;
    if let Some(rest) = value.strip_prefix("\\\\?\\") {
        if rest.starts_with("UNC\\") || rest.starts_with("unc\\") {
            return None;
        }
        Some(PathBuf::from(rest))
    } else {
        Some(path)
    }
}

#[cfg(not(windows))]
fn normalize_windows_extended_path(path: PathBuf) -> Option<PathBuf> {
    Some(path)
}

#[cfg(test)]
mod directory_capability_tests {
    use super::{DirectoryCapability, DirectoryEntryKind};
    use std::path::Path;

    fn symlink_directory(target: &Path, link: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link)
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(target, link)
        }
    }

    #[test]
    fn atomic_replace_creates_parents_beneath_the_open_directory_handle() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let capability = DirectoryCapability::open(&root).unwrap();

        let relative = Path::new("nested/notes.md");
        assert_eq!(
            capability.relative_path(&root.join(relative)).as_deref(),
            Some(relative)
        );
        capability
            .atomic_replace(relative, b"through directory handle")
            .unwrap();
        assert_eq!(
            std::fs::read(root.join(relative)).unwrap(),
            b"through directory handle"
        );
    }

    #[test]
    fn atomic_replace_replaces_an_existing_regular_file() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("notes.md");
        std::fs::write(&target, b"old content").unwrap();
        let capability = DirectoryCapability::open(temp.path()).unwrap();

        capability
            .atomic_replace(Path::new("notes.md"), b"new content")
            .unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"new content");
    }

    #[test]
    fn recursive_capability_cleanup_does_not_follow_nested_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("scratch-root");
        let scratch = root.join("owned-run");
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&scratch).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"outside").unwrap();
        if symlink_directory(&outside, &scratch.join("link")).is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }
        let capability = DirectoryCapability::open(&root).unwrap();

        capability.remove_dir_all(Path::new("owned-run")).unwrap();

        assert!(!scratch.exists());
        assert_eq!(std::fs::read(outside.join("keep.txt")).unwrap(), b"outside");
    }

    #[test]
    fn capability_directory_listing_is_bounded_and_reports_entry_kinds() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("note.txt"), b"note").unwrap();
        std::fs::create_dir(temp.path().join("folder")).unwrap();
        let capability = DirectoryCapability::open(temp.path()).unwrap();

        let entries = capability.entries(2).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(
            entries.iter().any(|entry| {
                entry.name == "note.txt" && entry.kind == DirectoryEntryKind::File
            })
        );
        assert!(entries.iter().any(|entry| {
            entry.name == "folder" && entry.kind == DirectoryEntryKind::Directory
        }));
        assert_eq!(
            capability.entries(1).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn capability_rejects_parent_and_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();
        let capability = DirectoryCapability::open(temp.path()).unwrap();

        assert!(
            capability
                .atomic_replace(Path::new("../outside.txt"), b"blocked")
                .is_err()
        );
        assert!(
            capability
                .atomic_replace(&temp.path().join("outside.txt"), b"blocked")
                .is_err()
        );
        assert!(!temp.path().join("outside.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn capability_rejects_symlink_escape() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let capability = DirectoryCapability::open(&root).unwrap();

        assert!(
            capability
                .atomic_replace(Path::new("link/escape.txt"), b"blocked")
                .is_err()
        );
        assert!(!outside.join("escape.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn open_directory_handle_stays_bound_after_its_path_is_renamed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let moved = temp.path().join("renamed-workspace");
        std::fs::create_dir(&root).unwrap();
        let capability = DirectoryCapability::open(&root).unwrap();
        std::fs::rename(&root, &moved).unwrap();
        std::fs::create_dir(&root).unwrap();

        capability
            .atomic_replace(Path::new("notes.md"), b"pinned")
            .unwrap();
        assert_eq!(std::fs::read(moved.join("notes.md")).unwrap(), b"pinned");
        assert!(!root.join("notes.md").exists());
    }

    #[cfg(windows)]
    #[test]
    fn open_directory_capability_blocks_root_rename_until_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let moved = temp.path().join("renamed-workspace");
        std::fs::create_dir(&root).unwrap();
        let capability = DirectoryCapability::open(&root).unwrap();

        assert!(std::fs::rename(&root, &moved).is_err());
        capability
            .atomic_replace(Path::new("notes.md"), b"pinned")
            .unwrap();
        assert_eq!(std::fs::read(root.join("notes.md")).unwrap(), b"pinned");

        drop(capability);
        std::fs::rename(&root, &moved).unwrap();
        assert_eq!(std::fs::read(moved.join("notes.md")).unwrap(), b"pinned");
    }
}

/// Whether metadata describes a symbolic link or a Windows reparse point.
///
/// This only inspects one metadata value. Callers retain ownership of path
/// traversal, canonicalization, root policy, and failure handling.
pub fn is_link_or_reparse_point(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Open a file for reading. On Windows, deny new write and delete handles for
/// the lifetime of the returned handle so an in-place rewrite or rename cannot
/// change the bytes being snapshotted.
pub fn open_read_only(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        // Refuse a reparse/symlink at the final path component. Ancestor
        // components are checked by the caller and bound to this handle by
        // comparing it with the resolved target.
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // FILE_SHARE_READ allows other readers while excluding write/delete.
        // OPEN_REPARSE_POINT keeps a final reparse point from being followed.
        options.share_mode(0x0000_0001);
        options.custom_flags(0x0020_0000);
    }
    options.open(path)
}

/// Open a file or directory handle for identity and metadata checks. Directory
/// handles require `FILE_FLAG_BACKUP_SEMANTICS` on Windows.
pub fn open_for_metadata(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;

        // FILE_SHARE_READ prevents concurrent replacement while validating;
        // BACKUP_SEMANTICS permits opening directory handles, and
        // OPEN_REPARSE_POINT lets the caller reject reparse points directly.
        options.share_mode(0x0000_0001);
        options.custom_flags(0x0200_0000 | 0x0020_0000);
    }
    options.open(path)
}

/// Stable identity for a file object while at least one handle remains open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    volume: u64,
    index: u128,
}

/// Read a stable object identity from an already-open file handle.
/// Platforms without a supported identity API fail closed.
pub fn file_identity(file: &File) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let metadata = file.metadata()?;
        Ok(FileIdentity {
            volume: metadata.dev(),
            index: u128::from(metadata.ino()),
        })
    }
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        // SAFETY: `file` owns a valid handle for the duration of the call,
        // and `information` points to writable storage of the API's type.
        let succeeded =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the API returned success and initialized the structure.
        let information = unsafe { information.assume_init() };
        let index =
            (u128::from(information.nFileIndexHigh) << 32) | u128::from(information.nFileIndexLow);
        Ok(FileIdentity {
            volume: u64::from(information.dwVolumeSerialNumber),
            index,
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "stable file identity is unavailable on this platform",
        ))
    }
}

/// Whether two already-open file handles identify the same filesystem object.
///
/// Skills uses this after opening an untrusted path and resolving its current
/// target. Comparing handles closes the gap where the path changes between
/// canonicalization and a later read. Platforms without stable file identity
/// support fail closed.
pub fn same_file_identity(left: &File, right: &File) -> io::Result<bool> {
    Ok(file_identity(left)? == file_identity(right)?)
}
