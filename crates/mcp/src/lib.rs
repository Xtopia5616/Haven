mod client;
mod features;
mod manager;
mod network;
mod protocol;
mod sse;
mod stdio_process;
mod transport;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

/// Process roots needed to authorize one MCP stdio generation. This is
/// separate from the per-call MCP remote resource authorization.
#[derive(Clone, Default)]
pub struct McpStdioProcessStartRequest {
    /// Cancellation of the owning connection attempt, including permission waits.
    pub cancellation: tokio_util::sync::CancellationToken,
    /// Hash of executable, arguments and environment; never contains secrets.
    pub command_fingerprint: String,
    pub working_directory: Option<PathBuf>,
    pub runtime_read_roots: Vec<PathBuf>,
}

/// Opaque short-lived permit held until the selected MCP stdio execution
/// boundary confirms that the generation has started.
pub trait McpStdioProcessStartPermit: Send {
    fn scratch_capacity_budget(&self) -> Option<(PathBuf, u64, usize)> {
        None
    }
    /// MXC policy compiled by the authorization owner for this process.
    fn sandboxed_process_policy(
        &self,
    ) -> Option<haven_platform::sandboxed_process::SandboxedProcessPolicy> {
        None
    }

    /// Working directory selected by the authorization owner for a sandboxed
    /// generation. Full Access keeps the configured cwd behavior.
    fn sandboxed_working_directory(&self) -> Option<&Path> {
        None
    }

    /// Per-generation writable scratch directory used for TEMP/TMP.
    fn sandboxed_temp_directory(&self) -> Option<&Path> {
        None
    }

    /// Transfer scratch ownership to the process so it remains alive until
    /// the generation is reaped.
    fn take_lifetime_resource(&mut self) -> Option<Box<dyn Send>> {
        None
    }
}

impl McpStdioProcessStartPermit for () {}

pub type McpStdioProcessStartPermitFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn McpStdioProcessStartPermit>>> + Send + 'a>>;

/// Tools-owned authorization for each MCP stdio generation start.
pub trait McpStdioProcessStartAuthorization: Send + Sync {
    /// HTTP connections use the same Tools-owned interaction gateway as
    /// local service starts. Standalone clients retain public-only access.
    fn authorize_http_destination<'a>(
        &'a self,
        raw_url: &'a str,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Pin<
        Box<
            dyn Future<Output = anyhow::Result<haven_common::network::ResolvedHttpDestination>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(haven_common::network::resolve_http_destination(
            raw_url,
            &[],
        ))
    }
    fn acquire_process_start_permit(
        &self,
        request: McpStdioProcessStartRequest,
    ) -> McpStdioProcessStartPermitFuture<'_>;
}

#[cfg(test)]
mod tests;

#[cfg(feature = "test-support")]
pub use client::McpClient;
pub use features::{
    McpCacheScope, McpElicitationAction, McpElicitationAnswer, McpElicitationField,
    McpElicitationFieldType, McpElicitationRequest, McpFeatureCallOutcome, McpFeatureCallStatus,
    McpPrompt, McpPromptArgument, McpPromptContent, McpPromptInfo, McpPromptList, McpPromptMessage,
    McpResourceContent, McpResourceInfo, McpResourceList, McpResourceRead, McpResourceTemplate,
    McpResourceTemplateList,
};
pub use manager::{McpManager, McpReconcile, McpTargetChanged};
pub use protocol::{
    MCP_TOOLS_NOT_DISCOVERED_DIAGNOSTIC, McpCallOutput, McpClientStatus, McpResourceChangeEvent,
    McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
pub(crate) use stdio_process::{
    McpStdioLaunchSpec, McpStdioProcess, McpStdioProcessLauncher, McpStdioReader, McpStdioWriter,
};

/// Maximum number of MCP server connection attempts that may run at once
/// across configuration loading, dynamic connections, reloads, and reconnects.
pub const MCP_MAX_PARALLEL_CONNECTIONS: usize = 8;
