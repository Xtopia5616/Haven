use crate::app_state::AppState;
use crate::commands::confirmation::UiConfirmationAction;
use crate::commands::log_err;
use crate::commands::process::spawn_detached;
use crate::commands::{
    app_command_authorization_request, emit_event_logged, queue_ui_confirmation,
};
use crate::events::{SKILLS_STATUS_CHANGED_EVENT, SkillsStatusChangedEvent, SkillsStatusOperation};
use haven_common::tools::NetworkAccess;
use haven_common::types::{CapabilityScope, RiskLevel, permission_key};
use haven_tools::{AuthorizationDecision, OperationPolicy, SkillInfo};
use std::path::Path;
use std::sync::Arc;
use tauri::AppHandle;
use tauri::State;

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillExecutionResponse {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

#[tauri::command]
pub async fn list_skills(state: State<'_, Arc<AppState>>) -> Result<Vec<SkillInfo>, String> {
    Ok(state.runtime.services.skills.list_skill_infos().await)
}

#[tauri::command]
pub async fn refresh_skills(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let _config_apply_guard = state.runtime.config_runtime_coordinator.lock().await;
    // Re-scan skills from the configured (or default) skills directory (M4-01).
    state
        .runtime
        .services
        .skills
        .refresh_from_disk()
        .await
        .map_err(|e| log_err("refresh_skills", e))?;
    // Rebuild tool catalog so skills appear in the Reasoner's tool list.
    state
        .runtime
        .tools
        .rebuild_catalog()
        .await
        .map_err(|error| log_err("refresh_skills catalog", error))?;
    // Notify the frontend that the registry changed so views can refetch.
    emit_event_logged(
        &app,
        SKILLS_STATUS_CHANGED_EVENT,
        SkillsStatusChangedEvent {
            op: SkillsStatusOperation::Refresh,
        },
        "skills_refreshed",
    );
    Ok(())
}

#[tauri::command]
pub async fn set_skill_enabled(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
    app: AppHandle,
) -> Result<(), String> {
    // Route through the native admin operation: one implementation for
    // the UI toggle and the LLM's skill_enable / skill_disable ops. The op
    // updates the registry allowlist and persists `skills.enabled` to config.toml
    // via ConfigService.
    crate::commands::authorize_admin_request(
        &state,
        &app,
        "set_skill_enabled",
        haven_tools::AdminRequest::Skills(if enabled {
            haven_tools::SkillsOperationArgs::SkillEnable { name }
        } else {
            haven_tools::SkillsOperationArgs::SkillDisable { name }
        }),
    )
    .await?;

    Ok(())
}

#[tauri::command]
pub async fn open_skills_dir(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<(), String> {
    let root = state.runtime.services.skills.resolved_root().await;
    validate_skills_directory(&root, "open_skills_dir")?;
    let params = serde_json::json!({"target": root});
    let capability = CapabilityScope::try_new(permission_key("open_skills_dir", &params))
        .map_err(|error| log_err("open_skills_dir", error))?;
    let policy = OperationPolicy::native(
        "open_skills_dir",
        capability,
        RiskLevel::Low,
        NetworkAccess::None,
    );
    let authorization_request =
        app_command_authorization_request("open_skills_dir", params, policy);
    match state
        .runtime
        .services
        .authorization
        .authorize(&authorization_request)
        .await
    {
        AuthorizationDecision::AutoApproved => {
            open_skills_directory(&root, "open_skills_dir")?;
        }
        AuthorizationDecision::Blocked { .. } => {
            return Err(log_err(
                "open_skills_dir",
                "opening skills directory blocked by security policy",
            ));
        }
        AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
            return Err(queue_ui_confirmation(
                &state,
                &app,
                authorization_request,
                receipt,
                UiConfirmationAction::OpenSkillsDirectory { root },
            )
            .await?);
        }
    }
    Ok(())
}

pub(crate) fn open_skills_directory(root: &Path, ctx: &str) -> Result<(), String> {
    validate_skills_directory(root, ctx)?;
    // Ensure the directory exists so the file manager opens something sensible
    // instead of erroring; users may have an empty skills root on first run.
    std::fs::create_dir_all(root).map_err(|error| log_err(ctx, error))?;
    if !haven_tools::is_safe_local_path(root) {
        return Err(log_err(
            ctx,
            "skills directory changed to an unsafe reparse point",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        spawn_detached(std::process::Command::new("explorer").arg(root.as_os_str()))
            .map_err(|error| log_err(ctx, error))?;
    }
    #[cfg(target_os = "macos")]
    {
        spawn_detached(std::process::Command::new("open").arg(root))
            .map_err(|error| log_err(ctx, error))?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        spawn_detached(std::process::Command::new("xdg-open").arg(root))
            .map_err(|error| log_err(ctx, error))?;
    }
    Ok(())
}

fn validate_skills_directory(root: &Path, ctx: &str) -> Result<(), String> {
    if haven_tools::is_safe_local_path(root) {
        Ok(())
    } else {
        Err(log_err(
            ctx,
            "skills directory contains an unsafe reparse point or cannot be resolved",
        ))
    }
}

#[tauri::command]
pub async fn execute_skill(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    name: String,
    params: serde_json::Value,
) -> Result<SkillExecutionResponse, String> {
    let skill_info = state
        .runtime
        .services
        .skills
        .get_skill_info(&name)
        .await
        .ok_or_else(|| log_err("execute_skill", format!("skill '{}' not found", name)))?;

    if !skill_info.executable {
        return Err(log_err(
            "execute_skill",
            format!("skill '{}' is not executable", name),
        ));
    }

    // Always run the authorization port — UI input must not bypass permanent deny /
    // path/op blocks. Use the same qualified name as SkillToolAdapter so
    // Always grants from agent confirms apply to UI preview.
    let tool_key = haven_tools::skill_tool_name(&name);
    let capability =
        CapabilityScope::try_new(&tool_key).map_err(|error| log_err("execute_skill", error))?;
    let policy = OperationPolicy::external(capability, RiskLevel::High);
    let authorization_request =
        app_command_authorization_request(&tool_key, params.clone(), policy);
    let authorization_check = state
        .runtime
        .services
        .authorization
        .authorize_for_execution(&authorization_request)
        .await;
    let proof = match authorization_check.decision {
        AuthorizationDecision::AutoApproved => authorization_check
            .execution_proof
            .ok_or_else(|| log_err("execute_skill", "authorization proof is unavailable"))?,
        AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
            return Err(queue_ui_confirmation(
                &state,
                &app,
                authorization_request,
                receipt,
                UiConfirmationAction::Skill { name, params },
            )
            .await?);
        }
        AuthorizationDecision::Blocked { reason, .. } => {
            return Err(log_err(
                "execute_skill",
                format!("skill execution blocked by security policy ({reason})"),
            ));
        }
    };

    let cancel = tokio_util::sync::CancellationToken::new();
    let result = state
        .runtime
        .services
        .skill_execution
        .execute_skill(&name, &params, &authorization_request, &proof, cancel)
        .await
        .map_err(|e| log_err("execute_skill", e))?;

    Ok(SkillExecutionResponse {
        success: result.success,
        output: result.output,
        error: result.error,
    })
}

#[cfg(test)]
mod tests {
    use super::SkillExecutionResponse;

    #[test]
    fn dynamic_execution_payloads_have_fixed_outer_shape() {
        let skill = serde_json::to_value(SkillExecutionResponse {
            success: false,
            output: serde_json::json!(null),
            error: Some("blocked".into()),
        })
        .unwrap();
        assert_eq!(skill["error"], "blocked");
    }
}
