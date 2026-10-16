//! Versioned, length-bounded messages for the Windows Container broker.
//!
//! This module defines a transport contract only. It does not authenticate a
//! named-pipe peer, attest HCS quota enforcement, or enable an execution
//! backend. Paths in messages are workspace-relative UTF-8 strings; host roots,
//! HCS JSON, authorization proofs, session identities, and executable paths
//! are deliberately absent.

use super::{
    MAX_COMMAND_BYTES, MAX_EXECUTION_TIMEOUT, MAX_STDIO_BYTES_PER_STREAM, MAX_STORAGE_QUOTA_BYTES,
    WindowsContainerExecutionLimits, WindowsContainerExecutionRequest, WindowsContainerShell,
    validate_relative_working_directory,
};
use crate::filesystem::{DirectoryTreeCopyLimits, DirectoryTreeDiffLimits};
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

pub(crate) const BROKER_PROTOCOL_VERSION: u16 = 1;
pub(crate) const MAX_CONTROL_PAYLOAD_BYTES: usize = 320 * 1024;
const MAX_BINARY_CHUNK_BYTES: usize = 64 * 1024;
const MAX_WIRE_PATH_BYTES: usize = 32 * 1024;
const MAX_WORKSPACE_ENTRIES: u32 = 100_000;
const MAX_WORKSPACE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_OUTPUT_ENTRIES: u32 = 4_096;
const MAX_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Frame kinds are fixed and version-independent so a receiver can reject an
/// unknown kind before interpreting a control payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum BrokerFrameKind {
    Control = 1,
    WorkspaceChunk = 2,
    StdoutChunk = 3,
    StderrChunk = 4,
    OutputFileChunk = 5,
}

impl TryFrom<u8> for BrokerFrameKind {
    type Error = io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Control),
            2 => Ok(Self::WorkspaceChunk),
            3 => Ok(Self::StdoutChunk),
            4 => Ok(Self::StderrChunk),
            5 => Ok(Self::OutputFileChunk),
            _ => Err(invalid_data("broker frame kind is unsupported")),
        }
    }
}

/// One bounded frame. Control payloads use JSON; binary payloads are raw file
/// or process bytes and must be at most 64 KiB per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokerFrame {
    kind: BrokerFrameKind,
    payload: Vec<u8>,
}

impl BrokerFrame {
    pub(crate) fn new(kind: BrokerFrameKind, payload: Vec<u8>) -> io::Result<Self> {
        validate_payload_size(kind, payload.len())?;
        Ok(Self { kind, payload })
    }

    pub(crate) fn control(message: &BrokerControlMessage) -> io::Result<Self> {
        message.validate()?;
        let payload = serde_json::to_vec(message)
            .map_err(|_| invalid_data("broker control message could not be serialized"))?;
        Self::new(BrokerFrameKind::Control, payload)
    }

    pub(crate) fn decode_control(&self) -> io::Result<BrokerControlMessage> {
        if self.kind != BrokerFrameKind::Control {
            return Err(invalid_data("broker frame is not a control message"));
        }
        let message: BrokerControlMessage = serde_json::from_slice(&self.payload)
            .map_err(|_| invalid_data("broker control message is malformed"))?;
        message.validate()?;
        Ok(message)
    }

    pub(crate) fn kind(&self) -> BrokerFrameKind {
        self.kind
    }

