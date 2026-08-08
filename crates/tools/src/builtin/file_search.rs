use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{ToolResult};

/// Search engine used by the `files` tool's `search` operation.
pub struct FileSearchEngine {
    /// Snippet chars around each content-mode match.
    pub(crate) snippet_chars: usize,
    /// Upper clamp for `max_results` — untrusted input cannot disable the cap.
    pub(crate) max_results_cap: usize,
    /// Content-mode skip cap: files larger than this are not searched (0 = unlimited).
    pub(crate) max_file_size: u64,
    /// Line-range search window cap in bytes. Ranges wider than this fall
    /// back to a whole-file scan with sink-side line filtering.
    pub(crate) max_window_bytes: u64,
}

impl Default for FileSearchEngine {
    fn default() -> Self {
        Self {
            snippet_chars: 200,
            max_results_cap: 1_000,
            max_file_size: 100 * 1024 * 1024,
            max_window_bytes: 16 * 1024 * 1024,
        }
    }
}

impl FileSearchEngine {
    pub fn new(
        snippet_chars: usize,
        max_results_cap: usize,
        max_file_size: u64,
        max_window_bytes: u64,
    ) -> Self {
        Self {
            snippet_chars,
            max_results_cap,
            max_file_size,
            max_window_bytes,
        }
    }

    pub async fn search(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let root = input["root"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("root is required"))?
            .to_string();
        let pattern_str = input["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))?
            .to_string();
        let mode = input["mode"].as_str().unwrap_or("filename").to_string();
        let max_depth = input["max_depth"].as_i64().unwrap_or(10) as usize;
        // Clamp: negative values would wrap to usize::MAX and disable the
        // result cap entirely.
        let max_results = input["max_results"]
            .as_i64()
            .filter(|v| *v > 0)
            .map(|v| (v as usize).min(self.max_results_cap))
            .unwrap_or(50);
        let ignore_hidden = input["ignore_hidden"].as_bool().unwrap_or(true);
        let max_file_size = input["max_file_size"]
            .as_u64()
            .unwrap_or(self.max_file_size);
        let start_line = input["start_line"].as_u64().unwrap_or(1).max(1);
        // end_line = 0 means "unbounded" (scan to EOF).
        let end_line = match input["end_line"].as_u64() {
            Some(e) if e > 0 => e.max(start_line),
            _ => 0,
        };

        let root_path = std::path::PathBuf::from(&root);
        if !root_path.exists() {
            anyhow::bail!("root path '{}' does not exist", root);
        }

        let mode_for_closure = mode.clone();
        let cancel_inner = cancel.clone();
        let snippet_chars = self.snippet_chars;
        let max_window_bytes = self.max_window_bytes;
        let (results, truncated) = tokio::task::spawn_blocking(move || {
            search_files(SearchParams {
                root: &root_path,
                pattern: &pattern_str,
                mode: &mode_for_closure,
                max_depth,
                max_results,
                ignore_hidden,
                max_file_size,
                start_line,
                end_line,
                snippet_chars,
                max_window_bytes,
                cancel: cancel_inner,
            })
        })
        .await?;

        let mut output = serde_json::json!({
            "results": results,
            "count": results.len(),
            "mode": mode,
        });
        if truncated {
            output["truncated"] = serde_json::Value::Bool(true);
            output["hint"] = serde_json::Value::String(format!(
                "Results hit the max_results cap ({max_results}). Narrow the pattern, add a line range (start_line/end_line), or raise max_results."
            ));
        }
        Ok(ToolResult::ok(output))
    }
}

/// Parameters shared across search modes.
struct SearchParams<'a> {
    root: &'a Path,
    pattern: &'a str,
    mode: &'a str,
    max_depth: usize,
    max_results: usize,
    ignore_hidden: bool,
    max_file_size: u64,
    start_line: u64,
    end_line: u64,
    snippet_chars: usize,
    max_window_bytes: u64,
    cancel: CancellationToken,
}

/// Content-mode search parameters (subset of [`SearchParams`]).
struct ContentSearchParams<'a> {
    root: &'a Path,
    pattern: &'a str,
    max_depth: usize,
    max_results: usize,
    ignore_hidden: bool,
    max_file_size: u64,
    start_line: u64,
    end_line: u64,
    snippet_chars: usize,
    max_window_bytes: u64,
    cancel: CancellationToken,
}

