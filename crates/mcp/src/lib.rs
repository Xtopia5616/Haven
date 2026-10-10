mod client;
mod manager;
mod network;
mod protocol;
mod sse;
mod transport;

#[cfg(test)]
mod tests;

#[cfg(feature = "test-support")]
pub use client::McpClient;
pub use manager::{McpManager, McpReconcile};
pub use protocol::{
    MCP_TOOLS_NOT_DISCOVERED_DIAGNOSTIC, McpCallOutput, McpClientStatus, McpServerSnapshot,
    McpStatusChangeEvent, McpToolInfo,
};