    pub(crate) fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Encode a little-endian byte count followed by kind and payload.
    pub(crate) fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        validate_payload_size(self.kind, self.payload.len())?;
        let body_length = self
            .payload
            .len()
            .checked_add(1)
            .and_then(|length| u32::try_from(length).ok())
            .ok_or_else(|| invalid_data("broker frame length is invalid"))?;
        writer.write_all(&body_length.to_le_bytes())?;
        writer.write_all(&[self.kind as u8])?;
        writer.write_all(&self.payload)
    }

    /// Read one frame while validating its declared size before allocation.
    pub(crate) fn read_from(reader: &mut impl Read) -> io::Result<Self> {
        let mut length_bytes = [0_u8; 4];
        reader.read_exact(&mut length_bytes)?;
        let body_length = u32::from_le_bytes(length_bytes) as usize;
        if !(1..=MAX_CONTROL_PAYLOAD_BYTES + 1).contains(&body_length) {
            return Err(invalid_data("broker frame length is outside its limit"));
        }

        let mut body = vec![0_u8; body_length];
        reader.read_exact(&mut body)?;
        let kind = BrokerFrameKind::try_from(body[0])?;
        let payload = body.split_off(1);
        validate_payload_size(kind, payload.len())?;
        Ok(Self { kind, payload })
    }
}

fn validate_payload_size(kind: BrokerFrameKind, payload_length: usize) -> io::Result<()> {
    let maximum = match kind {
        BrokerFrameKind::Control => MAX_CONTROL_PAYLOAD_BYTES,
        BrokerFrameKind::WorkspaceChunk
        | BrokerFrameKind::StdoutChunk
        | BrokerFrameKind::StderrChunk
        | BrokerFrameKind::OutputFileChunk => MAX_BINARY_CHUNK_BYTES,
    };
    if payload_length > maximum {
        return Err(invalid_data("broker frame payload exceeds its limit"));
    }
    Ok(())
}

/// Fixed control messages. Error variants intentionally carry no raw OS text,
/// command, path, or provider-controlled detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum BrokerControlMessage {
    Hello {
        minimum_version: u16,
        maximum_version: u16,
    },
    HelloAccepted {
        selected_version: u16,
    },
    Execute {
        request: BrokerExecutionRequest,
    },
    WorkspaceEntry {
        relative_path: String,
        kind: BrokerWorkspaceEntryKind,
        byte_length: u64,
        sha256: Option<[u8; 32]>,
    },
    WorkspaceSnapshotComplete {
        entry_count: u32,
        total_bytes: u64,
    },
    Cancel {
        request_id: [u8; 16],
    },
    OutputEntry {
        index: u32,
        relative_path: String,
        change: BrokerOutputChangeKind,
        before_kind: Option<BrokerWorkspaceEntryKind>,
        after_kind: Option<BrokerWorkspaceEntryKind>,
        before_bytes: Option<u64>,
        after_bytes: Option<u64>,
        before_sha256: Option<[u8; 32]>,
        after_sha256: Option<[u8; 32]>,
    },
    OutputSnapshotComplete {
        entry_count: u32,
        changed_bytes: u64,
        initial_snapshot_bytes: u64,
    },
    ExecutionComplete {
        request_id: [u8; 16],
        exit_code: u32,
        stdout_truncated: bool,
        stderr_truncated: bool,
    },
    Failure {
        request_id: Option<[u8; 16]>,
        code: BrokerFailureCode,
    },
}

