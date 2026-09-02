mod client;
mod manager;
mod protocol;
mod sse;
mod transport;

#[cfg(test)]
mod tests;

pub use client::McpClient;
pub use manager::{McpManager, McpReconcile};
pub use protocol::{
    McpCallOutput, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
