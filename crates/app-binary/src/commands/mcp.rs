use crate::app_state::{AppState, UiConfirmationAction};
use crate::commands::contracts::McpToolCallResponse;
use crate::commands::log_err;
use crate::commands::queue_ui_confirmation;
use crate::events::{MCP_STATUS_CHANGED_EVENT, McpStatusChangedEvent};
use crate::logging::sanitize_error_text;
use haven_common::McpServerConfig;
use haven_common::types::{RiskLevel, permission_key};
use haven_tools::{
    AuthorizationDecision, AuthorizationRequest, McpClientStatus, McpServerSnapshot, NetworkAccess,
    OperationPolicy,
};
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
        .services
        .mcp
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
                .unwrap_or_else(|| entry.clone())
        })
        .collect();
}

/// Replace the renderer's redaction marker with the already configured value
/// for that variable. This lets users edit the rest of an MCP profile without
/// returning the real value through read IPC or persisting `<redacted>`.
fn resolve_redacted_mcp_environment(
    existing: Option<&[String]>,
    submitted: &[String],
) -> anyhow::Result<Vec<String>> {
    submitted
        .iter()
        .map(|entry| {
            let Some((name, value)) = entry.split_once('=') else {
                return Ok(entry.clone());
            };
            if value != "<redacted>" {
                return Ok(entry.clone());
            }
            let preserved = existing
                .into_iter()
                .flatten()
                .filter_map(|previous| previous.split_once('='))
                .find(|(previous_name, _)| *previous_name == name)
                .map(|(_, previous_value)| previous_value)
                .ok_or_else(|| {
                    anyhow::anyhow!("redacted MCP value has no existing value to preserve")
                })?;
            Ok(format!("{name}={preserved}"))
        })
        .collect()
}

pub(crate) fn emit_mcp_status(
    app: &tauri::AppHandle,
    name: String,
    status: McpClientStatus,
    context: &str,
) {
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
pub async fn reconnect_mcp(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    name: String,
) -> Result<(), String> {
    let snapshot = state
        .config_service
        .snapshot()
        .map_err(|error| log_err("reconnect_mcp", error))?;
    if !snapshot
        .config
        .mcp_servers
        .iter()
        .any(|server| server.name == name && server.enabled)
        || state.services.mcp.get_client(&name).await.is_none()
    {
        return Err(format!("MCP server '{}' is not currently connected", name));
    }
    let request =
        haven_tools::AdminRequest::NativeMcp(haven_tools::NativeMcpOperationArgs::McpReconnect {
            name,
            config_version: snapshot.version,
        });
    crate::commands::authorize_admin_request(&state, &app, "reconnect_mcp", request.clone())
        .await?;
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
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
    let snapshot = state
        .config_service
        .snapshot()
        .map_err(|error| log_err("refresh_mcp_servers", error))?;
    let reconcile = state
        .services
        .mcp
        .reconcile_servers(&snapshot.config.mcp_servers)
        .await;
    let plan = haven_tools::McpRefreshPlan::from_reconcile(snapshot.version, &reconcile);
    let request =
        haven_tools::AdminRequest::NativeMcp(haven_tools::NativeMcpOperationArgs::McpRefresh {
            plan,
        });
    let result = crate::commands::authorize_admin_request(
        &state,
        &app,
        "refresh_mcp_servers",
        request.clone(),
    )
    .await?;
    crate::commands::finalize_admin_ui_operation(&state, &app, &request).await?;
    serde_json::from_value(result.output).map_err(|error| log_err("refresh_mcp_servers", error))
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
    let policy = OperationPolicy::native(
        &tool_key,
        permission_key(&tool_key, &args).into(),
        RiskLevel::High,
        NetworkAccess::Opaque,
    );
    let authorization_request =
        AuthorizationRequest::new(Some("ui"), &tool_key, args.clone(), policy);
    match state
        .services
        .authorization
        .authorize(&authorization_request)
        .await
    {
        AuthorizationDecision::AutoApproved => {}
        AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
            let action_args = args.clone();
            return Err(queue_ui_confirmation(
                &state,
                &app,
                authorization_request,
                receipt,
                UiConfirmationAction::Mcp {
                    client,
                    tool,
                    args: action_args,
                },
            )
            .await?);
        }
        AuthorizationDecision::Blocked { reason, .. } => {
            return Err(format!(
                "MCP tool call blocked by security policy ({reason})"
            ));
        }
    }

    let cancel = CancellationToken::new();
    let result = state
        .services
        .mcp
        .call_tool(&client, &tool, args, cancel)
        .await
        .map_err(|e| log_err("mcp_tool_call", e))?;
    Ok(McpToolCallResponse {
        success: result.success,
        output: result.output,
        error: result.error,
    })
}

