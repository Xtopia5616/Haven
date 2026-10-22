use crate::app_state::AppState;
use crate::commands::session::ConfirmationResolutionResult;
use crate::commands::{emit_event_logged, log_err};
use crate::events::{INTERACTION_REQUESTED_EVENT, NOTIFICATION_SHOW_EVENT};
use haven_agent::InteractionStatus;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

/// AppCommand confirmation requests belong to one private lifecycle owner.
/// The typed operation and authorization receipt stay backend-only until the
/// accepted request is continued.
pub(crate) struct AppConfirmationOwner {
    pending: Mutex<HashMap<String, UiConfirmationPending>>,
}

impl Default for AppConfirmationOwner {
    fn default() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }
}

impl AppConfirmationOwner {
    async fn insert(&self, request_id: String, pending: UiConfirmationPending) {
        self.pending.lock().await.insert(request_id, pending);
    }

    async fn discard(&self, request_id: &str) {
        self.pending.lock().await.remove(request_id);
    }

    async fn expire(&self, request_id: &str) -> Option<haven_agent::InteractionRequest> {
        let mut pending = self.pending.lock().await.remove(request_id)?;
        pending.request.expire().then_some(pending.request)
    }

    #[cfg(test)]
    pub(super) async fn insert_for_test(&self, request_id: String, pending: UiConfirmationPending) {
        self.insert(request_id, pending).await;
    }

    #[cfg(test)]
    pub(super) async fn contains(&self, request_id: &str) -> bool {
        self.pending.lock().await.contains_key(request_id)
    }

    #[cfg(test)]
    pub(super) async fn status(&self, request_id: &str) -> Option<InteractionStatus> {
        self.pending
            .lock()
            .await
            .get(request_id)
            .map(|pending| pending.request.status)
    }
}

/// A renderer-triggered Skill invocation waiting in the AppCommand owner.
/// Raw arguments never cross the Tauri projection.
pub(crate) enum UiConfirmationAction {
    Skill {
        name: String,
        params: serde_json::Value,
    },
    ExternalOpen {
        target: String,
    },
    OpenSkillsDirectory {
        root: PathBuf,
    },
    Admin {
        request: Box<haven_tools::AdminRequest>,
    },
}

pub(crate) struct UiConfirmationPending {
    /// Canonical interaction lifecycle shared with agent and scheduled confirms.
    pub request: haven_agent::InteractionRequest,
    pub authorization_request: haven_tools::AuthorizationRequest,
    pub receipt: haven_tools::ConfirmationReceipt,
    pub action: UiConfirmationAction,
}

/// Register a renderer-triggered MCP/skill invocation as the same canonical
/// confirmation interaction used by agent and scheduled confirmations. The
/// app-level store retains the typed execution payload needed after resolve;
/// only the renderer-safe projection crosses the Tauri boundary.
pub(crate) async fn queue_ui_confirmation(
    state: &AppState,
    app: &AppHandle,
    authorization_request: haven_tools::AuthorizationRequest,
    receipt: haven_tools::ConfirmationReceipt,
    action: UiConfirmationAction,
) -> Result<String, String> {
    let tool_name = authorization_request.tool_name.clone();
    let display_input =
        redact_mcp_admin_confirmation_input(&tool_name, authorization_request.input.clone());
    let summary = haven_tools::permission_prompt_summary(&tool_name, &display_input);
    let permission_key = authorization_request.policy.capability.to_string();
    let risk_level = receipt.effective_risk;
    let request =
        haven_agent::InteractionRequest::ui_confirm(tool_name, display_input, receipt.clone());
    let deadline = request
        .pending_permission_deadline()
        .map_err(|error| log_err("queue_ui_confirmation", error))?;
    if deadline <= chrono::Utc::now() {
        return Err(log_err(
            "queue_ui_confirmation",
            "permission confirmation deadline has already elapsed",
        ));
    }
    let request_id = request.request_id.clone();
    state
        .app_confirmations
        .insert(
            request_id.to_string(),
            UiConfirmationPending {
                request: request.clone(),
                authorization_request,
                receipt: receipt.clone(),
                action,
            },
        )
        .await;
    if let Err(error) = app.emit(
        INTERACTION_REQUESTED_EVENT,
        crate::bootstrap::project_interaction(&request, haven_agent::InteractionOwner::AppCommand),
    ) {
        state
            .app_confirmations
            .discard(&request_id.to_string())
            .await;
        return Err(log_err("queue_ui_confirmation", error));
    }
    tracing::info!(
        session_id = ?request.session_id,
        request_id = %request.request_id,
        interaction_kind = ?request.kind,
        status = ?request.status,
        "renderer permission request queued"
    );
    app.state::<std::sync::Arc<crate::notification::DesktopNotifications>>()
        .maybe_show_interaction_request(&request, &haven_agent::InteractionOwner::AppCommand);
    let expiry_delay = deadline
        .signed_duration_since(chrono::Utc::now())
        .to_std()
        .unwrap_or_default();
    let expiry_app = app.clone();
    let expiry_request_id = request_id.to_string();
    if !state.runtime.spawn("ui-confirmation-expiry", async move {
        tokio::time::sleep(expiry_delay).await;
        let state = expiry_app.state::<std::sync::Arc<AppState>>();
        let Some(expired_request) = state.app_confirmations.expire(&expiry_request_id).await else {
            return;
        };
        tracing::info!(
            session_id = ?expired_request.session_id,
            request_id = %expired_request.request_id,
            interaction_kind = ?expired_request.kind,
            status = ?expired_request.status,
            "renderer permission request expired"
        );
        expiry_app
            .state::<std::sync::Arc<crate::notification::DesktopNotifications>>()
            .maybe_show_interaction_request(
                &expired_request,
                &haven_agent::InteractionOwner::AppCommand,
            );
        emit_event_logged(
            &expiry_app,
            INTERACTION_REQUESTED_EVENT,
            crate::bootstrap::project_interaction(
                &expired_request,
                haven_agent::InteractionOwner::AppCommand,
            ),
            "ui_interaction_expired",
        );
    }) {
        tracing::warn!(
            request_id = %request.request_id,
            "could not schedule renderer permission expiry because application is shutting down"
        );
    }
    serde_json::to_string(&serde_json::json!({
        "requires_confirmation": true,
        "request_id": request_id,
        "summary": summary,
        "permission_key": permission_key,
        "risk_level": risk_level,
    }))
    .map_err(|error| log_err("queue_ui_confirmation", error))
}

