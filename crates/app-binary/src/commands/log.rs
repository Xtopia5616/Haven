use crate::app_state::AppState;
use crate::commands::log_err;
use haven_common::config::LogConfig;
use haven_common::log_file::{read_tail, resolve_current_log_file};
use std::sync::Arc;
use tauri::State;

#[derive(serde::Serialize)]
pub struct LogTail {
    pub path: String,
    pub content: String,
}

#[derive(Debug, serde::Serialize)]
pub struct LogInfo {
    pub enabled: bool,
    pub level: String,
    pub path: Option<String>,
}

/// Settings page: current log file location + whether file logging is on.
#[tauri::command]
pub fn get_log_info(state: State<'_, Arc<AppState>>) -> Result<LogInfo, String> {
    let cfg = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|e| log_err("get_log_info", e))?;
    let log_cfg = &cfg.config.log;
    let log_path = log_cfg
        .file_path
        .clone()
        .unwrap_or_else(LogConfig::default_log_path);
    let path = resolve_current_log_file(&log_path)
        .map_err(|e| log_err("get_log_info", e))?
        .map(|p| p.to_string_lossy().into_owned());
    Ok(LogInfo {
        enabled: log_cfg.file_enabled,
        level: log_cfg.level.as_str().to_string(),
        path,
    })
}

/// Settings page: read the tail of the current log file (default 200 lines,
/// clamped to [10, 2000]).
#[tauri::command]
pub fn read_log_tail(
    state: State<'_, Arc<AppState>>,
    max_lines: Option<usize>,
) -> Result<LogTail, String> {
    let cfg = state
        .runtime
        .config_service
        .snapshot()
        .map_err(|e| log_err("read_log_tail", e))?;
    let log_cfg = &cfg.config.log;
    if !log_cfg.file_enabled {
        return Err(log_err("read_log_tail", "file logging is disabled"));
    }
    let log_path = log_cfg
        .file_path
        .clone()
        .unwrap_or_else(LogConfig::default_log_path);
    let path = resolve_current_log_file(&log_path)
        .map_err(|e| log_err("read_log_tail", e))?
        .ok_or_else(|| log_err("read_log_tail", "no log file found yet"))?;
    let content = read_tail(&path, max_lines.unwrap_or(200).clamp(10, 2000))
        .map_err(|e| log_err("read_log_tail", e))?;
    Ok(LogTail {
        path: path.to_string_lossy().into_owned(),
        content,
    })
}

/// Mirror a user-visible renderer error into the backend log. The renderer
/// already normalized the text for the toast; sanitize again at this trust
/// boundary so provider-controlled paths, URLs, and secrets never enter the
/// persistent log verbatim.
#[tauri::command]
pub fn log_frontend_error(message: String) -> Result<(), String> {
    let safe = haven_common::error::sanitize_error_text(&message);
    tracing::error!(
        source = "frontend_notification",
        "frontend error notification: {}",
        safe
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── log viewing helpers ───────────────────────────────────────────────

    fn temp_log_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("haven-logtest-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_read_tail_returns_last_lines() {
        let dir = temp_log_dir("tail-lines");
        let path = dir.join("haven.2026-08-09");
        std::fs::write(&path, "l1\nl2\nl3\nl4\n").unwrap();
        assert_eq!(read_tail(&path, 2).unwrap(), "l3\nl4");
        assert_eq!(read_tail(&path, 100).unwrap(), "l1\nl2\nl3\nl4");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_tail_no_trailing_newline() {
        let dir = temp_log_dir("tail-nonl");
        let path = dir.join("haven.log");
        std::fs::write(&path, "a\nb").unwrap();
        assert_eq!(read_tail(&path, 10).unwrap(), "a\nb");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_tail_empty_and_missing() {
        let dir = temp_log_dir("tail-empty");
        let empty = dir.join("empty.log");
        std::fs::write(&empty, "").unwrap();
        assert_eq!(read_tail(&empty, 10).unwrap(), "");
        assert!(read_tail(&dir.join("nope.log"), 10).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_tail_lossy_decodes_non_utf8() {
        let dir = temp_log_dir("tail-lossy");
        let path = dir.join("haven.2026-08-09");
        // Invalid UTF-8 bytes (0xFF) must not panic; decode_lossy replaces them.
        std::fs::write(&path, b"ok\n\xff\xfe\nlast").unwrap();
        assert_eq!(read_tail(&path, 10).unwrap(), "ok\n\u{FFFD}\u{FFFD}\nlast");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_tail_caps_huge_line_count() {
        let dir = temp_log_dir("tail-cap");
        let path = dir.join("haven.log");
        std::fs::write(
            &path,
            (0..100)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        assert_eq!(read_tail(&path, 10).unwrap().split('\n').count(), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_current_log_file_picks_newest_mtime() {
        let dir = temp_log_dir("resolve");
        let old = dir.join("haven.2026-08-08");
        let new = dir.join("haven.2026-08-09");
        std::fs::write(&old, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        // Pin the old file's mtime in the past so ordering is deterministic.
        // (Windows rejects set_modified on read-only handles; open read-write.)
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let f = std::fs::OpenOptions::new().write(true).open(&old).unwrap();
        f.set_modified(past).unwrap();
        drop(f);

        let resolved = resolve_current_log_file(&dir.join("haven.log")).unwrap();
        assert_eq!(resolved, Some(new));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_current_log_file_ignores_unrelated_files() {
        let dir = temp_log_dir("resolve-ignore");
        std::fs::write(dir.join("other.log"), "x").unwrap();
        std::fs::write(dir.join("haven.txt"), "x").unwrap();
        assert!(
            resolve_current_log_file(&dir.join("haven.log"))
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn log_info_has_a_named_stable_wire_shape() {
        let info = LogInfo {
            enabled: false,
            level: "info".into(),
            path: None,
        };
        assert_eq!(
            serde_json::to_value(info).unwrap(),
            serde_json::json!({
                "enabled": false,
                "level": "info",
                "path": null,
            })
        );
    }
}
