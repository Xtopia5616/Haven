use std::sync::Arc;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::Mutex;

type Callback = Arc<Mutex<Option<Box<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>>>>;
type CallbackB = Arc<Mutex<Option<Box<dyn Fn(bool) + Send + Sync>>>>;
type CallbackStatus = Arc<Mutex<Option<Box<dyn Fn(TrayStatus) + Send + Sync>>>>;
type CallbackRebind = Arc<Mutex<Option<Box<dyn Fn(String) -> Result<(), String> + Send + Sync>>>>;
type CallbackNotify = Arc<Mutex<Option<Box<dyn Fn(String, String) + Send + Sync>>>>;

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
    pub on_recording_start: Callback,
    pub on_recording_stop: Callback,
    pub on_recording_cancel: Callback,
    pub on_toggle_change: CallbackB,
    pub on_mute_change: CallbackB,
    pub on_show_window: Callback,
    pub on_quit: Callback,
    pub on_tray_status: CallbackStatus,
    pub on_hotkey_rebind: CallbackRebind,
    pub on_notify: CallbackNotify,
}

impl DesktopShell {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ShellState::default())),
            on_recording_start: Arc::new(Mutex::new(None)),
            on_recording_stop: Arc::new(Mutex::new(None)),
            on_recording_cancel: Arc::new(Mutex::new(None)),
            on_toggle_change: Arc::new(Mutex::new(None)),
            on_mute_change: Arc::new(Mutex::new(None)),
            on_show_window: Arc::new(Mutex::new(None)),
            on_quit: Arc::new(Mutex::new(None)),
            on_tray_status: Arc::new(Mutex::new(None)),
            on_hotkey_rebind: Arc::new(Mutex::new(None)),
            on_notify: Arc::new(Mutex::new(None)),
        }
    }

    async fn set_tray(&self, status: TrayStatus) {
        let guard = self.on_tray_status.lock().await;
        if let Some(ref cb) = *guard {
            cb(status);
        }
    }

    pub async fn start_recording(&self) {
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        state.is_recording = true;
        state.tray_status = TrayStatus::Recording;
        let _ = Self::invoke_cb(&self.on_recording_start).await;
        drop(state);
        self.set_tray(TrayStatus::Recording).await;
    }

    pub async fn stop_recording(&self) {
        let mut state = self.state.lock().await;
        state.is_recording = false;
        state.tray_status = TrayStatus::Normal;
        let _ = Self::invoke_cb(&self.on_recording_stop).await;
        drop(state);
        self.set_tray(TrayStatus::Normal).await;
    }

    pub async fn cancel_recording(&self) {
        let mut state = self.state.lock().await;
        state.is_recording = false;
        state.tray_status = TrayStatus::Normal;
        let _ = Self::invoke_cb(&self.on_recording_cancel).await;
        drop(state);
        self.set_tray(TrayStatus::Normal).await;
    }

    async fn invoke_cb(cb: &Callback) {
        let guard = cb.lock().await;
        if let Some(ref f) = *guard {
            f().await;
        }
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
        {
            let guard = self.on_toggle_change.lock().await;
            if let Some(ref cb) = *guard {
                cb(new_val);
            }
        }
        if new_val {
            let _ = Self::invoke_cb(&self.on_recording_start).await;
            self.set_tray(TrayStatus::Recording).await;
        } else {
            let _ = Self::invoke_cb(&self.on_recording_stop).await;
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
        let _ = Self::invoke_cb(&self.on_recording_start).await;
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
        let _ = Self::invoke_cb(&self.on_recording_stop).await;
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
        {
            let guard = self.on_mute_change.lock().await;
            if let Some(ref cb) = *guard {
                cb(muted);
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

    pub async fn show_window(&self) {
        let guard = self.on_show_window.lock().await;
        if let Some(ref cb) = *guard {
            cb().await;
        }
    }

    pub async fn quit(&self) {
        if let Some(ref cb) = *self.on_quit.lock().await {
            cb().await;
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
        let guard = self.on_notify.lock().await;
        if let Some(ref cb) = *guard {
            cb(title.to_string(), message.to_string());
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
        // should still be recording, not double-invoke
        let state = shell.get_state().await;
        assert!(state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_release_noop_when_not_recording() {
        let shell = DesktopShell::new();
        shell.hold_release().await;
        // should not crash, state unchanged
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
        *shell.on_show_window.lock().await = Some(Box::new(move || {
            let inv = inv.clone();
            Box::pin(async move { inv.store(true, std::sync::atomic::Ordering::SeqCst); })
        }));
        shell.show_window().await;
        assert!(invoked.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_notify_invokes_callback() {
        let shell = DesktopShell::new();
        let last = Arc::new(std::sync::Mutex::new((String::new(), String::new())));
        let last_clone = last.clone();
        *shell.on_notify.lock().await = Some(Box::new(move |t: String, m: String| {
            *last_clone.lock().unwrap() = (t, m);
        }));
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
        *shell.on_quit.lock().await = Some(Box::new(move || {
            let inv = inv.clone();
            Box::pin(async move { inv.store(true, std::sync::atomic::Ordering::SeqCst); })
        }));
        shell.quit().await;
        assert!(invoked.load(std::sync::atomic::Ordering::SeqCst));
    }
}
