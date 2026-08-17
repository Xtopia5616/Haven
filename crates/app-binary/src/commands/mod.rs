//! Tauri command handlers, split by domain:
//! `recording` / `session` / `action` / `history` / `model` / `mcp` /
//! `skills` / `memory` / `settings` / `log`.
//!
//! Shared helpers (error conversion, router hot-swap, MCP connect, attachment
//! validation) live here so every submodule stays thin. `lib.rs` references
//! the submodule paths directly (`commands::recording::start_recording`, …)
//! because `generate_handler!` resolves each command's `__cmd__` symbol next
//! to its definition module.

pub mod action;
pub mod history;
pub mod log;
pub mod mcp;
pub mod memory;
pub mod model;
pub mod recording;
pub mod session;
pub mod settings;
pub mod skills;

use crate::app_state::AppState;
use haven_common::McpServerConfig;
use haven_common::types::RiskLevel;
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use serde::Serialize;
use std::sync::Arc;
use tauri::Emitter;

/// Event name for "the LlmRouter was rebuilt / model config changed". The
/// frontend +layout listens to this to re-probe LLM connectivity immediately
/// instead of waiting for the next backoff-scheduled probe (which may be up
/// to 120s away during a failure-streak).
pub(crate) const LLM_CONFIG_CHANGED_EVENT: &str = "llm:config_changed";

/// Emit [`LLM_CONFIG_CHANGED_EVENT`] so the frontend probes immediately after
/// a router hot-swap (settings save, model switch, …). Best-effort: a missing
/// renderer must never fail the command.
pub(crate) fn emit_llm_config_changed(app: &tauri::AppHandle) {
    let _ = app.emit(LLM_CONFIG_CHANGED_EVENT, ());
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

/// Convert any displayable error into a frontend-facing string while logging
/// it at ERROR level. Replaces the repetitive `.map_err(log_err)`
/// pattern so command failures are never silently swallowed.
///
/// `ctx` identifies the originating Tauri command and is logged as a
/// separate line so the original `command error: <e>` line is preserved
/// verbatim for log scrapers / dashboards.
pub(crate) fn log_err<E: std::fmt::Display>(ctx: &str, e: E) -> String {
    tracing::error!("command `{}` failed", ctx);
    tracing::error!("command error: {}", e);
    e.to_string()
}

/// Build the JSON payload returned to the frontend when a tool call needs
/// user confirmation. Used by both `mcp_tool_call` and `execute_skill` so the
/// wire shape is identical across tool types.
pub(crate) fn confirmation_error(
    tool_name: String,
    params: serde_json::Value,
    risk_level: RiskLevel,
) -> Result<String, String> {
    serde_json::to_string(&serde_json::json!({
        "requires_confirmation": true,
        "tool_name": tool_name,
        "params": params,
        "risk_level": risk_level,
    }))
    .map_err(|e| e.to_string())
}

/// Rebuild the LlmRouter from the current config and hot-swap it into the
/// runtime. Shared by `switch_model` and `set_reasoning_effort`, which both
/// follow the same "save config → rebuild router → swap live" sequence.
pub(crate) async fn rebuild_router(state: &AppState, ctx: &str) -> Result<(), String> {
    let config = {
        let guard = state.config_loader.lock().map_err(|e| log_err(ctx, e))?;
        guard.config().clone()
    };
    let new_router = Arc::new(LlmRouter::new(config.llm.materialize(
        Some(config.context_limits.max_response_tokens),
        Some(config.context_limits.reasoning_echo_max_chars),
    )));
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
    state.agent.replace_router(new_router.clone());
    state.tools.set_router(new_router.clone()).await;

    let stt_config = {
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("hot_swap_router", e))?;
        cfg.config().media.stt.clone()
    };
    let mcp_caller: Arc<dyn haven_llm::McpToolCaller> = Arc::new(state.tools.mcp_manager.clone());
    let stt_client: Option<Arc<dyn haven_llm::SttClient>> =
        match build_stt_client(new_router.clone(), Some(mcp_caller), &stt_config) {
            Ok(client) => client.map(std::sync::Arc::from),
            Err(e) => {
                tracing::warn!("STT client rebuild failed, transcription disabled: {e}");
                None
            }
        };
    state.pipeline.set_stt_client(stt_client.clone()).await;

    // Rebuild the media gateway with the new router so fallback extraction
    // calls (low confidence / failed dedicated provider) keep routing to the
    // freshly-switched model endpoints.
    {
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("hot_swap_router", e))?
            .config()
            .media
            .clone();
        let ocr: Option<Arc<dyn haven_llm::OcrClient>> = match haven_llm::build_ocr_client(&cfg.ocr)
        {
            Ok(c) => c.map(std::sync::Arc::from),
            Err(e) => {
                tracing::warn!("OCR client rebuild failed, OCR disabled: {e}");
                None
            }
        };
        let tts: Option<Arc<dyn haven_llm::TtsClient>> = match haven_llm::build_tts_client(&cfg.tts)
        {
            Ok(c) => c.map(std::sync::Arc::from),
            Err(e) => {
                tracing::warn!("TTS client rebuild failed, TTS disabled: {e}");
                None
            }
        };
        let image_gen: Option<Arc<dyn haven_llm::ImageGenClient>> =
            match haven_llm::build_image_gen_client(&cfg.image_gen) {
                Ok(c) => c.map(std::sync::Arc::from),
                Err(e) => {
                    tracing::warn!(
                        "image generation client rebuild failed, image generation disabled: {e}"
                    );
                    None
                }
            };
        let gateway = Arc::new(haven_input::gateway::MediaGateway::new(
            new_router, stt_client, ocr, tts, image_gen, cfg,
        ));
        state.agent.set_gateway(Some(gateway)).await;
    }
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
    let limits = state
        .config_loader
        .lock()
        .map_err(|e| log_err(ctx, e))?
        .config()
        .context_limits
        .clone();
    let client = Arc::new(haven_tools::McpClient::new(
        config,
        limits.mcp_max_binary_payload_bytes,
        limits.mcp_max_sse_buffer_bytes,
    ));
    if config.enabled {
        client.connect().await.map_err(|e| log_err(ctx, e))?;
        let health_interval = std::time::Duration::from_secs(discovery.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(discovery.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
        let status_tx = state.tools.mcp_manager.status_tx();
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
