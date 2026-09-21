use crate::app_state::AppState;
use crate::commands::{emit_event_logged, log_err};
use crate::events::{
    RECORDING_ERROR_EVENT, RECORDING_STARTED_EVENT, RECORDING_STOPPED_EVENT, RecordingErrorEvent,
    RecordingEvent, TRANSCRIPTION_ERROR_EVENT, TRANSCRIPTION_RESULT_EVENT,
    TRANSCRIPTION_STARTED_EVENT, TranscriptionErrorEvent, TranscriptionResultEvent,
    TranscriptionStartedEvent,
};
use haven_common::error::sanitize_error_text;
use haven_input::{
    RecordingReason, RecordingResult, capture::TARGET_SAMPLE_RATE, encode_wav_to_vec,
};
use haven_tools::MediaTranscriptionStatus;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[path = "attachment_ingress.rs"]
mod attachment_ingress;
use attachment_ingress::validate_attachments;

#[derive(Serialize)]
pub struct RecordingState {
    pub is_recording: bool,
    pub is_toggle: bool,
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

pub(crate) fn recording_reason_str(reason: RecordingReason) -> &'static str {
    match reason {
        RecordingReason::Manual => "manual",
        RecordingReason::Silence => "silence",
        RecordingReason::MaxDuration => "max_duration",
        RecordingReason::Cancel => "cancel",
    }
}

/// The `rec-{uuid}` session id of the in-flight recording: created on the
/// first start (button or hotkey), reused by every event of the same
/// recording until `finalize_transcription` consumes it. One recording =
/// one id, so `recording:started` and the later `transcription:*` events
/// correlate by id instead of by timing.
pub(crate) fn begin_recording_session(state: &AppState) -> haven_common::types::SessionId {
    let mut cur = state
        .recording_session
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let id = cur.get_or_insert_with(|| haven_common::types::new_id("rec").into());
    id.clone()
}

