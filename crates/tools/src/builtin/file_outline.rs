//! Bounded, dependency-free source outline extraction for the `files` tool.
//!
//! This is intentionally a structural hint rather than a language parser. It
//! reports headings and common declaration lines with stable line numbers so
//! the model can jump to a useful range before requesting file contents.

use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

use crate::ToolResult;

const MAX_SYMBOL_NAME_CHARS: usize = 160;
const MAX_SIGNATURE_CHARS: usize = 240;
const MAX_SYMBOLS: usize = 500;

pub(crate) async fn outline(
    path: &str,
    start_line: u64,
    max_symbols: usize,
    max_line_chars: usize,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    let file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let mut reader = BufReader::new(file);
    let mut line_buf = Vec::new();
    let mut line_number = 1_u64;
    let max_symbols = max_symbols.clamp(1, MAX_SYMBOLS);
    let line_cap = max_line_chars.clamp(256, 128_000);
    let mut symbols = Vec::new();
    let mut truncated = false;
    let mut next_start_line = None;

    loop {
        if cancel.is_cancelled() {
            anyhow::bail!("cancelled");
        }
        let Some((_, _line_was_bounded)) =
            read_line_bounded(&mut reader, &mut line_buf, line_cap).await?
        else {
            break;
        };
        if line_buf.contains(&0) {
            return Ok(ToolResult::ok(json!({
                "binary": true,
                "path": path,
                "size": size,
                "hint": "Binary files do not have a text outline.",
            })));
        }

        if line_number < start_line.max(1) {
            line_number = line_number.saturating_add(1);
            continue;
        }

        let decoded = haven_common::encoding::decode_preview(&line_buf);
        if let Some(symbol) = outline_symbol(&decoded, line_number) {
            if symbols.len() < max_symbols {
                symbols.push(symbol);
            } else {
                truncated = true;
                next_start_line = Some(line_number);
                break;
            }
        }
        line_number = line_number.saturating_add(1);
    }

    let scanned_until = next_start_line
        .map(|line| line.saturating_sub(1))
        .unwrap_or_else(|| line_number.saturating_sub(1).max(start_line));
    let mut output = json!({
        "path": path,
        "symbols": symbols,
        "count": symbols.len(),
        "symbol_count": symbols.len(),
        "max_symbols": max_symbols,
        "truncated": truncated,
        "has_more": truncated,
        "range": { "start_line": start_line, "end_line": scanned_until },
    });
    if let Some(next_start_line) = next_start_line {
        output["next_start_line"] = json!(next_start_line);
        output["next_page"] = json!({ "start_line": next_start_line });
        output["hint"] = json!(
            "Outline limit reached. Continue with operation=outline and the returned next_start_line using a narrower source read if needed."
        );
    }
    Ok(if truncated {
        ToolResult::truncated(output)
    } else {
        ToolResult::ok(output)
    })
}

fn outline_symbol(line: &str, line_number: u64) -> Option<serde_json::Value> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(heading) = trimmed.strip_prefix('#') {
        let name = heading.trim_start_matches('#').trim();
        if !name.is_empty() {
            return Some(symbol(line_number, "heading", name, trimmed));
        }
    }

    let mut declaration = trimmed;
    for prefix in [
        "export default ",
        "export ",
        "default ",
        "pub(crate) ",
        "pub(super) ",
        "pub ",
        "async ",
        "unsafe ",
        "abstract ",
    ] {
        if let Some(rest) = declaration.strip_prefix(prefix) {
            declaration = rest.trim_start();
        }
    }

    for (kind, keyword) in [
        ("function", "fn"),
        ("function", "function"),
        ("class", "class"),
        ("interface", "interface"),
        ("struct", "struct"),
        ("enum", "enum"),
        ("trait", "trait"),
        ("impl", "impl"),
        ("module", "mod"),
        ("type", "type"),
    ] {
        let Some(rest) = declaration.strip_prefix(keyword) else {
            continue;
        };
        if rest.chars().next().is_some_and(is_identifier_tail) {
            continue;
        }
        let name = rest
            .trim_start()
            .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == ':' || ch == '$'))
            .next()
            .filter(|name| !name.is_empty())?;
        return Some(symbol(line_number, kind, name, trimmed));
    }

    None
}

fn is_identifier_tail(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

fn symbol(line: u64, kind: &str, name: &str, signature: &str) -> serde_json::Value {
    json!({
        "line": line,
        "kind": kind,
        "name": name.chars().take(MAX_SYMBOL_NAME_CHARS).collect::<String>(),
        "signature": signature.chars().take(MAX_SIGNATURE_CHARS).collect::<String>(),
    })
}

async fn read_line_bounded(
    reader: &mut BufReader<tokio::fs::File>,
    buf: &mut Vec<u8>,
    cap: usize,
) -> anyhow::Result<Option<(usize, bool)>> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(if buf.is_empty() {
                None
            } else {
                Some((buf.len(), false))
            });
        }
        let remaining = cap.saturating_sub(buf.len());
        if remaining == 0 {
            discard_until_newline(reader).await?;
            return Ok(Some((buf.len(), true)));
        }
        let window_len = available.len().min(remaining);
        if let Some(pos) = available[..window_len]
            .iter()
            .position(|&byte| byte == b'\n')
        {
            let take = pos + 1;
            buf.extend_from_slice(&available[..take]);
            reader.consume(take);
            return Ok(Some((buf.len(), false)));
        }
        buf.extend_from_slice(&available[..window_len]);
        reader.consume(window_len);
    }
}

async fn discard_until_newline(reader: &mut BufReader<tokio::fs::File>) -> anyhow::Result<()> {
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(());
        }
        if let Some(pos) = available.iter().position(|&byte| byte == b'\n') {
            reader.consume(pos + 1);
            return Ok(());
        }
        let len = available.len();
        reader.consume(len);
    }
}