#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, Arc<AppState>>,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let env = resolve_redacted_mcp_environment(None, &config.env)
        .map_err(|error| log_err("add_mcp_server", error))?;
    // Route the config mutation through the native admin surface
    // (mcp_add): one implementation for the UI dialog and the LLM. The op
    // persists through ConfigService, keeps the in-memory index in sync, and
    // connects when enabled (UI always adds enabled servers).
    crate::commands::authorize_admin_request(
        &state,
        &app,
        "add_mcp_server",
        haven_tools::AdminRequest::Mcp(haven_tools::McpOperationArgs::McpAdd {
            name: config.name.clone(),
            transport: config.transport.clone(),
            command: Some(config.command.clone()),
            url: Some(config.url.clone()),
            args: config.args.clone(),
            env,
            cwd: config.cwd.clone(),
            enabled: config.enabled,
            auto_connect: config.enabled,
        }),
    )
    .await?;

    // McpManager::connect_server starts the monitor; the admin service already
    // rebuilt the catalog while holding the shared config apply gate.
    let connected = state.services.mcp.get_client(&config.name).await.is_some();
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
    let snapshot = state
        .config_service
        .snapshot()
        .map_err(|error| log_err("update_mcp_server", error))?;
    let existing_env = snapshot
        .config
        .mcp_servers
        .iter()
        .find(|server| server.name == name)
        .map(|server| server.env.as_slice());
    let env = resolve_redacted_mcp_environment(existing_env, &config.env)
        .map_err(|error| log_err("update_mcp_server", error))?;
    // Route through the native admin surface (mcp_update). The op
    // reconnects before persisting when the connection profile changed and
    // rolls the config back on a failed connect (stricter than the old
    // persist-then-connect order).
    crate::commands::authorize_admin_request(
        &state,
        &app,
        "update_mcp_server",
        haven_tools::AdminRequest::Mcp(haven_tools::McpOperationArgs::McpUpdate {
            name: name.clone(),
            transport: Some(config.transport.clone()),
            command: Some(config.command.clone()),
            url: Some(config.url.clone()),
            args: Some(config.args.clone()),
            env: Some(env),
            cwd: config.cwd.clone(),
            enabled: Some(config.enabled),
        }),
    )
    .await?;

    // McpManager::connect_server starts the monitor; the admin service already
    // rebuilt the catalog while holding the shared config apply gate.
    let connected = state.services.mcp.get_client(&name).await.is_some();
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
    crate::commands::authorize_admin_request(
        &state,
        &app,
        "remove_mcp_server",
        haven_tools::AdminRequest::Mcp(haven_tools::McpOperationArgs::McpRemove {
            name: name.clone(),
        }),
    )
    .await?;

    // The admin service removed the client and rebuilt the catalog while
    // holding the shared config apply gate; only the UI status event remains.
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
    crate::commands::authorize_admin_request(
        &state,
        &app,
        "toggle_mcp_server",
        haven_tools::AdminRequest::Mcp(haven_tools::McpOperationArgs::McpToggle {
            name: name.clone(),
            enabled,
        }),
    )
    .await?;

    // McpManager::connect_server starts the monitor; the admin service already
    // rebuilt the catalog while holding the shared config apply gate.
    let connected = state.services.mcp.get_client(&name).await.is_some();
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
    use super::{redact_mcp_snapshot, resolve_redacted_mcp_environment};
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

        assert_eq!(snapshot.env, vec!["API_KEY=<redacted>", "NO_VALUE"]);
    }

    #[test]
    fn mcp_edit_preserves_redacted_values_without_returning_them() {
        let existing = vec!["TOKEN=stored-secret-marker".to_string(), "FLAG".into()];
        let submitted = vec![
            "TOKEN=<redacted>".to_string(),
            "FLAG".into(),
            "NEW=visible".into(),
        ];
        let resolved = resolve_redacted_mcp_environment(Some(&existing), &submitted).unwrap();
        assert_eq!(
            resolved,
            ["TOKEN=stored-secret-marker", "FLAG", "NEW=visible"]
        );
        assert!(resolve_redacted_mcp_environment(None, &["TOKEN=<redacted>".into()]).is_err());
    }
}
