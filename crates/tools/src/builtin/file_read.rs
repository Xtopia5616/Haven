use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio_util::sync::CancellationToken;

use super::file_classification::classify_by_extension;
use super::file_paths::{binary_result, looks_like_binary};
use crate::ToolResult;

/// Read a text file in full. Multimodal path inputs are deliberately treated
/// as binary; managed media must enter through the `media(asset_id)` tool.
/// Refuses files larger than `max_read_chars` and reads only what the output
/// budget can hold instead of pulling the whole file into memory first.
pub(super) async fn read_full(
    path: &str,
    max_chars: usize,
    max_read_chars: u64,
    cancel: CancellationToken,
) -> anyhow::Result<ToolResult> {
    if cancel.is_cancelled() {
        anyhow::bail!("cancelled");
    }
    let (kind, _mime) = classify_by_extension(path);
    let meta = tokio::fs::metadata(path).await?;
    let size = meta.len();
    if matches!(kind, "image" | "audio" | "video") {
        return Ok(binary_result(path, size));
    }
    if size > max_read_chars {
        // Still return a bounded content prefix (budget-sized read, never the
        // whole file) so callers can see the head and reconstruct if needed.
        let to_read = ((max_chars as u64).saturating_mul(4)).min(size).max(1) as usize;
        let mut file = tokio::fs::File::open(path).await?;
        let mut buf = vec![0u8; to_read];
        let n = file.read(&mut buf).await?;
        buf.truncate(n);
        let decoded = haven_common::encoding::decode_with_encoding(&buf);
        let (output, truncated) = haven_common::encoding::truncate_output(&decoded.text, max_chars);
        let mut result = serde_json::json!({
            "too_large": true,
            "path": path,
            "size": size,
            "content": output,
            "encoding": decoded.encoding,
            "hint": format!(
                "File is {} bytes, above the {} byte full-read limit. The head is included above; read specific ranges with offset/limit (bytes) or start_line/end_line (lines), or locate text with search(mode=content).",
                size, max_read_chars
            ),
        });
        if truncated {
            result["truncated"] = serde_json::Value::Bool(true);
        }
        result["next_offset"] = serde_json::json!(continuation_offset(&buf, &decoded, max_chars,));
        return Ok(ToolResult::truncated(result));
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
    let decoded = haven_common::encoding::decode_with_encoding(&buf);
    let (output, truncated) = haven_common::encoding::truncate_output(&decoded.text, max_chars);
    let is_truncated = truncated || (n as u64) < size;
    let mut result = serde_json::json!({
        "content": output,
        "size": size,
        "encoding": decoded.encoding,
    });
    if is_truncated {
        result["truncated"] = serde_json::Value::Bool(true);
        result["next_offset"] = serde_json::json!(continuation_offset(&buf, &decoded, max_chars,));
        result["hint"] = serde_json::json!(
            "Output truncated to the max chars budget. Continue with operation=read and the returned next_offset, or use start_line/end_line (lines) or operation=summary."
        );
    }
    Ok(if is_truncated {
        ToolResult::truncated(result)
    } else {
        ToolResult::ok(result)
    })
}

/// Return a byte cursor that resumes at the first source character omitted
/// from a bounded decoded prefix. The cursor must be inside the bytes already
/// read when the output character budget, rather than EOF, caused truncation;
/// otherwise a model could resume at EOF and silently lose the remainder.
fn continuation_offset(
    bytes: &[u8],
    decoded: &haven_common::encoding::DecodedText,
    max_chars: usize,
) -> u64 {
    let offset = match decoded.encoding {
        "utf-8" => utf8_offset(bytes, 0, max_chars),
        "utf-8-bom" => utf8_offset(bytes, 3.min(bytes.len()), max_chars),
        "utf-16le" => utf16_offset(bytes, 2.min(bytes.len()), false, max_chars),
        "utf-16be" => utf16_offset(bytes, 2.min(bytes.len()), true, max_chars),
        "gbk" => gbk_offset(bytes, max_chars),
        _ => bytes.len(),
    };
    offset.min(bytes.len()) as u64
}

fn utf8_offset(bytes: &[u8], start: usize, max_chars: usize) -> usize {
    let text = std::str::from_utf8(bytes.get(start..).unwrap_or_default()).unwrap_or_default();
    start
        + text
            .char_indices()
            .nth(max_chars)
            .map(|(index, _)| index)
            .unwrap_or(text.len())
}

fn utf16_offset(bytes: &[u8], start: usize, big_endian: bool, max_chars: usize) -> usize {
    let data = bytes.get(start..).unwrap_or_default();
    let mut char_count = 0;
    let mut index = 0;
    while index + 1 < data.len() {
        if char_count >= max_chars {
            return start + index;
        }
        let unit = if big_endian {
            u16::from_be_bytes([data[index], data[index + 1]])
        } else {
            u16::from_le_bytes([data[index], data[index + 1]])
        };
        index += 2;
        if (0xD800..=0xDBFF).contains(&unit) && index + 1 < data.len() && {
            let next = if big_endian {
                u16::from_be_bytes([data[index], data[index + 1]])
            } else {
                u16::from_le_bytes([data[index], data[index + 1]])
            };
            (0xDC00..=0xDFFF).contains(&next)
        } {
            index += 2;
        }
        char_count += 1;
    }
    start + index
}

fn gbk_offset(bytes: &[u8], max_chars: usize) -> usize {
    let mut char_count = 0;
    let mut index = 0;
    while index < bytes.len() {
        if char_count >= max_chars {
            return index;
        }
        if is_gbk_lead(bytes[index]) && index + 1 < bytes.len() && is_gbk_trail(bytes[index + 1]) {
            index += 2;
        } else {
            index += 1;
        }
        char_count += 1;
    }
    // Do not resume after a dangling lead byte that the decoder represented
    // as a replacement character; replay it together with the next chunk.
    if bytes.last().is_some_and(|byte| is_gbk_lead(*byte)) {
        index.saturating_sub(1)
    } else {
        index
    }
}

fn is_gbk_lead(byte: u8) -> bool {
    (0x81..=0xFE).contains(&byte)
}

fn is_gbk_trail(byte: u8) -> bool {
    (0x40..=0xFE).contains(&byte) && byte != 0x7F
}

/// Byte-mode segmented read (B): seek to `offset` and read at most `limit` bytes.
pub(super) async fn read_bytes(
    path: &str,
    offset: u64,
    limit: u64,
    max_chars: usize,
    max_byte_read: u64,
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
        .clamp(1, max_byte_read)
        .min((max_chars as u64).saturating_mul(4).max(1))
        .min(total - offset) as usize;
    file.seek(tokio::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; effective_limit];
    let n = file.read(&mut buf).await?;
    buf.truncate(n);
    if looks_like_binary(&buf) {
        return Ok(binary_result(path, total));
    }
    let decoded = haven_common::encoding::decode_with_encoding(&buf);
    let content = decoded.text;
    let (output, text_truncated) = haven_common::encoding::truncate_output(&content, max_chars);
    let read_bytes = n as u64;
    let has_more = offset + read_bytes < total;
    let result = serde_json::json!({
        "content": output,
        "offset": offset,
        "read_bytes": read_bytes,
        "total_bytes": total,
        "mode": "bytes",
        "encoding": decoded.encoding,
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
pub(super) async fn read_line_bounded(
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
pub(super) async fn read_lines(
    path: &str,
    start_line: u64,
    end_line: u64,
    max_chars: usize,
    max_line_chars: usize,
) -> anyhow::Result<ToolResult> {
    let file = tokio::fs::File::open(path).await?;
    let total = file.metadata().await?.len();
    let mut reader = BufReader::new(file);
    let mut line_buf = Vec::new();
    let mut current: u64 = 1;
    let mut out = String::new();
    let mut last_line: u64 = 0;
    let mut more = false;
    let mut encoding: Option<&'static str> = None;

    loop {
        let Some((n, exceeded)) =
            read_line_bounded(&mut reader, &mut line_buf, max_line_chars).await?
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
            // Decode before the budget check: decode_lossy expands non-UTF-8
            // (GBK) bytes, so comparing the raw line bytes would under-count
            // and let `out` exceed the budget with truncated=false.
            let decoded = haven_common::encoding::decode_with_encoding(&line_buf);
            if looks_like_binary(decoded.text.as_bytes()) {
                return Ok(binary_result(path, total));
            }
            if out.chars().count() + decoded.text.chars().count() > max_chars {
                more = true;
                break;
            }
            encoding.get_or_insert(decoded.encoding);
            out.push_str(&decoded.text);
            last_line = current;
        }
        current += 1;
        if current > end_line {
            more = read_line_bounded(&mut reader, &mut line_buf, max_chars)
                .await?
                .is_some();
            break;
        }
    }

    if last_line == 0 {
        let mut result = serde_json::json!({
            "content": "",
            "start_line": start_line,
            "end_line": 0,
            "mode": "lines",
            "truncated": more,
        });
        if more {
            result["next_start_line"] = serde_json::json!(start_line);
            result["hint"] = serde_json::json!(
                "The first in-range line exceeds the output budget. Read this file with offset/limit (bytes mode), a narrower line range, or operation=summary."
            );
        }
        return Ok(if more {
            ToolResult::truncated(result)
        } else {
            ToolResult::ok(result)
        });
    }
    // `out` is never larger than max_chars (the budget is checked before each
    // line is appended), so no extra truncation pass is needed here.
    let truncated = more;
    let result = serde_json::json!({
        "content": out,
        "start_line": start_line,
        "end_line": last_line,
        "mode": "lines",
        "encoding": encoding.unwrap_or("empty"),
        "truncated": truncated,
    });
    Ok(if truncated {
        let mut result = result;
        result["next_start_line"] = serde_json::json!(last_line.saturating_add(1));
        result["hint"] = serde_json::json!(
            "Output stopped at the observation budget. Continue with the returned next_start_line using operation=read."
        );
        ToolResult::truncated(result)
    } else {
        ToolResult::ok(result)
    })
}
