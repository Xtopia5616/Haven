//! Typed platform boundary for brokered Windows Container executions.
//!
//! The port carries workspace capabilities and typed command policy. A
//! broker implementation must stream files through the capability rather
//! than serialize its root path. Authorization proofs and session identities
//! remain with Tools and are deliberately absent from this contract.

use crate::SandboxOutputReview;
use crate::filesystem::{
    DirectoryCapability, DirectoryTreeCopyLimits, DirectoryTreeCopySummary, DirectoryTreeDiffLimits,
};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

const BROKER_SERVICE_NAME: &str = "HavenContainerBroker";

pub(crate) mod broker_protocol;
pub(crate) mod broker_workspace;
pub(crate) mod image_catalog;
pub(crate) mod output_transfer;
pub(crate) mod workspace_transfer;

#[cfg(windows)]
pub(crate) mod named_pipe;

#[cfg(windows)]
mod broker_execution;

#[cfg(windows)]
mod broker_provider;

/// Build Haven's authenticated local broker provider for Windows Shell work.
/// The service still rejects execution until its storage quota is attested.
#[cfg(windows)]
pub fn new_brokered_execution_port() -> std::sync::Arc<dyn WindowsContainerExecutionPort> {
    std::sync::Arc::new(broker_provider::BrokerExecutionProvider)
}

#[cfg(windows)]
pub(crate) mod hcs_export_worker;

#[cfg(windows)]
pub mod broker_service;

const MAX_COMMAND_BYTES: usize = 256 * 1024;
const MAX_STORAGE_QUOTA_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_STDIO_BYTES_PER_STREAM: usize = 64 * 1024 * 1024;
const MAX_EXECUTION_TIMEOUT: Duration =
    Duration::from_secs(haven_common::config::MAX_TOOL_TIMEOUT_SECS);

/// Guest shell selected by the trusted Tools shell configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsContainerShell {
    Cmd,
    PowerShell,
}

/// Output stream identity for bounded live previews.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowsContainerOutputStream {
    Stdout,
    Stderr,
}

/// Authorized workspace input whose host root is not exposed to an execution
/// provider. Providers can create only a bounded copy into private staging.
pub struct WindowsContainerWorkspaceInput {
    source: DirectoryCapability,
}

/// Bounded stdout/stderr retained after the provider drains the guest pipes.
pub struct WindowsContainerCapturedOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl WindowsContainerCapturedOutput {
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    pub fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }
}

/// Enforces per-stream output ceilings before retaining or publishing bytes.
/// Providers must keep draining guest pipes after this sink truncates output.
pub struct WindowsContainerOutputSink<'a> {
    output: WindowsContainerCapturedOutput,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
    on_output: &'a mut dyn FnMut(WindowsContainerOutputStream, &[u8]),
}

impl<'a> WindowsContainerOutputSink<'a> {
    pub fn new(
        request: &WindowsContainerExecutionRequest,
        on_output: &'a mut dyn FnMut(WindowsContainerOutputStream, &[u8]),
    ) -> Self {
        Self {
            output: WindowsContainerCapturedOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
            max_stdout_bytes: request.limits.max_stdout_bytes,
            max_stderr_bytes: request.limits.max_stderr_bytes,
            on_output,
        }
    }

    /// Retain and publish only bytes remaining under the selected stream's
    /// cap. Excess bytes are discarded and mark the final stream truncated.
    pub fn write_chunk(&mut self, stream: WindowsContainerOutputStream, bytes: &[u8]) {
        let (retained, truncated, maximum) = match stream {
            WindowsContainerOutputStream::Stdout => (
                &mut self.output.stdout,
                &mut self.output.stdout_truncated,
                self.max_stdout_bytes,
            ),
            WindowsContainerOutputStream::Stderr => (
                &mut self.output.stderr,
                &mut self.output.stderr_truncated,
                self.max_stderr_bytes,
            ),
        };
        let retained_bytes = bytes.len().min(maximum.saturating_sub(retained.len()));
        if retained_bytes < bytes.len() {
            *truncated = true;
        }
        if retained_bytes > 0 {
            retained.extend_from_slice(&bytes[..retained_bytes]);
            (self.on_output)(stream, &bytes[..retained_bytes]);
        }
    }

