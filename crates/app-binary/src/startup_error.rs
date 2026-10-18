//! User-facing feedback for fatal failures before the app can render its UI.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StartupFailure {
    Configuration,
    ApplicationState,
    ApplicationBuild,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum WebviewFailure {
    Navigation = 1,
    Process = 2,
}

impl WebviewFailure {
    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::Navigation => "Haven 的界面没有成功加载。可以重试加载，或取消并退出。",
            Self::Process => "Haven 的 WebView2 进程已停止。可以重试加载，或取消并退出。",
        }
    }

    #[cfg(windows)]
    fn from_raw(value: u8) -> Option<Self> {
        match value {
            value if value == Self::Navigation as u8 => Some(Self::Navigation),
            value if value == Self::Process as u8 => Some(Self::Process),
            _ => None,
        }
    }
}

impl StartupFailure {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Configuration => {
                "Haven 无法读取配置，因此没有启动。请修复配置后重新启动；详细原因已写入日志。"
            }
            Self::ApplicationState => {
                "Haven 的核心服务无法初始化，因此没有启动。请根据下方错误详情修复后重新启动。"
            }
            Self::ApplicationBuild => {
                "Haven 无法启动主界面。请检查安装完整性和 WebView2 Runtime 后重新启动；错误原因如下。"
            }
        }
    }
}

pub(crate) const fn should_show_startup_error(is_autostart: bool) -> bool {
    !is_autostart
}

pub(crate) fn format_startup_error(failure: StartupFailure, detail: Option<&str>) -> String {
    let mut message = failure.message().to_string();
    if let Some(detail) = detail {
        let detail = haven_common::error::sanitize_error_text(detail);
        if !detail.is_empty() {
            message.push_str("\n\n");
            message.push_str(&detail);
        }
    }
    message
}

pub(crate) fn show_startup_error(failure: StartupFailure, detail: Option<&str>) {
    let message: Vec<u16> = format_startup_error(failure, detail)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let title: &[u16] = &[72, 97, 118, 101, 110, 0];

    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            MB_ICONERROR, MB_OK, MB_TOPMOST, MessageBoxW,
        };

        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR | MB_OK | MB_TOPMOST,
        );
    }

    #[cfg(not(windows))]
    {
        let _ = (message, title);
    }
}

#[cfg(windows)]
pub(crate) enum WebviewFailureAction {
    Retry,
    Exit,
}

#[cfg(windows)]
pub(crate) fn show_webview_failure(failure: WebviewFailure) -> WebviewFailureAction {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_RETRYCANCEL, MB_TOPMOST, MessageBoxW,
    };

    const ID_RETRY: i32 = 4;
    let message: Vec<u16> = failure
        .message()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let title: &[u16] = &[72, 97, 118, 101, 110, 0];
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR | MB_RETRYCANCEL | MB_TOPMOST,
        )
    };
    if result == ID_RETRY {
        WebviewFailureAction::Retry
    } else {
        WebviewFailureAction::Exit
    }
}

#[cfg(windows)]
#[derive(Default)]
pub(crate) struct WebviewFailureMonitor {
    pending_failure: std::sync::atomic::AtomicU8,
    dialog_open: std::sync::atomic::AtomicBool,
}

#[cfg(windows)]
impl WebviewFailureMonitor {
    pub(crate) fn report(
        self: &std::sync::Arc<Self>,
        app: &tauri::AppHandle,
        failure: WebviewFailure,
    ) {
        use std::sync::atomic::Ordering;
        use tauri::Manager;

        let window = app.get_webview_window("main");
        let visible = window
            .as_ref()
            .and_then(|window| window.is_visible().ok())
            .unwrap_or(false);
        if !visible {
            self.defer(failure);
            return;
        }
        if self.dialog_open.swap(true, Ordering::AcqRel) {
            return;
        }

        let monitor = std::sync::Arc::clone(self);
        let app_for_dialog = app.clone();
        let spawn_result = std::thread::Builder::new()
            .name("haven-webview-failure-dialog".into())
            .spawn(move || {
                match show_webview_failure(failure) {
                    WebviewFailureAction::Retry => {
                        monitor.dialog_open.store(false, Ordering::Release);
                        match app_for_dialog.get_webview_window("main") {
                            Some(window) => {
                                if let Err(error) = window.reload() {
                                    tracing::error!(
                                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                                        "failed to reload the main WebView2 after startup failure"
                                    );
                                    app_for_dialog.exit(1);
                                }
                            }
                            None => app_for_dialog.exit(1),
                        }
                    }
                    WebviewFailureAction::Exit => app_for_dialog.exit(1),
                }
            });
        if let Err(error) = spawn_result {
            self.dialog_open.store(false, Ordering::Release);
            tracing::error!(
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to open a WebView2 startup error dialog"
            );
            app.exit(1);
        }
    }

    pub(crate) fn on_window_focused(self: &std::sync::Arc<Self>, app: tauri::AppHandle) {
        if let Some(failure) = self.take_deferred() {
            self.report(&app, failure);
        }
    }

    fn defer(&self, failure: WebviewFailure) {
        use std::sync::atomic::Ordering;

        let _ = self.pending_failure.compare_exchange(
            0,
            failure as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn take_deferred(&self) -> Option<WebviewFailure> {
        use std::sync::atomic::Ordering;

        WebviewFailure::from_raw(self.pending_failure.swap(0, Ordering::AcqRel))
    }
}

#[cfg(test)]
mod tests {
    use super::{StartupFailure, WebviewFailure, format_startup_error, should_show_startup_error};

    #[test]
    fn fatal_startup_messages_are_actionable_and_do_not_expose_internal_errors() {
        for failure in [
            StartupFailure::Configuration,
            StartupFailure::ApplicationState,
            StartupFailure::ApplicationBuild,
        ] {
            let message = format_startup_error(failure, None);
            assert!(message.contains("Haven"));
            assert!(message.contains("日志") || message.contains("重新启动"));
            assert!(!message.contains("C:\\"));
        }
    }

    #[test]
    fn startup_dialog_includes_sanitized_failure_details() {
        let message = format_startup_error(
            StartupFailure::ApplicationState,
            Some("failed at C:\\Users\\olive\\haven.db with api_key=secret-value"),
        );

        assert!(message.contains("failed at [PATH]"));
        assert!(message.contains("api_key=[REDACTED]"));
        assert!(!message.contains("C:\\Users"));
        assert!(!message.contains("secret-value"));
    }

    #[test]
    fn autostart_failures_do_not_block_on_a_dialog() {
        assert!(!should_show_startup_error(true));
        assert!(should_show_startup_error(false));
    }

    #[test]
    fn webview_failures_offer_reload_and_exit_choices() {
        for failure in [WebviewFailure::Navigation, WebviewFailure::Process] {
            assert!(failure.message().contains("重试加载"));
            assert!(failure.message().contains("退出"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn hidden_autostart_keeps_the_first_webview_failure_for_window_focus() {
        let monitor = super::WebviewFailureMonitor::default();
        monitor.defer(WebviewFailure::Process);
        monitor.defer(WebviewFailure::Navigation);

        assert_eq!(monitor.take_deferred(), Some(WebviewFailure::Process));
        assert_eq!(monitor.take_deferred(), None);
    }
}
