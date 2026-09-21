use crate::app_state::{AppState, UiConfirmationAction, UiConfirmationPending};
use crate::commands::log_err;
use crate::commands::{SessionListResponse, emit_event_logged};
use crate::events::{
    InteractionRequestedEvent, SESSION_DELETED_EVENT, SESSION_TITLE_UPDATED_EVENT,
    SessionDeletedEvent, SessionTitleUpdatedEvent,
};
use crate::logging::sanitize_error_text;
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::session_steps::SessionStep;
use haven_memory::repositories::sessions::Session;
use serde::Serialize;
use std::sync::Arc;
use tauri::AppHandle;
use tauri::State;

#[tauri::command]
pub async fn reopen_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    tracing::debug!("reopen_session called: session_id={}", session_id);
    state
        .agent
        .reopen_session(&session_id)
        .await
        .map_err(|e| log_err("reopen_session", e))?;
    tracing::debug!("reopen_session done");
    Ok(())
}

#[tauri::command]
pub async fn get_sessions(state: State<'_, Arc<AppState>>) -> Result<SessionListResponse, String> {
    let sessions = state.executor.list_sessions().await;
    Ok(SessionListResponse { sessions })
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
    let title = state
        .executor
        .get_session(&session_id)
        .await
        .map(|t| t.title.clone().unwrap_or(t.input))
        .or_else(|| match state.db.get_session(&session_id) {
            Ok(session) => session.map(|t| t.title.unwrap_or(t.input_text)),
            Err(error) => {
                tracing::warn!(
                    session_id,
                    error = %sanitize_error_text(&error.to_string()),
                    "failed to resolve session title before ending session"
                );
                None
            }
        })
        .unwrap_or_default();

    let _ = state
        .executor
        .end_session(&session_id)
        .await
        .map_err(|e| log_err("end_session", e))?;
    // end_session always ends as Completed — the user explicitly finished the
    // session, so it is reported as completed (with notification), never error.
    state
        .agent
        .emit_session_completed(&session_id, &title)
        .await;
    Ok(())
}

/// Interrupt the active model/tool run but keep the session resumable.
#[tauri::command]
pub async fn interrupt_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    state
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
pub async fn resolve_confirmation(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    step_id: String,
    effect: String,
    scope: String,
    target: String,
) -> Result<(), String> {
    let (perm_effect, perm_scope) = parse_permission_decision(&effect, &scope)?;
    let perm_target = haven_common::types::PermissionTarget::parse(&target)?;
    let confirmed = matches!(perm_effect, haven_common::types::PermissionEffect::Allow);
    let confirmation_id: haven_common::types::ConfirmId = step_id.clone().into();
    if let Some(capability) = state
        .executor
        .pending_confirmation_capability(&confirmation_id)
        .await
    {
        capability.target(perm_target).ok_or_else(|| {
            format!(
                "permission target '{}' is broader than capability '{}'",
                perm_target.as_str(),
                capability
            )
        })?;
    }
    // Resolve the confirmation and capture tool/session context atomically
    // (under the executor's sessions lock). This avoids the previous race where
    // the resolution and a separate `list_sessions()` lookup could observe a
    // step that a concurrent `end_session`/rollback had already removed.
    let resolution = state
        .executor
        .resolve_confirmation(&confirmation_id, confirmed)
        .await
        .map_err(|e| log_err("resolve_confirmation", e))?;

    let Some(resolution) = resolution else {
        let pending = state.ui_confirmations.lock().await.remove(&step_id);
        let Some(pending) = pending else {
            return Err("Confirmation request is stale or already resolved".into());
        };
        return resolve_ui_confirmation(
            &state,
            &app,
            pending,
            perm_effect,
            perm_scope,
            perm_target,
        )
        .await;
    };

    // Once-scope (or no grant) — nothing to record beyond the one-shot resolve.
    if matches!(perm_scope, haven_common::types::PermissionScope::Once) {
        return Ok(());
    }

    // The renderer submits a target category, but the backend resolves it only
    // against the confirmed capability's ancestry. A UI cannot invent a
    // sibling or unrelated broad permission.
    let authorization_request = state
        .tools
        .get_authorization_request(
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
        })?;
    // Persist Always before publishing it to the live authorization engine.
    // If the atomic config write fails, the process must not temporarily
    // behave as if a permanent grant exists when restart would forget it.
    if matches!(perm_scope, haven_common::types::PermissionScope::Always) {
        persist_permanent_permission(&state, key.as_str(), perm_effect).await?;
    }
    state
        .tools
        .authorization()
        .grant(
            authorization_request.session_id.as_deref(),
            &key,
            perm_effect,
            perm_scope,
        )
        .await;
    Ok(())
}

