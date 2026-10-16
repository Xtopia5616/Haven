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
mod skill_contract;
pub(crate) mod workspace_transfer;

pub use skill_contract::{
    MAX_SKILL_PARAMETER_BYTES, WindowsContainerSkillBundleInput, WindowsContainerSkillDependencies,
    WindowsContainerSkillExecutionContract, WindowsContainerSkillFileAccess,
    WindowsContainerSkillNetworkPolicy, WindowsContainerSkillRuntime,
};

#[cfg(windows)]
pub(crate) mod named_pipe;

#[cfg(windows)]
mod broker_execution;

#[cfg(windows)]
mod broker_mcp_session;

#[cfg(windows)]
mod broker_mcp_client;

#[cfg(windows)]
mod broker_provider;

#[cfg(windows)]
mod quota_attestation;

/// Opaque proof that startup verified both the fixed Node runtime and this
/// exact administrator-pinned package entrypoint. Only quota attestation can
/// mint this token.
#[cfg(windows)]
pub(crate) struct McpNodePackageProof {
    package: image_catalog::McpNodePackageProfile,
}

#[cfg(windows)]
impl McpNodePackageProof {
    pub(super) fn package(&self) -> &image_catalog::McpNodePackageProfile {
        &self.package
    }
}

#[cfg(windows)]
impl quota_attestation::QuotaRuntimeAttestation {
    pub(super) fn mcp_node_package_proof(&self, id: &str) -> Option<McpNodePackageProof> {
        if !self.mcp_node_runtime_attested() || !self.mcp_node_package_attested(id) {
            return None;
        }
        Some(McpNodePackageProof {
            package: self.image_catalog().mcp_node_package(id)?.clone(),
        })
    }
}

#[cfg(all(windows, test))]
pub(crate) fn test_mcp_node_package_proof(
    package: image_catalog::McpNodePackageProfile,
) -> McpNodePackageProof {
    McpNodePackageProof { package }
}

/// Build Haven's authenticated local broker provider for Windows Shell work.
/// The service attests its fixed quota during startup and rejects execution
/// when the target runtime cannot prove that limit.
#[cfg(windows)]
pub fn new_brokered_execution_port() -> std::sync::Arc<dyn WindowsContainerExecutionPort> {
    std::sync::Arc::new(broker_provider::BrokerExecutionProvider)
}

/// Build Haven's authenticated broker port for fixed managed MCP stdio
/// packages. The caller supplies only a catalog package ID and bounded bytes;
/// command lines, host paths, environment and workspace data are not part of
/// this interface.
#[cfg(windows)]
pub fn new_brokered_mcp_session_port() -> std::sync::Arc<dyn WindowsContainerMcpSessionPort> {
    std::sync::Arc::new(broker_mcp_client::BrokerMcpSessionProvider)
}

#[cfg(windows)]
pub(crate) mod hcs_export_worker;

#[cfg(windows)]
pub mod broker_service;

const MAX_COMMAND_BYTES: usize = 256 * 1024;
const MAX_STORAGE_QUOTA_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Fixed scratch ceiling used by the Windows `WorkspaceWrite` Shell path.
/// The broker attests this exact value before accepting execution requests.
pub const WORKSPACE_WRITE_STORAGE_QUOTA_BYTES: u64 = 512 * 1024 * 1024;
const MCP_WORKSPACE_MAX_ENTRIES: usize = 10_000;
const MCP_WORKSPACE_MAX_DEPTH: usize = 32;
const MCP_WORKSPACE_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MCP_WORKSPACE_MAX_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
const MCP_WORKSPACE_MAX_CHANGES: usize = 4_096;
const MAX_STDIO_BYTES_PER_STREAM: usize = 64 * 1024 * 1024;
const MAX_EXECUTION_TIMEOUT: Duration =
    Duration::from_secs(haven_common::config::MAX_TOOL_TIMEOUT_SECS);
pub(super) const MCP_NODE_RUNTIME_PROFILE: &str = "haven-mcp-node-v1";
pub(super) const MCP_NODE_RUNTIME_VERSION: &str = "24.21.0";

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
#[derive(Clone)]
pub struct WindowsContainerWorkspaceInput {
    source: DirectoryCapability,
    review_stamp: Option<(u64, u64)>,
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

/// Per-attempt callbacks for the process-start authorization boundary,
/// cancellation watcher, and bounded live output path.
pub struct WindowsContainerExecutionCallbacks<'a> {
    pub(crate) before_process_start: &'a mut dyn FnMut() -> io::Result<()>,
    pub(crate) process_started: &'a mut dyn FnMut(),
    pub(crate) is_cancelled: &'a mut (dyn FnMut() -> bool + Send),
    pub(crate) on_output: &'a mut (dyn FnMut(WindowsContainerOutputStream, &[u8]) + Send),
}

