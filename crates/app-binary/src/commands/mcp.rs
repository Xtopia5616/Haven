use crate::app_state::AppState;
use crate::commands::confirmation_error;
use crate::commands::connect_and_monitor;
use crate::commands::log_err;
use haven_common::McpServerConfig;
use haven_common::types::RiskLevel;
use haven_tools::{ConfirmationResult, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::Emitter;
use tauri::State;
use tokio_util::sync::CancellationToken;

#[tauri::command]
pub async fn list_mcp_tools(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<McpServerSnapshot>, String> {
    let mut snapshots: HashMap<String, McpServerSnapshot> = state
        .tools
        .mcp_manager
        .snapshot()
        .await
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();

    // Include configured-but-disabled servers (no live client) so the UI can
    // show their state and re-enable them without re-adding.
    for config in state.tools.list_mcp_server_configs().await {
        let entry = snapshots
            .entry(config.name.clone())
            .or_insert_with(|| McpServerSnapshot {
                name: config.name.clone(),
                transport: config.transport.as_str().into(),
                command: config.command.clone(),
                args: config.args.clone(),
                env: config.env.clone(),
                cwd: config.cwd.clone(),
                url: config.url.clone(),
                enabled: config.enabled,
                status: McpClientStatus::Disconnected,
                tools: vec![],
                last_error: None,
                diagnostic: None,
                last_seen_at: None,
            });
        entry.enabled = config.enabled;
    }

    let mut result: Vec<_> = snapshots.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
}

#[tauri::command]
pub async fn reconnect_mcp(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    state
        .tools
        .mcp_manager
        .reconnect(&name)
        .await
        .map_err(|e| log_err("reconnect_mcp", e))?;
    // Restart health monitor for this client
    if let Some(client) = state.tools.mcp_manager.get_client(&name).await {
        let config = haven_common::config::ConfigLoader::load()
            .map_err(|e| log_err("reconnect_mcp", e))?
            .config()
            .mcp_discovery
            .clone();
        let health_interval = std::time::Duration::from_secs(config.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(config.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(config.reconnect_max_ms);
        let max_retries = config.reconnect_max_retries;
        let status_tx = state.tools.mcp_manager.status_tx();
        client.spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            max_retries,
            status_tx,
        );
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct McpRefreshResult {
    /// Servers configured (enabled) with no live client, connected by this
    /// refresh.
    pub added: Vec<String>,
    /// Live clients whose server was removed from config or disabled, now
    /// shut down.
    pub removed: Vec<String>,
    /// Servers whose persisted config changed since their live client was
    /// spawned (command / args / env / url): the old client was torn down and
    /// reconnected with the new config.
    pub updated: Vec<String>,
    /// Enabled configured servers that could not be connected.
    pub failed: Vec<String>,
}

/// Diff-only refresh: reconcile the live MCP clients with the persisted
/// config WITHOUT reconnecting servers that are already connected with an
/// unchanged config. Servers newly configured (or enabled) with no live
/// client are connected; a live client whose persisted config changed is
/// torn down and reconnected so credential / invocation changes take effect;
/// live clients whose server was removed from config or disabled are shut
/// down. Unchanged, already-connected servers keep their live session (a
/// heavy stdio server such as Ghidra is never restarted by a refresh).
/// Per-server reconnection is the card-level Refresh button's job
/// (`reconnect_mcp`).
#[tauri::command]
pub async fn refresh_mcp_servers(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<McpRefreshResult, String> {
    let (servers, discovery) = {
        let loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("refresh_mcp_servers", e))?;
        (
            loader.config().mcp_servers.clone(),
            loader.config().mcp_discovery.clone(),
        )
    };

    // Re-sync the in-memory server config index with the persisted config so
    // the UI snapshot and the MCP server index never diverge after an
    // external config.toml edit.
    {
        let mut map = state.tools.mcp_server_configs.write().await;
        map.clear();
        for server in &servers {
            map.insert(server.name.clone(), server.clone());
        }
    }

    // Single reconciliation rule shared with `McpManager::load_from_config`:
    // diff the live clients against the persisted config.
    let reconcile = state.tools.mcp_manager.reconcile_servers(&servers).await;

    // 1) Enabled servers whose live client was spawned from a different
    //    config → tear the old client down (its monitor must not keep a
    //    stale child process alive), then reconnect below.
    let changed = reconcile.to_connect_changed;
    let mut updated = Vec::new();
    for server in &changed {
        tracing::info!(
            "refresh_mcp_servers: config changed for '{}', reconnecting",
            server.name
        );
        state.tools.mcp_manager.remove_client(&server.name).await;
        updated.push(server.name.clone());
        let _ = app.emit(
            "mcp:status_change",
            McpStatusChangeEvent {
                name: server.name.clone(),
                status: McpClientStatus::Disconnected,
            },
        );
    }

    // 2) Enabled servers without a live client (new, or torn down above) →
    //    connect with a health monitor.
    let mut added = Vec::new();
    let mut failed = Vec::new();
    for server in reconcile.to_connect_new.into_iter().chain(changed) {
        match connect_and_monitor(&state, &discovery, &server, "refresh_mcp_servers").await {
            Ok(client) => {
                state.tools.mcp_manager.add_client(client).await;
                if !updated.iter().any(|n| n == &server.name) {
                    added.push(server.name.clone());
                }
                let _ = app.emit(
                    "mcp:status_change",
                    McpStatusChangeEvent {
                        name: server.name.clone(),
                        status: McpClientStatus::Connected,
                    },
                );
            }
            Err(e) => {
                tracing::warn!(
                    "refresh_mcp_servers: connect '{}' failed: {}",
                    server.name,
                    e
                );
                failed.push(server.name.clone());
            }
        }
    }

    // 3) Live clients whose server was removed from config or disabled →
    //    shut them down.
    let mut removed = Vec::new();
    for name in reconcile.to_remove {
        state.tools.mcp_manager.remove_client(&name).await;
        removed.push(name.clone());
        let _ = app.emit(
            "mcp:status_change",
            McpStatusChangeEvent {
                name,
                status: McpClientStatus::Disconnected,
            },
        );
    }

    state.tools.rebuild_catalog().await;
    Ok(McpRefreshResult {
        added,
        removed,
        updated,
        failed,
    })
}

#[tauri::command]
pub async fn mcp_tool_call(
    state: State<'_, Arc<AppState>>,
    client: String,
    tool: String,
    args: Value,
) -> Result<Value, String> {
    // Check SafetyGateway (MCP tools default to Medium risk). No session
    // context here — the call is invoked from the UI, not a conversation — so
    // only the threshold applies, never per-session trust.
    match state
        .tools
        .safety_gateway
        .check(
            None,
            &format!("mcp:{}:{}", client, tool),
            &args,
            RiskLevel::Medium,
        )
        .await
    {
        ConfirmationResult::AutoApproved => {}
        ConfirmationResult::RequiresConfirmation {
            tool_name,
            params,
            risk_level,
        } => {
            return Err(confirmation_error(tool_name, params, risk_level)
                .map_err(|e| log_err("mcp_tool_call", e))?);
        }
        ConfirmationResult::Blocked => {
            return Err("MCP tool call blocked by security policy".to_string());
        }
    }

    let cancel = CancellationToken::new();
    let result = state
        .tools
        .mcp_manager
        .call_tool(&client, &tool, args, cancel)
        .await
        .map_err(|e| log_err("mcp_tool_call", e))?;
    Ok(serde_json::json!({
        "success": result.success,
        "output": result.output,
        "error": result.error,
    }))
}

#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, Arc<AppState>>,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Persist to the shared config loader (single source of truth) so the
    // in-memory copy stays in sync with disk and later `self` tool writes
    // can never resurrect stale values.
    let discovery = {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("add_mcp_server", e))?;
        loader.config_mut().mcp_servers.push(config.clone());
        loader.save().map_err(|e| log_err("add_mcp_server", e))?;
        loader.config().mcp_discovery.clone()
    };

    // Create client and connect
    let client = connect_and_monitor(&state, &discovery, &config, "add_mcp_server").await?;
    state.tools.mcp_manager.add_client(client).await;
    // Keep the in-memory server_configs map in sync so `load_mcp` and the
    // MCP server index reflect the newly added server.
    state.tools.upsert_mcp_server_config(config.clone()).await;
    // Rebuild tool catalog so MCP tools appear in the Reasoner's tool list.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name: config.name,
            status: McpClientStatus::Connected,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn update_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let (config_changed, discovery) = {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_mcp_server", e))?;
        let servers = &mut loader.config_mut().mcp_servers;
        let Some(existing) = servers.iter_mut().find(|s| s.name == name) else {
            return Err(format!("MCP server '{}' not found", name));
        };
        let config_changed = existing.transport != config.transport
            || existing.command != config.command
            || existing.args != config.args
            || existing.env != config.env
            || existing.cwd != config.cwd
            || existing.url != config.url;
        *existing = config.clone();
        loader.save().map_err(|e| log_err("update_mcp_server", e))?;
        (config_changed, loader.config().mcp_discovery.clone())
    };
    // If command/args/env changed, reconnect; if only enabled, toggle
    if config_changed {
        state.tools.mcp_manager.remove_client(&name).await;
        if config.enabled {
            let client =
                connect_and_monitor(&state, &discovery, &config, "update_mcp_server").await?;
            state.tools.mcp_manager.add_client(client).await;
        }
    } else if config.enabled {
        // Toggle from disabled to enabled
        let client = connect_and_monitor(&state, &discovery, &config, "update_mcp_server").await?;
        state.tools.mcp_manager.add_client(client).await;
    } else {
        // Disabled: shutdown
        state.tools.mcp_manager.remove_client(&name).await;
    }

    // Keep server_configs in sync with the updated config.
    state.tools.upsert_mcp_server_config(config.clone()).await;

    // Rebuild tool catalog for consistency with add/remove/toggle commands.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name: config.name,
            status: if config.enabled {
                McpClientStatus::Connected
            } else {
                McpClientStatus::Disconnected
            },
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Remove from config via the shared loader (single source of truth).
    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("remove_mcp_server", e))?;
        loader.config_mut().mcp_servers.retain(|s| s.name != name);
        loader.save().map_err(|e| log_err("remove_mcp_server", e))?;
    }

    // Shutdown and remove from manager
    state.tools.mcp_manager.remove_client(&name).await;
    state.tools.remove_mcp_server_config(&name).await;

    // Rebuild tool catalog so removed MCP tools disappear from the Reasoner.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name,
            status: McpClientStatus::Disconnected,
        },
    );
    Ok(())
}