/// Emit `recording:started` with the session's id. Used by both the
/// `start_recording` Tauri command and the shell hotkey start path so the
/// wire shape stays consistent across entry points.
pub(crate) fn emit_recording_started(
    app: &tauri::AppHandle,
    session_id: &haven_common::types::SessionId,
) {
    emit_event_logged(
        app,
        RECORDING_STARTED_EVENT,
        RecordingEvent {
            is_recording: true,
            session_id: Some(session_id.clone()),
            reason: None,
            duration_ms: None,
        },
        "recording_started",
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
    emit_event_logged(
        app,
        RECORDING_STOPPED_EVENT,
        RecordingEvent {
            is_recording: false,
            session_id: None,
            reason: Some(reason.to_string()),
            duration_ms,
        },
        "recording_stopped",
    );
}

/// Emit `recording:error` with a freshly generated session id and the
/// user-facing error message.
pub(crate) fn emit_recording_error(app: &tauri::AppHandle, error: impl Into<String>) {
    let error = sanitize_error_text(&error.into());
    emit_event_logged(
        app,
        RECORDING_ERROR_EVENT,
        RecordingErrorEvent {
            session_id: haven_common::types::new_id("rec").into(),
            error,
        },
        "recording_error",
    );
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
/// currently open conversation (session) instead of always starting a new one.
pub(crate) async fn finalize_transcription(
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
    result: RecordingResult,
) -> Option<String> {
    // The session id of the recording that produced this transcription:
    // generated at start (recording:started) and consumed here, so both
    // event families of one recording share the same `rec-` id. The
    // fallback covers events that never went through a start (defensive).
    let session_id = state
        .recording_session
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .unwrap_or_else(|| haven_common::types::new_id("rec").into());

    // Tell the UI STT is about to run, before the (potentially slow)
    // network call, so it can show a "transcribing" hint right away.
    emit_event_logged(
        app,
        TRANSCRIPTION_STARTED_EVENT,
        TranscriptionStartedEvent {
            session_id: session_id.clone(),
        },
        "transcription_started",
    );

    if let Some(error) = result.capture_error {
        emit_event_logged(
            app,
            TRANSCRIPTION_ERROR_EVENT,
            TranscriptionErrorEvent {
                session_id,
                error: sanitize_error_text(&error),
            },
            "transcription_capture_error",
        );
        return None;
    }

    if result.pcm.is_empty() {
        emit_event_logged(
            app,
            TRANSCRIPTION_RESULT_EVENT,
            TranscriptionResultEvent {
                session_id,
                text: String::new(),
                duration_ms: result.duration_ms,
                confidence: None,
            },
            "transcription_empty_capture",
        );
        return None;
    }

    let wav = encode_wav_to_vec(&result.pcm, TARGET_SAMPLE_RATE, 1);
    let transcription = state
        .tools
        .transcribe_recording(&wav, tokio_util::sync::CancellationToken::new())
        .await;
    let llm_usage = transcription.llm_usage.clone();

    match transcription.status {
        MediaTranscriptionStatus::Succeeded => {
            let text = transcription
                .text
                .expect("successful transcription has text");
            if !llm_usage.is_empty() {
                let mut pending = state
                    .pending_recording_usage
                    .lock()
                    .unwrap_or_else(|p| p.into_inner());
                // The input pipeline permits only one active recording, but
                // transcript submission is asynchronous. Keep a small
                // bounded map so a delayed renderer cannot mix two `rec-*`
                // results while also preventing a stale client from growing
                // memory without limit.
                if pending.len() >= 8
                    && let Some(oldest) = pending.keys().next().cloned()
                {
                    pending.remove(&oldest);
                    tracing::warn!(
                        recording_id = %oldest,
                        "dropping stale pending recording usage"
                    );
                }
                pending.insert(session_id.to_string(), llm_usage);
            }
            emit_event_logged(
                app,
                TRANSCRIPTION_RESULT_EVENT,
                TranscriptionResultEvent {
                    session_id: session_id.clone(),
                    text: text.clone(),
                    duration_ms: result.duration_ms,
                    confidence: None,
                },
                "transcription_result",
            );
            Some(text)
        }
        MediaTranscriptionStatus::Empty => {
            state
                .pending_recording_usage
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(session_id.as_str());
            // There is nothing to submit, but the UI still needs the
            // "transcribing" overlay closed. The frontend treats an empty
            // `transcription:result` as "close, add no message".
            emit_event_logged(
                app,
                TRANSCRIPTION_RESULT_EVENT,
                TranscriptionResultEvent {
                    session_id: session_id.clone(),
                    text: String::new(),
                    duration_ms: result.duration_ms,
                    confidence: None,
                },
                "transcription_empty_result",
            );
            None
        }
        MediaTranscriptionStatus::Unavailable
        | MediaTranscriptionStatus::Failed
        | MediaTranscriptionStatus::TimedOut
        | MediaTranscriptionStatus::Cancelled => {
            state
                .pending_recording_usage
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(session_id.as_str());
            emit_event_logged(
                app,
                TRANSCRIPTION_ERROR_EVENT,
                TranscriptionErrorEvent {
                    session_id,
                    error: sanitize_error_text(
                        &transcription
                            .error
                            .unwrap_or_else(|| "语音转写失败".to_string()),
                    ),
                },
                "transcription_error",
            );
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
            let session_id = begin_recording_session(&state);
            emit_recording_started(&app, &session_id);
            return Ok(());
        }
        let msg = if matches!(pipeline_state, haven_input::RecordingState::Processing) {
            "正在处理上一条录音，请稍候再试".to_string()
        } else {
            format!("录音启动失败，请检查麦克风配置: {e}")
        };
        emit_recording_error(&app, msg.clone());
        return Err(log_err("start_recording", msg));
    }
    // Keep the shell state in sync so the tray icon, the mute hotkey and the
    // recording toggle reflect a UI-button-started recording.
    state.shell.sync_recording(true).await;
    let session_id = begin_recording_session(&state);
    emit_recording_started(&app, &session_id);
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
    // drives the rest of the UI through `transcription:*` / `session:*`
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
    // transcript via `process_transcript` (same path as typed input). It is
    // spawned detached so the invoke returns immediately — the UI is driven
    // by the `transcription:*` events, not by this command's result, and
    // awaiting the STT network call here would hold the command action for its
    // whole duration.
    let state = state.inner().clone();
    let runtime = state.runtime.clone();
    runtime.spawn("recording-transcription", async move {
        let _ = finalize_transcription(&state, &app, result).await;
    });
    Ok(String::new())
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
    // No transcription follows a cancel: drop the session id so the next
    // recording starts a fresh one instead of reusing the cancelled id.
    let cancelled_recording_id = state
        .recording_session
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    if let Some(recording_id) = cancelled_recording_id {
        state
            .pending_recording_usage
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(recording_id.as_str());
    }
    emit_recording_stopped(&app, "cancel", None);
    Ok(())
}

#[tauri::command]
pub async fn process_transcript(
    state: State<'_, Arc<AppState>>,
    transcript: String,
    active_session_id: Option<String>,
    attachments: Option<Vec<haven_common::types::MessageAttachment>>,
    voice: Option<bool>,
    recording_session_id: Option<String>,
) -> Result<haven_agent::ProcessResult, String> {
    let limits = state
        .config_service
        .snapshot()
        .map_err(|e| log_err("process_transcript", e))?
        .config
        .context_limits
        .clone();
    let attachments = validate_attachments(attachments.unwrap_or_default(), &limits)
        .map_err(|e| log_err("process_transcript", e))?;
    let attachments = persist_file_attachments(attachments, limits.max_upload_total_bytes)
        .await
        .map_err(|e| log_err("process_transcript", e))?;
    if let Some(session_id) = active_session_id.as_deref() {
        state
            .tools
            .register_managed_assets_for_session(session_id, &attachments);
    } else {
        // The new session id is allocated only after ingress persists the
        // first message. Keep the registry entry protected across that small
        // pre-session window, then bind it to the returned session lease.
        state.tools.register_managed_assets(&attachments);
    }
    let voice = voice.unwrap_or(false);
    tracing::debug!(
        "process_transcript called: text={:?} active_session_id={:?} attachments={} voice={}",
        transcript,
        active_session_id,
        attachments.len(),
        voice
    );
    let result = match state
        .agent
        .process_input_with_attachments(&transcript, active_session_id.clone(), &attachments, voice)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            if active_session_id.is_none() {
                state.tools.release_pending_managed_assets(&attachments);
            }
            return Err(log_err("process_transcript", error));
        }
    };
    if active_session_id.is_none()
        && let haven_agent::ProcessResult::SessionCreated { session_id, .. } = &result
    {
        state
            .tools
            .bind_pending_managed_assets_to_session(session_id, &attachments);
    }
    let pending_recording_usage = recording_session_id.as_deref().and_then(|id| {
        state
            .pending_recording_usage
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(id)
    });
    if let Some(usages) = pending_recording_usage {
        // A stale active id can be replaced by ingress when the old session
        // disappears, so prefer the actual SessionCreated owner when one is
        // returned. The recording's `rec-*` id never becomes a DB session id.
        let target_session_id = match &result {
            haven_agent::ProcessResult::SessionCreated { session_id, .. } => {
                Some(session_id.as_str())
            }
            _ => active_session_id.as_deref(),
        };
        if let Some(target_session_id) = target_session_id {
            state
                .agent
                .record_media_usage(target_session_id, &usages)
                .await;
        } else {
            tracing::warn!(
                recording_session_id,
                "discarding transcription usage without a target session"
            );
        }
    }
    tracing::debug!("process_transcript result: {:?}", result);
    Ok(result)
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
        clean = haven_common::types::new_id("file");
    }
    // `String::truncate` panics when the index is not a char boundary; pop
    // whole chars instead (CJK/emoji names are common).
    while clean.len() > 120 {
        clean.pop();
    }
    clean
}