async fn resolve_ui_confirmation(
    state: &AppState,
    app: &AppHandle,
    pending: UiConfirmationPending,
    perm_effect: haven_common::types::PermissionEffect,
    perm_scope: haven_common::types::PermissionScope,
    perm_target: haven_common::types::PermissionTarget,
) -> Result<(), String> {
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
        })?;
    tracing::debug!(
        tool = %pending.authorization_request.tool_name,
        risk = ?pending.receipt.effective_risk,
        summary = %pending.summary,
        "resolving renderer-triggered confirmation"
    );
    if matches!(perm_effect, haven_common::types::PermissionEffect::Allow) {
        let authorization_request = &pending.authorization_request;
        state
            .tools
            .authorization()
            .verify_receipt(authorization_request, &pending.receipt)
            .await
            .map_err(|reason| format!("confirmation is no longer valid: {reason}"))?;

        match &pending.action {
            UiConfirmationAction::Mcp { client, tool, args } => {
                state
                    .tools
                    .mcp_manager()
                    .call_tool(
                        client,
                        tool,
                        args.clone(),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                    .map_err(|error| log_err("resolve_ui_confirmation mcp", error))?;
            }
            UiConfirmationAction::Skill { name, params } => {
                let skill = state
                    .tools
                    .skills_engine()
                    .get_skill(name)
                    .await
                    .ok_or_else(|| format!("skill '{}' not found", name))?;
                state
                    .tools
                    .skill_runner()
                    .read()
                    .await
                    .execute(&skill, params, tokio_util::sync::CancellationToken::new())
                    .await
                    .map_err(|error| log_err("resolve_ui_confirmation skill", error))?;
            }
            UiConfirmationAction::Admin { request } => {
                crate::commands::execute_admin_surface(
                    state,
                    "resolve_ui_confirmation admin",
                    request.as_ref().clone(),
                )
                .await?;
                crate::commands::finalize_admin_ui_operation(state, app, request).await?;
            }
        }
    }

    if matches!(perm_scope, haven_common::types::PermissionScope::Once) {
        return Ok(());
    }
    if matches!(perm_scope, haven_common::types::PermissionScope::Always) {
        persist_permanent_permission(state, grant_key.as_str(), perm_effect).await?;
    }
    state
        .tools
        .authorization()
        .grant(
            Some(&pending.session_id),
            &grant_key,
            perm_effect,
            perm_scope,
        )
        .await;
    Ok(())
}

fn parse_permission_decision(
    effect: &str,
    scope: &str,
) -> Result<
    (
        haven_common::types::PermissionEffect,
        haven_common::types::PermissionScope,
    ),
    String,
> {
    use haven_common::types::{PermissionEffect, PermissionScope};

    let perm_effect = match effect.trim().to_ascii_lowercase().as_str() {
        "allow" => PermissionEffect::Allow,
        "deny" => PermissionEffect::Deny,
        other => return Err(format!("invalid permission effect '{other}'")),
    };
    let perm_scope = match scope.trim().to_ascii_lowercase().as_str() {
        "once" => PermissionScope::Once,
        "session" => PermissionScope::Session,
        "always" => PermissionScope::Always,
        other => return Err(format!("invalid permission scope '{other}'")),
    };

    Ok((perm_effect, perm_scope))
}

async fn persist_permanent_permission(
    state: &AppState,
    key: &str,
    effect: haven_common::types::PermissionEffect,
) -> Result<(), String> {
    use haven_common::config::StoredPermission;
    state
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
    Ok(())
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
        return Err("Title cannot be empty".into());
    }
    state
        .db
        .update_session_title(&session_id, &title)
        .map_err(|e| log_err("update_session_title", e))?;
    state
        .executor
        .update_session_title(&session_id, &title)
        .await;
    emit_event_logged(
        &app,
        SESSION_TITLE_UPDATED_EVENT,
        SessionTitleUpdatedEvent { session_id, title },
        "session_title_updated",
    );
    Ok(())
}

#[tauri::command]
pub async fn delete_session(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
) -> Result<(), String> {
    // Quiesce the actor and delete its durable row under one supervisor-owned
    // lifecycle gate. This prevents a concurrent resume/load from reinstalling
    // a stale actor between the in-memory removal and SQL delete.
    state
        .executor
        .delete_session(&session_id)
        .await
        .map_err(|e| log_err("delete_session", e))?;
    // The session is gone, so no `session:updated` terminal transition will ever
    // fire for it; a dedicated `session:deleted` lets listeners (busy-session
    // tracking, per-session state) release the id immediately.
    emit_event_logged(
        &app,
        SESSION_DELETED_EVENT,
        SessionDeletedEvent {
            session_id: Some(session_id),
        },
        "session_deleted",
    );
    Ok(())
}

