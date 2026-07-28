use async_trait::async_trait;
use haven_common::config::McpServerConfig;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::mcp::McpManager;
use crate::{Tool, ToolResult};

pub struct LoadMcpTool {
    pub mcp_manager: Arc<McpManager>,
    pub server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
}

#[async_trait]
impl Tool for LoadMcpTool {
    fn name(&self) -> String {
        "load_mcp".into()
    }
    fn description(&self) -> String {
        "Load an MCP server's full tool schema by server name. Use this when you need to access an MCP server listed in the MCP Index.".into()
    }

    fn risk_level(&self, _input: &Value) -> RiskLevel {
        RiskLevel::Safe
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "server_name": { "type": "string", "description": "The name of the MCP server to load" }
            },
            "required": ["server_name"]
        })
    }

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let server_name = input["server_name"].as_str().unwrap_or("");
        if server_name.is_empty() {
            anyhow::bail!("server_name is required");
        }

        let config = {
            let configs = self.server_configs.read().await;
            configs.get(server_name).cloned()
        };

        let available = {
            let configs = self.server_configs.read().await;
            configs
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        };
        let config = config.ok_or_else(|| {
            anyhow::anyhow!(
                "MCP server '{}' not found in config. Available servers: {}",
                server_name,
                available
            )
        })?;

        // Connect if not already connected
        if self.mcp_manager.get_client(server_name).await.is_none() {
            self.mcp_manager.connect_server(&config).await?;
        }

        // Fetch tools from cache
        let client = self
            .mcp_manager
            .get_client(server_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not available after connect", server_name))?;

        let tools = client.tools_cache().await;
        let tool_schemas: Vec<Value> = tools
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "name": format!("mcp::{}::{}", server_name, t.name),
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();

        Ok(ToolResult::ok(serde_json::json!({
            "server": {
                "name": server_name,
                "tools": tool_schemas,
            },
            "status": "loaded",
            "server_name": server_name,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;

    #[test]
    fn test_load_mcp_name() {
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
        };
        assert_eq!(tool.name(), "load_mcp");
    }

    #[test]
    fn test_load_mcp_input_schema() {
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
        };
        let schema = tool.input_schema();
        assert!(schema["properties"]["server_name"].is_object());
        assert_eq!(schema["required"][0], "server_name");
    }
}
