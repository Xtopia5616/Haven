//! Coordinates the native pre-WebView2 surface with the first app paint.

use std::sync::Arc;
#[cfg(windows)]
use std::sync::Mutex;
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
#[cfg(windows)]
use std::time::Duration;
#[cfg(windows)]
use std::time::Instant;

use tauri::{AppHandle, Manager};

#[cfg(windows)]
use crate::startup_splash::{NativeAction, NativeStartupSplash};

#[cfg(windows)]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(windows)]
const STARTUP_TIMEOUT_MESSAGE: &str =
    "Haven 的界面在 45 秒内没有完成加载。请重试；如果问题持续发生，请检查 WebView2 Runtime。";
#[cfg(windows)]
const NAVIGATION_FAILURE_MESSAGE: &str =
    "Haven 的界面没有成功加载。请重试；如果问题持续发生，请检查 WebView2 Runtime。";
#[cfg(windows)]
const PROCESS_FAILURE_MESSAGE: &str = "WebView2 运行组件已停止。可以重试加载 Haven，或取消并退出。";
#[cfg(windows)]
const NO_STARTUP_FAILURE: u8 = 0;

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum StartupFailure {
    Navigation = 1,
    Process = 2,
    Timeout = 3,
}

#[cfg(windows)]
impl StartupFailure {
    fn from_raw(value: u8) -> Option<Self> {
        match value {
            value if value == Self::Navigation as u8 => Some(Self::Navigation),
            value if value == Self::Process as u8 => Some(Self::Process),
            value if value == Self::Timeout as u8 => Some(Self::Timeout),
            _ => None,
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Navigation => NAVIGATION_FAILURE_MESSAGE,
            Self::Process => PROCESS_FAILURE_MESSAGE,
            Self::Timeout => STARTUP_TIMEOUT_MESSAGE,
        }
    }

    fn is_process_failure(self) -> bool {
        matches!(self, Self::Process)
    }
}

pub(crate) struct StartupCoordinator {
    autostart: bool,
    #[cfg(windows)]
    ready: AtomicBool,
    #[cfg(windows)]
    failure: AtomicU8,
    #[cfg(windows)]
    failure_prompted: AtomicBool,
    #[cfg(windows)]
    shutting_down: AtomicBool,
    #[cfg(windows)]
    attempt_started_at: Mutex<Instant>,
    #[cfg(windows)]
    native: Option<NativeStartupSplash>,
}

impl StartupCoordinator {
    pub(crate) fn new(autostart: bool) -> Arc<Self> {
        #[cfg(windows)]
        let native = if autostart {
            None
        } else {
            match NativeStartupSplash::start() {
                Ok(splash) => Some(splash),
                Err(error) => {
                    tracing::warn!(error = %error, "native startup surface is unavailable");
                    None
                }
            }
        };

        Arc::new(Self {
            autostart,
            #[cfg(windows)]
            ready: AtomicBool::new(false),
            #[cfg(windows)]
            failure: AtomicU8::new(NO_STARTUP_FAILURE),
            #[cfg(windows)]
            failure_prompted: AtomicBool::new(false),
            #[cfg(windows)]
            shutting_down: AtomicBool::new(false),
            #[cfg(windows)]
            attempt_started_at: Mutex::new(Instant::now()),
            #[cfg(windows)]
            native,
        })
    }