/// Write ordinary file attachments to disk under `uploads/<batch>/` and return
/// them with `path` set. `data` is cleared afterwards — the bytes live on
/// disk, keeping the persisted message and DB storage slim. Inline image and
/// audio media pass through because the active chat model consumes their
/// base64 payload directly.
async fn persist_file_attachments(
    attachments: Vec<haven_common::types::MessageAttachment>,
    max_total_bytes: u64,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    persist_file_attachments_to_with_limit(uploads_root(), attachments, max_total_bytes).await
}

/// Remove only generated upload batches/staging directories that have
/// outlived the session history retention window. This is a host-maintenance
/// operation, never a model-facing file operation: the target is constrained
/// to the dedicated uploads root and the `file-{uuid32}` naming contract.
#[cfg(test)]
pub(crate) async fn cleanup_stale_upload_batches(
    root: std::path::PathBuf,
    max_age: std::time::Duration,
) -> Result<usize, String> {
    cleanup_stale_upload_batches_with_registry(
        root,
        max_age,
        haven_tools::ManagedAssetRegistry::default(),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn cleanup_stale_upload_batches_with_registry(
    root: std::path::PathBuf,
    max_age: std::time::Duration,
    registry: haven_tools::ManagedAssetRegistry,
) -> Result<usize, String> {
    cleanup_stale_upload_batches_with_references(root, max_age, registry, None).await
}

pub(crate) async fn cleanup_stale_upload_batches_with_references(
    root: std::path::PathBuf,
    max_age: std::time::Duration,
    registry: haven_tools::ManagedAssetRegistry,
    referenced_paths: Option<Vec<std::path::PathBuf>>,
) -> Result<usize, String> {
    let _write_guard = upload_write_lock().lock().await;
    tokio::task::spawn_blocking(move || {
        cleanup_stale_upload_batches_sync(&root, max_age, &registry, referenced_paths.as_deref())
    })
    .await
    .map_err(|error| format!("上传目录清理任务失败: {error}"))?
}

/// Staging directories are crash leftovers, not durable session history. They
/// therefore have their own short retention window and are cleaned even when
/// `history_retention_days` is disabled.
pub(crate) const UPLOAD_STAGING_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

pub(crate) async fn cleanup_stale_upload_staging(
    root: std::path::PathBuf,
) -> Result<usize, String> {
    cleanup_stale_upload_staging_with_age(root, UPLOAD_STAGING_MAX_AGE).await
}

async fn cleanup_stale_upload_staging_with_age(
    root: std::path::PathBuf,
    max_age: std::time::Duration,
) -> Result<usize, String> {
    let _write_guard = upload_write_lock().lock().await;
    tokio::task::spawn_blocking(move || cleanup_stale_upload_staging_sync(&root, max_age))
        .await
        .map_err(|error| format!("上传暂存目录清理任务失败: {error}"))?
}

static UPLOAD_WRITE_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

fn upload_write_lock() -> &'static tokio::sync::Mutex<()> {
    UPLOAD_WRITE_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn cleanup_stale_upload_batches_sync(
    root: &std::path::Path,
    max_age: std::time::Duration,
    registry: &haven_tools::ManagedAssetRegistry,
    referenced_paths: Option<&[std::path::PathBuf]>,
) -> Result<usize, String> {
    if let Some(referenced_paths) = referenced_paths {
        registry.prune_unreferenced(referenced_paths);
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            registry.prune_missing();
            return Ok(0);
        }
        Err(error) => return Err(format!("读取上传目录失败: {error}")),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(error = %error, "跳过不可读取的上传目录项");
                continue;
            }
        };
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(error = %error, "跳过无法判断类型的上传目录项");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_batch = is_generated_upload_batch(&name);
        let is_staging = is_generated_upload_staging(&name);
        if !metadata.is_dir() || is_link_or_reparse(&metadata) || (!is_batch && !is_staging) {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                tracing::debug!(batch = %name, error = %error, "跳过没有修改时间的上传批次");
                continue;
            }
        };
        if modified.elapsed().map_or(true, |age| age <= max_age) {
            continue;
        }
        if is_batch
            && registry
                .protected_paths()
                .iter()
                .any(|path| path_is_equal_or_child(&entry.path(), path))
        {
            tracing::debug!(batch = %name, "保留仍被活动会话引用的上传批次");
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => {
                removed += 1;
                registry.prune_paths_under(&entry.path());
            }
            Err(error) => tracing::debug!(batch = %name, error = %error, "上传批次清理失败"),
        }
    }
    registry.prune_missing();
    Ok(removed)
}

