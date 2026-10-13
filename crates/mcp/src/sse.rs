//! Minimal Server-Sent Events (SSE) parser for the MCP Streamable HTTP
//! transport. Supports incremental feeds so a streamed response body can be
//! consumed chunk by chunk without buffering the whole stream.

use serde_json::Value;

/// Incremental SSE parser. Feed raw chunks with [`feed`](SseParser::feed) and
/// collect complete events; the accumulated `data:` payload of each event is
/// parsed as JSON (an MCP JSON-RPC message).
pub struct SseParser {
    buffer: Vec<u8>,
    data_lines: Vec<String>,
    event_bytes: usize,
    discarding_line: bool,
    discarding_event: bool,
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
            data_lines: Vec::new(),
            event_bytes: 0,
            discarding_line: false,
            discarding_event: false,
            max_buffer,
        }
    }

    /// Feed a chunk of bytes and return any complete events produced. Partial
    /// events are buffered until the remaining bytes arrive. A partial line
    /// larger than the configured cap is dropped (and the buffer reset) so a
    /// newline-free stream cannot grow the buffer without bound.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Value> {
        let mut events = Vec::new();
        let mut offset = 0;
        while offset < chunk.len() {
            if self.discarding_line {
                match chunk[offset..].iter().position(|byte| *byte == b'\n') {
                    Some(relative_end) => {
                        offset += relative_end + 1;
                        self.discarding_line = false;
                    }
                    None => return events,
                }
                continue;
            }

            let relative_end = chunk[offset..].iter().position(|byte| *byte == b'\n');
            let segment_end = relative_end.map_or(chunk.len(), |end| offset + end);
            let segment = &chunk[offset..segment_end];
            if self.buffer.len().saturating_add(segment.len()) > self.max_buffer {
                self.buffer.clear();
                self.drop_current_event();
                if relative_end.is_some() {
                    offset = segment_end + 1;
                } else {
                    self.discarding_line = true;
                    return events;
                }
                continue;
            }

            self.buffer.extend_from_slice(segment);
            if relative_end.is_some() {
                offset = segment_end + 1;
                let line = std::mem::take(&mut self.buffer);
                self.process_line(&line, &mut events);
            } else {
                break;
            }
        }
        events
    }

    fn process_line(&mut self, bytes: &[u8], events: &mut Vec<Value>) {
        let mut bytes = bytes;
        if bytes.last() == Some(&b'\r') {
            bytes = &bytes[..bytes.len() - 1];
        }
        let line = match std::str::from_utf8(bytes) {
            Ok(line) => line,
            Err(_) => {
                tracing::warn!(
                    line_bytes = bytes.len(),
                    "MCP SSE ignored an invalid UTF-8 line"
                );
                return;
            }
        };

        if line.is_empty() {
            if self.discarding_event {
                self.reset_event();
            } else if !self.data_lines.is_empty() {
                let data = self.data_lines.join("\n");
                match serde_json::from_str(&data) {
                    Ok(value) => events.push(value),
                    Err(_) => tracing::warn!(
                        data_bytes = data.len(),
                        "MCP SSE ignored an invalid JSON event"
                    ),
                }
                self.reset_event();
            }
            return;
        }

        if self.discarding_event {
            return;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            // SSE allows an optional single leading space after the colon.
            let data = rest.strip_prefix(' ').unwrap_or(rest);
            let separator_bytes = usize::from(!self.data_lines.is_empty());
            if self
                .event_bytes
                .saturating_add(separator_bytes)
                .saturating_add(data.len())
                > self.max_buffer
            {
                self.drop_current_event();
                return;
            }
            self.event_bytes += separator_bytes + data.len();
            self.data_lines.push(data.to_string());
        }
        // Comments (`:`) and `event:`/`id:`/`retry:` fields are ignored.
    }

    fn drop_current_event(&mut self) {
        self.data_lines.clear();
        self.event_bytes = 0;
        self.discarding_event = true;
    }

    fn reset_event(&mut self) {
        self.data_lines.clear();
        self.event_bytes = 0;
        self.discarding_event = false;
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
    fn sse_data_lines_survive_chunk_boundaries() {
        let mut parser = SseParser::new(DEFAULT_MAX_SSE_BUFFER);
        assert!(parser.feed(b"data: {\n").is_empty());
        let events = parser.feed(b"data: \"id\":7}\n\n");
        assert_eq!(events, vec![json!({"id": 7})]);
    }

    #[test]
    fn sse_large_chunk_with_many_small_events_is_parsed_incrementally() {
        let mut parser = SseParser::new(32);
        let events = parser.feed(
            b"data: {\"id\":1}\n\ndata: {\"id\":2}\n\ndata: {\"id\":3}\n\ndata: {\"id\":4}\n\n",
        );
        assert_eq!(
            events,
            vec![
                json!({"id": 1}),
                json!({"id": 2}),
                json!({"id": 3}),
                json!({"id": 4})
            ]
        );
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
        // Finish the oversized line and its event boundary; only after the
        // blank-line delimiter can the parser safely resume event parsing.
        assert!(parser.feed(b"\n").is_empty());
        assert!(parser.feed(b"\n").is_empty());
        // A subsequent well-formed event still parses.
        let events = parser.feed(b"data: {\"id\":9}\n\n");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["id"], 9);
    }
}
