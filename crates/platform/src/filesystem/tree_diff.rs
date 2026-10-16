//! Bounded comparisons between a sandbox output tree and its private baseline.

use super::tree_copy::{
    DirectoryTreeCopyLimits, cancelled, invalid_data, invalid_input, is_single_normal_component,
    reject_overlapping_roots,
};
use super::{
    DirectoryCapability, DirectoryEntryKind, is_cap_link_or_reparse_point,
    reject_relative_reparse_components,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const DIFF_BUFFER_BYTES: usize = 64 * 1024;
const MAX_TREE_DIFF_CHANGES: usize = 4_096;
const MAX_TREE_DIFF_CHANGED_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TREE_DIFF_CHANGED_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_TREE_DIFF_PATH_BYTES: usize = 4 * 1024 * 1024;

/// Per-file and whole-manifest bounds for comparing a sandbox output tree to
/// its private input baseline. Scan limits are independently bounded by the
/// same hard limits used for directory snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryTreeDiffLimits {
    scan: DirectoryTreeCopyLimits,
    max_changes: usize,
    max_changed_file_bytes: u64,
    max_changed_total_bytes: u64,
}

impl DirectoryTreeDiffLimits {
    pub(crate) fn scan(&self) -> DirectoryTreeCopyLimits {
        self.scan
    }

    pub(crate) fn max_changes(&self) -> usize {
        self.max_changes
    }

    pub(crate) fn max_changed_file_bytes(&self) -> u64 {
        self.max_changed_file_bytes
    }

    pub(crate) fn max_changed_total_bytes(&self) -> u64 {
        self.max_changed_total_bytes
    }

