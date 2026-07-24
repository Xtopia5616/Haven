pub mod adapters;
pub mod builtin;
pub mod mcp;
pub mod skills;
pub mod stt;
pub mod tool;

use haven_common::config::{SkillsExecConfig, ToolConfig};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use adapters::{McpToolAdapter, SkillToolAdapter};
pub use mcp::{
    McpClient, McpClientStatus, McpManager, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
pub use skills::runner::SkillRunner;
pub use skills::venv::VenvManager;
pub use skills::{Language, Skill, SkillInfo, SkillManifest, SkillsEngine};
pub use tool::{ConfirmationResult, SafetyGateway, Tool, ToolBox, ToolRegistry, ToolResult};

pub struct ToolsManager {
    pub registry: ToolRegistry,
    pub mcp_manager: McpManager,
    pub skills_engine: skills::SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub safety_gateway: SafetyGateway,
    tool_settings: RwLock<HashMap<String, ToolConfig>>,
}

impl ToolsManager {
    pub fn new() -> Self {
        let registry = ToolRegistry::new();
        let exec_config = SkillsExecConfig::default();
        Self {
            registry,
            mcp_manager: McpManager::new(),
            skills_engine: skills::SkillsEngine::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
            safety_gateway: SafetyGateway::new(RiskLevel::Low),
            tool_settings: RwLock::new(HashMap::new()),
        }
    }

    pub fn new_with_exec_config(exec_config: SkillsExecConfig) -> Self {
        let registry = ToolRegistry::new();
        Self {
            registry,
            mcp_manager: McpManager::new(),
            skills_engine: skills::SkillsEngine::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
            safety_gateway: SafetyGateway::new(RiskLevel::Low),
            tool_settings: RwLock::new(HashMap::new()),
        }
    }

    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        *self.tool_settings.write().await = settings;
    }

    pub async fn load_mcp_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        self.mcp_manager.load_from_config(servers).await;
    }

    pub async fn discover_all(
        &self,
        servers: &[haven_common::McpServerConfig],
        config: &haven_common::McpDiscoveryConfig,
    ) {
        self.mcp_manager.discover_all(servers, config).await;
    }

    /// Rebuild the tool catalog from the current MCP + Skills + builtin state.
    /// Called at startup and whenever MCP or Skills state changes.
    /// Skills are progressively loaded (refine §4.7): only the `load_skill`
    /// tool is registered; full skill adapters are NOT injected into the
    /// registry until the LLM explicitly calls `load_skill`.
    pub async fn rebuild_catalog(&self) {
        let mut all_tools: Vec<ToolBox> = Vec::new();

        // Register builtin tools (including progressive load_skill)
        builtin::register_builtin_tools(
            &mut all_tools,
            &self.skills_engine,
            &self.registry,
            &self.skill_runner,
        )
        .await;

        // Register MCP tools from connected clients
        let mcp_tools = self.mcp_manager.list_all_tools().await;
        for (server_name, tools) in mcp_tools {
            if let Some(client) = self.mcp_manager.get_client(&server_name).await {
                for info in tools {
                    all_tools.push(Arc::new(McpToolAdapter::new(
                        client.clone(),
                        &server_name,
                        info,
                    )));
                }
            }
        }

        self.registry.rebuild(all_tools).await;
    }

    /// Build a skill index (name + description only) for injection into the
    /// system prompt (refine §4.7). The LLM uses `load_skill` to get full schemas.
    pub async fn build_skill_index(&self) -> Vec<Value> {
        let skills = self.skills_engine.list().await;
        skills
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| {
                serde_json::json!({
                    "name": format!("skill::{}", s.name),
                    "description": s.description,
                })
            })
            .collect()
    }
}

impl Default for ToolsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolsManager {
    pub async fn execute_tool(
        &self,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if let Some(tool) = self.registry.get(tool_name).await {
            tool.validate_input(&input)?;
            let settings = self.tool_settings.read().await;
            let timeout_secs = settings
                .get(tool_name)
                .map(|c| c.timeout_secs)
                .unwrap_or_else(|| tool.default_timeout_secs());
            drop(settings);
            return tool.execute_with_timeout(input, cancel, timeout_secs).await;
        }
        anyhow::bail!("tool '{}' not found in registry", tool_name)
    }

    pub async fn get_tool(&self, name: &str) -> Option<ToolBox> {
        self.registry.get(name).await
    }

    pub async fn get_risk_level(&self, tool_name: &str, input: &Value) -> RiskLevel {
        self.registry
            .get(tool_name)
            .await
            .map(|t| t.risk_level(input))
            .unwrap_or(RiskLevel::Safe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_tools_manager_new() {
        let mgr = ToolsManager::new();
        let tools = mgr.registry.list().await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn test_tools_manager_set_tool_settings() {
        let mgr = ToolsManager::new();
        let mut settings = HashMap::new();
        settings.insert("test_tool".into(), ToolConfig::default());
        mgr.set_tool_settings(settings).await;
    }

    #[tokio::test]
    async fn test_tools_manager_get_tool_not_found() {
        let mgr = ToolsManager::new();
        let tool = mgr.get_tool("nonexistent").await;
        assert!(tool.is_none());
    }

    #[tokio::test]
    async fn test_tools_manager_execute_tool_not_found() {
        let mgr = ToolsManager::new();
        let result = mgr
            .execute_tool("nonexistent", json!({}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tools_manager_rebuild_catalog_registers_builtins() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let file_tool = mgr.get_tool("file").await;
        assert!(file_tool.is_some());

        let process_tool = mgr.get_tool("process").await;
        assert!(process_tool.is_some());

        let clipboard_tool = mgr.get_tool("clipboard").await;
        assert!(clipboard_tool.is_some());

        let load_skill_tool = mgr.get_tool("load_skill").await;
        assert!(load_skill_tool.is_some());
    }

    #[tokio::test]
    async fn test_tools_manager_execute_builtin_tool() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        tokio::fs::write(&file, "hello from manager").await.unwrap();

        let result = mgr
            .execute_tool(
                "file",
                json!({"operation": "read", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(
            result.output["content"].as_str().unwrap(),
            "hello from manager"
        );
    }

    #[tokio::test]
    async fn test_tools_manager_get_risk_level_unknown() {
        let mgr = ToolsManager::new();
        let risk = mgr.get_risk_level("nonexistent", &json!({})).await;
        assert_eq!(risk, RiskLevel::Safe);
    }
}
