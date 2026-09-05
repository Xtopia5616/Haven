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
        let result = match self.pipeline.stop_capture().await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!("pipeline stop_capture failed: {error}");
                self.shell_arc.stop_recording().await;
                crate::commands::emit_recording_error(
                    &self.app_h,
                    format!("录音停止失败: {error}"),
                );
                return;
            }
        };
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

pub(crate) const TRAY_ICON_SIZE: u32 = 256;

pub(crate) fn make_tray_icon(status: TrayStatus) -> tauri::image::Image<'static> {
    const BLUE: [u8; 4] = [44, 80, 144, 255];
    const BUBBLE: [u8; 4] = [215, 227, 255, 255];
    let status_color = match status {
        // The normal tray icon is the same fixed mark as the window icon.
        TrayStatus::Normal => BLUE,
        TrayStatus::Recording => [240, 68, 56, 255],
        TrayStatus::Muted => [169, 170, 178, 255],
        TrayStatus::Busy => [255, 183, 77, 255],
    };

    // Desktop tray/taskbar icons use an opaque brand tile for crisp small-size
    // rendering. The in-app HavenMark remains transparent in the UI.
    // Render a large RGBA icon and let Windows choose the appropriate shell
    // size; a hand-drawn 32px bitmap looks visibly jagged on high-DPI shells.
    let mut rgba = BLUE.repeat((TRAY_ICON_SIZE * TRAY_ICON_SIZE) as usize);
    let scale = TRAY_ICON_SIZE as f32 / 32.0;
    for y in 0..TRAY_ICON_SIZE {
        for x in 0..TRAY_ICON_SIZE {
            let px = (x as f32 + 0.5) / scale;
            let py = (y as f32 + 0.5) / scale;
            let outer = rounded_rect_contains(px, py, 2.0, 2.0, 30.0, 30.0, 8.0);
            if !outer {
                continue;
            }

            let inner = rounded_rect_contains(px, py, 4.0, 4.0, 28.0, 28.0, 6.0);
            let color = if !inner { status_color } else { BLUE };
            set_rgba_pixel(&mut rgba, x, y, color, TRAY_ICON_SIZE);

            let bubble_body = rounded_rect_contains(px, py, 4.0, 7.5, 28.0, 23.0, 5.0);
            let bubble_tail = triangle_contains(px, py, (10.5, 21.5), (10.5, 27.0), (16.0, 23.0));
            if bubble_body || bubble_tail {
                set_rgba_pixel(&mut rgba, x, y, BUBBLE, TRAY_ICON_SIZE);
                continue;
            }

            for (left, top, right, bottom, radius) in [
                (12.0, 12.25, 14.5, 18.25, 1.25),
                (14.75, 10.75, 17.25, 19.75, 1.25),
                (17.5, 12.25, 20.0, 18.25, 1.25),
            ] {
                if rounded_rect_contains(px, py, left, top, right, bottom, radius) {
                    set_rgba_pixel(&mut rgba, x, y, BLUE, TRAY_ICON_SIZE);
                    break;
                }
            }
        }
    }
    tauri::image::Image::new_owned(rgba, TRAY_ICON_SIZE, TRAY_ICON_SIZE)
}

fn set_rgba_pixel(rgba: &mut [u8], x: u32, y: u32, color: [u8; 4], size: u32) {
    let offset = ((y * size + x) * 4) as usize;
    rgba[offset..offset + 4].copy_from_slice(&color);
}

fn rounded_rect_contains(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> bool {
    if x < left || x > right || y < top || y > bottom {
        return false;
    }
    let closest_x = x.clamp(left + radius, right - radius);
    let closest_y = y.clamp(top + radius, bottom - radius);
    let dx = x - closest_x;
    let dy = y - closest_y;
    dx * dx + dy * dy <= radius * radius
}

fn triangle_contains(x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let sign = |p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)| {
        (p1.0 - p3.0) * (p2.1 - p3.1) - (p2.0 - p3.0) * (p1.1 - p3.1)
    };
    let d1 = sign((x, y), a, b);
    let d2 = sign((x, y), b, c);
    let d3 = sign((x, y), c, a);
    let has_negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_negative && has_positive)
}
