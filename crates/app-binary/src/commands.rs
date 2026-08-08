use crate::app_state::AppState;
use crate::events::*;
use haven_common::McpServerConfig;
use haven_common::config::{LlmConfig, ModelEndpoint};
use haven_common::types::RiskLevel;
use haven_input::{RecordingReason, RecordingResult};
use haven_llm::stt::build_stt_client;
use haven_llm::{EndpointRole, LlmRouter};
use haven_llm::{ModelInfo, ModelRegistry};
use haven_memory::repositories::messages::Message;
use haven_memory::repositories::task_steps::TaskStep;
use haven_memory::repositories::tasks::Task;
use haven_tools::{ConfirmationResult, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
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

/// Build an `McpClient`, connect it (when `config.enabled`), and spawn the
/// health monitor using the discovery settings from the supplied loader.
/// Returns the constructed client either way so the caller can register it
/// with the manager. The caller is responsible for persisting the config
/// (before or after the call, depending on whether a failed connect should
/// roll the change back — `toggle_mcp_server` connects first so a failure
/// leaves the config unchanged). Used by `add_mcp_server`, `update_mcp_server`,
/// and `toggle_mcp_server`.
async fn connect_and_monitor(
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

pub(crate) fn recording_reason_str(reason: RecordingReason) -> &'static str {
    match reason {
        RecordingReason::Manual => "manual",
        RecordingReason::Silence => "silence",
        RecordingReason::MaxDuration => "max_duration",
        RecordingReason::Cancel => "cancel",
    }
}

/// Emit `recording:started` with a freshly generated session id. Used by
/// both the `start_recording` Tauri command and the shell hotkey start path
/// so the wire shape stays consistent across entry points.
pub(crate) fn emit_recording_started(app: &tauri::AppHandle) {
    let _ = app.emit(
        "recording:started",
        RecordingEvent {
            is_recording: true,
            session_id: Some(uuid::Uuid::new_v4().to_string()),
            reason: None,
            duration_ms: None,
        },
    );
}

/// Emit `recording:stopped` with the supplied reason and duration. `reason`
/// may be either a `RecordingReason` (from the pipeline) or a literal
/// `"cancel"` for the manual cancel command, which doesn't go through the
/// pipeline's stop path.
pub(crate) fn emit_recording_stopped(
    app: &tauri::AppHandle,
    reason: &str,
    duration_ms: Option<u64>,
) {
    let _ = app.emit(
        "recording:stopped",
        RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: Some(reason.to_string()),
            duration_ms,
        },
    );
}

/// Emit `recording:error` with a freshly generated session id and the
/// user-facing error message.
pub(crate) fn emit_recording_error(app: &tauri::AppHandle, error: impl Into<String>) {
    let _ = app.emit(
        "recording:error",
        serde_json::json!({
            "session_id": uuid::Uuid::new_v4().to_string(),
            "error": error.into(),
        }),
    );
}

/// Build the JSON payload returned to the frontend when a tool call needs
/// user confirmation. Used by both `mcp_tool_call` and `execute_skill` so the
/// wire shape is identical across tool types.
fn confirmation_error(
    tool_name: String,
    params: Value,
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

/// Resolve a model role string to its endpoint slot, or `None` for unknown
/// roles. Single source of truth for the role names accepted by the model
/// commands (`switch_model`, `set_reasoning_effort`).
fn role_endpoint<'a>(cfg: &'a mut LlmConfig, role: &str) -> Option<&'a mut ModelEndpoint> {
    let endpoint = match EndpointRole::from_str(role)? {
        EndpointRole::SmallModel => &mut cfg.small_model,
        EndpointRole::DefaultModel => &mut cfg.default_model,
        EndpointRole::BalancedModel => &mut cfg.balanced_model,
        EndpointRole::ImageModel => &mut cfg.image_model,
        EndpointRole::AudioModel => &mut cfg.audio_model,
        EndpointRole::EmbeddingModel => &mut cfg.embedding_model,
    };
    Some(endpoint)
}

/// Normalize an endpoint URL for comparison: strip the trailing slash and
/// lowercase it (scheme/host comparisons are case-insensitive).
fn normalize_endpoint_url(url: &str) -> String {
    url.trim_end_matches('/').to_ascii_lowercase()
}

/// Rebuild the LlmRouter from the current config and hot-swap it into the
/// runtime. Shared by `switch_model` and `set_reasoning_effort`, which both
/// follow the same "save config → rebuild router → swap live" sequence.
async fn rebuild_router(state: &AppState, ctx: &str) -> Result<(), String> {
    let config = {
        let guard = state.config_loader.lock().map_err(|e| log_err(ctx, e))?;
        guard.config().clone()
    };
    let new_router = Arc::new(LlmRouter::new(config.llm.clone()));
    hot_swap_router(state, new_router).await
}

/// Rebuild the router-dependent runtime after a model/config change: the
/// agent's LlmRouter, the tools' router, and the pipeline STT client (which
/// captures the router at construction — without a rebuild it keeps calling
/// a stale router after a model switch).
async fn hot_swap_router(state: &AppState, new_router: Arc<LlmRouter>) -> Result<(), String> {
    state.agent.replace_router(new_router.clone());
    state.tools.set_router(new_router.clone()).await;

    let stt_config = {
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("hot_swap_router", e))?;
        cfg.config().stt.clone()
    };
    let mcp_caller: Arc<dyn haven_llm::McpToolCaller> = Arc::new(state.tools.mcp_manager.clone());
    match build_stt_client(new_router, Some(mcp_caller), &stt_config) {
        Ok(client) => {
            state
                .pipeline
                .set_stt_client(client.map(std::sync::Arc::from))
                .await;
        }
        Err(e) => {
            tracing::warn!("STT client rebuild failed, transcription disabled: {e}");
            state.pipeline.set_stt_client(None).await;
        }
    }
    Ok(())
}

