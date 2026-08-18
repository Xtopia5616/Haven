//! Minimal Server-Sent Events (SSE) parser for the MCP Streamable HTTP
//! transport. Supports incremental feeds so a streamed response body can be
//! consumed chunk by chunk without buffering the whole stream.

use serde_json::Value;

/// Incremental SSE parser. Feed raw chunks with [`feed`](SseParser::feed) and
/// collect complete events; the accumulated `data:` payload of each event is
/// parsed as JSON (an MCP JSON-RPC message).
pub struct SseParser {
    buffer: Vec<u8>,
    max_buffer: usize,
}

/// Default upper bound for a single buffered (incomplete) SSE line/event.
/// A hostile or buggy server streaming data without newlines would otherwise
/// grow the buffer without bound; past the cap the partial event is dropped
/// instead. The live value comes from `context_limits.mcp_max_sse_buffer_bytes`.
pub const DEFAULT_MAX_SSE_BUFFER: usize = 2 * 1024 * 1024;

impl Default for SseParser {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_SSE_BUFFER)
    }
}

impl SseParser {
    pub fn new(max_buffer: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer,
        }
    }

    /// Feed a chunk of bytes and return any complete events produced. Partial
    /// events are buffered until the remaining bytes arrive. A partial line
    /// larger than the configured cap is dropped (and the buffer reset) so a
    /// newline-free stream cannot grow the buffer without bound.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Value> {
        if self.buffer.len().saturating_add(chunk.len()) > self.max_buffer {
            self.buffer.clear();
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        let mut consumed = 0usize;
        let mut data_lines: Vec<String> = Vec::new();

        loop {
            let bytes = &self.buffer[consumed..];
            if bytes.is_empty() {
                break;
            }
            // Find the next LF; the line ends at that LF (excluding a leading CR).
            let line_end = match bytes.iter().position(|&b| b == b'\n') {
                Some(rel) => consumed + rel,
                None => break,
            };
            let mut line = &self.buffer[consumed..line_end];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            consumed = line_end + 1;

            let line = std::str::from_utf8(line).unwrap_or("");
            if line.is_empty() {
                // Blank line: dispatch the accumulated event.
                if !data_lines.is_empty() {
                    let data = data_lines.join("\n");
                    if let Ok(value) = serde_json::from_str(&data) {
                        events.push(value);
                    }
                    data_lines.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                // SSE allows an optional single leading space after the colon.
                data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // Comments (`:`) and `event:`/`id:`/`retry:` fields are ignored.
        }

        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_single_event() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        let events = parser.feed(b"event: message\ndata: {\"id\":1,\"result\":{}}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], json!({"id": 1, "result": {}}));
        assert!(parser.buffer.is_empty());
    }

    #[test]
    fn sse_crlf_endings() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        let events = parser.feed(b"data: {\"a\":1}\r\n\r\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0], json!({"a": 1}));
    }

    #[test]
    fn sse_chunked_feed() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        assert!(parser.feed(b"data: {\"id\":7,\"res").is_empty());
        let events = parser.feed(b"ult\":{\"ok\":true}}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["id"], 7);
        assert_eq!(events[0]["result"]["ok"], true);
        assert!(parser.buffer.is_empty());
    }

    #[test]
    fn sse_multiple_data_lines_join() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        let events = parser.feed(b"data: {\"x\":1}\ndata: {\"y\":2}\n\n");
        assert_eq!(events.len(), 0, "joined data is not valid JSON");
    }

    #[test]
    fn sse_multiple_events_in_one_chunk() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        let events = parser.feed(b"data: {\"id\":1}\n\ndata: {\"id\":2}\n\n");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["id"], 1);
        assert_eq!(events[1]["id"], 2);
    }

    #[test]
    fn sse_ignores_comments_and_retry() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        let events = parser.feed(b": keepalive\nretry: 3000\n\ndata: {\"id\":3}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["id"], 3);
    }

    #[test]
    fn sse_ignores_non_json_data() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        let events = parser.feed(b"data: not-json\n\n");
        assert!(events.is_empty());
    }

    #[test]
    fn sse_drops_partial_line_over_cap() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        // Feed a chunk that pushes the buffered partial line past the cap
        // without any newline: the parser must drop it instead of growing
        // the buffer without bound.
        let chunk = vec![b'x'; DEFAULT_MAX_SSE_BUFFER];
        assert!(parser.feed(&chunk).is_empty());
        assert!(parser.feed(&chunk).is_empty());
        assert!(parser.buffer.is_empty());
        // A subsequent well-formed event still parses.
        let events = parser.feed(b"data: {\"id\":9}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["id"], 9);
    }
}
