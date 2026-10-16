//! Bounded broker-to-client output-diff transfer and reconstruction.
//!
//! The HCS provider supplies a stopped, immutable output tree and its input
//! baseline. The broker sends only the bounded manifest changes and changed
//! file bytes. The receiver reconstructs a private final tree from its own
//! baseline, then recomputes the manifest before exposing either result.

use super::broker_protocol::{
    BrokerControlMessage, BrokerFrame, BrokerFrameKind, BrokerOutputChangeKind,
    BrokerWorkspaceEntryKind,
};
use crate::filesystem::{
    DirectoryCapability, DirectoryTreeChange, DirectoryTreeChangeKind, DirectoryTreeDiffLimits,
    DirectoryTreeEntryKind, DirectoryTreeOutputManifest,
};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const OUTPUT_CHUNK_BYTES: usize = 32 * 1024;
const OUTPUT_CHUNK_HEADER_BYTES: usize = std::mem::size_of::<u32>();

/// Reconstructed private output tree and the manifest verified against it.
pub(crate) struct ReceivedOutputSnapshot {
    pub(crate) output: DirectoryCapability,
    pub(crate) manifest: DirectoryTreeOutputManifest,
    pub(crate) initial_snapshot_bytes: u64,
}

/// Send the changed entries between `baseline` and the stopped output tree.
///
/// The caller must keep both source trees immutable during transfer and drop
/// the broker connection after any error. File bytes are rehashed while sent,
/// and the complete manifest is rescanned before the completion frame is sent.
pub(crate) fn send_output_snapshot(
    output: &DirectoryCapability,
    baseline: &DirectoryCapability,
    limits: DirectoryTreeDiffLimits,
    initial_snapshot_bytes: u64,
    mut send_frame: impl FnMut(BrokerFrame) -> io::Result<()>,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<DirectoryTreeOutputManifest> {
    if initial_snapshot_bytes > limits.scan().max_total_bytes() {
        return Err(invalid_data(
            "initial workspace exceeds the output transfer limit",
        ));
    }
    let manifest = output.output_manifest(baseline, limits, &mut is_cancelled)?;
    for (index, change) in manifest.changes.iter().enumerate() {
        check_cancelled(&mut is_cancelled)?;
        let wire_index = u32::try_from(index)
            .map_err(|_| invalid_data("output entry index exceeds the wire range"))?;
        let wire_path = change
            .relative_path
            .to_str()
            .ok_or_else(|| invalid_data("output path is not valid Unicode"))?
            .replace('\\', "/");
        send_control(
            &mut send_frame,
            BrokerControlMessage::OutputEntry {
                index: wire_index,
                relative_path: wire_path,
                change: encode_change(change.change),
                before_kind: encode_kind(change.before_kind),
                after_kind: encode_kind(change.after_kind),
                before_bytes: change.before_bytes,
                after_bytes: change.after_bytes,
                before_sha256: change.before_sha256,
                after_sha256: change.after_sha256,
            },
        )?;
        if change.after_kind == Some(DirectoryTreeEntryKind::File) {
            send_changed_file(
                output,
                change,
                wire_index,
                &mut send_frame,
                &mut is_cancelled,
            )?;
        }
    }

    check_cancelled(&mut is_cancelled)?;
    let final_manifest = output.output_manifest(baseline, limits, &mut is_cancelled)?;
    if final_manifest != manifest {
        return Err(invalid_data(
            "output tree changed during its snapshot transfer",
        ));
    }
    send_control(
        &mut send_frame,
        BrokerControlMessage::OutputSnapshotComplete {
            entry_count: u32::try_from(manifest.changes.len())
                .map_err(|_| invalid_data("output entry count exceeds the wire range"))?,
            changed_bytes: manifest.changed_bytes,
            initial_snapshot_bytes,
        },
    )?;
    Ok(manifest)
}

/// Receive a bounded output diff, rebuild its final tree from `baseline`, and
/// verify the reconstructed manifest. `output` and `file_staging` must be
/// separate empty directories owned by the enclosing attempt. The caller must
/// discard both after any error.
pub(crate) fn receive_output_snapshot(
    baseline: &DirectoryCapability,
    output: DirectoryCapability,
    file_staging: DirectoryCapability,
    limits: DirectoryTreeDiffLimits,
    mut read_frame: impl FnMut() -> io::Result<BrokerFrame>,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<ReceivedOutputSnapshot> {
    if roots_overlap(baseline, &output)
        || roots_overlap(baseline, &file_staging)
        || roots_overlap(&output, &file_staging)
    {
        return Err(permission_denied(
            "output snapshot roots must be separate private directories",
        ));
    }
    if !file_staging.entries(1)?.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output file staging directory must be empty",
        ));
    }
    let initial_snapshot = baseline.copy_tree_to(&output, limits.scan(), &mut is_cancelled)?;

    let mut changes = Vec::new();
    let mut changed_bytes = 0u64;
    loop {
        check_cancelled(&mut is_cancelled)?;
        let message = read_frame()?.decode_control()?;
        match message {
            BrokerControlMessage::OutputEntry {
                index,
                relative_path,
                change,
                before_kind,
                after_kind,
                before_bytes,
                after_bytes,
                before_sha256,
                after_sha256,
            } => {
                if index as usize != changes.len() || changes.len() >= limits.max_changes() {
                    return Err(invalid_data(
                        "output entries are duplicated, unordered, or over the limit",
                    ));
                }
                let change = DirectoryTreeChange {
                    relative_path: PathBuf::from(relative_path),
                    change: decode_change(change),
                    before_kind: decode_kind(before_kind),
                    after_kind: decode_kind(after_kind),
                    before_bytes,
                    after_bytes,
                    before_sha256,
                    after_sha256,
                };
                let path_depth = change.relative_path.components().count();
                let maximum_depth = [change.before_kind, change.after_kind]
                    .into_iter()
                    .flatten()
                    .map(|kind| match kind {
                        DirectoryTreeEntryKind::Directory => path_depth,
                        DirectoryTreeEntryKind::File => path_depth.saturating_sub(1),
                    })
                    .max()
                    .ok_or_else(|| invalid_data("output change has no file type"))?;
                if maximum_depth > limits.scan().max_depth() {
                    return Err(invalid_data("output path exceeds its depth limit"));
                }
                if change
                    .before_bytes
                    .into_iter()
                    .chain(change.after_bytes)
                    .any(|bytes| bytes > limits.max_changed_file_bytes())
                {
                    return Err(invalid_data("output file exceeds its size limit"));
                }
                changed_bytes = changed_bytes
                    .checked_add(
                        change
                            .before_bytes
                            .unwrap_or(0)
                            .max(change.after_bytes.unwrap_or(0)),
                    )
                    .ok_or_else(|| invalid_data("output changed-byte count overflowed"))?;
                if changed_bytes > limits.max_changed_total_bytes() {
                    return Err(invalid_data("output exceeds its changed-byte limit"));
                }
                if change.after_kind == Some(DirectoryTreeEntryKind::File) {
                    receive_changed_file(
                        index,
                        change.after_bytes.expect("validated file size"),
                        change.after_sha256.expect("validated file digest"),
                        &file_staging,
                        &mut read_frame,
                        &mut is_cancelled,
                    )?;
                }
                changes.push(change);
            }
            BrokerControlMessage::OutputSnapshotComplete {
                entry_count,
                changed_bytes,
                initial_snapshot_bytes,
            } => {
                if entry_count as usize != changes.len()
                    || initial_snapshot_bytes != initial_snapshot.byte_count
                {
                    return Err(invalid_data(
                        "output snapshot summary does not match its entries or baseline",
                    ));
                }
                apply_changes(&output, &file_staging, &changes, &mut is_cancelled)?;
                if !file_staging.entries(1)?.is_empty() {
                    return Err(invalid_data(
                        "output file staging contains unconsumed entries",
                    ));
                }
                let manifest = output.output_manifest(baseline, limits, &mut is_cancelled)?;
                if manifest.changes != changes || manifest.changed_bytes != changed_bytes {
                    return Err(invalid_data(
                        "reconstructed output does not match the transferred manifest",
                    ));
                }
                return Ok(ReceivedOutputSnapshot {
                    output,
                    manifest,
                    initial_snapshot_bytes: initial_snapshot.byte_count,
                });
            }
            _ => return Err(invalid_data("unexpected message during output transfer")),
        }
    }
}

