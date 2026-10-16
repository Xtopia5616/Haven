//! Bounded workspace snapshot framing for the broker named-pipe protocol.
//!
//! The sender reads from a capability-pinned, private snapshot. The receiver
//! writes only into an empty capability-pinned staging directory. Callers own
//! the attempt directory and must discard it after any transfer error.

use super::broker_protocol::{
    BrokerControlMessage, BrokerFrame, BrokerFrameKind, BrokerWorkspaceEntryKind,
};
use crate::filesystem::{
    DirectoryCapability, DirectoryEntryKind, DirectoryTreeCopyLimits, DirectoryTreeCopySummary,
};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const WORKSPACE_CHUNK_BYTES: usize = 32 * 1024;
const SKILL_PARAMETER_CHUNK_BYTES: usize = 32 * 1024;

pub(crate) fn send_skill_parameters(
    parameters: &[u8],
    mut send_frame: impl FnMut(BrokerFrame) -> io::Result<()>,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<()> {
    if parameters.is_empty()
        || parameters.len() > super::MAX_SKILL_PARAMETER_BYTES
        || !serde_json::from_slice::<serde_json::Value>(parameters)
            .is_ok_and(|value| value.is_object())
    {
        return Err(invalid_data("Skill parameters are outside their contract"));
    }
    for chunk in parameters.chunks(SKILL_PARAMETER_CHUNK_BYTES) {
        if is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Skill parameter transfer was cancelled",
            ));
        }
        send_frame(BrokerFrame::new(
            BrokerFrameKind::SkillParameterChunk,
            chunk.to_vec(),
        )?)?;
    }
    Ok(())
}

pub(crate) fn receive_skill_parameters(
    expected_bytes: usize,
    expected_sha256: [u8; 32],
    mut read_frame: impl FnMut() -> io::Result<BrokerFrame>,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<Vec<u8>> {
    if expected_bytes == 0 || expected_bytes > super::MAX_SKILL_PARAMETER_BYTES {
        return Err(invalid_data("Skill parameter length is outside its limit"));
    }
    let mut parameters = Vec::with_capacity(expected_bytes);
    while parameters.len() < expected_bytes {
        if is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Skill parameter receive was cancelled",
            ));
        }
        let frame = read_frame()?;
        if frame.kind() != BrokerFrameKind::SkillParameterChunk || frame.payload().is_empty() {
            return Err(invalid_data(
                "unexpected frame during Skill parameter receive",
            ));
        }
        if parameters.len().saturating_add(frame.payload().len()) > expected_bytes {
            return Err(invalid_data("Skill parameter stream contains excess bytes"));
        }
        parameters.extend_from_slice(frame.payload());
    }
    let digest: [u8; 32] = Sha256::digest(&parameters).into();
    if digest != expected_sha256
        || !serde_json::from_slice::<serde_json::Value>(&parameters)
            .is_ok_and(|value| value.is_object())
    {
        return Err(invalid_data("Skill parameter payload failed validation"));
    }
    Ok(parameters)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotPurpose {
    Workspace,
    SkillBundle,
}

impl SnapshotPurpose {
    fn entry(
        self,
        relative_path: String,
        kind: BrokerWorkspaceEntryKind,
        byte_length: u64,
        sha256: Option<[u8; 32]>,
    ) -> BrokerControlMessage {
        match self {
            Self::Workspace => BrokerControlMessage::WorkspaceEntry {
                relative_path,
                kind,
                byte_length,
                sha256,
            },
            Self::SkillBundle => BrokerControlMessage::SkillBundleEntry {
                relative_path,
                kind,
                byte_length,
                sha256,
            },
        }
    }

    fn complete(self, entry_count: u32, total_bytes: u64) -> BrokerControlMessage {
        match self {
            Self::Workspace => BrokerControlMessage::WorkspaceSnapshotComplete {
                entry_count,
                total_bytes,
            },
            Self::SkillBundle => BrokerControlMessage::SkillBundleSnapshotComplete {
                entry_count,
                total_bytes,
            },
        }
    }