/// Transcribe a captured recording and emit `transcription:result` /
/// `transcription:error`.
///
/// Shared by the `stop_recording` Tauri command and the shell hotkey/VAD stop
/// path (`HavenShellHandler::on_recording_stop`), so both surfaces behave
/// identically — previously the shell path silently dropped the transcript.
///
/// The transcript is **not** submitted to the agent here: the frontend
/// listens for `transcription:result` and delivers the text through the same
/// `process_transcript` path as a typed message, so voice input continues the
/// currently open conversation (task) instead of always starting a new one.
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
        if matches!(pipeline_state, haven_input::RecordingState::Recording) {
            state.shell.sync_recording(true).await;
            emit_recording_started(&app);
            return Ok(());
        }
        let msg = if matches!(pipeline_state, haven_input::RecordingState::Processing) {
            "正在处理上一条录音，请稍候再试".to_string()
        } else {
            format!("录音启动失败，请检查麦克风/STT 配置: {e}")
        };
        emit_recording_error(&app, msg.clone());
        return Err(msg);
    }
    // Keep the shell state in sync so the tray icon, the mute hotkey and the
    // recording toggle reflect a UI-button-started recording.
    state.shell.sync_recording(true).await;
    emit_recording_started(&app);
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
    emit_recording_stopped(
        &app,
        recording_reason_str(result.reason),
        Some(result.duration_ms),
    );

    // STT runs inside `finalize_transcription`; the frontend submits the
    // transcript via `process_transcript` (same path as typed input). The
    // command returns as soon as the recording has been handed off.
    let text = finalize_transcription(state.inner(), &app, result).await;
    Ok(text.unwrap_or_default())
}

#[tauri::command]
pub async fn cancel_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    state
        .pipeline
        .cancel_recording()
        .await
        .map_err(|e| log_err("cancel_recording", e))?;
    state.shell.sync_recording(false).await;
    emit_recording_stopped(&app, "cancel", None);
    Ok(())
}

#[tauri::command]
pub async fn process_transcript(
    state: State<'_, Arc<AppState>>,
    transcript: String,
    active_task_id: Option<String>,
    attachments: Option<Vec<haven_memory::repositories::messages::MessageAttachment>>,
    voice: Option<bool>,
) -> Result<Value, String> {
    let limits = state
        .config_loader
        .lock()
        .map_err(|e| log_err("process_transcript", e))?
        .config()
        .context_limits
        .clone();
    let attachments = validate_attachments(attachments.unwrap_or_default(), &limits)?;
    let attachments = persist_file_attachments(attachments).await?;
    let voice = voice.unwrap_or(false);
    tracing::debug!(
        "process_transcript called: text={:?} active_task_id={:?} attachments={} voice={}",
        transcript,
        active_task_id,
        attachments.len(),
        voice
    );
    let result = state
        .agent
        .process_input_with_attachments(&transcript, active_task_id.clone(), &attachments, voice)
        .await
        .map_err(|e| log_err("process_transcript", e))?;
    tracing::debug!("process_transcript result: {:?}", result);
    Ok(serde_json::to_value(result).unwrap_or_default())
}

/// Root folder for user-uploaded files. Lives under the agent's default Temp
/// working directory so the file tool can read uploads with the same access
/// the agent already has for its own scripts.
fn uploads_root() -> std::path::PathBuf {
    haven_common::default_work_dir().join("uploads")
}

/// Replace characters that are illegal in Windows file names (and path
/// traversal hazards) so an uploaded name cannot escape its batch directory.
/// Falls back to a random name for empty / "." / ".." / reserved device names
/// (CON, PRN, AUX, NUL, COM1–9, LPT1–9, incl. `NUL.txt` forms — writing to
/// those opens the device and silently discards the bytes) and caps the
/// length on a char boundary so full paths stay short without panicking.
fn sanitize_filename(name: &str) -> String {
    let mut clean: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\0' | '\n' | '\r' => '_',
            c => c,
        })
        .collect();
    // Windows strips trailing dots/spaces at the filesystem layer; drop them
    // here so `foo.` and `foo` can't silently collide (and overwrite) on disk.
    while clean.ends_with(['.', ' ']) {
        clean.pop();
    }
    let stem = clean
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" => true,
        s if (s.starts_with("COM") || s.starts_with("LPT")) && s.len() == 4 => {
            s.as_bytes()[3].is_ascii_digit()
        }
        _ => false,
    };
    if clean.trim().is_empty() || clean == "." || clean == ".." || reserved {
        clean = format!("file_{}", uuid::Uuid::new_v4());
    }
    // `String::truncate` panics when the index is not a char boundary; pop
    // whole chars instead (CJK/emoji names are common).
    while clean.len() > 120 {
        clean.pop();
    }
    clean
}

