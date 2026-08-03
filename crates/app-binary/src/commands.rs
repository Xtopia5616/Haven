use crate::app_state::AppState;
use crate::events::*;
use haven_common::McpServerConfig;
use haven_common::config::{LlmConfig, ModelEndpoint};
use haven_common::types::RiskLevel;
use haven_input::{RecordingReason, RecordingResult};
use haven_llm::LlmRouter;
use haven_llm::{ModelInfo, ModelRegistry};
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::task_steps::TaskStep;
use haven_memory::repositories::tasks::Task;
use haven_tools::stt::build_stt_client;
use haven_tools::{ConfirmationResult, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::filter::EnvFilter;

#[derive(Serialize)]
pub struct TaskListResponse {
    pub tasks: Vec<haven_task::TaskInfo>,
}

#[derive(Serialize)]
pub struct ToolListResponse {
    pub tools: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct RecordingState {
    pub is_recording: bool,
    pub is_toggle: bool,
}

/// SkillInfo now sourced from `haven_tools::skills::SkillInfo` (M4-01).
pub use haven_tools::SkillInfo;

/// Convert any displayable error into a frontend-facing string while logging
/// it at ERROR level. Replaces the repetitive `.map_err(log_err)`
/// pattern so command failures are never silently swallowed.
///
/// `ctx` identifies the originating Tauri command and is logged as a
/// separate line so the original `command error: <e>` line is preserved
/// verbatim for log scrapers / dashboards.
fn log_err<E: std::fmt::Display>(ctx: &str, e: E) -> String {
    tracing::error!("command `{}` failed", ctx);
    tracing::error!("command error: {}", e);
    e.to_string()
}

/// Resolve a model role string to its endpoint slot, or `None` for unknown
/// roles. Single source of truth for the role names accepted by the model
/// commands (`switch_model`, `set_reasoning_effort`).
fn role_endpoint<'a>(cfg: &'a mut LlmConfig, role: &str) -> Option<&'a mut ModelEndpoint> {
    match role {
        "small_model" => Some(&mut cfg.small_model),
        "default_model" => Some(&mut cfg.default_model),
        "balanced_model" => Some(&mut cfg.balanced_model),
        "image_model" => Some(&mut cfg.image_model),
        "audio_model" => Some(&mut cfg.audio_model),
        _ => None,
    }
}

/// Normalize an endpoint URL for comparison: strip the trailing slash and
/// lowercase it (scheme/host comparisons are case-insensitive).
fn normalize_endpoint_url(url: &str) -> String {
    url.trim_end_matches('/').to_ascii_lowercase()
}

/// Rebuild the router-dependent runtime after a model/config change: the
/// agent's LlmRouter, the tools' router, and the pipeline STT client (which
/// captures the router at construction — without a rebuild it keeps calling
/// a stale router after a model switch).
async fn hot_swap_router(state: &AppState, new_router: Arc<LlmRouter>) -> Result<(), String> {
    state.agent.replace_router(new_router.clone());
    state.tools.set_router(new_router.clone()).await;

    let stt_config = {
        let cfg = state.config_loader.lock().map_err(|e| log_err("hot_swap_router", e))?;
        cfg.config().stt.clone()
    };
    match build_stt_client(new_router, state.tools.mcp_manager.clone(), &stt_config) {
        Ok(client) => {
            state.pipeline.set_stt_client(client).await;
        }
        Err(e) => {
            tracing::warn!("STT client rebuild failed, transcription disabled: {e}");
            state.pipeline.set_stt_client(None).await;
        }
    }
    Ok(())
}

/// Transcribe a captured recording, emit `transcription:result` /
/// `transcription:error`, and auto-submit non-empty transcripts to the agent
/// in the background.
///
/// Shared by the `stop_recording` Tauri command and the shell hotkey/VAD stop
/// path (`HavenShellHandler::on_recording_stop`), so both surfaces behave
/// identically — previously the shell path silently dropped the transcript.
///
/// The agent submission runs on a background task: the caller (the Tauri
/// command, whose UI promise stays pending) returns as soon as the recording
/// is finalized, and rapid re-recordings never run two concurrent ReAct loops
/// inline on the same command path.
pub(crate) async fn finalize_transcription(
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
    mut result: RecordingResult,
) -> Option<String> {
    state.pipeline.transcribe(&mut result).await;

    match result.transcript {
        Some(text) => {
            let session_id = uuid::Uuid::new_v4().to_string();
            let _ = app.emit(
                "transcription:result",
                TranscriptionResultEvent {
                    session_id: session_id.clone(),
                    text: text.clone(),
                    duration_ms: result.duration_ms,
                    confidence: None,
                },
            );
            // Auto-submit transcript to agent in the background.
            // Find the most-recently-created running or pending task so voice
            // follow-ups supplement the active task instead of creating a
            // brand-new one every time.
            let agent = state.agent.clone();
            let executor = state.executor.clone();
            let submit_text = text.clone();
            tokio::spawn(async move {
                let active = {
                    let tasks = executor.list_tasks().await;
                    tasks
                        .iter()
                        .find(|t| {
                            let s = t.status.as_str();
                            s == "running" || s == "pending"
                        })
                        .map(|t| t.id.clone())
                };
                if let Err(e) = agent.process_input(&submit_text, active).await {
                    tracing::error!("auto-submit transcription failed: {e}");
                }
            });
            Some(text)
        }
        None => {
            if let Some(err) = result.transcript_error {
                let _ = app.emit(
                    "transcription:error",
                    TranscriptionErrorEvent {
                        session_id: uuid::Uuid::new_v4().to_string(),
                        error: err,
                    },
                );
            } else {
                // STT succeeded but returned no text (silence / too-short
                // clip): there is nothing to submit, but the UI still needs
                // the "transcribing" overlay closed. The frontend treats an
                // empty `transcription:result` as "close, add no message".
                let _ = app.emit(
                    "transcription:result",
                    TranscriptionResultEvent {
                        session_id: uuid::Uuid::new_v4().to_string(),
                        text: String::new(),
                        duration_ms: result.duration_ms,
                        confidence: None,
                    },
                );
            }
            None
        }
    }
}

