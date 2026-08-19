use async_trait::async_trait;
use haven_common::config::McpServerConfig;
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{McpToolAdapter, Tool, ToolResult};
use haven_mcp::McpManager;

pub struct LoadMcpTool {
    pub mcp_manager: Arc<McpManager>,
    pub server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
}

/// Typed parameters for `LoadMcpTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `LoadMcpTool::run`.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LoadMcpParams {
    /// The name of the MCP server to load.
    pub server_name: String,
}

impl LoadMcpTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: LoadMcpParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let server_name = params.server_name;
        if server_name.is_empty() {
            anyhow::bail!("server_name is required");
        }

        // Read config and the available-server list under one lock.
        let (config, available) = {
            let configs = self.server_configs.read().await;
            let available = configs.keys().cloned().collect::<Vec<_>>().join(", ");
            (configs.get(&server_name).cloned(), available)
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
        if self.mcp_manager.get_client(&server_name).await.is_none() {
            self.mcp_manager.connect_server(&config).await?;
        }

        // Fetch tools from cache
        let client = self
            .mcp_manager
            .get_client(&server_name)
            .await
            .ok_or_else(|| {
                anyhow::anyhow!("MCP server '{}' not available after connect", server_name)
            })?;

        let tools = client.tools_cache().await;
        // Schemas come from the per-tool McpToolAdapter's tool_def() so the
        // name / description / input_schema advertised to the model are the
        // exact ones the registered session adapter validates and executes.
        let tool_schemas: Vec<Value> = tools
            .into_iter()
            .map(|info| {
                McpToolAdapter::new(client.clone(), &server_name, info)
                    .tool_def()
                    .json()
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

    /// Entry ②: LLM JSON entry — convert/validate into `LoadMcpParams`,
    /// then land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool::parse_tool_input::<LoadMcpParams>(&self.name(), input)?;
        self.run(params, cancel).await
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

    #[tokio::test]
    async fn test_load_mcp_native_entry_lands_in_run() {
        let tool = LoadMcpTool {
            mcp_manager: Arc::new(McpManager::new()),
            server_configs: Arc::new(RwLock::new(HashMap::new())),
        };
        let result = tool
            .run(
                LoadMcpParams {
                    server_name: "nope".into(),
                },
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err(), "unknown server should be rejected");
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
