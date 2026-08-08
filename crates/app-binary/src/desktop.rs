use async_trait::async_trait;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

/// Unified shell hook surface, replacing the former 10 separate
/// `Arc<Mutex<Option<Box<dyn Fn…>>>>` callback fields on `DesktopShell`.
///
/// All methods have no-op default implementations, so an implementation only
/// needs to override the hooks it cares about. Async hooks model the former
/// `Callback`/`CallbackB`(sync) split: recording lifecycle is async (drives
/// futures), while toggle/mute/tray are sync.
#[async_trait]
pub trait ShellHandler: Send + Sync {
    async fn on_recording_start(&self) {}
    async fn on_recording_stop(&self) {}
    fn on_toggle_change(&self, _active: bool) {}
    fn on_mute_change(&self, _muted: bool) {}
    fn on_tray_status(&self, _status: TrayStatus) {}
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HotkeyConfig {
    pub recording: String,
    pub toggle: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            recording: "Ctrl+Shift+Space".into(),
            toggle: "Ctrl+Shift+T".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TrayStatus {
    Normal,
    Recording,
    Muted,
    Busy,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ShellState {
    pub is_recording: bool,
    pub is_recording_toggle: bool,
    pub is_muted: bool,
    pub tray_status: TrayStatus,
    pub hotkey: HotkeyConfig,
    pub hold_mode: bool,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            is_recording: false,
            is_recording_toggle: false,
            is_muted: false,
            tray_status: TrayStatus::Normal,
            hotkey: HotkeyConfig::default(),
            hold_mode: false,
        }
    }
}

pub struct DesktopShell {
    state: Arc<Mutex<ShellState>>,
    handler: OnceLock<Arc<dyn ShellHandler>>,
}

impl DesktopShell {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ShellState::default())),
            handler: OnceLock::new(),
        }
    }

    /// Install the single shell hook implementation. May only be installed
    /// once; a second install panics (the handler never changes at runtime).
    pub fn set_handler(&self, handler: Arc<dyn ShellHandler>) {
        if self.handler.set(handler).is_err() {
            panic!("shell handler already installed");
        }
    }

    /// Snapshot the current handler (installation is one-time, so this is
    /// lock-free) so callers never hold a lock across an await.
    fn handler_snap(&self) -> Option<Arc<dyn ShellHandler>> {
        self.handler.get().cloned()
    }

    /// Persist the tray status and notify the handler. Skips the notification
    /// when the status did not change: the handler rebuilds the tray icon and
    /// emits an IPC event, so unchanged updates are pure waste.
    async fn set_tray(&self, status: TrayStatus) {
        let handler = self.handler_snap();
        {
            let mut state = self.state.lock().await;
            if state.tray_status == status {
                return;
            }
            state.tray_status = status;
        }
        if let Some(h) = &handler {
            h.on_tray_status(status);
        }
    }

    pub async fn stop_recording(&self) {
        let handler = self.handler_snap();
        {
            let mut state = self.state.lock().await;
            state.is_recording = false;
        }
        if let Some(h) = &handler {
            h.on_recording_stop().await;
        }
        self.set_tray(TrayStatus::Normal).await;
    }

    /// Sync shell state with a recording started outside the shell (e.g. the
    /// UI record button, which drives the pipeline directly). Updates the
    /// flags and tray icon WITHOUT re-triggering the handler — calling
    /// `toggle_recording`/`hold_press` here would double-start the pipeline.
    /// Without this sync the tray icon stays idle, the mute hotkey would not
    /// stop a UI-started recording, and the toggle hotkey would attempt a
    /// duplicate start.
    pub async fn sync_recording(&self, recording: bool) {
        {
            let mut state = self.state.lock().await;
            state.is_recording = recording;
        }
        self.set_tray(if recording {
            TrayStatus::Recording
        } else {
            TrayStatus::Normal
        })
        .await;
    }

    pub async fn toggle_recording(&self) {
        let handler = self.handler_snap();
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        let was_recording = state.is_recording;
        state.is_recording_toggle = !state.is_recording_toggle;
        let new_val = state.is_recording_toggle;
        if new_val {
            state.is_recording = true;
        } else {
            state.is_recording = false;
        }
        drop(state);
        if let Some(h) = &handler {
            h.on_toggle_change(new_val);
        }
        if new_val {
            // Already recording via another source (UI button): keep the
            // toggle flag but do not double-start the pipeline.
            if !was_recording {
                if let Some(h) = &handler {
                    h.on_recording_start().await;
                }
            }
        } else if let Some(h) = &handler {
            h.on_recording_stop().await;
        }
        self.set_tray(if new_val {
            TrayStatus::Recording
        } else {
            TrayStatus::Normal
        })
        .await;
    }

    pub async fn hold_press(&self) {
        let handler = self.handler_snap();
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        if state.is_recording {
            return;
        }
        state.is_recording = true;
        drop(state);
        if let Some(h) = &handler {
            h.on_recording_start().await;
        }
        self.set_tray(TrayStatus::Recording).await;
    }

    pub async fn hold_release(&self) {
        let handler = self.handler_snap();
        let mut state = self.state.lock().await;
        if !state.is_recording && !state.is_recording_toggle {
            return;
        }
        state.is_recording = false;
        drop(state);
        if let Some(h) = &handler {
            h.on_recording_stop().await;
        }
        self.set_tray(TrayStatus::Normal).await;
    }

    pub async fn set_muted(&self, muted: bool) {
        let handler = self.handler_snap();
        let was_recording;
        {
            let mut state = self.state.lock().await;
            state.is_muted = muted;
            was_recording = state.is_recording;
            if muted {
                state.is_recording = false;
            }
        }
        if let Some(h) = &handler {
            h.on_mute_change(muted);
        }
        if muted && was_recording {
            // Muting while recording: stop the capture immediately so the
            // microphone is released instead of keeping the stream hot while
            // the user believes the mic is off. The handler finalizes the
            // recording (STT + transcript) as a normal stop.
            if let Some(h) = &handler {
                h.on_recording_stop().await;
            }
        }
        self.set_tray(if muted {
            TrayStatus::Muted
        } else {
            TrayStatus::Normal
        })
        .await;
    }

    pub async fn set_hold_mode(&self, hold: bool) {
        self.state.lock().await.hold_mode = hold;
    }

    pub async fn get_state(&self) -> ShellState {
        self.state.lock().await.clone()
    }

    pub async fn reset_toggle_on_auto_stop(&self) {
        let mut state = self.state.lock().await;
        state.is_recording_toggle = false;
    }
}