fn send_changed_file(
    output: &DirectoryCapability,
    change: &DirectoryTreeChange,
    index: u32,
    send_frame: &mut impl FnMut(BrokerFrame) -> io::Result<()>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    let expected_bytes = change
        .after_bytes
        .ok_or_else(|| invalid_data("changed output file is missing its size"))?;
    let expected_digest = change
        .after_sha256
        .ok_or_else(|| invalid_data("changed output file is missing its digest"))?;
    let mut file = output.open_read(&change.relative_path)?;
    let mut hash = Sha256::new();
    let mut sent_bytes = 0u64;
    let mut buffer = [0u8; OUTPUT_CHUNK_BYTES];
    loop {
        check_cancelled(is_cancelled)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        sent_bytes = sent_bytes
            .checked_add(count as u64)
            .ok_or_else(|| invalid_data("changed output file size overflowed"))?;
        if sent_bytes > expected_bytes {
            return Err(invalid_data("changed output file grew during transfer"));
        }
        hash.update(&buffer[..count]);
        let mut payload = Vec::with_capacity(OUTPUT_CHUNK_HEADER_BYTES + count);
        payload.extend_from_slice(&index.to_le_bytes());
        payload.extend_from_slice(&buffer[..count]);
        send_frame(BrokerFrame::new(BrokerFrameKind::OutputFileChunk, payload)?)?;
    }
    let digest: [u8; 32] = hash.finalize().into();
    if sent_bytes != expected_bytes || digest != expected_digest {
        return Err(invalid_data("changed output file changed during transfer"));
    }
    Ok(())
}