/// MCP environment values stay in the typed pending action so the approved
/// operation can execute, but confirmation summaries and interaction payloads
/// expose only variable names. Keep this at the renderer boundary so the
/// authorization receipt still binds the original input internally.
pub(super) fn redact_mcp_admin_confirmation_input(
    tool_name: &str,
    mut input: serde_json::Value,
) -> serde_json::Value {
    if !matches!(tool_name, "haven.mcp.add" | "haven.mcp.update") {
        return input;
    }
    let Some(environment) = input
        .get_mut("env")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return input;
    };
    for entry in environment {
        let Some(raw) = entry.as_str() else {
            continue;
        };
        let Some((name, _)) = raw.split_once('=') else {
            continue;
        };
        *entry = serde_json::Value::String(format!("{name}=<redacted>"));
    }
    input
}

pub(super) async fn resolve_app_confirmation(
    state: &Arc<AppState>,
    app: &AppHandle,
    owner: &haven_agent::InteractionOwner,
    request_id: &str,
    perm_effect: haven_common::types::PermissionEffect,
    perm_scope: haven_common::types::PermissionScope,
    perm_target: haven_common::types::PermissionTarget,
) -> Result<ConfirmationResolutionResult, String> {
    if owner != &haven_agent::InteractionOwner::AppCommand {
        tracing::warn!(
            request_id,
            ?owner,
            "app confirmation resolve used a different owner"
        );
        return Ok(ConfirmationResolutionResult::Stale);
    }
    let resolution = arbitrate_app_confirmation(
        state.as_ref(),
        owner,
        request_id,
        perm_effect,
        perm_scope,
        perm_target,
    )
    .await?;
    let pending = match resolution {
        AppConfirmationResolution::Stale => return Ok(ConfirmationResolutionResult::Stale),
        AppConfirmationResolution::Expired(pending) => {
            app.state::<Arc<crate::notification::DesktopNotifications>>()
                .maybe_show_interaction_request(
                    &pending.request,
                    &haven_agent::InteractionOwner::AppCommand,
                );
            emit_event_logged(
                app,
                INTERACTION_REQUESTED_EVENT,
                crate::bootstrap::project_interaction(
                    &pending.request,
                    haven_agent::InteractionOwner::AppCommand,
                ),
                "ui_interaction_expired",
            );
            return Ok(ConfirmationResolutionResult::Expired);
        }
        AppConfirmationResolution::Resolved(pending) => pending,
    };

    let confirmed = matches!(perm_effect, haven_common::types::PermissionEffect::Allow);
    let session_id = pending.request.session_id.clone();
    let request_id = pending.request.request_id.clone();
    let decision = if confirmed { "approved" } else { "denied" };
    app.state::<Arc<crate::notification::DesktopNotifications>>()
        .maybe_show_interaction_request(
            &pending.request,
            &haven_agent::InteractionOwner::AppCommand,
        );
    emit_event_logged(
        app,
        INTERACTION_REQUESTED_EVENT,
        crate::bootstrap::project_interaction(
            &pending.request,
            haven_agent::InteractionOwner::AppCommand,
        ),
        "ui_interaction_decision_accepted",
    );
    tracing::info!(
        session_id = ?session_id,
        request_id = %request_id,
        decision,
        status = "accepted",
        "renderer permission decision accepted"
    );
    if confirmed {
        let task_app = (*app).clone();
        let task_state = state.clone();
        let spawn_state = task_state.clone();
        let request_id = request_id.clone();
        let task = async move {
            let result = execute_ui_confirmation_action(&task_state, &task_app, pending).await;
            let (title, body) = match result {
                Ok(()) => ("操作已完成", "已授权的操作已经完成。"),
                Err(_) => {
                    tracing::error!(
                        request_id,
                        failure_kind = "confirmed_ui_action",
                        "renderer permission continuation failed"
                    );
                    (
                        "操作未完成",
                        "授权已接受，但操作执行失败。你可以重新发起该操作。",
                    )
                }
            };
            emit_event_logged(
                &task_app,
                NOTIFICATION_SHOW_EVENT,
                crate::events::AgentNotificationEvent {
                    session_id: None,
                    title: title.into(),
                    body: body.into(),
                    notification_kind: None,
                    tool_run_kind: None,
                    tool_run_id: None,
                    tool_run_status: None,
                },
                "ui_confirmation_continuation_result",
            );
        };
        if !spawn_state
            .runtime
            .spawn("ui-confirmation-continuation", task)
        {
            emit_event_logged(
                app,
                NOTIFICATION_SHOW_EVENT,
                crate::events::AgentNotificationEvent {
                    session_id: None,
                    title: "操作未启动".into(),
                    body: "授权已接受，但应用正在关闭，操作没有启动。请重新发起该操作。".into(),
                    notification_kind: None,
                    tool_run_kind: None,
                    tool_run_id: None,
                    tool_run_status: None,
                },
                "ui_confirmation_continuation_rejected",
            );
        }
    }
    Ok(ConfirmationResolutionResult::Resolved)
}

