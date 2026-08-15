use crate::app_state::AppState;
use crate::commands::log_err;
use crate::events::{
    RecordingEvent, TranscriptionErrorEvent, TranscriptionResultEvent, TranscriptionStartedEvent,
};
use haven_common::config::ContextLimitsConfig;
use haven_input::{RecordingReason, RecordingResult};
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use tauri::Emitter;
use tauri::State;

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
    let _ = app.emit(
        "recording:started",
        RecordingEvent {
            is_recording: true,
            session_id: Some(session_id.clone()),
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
            "session_id": haven_common::types::new_id("rec"),
            "error": error.into(),
        }),
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
    mut result: RecordingResult,
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
    let _ = app.emit(
        "transcription:started",
        TranscriptionStartedEvent {
            session_id: session_id.clone(),
        },
    );

    state.pipeline.transcribe(&mut result).await;

    match result.transcript {
        Some(text) => {
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
                        session_id: session_id.clone(),
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
                        session_id: session_id.clone(),
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
            let session_id = begin_recording_session(&state);
            emit_recording_started(&app, &session_id);
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
    // awaiting the STT network call here would hold the command task for its
    // whole duration.
    let state = state.inner().clone();
    std::mem::drop(tokio::spawn(async move {
        finalize_transcription(&state, &app, result).await
    }));
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
    *state
        .recording_session
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = None;
    emit_recording_stopped(&app, "cancel", None);
    Ok(())
}

#[tauri::command]
pub async fn process_transcript(
    state: State<'_, Arc<AppState>>,
    transcript: String,
    active_session_id: Option<String>,
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
        "process_transcript called: text={:?} active_session_id={:?} attachments={} voice={}",
        transcript,
        active_session_id,
        attachments.len(),
        voice
    );
    let result = state
        .agent
        .process_input_with_attachments(&transcript, active_session_id.clone(), &attachments, voice)
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
        clean = haven_common::types::new_id("file");
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
            .unwrap_or_else(|| haven_common::types::new_id("file"));
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
    limits: &ContextLimitsConfig,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