    pub fn finish(self) -> WindowsContainerCapturedOutput {
        self.output
    }
}

impl WindowsContainerWorkspaceInput {
    pub fn new(source: DirectoryCapability) -> Self {
        Self { source }
    }

    /// Copy the pinned source tree into an empty private staging capability.
    /// The source root remains hidden; the caller must discard a partial
    /// destination after any error.
    pub fn copy_snapshot_to(
        &self,
        private_staging: &DirectoryCapability,
        limits: DirectoryTreeCopyLimits,
        is_cancelled: impl FnMut() -> bool,
    ) -> io::Result<DirectoryTreeCopySummary> {
        self.source
            .copy_tree_to(private_staging, limits, is_cancelled)
    }
}

/// Per-attempt limits supplied by the Tools owner and independently enforced
/// by the broker. The workspace copy/diff limits are themselves validated
/// platform types with hard ceilings.
#[derive(Debug, Clone)]
pub struct WindowsContainerExecutionLimits {
    storage_quota_bytes: u64,
    timeout: Duration,
    max_stdout_bytes: usize,
    max_stderr_bytes: usize,
    workspace_copy: DirectoryTreeCopyLimits,
    output_diff: DirectoryTreeDiffLimits,
}

impl WindowsContainerExecutionLimits {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        storage_quota_bytes: u64,
        timeout: Duration,
        max_stdout_bytes: usize,
        max_stderr_bytes: usize,
        workspace_copy: DirectoryTreeCopyLimits,
        output_diff: DirectoryTreeDiffLimits,
    ) -> io::Result<Self> {
        if storage_quota_bytes == 0
            || storage_quota_bytes > MAX_STORAGE_QUOTA_BYTES
            || timeout.is_zero()
            || timeout > MAX_EXECUTION_TIMEOUT
            || max_stdout_bytes == 0
            || max_stdout_bytes > MAX_STDIO_BYTES_PER_STREAM
            || max_stderr_bytes == 0
            || max_stderr_bytes > MAX_STDIO_BYTES_PER_STREAM
        {
            return Err(invalid_request(
                "container execution limits are outside the supported range",
            ));
        }
        Ok(Self {
            storage_quota_bytes,
            timeout,
            max_stdout_bytes,
            max_stderr_bytes,
            workspace_copy,
            output_diff,
        })
    }

    pub fn storage_quota_bytes(&self) -> u64 {
        self.storage_quota_bytes
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub fn max_stdout_bytes(&self) -> usize {
        self.max_stdout_bytes
    }

    pub fn max_stderr_bytes(&self) -> usize {
        self.max_stderr_bytes
    }

    pub fn workspace_copy(&self) -> DirectoryTreeCopyLimits {
        self.workspace_copy
    }

    pub fn output_diff(&self) -> DirectoryTreeDiffLimits {
        self.output_diff
    }
}

/// One authorized Shell attempt expressed without host paths or permissions.
#[derive(Debug, Clone)]
pub struct WindowsContainerExecutionRequest {
    shell: WindowsContainerShell,
    command: String,
    working_directory: PathBuf,
    execution_workspace_generation: u64,
    policy_revision: u64,
    limits: WindowsContainerExecutionLimits,
}

impl WindowsContainerExecutionRequest {
    pub fn new(
        shell: WindowsContainerShell,
        command: impl Into<String>,
        working_directory: impl Into<PathBuf>,
        execution_workspace_generation: u64,
        policy_revision: u64,
        limits: WindowsContainerExecutionLimits,
    ) -> io::Result<Self> {
        let command = command.into();
        let working_directory = working_directory.into();
        if command.trim().is_empty()
            || command.len() > MAX_COMMAND_BYTES
            || command.chars().any(|character| character == '\0')
        {
            return Err(invalid_request("container command is invalid or too large"));
        }
        validate_relative_working_directory(&working_directory)?;
        if execution_workspace_generation == 0 || policy_revision == 0 {
            return Err(invalid_request(
                "container execution requires workspace and policy stamps",
            ));
        }

        Ok(Self {
            shell,
            command,
            working_directory,
            execution_workspace_generation,
            policy_revision,
            limits,
        })
    }

    pub fn shell(&self) -> WindowsContainerShell {
        self.shell
    }

    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub fn execution_workspace_generation(&self) -> u64 {
        self.execution_workspace_generation
    }

