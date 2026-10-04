use crate::app_state::AppState;
use crate::commands::{emit_event_logged, log_err, log_storage_err};
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
    let shell_state = state.runtime.shell.get_state().await;
    Ok(RecordingState {
        is_recording: shell_state.is_recording,
        is_toggle: shell_state.is_recording_toggle,
    })
}

/// Temporarily suppress the global recording shortcut while the renderer
/// captures a new key binding. This state is in-memory and is cleared when
/// the capture field stops listening.
#[tauri::command]
pub fn set_hotkey_capture_active(
    active: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    state
        .hotkey_capture_active
        .store(active, std::sync::atomic::Ordering::Release);
    Ok(())
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
        .runtime
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
    if let Err(e) = state.runtime.pipeline.start_recording().await {
        // The hotkey may have started a recording a moment earlier, or a VAD
        // auto-stop may be finalizing: the pipeline is busy, not broken.
        let pipeline_state = state.runtime.pipeline.get_state().await;
        if matches!(pipeline_state, haven_input::RecordingState::Recording) {
            state.runtime.shell.sync_recording(true).await;
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
    state.runtime.shell.sync_recording(true).await;
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
    let result = match state.runtime.pipeline.stop_capture().await {
        Ok(result) => result,
        Err(e) => {
            // Another path (VAD auto-stop, mute, double click) already owns
            // the stop: the pipeline is Pending (finished) or Processing
            // (finalizing elsewhere). Not an error for the UI — emitting a
            // failure toast here would blame the user for a race they won.
            let pipeline_state = state.runtime.pipeline.get_state().await;
            if matches!(
                pipeline_state,
                haven_input::RecordingState::Pending | haven_input::RecordingState::Processing
            ) {
                state.runtime.shell.sync_recording(false).await;
                return Ok(String::new());
            }
            return Err(log_err("stop_recording", e));
        }
    };
    // Keep the shell state in sync (tray icon, mute hotkey, toggle).
    state.runtime.shell.sync_recording(false).await;
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
        .runtime
        .pipeline
        .cancel_recording()
        .await
        .map_err(|e| log_err("cancel_recording", e))?;
    state.runtime.shell.sync_recording(false).await;
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
        .runtime
        .config_service
        .snapshot()
        .map_err(|e| log_err("process_transcript", e))?
        .config
        .context_limits
        .clone();
    let attachments = validate_attachments(attachments.unwrap_or_default(), &limits)
        .map_err(|e| log_err("process_transcript", e))?;
    let assets = state.runtime.tools.share_services().assets;
    let attachments = crate::commands::managed_media::persist_file_attachments(
        attachments,
        limits.max_upload_total_bytes,
        assets,
        active_session_id.clone(),
    )
    .await
    .map_err(|e| log_err("process_transcript", e))?;
    let voice = voice.unwrap_or(false);
    tracing::debug!(
        transcript_chars = transcript.chars().count(),
        active_session_id = active_session_id.as_deref().unwrap_or("new"),
        attachments = attachments.len(),
        voice,
        "process_transcript called"
    );
    let result = match state
        .runtime
        .agent
        .process_input_with_attachments(&transcript, active_session_id.clone(), &attachments, voice)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            if active_session_id.is_none() {
                state
                    .runtime
                    .tools
                    .release_pending_managed_assets(&attachments);
            }
            return Err(log_storage_err("process_transcript", error));
        }
    };
    match (&result, active_session_id.as_deref()) {
        (haven_agent::ProcessResult::SessionCreated { session_id, .. }, Some(previous_id)) => {
            state.runtime.tools.transfer_managed_assets_to_session(
                previous_id,
                session_id,
                &attachments,
            );
        }
        (haven_agent::ProcessResult::SessionCreated { session_id, .. }, None) => state
            .runtime
            .tools
            .bind_pending_managed_assets_to_session(session_id, &attachments),
        (haven_agent::ProcessResult::Supplemented { .. }, Some(_)) => {}
        (haven_agent::ProcessResult::Supplemented { .. }, None) => {
            state
                .runtime
                .tools
                .release_pending_managed_assets(&attachments);
        }
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
                .runtime
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
    match &result {
        haven_agent::ProcessResult::SessionCreated { session_id, .. } => {
            tracing::debug!(
                session_id,
                outcome = "session_created",
                "process_transcript completed"
            );
        }
        haven_agent::ProcessResult::Supplemented { .. } => {
            tracing::debug!(outcome = "supplemented", "process_transcript completed");
        }
    }
    Ok(result)
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
}
