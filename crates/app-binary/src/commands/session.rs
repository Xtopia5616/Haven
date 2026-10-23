use crate::app_state::AppState;
use crate::commands::confirmation::{persist_permanent_permission, resolve_app_confirmation};
use crate::commands::{
    RuntimeSessionListResponse, emit_event_logged, log_err, validate_command_id,
};
use crate::events::{InteractionRequestedEvent, SESSION_LIFECYCLE_EVENT, SessionLifecycleEvent};
use haven_agent::InteractionStatus;
use haven_common::error::sanitize_error_text;
use haven_memory::SessionStep;
use haven_memory::{Session, SessionCreation, SessionCreationSource};
use serde::Serialize;
use std::sync::Arc;
use tauri::AppHandle;
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
        haven_common::config::default_uploads_root(),
        haven_common::config::default_generated_media_root(),
        Arc::clone(&state.runtime.services.managed_assets),
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
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("reopen_session", error))?;
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

#[derive(Debug, Clone, Serialize)]
pub struct SessionExecutionWorkspaceResponse {
    pub root: Option<String>,
}

#[tauri::command]
pub async fn get_session_execution_workspace(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<SessionExecutionWorkspaceResponse, String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("get_session_execution_workspace", error))?;
    let root = state
        .runtime
        .executor
        .session_execution_workspace(&session_id)
        .await
        .map_err(|error| log_err("get_session_execution_workspace", error))?;
    Ok(SessionExecutionWorkspaceResponse {
        root: root.map(|path| path.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub async fn set_session_execution_workspace(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    root: Option<String>,
) -> Result<SessionExecutionWorkspaceResponse, String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("set_session_execution_workspace", error))?;
    let root = match root {
        Some(root) => Some(
            state
                .runtime
                .tools
                .validate_execution_workspace(std::path::Path::new(&root))
                .map_err(|error| log_err("set_session_execution_workspace", error))?,
        ),
        None => {
            state
                .runtime
                .services
                .authorization
                .execution_workspace_snapshot(None)
                .await
                .root
        }
    };
    let root = state
        .runtime
        .executor
        .set_session_execution_workspace(&session_id, root)
        .await
        .map_err(|error| log_err("set_session_execution_workspace", error))?;
    Ok(SessionExecutionWorkspaceResponse {
        root: root.map(|path| path.to_string_lossy().into_owned()),
    })
}

#[tauri::command]
pub async fn list_runtime_sessions(
    state: State<'_, Arc<AppState>>,
) -> Result<RuntimeSessionListResponse, String> {
    let sessions = state
        .runtime
        .executor
        .list_runtime_sessions()
        .await
        .into_iter()
        .map(Into::into)
        .collect();
    Ok(RuntimeSessionListResponse { sessions })
}

#[derive(Serialize)]
pub struct SessionRecordDto {
    pub session_id: String,
    pub input_text: String,
    pub title: Option<String>,
    pub status: haven_common::lifecycle::SessionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_end_kind: Option<haven_common::lifecycle::SessionRunEndKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_end_reason: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub creation: SessionCreation,
}

impl From<Session> for SessionRecordDto {
    fn from(session: Session) -> Self {
        Self {
            session_id: session.session_id,
            input_text: session.input_text,
            title: session.title,
            status: session.status,
            run_end_kind: session.run_end_kind,
            run_end_reason: session.run_end_reason,
            created_at: session.created_at,
            updated_at: session.updated_at,
            creation: session.creation,
        }
    }
}

#[derive(Serialize)]
pub struct SessionLineageChildDto {
    pub session: SessionRecordDto,
    pub parent_session_id: String,
}

#[derive(Serialize)]
pub struct SessionLineageResponse {
    pub session_id: String,
    pub parent_session_id: Option<String>,
    pub parent: Option<SessionRecordDto>,
    pub children: Vec<SessionLineageChildDto>,
}

async fn session_lineage_from_store(
    session_store: &haven_memory::SessionStore,
    session_id: &str,
) -> anyhow::Result<SessionLineageResponse> {
    let session = session_store
        .load_session_record(session_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
    let parent_session_id = match &session.creation.source {
        SessionCreationSource::Session { session_id }
        | SessionCreationSource::Message { session_id, .. } => Some(session_id.clone()),
        SessionCreationSource::None | SessionCreationSource::ScheduleRun { .. } => None,
    };
    let parent = match parent_session_id.as_deref() {
        Some(parent_id) => session_store.load_session_record(parent_id).await?,
        None => None,
    };
    let children = session_store
        .load_session_children(session_id, 50, 0)
        .await?
        .into_iter()
        .map(|child| {
            let child_parent_session_id = match &child.creation.source {
                SessionCreationSource::Session { session_id }
                | SessionCreationSource::Message { session_id, .. } => session_id.clone(),
                SessionCreationSource::None | SessionCreationSource::ScheduleRun { .. } => {
                    anyhow::bail!("session lineage child has no parent session identity")
                }
            };
            anyhow::ensure!(
                child_parent_session_id == session_id,
                "session lineage child belongs to a different parent"
            );
            Ok(SessionLineageChildDto {
                session: SessionRecordDto::from(child),
                parent_session_id: child_parent_session_id,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(SessionLineageResponse {
        session_id: session.session_id,
        parent_session_id,
        parent: parent.map(SessionRecordDto::from),
        children,
    })
}

/// Load the parent and direct child sessions for the active session switcher.
#[tauri::command]
pub async fn get_session_lineage(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<SessionLineageResponse, String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("get_session_lineage", error))?;
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
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("end_session", error))?;
    // L3: capture the title BEFORE end_session removes the session from the
    // in-memory list; reading afterwards would fall back to the DB and lose
    // the generated title (end_session clears the working set).
    let executor_session = state
        .runtime
        .executor
        .get_runtime_session(&session_id)
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
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("interrupt_session", error))?;
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
    validate_command_id(&request_id, "request_id", "conf")
        .map_err(|error| log_err("resolve_confirmation", error))?;
    match &owner {
        haven_agent::InteractionOwner::Session { session_id } => {
            validate_command_id(session_id, "owner.session_id", "ses")
                .map_err(|error| log_err("resolve_confirmation", error))?;
        }
        haven_agent::InteractionOwner::ScheduleRun { schedule_run_id } => {
            validate_command_id(schedule_run_id, "owner.schedule_run_id", "schrun")
                .map_err(|error| log_err("resolve_confirmation", error))?;
        }
        haven_agent::InteractionOwner::AppCommand => {}
    }
    let perm_effect = effect;
    let perm_scope = scope;
    let perm_target = target;
    let confirmed = matches!(perm_effect, haven_common::types::PermissionEffect::Allow);
    let confirm_id = haven_common::types::ConfirmId::try_from(request_id.clone())
        .map_err(|error| log_err("resolve_confirmation", error))?;
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
        .pending_confirmation_capability_for_owner(&owner, &confirm_id)
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
                &confirm_id,
                perm_target,
                perm_effect,
            )
            .await
    } else {
        state
            .runtime
            .executor
            .resolve_confirmation_for_owner(&owner, &confirm_id, confirmed)
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

    let expired = resolution.status == InteractionStatus::Expired;
    tracing::info!(
        request_id = %request_id,
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
        .await
        .map_err(|error| log_err("resolve_confirmation", error))?;
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

/// Manually update a session's display title.
#[tauri::command]
pub async fn update_session_title(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    session_id: String,
    title: String,
) -> Result<(), String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("update_session_title", error))?;
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
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("delete_session", error))?;
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

/// Create an independent child session from a selected user message. The
/// source session remains unchanged; the child replays that user request.
#[tauri::command]
pub async fn fork_session_from_message(
    state: State<'_, Arc<AppState>>,
    session_id: String,
    message_id: String,
) -> Result<String, String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("fork_session_from_message", error))?;
    validate_command_id(&message_id, "message_id", "msg")
        .map_err(|error| log_err("fork_session_from_message", error))?;
    state
        .runtime
        .agent
        .fork_session_from_message(&session_id, &message_id)
        .await
        .map_err(|error| log_err("fork_session_from_message", error))
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
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("rollback_session", error))?;
    if let Some(message_id) = target_message_id.as_deref() {
        validate_command_id(message_id, "target_message_id", "msg")
            .map_err(|error| log_err("rollback_session", error))?;
    }
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
/// step by replaying the durable `session_events` transcript.
#[tauri::command]
pub async fn continue_session(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<(), String> {
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("continue_session", error))?;
    state
        .runtime
        .agent
        .continue_session(&session_id)
        .await
        .map_err(|e| log_err("continue_session", e))
}

