//! Tauri command handlers, split by domain:
//! `recording` / `session` / `action` / `history` / `model` / `mcp` /
//! `skills` / `memory` / `settings` / `log` / `diagnostics`.
//!
//! Shared helpers (error conversion, router hot-swap, MCP connect, attachment
//! validation) live here so every submodule stays thin. `lib.rs` references
//! the submodule paths directly (`commands::recording::start_recording`, …)
//! because `generate_handler!` resolves each command's `__cmd__` symbol next
//! to its definition module.

pub mod action;
pub mod contracts;
pub mod diagnostics;
pub mod external;
pub mod history;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod recording;
pub mod session;
pub mod settings;
pub mod skills;

use crate::app_state::{AppState, UiConfirmationAction, UiConfirmationPending};
use crate::events::{
    INTERACTION_REQUESTED_EVENT, InteractionRequestedEvent, LLM_CONFIG_CHANGED_EVENT,
};
use crate::logging::sanitize_error_text;
use haven_common::McpServerConfig;
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use serde::Serialize;
use std::sync::Arc;
use tauri::AppHandle;
use tauri::Emitter;

/// Event name for "the LlmRouter was rebuilt / model config changed". The
/// frontend +layout listens to this to re-probe LLM connectivity immediately
/// instead of waiting for the next backoff-scheduled probe (which may be up
/// to 120s away during a failure-streak).
/// Emit [`LLM_CONFIG_CHANGED_EVENT`] so the frontend probes immediately after
/// a router hot-swap (settings save, model switch, …). Best-effort: a missing
/// renderer must never fail the command.
pub(crate) fn emit_llm_config_changed(app: &tauri::AppHandle) {
    emit_event_logged(app, LLM_CONFIG_CHANGED_EVENT, (), "llm_config_changed");
}

/// Emit a frontend event without turning a completed backend operation into a
/// false command failure. Tauri emit failures are still observable and are
/// sanitized because event payloads often contain provider-controlled text.
pub(crate) fn emit_event_logged<T: Serialize + Clone>(
    app: &tauri::AppHandle,
    event: &str,
    payload: T,
    context: &str,
) {
    if let Err(error) = app.emit(event, payload) {
        tracing::debug!(
            event,
            context,
            error = %sanitize_error_text(&error.to_string()),
            "failed to emit frontend event"
        );
    }
}

/// Recording helpers shared with the shell hotkey path in `lib.rs`.
pub(crate) use recording::{
    begin_recording_session, emit_recording_error, emit_recording_started, emit_recording_stopped,
    finalize_transcription, recording_reason_str,
};

#[derive(Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<haven_agent::SessionInfo>,
}

/// Re-export: command error logging lives in `crate::logging` (conventions §1).
pub(crate) use crate::logging::log_err;

/// Execute one native admin request through the typed operation that owns its
/// capability. This helper is intentionally separate from authorization so a
/// queued UI confirmation can resume the exact same request.
pub(crate) async fn execute_admin_surface(
    state: &AppState,
    ctx: &str,
    request: haven_tools::AdminRequest,
) -> Result<haven_tools::ToolResult, String> {
    let admin_surfaces = state
        .tools
        .admin_surfaces()
        .await
        .ok_or_else(|| log_err(ctx, "admin surfaces are not wired"))?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let result = admin_surfaces
        .execute(request, cancel)
        .await
        .map_err(|e| log_err(ctx, e))?;
    if !result.success {
        return Err(log_err(
            ctx,
            result
                .error
                .unwrap_or_else(|| "admin operation failed".into()),
        ));
    }
    Ok(result)
}

/// Authorize one native typed admin request and execute it, or place the typed
/// request in the UI confirmation queue. The operation metadata is read from
/// the same typed implementation used by the provider adapter.
pub(crate) async fn authorize_admin_request(
    state: &AppState,
    app: &AppHandle,
    ctx: &str,
    request: haven_tools::AdminRequest,
) -> Result<haven_tools::ToolResult, String> {
    let admin_surfaces = state
        .tools
        .admin_surfaces()
        .await
        .ok_or_else(|| log_err(ctx, "admin surfaces are not wired"))?;
    let operation_name = request.model_operation_name();
    let metadata = admin_surfaces.metadata(&request);
    let tool_name = operation_name.to_string();
    let risk_level = metadata.risk_level;
    let input = request.input();
    let network_access = if tool_name.starts_with("haven.mcp.") {
        haven_tools::NetworkAccess::Opaque
    } else {
        haven_tools::NetworkAccess::None
    };
    let policy = haven_tools::OperationPolicy::native(
        &tool_name,
        tool_name.clone().into(),
        risk_level,
        network_access,
    );
    let authorization_request =
        haven_tools::AuthorizationRequest::new(Some("ui"), &tool_name, input, policy);
    match state
        .tools
        .authorization()
        .authorize(&authorization_request)
        .await
    {
        haven_tools::AuthorizationDecision::AutoApproved => {
            execute_admin_surface(state, ctx, request).await
        }
        haven_tools::AuthorizationDecision::RequiresConfirmation { receipt, .. } => {
            Err(queue_ui_confirmation(
                state,
                app,
                authorization_request,
                receipt,
                UiConfirmationAction::Admin {
                    request: Box::new(request),
                },
            )
            .await?)
        }
        haven_tools::AuthorizationDecision::Blocked { reason, .. } => Err(format!(
            "native admin operation blocked by security policy ({reason})"
        )),
    }
}

