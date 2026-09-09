use crate::app_state::{AppState, UiConfirmationAction};
use crate::commands::connect_and_monitor;
use crate::commands::contracts::McpToolCallResponse;
use crate::commands::log_err;
use crate::commands::queue_ui_confirmation;
use crate::events::{MCP_STATUS_CHANGED_EVENT, McpStatusChangedEvent};
use crate::logging::sanitize_error_text;
use haven_common::McpServerConfig;
use haven_common::types::RiskLevel;
use haven_tools::{ConfirmationResult, McpClientStatus, McpServerSnapshot};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::AppHandle;
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
    for snapshot in &mut result {
        redact_mcp_snapshot(snapshot);
    }
    Ok(result)
}

/// MCP env entries are configuration secrets in practice (API keys, bearer
/// tokens, and private endpoints). Keep the shape needed by the settings UI,
/// but never return values across the Tauri boundary.
fn redact_mcp_snapshot(snapshot: &mut McpServerSnapshot) {
    snapshot.env = snapshot
        .env
        .iter()
        .map(|entry| {
            entry
                .split_once('=')
                .map(|(name, _)| format!("{name}=<redacted>"))
                .unwrap_or_else(|| "<redacted>".into())
        })
        .collect();
}

fn emit_mcp_status(app: &tauri::AppHandle, name: String, status: McpClientStatus, context: &str) {
    if let Err(error) = app.emit(
        MCP_STATUS_CHANGED_EVENT,
        McpStatusChangedEvent { name, status },
    ) {
        tracing::warn!(
            context,
            error = %sanitize_error_text(&error.to_string()),
            "failed to emit MCP status event"
        );
    }
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
        let config = state
            .config_service
            .snapshot()
            .map_err(|e| log_err("reconnect_mcp", e))?
            .config
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
    let config = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("refresh_mcp_servers", e))?
        .config;
    let servers = config.mcp_servers;
    let discovery = config.mcp_discovery;

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
        emit_mcp_status(
            &app,
            server.name.clone(),
            McpClientStatus::Disconnected,
            "refresh_mcp_servers",
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
                emit_mcp_status(
                    &app,
                    server.name.clone(),
                    McpClientStatus::Connected,
                    "refresh_mcp_servers",
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
        emit_mcp_status(
            &app,
            name,
            McpClientStatus::Disconnected,
            "refresh_mcp_servers",
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
    app: AppHandle,
    client: String,
    tool: String,
    args: Value,
) -> Result<McpToolCallResponse, String> {
    // Same qualified name + High risk as McpToolAdapter so Always grants from
    // Use the short-lived UI session so session-scope decisions made from a
    // direct invocation apply to subsequent direct invocations in this run.
    let tool_key = haven_tools::McpToolAdapter::qualified_name_of(&client, &tool);
    match state
        .tools
        .authorization
        .check(Some("ui"), &tool_key, &args, RiskLevel::High)
        .await
    {
        ConfirmationResult::AutoApproved => {}
        ConfirmationResult::RequiresConfirmation {
            tool_name,
            params,
            risk_level,
            receipt,
            ..
        } => {
            let action_args = params.clone();
            return Err(queue_ui_confirmation(
                &state,
                &app,
                tool_name,
                params,
                risk_level,
                receipt,
                UiConfirmationAction::Mcp {
                    client,
                    tool,
                    args: action_args,
                },
            )
            .await?);
        }
        ConfirmationResult::Blocked { reason } => {
            return Err(format!(
                "MCP tool call blocked by security policy ({reason})"
            ));
        }
    }

    let cancel = CancellationToken::new();
    let result = state
        .tools
        .mcp_manager
        .call_tool(&client, &tool, args, cancel)
        .await
        .map_err(|e| log_err("mcp_tool_call", e))?;
    Ok(McpToolCallResponse {
        success: result.success,
        output: result.output,
        error: result.error,
    })
}

/// Spawn the health monitor for a live MCP client. The native admin surface's
/// mcp_add/update/toggle ops connect clients without a monitor (the LLM path
/// does not need one), so the app commands re-attach it after routing through
/// the tool — same wiring as `reconnect_mcp`.
async fn spawn_monitor_if_client(state: &AppState, name: &str) -> Result<(), String> {
    let Some(client) = state.tools.mcp_manager.get_client(name).await else {
        return Ok(());
    };
    let discovery = state
        .config_service
        .snapshot()
        .map(|snapshot| snapshot.config.mcp_discovery)
        .map_err(|error| log_err("spawn_monitor_if_client", error))?;
    let health_interval = std::time::Duration::from_secs(discovery.health_interval_secs);
    let initial_backoff = std::time::Duration::from_millis(discovery.reconnect_initial_ms);
    let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
    let max_retries = discovery.reconnect_max_retries;
    let status_tx = state.tools.mcp_manager.status_tx();
    client.spawn_monitor(
        health_interval,
        initial_backoff,
        max_backoff,
        max_retries,
        status_tx,
    );
    Ok(())
}

#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, Arc<AppState>>,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Route the config mutation through the native admin surface
    // (mcp_add): one implementation for the UI dialog and the LLM. The op
    // persists through ConfigService, keeps the in-memory index in sync, and
    // connects when enabled (UI always adds enabled servers).
    crate::commands::run_admin_op(
        &state,
        "add_mcp_server",
        haven_tools::SelfParams {
            operation: haven_tools::SelfOperation::McpAdd,
            name: Some(config.name.clone()),
            transport: Some(config.transport.as_str().to_string()),
            command: Some(config.command.clone()),
            url: Some(config.url.clone()),
            args: Some(config.args.clone()),
            env: Some(config.env.clone()),
            cwd: config.cwd.clone(),
            enabled: Some(config.enabled),
            auto_connect: Some(config.enabled),
            ..Default::default()
        },
    )
    .await?;

    // App-level aftermath: health monitor + catalog rebuild + UI event.
    spawn_monitor_if_client(&state, &config.name).await?;
    state.tools.rebuild_catalog().await;
    let connected = state
        .tools
        .mcp_manager
        .get_client(&config.name)
        .await
        .is_some();
    emit_mcp_status(
        &app,
        config.name,
        if connected {
            McpClientStatus::Connected
        } else {
            McpClientStatus::Disconnected
        },
        "add_mcp_server",
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
    // Route through the native admin surface (mcp_update). The op
    // reconnects before persisting when the connection profile changed and
    // rolls the config back on a failed connect (stricter than the old
    // persist-then-connect order).
    crate::commands::run_admin_op(
        &state,
        "update_mcp_server",
        haven_tools::SelfParams {
            operation: haven_tools::SelfOperation::McpUpdate,
            name: Some(name.clone()),
            transport: Some(config.transport.as_str().to_string()),
            command: Some(config.command.clone()),
            url: Some(config.url.clone()),
            args: Some(config.args.clone()),
            env: Some(config.env.clone()),
            cwd: config.cwd.clone(),
            enabled: Some(config.enabled),
            ..Default::default()
        },
    )
    .await?;

    // App-level aftermath: health monitor + catalog rebuild + UI event.
    spawn_monitor_if_client(&state, &name).await?;
    state.tools.rebuild_catalog().await;
    let connected = state.tools.mcp_manager.get_client(&name).await.is_some();
    emit_mcp_status(
        &app,
        name,
        if connected {
            McpClientStatus::Connected
        } else {
            McpClientStatus::Disconnected
        },
        "update_mcp_server",
    );
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Route through the native admin surface (mcp_remove): removes the
    // server from config via ConfigService, shuts down the live client,
    // and drops it from the in-memory index.
    crate::commands::run_admin_op(
        &state,
        "remove_mcp_server",
        haven_tools::SelfParams {
            operation: haven_tools::SelfOperation::McpRemove,
            name: Some(name.clone()),
            ..Default::default()
        },
    )
    .await?;

    // App-level aftermath: catalog rebuild so removed MCP tools disappear
    // from the Reasoner, plus the UI status event.
    state.tools.rebuild_catalog().await;
    emit_mcp_status(
        &app,
        name,
        McpClientStatus::Disconnected,
        "remove_mcp_server",
    );
    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Route through the native admin surface (mcp_toggle). The op
    // connects before persisting when enabling (rolling the config back on a
    // failed connect) and shuts the live client down when disabling.
    crate::commands::run_admin_op(
        &state,
        "toggle_mcp_server",
        haven_tools::SelfParams {
            operation: haven_tools::SelfOperation::McpToggle,
            name: Some(name.clone()),
            enabled: Some(enabled),
            ..Default::default()
        },
    )
    .await?;

    // App-level aftermath: health monitor + catalog rebuild + UI event.
    spawn_monitor_if_client(&state, &name).await?;
    state.tools.rebuild_catalog().await;
    let connected = state.tools.mcp_manager.get_client(&name).await.is_some();
    emit_mcp_status(
        &app,
        name,
        if connected {
            McpClientStatus::Connected
        } else {
            McpClientStatus::Disconnected
        },
        "toggle_mcp_server",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::redact_mcp_snapshot;
    use haven_tools::{McpClientStatus, McpServerSnapshot};

    #[test]
    fn mcp_snapshot_redacts_environment_values() {
        let mut snapshot = McpServerSnapshot {
            name: "demo".into(),
            transport: "stdio".into(),
            command: "server".into(),
            args: vec![],
            env: vec!["API_KEY=secret".into(), "NO_VALUE".into()],
            cwd: None,
            url: String::new(),
            enabled: true,
            status: McpClientStatus::Disconnected,
            tools: vec![],
            last_error: None,
            diagnostic: None,
            last_seen_at: None,
        };

        redact_mcp_snapshot(&mut snapshot);

        assert_eq!(snapshot.env, vec!["API_KEY=<redacted>", "<redacted>"]);
    }
}
