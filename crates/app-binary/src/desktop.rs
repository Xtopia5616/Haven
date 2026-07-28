use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Unified shell hook surface, replacing the former 10 separate
/// `Arc<Mutex<Option<Box<dyn Fn…>>>>` callback fields on `DesktopShell`.
///
/// All methods have no-op default implementations, so an implementation only
/// needs to override the hooks it cares about. Async hooks model the former
/// `Callback`/`CallbackB`(sync) split: recording lifecycle and window/quit are
/// async (they drive futures), while toggle/mute/tray/notify/hotkey are sync.
#[allow(dead_code)]
#[async_trait]
pub trait ShellHandler: Send + Sync {
    async fn on_recording_start(&self) {}
    async fn on_recording_stop(&self) {}
    async fn on_recording_cancel(&self) {}
    fn on_toggle_change(&self, _active: bool) {}
    fn on_mute_change(&self, _muted: bool) {}
    async fn on_show_window(&self) {}
    async fn on_quit(&self) {}
    fn on_tray_status(&self, _status: TrayStatus) {}
    fn on_notify(&self, _title: &str, _body: &str) {}
    fn on_hotkey_rebind(&self, _key: String) -> Result<(), String> {
        Err("hotkey rebind not supported".into())
    }
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

#[allow(dead_code)]
pub struct DesktopShell {
    state: Arc<Mutex<ShellState>>,
    handler: Arc<Mutex<Option<Arc<dyn ShellHandler>>>>,
}

#[allow(dead_code)]
impl DesktopShell {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ShellState::default())),
            handler: Arc::new(Mutex::new(None)),
        }
    }

    /// Atomically install (or replace) the single shell hook implementation.
    pub async fn set_handler(&self, handler: Arc<dyn ShellHandler>) {
        *self.handler.lock().await = Some(handler);
    }

    /// Snapshot the current handler so callers never hold the lock across an await.
    async fn handler_snap(&self) -> Option<Arc<dyn ShellHandler>> {
        self.handler.lock().await.clone()
    }

    async fn set_tray(&self, status: TrayStatus) {
        if let Some(h) = self.handler_snap().await {
            h.on_tray_status(status);
        }
    }

    pub async fn start_recording(&self) {
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        state.is_recording = true;
        state.tray_status = TrayStatus::Recording;
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_recording_start().await;
        }
        self.set_tray(TrayStatus::Recording).await;
    }

    pub async fn stop_recording(&self) {
        let mut state = self.state.lock().await;
        state.is_recording = false;
        state.tray_status = TrayStatus::Normal;
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_recording_stop().await;
        }
        self.set_tray(TrayStatus::Normal).await;
    }

    pub async fn cancel_recording(&self) {
        let mut state = self.state.lock().await;
        state.is_recording = false;
        state.tray_status = TrayStatus::Normal;
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_recording_cancel().await;
        }
        self.set_tray(TrayStatus::Normal).await;
    }

    pub async fn toggle_recording(&self) {
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        state.is_recording_toggle = !state.is_recording_toggle;
        let new_val = state.is_recording_toggle;
        if new_val {
            state.is_recording = true;
            state.tray_status = TrayStatus::Recording;
        } else {
            state.is_recording = false;
            state.tray_status = TrayStatus::Normal;
        }
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_toggle_change(new_val);
        }
        if new_val {
            if let Some(h) = self.handler_snap().await {
                h.on_recording_start().await;
            }
            self.set_tray(TrayStatus::Recording).await;
        } else {
            if let Some(h) = self.handler_snap().await {
                h.on_recording_stop().await;
            }
            self.set_tray(TrayStatus::Normal).await;
        }
    }

    pub async fn hold_press(&self) {
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        if state.is_recording {
            return;
        }
        state.is_recording = true;
        state.tray_status = TrayStatus::Recording;
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_recording_start().await;
        }
        self.set_tray(TrayStatus::Recording).await;
    }

    pub async fn hold_release(&self) {
        let mut state = self.state.lock().await;
        if !state.is_recording && !state.is_recording_toggle {
            return;
        }
        state.is_recording = false;
        state.tray_status = TrayStatus::Normal;
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_recording_stop().await;
        }
        self.set_tray(TrayStatus::Normal).await;
    }

    pub async fn set_muted(&self, muted: bool) {
        let mut state = self.state.lock().await;
        state.is_muted = muted;
        if muted {
            state.tray_status = TrayStatus::Muted;
            if state.is_recording {
                state.is_recording = false;
            }
        } else {
            state.tray_status = TrayStatus::Normal;
        }
        drop(state);
        if let Some(h) = self.handler_snap().await {
            h.on_mute_change(muted);
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

    pub async fn show_window(&self) {
        if let Some(h) = self.handler_snap().await {
            h.on_show_window().await;
        }
    }

    pub async fn quit(&self) {
        if let Some(h) = self.handler_snap().await {
            h.on_quit().await;
        }
    }

    pub async fn get_state(&self) -> ShellState {
        self.state.lock().await.clone()
    }

    pub async fn reset_toggle_on_auto_stop(&self) {
        let mut state = self.state.lock().await;
        state.is_recording_toggle = false;
    }

    pub async fn notify(&self, title: &str, message: &str) {
        if let Some(h) = self.handler_snap().await {
            h.on_notify(title, message);
        }
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
    async fn test_start_recording_sets_state() {
        let shell = DesktopShell::new();
        shell.start_recording().await;
        let state = shell.get_state().await;
        assert!(state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Recording);
    }

    #[tokio::test]
    async fn test_stop_recording_clears_state() {
        let shell = DesktopShell::new();
        shell.start_recording().await;
        shell.stop_recording().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_cancel_recording_clears_state() {
        let shell = DesktopShell::new();
        shell.start_recording().await;
        shell.cancel_recording().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_muted_prevents_recording() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.start_recording().await;
        let state = shell.get_state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Muted);
    }

    #[tokio::test]
    async fn test_mute_stops_active_recording() {
        let shell = DesktopShell::new();
        shell.start_recording().await;
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
        shell.start_recording().await;
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

    #[tokio::test]
    async fn test_show_window_invokes_callback() {
        let shell = DesktopShell::new();
        let invoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let inv = invoked.clone();
        shell
            .set_handler(Arc::new(RecordHandler::new(
                inv,
                Arc::new(std::sync::Mutex::new((String::new(), String::new()))),
            )))
            .await;
        shell.show_window().await;
        assert!(invoked.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_notify_invokes_callback() {
        let shell = DesktopShell::new();
        let last = Arc::new(std::sync::Mutex::new((String::new(), String::new())));
        shell
            .set_handler(Arc::new(RecordHandler::new(
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                last.clone(),
            )))
            .await;
        shell.notify("title", "message").await;
        let (t, m) = last.lock().unwrap().clone();
        assert_eq!(t, "title");
        assert_eq!(m, "message");
    }

    #[tokio::test]
    async fn test_quit_invokes_callback() {
        let shell = DesktopShell::new();
        let invoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let inv = invoked.clone();
        shell.set_handler(Arc::new(QuitHandler(inv))).await;
        shell.quit().await;
        assert!(invoked.load(std::sync::atomic::Ordering::SeqCst));
    }
}

/// Test handler recording show_window + notify calls.
#[allow(dead_code)]
struct RecordHandler {
    show_invoked: Arc<std::sync::atomic::AtomicBool>,
    last_notify: Arc<std::sync::Mutex<(String, String)>>,
}

impl RecordHandler {
    #[allow(dead_code)]
    fn new(
        show_invoked: Arc<std::sync::atomic::AtomicBool>,
        last_notify: Arc<std::sync::Mutex<(String, String)>>,
    ) -> Self {
        Self {
            show_invoked,
            last_notify,
        }
    }
}

#[async_trait]
impl ShellHandler for RecordHandler {
    async fn on_show_window(&self) {
        self.show_invoked
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    fn on_notify(&self, title: &str, body: &str) {
        *self.last_notify.lock().unwrap() = (title.to_string(), body.to_string());
    }
}

#[allow(dead_code)]
struct QuitHandler(Arc<std::sync::atomic::AtomicBool>);

#[async_trait]
impl ShellHandler for QuitHandler {
    async fn on_quit(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}
