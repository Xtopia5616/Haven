use async_trait::async_trait;
use haven_common::config::ConfigService;
use haven_common::types::RiskLevel;
use haven_llm::LlmRouter;
use haven_memory::Database;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::{
    StructuredToolError, Tool, ToolConcurrency, ToolErrorMetadata, ToolRegistry, ToolResult,
};
use haven_mcp::McpManager;
use haven_skills::SkillsEngine;

mod admin_config;
mod admin_diagnostics;
mod admin_mcp;
mod admin_skills;

pub(crate) use admin_diagnostics::sanitize_diagnostic;

/// App-level dependencies for the native admin surface, wired in by the
/// desktop shell. Everything is optional so headless/test builds work without
/// the full app; write operations fail closed when the config service is
/// absent.
#[derive(Clone)]
pub struct SelfToolContext {
    /// Shared versioned config service (persists to `config.toml`).
    pub config_service: Option<Arc<ConfigService>>,
    /// Database handle for session/session introspection.
    pub db: Option<Arc<Database>>,
    /// LLM router for endpoint health checks.
    pub router: Option<Arc<LlmRouter>>,
    /// Path to the log file tailed by `logs_tail`.
    pub log_path: Option<PathBuf>,
    /// Runtime log-level switcher (wired to the tracing reload layer).
    pub set_log_level: Option<Arc<dyn Fn(String) + Send + Sync>>,
    /// Weak handle to the running `ToolsManager` so the tool enable/disable
    /// ops can apply the runtime change (in-memory `tool_settings` +
    /// catalog rebuild) after persisting config. `None` in headless builds.
    pub tools_weak: Option<std::sync::Weak<crate::ToolsManager>>,
}

/// Operations the `self` tool understands.
const OPERATIONS: &[&str] = &[
    "status",
    "config_get",
    "skills_list",
    "skill_enable",
    "skill_disable",
    "skill_create",
    "tool_enable",
    "tool_disable",
    "mcp_list",
    "mcp_connect",
    "mcp_disconnect",
    "mcp_add",
    "mcp_update",
    "mcp_toggle",
    "mcp_remove",
    "mcp_reload",
    "logs_tail",
    "logs_level",
    "sessions",
    "errors",
];

/// Read-only operations. Anything not in this list mutates Haven's state.
const READ_ONLY_OPS: &[&str] = &[
    "status",
    "config_get",
    "skills_list",
    "mcp_list",
    "logs_tail",
    "sessions",
    "errors",
];

/// Operations that only affect the running session (no config persistence).
const SESSION_MUTATING_OPS: &[&str] = &["mcp_connect", "mcp_disconnect", "mcp_reload"];

/// Native admin surface shared by Tauri commands and capability-scoped model
/// adapters. The surface itself is intentionally not registered in the model
/// catalog; see `builtin::admin::AdminCapabilityTool`.
pub struct SelfTool {
    context: SelfToolContext,
    skills_engine: SkillsEngine,
    mcp_manager: Arc<McpManager>,
    server_configs: Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
    registry: ToolRegistry,
    /// Max bytes of skill `instructions` accepted by the create-skill op.
    max_instructions_bytes: usize,
    /// Max bytes of a skill script file accepted by the create-skill op.
    max_script_bytes: usize,
}

impl SelfTool {
    pub fn new(
        context: SelfToolContext,
        skills_engine: SkillsEngine,
        mcp_manager: Arc<McpManager>,
        server_configs: Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
        registry: ToolRegistry,
        max_instructions_bytes: usize,
        max_script_bytes: usize,
    ) -> Self {
        Self {
            context,
            skills_engine,
            mcp_manager,
            server_configs,
            registry,
            max_instructions_bytes,
            max_script_bytes,
        }
    }
}

/// `self` operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfOperation {
    #[default]
    Status,
    ConfigGet,
    SkillsList,
    SkillEnable,
    SkillDisable,
    SkillCreate,
    ToolEnable,
    ToolDisable,
    McpList,
    McpConnect,
    McpDisconnect,
    McpAdd,
    McpUpdate,
    McpToggle,
    McpRemove,
    McpReload,
    LogsTail,
    LogsLevel,
    Sessions,
    Errors,
}

