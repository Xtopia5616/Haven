use async_trait::async_trait;
use haven_common::config::{ConfigLoader, LogConfig, LogLevel, McpServerConfig};
use haven_common::types::{McpTransportType, RiskLevel};
use haven_llm::EndpointRole;
use haven_llm::LlmRouter;
use haven_memory::Database;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::mcp::{McpClientStatus, McpManager};
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
    "skill_create",
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
const SESSION_MUTATING_OPS: &[&str] = &["mcp_connect", "mcp_disconnect", "mcp_reload"];

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
            let mut health = serde_json::Map::new();
            for role in EndpointRole::ALL {
                let configured = router.is_role_configured(*role).await;
                let status = if !configured {
                    "not_configured".to_string()
                } else {
                    match router.health_check(*role).await {
                        Ok(()) => "ok".to_string(),
                        Err(e) => format!("error: {e}"),
                    }
                };
                health.insert(
                    role.as_str().to_string(),
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
        let mut value = value_at(&root, path)
            .ok_or_else(|| anyhow::anyhow!("config key '{}' not found", path))?
            .clone();
        // Mask every api_key inside the result: an exact api_key path returns
        // a scalar, but a parent path (e.g. `llm.default_model` or `llm`)
        // would otherwise leak the secret embedded in the object.
        mask_api_keys(&mut value);
        if path.ends_with("api_key") {
            return Ok(serde_json::json!("[masked]"));
        }
        Ok(value)
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

    async fn op_skill_create(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the new skill name)"))?;
        validate_skill_name(name)?;
        let description = input["description"]
            .as_str()
            .filter(|d| !d.is_empty())
            .ok_or_else(|| anyhow::anyhow!("description is required"))?;
        let instructions = input["instructions"]
            .as_str()
            .filter(|i| !i.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("instructions are required (the '## Instructions' body)")
            })?;
        if instructions.len() > self.max_instructions_bytes {
            anyhow::bail!(
                "instructions too large (max {} bytes)",
                self.max_instructions_bytes
            );
        }
        let language = input["language"].as_str().unwrap_or("python");
        // Only Python skills are executable: `SkillRunner::run` rejects any
        // other language at fire time (skills/runner.rs), so reject
        // unsupported values here and let the agent learn immediately
        // instead of creating a skill that can never run.
        if language != "python" {
            anyhow::bail!("unsupported language '{language}': only 'python' is supported");
        }
        let version = input["version"]
            .as_str()
            .map(|v| v.replace(['\n', '\r'], " ").trim().to_string())
            .filter(|v| !v.is_empty());
        if let Some(v) = &version
            && v.len() > 64
        {
            anyhow::bail!("version too long (max 64 characters)");
        }
        let script = input["script"].as_str().map(str::to_string);
        if let Some(s) = &script
            && s.len() > self.max_script_bytes
        {
            anyhow::bail!("script too large (max {} bytes)", self.max_script_bytes);
        }

        let root = self.skills_engine.resolved_root().await;
        let skill_dir = root.join(name);
        if skill_dir.exists() {
            anyhow::bail!("skill '{}' already exists at {}", name, skill_dir.display());
        }

        tokio::fs::create_dir_all(&skill_dir).await?;
        let desc_line = description.replace(['\n', '\r'], " ");
        let mut md =
            format!("# Skill: {name}\n\n## Metadata\n- name: {name}\n- description: {desc_line}\n");
        if let Some(v) = &version {
            md.push_str(&format!("- version: {v}\n"));
        }
        md.push_str(&format!(
            "- language: {language}\n\n## Instructions\n{instructions}\n"
        ));
        tokio::fs::write(skill_dir.join("SKILL.md"), md).await?;

        let mut has_script = false;
        if let Some(script) = script {
            let scripts = skill_dir.join("scripts");
            tokio::fs::create_dir_all(&scripts).await?;
            tokio::fs::write(scripts.join("main.py"), script).await?;
            has_script = true;
        }

        self.skills_engine.refresh_from_disk().await?;
        // Ensure the new skill is enabled: no-op when the filter is None
        // (all enabled), adds it to the allowlist otherwise.
        self.skills_engine.set_enabled(name, true).await?;
        let filter = self.skills_engine.enabled_filter().await;
        self.mutate_config(|loader| {
            loader.config_mut().skills.enabled = filter;
            Ok(())
        })?;

        Ok(serde_json::json!({
            "name": name,
            "created": true,
            "root": skill_dir.to_string_lossy(),
            "has_script": has_script,
        }))
    }

    async fn mcp_status(&self) -> Value {
        let servers: Vec<McpServerConfig> = {
            let configs = self.server_configs.read().await;
            if !configs.is_empty() {
                configs.values().cloned().collect()
            } else {
                // Cold in-memory index (e.g. right after startup, before the
                // first config load): fall back to the persisted config so
                // `mcp_list` / `status` never report an empty server list
                // while servers exist in config.toml.
                self.read_config()
                    .map(|loader| loader.config().mcp_servers.clone())
                    .unwrap_or_default()
            }
        };

        let mut out = Vec::with_capacity(servers.len());
        for cfg in &servers {
            let client = self.mcp_manager.get_client(&cfg.name).await;
            let (connected, tool_count, last_error) = match &client {
                Some(c) => {
                    let status = c.status().await;
                    let is_connected = matches!(status, McpClientStatus::Connected);
                    // A client object exists even when its connection failed
                    // (load_from_config inserts clients before connecting), so
                    // the connected flag must come from the real status.
                    let error = match status {
                        McpClientStatus::Offline { error } => error,
                        _ => c.last_error().await.unwrap_or_default(),
                    };
                    (is_connected, c.tools_cache().await.len(), error)
                }
                None => (false, 0, String::new()),
            };
            out.push(serde_json::json!({
                "name": cfg.name,
                "enabled": cfg.enabled,
                "connected": connected,
                "tools": tool_count,
                "last_error": last_error,
            }));
        }
        Value::Array(out)
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
        // Ensure-connected semantics: drop any existing live client and
        // reconnect from the current config. Enabled servers are
        // auto-connected at startup, so a plain `mcp_connect` would otherwise
        // fail with "already loaded"; reconnecting also picks up config
        // changes made via `config_set mcp_servers.*`.
        self.mcp_manager.remove_client(name).await;
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

    async fn op_mcp_add(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to add)"))?;
        let transport = match input["transport"].as_str().unwrap_or("stdio") {
            "stdio" => McpTransportType::Stdio,
            "http" => McpTransportType::Http,
            other => anyhow::bail!("unknown transport '{}' (expected 'stdio' or 'http')", other),
        };
        let command = input["command"]
            .as_str()
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        let url = input["url"]
            .as_str()
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        match transport {
            McpTransportType::Stdio if command.is_none() => {
                anyhow::bail!("command is required (the binary to spawn) for stdio servers");
            }
            McpTransportType::Http if url.is_none() => {
                anyhow::bail!("url is required (the HTTP endpoint) for http servers");
            }
            _ => {}
        }
        let args = string_array(input, "args");
        let env = string_array(input, "env");
        let enabled = input["enabled"].as_bool().unwrap_or(true);
        let auto_connect = input["auto_connect"].as_bool().unwrap_or(true);

        let config = McpServerConfig {
            name: name.to_string(),
            transport,
            command: command.unwrap_or_default(),
            args,
            env,
            url: url.unwrap_or_default(),
            enabled,
        };

        // Upsert semantics: registering an existing name updates it in place
        // instead of erroring. Fresh names behave exactly as before.
        if let Some(existing) = self
            .read_config()?
            .config()
            .mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
        {
            return self
                .apply_mcp_config_update(&existing, &config, auto_connect)
                .await;
        }

        self.mutate_config(|loader| {
            loader.config_mut().mcp_servers.push(config.clone());
            Ok(())
        })?;
        // Keep the in-memory index in sync so `mcp_connect` / `load_mcp` see it.
        self.server_configs
            .write()
            .await
            .insert(config.name.clone(), config.clone());

        let mut result = serde_json::json!({
            "name": config.name,
            "enabled": enabled,
            "saved": true,
        });
        if enabled && auto_connect {
            match self.mcp_manager.connect_server(&config).await {
                Ok(()) => result["connected"] = serde_json::json!(true),
                Err(e) => {
                    result["connected"] = serde_json::json!(false);
                    result["warning"] = format!("config saved but connect failed: {e}").into();
                }
            }
        } else {
            result["connected"] = serde_json::json!(false);
        }
        Ok(result)
    }

    /// Update an existing MCP server's command/args/env/enabled by name.
    async fn op_mcp_update(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to update)"))?;
        let existing = self
            .read_config()?
            .config()
            .mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;

        let mut updated = existing.clone();
        if let Some(command) = input["command"].as_str().filter(|c| !c.is_empty()) {
            updated.command = command.to_string();
        }
        if input.get("args").is_some() {
            updated.args = string_array(input, "args");
        }
        if input.get("env").is_some() {
            updated.env = string_array(input, "env");
        }
        if let Some(enabled) = input["enabled"].as_bool() {
            updated.enabled = enabled;
        }

        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    /// Toggle an existing MCP server's enabled flag (the UI toggle equivalent).
    async fn op_mcp_toggle(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to toggle)"))?;
        let enabled = input["enabled"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("enabled (boolean) is required for mcp_toggle"))?;
        let existing = self
            .read_config()?
            .config()
            .mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;

        let mut updated = existing.clone();
        updated.enabled = enabled;
        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    /// Remove an MCP server from config, the in-memory index, and the live
    /// client manager.
    async fn op_mcp_remove(&self, input: &Value) -> anyhow::Result<Value> {
        let name = input["name"]
            .as_str()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to remove)"))?;
        self.mutate_config(|loader| {
            let servers = &mut loader.config_mut().mcp_servers;
            let before = servers.len();
            servers.retain(|s| s.name != name);
            if servers.len() == before {
                anyhow::bail!("MCP server '{}' not found in config", name);
            }
            Ok(())
        })?;
        self.mcp_manager.remove_client(name).await;
        self.server_configs.write().await.remove(name);
        Ok(serde_json::json!({
            "name": name,
            "removed": true,
            "connected": false,
        }))
    }

    /// Re-read `mcp_servers` from disk into the in-memory index and reconnect
    /// every enabled server, dropping clients that are disabled or gone.
    async fn op_mcp_reload(&self) -> anyhow::Result<Value> {
        let servers = self.read_config()?.config().mcp_servers.clone();

        // Resync the in-memory index with disk.
        let mut map = self.server_configs.write().await;
        map.clear();
        for s in &servers {
            map.insert(s.name.clone(), s.clone());
        }
        drop(map);

        // Drop every live client first so stale configs (command/args/env)
        // cannot survive a reload, then reconnect enabled servers fresh.
        for name in self.mcp_manager.list_clients().await {
            self.mcp_manager.remove_client(&name).await;
        }

        let mut connected = Vec::new();
        for s in &servers {
            if !s.enabled {
                continue;
            }
            match self.mcp_manager.connect_server(s).await {
                Ok(()) => {
                    connected.push(serde_json::json!({ "name": s.name, "connected": true }));
                }
                Err(e) => {
                    connected.push(serde_json::json!({
                        "name": s.name,
                        "connected": false,
                        "error": e.to_string(),
                    }));
                }
            }
        }
        Ok(serde_json::json!({ "reloaded": true, "connected": connected }))
    }

    /// Shared core for `mcp_update` / `mcp_toggle` / `mcp_add` (upsert path):
    /// replace the config entry for `new_config.name` with `new_config`,
    /// keeping `config.toml`, the in-memory `server_configs` index, and the
    /// live `McpManager` in sync.
    ///
    /// When the server ends up enabled with a changed connection profile (or
    /// a disabled→enabled transition), the new connection is established
    /// BEFORE the config is persisted: a failed connect leaves the config
    /// unchanged, so the on-disk `enabled` flag can never diverge from the
    /// runtime client state. Disabling shuts the live client down first.
    async fn apply_mcp_config_update(
        &self,
        old_config: &McpServerConfig,
        new_config: &McpServerConfig,
        connect_if_enabled: bool,
    ) -> anyhow::Result<Value> {
        let name = new_config.name.clone();
        let config_changed = old_config.transport != new_config.transport
            || old_config.command != new_config.command
            || old_config.args != new_config.args
            || old_config.env != new_config.env
            || old_config.url != new_config.url;
        let will_enable = new_config.enabled && !old_config.enabled;

        if new_config.enabled && connect_if_enabled && (will_enable || config_changed) {
            // (Re)start the connection with the new settings before saving.
            self.mcp_manager.remove_client(&name).await;
            if let Err(e) = self.mcp_manager.connect_server(new_config).await {
                // Roll back so runtime state matches the unchanged config.
                if old_config.enabled {
                    let _ = self.mcp_manager.connect_server(old_config).await;
                }
                anyhow::bail!(
                    "MCP server '{}' not connected; config left unchanged: {}",
                    name,
                    e
                );
            }
        } else if !new_config.enabled {
            // Disabled: shut down the live client.
            self.mcp_manager.remove_client(&name).await;
        } else if config_changed {
            // Settings changed but no (re)connect was requested (e.g. an
            // `mcp_add` upsert with auto_connect=false): drop the stale live
            // client so the runtime never keeps running the old command/
            // args/env that no longer match config.
            self.mcp_manager.remove_client(&name).await;
        }

        let persist_result = self.mutate_config(|loader| {
            let servers = &mut loader.config_mut().mcp_servers;
            let Some(existing) = servers.iter_mut().find(|s| s.name == name) else {
                anyhow::bail!("MCP server '{}' not found in config", name);
            };
            *existing = new_config.clone();
            Ok(())
        });
        if let Err(e) = persist_result {
            // Save failed after a successful connect: roll the live client
            // back so it keeps matching the unchanged config.
            self.mcp_manager.remove_client(&name).await;
            if old_config.enabled {
                let _ = self.mcp_manager.connect_server(old_config).await;
            }
            return Err(e);
        }

        // Keep the in-memory index in sync so `mcp_list` / `load_mcp` /
        // `mcp_connect` observe the updated settings.
        self.server_configs
            .write()
            .await
            .insert(name.clone(), new_config.clone());

        Ok(serde_json::json!({
            "name": name,
            "enabled": new_config.enabled,
            "saved": true,
            "connected": self.mcp_manager.get_client(&name).await.is_some(),
        }))
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

/// Recursively replace every `*_api_key` string with `[masked]`, so parent
/// config paths never leak secrets embedded in nested objects.
fn mask_api_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if k.ends_with("api_key") && v.is_string() {
                    *v = Value::String("[masked]".into());
                } else {
                    mask_api_keys(v);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                mask_api_keys(v);
            }
        }
        _ => {}
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