/// Write non-image attachments to disk under `uploads/<batch>/` and return
/// them with `path` set. `data` is cleared afterwards — the bytes live on
/// disk, keeping the persisted message and DB storage slim. Images pass
/// through untouched (their base64 payload is needed by the vision model).
async fn persist_file_attachments(
    attachments: Vec<haven_memory::repositories::messages::MessageAttachment>,
) -> Result<Vec<haven_memory::repositories::messages::MessageAttachment>, String> {
    persist_file_attachments_to(uploads_root(), attachments).await
}

async fn persist_file_attachments_to(
    root: std::path::PathBuf,
    attachments: Vec<haven_memory::repositories::messages::MessageAttachment>,
) -> Result<Vec<haven_memory::repositories::messages::MessageAttachment>, String> {
    use base64::Engine as _;

    let mut images = Vec::new();
    let mut files = Vec::new();
    for att in attachments {
        if att.is_image() {
            images.push(att);
        } else {
            files.push(att);
        }
    }
    if files.is_empty() {
        return Ok(images);
    }

    let batch_dir = root.join(uuid::Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&batch_dir)
        .await
        .map_err(|e| format!("创建上传目录失败: {e}"))?;

    let mut used_names = std::collections::HashSet::new();
    for mut att in files {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&att.data)
            .map_err(|_| "附件数据不是有效的 base64".to_string())?;
        let base_name = att
            .filename
            .as_deref()
            .map(sanitize_filename)
            .unwrap_or_else(|| format!("file_{}", uuid::Uuid::new_v4()));
        // Keep the extension for readability but dedupe collisions so two
        // same-named uploads in one batch never overwrite each other.
        let mut name = base_name.clone();
        let mut n = 2;
        while !used_names.insert(name.clone()) {
            let stem = std::path::Path::new(&base_name)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&base_name)
                .to_string();
            let ext = std::path::Path::new(&base_name)
                .extension()
                .and_then(|s| s.to_str())
                .map(|e| format!(".{e}"))
                .unwrap_or_default();
            name = format!("{stem}_{n}{ext}");
            n += 1;
        }
        let file_path = batch_dir.join(&name);
        tokio::fs::write(&file_path, bytes)
            .await
            .map_err(|e| format!("保存附件失败: {e}"))?;
        att.path = Some(file_path.to_string_lossy().into_owned());
        att.data = String::new();
        images.push(att);
    }
    Ok(images)
}

/// Server-side validation for user attachments, mirroring the frontend
/// limits (configurable via `[context_limits]`: max images/files, per-item
/// byte caps, decodable base64, files must carry a name). The webview must
/// not be the sole enforcement point for persisted payloads.
fn validate_attachments(
    attachments: Vec<haven_memory::repositories::messages::MessageAttachment>,
    limits: &haven_common::config::ContextLimitsConfig,
) -> Result<Vec<haven_memory::repositories::messages::MessageAttachment>, String> {
    let max_images = limits.max_attachment_images;
    let max_files = limits.max_attachment_files;
    let max_image_bytes = limits.max_attachment_image_bytes;
    let max_file_bytes = limits.max_attachment_file_bytes;
    use base64::Engine as _;
    let images = attachments.iter().filter(|a| a.is_image()).count();
    let files = attachments.len().saturating_sub(images);
    if images > max_images {
        return Err(format!("最多支持 {max_images} 张图片"));
    }
    if files > max_files {
        return Err(format!("最多支持 {max_files} 个文件"));
    }
    for att in &attachments {
        let (cap, label) = if att.is_image() {
            (max_image_bytes, "图片")
        } else {
            if att.filename.as_deref().unwrap_or("").trim().is_empty() {
                return Err("文件附件缺少文件名".to_string());
            }
            (max_file_bytes, "文件")
        };
        let decoded_len = att.data.len().saturating_mul(3) / 4;
        if decoded_len > cap {
            return Err(format!("{label}超过 {}MB 上限", cap / 1024 / 1024));
        }
        if base64::engine::general_purpose::STANDARD
            .decode(&att.data)
            .is_err()
        {
            return Err("附件数据不是有效的 base64".to_string());
        }
    }
    Ok(attachments)
}

#[tauri::command]
pub async fn reopen_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    tracing::debug!("reopen_task called: task_id={}", task_id);
    state
        .agent
        .reopen_task(&task_id)
        .await
        .map_err(|e| log_err("reopen_task", e))?;
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
        .get_task(&task_id)
        .await
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

    let _ = state
        .executor
        .end_task(&task_id)
        .await
        .map_err(|e| log_err("end_task", e))?;
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
    // List ALL builtin tools (enabled and disabled) with their enabled state
    // so the UI can toggle them. Disabled tools are excluded from the
    // registry the agent sees (see ToolsManager::rebuild_catalog).
    let tools = state.tools.list_builtin_tools().await;
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
    state
        .db
        .list_tasks(limit, offset)
        .map_err(|e| log_err("get_history", e))
}

#[tauri::command]
pub async fn count_history(state: State<'_, Arc<AppState>>) -> Result<i64, String> {
    state
        .db
        .count_tasks()
        .map_err(|e| log_err("count_history", e))
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
    state
        .db
        .count_tasks_search(&query)
        .map_err(|e| log_err("count_history_search", e))
}

