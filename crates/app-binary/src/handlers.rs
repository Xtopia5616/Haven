//! Host adapters for desktop shell and input event callbacks.

use crate::app_state::AppState;
use crate::desktop::{self, RecordingStartContext, RecordingStopContext, TrayStatus};
use crate::events;
use crate::events::*;
use crate::logging::sanitize_error_text;
use haven_common::types::SessionId;
use std::sync::Arc;
use tauri::{Emitter, Manager};

async fn stop_capture_for_owned_recording<T, E, Stop, StopFuture>(
    session_id: Option<SessionId>,
    stop_capture: Stop,
) -> Result<Option<(SessionId, T)>, (SessionId, E)>
where
    Stop: FnOnce() -> StopFuture,
    StopFuture: std::future::Future<Output = Result<T, E>>,
{
    let Some(session_id) = session_id else {
        return Ok(None);
    };

    match stop_capture().await {
        Ok(result) => Ok(Some((session_id, result))),
        Err(error) => Err((session_id, error)),
    }
}

/// Concrete `ShellHandler` wiring desktop hooks to the Tauri app handle,
/// input pipeline and tray icon. Replaces the former per-callback field
/// assignments on `DesktopShell`.
pub(crate) struct HavenShellHandler {
    pub(crate) app_h: tauri::AppHandle,
    pub(crate) input_pipeline: Arc<haven_input::InputPipeline>,
    pub(crate) shell_arc: Arc<desktop::DesktopShell>,
    pub(crate) tray: tauri::tray::TrayIcon,
}

#[async_trait::async_trait]
impl desktop::ShellHandler for HavenShellHandler {
    async fn on_recording_start(&self, context: RecordingStartContext) {
        let state = self.app_h.state::<Arc<AppState>>();
        let lifecycle = state.recording_sessions.lock().await;
        // Start the pipeline first: emitting `recording:started` before the
        // pipeline is actually recording would leave the UI stuck in the
        // recording state (and every stop attempt failing with "not
        // recording") if startup errors.
        if let Err(e) = self.input_pipeline.start_capture().await {
            if matches!(
                self.input_pipeline.state().await,
                haven_input::RecordingState::Recording
            ) && state.recording_sessions.current(&lifecycle).is_some()
            {
                // Another app entry point established this user capture while
                // the Shell callback waited for its lifecycle permit.
                self.shell_arc
                    .sync_recording_if_revision(true, context.recording_revision)
                    .await;
                return;
            }
            tracing::warn!("input pipeline start_capture failed: {e}");
            self.shell_arc
                .sync_recording_if_revision(false, context.recording_revision)
                .await;
            crate::commands::emit_recording_error(
                &self.app_h,
                None,
                format!("录音启动失败，请检查麦克风配置: {e}"),
            );
            return;
        }
        let session_id = crate::commands::begin_recording_session(&state, &lifecycle);
        crate::commands::emit_recording_started(&self.app_h, &session_id);
    }

