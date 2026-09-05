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

const MAX_PUBLIC_ERROR_LENGTH: usize = 240;

/// Convert an error into a bounded, single-line message safe for a UI/event
/// boundary. This is deliberately conservative because many callers pass
/// `anyhow`, `reqwest`, or provider errors whose Display implementation may
/// include URLs, local paths, credentials, or an entire response body.
pub(crate) fn sanitize_error_text(raw: &str) -> String {
    let normalized = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>();
    let normalized = collapse_whitespace(&normalized);
    let normalized = redact_key_value_segments(&normalized);
    let normalized = redact_prefixed_tokens(&normalized);
    let normalized = redact_windows_paths(&normalized);
    truncate_chars(&normalized, MAX_PUBLIC_ERROR_LENGTH)
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

fn collapse_whitespace(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for ch in value.chars() {
        if ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
    }
    out.trim().to_string()
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut out = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

fn is_boundary(ch: Option<char>) -> bool {
    ch.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

fn redact_key_value_segments(value: &str) -> String {
    const KEYS: &[&str] = &[
        "client_secret",
        "access_token",
        "api_key",
        "api-key",
        "apikey",
        "authorization",
        "password",
        "secret",
        "token",
        "key",
    ];

    let chars = value.chars().collect::<Vec<_>>();
    let lower = chars
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;

    while i < chars.len() {
        let matched = KEYS.iter().find_map(|key| {
            let key_chars = key.chars().collect::<Vec<_>>();
            let end = i + key_chars.len();
            if end > chars.len()
                || !is_boundary(i.checked_sub(1).and_then(|index| lower.get(index).copied()))
                || lower[i..end] != key_chars[..]
            {
                return None;
            }
            Some(key_chars.len())
        });

        let Some(key_len) = matched else {
            out.push(chars[i]);
            i += 1;
            continue;
        };

        let mut value_start = i + key_len;
        while value_start < chars.len() && chars[value_start].is_whitespace() {
            value_start += 1;
        }
        if value_start >= chars.len() || !matches!(chars[value_start], '=' | ':') {
            out.extend(chars[i..i + key_len].iter().copied());
            i += key_len;
            continue;
        }
        value_start += 1;
        while value_start < chars.len() && chars[value_start].is_whitespace() {
            value_start += 1;
        }
        let mut end = value_start;
        while end < chars.len()
            && !matches!(
                chars[end],
                '&' | ' ' | '\t' | '\r' | '\n' | ',' | ';' | '"' | '\'' | ')' | ']' | '}' | '#'
            )
        {
            end += 1;
        }

        out.extend(chars[i..value_start].iter().copied());
        if value_start < end {
            out.push_str("[REDACTED]");
        }
        i = end;
    }
    out
}

fn redact_prefixed_tokens(value: &str) -> String {
    const PREFIXES: &[&str] = &["bearer ", "sk-", "gsk_", "AIza", "xai-", "AKIA"];
    let chars = value.chars().collect::<Vec<_>>();
    let lower = chars
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;

    while i < chars.len() {
        let matched = PREFIXES.iter().find_map(|prefix| {
            let prefix_chars = prefix
                .chars()
                .map(|c| c.to_ascii_lowercase())
                .collect::<Vec<_>>();
            let end = i + prefix_chars.len();
            if end <= chars.len()
                && is_boundary(i.checked_sub(1).and_then(|index| lower.get(index).copied()))
                && lower[i..end] == prefix_chars[..]
            {
                Some(prefix_chars.len())
            } else {
                None
            }
        });
        let Some(prefix_len) = matched else {
            out.push(chars[i]);
            i += 1;
            continue;
        };

        let mut end = i + prefix_len;
        while end < chars.len()
            && !matches!(
                chars[end],
                ' ' | '\t' | '\r' | '\n' | ',' | ';' | '"' | '\'' | ')' | ']' | '}'
            )
        {
            end += 1;
        }
        out.extend(chars[i..i + prefix_len].iter().copied());
        if end > i + prefix_len {
            out.push_str("[REDACTED]");
        }
        i = end;
    }
    out
}

fn redact_windows_paths(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        let drive_path = i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && matches!(chars[i + 2], '\\' | '/');
        let unc_path = i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '\\';
        if (drive_path || unc_path)
            && is_boundary(i.checked_sub(1).and_then(|index| chars.get(index).copied()))
        {
            let mut end = i;
            while end < chars.len()
                && !matches!(
                    chars[end],
                    ' ' | '\t' | '\r' | '\n' | ',' | ';' | '"' | '\'' | ')' | ']' | '}'
                )
            {
                end += 1;
            }
            out.push_str("[PATH]");
            i = end;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
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
    fn sanitize_error_text_redacts_secrets_and_paths() {
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
    fn sanitize_error_text_truncates_without_splitting_utf8() {
        let value = sanitize_error_text(&"错误".repeat(200));
        assert!(value.chars().count() <= MAX_PUBLIC_ERROR_LENGTH);
        assert!(value.ends_with('…'));
    }
}