    fn is_complete(self, message: &BrokerControlMessage) -> bool {
        matches!(
            (self, message),
            (
                Self::Workspace,
                BrokerControlMessage::WorkspaceSnapshotComplete { .. }
            ) | (
                Self::SkillBundle,
                BrokerControlMessage::SkillBundleSnapshotComplete { .. }
            )
        )
    }

    fn chunk_kind(self) -> BrokerFrameKind {
        match self {
            Self::Workspace => BrokerFrameKind::WorkspaceChunk,
            Self::SkillBundle => BrokerFrameKind::SkillBundleChunk,
        }
    }
}

/// Send one bounded snapshot as ordered entry controls and file chunks.
///
/// File metadata is hashed before its entry control is sent, then the same
/// open file handle is rewound and streamed. A concurrent source mutation is
/// detected by rechecking size and digest while sending; the caller must drop
/// the broker connection after an error because the receiver has a partial
/// attempt snapshot.
pub(crate) fn send_workspace_snapshot(
    snapshot: &DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    send_frame: impl FnMut(BrokerFrame) -> io::Result<()>,
    is_cancelled: impl FnMut() -> bool,
) -> io::Result<DirectoryTreeCopySummary> {
    send_snapshot(
        snapshot,
        limits,
        SnapshotPurpose::Workspace,
        send_frame,
        is_cancelled,
    )
}

pub(crate) fn send_skill_bundle_snapshot(
    snapshot: &DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    send_frame: impl FnMut(BrokerFrame) -> io::Result<()>,
    is_cancelled: impl FnMut() -> bool,
) -> io::Result<DirectoryTreeCopySummary> {
    send_snapshot(
        snapshot,
        limits,
        SnapshotPurpose::SkillBundle,
        send_frame,
        is_cancelled,
    )
}

fn send_snapshot(
    snapshot: &DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    purpose: SnapshotPurpose,
    mut send_frame: impl FnMut(BrokerFrame) -> io::Result<()>,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<DirectoryTreeCopySummary> {
    let mut state = SenderState {
        snapshot,
        limits,
        purpose,
        summary: DirectoryTreeCopySummary::default(),
        send_frame: &mut send_frame,
        is_cancelled: &mut is_cancelled,
    };
    state.send_directory(Path::new(""), 0)?;
    state.send_control(
        purpose.complete(
            u32::try_from(state.summary.entry_count)
                .map_err(|_| invalid_data("snapshot entry count exceeds the wire range"))?,
            state.summary.byte_count,
        ),
    )?;
    Ok(state.summary)
}

/// Read a workspace snapshot from a framed transport into the attempt's empty
/// staging capability. The caller owns and must discard the attempt after any
/// read, validation, write, or cancellation error.
pub(crate) fn receive_workspace_snapshot(
    staging: DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    read_frame: impl FnMut() -> io::Result<BrokerFrame>,
    is_cancelled: impl FnMut() -> bool,
) -> io::Result<(DirectoryCapability, DirectoryTreeCopySummary)> {
    receive_snapshot(
        staging,
        limits,
        SnapshotPurpose::Workspace,
        read_frame,
        is_cancelled,
    )
}

pub(crate) fn receive_skill_bundle_snapshot(
    staging: DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    read_frame: impl FnMut() -> io::Result<BrokerFrame>,
    is_cancelled: impl FnMut() -> bool,
) -> io::Result<(DirectoryCapability, DirectoryTreeCopySummary)> {
    receive_snapshot(
        staging,
        limits,
        SnapshotPurpose::SkillBundle,
        read_frame,
        is_cancelled,
    )
}

fn receive_snapshot(
    staging: DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    purpose: SnapshotPurpose,
    mut read_frame: impl FnMut() -> io::Result<BrokerFrame>,
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<(DirectoryCapability, DirectoryTreeCopySummary)> {
    let mut receiver = WorkspaceSnapshotReceiver::new_for(staging, limits, purpose)?;
    loop {
        if is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "workspace snapshot receive was cancelled",
            ));
        }
        let frame = read_frame()?;
        if frame.kind() == BrokerFrameKind::Control {
            let message = frame.decode_control()?;
            let completed = purpose.is_complete(&message);
            receiver.accept_control(message)?;
            if completed {
                return receiver.finish();
            }
        } else {
            receiver.accept_chunk(&frame)?;
        }
    }
}