#[tauri::command]
pub async fn clear_history(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<u64, String> {
    // Stop all in-memory work and delete durable rows under one lifecycle
    // gate; no concurrent create/load can cross the purge boundary.
    let count = state
        .executor
        .clear_sessions_and_delete()
        .await
        .map(|n| n as u64)
        .map_err(|e| log_err("clear_history", e))?;
    // `session_id: null` signals "every session was removed" so listeners clear
    // per-session state (e.g. the busy set) in one shot instead of one event
    // per deleted session.
    emit_event_logged(
        &app,
        SESSION_DELETED_EVENT,
        SessionDeletedEvent { session_id: None },
        "history_cleared",
    );
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
        .agent
        .continue_session(&session_id)
        .await
        .map_err(|e| log_err("continue_session", e))
}

#[derive(Serialize)]
pub struct SessionResumeResponse {
    pub session: Session,
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
    /// Persisted cumulative token/cost counters for the session, so a resumed
    /// or auto-restored conversation can restore the token-stats display.
    pub usage: Option<haven_memory::repositories::usage::SessionUsage>,
    /// Per-LLM-call usage detail (one row per model response: step, role,
    /// model, tokens, cost, duration), oldest first.
    pub llm_usage: Vec<haven_memory::repositories::usage::LlmCallUsage>,
    /// Renderer-safe projections of the persisted interaction registry.
    pub interactions: Vec<InteractionRequestedEvent>,
}

/// Load the session's messages and steps into a resume response.
/// Shared by `get_session_for_resume` and `get_last_conversation`.
fn resume_response_for_session(
    db: &haven_memory::Database,
    session: Session,
) -> Result<SessionResumeResponse, String> {
    let messages = db
        .get_session_messages(&session.id)
        .map_err(|e| log_err("resume_response_for_session", e))?;
    let steps = db
        .get_session_steps(&session.id)
        .map_err(|e| log_err("resume_response_for_session", e))?;
    let usage = db
        .get_session_usage(&session.id)
        .map_err(|e| log_err("resume_response_for_session", e))?;
    let llm_usage = db
        .get_session_llm_usage(&session.id)
        .map_err(|e| log_err("resume_response_for_session", e))?;
    // `react_state` is a checkpoint cache. A damaged cache must not make the
    // history view unavailable; the Agent resume path will use the durable
    // event stream and restore whatever interaction state is still present.
    let interactions = match db.get_react_state(&session.id) {
        Ok(Some(json)) => match haven_agent::ReActSnapshot::from_json(&json) {
            Ok(snapshot) => snapshot
                .interactions
                .iter()
                .map(crate::bootstrap::project_interaction)
                .collect(),
            Err(error) => {
                tracing::warn!(
                    session_id = %session.id,
                    error = %error,
                    "ignoring corrupt react_state cache while loading session history"
                );
                Vec::new()
            }
        },
        Ok(None) => Vec::new(),
        Err(error) => {
            tracing::warn!(
                session_id = %session.id,
                error = %error,
                "ignoring unreadable react_state cache while loading session history"
            );
            Vec::new()
        }
    };
    Ok(SessionResumeResponse {
        session,
        messages,
        steps,
        usage,
        llm_usage,
        interactions,
    })
}

#[tauri::command]
pub async fn get_session_for_resume(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<SessionResumeResponse, String> {
    let session = state
        .db
        .get_session(&session_id)
        .map_err(|e| log_err("get_session_for_resume", e))?
        .ok_or_else(|| format!("Session not found: {}", session_id))?;
    resume_response_for_session(&state.db, session)
}

/// Return the most recent persisted session with its session messages and
/// steps, for the chat page to auto-restore the last conversation on app
/// start. Returns `None` when no session exists yet.
#[tauri::command]
pub async fn get_last_conversation(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<SessionResumeResponse>, String> {
    let sessions = state
        .db
        .list_sessions(1, 0)
        .map_err(|e| log_err("get_last_conversation", e))?;
    match sessions.into_iter().next() {
        Some(session) => resume_response_for_session(&state.db, session).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use crate::commands::SessionListResponse;

    #[test]
    fn test_session_list_response_serde() {
        let resp = SessionListResponse { sessions: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"sessions":[]}"#);
    }
}
