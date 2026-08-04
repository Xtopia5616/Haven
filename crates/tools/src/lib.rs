pub mod adapters;
pub mod bg;
pub mod builtin;
pub mod circuit;
pub mod mcp;
pub mod skills;
pub mod stt;
pub mod tool;
pub mod util;

use haven_common::config::{McpServerConfig, SkillsExecConfig, ToolConfig};
use haven_common::types::RiskLevel;
use haven_llm::LlmRouter;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use adapters::{McpToolAdapter, SkillToolAdapter};
pub use builtin::{ReminderMode, SelfTool, SelfToolContext};
pub use circuit::ToolCircuitRegistry;
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
    pub mcp_server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub skills_engine: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub safety_gateway: SafetyGateway,
    tool_settings: RwLock<HashMap<String, ToolConfig>>,
    task_registrations: RwLock<HashMap<String, HashMap<String, ToolBox>>>,
    tool_circuits: ToolCircuitRegistry,
    /// Shared LlmRouter. Tools that need a model (currently the file `summary`
    /// and image-understanding operations) call `router.chat(...)` — text
    /// summarization uses the SmallModel role, image understanding uses the
    /// ImageModel role; the router handles retries and the balanced-model
    /// fallback.
    router: RwLock<Option<Arc<LlmRouter>>>,
    /// Registry of background jobs (shell with background: true).
    pub background_jobs: Arc<bg::BackgroundJobs>,
    /// Registry of in-process reminders (the `reminder` tool). The fired
    /// channel is consumed by the agent layer, which notifies, runs the
    /// scheduled tool, or resumes the scheduling task (see `ReminderMode`).
    pub reminders: Arc<builtin::reminder::ReminderCenter>,
    /// App-level context for the `self` management tool (config loader, DB,
    /// router, log file). Wired in by the desktop shell; `None` in headless
    /// tests so the tool is simply not registered.
    self_context: RwLock<Option<builtin::SelfToolContext>>,
}