struct SenderState<'a, SendFrame, IsCancelled> {
    snapshot: &'a DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    purpose: SnapshotPurpose,
    summary: DirectoryTreeCopySummary,
    send_frame: &'a mut SendFrame,
    is_cancelled: &'a mut IsCancelled,
}

impl<SendFrame, IsCancelled> SenderState<'_, SendFrame, IsCancelled>
where
    SendFrame: FnMut(BrokerFrame) -> io::Result<()>,
    IsCancelled: FnMut() -> bool,
{
    fn send_directory(&mut self, relative_directory: &Path, depth: usize) -> io::Result<()> {
        self.check_cancelled()?;
        let remaining_entries = self
            .limits
            .max_entries()
            .saturating_sub(self.summary.entry_count);
        let mut entries = if relative_directory.as_os_str().is_empty() {
            self.snapshot.entries(remaining_entries)?
        } else {
            self.snapshot
                .entries_at(relative_directory, remaining_entries)?
        };
        entries.sort_by(|left, right| left.name.cmp(&right.name));

        for entry in entries {
            self.check_cancelled()?;
            self.add_entry()?;
            let mut relative_path = relative_directory.to_path_buf();
            relative_path.push(&entry.name);
            let wire_path = relative_path
                .to_str()
                .ok_or_else(|| invalid_data("workspace path is not valid Unicode"))?
                .replace('\\', "/");

            match entry.kind {
                DirectoryEntryKind::Directory => {
                    let child_depth = depth
                        .checked_add(1)
                        .ok_or_else(|| invalid_data("workspace depth overflowed"))?;
                    if child_depth > self.limits.max_depth() {
                        return Err(invalid_data("workspace exceeds its directory depth limit"));
                    }
                    self.summary.directory_count = self
                        .summary
                        .directory_count
                        .checked_add(1)
                        .ok_or_else(|| invalid_data("workspace directory count overflowed"))?;
                    self.send_control(self.purpose.entry(
                        wire_path,
                        BrokerWorkspaceEntryKind::Directory,
                        0,
                        None,
                    ))?;
                    self.send_directory(&relative_path, child_depth)?;
                }
                DirectoryEntryKind::File => {
                    self.summary.file_count = self
                        .summary
                        .file_count
                        .checked_add(1)
                        .ok_or_else(|| invalid_data("workspace file count overflowed"))?;
                    self.send_file(&relative_path, wire_path)?;
                }
                DirectoryEntryKind::ReparsePoint => {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "workspace snapshot contains a reparse point",
                    ));
                }
                DirectoryEntryKind::Other => {
                    return Err(invalid_data("workspace snapshot contains a non-file entry"));
                }
            }
        }
        Ok(())
    }

    fn send_file(&mut self, relative_path: &Path, wire_path: String) -> io::Result<()> {
        let mut file = self.snapshot.open_read(relative_path)?;
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; WORKSPACE_CHUNK_BYTES];
        let mut byte_length = 0_u64;
        loop {
            self.check_cancelled()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            byte_length = byte_length
                .checked_add(count as u64)
                .ok_or_else(|| invalid_data("workspace file size overflowed"))?;
            if byte_length > self.limits.max_file_bytes() {
                return Err(invalid_data("workspace file exceeds its size limit"));
            }
            hash.update(&buffer[..count]);
        }
        let next_total = self
            .summary
            .byte_count
            .checked_add(byte_length)
            .ok_or_else(|| invalid_data("workspace byte count overflowed"))?;
        if next_total > self.limits.max_total_bytes() {
            return Err(invalid_data("workspace exceeds its byte limit"));
        }
        self.summary.byte_count = next_total;
        let expected_sha256: [u8; 32] = hash.finalize().into();

        self.send_control(self.purpose.entry(
            wire_path,
            BrokerWorkspaceEntryKind::File,
            byte_length,
            Some(expected_sha256),
        ))?;

        file.seek(SeekFrom::Start(0))?;
        let mut sent_hash = Sha256::new();
        let mut sent_bytes = 0_u64;
        loop {
            self.check_cancelled()?;
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            sent_bytes = sent_bytes
                .checked_add(count as u64)
                .ok_or_else(|| invalid_data("workspace stream size overflowed"))?;
            if sent_bytes > byte_length {
                return Err(invalid_data("workspace file changed during transfer"));
            }
            sent_hash.update(&buffer[..count]);
            let frame = BrokerFrame::new(self.purpose.chunk_kind(), buffer[..count].to_vec())?;
            (self.send_frame)(frame)?;
        }
        let sent_sha256: [u8; 32] = sent_hash.finalize().into();
        if sent_bytes != byte_length || sent_sha256 != expected_sha256 {
            return Err(invalid_data("workspace file changed during transfer"));
        }
        Ok(())
    }

    fn send_control(&mut self, message: BrokerControlMessage) -> io::Result<()> {
        (self.send_frame)(BrokerFrame::control(&message)?)
    }

    fn add_entry(&mut self) -> io::Result<()> {
        self.summary.entry_count = self
            .summary
            .entry_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("workspace entry count overflowed"))?;
        if self.summary.entry_count > self.limits.max_entries() {
            return Err(invalid_data("workspace exceeds its entry limit"));
        }
        Ok(())
    }

    fn check_cancelled(&mut self) -> io::Result<()> {
        if (self.is_cancelled)() {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "workspace snapshot transfer was cancelled",
            ))
        } else {
            Ok(())
        }
    }
}

