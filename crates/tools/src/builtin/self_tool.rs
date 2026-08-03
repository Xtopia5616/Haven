use async_trait::async_trait;
use haven_common::config::{ConfigLoader, LogConfig, LogLevel};
use haven_common::types::RiskLevel;
use haven_llm::EndpointRole;
use haven_llm::LlmRouter;
use haven_memory::Database;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::mcp::McpManager;
use crate::skills::SkillsEngine;
use crate::{Tool, ToolRegistry, ToolResult};

/// App-level dependencies for the `self` tool, wired in by the desktop shell.
/// Everything is optional so headless/test builds work without the full app.
#[derive(Clone)]
pub struct SelfToolContext {
    /// Shared config loader (persists to `config.toml`). Falls back to a
    /// fresh `ConfigLoader::load()` when absent.
    pub config_loader: Option<Arc<std::sync::Mutex<ConfigLoader>>>,
    /// Database handle for task/session introspection.
    pub db: Option<Arc<Database>>,
    /// LLM router for endpoint health checks.
    pub router: Option<Arc<LlmRouter>>,
    /// Path to the log file tailed by `logs_tail`.
    pub log_path: Option<PathBuf>,
    /// Runtime log-level switcher (wired to the tracing reload layer).
    pub set_log_level: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

/// Operations the `self` tool understands.
const OPERATIONS: &[&str] = &[
    "status",
    "config_get",
    "config_set",
    "skills_list",
    "skill_enable",
    "skill_disable",
    "mcp_list",
    "mcp_connect",
    "mcp_disconnect",
    "logs_tail",
    "logs_level",
    "tasks",
    "errors",
];

/// Read-only operations. Anything not in this list mutates Haven's state.
const READ_ONLY_OPS: &[&str] = &[
    "status",
    "config_get",
    "skills_list",
    "mcp_list",
    "logs_tail",
    "tasks",
    "errors",
];

/// Operations that only affect the running session (no config persistence).
const SESSION_MUTATING_OPS: &[&str] = &["mcp_connect", "mcp_disconnect"];

/// Keys under `config_set` that are applied live (the rest need a restart).
fn live_appliable(path: &str) -> bool {
    path.starts_with("skills.enabled")
        || path.starts_with("log.level")
        || path.starts_with("mcp_servers")
}

/// The `self` management tool: lets the assistant inspect and update Haven's
/// own configuration, skills, MCP servers, logs, and task state, and diagnose
/// its own errors.
pub struct SelfTool {
    context: SelfToolContext,
    skills_engine: SkillsEngine,
    mcp_manager: Arc<McpManager>,
    server_configs: Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
    registry: ToolRegistry,
}

impl SelfTool {
    pub fn new(
        context: SelfToolContext,
        skills_engine: SkillsEngine,
        mcp_manager: Arc<McpManager>,
        server_configs: Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
        registry: ToolRegistry,
    ) -> Self {
        Self {
            context,
            skills_engine,
            mcp_manager,
            server_configs,
            registry,
        }
    }

    /// Read the current config, preferring the shared loader when present.
    fn read_config(&self) -> anyhow::Result<ConfigLoader> {
        match &self.context.config_loader {
            Some(loader) => Ok(loader.lock().unwrap().clone()),
            None => ConfigLoader::load(),
        }
    }

    /// Mutate and persist the config through the shared loader (or a fresh
    /// load when absent), always saving afterwards.
    fn mutate_config<R>(
        &self,
        f: impl FnOnce(&mut ConfigLoader) -> anyhow::Result<R>,
    ) -> anyhow::Result<R> {
        match &self.context.config_loader {
            Some(loader) => {
                let mut guard = loader.lock().unwrap();
                let r = f(&mut guard)?;
                guard.save()?;
                Ok(r)
            }
            None => {
                let mut loader = ConfigLoader::load()?;
                let r = f(&mut loader)?;
                loader.save()?;
                Ok(r)
            }
        }
    }

    fn op(input: &Value) -> anyhow::Result<&str> {
        let op = input["operation"]
            .as_str()
            .filter(|o| OPERATIONS.contains(o))
            .ok_or_else(|| {
                anyhow::anyhow!("operation must be one of: {}", OPERATIONS.join(", "))
            })?;
        Ok(op)
    }