fn search_files(params: SearchParams<'_>) -> (Vec<Value>, bool) {
    match params.mode {
        "content" => search_content_parallel(&ContentSearchParams {
            root: params.root,
            pattern: params.pattern,
            max_depth: params.max_depth,
            max_results: params.max_results,
            ignore_hidden: params.ignore_hidden,
            max_file_size: params.max_file_size,
            start_line: params.start_line,
            end_line: params.end_line,
            snippet_chars: params.snippet_chars,
            max_window_bytes: params.max_window_bytes,
            cancel: params.cancel,
        }),
        _ => search_filenames_parallel(
            params.root,
            params.pattern,
            params.max_depth,
            params.max_results,
            params.ignore_hidden,
            params.cancel,
        ),
    }
}

/// Shared walker configuration honoring ignore rules, depth and hidden filters.
fn walk_builder(root: &Path, max_depth: usize, ignore_hidden: bool) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .max_depth(if max_depth == 0 {
            None
        } else {
            Some(max_depth)
        })
        .hidden(ignore_hidden)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .threads(0); // 0 = auto (num cpus)
    builder
}

fn finalize(results: Arc<Mutex<Vec<Value>>>, max_results: usize) -> Vec<Value> {
    let mut results: Vec<Value> = results.lock().unwrap().drain(..).collect();
    results.sort_by(|a, b| {
        a["path"]
            .as_str()
            .unwrap_or("")
            .cmp(b["path"].as_str().unwrap_or(""))
    });
    results.truncate(max_results);
    results
}

/// Filename matching with parallel traversal. The glob is converted to a
/// regex; if that fails, matching falls back to a substring check.
fn search_filenames_parallel(
    root: &Path,
    pattern: &str,
    max_depth: usize,
    max_results: usize,
    ignore_hidden: bool,
    cancel: CancellationToken,
) -> (Vec<Value>, bool) {
    let found_flag = Arc::new(AtomicBool::new(false));
    let results = Arc::new(Mutex::new(Vec::new()));
    let re = regex::Regex::new(&glob_to_regex(pattern)).ok();
    let glob = pattern.to_string();

    let builder = walk_builder(root, max_depth, ignore_hidden);
    builder.build_parallel().run(|| {
        let re = re.clone();
        let glob = glob.clone();
        let found_flag = found_flag.clone();
        let results = results.clone();
        let cancel = cancel.clone();
        Box::new(move |entry| {
            if cancel.is_cancelled() || found_flag.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return ignore::WalkState::Continue,
            };
            let name = entry.file_name().to_string_lossy();
            let matched = re
                .as_ref()
                .map(|r| r.is_match(&name))
                .unwrap_or_else(|| name.contains(&glob));
            if matched {
                let mut guard = results.lock().unwrap();
                if guard.len() >= max_results {
                    found_flag.store(true, Ordering::Relaxed);
                    return ignore::WalkState::Quit;
                }
                guard.push(serde_json::json!({
                    "path": entry.path().to_string_lossy(),
                }));
            }
            ignore::WalkState::Continue
        })
    });
    (
        finalize(results, max_results),
        found_flag.load(Ordering::Relaxed),
    )
}

