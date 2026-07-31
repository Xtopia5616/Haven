use async_trait::async_trait;
use grep_regex::RegexMatcher;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use haven_common::types::RiskLevel;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

const MAX_SNIPPET_CHARS: usize = 200;
/// Content-mode skip cap: files larger than this are not searched (0 = unlimited).
const DEFAULT_MAX_FILE_SIZE: u64 = 100 * 1024 * 1024;
/// Line-range search window cap. Ranges wider than this fall back to a whole-file
/// scan with sink-side line filtering (correct, just slower).
const MAX_WINDOW_BYTES: u64 = 16 * 1024 * 1024;

pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> String {
        "search".into()
    }
    fn description(&self) -> String {
        "Fast file search by name pattern (glob or regex) or full-text content search. Uses ripgrep's search engine with parallel traversal, .gitignore/.ignore support, and binary detection.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["mode"].as_str() {
            Some("content") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "root": { "type": "string", "description": "Root directory to search from" },
                "pattern": { "type": "string", "description": "Filename glob or regex pattern (e.g. *.rs, test_*.py, config\\.json$). In content mode the pattern is a regex; invalid regex falls back to literal substring search." },
                "mode": { "type": "string", "enum": ["filename", "content"], "default": "filename", "description": "Search mode: filename (match file names) or content (full-text grep with line numbers)" },
                "max_depth": { "type": "integer", "description": "Maximum directory depth. 0 = unlimited.", "default": 10 },
                "max_results": { "type": "integer", "description": "Maximum results to return", "default": 50 },
                "ignore_hidden": { "type": "boolean", "description": "Skip hidden files and directories", "default": true },
                "max_file_size": { "type": "integer", "description": "Skip files larger than this many bytes in content mode. 0 = unlimited.", "default": 104857600 },
                "start_line": { "type": "integer", "description": "Content mode: 1-based first line to search within each file (overrides byte scanning)", "default": 1 },
                "end_line": { "type": "integer", "description": "Content mode: 1-based last line to search within each file", "default": 0 }
            },
            "required": ["root", "pattern"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        let root = input["root"].as_str().ok_or_else(|| anyhow::anyhow!("root is required"))?.to_string();
        let pattern_str = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern is required"))?.to_string();
        let mode = input["mode"].as_str().unwrap_or("filename").to_string();
        let max_depth = input["max_depth"].as_i64().unwrap_or(10) as usize;
        let max_results = input["max_results"].as_i64().unwrap_or(50) as usize;
        let ignore_hidden = input["ignore_hidden"].as_bool().unwrap_or(true);
        let max_file_size = input["max_file_size"].as_u64().unwrap_or(DEFAULT_MAX_FILE_SIZE);
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
            output["hint"] = serde_json::Value::String(
                format!("Results hit the max_results cap ({max_results}). Narrow the pattern, add a line range (start_line/end_line), or raise max_results.")
            );
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
fn walk_builder(
    root: &Path,
    max_depth: usize,
    ignore_hidden: bool,
) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .max_depth(if max_depth == 0 { None } else { Some(max_depth) })
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
    (finalize(results, max_results), found_flag.load(Ordering::Relaxed))
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
                );
            } else {
                let sink = CollectingSink::new(
                    entry.path().to_path_buf(),
                    max_results,
                    found_flag.clone(),
                    results.clone(),
                );
                let _ = searcher.search_path(matcher.clone(), entry.path(), sink);
            }
            ignore::WalkState::Continue
        })
    });
    (finalize(results, max_results), found_flag.load(Ordering::Relaxed))
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
) {
    let Ok(Some((start, end))) = line_range_bytes(path, start_line, end_line) else {
        return;
    };
    let window_len = end - start;
    if window_len > MAX_WINDOW_BYTES {
        let sink = CollectingSink::new(path.to_path_buf(), max_results, found_flag.clone(), results.clone())
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
    let sink = CollectingSink::new(path.to_path_buf(), max_results, found_flag.clone(), results.clone())
        .with_line_offset(start_line - 1);
    let _ = searcher.search_slice(matcher.clone(), &bytes, sink);
}

/// Byte range `[start, end)` covering lines `start_line`..=`end_line` (1-based,
/// `end_line` inclusive). Returns `None` when `start_line` is past the last line;
/// when `end_line` is past EOF the range ends at the file length.
fn line_range_bytes(path: &Path, start_line: u64, end_line: u64) -> std::io::Result<Option<(u64, u64)>> {
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
                    return Ok(Some((range_start.unwrap_or(pos + i as u64), pos + i as u64 + 1)));
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
}

impl CollectingSink {
    fn new(
        path: std::path::PathBuf,
        max: usize,
        found_flag: Arc<AtomicBool>,
        results: Arc<Mutex<Vec<Value>>>,
    ) -> Self {
        Self {
            path,
            max,
            found_flag,
            results,
            last_line: None,
            line_offset: 0,
            line_filter: None,
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
        let line_number = mat.line_number().unwrap_or(0).saturating_add(self.line_offset);
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
            .map(|line| snippet_of(&haven_common::encoding::decode_lossy(line)))
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

/// Trim a matched line for a result snippet, keeping the leading newline off.
fn snippet_of(line: &str) -> String {
    let line = line.trim_end();
    if line.len() <= MAX_SNIPPET_CHARS {
        line.to_string()
    } else {
        let cutoff = line.floor_char_boundary(MAX_SNIPPET_CHARS);
        format!("{}…", &line[..cutoff])
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
    use crate::Tool;
    use serde_json::json;

    #[test]
    fn test_search_tool_name() {
        assert_eq!(SearchTool.name(), "search");
    }

    #[test]
    fn test_search_tool_risk_level() {
        assert_eq!(SearchTool.risk_level(&json!({"mode": "filename"})), RiskLevel::Low);
        assert_eq!(SearchTool.risk_level(&json!({"mode": "content"})), RiskLevel::Medium);
    }

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
        assert_eq!(snippet_of("hello world\n"), "hello world");
    }

    #[test]
    fn test_snippet_of_long_truncates_at_boundary() {
        let line = "中".repeat(300);
        let snippet = snippet_of(&line);
        assert!(snippet.len() < line.len());
        assert!(snippet.ends_with('…'));
    }

    #[tokio::test]
    async fn test_search_content_mode_reports_line_numbers() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("a.rs"), "fn alpha() {}\nfn beta() {}\nfn alpha() {}\n")
            .await
            .unwrap();

        let result = SearchTool
            .execute(
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
            assert_eq!(r["path"].as_str().unwrap(), tmp.path().join("a.rs").to_string_lossy());
            assert!(r["line"].as_u64().is_some());
            assert!(r["snippet"].as_str().unwrap().contains("alpha"));
        }
        let lines: Vec<u64> = results.iter().map(|r| r["line"].as_u64().unwrap()).collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[tokio::test]
    async fn test_search_content_mode_line_range() {
        let tmp = tempfile::TempDir::new().unwrap();
        let content = (1..=30)
            .map(|i| format!("line {i}: needle {}", if i % 2 == 0 { "even" } else { "odd" }))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(tmp.path().join("log.txt"), content).await.unwrap();

        let result = SearchTool
            .execute(
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
        let lines: Vec<u64> = results.iter().map(|r| r["line"].as_u64().unwrap()).collect();
        assert_eq!(lines, vec![5, 6, 7, 8, 9, 10]);
    }

    #[tokio::test]
    async fn test_search_content_mode_line_range_wide_falls_back_to_filter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let content = (1..=100)
            .map(|i| format!("row {i}: token {}", if i % 10 == 0 { "found" } else { "x" }))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(tmp.path().join("wide.txt"), content).await.unwrap();

        let result = SearchTool
            .execute(
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
        let lines: Vec<u64> = results.iter().map(|r| r["line"].as_u64().unwrap()).collect();
        assert_eq!(lines, vec![20, 30, 40, 50, 60, 70, 80, 90]);
    }

    #[tokio::test]
    async fn test_search_content_mode_start_line_past_eof_returns_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("short.txt"), "one\ntwo\n").await.unwrap();

        let result = SearchTool
            .execute(
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
        let content = (1..=100).map(|i| format!("hit {i}")).collect::<Vec<_>>().join("\n");
        tokio::fs::write(tmp.path().join("many.txt"), content).await.unwrap();

        let result = SearchTool
            .execute(
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
        assert_eq!(result.output["truncated"].as_bool().unwrap(), true);
        assert!(result.output["hint"].as_str().unwrap().contains("max_results"));
    }

    #[tokio::test]
    async fn test_search_content_mode_regex_pattern() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("log.txt"), "error 123\nwarning 456\nerror 789\n")
            .await
            .unwrap();

        let result = SearchTool
            .execute(
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
        let lines: Vec<u64> = results.iter().map(|r| r["line"].as_u64().unwrap()).collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[tokio::test]
    async fn test_search_content_mode_invalid_regex_falls_back_to_literal() {
        let tmp = tempfile::TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("f.txt"), "foo(bar\nbaz\n").await.unwrap();

        let result = SearchTool
            .execute(
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
        tokio::fs::write(tmp.path().join("text.txt"), "needle here\n").await.unwrap();

        let result = SearchTool
            .execute(
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
        tokio::fs::write(tmp.path().join("big.txt"), vec![b'a'; 10_000]).await.unwrap();
        tokio::fs::write(tmp.path().join("small.txt"), "needle\n").await.unwrap();

        let result = SearchTool
            .execute(
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
        tokio::fs::write(tmp.path().join("alpha.rs"), "x").await.unwrap();
        tokio::fs::write(tmp.path().join("alpha.py"), "x").await.unwrap();
        tokio::fs::write(tmp.path().join("beta.rs"), "x").await.unwrap();

        let result = SearchTool
            .execute(
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
            let mut walker = ignore::WalkBuilder::new(root).build();
            while let Some(entry) = walker.next() {
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
            cancel: CancellationToken::new(),
        });
        let new_elapsed = t1.elapsed();

        assert_eq!(legacy_count, results.len());
        eprintln!(
            "legacy single-thread: {:?}, ripgrep parallel: {:?}, matches: {}",
            legacy_elapsed, new_elapsed, results.len()
        );
        assert!(
            new_elapsed <= legacy_elapsed,
            "ripgrep engine should not be slower (legacy {:?} vs new {:?})",
            legacy_elapsed,
            new_elapsed
        );
    }
}
