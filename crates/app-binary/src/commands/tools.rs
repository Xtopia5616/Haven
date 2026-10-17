use crate::app_state::AppState;
use std::sync::Arc;
use tauri::{AppHandle, State};

/// Renderer-safe projection of one ephemeral sandbox review. The source tree,
/// private workspace, and file contents remain owned by Tools.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxOutputReviewManifestDto {
    pub session_id: String,
    pub source_step_id: String,
    pub expires_at_unix_ms: u64,
    pub storage_charge_bytes: u64,
    pub changes: Vec<SandboxOutputReviewChangeDto>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxOutputReviewChangeDto {
    pub change_index: u32,
    pub relative_path: String,
    pub change: SandboxOutputReviewChangeKindDto,
    pub before_bytes: Option<u64>,
    pub after_bytes: Option<u64>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub promoted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxOutputReviewChangeKindDto {
    Created,
    Modified,
    Deleted,
    TypeChanged,
}

#[cfg(windows)]
impl From<haven_tools::SandboxOutputReviewManifest> for SandboxOutputReviewManifestDto {
    fn from(manifest: haven_tools::SandboxOutputReviewManifest) -> Self {
        Self {
            session_id: manifest.session_id,
            source_step_id: manifest.source_step_id,
            expires_at_unix_ms: manifest.expires_at_unix_ms,
            storage_charge_bytes: manifest.storage_charge_bytes,
            changes: manifest.changes.into_iter().map(Into::into).collect(),
        }
    }
}

#[cfg(windows)]
impl From<haven_tools::SandboxOutputReviewChange> for SandboxOutputReviewChangeDto {
    fn from(change: haven_tools::SandboxOutputReviewChange) -> Self {
        Self {
            change_index: change.change_index,
            relative_path: change.relative_path,
            change: match change.change {
                haven_tools::SandboxOutputReviewChangeKind::Created => {
                    SandboxOutputReviewChangeKindDto::Created
                }
                haven_tools::SandboxOutputReviewChangeKind::Modified => {
                    SandboxOutputReviewChangeKindDto::Modified
                }
                haven_tools::SandboxOutputReviewChangeKind::Deleted => {
                    SandboxOutputReviewChangeKindDto::Deleted
                }
                haven_tools::SandboxOutputReviewChangeKind::TypeChanged => {
                    SandboxOutputReviewChangeKindDto::TypeChanged
                }
            },
            before_bytes: change.before_bytes,
            after_bytes: change.after_bytes,
            before_sha256: change.before_sha256,
            after_sha256: change.after_sha256,
            promoted: change.promoted,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BuiltinToolManifestListResponse {
    pub tools: Vec<haven_common::tools::ToolManifest>,
}

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

/// Read only the bounded review summary owned by Tools for this terminal Shell
/// or isolated Skill step. Source paths and contents never cross this command boundary.
#[tauri::command]
pub async fn get_sandbox_output_review(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    source_step_id: String,
) -> Result<Option<SandboxOutputReviewManifestDto>, String> {
    validate_sandbox_review_identity(&session_id, &source_step_id)?;
    validate_sandbox_review_step(&state, &session_id, &source_step_id).await?;

    #[cfg(windows)]
    {
        state
            .runtime
            .services
            .sandbox_output_reviews
            .manifest(&session_id, &source_step_id)
            .await
            .map(|manifest| manifest.map(Into::into))
            .map_err(|error| crate::commands::log_err("get_sandbox_output_review", error))
    }

    #[cfg(not(windows))]
    {
        Ok(None)
    }
}

/// Drop the private output and baseline for one reviewed Shell or Skill step.
#[tauri::command]
pub async fn discard_sandbox_output_review(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    source_step_id: String,
) -> Result<bool, String> {
    validate_sandbox_review_identity(&session_id, &source_step_id)?;
    validate_sandbox_review_step(&state, &session_id, &source_step_id).await?;

    #[cfg(windows)]
    {
        state
            .runtime
            .services
            .sandbox_output_reviews
            .discard(&session_id, &source_step_id)
            .await
            .map_err(|error| crate::commands::log_err("discard_sandbox_output_review", error))
    }

    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

/// Ask the native save dialog for a candidate path. Selection does not grant
/// access; Files policy and once-only confirmation still govern the write.
#[tauri::command]
pub async fn pick_sandbox_output_destination(
    window: tauri::WebviewWindow,
    suggested_name: String,
) -> Result<Option<String>, String> {
    #[cfg(windows)]
    {
        use tauri_plugin_dialog::DialogExt;

        let name = std::path::Path::new(&suggested_name)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .filter(|name| {
                !name.is_empty()
                    && name.len() <= 240
                    && !name.chars().any(char::is_control)
                    && *name != "."
                    && *name != ".."
            })
            .unwrap_or("sandbox-output.txt")
            .to_owned();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        window
            .dialog()
            .file()
            .set_parent(&window)
            .set_file_name(name)
            .set_title("选择审阅文件的保存位置")
            .save_file(move |selection| {
                let _ = sender.send(selection);
            });
        let Some(selection) = receiver.await.map_err(|_| {
            crate::commands::log_err("pick_sandbox_output_destination", "save dialog closed")
        })?
        else {
            return Ok(None);
        };
        let path = selection.into_path().map_err(|_| {
            crate::commands::log_err("pick_sandbox_output_destination", "invalid local path")
        })?;
        let path = path.to_str().ok_or_else(|| {
            crate::commands::log_err("pick_sandbox_output_destination", "invalid local path")
        })?;
        Ok(Some(path.to_owned()))
    }

    #[cfg(not(windows))]
    {
        let _ = (window, suggested_name);
        Ok(None)
    }
}

fn validate_sandbox_review_identity(session_id: &str, source_step_id: &str) -> Result<(), String> {
    crate::commands::validate_command_id(session_id, "session_id", "ses")
        .map_err(|error| crate::commands::log_err("sandbox_output_review", error))?;
    crate::commands::validate_command_id(source_step_id, "source_step_id", "step")
        .map_err(|error| crate::commands::log_err("sandbox_output_review", error))
}

async fn validate_sandbox_review_step(
    state: &AppState,
    session_id: &str,
    source_step_id: &str,
) -> Result<(), String> {
    let projection = state
        .runtime
        .session_store
        .session_resume_projection(session_id)
        .await
        .map_err(|error| crate::commands::log_err("sandbox_output_review", error))?;
    if projection
        .steps
        .iter()
        .any(|step| is_sandbox_review_step(step, source_step_id))
    {
        Ok(())
    } else {
        Err(crate::commands::log_err(
            "sandbox_output_review",
            "sandbox output review is unavailable",
        ))
    }
}

fn is_sandbox_review_step(step: &haven_memory::SessionStep, source_step_id: &str) -> bool {
    step.step_id == source_step_id
        && step
            .tool_name
            .as_deref()
            .is_some_and(|tool_name| tool_name == "shell" || tool_name.starts_with("skill__"))
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
    validate_sandbox_review_step(&state, &session_id, &source_step_id).await?;

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
                    crate::commands::confirmation::UiConfirmationAction::SandboxOutputPromotion,
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

#[cfg(test)]
mod tests {
    use super::{
        SandboxOutputReviewChangeDto, SandboxOutputReviewChangeKindDto,
        SandboxOutputReviewManifestDto, is_sandbox_review_step, validate_sandbox_review_identity,
    };
    use haven_common::lifecycle::SessionStepStatus;
    use haven_memory::SessionStep;

    fn shell_step(tool_name: &str) -> SessionStep {
        SessionStep {
            step_id: "step-00000000000000000000000000000001".into(),
            session_id: "ses-00000000000000000000000000000002".into(),
            step_number: 1,
            tool_index: 0,
            thought: None,
            tool_name: Some(tool_name.into()),
            tool_input: None,
            tool_call_id: None,
            observation: None,
            status: SessionStepStatus::Completed,
            is_high_risk: true,
            confirmed: Some(true),
            silent: false,
            started_at: None,
            completed_at: None,
            created_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn review_source_must_be_the_exact_shell_or_skill_step() {
        let step = shell_step("shell");
        assert!(is_sandbox_review_step(&step, &step.step_id));
        assert!(!is_sandbox_review_step(
            &step,
            "step-00000000000000000000000000000003"
        ));
        assert!(is_sandbox_review_step(
            &shell_step("skill__weather"),
            &step.step_id
        ));
        assert!(!is_sandbox_review_step(
            &shell_step("skill::weather"),
            &step.step_id
        ));
        assert!(!is_sandbox_review_step(
            &shell_step("files.write"),
            &step.step_id
        ));
    }

    #[test]
    fn review_ipc_rejects_noncanonical_session_or_step_ids() {
        assert!(
            validate_sandbox_review_identity(
                "ses-00000000000000000000000000000002",
                "step-00000000000000000000000000000001"
            )
            .is_ok()
        );
        assert!(
            validate_sandbox_review_identity(
                "ses-invalid",
                "step-00000000000000000000000000000001"
            )
            .is_err()
        );
        assert!(
            validate_sandbox_review_identity(
                "ses-00000000000000000000000000000002",
                "step-invalid"
            )
            .is_err()
        );
    }

    #[test]
    fn sandbox_review_dto_uses_camel_case_and_exposes_only_bounded_metadata() {
        let manifest = SandboxOutputReviewManifestDto {
            session_id: "ses-00000000000000000000000000000002".into(),
            source_step_id: "step-00000000000000000000000000000001".into(),
            expires_at_unix_ms: 1_800_000_000_000,
            storage_charge_bytes: 128,
            changes: vec![SandboxOutputReviewChangeDto {
                change_index: 0,
                relative_path: "reports/result.txt".into(),
                change: SandboxOutputReviewChangeKindDto::Created,
                before_bytes: None,
                after_bytes: Some(12),
                before_sha256: None,
                after_sha256: Some("a".repeat(64)),
                promoted: false,
            }],
        };
        let value = serde_json::to_value(manifest).unwrap();
        assert_eq!(
            value["sourceStepId"],
            "step-00000000000000000000000000000001"
        );
        assert_eq!(value["changes"][0]["changeIndex"], 0);
        assert_eq!(value["changes"][0]["relativePath"], "reports/result.txt");
        assert_eq!(value["changes"][0]["afterBytes"], 12);
        assert_eq!(value["changes"][0]["promoted"], false);
        assert!(value.get("absolute_path").is_none());
        assert!(value.get("contents").is_none());
    }
}
