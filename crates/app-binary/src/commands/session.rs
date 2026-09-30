use crate::app_state::{AppState, UiConfirmationAction, UiConfirmationPending};
use crate::commands::log_err;
use crate::commands::{SessionListResponse, emit_event_logged};
use crate::events::{
    InteractionRequestedEvent, SESSION_TITLE_UPDATED_EVENT, SessionTitleUpdatedEvent,
};
use crate::logging::sanitize_error_text;
use haven_agent::{InteractionRequest, InteractionStatus};
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::session_steps::SessionStep;
use haven_memory::repositories::sessions::Session;
use serde::Serialize;
use std::sync::Arc;
use tauri::AppHandle;
use tauri::State;

/// Reconcile host-managed media after a successful explicit history deletion.
/// A failed reference query must leave every file untouched.
async fn cleanup_unreferenced_session_media(state: &AppState, context: &str) {
    let referenced_paths = match state.session_store.list_managed_attachment_paths().await {
        Ok(paths) => paths,
        Err(error) => {
            tracing::warn!(
                context,
                error = %sanitize_error_text(&error.to_string()),
                "media cleanup skipped because session references could not be read"
            );
            return;
        }
    };
    let cleanup = crate::commands::recording::cleanup_unreferenced_managed_media(
        haven_common::default_work_dir().join("uploads"),
        haven_common::config::default_generated_media_dir(),
        state.tools.share_services().assets,
        referenced_paths,
    )
    .await;
    match cleanup {
        Ok((uploads, generated)) if uploads > 0 || generated > 0 => {
            tracing::info!(
                context,
                uploads,
                generated,
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
    let executor_session = state
        .executor
        .get_session(&session_id)
        .await
        .map(|session| ExecutorSessionDisplay {
            title: session.title,
            input: session.input,
        });
    let title =
        end_session_display_title(&session_id, executor_session, &state.session_store).await;

    let _ = state
        .executor
        .end_session(&session_id)
        .await
        .map_err(|e| log_err("end_session", e))?;
    // end_session always ends as Completed — the user explicitly finished the
    // session, so it is reported as completed (with notification), never error.
    state
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

    match session_store.session_display_title(session_id).await {
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
    let (perm_effect, perm_scope) = parse_permission_decision(&effect, &scope)
        .map_err(|error| log_err("resolve_confirmation", error))?;
    let perm_target = haven_common::types::PermissionTarget::parse(&target)
        .map_err(|error| log_err("resolve_confirmation", error))?;
    let confirmed = matches!(perm_effect, haven_common::types::PermissionEffect::Allow);
    let confirmation_id: haven_common::types::ConfirmId = step_id.clone().into();
    if let Some(capability) = state
        .executor
        .pending_confirmation_capability(&confirmation_id)
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
    // Resolve the confirmation and capture tool/session context atomically
    // (under the executor's sessions lock). Session scope uses the executor's
    // grant-aware path, which commits before resolving can wake the actor.
    let resolution = if matches!(perm_scope, haven_common::types::PermissionScope::Session) {
        state
            .executor
            .resolve_confirmation_with_session_grant(&confirmation_id, perm_target, perm_effect)
            .await
    } else {
        state
            .executor
            .resolve_confirmation(&confirmation_id, confirmed)
            .await
    }
    .map_err(|e| log_err("resolve_confirmation", e))?;

    let Some(resolution) = resolution else {
        let pending = state.ui_confirmations.lock().await.remove(&step_id);
        let Some(pending) = pending else {
            tracing::warn!(step_id, "confirmation request is stale or already resolved");
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

    // Once is only this invocation. Session scope was durably committed by the
    // grant-aware resolver before it woke the operation.
    if matches!(
        perm_scope,
        haven_common::types::PermissionScope::Once | haven_common::types::PermissionScope::Session
    ) {
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
        .services
        .authorization
        .grant(
            authorization_request.session_id.as_deref(),
            key,
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
        })
        .map_err(|error| log_err("resolve_ui_confirmation", error))?;
    tracing::debug!(
        interaction_id = %pending.request.id,
        interaction_kind = ?pending.request.kind,
        tool = %pending.authorization_request.tool_name,
        risk = ?pending.receipt.effective_risk,
        summary = %pending.summary,
        "resolving renderer-triggered confirmation"
    );
    if matches!(perm_effect, haven_common::types::PermissionEffect::Allow) {
        let authorization_request = &pending.authorization_request;
        state
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

        // Direct UI confirmations execute their typed action in this command
        // instead of waking the ReAct actor. Persist a requested session grant
        // before that action can produce an external side effect.
        if matches!(perm_scope, haven_common::types::PermissionScope::Session) {
            state
                .executor
                .grant_session_permission(
                    &pending.session_id,
                    grant_key.clone(),
                    perm_target,
                    perm_effect,
                )
                .await
                .map_err(|error| log_err("persist_session_permission", error))?;
        }

        match &pending.action {
            UiConfirmationAction::Mcp { client, tool, args } => {
                state
                    .services
                    .mcp
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
                let skill = state.services.skills.get_skill(name).await.ok_or_else(|| {
                    log_err(
                        "resolve_ui_confirmation skill",
                        format!("skill '{}' not found", name),
                    )
                })?;
                state
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
                crate::commands::finalize_confirmed_admin_ui_operation(
                    state, app, request, &result,
                )
                .await?;
            }
        }
    }

    if matches!(perm_scope, haven_common::types::PermissionScope::Once) {
        return Ok(());
    }
    let _config_apply_guard = if matches!(perm_scope, haven_common::types::PermissionScope::Always)
    {
        Some(persist_permanent_permission(state, grant_key.as_str(), perm_effect).await?)
    } else {
        None
    };
    if matches!(perm_scope, haven_common::types::PermissionScope::Session) {
        if matches!(perm_effect, haven_common::types::PermissionEffect::Deny) {
            state
                .executor
                .grant_session_permission(&pending.session_id, grant_key, perm_target, perm_effect)
                .await
                .map_err(|error| log_err("persist_session_permission", error))?;
        }
    } else {
        state
            .services
            .authorization
            .grant(
                Some(&pending.session_id),
                grant_key,
                perm_effect,
                perm_scope,
            )
            .await;
    }
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
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    use haven_common::config::StoredPermission;
    let guard = state.config_apply_gate.lock_owned().await;
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
        .session_store
        .update_session_title(&session_id, &title)
        .await
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
    session_id: String,
) -> Result<(), String> {
    state
        .agent
        .delete_session(&session_id)
        .await
        .map_err(|e| log_err("delete_session", e))?;
    cleanup_unreferenced_session_media(state.inner().as_ref(), "delete_session").await;
    Ok(())
}

#[tauri::command]
pub async fn clear_history(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let count = state
        .agent
        .clear_history()
        .await
        .map(|n| n as u64)
        .map_err(|e| log_err("clear_history", e))?;
    cleanup_unreferenced_session_media(state.inner().as_ref(), "clear_history").await;
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
async fn resume_response_for_session(
    session_store: haven_memory::SessionStore,
    session: Session,
) -> Result<SessionResumeResponse, String> {
    let projection = session_store
        .session_resume_projection(&session.id)
        .await
        .map_err(|e| log_err("resume_response_for_session", e))?;
    // Interactions are domain events owned by the session actor. The UI
    // history projection replays only that small control stream; messages and
    // steps remain projections and are not recovery input.
    let mut active_interactions: Vec<InteractionRequest> = Vec::new();
    for event in &projection.active_domain_events {
        match event.event_type.as_str() {
            haven_memory::INTERACTION_REQUESTED_EVENT_TYPE
            | haven_memory::INTERACTION_RESOLVED_EVENT_TYPE => {
                let request: InteractionRequest = serde_json::from_str(&event.payload)
                    .map_err(|e| log_err("resume_response_for_session", e))?;
                active_interactions.retain(|existing| existing.id != request.id);
                if request.status == InteractionStatus::Pending {
                    active_interactions.push(request);
                }
            }
            haven_memory::INTERACTION_CLEARED_EVENT_TYPE => {
                let ids = serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|payload| payload.get("ids")?.as_array().cloned())
                    .unwrap_or_default();
                active_interactions.retain(|request| {
                    !ids.iter()
                        .any(|id| id.as_str() == Some(request.id.as_str()))
                });
            }
            _ => {}
        }
    }
    let interactions = active_interactions
        .iter()
        .map(crate::bootstrap::project_interaction)
        .collect();
    Ok(SessionResumeResponse {
        session,
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

async fn last_conversation_from_store(
    session_store: haven_memory::SessionStore,
) -> Result<Option<SessionResumeResponse>, String> {
    match session_store
        .latest_session_record()
        .await
        .map_err(|e| log_err("get_last_conversation", e))?
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
    resume_session_from_store(state.session_store.clone(), &session_id).await
}

/// Return the most recent persisted session with its session messages and
/// steps, for the chat page to auto-restore the last conversation on app
/// start. Returns `None` when no session exists yet.
#[tauri::command]
pub async fn get_last_conversation(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<SessionResumeResponse>, String> {
    last_conversation_from_store(state.session_store.clone()).await
}

#[cfg(test)]
mod tests {
    use super::{
        ExecutorSessionDisplay, end_session_display_title, last_conversation_from_store,
        resume_session_from_store,
    };
    use crate::commands::SessionListResponse;

    #[test]
    fn test_session_list_response_serde() {
        let resp = SessionListResponse { sessions: vec![] };
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
    async fn last_conversation_returns_none_when_no_session_exists() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());

        assert!(
            last_conversation_from_store(haven_memory::SessionStore::new(db))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn last_conversation_returns_the_selected_session() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("latest conversation").unwrap();

        let response = last_conversation_from_store(haven_memory::SessionStore::new(db))
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
}
