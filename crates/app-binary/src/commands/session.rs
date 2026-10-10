use crate::app_state::{AppState, UiConfirmationAction, UiConfirmationPending};
use crate::commands::log_err;
use crate::commands::{RuntimeSessionListResponse, emit_event_logged};
use crate::events::{
    INTERACTION_REQUESTED_EVENT, InteractionRequestedEvent, NOTIFICATION_SHOW_EVENT,
    SESSION_LIFECYCLE_EVENT, SessionLifecycleEvent,
};
use haven_agent::InteractionStatus;
use haven_common::error::sanitize_error_text;
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::session_steps::SessionStep;
use haven_memory::repositories::sessions::{Session, SessionOrigin};
use serde::Serialize;
use std::sync::Arc;
use tauri::AppHandle;
use tauri::Manager;
use tauri::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmationResolutionResult {
    Resolved,
    Expired,
    Stale,
}

/// Reconcile host-managed media after a successful explicit history deletion.
/// A failed reference query must leave every file untouched.
async fn cleanup_unreferenced_session_media(state: &AppState, context: &str) {
    let cleanup = crate::commands::managed_media::cleanup_unreferenced_managed_media(
        haven_common::default_work_dir().join("uploads"),
        haven_common::config::default_generated_media_dir(),
        state.runtime.tools.share_services().assets,
        &state.runtime.session_store,
    )
    .await;
    match cleanup {
        Ok(counts)
            if counts.removed_upload_batches > 0 || counts.removed_generated_media_files > 0 =>
        {
            tracing::info!(
                context,
                uploads = counts.removed_upload_batches,
                generated = counts.removed_generated_media_files,
                "removed unreferenced session media"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(
            context,
            error = %sanitize_error_text(&error),
            "unreferenced session media cleanup failed"
        ),
    }
}

#[tauri::command]
pub async fn reopen_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    tracing::debug!("reopen_session called: session_id={}", session_id);
    state
        .runtime
        .agent
        .reopen_session(&session_id)
        .await
        .map_err(|e| log_err("reopen_session", e))?;
    tracing::debug!("reopen_session done");
    Ok(())
}

#[tauri::command]
pub async fn list_runtime_sessions(
    state: State<'_, Arc<AppState>>,
) -> Result<RuntimeSessionListResponse, String> {
    let sessions = state.runtime.executor.list_runtime_sessions().await;
    Ok(RuntimeSessionListResponse { sessions })
}

#[derive(Serialize)]
pub struct SessionRecordDto {
    pub id: String,
    pub input_text: String,
    pub title: Option<String>,
    pub status: haven_common::SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_end_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl From<Session> for SessionRecordDto {
    fn from(session: Session) -> Self {
        Self {
            id: session.id,
            input_text: session.input_text,
            title: session.title,
            status: session.status,
            run_end_reason: session.run_end_reason,
            created_at: session.created_at,
            updated_at: session.updated_at,
        }
    }
}

#[derive(Serialize)]
pub struct SessionLineageResponse {
    pub parent: Option<SessionRecordDto>,
    pub children: Vec<SessionRecordDto>,
}

async fn session_lineage_from_store(
    session_store: &haven_memory::SessionStore,
    session_id: &str,
) -> anyhow::Result<SessionLineageResponse> {
    let session = session_store
        .load_session_record(session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
    let parent_id = match session.origin {
        SessionOrigin::AgentSpawn { parent_session_id } => Some(parent_session_id),
        SessionOrigin::User => None,
    };
    let parent = match parent_id {
        Some(parent_id) => session_store.load_session_record(&parent_id).await?,
        None => None,
    };
    let children = session_store
        .load_session_children(session_id, 50, 0)
        .await?;
    Ok(SessionLineageResponse {
        parent: parent.map(SessionRecordDto::from),
        children: children.into_iter().map(SessionRecordDto::from).collect(),
    })
}

/// Load the parent and direct child sessions for the active session switcher.
#[tauri::command]
pub async fn get_session_lineage(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<SessionLineageResponse, String> {
    session_lineage_from_store(&state.runtime.session_store, &session_id)
        .await
        .map_err(|error| log_err("get_session_lineage", error))
}

#[tauri::command]
pub async fn end_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    _app: tauri::AppHandle,
) -> Result<(), String> {
    // L3: capture the title BEFORE end_session removes the session from the
    // in-memory list; reading afterwards would fall back to the DB and lose
    // the generated title (end_session clears the working set).
    let executor_session = state
        .runtime
        .executor
        .get_session(&session_id)
        .await
        .map(|session| ExecutorSessionDisplay {
            title: session.title,
            input: session.input,
        });
    let title =
        end_session_display_title(&session_id, executor_session, &state.runtime.session_store)
            .await;

    let _ = state
        .runtime
        .executor
        .end_session(&session_id)
        .await
        .map_err(|e| log_err("end_session", e))?;
    // end_session always ends as Completed — the user explicitly finished the
    // session, so it is reported as completed (with notification), never error.
    state
        .runtime
        .agent
        .emit_session_completed(&session_id, &title, "用户主动结束会话")
        .await;
    Ok(())
}

#[derive(Debug, Clone)]
struct ExecutorSessionDisplay {
    title: Option<String>,
    input: String,
}

async fn end_session_display_title(
    session_id: &str,
    executor_session: Option<ExecutorSessionDisplay>,
    session_store: &haven_memory::SessionStore,
) -> String {
    if let Some(session) = executor_session {
        return session.title.unwrap_or(session.input);
    }

    match session_store.get_session_title_or_input(session_id).await {
        Ok(title) => title.unwrap_or_default(),
        Err(error) => {
            tracing::warn!(
                session_id,
                error = %sanitize_error_text(&error.to_string()),
                "failed to resolve session title before ending session"
            );
            String::new()
        }
    }
}

/// Interrupt the active model/tool run but keep the session resumable.
#[tauri::command]
pub async fn interrupt_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state
        .runtime
        .agent
        .interrupt_session(&session_id)
        .await
        .map_err(|e| log_err("interrupt_session", e))
}

/// Resolve a confirm dialog.
///
/// Resolve a confirmation using explicit effect, lifetime, and target
/// decisions. The command deliberately has no boolean or trust-session
/// compatibility bridge.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn resolve_confirmation(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    owner: haven_agent::InteractionOwner,
    request_id: String,
    effect: haven_common::types::PermissionEffect,
    scope: haven_common::types::PermissionScope,
    target: haven_common::types::PermissionTarget,
) -> Result<ConfirmationResolutionResult, String> {
    let perm_effect = effect;
    let perm_scope = scope;
    let perm_target = target;
    let confirmed = matches!(perm_effect, haven_common::types::PermissionEffect::Allow);
    let confirmation_id: haven_common::types::ConfirmId = request_id.clone().into();
    if owner == haven_agent::InteractionOwner::AppCommand {
        return resolve_app_confirmation(
            state.inner(),
            &app,
            &owner,
            &request_id,
            perm_effect,
            perm_scope,
            perm_target,
        )
        .await;
    }
    if let Some(capability) = state
        .runtime
        .executor
        .pending_confirmation_capability_for_owner(&owner, &confirmation_id)
        .await
    {
        capability
            .target(perm_target)
            .ok_or_else(|| {
                format!(
                    "permission target '{}' is broader than capability '{}'",
                    perm_target.as_str(),
                    capability
                )
            })
            .map_err(|error| log_err("resolve_confirmation", error))?;
    }
    // Resolve through the explicit owner while holding the executor's
    // confirmation-resolution gate. Session scope uses the grant-aware path,
    // which commits before resolving can wake the owning actor.
    let resolution = if matches!(perm_scope, haven_common::types::PermissionScope::Session) {
        state
            .runtime
            .executor
            .resolve_confirmation_with_session_grant_for_owner(
                &owner,
                &confirmation_id,
                perm_target,
                perm_effect,
            )
            .await
    } else {
        state
            .runtime
            .executor
            .resolve_confirmation_for_owner(&owner, &confirmation_id, confirmed)
            .await
    }
    .map_err(|e| log_err("resolve_confirmation", e))?;

    let Some(resolution) = resolution else {
        tracing::warn!(
            request_id,
            "confirmation request is stale or already resolved"
        );
        return Ok(ConfirmationResolutionResult::Stale);
    };

    let expired = resolution.status == haven_agent::interaction::InteractionStatus::Expired;
    tracing::info!(
        interaction_id = %confirmation_id,
        outcome = if expired { "expired" } else if confirmed { "approved" } else { "denied" },
        "permission request resolved"
    );

    if expired {
        return Ok(ConfirmationResolutionResult::Expired);
    }

    // Once is only this invocation. Session scope was durably committed by the
    // grant-aware resolver before it woke the operation.
    if matches!(
        perm_scope,
        haven_common::types::PermissionScope::Once | haven_common::types::PermissionScope::Session
    ) {
        return Ok(ConfirmationResolutionResult::Resolved);
    }

    // The renderer submits a target category, but the backend resolves it only
    // against the confirmed capability's ancestry. A UI cannot invent a
    // sibling or unrelated broad permission.
    let authorization_request = state
        .runtime
        .tools
        .resolve_authorization_request(
            resolution.session_id.as_deref(),
            &resolution.tool_name,
            &resolution.tool_input,
        )
        .await;
    let key = authorization_request
        .policy
        .capability
        .target(perm_target)
        .ok_or_else(|| {
            format!(
                "permission target '{}' is broader than capability '{}'",
                perm_target.as_str(),
                authorization_request.policy.capability
            )
        })
        .map_err(|error| log_err("resolve_confirmation", error))?;
    // Persist Always before publishing it to the live authorization engine.
    // If the atomic config write fails, the process must not temporarily
    // behave as if a permanent grant exists when restart would forget it.
    let _config_apply_guard = if matches!(perm_scope, haven_common::types::PermissionScope::Always)
    {
        Some(persist_permanent_permission(&state, key.as_str(), perm_effect).await?)
    } else {
        None
    };
    state
        .runtime
        .services
        .authorization
        .grant(
            authorization_request.session_id.as_deref(),
            key,
            perm_effect,
            perm_scope,
        )
        .await;
    Ok(ConfirmationResolutionResult::Resolved)
}

async fn resolve_app_confirmation(
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
    let interaction_id = pending.request.id.clone();
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
        interaction_id = %interaction_id,
        decision,
        status = "accepted",
        "renderer permission decision accepted"
    );
    if confirmed {
        let task_app = (*app).clone();
        let task_state = state.clone();
        let spawn_state = task_state.clone();
        let interaction_id = interaction_id.clone();
        let task = async move {
            let result = execute_ui_confirmation_action(&task_state, &task_app, pending).await;
            let (title, body) = match result {
                Ok(()) => ("操作已完成", "已授权的操作已经完成。"),
                Err(error) => {
                    tracing::error!(
                        interaction_id,
                        error = %sanitize_error_text(&error),
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

enum AppConfirmationResolution {
    Resolved(UiConfirmationPending),
    Expired(UiConfirmationPending),
    Stale,
}

async fn arbitrate_app_confirmation(
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
    let mut pending_registry = state.ui_confirmations.lock().await;
    let Some(pending) = pending_registry.get_mut(request_id) else {
        return Ok(AppConfirmationResolution::Stale);
    };
    if pending.request.id != request_id
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

async fn accept_ui_confirmation(
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
        interaction_id = %pending.request.id,
        session_id = ?pending.request.session_id,
        interaction_kind = ?pending.request.kind,
        tool = %pending.authorization_request.tool_name,
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
            let skill = state
                .runtime
                .services
                .skills
                .get_skill(name)
                .await
                .ok_or_else(|| {
                    log_err(
                        "resolve_ui_confirmation skill",
                        format!("skill '{}' not found", name),
                    )
                })?;
            state
                .runtime
                .services
                .skill_runner
                .read()
                .await
                .execute(&skill, params, tokio_util::sync::CancellationToken::new())
                .await
                .map_err(|error| log_err("resolve_ui_confirmation skill", error))?;
        }
        UiConfirmationAction::Admin { request } => {
            let result = crate::commands::execute_admin_surface(
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

async fn persist_permanent_permission(
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

/// Manually update a session's display title.
#[tauri::command]
pub async fn update_session_title(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err(log_err("update_session_title", "Title cannot be empty"));
    }
    state
        .runtime
        .session_store
        .update_session_title(&session_id, &title)
        .await
        .map_err(|e| log_err("update_session_title", e))?;
    state
        .runtime
        .executor
        .update_session_title(&session_id, &title)
        .await;
    emit_event_logged(
        &app,
        SESSION_LIFECYCLE_EVENT,
        SessionLifecycleEvent::TitleUpdated { session_id, title },
        "session_lifecycle_title_updated",
    );
    Ok(())
}

#[tauri::command]
pub async fn delete_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state
        .runtime
        .agent
        .delete_session(&session_id)
        .await
        .map_err(|e| log_err("delete_session", e))?;
    cleanup_unreferenced_session_media(state.inner().as_ref(), "delete_session").await;
    Ok(())
}

#[tauri::command]
pub async fn delete_all_sessions(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let count = state
        .runtime
        .agent
        .delete_all_sessions()
        .await
        .map(|n| n as u64)
        .map_err(|e| log_err("delete_all_sessions", e))?;
    cleanup_unreferenced_session_media(state.inner().as_ref(), "delete_all_sessions").await;
    Ok(count)
}

/// Roll back a session to a specific branch point. The session is rewound to
/// the saved state at that step. When `pause` is true the session is set to
/// Paused (user wants to edit the message before re-sending); otherwise it
/// is set to Pending for immediate re-execution. `target_message_id` is the
/// id of the exact message being rolled back; it lets the backend detect an
/// orphan rollback (a user message that was never processed into the
/// ReAct context). The id must resolve to a persisted session message when
/// `pause` is true — an unresolvable id is an error, not a content-based
/// guess.
#[tauri::command]
pub async fn rollback_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    target_step: u32,
    pause: Option<bool>,
    target_message_id: Option<String>,
) -> Result<(), String> {
    state
        .runtime
        .agent
        .rollback_session(
            &session_id,
            target_step,
            pause.unwrap_or(false),
            target_message_id.as_deref(),
        )
        .await
        .map_err(|e| log_err("rollback_session", e))
}

/// Resume a session that errored mid-step. Removes partial output persisted on
/// error and sets the session to Pending so the dispatcher retries the failed
/// step from the saved snapshot.
#[tauri::command]
pub async fn continue_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state
        .runtime
        .agent
        .continue_session(&session_id)
        .await
        .map_err(|e| log_err("continue_session", e))
}

#[derive(Serialize)]
pub struct SessionResumeResponse {
    pub session: SessionRecordDto,
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
    /// Persisted cumulative token/cost counters for the session, so a resumed
    /// or auto-restored session can restore the token-stats display.
    pub usage: Option<haven_memory::repositories::usage::SessionUsage>,
    /// Per-LLM-call usage detail (one row per model response: step, role,
    /// model, tokens, cost, duration), oldest first.
    pub llm_usage: Vec<haven_memory::repositories::usage::LlmUsageRecord>,
    /// Renderer-safe projections of the persisted interaction registry.
    pub interactions: Vec<InteractionRequestedEvent>,
}

/// Load the session's messages and steps into a resume response.
/// Shared by `get_session_for_resume` and `get_latest_session_for_resume`.
async fn resume_response_for_session(
    session_store: haven_memory::SessionStore,
    session: Session,
) -> Result<SessionResumeResponse, String> {
    let projection = session_store
        .session_resume_projection(&session.id)
        .await
        .map_err(|e| log_err("resume_response_for_session", e))?;
    // Agent owns interaction replay. Transcript ask results recover a pending
    // ask if shutdown happened after its question committed but before the
    // separate interaction_requested event did.
    let active_interactions =
        haven_agent::replay_session_interactions(&session.id, &projection.active_events)
            .map_err(|error| log_err("resume_response_for_session", error))?;
    let interactions = active_interactions
        .iter()
        .filter(|request| request.status == InteractionStatus::Pending)
        .map(|request| {
            crate::bootstrap::project_interaction(
                request,
                haven_agent::InteractionOwner::Session {
                    session_id: session.id.clone(),
                },
            )
        })
        .collect();
    Ok(SessionResumeResponse {
        session: SessionRecordDto::from(session),
        messages: projection.messages,
        steps: projection.steps,
        usage: projection.usage,
        llm_usage: projection.llm_usage,
        interactions,
    })
}

async fn resume_session_from_store(
    session_store: haven_memory::SessionStore,
    session_id: &str,
) -> Result<SessionResumeResponse, String> {
    let session = session_store
        .load_session_record(session_id)
        .await
        .map_err(|e| log_err("get_session_for_resume", e))?
        .ok_or_else(|| format!("Session not found: {}", session_id))?;
    resume_response_for_session(session_store, session).await
}

async fn latest_session_for_resume_from_store(
    session_store: haven_memory::SessionStore,
) -> Result<Option<SessionResumeResponse>, String> {
    match session_store
        .latest_session_record()
        .await
        .map_err(|e| log_err("get_latest_session_for_resume", e))?
    {
        Some(session) => resume_response_for_session(session_store, session)
            .await
            .map(Some),
        None => Ok(None),
    }
}

#[tauri::command]
pub async fn get_session_for_resume(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<SessionResumeResponse, String> {
    resume_session_from_store(state.runtime.session_store.clone(), &session_id).await
}

/// Return the most recent persisted session with its messages and steps for
/// the chat page to resume on app start. Returns `None` when no session exists.
#[tauri::command]
pub async fn get_latest_session_for_resume(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<SessionResumeResponse>, String> {
    latest_session_for_resume_from_store(state.runtime.session_store.clone()).await
}

#[cfg(test)]
mod tests {
    use super::{
        AppConfirmationResolution, ConfirmationResolutionResult, ExecutorSessionDisplay,
        InteractionStatus, SessionOrigin, accept_ui_confirmation, arbitrate_app_confirmation,
        end_session_display_title, latest_session_for_resume_from_store,
        resume_response_for_session, resume_session_from_store, session_lineage_from_store,
    };
    use crate::app_state::{AppState, UiConfirmationAction, UiConfirmationPending};
    use crate::commands::RuntimeSessionListResponse;
    use haven_agent::{InteractionOwner, InteractionRequest};
    use haven_common::types::{PermissionEffect, PermissionScope, PermissionTarget, RiskLevel};
    use haven_tools::ConfirmationReceipt;

    async fn test_state_and_ui_confirmation(
        expires_at: u64,
    ) -> (tempfile::TempDir, AppState, String) {
        let directory = tempfile::tempdir().unwrap();
        let loader =
            haven_common::config::ConfigLoader::load_from(&directory.path().join("config.toml"))
                .unwrap();
        let state = AppState::new_for_test(
            &directory.path().join("test.db"),
            vec![],
            loader,
            directory.path(),
        )
        .await
        .unwrap();
        let tool_name = "skill__test";
        let tool_input = serde_json::json!({});
        let authorization_request = state
            .runtime
            .tools
            .resolve_authorization_request(None, tool_name, &tool_input)
            .await;
        let receipt = ConfirmationReceipt {
            confirmation_id: haven_common::types::new_id("conf").into(),
            capability: authorization_request.policy.capability.clone(),
            canonical_input_hash: String::new(),
            effective_risk: RiskLevel::Medium,
            policy_revision: 0,
            expires_at,
        };
        let request =
            InteractionRequest::ui_confirm(tool_name.into(), tool_input.clone(), receipt.clone());
        let request_id = request.id.clone();
        state.ui_confirmations.lock().await.insert(
            request_id.clone(),
            UiConfirmationPending {
                request,
                authorization_request,
                receipt,
                action: UiConfirmationAction::Skill {
                    name: "test".into(),
                    params: tool_input,
                },
            },
        );
        (directory, state, request_id)
    }

    fn app_command_owner() -> InteractionOwner {
        InteractionOwner::AppCommand
    }

    async fn deny_app_confirmation(
        state: &AppState,
        owner: &InteractionOwner,
        request_id: &str,
    ) -> Result<AppConfirmationResolution, String> {
        arbitrate_app_confirmation(
            state,
            owner,
            request_id,
            PermissionEffect::Deny,
            PermissionScope::Once,
            PermissionTarget::Operation,
        )
        .await
    }

    #[test]
    fn test_session_list_response_serde() {
        let resp = RuntimeSessionListResponse { sessions: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"sessions":[]}"#);
    }

    #[tokio::test]
    async fn resume_response_keeps_existing_ipc_field_names() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("resume wire shape").unwrap();
        let response = resume_session_from_store(haven_memory::SessionStore::new(db), &session.id)
            .await
            .unwrap();

        let value = serde_json::to_value(response).unwrap();
        let fields = value.as_object().unwrap();
        assert_eq!(fields.len(), 6);
        for field in [
            "session",
            "messages",
            "steps",
            "usage",
            "llm_usage",
            "interactions",
        ] {
            assert!(fields.contains_key(field), "missing IPC field {field}");
        }
        assert!(!fields.contains_key("active_domain_events"));
    }

    #[tokio::test]
    async fn session_lineage_returns_parent_and_direct_children() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let store = haven_memory::SessionStore::new(db.clone());
        let parent = store.create_session("parent").await.unwrap();
        let child = store
            .create_session_with_origin(
                "child",
                SessionOrigin::AgentSpawn {
                    parent_session_id: parent.id.clone(),
                },
            )
            .await
            .unwrap();
        let sibling = store
            .create_session_with_origin(
                "sibling",
                SessionOrigin::AgentSpawn {
                    parent_session_id: parent.id.clone(),
                },
            )
            .await
            .unwrap();

        let parent_lineage = session_lineage_from_store(&store, &parent.id)
            .await
            .unwrap();
        assert!(parent_lineage.parent.is_none());
        assert_eq!(
            parent_lineage
                .children
                .iter()
                .map(|session| session.id.as_str())
                .collect::<std::collections::HashSet<_>>(),
            [sibling.id.as_str(), child.id.as_str()]
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        );
        let parent_wire = serde_json::to_value(&parent_lineage).unwrap();
        assert!(parent_wire["children"][0].get("origin").is_none());

        let child_lineage = session_lineage_from_store(&store, &child.id).await.unwrap();
        let child_wire = serde_json::to_value(&child_lineage).unwrap();
        assert!(child_wire["parent"].get("origin").is_none());
        assert_eq!(child_lineage.parent.unwrap().id, parent.id);
        assert!(child_lineage.children.is_empty());

        db.delete_session(&parent.id).unwrap();
        let orphan_lineage = session_lineage_from_store(&store, &child.id).await.unwrap();
        assert!(orphan_lineage.parent.is_none());
        assert!(orphan_lineage.children.is_empty());
    }

    #[tokio::test]
    async fn resume_recovers_pending_ask_from_committed_transcript() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("interrupted ask").unwrap();
        let session_store = haven_memory::SessionStore::new(db);
        let step_id = haven_common::types::new_id("step");
        let payload = serde_json::json!({
            "type": "tool_result",
            "step_number": 1,
            "tool_index": 0,
            "step_id": step_id,
            "canonical_observation": "Pick one?",
            "history_observation": "Pick one?",
            "tool_call_id": "ask-call",
            "action": {
                "tool_name": "ask",
                "tool_input": {"question": "Pick one?", "options": ["A", "B"]},
                "is_final": false,
                "tool_call_id": "ask-call"
            }
        });
        session_store
            .append(
                &session.id,
                haven_memory::TRANSCRIPT_EVENT_TYPE,
                &payload.to_string(),
                Some(1),
                Some(1),
            )
            .unwrap();

        let response = resume_response_for_session(session_store, session)
            .await
            .unwrap();
        assert!(
            serde_json::to_value(&response).unwrap()["session"]
                .get("origin")
                .is_none()
        );
        assert_eq!(response.interactions.len(), 1);
        let interaction = serde_json::to_value(&response.interactions[0]).unwrap();
        assert_eq!(interaction["id"], step_id);
        assert_eq!(interaction["kind"], "ask");
        assert_eq!(interaction["status"], "pending");
        assert_eq!(interaction["options"], serde_json::json!(["A", "B"]));
    }

    #[tokio::test]
    async fn resume_lookup_preserves_exact_not_found_error() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session_store = haven_memory::SessionStore::new(db);
        let session_id = haven_common::types::new_id("ses");

        let error = resume_session_from_store(session_store, &session_id)
            .await
            .err()
            .expect("missing session must return not-found");

        assert_eq!(error, format!("Session not found: {session_id}"));
    }

    #[tokio::test]
    async fn latest_session_for_resume_returns_none_when_no_session_exists() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());

        assert!(
            latest_session_for_resume_from_store(haven_memory::SessionStore::new(db))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn latest_session_for_resume_returns_the_selected_session() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("latest session").unwrap();

        let response = latest_session_for_resume_from_store(haven_memory::SessionStore::new(db))
            .await
            .unwrap()
            .expect("a persisted session must be returned");

        assert_eq!(response.session.id, session.id);
    }

    #[tokio::test]
    async fn end_session_title_prefers_executor_title_or_input() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("persisted input").unwrap();
        db.update_session_title(&session.id, "persisted title")
            .unwrap();
        let session_store = haven_memory::SessionStore::new(db);

        let generated_title = end_session_display_title(
            &session.id,
            Some(ExecutorSessionDisplay {
                title: Some("executor title".into()),
                input: "executor input".into(),
            }),
            &session_store,
        )
        .await;
        assert_eq!(generated_title, "executor title");

        let executor_input = end_session_display_title(
            &session.id,
            Some(ExecutorSessionDisplay {
                title: None,
                input: "executor input".into(),
            }),
            &session_store,
        )
        .await;
        assert_eq!(executor_input, "executor input");
    }

    #[tokio::test]
    async fn end_session_title_uses_persisted_display_title_when_executor_has_no_session() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let titled_session = db.create_session("persisted input").unwrap();
        db.update_session_title(&titled_session.id, "persisted title")
            .unwrap();
        let untitled_session = db.create_session("input fallback").unwrap();
        let session_store = haven_memory::SessionStore::new(db);

        assert_eq!(
            end_session_display_title(&titled_session.id, None, &session_store).await,
            "persisted title"
        );
        assert_eq!(
            end_session_display_title(&untitled_session.id, None, &session_store).await,
            "input fallback"
        );
    }

    #[tokio::test]
    async fn end_session_title_query_failure_falls_back_to_empty_title() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("persisted input").unwrap();
        db.conn().execute_batch("DROP TABLE sessions").unwrap();
        let session_store = haven_memory::SessionStore::new(db);

        assert_eq!(
            end_session_display_title(&session.id, None, &session_store).await,
            ""
        );
    }

    #[tokio::test]
    async fn renderer_confirmation_rejects_fake_session_scope_and_remains_resolvable() {
        let directory = tempfile::tempdir().unwrap();
        let loader =
            haven_common::config::ConfigLoader::load_from(&directory.path().join("config.toml"))
                .unwrap();
        let state = AppState::new_for_test(
            &directory.path().join("test.db"),
            vec![],
            loader,
            directory.path(),
        )
        .await
        .unwrap();
        let tool_name = "skill__test";
        let tool_input = serde_json::json!({});
        let authorization_request = state
            .runtime
            .tools
            .resolve_authorization_request(None, tool_name, &tool_input)
            .await;
        let receipt = ConfirmationReceipt {
            confirmation_id: haven_common::types::new_id("conf").into(),
            capability: authorization_request.policy.capability.clone(),
            canonical_input_hash: String::new(),
            effective_risk: RiskLevel::Medium,
            policy_revision: 0,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 60,
        };
        let request =
            InteractionRequest::ui_confirm(tool_name.into(), tool_input.clone(), receipt.clone());
        let mut pending = UiConfirmationPending {
            request,
            authorization_request,
            receipt,
            action: UiConfirmationAction::Skill {
                name: "test".into(),
                params: tool_input,
            },
        };

        let error = accept_ui_confirmation(
            &state,
            &mut pending,
            PermissionEffect::Allow,
            PermissionScope::Session,
            PermissionTarget::Operation,
        )
        .await
        .unwrap_err();
        assert!(error.contains("persisted session"));
        assert_eq!(pending.request.status, InteractionStatus::Pending);
        assert!(
            state
                .runtime
                .session_store
                .all_session_authorization_grants()
                .await
                .unwrap()
                .is_empty()
        );

        accept_ui_confirmation(
            &state,
            &mut pending,
            PermissionEffect::Deny,
            PermissionScope::Once,
            PermissionTarget::Operation,
        )
        .await
        .unwrap();
        assert_eq!(pending.request.status, InteractionStatus::Resolved);
        state.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn app_confirmation_routes_only_by_owner_and_request_id() {
        let (_directory, state, request_id) =
            test_state_and_ui_confirmation(chrono::Utc::now().timestamp().max(0) as u64 + 60).await;
        let session_owner = InteractionOwner::Session {
            session_id: "ses-wrong-owner".into(),
        };
        assert!(matches!(
            deny_app_confirmation(&state, &session_owner, &request_id).await,
            Ok(AppConfirmationResolution::Stale)
        ));
        assert!(
            state
                .ui_confirmations
                .lock()
                .await
                .contains_key(&request_id)
        );

        let owner = app_command_owner();
        assert!(matches!(
            deny_app_confirmation(&state, &owner, "conf-wrong-request").await,
            Ok(AppConfirmationResolution::Stale)
        ));
        assert!(
            state
                .ui_confirmations
                .lock()
                .await
                .contains_key(&request_id)
        );

        let AppConfirmationResolution::Resolved(pending) =
            deny_app_confirmation(&state, &owner, &request_id)
                .await
                .unwrap()
        else {
            panic!("matching app owner and request id should resolve")
        };
        assert_eq!(pending.request.status, InteractionStatus::Resolved);
        assert!(
            !state
                .ui_confirmations
                .lock()
                .await
                .contains_key(&request_id)
        );
        assert!(matches!(
            deny_app_confirmation(&state, &owner, &request_id).await,
            Ok(AppConfirmationResolution::Stale)
        ));
        state.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn app_confirmation_expiry_returns_a_distinct_terminal_result() {
        let (_directory, state, request_id) = test_state_and_ui_confirmation(
            chrono::Utc::now().timestamp().max(0).saturating_sub(1) as u64,
        )
        .await;
        let owner = app_command_owner();
        let AppConfirmationResolution::Expired(pending) =
            deny_app_confirmation(&state, &owner, &request_id)
                .await
                .unwrap()
        else {
            panic!("expired request should return the Expired result")
        };
        assert_eq!(pending.request.status, InteractionStatus::Expired);
        assert!(
            !state
                .ui_confirmations
                .lock()
                .await
                .contains_key(&request_id)
        );
        assert_eq!(
            serde_json::to_string(&ConfirmationResolutionResult::Expired).unwrap(),
            "\"expired\""
        );
        state.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn app_confirmation_retryable_failure_keeps_pending_entry() {
        let (_directory, state, request_id) =
            test_state_and_ui_confirmation(chrono::Utc::now().timestamp().max(0) as u64 + 60).await;
        let error = match arbitrate_app_confirmation(
            &state,
            &app_command_owner(),
            &request_id,
            PermissionEffect::Allow,
            PermissionScope::Session,
            PermissionTarget::Operation,
        )
        .await
        {
            Ok(_) => panic!("session scope must stay retryable for an app-only request"),
            Err(error) => error,
        };
        assert!(error.contains("persisted session"));
        let registry = state.ui_confirmations.lock().await;
        assert_eq!(
            registry.get(&request_id).unwrap().request.status,
            InteractionStatus::Pending
        );
        drop(registry);
        state.runtime.shutdown().await;
    }

    #[tokio::test]
    async fn concurrent_app_confirmation_clicks_accept_only_one_terminal_result() {
        let (_directory, state, request_id) =
            test_state_and_ui_confirmation(chrono::Utc::now().timestamp().max(0) as u64 + 60).await;
        let owner = app_command_owner();
        let (first, second) = tokio::join!(
            deny_app_confirmation(&state, &owner, &request_id),
            deny_app_confirmation(&state, &owner, &request_id),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(matches!(
            (&first, &second),
            (
                AppConfirmationResolution::Resolved(_),
                AppConfirmationResolution::Stale
            ) | (
                AppConfirmationResolution::Stale,
                AppConfirmationResolution::Resolved(_)
            )
        ));
        state.runtime.shutdown().await;
    }
}