impl Default for DesktopShell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotkey_config_default() {
        let cfg = HotkeyConfig::default();
        assert_eq!(cfg.recording, "Ctrl+Shift+Space");
        assert_eq!(cfg.toggle, "Ctrl+Shift+T");
    }

    #[test]
    fn test_tray_status_serde() {
        let json = serde_json::to_string(&TrayStatus::Normal).unwrap();
        assert_eq!(json, "\"Normal\"");
        let des: TrayStatus = serde_json::from_str("\"Recording\"").unwrap();
        assert_eq!(des, TrayStatus::Recording);
    }

    #[test]
    fn test_shell_state_default() {
        let state = ShellState::default();
        assert!(!state.is_recording);
        assert!(!state.is_recording_toggle);
        assert!(!state.is_muted);
        assert_eq!(state.tray_status, TrayStatus::Normal);
        assert!(!state.hold_mode);
    }

    #[tokio::test]
    async fn test_shell_new_state_is_idle() {
        let shell = DesktopShell::new();
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_stop_recording_clears_state() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.stop_recording().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_sync_recording_sets_state_and_tray() {
        let shell = DesktopShell::new();
        shell.sync_recording(true).await;
        let state = shell.get_state().await;
        assert!(state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Recording);
        // `is_recording_toggle` is shell-hotkey-only; sync must not set it.
        assert!(!state.is_recording_toggle);
        shell.sync_recording(false).await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_muted_prevents_recording() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.toggle_recording().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Muted);
    }

    #[tokio::test]
    async fn test_mute_stops_active_recording() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.set_muted(true).await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Muted);
    }

    #[tokio::test]
    async fn test_unmute_restores_normal() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.set_muted(false).await;
        let state = shell.get_state().await;
        assert!(!state.is_muted);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_toggle_recording_starts_and_stops() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        let state = shell.get_state().await;
        assert!(state.is_recording);
        assert!(state.is_recording_toggle);
        shell.toggle_recording().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert!(!state.is_recording_toggle);
    }

    #[tokio::test]
    async fn test_hold_press_and_release() {
        let shell = DesktopShell::new();
        shell.hold_press().await;
        let state = shell.get_state().await;
        assert!(state.is_recording);
        shell.hold_release().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_press_noop_when_muted() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.hold_press().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_press_noop_when_already_recording() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.hold_press().await;
        let state = shell.get_state().await;
        assert!(state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_release_noop_when_not_recording() {
        let shell = DesktopShell::new();
        shell.hold_release().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
    }

    #[tokio::test]
    async fn test_set_hold_mode() {
        let shell = DesktopShell::new();
        shell.set_hold_mode(true).await;
        assert!(shell.state.lock().await.hold_mode);
        shell.set_hold_mode(false).await;
        assert!(!shell.state.lock().await.hold_mode);
    }

    #[tokio::test]
    async fn test_reset_toggle() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.reset_toggle_on_auto_stop().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording_toggle);
    }

    #[test]
    fn test_desktop_shell_default_impl() {
        let shell = DesktopShell::default();
        let state = shell.state.blocking_lock();
        assert!(!state.is_recording);
    }
}
