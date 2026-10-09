//! Shared low-level filesystem metadata checks.

use std::fs::Metadata;

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