pub(super) enum AppConfirmationResolution {
    Resolved(UiConfirmationPending),
    Expired(UiConfirmationPending),
    Stale,
}

pub(super) async fn arbitrate_app_confirmation(
    state: &AppState,
    owner: &haven_agent::InteractionOwner,
    request_id: &str,
    perm_effect: haven_common::types::PermissionEffect,
    perm_scope: haven_common::types::PermissionScope,
    perm_target: haven_common::types::PermissionTarget,
) -> Result<AppConfirmationResolution, String> {
    if owner != &haven_agent::InteractionOwner::AppCommand {
        return Ok(AppConfirmationResolution::Stale);
    }
    let mut pending_registry = state.app_confirmations.pending.lock().await;
    let Some(pending) = pending_registry.get_mut(request_id) else {
        return Ok(AppConfirmationResolution::Stale);
    };
    if pending.request.request_id != request_id
        || pending.receipt.confirmation_id.to_string() != request_id
        || pending.request.session_id.is_some()
        || pending.request.kind != haven_agent::InteractionKind::Confirm
        || pending.request.status != InteractionStatus::Pending
    {
        return Ok(AppConfirmationResolution::Stale);
    }
    if matches!(perm_scope, haven_common::types::PermissionScope::Session) {
        return Err(log_err(
            "resolve_confirmation",
            "session-scoped authorization requires a persisted session; choose once or permanent",
        ));
    }
    if pending
        .request
        .pending_permission_deadline()
        .map(|deadline| deadline <= chrono::Utc::now())
        .unwrap_or(true)
    {
        let mut pending = pending_registry
            .remove(request_id)
            .expect("checked pending app confirmation");
        let _ = pending.request.expire();
        return Ok(AppConfirmationResolution::Expired(pending));
    }
    accept_ui_confirmation(state, pending, perm_effect, perm_scope, perm_target).await?;
    let pending = pending_registry
        .remove(request_id)
        .expect("accepted app confirmation remains registered");
    Ok(AppConfirmationResolution::Resolved(pending))
}

