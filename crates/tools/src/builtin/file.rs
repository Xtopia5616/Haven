use async_trait::async_trait;
use haven_common::types::RiskLevel;
use haven_llm::LlmClient;
use haven_llm::types::{ContentPart, LlmMessage, LlmRole};
use serde_json::Value;
use std::path::{Component, Path};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::{Tool, ToolResult};

/// Full-read cap: files larger than this are not read entirely; callers must
/// use `offset`/`limit` (bytes) or `start_line`/`end_line` (lines) instead.
const MAX_READ_BYTES: u64 = 128_000;
/// Default `limit` for byte-mode reads when only `offset` is given.
const DEFAULT_LIMIT_BYTES: u64 = 128_000;
/// Default lines to read in line mode when only `start_line` is given.
const DEFAULT_LINE_SPAN: u64 = 100;
/// Single line too long to buffer safely.
const MAX_LINE_BYTES: usize = 128_000;
/// Default input budget (chars) sent to the summarizer model.
const SUMMARY_INPUT_BUDGET: usize = 60_000;
/// Outer timeout for a summarization LLM call.
const SUMMARY_TIMEOUT_SECS: u64 = 120;
/// Cap on directory entries returned by `list`.
const MAX_LIST_ENTRIES: usize = 1_000;
/// Absolute safety cap for byte-mode reads, regardless of caller `limit`.
const MAX_BYTE_READ: u64 = 16 * 1024 * 1024;

fn sanitize_path(path: &str) -> anyhow::Result<String> {
    let normalized = Path::new(path)
        .components()
        .collect::<std::path::PathBuf>();
    if normalized.components().any(|c| matches!(c, Component::ParentDir)) {
        anyhow::bail!("path traversal detected: '{}'", path);
    }
    Ok(normalized.to_string_lossy().to_string())
}

/// NUL byte in the first sample bytes is a strong binary indicator.
fn looks_like_binary(bytes: &[u8]) -> bool {
    let sample = &bytes[..bytes.len().min(8192)];
    sample.contains(&0)
}

fn binary_result(path: &str, size: u64) -> ToolResult {
    ToolResult::ok(serde_json::json!({
        "binary": true,
        "path": path,
        "size": size,
        "hint": "Binary file. Use search(mode=content) to locate text, or read specific parts with offset/limit."
    }))
}

/// Read a file in full. Refuses files larger than `MAX_READ_BYTES` (A) and
/// rejects binary content (E). Only reads what the output budget can hold,
/// instead of pulling the whole file into memory first.
async fn read_full(path: &str, max_chars: usize) -> anyhow::Result<ToolResult> {
    let meta = tokio::fs::metadata(path).await?;
    let size = meta.len();
    if size > MAX_READ_BYTES {
        return Ok(ToolResult::ok(serde_json::json!({
            "too_large": true,
            "path": path,
            "size": size,
            "hint": format!(
                "File is {} bytes, above the {} byte full-read limit. Read it in parts with offset/limit (bytes) or start_line/end_line (lines), or locate text with search(mode=content).",
                size, MAX_READ_BYTES
            ),
        })));
    }
    // Output is truncated to max_chars anyway; reading more bytes than 4x that
    // (worst-case UTF-8 width) is wasted IO.
    let to_read = (max_chars as u64).saturating_mul(4).min(size).max(1) as usize;
    let mut file = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; to_read];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);
    if looks_like_binary(&buf) {
        return Ok(binary_result(path, size));
    }
    let content = haven_common::encoding::decode_lossy(&buf);
    let (output, truncated) = haven_common::encoding::truncate_output(&content, max_chars);
    let mut result = serde_json::json!({"content": output, "size": size});
    if truncated || (n as u64) < size {
        result["truncated"] = serde_json::Value::Bool(true);
        result["hint"] = serde_json::json!(
            "Output truncated to the max chars budget. Read specific ranges with offset/limit (bytes) or start_line/end_line (lines), or use operation=summary."
        );
    }
    Ok(ToolResult::truncated(result))
}