/// Extract an array of strings from `input[key]`, ignoring non-string
/// elements (used by `mcp_add` for `args` / `env`).
fn string_array(input: &Value, key: &str) -> Vec<String> {
    input[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Validate a skill name for safe use as a directory and as the
/// `skill__<name>` tool identifier (after sanitization).
fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !ok {
        anyhow::bail!(
            "invalid skill name '{}': use 1-128 characters of a-z, A-Z, 0-9, '-' or '_'",
            name
        );
    }
    Ok(())
}

#[async_trait]
impl Tool for SelfTool {
    fn name(&self) -> String {
        "self".into()
    }

    fn description(&self) -> String {
        "Inspect and manage Haven's own state.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        let op = input["operation"].as_str().unwrap_or("");
        if READ_ONLY_OPS.contains(&op) {
            RiskLevel::Low
        } else if SESSION_MUTATING_OPS.contains(&op)
            || op == "logs_level"
            || op == "skill_enable"
            || op == "skill_disable"
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
                "command": {
                    "type": "string",
                    "description": "MCP server command to spawn (mcp_add / mcp_update)"
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
                "enabled": {
                    "type": "boolean",
                    "description": "Enabled flag (mcp_add default true; mcp_update / mcp_toggle set it explicitly)"
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
            "skill_create" => self.op_skill_create(&input).await?,
            "mcp_list" => self.op_mcp_list().await?,
            "mcp_connect" => self.op_mcp_connect(&input).await?,
            "mcp_disconnect" => self.op_mcp_disconnect(&input).await?,
            "mcp_add" => self.op_mcp_add(&input).await?,
            "mcp_update" => self.op_mcp_update(&input).await?,
            "mcp_toggle" => self.op_mcp_toggle(&input).await?,
            "mcp_remove" => self.op_mcp_remove(&input).await?,
            "mcp_reload" => self.op_mcp_reload().await?,
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
            256 * 1024,
            512 * 1024,
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
    async fn test_config_get_parent_paths_mask_nested_api_keys() {
        let (tool, _dir) = make_tool();
        tool.mutate_config(|l| {
            l.config_mut().llm.default_model.api_key = "super-secret".into();
            Ok(())
        })
        .unwrap();

        // Parent paths must not leak the embedded api_key.
        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "llm.default_model"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["api_key"], "[masked]");
        assert_eq!(result.output["model_name"].as_str().is_some(), true);

        let result = tool
            .execute(
                json!({"operation": "config_get", "path": "llm"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["default_model"]["api_key"], "[masked]");
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
    async fn test_mcp_list_falls_back_to_config_when_index_empty() {
        let (tool, _dir) = make_tool();
        // Persisted config has servers, but the in-memory index is empty
        // (simulates cold startup before any config mutation).
        tool.mutate_config(|l| {
            l.config_mut().mcp_servers.push(McpServerConfig {
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
        let loader = tool.read_config().unwrap();
        let server = loader
            .config()
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
        tool.mutate_config(|l| {
            l.config_mut().mcp_servers.push(McpServerConfig {
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
        let loader = tool.read_config().unwrap();
        let matches: Vec<_> = loader
            .config()
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

    /// Path to the fixture echo MCP server, whichever directory the test
    /// binary happens to run from (workspace root or crate root).
    fn fixture_path() -> String {
        let p = std::env::current_dir().unwrap_or_default();
        let candidates = [
            p.join("crates/tools/tests/fixtures/echo_mcp_server.py"),
            p.join("tests/fixtures/echo_mcp_server.py"),
        ];
        for c in &candidates {
            if c.exists() {
                return c.to_string_lossy().to_string();
            }
        }
        candidates[0].to_string_lossy().to_string()
    }

    #[tokio::test]
    async fn test_mcp_toggle_enable_connects_persists_and_disables() {
        let (tool, _dir) = make_tool();

        // Register a disabled server (acceptance criteria starting point).
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "command": "python",
                    "args": [fixture_path()],
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

        let loader = tool.read_config().unwrap();
        let server = loader
            .config()
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
        assert!(srv["tools"].as_i64().unwrap() > 0);

        // Toggle back off: disconnects and persists enabled=false.
        let result = tool
            .execute(
                json!({"operation": "mcp_toggle", "name": "echo-srv", "enabled": false}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(false));

        let loader = tool.read_config().unwrap();
        let server = loader
            .config()
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
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "srv",
                "command": "python",
                "args": [fixture_path()],
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
                    "command": "python",
                    "args": [fixture_path()],
                    "env": ["API_KEY=abc"],
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["saved"], json!(true));
        assert_eq!(result.output["connected"], json!(false));

        let loader = tool.read_config().unwrap();
        let server = loader
            .config()
            .mcp_servers
            .iter()
            .find(|s| s.name == "srv")
            .unwrap();
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
        let loader = tool.read_config().unwrap();
        assert!(
            loader
                .config()
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
                "command": "python",
                "args": [fixture_path()],
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
                "command": "python",
                "args": [fixture_path()],
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

        let loader = tool.read_config().unwrap();
        let server = loader
            .config()
            .mcp_servers
            .iter()
            .find(|s| s.name == "srv")
            .unwrap();
        assert!(!server.enabled, "failed enable must stay disabled");
        assert_eq!(server.command, "python");
        assert!(tool.mcp_manager.get_client("srv").await.is_none());
    }

    #[tokio::test]
    async fn test_mcp_remove_disconnects_and_persists() {
        let (tool, _dir) = make_tool();
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "command": "python",
                    "args": [fixture_path()],
                    "enabled": true,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(true));

        let result = tool
            .execute(
                json!({"operation": "mcp_remove", "name": "echo-srv"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["removed"], json!(true));

        let loader = tool.read_config().unwrap();
        assert!(
            !loader
                .config()
                .mcp_servers
                .iter()
                .any(|s| s.name == "echo-srv")
        );
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
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "command": "python",
                    "args": [fixture_path()],
                    "enabled": true,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
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
        tool.execute(
            json!({
                "operation": "mcp_add",
                "name": "echo-srv",
                "command": "python",
                "args": [fixture_path()],
                "enabled": true,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
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
                "command": "python",
                "args": [fixture_path()],
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
        // Register a connected server.
        let result = tool
            .execute(
                json!({
                    "operation": "mcp_add",
                    "name": "echo-srv",
                    "command": "python",
                    "args": [fixture_path()],
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
                    "command": "python",
                    "args": ["-u", fixture_path()],
                    "auto_connect": false,
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["connected"], json!(false));
        assert!(tool.mcp_manager.get_client("echo-srv").await.is_none());

        let loader = tool.read_config().unwrap();
        assert!(
            loader
                .config()
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
        let loader = tool.read_config().unwrap();
        assert_eq!(loader.config().skills.enabled, None);
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
        let loader = tool.read_config().unwrap();
        assert_eq!(
            loader.config().skills.enabled,
            Some(vec!["solo".to_string()])
        );
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