    pub fn policy_revision(&self) -> u64 {
        self.policy_revision
    }

    pub fn limits(&self) -> &WindowsContainerExecutionLimits {
        &self.limits
    }
}

/// Completed, bounded command output and its private review owner.
pub struct WindowsContainerExecutionResult {
    exit_code: u32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    output_review: SandboxOutputReview,
}

impl WindowsContainerExecutionResult {
    /// Build a result after the provider has drained the guest process tree.
    pub fn new(
        exit_code: u32,
        output: WindowsContainerCapturedOutput,
        output_review: SandboxOutputReview,
    ) -> Self {
        Self {
            exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            stdout_truncated: output.stdout_truncated,
            stderr_truncated: output.stderr_truncated,
            output_review,
        }
    }

    pub fn exit_code(&self) -> u32 {
        self.exit_code
    }

    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }

    pub fn stdout_truncated(&self) -> bool {
        self.stdout_truncated
    }

    pub fn stderr_truncated(&self) -> bool {
        self.stderr_truncated
    }

    pub fn output_review(&self) -> &SandboxOutputReview {
        &self.output_review
    }

    pub fn into_output_review(self) -> SandboxOutputReview {
        self.output_review
    }
}

/// OS/broker implementation for one already-authorized Shell attempt.
///
/// Implementations must finish draining/terminating the guest process tree
/// before returning. They receive no proof or session identity. The source
/// workspace root is hidden behind `WindowsContainerWorkspaceInput`; only a
/// bounded copy can be made into private staging. Absolute paths must never
/// cross broker IPC. Output callbacks must never receive more than the
/// request's per-stream limit; the provided output sink enforces this before
/// retaining or forwarding bytes. Providers must keep draining pipes after
/// truncation. `Err` on cancellation, timeout, staging failure, or unproven
/// quota always fails the execution.
pub trait WindowsContainerExecutionPort: private::Sealed + Send + Sync {
    fn execute(
        &self,
        request: &WindowsContainerExecutionRequest,
        workspace: &WindowsContainerWorkspaceInput,
        is_cancelled: &mut (dyn FnMut() -> bool + Send),
        output: WindowsContainerOutputSink<'_>,
    ) -> io::Result<WindowsContainerExecutionResult>;
}

// Providers are implemented and selected inside haven-platform. Tools may
// consume the port but cannot install an arbitrary implementation.
mod private {
    pub trait Sealed {}
}

fn validate_relative_working_directory(path: &Path) -> io::Result<()> {
    if path.is_absolute()
        || path.components().any(|component| match component {
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => true,
            Component::Normal(name) => name.to_string_lossy().contains(':'),
        })
        || path.to_string_lossy().chars().any(char::is_control)
    {
        return Err(invalid_request(
            "container working directory must be a clean workspace-relative path",
        ));
    }
    Ok(())
}