/// Byte-mode segmented read (B): seek to `offset` and read at most `limit` bytes.
async fn read_bytes(
    path: &str,
    offset: u64,
    limit: u64,
    max_chars: usize,
) -> anyhow::Result<ToolResult> {
    let mut file = tokio::fs::File::open(path).await?;
    let total = file.metadata().await?.len();
    if offset >= total {
        return Ok(ToolResult::ok(serde_json::json!({
            "content": "",
            "offset": offset,
            "read_bytes": 0,
            "total_bytes": total,
            "mode": "bytes",
            "truncated": false,
        })));
    }
    // The output is truncated to max_chars anyway, and the caller-supplied
    // limit is untrusted: cap the allocation and the actual read.
    let effective_limit = limit
        .clamp(1, MAX_BYTE_READ)
        .min((max_chars as u64).saturating_mul(4).max(1))
        .min(total - offset) as usize;
    file.seek(tokio::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; effective_limit];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);
    if looks_like_binary(&buf) {
        return Ok(binary_result(path, total));
    }
    let content = haven_common::encoding::decode_lossy(&buf);
    let (output, text_truncated) = haven_common::encoding::truncate_output(&content, max_chars);
    let read_bytes = n as u64;
    let has_more = offset + read_bytes < total;
    let result = serde_json::json!({
        "content": output,
        "offset": offset,
        "read_bytes": read_bytes,
        "total_bytes": total,
        "mode": "bytes",
        "truncated": has_more || text_truncated,
        "next_offset": offset + read_bytes,
    });
    Ok(if has_more || text_truncated {
        ToolResult::truncated(result)
    } else {
        ToolResult::ok(result)
    })
}

/// Read one line via `fill_buf`/`consume`, never buffering more than `cap`
/// bytes. Returns `Ok(None)` at EOF, else `Ok(Some((bytes, exceeded)))` where
/// `exceeded` is true when the line is longer than `cap` (only the first
/// `cap` bytes were copied and the remainder stays in the reader). Bounds the
/// memory used by pathological single-line files (minified bundles, base64).
async fn read_line_bounded(
    reader: &mut BufReader<tokio::fs::File>,
    buf: &mut Vec<u8>,
    cap: usize,
) -> anyhow::Result<Option<(usize, bool)>> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() { None } else { Some((buf.len(), false)) });
        }
        let remaining = cap.saturating_sub(buf.len());
        if remaining == 0 {
            return Ok(Some((buf.len(), true)));
        }
        let window = &available[..available.len().min(remaining)];
        if let Some(pos) = window.iter().position(|&b| b == b'\n') {
            let take = pos + 1;
            buf.extend_from_slice(&available[..take]);
            reader.consume(take);
            return Ok(Some((buf.len(), false)));
        }
        buf.extend_from_slice(window);
        let n = window.len();
        reader.consume(n);
    }
}

/// Line-mode segmented read (C): return lines `start_line`..=`end_line` (1-based).
async fn read_lines(
    path: &str,
    start_line: u64,
    end_line: u64,
    max_chars: usize,
) -> anyhow::Result<ToolResult> {
    let file = tokio::fs::File::open(path).await?;
    let total = file.metadata().await?.len();
    let mut reader = BufReader::new(file);
    let mut line_buf = Vec::new();
    let mut current: u64 = 1;
    let mut out = String::new();
    let mut last_line: u64 = 0;
    let mut more = false;

    loop {
        let Some((n, exceeded)) = read_line_bounded(&mut reader, &mut line_buf, MAX_LINE_BYTES).await?
        else {
            break;
        };
        if exceeded {
            return Ok(ToolResult::ok(serde_json::json!({
                "error": "line exceeds single-line read limit",
                "path": path,
                "line": current,
                "bytes": n,
                "hint": "Read this file with offset/limit (bytes mode) instead.",
            })));
        }
        if current >= start_line {
            if out.len() + n > max_chars {
                more = true;
                break;
            }
            let decoded = haven_common::encoding::decode_lossy(&line_buf);
            if looks_like_binary(decoded.as_bytes()) {
                return Ok(binary_result(path, total));
            }
            out.push_str(&decoded);
            last_line = current;
        }
        current += 1;
        if current > end_line {
            more = read_line_bounded(&mut reader, &mut line_buf, MAX_LINE_BYTES)
                .await?
                .is_some();
            break;
        }
    }

    if last_line == 0 {
        return Ok(ToolResult::ok(serde_json::json!({
            "content": "",
            "start_line": start_line,
            "end_line": 0,
            "mode": "lines",
            "truncated": false,
        })));
    }
    let (output, text_truncated) = haven_common::encoding::truncate_output(&out, max_chars);
    let truncated = more || text_truncated;
    let result = serde_json::json!({
        "content": output,
        "start_line": start_line,
        "end_line": last_line,
        "mode": "lines",
        "truncated": truncated,
    });
    Ok(if truncated {
        ToolResult::truncated(result)
    } else {
        ToolResult::ok(result)
    })
}