/// Apply the app-shell side effects that normally follow a native admin
/// command after a queued confirmation resumes it. The structured admin
/// operation owns persistence and live state; this bridge owns catalog/event
/// refresh so a delayed confirmation updates the same UI surfaces as an
/// immediately approved command.
pub(crate) async fn finalize_admin_ui_operation(
    state: &AppState,
    app: &AppHandle,
    request: &haven_tools::AdminRequest,
) -> Result<(), String> {
    use haven_tools::{McpOperationArgs, SkillsOperationArgs, ToolsOperationArgs};

    match request {
        haven_tools::AdminRequest::Skills(
            SkillsOperationArgs::SkillEnable { .. } | SkillsOperationArgs::SkillDisable { .. },
        ) => {
            state.tools.rebuild_catalog().await;
            emit_event_logged(
                app,
                crate::events::SKILLS_STATUS_CHANGED_EVENT,
                crate::events::SkillsStatusChangedEvent {
                    op: "toggle".into(),
                },
                "resolve_ui_confirmation skill",
            );
        }
        haven_tools::AdminRequest::Tools(
            ToolsOperationArgs::ToolEnable { .. } | ToolsOperationArgs::ToolDisable { .. },
        ) => {
            state.tools.rebuild_catalog().await;
        }
        haven_tools::AdminRequest::Mcp(
            McpOperationArgs::McpAdd { .. }
            | McpOperationArgs::McpUpdate { .. }
            | McpOperationArgs::McpToggle { .. }
            | McpOperationArgs::McpConnect { .. },
        ) => {
            if let Some(name) = request.server_name() {
                crate::commands::mcp::spawn_monitor_if_client(state, name).await?;
                state.tools.rebuild_catalog().await;
                let connected = state.tools.mcp_manager().get_client(name).await.is_some();
                crate::commands::mcp::emit_mcp_status(
                    app,
                    name.to_string(),
                    if connected {
                        haven_tools::McpClientStatus::Connected
                    } else {
                        haven_tools::McpClientStatus::Disconnected
                    },
                    "resolve_ui_confirmation mcp",
                );
            }
        }
        haven_tools::AdminRequest::Mcp(
            McpOperationArgs::McpDisconnect { .. } | McpOperationArgs::McpRemove { .. },
        ) => {
            state.tools.rebuild_catalog().await;
            if let Some(name) = request.server_name() {
                crate::commands::mcp::emit_mcp_status(
                    app,
                    name.to_string(),
                    haven_tools::McpClientStatus::Disconnected,
                    "resolve_ui_confirmation mcp",
                );
            }
        }
        haven_tools::AdminRequest::Mcp(McpOperationArgs::McpReload) => {
            state.tools.rebuild_catalog().await;
        }
        _ => {}
    }
    Ok(())
}

