use crate::app_state::AppState;
use crate::commands::log_err;
use haven_common::config::LogConfig;
use std::sync::Arc;
use tauri::State;

/// True when `name` looks like a rolling daily log file: `{stem}.{YYYY-MM-DD}`.
fn is_daily_log_name(name: &str, prefix: &str) -> bool {
    let Some(rest) = name.strip_prefix(prefix) else {
        return false;
    };
    rest.len() == 10 && rest.bytes().all(|b| b.is_ascii_digit() || b == b'-')
}

/// Resolve the log file the app is currently writing to. `log_path` is the
/// configured `[log] file_path` (or the default). The tracing rolling
/// appender writes `{stem}.{YYYY-MM-DD}` files (e.g. `haven.2026-08-09`) in
/// the same directory, so the newest matching file wins.
fn resolve_current_log_file(log_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = log_path.parent()?;
    let stem = log_path.file_stem()?.to_string_lossy();
    let prefix = format!("{}.", stem);
    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_daily_log_name(&name, &prefix) {
            continue;
        }
        let mtime = entry.metadata().ok().and_then(|m| m.modified().ok());
        let Some(mtime) = mtime else { continue };
        if best.is_none() || mtime > best.as_ref().unwrap().1 {
            best = Some((entry.path(), mtime));
        }
    }
    best.map(|(p, _)| p)
}

/// Read the last `max_lines` lines of a text file. Reads backwards from the
/// end in fixed-size chunks so a multi-MB log file costs O(tail) instead of a
/// full read. Output decoded via `decode_lossy` (UTF-8, GBK fallback).
fn read_tail(path: &std::path::Path, max_lines: usize) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 {
        return Ok(String::new());
    }
    const CHUNK: u64 = 8192;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut pos = file_len;
    let mut lines = 0usize;
    loop {
        let read_from = pos.saturating_sub(CHUNK);
        let chunk_len = (pos - read_from) as usize;
        file.seek(SeekFrom::Start(read_from))?;
        let mut chunk = vec![0u8; chunk_len];
        file.read_exact(&mut chunk)?;
        lines += chunk.iter().filter(|&&b| b == b'\n').count();
        chunks.push(chunk);
        if lines >= max_lines || read_from == 0 {
            break;
        }
        pos = read_from;
    }
    let mut buffer = Vec::new();
    for c in chunks.into_iter().rev() {
        buffer.extend_from_slice(&c);
    }
    let text = haven_common::encoding::decode_lossy(&buffer);
    let mut all_lines: Vec<&str> = text.split('\n').collect();
    // A trailing newline produces a final empty element; drop it so
    // "last N lines" means the last N real lines.
    if all_lines.last() == Some(&"") {
        all_lines.pop();
    }
    let start = all_lines.len().saturating_sub(max_lines);
    Ok(all_lines[start..].join("\n"))
}

#[derive(serde::Serialize)]
pub struct LogTail {
    pub path: String,
    pub content: String,
}

/// Settings page: current log file location + whether file logging is on.
#[tauri::command]
pub fn get_log_info(state: State<'_, Arc<AppState>>) -> Result<serde_json::Value, String> {
    let cfg = state
        .config_loader
        .lock()
        .map_err(|e| log_err("get_log_info", e))?;
    let log_cfg = &cfg.config().log;
    let log_path = log_cfg
        .file_path
        .clone()
        .unwrap_or_else(LogConfig::default_log_path);
    let path = resolve_current_log_file(&log_path).map(|p| p.to_string_lossy().into_owned());
    Ok(serde_json::json!({
        "enabled": log_cfg.file_enabled,
        "level": log_cfg.level.as_str(),
        "path": path,
    }))
}

/// Settings page: read the tail of the current log file (default 200 lines,
/// clamped to [10, 2000]).
#[tauri::command]
pub fn read_log_tail(
    state: State<'_, Arc<AppState>>,
    max_lines: Option<usize>,
) -> Result<LogTail, String> {
    let cfg = state
        .config_loader
        .lock()
        .map_err(|e| log_err("read_log_tail", e))?;
    let log_cfg = &cfg.config().log;
    if !log_cfg.file_enabled {
        return Err(log_err("read_log_tail", "file logging is disabled"));
    }
    let log_path = log_cfg
        .file_path
        .clone()
        .unwrap_or_else(LogConfig::default_log_path);
    let path = resolve_current_log_file(&log_path)
        .ok_or_else(|| log_err("read_log_tail", "no log file found yet"))?;
    let content = read_tail(&path, max_lines.unwrap_or(200).clamp(10, 2000))
        .map_err(|e| log_err("read_log_tail", e))?;
    Ok(LogTail {
        path: path.to_string_lossy().into_owned(),
        content,
    })
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
        assert_eq!(resolved, new);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_current_log_file_ignores_unrelated_files() {
        let dir = temp_log_dir("resolve-ignore");
        std::fs::write(dir.join("other.log"), "x").unwrap();
        std::fs::write(dir.join("haven.txt"), "x").unwrap();
        assert!(resolve_current_log_file(&dir.join("haven.log")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