#[derive(Default)]
pub struct FileOpTool {
    /// Summarizer LLM client (the `small_model` endpoint). `None` disables the
    /// `summary` operation with a helpful message.
    summarizer: Option<Arc<dyn LlmClient>>,
}

impl FileOpTool {
    pub fn new(summarizer: Option<Arc<dyn LlmClient>>) -> Self {
        Self { summarizer }
    }
}

#[async_trait]
impl Tool for FileOpTool {
    fn name(&self) -> String {
        "file".into()
    }
    fn description(&self) -> String {
        "File operations: read, write, edit, copy, move, delete, list, summary. read supports three modes: full (default, capped), bytes (offset/limit), and lines (start_line/end_line). summary sends file content (or a start_line/end_line range) to the small_model endpoint and returns a concise summary.".into()
    }

    fn risk_level(&self, input: &Value) -> RiskLevel {
        match input["operation"].as_str() {
            Some("delete") => RiskLevel::High,
            Some("edit") | Some("copy") | Some("write") | Some("move") => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["read", "write", "edit", "copy", "move", "delete", "list", "summary"] },
                "path": { "type": "string" },
                "destination": { "type": "string" },
                "content": { "type": "string" },
                "old_string": { "type": "string", "description": "Text to search for (edit operation)" },
                "new_string": { "type": "string", "description": "Replacement text (edit operation)" },
                "offset": { "type": "integer", "description": "Byte offset to start reading from (bytes mode)", "default": 0, "minimum": 0 },
                "limit": { "type": "integer", "description": "Max bytes to read (bytes mode)", "default": 128_000, "minimum": 1, "maximum": 16777216 },
                "start_line": { "type": "integer", "description": "1-based first line to read or summarize (lines mode / summary)", "default": 1 },
                "end_line": { "type": "integer", "description": "1-based last line to read or summarize (lines mode / summary). When omitted, start_line + 100 lines are read.", "default": 100 },
                "focus": { "type": "string", "description": "Optional focus/topic for summary (summary operation)" },
                "max_chars": { "type": "integer", "description": "Max input characters sent to the summarizer (summary operation)", "default": 60000 }
            },
            "required": ["operation", "path"]
        })
    }

    async fn execute(
        &self,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let op = input["operation"].as_str().unwrap_or("read");
        let path = sanitize_path(input["path"].as_str().unwrap_or(""))?;
        let max_chars = self.max_output_chars();

        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }

        match op {
            "read" => {
                let has_line_args = input.get("start_line").is_some() || input.get("end_line").is_some();
                let has_byte_args = input.get("offset").is_some() || input.get("limit").is_some();
                if has_line_args {
                    let start_line = input.get("start_line")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1)
                        .max(1);
                    let end_line = input.get("end_line")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(start_line + DEFAULT_LINE_SPAN)
                        .max(start_line);
                    read_lines(&path, start_line, end_line, max_chars).await
                } else if has_byte_args {
                    let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
                    let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(DEFAULT_LIMIT_BYTES);
                    read_bytes(&path, offset, limit, max_chars).await
                } else {
                    read_full(&path, max_chars).await
                }
            }
            "write" => {
                let content = input["content"].as_str().unwrap_or("");
                tokio::fs::write(&path, content).await?;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"written": true, "path": path}),
                ))
            }
            "edit" => {
                let old = input["old_string"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("'old_string' is required for edit operation")
                })?;
                let new = input["new_string"].as_str().unwrap_or("");
                // edit rewrites the whole file; refuse files beyond the read cap
                // to avoid loading multi-hundred-MB files into memory.
                let meta = tokio::fs::metadata(&path).await?;
                if meta.len() > MAX_READ_BYTES {
                    anyhow::bail!(
                        "file is {} bytes, above the {} byte edit limit. Locate the text with search(mode=content) and rewrite the file in smaller pieces.",
                        meta.len(),
                        MAX_READ_BYTES
                    );
                }
                let bytes = tokio::fs::read(&path).await?;
                let content = haven_common::encoding::decode_lossy(&bytes);
                let positions: Vec<usize> = content.match_indices(old).map(|(i, _)| i).collect();
                if positions.is_empty() {
                    anyhow::bail!("old_string not found in '{}'", path);
                }
                if positions.len() > 1 {
                    let lines: Vec<usize> = positions.iter().map(|&p| content[..p].matches('\n').count() + 1).collect();
                    let snippet = |pos: usize| -> String {
                        let start = pos.saturating_sub(40);
                        let end = (pos + old.len() + 40).min(content.len());
                        let mut s = String::new();
                        if start > 0 { s.push('…'); }
                        s.push_str(&content[start..end]);
                        if end < content.len() { s.push('…'); }
                        s
                    };
                    let matches: Vec<serde_json::Value> = lines.iter().zip(positions.iter()).map(|(&l, &p)| {
                        serde_json::json!({"line": l, "snippet": snippet(p)})
                    }).collect();
                    return Ok(ToolResult {
                        success: true,
                        output: serde_json::json!({
                            "warning": format!("old_string appears {} times; provide more context in old_string to disambiguate", positions.len()),
                            "matches": matches,
                        }),
                        error: None,
                        truncated: false,
                    });
                }
                let result = content.replace(old, new);
                tokio::fs::write(&path, &result).await?;
                let line = content[..positions[0]].matches('\n').count() + 1;
                Ok(ToolResult::ok(
                    serde_json::json!({"edited": true, "path": path, "line": line}),
                ))
            }
            "copy" => {
                let dest = sanitize_path(input["destination"].as_str().unwrap_or(""))?;
                tokio::fs::copy(&path, &dest).await?;
                if cancel.is_cancelled() {
                    anyhow::bail!("cancelled");
                }
                Ok(ToolResult::ok(
                    serde_json::json!({"copied": true, "from": path, "to": dest}),
                ))
            }
            "move" => {
                let dest = sanitize_path(input["destination"].as_str().unwrap_or(""))?;
                match tokio::fs::rename(&path, &dest).await {
                    Ok(()) => {}
                    // Cross-device rename (e.g. C: → D:) fails with EXDEV;
                    // fall back to copy + remove.
                    Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                        tokio::fs::copy(&path, &dest).await?;
                        tokio::fs::remove_file(&path).await?;
                    }
                    Err(e) => return Err(e.into()),
                }
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(
                    serde_json::json!({"moved": true, "from": path, "to": dest}),
                ))
            }
            "delete" => {
                tokio::fs::remove_file(&path).await?;
                if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                Ok(ToolResult::ok(
                    serde_json::json!({"deleted": true, "path": path}),
                ))
            }
            "list" => {
                let mut entries = tokio::fs::read_dir(&path).await?;
                let mut names = Vec::new();
                let mut truncated = false;
                while let Some(entry) = entries.next_entry().await? {
                    if cancel.is_cancelled() { anyhow::bail!("cancelled"); }
                    if names.len() >= MAX_LIST_ENTRIES {
                        truncated = true;
                        break;
                    }
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();
                let mut result = serde_json::json!({"entries": names, "count": names.len()});
                if truncated {
                    result["truncated"] = serde_json::Value::Bool(true);
                    result["hint"] = serde_json::json!(
                        format!("Directory has more than {} entries; only the first {} are listed.", MAX_LIST_ENTRIES, MAX_LIST_ENTRIES)
                    );
                }
                Ok(ToolResult::ok(result))
            }
            "summary" => {
                let start_line = input.get("start_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .max(1);
                let end_line = input.get("end_line")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let focus = input["focus"].as_str().map(|s| s.to_string());
                let input_budget = input["max_chars"].as_u64().unwrap_or(SUMMARY_INPUT_BUDGET as u64) as usize;
                summarize(&path, start_line, end_line, focus.as_deref(), input_budget, self.summarizer.clone(), cancel.clone()).await
            }
            _ => anyhow::bail!("unknown file operation: {}", op),
        }
    }
}