#[tauri::command]
pub async fn list_mcp_tools(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<McpServerSnapshot>, String> {
    let mut snapshots: HashMap<String, McpServerSnapshot> = state
        .tools
        .mcp_manager
        .snapshot()
        .await
        .into_iter()
        .map(|s| (s.name.clone(), s))
        .collect();

    // Include configured-but-disabled servers (no live client) so the UI can
    // show their state and re-enable them without re-adding.
    for config in state.tools.list_mcp_server_configs().await {
        let entry = snapshots
            .entry(config.name.clone())
            .or_insert_with(|| McpServerSnapshot {
                name: config.name.clone(),
                transport: config.transport.as_str().into(),
                command: config.command.clone(),
                args: config.args.clone(),
                env: config.env.clone(),
                url: config.url.clone(),
                enabled: config.enabled,
                status: McpClientStatus::Disconnected,
                tools: vec![],
                last_error: None,
                last_seen_at: None,
            });
        entry.enabled = config.enabled;
    }

    let mut result: Vec<_> = snapshots.into_values().collect();
    result.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(result)
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
            return Err(confirmation_error(tool_name, params, risk_level)
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
    // Persist to the shared config loader (single source of truth) so the
    // in-memory copy stays in sync with disk and later `self` tool writes
    // can never resurrect stale values.
    let discovery = {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("add_mcp_server", e))?;
        loader.config_mut().mcp_servers.push(config.clone());
        loader.save().map_err(|e| log_err("add_mcp_server", e))?;
        loader.config().mcp_discovery.clone()
    };

    // Create client and connect
    let client = connect_and_monitor(&state, &discovery, &config, "add_mcp_server").await?;
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
    let (config_changed, discovery) = {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_mcp_server", e))?;
        let servers = &mut loader.config_mut().mcp_servers;
        let Some(existing) = servers.iter_mut().find(|s| s.name == name) else {
            return Err(format!("MCP server '{}' not found", name));
        };
        let config_changed = existing.transport != config.transport
            || existing.command != config.command
            || existing.args != config.args
            || existing.env != config.env
            || existing.url != config.url;
        *existing = config.clone();
        loader.save().map_err(|e| log_err("update_mcp_server", e))?;
        (config_changed, loader.config().mcp_discovery.clone())
    };
    // If command/args/env changed, reconnect; if only enabled, toggle
    if config_changed {
        state.tools.mcp_manager.remove_client(&name).await;
        if config.enabled {
            let client =
                connect_and_monitor(&state, &discovery, &config, "update_mcp_server").await?;
            state.tools.mcp_manager.add_client(client).await;
        }
    } else if config.enabled {
        // Toggle from disabled to enabled
        let client = connect_and_monitor(&state, &discovery, &config, "update_mcp_server").await?;
        state.tools.mcp_manager.add_client(client).await;
    } else {
        // Disabled: shutdown
        state.tools.mcp_manager.remove_client(&name).await;
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
    // Remove from config via the shared loader (single source of truth).
    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("remove_mcp_server", e))?;
        loader.config_mut().mcp_servers.retain(|s| s.name != name);
        loader.save().map_err(|e| log_err("remove_mcp_server", e))?;
    }

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
    let (existing_config, discovery) = {
        let loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("toggle_mcp_server", e))?;
        let Some(existing) = loader.config().mcp_servers.iter().find(|s| s.name == name) else {
            return Err(format!("MCP server '{}' not found", name));
        };
        (existing.clone(), loader.config().mcp_discovery.clone())
    };
    let mut config = existing_config;
    config.enabled = enabled;

    // Persist via the shared loader (single source of truth) so the in-memory
    // copy never diverges from disk.
    if enabled {
        // Reconnect. Connect BEFORE persisting the enabled flag: if the
        // server is unreachable, config must stay disabled so it never
        // diverges from the (absent) live client and monitor.
        let client = connect_and_monitor(&state, &discovery, &config, "toggle_mcp_server").await?;
        {
            let mut loader = state
                .config_loader
                .lock()
                .map_err(|e| log_err("toggle_mcp_server", e))?;
            if let Some(existing) = loader
                .config_mut()
                .mcp_servers
                .iter_mut()
                .find(|s| s.name == name)
            {
                existing.enabled = enabled;
            }
            loader.save().map_err(|e| log_err("toggle_mcp_server", e))?;
        }
        state.tools.mcp_manager.add_client(client).await;
    } else {
        // Disable: no connect to validate, persist the flag now.
        {
            let mut loader = state
                .config_loader
                .lock()
                .map_err(|e| log_err("toggle_mcp_server", e))?;
            if let Some(existing) = loader
                .config_mut()
                .mcp_servers
                .iter_mut()
                .find(|s| s.name == name)
            {
                existing.enabled = enabled;
            }
            loader.save().map_err(|e| log_err("toggle_mcp_server", e))?;
        }
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
    // via the shared loader (single source of truth) so it also survives app
    // restart and never diverges from the in-memory copy.
    let filter = state.tools.skills_engine.enabled_filter().await;
    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("set_skill_enabled", e))?;
        loader.config_mut().skills.enabled = filter;
        loader.save().map_err(|e| log_err("set_skill_enabled", e))?;
    }

    // Rebuild tool catalog so the enable/disable takes effect in the Reasoner.
    state.tools.rebuild_catalog().await;

    Ok(())
}