/// Register a renderer-triggered MCP/skill invocation and expose only the
/// renderer-safe confirmation contract. The executor owns agent/scheduled
/// confirmations; this small app-level store gives direct UI invocations the
/// same resolve path without moving provider arguments across the boundary.
pub(crate) async fn queue_ui_confirmation(
    state: &AppState,
    app: &AppHandle,
    authorization_request: haven_tools::AuthorizationRequest,
    receipt: haven_tools::ConfirmationReceipt,
    action: UiConfirmationAction,
) -> Result<String, String> {
    let request_id = receipt.confirmation_id.clone();
    let tool_name = authorization_request.tool_name.clone();
    let summary = haven_tools::permission_prompt_summary(&tool_name, &authorization_request.input);
    let permission_key = authorization_request.policy.capability.to_string();
    let risk_level = receipt.effective_risk;
    state.ui_confirmations.lock().await.insert(
        request_id.to_string(),
        UiConfirmationPending {
            session_id: "ui".into(),
            authorization_request,
            summary: summary.clone(),
            receipt: receipt.clone(),
            action,
        },
    );
    if let Err(error) = app.emit(
        INTERACTION_REQUESTED_EVENT,
        InteractionRequestedEvent {
            id: request_id.clone().to_string(),
            kind: "confirm".into(),
            status: "pending".into(),
            prompt: summary.clone(),
            options: Vec::new(),
            tool_name: Some(tool_name),
            session_id: "ui".into(),
            risk_level: Some(risk_level),
            summary: Some(summary.clone()),
            permission_key: Some(permission_key.clone()),
            invocation_step_id: None,
            action_index: Some(0),
            tool_call_id: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
        },
    ) {
        state
            .ui_confirmations
            .lock()
            .await
            .remove(&request_id.to_string());
        return Err(log_err("queue_ui_confirmation", error));
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

/// Rebuild the LlmRouter from the current config and hot-swap it into the
/// runtime. Shared by `switch_model` and `set_reasoning_effort`, which both
/// follow the same "save config → rebuild router → swap live" sequence.
pub(crate) async fn rebuild_router(state: &AppState, ctx: &str) -> Result<(), String> {
    let config = state
        .config_service
        .snapshot()
        .map_err(|e| log_err(ctx, e))?
        .config;
    let new_router = Arc::new(LlmRouter::with_default_context_window(
        config.llm.materialize(
            Some(config.context_limits.max_response_tokens),
            Some(config.context_limits.reasoning_echo_max_chars),
        ),
        config.context_limits.default_context_window,
    ));
    hot_swap_router(state, new_router).await
}

/// Rebuild the router-dependent runtime after a model/config change: the
/// agent's LlmRouter, the tools' router, and the pipeline STT client (which
/// captures the router at construction — without a rebuild it keeps calling
/// a stale router after a model switch).
pub(crate) async fn hot_swap_router(
    state: &AppState,
    new_router: Arc<LlmRouter>,
) -> Result<(), String> {
    let config = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("hot_swap_router", e))?
        .config;
    let stt_config = config.media.stt.clone();
    let providers = config.llm.providers.clone();
    let mcp_caller: Arc<dyn haven_llm::McpToolCaller> = Arc::new(state.tools.mcp_manager().clone());
    let stt_client: Option<Arc<dyn haven_llm::SttClient>> =
        match build_stt_client(Some(mcp_caller), &stt_config, &providers) {
            Ok(client) => client.map(std::sync::Arc::from),
            Err(e) => {
                return Err(log_err("hot_swap_router STT", e));
            }
        };

    // Rebuild the canonical media runtime with the new router so fallback
    // extraction calls (low confidence / failed dedicated provider) and image
    // generation keep using one freshly-switched tool boundary.
    let media = config.media;
    let ocr: Option<Arc<dyn haven_llm::OcrClient>> = haven_llm::build_ocr_client(&media.ocr)
        .map_err(|e| log_err("hot_swap_router OCR", e))?
        .map(std::sync::Arc::from);
    let tts: Option<Arc<dyn haven_llm::TtsClient>> =
        haven_llm::build_tts_client(&media.tts, &providers)
            .map_err(|e| log_err("hot_swap_router TTS", e))?
            .map(std::sync::Arc::from);
    let image_gen: Option<Arc<dyn haven_llm::ImageGenClient>> =
        haven_llm::build_image_gen_client(&media.image_gen, &providers)
            .map_err(|e| log_err("hot_swap_router image generation", e))?
            .map(std::sync::Arc::from);

    // All dependent clients are valid before swapping any shared runtime
    // pointer. This keeps a failed rebuild from leaving a mixed-generation
    // router/pipeline/media-tool state.
    state.agent.replace_router(new_router.clone());
    state
        .tools
        .set_router_and_media_clients(
            new_router.clone(),
            stt_client.clone(),
            ocr.clone(),
            image_gen.clone(),
            tts.clone(),
            media.clone(),
        )
        .await;
    Ok(())
}

/// Build an `McpClient`, connect it (when `config.enabled`), and spawn the
/// health monitor using the discovery settings from the supplied loader.
/// Returns the constructed client either way so the caller can register it
/// with the manager. The caller is responsible for persisting the config
/// (before or after the call, depending on whether a failed connect should
/// roll the change back — `toggle_mcp_server` connects first so a failure
/// leaves the config unchanged). Used by `add_mcp_server`, `update_mcp_server`,
/// and `toggle_mcp_server`.
pub(crate) async fn connect_and_monitor(
    state: &AppState,
    discovery: &haven_common::config::McpDiscoveryConfig,
    config: &McpServerConfig,
    ctx: &str,
) -> Result<Arc<haven_tools::McpClient>, String> {
    if matches!(
        state.tools.mcp_manager().network_policy().await,
        haven_common::types::NetworkPolicy::Deny
    ) && config.enabled
    {
        return Err("MCP connection blocked by network policy".into());
    }
    let limits = state
        .config_service
        .snapshot()
        .map_err(|e| log_err(ctx, e))?
        .config
        .context_limits
        .clone();
    let client = Arc::new(haven_tools::McpClient::new(
        config,
        limits.mcp_max_binary_payload_bytes,
        limits.mcp_max_sse_buffer_bytes,
    ));
    client
        .set_network_policy(state.tools.mcp_manager().network_policy().await)
        .await;
    if config.enabled {
        client.connect().await.map_err(|e| log_err(ctx, e))?;
        let health_interval = std::time::Duration::from_secs(discovery.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(discovery.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
        let status_tx = state.tools.mcp_manager().status_tx();
        client.clone().spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            discovery.reconnect_max_retries,
            status_tx,
        );
    }
    Ok(client)
}
