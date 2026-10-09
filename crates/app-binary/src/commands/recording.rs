use crate::app_state::{AppState, RecordingLifecycleGuard};
use crate::commands::{emit_event_logged, log_err, log_storage_err};
use crate::desktop::{DesktopShell, RecordingStopContext};
use crate::events::{
    RECORDING_ERROR_EVENT, RECORDING_STARTED_EVENT, RECORDING_STOPPED_EVENT, RecordingErrorEvent,
    RecordingEvent, RecordingStopReasonDto, TRANSCRIPTION_ERROR_EVENT, TRANSCRIPTION_RESULT_EVENT,
    TRANSCRIPTION_STARTED_EVENT, TranscriptionErrorEvent, TranscriptionResultEvent,
    TranscriptionStartedEvent,
};
use haven_common::error::sanitize_error_text;
use haven_input::{RecordingReason, RecordingResult};
use haven_tools::MediaTranscriptionStatus;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[path = "attachment_ingress.rs"]
mod attachment_ingress;
use attachment_ingress::validate_attachments;

#[derive(Serialize)]
pub struct RecordingStatus {
    pub is_recording: bool,
    pub is_toggle: bool,
}

#[tauri::command]
pub async fn get_recording_state(
    state: State<'_, Arc<AppState>>,
) -> Result<RecordingStatus, String> {
    let shell_state = state.runtime.shell.state().await;
    Ok(RecordingStatus {
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

pub(crate) fn recording_stop_reason_dto(reason: RecordingReason) -> RecordingStopReasonDto {
    match reason {
        RecordingReason::Manual => RecordingStopReasonDto::Manual,
        RecordingReason::Silence => RecordingStopReasonDto::Silence,
        RecordingReason::MaxDuration => RecordingStopReasonDto::MaxDuration,
        RecordingReason::Cancel => RecordingStopReasonDto::Cancel,
    }
}

/// The `rec-*` ID of an app-owned recording. Call while holding
/// `recording_lifecycle.lock()`; duplicate starts can reuse the active ID.
pub(crate) fn begin_recording(
    state: &AppState,
    lifecycle: &RecordingLifecycleGuard<'_>,
) -> haven_common::types::RecordingId {
    state.recording_lifecycle.begin(lifecycle)
}

/// Emit `recording:started` with the recording's ID. Used by both the
/// `start_recording` Tauri command and the shell hotkey start path so the
/// wire shape stays consistent across entry points.
pub(crate) fn emit_recording_started(
    app: &tauri::AppHandle,
    recording_id: &haven_common::types::RecordingId,
) {
    emit_event_logged(
        app,
        RECORDING_STARTED_EVENT,
        RecordingEvent {
            is_recording: true,
            recording_id: Some(recording_id.clone()),
            reason: None,
            duration_ms: None,
        },
        "recording_started",
    );
}

/// Emit `recording:stopped` with the supplied reason and duration.
pub(crate) fn emit_recording_stopped(
    app: &tauri::AppHandle,
    recording_id: haven_common::types::RecordingId,
    reason: RecordingStopReasonDto,
    duration_ms: Option<u64>,
) {
    emit_event_logged(
        app,
        RECORDING_STOPPED_EVENT,
        RecordingEvent {
            is_recording: false,
            recording_id: Some(recording_id),
            reason: Some(reason),
            duration_ms,
        },
        "recording_stopped",
    );
}

/// Emit `recording:error` for the affected capture when one exists. Start
/// failures have no active capture and receive a fresh correlation ID.
pub(crate) fn emit_recording_error(
    app: &tauri::AppHandle,
    recording_id: Option<haven_common::types::RecordingId>,
    error: impl Into<String>,
) {
    let error = sanitize_error_text(&error.into());
    emit_event_logged(
        app,
        RECORDING_ERROR_EVENT,
        RecordingErrorEvent {
            recording_id: recording_id.unwrap_or_else(|| haven_common::types::new_id("rec").into()),
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
/// currently open session instead of always starting a new one.
pub(crate) async fn finalize_transcription(
    state: &Arc<AppState>,
    app: &tauri::AppHandle,
    recording_id: haven_common::types::RecordingId,
    result: RecordingResult,
) -> Option<String> {
    // Tell the UI STT is about to run, before the (potentially slow)
    // network call, so it can show a "transcribing" hint right away.
    emit_event_logged(
        app,
        TRANSCRIPTION_STARTED_EVENT,
        TranscriptionStartedEvent {
            recording_id: recording_id.clone(),
        },
        "transcription_started",
    );

    if let Some(error) = result.capture_error {
        emit_event_logged(
            app,
            TRANSCRIPTION_ERROR_EVENT,
            TranscriptionErrorEvent {
                recording_id,
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
                recording_id,
                text: String::new(),
                duration_ms: result.duration_ms,
                confidence: None,
            },
            "transcription_empty_capture",
        );
        return None;
    }

    let wav = result.encode_wav();
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
                pending.insert(recording_id.to_string(), llm_usage);
            }
            emit_event_logged(
                app,
                TRANSCRIPTION_RESULT_EVENT,
                TranscriptionResultEvent {
                    recording_id: recording_id.clone(),
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
                .remove(recording_id.as_str());
            // There is nothing to submit, but the UI still needs the
            // "transcribing" overlay closed. The frontend treats an empty
            // `transcription:result` as "close, add no message".
            emit_event_logged(
                app,
                TRANSCRIPTION_RESULT_EVENT,
                TranscriptionResultEvent {
                    recording_id: recording_id.clone(),
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
                .remove(recording_id.as_str());
            emit_event_logged(
                app,
                TRANSCRIPTION_ERROR_EVENT,
                TranscriptionErrorEvent {
                    recording_id,
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

pub(crate) enum RecordingStopShellUpdate {
    /// The initiating Shell control already wrote its stopped state. Refresh
    /// only the tray from the latest state so an older handler cannot undo a
    /// newer start/toggle while it was awaiting the pipeline.
    RefreshCurrent,
    /// A Tauri command started the stop. Apply its state only if no newer
    /// Shell transition has changed the recording revision in the meantime.
    StopIfRevision(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopCaptureErrorClass {
    /// Another stop path has finished capture or is finalizing its result.
    AlreadyFinalizing,
    /// Capture is still active, so this call did not stop it.
    CaptureStillActive,
}

pub(crate) fn classify_stop_capture_error(
    state: &haven_input::RecordingState,
) -> StopCaptureErrorClass {
    match state {
        haven_input::RecordingState::Pending | haven_input::RecordingState::Processing => {
            StopCaptureErrorClass::AlreadyFinalizing
        }
        haven_input::RecordingState::Recording => StopCaptureErrorClass::CaptureStillActive,
    }
}

pub(crate) async fn settle_owned_stop_capture_error<SyncShell, SyncFuture, Detach, EmitError>(
    state: &haven_input::RecordingState,
    expected_recording_id: &haven_common::types::RecordingId,
    error: String,
    sync_shell: SyncShell,
    detach: Detach,
    emit_error: EmitError,
) -> Result<(), String>
where
    SyncShell: FnOnce() -> SyncFuture,
    SyncFuture: std::future::Future<Output = ()>,
    Detach: FnOnce() -> Option<haven_common::types::RecordingId>,
    EmitError: FnOnce(haven_common::types::RecordingId, String),
{
    if classify_stop_capture_error(state) == StopCaptureErrorClass::CaptureStillActive {
        return Err(error);
    }

    sync_shell().await;
    if let Some(recording_id) = stop_error_event_recording_id(detach(), expected_recording_id) {
        emit_error(recording_id, error);
    }
    Ok(())
}

/// Return the detached capture ID only when it is the same recording whose
/// stop failed. This prevents a delayed failure from emitting an error for a
/// newer recording that acquired ownership in between.
pub(crate) fn stop_error_event_recording_id(
    detached: Option<haven_common::types::RecordingId>,
    expected: &haven_common::types::RecordingId,
) -> Option<haven_common::types::RecordingId> {
    detached.filter(|detached_id| detached_id == expected)
}

pub(crate) struct RecordingStopCompletion<'a> {
    pub(crate) lifecycle: RecordingLifecycleGuard<'a>,
    pub(crate) stop_context: RecordingStopContext,
    pub(crate) recording_id: haven_common::types::RecordingId,
    pub(crate) result: RecordingResult,
    pub(crate) shell_update: RecordingStopShellUpdate,
}

/// Complete the synchronous portion of a successful app-owned recording
/// stop, release the lifecycle guard, then hand transcription to the single
/// app-scoped scheduler. The closures keep event and scheduling adapters
/// injectable so the ordering can be tested without a Tauri runtime.
pub(crate) async fn finish_recording_stop<EmitStopped, Schedule, ScheduleFuture>(
    shell: &DesktopShell,
    completion: RecordingStopCompletion<'_>,
    emit_stopped: EmitStopped,
    schedule: Schedule,
) where
    EmitStopped: FnOnce(haven_common::types::RecordingId, RecordingStopReasonDto, u64) + Send,
    Schedule: FnOnce(haven_common::types::RecordingId, RecordingResult) -> ScheduleFuture + Send,
    ScheduleFuture: std::future::Future<Output = bool> + Send,
{
    let RecordingStopCompletion {
        lifecycle,
        stop_context,
        recording_id,
        result,
        shell_update,
    } = completion;

    match shell_update {
        RecordingStopShellUpdate::RefreshCurrent => shell.refresh_tray().await,
        RecordingStopShellUpdate::StopIfRevision(revision) => {
            shell.sync_recording_if_revision(false, revision).await;
        }
    }

    emit_stopped(
        recording_id.clone(),
        recording_stop_reason_dto(result.reason),
        result.duration_ms,
    );

    if matches!(
        result.reason,
        RecordingReason::Silence | RecordingReason::MaxDuration
    ) && !shell
        .reset_toggle_on_auto_stop_if_generation(stop_context.toggle_generation)
        .await
    {
        tracing::trace!(
            recording_id = %recording_id,
            "skipping stale auto-stop toggle reset"
        );
    }

    drop(lifecycle);
    if result.reason == RecordingReason::Cancel {
        return;
    }
    if !schedule(recording_id.clone(), result).await {
        tracing::warn!(
            recording_id = %recording_id,
            "recording transcription task rejected during application shutdown"
        );
    }
}

/// Register transcription as an app-scoped task so application shutdown
/// cancels/joins it under the same owner as other runtime work.
pub(crate) fn schedule_recording_transcription(
    state: Arc<AppState>,
    app: tauri::AppHandle,
    recording_id: haven_common::types::RecordingId,
    result: RecordingResult,
) -> bool {
    let runtime = state.runtime.clone();
    runtime.spawn("recording-transcription", async move {
        let _ = finalize_transcription(&state, &app, recording_id, result).await;
    })
}

#[tauri::command]
pub async fn start_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let expected_recording_revision = state
        .runtime
        .shell
        .recording_stop_context()
        .await
        .recording_revision;
    let lifecycle = state.recording_lifecycle.lock().await;
    if let Err(e) = state.runtime.input_pipeline.start_capture().await {
        // The hotkey may have started a recording a moment earlier, or a VAD
        // auto-stop may be finalizing: the pipeline is busy, not broken.
        let capture_state = state.runtime.input_pipeline.state().await;
        if matches!(capture_state, haven_input::RecordingState::Recording)
            && let Some(recording_id) = state.recording_lifecycle.current(&lifecycle)
        {
            state
                .runtime
                .shell
                .sync_recording_if_revision(true, expected_recording_revision)
                .await;
            // This command may be reconciling a toolbar click with a capture
            // that was started by the hotkey. Re-emit the stable identity so
            // the renderer can bind its optimistic state to that capture.
            emit_recording_started(&app, &recording_id);
            return Ok(());
        }
        let msg = if matches!(capture_state, haven_input::RecordingState::Processing) {
            "正在处理上一条录音，请稍候再试".to_string()
        } else if matches!(capture_state, haven_input::RecordingState::Recording) {
            "麦克风正由其他操作使用，请稍候再试".to_string()
        } else {
            format!("录音启动失败，请检查麦克风配置: {e}")
        };
        emit_recording_error(&app, None, msg.clone());
        return Err(log_err("start_recording", msg));
    }
    // Keep the shell state in sync so the tray icon, the mute hotkey and the
    // recording toggle reflect a UI-button-started recording.
    state
        .runtime
        .shell
        .sync_recording_if_revision(true, expected_recording_revision)
        .await;
    let recording_id = begin_recording(&state, &lifecycle);
    emit_recording_started(&app, &recording_id);
    Ok(())
}

#[tauri::command]
pub async fn stop_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let stop_context = state.runtime.shell.recording_stop_context().await;
    let lifecycle = state.recording_lifecycle.lock().await;
    let Some(recording_id) = state.recording_lifecycle.current(&lifecycle) else {
        return match classify_stop_capture_error(&state.runtime.input_pipeline.state().await) {
            StopCaptureErrorClass::AlreadyFinalizing => Ok(()),
            StopCaptureErrorClass::CaptureStillActive => Err(log_err(
                "stop_recording",
                "麦克风正由其他操作使用，无法停止此录音".to_string(),
            )),
        };
    };
    // Stop the audio capture first, *then* notify the UI that the
    // recording has ended. The previous ordering awaited STT (network
    // call) and the agent ReAct loop (multiple LLM/tool round-trips)
    // before emitting `recording:stopped`, so the UI kept the red
    // "recording" overlay up for the entire post-processing run. With
    // this split the overlay disappears within ~80 ms of the user
    // clicking stop, and STT + agent run as background work that
    // drives the rest of the UI through `transcription:*` / `session:*`
    // events.
    let result = match state.runtime.input_pipeline.stop_capture().await {
        Ok(result) => result,
        Err(e) => {
            // Another path (VAD auto-stop, mute, double click) already owns
            // the stop: the pipeline is Pending (finished) or Processing
            // (finalizing elsewhere). Not an error for the UI — emitting a
            // failure toast here would blame the user for a race they won.
            let capture_state = state.runtime.input_pipeline.state().await;
            return match settle_owned_stop_capture_error(
                &capture_state,
                &recording_id,
                e.to_string(),
                || async {
                    state
                        .runtime
                        .shell
                        .sync_recording_if_revision(false, stop_context.recording_revision)
                        .await;
                },
                || state.recording_lifecycle.finish(&lifecycle),
                |recording_id, error| emit_recording_error(&app, Some(recording_id), error),
            )
            .await
            {
                Ok(()) => Ok(()),
                Err(error) => Err(log_err("stop_recording", error)),
            };
        }
    };
    let detached_recording_id = state.recording_lifecycle.finish(&lifecycle);
    debug_assert_eq!(detached_recording_id.as_ref(), Some(&recording_id));
    let state_for_transcription = state.inner().clone();
    let app_for_transcription = app.clone();
    let shell = state.runtime.shell.clone();
    finish_recording_stop(
        &shell,
        RecordingStopCompletion {
            lifecycle,
            stop_context,
            recording_id,
            result,
            shell_update: RecordingStopShellUpdate::StopIfRevision(stop_context.recording_revision),
        },
        move |recording_id, reason, duration_ms| {
            emit_recording_stopped(&app, recording_id, reason, Some(duration_ms));
        },
        move |recording_id, result| async move {
            schedule_recording_transcription(
                state_for_transcription,
                app_for_transcription,
                recording_id,
                result,
            )
        },
    )
    .await;
    Ok(())
}

#[tauri::command]
pub async fn cancel_recording(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let stop_context = state.runtime.shell.recording_stop_context().await;
    let lifecycle = state.recording_lifecycle.lock().await;
    let Some(recording_id) = state.recording_lifecycle.current(&lifecycle) else {
        // Timed media-tool captures share the pipeline but have no overlay
        // identity, so the voice cancel command must not stop them.
        return Ok(());
    };
    state
        .runtime
        .input_pipeline
        .cancel_capture()
        .await
        .map_err(|e| log_err("cancel_recording", e))?;
    state
        .runtime
        .shell
        .sync_recording_if_revision(false, stop_context.recording_revision)
        .await;
    // No transcription follows a cancel: detach this identity before a new
    // capture can start and remove any usage awaiting renderer submission.
    let cancelled_recording_id = state.recording_lifecycle.finish(&lifecycle);
    if let Some(recording_id) = cancelled_recording_id {
        state
            .pending_recording_usage
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(recording_id.as_str());
    }
    emit_recording_stopped(&app, recording_id, RecordingStopReasonDto::Cancel, None);
    Ok(())
}

#[tauri::command]
pub async fn process_transcript(
    state: State<'_, Arc<AppState>>,
    transcript: String,
    active_session_id: Option<String>,
    attachments: Option<Vec<haven_common::types::MessageAttachment>>,
    voice: Option<bool>,
    recording_id: Option<String>,
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
    let pending_recording_usage = recording_id.as_deref().and_then(|id| {
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
                recording_id,
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
    use crate::app_state::RecordingLifecycleOwner;
    use crate::desktop::{ShellHandler, TrayStatus};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::Ordering;
    use tokio::sync::oneshot;

    #[test]
    fn stop_capture_failure_class_preserves_pending_processing_race_semantics() {
        assert_eq!(
            classify_stop_capture_error(&haven_input::RecordingState::Pending),
            StopCaptureErrorClass::AlreadyFinalizing
        );
        assert_eq!(
            classify_stop_capture_error(&haven_input::RecordingState::Processing),
            StopCaptureErrorClass::AlreadyFinalizing
        );
        assert_eq!(
            classify_stop_capture_error(&haven_input::RecordingState::Recording),
            StopCaptureErrorClass::CaptureStillActive
        );
    }

    #[tokio::test]
    async fn pending_and_processing_stop_failures_return_success_and_correlate_one_error() {
        for capture_state in [
            haven_input::RecordingState::Pending,
            haven_input::RecordingState::Processing,
        ] {
            let owner = RecordingLifecycleOwner::default();
            let lifecycle = owner.lock().await;
            let recording_id = owner.begin(&lifecycle);
            let sync_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let events = Arc::new(StdMutex::new(Vec::new()));
            let sync_calls_in_closure = sync_calls.clone();
            let events_in_closure = events.clone();

            let outcome = settle_owned_stop_capture_error(
                &capture_state,
                &recording_id,
                "stop raced with another finalizer".to_string(),
                move || async move {
                    sync_calls_in_closure.fetch_add(1, Ordering::SeqCst);
                },
                || owner.finish(&lifecycle),
                move |event_recording_id, error| {
                    events_in_closure
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push((event_recording_id, error));
                },
            )
            .await;

            assert_eq!(outcome, Ok(()));
            assert_eq!(sync_calls.load(Ordering::SeqCst), 1);
            assert_eq!(owner.current(&lifecycle), None);
            assert_eq!(
                *events
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                vec![(
                    recording_id,
                    "stop raced with another finalizer".to_string()
                )]
            );
        }
    }

    #[tokio::test]
    async fn active_capture_stop_failure_returns_error_without_side_effects() {
        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        let recording_id = owner.begin(&lifecycle);
        let sync_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let detach_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let event_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sync_in_closure = sync_calls.clone();
        let detach_in_closure = detach_calls.clone();
        let event_in_closure = event_calls.clone();
        let owner_for_detach = &owner;
        let lifecycle_for_detach = &lifecycle;

        let outcome = settle_owned_stop_capture_error(
            &haven_input::RecordingState::Recording,
            &recording_id,
            "capture is still recording".to_string(),
            move || async move {
                sync_in_closure.fetch_add(1, Ordering::SeqCst);
            },
            move || {
                detach_in_closure.fetch_add(1, Ordering::SeqCst);
                owner_for_detach.finish(lifecycle_for_detach)
            },
            move |_recording_id, _error| {
                event_in_closure.fetch_add(1, Ordering::SeqCst);
            },
        )
        .await;

        assert_eq!(outcome, Err("capture is still recording".to_string()));
        assert_eq!(sync_calls.load(Ordering::SeqCst), 0);
        assert_eq!(detach_calls.load(Ordering::SeqCst), 0);
        assert_eq!(event_calls.load(Ordering::SeqCst), 0);
        assert_eq!(owner.current(&lifecycle), Some(recording_id));
    }

    #[tokio::test]
    async fn stop_error_event_uses_only_the_detached_recording_identity() {
        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        let recording_id = owner.begin(&lifecycle);
        let detached = owner.finish(&lifecycle);

        assert_eq!(
            stop_error_event_recording_id(detached, &recording_id),
            Some(recording_id.clone())
        );
        assert_eq!(
            stop_error_event_recording_id(
                Some(haven_common::types::new_id("rec").into()),
                &recording_id
            ),
            None
        );
        assert_eq!(stop_error_event_recording_id(None, &recording_id), None);
    }

    struct TrayOrderRecorder(Arc<StdMutex<Vec<&'static str>>>);

    #[async_trait::async_trait]
    impl ShellHandler for TrayOrderRecorder {
        fn on_tray_status(&self, status: TrayStatus) {
            let label = match status {
                TrayStatus::Normal => "tray:normal",
                TrayStatus::Recording => "tray:recording",
                TrayStatus::Muted => "tray:muted",
                TrayStatus::Busy => "tray:busy",
            };
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(label);
        }
    }

    fn push_order(order: &StdMutex<Vec<&'static str>>, event: &'static str) {
        order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }

    #[tokio::test]
    async fn successful_stop_publishes_before_gated_transcription_and_allows_new_capture() {
        let shell = Arc::new(DesktopShell::new());
        shell.toggle_recording().await;
        let order = Arc::new(StdMutex::new(Vec::new()));
        shell.set_handler(Arc::new(TrayOrderRecorder(order.clone())));
        let stop_context = shell.recording_stop_context().await;

        let owner = Arc::new(RecordingLifecycleOwner::default());
        let owner_for_schedule = owner.clone();
        let lifecycle = owner.lock().await;
        let old_recording_id = owner.begin(&lifecycle);
        assert_eq!(owner.finish(&lifecycle), Some(old_recording_id.clone()));

        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let (finished_tx, mut finished_rx) = oneshot::channel();
        let (next_recording_tx, next_recording_rx) = oneshot::channel();

        let mut result = RecordingResult::default();
        result.reason = RecordingReason::Silence;
        result.duration_ms = 37;

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            finish_recording_stop(
                &shell,
                RecordingStopCompletion {
                    lifecycle,
                    stop_context,
                    recording_id: old_recording_id.clone(),
                    result,
                    shell_update: RecordingStopShellUpdate::StopIfRevision(
                        stop_context.recording_revision,
                    ),
                },
                {
                    let order = order.clone();
                    let expected_recording_id = old_recording_id.clone();
                    move |recording_id, reason, duration_ms| {
                        assert_eq!(recording_id, expected_recording_id);
                        assert_eq!(reason, RecordingStopReasonDto::Silence);
                        assert_eq!(duration_ms, 37);
                        push_order(&order, "stopped");
                    }
                },
                {
                    let shell = shell.clone();
                    let order = order.clone();
                    let expected_recording_id = old_recording_id.clone();
                    move |recording_id, _result| async move {
                        assert_eq!(recording_id, expected_recording_id);
                        assert_eq!(
                            *order
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner()),
                            vec!["tray:normal", "stopped"]
                        );
                        assert!(!shell.state().await.is_recording_toggle);

                        let lifecycle = owner_for_schedule.lock().await;
                        assert!(owner_for_schedule.current(&lifecycle).is_none());
                        let next_recording_id = owner_for_schedule.begin(&lifecycle);
                        drop(lifecycle);
                        next_recording_tx.send(next_recording_id).unwrap();
                        push_order(&order, "scheduled");

                        tokio::spawn(async move {
                            started_tx.send(recording_id.clone()).unwrap();
                            let _ = release_rx.await;
                            finished_tx.send(recording_id).unwrap();
                        });
                        true
                    }
                },
            ),
        )
        .await
        .expect("stop completion must not wait for transcription");

        let next_recording_id = next_recording_rx.await.unwrap();
        assert_ne!(next_recording_id, old_recording_id);
        assert_eq!(started_rx.await.unwrap(), old_recording_id);
        assert_eq!(
            finished_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty),
            "the fake transcription must remain behind its gate"
        );
        release_tx.send(()).unwrap();
        assert_eq!(finished_rx.await.unwrap(), old_recording_id);
        assert_eq!(
            *order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            vec!["tray:normal", "stopped", "scheduled"]
        );
    }

    #[tokio::test]
    async fn manual_stop_does_not_reset_hotkey_toggle() {
        let shell = Arc::new(DesktopShell::new());
        shell.toggle_recording().await;
        let stop_context = shell.recording_stop_context().await;
        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        let recording_id = owner.begin(&lifecycle);
        owner.finish(&lifecycle);

        let mut result = RecordingResult::default();
        result.reason = RecordingReason::Manual;
        finish_recording_stop(
            &shell,
            RecordingStopCompletion {
                lifecycle,
                stop_context,
                recording_id,
                result,
                shell_update: RecordingStopShellUpdate::StopIfRevision(
                    stop_context.recording_revision,
                ),
            },
            |_recording_id, _reason, _duration_ms| {},
            {
                let shell = shell.clone();
                |_recording_id, _result| async move {
                    assert!(shell.state().await.is_recording_toggle);
                    true
                }
            },
        )
        .await;
        assert!(shell.state().await.is_recording_toggle);
    }

    #[tokio::test]
    async fn cancel_reason_does_not_schedule_transcription() {
        let shell = DesktopShell::new();
        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        let recording_id = owner.begin(&lifecycle);
        owner.finish(&lifecycle);
        let mut result = RecordingResult::default();
        result.reason = RecordingReason::Cancel;
        let schedule_called = Arc::new(std::sync::atomic::AtomicBool::new(false));

        finish_recording_stop(
            &shell,
            RecordingStopCompletion {
                lifecycle,
                stop_context: shell.recording_stop_context().await,
                recording_id,
                result,
                shell_update: RecordingStopShellUpdate::RefreshCurrent,
            },
            |_recording_id, reason, _duration_ms| {
                assert_eq!(reason, RecordingStopReasonDto::Cancel)
            },
            {
                let schedule_called = schedule_called.clone();
                move |_recording_id, _result| async move {
                    schedule_called.store(true, Ordering::SeqCst);
                    true
                }
            },
        )
        .await;

        assert!(!schedule_called.load(Ordering::SeqCst));
        tokio::time::timeout(std::time::Duration::from_secs(1), owner.lock())
            .await
            .expect("cancel completion must release the lifecycle permit");
    }

    #[tokio::test]
    async fn shell_stop_refresh_keeps_newer_toggle_before_transcription_schedule() {
        let shell = Arc::new(DesktopShell::new());
        shell.toggle_recording().await;
        let stop_context = shell.recording_stop_context().await;

        // Model state changes that arrive while the old stop callback waits
        // on the pipeline lifecycle permit.
        shell.stop_recording().await;
        shell.toggle_recording().await;
        shell.toggle_recording().await;

        let owner = RecordingLifecycleOwner::default();
        let lifecycle = owner.lock().await;
        let recording_id = owner.begin(&lifecycle);
        owner.finish(&lifecycle);
        let mut result = RecordingResult::default();
        result.reason = RecordingReason::Silence;
        let stopped_recording_id = recording_id.clone();

        finish_recording_stop(
            &shell,
            RecordingStopCompletion {
                lifecycle,
                stop_context,
                recording_id,
                result,
                shell_update: RecordingStopShellUpdate::RefreshCurrent,
            },
            move |event_recording_id, reason, _duration_ms| {
                assert_eq!(event_recording_id, stopped_recording_id);
                assert_eq!(reason, RecordingStopReasonDto::Silence);
            },
            {
                let shell = shell.clone();
                move |_recording_id, _result| async move {
                    let state = shell.state().await;
                    assert!(state.is_recording);
                    assert!(state.is_recording_toggle);
                    assert_eq!(state.tray_status, TrayStatus::Recording);
                    true
                }
            },
        )
        .await;

        let state = shell.state().await;
        assert!(state.is_recording);
        assert!(state.is_recording_toggle);
        assert_eq!(state.tray_status, TrayStatus::Recording);
    }

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
