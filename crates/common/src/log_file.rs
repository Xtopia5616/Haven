//! Helpers for locating and reading Haven's date-rolled log files.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Resolve the newest rolling log file for a configured base path.
///
/// The daily appender writes `{stem}.{YYYY-MM-DD}` beside the configured path.
pub fn resolve_current_log_file(log_path: &Path) -> io::Result<Option<PathBuf>> {
    let Some(dir) = log_path.parent() else {
        return Ok(None);
    };
    let Some(stem) = log_path.file_stem() else {
        return Ok(None);
    };
    let prefix = format!("{}.", stem.to_string_lossy());
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::debug!(
                    error = %crate::error::sanitize_error_text(&error.to_string()),
                    "failed to inspect a log directory entry"
                );
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_daily_log_name(&name, &prefix) {
            continue;
        }
        let modified = match entry.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modified) => modified,
            Err(error) => {
                tracing::debug!(
                    error = %crate::error::sanitize_error_text(&error.to_string()),
                    "failed to read log file metadata"
                );
                continue;
            }
        };
        if best
            .as_ref()
            .is_none_or(|(_, current_modified)| modified > *current_modified)
        {
            best = Some((entry.path(), modified));
        }
    }

    Ok(best.map(|(path, _)| path))
}

/// Read the last `max_lines` lines without loading the complete file.
///
/// Reads backwards in fixed-size chunks and caps raw tail bytes at 2 MiB, so
/// memory use does not grow with the total size of the log file.
pub fn read_tail(path: &Path, max_lines: usize) -> io::Result<String> {
    let (buffer, truncated) = read_tail_bytes(path, max_lines)?;
    let mut text = crate::encoding::decode_lossy(&buffer);
    if truncated && let Some((_, complete_tail)) = text.split_once('\n') {
        text = complete_tail.to_owned();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let start = lines.len().saturating_sub(max_lines);
    Ok(lines[start..].join("\n"))
}

/// Read the last `max_lines` entries and the total line count without keeping
/// the full log in memory.
pub fn read_tail_lines_with_count(
    path: &Path,
    max_lines: usize,
) -> io::Result<(Vec<String>, usize)> {
    let total_lines = count_lines(path)?;
    let (buffer, truncated) = read_tail_bytes(path, max_lines)?;
    let mut text = crate::encoding::decode_lossy(&buffer);
    if truncated && let Some((_, complete_tail)) = text.split_once('\n') {
        text = complete_tail.to_owned();
    } else if truncated {
        return Ok((
            (total_lines > 0)
                .then(|| "[redacted diagnostic line]".to_owned())
                .into_iter()
                .collect(),
            total_lines,
        ));
    }
    let lines = text
        .lines()
        .rev()
        .take(max_lines)
        .map(str::to_owned)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok((lines, total_lines))
}

fn read_tail_bytes(path: &Path, max_lines: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 || max_lines == 0 {
        return Ok((Vec::new(), false));
    }

    const CHUNK: u64 = 8192;
    const MAX_TAIL_BYTES: u64 = 2 * 1024 * 1024;
    let mut chunks: Vec<Vec<u8>> = Vec::new();
    let mut pos = file_len;
    let mut line_breaks = 0usize;
    let mut truncated = false;
    loop {
        let read_from = pos
            .saturating_sub(CHUNK)
            .max(file_len.saturating_sub(MAX_TAIL_BYTES));
        let chunk_len = (pos - read_from) as usize;
        file.seek(SeekFrom::Start(read_from))?;
        let mut chunk = vec![0u8; chunk_len];
        file.read_exact(&mut chunk)?;
        line_breaks += chunk.iter().filter(|&&byte| byte == b'\n').count();
        chunks.push(chunk);
        if line_breaks > max_lines || read_from == 0 {
            break;
        }
        if pos - read_from >= MAX_TAIL_BYTES {
            truncated = read_from > 0;
            break;
        }
        pos = read_from;
    }

    let mut buffer = Vec::new();
    for chunk in chunks.into_iter().rev() {
        buffer.extend_from_slice(&chunk);
    }
    Ok((buffer, truncated))
}

fn count_lines(path: &Path) -> io::Result<usize> {
    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len == 0 {
        return Ok(0);
    }

    let mut remaining = file_len;
    let mut buffer = [0u8; 8192];
    let mut line_breaks = 0usize;
    let mut last_byte = None;
    while remaining > 0 {
        let chunk_len = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..chunk_len])?;
        line_breaks += buffer[..chunk_len]
            .iter()
            .filter(|&&byte| byte == b'\n')
            .count();
        last_byte = buffer.get(chunk_len - 1).copied();
        remaining -= chunk_len as u64;
    }

    Ok(line_breaks + usize::from(last_byte != Some(b'\n')))
}

fn is_daily_log_name(name: &str, prefix: &str) -> bool {
    let Some(date) = name.strip_prefix(prefix) else {
        return false;
    };
    date.len() == 10
        && date
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
}