/// Typed parameters for `SelfTool`. Entry ① (native `run`) and entry ②
/// (`Tool::execute` with LLM JSON) both land in `SelfTool::run`.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct SelfParams {
    /// What to do.
    pub operation: SelfOperation,
    /// Allowlisted read-only config path, e.g. session.max_concurrent or llm.roles.
    #[serde(default)]
    pub path: Option<String>,
    /// Skill or MCP server name.
    #[serde(default)]
    pub name: Option<String>,
    /// MCP server command to spawn (mcp_add / mcp_update).
    #[serde(default)]
    pub command: Option<String>,
    /// Command-line args for the MCP server (mcp_add / mcp_update).
    #[serde(default)]
    pub args: Option<Vec<String>>,
    /// KEY=VALUE environment variables for the MCP server.
    #[serde(default)]
    pub env: Option<Vec<String>>,
    /// Working directory to spawn the MCP server from.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Enabled flag (mcp_add default true; mcp_update / mcp_toggle set it).
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Connect the new MCP server right away (mcp_add, default true).
    #[serde(default)]
    pub auto_connect: Option<bool>,
    /// Skill description shown to the agent (skill_create).
    #[serde(default)]
    pub description: Option<String>,
    /// The '## Instructions' body of the new SKILL.md (skill_create).
    #[serde(default)]
    pub instructions: Option<String>,
    /// Skill language (skill_create, only 'python' is supported).
    #[serde(default)]
    pub language: Option<String>,
    /// Skill version string (skill_create, optional).
    #[serde(default)]
    pub version: Option<String>,
    /// Optional content of scripts/main.py for the new skill (skill_create).
    #[serde(default)]
    pub script: Option<String>,
    /// Row/line limit (default 10-50, max 500 for logs).
    #[serde(default)]
    pub limit: Option<i64>,
    /// Log level: trace, debug, info, warn, error.
    #[serde(default)]
    pub level: Option<String>,
    /// MCP transport (mcp_add): stdio or http.
    #[serde(default)]
    pub transport: Option<String>,
    /// HTTP endpoint URL (mcp_add with transport http).
    #[serde(default)]
    pub url: Option<String>,
}

impl SelfTool {
    /// Entry ①: structured native interface (internal code calls — zero
    /// serialization overhead). Entry ② deserializes JSON and delegates here.
    pub async fn run(
        &self,
        params: SelfParams,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let output = match params.operation {
            SelfOperation::Status => self.op_status().await?,
            SelfOperation::ConfigGet => self.op_config_get(&params).await?,
            SelfOperation::SkillsList => self.op_skills_list().await?,
            SelfOperation::SkillEnable => self.op_skill_set(&params, true).await?,
            SelfOperation::SkillDisable => self.op_skill_set(&params, false).await?,
            SelfOperation::SkillCreate => self.op_skill_create(&params).await?,
            SelfOperation::ToolEnable => self.op_tool_set(&params, true).await?,
            SelfOperation::ToolDisable => self.op_tool_set(&params, false).await?,
            SelfOperation::McpList => self.op_mcp_list().await?,
            SelfOperation::McpConnect => self.op_mcp_connect(&params).await?,
            SelfOperation::McpDisconnect => self.op_mcp_disconnect(&params).await?,
            SelfOperation::McpAdd => self.op_mcp_add(&params).await?,
            SelfOperation::McpUpdate => self.op_mcp_update(&params).await?,
            SelfOperation::McpToggle => self.op_mcp_toggle(&params).await?,
            SelfOperation::McpRemove => self.op_mcp_remove(&params).await?,
            SelfOperation::McpReload => self.op_mcp_reload().await?,
            SelfOperation::LogsTail => self.op_logs_tail(&params).await?,
            SelfOperation::LogsLevel => self.op_logs_level(&params).await?,
            SelfOperation::Sessions => self.op_sessions(&params).await?,
            SelfOperation::Errors => self.op_errors(&params).await?,
        };
        Ok(ToolResult::ok(output))
    }
}

#[async_trait]
impl Tool for SelfTool {
    fn name(&self) -> String {
        "haven".into()
    }

    fn description(&self) -> String {
        "Inspect and manage Haven itself: config, skills, builtin tools, MCP servers, logs, sessions.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        let op = input["operation"].as_str().unwrap_or("");
        if READ_ONLY_OPS.contains(&op) {
            RiskLevel::Low
        } else if SESSION_MUTATING_OPS.contains(&op)
            || op == "logs_level"
            || op == "skill_enable"
            || op == "skill_disable"
            || op == "tool_enable"
            || op == "tool_disable"
        {
            RiskLevel::Medium
        } else {
            RiskLevel::High
        }
    }

