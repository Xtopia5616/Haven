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
            // `tracing_appender::rolling::daily` panics when its directory
            // cannot be created. Logging must never prevent the desktop
            // app from starting, so keep the console layer and degrade.
            eprintln!(
                "file logging disabled: cannot create {}: {}",
                parent.display(),
                e
            );
            let _ = tracing::subscriber::set_global_default(subscriber);
            let mut effective_cfg = log_cfg.clone();
            effective_cfg.file_enabled = false;
            return (handles, Arc::new(std::sync::Mutex::new(effective_cfg)));
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

/// Convert an error into a bounded, single-line message safe for a UI/event
/// boundary. This is deliberately conservative because many callers pass
/// `anyhow`, `reqwest`, or provider errors whose Display implementation may
/// include URLs, local paths, credentials, or an entire response body.
pub(crate) fn sanitize_error_text(raw: &str) -> String {
    haven_common::error::sanitize_error_text(raw)
}

/// Convert any displayable error into a frontend-facing string while logging
/// it at ERROR level. Replaces the repetitive `.map_err(log_err)` pattern so
/// command failures are never silently swallowed.
///
/// `ctx` identifies the originating Tauri command. Both the returned message
/// and the diagnostic field are sanitized before crossing the command/logging
/// boundary; the raw Display string is intentionally never emitted here.
pub(crate) fn log_err<E: std::fmt::Display>(ctx: &str, e: E) -> String {
    let safe = sanitize_error_text(&e.to_string());
    tracing::error!(command = ctx, "command `{}` failed", ctx);
    tracing::error!(command = ctx, error = %safe, "command error: {}", safe);
    safe
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_tracing_creates_handle() {
        let cfg = LogConfig {
            file_enabled: false,
            ..Default::default()
        };
        let (_handles, _log_cfg) = init_tracing(&cfg);
        let cfg_ref = _log_cfg.lock().unwrap();
        assert_eq!(cfg_ref.level.as_str(), "info");
    }

    #[test]
    fn test_init_tracing_with_file_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = LogConfig {
            file_enabled: true,
            file_path: Some(dir.path().join("haven.log")),
            ..Default::default()
        };
        let (_handles, _log_cfg) = init_tracing(&cfg);
    }

    #[test]
    fn test_init_tracing_disables_file_output_when_parent_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let blocked_parent = dir.path().join("blocked");
        std::fs::write(&blocked_parent, "not a directory").unwrap();
        let cfg = LogConfig {
            file_enabled: true,
            file_path: Some(blocked_parent.join("haven.log")),
            ..Default::default()
        };

        let (_handles, effective_cfg) = init_tracing(&cfg);
        assert!(!effective_cfg.lock().unwrap().file_enabled);
    }

    #[test]
    fn log_err_preserves_safe_message() {
        let msg = log_err("demo_cmd", "boom");
        assert_eq!(msg, "boom");
    }

    #[test]
    fn sanitize_error_text_delegates_to_common() {
        let value = sanitize_error_text(
            "request failed https://example.test/?api_key=sk-secret&client_secret=topsecret at C:\\Users\\olive\\haven.db",
        );
        assert!(!value.contains("sk-secret"));
        assert!(!value.contains("topsecret"));
        assert!(!value.contains("C:\\Users"));
        assert!(value.contains("[REDACTED]"));
        assert!(value.contains("[PATH]"));
    }

    #[test]
    fn sanitize_error_text_keeps_utf8_boundaries() {
        let value = sanitize_error_text(&"错误".repeat(200));
        assert!(value.chars().count() <= 240);
        assert!(value.ends_with('…'));
    }
}