    async fn on_recording_stop(&self, stop_context: RecordingStopContext) {
        let state = self.app_h.state::<Arc<AppState>>();
        let lifecycle = state.recording_sessions.lock().await;
        let (session_id, result) = match stop_capture_for_owned_recording(
            state.recording_sessions.current(&lifecycle),
            || self.input_pipeline.stop_capture(),
        )
        .await
        {
            Ok(Some(stopped)) => stopped,
            Ok(None) => {
                // A timed media-tool capture shares InputPipeline but is not
                // an app voice session. Clear stale shell chrome without
                // stopping it.
                self.shell_arc.refresh_tray().await;
                return;
            }
            Err((session_id, error)) => {
                tracing::warn!("pipeline stop_capture failed: {error}");
                self.shell_arc.refresh_tray().await;
                let detached = state.recording_sessions.finish(&lifecycle);
                if let Some(session_id) =
                    crate::commands::stop_error_event_session_id(detached, &session_id)
                {
                    crate::commands::emit_recording_error(
                        &self.app_h,
                        Some(session_id),
                        format!("录音停止失败: {error}"),
                    );
                }
                return;
            }
        };
        // Same split as the `stop_recording` Tauri command: stop the audio
        // capture first and notify the UI, then run STT in the background.
        // Without this, VAD-triggered auto-stops would also keep the
        // "recording" overlay visible for the duration of the STT call.
        let detached_session_id = state.recording_sessions.finish(&lifecycle);
        debug_assert_eq!(detached_session_id.as_ref(), Some(&session_id));
        let state_for_transcription = state.inner().clone();
        let app_for_transcription = self.app_h.clone();
        let app_for_event = self.app_h.clone();
        crate::commands::finish_recording_stop(
            &self.shell_arc,
            crate::commands::RecordingStopCompletion {
                lifecycle,
                stop_context,
                session_id,
                result,
                shell_update: crate::commands::RecordingStopShellUpdate::RefreshCurrent,
            },
            move |session_id, reason, duration_ms| {
                crate::commands::emit_recording_stopped(
                    &app_for_event,
                    session_id,
                    reason,
                    Some(duration_ms),
                );
            },
            move |session_id, result| async move {
                crate::commands::schedule_recording_transcription(
                    state_for_transcription,
                    app_for_transcription,
                    session_id,
                    result,
                )
            },
        )
        .await;
    }

    fn on_tray_status(&self, status: TrayStatus) {
        let tooltip = match status {
            TrayStatus::Normal => "Haven",
            TrayStatus::Recording => "Haven - Recording",
            TrayStatus::Muted => "Haven - Muted",
            TrayStatus::Busy => "Haven - Busy",
        };
        if let Err(error) = self.tray.set_icon(Some(make_tray_icon(status))) {
            tracing::warn!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to update tray icon"
            );
        }
        if let Err(error) = self.app_h.emit(
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
        ) {
            tracing::debug!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit tray status event"
            );
        }
    }

    fn on_mute_change(&self, muted: bool) {
        if let Err(error) = self
            .app_h
            .emit(MUTE_CHANGED_EVENT, MuteChangedEvent { muted })
        {
            tracing::debug!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit mute status event"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn unowned_capture_does_not_invoke_stop_capture() {
        let stop_calls = Arc::new(AtomicUsize::new(0));
        let capture_state = Arc::new(std::sync::Mutex::new(
            haven_input::RecordingState::Recording,
        ));
        let stop_calls_in_closure = stop_calls.clone();
        let capture_in_closure = capture_state.clone();

        let outcome = stop_capture_for_owned_recording(None::<SessionId>, move || async move {
            stop_calls_in_closure.fetch_add(1, Ordering::SeqCst);
            *capture_in_closure
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                haven_input::RecordingState::Pending;
            Ok::<(), &'static str>(())
        })
        .await;

        assert!(matches!(outcome, Ok(None)));
        assert_eq!(stop_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            *capture_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            haven_input::RecordingState::Recording
        );
    }
}

/// Concrete `InputEventHandler` wiring VAD status + auto-stop to the Tauri app
/// handle and the desktop shell. Replaces the former separate
/// `set_vad_status_callback` + `set_on_auto_stop` bindings on `InputPipeline`.
pub(crate) struct HavenInputEventHandler {
    pub(crate) app_h: tauri::AppHandle,
    pub(crate) shell_arc: Arc<desktop::DesktopShell>,
}

#[async_trait::async_trait]
impl haven_input::InputEventHandler for HavenInputEventHandler {
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
        if let Err(error) = self.app_h.emit(
            RECORDING_VAD_STATUS_EVENT,
            events::VadStatusEvent {
                signal: signal_str.to_string(),
                state: state_str.to_string(),
            },
        ) {
            tracing::debug!(
                error = %sanitize_error_text(&error.to_string()),
                "failed to emit VAD status event"
            );
        }
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