fn receive_changed_file(
    index: u32,
    expected_bytes: u64,
    expected_digest: [u8; 32],
    file_staging: &DirectoryCapability,
    read_frame: &mut impl FnMut() -> io::Result<BrokerFrame>,
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    let file_path = staged_file_path(index);
    let mut file = file_staging.create_new_file(&file_path)?;
    let mut received_bytes = 0u64;
    let mut hash = Sha256::new();
    while received_bytes < expected_bytes {
        check_cancelled(is_cancelled)?;
        let frame = read_frame()?;
        if frame.kind() != BrokerFrameKind::OutputFileChunk
            || frame.payload().len() <= OUTPUT_CHUNK_HEADER_BYTES
        {
            return Err(invalid_data("changed output file chunk is invalid"));
        }
        let (index_bytes, contents) = frame.payload().split_at(OUTPUT_CHUNK_HEADER_BYTES);
        if u32::from_le_bytes(index_bytes.try_into().expect("fixed chunk header")) != index {
            return Err(invalid_data(
                "changed output chunk has the wrong entry index",
            ));
        }
        let next_bytes = received_bytes
            .checked_add(contents.len() as u64)
            .ok_or_else(|| invalid_data("changed output file size overflowed"))?;
        if next_bytes > expected_bytes {
            return Err(invalid_data("changed output file contains excess bytes"));
        }
        file.write_all(contents)?;
        hash.update(contents);
        received_bytes = next_bytes;
    }
    file.flush()?;
    let digest: [u8; 32] = hash.finalize().into();
    if received_bytes != expected_bytes || digest != expected_digest {
        return Err(invalid_data(
            "changed output file digest or length does not match",
        ));
    }
    Ok(())
}