fn cleanup_stale_upload_staging_sync(
    root: &std::path::Path,
    max_age: std::time::Duration,
) -> Result<usize, String> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取上传暂存目录失败: {error}")),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(error = %error, "跳过不可读取的上传暂存目录项");
                continue;
            }
        };
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(error = %error, "跳过无法判断类型的上传暂存目录项");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !metadata.is_dir()
            || is_link_or_reparse(&metadata)
            || !is_generated_upload_staging(&name)
        {
            continue;
        }
        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(error) => {
                tracing::debug!(staging = %name, error = %error, "跳过没有修改时间的上传暂存目录");
                continue;
            }
        };
        if modified.elapsed().map_or(true, |age| age <= max_age) {
            continue;
        }
        match std::fs::remove_dir_all(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) => {
                tracing::debug!(staging = %name, error = %error, "上传暂存目录清理失败")
            }
        }
    }
    Ok(removed)
}

/// Remove expired generated-media files from the dedicated generated root.
/// Active session leases override expiry so a running session can finish using
/// its generated attachment; durable history alone does not extend this
/// artifact's independent lifetime.
pub(crate) async fn cleanup_stale_generated_media(
    root: std::path::PathBuf,
    registry: haven_tools::ManagedAssetRegistry,
) -> Result<usize, String> {
    let _write_guard = upload_write_lock().lock().await;
    tokio::task::spawn_blocking(move || cleanup_stale_generated_media_sync(&root, &registry))
        .await
        .map_err(|error| format!("生成媒体清理任务失败: {error}"))?
}