pub(super) async fn accept_ui_confirmation(
    state: &AppState,
    pending: &mut UiConfirmationPending,
    perm_effect: haven_common::types::PermissionEffect,
    perm_scope: haven_common::types::PermissionScope,
    perm_target: haven_common::types::PermissionTarget,
) -> Result<(), String> {
    if matches!(perm_scope, haven_common::types::PermissionScope::Session) {
        return Err(log_err(
            "resolve_ui_confirmation",
            "session-scoped authorization requires a persisted session",
        ));
    }
    let grant_key = pending
        .authorization_request
        .policy
        .capability
        .target(perm_target)
        .ok_or_else(|| {
            format!(
                "permission target '{}' is broader than capability '{}'",
                perm_target.as_str(),
                pending.authorization_request.policy.capability
            )
        })
        .map_err(|error| log_err("resolve_ui_confirmation", error))?;
    tracing::debug!(
        request_id = %pending.request.request_id,
        session_id = ?pending.request.session_id,
        interaction_kind = ?pending.request.kind,
        risk = ?pending.receipt.effective_risk,
        "resolving renderer-triggered confirmation"
    );
    let allowed = matches!(perm_effect, haven_common::types::PermissionEffect::Allow);
    if allowed {
        let authorization_request = &pending.authorization_request;
        state
            .runtime
            .services
            .authorization
            .verify_receipt(authorization_request, &pending.receipt)
            .await
            .map_err(|reason| {
                log_err(
                    "resolve_ui_confirmation",
                    format!("confirmation is no longer valid: {reason}"),
                )
            })?;
    }

    match perm_scope {
        haven_common::types::PermissionScope::Once => {}
        haven_common::types::PermissionScope::Session => {
            return Err(log_err(
                "resolve_ui_confirmation",
                "session-scoped authorization requires a persisted session",
            ));
        }
        haven_common::types::PermissionScope::Always => {
            let config_apply_guard =
                persist_permanent_permission(state, grant_key.as_str(), perm_effect).await?;
            state
                .runtime
                .services
                .authorization
                .grant(None, grant_key, perm_effect, perm_scope)
                .await;
            drop(config_apply_guard);
        }
    }

    if !pending.request.resolve(serde_json::Value::Bool(allowed)) {
        return Err(log_err(
            "resolve_ui_confirmation",
            "UI confirmation request is no longer pending",
        ));
    }
    Ok(())
}

async fn execute_ui_confirmation_action(
    state: &AppState,
    app: &AppHandle,
    pending: UiConfirmationPending,
) -> Result<(), String> {
    match &pending.action {
        UiConfirmationAction::Skill { name, params } => {
            let proof = state
                .runtime
                .services
                .authorization
                .confirm_for_execution(&pending.authorization_request, &pending.receipt)
                .await
                .map_err(|error| log_err("resolve_ui_confirmation skill", error))?;
            state
                .runtime
                .services
                .skill_execution
                .execute_skill(
                    name,
                    params,
                    &pending.authorization_request,
                    &proof,
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
                .map_err(|error| log_err("resolve_ui_confirmation skill", error))?;
        }
        UiConfirmationAction::ExternalOpen { target } => {
            let _proof = state
                .runtime
                .services
                .authorization
                .confirm_for_execution(&pending.authorization_request, &pending.receipt)
                .await
                .map_err(|error| log_err("resolve_ui_confirmation external open", error))?;
            crate::commands::external::open_external_target(
                "resolve_ui_confirmation external open",
                target,
            )?;
        }
        UiConfirmationAction::OpenSkillsDirectory { root } => {
            let _proof = state
                .runtime
                .services
                .authorization
                .confirm_for_execution(&pending.authorization_request, &pending.receipt)
                .await
                .map_err(|error| log_err("resolve_ui_confirmation skills directory", error))?;
            crate::commands::skills::open_skills_directory(
                root,
                "resolve_ui_confirmation skills directory",
            )?;
        }
        UiConfirmationAction::Admin { request } => {
            let result = crate::commands::execute_admin_operation(
                state,
                "resolve_ui_confirmation admin",
                request.as_ref().clone(),
            )
            .await?;
            crate::commands::finalize_confirmed_admin_ui_operation(state, app, request, &result)
                .await?;
        }
    }
    Ok(())
}

pub(super) async fn persist_permanent_permission(
    state: &AppState,
    key: &str,
    effect: haven_common::types::PermissionEffect,
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    use haven_common::config::StoredPermission;
    let guard = state.runtime.config_runtime_coordinator.lock_owned().await;
    state
        .runtime
        .config_service
        .edit(|config| {
            let permissions = &mut config.security.permissions;
            if let Some(existing) = permissions.iter_mut().find(|p| p.key == key) {
                existing.effect = effect;
            } else {
                permissions.push(StoredPermission {
                    key: key.to_string(),
                    effect,
                });
            }
            Ok(())
        })
        .map_err(|e| log_err("persist_permanent_permission", e))?;
    Ok(guard)
}