/// Receives ordered workspace entries into an empty private staging root.
///
/// Directory entries must precede their children. File chunks are written
/// directly to newly created files, with a bounded buffer and a digest check
/// before the file is accepted. The owner of the enclosing attempt must remove
/// the whole staging directory if this receiver is dropped before completion.
pub(crate) struct WorkspaceSnapshotReceiver {
    root: DirectoryCapability,
    limits: DirectoryTreeCopyLimits,
    summary: DirectoryTreeCopySummary,
    current_file: Option<PendingFile>,
    complete: bool,
    purpose: SnapshotPurpose,
}

struct PendingFile {
    relative_path: PathBuf,
    file: File,
    expected_bytes: u64,
    received_bytes: u64,
    expected_sha256: [u8; 32],
    hash: Sha256,
}

impl WorkspaceSnapshotReceiver {
    #[cfg(test)]
    pub(crate) fn new(
        root: DirectoryCapability,
        limits: DirectoryTreeCopyLimits,
    ) -> io::Result<Self> {
        Self::new_for(root, limits, SnapshotPurpose::Workspace)
    }

    fn new_for(
        root: DirectoryCapability,
        limits: DirectoryTreeCopyLimits,
        purpose: SnapshotPurpose,
    ) -> io::Result<Self> {
        if !root.entries(1)?.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "workspace staging root must be empty",
            ));
        }
        Ok(Self {
            root,
            limits,
            summary: DirectoryTreeCopySummary::default(),
            current_file: None,
            complete: false,
            purpose,
        })
    }

    pub(crate) fn accept_control(&mut self, message: BrokerControlMessage) -> io::Result<()> {
        if self.complete || self.current_file.is_some() {
            return Err(invalid_data(
                "unexpected control frame in snapshot transfer",
            ));
        }
        BrokerFrame::control(&message)?;
        match message {
            BrokerControlMessage::WorkspaceEntry {
                relative_path,
                kind,
                byte_length,
                sha256,
            } if self.purpose == SnapshotPurpose::Workspace => {
                self.accept_entry(relative_path, kind, byte_length, sha256)
            }
            BrokerControlMessage::SkillBundleEntry {
                relative_path,
                kind,
                byte_length,
                sha256,
            } if self.purpose == SnapshotPurpose::SkillBundle => {
                self.accept_entry(relative_path, kind, byte_length, sha256)
            }
            BrokerControlMessage::WorkspaceSnapshotComplete {
                entry_count,
                total_bytes,
            } if self.purpose == SnapshotPurpose::Workspace
                && entry_count as usize == self.summary.entry_count
                && total_bytes == self.summary.byte_count =>
            {
                self.complete = true;
                Ok(())
            }
            BrokerControlMessage::WorkspaceSnapshotComplete { .. }
                if self.purpose == SnapshotPurpose::Workspace =>
            {
                Err(invalid_data(
                    "workspace snapshot summary does not match entries",
                ))
            }
            BrokerControlMessage::SkillBundleSnapshotComplete {
                entry_count,
                total_bytes,
            } if self.purpose == SnapshotPurpose::SkillBundle
                && entry_count as usize == self.summary.entry_count
                && total_bytes == self.summary.byte_count =>
            {
                self.complete = true;
                Ok(())
            }
            BrokerControlMessage::SkillBundleSnapshotComplete { .. }
                if self.purpose == SnapshotPurpose::SkillBundle =>
            {
                Err(invalid_data(
                    "Skill bundle snapshot summary does not match entries",
                ))
            }
            _ => Err(invalid_data("unexpected message during snapshot transfer")),
        }
    }

    fn accept_entry(
        &mut self,
        relative_path: String,
        kind: BrokerWorkspaceEntryKind,
        byte_length: u64,
        sha256: Option<[u8; 32]>,
    ) -> io::Result<()> {
        self.add_entry()?;
        let relative_path = PathBuf::from(relative_path);
        let depth = relative_path.components().count();
        match kind {
            BrokerWorkspaceEntryKind::Directory => {
                if depth > self.limits.max_depth() {
                    return Err(invalid_data("snapshot exceeds its directory depth limit"));
                }
                self.root.create_dir(&relative_path)?;
                self.summary.directory_count = self
                    .summary
                    .directory_count
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("snapshot directory count overflowed"))?;
            }
            BrokerWorkspaceEntryKind::File => {
                if depth.saturating_sub(1) > self.limits.max_depth() {
                    return Err(invalid_data(
                        "snapshot file exceeds its directory depth limit",
                    ));
                }
                if byte_length > self.limits.max_file_bytes() {
                    return Err(invalid_data("snapshot file exceeds its size limit"));
                }
                let next_total = self
                    .summary
                    .byte_count
                    .checked_add(byte_length)
                    .ok_or_else(|| invalid_data("snapshot byte count overflowed"))?;
                if next_total > self.limits.max_total_bytes() {
                    return Err(invalid_data("snapshot exceeds its byte limit"));
                }
                let expected_sha256 =
                    sha256.ok_or_else(|| invalid_data("snapshot file digest is missing"))?;
                let file = self.root.create_new_file(&relative_path)?;
                self.summary.file_count = self
                    .summary
                    .file_count
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("snapshot file count overflowed"))?;
                self.summary.byte_count = next_total;
                self.current_file = Some(PendingFile {
                    relative_path,
                    file,
                    expected_bytes: byte_length,
                    received_bytes: 0,
                    expected_sha256,
                    hash: Sha256::new(),
                });
                if byte_length == 0 {
                    self.finish_current_file()?;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn accept_chunk(&mut self, frame: &BrokerFrame) -> io::Result<()> {
        if self.complete || frame.kind() != self.purpose.chunk_kind() || frame.payload().is_empty()
        {
            return Err(invalid_data("unexpected workspace chunk"));
        }
        let file = self
            .current_file
            .as_mut()
            .ok_or_else(|| invalid_data("workspace chunk has no active file"))?;
        let next_bytes = file
            .received_bytes
            .checked_add(frame.payload().len() as u64)
            .ok_or_else(|| invalid_data("workspace file size overflowed"))?;
        if next_bytes > file.expected_bytes {
            return Err(invalid_data("workspace file contains excess bytes"));
        }
        file.file.write_all(frame.payload())?;
        file.hash.update(frame.payload());
        file.received_bytes = next_bytes;
        if next_bytes == file.expected_bytes {
            self.finish_current_file()?;
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> io::Result<(DirectoryCapability, DirectoryTreeCopySummary)> {
        if !self.complete || self.current_file.is_some() {
            return Err(invalid_data("workspace snapshot transfer is incomplete"));
        }
        Ok((self.root.clone(), self.summary))
    }

    fn add_entry(&mut self) -> io::Result<()> {
        self.summary.entry_count = self
            .summary
            .entry_count
            .checked_add(1)
            .ok_or_else(|| invalid_data("workspace entry count overflowed"))?;
        if self.summary.entry_count > self.limits.max_entries() {
            return Err(invalid_data("workspace exceeds its entry limit"));
        }
        Ok(())
    }

    fn finish_current_file(&mut self) -> io::Result<()> {
        let mut file = self
            .current_file
            .take()
            .ok_or_else(|| invalid_data("workspace file state is missing"))?;
        file.file.flush()?;
        let actual_sha256: [u8; 32] = file.hash.finalize().into();
        if file.received_bytes != file.expected_bytes || actual_sha256 != file.expected_sha256 {
            drop(file.file);
            let _ = self.root.remove_file(&file.relative_path);
            return Err(invalid_data(
                "workspace file digest or length does not match",
            ));
        }
        Ok(())
    }
}

impl Drop for WorkspaceSnapshotReceiver {
    fn drop(&mut self) {
        if let Some(file) = self.current_file.take() {
            drop(file.file);
            let _ = self.root.remove_file(&file.relative_path);
        }
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> DirectoryTreeCopyLimits {
        DirectoryTreeCopyLimits::new(16, 8, 128 * 1024, 256 * 1024).unwrap()
    }

    #[test]
    fn workspace_snapshot_round_trips_with_bounded_chunks_and_digest_checks() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("src")).unwrap();
        std::fs::create_dir(source.path().join("src/nested")).unwrap();
        std::fs::create_dir(source.path().join("empty")).unwrap();
        std::fs::write(source.path().join("src/nested/main.rs"), b"fn main() {}\n").unwrap();
        std::fs::write(source.path().join("README.md"), b"hello\n").unwrap();
        std::fs::write(source.path().join("empty.txt"), b"").unwrap();
        let large_file = vec![0x5a; WORKSPACE_CHUNK_BYTES * 2 + 17];
        std::fs::write(source.path().join("large.bin"), &large_file).unwrap();
        let source = DirectoryCapability::open(source.path()).unwrap();
        let destination = tempfile::tempdir().unwrap();
        let destination = DirectoryCapability::open(destination.path()).unwrap();
        let mut frames = Vec::new();

        let sent = send_workspace_snapshot(
            &source,
            limits(),
            |frame| {
                frames.push(frame);
                Ok(())
            },
            || false,
        )
        .unwrap();
        let chunk_sizes: Vec<_> = frames
            .iter()
            .filter(|frame| frame.kind() == BrokerFrameKind::WorkspaceChunk)
            .map(|frame| frame.payload().len())
            .collect();
        assert_eq!(chunk_sizes.len(), 5);
        assert!(
            chunk_sizes
                .iter()
                .all(|size| *size <= WORKSPACE_CHUNK_BYTES)
        );
        let mut receiver = WorkspaceSnapshotReceiver::new(destination, limits()).unwrap();
        for frame in frames {
            if frame.kind() == BrokerFrameKind::Control {
                receiver
                    .accept_control(frame.decode_control().unwrap())
                    .unwrap();
            } else {
                receiver.accept_chunk(&frame).unwrap();
            }
        }
        let (received_root, received) = receiver.finish().unwrap();

        assert_eq!(sent, received);
        assert_eq!(received.entry_count, 7);
        assert_eq!(received.directory_count, 3);
        assert_eq!(received.file_count, 4);
        assert_eq!(received.byte_count, large_file.len() as u64 + 19);
        assert_eq!(
            std::fs::read(received_root.root().join("src/nested/main.rs")).unwrap(),
            b"fn main() {}\n"
        );
        assert_eq!(
            std::fs::read(received_root.root().join("README.md")).unwrap(),
            b"hello\n"
        );
        assert!(
            std::fs::read(received_root.root().join("empty.txt"))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            std::fs::read(received_root.root().join("large.bin")).unwrap(),
            large_file
        );
    }

    #[test]
    fn skill_bundle_uses_a_separate_bounded_frame_channel() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("scripts")).unwrap();
        std::fs::write(source.path().join("scripts/main.py"), b"print(input())").unwrap();
        let source = DirectoryCapability::open(source.path()).unwrap();
        let destination = tempfile::tempdir().unwrap();
        let destination = DirectoryCapability::open(destination.path()).unwrap();
        let mut frames = Vec::new();
        let sent = send_skill_bundle_snapshot(
            &source,
            limits(),
            |frame| {
                frames.push(frame);
                Ok(())
            },
            || false,
        )
        .unwrap();
        assert!(
            frames
                .iter()
                .any(|frame| { frame.kind() == BrokerFrameKind::SkillBundleChunk })
        );
        assert!(
            frames
                .iter()
                .all(|frame| { frame.kind() != BrokerFrameKind::WorkspaceChunk })
        );
        let mut incoming = frames.into_iter();
        let (received_root, received) = receive_skill_bundle_snapshot(
            destination,
            limits(),
            || {
                incoming.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "missing Skill frame")
                })
            },
            || false,
        )
        .unwrap();
        assert_eq!(sent, received);
        assert_eq!(
            std::fs::read(received_root.root().join("scripts/main.py")).unwrap(),
            b"print(input())"
        );
    }

    #[test]
    fn skill_parameters_are_length_digest_and_json_checked_as_data_frames() {
        let parameters = br#"{"message":"private"}"#;
        let mut frames = Vec::new();
        send_skill_parameters(
            parameters,
            |frame| {
                frames.push(frame);
                Ok(())
            },
            || false,
        )
        .unwrap();
        assert!(
            frames
                .iter()
                .all(|frame| { frame.kind() == BrokerFrameKind::SkillParameterChunk })
        );
        let mut incoming = frames.into_iter();
        let digest: [u8; 32] = Sha256::digest(parameters).into();
        let received = receive_skill_parameters(
            parameters.len(),
            digest,
            || {
                incoming.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "missing parameter frame")
                })
            },
            || false,
        )
        .unwrap();
        assert_eq!(received, parameters);

        let wrong_digest = [0; 32];
        let error = receive_skill_parameters(
            parameters.len(),
            wrong_digest,
            || {
                Ok(
                    BrokerFrame::new(BrokerFrameKind::SkillParameterChunk, parameters.to_vec())
                        .unwrap(),
                )
            },
            || false,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn receiver_rejects_digest_mismatch_and_incomplete_snapshots() {
        let temp = tempfile::tempdir().unwrap();
        let root = DirectoryCapability::open(temp.path()).unwrap();
        let mut receiver = WorkspaceSnapshotReceiver::new(root, limits()).unwrap();
        let control = BrokerFrame::control(&BrokerControlMessage::WorkspaceEntry {
            relative_path: "file.txt".into(),
            kind: BrokerWorkspaceEntryKind::File,
            byte_length: 3,
            sha256: Some(Sha256::digest(b"abc").into()),
        })
        .unwrap();
        receiver
            .accept_control(control.decode_control().unwrap())
            .unwrap();
        let chunk = BrokerFrame::new(BrokerFrameKind::WorkspaceChunk, b"abd".to_vec()).unwrap();
        assert_eq!(
            receiver.accept_chunk(&chunk).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(receiver.finish().is_err());
    }

    #[test]
    fn receiver_enforces_depth_and_snapshot_summary() {
        let temp = tempfile::tempdir().unwrap();
        let root = DirectoryCapability::open(temp.path()).unwrap();
        let shallow_limits = DirectoryTreeCopyLimits::new(16, 1, 1024, 4096).unwrap();
        let mut receiver = WorkspaceSnapshotReceiver::new(root, shallow_limits).unwrap();
        let directory = BrokerFrame::control(&BrokerControlMessage::WorkspaceEntry {
            relative_path: "a".into(),
            kind: BrokerWorkspaceEntryKind::Directory,
            byte_length: 0,
            sha256: None,
        })
        .unwrap();
        receiver
            .accept_control(directory.decode_control().unwrap())
            .unwrap();
        let nested = BrokerFrame::control(&BrokerControlMessage::WorkspaceEntry {
            relative_path: "a/b".into(),
            kind: BrokerWorkspaceEntryKind::Directory,
            byte_length: 0,
            sha256: None,
        })
        .unwrap();
        assert_eq!(
            receiver
                .accept_control(nested.decode_control().unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let temp = tempfile::tempdir().unwrap();
        let root = DirectoryCapability::open(temp.path()).unwrap();
        let mut receiver = WorkspaceSnapshotReceiver::new(root, limits()).unwrap();
        let incorrect_summary =
            BrokerFrame::control(&BrokerControlMessage::WorkspaceSnapshotComplete {
                entry_count: 1,
                total_bytes: 0,
            })
            .unwrap();
        assert_eq!(
            receiver
                .accept_control(incorrect_summary.decode_control().unwrap())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn sender_fails_closed_on_reparse_points() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        std::fs::write(target.path().join("target.txt"), b"target").unwrap();
        #[cfg(windows)]
        let symlink_result = std::os::windows::fs::symlink_file(
            target.path().join("target.txt"),
            source.path().join("link"),
        );
        #[cfg(unix)]
        let symlink_result = std::os::unix::fs::symlink(
            target.path().join("target.txt"),
            source.path().join("link"),
        );
        if symlink_result.is_err() {
            // Windows CI may not grant symlink privileges to the test process.
            return;
        }

        let source = DirectoryCapability::open(source.path()).unwrap();
        let error = send_workspace_snapshot(&source, limits(), |_| Ok(()), || false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn sender_checks_cancellation_before_emitting_entries() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("file.txt"), b"data").unwrap();
        let source = DirectoryCapability::open(source.path()).unwrap();
        let mut sent_frames = 0;
        let error = send_workspace_snapshot(
            &source,
            limits(),
            |_| {
                sent_frames += 1;
                Ok(())
            },
            || true,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(sent_frames, 0);
    }

    #[test]
    fn transport_receiver_reads_until_a_valid_snapshot_completion() {
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("payload.bin"), b"payload").unwrap();
        let source = DirectoryCapability::open(source.path()).unwrap();
        let mut frames = Vec::new();
        let sent = send_workspace_snapshot(
            &source,
            limits(),
            |frame| {
                frames.push(frame);
                Ok(())
            },
            || false,
        )
        .unwrap();
        let destination = tempfile::tempdir().unwrap();
        let staging = DirectoryCapability::open(destination.path()).unwrap();
        let mut frames = frames.into_iter();

        let (received_root, received) = receive_workspace_snapshot(
            staging,
            limits(),
            || {
                frames.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::UnexpectedEof, "snapshot frames ended")
                })
            },
            || false,
        )
        .unwrap();

        assert_eq!(sent, received);
        assert_eq!(
            std::fs::read(received_root.root().join("payload.bin")).unwrap(),
            b"payload"
        );
        assert!(frames.next().is_none());
    }

    #[test]
    fn transport_receiver_cancels_before_reading_the_next_frame() {
        let destination = tempfile::tempdir().unwrap();
        let staging = DirectoryCapability::open(destination.path()).unwrap();
        let mut read_calls = 0;
        let error = receive_workspace_snapshot(
            staging,
            limits(),
            || {
                read_calls += 1;
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "receiver should have cancelled first",
                ))
            },
            || true,
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(read_calls, 0);
    }
}
