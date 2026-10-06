use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

use haven_common::hooks::OnceHandler;

/// Unified shell hook surface, replacing the former 10 separate
/// `Arc<Mutex<Option<Box<dyn Fn…>>>>` callback fields on `DesktopShell`.
///
/// All methods have no-op default implementations, so an implementation only
/// needs to override the hooks it cares about. Async hooks model the former
/// `Callback`/`CallbackB`(sync) split: recording lifecycle is async (drives
/// futures), while toggle/mute/tray are sync.
#[async_trait]
pub trait ShellHandler: Send + Sync {
    async fn on_recording_start(&self, _context: RecordingStartContext) {}
    async fn on_recording_stop(&self, _context: RecordingStopContext) {}
    fn on_toggle_change(&self, _active: bool) {}
    fn on_mute_change(&self, _muted: bool) {}
    fn on_tray_status(&self, _status: TrayStatus) {}
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
    pub hold_mode: bool,
    #[serde(skip)]
    recording_revision: u64,
    #[serde(skip)]
    toggle_generation: u64,
}

/// Revision captured before an async app command starts touching the input
/// pipeline. A later shell transition makes an old command-side state sync
/// stale, so it must leave the newer shell state intact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingStartContext {
    pub(crate) recording_revision: u64,
}

/// Stop identity captured by the input that initiated the stop. The toggle
/// generation lets delayed auto-stop work avoid clearing a newer hotkey intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordingStopContext {
    pub(crate) recording_revision: u64,
    pub(crate) toggle_generation: u64,
}

impl Default for ShellState {
    fn default() -> Self {
        Self {
            is_recording: false,
            is_recording_toggle: false,
            is_muted: false,
            tray_status: TrayStatus::Normal,
            hold_mode: false,
            recording_revision: 0,
            toggle_generation: 0,
        }
    }
}

pub struct DesktopShell {
    state: Arc<Mutex<ShellState>>,
    handler: OnceHandler<dyn ShellHandler>,
}

