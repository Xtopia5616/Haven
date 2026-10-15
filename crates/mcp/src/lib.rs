mod client;
mod features;
mod manager;
mod network;
mod protocol;
mod sse;
mod transport;

use std::future::Future;
use std::pin::Pin;

/// Opaque short-lived permit held while an MCP stdio child is created and
/// attached to process containment.
pub trait McpStdioProcessStartPermit: Send {}

impl<T: Send> McpStdioProcessStartPermit for T {}

pub type McpStdioProcessStartPermitFuture<'a> =
    Pin<Box<dyn Future<Output = anyhow::Result<Box<dyn McpStdioProcessStartPermit>>> + Send + 'a>>;

/// Tools-owned authorization for each MCP stdio process start. Implementations
/// must keep the returned permit alive through OS spawn and containment attach.
pub trait McpStdioProcessStartAuthorization: Send + Sync {
    fn acquire_process_start_permit(&self) -> McpStdioProcessStartPermitFuture<'_>;
}

#[cfg(test)]
mod tests;

#[cfg(feature = "test-support")]
pub use client::McpClient;
pub use features::{
    McpCacheScope, McpPrompt, McpPromptArgument, McpPromptContent, McpPromptInfo, McpPromptList,
    McpPromptMessage, McpResourceContent, McpResourceInfo, McpResourceList, McpResourceRead,
};
pub use manager::{McpManager, McpReconcile, McpTargetChanged};
pub use protocol::{
    MCP_TOOLS_NOT_DISCOVERED_DIAGNOSTIC, McpCallOutput, McpClientStatus, McpServerSnapshot,
    McpStatusChangeEvent, McpToolInfo,
};

/// Maximum number of MCP server connection attempts that may run at once
/// across configuration loading, dynamic connections, reloads, and reconnects.
pub const MCP_MAX_PARALLEL_CONNECTIONS: usize = 8;
