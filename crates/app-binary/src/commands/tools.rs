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
    state.runtime.tools.reset_tool_circuits();
    Ok(())
}

/// Queue a one-time confirmation before promoting one created file from a
/// completed sandbox review. The source is selected by its owner-held index;
/// the renderer never supplies a sandbox path or filesystem capability.
#[tauri::command]
pub async fn promote_sandbox_output(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    session_id: String,
    source_step_id: String,
    change_index: u32,
    destination: String,
) -> Result<(), String> {
    crate::commands::validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| crate::commands::log_err("promote_sandbox_output", error))?;
    crate::commands::validate_command_id(&source_step_id, "source_step_id", "step")
        .map_err(|error| crate::commands::log_err("promote_sandbox_output", error))?;

    #[cfg(windows)]
    {
        let request = state
            .runtime
            .tools
            .sandbox_output_promotion_authorization_request(
                &session_id,
                &source_step_id,
                change_index,
                std::path::Path::new(&destination),
            )
            .map_err(|error| crate::commands::log_err("promote_sandbox_output", error))?;
        match state
            .runtime
            .services
            .authorization
            .authorize(&request)
            .await
        {
            haven_tools::AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
                Err(crate::commands::queue_ui_confirmation(
                    &state,
                    &app,
                    request,
                    receipt,
                    crate::app_state::UiConfirmationAction::SandboxOutputPromotion,
                )
                .await?)
            }
            haven_tools::AuthorizationDecision::AutoApproved => Err(crate::commands::log_err(
                "promote_sandbox_output",
                "sandbox output promotion must always require explicit confirmation",
            )),
            haven_tools::AuthorizationDecision::Blocked { reason, .. } => {
                Err(crate::commands::log_err("promote_sandbox_output", reason))
            }
        }
    }

    #[cfg(not(windows))]
    {
        let _ = (app, change_index, destination);
        Err(crate::commands::log_err(
            "promote_sandbox_output",
            "sandbox output promotion is available only on Windows",
        ))
    }
}