#[derive(Serialize)]
pub struct SessionResumeMessageDto {
    pub message_id: String,
    pub session_id: String,
    pub role: haven_common::types::CanonicalRole,
    pub content: String,
    pub message_type: Option<haven_common::types::TranscriptMessageKind>,
    pub created_at: String,
    pub tool_call_id: Option<String>,
    pub attachments: Vec<haven_common::types::MessageAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_inputs: Vec<haven_common::media::MediaInput>,
    pub voice: bool,
}

impl From<haven_memory::Message> for SessionResumeMessageDto {
    fn from(message: haven_memory::Message) -> Self {
        Self {
            message_id: message.message_id,
            session_id: message.session_id,
            role: message.role,
            content: message.content,
            message_type: message.message_type,
            created_at: message.created_at,
            tool_call_id: message.tool_call_id,
            attachments: message.attachments,
            media_inputs: message.media_inputs,
            voice: message.voice,
        }
    }
}

#[derive(Serialize)]
pub struct SessionResumeResponse {
    pub session: SessionRecordDto,
    pub messages: Vec<SessionResumeMessageDto>,
    pub steps: Vec<SessionStep>,
    /// Persisted cumulative token/cost counters for the session, so a resumed
    /// or auto-restored session can restore the token-stats display.
    pub usage: Option<haven_memory::SessionUsage>,
    /// Per-LLM-call usage detail (one row per model response: step, role,
    /// model, tokens, cost, duration), oldest first.
    pub llm_usage: Vec<haven_memory::LlmUsageRecord>,
    /// Renderer-safe projections of the persisted interaction registry.
    pub interactions: Vec<InteractionRequestedEvent>,
    /// Latest active session working document and its durable event sequence.
    pub working_document: Option<String>,
    pub working_document_sequence: Option<u64>,
    /// Append-only event high-water mark from the same snapshot as the active document.
    pub event_sequence_watermark: u64,
}