/// Callbacks around the narrow process creation boundary.
pub struct WindowsContainerProcessStartCallbacks<'a> {
    pub(crate) before_process_start: &'a mut dyn FnMut() -> io::Result<()>,
    pub(crate) process_started: &'a mut dyn FnMut(),
}

/// Events returned by the brokered MCP generation after a bounded pipe read.
pub enum WindowsContainerMcpSessionEvent {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    OutputReviewReady,
    Exited(Option<u32>),
}

/// One broker-owned MCP generation. Implementations keep all pipes and
/// session identity private, and return at most one protocol frame per read.
pub trait WindowsContainerMcpSession {
    fn write_stdin_chunk(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn close_stdin(&mut self) -> io::Result<()>;
    fn read_event(&mut self, timeout: Duration) -> io::Result<WindowsContainerMcpSessionEvent>;
    fn stop_handle(&self) -> std::sync::Arc<dyn WindowsContainerMcpSessionStopHandle>;
    fn take_output_review(&mut self) -> Option<SandboxOutputReview> {
        None
    }
}

/// Independent stop capability so process cancellation can reach the broker
/// even while the session pipe is backpressured.
pub trait WindowsContainerMcpSessionStopHandle: Send + Sync {
    fn request_stop(&self) -> io::Result<()>;
}

/// The immutable Windows broker boundary for fixed managed MCP sessions.
pub trait WindowsContainerMcpSessionPort: private::McpSessionSealed + Send + Sync {
    fn open_session(
        &self,
        package_id: &str,
        workspace_input: Option<&WindowsContainerWorkspaceInput>,
        callbacks: &mut WindowsContainerMcpSessionCallbacks<'_>,
    ) -> io::Result<Box<dyn WindowsContainerMcpSession>>;
}

/// Authorization and cancellation callbacks around the broker's process
/// start handshake. The caller keeps its permit alive until `open_session`
/// returns after `McpSessionReady`.
pub struct WindowsContainerMcpSessionCallbacks<'a> {
    pub(crate) before_session_start: &'a mut dyn FnMut() -> io::Result<()>,
    pub(crate) session_started: &'a mut dyn FnMut(),
    pub(crate) is_cancelled: &'a mut (dyn FnMut() -> bool + Send),
}

impl<'a> WindowsContainerMcpSessionCallbacks<'a> {
    pub fn new(
        before_session_start: &'a mut dyn FnMut() -> io::Result<()>,
        session_started: &'a mut dyn FnMut(),
        is_cancelled: &'a mut (dyn FnMut() -> bool + Send),
    ) -> Self {
        Self {
            before_session_start,
            session_started,
            is_cancelled,
        }
    }
}

impl<'a> WindowsContainerProcessStartCallbacks<'a> {
    pub fn new(
        before_process_start: &'a mut dyn FnMut() -> io::Result<()>,
        process_started: &'a mut dyn FnMut(),
    ) -> Self {
        Self {
            before_process_start,
            process_started,
        }
    }
}

impl<'a> WindowsContainerExecutionCallbacks<'a> {
    pub fn new(
        before_process_start: &'a mut dyn FnMut() -> io::Result<()>,
        process_started: &'a mut dyn FnMut(),
        is_cancelled: &'a mut (dyn FnMut() -> bool + Send),
        on_output: &'a mut (dyn FnMut(WindowsContainerOutputStream, &[u8]) + Send),
    ) -> Self {
        Self {
            before_process_start,
            process_started,
            is_cancelled,
            on_output,
        }
    }
}

impl<'a> WindowsContainerOutputSink<'a> {
    pub fn new(
        request: &WindowsContainerExecutionRequest,
        on_output: &'a mut dyn FnMut(WindowsContainerOutputStream, &[u8]),
    ) -> Self {
        Self::new_with_limits(request.limits(), on_output)
    }

