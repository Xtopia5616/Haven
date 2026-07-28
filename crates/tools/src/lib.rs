pub mod adapters;
pub mod builtin;
pub mod circuit;
pub mod mcp;
pub mod skills;
pub mod stt;
pub mod tool;

use haven_common::config::{McpServerConfig, SkillsExecConfig, ToolConfig};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use adapters::{McpToolAdapter, SkillToolAdapter};
pub use mcp::{
    McpClient, McpClientStatus, McpManager, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
pub use skills::runner::SkillRunner;
pub use skills::venv::VenvManager;
pub use skills::{Language, Skill, SkillInfo, SkillManifest, SkillsEngine};
pub use tool::{
    ConfirmationResult, SafetyGateway, Tool, ToolBox, ToolRegistry,
    ToolResult,
};
pub use circuit::ToolCircuitRegistry;

pub struct ToolsManager {
    pub registry: ToolRegistry,
    pub mcp_manager: McpManager,
    pub mcp_server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub skills_engine: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub safety_gateway: SafetyGateway,
    tool_settings: RwLock<HashMap<String, ToolConfig>>,
    task_registrations: RwLock<HashMap<String, HashMap<String, ToolBox>>>,
    tool_circuits: ToolCircuitRegistry,
}

impl ToolsManager {
    pub fn new() -> Self {
        let registry = ToolRegistry::new();
        let exec_config = SkillsExecConfig::default();
        Self {
            registry,
            mcp_manager: McpManager::new(),
            mcp_server_configs: Arc::new(RwLock::new(HashMap::new())),
            skills_engine: skills::SkillsEngine::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
            safety_gateway: SafetyGateway::new(RiskLevel::Low),
            tool_settings: RwLock::new(HashMap::new()),
            task_registrations: RwLock::new(HashMap::new()),
            tool_circuits: ToolCircuitRegistry::new(),
        }
    }

    pub fn new_with_exec_config(exec_config: SkillsExecConfig) -> Self {
        let registry = ToolRegistry::new();
        Self {
            registry,
            mcp_manager: McpManager::new(),
            mcp_server_configs: Arc::new(RwLock::new(HashMap::new())),
            skills_engine: skills::SkillsEngine::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
            safety_gateway: SafetyGateway::new(RiskLevel::Low),
            tool_settings: RwLock::new(HashMap::new()),
            task_registrations: RwLock::new(HashMap::new()),
            tool_circuits: ToolCircuitRegistry::new(),
        }
    }

    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        *self.tool_settings.write().await = settings;
    }

    pub async fn load_mcp_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        // Store configs for dynamic loading via load_mcp tool
        let mut configs = self.mcp_server_configs.write().await;
        configs.clear();
        for server in servers {
            configs.insert(server.name.clone(), server.clone());
        }
        drop(configs);

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

        // Register builtin tools (including progressive load_skill and load_mcp)
        builtin::register_builtin_tools(
            &mut all_tools,
            &self.skills_engine,
            &self.skill_runner,
            &Arc::new(self.mcp_manager.clone()),
            &self.mcp_server_configs,
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

    /// Register a tool for a specific task (per-task skill overlay).
    /// Does NOT modify the global registry.
    pub async fn register_for_task(&self, task_id: &str, tool: ToolBox) {
        let name = tool.name();
        self.task_registrations
            .write()
            .await
            .entry(task_id.to_string())
            .or_default()
            .insert(name, tool);
    }

    /// Remove all per-task tool registrations for a given task.
    pub async fn unregister_task(&self, task_id: &str) {
        self.task_registrations.write().await.remove(task_id);
    }

    /// Look up a tool: first check per-task registrations, then global registry.
    pub async fn get_tool_for_task(&self, task_id: Option<&str>, name: &str) -> Option<ToolBox> {
        if let Some(tid) = task_id
            && let reg = self.task_registrations.read().await
            && let Some(tools) = reg.get(tid)
            && let Some(tool) = tools.get(name)
        {
            return Some(tool.clone());
        }
        self.registry.get(name).await
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

    /// Build an MCP server index (name + description only) for injection into the
    /// system prompt. The LLM uses `load_mcp` to get full schemas.
    pub async fn build_mcp_index(&self) -> Vec<Value> {
        let configs = self.mcp_server_configs.read().await;
        configs
            .values()
            .map(|s| {
                serde_json::json!({
                    "name": s.name.clone(),
                    "description": format!("MCP server '{}' via {} ({})", s.name, s.command, s.args.join(" ")),
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
        task_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if !self.tool_circuits.allow_request(tool_name) {
            tracing::warn!(
                "tool '{}' circuit breaker open — fast-failing",
                tool_name
            );
            anyhow::bail!(
                "tool '{}' is temporarily unavailable (circuit breaker open)",
                tool_name
            );
        }

        let tool = self.get_tool_for_task(task_id, tool_name).await
            .ok_or_else(|| anyhow::anyhow!("tool '{}' not found in registry", tool_name))?;
        tool.validate_input(&input)?;
        let settings = self.tool_settings.read().await;
        let cfg = settings.get(tool_name);
        let timeout_secs = cfg
            .map(|c| c.timeout_secs)
            .unwrap_or_else(|| tool.default_timeout_secs());
        let max_retries = cfg.map(|c| c.max_retries).unwrap_or(0);
        let backoff_secs = cfg
            .map(|c| c.retry_backoff_secs)
            .unwrap_or(2);
        drop(settings);

        let max_attempts = 1 + max_retries;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = Duration::from_secs(backoff_secs * 2u64.pow(attempt - 1));
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                }
            }
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }

            match tool
                .execute_with_timeout(input.clone(), cancel.clone(), timeout_secs)
                .await
            {
                Ok(result) => {
                    self.tool_circuits.record_success(tool_name);
                    return Ok(result);
                }
                Err(e)
                    if attempt + 1 < max_attempts && is_retryable_tool_error(&e) =>
                {
                    tracing::debug!(
                        "tool '{}' attempt {} failed, retrying: {}",
                        tool_name,
                        attempt + 1,
                        e
                    );
                    continue;
                }
                Err(e) => {
                    self.tool_circuits.record_failure(tool_name);
                    return Err(e);
                }
            }
        }
        self.tool_circuits.record_failure(tool_name);
        anyhow::bail!("tool '{}' retries exhausted", tool_name);
    }

    pub fn tool_circuits(&self) -> &ToolCircuitRegistry {
        &self.tool_circuits
    }

    pub async fn get_tool(&self, name: &str) -> Option<ToolBox> {
        self.registry.get(name).await
    }

    pub async fn get_risk_level(&self, task_id: Option<&str>, tool_name: &str, input: &Value) -> RiskLevel {
        self.get_tool_for_task(task_id, tool_name)
            .await
            .map(|t| t.risk_level(input))
            .unwrap_or(RiskLevel::Safe)
    }
}

/// Returns `true` if a tool execution error is transient and worth retrying.
fn is_retryable_tool_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("connection refused")
        || msg.contains("connection reset")
        || msg.contains("eof")
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
            .execute_tool(None, "nonexistent", json!({}), CancellationToken::new())
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
                None,
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
        let risk = mgr.get_risk_level(None, "nonexistent", &json!({})).await;
        assert_eq!(risk, RiskLevel::Safe);
    }

    /// End-to-end: execute_tool fast-fails once the per-tool circuit opens
    /// (refine §5).
    #[tokio::test]
    async fn test_execute_tool_circuit_breaker_opens() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct FailingTool {
            name: String,
            call_count: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl Tool for FailingTool {
            fn name(&self) -> String {
                self.name.clone()
            }
            fn description(&self) -> String {
                "always fails".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(
                &self,
                _: Value,
                _: tokio_util::sync::CancellationToken,
            ) -> anyhow::Result<ToolResult> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("deliberate failure")
            }
        }

        let mgr = ToolsManager::new();
        let call_count = Arc::new(AtomicU32::new(0));
        mgr.registry
            .register(Arc::new(FailingTool {
                name: "failing".into(),
                call_count: call_count.clone(),
            }))
            .await;

        for i in 0..5 {
            let r = mgr
                .execute_tool(None, "failing", json!({}), CancellationToken::new())
                .await;
            assert!(r.is_err(), "call {} should fail", i + 1);
        }
        assert!(mgr.tool_circuits().is_open("failing"));

        let before = call_count.load(Ordering::SeqCst);
        let r = mgr
            .execute_tool(None, "failing", json!({}), CancellationToken::new())
            .await;
        assert!(r.is_err());
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            before,
            "tool should not be called when breaker is open"
        );
        assert!(
            r.unwrap_err().to_string().contains("circuit breaker"),
            "error should mention circuit breaker"
        );
    }
}