/// Load the session's messages and steps into a resume response.
/// Shared by `get_session_for_resume` and `get_latest_session_for_resume`.
async fn resume_response_for_session(
    session_store: haven_memory::SessionStore,
    session: Session,
) -> Result<SessionResumeResponse, String> {
    let projection = session_store
        .session_resume_projection(&session.session_id)
        .await
        .map_err(|e| log_err("resume_response_for_session", e))?;
    // Agent owns interaction replay. Transcript ask results recover a pending
    // ask if shutdown happened after its question committed but before the
    // separate interaction_requested event did.
    let active_interactions =
        haven_agent::replay_session_interactions(&session.session_id, &projection.active_events)
            .map_err(|error| log_err("resume_response_for_session", error))?;
    let interactions = active_interactions
        .iter()
        .filter(|request| request.status == InteractionStatus::Pending)
        .map(|request| {
            crate::bootstrap::project_interaction(
                request,
                haven_agent::InteractionOwner::Session {
                    session_id: session.session_id.clone(),
                },
            )
        })
        .collect();
    let working_document =
        haven_agent::session_working_document_from_events(&projection.active_domain_events)
            .map_err(|error| log_err("resume_response_for_session", error))?;
    let working_document_sequence = projection
        .active_domain_events
        .iter()
        .filter(|event| event.event_type == haven_agent::SESSION_WORKING_DOCUMENT_EVENT_TYPE)
        .map(|event| event.sequence)
        .max()
        .and_then(|sequence| u64::try_from(sequence).ok());
    let event_sequence_watermark = u64::try_from(projection.event_sequence_watermark)
        .map_err(|error| log_err("resume_response_for_session", error))?;
    Ok(SessionResumeResponse {
        session: SessionRecordDto::from(session),
        messages: projection.messages.into_iter().map(Into::into).collect(),
        steps: projection.steps,
        usage: projection.usage,
        llm_usage: projection.llm_usage,
        interactions,
        working_document,
        working_document_sequence,
        event_sequence_watermark,
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
        .ok_or_else(|| {
            log_err(
                "get_session_for_resume",
                format!("Session not found: {session_id}"),
            )
        })?;
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
    validate_command_id(&session_id, "session_id", "ses")
        .map_err(|error| log_err("get_session_for_resume", error))?;
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
        ConfirmationResolutionResult, ExecutorSessionDisplay, InteractionStatus, SessionCreation,
        SessionRecordDto, SessionResumeMessageDto, end_session_display_title,
        latest_session_for_resume_from_store, resume_response_for_session,
        resume_session_from_store, session_lineage_from_store,
    };
    use crate::app_state::AppState;
    use crate::commands::confirmation::{
        AppConfirmationResolution, UiConfirmationAction, UiConfirmationPending,
        accept_ui_confirmation, arbitrate_app_confirmation,
    };
    use crate::commands::{RuntimeSessionDto, RuntimeSessionListResponse};
    use haven_agent::{InteractionOwner, InteractionRequest, SessionInfo, StepInfo};
    use haven_common::lifecycle::SessionStatus;
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
            .await
            .unwrap();
        let receipt = ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
            capability: authorization_request.policy.capability.clone(),
            canonical_input_hash: String::new(),
            effective_risk: RiskLevel::Medium,
            policy_revision: 0,
            expires_at,
        };
        let request =
            InteractionRequest::ui_confirm(tool_name.into(), tool_input.clone(), receipt.clone());
        let request_id = request.request_id.clone();
        state
            .app_confirmations
            .insert_for_test(
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
            )
            .await;
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

    #[test]
    fn runtime_session_list_serializes_only_the_app_owned_projection() {
        let response = RuntimeSessionListResponse {
            sessions: vec![RuntimeSessionDto::from(SessionInfo {
                session_id: "ses-00000000000000000000000000000001".into(),
                input: "visible input".into(),
                summary: "private agent summary".into(),
                title: Some("Visible title".into()),
                status: SessionStatus::Running,
                run_end_kind: None,
                waiting_reason: None,
                steps: vec![StepInfo {
                    step_id: "step-00000000000000000000000000000002".into(),
                    step_number: 1,
                    tool_name: "shell".into(),
                    input: serde_json::json!({"private": "step input"}),
                    output: Some(serde_json::json!({"private": "step output"})),
                    status: haven_common::lifecycle::SessionStepStatus::Completed,
                    risk_level: RiskLevel::Low,
                    confirmed: None,
                }],
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            })],
        };

        let value = serde_json::to_value(response).unwrap();
        let session = &value["sessions"][0];
        assert_eq!(
            session["session_id"],
            "ses-00000000000000000000000000000001"
        );
        assert_eq!(session["input_text"], "visible input");
        assert_eq!(session["title"], "Visible title");
        for internal_field in [
            "id",
            "input",
            "summary",
            "steps",
            "created_at",
            "updated_at",
        ] {
            assert!(
                session.get(internal_field).is_none(),
                "leaked {internal_field}"
            );
        }
        let serialized = value.to_string();
        for private_value in ["private agent summary", "step input", "step output"] {
            assert!(
                !serialized.contains(private_value),
                "leaked {private_value}"
            );
        }
    }

    #[test]
    fn session_record_wire_identity_uses_session_id() {
        let db = haven_memory::Database::open_in_memory().unwrap();
        let session = db.create_session("session record wire identity").unwrap();
        let value = serde_json::to_value(SessionRecordDto::from(session)).unwrap();

        assert!(value.get("session_id").is_some());
        assert!(value.get("id").is_none());
    }

    #[test]
    fn session_resume_message_wire_identity_uses_message_id() {
        let message = haven_memory::Message {
            message_id: "msg-00000000000000000000000000000001".into(),
            session_id: "ses-00000000000000000000000000000002".into(),
            role: haven_common::types::CanonicalRole::User,
            content: "hello".into(),
            message_type: None,
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            attachments: Vec::new(),
            media_inputs: Vec::new(),
            voice: false,
            ingress_seq: 42,
        };

        let value = serde_json::to_value(SessionResumeMessageDto::from(message)).unwrap();
        assert_eq!(value["message_id"], "msg-00000000000000000000000000000001");
        assert!(value.get("id").is_none());
        assert!(value.get("ingress_seq").is_none());
    }

    #[tokio::test]
    async fn resume_response_keeps_existing_ipc_field_names() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("resume wire shape").unwrap();
        let response =
            resume_session_from_store(haven_memory::SessionStore::new(db), &session.session_id)
                .await
                .unwrap();

        let value = serde_json::to_value(response).unwrap();
        let fields = value.as_object().unwrap();
        assert_eq!(fields.len(), 9);
        for field in [
            "session",
            "messages",
            "steps",
            "usage",
            "llm_usage",
            "interactions",
            "working_document",
            "working_document_sequence",
            "event_sequence_watermark",
        ] {
            assert!(fields.contains_key(field), "missing IPC field {field}");
        }
        assert!(fields["working_document"].is_null());
        assert!(fields["working_document_sequence"].is_null());
        assert_eq!(fields["event_sequence_watermark"], 0);
        assert!(!fields.contains_key("active_domain_events"));
    }

    #[tokio::test]
    async fn resume_response_rewinds_working_document_and_keeps_append_only_watermark() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("working document rollback").unwrap();
        let store = haven_memory::SessionStore::new(db);
        store
            .append_domain_event(
                &session.session_id,
                haven_agent::SESSION_WORKING_DOCUMENT_EVENT_TYPE,
                &serde_json::json!({ "markdown": "# Earlier plan" }).to_string(),
            )
            .await
            .unwrap();
        store
            .append_domain_event(
                &session.session_id,
                haven_agent::SESSION_WORKING_DOCUMENT_EVENT_TYPE,
                &serde_json::json!({ "markdown": "# Abandoned plan" }).to_string(),
            )
            .await
            .unwrap();
        store
            .append_domain_event(
                &session.session_id,
                haven_memory::TIMELINE_ROLLBACK_EVENT_TYPE,
                r#"{"to_sequence":1}"#,
            )
            .await
            .unwrap();

        let response = resume_session_from_store(store, &session.session_id)
            .await
            .unwrap();

        assert_eq!(response.working_document.as_deref(), Some("# Earlier plan"));
        assert_eq!(response.working_document_sequence, Some(1));
        assert_eq!(response.event_sequence_watermark, 3);
    }

    #[tokio::test]
    async fn session_lineage_returns_parent_and_direct_children() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let store = haven_memory::SessionStore::new(db.clone());
        let parent = store.create_session("parent").await.unwrap();
        let child = store
            .create_session_with_creation(
                "child",
                SessionCreation::agent_spawn(parent.session_id.clone()),
            )
            .await
            .unwrap();
        let sibling = store
            .create_session_with_creation(
                "sibling",
                SessionCreation::agent_spawn(parent.session_id.clone()),
            )
            .await
            .unwrap();

        let parent_lineage = session_lineage_from_store(&store, &parent.session_id)
            .await
            .unwrap();
        assert!(parent_lineage.parent.is_none());
        assert_eq!(
            parent_lineage
                .children
                .iter()
                .map(|child| child.session.session_id.as_str())
                .collect::<std::collections::HashSet<_>>(),
            [sibling.session_id.as_str(), child.session_id.as_str()]
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        );
        assert!(
            parent_lineage
                .children
                .iter()
                .all(|child| { child.parent_session_id == parent_lineage.session_id })
        );
        assert_eq!(parent_lineage.session_id, parent.session_id);
        assert_eq!(parent_lineage.parent_session_id, None);
        let parent_wire = serde_json::to_value(&parent_lineage).unwrap();
        assert!(parent_wire["children"][0].get("origin").is_none());

        let child_lineage = session_lineage_from_store(&store, &child.session_id)
            .await
            .unwrap();
        let child_wire = serde_json::to_value(&child_lineage).unwrap();
        assert!(child_wire["parent"].get("origin").is_none());
        assert_eq!(child_lineage.session_id, child.session_id);
        assert_eq!(
            child_lineage.parent_session_id,
            Some(parent.session_id.clone())
        );
        assert_eq!(child_lineage.parent.unwrap().session_id, parent.session_id);
        assert!(child_lineage.children.is_empty());

        db.delete_session(&parent.session_id).unwrap();
        let orphan_lineage = session_lineage_from_store(&store, &child.session_id)
            .await
            .unwrap();
        assert_eq!(orphan_lineage.parent_session_id, Some(parent.session_id));
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
                &session.session_id,
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
        assert_eq!(interaction["request_id"], step_id);
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

        assert_eq!(response.session.session_id, session.session_id);
    }

    #[tokio::test]
    async fn end_session_title_prefers_executor_title_or_input() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("persisted input").unwrap();
        db.update_session_title(&session.session_id, "persisted title")
            .unwrap();
        let session_store = haven_memory::SessionStore::new(db);

        let generated_title = end_session_display_title(
            &session.session_id,
            Some(ExecutorSessionDisplay {
                title: Some("executor title".into()),
                input: "executor input".into(),
            }),
            &session_store,
        )
        .await;
        assert_eq!(generated_title, "executor title");

        let executor_input = end_session_display_title(
            &session.session_id,
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
        db.update_session_title(&titled_session.session_id, "persisted title")
            .unwrap();
        let untitled_session = db.create_session("input fallback").unwrap();
        let session_store = haven_memory::SessionStore::new(db);

        assert_eq!(
            end_session_display_title(&titled_session.session_id, None, &session_store).await,
            "persisted title"
        );
        assert_eq!(
            end_session_display_title(&untitled_session.session_id, None, &session_store).await,
            "input fallback"
        );
    }

    #[tokio::test]
    async fn end_session_title_query_failure_falls_back_to_empty_title() {
        let db = std::sync::Arc::new(haven_memory::Database::open_in_memory().unwrap());
        let session = db.create_session("persisted input").unwrap();
        db.conn()
            .expect("test database connection checkout failed")
            .execute_batch("DROP TABLE sessions")
            .unwrap();
        let session_store = haven_memory::SessionStore::new(db);

        assert_eq!(
            end_session_display_title(&session.session_id, None, &session_store).await,
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
            .await
            .unwrap();
        let receipt = ConfirmationReceipt {
            confirmation_id: haven_common::types::ConfirmId::new(),
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
                .list_session_authorization_grants()
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
            session_id: "ses-c996baf34b6e23cdd181800ff4fc4298".into(),
        };
        assert!(matches!(
            deny_app_confirmation(&state, &session_owner, &request_id).await,
            Ok(AppConfirmationResolution::Stale)
        ));
        assert!(state.app_confirmations.contains(&request_id).await);

        let owner = app_command_owner();
        assert!(matches!(
            deny_app_confirmation(&state, &owner, "conf-97baf3a9879fa02d47c84b6f30c91057").await,
            Ok(AppConfirmationResolution::Stale)
        ));
        assert!(state.app_confirmations.contains(&request_id).await);

        let AppConfirmationResolution::Resolved(pending) =
            deny_app_confirmation(&state, &owner, &request_id)
                .await
                .unwrap()
        else {
            panic!("matching app owner and request id should resolve")
        };
        assert_eq!(pending.request.status, InteractionStatus::Resolved);
        assert!(!state.app_confirmations.contains(&request_id).await);
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
        assert!(!state.app_confirmations.contains(&request_id).await);
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
        assert_eq!(
            state.app_confirmations.status(&request_id).await,
            Some(InteractionStatus::Pending)
        );
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
