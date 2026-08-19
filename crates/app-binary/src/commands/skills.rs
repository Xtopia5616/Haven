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
    // Route through the `self` tool's native entry: one implementation for
    // the UI toggle and the LLM's skill_enable / skill_disable ops. The op
    // flips the engine filter and persists `skills.enabled` to config.toml
    // via the shared loader.
    crate::commands::run_self_op(
        &state,
        "set_skill_enabled",
        haven_tools::SelfParams {
            operation: if enabled {
                haven_tools::SelfOperation::SkillEnable
            } else {
                haven_tools::SelfOperation::SkillDisable
            },
            name: Some(name),
            ..Default::default()
        },
    )
    .await?;

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
    // Route through the `self` tool's native entry: one implementation for
    // the UI switch and the LLM's tool_enable / tool_disable ops. The op
    // persists `tool_settings.<name>.enabled` to config.toml AND applies the
    // runtime change (in-memory tool_settings + catalog rebuild) through the
    // ToolsManager, so the toggle takes effect in the Reasoner immediately.
    crate::commands::run_self_op(
        &state,
        "set_tool_enabled",
        haven_tools::SelfParams {
            operation: if enabled {
                haven_tools::SelfOperation::ToolEnable
            } else {
                haven_tools::SelfOperation::ToolDisable
            },
            name: Some(name),
            ..Default::default()
        },
    )
    .await?;

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
    if !confirmed.unwrap_or(false) {
        match state
            .tools
            .safety_gateway
            .check(None, &format!("skill:{}", name), &params, risk_level)
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