/// Full-text search using ripgrep's engine (`grep-searcher`): parallel
/// traversal, memory-mapped reads, regex matching, and NUL-based binary
/// detection. With `start_line`/`end_line`, each file is searched only within
/// that 1-based line range (windowed slice when the range is small, sink-side
/// filtering otherwise). Returns results and whether the result cap was hit.
fn search_content_parallel(p: &ContentSearchParams<'_>) -> (Vec<Value>, bool) {
    let root = p.root;
    let pattern = p.pattern;
    let max_depth = p.max_depth;
    let max_results = p.max_results;
    let ignore_hidden = p.ignore_hidden;
    let max_file_size = p.max_file_size;
    let start_line = p.start_line;
    let end_line = p.end_line;
    let snippet_chars = p.snippet_chars;
    let max_window_bytes = p.max_window_bytes;
    let cancel = p.cancel.clone();
    let matcher = match RegexMatcher::new(pattern) {
        Ok(m) => m,
        Err(_) => {
            // Not a valid regex: treat it as a literal (preserves substring behavior).
            RegexMatcher::new(&regex::escape(pattern)).expect("escaped regex always compiles")
        }
    };
    let searcher = SearcherBuilder::new()
        .line_number(true)
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();

    let line_range = if start_line > 1 || end_line > 0 {
        Some((start_line, end_line))
    } else {
        None
    };

    let found_flag = Arc::new(AtomicBool::new(false));
    let results = Arc::new(Mutex::new(Vec::new()));

    let builder = walk_builder(root, max_depth, ignore_hidden);
    builder.build_parallel().run(|| {
        let matcher = matcher.clone();
        let mut searcher = searcher.clone();
        let found_flag = found_flag.clone();
        let results = results.clone();
        let cancel = cancel.clone();
        Box::new(move |entry| {
            if cancel.is_cancelled() || found_flag.load(Ordering::Relaxed) {
                return ignore::WalkState::Quit;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => return ignore::WalkState::Continue,
            };
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                return ignore::WalkState::Continue;
            }
            if max_file_size > 0
                && let Ok(meta) = entry.metadata()
                && meta.len() > max_file_size
            {
                return ignore::WalkState::Continue;
            }
            if let Some((sl, el)) = line_range {
                search_content_line_range(
                    entry.path(),
                    &matcher,
                    &mut searcher,
                    sl,
                    el,
                    max_results,
                    &found_flag,
                    &results,
                    snippet_chars,
                    max_window_bytes,
                );
            } else {
                let sink = CollectingSink::new(
                    entry.path().to_path_buf(),
                    max_results,
                    found_flag.clone(),
                    results.clone(),
                    snippet_chars,
                );
                let _ = searcher.search_path(matcher.clone(), entry.path(), sink);
            }
            ignore::WalkState::Continue
        })
    });
    (
        finalize(results, max_results),
        found_flag.load(Ordering::Relaxed),
    )
}