    async fn op_status(&self) -> anyhow::Result<Value> {
        let mut out = serde_json::json!({});

        // Config overview (API keys masked via Settings).
        match self.read_config() {
            Ok(loader) => {
                out["config_path"] = loader.path().to_string_lossy().to_string().into();
                out["settings"] = serde_json::to_value(loader.settings()).unwrap_or_default();
            }
            Err(e) => {
                out["config_error"] = e.to_string().into();
            }
        }

        // Model endpoint health.
        if let Some(router) = &self.context.router {
            let roles = [
                (EndpointRole::SmallModel, "small_model"),
                (EndpointRole::DefaultModel, "default_model"),
                (EndpointRole::BalancedModel, "balanced_model"),
                (EndpointRole::ImageModel, "image_model"),
                (EndpointRole::AudioModel, "audio_model"),
            ];
            let mut health = serde_json::Map::new();
            for (role, name) in roles {
                let configured = router.is_role_configured(role).await;
                let status = if !configured {
                    "not_configured".to_string()
                } else {
                    match router.health_check(role).await {
                        Ok(()) => "ok".to_string(),
                        Err(e) => format!("error: {e}"),
                    }
                };
                health.insert(
                    name.to_string(),
                    serde_json::json!({ "configured": configured, "status": status }),
                );
            }
            out["models"] = Value::Object(health);
        }

        // Registered global tools.
        let schemas = self.registry.list_schemas().await;
        let names: Vec<Value> = schemas.iter().map(|s| s["name"].clone()).collect();
        out["tools"] = serde_json::json!({ "count": schemas.len(), "names": names });

        // MCP servers.
        out["mcp"] = self.mcp_status().await;

        // Skills.
        let skills: Vec<Value> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "enabled": s.enabled,
                    "description": s.description,
                })
            })
            .collect();
        out["skills"] = Value::Array(skills);

        // Recent task counts.
        if let Some(db) = &self.context.db {
            let mut counts: HashMap<String, usize> = HashMap::new();
            if let Ok(tasks) = db.list_tasks(50, 0) {
                for t in &tasks {
                    *counts.entry(t.status.clone()).or_default() += 1;
                }
            }
            let total = db.count_tasks().unwrap_or(0);
            out["tasks"] = serde_json::json!({ "total": total, "recent_50_by_status": counts });
        } else {
            out["tasks"] = serde_json::json!({ "unavailable": true });
        }

        // Logging info.
        let log_path = self
            .context
            .log_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| LogConfigDefaultPath::path().to_string_lossy().to_string());
        out["log"] = serde_json::json!({ "path": log_path });

        Ok(out)
    }

    async fn op_config_get(&self, input: &Value) -> anyhow::Result<Value> {
        let loader = self.read_config()?;
        let Some(path) = input["path"].as_str().filter(|p| !p.is_empty()) else {
            // Full view with API keys masked.
            return Ok(serde_json::to_value(loader.settings()).unwrap_or_default());
        };
        let root = serde_json::to_value(loader.config())?;
        let value = value_at(&root, path)
            .ok_or_else(|| anyhow::anyhow!("config key '{}' not found", path))?;
        if path.ends_with("api_key") {
            return Ok(serde_json::json!("[masked]"));
        }
        Ok(value.clone())
    }

    async fn op_config_set(&self, input: &Value) -> anyhow::Result<Value> {
        let path = input["path"]
            .as_str()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow::anyhow!("path is required for config_set"))?;
        let value = input
            .get("value")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("value is required for config_set"))?;

        self.mutate_config(|loader| {
            let mut root = serde_json::to_value(loader.config())?;
            set_value_at(&mut root, path, value.clone())?;
            let updated: haven_common::config::AppConfig = serde_json::from_value(root)
                .map_err(|e| anyhow::anyhow!("invalid value for '{}': {}", path, e))?;
            *loader.config_mut() = updated;
            Ok(())
        })?;

        // Apply supported keys live.
        let mut live_applied: Vec<String> = Vec::new();
        if path.starts_with("skills.") {
            let cfg = self.read_config()?;
            let root = cfg.config().skills.root.clone();
            let enabled = cfg.config().skills.enabled.clone();
            match self.skills_engine.set_config(root, enabled).await {
                Ok(()) => live_applied.push("skills".into()),
                Err(e) => live_applied.push(format!("skills (failed: {e})")),
            }
        }
        if path.starts_with("log.level") {
            let cfg = self.read_config()?;
            let level = cfg.config().log.level.as_str().to_string();
            if let Some(f) = &self.context.set_log_level {
                f(level);
                live_applied.push("log.level".into());
            }
        }
        if path.starts_with("mcp_servers") {
            let cfg = self.read_config()?;
            let servers = cfg.config().mcp_servers.clone();
            let mut map = self.server_configs.write().await;
            map.clear();
            for s in &servers {
                map.insert(s.name.clone(), s.clone());
            }
            drop(map);
            live_applied
                .push("mcp_servers (index updated; reconnect via mcp_connect or restart)".into());
        }

        Ok(serde_json::json!({
            "path": path,
            "set": value,
            "saved": true,
            "live_applied": live_applied,
            "needs_restart": !live_appliable(path),
        }))
    }

    async fn op_skills_list(&self) -> anyhow::Result<Value> {
        let skills: Vec<Value> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "enabled": s.enabled,
                    "description": s.description,
                    "root": s.root,
                })
            })
            .collect();
        Ok(serde_json::json!({ "skills": skills }))
    }

    async fn op_skill_set(&self, input: &Value, enabled: bool) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "name is required (the skill to {}able)",
                    if enabled { "en" } else { "dis" }
                )
            })?;
        if self.skills_engine.get_skill(name).await.is_none() {
            anyhow::bail!("skill '{}' not found", name);
        }
        self.skills_engine.set_enabled(name, enabled).await?;
        let filter = self.skills_engine.enabled_filter().await;
        self.mutate_config(|loader| {
            loader.config_mut().skills.enabled = filter;
            Ok(())
        })?;
        Ok(serde_json::json!({
            "name": name,
            "enabled": enabled,
            "saved": true,
            "note": "take effect immediately for new loads"
        }))
    }

    async fn mcp_status(&self) -> Value {
        let configs = self.server_configs.read().await;
        let mut servers = Vec::with_capacity(configs.len());
        for (name, cfg) in configs.iter() {
            let client = self.mcp_manager.get_client(name).await;
            let (connected, tool_count, last_error) = match &client {
                Some(c) => (
                    true,
                    c.tools_cache().await.len(),
                    c.last_error().await.unwrap_or_default(),
                ),
                None => (false, 0, String::new()),
            };
            servers.push(serde_json::json!({
                "name": name,
                "enabled": cfg.enabled,
                "connected": connected,
                "tools": tool_count,
                "last_error": last_error,
            }));
        }
        Value::Array(servers)
    }

    async fn op_mcp_list(&self) -> anyhow::Result<Value> {
        Ok(serde_json::json!({ "servers": self.mcp_status().await }))
    }

    async fn op_mcp_connect(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to connect)"))?;
        let config = self
            .server_configs
            .read()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;
        if !config.enabled {
            anyhow::bail!("MCP server '{}' is disabled", name);
        }
        self.mcp_manager.connect_server(&config).await?;
        Ok(serde_json::json!({ "name": name, "connected": true }))
    }

    async fn op_mcp_disconnect(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to disconnect)"))?;
        if let Some(client) = self.mcp_manager.get_client(name).await {
            let _ = client.shutdown().await;
        }
        self.mcp_manager.remove_client(name).await;
        Ok(serde_json::json!({ "name": name, "connected": false }))
    }

    async fn op_logs_tail(&self, input: &Value) -> anyhow::Result<Value> {
        let limit = input["limit"].as_i64().unwrap_or(50).clamp(1, 500) as usize;
        let path = self
            .context
            .log_path
            .clone()
            .unwrap_or_else(LogConfigDefaultPath::path);
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(serde_json::json!({
                    "path": path.to_string_lossy(),
                    "error": format!("cannot read log file: {e}"),
                }));
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let start = lines.len().saturating_sub(limit);
        Ok(serde_json::json!({
            "path": path.to_string_lossy(),
            "total_lines": lines.len(),
            "lines": lines[start..].to_vec(),
        }))
    }

    async fn op_logs_level(&self, input: &Value) -> anyhow::Result<Value> {
        let level = input["level"]
            .as_str()
            .map(str::to_lowercase)
            .filter(|l| matches!(l.as_str(), "trace" | "debug" | "info" | "warn" | "error"))
            .ok_or_else(|| {
                anyhow::anyhow!("level must be one of: trace, debug, info, warn, error")
            })?;
        let parsed = match level.as_str() {
            "trace" => LogLevel::Trace,
            "debug" => LogLevel::Debug,
            "warn" => LogLevel::Warn,
            "error" => LogLevel::Error,
            _ => LogLevel::Info,
        };
        if let Some(f) = &self.context.set_log_level {
            f(level.clone());
        }
        self.mutate_config(|loader| {
            loader.config_mut().log.level = parsed;
            Ok(())
        })?;
        Ok(serde_json::json!({ "level": level, "saved": true }))
    }

    async fn op_tasks(&self, input: &Value) -> anyhow::Result<Value> {
        let limit = input["limit"].as_i64().unwrap_or(10).clamp(1, 50);
        let Some(db) = &self.context.db else {
            return Ok(serde_json::json!({ "unavailable": true }));
        };
        let tasks = db.list_tasks(limit, 0)?;
        let rows: Vec<Value> = tasks
            .into_iter()
            .map(|t| {
                let input_text = if t.input_text.chars().count() > 200 {
                    let cut = t.input_text.floor_char_boundary(200);
                    format!("{}…", &t.input_text[..cut])
                } else {
                    t.input_text.clone()
                };
                serde_json::json!({
                    "id": t.id,
                    "status": t.status,
                    "title": t.title,
                    "input": input_text,
                    "created_at": t.created_at,
                    "updated_at": t.updated_at,
                })
            })
            .collect();
        Ok(serde_json::json!({ "tasks": rows }))
    }

    async fn op_errors(&self, input: &Value) -> anyhow::Result<Value> {
        let limit = input["limit"].as_i64().unwrap_or(10).clamp(1, 50);
        let Some(db) = &self.context.db else {
            return Ok(serde_json::json!({ "unavailable": true }));
        };
        let tasks = db.list_tasks(limit, 0)?;
        let rows: Vec<Value> = tasks
            .into_iter()
            .filter(|t| t.status == "error")
            .map(|t| {
                let transcript: String = t
                    .transcript
                    .chars()
                    .rev()
                    .take(600)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let transcript = if transcript.chars().count() >= 600 {
                    format!("…{}", transcript)
                } else {
                    transcript
                };
                serde_json::json!({
                    "id": t.id,
                    "title": t.title,
                    "input": t.input_text,
                    "created_at": t.created_at,
                    "transcript_tail": transcript,
                })
            })
            .collect();
        Ok(serde_json::json!({ "errors": rows }))
    }
}