/// Summarize a file (or a `start_line`..=`end_line` range) using the
/// `small_model` endpoint. Content is read line-streamed (never fully buffered)
/// and capped at `input_budget` chars before the LLM call.
async fn summarize(
    path: &str,
    start_line: u64,
    end_line: u64,
    focus: Option<&str>,
    input_budget: usize,
    summarizer: Option<Arc<dyn LlmClient>>,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    let Some(client) = summarizer else {
        return Ok(ToolResult::ok(serde_json::json!({
            "summary_unavailable": true,
            "path": path,
            "reason": "No small_model endpoint configured. Read the file in parts with start_line/end_line instead.",
        })));
    };

    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    let (content, actual_start, actual_end, size, truncated) =
        read_for_summary(path, start_line, end_line, input_budget).await?;

    if content.is_empty() {
        return Ok(ToolResult::ok(serde_json::json!({
            "summary": "(empty)",
            "path": path,
            "size": size,
            "lines": [actual_start, actual_end],
        })));
    }

    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }

    let mut sys = String::from(
        "You are a summarizer. Summarize the following file content concisely. \
         Focus on the most important points, structure, and notable details. \
         Respond in the same language as the content. Keep the summary under 250 words.",
    );
    if let Some(f) = focus {
        sys.push_str("\nPay special attention to this topic: ");
        sys.push_str(f);
    }

    let messages = vec![
        LlmMessage {
            role: LlmRole::System,
            content: vec![ContentPart::text(sys)],
            tool_call_id: None,
            tool_calls: None,
        },
        LlmMessage {
            role: LlmRole::User,
            content: vec![ContentPart::text(content)],
            tool_call_id: None,
            tool_calls: None,
        },
    ];

    let call = async {
        tokio::time::timeout(
            std::time::Duration::from_secs(SUMMARY_TIMEOUT_SECS),
            client.chat(messages),
        )
        .await
    };

    let response = match call.await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            return Ok(ToolResult {
                success: false,
                output: serde_json::json!({"summary_error": true, "path": path}),
                error: Some(format!("summarizer call failed: {}", e)),
                truncated: false,
            });
        }
        Err(_) => {
            return Ok(ToolResult {
                success: false,
                output: serde_json::json!({"summary_error": true, "path": path}),
                error: Some(format!("summarizer timed out after {}s", SUMMARY_TIMEOUT_SECS)),
                truncated: false,
            });
        }
    };

    let mut result = serde_json::json!({
        "summary": response.text.trim().to_string(),
        "path": path,
        "size": size,
        "lines": [actual_start, actual_end],
        "model": response.model,
    });
    if truncated {
        result["input_truncated"] = serde_json::Value::Bool(true);
        result["hint"] = serde_json::json!(
            "Only part of the file was sent to the summarizer due to the max_chars budget. Use start_line/end_line ranges for full coverage."
        );
    }
    Ok(ToolResult::ok(result))
}

