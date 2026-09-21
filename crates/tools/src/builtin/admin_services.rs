//! Domain services shared by the five typed administration operations.
//!
//! This module deliberately contains no operation selector. Each public method
//! accepts the fields for exactly one operation; the typed operation wrappers in
//! `admin.rs` own selection, schema, policy, and error conversion.

use super::{AdminContext, McpAddFields, McpUpdateFields};
use crate::ToolRegistry;
use anyhow::Result;
use haven_common::config::{
    AppConfig, ConfigLoader, ConfigPatch, LogConfig, LogLevel, McpServerConfig, RequestKind,
    Settings,
};
use haven_common::types::McpTransportType;
use haven_mcp::McpClientStatus;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use haven_mcp::McpManager;
use haven_skills::SkillsEngine;

/// Runtime implementation dependencies for the five capability-scoped admin
/// surfaces. The context is app-level; the skills/MCP/catalog dependencies are
/// supplied by the builtin composition root.
pub(crate) struct AdminServices {
    pub(crate) context: AdminContext,
    pub(crate) skills_engine: SkillsEngine,
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub(crate) registry: ToolRegistry,
    pub(crate) max_instructions_bytes: usize,
    pub(crate) max_script_bytes: usize,
}

impl AdminServices {
    pub(crate) fn new(
        context: AdminContext,
        skills_engine: SkillsEngine,
        mcp_manager: Arc<McpManager>,
        server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
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

    pub(crate) fn read_config(&self) -> Result<AppConfig> {
        match &self.context.config_service {
            Some(service) => Ok(service.snapshot()?.config),
            None => Ok(ConfigLoader::load()?.config().clone()),
        }
    }

    pub(crate) fn config_path(&self) -> Result<PathBuf> {
        match &self.context.config_service {
            Some(service) => Ok(service.path()?),
            None => Ok(ConfigLoader::default_path()),
        }
    }

    fn config_service(&self) -> Result<&haven_common::config::ConfigService> {
        self.context
            .config_service
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))
    }

    async fn rebuild_catalog(&self) -> Result<()> {
        if let Some(tool_control) = &self.context.tool_control {
            tool_control.rebuild_catalog().await?;
        }
        Ok(())
    }

    pub(crate) async fn config_get(&self, path: Option<&str>) -> Result<Value> {
        let config = self.read_config()?;
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            let mut settings = serde_json::to_value(Settings::from(&config))?;
            super::super::admin_support::mask_sensitive_config(&mut settings);
            return Ok(settings);
        };
        let mut root = serde_json::to_value(&config)?;
        super::super::admin_support::mask_sensitive_config(&mut root);
        super::super::admin_support::value_at(&root, path)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("config key '{}' not found", path))
    }

    pub(crate) async fn logs_level(&self, level: LogLevel) -> Result<Value> {
        let update = self
            .config_service()?
            .apply_patch(ConfigPatch::LogLevel(level.clone()))?;
        if let Some(log_level) = &self.context.log_level {
            log_level.set_level(&level)?;
        }
        Ok(serde_json::json!({
            "level": level,
            "saved": true,
            "version": update.snapshot.version,
        }))
    }

    pub(crate) async fn tool_set(&self, name: &str, enabled: bool) -> Result<Value> {
        let mut settings = self.config_service()?.snapshot()?.config.tool_settings;
        settings.entry(name.to_string()).or_default().enabled = enabled;
        self.config_service()?
            .apply_patch(ConfigPatch::Tools(settings))?;
        if let Some(tool_control) = &self.context.tool_control {
            tool_control.set_tool_enabled(name, enabled).await?;
        }
        Ok(serde_json::json!({
            "name": name,
            "enabled": enabled,
            "saved": true,
            "note": "take effect immediately",
        }))
    }

    pub(crate) async fn diagnostics_status(&self) -> Result<Value> {
        let mut out = serde_json::json!({});
        match self.read_config() {
            Ok(config) => {
                out["config_path"] = self.config_path()?.to_string_lossy().to_string().into();
                let mut settings = serde_json::to_value(Settings::from(&config))?;
                super::super::admin_support::mask_sensitive_config(&mut settings);
                out["settings"] = settings;
            }
            Err(error) => {
                out["config_error"] = sanitize_diagnostic(&error.to_string()).into();
            }
        }

        if let Some(router) = &self.context.router {
            let mut health = serde_json::Map::new();
            for request in RequestKind::ALL {
                let configured = router.is_request_configured(*request).await;
                let status = if !configured {
                    "not_configured".to_string()
                } else {
                    match router.health_check(*request).await {
                        Ok(()) => "ok".to_string(),
                        Err(error) => {
                            format!("error: {}", sanitize_diagnostic(&error.to_string()))
                        }
                    }
                };
                health.insert(
                    request.as_str().to_string(),
                    serde_json::json!({"configured": configured, "status": status}),
                );
            }
            out["models"] = Value::Object(health);
        }

        let schemas = self.registry.list_schemas().await;
        let names: Vec<Value> = schemas
            .iter()
            .map(|schema| schema["name"].clone())
            .collect();
        out["tools"] = serde_json::json!({"count": schemas.len(), "names": names});

        match self.mcp_status().await {
            Ok(mcp) => out["mcp"] = mcp,
            Err(error) => out["mcp_error"] = sanitize_diagnostic(&error.to_string()).into(),
        }

        let skills: Vec<Value> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name,
                    "enabled": skill.enabled,
                    "description": skill.description,
                })
            })
            .collect();
        out["skills"] = Value::Array(skills);

        if let Some(db) = &self.context.db {
            let mut counts: HashMap<String, usize> = HashMap::new();
            match db.list_sessions(50, 0) {
                Ok(sessions) => {
                    for session in &sessions {
                        *counts
                            .entry(session.status.as_str().to_string())
                            .or_default() += 1;
                    }
                }
                Err(error) => {
                    tracing::warn!(error = %error, "admin diagnostics list_sessions failed")
                }
            }
            let total = match db.count_sessions() {
                Ok(total) => total,
                Err(error) => {
                    tracing::warn!(error = %error, "admin diagnostics count_sessions failed");
                    0
                }
            };
            out["sessions"] = serde_json::json!({
                "total": total,
                "recent_50_by_status": counts,
            });
        } else {
            out["sessions"] = serde_json::json!({"unavailable": true});
        }

        let log_path = self
            .context
            .log_path
            .as_ref()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|| LogConfig::default_log_path().to_string_lossy().to_string());
        out["log"] = serde_json::json!({"path": log_path});
        Ok(out)
    }

    pub(crate) async fn logs_tail(&self, limit: Option<i64>) -> Result<Value> {
        let limit = limit.unwrap_or(50).clamp(1, 500) as usize;
        let path = self
            .context
            .log_path
            .clone()
            .unwrap_or_else(LogConfig::default_log_path);
        let content = match tokio::fs::read(&path).await {
            Ok(bytes) => haven_common::encoding::decode_lossy(&bytes),
            Err(error) => {
                return Ok(serde_json::json!({
                    "path": path.to_string_lossy(),
                    "error": format!("cannot read log file: {error}"),
                }));
            }
        };
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();
        let start = lines.len().saturating_sub(limit);
        let lines: Vec<String> = lines[start..]
            .iter()
            .map(|line| sanitize_log_line(line))
            .collect();
        Ok(serde_json::json!({
            "path": path.to_string_lossy(),
            "total_lines": total_lines,
            "lines": lines,
        }))
    }

    pub(crate) async fn sessions(&self, limit: Option<i64>) -> Result<Value> {
        let Some(db) = &self.context.db else {
            return Ok(serde_json::json!({"unavailable": true}));
        };
        let sessions = db.list_sessions(limit.unwrap_or(10).clamp(1, 50), 0)?;
        let rows: Vec<Value> = sessions
            .into_iter()
            .map(|session| {
                serde_json::json!({
                    "id": session.id,
                    "status": session.status,
                    "title": session.title,
                    "input_chars": session.input_text.chars().count(),
                    "created_at": session.created_at,
                    "updated_at": session.updated_at,
                })
            })
            .collect();
        Ok(serde_json::json!({"sessions": rows}))
    }

    pub(crate) async fn errors(&self, limit: Option<i64>) -> Result<Value> {
        let Some(db) = &self.context.db else {
            return Ok(serde_json::json!({"unavailable": true}));
        };
        let sessions = db.list_sessions(limit.unwrap_or(10).clamp(1, 50), 0)?;
        let rows: Vec<Value> = sessions
            .into_iter()
            .filter(|session| session.status == haven_common::SessionStatus::Error)
            .map(|session| {
                serde_json::json!({
                    "id": session.id,
                    "title": session.title,
                    "input_chars": session.input_text.chars().count(),
                    "created_at": session.created_at,
                    "transcript_chars": session.transcript.chars().count(),
                })
            })
            .collect();
        Ok(serde_json::json!({"errors": rows}))
    }

    pub(crate) async fn skills_list(&self) -> Result<Value> {
        let skills: Vec<Value> = self
            .skills_engine
            .list()
            .await
            .into_iter()
            .map(|skill| {
                serde_json::json!({
                    "name": skill.name,
                    "enabled": skill.enabled,
                    "description": skill.description,
                    "root": skill.root,
                })
            })
            .collect();
        Ok(serde_json::json!({"skills": skills}))
    }

    pub(crate) async fn skill_set(&self, name: &str, enabled: bool) -> Result<Value> {
        let config_service = Arc::clone(
            self.context
                .config_service
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))?,
        );
        if self.skills_engine.get_skill(name).await.is_none() {
            anyhow::bail!("skill '{}' not found", name);
        }
        self.skills_engine.set_enabled(name, enabled).await?;
        let filter = self.skills_engine.enabled_filter().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = filter;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = self.skills_engine.set_enabled(name, !enabled).await {
                tracing::error!(
                    skill = name,
                    error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                    "skill enable rollback failed after config persistence error"
                );
            }
            return Err(error);
        }
        self.rebuild_catalog().await?;
        Ok(serde_json::json!({
            "name": name,
            "enabled": enabled,
            "saved": true,
            "note": "take effect immediately for new loads",
        }))
    }

    pub(crate) async fn skill_create(
        &self,
        name: &str,
        description: &str,
        instructions: &str,
        language: Option<&str>,
        version: Option<&str>,
        script: Option<&str>,
    ) -> Result<Value> {
        let config_service = Arc::clone(
            self.context
                .config_service
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("configuration administration is unavailable"))?,
        );
        validate_skill_name(name)?;
        if description.is_empty() {
            anyhow::bail!("description is required");
        }
        if instructions.is_empty() {
            anyhow::bail!("instructions are required (the '## Instructions' body)");
        }
        if instructions.len() > self.max_instructions_bytes {
            anyhow::bail!(
                "instructions too large (max {} bytes)",
                self.max_instructions_bytes
            );
        }
        let language = language.unwrap_or("python");
        if language != "python" {
            anyhow::bail!("unsupported language '{language}': only 'python' is supported");
        }
        let version = version
            .map(|value| value.replace(['\n', '\r'], " ").trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(version) = &version
            && version.len() > 64
        {
            anyhow::bail!("version too long (max 64 characters)");
        }
        if let Some(script) = script
            && script.len() > self.max_script_bytes
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
        let mut markdown =
            format!("# Skill: {name}\n\n## Metadata\n- name: {name}\n- description: {desc_line}\n");
        if let Some(version) = &version {
            markdown.push_str(&format!("- version: {version}\n"));
        }
        markdown.push_str(&format!(
            "- language: {language}\n\n## Instructions\n{instructions}\n"
        ));
        tokio::fs::write(skill_dir.join("SKILL.md"), markdown).await?;

        let mut has_script = false;
        if let Some(script) = script {
            let scripts = skill_dir.join("scripts");
            tokio::fs::create_dir_all(&scripts).await?;
            tokio::fs::write(scripts.join("main.py"), script).await?;
            has_script = true;
        }

        self.skills_engine.refresh_from_disk().await?;
        self.skills_engine.set_enabled(name, true).await?;
        let filter = self.skills_engine.enabled_filter().await;
        let snapshot = config_service.snapshot()?;
        let mut skills = snapshot.config.skills;
        skills.enabled = filter;
        if let Err(error) = config_service.apply_patch(ConfigPatch::Skills {
            config: skills,
            exec: snapshot.config.skills_exec,
        }) {
            if let Err(rollback) = tokio::fs::remove_dir_all(&skill_dir).await {
                tracing::warn!(path = %skill_dir.display(), error = %rollback, "skill creation rollback failed");
            }
            if let Err(refresh_error) = self.skills_engine.refresh_from_disk().await {
                tracing::warn!(error = %haven_common::error::sanitize_error_text(&refresh_error.to_string()), "skill catalog refresh failed while rolling back");
            }
            return Err(error);
        }
        self.rebuild_catalog().await?;
        Ok(serde_json::json!({
            "name": name,
            "created": true,
            "root": skill_dir.to_string_lossy(),
            "has_script": has_script,
        }))
    }

    pub(crate) async fn mcp_status(&self) -> Result<Value> {
        let servers: Vec<McpServerConfig> = {
            let configs = self.server_configs.read().await;
            if !configs.is_empty() {
                configs.values().cloned().collect()
            } else {
                self.read_config()?.mcp_servers.clone()
            }
        };
        let mut out = Vec::with_capacity(servers.len());
        for config in &servers {
            let client = self.mcp_manager.get_client(&config.name).await;
            let (connected, tool_count, last_error, diagnostic) = match &client {
                Some(client) => {
                    let status = client.status().await;
                    let connected = matches!(status, McpClientStatus::Connected);
                    let error = match status {
                        McpClientStatus::Offline { error } => error,
                        _ => client.last_error().await.unwrap_or_default(),
                    };
                    (
                        connected,
                        client.tools_cache().await.len(),
                        sanitize_diagnostic(&error),
                        client
                            .diagnostic()
                            .await
                            .map(|text| sanitize_diagnostic(&text)),
                    )
                }
                None => (false, 0, String::new(), None),
            };
            out.push(serde_json::json!({
                "name": config.name,
                "enabled": config.enabled,
                "connected": connected,
                "tools": tool_count,
                "last_error": last_error,
                "diagnostic": diagnostic,
            }));
        }
        Ok(Value::Array(out))
    }

    pub(crate) async fn mcp_connect(&self, name: &str) -> Result<Value> {
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
        self.mcp_manager.remove_client(name).await;
        self.mcp_manager.connect_server(&config).await?;
        Ok(serde_json::json!({"name": name, "connected": true}))
    }

    pub(crate) async fn mcp_disconnect(&self, name: &str) -> Result<Value> {
        self.mcp_manager.remove_client(name).await;
        Ok(serde_json::json!({"name": name, "connected": false}))
    }

    pub(crate) async fn mcp_add(&self, fields: &McpAddFields) -> Result<Value> {
        let command = fields.command.clone().filter(|value| !value.is_empty());
        let url = fields.url.clone().filter(|value| !value.is_empty());
        match fields.transport {
            McpTransportType::Stdio if command.is_none() => {
                anyhow::bail!("command is required (the binary to spawn) for stdio servers")
            }
            McpTransportType::Http if url.is_none() => {
                anyhow::bail!("url is required (the HTTP endpoint) for http servers")
            }
            _ => {}
        }
        let config = McpServerConfig {
            name: fields.name.clone(),
            transport: fields.transport.clone(),
            command: command.unwrap_or_default(),
            args: fields.args.clone(),
            env: fields.env.clone(),
            cwd: fields.cwd.clone().filter(|value| !value.is_empty()),
            url: url.unwrap_or_default(),
            enabled: fields.enabled,
        };
        if let Some(existing) = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|server| server.name == fields.name)
            .cloned()
        {
            return self
                .apply_mcp_config_update(&existing, &config, fields.auto_connect)
                .await;
        }
        let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
        servers.push(config.clone());
        self.config_service()?
            .apply_patch(ConfigPatch::McpServers(servers))?;
        self.server_configs
            .write()
            .await
            .insert(config.name.clone(), config.clone());
        self.mcp_manager.invalidate_catalog();

        let mut result = serde_json::json!({
            "name": config.name,
            "enabled": config.enabled,
            "saved": true,
        });
        if config.enabled && fields.auto_connect {
            match self.mcp_manager.connect_server(&config).await {
                Ok(()) => result["connected"] = serde_json::json!(true),
                Err(error) => {
                    result["connected"] = serde_json::json!(false);
                    result["warning"] = format!(
                        "config saved but connect failed: {}",
                        sanitize_diagnostic(&error.to_string())
                    )
                    .into();
                }
            }
        } else {
            result["connected"] = serde_json::json!(false);
        }
        self.rebuild_catalog().await?;
        Ok(result)
    }

    pub(crate) async fn mcp_update(&self, fields: &McpUpdateFields) -> Result<Value> {
        let existing = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|server| server.name == fields.name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", fields.name))?;
        let mut updated = existing.clone();
        if let Some(command) = fields.command.as_deref().filter(|value| !value.is_empty()) {
            updated.command = command.to_string();
        }
        if let Some(args) = &fields.args {
            updated.args = args.clone();
        }
        if let Some(env) = &fields.env {
            updated.env = env.clone();
        }
        if let Some(cwd) = &fields.cwd {
            updated.cwd = Some(cwd.clone());
        }
        if let Some(url) = &fields.url {
            updated.url = url.clone();
        }
        if let Some(transport) = &fields.transport {
            updated.transport = transport.clone();
        }
        if let Some(enabled) = fields.enabled {
            updated.enabled = enabled;
        }
        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    pub(crate) async fn mcp_toggle(&self, name: &str, enabled: bool) -> Result<Value> {
        let existing = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|server| server.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;
        let mut updated = existing.clone();
        updated.enabled = enabled;
        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    pub(crate) async fn mcp_remove(&self, name: &str) -> Result<Value> {
        let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
        let before = servers.len();
        servers.retain(|server| server.name != name);
        if servers.len() == before {
            anyhow::bail!("MCP server '{}' not found in config", name);
        }
        self.config_service()?
            .apply_patch(ConfigPatch::McpServers(servers))?;
        self.mcp_manager.remove_client(name).await;
        self.server_configs.write().await.remove(name);
        self.mcp_manager.invalidate_catalog();
        self.rebuild_catalog().await?;
        Ok(serde_json::json!({"name": name, "removed": true, "connected": false}))
    }

    pub(crate) async fn mcp_reload(&self) -> Result<Value> {
        let servers = self.read_config()?.mcp_servers.clone();
        let mut map = self.server_configs.write().await;
        map.clear();
        for server in &servers {
            map.insert(server.name.clone(), server.clone());
        }
        drop(map);
        self.mcp_manager.invalidate_catalog();
        for name in self.mcp_manager.list_clients().await {
            self.mcp_manager.remove_client(&name).await;
        }
        let mut connected = Vec::new();
        for server in &servers {
            if !server.enabled {
                continue;
            }
            match self.mcp_manager.connect_server(server).await {
                Ok(()) => {
                    connected.push(serde_json::json!({"name": server.name, "connected": true}))
                }
                Err(error) => connected.push(serde_json::json!({
                    "name": server.name,
                    "connected": false,
                    "error": sanitize_diagnostic(&error.to_string()),
                })),
            }
        }
        self.rebuild_catalog().await?;
        Ok(serde_json::json!({"reloaded": true, "connected": connected}))
    }

    async fn apply_mcp_config_update(
        &self,
        old_config: &McpServerConfig,
        new_config: &McpServerConfig,
        connect_if_enabled: bool,
    ) -> Result<Value> {
        let name = new_config.name.clone();
        let config_changed = old_config.transport != new_config.transport
            || old_config.command != new_config.command
            || old_config.args != new_config.args
            || old_config.env != new_config.env
            || old_config.url != new_config.url;
        let will_enable = new_config.enabled && !old_config.enabled;
        if new_config.enabled && connect_if_enabled && (will_enable || config_changed) {
            self.mcp_manager.remove_client(&name).await;
            if let Err(error) = self.mcp_manager.connect_server(new_config).await {
                if old_config.enabled
                    && let Err(rollback) = self.mcp_manager.connect_server(old_config).await
                {
                    tracing::error!(
                        server = name,
                        error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                        "MCP reconnect rollback failed after config update failure"
                    );
                }
                anyhow::bail!(
                    "MCP server '{}' not connected; config left unchanged: {}",
                    name,
                    sanitize_diagnostic(&error.to_string())
                );
            }
        } else if !new_config.enabled || config_changed {
            self.mcp_manager.remove_client(&name).await;
        }

        let persist_result = (|| -> Result<()> {
            let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
            let Some(existing) = servers.iter_mut().find(|server| server.name == name) else {
                anyhow::bail!("MCP server '{}' not found in config", name);
            };
            *existing = new_config.clone();
            self.config_service()?
                .apply_patch(ConfigPatch::McpServers(servers))?;
            Ok(())
        })();
        if let Err(error) = persist_result {
            self.mcp_manager.remove_client(&name).await;
            if old_config.enabled
                && let Err(rollback) = self.mcp_manager.connect_server(old_config).await
            {
                tracing::error!(
                    server = name,
                    error = %haven_common::error::sanitize_error_text(&rollback.to_string()),
                    "MCP reconnect rollback failed after config persistence error"
                );
            }
            return Err(error);
        }
        self.server_configs
            .write()
            .await
            .insert(name.clone(), new_config.clone());
        self.mcp_manager.invalidate_catalog();
        self.rebuild_catalog().await?;
        Ok(serde_json::json!({
            "name": name,
            "enabled": new_config.enabled,
            "saved": true,
            "connected": self.mcp_manager.get_client(&name).await.is_some(),
        }))
    }
}

pub(crate) fn validate_skill_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        });
    if !valid {
        anyhow::bail!(
            "invalid skill name '{}': use 1-128 characters of a-z, A-Z, 0-9, '-' or '_'",
            name
        );
    }
    Ok(())
}

pub(crate) fn sanitize_log_line(line: &str) -> String {
    const SENSITIVE_MARKERS: &[&str] = &[
        "api_key",
        "access_token",
        "authorization",
        "password",
        "secret",
        "prompt",
        "transcript",
        "message",
        "content",
        "command",
        "stdout",
        "stderr",
    ];
    let lower = line.to_ascii_lowercase();
    if SENSITIVE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return "[redacted diagnostic line]".into();
    }
    let mut bounded = line.to_string();
    if bounded.chars().count() > 512 {
        let cut = bounded.floor_char_boundary(512);
        bounded.truncate(cut);
        bounded.push('…');
    }
    bounded
}

pub(crate) fn sanitize_diagnostic(text: &str) -> String {
    sanitize_log_line(text)
}