/// Wrapper so `unwrap_or_else(LogConfigDefaultPath::path)` works with the
/// static method shape the config module already exposes.
struct LogConfigDefaultPath;

impl LogConfigDefaultPath {
    fn path() -> PathBuf {
        LogConfig::default_log_path()
    }
}

/// Resolve a dotted path inside a JSON tree, descending through object keys
/// and numeric array indices (e.g. `mcp_servers.0.name`).
fn value_at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for seg in path.split('.') {
        match (cur, seg.parse::<usize>()) {
            (Value::Array(arr), Ok(idx)) => cur = arr.get(idx)?,
            (Value::Object(map), _) => cur = map.get(seg)?,
            _ => return None,
        }
    }
    Some(cur)
}

/// Set a value at a dotted path inside a JSON tree, creating intermediate
/// objects (or arrays for numeric segments) as needed.
fn set_value_at(root: &mut Value, path: &str, value: Value) -> anyhow::Result<()> {
    let segs: Vec<&str> = path.split('.').collect();
    if segs.is_empty() || segs.iter().any(|s| s.is_empty()) {
        anyhow::bail!("invalid config path: '{}'", path);
    }
    let mut cur = root;
    for (i, seg) in segs.iter().enumerate() {
        if i == segs.len() - 1 {
            match (cur, seg.parse::<usize>()) {
                (Value::Array(arr), Ok(idx)) => {
                    if idx >= arr.len() {
                        anyhow::bail!("array index {} out of range at '{}'", idx, path);
                    }
                    arr[idx] = value;
                }
                (Value::Object(map), _) => {
                    map.insert(seg.to_string(), value);
                }
                _ => anyhow::bail!("cannot set value at '{}'", path),
            }
            return Ok(());
        }
        let next_is_index = segs[i + 1].parse::<usize>().is_ok();
        match cur {
            Value::Object(map) => {
                if !map.contains_key(*seg) {
                    let empty = if next_is_index {
                        Value::Array(Vec::new())
                    } else {
                        Value::Object(serde_json::Map::new())
                    };
                    map.insert(seg.to_string(), empty);
                }
                cur = map.get_mut(*seg).expect("just inserted");
            }
            Value::Array(arr) => {
                let idx = seg
                    .parse::<usize>()
                    .map_err(|_| anyhow::anyhow!("cannot descend into array with key '{}'", seg))?;
                if idx >= arr.len() {
                    anyhow::bail!("array index {} out of range at '{}'", idx, path);
                }
                cur = &mut arr[idx];
            }
            _ => anyhow::bail!("cannot descend into '{}'", seg),
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for SelfTool {
    fn name(&self) -> String {
        "self".into()
    }

    fn description(&self) -> String {
        "Inspect and manage Haven's own state: read status and configuration, \
         change config values (config_set), enable/disable skills, connect or \
         disconnect MCP servers, tail or set the log level, and list recent \
         tasks or failed-task errors for diagnosis."
            .into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        let op = input["operation"].as_str().unwrap_or("");
        if READ_ONLY_OPS.contains(&op) {
            RiskLevel::Low
        } else if SESSION_MUTATING_OPS.contains(&op)
            || op == "logs_level"
            || op.starts_with("skill_")
        {
            RiskLevel::Medium
        } else {
            RiskLevel::High
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
                    "description": "Dotted config path, e.g. task.max_concurrent or llm.default_model.model_name"
                },
                "value": {
                    "description": "New JSON value for config_set"
                },
                "name": {
                    "type": "string",
                    "description": "Skill or MCP server name"
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

    async fn execute(&self, input: Value, cancel: CancellationToken) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let op = Self::op(&input)?;
        let output = match op {
            "status" => self.op_status().await?,
            "config_get" => self.op_config_get(&input).await?,
            "config_set" => self.op_config_set(&input).await?,
            "skills_list" => self.op_skills_list().await?,
            "skill_enable" => self.op_skill_set(&input, true).await?,
            "skill_disable" => self.op_skill_set(&input, false).await?,
            "mcp_list" => self.op_mcp_list().await?,
            "mcp_connect" => self.op_mcp_connect(&input).await?,
            "mcp_disconnect" => self.op_mcp_disconnect(&input).await?,
            "logs_tail" => self.op_logs_tail(&input).await?,
            "logs_level" => self.op_logs_level(&input).await?,
            "tasks" => self.op_tasks(&input).await?,
            "errors" => self.op_errors(&input).await?,
            _ => anyhow::bail!("unknown operation '{}'", op),
        };
        Ok(ToolResult::ok(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::McpServerConfig;
    use serde_json::json;
    use tempfile::TempDir;

    fn make_tool() -> (SelfTool, TempDir) {
        let dir = TempDir::new().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let ctx = SelfToolContext {
            config_loader: Some(Arc::new(std::sync::Mutex::new(loader))),
            db: None,
            router: None,
            log_path: Some(dir.path().join("logs").join("haven.log")),
            set_log_level: None,
        };
        let tool = SelfTool::new(
            ctx,
            SkillsEngine::new(),
            Arc::new(McpManager::new()),
            Arc::new(RwLock::new(HashMap::new())),
            ToolRegistry::new(),
        );
        (tool, dir)
    }

    #[test]
    fn test_self_name_and_schema() {
        let (tool, _dir) = make_tool();
        assert_eq!(tool.name(), "self");
        let schema = tool.input_schema();
        let ops = schema["properties"]["operation"]["enum"]
            .as_array()
            .unwrap();
        assert!(ops.iter().any(|o| o == "status"));
        assert!(ops.iter().any(|o| o == "config_set"));
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
            tool.risk_level(&json!({"operation": "config_set"})),
            RiskLevel::High
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "skill_disable"})),
            RiskLevel::Medium
        );
        assert_eq!(
            tool.risk_level(&json!({"operation": "mcp_connect"})),
            RiskLevel::Medium
        );
        assert_eq!(tool.risk_level(&json!({})), RiskLevel::High);
    }

    #[tokio::test]
    async fn test_config_get_full_masks_api_keys() {
        let (tool, _dir) = make_tool();
        tool.mutate_config(|l| {
            l.config_mut().llm.default_model.api_key = "super-secret".into();
            Ok(())
        })
        .unwrap();
        let result = tool
            .execute(json!({"operation": "config_get"}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["llm"]["default_model"]["api_key"], "");
        assert_eq!(result.output["task"]["max_concurrent"], 3);
    }

    #[tokio::test]
    async fn test_config_get_by_path_and_masking() {
        let (tool, _dir) = make_tool();
        tool.mutate_config(|l| {
            l.config_mut().task.max_concurrent = 7;
            Ok(())
        })
        .unwrap();

        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "task.max_concurrent"}),
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
                json!({"operation": "config_get", "path": "llm.default_model.api_key"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output, json!("[masked]"));
    }

    #[tokio::test]
    async fn test_config_set_persists_and_restart_flag() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(
                json!({"operation": "config_set", "path": "task.max_concurrent", "value": 7}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.output["saved"].as_bool().unwrap());
        assert_eq!(result.output["needs_restart"], json!(true));
        assert!(result.output["live_applied"].as_array().unwrap().is_empty());

        // Reloaded from disk.
        let loader = tool.read_config().unwrap();
        assert_eq!(loader.config().task.max_concurrent, 7);
    }

    #[tokio::test]
    async fn test_config_set_invalid_type_rejected() {
        let (tool, _dir) = make_tool();
        let err = tool
            .execute(
                json!({"operation": "config_set", "path": "task.max_concurrent", "value": "lots"}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("invalid value"));
        // Config unchanged.
        let loader = tool.read_config().unwrap();
        assert_eq!(loader.config().task.max_concurrent, 3);
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
        let loader = tool.read_config().unwrap();
        assert_eq!(loader.config().skills.enabled, Some(vec![]));

        let result = tool
            .execute(
                json!({"operation": "skill_enable", "name": "echo"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["enabled"], json!(true));
        let loader = tool.read_config().unwrap();
        assert_eq!(
            loader.config().skills.enabled,
            Some(vec!["echo".to_string()])
        );
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
        let loader = tool.read_config().unwrap();
        assert_eq!(loader.config().log.level, LogLevel::Debug);
    }

    #[tokio::test]
    async fn test_tasks_and_errors_unavailable_without_db() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(json!({"operation": "tasks"}), CancellationToken::new())
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
        assert!(err.to_string().contains("operation must be one of"));
    }

    #[tokio::test]
    async fn test_set_value_at_helpers() {
        let mut root = json!({"a": {"b": [1, 2]}});
        set_value_at(&mut root, "a.b.1", json!(99)).unwrap();
        assert_eq!(root["a"]["b"][1], json!(99));
        set_value_at(&mut root, "c.d", json!(true)).unwrap();
        assert_eq!(root["c"]["d"], json!(true));
        assert_eq!(value_at(&root, "a.b.1"), Some(&json!(99)));
        assert!(value_at(&root, "a.b.9").is_none());
    }
}