/// Search one file restricted to the 1-based line range `[start_line, end_line]`.
/// Uses a windowed slice when the byte span is small; otherwise scans the whole
/// file and filters matches by line number in the sink.
#[allow(clippy::too_many_arguments)]
fn search_content_line_range(
    path: &Path,
    matcher: &grep_regex::RegexMatcher,
    searcher: &mut grep_searcher::Searcher,
    start_line: u64,
    end_line: u64,
    max_results: usize,
    found_flag: &Arc<AtomicBool>,
    results: &Arc<Mutex<Vec<Value>>>,
    snippet_chars: usize,
    max_window_bytes: u64,
) {
    let Ok(Some((start, end))) = line_range_bytes(path, start_line, end_line) else {
        return;
    };
    let window_len = end - start;
    if window_len > max_window_bytes {
        let sink = CollectingSink::new(
            path.to_path_buf(),
            max_results,
            found_flag.clone(),
            results.clone(),
            snippet_chars,
        )
        .with_line_filter(start_line, end_line);
        let _ = searcher.search_path(matcher.clone(), path, sink);
        return;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return;
    };
    use std::io::{Read, Seek, SeekFrom};
    if file.seek(SeekFrom::Start(start)).is_err() {
        return;
    }
    let mut bytes = vec![0u8; window_len as usize];
    let mut filled = 0usize;
    while filled < bytes.len() {
        match file.read(&mut bytes[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    bytes.truncate(filled);
    let sink = CollectingSink::new(
        path.to_path_buf(),
        max_results,
        found_flag.clone(),
        results.clone(),
        snippet_chars,
    )
    .with_line_offset(start_line - 1);
    let _ = searcher.search_slice(matcher.clone(), &bytes, sink);
}

/// Byte range `[start, end)` covering lines `start_line`..=`end_line` (1-based,
/// `end_line` inclusive). Returns `None` when `start_line` is past the last line;
/// when `end_line` is past EOF the range ends at the file length.
fn line_range_bytes(
    path: &Path,
    start_line: u64,
    end_line: u64,
) -> std::io::Result<Option<(u64, u64)>> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let total = file.metadata()?.len();
    let mut buf = [0u8; 65536];
    let mut pos: u64 = 0;
    let mut line: u64 = 1;
    let mut range_start: Option<u64> = None;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for (i, &b) in buf[..n].iter().enumerate() {
            if range_start.is_none() && line == start_line {
                range_start = Some(pos + i as u64);
            }
            if b == b'\n' {
                if line == end_line {
                    return Ok(Some((
                        range_start.unwrap_or(pos + i as u64),
                        pos + i as u64 + 1,
                    )));
                }
                line += 1;
            }
        }
        pos += n as u64;
    }
    match range_start {
        Some(s) => Ok(Some((s, total))),
        None => Ok(None),
    }
}

/// Sink fed by the ripgrep engine: appends one result per matched line.
/// `line_offset` shifts reported line numbers (windowed slice searches), and
/// `line_filter` restricts results to an absolute 1-based line range.
struct CollectingSink {
    path: std::path::PathBuf,
    max: usize,
    found_flag: Arc<AtomicBool>,
    results: Arc<Mutex<Vec<Value>>>,
    last_line: Option<u64>,
    line_offset: u64,
    line_filter: Option<(u64, u64)>,
    snippet_chars: usize,
}

impl CollectingSink {
    fn new(
        path: std::path::PathBuf,
        max: usize,
        found_flag: Arc<AtomicBool>,
        results: Arc<Mutex<Vec<Value>>>,
        snippet_chars: usize,
    ) -> Self {
        Self {
            path,
            max,
            found_flag,
            results,
            last_line: None,
            line_offset: 0,
            line_filter: None,
            snippet_chars,
        }
    }

    fn with_line_offset(mut self, offset: u64) -> Self {
        self.line_offset = offset;
        self
    }

    fn with_line_filter(mut self, start_line: u64, end_line: u64) -> Self {
        self.line_filter = Some((start_line, end_line));
        self
    }
}

impl Sink for CollectingSink {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.found_flag.load(Ordering::Relaxed) {
            return Ok(false);
        }
        let line_number = mat
            .line_number()
            .unwrap_or(0)
            .saturating_add(self.line_offset);
        if let Some((start, end)) = self.line_filter
            && (line_number < start || (end > 0 && line_number > end))
        {
            return Ok(true);
        }
        // A pattern may match several times on the same line; grep reports the
        // line once, so skip duplicate line matches.
        if self.last_line == Some(line_number) {
            return Ok(true);
        }
        let snippet = mat
            .lines()
            .next()
            .map(|l| snippet_of(l, self.snippet_chars))
            .unwrap_or_default();
        let mut guard = self.results.lock().unwrap();
        if guard.len() >= self.max {
            self.found_flag.store(true, Ordering::Relaxed);
            return Ok(false);
        }
        guard.push(serde_json::json!({
            "path": self.path.to_string_lossy(),
            "line": line_number,
            "snippet": snippet,
        }));
        self.last_line = Some(line_number);
        Ok(true)
    }
}

/// Trim a matched line for a result snippet. Never materializes the whole
/// line: only a bounded byte window is decoded, then the text is truncated at
/// a char boundary. A `…` marker is appended when the window cut the line or
/// the decoded text exceeds the snippet cap.
fn snippet_of(line: &[u8], snippet_chars: usize) -> String {
    let windowed = line.len() > snippet_chars * 4;
    let window = &line[..line.len().min(snippet_chars * 4)];
    // decode_preview keeps the valid UTF-8 prefix of a window cut mid-sequence
    // and falls back to GBK for non-UTF-8 (CP936) files.
    let s = haven_common::encoding::decode_preview(window);
    let s = s.trim_end();
    if s.len() <= snippet_chars && !windowed {
        s.to_string()
    } else {
        let cutoff = s.floor_char_boundary(snippet_chars);
        format!("{}…", &s[..cutoff])
    }
}