impl ToolsManager {
    pub fn new() -> Self {
        Self::new_with_exec_config(SkillsExecConfig::default())
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
            router: RwLock::new(None),
            background_jobs: Arc::new(bg::BackgroundJobs::new()),
            reminders: Arc::new(builtin::reminder::ReminderCenter::new()),
            self_context: RwLock::new(None),
        }
    }

    /// Replace the shared LlmRouter and rebuild the catalog so tools (e.g.
    /// `file summary`) pick up the new endpoint config.
    pub async fn set_router(&self, router: Arc<LlmRouter>) {
        *self.router.write().await = Some(router);
        self.rebuild_catalog().await;
    }

    /// Wire the app-level context for the `self` management tool and register
    /// the tool. Called by the desktop shell after the config loader exists;
    /// later catalog rebuilds keep the tool registered. Also hands the DB to
    /// the reminder registry so reminders persist across restarts.
    pub async fn set_self_context(&self, ctx: builtin::SelfToolContext) {
        self.reminders.set_db(ctx.db.clone()).await;
        *self.self_context.write().await = Some(ctx);
        self.rebuild_catalog().await;
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

    /// Rebuild the tool catalog from the current builtin state.
    /// Called at startup and whenever MCP or Skills state changes.
    ///
    /// Skills and MCP servers are progressively loaded (refine §4.7): only the
    /// `load_skill` / `load_mcp` meta-tools are registered globally. Full skill
    /// and MCP tool adapters are NOT injected into the global registry until the
    /// LLM explicitly calls `load_skill` / `load_mcp`, which registers them
    /// per-task (see `register_for_task`).
    pub async fn rebuild_catalog(&self) {
        let mut all_tools: Vec<ToolBox> = Vec::new();

        // Register builtin tools (including progressive load_skill and load_mcp)
        let router = self.router.read().await.clone();
        let self_context = self.self_context.read().await.clone();
        builtin::register_builtin_tools(
            &mut all_tools,
            &self.skills_engine,
            &self.skill_runner,
            &Arc::new(self.mcp_manager.clone()),
            &self.mcp_server_configs,
            router,
            self.background_jobs.clone(),
            self.reminders.clone(),
            self_context,
            self.registry.clone(),
        )
        .await;

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

    /// Register a skill as a per-task tool adapter. Looks up the skill by
    /// name, checks `enabled`, and registers `SkillToolAdapter` for the task.
    /// Returns `true` if the skill was found and enabled.
    pub async fn register_skill_for_task(&self, task_id: &str, skill_name: &str) -> bool {
        let Some(skill) = self.skills_engine.get_skill(skill_name).await else {
            return false;
        };
        if !skill.enabled() {
            return false;
        }
        let runner = self.skill_runner.read().await.clone();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
        self.register_for_task(task_id, Arc::new(adapter)).await;
        true
    }

    /// Register all tools from an MCP server as per-task tool adapters.
    /// Looks up the client by server name and registers `McpToolAdapter`
    /// for each cached tool. Returns `true` if the client was found.
    pub async fn register_mcp_for_task(&self, task_id: &str, server_name: &str) -> bool {
        let Some(client) = self.mcp_manager.get_client(server_name).await else {
            return false;
        };
        let tools = client.tools_cache().await;
        for info in tools {
            let adapter = McpToolAdapter::new(client.clone(), server_name, info);
            self.register_for_task(task_id, Arc::new(adapter)).await;
        }
        true
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
    /// Only enabled servers are listed — disabled ones cannot be loaded.
    pub async fn build_mcp_index(&self) -> Vec<Value> {
        let configs = self.mcp_server_configs.read().await;
        configs
            .values()
            .filter(|s| s.enabled)
            .map(|s| {
                serde_json::json!({
                    "name": s.name.clone(),
                    "description": format!("MCP server '{}' via {} ({})", s.name, s.command, s.args.join(" ")),
                })
            })
            .collect()
    }

    /// Return tool schemas for a task: global registry schemas merged with
    /// per-task registered skill/MCP adapters. Called before each LLM step so
    /// that tools loaded via `load_skill`/`load_mcp` become visible to the model.
    pub async fn list_schemas_for_task(&self, task_id: &str) -> Vec<Value> {
        let mut schemas = self.registry.list_schemas().await;
        let reg = self.task_registrations.read().await;
        if let Some(tools) = reg.get(task_id) {
            for tool in tools.values() {
                let risk = tool.risk_level(&serde_json::json!({}));
                schemas.push(serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "risk_level": risk,
                    "input_schema": tool.input_schema(),
                }));
            }
        }
        schemas
    }

    /// Insert or replace a single MCP server config in the in-memory map.
    /// Used by bridge commands (add/update/toggle) to keep `server_configs`
    /// in sync without reconnecting all servers.
    pub async fn upsert_mcp_server_config(&self, config: McpServerConfig) {
        self.mcp_server_configs
            .write()
            .await
            .insert(config.name.clone(), config);
    }

    /// Remove a single MCP server config from the in-memory map.
    pub async fn remove_mcp_server_config(&self, name: &str) {
        self.mcp_server_configs.write().await.remove(name);
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
            tracing::warn!("tool '{}' circuit breaker open — fast-failing", tool_name);
            anyhow::bail!(
                "tool '{}' is temporarily unavailable (circuit breaker open)",
                tool_name
            );
        }

        let tool = self
            .get_tool_for_task(task_id, tool_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("tool '{}' not found in registry", tool_name))?;

        // The reminder tool needs the scheduling task id to support `continue`
        // (resume that task) and `tool` (run with the task's per-task tool
        // context) modes. It is injected privately here — after the LLM-facing
        // input was captured by the caller — so it never reaches the tool
        // schema, the step history, or the LLM.
        let mut exec_input = input;
        if tool_name == "reminder"
            && let Some(tid) = task_id
            && let Some(obj) = exec_input.as_object_mut()
        {
            obj.insert("_task_id".into(), serde_json::json!(tid));
        }
        tool.validate_input(&exec_input)?;
        let settings = self.tool_settings.read().await;
        let cfg = settings.get(tool_name);
        let timeout_secs = cfg
            .map(|c| c.timeout_secs)
            .unwrap_or_else(|| tool.default_timeout_secs());
        let max_retries = cfg.map(|c| c.max_retries).unwrap_or(0);
        let backoff_secs = cfg.map(|c| c.retry_backoff_secs).unwrap_or(2);
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
                .execute_with_timeout(exec_input.clone(), cancel.clone(), timeout_secs)
                .await
            {
                Ok(result) => {
                    self.tool_circuits.record_success(tool_name);
                    return Ok(result);
                }
                Err(e) if attempt + 1 < max_attempts && is_retryable_tool_error(&e) => {
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

    pub async fn get_risk_level(
        &self,
        task_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> RiskLevel {
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

    // ── Progressive loading: per-task schemas & MCP index ──────────────

    #[tokio::test]
    async fn test_list_schemas_for_task_includes_per_task_tools() {
        use crate::skills::SkillManifest;

        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        // Before registering a per-task tool, schemas come only from the
        // global registry.
        let base_schemas = mgr.list_schemas_for_task("task-a").await;
        let base_count = base_schemas.len();

        // Register a fake per-task tool.
        let manifest = SkillManifest {
            name: "demo".into(),
            description: "demo skill".into(),
            version: None,
            language: crate::skills::Language::Python,
            allowed_tools: vec![],
            instructions: "do stuff".into(),
        };
        let skill = Skill::from_manifest_unchecked(manifest, std::path::PathBuf::from("."), true);
        let runner = mgr.skill_runner.read().await.clone();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
        mgr.register_for_task("task-a", Arc::new(adapter)).await;

        let schemas = mgr.list_schemas_for_task("task-a").await;
        assert_eq!(
            schemas.len(),
            base_count + 1,
            "per-task skill tool should appear in schemas"
        );
        assert!(schemas.iter().any(|s| s["name"] == "skill::demo"));

        // Other tasks should NOT see this tool.
        let other = mgr.list_schemas_for_task("task-b").await;
        assert_eq!(other.len(), base_count);
        assert!(!other.iter().any(|s| s["name"] == "skill::demo"));
    }

    #[tokio::test]
    async fn test_build_mcp_index_filters_disabled() {
        use haven_common::config::McpServerConfig;

        let mgr = ToolsManager::new();
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "on".into(),
            enabled: true,
            ..Default::default()
        })
        .await;
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "off".into(),
            enabled: false,
            ..Default::default()
        })
        .await;

        let index = mgr.build_mcp_index().await;
        let names: Vec<&str> = index.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(names.contains(&"on"));
        assert!(!names.contains(&"off"), "disabled server should not appear");
    }

    #[tokio::test]
    async fn test_upsert_and_remove_mcp_server_config() {
        use haven_common::config::McpServerConfig;

        let mgr = ToolsManager::new();
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "srv".into(),
            enabled: true,
            ..Default::default()
        })
        .await;
        assert_eq!(mgr.build_mcp_index().await.len(), 1);

        mgr.remove_mcp_server_config("srv").await;
        assert!(mgr.build_mcp_index().await.is_empty());
    }

    #[tokio::test]
    async fn test_rebuild_catalog_does_not_register_mcp_tools() {
        // Progressive loading: MCP tools must NOT be in the global registry.
        // They should only appear per-task after `load_mcp`.
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let schemas = mgr.registry.list_schemas().await;
        assert!(
            !schemas
                .iter()
                .any(|s| { s["name"].as_str().unwrap_or("").starts_with("mcp::") }),
            "MCP tools must not be pre-registered globally"
        );
    }
}