fn apply_changes(
    output: &DirectoryCapability,
    file_staging: &DirectoryCapability,
    changes: &[DirectoryTreeChange],
    is_cancelled: &mut impl FnMut() -> bool,
) -> io::Result<()> {
    let mut removals = changes
        .iter()
        .filter(|change| {
            change.change == DirectoryTreeChangeKind::Deleted
                || change.change == DirectoryTreeChangeKind::TypeChanged
        })
        .collect::<Vec<_>>();
    removals.sort_by_key(|change| std::cmp::Reverse(change.relative_path.components().count()));
    for change in removals {
        check_cancelled(is_cancelled)?;
        match change.before_kind {
            Some(DirectoryTreeEntryKind::File) => output.remove_file(&change.relative_path)?,
            Some(DirectoryTreeEntryKind::Directory)
                if change.change == DirectoryTreeChangeKind::Deleted =>
            {
                output.remove_empty_dir(&change.relative_path)?;
            }
            Some(DirectoryTreeEntryKind::Directory) => {
                output.remove_dir_all(&change.relative_path)?;
            }
            None => return Err(invalid_data("output removal is missing its before type")),
        }
    }

    let mut directories = changes
        .iter()
        .filter(|change| {
            change.after_kind == Some(DirectoryTreeEntryKind::Directory)
                && change.before_kind != Some(DirectoryTreeEntryKind::Directory)
        })
        .collect::<Vec<_>>();
    directories.sort_by_key(|change| change.relative_path.components().count());
    for change in directories {
        check_cancelled(is_cancelled)?;
        output.create_dir(&change.relative_path)?;
    }

    for (index, change) in changes.iter().enumerate() {
        check_cancelled(is_cancelled)?;
        if change.after_kind != Some(DirectoryTreeEntryKind::File) {
            continue;
        }
        let staged_path = staged_file_path(
            u32::try_from(index).map_err(|_| invalid_data("output index overflowed"))?,
        );
        let mut staged = file_staging.open_read(&staged_path)?;
        let expected_bytes = change
            .after_bytes
            .ok_or_else(|| invalid_data("output file is missing its size"))?;
        let capacity = usize::try_from(expected_bytes)
            .map_err(|_| invalid_data("output file does not fit the host address space"))?;
        let mut contents = Vec::with_capacity(capacity);
        let mut buffer = [0u8; OUTPUT_CHUNK_BYTES];
        loop {
            check_cancelled(is_cancelled)?;
            let count = staged.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            if contents.len().saturating_add(count) > capacity {
                return Err(invalid_data("staged output file grew after validation"));
            }
            contents.extend_from_slice(&buffer[..count]);
        }
        if contents.len() as u64 != expected_bytes {
            return Err(invalid_data("staged output file changed after validation"));
        }
        match change.change {
            DirectoryTreeChangeKind::Created | DirectoryTreeChangeKind::TypeChanged => {
                let mut destination = output.create_new_file(&change.relative_path)?;
                destination.write_all(&contents)?;
                drop(destination);
            }
            DirectoryTreeChangeKind::Modified => {
                output.atomic_replace(&change.relative_path, &contents)?;
            }
            DirectoryTreeChangeKind::Deleted => {
                return Err(invalid_data("deleted output entry has file content"));
            }
        }
        drop(staged);
        file_staging.remove_file(&staged_path)?;
    }
    Ok(())
}

fn staged_file_path(index: u32) -> PathBuf {
    PathBuf::from(format!("output-{index:04}.bin"))
}

fn encode_change(change: DirectoryTreeChangeKind) -> BrokerOutputChangeKind {
    match change {
        DirectoryTreeChangeKind::Created => BrokerOutputChangeKind::Created,
        DirectoryTreeChangeKind::Modified => BrokerOutputChangeKind::Modified,
        DirectoryTreeChangeKind::Deleted => BrokerOutputChangeKind::Deleted,
        DirectoryTreeChangeKind::TypeChanged => BrokerOutputChangeKind::TypeChanged,
    }
}

fn decode_change(change: BrokerOutputChangeKind) -> DirectoryTreeChangeKind {
    match change {
        BrokerOutputChangeKind::Created => DirectoryTreeChangeKind::Created,
        BrokerOutputChangeKind::Modified => DirectoryTreeChangeKind::Modified,
        BrokerOutputChangeKind::Deleted => DirectoryTreeChangeKind::Deleted,
        BrokerOutputChangeKind::TypeChanged => DirectoryTreeChangeKind::TypeChanged,
    }
}

fn encode_kind(kind: Option<DirectoryTreeEntryKind>) -> Option<BrokerWorkspaceEntryKind> {
    kind.map(|kind| match kind {
        DirectoryTreeEntryKind::File => BrokerWorkspaceEntryKind::File,
        DirectoryTreeEntryKind::Directory => BrokerWorkspaceEntryKind::Directory,
    })
}