fn invalid_request(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::{DirectoryTreeChange, DirectoryTreeFileCopySummary};
    use crate::sandbox_output::SandboxOutputWorkspaceOwner;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct RecordedRequest {
        shell: WindowsContainerShell,
        command: String,
        working_directory: PathBuf,
        workspace_generation: u64,
        policy_revision: u64,
        storage_quota_bytes: u64,
    }

    #[derive(Default)]
    struct MockExecutionPort {
        recorded: Mutex<Option<RecordedRequest>>,
    }

    impl private::Sealed for MockExecutionPort {}

    impl WindowsContainerExecutionPort for MockExecutionPort {
        fn execute(
            &self,
            request: &WindowsContainerExecutionRequest,
            _workspace: &WindowsContainerWorkspaceInput,
            is_cancelled: &mut (dyn FnMut() -> bool + Send),
            mut output: WindowsContainerOutputSink<'_>,
        ) -> io::Result<WindowsContainerExecutionResult> {
            if is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            *self
                .recorded
                .lock()
                .map_err(|_| io::Error::other("mock lock poisoned"))? = Some(RecordedRequest {
                shell: request.shell,
                command: request.command.clone(),
                working_directory: request.working_directory.clone(),
                workspace_generation: request.execution_workspace_generation,
                policy_revision: request.policy_revision,
                storage_quota_bytes: request.limits.storage_quota_bytes,
            });
            output.write_chunk(
                WindowsContainerOutputStream::Stdout,
                b"mock output exceeds cap",
            );
            output.write_chunk(WindowsContainerOutputStream::Stderr, b"mock error");
            Ok(WindowsContainerExecutionResult::new(
                0,
                output.finish(),
                SandboxOutputReview::new(Box::new(MockReviewWorkspace), Default::default()),
            ))
        }
    }

    struct MockReviewWorkspace;

    impl SandboxOutputWorkspaceOwner for MockReviewWorkspace {
        fn execution_workspace_generation(&self) -> u64 {
            17
        }

        fn policy_revision(&self) -> u64 {
            23
        }

        fn initial_snapshot_byte_count(&self) -> u64 {
            0
        }

        fn promote_created_file(
            &self,
            _change: &DirectoryTreeChange,
            _destination: &DirectoryCapability,
            _destination_relative: &Path,
            _is_cancelled: &mut dyn FnMut() -> bool,
        ) -> io::Result<DirectoryTreeFileCopySummary> {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mock review has no output files",
            ))
        }

        fn cleanup(self: Box<Self>) -> io::Result<()> {
            Ok(())
        }
    }

    fn test_limits(max_stdout_bytes: usize) -> io::Result<WindowsContainerExecutionLimits> {
        test_limits_with_output(max_stdout_bytes, 64 * 1024)
    }

    fn test_limits_with_output(
        max_stdout_bytes: usize,
        max_stderr_bytes: usize,
    ) -> io::Result<WindowsContainerExecutionLimits> {
        let copy = DirectoryTreeCopyLimits::new(64, 8, 64 * 1024, 256 * 1024).unwrap();
        let diff = DirectoryTreeDiffLimits::new(copy, 64, 16 * 1024, 64 * 1024).unwrap();
        WindowsContainerExecutionLimits::new(
            1024 * 1024,
            Duration::from_secs(30),
            max_stdout_bytes,
            max_stderr_bytes,
            copy,
            diff,
        )
    }

    #[test]
    fn typed_execution_port_receives_only_scoped_request_and_returns_bounded_review() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            WindowsContainerWorkspaceInput::new(DirectoryCapability::open(temp.path()).unwrap());
        let request = WindowsContainerExecutionRequest::new(
            WindowsContainerShell::PowerShell,
            "Write-Output 'hello'",
            "src",
            17,
            23,
            test_limits(8).unwrap(),
        )
        .unwrap();
        let port = MockExecutionPort::default();
        let output_events = Arc::new(Mutex::new(Vec::new()));
        let received_events = Arc::clone(&output_events);
        let mut on_output = move |stream, bytes: &[u8]| {
            received_events
                .lock()
                .unwrap()
                .push((stream, bytes.to_vec()));
        };
        let sink = WindowsContainerOutputSink::new(&request, &mut on_output);
        let mut cancelled = || false;
        let result = port
            .execute(&request, &workspace, &mut cancelled, sink)
            .unwrap();

        assert_eq!(
            *port.recorded.lock().unwrap(),
            Some(RecordedRequest {
                shell: WindowsContainerShell::PowerShell,
                command: "Write-Output 'hello'".into(),
                working_directory: PathBuf::from("src"),
                workspace_generation: 17,
                policy_revision: 23,
                storage_quota_bytes: 1024 * 1024,
            })
        );
        assert_eq!(result.exit_code(), 0);
        assert_eq!(result.stdout(), b"mock out");
        assert!(result.stdout_truncated());
        assert_eq!(result.stderr(), b"mock error");
        assert!(!result.stderr_truncated());
        assert_eq!(
            *output_events.lock().unwrap(),
            vec![
                (WindowsContainerOutputStream::Stdout, b"mock out".to_vec()),
                (WindowsContainerOutputStream::Stderr, b"mock error".to_vec()),
            ]
        );
        assert!(result.output_review().manifest().changes.is_empty());
    }

    #[test]
    fn execution_request_rejects_zero_limits_and_host_paths() {
        assert!(test_limits(0).is_err());
        let copy = DirectoryTreeCopyLimits::new(1, 1, 1, 1).unwrap();
        let diff = DirectoryTreeDiffLimits::new(copy, 1, 1, 1).unwrap();
        assert!(
            WindowsContainerExecutionLimits::new(0, Duration::from_secs(1), 1, 1, copy, diff,)
                .is_err()
        );
        assert!(
            WindowsContainerExecutionLimits::new(
                MAX_STORAGE_QUOTA_BYTES + 1,
                Duration::from_secs(1),
                1,
                1,
                copy,
                diff,
            )
            .is_err()
        );
        assert!(
            WindowsContainerExecutionLimits::new(
                1,
                Duration::from_secs(haven_common::config::MAX_TOOL_TIMEOUT_SECS + 1),
                1,
                1,
                copy,
                diff,
            )
            .is_err()
        );

        for path in [
            PathBuf::from("..\\outside"),
            PathBuf::from("C:\\host\\path"),
            PathBuf::from("\\\\server\\share"),
            PathBuf::from("src:stream"),
        ] {
            assert!(
                WindowsContainerExecutionRequest::new(
                    WindowsContainerShell::Cmd,
                    "echo hi",
                    path,
                    1,
                    1,
                    test_limits(1).unwrap(),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn cancelled_execution_fails_before_the_provider_starts() {
        let temp = tempfile::tempdir().unwrap();
        let workspace =
            WindowsContainerWorkspaceInput::new(DirectoryCapability::open(temp.path()).unwrap());
        let request = WindowsContainerExecutionRequest::new(
            WindowsContainerShell::Cmd,
            "echo hi",
            "",
            17,
            23,
            test_limits(8).unwrap(),
        )
        .unwrap();
        let port = MockExecutionPort::default();
        let mut cancelled = || true;
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let sink = WindowsContainerOutputSink::new(&request, &mut on_output);

        let error = match port.execute(&request, &workspace, &mut cancelled, sink) {
            Ok(_) => panic!("cancelled execution unexpectedly succeeded"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(port.recorded.lock().unwrap().is_none());
    }

    #[test]
    fn output_sink_caps_retained_and_live_bytes_per_stream() {
        let request = WindowsContainerExecutionRequest::new(
            WindowsContainerShell::Cmd,
            "echo hi",
            "",
            1,
            1,
            test_limits_with_output(4, 4).unwrap(),
        )
        .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let received_events = Arc::clone(&events);
        let mut on_output = move |stream, bytes: &[u8]| {
            received_events
                .lock()
                .unwrap()
                .push((stream, bytes.to_vec()));
        };
        let mut sink = WindowsContainerOutputSink::new(&request, &mut on_output);

        sink.write_chunk(WindowsContainerOutputStream::Stdout, b"abc");
        sink.write_chunk(WindowsContainerOutputStream::Stdout, b"def");
        sink.write_chunk(WindowsContainerOutputStream::Stderr, b"123456");
        let captured = sink.finish();

        assert_eq!(captured.stdout(), b"abcd");
        assert!(captured.stdout_truncated());
        assert_eq!(captured.stderr(), b"1234");
        assert!(captured.stderr_truncated());
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                (WindowsContainerOutputStream::Stdout, b"abc".to_vec()),
                (WindowsContainerOutputStream::Stdout, b"d".to_vec()),
                (WindowsContainerOutputStream::Stderr, b"1234".to_vec()),
            ]
        );
    }

    #[test]
    fn workspace_input_copies_a_bounded_tree_without_exposing_its_root() {
        use std::io::Read;

        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir(source.path().join("src")).unwrap();
        std::fs::write(source.path().join("src/main.rs"), b"fn main() {}\n").unwrap();
        let staging = tempfile::tempdir().unwrap();
        let source =
            WindowsContainerWorkspaceInput::new(DirectoryCapability::open(source.path()).unwrap());
        let staging = DirectoryCapability::open(staging.path()).unwrap();
        let limits = DirectoryTreeCopyLimits::new(8, 4, 1024, 4096).unwrap();

        let summary = source.copy_snapshot_to(&staging, limits, || false).unwrap();

        assert_eq!(summary.file_count, 1);
        assert_eq!(summary.byte_count, 13);
        let mut copied = staging.open_read(Path::new("src/main.rs")).unwrap();
        let mut contents = Vec::new();
        copied.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"fn main() {}\n");
    }
}
