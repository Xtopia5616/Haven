//! Bounded, non-reparse directory accounting shared by process adapters.

use super::DirectoryCapability;
use std::path::Path;

pub fn directory_size_exceeds(root: &Path, limit: u64, max_entries: usize) -> Result<bool, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if super::is_link_or_reparse_point(&root_metadata) || !root_metadata.is_dir() {
        return Err("runtime directory is not a safe local directory".into());
    }
    DirectoryCapability::open(root)
        .map_err(|_| "runtime directory could not be safely opened".to_owned())?;

    let mut pending = vec![root.to_path_buf()];
    let mut total_bytes = 0u64;
    let mut entry_count = 0usize;
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                return Err("runtime directory could not be inspected safely".into());
            }
        };
        for entry in entries {
            let entry = entry.map_err(|_| "runtime directory could not be inspected safely")?;
            entry_count = entry_count.saturating_add(1);
            if entry_count > max_entries {
                return Ok(true);
            }
            let metadata = match std::fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    return Err("runtime directory could not be inspected safely".into());
                }
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() || super::is_link_or_reparse_point(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                total_bytes = total_bytes.saturating_add(metadata.len());
                if total_bytes > limit {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
