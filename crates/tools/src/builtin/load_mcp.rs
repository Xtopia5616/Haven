use async_trait::async_trait;
use haven_common::config::McpServerConfig;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::llm_tool_name;
use crate::{Tool, ToolResult};
use haven_mcp::McpManager;

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
        "Load an MCP server's tools by server name, activating them for this session. Prefer this over weaker built-in tools when the server's tools fit the session.".into()
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

        // Read config and the available-server list under one lock.
        let (config, available) = {
            let configs = self.server_configs.read().await;
            let available = configs.keys().cloned().collect::<Vec<_>>().join(", ");
            (configs.get(server_name).cloned(), available)
        };
        let config = config.ok_or_else(|| {
            anyhow::anyhow!(
                "MCP server '{}' not found in config. Available servers: {}",
                server_name,
                available
            )
        })?;
        if !config.enabled {
            anyhow::bail!("MCP server '{}' is disabled", server_name);
        }

        // Connect if not already connected
        if self.mcp_manager.get_client(server_name).await.is_none() {
            self.mcp_manager.connect_server(&config).await?;
        }

        // Fetch tools from cache
        let client = self
            .mcp_manager
            .get_client(server_name)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}' not available after connect", server_name)
            })?;

        let tools = client.tools_cache().await;
        let tool_schemas: Vec<Value> = tools
            .into_iter()
            .map(|t| {
                serde_json::json!({
                    "name": llm_tool_name(&format!("mcp::{}::{}", server_name, t.name)),
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();

        let mut result = serde_json::json!({
            "server": {
                "name": server_name,
                "tools": tool_schemas,
            },
            "status": "loaded",
            "server_name": server_name,
        });
        // Zero tools after a successful handshake is usually a client/server
        // incompatibility, not an empty server: surface the handshake
        // diagnostic so the model can distinguish the two.
        if tool_schemas.is_empty()
            && let Some(diagnostic) = client.diagnostic().await
        {
            result["diagnostic"] = serde_json::json!(diagnostic);
        }

        Ok(ToolResult::ok(result))
    }

    /// Declare the per-session MCP adapter registration so the executor
    /// registers it without name-matching "load_mcp".
    fn registrations(&self, output: &Value) -> Vec<crate::tool::ToolRegistration> {
        output
            .get("server_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|name| vec![crate::tool::ToolRegistration::McpServer(name.to_string())])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use haven_common::config::McpServerConfig;

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

    #[tokio::test]
    async fn test_load_mcp_rejects_disabled() {
        let configs = Arc::new(RwLock::new(HashMap::from([(
            "srv".to_string(),
            McpServerConfig {
                name: "srv".into(),
                enabled: false,
                ..Default::default()
            },
        )])));
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: configs,
        };
        let result = tool
            .execute(
                serde_json::json!({"server_name": "srv"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "disabled server should be rejected");
        assert!(result.unwrap_err().to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn test_load_mcp_rejects_unknown() {
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
        };
        let result = tool
            .execute(
                serde_json::json!({"server_name": "nope"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "unknown server should be rejected");
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
