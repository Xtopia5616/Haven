use crate::app_state::AppState;
use crate::commands::confirmation_error;
use crate::commands::log_err;
use haven_common::types::RiskLevel;
use haven_tools::{ConfirmationResult, SkillInfo};
use std::sync::Arc;
use tauri::Emitter;
use tauri::State;

#[tauri::command]
pub async fn list_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillInfo>, String> {
    Ok(state.tools.skills_engine.list().await)
}

#[tauri::command]
pub async fn refresh_skills(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Re-scan skills from the configured (or default) skills directory (M4-01).
    state
        .tools
        .skills_engine
        .refresh_from_disk()
        .await
        .map_err(|e| log_err("refresh_skills", e))?;
    // Rebuild tool catalog so skills appear in the Reasoner's tool list.
    state.tools.rebuild_catalog().await;
    // Notify the frontend that the registry changed so views can refetch.
    let _ = app.emit(
        "skills:status_change",
        serde_json::json!({ "op": "refresh" }),
    );
    Ok(())
}

#[tauri::command]
pub async fn set_skill_enabled(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    state
        .tools
        .skills_engine
        .set_enabled(&name, enabled)
        .await
        .map_err(|e| log_err("set_skill_enabled", e))?;
    // The engine's `set_enabled` already syncs its internal filter so the
    // toggle survives `refresh_from_disk`. Persist the filter to config.toml
    // via the shared loader (single source of truth) so it also survives app
    // restart and never diverges from the in-memory copy.
    let filter = state.tools.skills_engine.enabled_filter().await;
    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("set_skill_enabled", e))?;
        loader.config_mut().skills.enabled = filter;
        loader.save().map_err(|e| log_err("set_skill_enabled", e))?;
    }

    // Rebuild tool catalog so the enable/disable takes effect in the Reasoner.
    state.tools.rebuild_catalog().await;

    Ok(())
}

#[tauri::command]
pub async fn set_tool_enabled(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    // Persist via the shared loader (single source of truth) so the in-memory
    // copy never diverges from disk and later settings saves can't resurrect
    // stale values.
    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("set_tool_enabled", e))?;
        let entry = loader
            .config_mut()
            .tool_settings
            .entry(name.clone())
            .or_insert_with(haven_common::config::ToolConfig::default);
        entry.enabled = enabled;
        loader.save().map_err(|e| log_err("set_tool_enabled", e))?;
    }

    // Push the updated settings into the runtime manager. `set_tool_settings`
    // rebuilds the catalog so the toggle takes effect in the Reasoner.
    let tool_settings = {
        let loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("set_tool_enabled", e))?;
        loader.config().tool_settings.clone()
    };
    state.tools.set_tool_settings(tool_settings).await;

    Ok(())
}

#[tauri::command]
pub async fn open_skills_dir(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let root = state.tools.skills_engine.resolved_root().await;
    // Ensure the directory exists so the file manager opens something sensible
    // instead of erroring; users may have an empty skills root on first run.
    let _ = std::fs::create_dir_all(&root);
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(root.as_os_str())
            .spawn()
            .map_err(|e| log_err("open_skills_dir", e))?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&root)
            .spawn()
            .map_err(|e| log_err("open_skills_dir", e))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(&root)
            .spawn()
            .map_err(|e| log_err("open_skills_dir", e))?;
    }
    Ok(root.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn execute_skill(
    state: State<'_, Arc<AppState>>,
    name: String,
    params: serde_json::Value,
    confirmed: Option<bool>,
) -> Result<serde_json::Value, String> {
    let skill_info = state
        .tools
        .skills_engine
        .get(&name)
        .await
        .ok_or_else(|| format!("skill '{}' not found", name))?;

    if !skill_info.enabled {
        return Err(format!("skill '{}' is not enabled", name));
    }

    let risk_level = RiskLevel::Medium;
    if confirmed.unwrap_or(false) {
        state
            .tools
            .safety_gateway
            .trust_risk_level(risk_level)
            .await;
    } else {
        match state
            .tools
            .safety_gateway
            .check(&format!("skill:{}", name), &params, risk_level)
            .await
        {
            ConfirmationResult::AutoApproved => {}
            ConfirmationResult::RequiresConfirmation {
                tool_name,
                params,
                risk_level,
            } => {
                return Err(confirmation_error(tool_name, params, risk_level)
                    .map_err(|e| log_err("execute_skill", e))?);
            }
            ConfirmationResult::Blocked => {
                return Err("skill execution blocked by security policy".to_string());
            }
        }
    }

    let skill = state
        .tools
        .skills_engine
        .get_skill(&name)
        .await
        .ok_or_else(|| format!("skill '{}' not found", name))?;

    let cancel = tokio_util::sync::CancellationToken::new();
    let result = state
        .tools
        .skill_runner
        .read()
        .await
        .execute(&skill, &params, cancel)
        .await
        .map_err(|e| log_err("execute_skill", e))?;

    Ok(serde_json::json!({
        "success": result.success,
        "output": result.output,
        "error": result.error,
    }))
}

#[tauri::command]
pub async fn get_tools(state: State<'_, Arc<AppState>>) -> Result<ToolListResponse, String> {
    // List ALL builtin tools (enabled and disabled) with their enabled state
    // so the UI can toggle them. Disabled tools are excluded from the
    // registry the agent sees (see ToolsManager::rebuild_catalog).
    let tools = state.tools.list_builtin_tools().await;
    Ok(ToolListResponse { tools })
}

#[derive(serde::Serialize)]
pub struct ToolListResponse {
    pub tools: Vec<serde_json::Value>,
}