    fn concurrency(&self, input: &Value) -> ToolConcurrency {
        let operation = input["operation"].as_str().unwrap_or("status");
        if READ_ONLY_OPS.contains(&operation) {
            ToolConcurrency::SharedResource("haven".into())
        } else {
            ToolConcurrency::Resource("haven".into())
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": OPERATIONS,
                    "description": "What to do"
                },
                "path": {
                    "type": "string",
                    "description": "Dotted config path, e.g. session.max_concurrent or llm.roles (provider/role assignments)"
                },
                "name": {
                    "type": "string",
                    "description": "Skill, MCP server, or builtin tool name"
                },
                "command": {
                    "type": "string",
                    "description": "MCP server command to spawn (mcp_add / mcp_update)"
                },
                "transport": {
                    "type": "string",
                    "enum": ["stdio", "http"],
                    "description": "MCP transport (mcp_add default stdio; mcp_update can change it)"
                },
                "url": {
                    "type": "string",
                    "description": "HTTP endpoint URL for the MCP server (mcp_add / mcp_update with transport http)"
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Command-line args for the MCP server (mcp_add / mcp_update)"
                },
                "env": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "KEY=VALUE environment variables for the MCP server (mcp_add / mcp_update)"
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory to spawn the MCP server from (mcp_add / mcp_update). Use it when command/args use relative paths; pass an empty string to clear (mcp_update)."
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Enabled flag (mcp_add default true; mcp_update / mcp_toggle set it explicitly; tool_enable / tool_disable pick the state from the operation)"
                },
                "auto_connect": {
                    "type": "boolean",
                    "description": "Connect the new MCP server right away (mcp_add, default true)"
                },
                "description": {
                    "type": "string",
                    "description": "Skill description shown to the agent (skill_create)"
                },
                "instructions": {
                    "type": "string",
                    "description": "The '## Instructions' body of the new SKILL.md (skill_create)"
                },
                "language": {
                    "type": "string",
                    "description": "Skill language (skill_create, only 'python' is supported; default python)"
                },
                "version": {
                    "type": "string",
                    "description": "Skill version string (skill_create, optional)"
                },
                "script": {
                    "type": "string",
                    "description": "Optional content of scripts/main.py for the new skill (skill_create)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Row/line limit (default 10-50, max 500 for logs)"
                },
                "level": {
                    "type": "string",
                    "description": "Log level: trace, debug, info, warn, error"
                }
            },
            "required": ["operation"]
        })
    }

    /// Entry ②: LLM JSON entry — convert/validate into `SelfParams`, then
    /// land in the same implementation as entry ①.
    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        let params = crate::tool_contract::parse_tool_input::<SelfParams>(&self.name(), input)
            .map_err(|error| {
                anyhow::Error::new(StructuredToolError::new(
                    error.to_string(),
                    ToolErrorMetadata::validation(),
                ))
            })?;
        self.run(params, cancel).await.map_err(|error| {
            anyhow::Error::new(StructuredToolError::new(
                error.to_string(),
                ToolErrorMetadata::other(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::admin_diagnostics::sanitize_log_line;
    use super::*;
    use crate::ToolsManager;
    use haven_common::config::{AppConfig, ConfigLoader, ConfigService, LogLevel, McpServerConfig};
    use serde_json::json;
    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    fn edit_config(
        tool: &SelfTool,
        edit: impl FnOnce(&mut AppConfig) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        tool.context
            .config_service
            .as_ref()
            .expect("test tool has config service")
            .edit(edit)
            .map(|update| update.value)
    }

    fn make_tool() -> (SelfTool, TempDir) {
        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let ctx = SelfToolContext {
            config_service: Some(Arc::new(ConfigService::new(loader))),
            db: None,
            router: None,
            log_path: Some(dir.path().join("logs").join("haven.log")),
            set_log_level: None,
            tools_weak: None,
        };
        let tool = SelfTool::new(
            ctx,
            SkillsEngine::new(),
            Arc::new(McpManager::new()),
            Arc::new(RwLock::new(HashMap::new())),
            ToolRegistry::new(),
            256 * 1024,
            512 * 1024,
        );
        (tool, dir)
    }

    #[test]
    fn test_self_name_and_schema() {
        let (tool, _dir) = make_tool();
        assert_eq!(tool.name(), "haven");
        let schema = tool.input_schema();
        let ops = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        assert!(ops.iter().any(|o| o == "status"));
        assert!(!ops.iter().any(|o| o == "config_set"));
        assert_eq!(schema["required"][0], "operation");
    }

    #[test]
    fn test_risk_levels() {
        let (tool, _dir) = make_tool();
        assert_eq!(
            tool.risk_level(&json!({"operation": "status"})),
            RiskLevel::Low
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "skill_disable"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "mcp_connect"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "mcp_reload"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "mcp_toggle"})),
            RiskLevel::High
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "mcp_remove"})),
            RiskLevel::High
        );
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::High);
    }

    #[tokio::test]
    async fn test_config_get_full_masks_api_keys() {
        let (tool, _dir) = make_tool();
        edit_config(&tool, |config| {
            config
                .llm
                .providers
                .push(haven_common::config::ProviderConfig {
                    name: "openai".into(),
                    api_key: "super-secret".into(),
                    ..Default::default()
                });
            config.mcp_servers.push(McpServerConfig {
                name: "private-mcp".into(),
                env: vec!["API_TOKEN=super-secret".into(), "MODE=test".into()],
                ..Default::default()
            });
            Ok(())
        })
        .unwrap();
        let result = tool
            .execute(json!({"operation": "config_get"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["llm"]["providers"][0]["api_key"], "");
        assert_eq!(
            result.output["mcp_servers"][0]["env"],
            json!(["API_TOKEN=[masked]", "MODE=[masked]"])
        );
        assert!(!result.output.to_string().contains("super-secret"));
        assert_eq!(result.output["session"]["max_concurrent"], 3);
    }

    #[tokio::test]
    async fn test_config_get_by_path_and_masking() {
        let (tool, _dir) = make_tool();
        edit_config(&tool, |config| {
            config.session.max_concurrent = 7;
            config
                .llm
                .providers
                .push(haven_common::config::ProviderConfig {
                    name: "openai".into(),
                    api_key: "super-secret".into(),
                    ..Default::default()
                });
            Ok(())
        })
        .unwrap();

        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "session.max_concurrent"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output, json!(7));

        // Missing key → error.
        let err = tool
            .execute(
                json!({"operation": "config_get", "path": "nope.missing"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));

        // api_key paths are masked.
        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "llm.providers.0.api_key"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output, json!("[masked]"));
    }

    #[tokio::test]
    async fn test_config_get_parent_paths_mask_nested_api_keys() {
        let (tool, _dir) = make_tool();
        edit_config(&tool, |config| {
            config
                .llm
                .providers
                .push(haven_common::config::ProviderConfig {
                    name: "openai".into(),
                    api_key: "super-secret".into(),
                    ..Default::default()
                });
            Ok(())
        })
        .unwrap();

        // The provider path must mask its embedded api_key.
        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "llm.providers.0"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["api_key"], "[masked]");
        assert!(result.output["base_url"].as_str().is_some());

        // The parent `llm` path must mask every nested provider api_key.
        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "llm"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["providers"][0]["api_key"], "[masked]");
    }

    #[tokio::test]
    async fn test_config_get_mcp_environment_is_masked_by_path() {
        let (tool, _dir) = make_tool();
        edit_config(&tool, |config| {
            config.mcp_servers.push(McpServerConfig {
                name: "private-mcp".into(),
                env: vec!["API_TOKEN=super-secret".into()],
                ..Default::default()
            });
            Ok(())
        })
        .unwrap();

        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "mcp_servers.0.env"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output, json!(["API_TOKEN=[masked]"]));
        assert!(!result.output.to_string().contains("super-secret"));
    }

    #[tokio::test]
    async fn test_arbitrary_config_set_is_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "config_set", "path": "session.max_concurrent", "value": 7}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown variant `config_set`"));
        let config = tool.read_config().unwrap();
        assert_eq!(config.session.max_concurrent, 3);
    }

    #[tokio::test]
    async fn test_skill_enable_disable_persists_filter() {
        let (tool, dir) = make_tool();
        let skill_dir = dir.path().join("echo");
        std::fs::create_dir_all(skill_dir.join("scripts")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# Skill: echo\n## Metadata\n- description: echo skill\n## Instructions\ndo echo\n",
        )
        .unwrap();
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();

        let result = tool
            .execute(
                json!({"operation": "skill_disable", "name": "echo"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["enabled"], json!(false));

        let list = tool
            .execute(
                json!({"operation": "skills_list"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(list.output["skills"][0]["enabled"], json!(false));

        // Filter persisted to config.
        let config = tool.read_config().unwrap();
        assert_eq!(config.skills.enabled, Some(vec![]));

        let result = tool
            .execute(
                json!({"operation": "skill_enable", "name": "echo"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["enabled"], json!(true));
        let config = tool.read_config().unwrap();
        assert_eq!(config.skills.enabled, Some(vec!["echo".to_string()]));
    }

    #[tokio::test]
    async fn test_skill_ops_reject_unknown() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "skill_enable", "name": "nope"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mcp_list_and_connect_unknown() {
        let (tool, _dir) = make_tool();
        tool.server_configs.write().await.insert(
            "srv".into(),
            McpServerConfig {
                name: "srv".into(),
                enabled: false,
                ..Default::default()
            },
        );

        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(list.output["servers"][0]["name"], "srv");
        assert_eq!(list.output["servers"][0]["connected"], json!(false));

        // Disabled server cannot connect.
        let err = tool
            .execute(
                json!({"operation": "mcp_connect", "name": "srv"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));

        // Unknown server.
        let err = tool
            .execute(
                json!({"operation": "mcp_disconnect", "name": "ghost"}),
                CancellationToken::new(),
            )
            .await;
        assert!(err.is_ok(), "disconnecting an unknown server is a no-op");
    }

    #[tokio::test]
    async fn test_mcp_list_falls_back_to_config_when_index_empty() {
        let (tool, _dir) = make_tool();
        // Persisted config has servers, but the in-memory index is empty
        // (simulates cold startup before any config mutation).
        edit_config(&tool, |config| {
            config.mcp_servers.push(McpServerConfig {
                name: "cold-srv".into(),
                command: "python".into(),
                args: vec!["-m".to_string(), "demo".into()],
                enabled: false,
                ..Default::default()
            });
            Ok(())
        })
        .unwrap();

        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(list.output["servers"][0]["name"], "cold-srv");
        assert_eq!(list.output["servers"][0]["connected"], json!(false));
        assert_eq!(
            list.output["servers"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            1
        );

        // Once the index is populated it takes precedence (fresh source of truth).
        tool.server_configs.write().await.insert(
            "warm-srv".into(),
            McpServerConfig {
                name: "warm-srv".into(),
                enabled: false,
                ..Default::default()
            },
        );
        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(list.output["servers"][0]["name"], "warm-srv");
    }

    #[tokio::test]
    async fn test_mcp_add_persists_and_updates_index() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "new-srv",
                    "command": "python",
                    "args": ["-m", "demo"],
                    "env": ["API_KEY=abc"],
                    "enabled": true,
                    "auto_connect": false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["saved"], json!(true));
        assert_eq!(result.output["connected"], json!(false));

        // Persisted to config.
        let config = tool.read_config().unwrap();
        let server = config
            .mcp_servers
            .iter()
            .find(|s| s.name == "new-srv")
            .unwrap();
        assert_eq!(server.command, "python");
        assert_eq!(server.args, vec!["-m", "demo"]);
        assert_eq!(server.env, vec!["API_KEY=abc"]);
        assert!(server.enabled);

        // Visible in the in-memory index (used by load_mcp / mcp_connect).
        let index = tool.server_configs.read().await;
        assert!(index.contains_key("new-srv"));
        assert_eq!(index["new-srv"].args, vec!["-m", "demo"]);
    }

    #[tokio::test]
    async fn test_mcp_add_same_name_upserts() {
        let (tool, _dir) = make_tool();
        edit_config(&tool, |config| {
            config.mcp_servers.push(McpServerConfig {
                name: "dup".into(),
                command: "python".into(),
                args: vec!["old".into()],
                ..Default::default()
            });
            Ok(())
        })
        .unwrap();

        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "dup",
                    "command": "node",
                    "args": ["-m", "x"],
                    "enabled": false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["saved"], json!(true));

        // Same-name add updates in place instead of erroring.
        let config = tool.read_config().unwrap();
        let matches: Vec<_> = config
            .mcp_servers
            .iter()
            .filter(|s| s.name == "dup")
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].command, "node");
        assert_eq!(matches[0].args, vec!["-m", "x"]);
        assert!(!matches[0].enabled);

        // In-memory index reflects the update too.
        let index = tool.server_configs.read().await;
        assert_eq!(index["dup"].command, "node");
        assert_eq!(index["dup"].args, vec!["-m", "x"]);
        assert!(!index["dup"].enabled);
    }

    #[tokio::test]
    async fn test_mcp_add_missing_command_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "mcp_add", "name": "srv"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("command is required"));
    }

    /// Minimal in-process HTTP MCP endpoint for configuration lifecycle tests.
    /// It avoids requiring Python, a PATH entry, or an external process while
    /// still exercising `SelfTool` through the real `McpManager` handshake.
    struct TestMcpServer {
        url: String,
        task: tokio::task::JoinHandle<()>,
    }

    impl TestMcpServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(serve_test_mcp_connection(stream));
                }
            });
            Self { url, task }
        }
    }

    impl Drop for TestMcpServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn serve_test_mcp_connection(mut stream: TcpStream) {
        const HEADER_END: &[u8] = b"\r\n\r\n";
        let mut bytes = Vec::new();
        let header_end = loop {
            if let Some(end) = bytes
                .windows(HEADER_END.len())
                .position(|window| window == HEADER_END)
            {
                break end + HEADER_END.len();
            }
            let mut chunk = [0_u8; 1024];
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            bytes.extend_from_slice(&chunk[..read]);
        };

        let header = String::from_utf8_lossy(&bytes[..header_end]);
        let is_post = header.starts_with("POST ");
        let content_length = header
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then_some(value.trim())
            })
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default();
        while bytes.len() < header_end + content_length {
            let mut chunk = [0_u8; 1024];
            let Ok(read) = stream.read(&mut chunk).await else {
                return;
            };
            if read == 0 {
                return;
            }
            bytes.extend_from_slice(&chunk[..read]);
        }

        if !is_post {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return;
        }

        let request =
            serde_json::from_slice::<serde_json::Value>(&bytes[header_end..]).unwrap_or_default();
        if request.get("id").is_none() {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return;
        }

        let id = request["id"].clone();
        let result = match request["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": "2024-11-05",
                "serverInfo": {"name": "test-mcp", "version": "1.0.0"},
            }),
            Some("tools/list") => json!({"tools": []}),
            _ => json!({}),
        };
        let body =
            serde_json::to_vec(&json!({"jsonrpc": "2.0", "id": id, "result": result})).unwrap();
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nMcp-Session-Id: test-session\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(headers.as_bytes()).await;
        let _ = stream.write_all(&body).await;
    }

    /// Add the in-process MCP server with `enabled: true` via `mcp_add`.
    async fn add_echo_server(tool: &SelfTool, server: &TestMcpServer) -> ToolResult {
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "echo-srv",
                "transport": "http",
                "url": server.url,
                "enabled": true,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn test_mcp_toggle_enable_connects_persists_and_disables() {
        let (tool, _dir) = make_tool();
        let server = TestMcpServer::start().await;

        // Register a disabled server (acceptance criteria starting point).
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "transport": "http",
                    "url": server.url,
                    "enabled": false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["saved"], json!(true));
        assert_eq!(result.output["connected"], json!(false));

        // Toggle on: connects first, then persists enabled=true.
        let result = tool
            .execute(
                json!({"operation": "mcp_toggle", "name": "echo-srv", "enabled": true}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(true));

        let config = tool.read_config().unwrap();
        let server = config
            .mcp_servers
            .iter()
            .find(|s| s.name == "echo-srv")
            .unwrap();
        assert!(server.enabled);

        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        let srv = &list.output["servers"][0];
        assert_eq!(srv["enabled"], json!(true));
        assert_eq!(srv["connected"], json!(true));
        assert_eq!(srv["tools"], json!(0));

        // Toggle back off: disconnects and persists enabled=false.
        let result = tool
            .execute(
                json!({"operation": "mcp_toggle", "name": "echo-srv", "enabled": false}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(false));

        let config = tool.read_config().unwrap();
        let server = config
            .mcp_servers
            .iter()
            .find(|s| s.name == "echo-srv")
            .unwrap();
        assert!(!server.enabled);

        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        let srv = &list.output["servers"][0];
        assert_eq!(srv["enabled"], json!(false));
        assert_eq!(srv["connected"], json!(false));
    }

    #[tokio::test]
    async fn test_mcp_update_updates_fields_and_enables() {
        let (tool, _dir) = make_tool();
        let server = TestMcpServer::start().await;
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "srv",
                "transport": "http",
                "url": server.url,
                "enabled": false,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // Update command/args/env while staying disabled (no connect).
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_update",
                    "name": "srv",
                    "env": ["API_KEY=abc"],
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["saved"], json!(true));
        assert_eq!(result.output["connected"], json!(false));

        let config = tool.read_config().unwrap();
        let server = config.mcp_servers.iter().find(|s| s.name == "srv").unwrap();
        assert_eq!(server.env, vec!["API_KEY=abc"]);
        assert!(!server.enabled);

        // Enable via mcp_update: connects and persists.
        let result = tool
            .execute(
                json!({"operation": "mcp_update", "name": "srv", "enabled": true}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(true));
        let config = tool.read_config().unwrap();
        assert!(
            config
                .mcp_servers
                .iter()
                .find(|s| s.name == "srv")
                .unwrap()
                .enabled
        );

        // Clean up the live client.
        tool.mcp_manager.remove_client("srv").await;
    }

    #[tokio::test]
    async fn test_mcp_update_unknown_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "mcp_update", "name": "ghost", "command": "x"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mcp_toggle_unknown_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "mcp_toggle", "name": "ghost", "enabled": true}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mcp_toggle_missing_enabled_rejected() {
        let (tool, _dir) = make_tool();
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "srv",
                "command": "original-command",
                "enabled": false,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let err = tool
            .execute(
                json!({"operation": "mcp_toggle", "name": "srv"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("enabled"));
    }

    #[tokio::test]
    async fn test_mcp_update_enable_connect_failure_keeps_disabled() {
        let (tool, _dir) = make_tool();
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "srv",
                "command": "original-command",
                "enabled": false,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        // Pointing at a binary that cannot spawn must fail the enable without
        // persisting enabled=true (config and runtime stay in sync).
        let err = tool
            .execute(
                json!({
                    "operation": "mcp_update",
                    "name": "srv",
                    "command": "definitely-not-a-real-binary",
                    "enabled": true,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not connected"));

        let config = tool.read_config().unwrap();
        let server = config.mcp_servers.iter().find(|s| s.name == "srv").unwrap();
        assert!(!server.enabled, "failed enable must stay disabled");
        assert_eq!(server.command, "original-command");
        assert!(tool.mcp_manager.get_client("srv").await.is_none());
    }

    #[tokio::test]
    async fn test_mcp_remove_disconnects_and_persists() {
        let (tool, _dir) = make_tool();
        let server = TestMcpServer::start().await;
        let result = add_echo_server(&tool, &server).await;
        assert_eq!(result.output["connected"], json!(true));

        let result = tool
            .execute(
                json!({"operation": "mcp_remove", "name": "echo-srv"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["removed"], json!(true));

        let config = tool.read_config().unwrap();
        assert!(!config.mcp_servers.iter().any(|s| s.name == "echo-srv"));
        assert!(!tool.server_configs.read().await.contains_key("echo-srv"));
        assert!(tool.mcp_manager.get_client("echo-srv").await.is_none());
    }

    #[tokio::test]
    async fn test_mcp_remove_unknown_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "mcp_remove", "name": "ghost"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mcp_reload_reconnects_enabled() {
        let (tool, _dir) = make_tool();
        let server = TestMcpServer::start().await;
        let result = add_echo_server(&tool, &server).await;
        assert_eq!(result.output["connected"], json!(true));

        // Kill the live client but keep enabled=true in config.
        tool.mcp_manager.remove_client("echo-srv").await;
        assert!(tool.mcp_manager.get_client("echo-srv").await.is_none());

        let result = tool
            .execute(json!({"operation": "mcp_reload"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.output["reloaded"], json!(true));
        let connected = result.output["connected"].as_array().unwrap();
        assert_eq!(connected[0]["name"], "echo-srv");
        assert_eq!(connected[0]["connected"], json!(true));

        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(list.output["servers"][0]["connected"], json!(true));

        tool.mcp_manager.remove_client("echo-srv").await;
    }

    #[tokio::test]
    async fn test_mcp_reload_reconnects_even_when_client_already_exists() {
        let (tool, _dir) = make_tool();
        let server = TestMcpServer::start().await;
        add_echo_server(&tool, &server).await;
        assert!(tool.mcp_manager.get_client("echo-srv").await.is_some());

        // Reload restarts every enabled server, so the existing client is
        // torn down and rebuilt from the disk config rather than kept stale.
        let result = tool
            .execute(json!({"operation": "mcp_reload"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.output["connected"][0]["connected"], json!(true));
        let list = tool
            .execute(json!({"operation": "mcp_list"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(list.output["servers"][0]["connected"], json!(true));

        tool.mcp_manager.remove_client("echo-srv").await;
    }

    #[tokio::test]
    async fn test_mcp_reload_skips_disabled() {
        let (tool, _dir) = make_tool();
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "off",
                "command": "unused-command",
                "enabled": false,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        let result = tool
            .execute(json!({"operation": "mcp_reload"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.output["connected"].as_array().unwrap().is_empty());
        assert!(tool.mcp_manager.get_client("off").await.is_none());
    }

    #[tokio::test]
    async fn test_mcp_add_upsert_auto_connect_false_drops_stale_client() {
        let (tool, _dir) = make_tool();
        let server = TestMcpServer::start().await;
        // Register a connected server.
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "transport": "http",
                    "url": server.url,
                    "enabled": true,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(true));

        // Upsert with a changed connection profile and auto_connect=false:
        // the stale client (old command) must be dropped even though no
        // reconnect happens, so runtime cannot diverge from config.
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "transport": "http",
                    "url": format!("{}?updated=true", server.url),
                    "auto_connect": false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(false));
        assert!(tool.mcp_manager.get_client("echo-srv").await.is_none());

        let config = tool.read_config().unwrap();
        assert!(
            config
                .mcp_servers
                .iter()
                .find(|s| s.name == "echo-srv")
                .unwrap()
                .enabled
        );
    }

    #[tokio::test]
    async fn test_skill_create_builds_skill_on_disk() {
        let (tool, dir) = make_tool();
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();

        let result = tool
            .execute(
                json!({
                    "operation": "skill_create",
                    "name": "organizer",
                    "description": "Organizes files",
                    "instructions": "Group files by extension.\nUse file_move.",
                    "version": "1.0.0",
                    "language": "python",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["created"], json!(true));
        assert_eq!(result.output["has_script"], json!(false));

        // SKILL.md written with the expected layout.
        let md = std::fs::read_to_string(dir.path().join("organizer").join("SKILL.md")).unwrap();
        assert!(md.contains("# Skill: organizer"));
        assert!(md.contains("- description: Organizes files"));
        assert!(md.contains("- version: 1.0.0"));
        assert!(md.contains("## Instructions"));
        assert!(md.contains("Group files by extension."));

        // Engine sees it and the config filter stays None (all enabled).
        let skill = tool.skills_engine.get_skill("organizer").await.unwrap();
        assert!(skill.enabled());
        let config = tool.read_config().unwrap();
        assert_eq!(config.skills.enabled, None);
    }

    #[tokio::test]
    async fn test_skill_create_with_script() {
        let (tool, dir) = make_tool();
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();

        let result = tool
            .execute(
                json!({
                    "operation": "skill_create",
                    "name": "echo",
                    "description": "Echo skill",
                    "instructions": "Echo the input.",
                    "script": "import sys, json\nprint(json.load(sys.stdin))\n",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["has_script"], json!(true));

        let script =
            std::fs::read_to_string(dir.path().join("echo").join("scripts").join("main.py"))
                .unwrap();
        assert!(script.contains("json.load"));
        assert!(
            tool.skills_engine
                .get_skill("echo")
                .await
                .unwrap()
                .has_script()
        );
    }

    #[tokio::test]
    async fn test_skill_create_invalid_name_rejected() {
        let (tool, dir) = make_tool();
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();

        for bad in ["../escape", "a/b", "has space", "中文", "a:b"] {
            let err = tool
                .execute(
                    json!({
                        "operation": "skill_create",
                        "name": bad,
                        "description": "d",
                        "instructions": "i",
                    }),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains("invalid skill name"),
                "'{bad}' should be rejected, got: {err}"
            );
        }
    }

    #[tokio::test]
    async fn test_skill_create_unsupported_language_rejected() {
        let (tool, dir) = make_tool();
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();

        let err = tool
            .execute(
                json!({
                    "operation": "skill_create",
                    "name": "sh-skill",
                    "description": "d",
                    "instructions": "i",
                    "language": "bash",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unsupported language"),
            "expected unsupported language error, got: {err}"
        );
        assert!(
            !dir.path().join("sh-skill").exists(),
            "no skill directory should be created for a rejected language"
        );
    }

    #[tokio::test]
    async fn test_skill_create_existing_rejected() {
        let (tool, dir) = make_tool();
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), None)
            .await
            .unwrap();
        std::fs::create_dir_all(dir.path().join("taken")).unwrap();
        std::fs::write(
            dir.path().join("taken").join("SKILL.md"),
            "# Skill: taken\n## Metadata\n- description: d\n## Instructions\ni\n",
        )
        .unwrap();
        tool.skills_engine.refresh_from_disk().await.unwrap();

        let err = tool
            .execute(
                json!({
                    "operation": "skill_create",
                    "name": "taken",
                    "description": "d",
                    "instructions": "i",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn test_skill_create_adds_to_existing_allowlist() {
        let (tool, dir) = make_tool();
        // Exhaustive empty allowlist: nothing enabled.
        tool.skills_engine
            .set_config(Some(dir.path().to_path_buf()), Some(vec![]))
            .await
            .unwrap();

        let result = tool
            .execute(
                json!({
                    "operation": "skill_create",
                    "name": "solo",
                    "description": "d",
                    "instructions": "i",
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["created"], json!(true));

        // The new skill was added to the allowlist and persisted.
        let config = tool.read_config().unwrap();
        assert_eq!(config.skills.enabled, Some(vec!["solo".to_string()]));
        assert!(
            tool.skills_engine
                .get_skill("solo")
                .await
                .unwrap()
                .enabled()
        );
    }

    #[tokio::test]
    async fn test_logs_tail() {
        let (tool, _dir) = make_tool();
        let path = tool.context.log_path.clone().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, content).unwrap();

        let result = tool
            .execute(
                json!({"operation": "logs_tail", "limit": 3}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["total_lines"], json!(100));
        let lines = result.output["lines"].as_array().unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "line 98");
        assert_eq!(lines[2], "line 100");
    }

    #[test]
    fn diagnostic_log_sanitization_redacts_sensitive_lines_and_bounds_text() {
        assert_eq!(
            sanitize_log_line("provider request api_key=super-secret"),
            "[redacted diagnostic line]"
        );
        let bounded = sanitize_log_line(&"safe ".repeat(200));
        assert_eq!(bounded.chars().count(), 513);
        assert!(bounded.ends_with('…'));
    }

    #[tokio::test]
    async fn test_logs_level_invalid_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "logs_level", "level": "verbose"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("trace"));
    }

    #[tokio::test]
    async fn test_logs_level_persists() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(
                json!({"operation": "logs_level", "level": "debug"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["level"], json!("debug"));
        let config = tool.read_config().unwrap();
        assert_eq!(config.log.level, LogLevel::Debug);
    }

    #[tokio::test]
    async fn test_actions_and_errors_unavailable_without_db() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(json!({"operation": "sessions"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.output["unavailable"], json!(true));
        let result = tool
            .execute(json!({"operation": "errors"}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.output["unavailable"], json!(true));
    }

    #[tokio::test]
    async fn test_status_returns_overview() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(json!({"operation": "status"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.output["config_path"].is_string());
        assert!(result.output["settings"].is_object());
        assert!(result.output["tools"].is_object());
        assert!(result.output["mcp"].is_array());
        assert!(result.output["skills"].is_array());
    }

    #[tokio::test]
    async fn test_invalid_operation_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(json!({"operation": "explode"}), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("unknown variant `explode`"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_tool_disable_applies_runtime_and_persists() {
        let mgr = Arc::new(ToolsManager::new());
        let dir = TempDir::new().unwrap();
        let loader = Arc::new(ConfigService::new(
            ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap(),
        ));
        let ctx = SelfToolContext {
            config_service: Some(loader.clone()),
            db: None,
            router: None,
            log_path: None,
            set_log_level: None,
            tools_weak: Some(Arc::downgrade(&mgr)),
        };
        mgr.set_admin_context(ctx).await;
        assert!(mgr.get_tool("shell").await.is_some());
        assert!(mgr.get_tool("haven").await.is_none());
        assert!(mgr.get_tool("haven.diagnostics.status").await.is_some());
        assert!(mgr.get_tool("haven_session_diagnostics").await.is_none());

        let tool = mgr.admin_surface().await.expect("admin surface wired");
        let result = tool
            .run(
                SelfParams {
                    operation: SelfOperation::ToolDisable,
                    name: Some("shell".into()),
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        // Runtime: the disabled tool must leave the registry the agent sees.
        assert!(
            mgr.get_tool("shell").await.is_none(),
            "disabled tool must leave the registry"
        );
        // Persisted: config.toml carries the flag through the shared loader.
        let persisted = loader.snapshot().unwrap().config.tool_settings["shell"].enabled;
        assert!(!persisted);
    }

    #[tokio::test]
    async fn test_tool_enable_json_entry_roundtrip() {
        let (tool, dir) = make_tool();
        let result = tool
            .execute(
                json!({"operation": "tool_enable", "name": "system"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["enabled"].as_bool().unwrap());
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        assert!(loader.config().tool_settings["system"].enabled);
    }
}