#[tauri::command]
pub async fn start_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    if let Err(e) = state.pipeline.start_recording().await {
        // The hotkey may have started a recording a moment earlier, or a VAD
        // auto-stop may be finalizing: the pipeline is busy, not broken.
        let pipeline_state = state.pipeline.get_state().await;
        if matches!(
            pipeline_state,
            haven_input::RecordingState::Recording
        ) {
            state.shell.sync_recording(true).await;
            let session_id = uuid::Uuid::new_v4().to_string();
            let _ = app.emit(
                "recording:started",
                RecordingEvent {
                    is_recording: true,
                    session_id: Some(session_id),
                    reason: None,
                    duration_ms: None,
                },
            );
            return Ok(());
        }
        let msg = if matches!(
            pipeline_state,
            haven_input::RecordingState::Processing
        ) {
            "正在处理上一条录音，请稍候再试".to_string()
        } else {
            format!("录音启动失败，请检查麦克风/STT 配置: {e}")
        };
        let session_id = uuid::Uuid::new_v4().to_string();
        let _ = app.emit(
            "recording:error",
            serde_json::json!({
                "session_id": session_id,
                "error": msg,
            }),
        );
        return Err(msg);
    }
    // Keep the shell state in sync so the tray icon, the mute hotkey and the
    // recording toggle reflect a UI-button-started recording.
    state.shell.sync_recording(true).await;
    let session_id = uuid::Uuid::new_v4().to_string();
    let _ = app.emit(
        "recording:started",
        RecordingEvent {
            is_recording: true,
            session_id: Some(session_id),
            reason: None,
            duration_ms: None,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn stop_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // Stop the audio capture first, *then* notify the UI that the
    // recording has ended. The previous ordering awaited STT (network
    // call) and the agent ReAct loop (multiple LLM/tool round-trips)
    // before emitting `recording:stopped`, so the UI kept the red
    // "recording" overlay up for the entire post-processing run. With
    // this split the overlay disappears within ~80 ms of the user
    // clicking stop, and STT + agent run as background work that
    // drives the rest of the UI through `transcription:*` / `task:*`
    // events.
    let result = match state.pipeline.stop_capture().await {
        Ok(result) => result,
        Err(e) => {
            // Another path (VAD auto-stop, mute, double click) already owns
            // the stop: the pipeline is Pending (finished) or Processing
            // (finalizing elsewhere). Not an error for the UI — emitting a
            // failure toast here would blame the user for a race they won.
            let pipeline_state = state.pipeline.get_state().await;
            if matches!(
                pipeline_state,
                haven_input::RecordingState::Pending | haven_input::RecordingState::Processing
            ) {
                state.shell.sync_recording(false).await;
                return Ok(String::new());
            }
            return Err(log_err("stop_recording", e));
        }
    };
    // Keep the shell state in sync (tray icon, mute hotkey, toggle).
    state.shell.sync_recording(false).await;
    let reason_str = match result.reason {
        RecordingReason::Manual => "manual",
        RecordingReason::Silence => "silence",
        RecordingReason::MaxDuration => "max_duration",
        RecordingReason::Cancel => "cancel",
    };
    let _ = app.emit(
        "recording:stopped",
        RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: Some(reason_str.to_string()),
            duration_ms: Some(result.duration_ms),
        },
    );

    // STT + agent auto-submit run in the background (see
    // `finalize_transcription`); the command returns as soon as the
    // recording has been handed off.
    let text = finalize_transcription(state.inner(), &app, result).await;
    Ok(text.unwrap_or_default())
}

#[tauri::command]
pub async fn cancel_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    state.pipeline.cancel_recording().await.map_err(|e| log_err("cancel_recording", e))?;
    state.shell.sync_recording(false).await;
    let _ = app.emit(
        "recording:stopped",
        RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: Some("cancel".to_string()),
            duration_ms: None,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn process_transcript(
    state: State<'_, Arc<AppState>>,
    transcript: String,
    active_task_id: Option<String>,
    images: Option<Vec<haven_memory::repositories::messages::MessageAttachment>>,
) -> Result<Value, String> {
    let images = validate_images(images.unwrap_or_default())?;
    tracing::debug!(
        "process_transcript called: text={:?} active_task_id={:?} images={}",
        transcript,
        active_task_id,
        images.len()
    );
    let result = state
        .agent
        .process_input_with_images(&transcript, active_task_id.clone(), &images)
        .await
        .map_err(|e| log_err("process_transcript", e))?;
    tracing::debug!("process_transcript result: {:?}", result);
    Ok(serde_json::to_value(result).unwrap_or_default())
}

/// Server-side validation for image attachments, mirroring the frontend
/// limits (≤4 images, ≤10 MiB each, image/* MIME, decodable base64). The
/// webview must not be the sole enforcement point for persisted payloads.
fn validate_images(
    images: Vec<haven_memory::repositories::messages::MessageAttachment>,
) -> Result<Vec<haven_memory::repositories::messages::MessageAttachment>, String> {
    const MAX_IMAGES: usize = 4;
    const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
    use base64::Engine as _;
    if images.len() > MAX_IMAGES {
        return Err(format!("最多支持 {MAX_IMAGES} 张图片"));
    }
    for img in &images {
        if !img.media_type.starts_with("image/") {
            return Err(format!("不支持的图片类型: {}", img.media_type));
        }
        let decoded_len = img.data.len().saturating_mul(3) / 4;
        if decoded_len > MAX_IMAGE_BYTES {
            return Err("图片超过 10MB 上限".to_string());
        }
        if base64::engine::general_purpose::STANDARD
            .decode(&img.data)
            .is_err()
        {
            return Err("图片数据不是有效的 base64".to_string());
        }
    }
    Ok(images)
}

#[tauri::command]
pub async fn reopen_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    tracing::debug!("reopen_task called: task_id={}", task_id);
    state.agent.reopen_task(&task_id).await.map_err(|e| log_err("reopen_task", e))?;
    tracing::debug!("reopen_task done");
    Ok(())
}