fn cleanup_stale_generated_media_sync(
    root: &std::path::Path,
    registry: &haven_tools::ManagedAssetRegistry,
) -> Result<usize, String> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取生成媒体根目录失败: {error}")),
    };
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Ok(0);
    }
    let expired_paths = registry.expired_paths();
    let leased_paths = registry.leased_paths();
    let fallback_max_age =
        std::time::Duration::from_secs(haven_common::config::GENERATED_MEDIA_RETENTION_SECS);
    let entries =
        std::fs::read_dir(root).map_err(|error| format!("读取生成媒体目录失败: {error}"))?;
    let mut removed = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(error = %error, "跳过不可读取的生成媒体目录项");
                continue;
            }
        };
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(error = %error, "跳过无法判断类型的生成媒体目录项");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !metadata.is_file() || is_link_or_reparse(&metadata) || !is_generated_media_file(&name) {
            continue;
        }
        if leased_paths
            .iter()
            .any(|candidate| path_is_equal(candidate, &path))
        {
            tracing::debug!(file = %name, "保留仍被活动会话引用的生成媒体");
            continue;
        }
        let expired = expired_paths
            .iter()
            .any(|candidate| path_is_equal(candidate, &path));
        let old_enough = metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age > fallback_max_age);
        if !expired && !old_enough {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(error) => tracing::debug!(file = %name, error = %error, "生成媒体清理失败"),
        }
    }
    registry.prune_missing();
    Ok(removed)
}

