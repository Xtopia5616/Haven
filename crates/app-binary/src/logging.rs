//! Backend logging: tracing subscriber init + Tauri command error helper.
//!
//! See `docs/conventions.md` §1.

use haven_common::config::LogConfig;
use std::sync::Arc;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::reload;

/// Initialize the tracing subscriber with console output and optional rolling
/// file output. A single reloadable filter is applied at the subscriber level
/// so runtime log-level changes affect both console and file output.
pub(crate) fn init_tracing(
    log_cfg: &LogConfig,
) -> (
    Vec<reload::Handle<EnvFilter, Registry>>,
    Arc<std::sync::Mutex<LogConfig>>,
) {
    let level_str = log_cfg.level.as_str();

    // Single reloadable filter applied at the subscriber level — both
    // console and file layers inherit it, so updating the filter at
    // runtime changes both outputs.
    let (reloadable, handle) = reload::Layer::new(EnvFilter::new(format!("haven={}", level_str)));

    let subscriber = tracing_subscriber::registry().with(reloadable);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_line_number(true);

    let subscriber = subscriber.with(fmt_layer);

    let handles = vec![handle];

    if log_cfg.file_enabled {
        let log_path = log_cfg
            .file_path
            .clone()
            .unwrap_or_else(LogConfig::default_log_path);
        if let Some(parent) = log_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            eprintln!("failed to create log directory {}: {}", parent.display(), e);
        }
        let file_appender = tracing_appender::rolling::daily(
            log_path.parent().unwrap_or(std::path::Path::new(".")),
            log_path
                .file_stem()
                .unwrap_or(std::ffi::OsStr::new("haven")),
        );
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(file_appender)
            .with_target(true)
            .with_line_number(true)
            .with_ansi(false);

        let subscriber = subscriber.with(file_layer);
        let _ = tracing::subscriber::set_global_default(subscriber);
    } else {
        let _ = tracing::subscriber::set_global_default(subscriber);
    }

    let log_config = Arc::new(std::sync::Mutex::new(log_cfg.clone()));
    (handles, log_config)
}

/// Convert any displayable error into a frontend-facing string while logging
/// it at ERROR level. Replaces the repetitive `.map_err(log_err)` pattern so
/// command failures are never silently swallowed.
///
/// `ctx` identifies the originating Tauri command and is logged as a
/// separate line so the original `command error: <e>` line is preserved
/// verbatim for log scrapers / dashboards.
pub(crate) fn log_err<E: std::fmt::Display>(ctx: &str, e: E) -> String {
    tracing::error!("command `{}` failed", ctx);
    tracing::error!("command error: {}", e);
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_tracing_creates_handle() {
        let cfg = LogConfig::default();
        let (_handles, _log_cfg) = init_tracing(&cfg);
        let cfg_ref = _log_cfg.lock().unwrap();
        assert_eq!(cfg_ref.level.as_str(), "info");
    }

    #[test]
    fn test_init_tracing_with_file_enabled() {
        let cfg = LogConfig {
            file_enabled: true,
            file_path: Some(std::env::temp_dir().join("haven_test_log")),
            ..Default::default()
        };
        let (_handles, _log_cfg) = init_tracing(&cfg);
    }

    #[test]
    fn log_err_preserves_message() {
        let msg = log_err("demo_cmd", "boom");
        assert_eq!(msg, "boom");
    }
}