#[tauri::command]
pub async fn get_tasks(state: State<'_, Arc<AppState>>) -> Result<TaskListResponse, String> {
    let tasks = state.executor.list_tasks().await;
    Ok(TaskListResponse { tasks })
}

#[tauri::command]
pub async fn end_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    _app: tauri::AppHandle,
) -> Result<(), String> {
    // L3: capture the title BEFORE end_task removes the task from the
    // in-memory list; reading afterwards would fall back to the DB and lose
    // the generated title (end_task clears the working set).
    let title = state
        .executor
        .list_tasks()
        .await
        .into_iter()
        .find(|t| t.id == task_id)
        .map(|t| t.title.clone().unwrap_or(t.input))
        .or_else(|| {
            state
                .db
                .get_task(&task_id)
                .ok()
                .flatten()
                .map(|t| t.title.unwrap_or(t.input_text))
        })
        .unwrap_or_default();

    let _ = state.executor.end_task(&task_id).await.map_err(|e| log_err("end_task", e))?;
    // end_task always ends as Completed — the user explicitly finished the
    // task, so it is reported as completed (with notification), never error.
    state.agent.emit_task_completed(&task_id, &title).await;
    Ok(())
}

#[tauri::command]
pub async fn resolve_confirmation(
    state: State<'_, Arc<AppState>>,
    step_id: String,
    confirmed: bool,
    trust_session: Option<bool>,
) -> Result<(), String> {
    // Resolve the confirmation and capture the step's risk level atomically
    // (under the executor's tasks lock). This avoids the previous race where
    // `confirm_step` and a separate `list_tasks()` lookup could observe a
    // step that a concurrent `end_task`/rollback had already removed.
    let risk_level = state
        .executor
        .resolve_confirmation(&step_id, confirmed)
        .await
        .map_err(|e| log_err("resolve_confirmation", e))?;
    if trust_session.unwrap_or(false)
        && confirmed
        && let Some(level) = risk_level
    {
        state.tools.safety_gateway.trust_risk_level(level).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_tools(state: State<'_, Arc<AppState>>) -> Result<ToolListResponse, String> {
    let tools = state.tools.registry.list_schemas().await;
    Ok(ToolListResponse { tools })
}

#[tauri::command]
pub async fn get_recording_state(
    state: State<'_, Arc<AppState>>,
) -> Result<RecordingState, String> {
    let shell_state = state.shell.get_state().await;
    Ok(RecordingState {
        is_recording: shell_state.is_recording,
        is_toggle: shell_state.is_recording_toggle,
    })
}

#[tauri::command]
pub async fn get_history(
    state: State<'_, Arc<AppState>>,
    limit: i64,
    offset: i64,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    state.db.list_tasks(limit, offset).map_err(|e| log_err("get_history", e))
}

#[tauri::command]
pub async fn count_history(state: State<'_, Arc<AppState>>) -> Result<i64, String> {
    state.db.count_tasks().map_err(|e| log_err("count_history", e))
}

#[tauri::command]
pub async fn search_history_paginated(
    state: State<'_, Arc<AppState>>,
    query: String,
    limit: i64,
    offset: i64,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    state
        .db
        .search_tasks_paginated(&query, limit, offset)
        .map_err(|e| log_err("search_history_paginated", e))
}

#[tauri::command]
pub async fn count_history_search(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<i64, String> {
    state.db.count_tasks_search(&query).map_err(|e| log_err("count_history_search", e))
}

#[tauri::command]
pub async fn list_mcp_tools(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<McpServerSnapshot>, String> {
    Ok(state.tools.mcp_manager.snapshot().await)
}

#[tauri::command]
pub async fn reconnect_mcp(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    state
        .tools
        .mcp_manager
        .reconnect(&name)
        .await
        .map_err(|e| log_err("reconnect_mcp", e))?;
    // Restart health monitor for this client
    if let Some(client) = state.tools.mcp_manager.get_client(&name).await {
        let config = haven_common::config::ConfigLoader::load()
            .map_err(|e| log_err("reconnect_mcp", e))?
            .config()
            .mcp_discovery
            .clone();
        let health_interval = std::time::Duration::from_secs(config.health_interval_secs);
        let initial_backoff = std::time::Duration::from_millis(config.reconnect_initial_ms);
        let max_backoff = std::time::Duration::from_millis(config.reconnect_max_ms);
        let max_retries = config.reconnect_max_retries;
        let status_tx = state.tools.mcp_manager.status_tx();
        client.spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            max_retries,
            status_tx,
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn mcp_tool_call(
    state: State<'_, Arc<AppState>>,
    client: String,
    tool: String,
    args: Value,
) -> Result<Value, String> {
    // Check SafetyGateway (MCP tools default to Medium risk)
    match state
        .tools
        .safety_gateway
        .check(
            &format!("mcp:{}:{}", client, tool),
            &args,
            RiskLevel::Medium,
        )
        .await
    {
        ConfirmationResult::AutoApproved => {}
        ConfirmationResult::RequiresConfirmation {
            tool_name,
            params,
            risk_level,
        } => {
            return Err(serde_json::to_string(&serde_json::json!({
                "requires_confirmation": true,
                "tool_name": tool_name,
                "params": params,
                "risk_level": risk_level,
            }))
            .map_err(|e| log_err("mcp_tool_call", e))?);
        }
        ConfirmationResult::Blocked => {
            return Err("MCP tool call blocked by security policy".to_string());
        }
    }

    let cancel = CancellationToken::new();
    let result = state
        .tools
        .mcp_manager
        .call_tool(&client, &tool, args, cancel)
        .await
        .map_err(|e| log_err("mcp_tool_call", e))?;
    Ok(serde_json::json!({
        "success": result.success,
        "output": result.output,
        "error": result.error,
    }))
}

#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, Arc<AppState>>,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Persist to config
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| log_err("add_mcp_server", e))?;
    loader.config_mut().mcp_servers.push(config.clone());
    loader.save().map_err(|e| log_err("add_mcp_server", e))?;

    // Create client and connect
    let client = std::sync::Arc::new(haven_tools::McpClient::new(
        &config.name,
        &config.command,
        &config.args,
        &config.env,
    ));
    if config.enabled {
        client.connect().await.map_err(|e| log_err("add_mcp_server", e))?;
        // Start health monitor
        let discovery = loader.config().mcp_discovery.clone();
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
    state.tools.mcp_manager.add_client(client).await;
    // Keep the in-memory server_configs map in sync so `load_mcp` and the
    // MCP server index reflect the newly added server.
    state.tools.upsert_mcp_server_config(config.clone()).await;
    // Rebuild tool catalog so MCP tools appear in the Reasoner's tool list.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name: config.name,
            status: McpClientStatus::Connected,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn update_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    config: McpServerConfig,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| log_err("update_mcp_server", e))?;
    let servers = &mut loader.config_mut().mcp_servers;
    if let Some(existing) = servers.iter_mut().find(|s| s.name == name) {
        let config_changed = existing.command != config.command
            || existing.args != config.args
            || existing.env != config.env;
        *existing = config.clone();
        loader.save().map_err(|e| log_err("update_mcp_server", e))?;

        // If command/args/env changed, reconnect; if only enabled, toggle
        if config_changed {
            state.tools.mcp_manager.remove_client(&name).await;
            if config.enabled {
                let client = std::sync::Arc::new(haven_tools::McpClient::new(
                    &config.name,
                    &config.command,
                    &config.args,
                    &config.env,
                ));
                client.connect().await.map_err(|e| log_err("update_mcp_server", e))?;
                let discovery = loader.config().mcp_discovery.clone();
                let health_interval =
                    std::time::Duration::from_secs(discovery.health_interval_secs);
                let initial_backoff =
                    std::time::Duration::from_millis(discovery.reconnect_initial_ms);
                let max_backoff = std::time::Duration::from_millis(discovery.reconnect_max_ms);
                let status_tx = state.tools.mcp_manager.status_tx();
                client.clone().spawn_monitor(
                    health_interval,
                    initial_backoff,
                    max_backoff,
                    discovery.reconnect_max_retries,
                    status_tx,
                );
                state.tools.mcp_manager.add_client(client).await;
            }
        } else if config.enabled {
            // Toggle from disabled to enabled
            let client = std::sync::Arc::new(haven_tools::McpClient::new(
                &config.name,
                &config.command,
                &config.args,
                &config.env,
            ));
            client.connect().await.map_err(|e| log_err("update_mcp_server", e))?;
            let discovery = loader.config().mcp_discovery.clone();
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
            state.tools.mcp_manager.add_client(client).await;
        } else {
            // Disabled: shutdown
            state.tools.mcp_manager.remove_client(&name).await;
        }
    } else {
        return Err(format!("MCP server '{}' not found", name));
    }

    // Keep server_configs in sync with the updated config.
    state.tools.upsert_mcp_server_config(config.clone()).await;

    // Rebuild tool catalog for consistency with add/remove/toggle commands.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name: config.name,
            status: if config.enabled {
                McpClientStatus::Connected
            } else {
                McpClientStatus::Disconnected
            },
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn remove_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    // Remove from config
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| log_err("remove_mcp_server", e))?;
    loader.config_mut().mcp_servers.retain(|s| s.name != name);
    loader.save().map_err(|e| log_err("remove_mcp_server", e))?;

    // Shutdown and remove from manager
    state.tools.mcp_manager.remove_client(&name).await;
    state.tools.remove_mcp_server_config(&name).await;

    // Rebuild tool catalog so removed MCP tools disappear from the Reasoner.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name,
            status: McpClientStatus::Disconnected,
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn toggle_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| log_err("toggle_mcp_server", e))?;
    let servers = &mut loader.config_mut().mcp_servers;
    if let Some(existing) = servers.iter_mut().find(|s| s.name == name) {
        existing.enabled = enabled;
        loader.save().map_err(|e| log_err("toggle_mcp_server", e))?;
    } else {
        return Err(format!("MCP server '{}' not found", name));
    }

    let config = loader
        .config()
        .mcp_servers
        .iter()
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| format!("MCP server '{}' not found", name))?;

    if enabled {
        // Reconnect
        let client = std::sync::Arc::new(haven_tools::McpClient::new(
            &config.name,
            &config.command,
            &config.args,
            &config.env,
        ));
        client.connect().await.map_err(|e| log_err("toggle_mcp_server", e))?;
        let discovery = loader.config().mcp_discovery.clone();
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
        state.tools.mcp_manager.add_client(client).await;
    } else {
        state.tools.mcp_manager.remove_client(&name).await;
    }

    // Keep server_configs in sync (enabled flag changed).
    state.tools.upsert_mcp_server_config(config.clone()).await;

    // Rebuild tool catalog so the tool list reflects the toggle.
    state.tools.rebuild_catalog().await;

    let _ = app.emit(
        "mcp:status_change",
        McpStatusChangeEvent {
            name,
            status: if enabled {
                McpClientStatus::Connected
            } else {
                McpClientStatus::Disconnected
            },
        },
    );
    Ok(())
}

#[tauri::command]
pub async fn configure_mcp(
    state: State<'_, Arc<AppState>>,
    name: String,
    transport: String,
    config: String,
) -> Result<(), String> {
    let _ = state
        .db
        .save_mcp_server(&name, &transport, &config)
        .map_err(|e| log_err("configure_mcp", e))?;
    Ok(())
}

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
    state
        .tools
        .skills_engine
        .set_enabled(&name, enabled)
        .await
        .map_err(|e| log_err("set_skill_enabled", e))?;
    // The engine's `set_enabled` already syncs its internal filter so the
    // toggle survives `refresh_from_disk`. Persist the filter to config.toml
    // so it also survives app restart.
    let filter = state.tools.skills_engine.enabled_filter().await;
    let mut loader = haven_common::config::ConfigLoader::load().map_err(|e| log_err("set_skill_enabled", e))?;
    loader.config_mut().skills.enabled = filter;
    loader.save().map_err(|e| log_err("set_skill_enabled", e))?;

    // Rebuild tool catalog so the enable/disable takes effect in the Reasoner.
    state.tools.rebuild_catalog().await;

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

    let risk_level = haven_common::types::RiskLevel::Medium;
    if confirmed.unwrap_or(false) {
        state
            .tools
            .safety_gateway
            .trust_risk_level(risk_level)
            .await;
    } else {
        match state
            .tools
            .safety_gateway
            .check(&format!("skill:{}", name), &params, risk_level)
            .await
        {
            haven_tools::ConfirmationResult::AutoApproved => {}
            haven_tools::ConfirmationResult::RequiresConfirmation {
                tool_name,
                params,
                risk_level,
            } => {
                return Err(serde_json::to_string(&serde_json::json!({
                    "requires_confirmation": true,
                    "tool_name": tool_name,
                    "params": params,
                    "risk_level": risk_level,
                }))
                .map_err(|e| log_err("execute_skill", e))?);
            }
            haven_tools::ConfirmationResult::Blocked => {
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
pub async fn search_history(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    state.db.search_tasks(&query).map_err(|e| log_err("search_history", e))
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_history_filtered(
    state: State<'_, Arc<AppState>>,
    query: Option<String>,
    status: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<haven_memory::repositories::tasks::Task>, String> {
    state
        .db
        .search_tasks_filtered(
            query.as_deref(),
            status.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            limit.unwrap_or(50),
            offset.unwrap_or(0),
        )
        .map_err(|e| log_err("search_history_filtered", e))
}

/// Manually update a task's display title.
#[tauri::command]
pub async fn update_task_title(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    task_id: String,
    title: String,
) -> Result<(), String> {
    let title = title.trim().to_string();
    if title.is_empty() {
        return Err("Title cannot be empty".into());
    }
    state
        .db
        .update_task_title(&task_id, &title)
        .map_err(|e| log_err("update_task_title", e))?;
    state.executor.update_task_title(&task_id, &title).await;
    let _ = app.emit(
        "task:title-updated",
        serde_json::json!({
            "task_id": task_id,
            "title": title,
        }),
    );
    Ok(())
}

#[tauri::command]
pub async fn delete_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    state.db.delete_task(&task_id).map_err(|e| log_err("delete_task", e))?;
    state.executor.remove_task(&task_id).await;
    Ok(())
}

#[tauri::command]
pub async fn clear_history(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let count = state.db.clear_tasks().map(|n| n as u64).map_err(|e| log_err("clear_history", e))?;
    state.executor.clear_all_tasks().await;
    Ok(count)
}

#[tauri::command]
pub async fn get_api_key_status() -> Result<serde_json::Value, String> {
    let loader = haven_common::config::ConfigLoader::load().map_err(|e| log_err("get_api_key_status", e))?;
    let cfg = loader.config();
    Ok(serde_json::json!({
        "small_model": !cfg.llm.small_model.api_key.is_empty(),
        "default_model": !cfg.llm.default_model.api_key.is_empty(),
        "balanced_model": !cfg.llm.balanced_model.api_key.is_empty(),
        "image_model": !cfg.llm.image_model.api_key.is_empty(),
        "audio_model": !cfg.llm.audio_model.api_key.is_empty(),
        "stt": !cfg.stt.api_key.is_empty(),
    }))
}

/// Probe the configured default-model endpoint for live connectivity
/// (GET /models). The top-right status indicator uses this to show
/// Ready (green) when reachable or Disconnected (gray) when not.
#[tauri::command]
pub async fn check_llm_connection(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.agent.check_llm_connection().await)
}

/// §2.7: List available models from the built-in catalog
#[tauri::command]
pub async fn list_models(query: Option<String>) -> Result<Vec<ModelInfo>, String> {
    let reg = ModelRegistry::new();
    let results = match query {
        Some(q) if !q.is_empty() => {
            // Return owned ModelInfo from search
            let found = reg.search(&q);
            found.into_iter().cloned().collect()
        }
        _ => reg.all().into_iter().cloned().collect(),
    };
    Ok(results)
}

/// Resolve the default base URL for an STT provider (used when the user has
/// not overridden it), so the stored-key guard in `discover_models` can match
/// the requested URL.
fn stt_default_base_url(provider: &str) -> &'static str {
    match provider {
        "groq" => "https://api.groq.com/openai/v1",
        "gemini" => "https://generativelanguage.googleapis.com/v1beta",
        "deepgram" => "https://api.deepgram.com/v1",
        "assemblyai" => "https://api.assemblyai.com",
        _ => "https://api.openai.com/v1",
    }
}

/// §2.7: Fetch models from a provider's `/models` endpoint (OpenAI-
/// compatible). Used by the settings UI to populate the model dropdown after
/// the base URL and API key are entered. When `api_key` is empty (the
/// frontend masks stored keys) and `role` names a configured slot, the stored
/// key for that role is used — but only when the requested URL matches the
/// role's configured endpoint, so the stored key can never be sent to an
/// arbitrary renderer-supplied host.
#[tauri::command]
pub async fn discover_models(
    base_url: String,
    api_key: String,
    role: Option<String>,
    app: tauri::AppHandle,
) -> Result<Vec<ModelInfo>, String> {
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        return Err("base_url must be an http(s) URL".to_string());
    }
    let key = if api_key.is_empty() {
        if let Some(role) = role.as_deref() {
            let state = app.state::<Arc<AppState>>();
            let cfg = {
                let guard = state.config_loader.lock().map_err(|e| log_err("discover_models", e))?;
                guard.config().clone()
            };
            if role == "stt" {
                let stt = &cfg.stt;
                let stt_base = if stt.base_url.is_empty() {
                    stt_default_base_url(&stt.provider)
                } else {
                    stt.base_url.as_str()
                };
                if normalize_endpoint_url(stt_base) == normalize_endpoint_url(&base_url) {
                    stt.api_key.clone()
                } else {
                    String::new()
                }
            } else {
                let mut llm = cfg.llm.clone();
                match role_endpoint(&mut llm, role) {
                    Some(ep)
                        if normalize_endpoint_url(&ep.base_url)
                            == normalize_endpoint_url(&base_url) =>
                    {
                        ep.api_key.clone()
                    }
                    _ => String::new(),
                }
            }
        } else {
            String::new()
        }
    } else {
        api_key
    };
    // Resolve the role endpoint's auth scheme so discovery works for
    // Anthropic (`x-api-key`), Gemini (`x-goog-api-key`) and custom-gateway
    // endpoints — not just OpenAI-style `Authorization: Bearer`.
    let auth_header = {
        let state = app.state::<Arc<AppState>>();
        let cfg = {
            let guard = state.config_loader.lock().map_err(|e| log_err("discover_models", e))?;
            guard.config().clone()
        };
        if role.as_deref() == Some("stt") {
            match cfg.stt.provider.as_str() {
                "gemini" => Some(("x-goog-api-key".to_string(), key.clone())),
                _ => Some(("Authorization".to_string(), format!("Bearer {}", key))),
            }
        } else {
            let mut llm = cfg.llm.clone();
            role.as_deref().and_then(|role| {
                role_endpoint(&mut llm, role)
                    .filter(|ep| {
                        normalize_endpoint_url(&ep.base_url) == normalize_endpoint_url(&base_url)
                    })
                    .map(|ep| {
                        let customized =
                            ep.auth_header_name != "Authorization" || ep.auth_header_prefix != "Bearer";
                        if customized {
                            (
                                ep.auth_header_name.clone(),
                                format!("{} {}", ep.auth_header_prefix, key),
                            )
                        } else {
                            match ep.provider.as_str() {
                                "anthropic" => ("x-api-key".to_string(), key.clone()),
                                "google" | "gemini" => ("x-goog-api-key".to_string(), key.clone()),
                                _ => ("Authorization".to_string(), format!("Bearer {}", key)),
                            }
                        }
                    })
            })
        }
    };
    let mut reg = ModelRegistry::new();
    tracing::info!("discovering models from {}", base_url);
    let models = reg
        .discover_from(
            &base_url,
            &key,
            auth_header.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
        )
        .await
        .map_err(|e| {
            tracing::warn!("model discovery failed for {}: {}", base_url, e);
            e.to_string()
        })?;
    tracing::info!("discovered {} models from {}", models.len(), base_url);
    Ok(models)
}

/// §2.7: Switch a model endpoint role to a different model.
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn switch_model(
    role: String,
    model_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();

    // Lock and update config
    let mut loader = {
        let guard = state.config_loader.lock().map_err(|e| log_err("switch_model", e))?;
        guard.clone()
    };
    {
        let cfg = loader.config_mut();
        let ep = role_endpoint(&mut cfg.llm, role.as_str())
            .ok_or_else(|| format!("unknown role: {}", role))?;
        ep.model_name = model_id;
    }
    loader.save().map_err(|e| log_err("switch_model", e))?;

    // Replace the in-memory config_loader
    {
        let mut guard = state.config_loader.lock().map_err(|e| log_err("switch_model", e))?;
        *guard = loader;
    }

    // Hot-swap the LlmRouter and all router-dependent runtime state
    let config = {
        let guard = state.config_loader.lock().map_err(|e| log_err("switch_model", e))?;
        guard.config().clone()
    };
    let new_router = Arc::new(LlmRouter::new(config.llm.clone()));
    hot_swap_router(&state, new_router).await?;

    Ok(())
}

/// Set the reasoning effort of a model endpoint role (e.g. "low"/"medium"/"high").
/// Updates config.toml and hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn set_reasoning_effort(
    role: String,
    effort: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();

    let normalized = match effort {
        Some(e) if e.trim().is_empty() => None,
        Some(e) => Some(e.trim().to_string()),
        None => None,
    };

    // Lock and update config
    let mut loader = {
        let guard = state.config_loader.lock().map_err(|e| log_err("set_reasoning_effort", e))?;
        guard.clone()
    };
    {
        let cfg = loader.config_mut();
        let ep = role_endpoint(&mut cfg.llm, role.as_str())
            .ok_or_else(|| format!("unknown role: {}", role))?;
        ep.reasoning_effort = normalized;
    }
    loader.save().map_err(|e| log_err("set_reasoning_effort", e))?;

    // Replace the in-memory config_loader
    {
        let mut guard = state.config_loader.lock().map_err(|e| log_err("set_reasoning_effort", e))?;
        *guard = loader;
    }

    // Hot-swap the LlmRouter and all router-dependent runtime state
    let config = {
        let guard = state.config_loader.lock().map_err(|e| log_err("set_reasoning_effort", e))?;
        guard.config().clone()
    };
    let new_router = Arc::new(LlmRouter::new(config.llm.clone()));
    hot_swap_router(&state, new_router).await?;

    Ok(())
}

#[tauri::command]
pub async fn export_history(
    state: State<'_, Arc<AppState>>,
    start_date: Option<String>,
    end_date: Option<String>,
    status: Option<String>,
) -> Result<String, String> {
    let tasks = state
        .db
        .search_tasks_filtered(
            None,
            status.as_deref(),
            start_date.as_deref(),
            end_date.as_deref(),
            10000,
            0,
        )
        .map_err(|e| log_err("export_history", e))?;
    serde_json::to_string_pretty(&serde_json::json!({
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "count": tasks.len(),
        "tasks": tasks,
    }))
    .map_err(|e| log_err("export_history", e))
}

#[tauri::command]
pub async fn get_conversation_memory(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<Vec<haven_memory::repositories::messages::Message>, String> {
    state.db.get_session_messages(&session_id).map_err(|e| log_err("get_conversation_memory", e))
}

#[tauri::command]
pub async fn clear_conversation(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // Close the current session and create a new one
    let _ = state.db.close_active_session();
    let _ = state.db.get_or_create_active_session().map_err(|e| log_err("clear_conversation", e))?;
    Ok(())
}

// M6-04: Fact management commands
#[tauri::command]
pub async fn list_facts(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<haven_memory::repositories::facts::Fact>, String> {
    state.db.list_facts().map_err(|e| log_err("list_facts", e))
}

#[tauri::command]
pub async fn add_fact(
    state: State<'_, Arc<AppState>>,
    subject: String,
    predicate: String,
    object: String,
    tags: Option<Vec<String>>,
) -> Result<haven_memory::repositories::facts::Fact, String> {
    let tags_owned = tags.unwrap_or_default();
    let tags: Vec<&str> = tags_owned.iter().map(|s| s.as_str()).collect();
    state
        .db
        .insert_fact(&subject, &predicate, &object, "user", 1.0, &tags)
        .map_err(|e| log_err("add_fact", e))
}

#[tauri::command]
pub async fn delete_fact(state: State<'_, Arc<AppState>>, fact_id: String) -> Result<(), String> {
    state.db.delete_fact(&fact_id).map_err(|e| log_err("delete_fact", e))
}

#[tauri::command]
pub async fn get_preference(
    state: State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<String>, String> {
    state.db.get_preference(&key).map_err(|e| log_err("get_preference", e))
}

#[tauri::command]
pub async fn list_preferences(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<(String, String)>, String> {
    state.db.list_preferences().map_err(|e| log_err("list_preferences", e))
}

#[tauri::command]
pub async fn update_preference(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    state.db.set_preference(&key, &value).map_err(|e| log_err("update_preference", e))
}

#[tauri::command]
pub async fn delete_preference(state: State<'_, Arc<AppState>>, key: String) -> Result<(), String> {
    state.db.delete_preference(&key).map_err(|e| log_err("delete_preference", e))
}

#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<haven_common::config::Settings, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = state.config_loader.lock().map_err(|e| log_err("get_settings", e))?;
    let settings = cfg.settings();
    Ok(settings)
}

#[tauri::command]
pub async fn update_settings(
    settings: haven_common::config::Settings,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    let old_hotkey = {
        let cfg = state.config_loader.lock().map_err(|e| log_err("update_settings", e))?;
        cfg.config().hotkey.key_binding.clone()
    };

    {
        let mut loader = state.config_loader.lock().map_err(|e| log_err("update_settings", e))?;
        loader.apply_settings(&settings);
        loader.save().map_err(|e| log_err("update_settings", e))?;
    }

    // Propagate audio config to running pipeline
    state.pipeline.update_config(settings.audio).await;

    // Reload MCP servers from config
    let (mcp_servers, mcp_discovery, task_max_steps, llm_config, min_risk_level) = {
        let cfg = state.config_loader.lock().map_err(|e| log_err("update_settings", e))?;
        let config = cfg.config();
        (
            config.mcp_servers.clone(),
            config.mcp_discovery.clone(),
            config.task.max_steps,
            config.llm.clone(),
            config.security.min_risk_level,
        )
    };
    state.tools.load_mcp_from_config(&mcp_servers).await;
    state.tools.mcp_manager.start_monitors(&mcp_discovery).await;
    let new_router = Arc::new(LlmRouter::new(llm_config));
    hot_swap_router(&state, new_router).await?;
    state.agent.set_max_steps(task_max_steps);
    state
        .tools
        .safety_gateway
        .set_min_risk_level(min_risk_level)
        .await;

    // Propagate log level to tracing subscriber (console + file)
    let level = settings.log.level.as_str();
    for handle in &state.log_filter_handles {
        let _ = handle.modify(|filter| {
            *filter = EnvFilter::new(format!("haven={}", level));
        });
    }

    // Propagate hotkey mode change (always)
    use haven_common::types::HotkeyMode;
    state
        .shell
        .set_hold_mode(settings.hotkey.mode == HotkeyMode::Hold)
        .await;

    if settings.hotkey.key_binding != old_hotkey {
        use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

        if let Some(old_shortcut) = crate::parse_shortcut(&old_hotkey) {
            let _ = app.global_shortcut().unregister(old_shortcut);
        }

        if let Some(new_shortcut) = crate::parse_shortcut(&settings.hotkey.key_binding) {
            let result =
                app.global_shortcut()
                    .on_shortcut(new_shortcut, move |_app, _sc, event| {
                        let state = _app.state::<Arc<AppState>>();
                        let shell = &state.shell;
                        tokio::task::block_in_place(|| {
                            let rt = tokio::runtime::Handle::current();
                            let shell_state = rt.block_on(shell.get_state());
                            if shell_state.is_muted {
                                return;
                            }
                            if shell_state.hold_mode {
                                if event.state == ShortcutState::Pressed {
                                    rt.block_on(shell.hold_press());
                                } else {
                                    rt.block_on(shell.hold_release());
                                }
                            } else {
                                if event.state == ShortcutState::Pressed {
                                    rt.block_on(shell.toggle_recording());
                                }
                            }
                        });
                    });

            match result {
                Ok(()) => {
                    tracing::info!(
                        "Hotkey rebound: {} -> {}",
                        old_hotkey,
                        settings.hotkey.key_binding,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "Hotkey rebind conflict: {} - {}",
                        settings.hotkey.key_binding,
                        e,
                    );
                }
            }
        }

        let _ = app.emit(
            "hotkey:rebind",
            serde_json::json!({
                "old_binding": old_hotkey,
                "new_binding": settings.hotkey.key_binding,
            }),
        );
    }
    Ok(())
}

#[tauri::command]
pub async fn enable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    // Debug builds load from devUrl (localhost:4721) — autostart would
    // launch the binary without the Vite dev server, showing a blank/
    // connection-error page.  Only release builds embed the frontend
    // and can be safely autostarted.
    if cfg!(debug_assertions) {
        return Err("自动启动仅支持生产版本（cargo tauri build）。开发模式下请手动运行。".into());
    }
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().enable().map_err(|e| log_err("enable_autostart", e))?;
    Ok(())
}

#[tauri::command]
pub async fn disable_autostart(app: tauri::AppHandle) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().disable().map_err(|e| log_err("disable_autostart", e))?;
    Ok(())
}

#[tauri::command]
pub async fn is_autostart_enabled(app: tauri::AppHandle) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().map_err(|e| log_err("is_autostart_enabled", e))
}

#[derive(Serialize)]
pub struct TaskReviewResponse {
    pub task: Task,
    pub messages: Vec<Message>,
    pub steps: Vec<TaskStep>,
}

/// Load the session messages and steps for a task into a review response.
/// Shared by `get_task_for_review` and `get_last_conversation`.
fn review_response_for_task(
    db: &haven_memory::Database,
    task: Task,
) -> Result<TaskReviewResponse, String> {
    let messages = match task.session_id.as_deref() {
        Some(session_id) if !session_id.is_empty() => {
            db.get_session_messages(session_id).map_err(|e| log_err("is_autostart_enabled", e))?
        }
        _ => Vec::new(),
    };
    let steps = db.get_task_steps(&task.id).map_err(|e| log_err("is_autostart_enabled", e))?;
    Ok(TaskReviewResponse {
        task,
        messages,
        steps,
    })
}

#[tauri::command]
pub async fn get_task_for_review(
    state: State<'_, Arc<AppState>>,
    task_id: String,
) -> Result<TaskReviewResponse, String> {
    let task = state
        .db
        .get_task(&task_id)
        .map_err(|e| log_err("get_task_for_review", e))?
        .ok_or_else(|| format!("Task not found: {}", task_id))?;
    review_response_for_task(&state.db, task)
}

/// Return the most recent persisted task with its session messages and
/// steps, for the chat page to auto-restore the last conversation on app
/// start. Returns `None` when no task exists yet.
#[tauri::command]
pub async fn get_last_conversation(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<TaskReviewResponse>, String> {
    let tasks = state.db.list_tasks(1, 0).map_err(|e| log_err("get_last_conversation", e))?;
    match tasks.into_iter().next() {
        Some(task) => review_response_for_task(&state.db, task).map(Some),
        None => Ok(None),
    }
}

/// Roll back a task to a specific branch point. The task is rewound to
/// the saved state at that step. When `pause` is true the task is set to
/// Paused (user wants to edit the message before re-sending); otherwise it
/// is set to Pending for immediate re-execution.
#[tauri::command]
pub async fn rollback_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    target_step: u32,
    pause: Option<bool>,
) -> Result<(), String> {
    state
        .agent
        .rollback_task(&task_id, target_step, pause.unwrap_or(false))
        .await
        .map_err(|e| log_err("rollback_task", e))
}

/// Branch a task from a specific step into a new conversation. Copies all
/// messages up to that step into a new child session and creates a new Paused
/// task seeded with the branch point state. Returns the new task id.
#[tauri::command]
pub async fn branch_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    target_step: u32,
) -> Result<String, String> {
    state
        .agent
        .branch_task(&task_id, target_step)
        .await
        .map_err(|e| log_err("branch_task", e))
}