#[tauri::command]
pub async fn set_tool_enabled(
    state: State<'_, Arc<AppState>>,
    name: String,
    enabled: bool,
) -> Result<(), String> {
    // Persist via the shared loader (single source of truth) so the in-memory
    // copy never diverges from disk and later settings saves can't resurrect
    // stale values.
    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("set_tool_enabled", e))?;
        let entry = loader
            .config_mut()
            .tool_settings
            .entry(name.clone())
            .or_insert_with(haven_common::config::ToolConfig::default);
        entry.enabled = enabled;
        loader.save().map_err(|e| log_err("set_tool_enabled", e))?;
    }

    // Push the updated settings into the runtime manager. `set_tool_settings`
    // rebuilds the catalog so the toggle takes effect in the Reasoner.
    let tool_settings = {
        let loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("set_tool_enabled", e))?;
        loader.config().tool_settings.clone()
    };
    state.tools.set_tool_settings(tool_settings).await;

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
                return Err(confirmation_error(tool_name, params, risk_level)
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
    state
        .db
        .search_tasks(&query)
        .map_err(|e| log_err("search_history", e))
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
    state
        .db
        .delete_task(&task_id)
        .map_err(|e| log_err("delete_task", e))?;
    state.executor.remove_task(&task_id).await;
    Ok(())
}

#[tauri::command]
pub async fn clear_history(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let count = state
        .db
        .clear_tasks()
        .map(|n| n as u64)
        .map_err(|e| log_err("clear_history", e))?;
    state.executor.clear_all_tasks().await;
    Ok(count)
}

#[tauri::command]
pub async fn get_api_key_status() -> Result<serde_json::Value, String> {
    let loader =
        haven_common::config::ConfigLoader::load().map_err(|e| log_err("get_api_key_status", e))?;
    let cfg = loader.config();
    let mut status = serde_json::Map::new();
    for role in EndpointRole::ALL {
        let ep = match role {
            EndpointRole::SmallModel => &cfg.llm.small_model,
            EndpointRole::DefaultModel => &cfg.llm.default_model,
            EndpointRole::BalancedModel => &cfg.llm.balanced_model,
            EndpointRole::ImageModel => &cfg.llm.image_model,
            EndpointRole::AudioModel => &cfg.llm.audio_model,
            EndpointRole::EmbeddingModel => &cfg.llm.embedding_model,
        };
        status.insert(
            role.as_str().to_string(),
            serde_json::json!(!ep.api_key.is_empty()),
        );
    }
    status.insert(
        "stt".to_string(),
        serde_json::json!(!cfg.stt.api_key.is_empty()),
    );
    Ok(serde_json::Value::Object(status))
}

/// Run the memory maintenance pass (fact dedup, sensitive purge, stale-fact
/// flush, embedding pruning). The agent already runs it after each inference;
/// this exposes the same pass for periodic app-level scheduling.
#[tauri::command]
pub async fn run_memory_maintenance(state: State<'_, Arc<AppState>>) -> Result<u64, String> {
    let db = state.db.clone();
    let result = db
        .run_blocking(move |db| {
            let deduped = db.dedup_facts()?;
            let purged = db.delete_sensitive_facts()?;
            let flushed = db.flush_low_confidence(0.3)?;
            let pruned = db.prune_orphaned_embeddings()?;
            Ok::<u64, anyhow::Error>(deduped + purged + flushed + pruned)
        })
        .await
        .map_err(|e| log_err("run_memory_maintenance", e))?;
    Ok(result)
}