    pub fn new(
        scan: DirectoryTreeCopyLimits,
        max_changes: usize,
        max_changed_file_bytes: u64,
        max_changed_total_bytes: u64,
    ) -> io::Result<Self> {
        if max_changes == 0
            || max_changes > MAX_TREE_DIFF_CHANGES
            || max_changed_file_bytes == 0
            || max_changed_file_bytes > MAX_TREE_DIFF_CHANGED_FILE_BYTES
            || max_changed_total_bytes == 0
            || max_changed_total_bytes > MAX_TREE_DIFF_CHANGED_TOTAL_BYTES
            || max_changed_file_bytes > max_changed_total_bytes
        {
            return Err(invalid_input(
                "directory output limits are outside the supported range",
            ));
        }
        Ok(Self {
            scan,
            max_changes,
            max_changed_file_bytes,
            max_changed_total_bytes,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryTreeEntryKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryTreeChangeKind {
    Created,
    Modified,
    Deleted,
    TypeChanged,
}

/// One relative path changed between the input baseline and sandbox output.
/// Digests identify the exact file bytes observed during the bounded scan;
/// they are not an authorization token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryTreeChange {
    pub relative_path: PathBuf,
    pub change: DirectoryTreeChangeKind,
    pub before_kind: Option<DirectoryTreeEntryKind>,
    pub after_kind: Option<DirectoryTreeEntryKind>,
    pub before_bytes: Option<u64>,
    pub after_bytes: Option<u64>,
    pub before_sha256: Option<[u8; 32]>,
    pub after_sha256: Option<[u8; 32]>,
}

/// Bounded file and directory changes between two capability roots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectoryTreeOutputManifest {
    pub changes: Vec<DirectoryTreeChange>,
    pub compared_entry_count: usize,
    pub changed_bytes: u64,
}

/// Summary of one verified sandbox file atomically created at a destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryTreeFileCopySummary {
    pub bytes: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScannedTreeEntry {
    kind: DirectoryTreeEntryKind,
    bytes: Option<u64>,
    sha256: Option<[u8; 32]>,
}

impl DirectoryCapability {
    /// Compare this output directory with its immutable input baseline.
    /// Symlinks/reparse points, special files, invalid UTF-8 paths on Unix,
    /// cancellation, or any resource limit fail the entire manifest.
    pub fn output_manifest(
        &self,
        baseline: &DirectoryCapability,
        limits: DirectoryTreeDiffLimits,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeOutputManifest> {
        reject_overlapping_roots(self, baseline)?;
        let mut output_entries = BTreeMap::new();
        let mut baseline_entries = BTreeMap::new();
        let mut output_count = 0usize;
        let mut baseline_count = 0usize;
        let mut output_path_bytes = 0usize;
        let mut baseline_path_bytes = 0usize;
        let mut output_scan_bytes = 0u64;
        let mut baseline_scan_bytes = 0u64;
        scan_tree(
            self,
            &PathBuf::new(),
            0,
            limits.scan,
            &mut output_entries,
            &mut output_count,
            &mut output_path_bytes,
            &mut output_scan_bytes,
            &mut is_cancelled,
        )?;
        scan_tree(
            baseline,
            &PathBuf::new(),
            0,
            limits.scan,
            &mut baseline_entries,
            &mut baseline_count,
            &mut baseline_path_bytes,
            &mut baseline_scan_bytes,
            &mut is_cancelled,
        )?;
        reject_case_collisions(&output_entries)?;
        reject_case_collisions(&baseline_entries)?;

        let paths = output_entries
            .keys()
            .chain(baseline_entries.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut manifest = DirectoryTreeOutputManifest {
            compared_entry_count: paths.len(),
            ..DirectoryTreeOutputManifest::default()
        };
        for relative_path in paths {
            if is_cancelled() {
                return Err(cancelled());
            }
            let before = baseline_entries.get(&relative_path).copied();
            let after = output_entries.get(&relative_path).copied();
            let change = match (before, after) {
                (None, Some(after)) => Some((DirectoryTreeChangeKind::Created, None, Some(after))),
                (Some(before), None) => {
                    Some((DirectoryTreeChangeKind::Deleted, Some(before), None))
                }
                (Some(before), Some(after)) if before.kind != after.kind => Some((
                    DirectoryTreeChangeKind::TypeChanged,
                    Some(before),
                    Some(after),
                )),
                (Some(before), Some(after))
                    if before.kind == DirectoryTreeEntryKind::File
                        && before.sha256 != after.sha256 =>
                {
                    Some((DirectoryTreeChangeKind::Modified, Some(before), Some(after)))
                }
                _ => None,
            };
            let Some((change, before, after)) = change else {
                continue;
            };
            if manifest.changes.len() >= limits.max_changes {
                return Err(invalid_data(
                    "directory output manifest exceeds its change limit",
                ));
            }
            let before_bytes = before.and_then(|entry| entry.bytes);
            let after_bytes = after.and_then(|entry| entry.bytes);
            let changed_bytes = before_bytes.unwrap_or(0).max(after_bytes.unwrap_or(0));
            if changed_bytes > limits.max_changed_file_bytes {
                return Err(invalid_data(
                    "changed file exceeds the directory output file limit",
                ));
            }
            manifest.changed_bytes = manifest
                .changed_bytes
                .checked_add(changed_bytes)
                .ok_or_else(|| invalid_data("directory output byte count overflowed"))?;
            if manifest.changed_bytes > limits.max_changed_total_bytes {
                return Err(invalid_data(
                    "directory output manifest exceeds its changed-byte limit",
                ));
            }
            manifest.changes.push(DirectoryTreeChange {
                relative_path,
                change,
                before_kind: before.map(|entry| entry.kind),
                after_kind: after.map(|entry| entry.kind),
                before_bytes,
                after_bytes,
                before_sha256: before.and_then(|entry| entry.sha256),
                after_sha256: after.and_then(|entry| entry.sha256),
            });
        }
        Ok(manifest)
    }

    /// Copy one manifest-created regular file to a new destination after
    /// rechecking its exact size and digest while streaming. The destination
    /// must not exist and its parent must already exist. This is a bounded
    /// storage primitive; callers still own user review and authorization.
    pub(crate) fn copy_reviewed_created_file_to(
        &self,
        change: &DirectoryTreeChange,
        destination_root: &DirectoryCapability,
        destination: &Path,
        mut is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeFileCopySummary> {
        if change.change != DirectoryTreeChangeKind::Created
            || change.before_kind.is_some()
            || change.before_bytes.is_some()
            || change.before_sha256.is_some()
            || change.after_kind != Some(DirectoryTreeEntryKind::File)
        {
            return Err(invalid_input(
                "sandbox promotion accepts only newly created regular files",
            ));
        }
        let expected_bytes = change
            .after_bytes
            .ok_or_else(|| invalid_data("created file manifest is missing its byte count"))?;
        let expected_sha256 = change
            .after_sha256
            .ok_or_else(|| invalid_data("created file manifest is missing its digest"))?;
        if expected_bytes > MAX_TREE_DIFF_CHANGED_FILE_BYTES {
            return Err(invalid_data(
                "sandbox promotion exceeds the single-file size limit",
            ));
        }
        reject_overlapping_roots(self, destination_root)?;

        let mut source = self.open_read(&change.relative_path)?;
        let before = source.metadata()?;
        if !before.is_file() || before.len() != expected_bytes {
            return Err(invalid_data(
                "sandbox output no longer matches its reviewed file size",
            ));
        }
        let before_modified = before.modified().ok();
        let mut bytes = 0_u64;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; DIFF_BUFFER_BYTES];
        destination_root.atomic_create_with_existing_parent(destination, |target| {
            loop {
                if is_cancelled() {
                    return Err(cancelled());
                }
                let read = source.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                bytes = bytes
                    .checked_add(read as u64)
                    .ok_or_else(|| invalid_data("sandbox output file size overflowed"))?;
                if bytes > expected_bytes || bytes > MAX_TREE_DIFF_CHANGED_FILE_BYTES {
                    return Err(invalid_data(
                        "sandbox output grew beyond its reviewed file size",
                    ));
                }
                hasher.update(&buffer[..read]);
                target.write_all(&buffer[..read])?;
            }
            if is_cancelled() {
                return Err(cancelled());
            }
            let after = source.metadata()?;
            let modified_during_copy = before_modified
                .zip(after.modified().ok())
                .is_some_and(|(before, after)| before != after);
            let sha256: [u8; 32] = hasher.clone().finalize().into();
            if bytes != expected_bytes
                || bytes != after.len()
                || modified_during_copy
                || sha256 != expected_sha256
            {
                return Err(invalid_data(
                    "sandbox output changed after its manifest was reviewed",
                ));
            }
            Ok(())
        })?;

        Ok(DirectoryTreeFileCopySummary {
            bytes,
            sha256: hasher.finalize().into(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn scan_tree(
    root: &DirectoryCapability,
    relative_directory: &Path,
    depth: usize,
    limits: DirectoryTreeCopyLimits,
    entries: &mut BTreeMap<PathBuf, ScannedTreeEntry>,
    entry_count: &mut usize,
    path_bytes: &mut usize,
    total_bytes: &mut u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    if is_cancelled() {
        return Err(cancelled());
    }
    let directory = if relative_directory.as_os_str().is_empty() {
        root.directory.try_clone()?
    } else {
        reject_relative_reparse_components(&root.directory, relative_directory, true)?;
        root.directory.open_dir(relative_directory)?
    };
    for entry in directory.entries()? {
        if is_cancelled() {
            return Err(cancelled());
        }
        let entry = entry?;
        let name = entry.file_name();
        if !is_single_normal_component(&name) {
            return Err(invalid_data("directory contains an invalid entry name"));
        }
        *entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("directory entry count overflowed"))?;
        if *entry_count > limits.max_entries() {
            return Err(invalid_data(
                "directory output scan exceeds its entry limit",
            ));
        }
        let mut relative_path = relative_directory.to_path_buf();
        relative_path.push(&name);
        let Some(path_text) = relative_path.to_str() else {
            return Err(invalid_data(
                "directory output path cannot be represented as Unicode",
            ));
        };
        *path_bytes = path_bytes
            .checked_add(path_text.len())
            .ok_or_else(|| invalid_data("directory output path budget overflowed"))?;
        if *path_bytes > MAX_TREE_DIFF_PATH_BYTES {
            return Err(invalid_data(
                "directory output manifest exceeds its path budget",
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
        match kind {
            DirectoryEntryKind::Directory => {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("directory depth overflowed"))?;
                if child_depth > limits.max_depth() {
                    return Err(invalid_data(
                        "directory output scan exceeds its depth limit",
                    ));
                }
                reject_relative_reparse_components(&root.directory, &relative_path, true)?;
                entries.insert(
                    relative_path.clone(),
                    ScannedTreeEntry {
                        kind: DirectoryTreeEntryKind::Directory,
                        bytes: None,
                        sha256: None,
                    },
                );
                scan_tree(
                    root,
                    &relative_path,
                    child_depth,
                    limits,
                    entries,
                    entry_count,
                    path_bytes,
                    total_bytes,
                    is_cancelled,
                )?;
            }
            DirectoryEntryKind::File => {
                reject_relative_reparse_components(&root.directory, &relative_path, true)?;
                let (bytes, sha256) =
                    hash_file(root, &relative_path, limits.max_file_bytes(), is_cancelled)?;
                *total_bytes = total_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| invalid_data("directory scan byte count overflowed"))?;
                if *total_bytes > limits.max_total_bytes() {
                    return Err(invalid_data("directory output scan exceeds its byte limit"));
                }
                entries.insert(
                    relative_path,
                    ScannedTreeEntry {
                        kind: DirectoryTreeEntryKind::File,
                        bytes: Some(bytes),
                        sha256: Some(sha256),
                    },
                );
            }
            DirectoryEntryKind::ReparsePoint => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "directory output scan contains a reparse point",
                ));
            }
            DirectoryEntryKind::Other => {
                return Err(invalid_data(
                    "directory output scan contains a non-regular entry",
                ));
            }
        }
    }
    Ok(())
}

fn hash_file(
    root: &DirectoryCapability,
    relative_path: &Path,
    max_bytes: u64,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<(u64, [u8; 32])> {
    let mut source = root.open_read(relative_path)?;
    let before = source.metadata()?;
    if !before.is_file() || before.len() > max_bytes {
        return Err(invalid_data("directory output file exceeds its scan limit"));
    }
    let before_modified = before.modified().ok();
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; DIFF_BUFFER_BYTES];
    let mut bytes = 0_u64;
    loop {
        if is_cancelled() {
            return Err(cancelled());
        }
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or_else(|| invalid_data("directory output file size overflowed"))?;
        if bytes > max_bytes {
            return Err(invalid_data("directory output file exceeds its scan limit"));
        }
        hasher.update(&buffer[..read]);
    }
    let after = source.metadata()?;
    let modified_during_scan = before_modified
        .zip(after.modified().ok())
        .is_some_and(|(before, after)| before != after);
    if bytes != before.len() || bytes != after.len() || modified_during_scan {
        return Err(invalid_data(
            "directory output file changed during manifest scan",
        ));
    }
    Ok((bytes, hasher.finalize().into()))
}

fn reject_case_collisions(entries: &BTreeMap<PathBuf, ScannedTreeEntry>) -> io::Result<()> {
    let mut seen = BTreeMap::<String, &Path>::new();
    for path in entries.keys() {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if seen.insert(key, path).is_some() {
            return Err(invalid_data(
                "directory output manifest contains case-colliding paths",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DirectoryTreeChangeKind, DirectoryTreeDiffLimits};
    use crate::filesystem::DirectoryCapability;
    use crate::filesystem::tree_copy::DirectoryTreeCopyLimits;
    use std::collections::BTreeMap;
    use std::io;
    use std::path::Path;

    fn limits(
        entries: usize,
        depth: usize,
        file_bytes: u64,
        total_bytes: u64,
    ) -> DirectoryTreeCopyLimits {
        DirectoryTreeCopyLimits::new(entries, depth, file_bytes, total_bytes).unwrap()
    }

    fn diff_limits(
        scan: DirectoryTreeCopyLimits,
        max_changes: usize,
        max_changed_file_bytes: u64,
        max_changed_total_bytes: u64,
    ) -> DirectoryTreeDiffLimits {
        DirectoryTreeDiffLimits::new(
            scan,
            max_changes,
            max_changed_file_bytes,
            max_changed_total_bytes,
        )
        .unwrap()
    }

    #[test]
    fn output_manifest_reports_file_and_directory_changes_with_digests() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline");
        let output_path = temp.path().join("output");
        std::fs::create_dir_all(baseline_path.join("removed")).unwrap();
        std::fs::create_dir(&output_path).unwrap();
        std::fs::write(baseline_path.join("modified.txt"), b"before").unwrap();
        std::fs::write(baseline_path.join("removed.txt"), b"remove me").unwrap();
        std::fs::write(baseline_path.join("removed/child.txt"), b"child").unwrap();
        std::fs::write(baseline_path.join("unchanged.txt"), b"same").unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();
        let output = DirectoryCapability::open(&output_path).unwrap();
        baseline
            .copy_tree_to(&output, limits(32, 8, 128, 512), || false)
            .unwrap();

        std::fs::write(output_path.join("modified.txt"), b"after").unwrap();
        std::fs::remove_file(output_path.join("removed.txt")).unwrap();
        std::fs::remove_file(output_path.join("removed/child.txt")).unwrap();
        std::fs::remove_dir(output_path.join("removed")).unwrap();
        std::fs::create_dir_all(output_path.join("added/nested")).unwrap();
        std::fs::write(output_path.join("added/nested/new.txt"), b"new").unwrap();

        let manifest = output
            .output_manifest(
                &baseline,
                diff_limits(limits(32, 8, 128, 512), 16, 64, 128),
                || false,
            )
            .unwrap();
        let by_path = manifest
            .changes
            .iter()
            .map(|change| {
                (
                    change.relative_path.to_string_lossy().replace('\\', "/"),
                    change,
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(manifest.compared_entry_count, 8);
        assert_eq!(by_path.len(), 7);
        assert_eq!(
            by_path["modified.txt"].change,
            DirectoryTreeChangeKind::Modified
        );
        assert_eq!(by_path["modified.txt"].before_bytes, Some(6));
        assert_eq!(by_path["modified.txt"].after_bytes, Some(5));
        assert_ne!(
            by_path["modified.txt"].before_sha256,
            by_path["modified.txt"].after_sha256
        );
        assert_eq!(
            by_path["removed.txt"].change,
            DirectoryTreeChangeKind::Deleted
        );
        assert_eq!(
            by_path["removed/child.txt"].change,
            DirectoryTreeChangeKind::Deleted
        );
        assert_eq!(by_path["removed"].change, DirectoryTreeChangeKind::Deleted);
        assert_eq!(by_path["added"].change, DirectoryTreeChangeKind::Created);
        assert_eq!(
            by_path["added/nested"].change,
            DirectoryTreeChangeKind::Created
        );
        assert_eq!(
            by_path["added/nested/new.txt"].change,
            DirectoryTreeChangeKind::Created
        );
        assert_eq!(manifest.changed_bytes, 23);
        assert!(!by_path.contains_key("unchanged.txt"));
    }

    #[test]
    fn output_manifest_fails_closed_on_change_count_and_byte_limits() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline");
        let output_path = temp.path().join("output");
        std::fs::create_dir(&baseline_path).unwrap();
        std::fs::create_dir(&output_path).unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();
        let output = DirectoryCapability::open(&output_path).unwrap();
        std::fs::write(output_path.join("one"), b"1234").unwrap();
        std::fs::write(output_path.join("two"), b"5678").unwrap();

        assert!(
            output
                .output_manifest(
                    &baseline,
                    diff_limits(limits(8, 2, 32, 64), 1, 16, 32),
                    || false,
                )
                .is_err()
        );
        assert!(
            output
                .output_manifest(
                    &baseline,
                    diff_limits(limits(8, 2, 32, 64), 8, 3, 16),
                    || false,
                )
                .is_err()
        );
        assert!(
            output
                .output_manifest(
                    &baseline,
                    diff_limits(limits(8, 2, 32, 64), 8, 6, 7),
                    || false,
                )
                .is_err()
        );
        assert!(
            output
                .output_manifest(
                    &baseline,
                    diff_limits(limits(1, 2, 32, 64), 8, 16, 32),
                    || false,
                )
                .is_err()
        );
        assert!(
            output
                .output_manifest(
                    &baseline,
                    diff_limits(limits(8, 2, 3, 64), 8, 16, 32),
                    || false,
                )
                .is_err()
        );
    }

    #[test]
    fn output_manifest_rejects_overlap_and_observes_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let baseline_path = temp.path().join("baseline");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&baseline_path).unwrap();
        std::fs::write(root.join("file"), b"data").unwrap();
        let capability = DirectoryCapability::open(&root).unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();
        let scan_limits = limits(8, 2, 32, 64);
        let limits = diff_limits(scan_limits, 8, 8, 16);

        assert!(
            capability
                .output_manifest(&capability, limits, || false)
                .is_err()
        );
        assert_eq!(
            capability
                .output_manifest(&baseline, limits, || true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn output_manifest_rejects_reparse_entries_without_following_them() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline");
        let output_path = temp.path().join("output");
        let outside_path = temp.path().join("outside");
        std::fs::create_dir(&baseline_path).unwrap();
        std::fs::create_dir(&output_path).unwrap();
        std::fs::create_dir(&outside_path).unwrap();
        std::fs::write(outside_path.join("keep"), b"outside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_path, output_path.join("link")).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&outside_path, output_path.join("link")).is_err() {
            return;
        }
        let output = DirectoryCapability::open(&output_path).unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();

        assert_eq!(
            output
                .output_manifest(
                    &baseline,
                    diff_limits(limits(10, 2, 32, 64), 8, 16, 32),
                    || false,
                )
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
    fn reviewed_created_file_is_copied_only_to_a_new_existing_parent() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline");
        let output_path = temp.path().join("output");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir(&baseline_path).unwrap();
        std::fs::create_dir(&output_path).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();
        let output = DirectoryCapability::open(&output_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();
        baseline
            .copy_tree_to(&output, limits(8, 2, 128, 256), || false)
            .unwrap();
        std::fs::write(output_path.join("new.txt"), b"reviewed bytes").unwrap();
        let change = output
            .output_manifest(
                &baseline,
                diff_limits(limits(8, 2, 128, 256), 8, 128, 256),
                || false,
            )
            .unwrap()
            .changes
            .into_iter()
            .find(|change| change.relative_path == Path::new("new.txt"))
            .unwrap();

        let copied = output
            .copy_reviewed_created_file_to(&change, &destination, Path::new("new.txt"), || false)
            .unwrap();
        assert_eq!(copied.bytes, b"reviewed bytes".len() as u64);
        assert_eq!(
            std::fs::read(destination_path.join("new.txt")).unwrap(),
            b"reviewed bytes"
        );

        assert_eq!(
            output
                .copy_reviewed_created_file_to(&change, &destination, Path::new("new.txt"), || {
                    false
                },)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(destination_path.join("new.txt")).unwrap(),
            b"reviewed bytes"
        );

        assert!(
            output
                .copy_reviewed_created_file_to(
                    &change,
                    &destination,
                    Path::new("missing/child.txt"),
                    || false
                )
                .is_err()
        );
        assert!(!destination_path.join("missing").exists());
    }

    #[test]
    fn reviewed_created_file_copy_rejects_stale_source_and_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline");
        let output_path = temp.path().join("output");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir(&baseline_path).unwrap();
        std::fs::create_dir(&output_path).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();
        let output = DirectoryCapability::open(&output_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();
        std::fs::write(output_path.join("new.txt"), b"reviewed bytes").unwrap();
        let change = output
            .output_manifest(
                &baseline,
                diff_limits(limits(8, 2, 128, 256), 8, 128, 256),
                || false,
            )
            .unwrap()
            .changes
            .into_iter()
            .find(|change| change.relative_path == Path::new("new.txt"))
            .unwrap();

        std::fs::write(output_path.join("new.txt"), b"different bytes").unwrap();
        assert!(
            output
                .copy_reviewed_created_file_to(
                    &change,
                    &destination,
                    Path::new("stale.txt"),
                    || false
                )
                .is_err()
        );
        assert!(!destination_path.join("stale.txt").exists());

        let change = output
            .output_manifest(
                &baseline,
                diff_limits(limits(8, 2, 128, 256), 8, 128, 256),
                || false,
            )
            .unwrap()
            .changes
            .into_iter()
            .find(|change| change.relative_path == Path::new("new.txt"))
            .unwrap();
        assert_eq!(
            output
                .copy_reviewed_created_file_to(
                    &change,
                    &destination,
                    Path::new("cancelled.txt"),
                    || true,
                )
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!destination_path.join("cancelled.txt").exists());
    }

    #[test]
    fn reviewed_file_copy_rejects_non_created_manifest_entries() {
        let temp = tempfile::tempdir().unwrap();
        let baseline_path = temp.path().join("baseline");
        let output_path = temp.path().join("output");
        let destination_path = temp.path().join("destination");
        std::fs::create_dir(&baseline_path).unwrap();
        std::fs::create_dir(&output_path).unwrap();
        std::fs::create_dir(&destination_path).unwrap();
        std::fs::write(baseline_path.join("modified.txt"), b"before").unwrap();
        let baseline = DirectoryCapability::open(&baseline_path).unwrap();
        let output = DirectoryCapability::open(&output_path).unwrap();
        let destination = DirectoryCapability::open(&destination_path).unwrap();
        baseline
            .copy_tree_to(&output, limits(8, 2, 128, 256), || false)
            .unwrap();
        std::fs::write(output_path.join("modified.txt"), b"after").unwrap();
        let modified = output
            .output_manifest(
                &baseline,
                diff_limits(limits(8, 2, 128, 256), 8, 128, 256),
                || false,
            )
            .unwrap()
            .changes
            .into_iter()
            .find(|change| change.relative_path == Path::new("modified.txt"))
            .unwrap();
        assert_eq!(modified.change, DirectoryTreeChangeKind::Modified);
        assert!(
            output
                .copy_reviewed_created_file_to(
                    &modified,
                    &destination,
                    Path::new("modified.txt"),
                    || false
                )
                .is_err()
        );
        assert!(!destination_path.join("modified.txt").exists());
    }
}