/// Resume a task that errored mid-step. Removes partial output persisted on
/// error and sets the task to Pending so the dispatcher retries the failed
/// step from the saved snapshot.
#[tauri::command]
pub async fn continue_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    state.agent.continue_task(&task_id).await.map_err(|e| log_err("continue_task", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_list_response_serde() {
        let resp = TaskListResponse { tasks: vec![] };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"tasks":[]}"#);
    }

    #[test]
    fn test_tool_list_response_serde() {
        let tools = vec![serde_json::json!({"name": "file", "description": "File operations"})];
        let resp = ToolListResponse { tools };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("file"));
        assert!(json.contains("File operations"));
    }

    #[test]
    fn test_recording_state_default() {
        let state = RecordingState {
            is_recording: false,
            is_toggle: false,
        };
        assert!(!state.is_recording);
        assert!(!state.is_toggle);
    }

    #[test]
    fn test_recording_state_serde() {
        let state = RecordingState {
            is_recording: true,
            is_toggle: true,
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"is_recording\":true"));
        assert!(json.contains("\"is_toggle\":true"));
    }

    fn att(
        media_type: &str,
        data: &str,
    ) -> haven_memory::repositories::messages::MessageAttachment {
        haven_memory::repositories::messages::MessageAttachment {
            media_type: media_type.into(),
            data: data.into(),
        }
    }

    #[test]
    fn test_validate_images_accepts_valid() {
        let imgs = vec![att("image/png", "aGVsbG8="), att("image/jpeg", "YWJj")];
        let out = validate_images(imgs.clone()).unwrap();
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn test_validate_images_rejects_over_count() {
        let imgs: Vec<_> = (0..5).map(|_| att("image/png", "aGVsbG8=")).collect();
        let err = validate_images(imgs).unwrap_err();
        assert!(err.contains("最多支持"));
    }

    #[test]
    fn test_validate_images_rejects_non_image_mime() {
        let imgs = vec![att("application/x-msdownload", "aGVsbG8=")];
        let err = validate_images(imgs).unwrap_err();
        assert!(err.contains("不支持的图片类型"));
    }

    #[test]
    fn test_validate_images_rejects_oversized() {
        let big = "A".repeat(15 * 1024 * 1024);
        let imgs = vec![att("image/png", &big)];
        let err = validate_images(imgs).unwrap_err();
        assert!(err.contains("10MB"));
    }

    #[test]
    fn test_validate_images_rejects_invalid_base64() {
        let imgs = vec![att("image/png", "not-base64!!!")];
        let err = validate_images(imgs).unwrap_err();
        assert!(err.contains("base64"));
    }
}
