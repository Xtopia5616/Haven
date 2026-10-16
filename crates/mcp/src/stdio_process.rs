//! Process lifecycle port for MCP's stdio-compatible bidirectional stream.
//!
//! MCP owns message framing and protocol state. A process launcher supplies
//! byte streams plus the lifecycle operations needed by that protocol owner;
//! platform-specific launchers must enforce the same process-start permit.

use crate::McpStdioProcessStartPermit;
use futures_util::future::BoxFuture;
use haven_common::config::McpStdioExecution;
use std::fmt;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

pub type McpStdioReader = Box<dyn AsyncRead + Send + Unpin>;
pub type McpStdioWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Immutable, trusted server configuration passed from the MCP manager to a
/// process launcher. Environment values and arguments may contain secrets or
/// user data, so the Debug implementation deliberately redacts them.
#[derive(Clone)]
pub struct McpStdioLaunchSpec {
    server_name: String,
    stdio_execution: McpStdioExecution,
    command: String,
    args: Vec<String>,
    environment: Vec<String>,
    working_directory: Option<String>,
}

impl McpStdioLaunchSpec {
    pub(crate) fn new(
        server_name: String,
        stdio_execution: McpStdioExecution,
        command: String,
        args: Vec<String>,
        environment: Vec<String>,
        working_directory: Option<String>,
    ) -> Self {
        Self {
            server_name,
            stdio_execution,
            command,
            args,
            environment,
            working_directory,
        }
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    pub fn stdio_execution(&self) -> &McpStdioExecution {
        &self.stdio_execution
    }

    /// Configured host command, meaningful only for `HostProcess`. A managed
    /// package launcher must ignore this field and resolve the typed package
    /// ID only through its pinned catalog; it must never forward a host
    /// command or package ID as a guest executable path or shell command.
    pub fn command(&self) -> &str {
        &self.command
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn environment(&self) -> &[String] {
        &self.environment
    }

    pub fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }
}

impl fmt::Debug for McpStdioLaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("McpStdioLaunchSpec")
            .field("server_name", &self.server_name)
            .field("stdio_execution", &self.stdio_execution)
            .field("command", &"<configured>")
            .field("argument_count", &self.args.len())
            .field("environment_count", &self.environment.len())
            .field(
                "working_directory",
                &self.working_directory.as_ref().map(|_| "<configured>"),
            )
            .finish()
    }
}

/// One live MCP server generation with its bidirectional stdio-compatible
/// stream and process lifecycle. The reader/writer are each taken exactly
/// once by `McpClient`; `stderr` is optional and is drained independently.
pub trait McpStdioProcess: Send {
    fn take_stdin(&mut self) -> io::Result<McpStdioWriter>;
    fn take_stdout(&mut self) -> io::Result<McpStdioReader>;
    fn take_stderr(&mut self) -> io::Result<Option<McpStdioReader>>;
    fn is_alive(&mut self) -> io::Result<bool>;
    /// Request termination without waiting for process exit. Implementations
    /// must keep this operation bounded so generation shutdown can enforce
    /// its own wait deadline.
    fn start_kill(&mut self) -> io::Result<()>;
    /// Wait for exit. The returned future must be cancellation-safe: callers
    /// may stop polling at a deadline and poll a fresh wait future later.
    fn wait(&mut self) -> BoxFuture<'_, io::Result<()>>;
}

/// Creates a server process generation after receiving the opaque live start
/// permit. The implementation must retain the permit until the process is
/// attached to containment or the broker confirms process start. If the
/// returned future is cancelled before yielding a process, the launcher must
/// stop any partially created generation before releasing its resources.
pub trait McpStdioProcessLauncher: Send + Sync {
    fn launch<'a>(
        &'a self,
        spec: &'a McpStdioLaunchSpec,
        permit: Box<dyn McpStdioProcessStartPermit>,
    ) -> BoxFuture<'a, anyhow::Result<Box<dyn McpStdioProcess>>>;
}
