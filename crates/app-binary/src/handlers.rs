//! Host adapters for desktop shell and input callbacks.

use crate::app_state::AppState;
use crate::desktop::{self, TrayStatus};
use crate::events;
use crate::events::*;
use std::sync::Arc;
use tauri::{Emitter, Manager};

/// Concrete `ShellHandler` wiring desktop hooks to the Tauri app handle,
/// input pipeline and tray icon. Replaces the former per-callback field
/// assignments on `DesktopShell`.
pub(crate) struct HavenShellHandler {
    pub(crate) app_h: tauri::AppHandle,
    pub(crate) pipeline: Arc<haven_input::InputPipeline>,
    pub(crate) shell_arc: Arc<desktop::DesktopShell>,
    pub(crate) tray: tauri::tray::TrayIcon,
}

#[async_trait::async_trait]
impl desktop::ShellHandler for HavenShellHandler {
    async fn on_recording_start(&self) {
        // Start the pipeline first: emitting `recording:started` before the
        // pipeline is actually recording would leave the UI stuck in the
        // recording state (and every stop attempt failing with "not
        // recording") if startup errors.
        if let Err(e) = self.pipeline.start_recording().await {
            tracing::warn!("pipeline start_recording failed: {e}");
            self.shell_arc.stop_recording().await;
            crate::commands::emit_recording_error(
                &self.app_h,
                format!("录音启动失败，请检查麦克风/STT 配置: {e}"),
            );
            return;
        }
        let state = self.app_h.state::<Arc<AppState>>();
        let session_id = crate::commands::begin_recording_session(&state);
        crate::commands::emit_recording_started(&self.app_h, &session_id);
    }

    async fn on_recording_stop(&self) {
        // Same split as the `stop_recording` Tauri command: stop the audio
        // capture first and notify the UI, then run STT in the background.
        // Without this, VAD-triggered auto-stops would also keep the
        // "recording" overlay visible for the duration of the STT call.
        let result = self.pipeline.stop_capture().await;
        if let Ok(result) = result {
            crate::commands::emit_recording_stopped(
                &self.app_h,
                crate::commands::recording_reason_str(result.reason),
                Some(result.duration_ms),
            );
            if matches!(
                result.reason,
                haven_input::RecordingReason::Silence | haven_input::RecordingReason::MaxDuration
            ) {
                self.shell_arc.reset_toggle_on_auto_stop().await;
            }

            // Same finalize path as the `stop_recording` Tauri command: run
            // STT and emit `transcription:result` / `transcription:error`.
            // The frontend then submits the transcript through
            // `process_transcript` like a typed message, so voice input
            // continues the open conversation. Without this, hotkey / VAD-
            // triggered stops silently dropped the transcript — the text
            // never reached the chat UI nor the agent.
            let state = self.app_h.state::<Arc<AppState>>();
            crate::commands::finalize_transcription(state.inner(), &self.app_h, result).await;
        }
    }

    fn on_tray_status(&self, status: TrayStatus) {
        let tooltip = match status {
            TrayStatus::Normal => "Haven",
            TrayStatus::Recording => "Haven - Recording",
            TrayStatus::Muted => "Haven - Muted",
            TrayStatus::Busy => "Haven - Busy",
        };
        let _ = self.tray.set_icon(Some(make_tray_icon(status)));
        let _ = self.app_h.emit(
            TRAY_STATUS_CHANGED_EVENT,
            TrayStatusChangedEvent {
                status: match status {
                    TrayStatus::Normal => "normal",
                    TrayStatus::Recording => "recording",
                    TrayStatus::Muted => "muted",
                    TrayStatus::Busy => "busy",
                }
                .into(),
                tooltip: tooltip.into(),
            },
        );
    }

    fn on_mute_change(&self, muted: bool) {
        let _ = self
            .app_h
            .emit(MUTE_CHANGED_EVENT, MuteChangedEvent { muted });
    }
}

/// Concrete `InputHandler` wiring VAD status + auto-stop to the Tauri app
/// handle and the desktop shell. Replaces the former separate
/// `set_vad_status_callback` + `set_on_auto_stop` bindings on `InputPipeline`.
pub(crate) struct HavenInputHandler {
    pub(crate) app_h: tauri::AppHandle,
    pub(crate) shell_arc: Arc<desktop::DesktopShell>,
}

#[async_trait::async_trait]
impl haven_input::InputHandler for HavenInputHandler {
    fn on_vad_status(
        &self,
        signal: haven_input::vad::VadSignal,
        state: haven_input::vad::VadState,
    ) {
        let signal_str = match signal {
            haven_input::vad::VadSignal::None => "none",
            haven_input::vad::VadSignal::SpeechStart => "speech_start",
            haven_input::vad::VadSignal::SpeechEnd => "speech_end",
            haven_input::vad::VadSignal::AutoStop => "auto_stop",
        };
        let state_str = match state {
            haven_input::vad::VadState::Silent => "silent",
            haven_input::vad::VadState::Speech => "speech",
            haven_input::vad::VadState::SilenceAfterSpeech { .. } => "silence_after_speech",
        };
        let _ = self.app_h.emit(
            RECORDING_VAD_STATUS_EVENT,
            events::VadStatusEvent {
                signal: signal_str.to_string(),
                state: state_str.to_string(),
            },
        );
    }

    async fn on_auto_stop(&self) {
        self.shell_arc.stop_recording().await;
    }
}

pub(crate) fn make_tray_icon(status: TrayStatus) -> tauri::image::Image<'static> {
    let (r, g, b) = match status {
        TrayStatus::Normal => (60, 100, 200),
        TrayStatus::Recording => (220, 50, 50),
        TrayStatus::Muted => (120, 120, 120),
        TrayStatus::Busy => (220, 140, 40),
    };
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for _ in 0..(32 * 32) {
        rgba.push(r);
        rgba.push(g);
        rgba.push(b);
        rgba.push(255);
    }
    tauri::image::Image::new_owned(rgba, 32, 32)
}