impl DesktopShell {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(ShellState::default())),
            handler: OnceHandler::new(),
        }
    }

    /// Install the single shell hook implementation. May only be installed
    /// once; a second install is ignored and logged (the handler never
    /// changes at runtime).
    pub fn set_handler(&self, handler: Arc<dyn ShellHandler>) {
        self.handler.set(handler);
    }

    /// Snapshot the current handler (installation is one-time, so this is
    /// lock-free) so callers never hold a lock across an await.
    fn handler_snap(&self) -> Option<Arc<dyn ShellHandler>> {
        self.handler.snap()
    }

    /// Reconcile the tray status from the current shell state. Deriving it
    /// here — rather than having each call site pass an explicit status to
    /// `set_tray` — keeps the tray icon a pure function of the state, so a new
    /// state transition cannot forget to update the tray.
    async fn derive_tray(&self) {
        let status = {
            let state = self.state.lock().await;
            if state.is_muted {
                TrayStatus::Muted
            } else if state.is_recording {
                TrayStatus::Recording
            } else {
                TrayStatus::Normal
            }
        };
        self.set_tray(status).await;
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
        let context = {
            let mut state = self.state.lock().await;
            state.is_recording = false;
            state.recording_revision = state.recording_revision.wrapping_add(1);
            RecordingStopContext {
                recording_revision: state.recording_revision,
                toggle_generation: state.toggle_generation,
            }
        };
        if let Some(h) = &handler {
            h.on_recording_stop(context).await;
        }
        self.refresh_tray().await;
    }

    /// Sync shell state with an app command only while its captured revision
    /// is still current. Updates the tray without re-triggering the handler —
    /// calling `toggle_recording`/`hold_press` here would double-start the
    /// pipeline. A stale async command still refreshes the tray from whatever
    /// state is current, but cannot overwrite it.
    pub(crate) async fn sync_recording_if_revision(
        &self,
        recording: bool,
        expected_revision: u64,
    ) -> bool {
        let applied = {
            let mut state = self.state.lock().await;
            if state.recording_revision == expected_revision {
                state.is_recording = recording;
                state.recording_revision = state.recording_revision.wrapping_add(1);
                true
            } else {
                false
            }
        };
        self.refresh_tray().await;
        applied
    }

    /// Return the current versions as one snapshot so command paths can use a
    /// consistent recording revision and toggle generation across awaits.
    pub(crate) async fn recording_stop_context(&self) -> RecordingStopContext {
        let state = self.state.lock().await;
        RecordingStopContext {
            recording_revision: state.recording_revision,
            toggle_generation: state.toggle_generation,
        }
    }

    /// Re-derive tray status from the latest ShellState without changing any
    /// recording or toggle flags.
    pub(crate) async fn refresh_tray(&self) {
        self.derive_tray().await;
    }

    pub async fn toggle_recording(&self) {
        let handler = self.handler_snap();
        let mut state = self.state.lock().await;
        if state.is_muted {
            return;
        }
        let was_recording = state.is_recording;
        state.is_recording_toggle = !state.is_recording_toggle;
        state.toggle_generation = state.toggle_generation.wrapping_add(1);
        let new_val = state.is_recording_toggle;
        state.is_recording = new_val;
        state.recording_revision = state.recording_revision.wrapping_add(1);
        let recording_revision = state.recording_revision;
        let toggle_generation = state.toggle_generation;
        drop(state);
        if let Some(h) = &handler {
            h.on_toggle_change(new_val);
        }
        if new_val {
            // Already recording via another source (UI button): keep the
            // toggle flag but do not double-start the pipeline.
            if !was_recording && let Some(h) = &handler {
                h.on_recording_start(RecordingStartContext { recording_revision })
                    .await;
            }
        } else if let Some(h) = &handler {
            h.on_recording_stop(RecordingStopContext {
                recording_revision,
                toggle_generation,
            })
            .await;
        }
        self.refresh_tray().await;
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
        state.recording_revision = state.recording_revision.wrapping_add(1);
        let recording_revision = state.recording_revision;
        drop(state);
        if let Some(h) = &handler {
            h.on_recording_start(RecordingStartContext { recording_revision })
                .await;
        }
        self.refresh_tray().await;
    }

    pub async fn hold_release(&self) {
        let handler = self.handler_snap();
        let mut state = self.state.lock().await;
        if !state.is_recording && !state.is_recording_toggle {
            return;
        }
        state.is_recording = false;
        state.recording_revision = state.recording_revision.wrapping_add(1);
        let context = RecordingStopContext {
            recording_revision: state.recording_revision,
            toggle_generation: state.toggle_generation,
        };
        drop(state);
        if let Some(h) = &handler {
            h.on_recording_stop(context).await;
        }
        self.refresh_tray().await;
    }

    pub async fn set_muted(&self, muted: bool) {
        let handler = self.handler_snap();
        let (was_recording, stop_context) = {
            let mut state = self.state.lock().await;
            state.is_muted = muted;
            let was_recording = state.is_recording;
            if muted {
                state.is_recording = false;
                if was_recording {
                    state.recording_revision = state.recording_revision.wrapping_add(1);
                }
            }
            (
                was_recording,
                RecordingStopContext {
                    recording_revision: state.recording_revision,
                    toggle_generation: state.toggle_generation,
                },
            )
        };
        if let Some(h) = &handler {
            h.on_mute_change(muted);
        }
        if muted && was_recording {
            // Muting while recording: stop the capture immediately so the
            // microphone is released instead of keeping the stream hot while
            // the user believes the mic is off. The handler finalizes the
            // recording (STT + transcript) as a normal stop.
            if let Some(h) = &handler {
                h.on_recording_stop(stop_context).await;
            }
        }
        self.refresh_tray().await;
    }

    pub async fn set_hold_mode(&self, hold: bool) {
        self.state.lock().await.hold_mode = hold;
    }

    /// Current shell state used by tray, hotkey, and recording orchestration.
    pub async fn state(&self) -> ShellState {
        self.state.lock().await.clone()
    }

    pub(crate) async fn reset_toggle_on_auto_stop_if_generation(
        &self,
        expected_generation: u64,
    ) -> bool {
        let mut state = self.state.lock().await;
        if state.toggle_generation != expected_generation {
            return false;
        }
        state.is_recording_toggle = false;
        state.toggle_generation = state.toggle_generation.wrapping_add(1);
        true
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    struct DelayedAutoStopHandler {
        shell: std::sync::Weak<DesktopShell>,
        entered: Arc<Notify>,
        release: Arc<Notify>,
        first_stop: AtomicBool,
        reset_applied: AtomicBool,
    }

    #[async_trait]
    impl ShellHandler for DelayedAutoStopHandler {
        async fn on_recording_stop(&self, context: RecordingStopContext) {
            if self.first_stop.swap(true, Ordering::AcqRel) {
                return;
            }
            self.entered.notify_one();
            self.release.notified().await;
            if let Some(shell) = self.shell.upgrade() {
                let applied = shell
                    .reset_toggle_on_auto_stop_if_generation(context.toggle_generation)
                    .await;
                self.reset_applied.store(applied, Ordering::Release);
            }
        }
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
        let state = shell.state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_stop_recording_clears_state() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.stop_recording().await;
        let state = shell.state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_revision_checked_sync_recording_sets_state_and_tray() {
        let shell = DesktopShell::new();
        assert!(shell.sync_recording_if_revision(true, 0).await);
        let state = shell.state().await;
        assert!(state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Recording);
        // `is_recording_toggle` is shell-hotkey-only; sync must not set it.
        assert!(!state.is_recording_toggle);
        assert!(shell.sync_recording_if_revision(false, 1).await);
        let state = shell.state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn stale_revision_sync_preserves_new_recording_state() {
        let shell = DesktopShell::new();
        let stale_revision = shell.recording_stop_context().await.recording_revision;
        shell.toggle_recording().await;

        assert!(
            !shell
                .sync_recording_if_revision(false, stale_revision)
                .await
        );
        let state = shell.state().await;
        assert!(state.is_recording);
        assert!(state.is_recording_toggle);
        assert_eq!(state.tray_status, TrayStatus::Recording);
    }

    #[tokio::test]
    async fn test_muted_prevents_recording() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.toggle_recording().await;
        let state = shell.state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Muted);
    }

    #[tokio::test]
    async fn test_mute_stops_active_recording() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.set_muted(true).await;
        let state = shell.state().await;
        assert!(!state.is_recording);
        assert_eq!(state.tray_status, TrayStatus::Muted);
    }

    #[tokio::test]
    async fn test_unmute_restores_normal() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.set_muted(false).await;
        let state = shell.state().await;
        assert!(!state.is_muted);
        assert_eq!(state.tray_status, TrayStatus::Normal);
    }

    #[tokio::test]
    async fn test_toggle_recording_starts_and_stops() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        let state = shell.state().await;
        assert!(state.is_recording);
        assert!(state.is_recording_toggle);
        shell.toggle_recording().await;
        let state = shell.state().await;
        assert!(!state.is_recording);
        assert!(!state.is_recording_toggle);
    }

    #[tokio::test]
    async fn test_hold_press_and_release() {
        let shell = DesktopShell::new();
        shell.hold_press().await;
        let state = shell.state().await;
        assert!(state.is_recording);
        shell.hold_release().await;
        let state = shell.state().await;
        assert!(!state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_press_noop_when_muted() {
        let shell = DesktopShell::new();
        shell.set_muted(true).await;
        shell.hold_press().await;
        let state = shell.state().await;
        assert!(!state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_press_noop_when_already_recording() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        shell.hold_press().await;
        let state = shell.state().await;
        assert!(state.is_recording);
    }

    #[tokio::test]
    async fn test_hold_release_noop_when_not_recording() {
        let shell = DesktopShell::new();
        shell.hold_release().await;
        let state = shell.state().await;
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
        let generation = shell.recording_stop_context().await.toggle_generation;
        assert!(
            shell
                .reset_toggle_on_auto_stop_if_generation(generation)
                .await
        );
        let state = shell.state().await;
        assert!(!state.is_recording_toggle);
    }

    #[tokio::test]
    async fn stale_auto_stop_does_not_clear_new_toggle_generation() {
        let shell = DesktopShell::new();
        shell.toggle_recording().await;
        let stale_generation = shell.recording_stop_context().await.toggle_generation;
        shell.toggle_recording().await;
        shell.toggle_recording().await;

        assert!(
            !shell
                .reset_toggle_on_auto_stop_if_generation(stale_generation)
                .await
        );
        let state = shell.state().await;
        assert!(state.is_recording);
        assert!(state.is_recording_toggle);
    }

    #[tokio::test]
    async fn delayed_auto_stop_preserves_newer_toggle_state_and_tray() {
        let shell = Arc::new(DesktopShell::new());
        let handler = Arc::new(DelayedAutoStopHandler {
            shell: Arc::downgrade(&shell),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            first_stop: AtomicBool::new(false),
            reset_applied: AtomicBool::new(false),
        });
        shell.set_handler(handler.clone());
        shell.toggle_recording().await;

        let stop_shell = shell.clone();
        let stop_task = tokio::spawn(async move { stop_shell.stop_recording().await });
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handler.entered.notified(),
        )
        .await
        .expect("the first stop callback must enter its gate");

        // These state changes happen before their callbacks can acquire the
        // recording lifecycle permit. The older auto-stop must not overwrite
        // them when its callback resumes.
        shell.toggle_recording().await;
        shell.toggle_recording().await;
        handler.release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), stop_task)
            .await
            .expect("the stopped shell operation must finish")
            .expect("the stopped shell task must not panic");

        let state = shell.state().await;
        assert!(state.is_recording);
        assert!(state.is_recording_toggle);
        assert_eq!(state.tray_status, TrayStatus::Recording);
        assert!(
            !handler.reset_applied.load(Ordering::Acquire),
            "stale auto-stop reset must not consume the newer toggle generation"
        );
    }

    #[test]
    fn test_desktop_shell_default_impl() {
        let shell = DesktopShell::default();
        let state = shell.state.blocking_lock();
        assert!(!state.is_recording);
    }
}