/// Recall memory items (facts or episodes) most relevant to a query. Uses
/// the `embedding_model` slot when configured, keyword search otherwise.
#[tauri::command]
pub async fn recall_memory(
    query: String,
    kind: Option<String>,
    limit: Option<usize>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<serde_json::Value>, String> {
    let kind = kind.as_deref().unwrap_or("fact");
    let limit = limit.unwrap_or(5);
    Ok(state.agent.recall_memory(&query, kind, limit).await)
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
                let guard = state
                    .config_loader
                    .lock()
                    .map_err(|e| log_err("discover_models", e))?;
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
            let guard = state
                .config_loader
                .lock()
                .map_err(|e| log_err("discover_models", e))?;
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
                        let customized = ep.auth_header_name != "Authorization"
                            || ep.auth_header_prefix != "Bearer";
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
/// Apply a mutation to a model endpoint via the shared config loader and
/// hot-swap the LlmRouter at runtime. Holds the loader lock across
/// mutate + save so concurrent config writes (settings saves, MCP/skill
/// toggles) can never clobber each other with a stale copy.
async fn update_endpoint_field(
    state: &AppState,
    ctx: &str,
    role: &str,
    mutate: impl FnOnce(&mut ModelEndpoint) -> Result<(), String>,
) -> Result<(), String> {
    {
        let mut loader = state.config_loader.lock().map_err(|e| log_err(ctx, e))?;
        let ep = role_endpoint(&mut loader.config_mut().llm, role)
            .ok_or_else(|| format!("unknown role: {}", role))?;
        mutate(ep)?;
        loader.save().map_err(|e| log_err(ctx, e))?;
    }
    rebuild_router(state, ctx).await
}

/// Switch a model endpoint role to another model id. Updates config.toml and
/// hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn switch_model(
    role: String,
    model_id: String,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();
    update_endpoint_field(&state, "switch_model", &role, |ep| {
        ep.model_name = model_id;
        Ok(())
    })
    .await
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

    update_endpoint_field(&state, "set_reasoning_effort", &role, |ep| {
        ep.reasoning_effort = normalized;
        Ok(())
    })
    .await
}

/// Set the provider built-in web search mode of a model endpoint role
/// ("off" | "auto" | "always"). "auto" lets the model decide when to search;
/// any other value (including empty) is rejected. Updates config.toml and
/// hot-swaps the LlmRouter at runtime.
#[tauri::command]
pub async fn set_web_search(
    role: String,
    mode: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let state = app.state::<Arc<AppState>>();

    let normalized = mode.as_deref().map(|m| m.trim().to_ascii_lowercase());
    match normalized.as_deref() {
        Some("off") | Some("auto") | Some("always") | None => {}
        _ => {
            return Err(format!(
                "invalid web search mode: {:?} (expected off|auto|always)",
                mode
            ));
        }
    }

    update_endpoint_field(&state, "set_web_search", &role, |ep| {
        ep.web_search = normalized;
        Ok(())
    })
    .await
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
    state
        .db
        .delete_fact(&fact_id)
        .map_err(|e| log_err("delete_fact", e))
}

#[tauri::command]
pub async fn get_preference(
    state: State<'_, Arc<AppState>>,
    key: String,
) -> Result<Option<String>, String> {
    state
        .db
        .get_preference(&key)
        .map_err(|e| log_err("get_preference", e))
}

#[tauri::command]
pub async fn list_preferences(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<(String, String)>, String> {
    state
        .db
        .list_preferences()
        .map_err(|e| log_err("list_preferences", e))
}

#[tauri::command]
pub async fn update_preference(
    state: State<'_, Arc<AppState>>,
    key: String,
    value: String,
) -> Result<(), String> {
    state
        .db
        .set_preference(&key, &value)
        .map_err(|e| log_err("update_preference", e))
}

#[tauri::command]
pub async fn delete_preference(state: State<'_, Arc<AppState>>, key: String) -> Result<(), String> {
    state
        .db
        .delete_preference(&key)
        .map_err(|e| log_err("delete_preference", e))
}

#[tauri::command]
pub async fn get_settings(app: tauri::AppHandle) -> Result<haven_common::config::Settings, String> {
    let state = app.state::<Arc<AppState>>();
    let cfg = state
        .config_loader
        .lock()
        .map_err(|e| log_err("get_settings", e))?;
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
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_settings", e))?;
        cfg.config().hotkey.key_binding.clone()
    };

    {
        let mut loader = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_settings", e))?;
        loader.apply_settings(&settings);
        // The settings form does not manage MCP servers / skills / tool
        // settings: those are mutated by dedicated commands (add/update/
        // remove/toggle MCP, skill ops) that write config.toml directly via a
        // fresh `ConfigLoader::load()`, leaving the shared in-memory loader
        // stale. Restore the authoritative on-disk copies before saving so a
        // settings save can never wipe configured servers/skills.
        let disk = haven_common::config::ConfigLoader::load()
            .map_err(|e| log_err("update_settings", e))?;
        let cfg = loader.config_mut();
        cfg.mcp_servers = disk.config().mcp_servers.clone();
        cfg.mcp_discovery = disk.config().mcp_discovery.clone();
        cfg.skills = disk.config().skills.clone();
        cfg.skills_exec = disk.config().skills_exec.clone();
        cfg.tool_settings = disk.config().tool_settings.clone();
        loader.save().map_err(|e| log_err("update_settings", e))?;
    }

    // Propagate audio config to running pipeline
    state.pipeline.update_config(settings.audio).await;

    // Reload MCP servers from config
    let (mcp_servers, mcp_discovery, task_max_steps, llm_config, min_risk_level) = {
        let cfg = state
            .config_loader
            .lock()
            .map_err(|e| log_err("update_settings", e))?;
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
pub async fn enable_autostart() -> Result<(), String> {
    // Debug builds load from devUrl (localhost:4721) — autostart would
    // launch the binary without the Vite dev server, showing a blank/
    // connection-error page.  Only release builds embed the frontend
    // and can be safely autostarted.
    if cfg!(debug_assertions) {
        return Err("自动启动仅支持生产版本（cargo tauri build）。开发模式下请手动运行。".into());
    }
    crate::autostart::enable()
}

#[tauri::command]
pub async fn disable_autostart() -> Result<(), String> {
    crate::autostart::disable()
}

#[tauri::command]
pub async fn is_autostart_enabled() -> Result<bool, String> {
    crate::autostart::is_enabled()
}

#[derive(Serialize)]
pub struct TaskReviewResponse {
    pub task: Task,
    pub messages: Vec<Message>,
    pub steps: Vec<TaskStep>,
    /// Persisted cumulative token/cost counters for the task, so a resumed
    /// or auto-restored conversation can restore the token-stats display.
    /// When the task predates usage persistence (no `task_usage` row) this
    /// falls back to a rough estimate derived from the persisted message
    /// and step text, flagged by `usage_estimated`.
    pub usage: Option<haven_memory::repositories::usage::TaskUsage>,
    /// True when `usage` is an estimate (task created before per-task usage
    /// counters were persisted) rather than the real recorded totals.
    pub usage_estimated: bool,
}

/// Rough token-count estimate for tasks that predate usage persistence.
/// Counts CJK characters as ~1 token and other characters as ~1/4 token
/// across persisted messages and tool steps, adds a flat prompt/tool
/// definition overhead, and charges 800 tokens per image attachment.
/// Cost is unknown, so `has_cost` stays false. Estimates are computed on
/// read and never written to `task_usage`, so a resumed conversation's
/// real counters can never be contaminated by them.
fn estimate_task_usage(
    messages: &[Message],
    steps: &[TaskStep],
) -> haven_memory::repositories::usage::TaskUsage {
    use haven_memory::repositories::usage::TaskUsage;

    fn estimate_text(text: &str) -> u32 {
        let mut cjk: u32 = 0;
        let mut other: u32 = 0;
        for ch in text.chars() {
            let cp = ch as u32;
            if (0x4E00..=0x9FFF).contains(&cp)
                || (0x3000..=0x303F).contains(&cp)
                || (0x3040..=0x30FF).contains(&cp)
            {
                cjk += 1;
            } else {
                other += 1;
            }
        }
        cjk + other / 4
    }

    let mut total: u32 = 0;
    for m in messages {
        total += estimate_text(&m.content);
        for att in &m.attachments {
            if att.media_type.starts_with("image/") {
                total += 800;
            } else {
                total += 300;
            }
        }
    }
    for s in steps {
        if let Some(t) = &s.thought {
            total += estimate_text(t);
        }
        if let Some(i) = &s.action_input {
            total += estimate_text(i);
        }
        if let Some(o) = &s.observation {
            total += estimate_text(o);
        }
    }
    // Fixed system prompt + tool definition overhead (~6.5K chars prompt).
    total += 3000;
    let prompt = total * 2 / 3;
    let completion = total - prompt;
    TaskUsage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        cost_usd: 0.0,
        has_cost: false,
    }
}

/// Load the task's messages and steps into a review response.
/// Shared by `get_task_for_review` and `get_last_conversation`.
fn review_response_for_task(
    db: &haven_memory::Database,
    task: Task,
) -> Result<TaskReviewResponse, String> {
    let messages = db
        .get_task_messages(&task.id)
        .map_err(|e| log_err("review_response_for_task", e))?;
    let steps = db
        .get_task_steps(&task.id)
        .map_err(|e| log_err("review_response_for_task", e))?;
    let (usage, usage_estimated) = match db
        .get_task_usage(&task.id)
        .map_err(|e| log_err("review_response_for_task", e))?
    {
        Some(u) => (Some(u), false),
        None => (Some(estimate_task_usage(&messages, &steps)), true),
    };
    Ok(TaskReviewResponse {
        task,
        messages,
        steps,
        usage,
        usage_estimated,
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
    let tasks = state
        .db
        .list_tasks(1, 0)
        .map_err(|e| log_err("get_last_conversation", e))?;
    match tasks.into_iter().next() {
        Some(task) => review_response_for_task(&state.db, task).map(Some),
        None => Ok(None),
    }
}

/// Roll back a task to a specific branch point. The task is rewound to
/// the saved state at that step. When `pause` is true the task is set to
/// Paused (user wants to edit the message before re-sending); otherwise it
/// is set to Pending for immediate re-execution. `target_message_id` is the
/// id of the exact message being rolled back; it lets the backend detect an
/// orphan rollback (a user message that was never processed into the
/// ReAct context).
#[tauri::command]
pub async fn rollback_task(
    state: State<'_, Arc<AppState>>,
    task_id: String,
    target_step: u32,
    pause: Option<bool>,
    target_message_id: Option<String>,
) -> Result<(), String> {
    state
        .agent
        .rollback_task(
            &task_id,
            target_step,
            pause.unwrap_or(false),
            target_message_id.as_deref(),
        )
        .await
        .map_err(|e| log_err("rollback_task", e))
}

/// Resume a task that errored mid-step. Removes partial output persisted on
/// error and sets the task to Pending so the dispatcher retries the failed
/// step from the saved snapshot.
#[tauri::command]
pub async fn continue_task(state: State<'_, Arc<AppState>>, task_id: String) -> Result<(), String> {
    state
        .agent
        .continue_task(&task_id)
        .await
        .map_err(|e| log_err("continue_task", e))
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
        haven_memory::repositories::messages::MessageAttachment::new(media_type, data)
    }

    fn limits() -> haven_common::config::ContextLimitsConfig {
        haven_common::config::ContextLimitsConfig::default()
    }

    #[test]
    fn test_validate_attachments_accepts_valid() {
        let imgs = vec![att("image/png", "aGVsbG8="), att("image/jpeg", "YWJj")];
        let out = validate_attachments(imgs.clone(), &limits()).unwrap();
        assert_eq!(out.len(), 2);
        // Files need a name; with one attached they pass through fine.
        let mut file = att("application/pdf", "aGVsbG8=");
        file.filename = Some("report.pdf".into());
        let out = validate_attachments(vec![file], &limits()).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_validate_attachments_rejects_images_over_count() {
        let imgs: Vec<_> = (0..5).map(|_| att("image/png", "aGVsbG8=")).collect();
        let err = validate_attachments(imgs, &limits()).unwrap_err();
        assert!(err.contains("最多支持"));
    }

    #[test]
    fn test_validate_attachments_rejects_files_over_count() {
        let mut files: Vec<_> = (0..6)
            .map(|i| {
                let mut a = att("application/octet-stream", "aGVsbG8=");
                a.filename = Some(format!("f{i}.bin"));
                a
            })
            .collect();
        files.push(att("image/png", "aGVsbG8="));
        let err = validate_attachments(files, &limits()).unwrap_err();
        assert!(err.contains("最多支持 5 个文件"));
    }

    #[test]
    fn test_validate_attachments_requires_filename_for_files() {
        let imgs = vec![att("application/x-msdownload", "aGVsbG8=")];
        let err = validate_attachments(imgs, &limits()).unwrap_err();
        assert!(err.contains("文件名"));
    }

    #[test]
    fn test_validate_attachments_rejects_oversized_image() {
        let big = "A".repeat(15 * 1024 * 1024);
        let imgs = vec![att("image/png", &big)];
        let err = validate_attachments(imgs, &limits()).unwrap_err();
        assert!(err.contains("10MB"));
    }

    #[test]
    fn test_validate_attachments_rejects_oversized_file() {
        let mut file = att("application/zip", &"A".repeat(28 * 1024 * 1024));
        file.filename = Some("big.zip".into());
        let err = validate_attachments(vec![file], &limits()).unwrap_err();
        assert!(err.contains("20MB"));
    }

    #[test]
    fn test_validate_attachments_rejects_invalid_base64() {
        let imgs = vec![att("image/png", "not-base64!!!")];
        let err = validate_attachments(imgs, &limits()).unwrap_err();
        assert!(err.contains("base64"));
    }

    #[tokio::test]
    async fn test_persist_file_attachments_writes_disk_and_clears_data() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut file = att(
            "application/pdf",
            &base64::engine::general_purpose::STANDARD.encode(b"hello pdf"),
        );
        file.filename = Some("报告.pdf".into());
        let img = att("image/png", "aGVsbG8=");

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![file, img])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);

        let saved = out.iter().find(|a| !a.is_image()).unwrap();
        assert!(
            saved.data.is_empty(),
            "file bytes must not be kept in the message"
        );
        let path = saved.path.as_ref().unwrap();
        assert!(
            path.ends_with("报告.pdf") || path.contains("报告"),
            "keeps the original name"
        );
        let on_disk = std::fs::read(path).unwrap();
        assert_eq!(on_disk, b"hello pdf");

        let image = out.iter().find(|a| a.is_image()).unwrap();
        assert_eq!(image.data, "aGVsbG8=", "images keep their base64 payload");
        assert!(image.path.is_none());
    }

    #[tokio::test]
    async fn test_persist_file_attachments_dedupes_collisions() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut a = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"one"),
        );
        a.filename = Some("same.txt".into());
        let mut b = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"two"),
        );
        b.filename = Some("same.txt".into());

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![a, b])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        let paths: Vec<_> = out.iter().map(|f| f.path.as_deref().unwrap()).collect();
        assert_ne!(paths[0], paths[1], "colliding names must not overwrite");
        assert!(
            paths[0].ends_with("same.txt") && paths[1].ends_with("same_2.txt")
                || paths[1].ends_with("same.txt") && paths[0].ends_with("same_2.txt")
        );
    }

    #[test]
    fn test_sanitize_filename_blocks_path_traversal() {
        assert_eq!(sanitize_filename("a/b\\c:d"), "a_b_c_d");
        let traversal = sanitize_filename("..");
        assert_ne!(traversal, "..");
        assert!(!traversal.contains('/') && !traversal.contains('\\'));
        let named = sanitize_filename("a");
        assert_eq!(named, "a");
    }

    #[test]
    fn test_estimate_task_usage_has_no_cost() {
        let msg = Message {
            id: "m1".into(),
            task_id: "t1".into(),
            role: "user".into(),
            content: "你好 world".into(),
            message_type: None,
            created_at: String::new(),
            tool_call_id: None,
            parent_message_id: None,
            attachments: vec![att("image/png", "aGVsbG8=")],
            voice: false,
        };
        let step = TaskStep {
            id: "s1".into(),
            task_id: "t1".into(),
            step_index: 0,
            thought: Some("查找文件".into()),
            action_tool: Some("file".into()),
            action_input: Some("{\"path\":\"C:/tmp\"}".into()),
            observation: Some("found 3 files".into()),
            status: "completed".into(),
            is_high_risk: false,
            confirmed: Some(true),
            silent: false,
            started_at: None,
            completed_at: None,
            created_at: String::new(),
        };
        let u = estimate_task_usage(&[msg], &[step]);
        assert!(!u.has_cost);
        assert_eq!(u.cost_usd, 0.0);
        // CJK chars count 1 token, latin chars count 1/4, image +800,
        // plus the 3000 flat prompt overhead.
        assert_eq!(u.prompt_tokens + u.completion_tokens, u.total_tokens);
        assert!(u.total_tokens > 3000);
    }

    #[test]
    fn test_estimate_task_usage_cjk_weighting() {
        let cjk = Message {
            id: "m1".into(),
            task_id: "t1".into(),
            role: "user".into(),
            content: "你好世界".into(),
            message_type: None,
            created_at: String::new(),
            tool_call_id: None,
            parent_message_id: None,
            attachments: vec![],
            voice: false,
        };
        let latin = Message {
            id: "m2".into(),
            task_id: "t1".into(),
            role: "user".into(),
            content: "hello".into(),
            message_type: None,
            created_at: String::new(),
            tool_call_id: None,
            parent_message_id: None,
            attachments: vec![],
            voice: false,
        };
        let u1 = estimate_task_usage(&[cjk], &[]);
        let u2 = estimate_task_usage(&[latin], &[]);
        // 4 CJK chars = 4 tokens; 5 latin chars = 1 token.
        assert_eq!(u1.total_tokens - u2.total_tokens, 3);
    }
}