fn is_generated_media_file(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("file-") else {
        return false;
    };
    let Some((id, extension)) = suffix.split_once('.') else {
        return false;
    };
    id.len() == 32
        && id.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !extension.is_empty()
        && extension.len() <= 16
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn is_generated_upload_batch(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("file-") else {
        return false;
    };
    suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_generated_upload_staging(name: &str) -> bool {
    name.strip_prefix(".file-")
        .and_then(|value| value.strip_suffix(".tmp"))
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

#[cfg(test)]
async fn persist_file_attachments_to(
    root: std::path::PathBuf,
    attachments: Vec<haven_common::types::MessageAttachment>,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    persist_file_attachments_to_with_limit(root, attachments, DEFAULT_MAX_UPLOAD_TOTAL_BYTES).await
}

#[cfg(test)]
const DEFAULT_MAX_UPLOAD_TOTAL_BYTES: u64 = 512 * 1024 * 1024;

struct UploadBatchGuard {
    path: std::path::PathBuf,
    committed: bool,
}

impl Drop for UploadBatchGuard {
    fn drop(&mut self) {
        if !self.committed {
            // This also runs when an in-flight upload task is cancelled. The
            // staging directory is private to this operation, so a best-
            // effort synchronous cleanup is preferable to leaving bytes
            // behind until the next retention pass.
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

async fn persist_file_attachments_to_with_limit(
    root: std::path::PathBuf,
    attachments: Vec<haven_common::types::MessageAttachment>,
    max_total_bytes: u64,
) -> Result<Vec<haven_common::types::MessageAttachment>, String> {
    use base64::Engine as _;

    let mut files = Vec::new();
    for mut att in attachments {
        // Clear renderer-only metadata here as a second defense, not only in
        // the Tauri validation command.
        att.path = None;
        // Asset identity is host-owned; never allow the renderer to alias a
        // previously registered managed asset.
        if att.asset_id.is_none() {
            att.asset_id = Some(haven_common::types::new_id("asset"));
        }
        // All binary inputs now become managed assets.  Inline base64 is an
        // ingress-only transport representation; keeping it in messages and
        // snapshots made the same bytes live in multiple authorities.
        files.push(att);
    }
    if files.is_empty() {
        return Ok(Vec::new());
    }

    // Serialize quota accounting and staging-directory commits so concurrent
    // transcript submissions cannot each observe the same free capacity.
    let _write_guard = upload_write_lock().lock().await;

    let existing_bytes = tokio::task::spawn_blocking({
        let root = root.clone();
        move || upload_tree_size(&root)
    })
    .await
    .map_err(|error| format!("计算上传目录容量失败: {error}"))??;
    let batch_id = haven_common::types::new_id("file");
    let staging_dir = root.join(format!(".{batch_id}.tmp"));
    let batch_dir = root.join(&batch_id);
    tokio::fs::create_dir_all(&root)
        .await
        .map_err(|e| format!("创建上传目录失败: {e}"))?;
    let root_metadata = tokio::fs::symlink_metadata(&root)
        .await
        .map_err(|e| format!("读取上传目录元数据失败: {e}"))?;
    if !root_metadata.is_dir() || is_link_or_reparse(&root_metadata) {
        return Err("上传目录不能是符号链接或重解析点".to_string());
    }
    tokio::fs::create_dir(&staging_dir)
        .await
        .map_err(|e| format!("创建上传临时目录失败: {e}"))?;
    let mut guard = UploadBatchGuard {
        path: staging_dir.clone(),
        committed: false,
    };

    let mut used_names = std::collections::HashSet::new();
    let mut persisted = Vec::with_capacity(files.len());
    let mut total_bytes = existing_bytes;
    for mut att in files {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&att.data)
            .map_err(|_| "附件数据不是有效的 base64".to_string())?;
        let decoded_len = bytes.len() as u64;
        if total_bytes.saturating_add(decoded_len) > max_total_bytes {
            return Err(format!(
                "上传目录超过 {}MB 总容量上限",
                max_total_bytes / 1024 / 1024
            ));
        }
        total_bytes = total_bytes.saturating_add(decoded_len);
        let base_name = att
            .filename
            .as_deref()
            .map(sanitize_filename)
            .unwrap_or_else(|| haven_common::types::new_id("file"));
        // Keep the extension for readability but dedupe collisions so two
        // same-named uploads in one batch never overwrite each other.
        let mut name = base_name.clone();
        let mut n = 2;
        while !used_names.insert(filename_collision_key(&name)) {
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
        let file_path = staging_dir.join(&name);
        tokio::fs::write(&file_path, bytes)
            .await
            .map_err(|e| format!("保存附件失败: {e}"))?;
        att.path = Some(batch_dir.join(&name).to_string_lossy().into_owned());
        // Keep the decoded transport data in the returned in-memory value so
        // the ReAct media projection can still reference the just-persisted
        // asset while the model-facing media tool performs OCR/STT.
        // `messages.ui_metadata` strips it at the DB boundary and snapshots
        // use `MediaInput::for_snapshot`, so this is not a second durable
        // authority.
        persisted.push(att);
    }
    tokio::fs::rename(&staging_dir, &batch_dir)
        .await
        .map_err(|e| format!("提交上传批次失败: {e}"))?;
    guard.committed = true;
    Ok(persisted)
}

fn filename_collision_key(name: &str) -> String {
    #[cfg(windows)]
    {
        name.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        name.to_string()
    }
}

fn upload_tree_size(path: &std::path::Path) -> Result<u64, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("读取上传目录元数据失败: {error}")),
    };
    if is_link_or_reparse(&metadata) {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0u64;
    for entry in std::fs::read_dir(path).map_err(|error| format!("读取上传目录失败: {error}"))?
    {
        let entry = entry.map_err(|error| format!("读取上传目录项失败: {error}"))?;
        total = total.saturating_add(upload_tree_size(&entry.path())?);
    }
    Ok(total)
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn path_is_equal_or_child(root: &std::path::Path, candidate: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        let root = root.to_string_lossy().to_lowercase();
        let candidate = candidate.to_string_lossy().to_lowercase();
        candidate == root
            || candidate.starts_with(&format!("{root}\\"))
            || candidate.starts_with(&format!("{root}/"))
    }
    #[cfg(not(windows))]
    {
        candidate == root || candidate.strip_prefix(root).is_ok()
    }
}

fn path_is_equal(left: &std::path::Path, right: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn att(media_type: &str, data: &str) -> haven_common::types::MessageAttachment {
        haven_common::types::MessageAttachment::new(media_type, data)
    }

    fn limits() -> haven_common::config::ContextLimitsConfig {
        haven_common::config::ContextLimitsConfig::default()
    }

    #[test]
    fn test_validate_attachments_accepts_valid() {
        let imgs = vec![att("image/png", "iVBORw0KGgo="), att("image/jpeg", "/9j/")];
        let out = validate_attachments(imgs.clone(), &limits()).unwrap();
        assert_eq!(out.len(), 2);
        // Files need a name; with one attached they pass through fine.
        let mut file = att("application/pdf", "aGVsbG8=");
        file.filename = Some("report.pdf".into());
        let out = validate_attachments(vec![file], &limits()).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn test_validate_attachments_normalizes_extension_only_media() {
        let mut audio = att("application/octet-stream", "YXVkaW8=");
        audio.filename = Some("voice.aac".into());
        let out = validate_attachments(vec![audio], &limits()).unwrap();
        assert_eq!(out[0].media_type, "audio/aac");
        assert!(out[0].is_audio());
    }

    #[test]
    fn test_validate_attachments_rejects_images_over_count() {
        let imgs: Vec<_> = (0..5).map(|_| att("image/png", "iVBORw0KGgo=")).collect();
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

    #[test]
    fn test_validate_attachments_drops_renderer_managed_metadata() {
        let mut image = att("image/png", "iVBORw0KGgo=");
        image.asset_id = Some("asset-attacker-choice".into());
        image.path = Some(r"C:\Windows\win.ini".into());

        let out = validate_attachments(vec![image], &limits()).unwrap();

        assert!(out[0].asset_id.is_none());
        assert!(out[0].path.is_none());
    }

    #[test]
    fn test_generated_upload_batch_name_is_strict() {
        assert!(is_generated_upload_batch(
            "file-0123456789abcdef0123456789abcdef"
        ));
        assert!(!is_generated_upload_batch("file-user-created"));
        assert!(!is_generated_upload_batch(
            "file-0123456789abcdef0123456789abcdeg"
        ));
        assert!(!is_generated_upload_batch("uploads-file-0123456789abcdef"));
    }

    #[test]
    fn test_generated_media_file_name_is_strict() {
        assert!(is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef.png"
        ));
        assert!(!is_generated_media_file("file-user-created.png"));
        assert!(!is_generated_media_file(
            "file-0123456789abcdef0123456789abcdef.png.tmp"
        ));
        assert!(!is_generated_media_file(
            "file-0123456789abcdef0123456789abcdeg.png"
        ));
    }

    #[tokio::test]
    async fn test_cleanup_stale_upload_batches_only_removes_generated_dirs() {
        let root = tempfile::TempDir::new().unwrap();
        let stale = root.path().join("file-0123456789abcdef0123456789abcdef");
        let unrelated = root.path().join("file-user-created");
        tokio::fs::create_dir_all(&stale).await.unwrap();
        tokio::fs::create_dir_all(&unrelated).await.unwrap();
        let removed =
            cleanup_stale_upload_batches(root.path().to_path_buf(), std::time::Duration::ZERO)
                .await
                .unwrap();
        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(unrelated.exists());
    }

    #[tokio::test]
    async fn test_cleanup_stale_upload_staging_is_independent_from_batch_retention() {
        let root = tempfile::TempDir::new().unwrap();
        let stale = root
            .path()
            .join(".file-0123456789abcdef0123456789abcdef.tmp");
        let unrelated = root.path().join("file-user-created");
        tokio::fs::create_dir_all(&stale).await.unwrap();
        tokio::fs::create_dir_all(&unrelated).await.unwrap();

        let removed = cleanup_stale_upload_staging_with_age(
            root.path().to_path_buf(),
            std::time::Duration::ZERO,
        )
        .await
        .unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(unrelated.exists());
    }

    #[tokio::test]
    async fn test_generated_media_expiry_respects_active_session_lease() {
        let root = tempfile::TempDir::new().unwrap();
        let file = root
            .path()
            .join("file-0123456789abcdef0123456789abcdef.png");
        tokio::fs::write(&file, b"png-bytes").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root_for_session_with_metadata(
            "ses-active",
            root.path(),
            "asset-generated",
            file.clone(),
            Some("generated.png".into()),
            "image/png",
            Some("hash".into()),
            Some(9),
            Some(chrono::Utc::now() - chrono::Duration::seconds(1)),
        ));

        assert_eq!(
            cleanup_stale_generated_media(root.path().to_path_buf(), registry.clone())
                .await
                .unwrap(),
            0
        );
        assert!(file.exists());

        registry.release_session("ses-active");
        assert_eq!(
            cleanup_stale_generated_media(root.path().to_path_buf(), registry)
                .await
                .unwrap(),
            1
        );
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn test_cleanup_missing_upload_root_is_idempotent() {
        let root = tempfile::TempDir::new().unwrap();
        let missing = root.path().join("uploads");

        let first = cleanup_stale_upload_batches(missing.clone(), std::time::Duration::ZERO)
            .await
            .unwrap();
        let second = cleanup_stale_upload_batches(missing, std::time::Duration::ZERO)
            .await
            .unwrap();

        assert_eq!(first, 0);
        assert_eq!(second, 0);
    }

    #[tokio::test]
    async fn test_persist_file_attachments_writes_every_binary_asset() {
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
        assert!(saved.asset_id.as_deref().unwrap().starts_with("asset-"));
        assert_eq!(
            saved.data,
            base64::engine::general_purpose::STANDARD.encode(b"hello pdf")
        );
        let path = saved.path.as_ref().unwrap();
        assert!(
            path.ends_with("报告.pdf") || path.contains("报告"),
            "keeps the original name"
        );
        let on_disk = std::fs::read(path).unwrap();
        assert_eq!(on_disk, b"hello pdf");

        let image = out.iter().find(|a| a.is_image()).unwrap();
        assert!(image.asset_id.as_deref().unwrap().starts_with("asset-"));
        assert_eq!(image.data, "aGVsbG8=", "gateway keeps a transient payload");
        assert!(image.path.is_some());
    }

    #[tokio::test]
    async fn test_persist_file_attachments_manages_audio_assets() {
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut audio = att("audio/wav", "UklGRg==");
        audio.filename = Some("voice.wav".into());

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![audio])
            .await
            .unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].is_audio());
        assert!(out[0].asset_id.as_deref().unwrap().starts_with("asset-"));
        assert_eq!(out[0].data, "UklGRg==");
        assert!(out[0].path.is_some());
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

    #[tokio::test]
    async fn test_persist_file_attachments_rolls_back_failed_batch() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut valid = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"valid"),
        );
        valid.filename = Some("valid.txt".into());
        let mut invalid = att("text/plain", "not-base64");
        invalid.filename = Some("invalid.txt".into());

        let result =
            persist_file_attachments_to(tmp.path().to_path_buf(), vec![valid, invalid]).await;

        assert!(result.is_err());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn test_persist_file_attachments_enforces_total_upload_quota() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut file = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"12345"),
        );
        file.filename = Some("quota.txt".into());

        let result =
            persist_file_attachments_to_with_limit(tmp.path().to_path_buf(), vec![file], 4).await;

        assert!(result.unwrap_err().contains("总容量"));
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn test_cleanup_preserves_registered_asset_and_prunes_deleted_entry() {
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let file = batch.join("keep.txt");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(&file, "keep").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            root.path(),
            "asset-live",
            file.clone(),
            Some("keep.txt".into()),
            "text/plain",
        ));

        let removed = cleanup_stale_upload_batches_with_registry(
            root.path().to_path_buf(),
            std::time::Duration::ZERO,
            registry.clone(),
        )
        .await
        .unwrap();
        assert_eq!(removed, 0);
        assert!(file.exists());
        assert!(registry.contains("asset-live"));

        tokio::fs::remove_dir_all(&batch).await.unwrap();
        assert_eq!(registry.prune_missing(), 1);
        assert!(!registry.contains("asset-live"));
    }

    #[tokio::test]
    async fn test_cleanup_preserves_active_session_lease_without_message_reference() {
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let file = batch.join("active.txt");
        tokio::fs::create_dir_all(&batch).await.unwrap();
        tokio::fs::write(&file, "active").await.unwrap();
        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root_for_session(
            "ses-active",
            root.path(),
            "asset-live",
            file.clone(),
            Some("active.txt".into()),
            "text/plain",
        ));

        let removed = cleanup_stale_upload_batches_with_references(
            root.path().to_path_buf(),
            std::time::Duration::ZERO,
            registry.clone(),
            Some(Vec::new()),
        )
        .await
        .unwrap();
        assert_eq!(removed, 0);
        assert!(file.exists());

        registry.release_session("ses-active");
        let removed = cleanup_stale_upload_batches_with_references(
            root.path().to_path_buf(),
            std::time::Duration::ZERO,
            registry,
            Some(Vec::new()),
        )
        .await
        .unwrap();
        assert_eq!(removed, 1);
        assert!(!batch.exists());
    }

    #[tokio::test]
    async fn test_cleanup_removes_registered_assets_deleted_from_history() {
        use tempfile::TempDir;

        let root = TempDir::new().unwrap();
        let keep_batch = root.path().join("file-0123456789abcdef0123456789abcdef");
        let old_batch = root.path().join("file-fedcba9876543210fedcba9876543210");
        let keep_file = keep_batch.join("keep.txt");
        let old_file = old_batch.join("old.txt");
        tokio::fs::create_dir_all(&keep_batch).await.unwrap();
        tokio::fs::create_dir_all(&old_batch).await.unwrap();
        tokio::fs::write(&keep_file, "keep").await.unwrap();
        tokio::fs::write(&old_file, "old").await.unwrap();

        let registry = haven_tools::ManagedAssetRegistry::default();
        assert!(registry.register_under_root(
            root.path(),
            "asset-keep",
            keep_file.clone(),
            Some("keep.txt".into()),
            "text/plain",
        ));
        assert!(registry.register_under_root(
            root.path(),
            "asset-old",
            old_file,
            Some("old.txt".into()),
            "text/plain",
        ));

        let removed = cleanup_stale_upload_batches_with_references(
            root.path().to_path_buf(),
            std::time::Duration::ZERO,
            registry.clone(),
            Some(vec![keep_file]),
        )
        .await
        .unwrap();

        assert_eq!(removed, 1);
        assert!(keep_batch.exists());
        assert!(!old_batch.exists());
        assert!(registry.contains("asset-keep"));
        assert!(!registry.contains("asset-old"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_persist_file_attachments_dedupes_case_insensitive_windows_names() {
        use base64::Engine as _;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mut upper = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"upper"),
        );
        upper.filename = Some("A.txt".into());
        let mut lower = att(
            "text/plain",
            &base64::engine::general_purpose::STANDARD.encode(b"lower"),
        );
        lower.filename = Some("a.txt".into());

        let out = persist_file_attachments_to(tmp.path().to_path_buf(), vec![upper, lower])
            .await
            .unwrap();
        let names: Vec<_> = out
            .iter()
            .map(|attachment| {
                std::path::Path::new(attachment.path.as_deref().unwrap())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_lowercase()
            })
            .collect();
        assert!(names.contains(&"a.txt".into()));
        assert!(names.contains(&"a_2.txt".into()));
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
}