fn decode_kind(kind: Option<BrokerWorkspaceEntryKind>) -> Option<DirectoryTreeEntryKind> {
    kind.map(|kind| match kind {
        BrokerWorkspaceEntryKind::File => DirectoryTreeEntryKind::File,
        BrokerWorkspaceEntryKind::Directory => DirectoryTreeEntryKind::Directory,
    })
}

fn roots_overlap(left: &DirectoryCapability, right: &DirectoryCapability) -> bool {
    roots_equal(left.root(), right.root())
        || left.relative_path(right.root()).is_some()
        || right.relative_path(left.root()).is_some()
}

fn roots_equal(left: &Path, right: &Path) -> bool {
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
}

fn check_cancelled(is_cancelled: &mut impl FnMut() -> bool) -> io::Result<()> {
    if is_cancelled() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "output snapshot transfer was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn send_control(
    send_frame: &mut impl FnMut(BrokerFrame) -> io::Result<()>,
    message: BrokerControlMessage,
) -> io::Result<()> {
    send_frame(BrokerFrame::control(&message)?)
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
    use crate::filesystem::{DirectoryTreeCopyLimits, DirectoryTreeOutputManifest};
    use std::collections::VecDeque;

    fn limits() -> DirectoryTreeDiffLimits {
        let scan = DirectoryTreeCopyLimits::new(64, 8, 128 * 1024, 512 * 1024).unwrap();
        DirectoryTreeDiffLimits::new(scan, 64, 128 * 1024, 512 * 1024).unwrap()
    }

    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        DirectoryTreeOutputManifest,
        u64,
    ) {
        let baseline_temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(baseline_temp.path().join("gone-dir")).unwrap();
        std::fs::create_dir_all(baseline_temp.path().join("to-file/nested")).unwrap();
        std::fs::write(baseline_temp.path().join("same.txt"), b"same").unwrap();
        std::fs::write(baseline_temp.path().join("modify.txt"), b"old").unwrap();
        std::fs::write(baseline_temp.path().join("delete.txt"), b"gone").unwrap();
        std::fs::write(baseline_temp.path().join("gone-dir/child.txt"), b"child").unwrap();
        std::fs::write(baseline_temp.path().join("to-dir"), b"was-file").unwrap();
        std::fs::write(
            baseline_temp.path().join("to-file/nested/child.txt"),
            b"was-directory",
        )
        .unwrap();
        let baseline = DirectoryCapability::open(baseline_temp.path()).unwrap();

        let output_temp = tempfile::tempdir().unwrap();
        let output = DirectoryCapability::open(output_temp.path()).unwrap();
        let baseline_summary = baseline
            .copy_tree_to(&output, limits().scan(), || false)
            .unwrap();
        std::fs::write(output_temp.path().join("modify.txt"), b"updated").unwrap();
        std::fs::remove_file(output_temp.path().join("delete.txt")).unwrap();
        std::fs::remove_dir_all(output_temp.path().join("gone-dir")).unwrap();
        std::fs::remove_file(output_temp.path().join("to-dir")).unwrap();
        std::fs::create_dir(output_temp.path().join("to-dir")).unwrap();
        std::fs::write(
            output_temp.path().join("to-dir/child.txt"),
            b"now-directory",
        )
        .unwrap();
        std::fs::remove_dir_all(output_temp.path().join("to-file")).unwrap();
        std::fs::write(output_temp.path().join("to-file"), b"now-file").unwrap();
        std::fs::create_dir_all(output_temp.path().join("new-dir/nested")).unwrap();
        std::fs::write(
            output_temp.path().join("new-dir/nested/new.txt"),
            b"created",
        )
        .unwrap();
        let large_file = vec![0x5a; 70 * 1024];
        std::fs::write(output_temp.path().join("large.bin"), &large_file).unwrap();

        let output_tree = DirectoryCapability::open(output_temp.path()).unwrap();
        let baseline_manifest = output_tree
            .output_manifest(&baseline, limits(), || false)
            .unwrap();
        let chunks_temp = tempfile::tempdir().unwrap();
        let reconstructed_temp = tempfile::tempdir().unwrap();
        (
            baseline_temp,
            output_temp,
            chunks_temp,
            reconstructed_temp,
            baseline_manifest,
            baseline_summary.byte_count,
        )
    }

    #[test]
    fn output_snapshot_round_trips_changes_and_reconstructs_a_verified_tree() {
        let (
            baseline_temp,
            output_temp,
            chunks_temp,
            reconstructed_temp,
            expected_manifest,
            initial_snapshot_bytes,
        ) = fixture();
        let baseline = DirectoryCapability::open(baseline_temp.path()).unwrap();
        let output = DirectoryCapability::open(output_temp.path()).unwrap();
        let chunks = DirectoryCapability::open(chunks_temp.path()).unwrap();
        let mut frames = Vec::new();
        let sent = send_output_snapshot(
            &output,
            &baseline,
            limits(),
            initial_snapshot_bytes,
            |frame| {
                frames.push(frame);
                Ok(())
            },
            || false,
        )
        .unwrap();
        assert_eq!(sent, expected_manifest);
        assert!(
            frames
                .iter()
                .filter(|frame| frame.kind() == BrokerFrameKind::OutputFileChunk)
                .all(
                    |frame| frame.payload().len() <= OUTPUT_CHUNK_HEADER_BYTES + OUTPUT_CHUNK_BYTES
                )
        );

        let output_root = DirectoryCapability::open(reconstructed_temp.path()).unwrap();
        let frames = std::cell::RefCell::new(VecDeque::from(frames));
        let received = receive_output_snapshot(
            &baseline,
            output_root,
            chunks.clone(),
            limits(),
            || {
                frames
                    .borrow_mut()
                    .pop_front()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
            },
            || false,
        )
        .unwrap();

        assert_eq!(received.manifest, expected_manifest);
        assert_eq!(received.initial_snapshot_bytes, initial_snapshot_bytes);
        assert_eq!(
            std::fs::read(received.output.root().join("large.bin")).unwrap(),
            vec![0x5a; 70 * 1024]
        );
        assert_eq!(
            std::fs::read(received.output.root().join("modify.txt")).unwrap(),
            b"updated"
        );
        assert_eq!(
            std::fs::read(received.output.root().join("to-dir/child.txt")).unwrap(),
            b"now-directory"
        );
        assert_eq!(
            std::fs::read(received.output.root().join("to-file")).unwrap(),
            b"now-file"
        );
        assert!(!received.output.root().join("delete.txt").exists());
        assert!(chunks.entries(1).unwrap().is_empty());
        assert!(frames.borrow().is_empty());
    }

    #[test]
    fn receiver_rejects_changed_output_file_digest_before_reconstruction() {
        let (
            baseline_temp,
            output_temp,
            chunks_temp,
            reconstructed_temp,
            _,
            initial_snapshot_bytes,
        ) = fixture();
        let baseline = DirectoryCapability::open(baseline_temp.path()).unwrap();
        let output = DirectoryCapability::open(output_temp.path()).unwrap();
        let mut frames = Vec::new();
        send_output_snapshot(
            &output,
            &baseline,
            limits(),
            initial_snapshot_bytes,
            |frame| {
                frames.push(frame);
                Ok(())
            },
            || false,
        )
        .unwrap();
        let chunk = frames
            .iter_mut()
            .find(|frame| frame.kind() == BrokerFrameKind::OutputFileChunk)
            .unwrap();
        let mut payload = chunk.payload().to_vec();
        let last = payload.len() - 1;
        payload[last] ^= 0xff;
        *chunk = BrokerFrame::new(BrokerFrameKind::OutputFileChunk, payload).unwrap();

        let output_root = DirectoryCapability::open(reconstructed_temp.path()).unwrap();
        let chunks_root = DirectoryCapability::open(chunks_temp.path()).unwrap();
        let mut frames = VecDeque::from(frames);
        let error = match receive_output_snapshot(
            &baseline,
            output_root,
            chunks_root,
            limits(),
            || {
                frames
                    .pop_front()
                    .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
            },
            || false,
        ) {
            Ok(_) => panic!("the receiver rejects output payload changed in transit"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