impl BrokerControlMessage {
    fn validate(&self) -> io::Result<()> {
        match self {
            Self::Hello {
                minimum_version,
                maximum_version,
            } => negotiate_version(*minimum_version, *maximum_version).map(|_| ()),
            Self::HelloAccepted { selected_version }
                if *selected_version == BROKER_PROTOCOL_VERSION =>
            {
                Ok(())
            }
            Self::HelloAccepted { .. } => Err(invalid_data(
                "broker selected an unsupported protocol version",
            )),
            Self::Execute { request } => request
                .validate()
                .map(|_| ())
                .map_err(|_| invalid_data("broker execution request is invalid")),
            Self::WorkspaceEntry {
                relative_path,
                kind,
                byte_length,
                sha256,
            } => {
                validate_wire_relative_path(relative_path)?;
                match kind {
                    BrokerWorkspaceEntryKind::Directory
                        if *byte_length == 0 && sha256.is_none() =>
                    {
                        Ok(())
                    }
                    BrokerWorkspaceEntryKind::File
                        if sha256.is_some() && *byte_length <= MAX_WORKSPACE_BYTES =>
                    {
                        Ok(())
                    }
                    _ => Err(invalid_data("workspace entry metadata is inconsistent")),
                }
            }
            Self::WorkspaceSnapshotComplete {
                entry_count,
                total_bytes,
            } if *entry_count <= MAX_WORKSPACE_ENTRIES && *total_bytes <= MAX_WORKSPACE_BYTES => {
                Ok(())
            }
            Self::WorkspaceSnapshotComplete { .. } => Err(invalid_data(
                "workspace snapshot exceeds its transfer limits",
            )),
            Self::Cancel { request_id } | Self::ExecutionComplete { request_id, .. }
                if request_id.iter().all(|byte| *byte == 0) =>
            {
                Err(invalid_data("broker request ID is invalid"))
            }
            Self::Cancel { .. } | Self::ExecutionComplete { .. } => Ok(()),
            Self::OutputEntry {
                relative_path,
                change,
                before_kind,
                after_kind,
                before_bytes,
                after_bytes,
                before_sha256,
                after_sha256,
                index,
            } => {
                validate_wire_relative_path(relative_path)?;
                if *index >= MAX_OUTPUT_ENTRIES {
                    return Err(invalid_data("output entry index exceeds its limit"));
                }
                validate_file_state(
                    *before_kind,
                    *before_bytes,
                    *before_sha256,
                    MAX_OUTPUT_BYTES,
                )?;
                validate_file_state(*after_kind, *after_bytes, *after_sha256, MAX_OUTPUT_BYTES)?;
                let valid_change = match change {
                    BrokerOutputChangeKind::Created => {
                        before_kind.is_none() && after_kind.is_some()
                    }
                    BrokerOutputChangeKind::Modified => {
                        matches!(before_kind, Some(BrokerWorkspaceEntryKind::File))
                            && matches!(after_kind, Some(BrokerWorkspaceEntryKind::File))
                    }
                    BrokerOutputChangeKind::Deleted => {
                        before_kind.is_some() && after_kind.is_none()
                    }
                    BrokerOutputChangeKind::TypeChanged => {
                        before_kind.is_some() && after_kind.is_some() && before_kind != after_kind
                    }
                };
                if valid_change {
                    Ok(())
                } else {
                    Err(invalid_data("output entry metadata is inconsistent"))
                }
            }
            Self::OutputSnapshotComplete {
                entry_count,
                changed_bytes,
                initial_snapshot_bytes,
            } if *entry_count <= MAX_OUTPUT_ENTRIES
                && *changed_bytes <= MAX_OUTPUT_BYTES
                && *initial_snapshot_bytes <= MAX_WORKSPACE_BYTES =>
            {
                Ok(())
            }
            Self::OutputSnapshotComplete { .. } => {
                Err(invalid_data("output snapshot exceeds its transfer limits"))
            }
            Self::Failure {
                request_id: Some(request_id),
                ..
            } if request_id.iter().all(|byte| *byte == 0) => {
                Err(invalid_data("broker request ID is invalid"))
            }
            Self::Failure { .. } => Ok(()),
        }
    }
}

fn validate_file_state(
    kind: Option<BrokerWorkspaceEntryKind>,
    byte_length: Option<u64>,
    sha256: Option<[u8; 32]>,
    max_file_bytes: u64,
) -> io::Result<()> {
    let consistent = match kind {
        None => byte_length.is_none() && sha256.is_none(),
        Some(BrokerWorkspaceEntryKind::Directory) => byte_length.is_none() && sha256.is_none(),
        Some(BrokerWorkspaceEntryKind::File) => {
            byte_length.is_some_and(|bytes| bytes <= max_file_bytes) && sha256.is_some()
        }
    };
    if consistent {
        Ok(())
    } else {
        Err(invalid_data("file state metadata is inconsistent"))
    }
}

