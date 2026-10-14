mod client;
mod features;
mod manager;
mod network;
mod protocol;
mod sse;
mod transport;

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
