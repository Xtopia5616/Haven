use async_trait::async_trait;
use haven_common::types::RiskLevel;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

pub struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> String {
        "search".into()
    }
    fn description(&self) -> String {
        "Fast file search by name pattern (glob or regex) or full-text content search. Uses parallel traversal with .gitignore/.ignore support.".into()
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
                "pattern": { "type": "string", "description": "Filename glob or regex pattern (e.g. *.rs, test_*.py, config\\.json$)" },
                "mode": { "type": "string", "enum": ["filename", "content"], "default": "filename", "description": "Search mode: filename (match file names) or content (full-text grep)" },
                "max_depth": { "type": "integer", "description": "Maximum directory depth. 0 = unlimited.", "default": 10 },
                "max_results": { "type": "integer", "description": "Maximum results to return", "default": 50 },
                "ignore_hidden": { "type": "boolean", "description": "Skip hidden files and directories", "default": true }
            },
            "required": ["root", "pattern"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        if cancel.is_cancelled() { anyhow::bail!("cancelled"); }

        let root = input["root"].as_str().ok_or_else(|| anyhow::anyhow!("root is required"))?.to_string();
        let pattern_str = input["pattern"].as_str().ok_or_else(|| anyhow::anyhow!("pattern is required"))?.to_string();
        let mode = input["mode"].as_str().unwrap_or("filename").to_string();
        let max_depth = input["max_depth"].as_i64().unwrap_or(10) as usize;
        let max_results = input["max_results"].as_i64().unwrap_or(50) as usize;
        let ignore_hidden = input["ignore_hidden"].as_bool().unwrap_or(true);

        let root_path = std::path::Path::new(&root).to_path_buf();
        if !root_path.exists() {
            anyhow::bail!("root path '{}' does not exist", root);
        }

        let mode_for_closure = mode.clone();
        let cancel_inner = cancel.clone();
        let results = tokio::task::spawn_blocking(move || {
            search_files(&root_path, &pattern_str, &mode_for_closure, max_depth, max_results, ignore_hidden, cancel_inner)
        }).await?;

        Ok(ToolResult::ok(serde_json::json!({
            "results": results,
            "count": results.len(),
            "mode": mode,
        })))
    }
}

fn search_files(
    root: &std::path::Path,
    pattern: &str,
    mode: &str,
    max_depth: usize,
    max_results: usize,
    ignore_hidden: bool,
    cancel: tokio_util::sync::CancellationToken,
) -> Vec<Value> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let found_flag = Arc::new(AtomicBool::new(false));
    let results = std::sync::Mutex::new(Vec::new());
    let max = max_results;
    let pattern_owned = pattern.to_string();

    let walker = ignore::WalkBuilder::new(root)
        .max_depth(if max_depth == 0 { None } else { Some(max_depth) })
        .hidden(ignore_hidden)
        .ignore(true)       // respect .gitignore
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .build();

    match mode {
        "content" => {
            // Full-text content search using ripgrep-style approach
            for entry in walker {
                if cancel.is_cancelled() || found_flag.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    continue;
                }
                let path = entry.path();
                // Quick read and check
                if let Ok(content) = std::fs::read_to_string(path)
                    && content.contains(&pattern_owned)
                {
                    let mut results_guard = results.lock().unwrap();
                    if results_guard.len() >= max {
                        found_flag.store(true, Ordering::Relaxed);
                        break;
                    }
                    results_guard.push(serde_json::json!({
                        "path": path.to_string_lossy(),
                    }));
                }
            }
        }
        _ => {
            // Filename matching with regex
            let re = match regex::Regex::new(&glob_to_regex(&pattern_owned)) {
                Ok(r) => r,
                Err(_) => {
                    // Fall back to simple substring match
                    for entry in walker {
                        if cancel.is_cancelled() || found_flag.load(Ordering::Relaxed) {
                            break;
                        }
                        let entry = match entry {
                            Ok(e) => e,
                            Err(_) => continue,
                        };
                        let name = entry.file_name().to_string_lossy();
                        if name.contains(&pattern_owned) {
                            let mut results_guard = results.lock().unwrap();
                            if results_guard.len() >= max {
                                found_flag.store(true, Ordering::Relaxed);
                                break;
                            }
                            results_guard.push(serde_json::json!({
                                "path": entry.path().to_string_lossy(),
                            }));
                        }
                    }
                    return results.into_inner().unwrap();
                }
            };

            for entry in walker {
                if cancel.is_cancelled() || found_flag.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let name = entry.file_name().to_string_lossy();
                if re.is_match(&name) {
                    let mut results_guard = results.lock().unwrap();
                    if results_guard.len() >= max {
                        found_flag.store(true, Ordering::Relaxed);
                        break;
                    }
                    results_guard.push(serde_json::json!({
                        "path": entry.path().to_string_lossy(),
                    }));
                }
            }
        }
    };

    // Collect and sort by path
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| {
        a["path"].as_str().unwrap_or("").cmp(b["path"].as_str().unwrap_or(""))
    });
    results.truncate(max);
    results
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
}