fn validate_wire_relative_path(path: &str) -> io::Result<()> {
    if path.is_empty()
        || path.len() > MAX_WIRE_PATH_BYTES
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
        || path
            .chars()
            .any(|character| matches!(character, '<' | '>' | '"' | '|' | '?' | '*'))
        || path.chars().any(char::is_control)
        || path.split('/').any(|component| {
            component.is_empty()
                || component.encode_utf16().count() > 255
                || component == "."
                || component == ".."
                || component.ends_with('.')
                || component.ends_with(' ')
                || is_reserved_windows_component(component)
        })
    {
        return Err(invalid_data("broker path is not a clean relative path"));
    }
    Ok(())
}

fn is_reserved_windows_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrokerWorkspaceEntryKind {
    Directory,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrokerOutputChangeKind {
    Created,
    Modified,
    Deleted,
    TypeChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrokerFailureCode {
    UnsupportedProtocolVersion,
    BackendUnavailable,
    UnauthorizedPeer,
    InvalidRequest,
    PolicyRevoked,
    Cancelled,
    TimedOut,
    QuotaUnattested,
    StagingFailed,
    HcsFailed,
    CleanupFailed,
}

/// Attempt data sent across the broker boundary. It has no host root, tool
/// proof, session identity, executable path, environment override, or HCS JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerExecutionRequest {
    pub(crate) request_id: [u8; 16],
    pub(crate) shell: BrokerShell,
    pub(crate) command: String,
    pub(crate) working_directory: String,
    pub(crate) execution_workspace_generation: u64,
    pub(crate) policy_revision: u64,
    pub(crate) storage_quota_bytes: u64,
    pub(crate) timeout_millis: u64,
    pub(crate) max_stdout_bytes: u32,
    pub(crate) max_stderr_bytes: u32,
    pub(crate) workspace_copy: BrokerTreeLimits,
    pub(crate) output_diff: BrokerDiffLimits,
}

impl BrokerExecutionRequest {
    pub(crate) fn from_execution_request(
        request: &WindowsContainerExecutionRequest,
        request_id: [u8; 16],
    ) -> io::Result<Self> {
        let working_directory = request
            .working_directory
            .components()
            .map(|component| match component {
                std::path::Component::Normal(name) => name
                    .to_str()
                    .ok_or_else(|| invalid_request("workspace-relative cwd is not valid Unicode")),
                _ => Err(invalid_request("workspace-relative cwd is invalid")),
            })
            .collect::<io::Result<Vec<_>>>()?
            .join("/");
        let limits = &request.limits;
        let workspace_copy = BrokerTreeLimits::from_limits(limits.workspace_copy);
        let output_diff = BrokerDiffLimits::from_limits(limits.output_diff);
        let wire = Self {
            request_id,
            shell: match request.shell {
                WindowsContainerShell::Cmd => BrokerShell::Cmd,
                WindowsContainerShell::PowerShell => BrokerShell::PowerShell,
            },
            command: request.command.clone(),
            working_directory,
            execution_workspace_generation: request.execution_workspace_generation,
            policy_revision: request.policy_revision,
            storage_quota_bytes: limits.storage_quota_bytes,
            timeout_millis: u64::try_from(limits.timeout.as_millis())
                .map_err(|_| invalid_request("execution timeout is outside the wire range"))?,
            max_stdout_bytes: u32::try_from(limits.max_stdout_bytes)
                .map_err(|_| invalid_request("stdout limit is outside the wire range"))?,
            max_stderr_bytes: u32::try_from(limits.max_stderr_bytes)
                .map_err(|_| invalid_request("stderr limit is outside the wire range"))?,
            workspace_copy,
            output_diff,
        };
        wire.validate()?;
        Ok(wire)
    }

    /// Revalidate untrusted bytes and rebuild the platform limits on the
    /// service side before any staging or HCS call.
    pub(crate) fn validate(&self) -> io::Result<WindowsContainerExecutionLimits> {
        if self.request_id.iter().all(|byte| *byte == 0)
            || self.command.trim().is_empty()
            || self.command.len() > MAX_COMMAND_BYTES
            || self.command.chars().any(|character| character == '\0')
            || self.working_directory.len() > MAX_WIRE_PATH_BYTES
            || self.execution_workspace_generation == 0
            || self.policy_revision == 0
            || self.storage_quota_bytes == 0
            || self.storage_quota_bytes > MAX_STORAGE_QUOTA_BYTES
            || self.timeout_millis == 0
            || self.timeout_millis > MAX_EXECUTION_TIMEOUT.as_millis() as u64
            || self.max_stdout_bytes == 0
            || self.max_stdout_bytes as usize > MAX_STDIO_BYTES_PER_STREAM
            || self.max_stderr_bytes == 0
            || self.max_stderr_bytes as usize > MAX_STDIO_BYTES_PER_STREAM
        {
            return Err(invalid_request(
                "broker execution request is outside its limits",
            ));
        }
        if !self.working_directory.is_empty() {
            validate_wire_relative_path(&self.working_directory)
                .map_err(|_| invalid_request("workspace-relative cwd is invalid"))?;
        }
        validate_relative_working_directory(&PathBuf::from(&self.working_directory))?;
        let workspace_copy = self.workspace_copy.to_platform_limits()?;
        let output_diff = self.output_diff.to_platform_limits()?;
        WindowsContainerExecutionLimits::new(
            self.storage_quota_bytes,
            Duration::from_millis(self.timeout_millis),
            self.max_stdout_bytes as usize,
            self.max_stderr_bytes as usize,
            workspace_copy,
            output_diff,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BrokerShell {
    Cmd,
    PowerShell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerTreeLimits {
    pub(crate) max_entries: u32,
    pub(crate) max_depth: u32,
    pub(crate) max_file_bytes: u64,
    pub(crate) max_total_bytes: u64,
}

impl BrokerTreeLimits {
    fn from_limits(limits: DirectoryTreeCopyLimits) -> Self {
        Self {
            max_entries: limits.max_entries() as u32,
            max_depth: limits.max_depth() as u32,
            max_file_bytes: limits.max_file_bytes(),
            max_total_bytes: limits.max_total_bytes(),
        }
    }

    fn to_platform_limits(self) -> io::Result<DirectoryTreeCopyLimits> {
        DirectoryTreeCopyLimits::new(
            self.max_entries as usize,
            self.max_depth as usize,
            self.max_file_bytes,
            self.max_total_bytes,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BrokerDiffLimits {
    pub(crate) scan: BrokerTreeLimits,
    pub(crate) max_changes: u32,
    pub(crate) max_changed_file_bytes: u64,
    pub(crate) max_changed_total_bytes: u64,
}

impl BrokerDiffLimits {
    fn from_limits(limits: DirectoryTreeDiffLimits) -> Self {
        Self {
            scan: BrokerTreeLimits::from_limits(limits.scan()),
            max_changes: limits.max_changes() as u32,
            max_changed_file_bytes: limits.max_changed_file_bytes(),
            max_changed_total_bytes: limits.max_changed_total_bytes(),
        }
    }

    fn to_platform_limits(self) -> io::Result<DirectoryTreeDiffLimits> {
        DirectoryTreeDiffLimits::new(
            self.scan.to_platform_limits()?,
            self.max_changes as usize,
            self.max_changed_file_bytes,
            self.max_changed_total_bytes,
        )
    }
}

/// Negotiate only the protocol version implemented by this binary.
pub(crate) fn negotiate_version(minimum: u16, maximum: u16) -> io::Result<u16> {
    if minimum == 0 || minimum > maximum {
        return Err(invalid_data("broker protocol range is invalid"));
    }
    if minimum <= BROKER_PROTOCOL_VERSION && BROKER_PROTOCOL_VERSION <= maximum {
        Ok(BROKER_PROTOCOL_VERSION)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "broker protocol version is unsupported",
        ))
    }
}

fn invalid_data(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_request(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_request() -> WindowsContainerExecutionRequest {
        let copy = DirectoryTreeCopyLimits::new(32, 8, 64 * 1024, 512 * 1024).unwrap();
        let diff = DirectoryTreeDiffLimits::new(copy, 32, 64 * 1024, 512 * 1024).unwrap();
        let limits = WindowsContainerExecutionLimits::new(
            1024 * 1024,
            Duration::from_secs(30),
            16 * 1024,
            16 * 1024,
            copy,
            diff,
        )
        .unwrap();
        WindowsContainerExecutionRequest::new(
            WindowsContainerShell::PowerShell,
            "Write-Output 'hello'",
            "src",
            7,
            11,
            limits,
        )
        .unwrap()
    }

    #[test]
    fn control_and_binary_frames_round_trip_with_fixed_framing() {
        let control = BrokerFrame::control(&BrokerControlMessage::Hello {
            minimum_version: BROKER_PROTOCOL_VERSION,
            maximum_version: BROKER_PROTOCOL_VERSION,
        })
        .unwrap();
        let binary = BrokerFrame::new(BrokerFrameKind::WorkspaceChunk, vec![0, 1, 2, 255]).unwrap();
        let mut encoded = Vec::new();
        control.write_to(&mut encoded).unwrap();
        binary.write_to(&mut encoded).unwrap();
        let mut reader = Cursor::new(encoded);

        let decoded_control = BrokerFrame::read_from(&mut reader).unwrap();
        assert_eq!(decoded_control.kind(), BrokerFrameKind::Control);
        assert_eq!(
            decoded_control.decode_control().unwrap(),
            BrokerControlMessage::Hello {
                minimum_version: BROKER_PROTOCOL_VERSION,
                maximum_version: BROKER_PROTOCOL_VERSION,
            }
        );
        let decoded_binary = BrokerFrame::read_from(&mut reader).unwrap();
        assert_eq!(decoded_binary.kind(), BrokerFrameKind::WorkspaceChunk);
        assert_eq!(decoded_binary.payload(), &[0, 1, 2, 255]);
    }

    #[test]
    fn frame_reader_rejects_oversized_and_unknown_frames_before_payload_read() {
        let too_large = (MAX_CONTROL_PAYLOAD_BYTES as u32 + 2).to_le_bytes();
        let mut reader = Cursor::new(too_large);
        assert_eq!(
            BrokerFrame::read_from(&mut reader).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(reader.position(), 4);

        let mut unknown = Cursor::new([1_u32.to_le_bytes().as_slice(), &[255]].concat());
        assert_eq!(
            BrokerFrame::read_from(&mut unknown).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn binary_frames_and_control_json_have_independent_size_limits() {
        assert!(
            BrokerFrame::new(
                BrokerFrameKind::StdoutChunk,
                vec![0; MAX_BINARY_CHUNK_BYTES + 1]
            )
            .is_err()
        );
        assert!(
            BrokerFrame::new(
                BrokerFrameKind::Control,
                vec![0; MAX_CONTROL_PAYLOAD_BYTES + 1]
            )
            .is_err()
        );
        assert!(
            BrokerFrame::new(
                BrokerFrameKind::StderrChunk,
                vec![0; MAX_BINARY_CHUNK_BYTES]
            )
            .is_ok()
        );
    }

    #[test]
    fn execution_request_is_typed_revalidated_and_contains_no_host_workspace_root() {
        let request = test_request();
        let wire = BrokerExecutionRequest::from_execution_request(&request, [9; 16]).unwrap();
        let rebuilt = wire.validate().unwrap();
        assert_eq!(rebuilt.storage_quota_bytes(), 1024 * 1024);
        assert_eq!(wire.working_directory, "src");

        let json = serde_json::to_string(&wire).unwrap();
        assert!(!json.contains("D:\\Workspace"));
        assert!(!json.contains("session_id"));
        assert!(!json.contains("proof"));

        let mut invalid = wire;
        invalid.working_directory = r"C:\outside".into();
        assert_eq!(
            invalid.validate().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn execution_request_uses_canonical_slash_paths_and_rejects_unsafe_cwd() {
        let copy = DirectoryTreeCopyLimits::new(32, 8, 64 * 1024, 512 * 1024).unwrap();
        let diff = DirectoryTreeDiffLimits::new(copy, 32, 64 * 1024, 512 * 1024).unwrap();
        let limits = WindowsContainerExecutionLimits::new(
            1024 * 1024,
            Duration::from_secs(30),
            16 * 1024,
            16 * 1024,
            copy,
            diff,
        )
        .unwrap();
        let request = WindowsContainerExecutionRequest::new(
            WindowsContainerShell::PowerShell,
            "Write-Output 'hello'",
            PathBuf::from("src").join("nested"),
            7,
            11,
            limits,
        )
        .unwrap();
        let wire = BrokerExecutionRequest::from_execution_request(&request, [9; 16]).unwrap();
        assert_eq!(wire.working_directory, "src/nested");

        for path in ["../outside", "src//nested", "src/COM1.txt", "src/bad?.txt"] {
            let mut invalid = wire.clone();
            invalid.working_directory = path.into();
            assert_eq!(
                invalid.validate().unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "path should have failed validation: {path}"
            );
        }
    }

    #[test]
    fn control_decoder_rejects_unknown_fields_and_inconsistent_file_metadata() {
        let unknown = BrokerFrame::new(
            BrokerFrameKind::Control,
            br#"{"message":"hello","minimum_version":1,"maximum_version":1,"unexpected":true}"#
                .to_vec(),
        )
        .unwrap();
        assert_eq!(
            unknown.decode_control().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let invalid_workspace_entry = BrokerControlMessage::WorkspaceEntry {
            relative_path: "src/file.txt".into(),
            kind: BrokerWorkspaceEntryKind::File,
            byte_length: 10,
            sha256: None,
        };
        assert!(BrokerFrame::control(&invalid_workspace_entry).is_err());

        let invalid_output_entry = BrokerControlMessage::OutputEntry {
            index: MAX_OUTPUT_ENTRIES,
            relative_path: "src/file.txt".into(),
            change: BrokerOutputChangeKind::Created,
            before_kind: None,
            after_kind: Some(BrokerWorkspaceEntryKind::File),
            before_bytes: None,
            after_bytes: Some(10),
            before_sha256: None,
            after_sha256: Some([7; 32]),
        };
        assert!(BrokerFrame::control(&invalid_output_entry).is_err());
    }

    #[test]
    fn wire_paths_reject_win32_aliases_and_unsupported_characters() {
        for path in [
            "CON",
            "nul.txt",
            "COM1.log",
            "COM³.log",
            "LPT9",
            "CONIN$",
            "dir/name.",
            "dir/name ",
            "dir/a:b",
            "dir/a|b",
            "dir//file",
            "dir/../file",
        ] {
            assert!(
                validate_wire_relative_path(path).is_err(),
                "path should have failed validation: {path}"
            );
        }
        assert!(validate_wire_relative_path("src/nested/file.txt").is_ok());
    }

    #[test]
    fn request_id_and_protocol_range_fail_closed() {
        let request = test_request();
        assert!(BrokerExecutionRequest::from_execution_request(&request, [0; 16]).is_err());
        assert_eq!(
            negotiate_version(BROKER_PROTOCOL_VERSION, BROKER_PROTOCOL_VERSION).unwrap(),
            BROKER_PROTOCOL_VERSION
        );
        assert!(negotiate_version(2, 3).is_err());
        assert!(negotiate_version(0, 1).is_err());
        assert!(negotiate_version(3, 2).is_err());
    }
}
