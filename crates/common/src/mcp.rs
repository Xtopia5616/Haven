//! Shared MCP server transport vocabulary for configuration, protocol and IPC.

use serde::{Deserialize, Serialize};

/// Connection transport supported by an MCP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpServerTransport {
    #[default]
    Stdio,
    Http,
}

impl McpServerTransport {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}