/// Flip the `enabled` flag for an MCP server in the persisted config via the
/// shared loader (single source of truth) and save.
async fn persist_mcp_enabled(
    state: &State<'_, Arc<AppState>>,
    name: &str,
    enabled: bool,
) -> Result<(), String> {
    let mut loader = state
        .config_loader
        .lock()
        .map_err(|e| log_err("toggle_mcp_server", e))?;
    if let Some(existing) = loader
        .config_mut()
        .mcp_servers
        .iter_mut()
        .find(|s| s.name == name)
    {
        existing.enabled = enabled;
    }
    loader.save().map_err(|e| log_err("toggle_mcp_server", e))
}

#[tauri::command]
pub async fn toggle_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let (existing_config, discovery) = {
        let loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("toggle_mcp_server", e))?;
        let Some(existing) = loader.config().mcp_servers.iter().find(|s| s.name == name) else {
            return Err(format!("MCP server '{}' not found", name));
        };
        (existing.clone(), loader.config().mcp_discovery.clone())
    };
    let mut config = existing_config;
    config.enabled = enabled;

    // Persist via the shared loader (single source of truth) so the in-memory
    // copy never diverges from disk.
    if enabled {
        // Reconnect. Connect BEFORE persisting the enabled flag: if the
        // server is unreachable, config must stay disabled so it never
        // diverges from the (absent) live client and monitor.
        let client = connect_and_monitor(&state, &discovery, &config, "toggle_mcp_server").await?;
        persist_mcp_enabled(&state, &name, true).await?;
        state.tools.mcp_manager.add_client(client).await;
    } else {
        // Disable: no connect to validate, persist the flag now.
        persist_mcp_enabled(&state, &name, false).await?;
        state.tools.mcp_manager.remove_client(&name).await;
    }

    // Keep server_configs in sync (enabled flag changed).
    state.tools.upsert_mcp_server_config(config.clone()).await;

    // Rebuild tool catalog so the tool list reflects the toggle.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name,
            status: if enabled {
                McpClientStatus::Connected
            } else {
                McpClientStatus::Disconnected
            },
        },
    );
    Ok(())
}
