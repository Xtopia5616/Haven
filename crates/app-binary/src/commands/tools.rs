use crate::app_state::AppState;
use crate::commands::contracts::BuiltinToolManifestListResponse;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn list_builtin_tool_manifests(
    state: State<'_, Arc<AppState>>,
) -> Result<BuiltinToolManifestListResponse, String> {
    // Include enabled and disabled builtins so the UI can render their state
    // and let the user change it. Disabled tools stay out of Agent's catalog.
    let tools = state.runtime.tools.list_builtin_manifests().await;
    Ok(BuiltinToolManifestListResponse { tools })
}

#[tauri::command]
pub async fn set_tool_enabled(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
    app: AppHandle,
) -> Result<(), String> {
    crate::commands::authorize_admin_request(
        &state,
        &app,
        "set_tool_enabled",
        haven_tools::AdminRequest::Tools(if enabled {
            haven_tools::ToolsOperationArgs::ToolEnable { name }
        } else {
            haven_tools::ToolsOperationArgs::ToolDisable { name }
        }),
    )
    .await?;
    Ok(())
}

/// Clear every per-tool circuit breaker so previously open tools become
/// callable again without waiting for the cooldown window.
#[tauri::command]
pub async fn reset_tool_circuits(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.runtime.tools.tool_circuits().reset_all();
    Ok(())
}