/// Stream a file's lines `start_line`..=`end_line` (1-based; `end_line=0` means
/// to EOF), capped at `max_chars`. Returns content plus actual line bounds.
async fn read_for_summary(
    path: &str,
    start_line: u64,
    end_line: u64,
    max_chars: usize,
) -> anyhow::Result<(String, u64, u64, u64, bool)> {
    let file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let mut reader = BufReader::new(file);
    let mut line_buf = Vec::new();
    let mut current: u64 = 1;
    let mut out = String::new();
    let mut last_line: u64 = 0;
    let mut truncated = false;

    loop {
        let Some((n, exceeded)) = read_line_bounded(&mut reader, &mut line_buf, MAX_LINE_BYTES).await?
        else {
            break;
        };
        if exceeded {
            anyhow::bail!(
                "line {} exceeds the {} byte single-line limit; summarize a narrower range",
                current,
                MAX_LINE_BYTES
            );
        }
        if current >= start_line {
            if out.len() + n > max_chars {
                truncated = true;
                break;
            }
            let decoded = haven_common::encoding::decode_lossy(&line_buf);
            if looks_like_binary(decoded.as_bytes()) {
                anyhow::bail!("cannot summarize a binary file");
            }
            out.push_str(&decoded);
            last_line = current;
        }
        current += 1;
        if end_line > 0 && current > end_line {
            break;
        }
    }

    Ok((out, start_line, if last_line > 0 { last_line } else { start_line }, size, truncated))
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::RiskLevel;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_sanitize_path_normal() {
        let result = sanitize_path("file.txt");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "file.txt");
    }

    #[test]
    fn test_sanitize_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let dotted = tmp.path().join("..").join("file.txt");
        let path_str = dotted.to_string_lossy().to_string();
        let result = sanitize_path(&path_str);
        assert!(result.is_err());
    }

    #[test]
    fn test_sanitize_path_relative() {
        let result = sanitize_path("relative/path/file.txt");
        assert!(result.is_ok());
        assert!(!result.unwrap().contains(".."));
    }

    #[test]
    fn test_file_name() {
        assert_eq!(FileOpTool::default().name(), "file");
    }

    #[test]
    fn test_file_description() {
        assert!(FileOpTool::default().description().contains("File operations"));
        assert!(FileOpTool::default().description().contains("edit"));
    }

    #[test]
    fn test_file_risk_level() {
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "delete"})), RiskLevel::High);
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "write"})), RiskLevel::Medium);
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "edit"})), RiskLevel::Medium);
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "move"})), RiskLevel::Medium);
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "copy"})), RiskLevel::Medium);
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "read"})), RiskLevel::Low);
        assert_eq!(FileOpTool::default().risk_level(&json!({"operation": "list"})), RiskLevel::Low);
    }

    #[test]
    fn test_file_input_schema() {
        let schema = FileOpTool::default().input_schema();
        assert_eq!(schema["type"].as_str().unwrap(), "object");
        let required = schema["required"].as_array().unwrap();
        let req: Vec<&str> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(req.contains(&"operation"));
        assert!(req.contains(&"path"));
        let enum_vals = schema["properties"]["operation"]["enum"].as_array().unwrap();
        let ops: Vec<&str> = enum_vals.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(ops.contains(&"read"));
        assert!(ops.contains(&"write"));
        assert!(ops.contains(&"edit"));
        assert!(ops.contains(&"copy"));
        assert!(ops.contains(&"move"));
        assert!(ops.contains(&"delete"));
        assert!(ops.contains(&"list"));
    }

    #[test]
    fn test_file_read_schema_has_segmented_args() {
        let schema = FileOpTool::default().input_schema();
        let props = &schema["properties"];
        assert!(props["offset"]["type"].as_str().is_some());
        assert!(props["limit"]["type"].as_str().is_some());
        assert!(props["start_line"]["type"].as_str().is_some());
        assert!(props["end_line"]["type"].as_str().is_some());
    }

    #[test]
    fn test_looks_like_binary() {
        assert!(!looks_like_binary(b"hello world\nplain text"));
        assert!(looks_like_binary(b"\x00\x01\x02"));
        assert!(looks_like_binary(b"text with \x00 nul inside"));
    }

    #[tokio::test]
    async fn test_file_execute_read_too_large() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("big.txt");
        tokio::fs::write(&file, vec![b'a'; (MAX_READ_BYTES + 1) as usize])
            .await
            .unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(json!({"operation": "read", "path": path_str}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["too_large"].as_bool().unwrap(), true);
        assert!(result.output["hint"].as_str().unwrap().contains("offset/limit"));
    }

    #[tokio::test]
    async fn test_file_execute_read_binary() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("blob.bin");
        tokio::fs::write(&file, b"\x00\x01\x02\x03binary\x00").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(json!({"operation": "read", "path": path_str}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["binary"].as_bool().unwrap(), true);
    }

    #[tokio::test]
    async fn test_file_execute_read_bytes_mode() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("data.txt");
        tokio::fs::write(&file, "0123456789abcdefghij").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "read", "path": path_str, "offset": 5, "limit": 5}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "56789");
        assert_eq!(result.output["mode"].as_str().unwrap(), "bytes");
        assert_eq!(result.output["offset"].as_u64().unwrap(), 5);
        assert_eq!(result.output["total_bytes"].as_u64().unwrap(), 20);
        assert_eq!(result.output["truncated"].as_bool().unwrap(), true);
    }

    #[tokio::test]
    async fn test_file_execute_read_lines_mode() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        tokio::fs::write(&file, "line1\nline2\nline3\nline4\nline5\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "read", "path": path_str, "start_line": 2, "end_line": 4}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["mode"].as_str().unwrap(), "lines");
        assert_eq!(result.output["start_line"].as_u64().unwrap(), 2);
        assert_eq!(result.output["end_line"].as_u64().unwrap(), 4);
        assert_eq!(result.output["content"].as_str().unwrap(), "line2\nline3\nline4\n");
    }

    #[tokio::test]
    async fn test_file_execute_read_lines_last_chunk_not_truncated() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("lines.txt");
        tokio::fs::write(&file, "line1\nline2\nline3\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "read", "path": path_str, "start_line": 3}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "line3\n");
        assert_eq!(result.output["truncated"].as_bool().unwrap(), false);
    }

    #[tokio::test]
    async fn test_file_execute_read() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("readme.txt");
        tokio::fs::write(&file, "hello world").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(json!({"operation": "read", "path": path_str}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["content"].as_str().unwrap(), "hello world");
    }

    #[tokio::test]
    async fn test_file_execute_write() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("output.txt");
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "write", "path": path_str, "content": "written content"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["written"].as_bool().unwrap());
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "written content");
    }

    #[tokio::test]
    async fn test_file_execute_edit() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("edit.txt");
        tokio::fs::write(&file, "hello\nworld\nfoo\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "edit", "path": path_str, "old_string": "world", "new_string": "there"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output["line"].as_u64().unwrap(), 2);
        let content = tokio::fs::read_to_string(&file).await.unwrap();
        assert_eq!(content, "hello\nthere\nfoo\n");
    }

    #[tokio::test]
    async fn test_file_execute_edit_not_found() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("edit.txt");
        tokio::fs::write(&file, "hello\nworld\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "edit", "path": path_str, "old_string": "nope", "new_string": "x"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_execute_edit_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("edit.txt");
        tokio::fs::write(&file, "foo\nfoo\n").await.unwrap();
        let path_str = file.to_string_lossy().to_string();

        let result = FileOpTool::default().execute(
                json!({"operation": "edit", "path": path_str, "old_string": "foo", "new_string": "bar"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["warning"].as_str().unwrap().contains("2 times"));
        assert_eq!(result.output["matches"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_file_execute_copy() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("dest.txt");
        tokio::fs::write(&src, "copy me").await.unwrap();

        let result = FileOpTool::default().execute(
                json!({
                    "operation": "copy",
                    "path": src.to_string_lossy(),
                    "destination": dst.to_string_lossy()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(src.exists());
        assert!(dst.exists());
        let content = tokio::fs::read_to_string(&dst).await.unwrap();
        assert_eq!(content, "copy me");
    }

    #[tokio::test]
    async fn test_file_execute_move() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("source.txt");
        let dst = tmp.path().join("target.txt");
        tokio::fs::write(&src, "move me").await.unwrap();

        let result = FileOpTool::default().execute(
                json!({
                    "operation": "move",
                    "path": src.to_string_lossy(),
                    "destination": dst.to_string_lossy()
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(!src.exists());
        assert!(dst.exists());
    }

    #[tokio::test]
    async fn test_file_execute_delete() {
        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("to_delete.txt");
        tokio::fs::write(&file, "delete me").await.unwrap();
        assert!(file.exists());

        let result = FileOpTool::default().execute(
                json!({"operation": "delete", "path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output["deleted"].as_bool().unwrap());
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn test_file_execute_list() {
        let tmp = TempDir::new().unwrap();
        tokio::fs::write(tmp.path().join("a.txt"), "a").await.unwrap();
        tokio::fs::write(tmp.path().join("b.txt"), "b").await.unwrap();

        let result = FileOpTool::default().execute(
                json!({"operation": "list", "path": tmp.path().to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        let entries = result.output["entries"].as_array().unwrap();
        let names: Vec<&str> = entries.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[tokio::test]
    async fn test_file_execute_unknown() {
        let result = FileOpTool::default().execute(
                json!({"operation": "unknown", "path": "file.txt"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_file_execute_cancelled() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = FileOpTool::default().execute(json!({"operation": "read", "path": "file.txt"}), cancel)
            .await;
        assert!(result.is_err());
    }
}