/// Convert a simple glob pattern (e.g. "*.rs", "test_*.py") to a regex.
fn glob_to_regex(glob: &str) -> String {
    let mut regex = String::with_capacity(glob.len() * 2);
    regex.push('^');
    for ch in glob.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' => regex.push_str("\\."),
            '+' => regex.push_str("\\+"),
            '\\' => regex.push_str("\\\\"),
            '|' => regex.push_str("\\|"),
            '^' => regex.push_str("\\^"),
            '$' => regex.push_str("\\$"),
            '(' => regex.push_str("\\("),
            ')' => regex.push_str("\\)"),
            '[' => regex.push('['),
            ']' => regex.push(']'),
            '{' => regex.push_str("\\{"),
            '}' => regex.push_str("\\}"),
            '!' => regex.push('!'),
            c => regex.push(c),
        }
    }
    regex.push('$');
    regex
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_glob_to_regex() {
        let re = regex::Regex::new(&glob_to_regex("*.rs")).unwrap();
        assert!(re.is_match("main.rs"));
        assert!(re.is_match("lib.rs"));
        assert!(!re.is_match("main.rs.bak"));
        assert!(!re.is_match("test.py"));

        let re2 = regex::Regex::new(&glob_to_regex("test_???.py")).unwrap();
        assert!(re2.is_match("test_abc.py"));
        assert!(!re2.is_match("test_ab.py"));
        assert!(!re2.is_match("test_abcd.py"));
    }

    #[test]
    fn test_snippet_of_short() {
        assert_eq!(snippet_of(b"hello world\n", 200), "hello world");
    }

    #[test]
    fn test_snippet_of_long_truncates_at_boundary() {
        let line = "中".repeat(300);
        let snippet = snippet_of(line.as_bytes(), 200);
        assert!(snippet.len() < line.len());
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn test_snippet_of_truncated_multibyte_prefix() {
        // A byte window cut mid-CJK-sequence must keep only the valid UTF-8
        // prefix and never garble or panic.
        let line = "中".repeat(1000).into_bytes();
        let snippet = snippet_of(&line[..500], 200);
        assert!(snippet.is_char_boundary(snippet.len()));
        assert!(snippet.len() < 500);
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn test_snippet_of_gbk_line_decoded() {
        // "你好世界" in GBK: the snippet must decode to the CJK text, not an
        // empty/ASCII-only prefix.
        let gbk = [0xC4, 0xE3, 0xBA, 0xC3, 0xCA, 0xC0, 0xBD, 0xE7];
        let snippet = snippet_of(&gbk, 200);
        assert_eq!(snippet, "你好世界");
    }

    #[tokio::test]
    async fn test_search_content_mode_reports_line_numbers() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(
            tmp.path().join("a.rs"),
            "fn alpha() {}\nfn beta() {}\nfn alpha() {}\n",
        )
        .await
        .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().to_string_lossy(),
                    "pattern": "alpha",
                    "mode": "content",
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        for r in results {
            assert_eq!(
                r["path"].as_str().unwrap(),
                tmp.path().join("a.rs").to_string_lossy()
            );
            assert!(r["line"].as_u64().is_some());
            assert!(r["snippet"].as_str().unwrap().contains("alpha"));
        }
        let lines: Vec<u64> = results
            .iter()
            .map(|r| r["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[tokio::test]
    async fn test_search_content_mode_line_range() {
        let tmp = tempfile::TempDir::new().unwrap();
        let content = (1..=30)
            .map(|i| {
                format!(
                    "line {i}: needle {}",
                    if i % 2 == 0 { "even" } else { "odd" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(tmp.path().join("log.txt"), content)
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().join("log.txt").to_string_lossy(),
                    "pattern": "needle",
                    "mode": "content",
                    "start_line": 5,
                    "end_line": 10,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        let lines: Vec<u64> = results
            .iter()
            .map(|r| r["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, vec![5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn test_search_content_mode_line_range_wide_falls_back_to_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let content = (1..=100)
            .map(|i| format!("row {i}: token {}", if i % 10 == 0 { "found" } else { "x" }))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(tmp.path().join("wide.txt"), content)
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().join("wide.txt").to_string_lossy(),
                    "pattern": "found",
                    "mode": "content",
                    "start_line": 15,
                    "end_line": 95,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        let lines: Vec<u64> = results
            .iter()
            .map(|r| r["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, vec![20, 30, 40, 50, 60, 70, 80, 90]);
    }

    #[tokio::test]
    async fn test_search_content_mode_start_line_past_eof_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("short.txt"), "one\ntwo\n")
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().join("short.txt").to_string_lossy(),
                    "pattern": "one",
                    "mode": "content",
                    "start_line": 99,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["results"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_search_content_mode_truncation_hint() {
        let tmp = tempfile::TempDir::new().unwrap();
        let content = (1..=100)
            .map(|i| format!("hit {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(tmp.path().join("many.txt"), content)
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().join("many.txt").to_string_lossy(),
                    "pattern": "hit",
                    "mode": "content",
                    "max_results": 10,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["truncated"].as_bool().unwrap());
        assert!(
            result.output["hint"]
                .as_str()
                .unwrap()
                .contains("max_results")
        );
    }

    #[tokio::test]
    async fn test_search_content_mode_regex_pattern() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(
            tmp.path().join("log.txt"),
            "error 123\nwarning 456\nerror 789\n",
        )
        .await
        .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().to_string_lossy(),
                    "pattern": "error\\s+\\d{3}",
                    "mode": "content",
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        let lines: Vec<u64> = results
            .iter()
            .map(|r| r["line"].as_u64().unwrap())
            .collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[tokio::test]
    async fn test_search_content_mode_invalid_regex_falls_back_to_literal() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("f.txt"), "foo(bar\nbaz\n")
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().to_string_lossy(),
                    "pattern": "foo(bar",
                    "mode": "content",
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]["snippet"].as_str().unwrap().contains("foo(bar"));
    }

    #[tokio::test]
    async fn test_search_content_mode_skips_binary_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("bin.exe"), b"\x00\x01\x02\x03MZ\x90\x00")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("text.txt"), "needle here\n")
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().to_string_lossy(),
                    "pattern": "needle",
                    "mode": "content",
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]["path"].as_str().unwrap().ends_with("text.txt"));
    }

    #[tokio::test]
    async fn test_search_content_mode_skips_oversized_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("big.txt"), vec![b'a'; 10_000])
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("small.txt"), "needle\n")
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().to_string_lossy(),
                    "pattern": "needle",
                    "mode": "content",
                    "max_file_size": 1000,
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]["path"].as_str().unwrap().ends_with("small.txt"));
    }

    #[tokio::test]
    async fn test_search_filename_mode_parallel() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("alpha.rs"), "x")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("alpha.py"), "x")
            .await
            .unwrap();
        tokio::fs::write(tmp.path().join("beta.rs"), "x")
            .await
            .unwrap();

        let result = FileSearchEngine::default()
            .search(
                json!({
                    "root": tmp.path().to_string_lossy(),
                    "pattern": "*.rs",
                    "mode": "filename",
                }),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let results = result.output["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
    }

    #[test]
    #[ignore]
    fn bench_old_vs_ripgrep_engine() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        for i in 0..200 {
            let dir = root.join(format!("d{:03}", i));
            std::fs::create_dir_all(&dir).unwrap();
            for j in 0..10 {
                let content = (0..2000)
                    .map(|k| {
                        format!(
                            "line {k}: some log data {}",
                            if k % 97 == 0 { "needle" } else { "filler" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                std::fs::write(dir.join(format!("file_{j}.log")), content).unwrap();
            }
        }

        // Legacy single-threaded scan: whole-file read + decode_lossy + contains.
        let legacy = || {
            let mut found = 0usize;
            let walker = ignore::WalkBuilder::new(root).build();
            for entry in walker {
                let Ok(entry) = entry else { continue };
                if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    continue;
                }
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    let text = haven_common::encoding::decode_lossy(&bytes);
                    found += text.lines().filter(|l| l.contains("needle")).count();
                }
            }
            found
        };

        let t0 = std::time::Instant::now();
        let legacy_count = legacy();
        let legacy_elapsed = t0.elapsed();

        let t1 = std::time::Instant::now();
        let (results, _) = search_content_parallel(&ContentSearchParams {
            root,
            pattern: "needle",
            max_depth: 0,
            max_results: 100_000,
            ignore_hidden: true,
            max_file_size: 0,
            start_line: 1,
            end_line: 0,
            snippet_chars: 200,
            max_window_bytes: 16 * 1024 * 1024,
            cancel: CancellationToken::new(),
        });
        let new_elapsed = t1.elapsed();

        assert_eq!(legacy_count, results.len());
        tracing::debug!(
            "legacy single-thread: {:?}, ripgrep parallel: {:?}, matches: {}",
            legacy_elapsed,
            new_elapsed,
            results.len()
        );
        assert!(
            new_elapsed <= legacy_elapsed,
            "ripgrep engine should not be slower (legacy {:?} vs new {:?})",
            legacy_elapsed,
            new_elapsed
        );
    }
}
