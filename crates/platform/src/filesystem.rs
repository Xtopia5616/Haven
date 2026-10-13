//! Shared low-level filesystem metadata checks.

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::Path;

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