    pub(crate) fn report_ui_ready(&self, app: &AppHandle) -> Result<(), String> {
        #[cfg(windows)]
        if self.failure.load(Ordering::Acquire) != NO_STARTUP_FAILURE {
            return Err("WebView2 尚未恢复".into());
        }
        if !self.autostart {
            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "Haven 主窗口不可用".to_string())?;
            window.show().map_err(|error| error.to_string())?;
            let _ = window.set_focus();
        }
        #[cfg(windows)]
        {
            self.ready.store(true, Ordering::Release);
            self.failure.store(NO_STARTUP_FAILURE, Ordering::Release);
            if let Some(native) = &self.native {
                native.hide();
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn start_driver(self: &Arc<Self>, app: AppHandle) {
        let coordinator = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if coordinator.shutting_down.load(Ordering::Acquire) {
                    break;
                }
                if let Some(action) = coordinator.take_native_action() {
                    match action {
                        NativeAction::Retry => coordinator.retry(&app),
                        NativeAction::Exit => app.exit(0),
                    }
                }
                if let Some(message) = coordinator.should_prompt_deferred_failure(&app) {
                    match crate::startup_splash::show_failure_dialog(message) {
                        NativeAction::Retry => coordinator.retry(&app),
                        NativeAction::Exit => app.exit(0),
                    }
                }
                if coordinator.startup_timed_out(&app) {
                    coordinator.fail(&app, StartupFailure::Timeout);
                }
            }
        });
    }

    #[cfg(windows)]
    pub(crate) fn report_navigation_failure(&self, app: &AppHandle) {
        self.fail(app, StartupFailure::Navigation);
    }

    #[cfg(windows)]
    pub(crate) fn report_process_failure(&self, app: &AppHandle) {
        self.fail(app, StartupFailure::Process);
    }

    pub(crate) fn shutdown(&self) {
        #[cfg(windows)]
        self.shutting_down.store(true, Ordering::Release);
        #[cfg(windows)]
        if let Some(native) = &self.native {
            native.close();
        }
    }

    pub(crate) fn report_builder_failure(&self) {
        if self.autostart {
            return;
        }
        #[cfg(windows)]
        crate::startup_splash::show_builder_failure_dialog();
    }

    #[cfg(windows)]
    fn startup_timed_out(&self, app: &AppHandle) -> bool {
        if self.ready.load(Ordering::Acquire)
            || self.failure.load(Ordering::Acquire) != NO_STARTUP_FAILURE
        {
            return false;
        }
        if self.autostart {
            let visible = app
                .get_webview_window("main")
                .and_then(|window| window.is_visible().ok())
                .unwrap_or(false);
            if !visible {
                if let Ok(mut started) = self.attempt_started_at.lock() {
                    *started = Instant::now();
                }
                return false;
            }
        }
        self.attempt_started_at
            .lock()
            .map(|started| started.elapsed() >= STARTUP_TIMEOUT)
            .unwrap_or(false)
    }

    #[cfg(windows)]
    fn should_prompt_deferred_failure(&self, app: &AppHandle) -> Option<&'static str> {
        if !self.autostart {
            return None;
        }
        let failure = StartupFailure::from_raw(self.failure.load(Ordering::Acquire))?;
        let visible = app
            .get_webview_window("main")
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        (visible && !self.failure_prompted.swap(true, Ordering::AcqRel))
            .then_some(failure.message())
    }

    #[cfg(windows)]
    fn fail(&self, app: &AppHandle, failure: StartupFailure) {
        if self.shutting_down.load(Ordering::Acquire) {
            tracing::error!(
                message = failure.message(),
                process_failure = failure.is_process_failure(),
                "WebView2 startup failed"
            );
            return;
        }
        if self
            .failure
            .compare_exchange(
                NO_STARTUP_FAILURE,
                failure as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return;
        }
        self.ready.store(false, Ordering::Release);
        tracing::error!(
            message = failure.message(),
            process_failure = failure.is_process_failure(),
            "WebView2 startup failed"
        );
        if self.autostart {
            return;
        }
        #[cfg(windows)]
        if let Some(native) = &self.native
            && native.show_failure(failure.message())
        {
            return;
        }
        #[cfg(windows)]
        match crate::startup_splash::show_failure_dialog(failure.message()) {
            NativeAction::Retry => self.retry(app),
            NativeAction::Exit => app.exit(1),
        }
    }

    #[cfg(windows)]
    fn retry(&self, app: &AppHandle) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        self.failure.store(NO_STARTUP_FAILURE, Ordering::Release);
        self.failure_prompted.store(false, Ordering::Release);
        self.ready.store(false, Ordering::Release);
        if let Ok(mut started) = self.attempt_started_at.lock() {
            *started = Instant::now();
        }
        #[cfg(windows)]
        if let Some(native) = &self.native {
            native.show_loading();
        }

        let Some(window) = app.get_webview_window("main") else {
            self.fail(app, StartupFailure::Navigation);
            return;
        };
        if let Err(error) = window.reload() {
            tracing::error!(error = %error, "failed to reload the main WebView2");
            self.fail(app, StartupFailure::Navigation);
        }
    }

    #[cfg(windows)]
    fn take_native_action(&self) -> Option<NativeAction> {
        self.native.as_ref()?.take_action()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{PROCESS_FAILURE_MESSAGE, StartupFailure};

    #[test]
    fn deferred_process_failure_keeps_its_original_message() {
        let failure = StartupFailure::from_raw(StartupFailure::Process as u8).unwrap();

        assert!(failure.is_process_failure());
        assert_eq!(failure.message(), PROCESS_FAILURE_MESSAGE);
    }
}
