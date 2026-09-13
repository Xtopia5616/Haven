use anyhow::Result;
use haven_common::config::{ConfigPatch, McpServerConfig};
use haven_common::types::McpTransportType;
use haven_mcp::McpClientStatus;
use serde_json::Value;

use super::{SelfParams, SelfTool, sanitize_diagnostic};

impl SelfTool {
    pub(super) async fn mcp_status(&self) -> Result<Value> {
        let servers: Vec<McpServerConfig> = {
            let configs = self.server_configs.read().await;
            if !configs.is_empty() {
                configs.values().cloned().collect()
            } else {
                // Cold in-memory index (e.g. right after startup, before the
                // first config load): fall back to persisted config.
                self.read_config()?.mcp_servers.clone()
            }
        };

        let mut out = Vec::with_capacity(servers.len());
        for cfg in &servers {
            let client = self.mcp_manager.get_client(&cfg.name).await;
            let (connected, tool_count, last_error, diagnostic) = match &client {
                Some(c) => {
                    let status = c.status().await;
                    let is_connected = matches!(status, McpClientStatus::Connected);
                    let error = match status {
                        McpClientStatus::Offline { error } => error,
                        _ => c.last_error().await.unwrap_or_default(),
                    };
                    (
                        is_connected,
                        c.tools_cache().await.len(),
                        sanitize_diagnostic(&error),
                        c.diagnostic().await.map(|text| sanitize_diagnostic(&text)),
                    )
                }
                None => (false, 0, String::new(), None),
            };
            out.push(serde_json::json!({
                "name": cfg.name,
                "enabled": cfg.enabled,
                "connected": connected,
                "tools": tool_count,
                "last_error": last_error,
                "diagnostic": diagnostic,
            }));
        }
        Ok(Value::Array(out))
    }

    pub(super) async fn op_mcp_list(&self) -> Result<Value> {
        Ok(serde_json::json!({ "servers": self.mcp_status().await? }))
    }

    pub(super) async fn op_mcp_connect(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
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
        self.mcp_manager.remove_client(name).await;
        self.mcp_manager.connect_server(&config).await?;
        Ok(serde_json::json!({ "name": name, "connected": true }))
    }

    pub(super) async fn op_mcp_disconnect(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to disconnect)"))?;
        self.mcp_manager.remove_client(name).await;
        Ok(serde_json::json!({ "name": name, "connected": false }))
    }

    pub(super) async fn op_mcp_add(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to add)"))?;
        let transport = match params.transport.as_deref().unwrap_or("stdio") {
            "stdio" => McpTransportType::Stdio,
            "http" => McpTransportType::Http,
            other => anyhow::bail!("unknown transport '{}' (expected 'stdio' or 'http')", other),
        };
        let command = params
            .command
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        let url = params
            .url
            .as_deref()
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
        let args = params.args.clone().unwrap_or_default();
        let env = params.env.clone().unwrap_or_default();
        let cwd = params
            .cwd
            .as_deref()
            .filter(|c| !c.is_empty())
            .map(str::to_string);
        let enabled = params.enabled.unwrap_or(true);
        let auto_connect = params.auto_connect.unwrap_or(true);

        let config = McpServerConfig {
            name: name.to_string(),
            transport,
            command: command.unwrap_or_default(),
            args,
            env,
            cwd,
            url: url.unwrap_or_default(),
            enabled,
        };

        if let Some(existing) = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
        {
            return self
                .apply_mcp_config_update(&existing, &config, auto_connect)
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
                    result["warning"] = format!(
                        "config saved but connect failed: {}",
                        sanitize_diagnostic(&e.to_string())
                    )
                    .into();
                }
            }
        } else {
            result["connected"] = serde_json::json!(false);
        }
        Ok(result)
    }

    pub(super) async fn op_mcp_update(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to update)"))?;
        let existing = self
            .read_config()?
            .mcp_servers
            .iter()
            .find(|s| s.name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP server '{}' not found in config", name))?;

        let mut updated = existing.clone();
        if let Some(command) = params.command.as_deref().filter(|c| !c.is_empty()) {
            updated.command = command.to_string();
        }
        if params.args.is_some() {
            updated.args = params.args.clone().unwrap_or_default();
        }
        if params.env.is_some() {
            updated.env = params.env.clone().unwrap_or_default();
        }
        if params.cwd.is_some() {
            updated.cwd = params.cwd.clone();
        }
        if let Some(url) = params.url.clone() {
            updated.url = url;
        }
        if let Some(transport) = params.transport.as_deref() {
            updated.transport = match transport {
                "stdio" => McpTransportType::Stdio,
                "http" => McpTransportType::Http,
                other => {
                    anyhow::bail!("unknown transport '{}' (expected 'stdio' or 'http')", other)
                }
            };
        }
        if let Some(enabled) = params.enabled {
            updated.enabled = enabled;
        }

        self.apply_mcp_config_update(&existing, &updated, true)
            .await
    }

    pub(super) async fn op_mcp_toggle(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to toggle)"))?;
        let enabled = params
            .enabled
            .ok_or_else(|| anyhow::anyhow!("enabled (boolean) is required for mcp_toggle"))?;
        let existing = self
            .read_config()?
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

    pub(super) async fn op_mcp_remove(&self, params: &SelfParams) -> Result<Value> {
        let name = params
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("name is required (the MCP server to remove)"))?;
        let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
        let before = servers.len();
        servers.retain(|s| s.name != name);
        if servers.len() == before {
            anyhow::bail!("MCP server '{}' not found in config", name);
        }
        self.config_service()?
            .apply_patch(ConfigPatch::McpServers(servers))?;
        self.mcp_manager.remove_client(name).await;
        self.server_configs.write().await.remove(name);
        Ok(serde_json::json!({
            "name": name,
            "removed": true,
            "connected": false,
        }))
    }

    pub(super) async fn op_mcp_reload(&self) -> Result<Value> {
        let servers = self.read_config()?.mcp_servers.clone();

        let mut map = self.server_configs.write().await;
        map.clear();
        for s in &servers {
            map.insert(s.name.clone(), s.clone());
        }
        drop(map);

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
                        "error": sanitize_diagnostic(&e.to_string()),
                    }));
                }
            }
        }
        Ok(serde_json::json!({ "reloaded": true, "connected": connected }))
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
            if let Err(e) = self.mcp_manager.connect_server(new_config).await {
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
                    sanitize_diagnostic(&e.to_string())
                );
            }
        } else if !new_config.enabled || config_changed {
            self.mcp_manager.remove_client(&name).await;
        }

        let persist_result = (|| {
            let mut servers = self.config_service()?.snapshot()?.config.mcp_servers;
            let Some(existing) = servers.iter_mut().find(|s| s.name == name) else {
                anyhow::bail!("MCP server '{}' not found in config", name);
            };
            *existing = new_config.clone();
            self.config_service()?
                .apply_patch(ConfigPatch::McpServers(servers))?;
            Ok(())
        })();
        if let Err(e) = persist_result {
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
            return Err(e);
        }

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
}