    pub fn new_with_limits(
        limits: &WindowsContainerExecutionLimits,
        on_output: &'a mut dyn FnMut(WindowsContainerOutputStream, &[u8]),
    ) -> Self {
        Self {
            output: WindowsContainerCapturedOutput {
                stdout: Vec::new(),
                stderr: Vec::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            },
            max_stdout_bytes: limits.max_stdout_bytes,
            max_stderr_bytes: limits.max_stderr_bytes,
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
        Self {
            source,
            review_stamp: None,
        }
    }

    /// Bind a brokered MCP output review to the authorization that supplied
    /// this exact workspace snapshot.
    pub fn with_review_stamp(
        mut self,
        execution_workspace_generation: u64,
        policy_revision: u64,
    ) -> io::Result<Self> {
        if execution_workspace_generation == 0 || policy_revision == 0 {
            return Err(invalid_request(
                "MCP output review requires workspace and policy stamps",
            ));
        }
        self.review_stamp = Some((execution_workspace_generation, policy_revision));
        Ok(self)
    }

    pub fn review_stamp(&self) -> Option<(u64, u64)> {
        self.review_stamp
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

/// Fixed copy ceiling for a project snapshot supplied to one managed MCP
/// invocation. The broker independently enforces the same profile before
/// staging or mounting the snapshot.
pub fn mcp_workspace_copy_limits() -> io::Result<DirectoryTreeCopyLimits> {
    DirectoryTreeCopyLimits::new(
        MCP_WORKSPACE_MAX_ENTRIES,
        MCP_WORKSPACE_MAX_DEPTH,
        MCP_WORKSPACE_MAX_FILE_BYTES,
        MCP_WORKSPACE_MAX_TOTAL_BYTES,
    )
}

/// Fixed changed-file budget for output reviews from managed MCP calls.
pub fn mcp_workspace_output_diff_limits() -> io::Result<DirectoryTreeDiffLimits> {
    DirectoryTreeDiffLimits::new(
        mcp_workspace_copy_limits()?,
        MCP_WORKSPACE_MAX_CHANGES,
        MCP_WORKSPACE_MAX_FILE_BYTES,
        MCP_WORKSPACE_MAX_TOTAL_BYTES,
    )
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

/// One authorized, fixed-profile Python Skill attempt. Its input bundle and
/// parameters are transferred separately from the project workspace.
pub struct WindowsContainerSkillExecutionRequest {
    contract: WindowsContainerSkillExecutionContract,
    execution_workspace_generation: u64,
    policy_revision: u64,
    limits: WindowsContainerExecutionLimits,
}

impl WindowsContainerSkillExecutionRequest {
    pub fn new(
        contract: WindowsContainerSkillExecutionContract,
        execution_workspace_generation: u64,
        policy_revision: u64,
        limits: WindowsContainerExecutionLimits,
    ) -> io::Result<Self> {
        if execution_workspace_generation == 0 || policy_revision == 0 {
            return Err(invalid_request(
                "Skill execution requires workspace and policy stamps",
            ));
        }
        Ok(Self {
            contract,
            execution_workspace_generation,
            policy_revision,
            limits,
        })
    }

    pub fn contract(&self) -> &WindowsContainerSkillExecutionContract {
        &self.contract
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

    /// Skills use a broker-fixed bundle budget. Callers cannot widen it.
    pub fn bundle_copy_limits(&self) -> DirectoryTreeCopyLimits {
        DirectoryTreeCopyLimits::new(1_024, 16, 16 * 1024 * 1024, 64 * 1024 * 1024)
            .expect("fixed Skill bundle limits are valid")
    }
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
        callbacks: &mut WindowsContainerExecutionCallbacks<'_>,
    ) -> io::Result<WindowsContainerExecutionResult>;

    fn execute_skill(
        &self,
        request: &WindowsContainerSkillExecutionRequest,
        workspace: &WindowsContainerWorkspaceInput,
        skill_bundle: &WindowsContainerSkillBundleInput,
        callbacks: &mut WindowsContainerExecutionCallbacks<'_>,
    ) -> io::Result<WindowsContainerExecutionResult>;
}

// Providers are implemented and selected inside haven-platform. Tools may
// consume the port but cannot install an arbitrary implementation.
mod private {
    pub trait Sealed {}

    pub trait McpSessionSealed {}
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
            callbacks: &mut WindowsContainerExecutionCallbacks<'_>,
        ) -> io::Result<WindowsContainerExecutionResult> {
            if (callbacks.is_cancelled)() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            (callbacks.before_process_start)()?;
            (callbacks.process_started)();
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
            let mut output = WindowsContainerOutputSink::new(request, callbacks.on_output);
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

        fn execute_skill(
            &self,
            _request: &WindowsContainerSkillExecutionRequest,
            _workspace: &WindowsContainerWorkspaceInput,
            _skill_bundle: &WindowsContainerSkillBundleInput,
            _callbacks: &mut WindowsContainerExecutionCallbacks<'_>,
        ) -> io::Result<WindowsContainerExecutionResult> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "mock Skill execution is not configured",
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
        let mut cancelled = || false;
        let mut before_shell_start = || Ok(());
        let mut shell_started = || {};
        let mut callbacks = WindowsContainerExecutionCallbacks::new(
            &mut before_shell_start,
            &mut shell_started,
            &mut cancelled,
            &mut on_output,
        );
        let result = port.execute(&request, &workspace, &mut callbacks).unwrap();

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
        let mut before_shell_start = || Ok(());
        let mut shell_started = || {};
        let mut on_output = |_: WindowsContainerOutputStream, _: &[u8]| {};
        let mut callbacks = WindowsContainerExecutionCallbacks::new(
            &mut before_shell_start,
            &mut shell_started,
            &mut cancelled,
            &mut on_output,
        );
        let error = match port.execute(&request, &workspace, &mut callbacks) {
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
