//! Shared response-body line reader and stream chunk baseline.
//!
//! Provider adapters own the meaning of each decoded JSON payload. This module
//! only normalizes the transport framing (SSE or JSON lines) and provides the
//! empty chunk used by their provider-specific state machines.

use futures_util::FutureExt;
use futures_util::StreamExt;
use tokio::sync::mpsc;

use crate::types::LlmError;
use crate::types::StreamChunk;

pub(crate) type LinePayload = Result<String, LlmError>;

/// An empty `StreamChunk` — the "no payload" baseline emitted by every
/// adapter's stream unfolding.
pub(crate) fn empty_chunk() -> StreamChunk {
    StreamChunk {
        text: None,
        tool_calls: Vec::new(),
        finish_reason: None,
        usage: None,
        model: None,
        reasoning: None,
        web_search: None,
        web_search_calls: Vec::new(),
        thinking_blocks: Vec::new(),
    }
}

/// How the shared line reader should interpret each line of the response body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineMode {
    /// Forward only SSE `data: …` payloads; ignore `event:` and comment lines
    /// (Anthropic / OpenAI Responses-style SSE).
    SseDataOnly,
    /// Forward `data: …` payloads, treating any other line as raw JSON
    /// (OpenAI-style SSE with non-standard providers; also tolerates Gemini
    /// gateways that ignore `alt=sse` and return NDJSON).
    SseOrRaw,
}

/// Spawn a session that reads an HTTP response byte stream line-by-line and
/// forwards parsed payloads on `tx`. Handles SSE (`data: …`) and raw-JSON-lines
/// formats in one pass; the interpretation is selected via `mode`.
pub(crate) fn spawn_line_reader<S>(
    byte_stream: S,
    tx: mpsc::UnboundedSender<LinePayload>,
    mode: LineMode,
) where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let panic_tx = tx.clone();
        let result = std::panic::AssertUnwindSafe(async {
            let mut buf = String::new();
            tokio::pin!(byte_stream);
            loop {
                let chunk = byte_stream.next().await;
                match chunk {
                    Some(Ok(bytes)) => {
                        buf.push_str(&String::from_utf8_lossy(&bytes));
                        // Process all complete lines in the buffer.
                        while let Some(newline) = buf.find('\n') {
                            let line = buf[..newline].trim().to_string();
                            buf.drain(..=newline);
                            if line.is_empty() || line.starts_with(':') {
                                continue; // SSE comment or blank line
                            }
                            match mode {
                                LineMode::SseDataOnly => {
                                    if let Some(payload) = line.strip_prefix("data: ") {
                                        let payload = payload.trim().to_string();
                                        if payload.is_empty() || payload == "[DONE]" {
                                            continue;
                                        }
                                        tracing::trace!("stream payload: {} chars", payload.len());
                                        if tx.send(Ok(payload)).is_err() {
                                            return;
                                        }
                                    }
                                    // `event: …` lines carry no payload; skip.
                                }
                                LineMode::SseOrRaw => {
                                    let payload = if let Some(p) = line.strip_prefix("data: ") {
                                        p.trim().to_string()
                                    } else {
                                        line
                                    };
                                    if payload == "[DONE]" || payload.is_empty() {
                                        continue;
                                    }
                                    tracing::trace!("stream payload: {} chars", payload.len());
                                    if tx.send(Ok(payload)).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(error)) => {
                        let _ = tx.send(Err(LlmError::from(error)));
                        break;
                    }
                    None => {
                        // Flush any remaining buffered data before EOF.
                        let remaining = buf.trim().to_string();
                        if !remaining.is_empty() && remaining != "[DONE]" {
                            match mode {
                                LineMode::SseDataOnly => {
                                    if let Some(payload) = remaining.strip_prefix("data: ") {
                                        let payload = payload.trim().to_string();
                                        if !payload.is_empty() && payload != "[DONE]" {
                                            tracing::trace!(
                                                "stream flush: {} chars",
                                                payload.len()
                                            );
                                            let _ = tx.send(Ok(payload));
                                        }
                                    }
                                }
                                LineMode::SseOrRaw => {
                                    tracing::trace!("stream flush: {} chars", remaining.len());
                                    let _ = tx.send(Ok(remaining));
                                }
                            }
                        }
                        break;
                    }
                }
            }
        })
        .catch_unwind()
        .await;
        if let Err(panic) = result {
            tracing::error!(
                "byte stream reader panicked: {:?}",
                panic.downcast_ref::<String>().unwrap_or(&"unknown".into())
            );
            let _ = panic_tx.send(Err(LlmError::Unknown("byte stream reader panicked".into())));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sse_data_only_skips_event_lines_and_comments() {
        let body = "event: message_start\ndata: {\"a\":1}\n\n: comment\n\
                    event: content_block_delta\ndata: {\"b\":2}\n";
        let stream =
            futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(body))]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_line_reader(stream, tx, LineMode::SseDataOnly);
        let mut got = Vec::new();
        while let Some(payload) = rx.recv().await {
            got.push(payload.unwrap());
        }
        assert_eq!(got, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    #[tokio::test]
    async fn sse_or_raw_accepts_raw_lines_and_flushes_eof() {
        let body = "data: {\"a\":1}\n{\"b\":2}\n[DONE]\n{\"c\":3}";
        let stream =
            futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(body))]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_line_reader(stream, tx, LineMode::SseOrRaw);
        let mut got = Vec::new();
        while let Some(payload) = rx.recv().await {
            got.push(payload.unwrap());
        }
        assert_eq!(got, vec![r#"{"a":1}"#, r#"{"b":2}"#, r#"{"c":3}"#]);
    }

    #[tokio::test]
    async fn forwards_transport_errors_instead_of_treating_them_as_eof() {
        let error = reqwest::Client::new()
            .get("http://[::1")
            .send()
            .await
            .expect_err("malformed URL should produce a reqwest error");
        let stream = futures_util::stream::iter(vec![
            Ok::<_, reqwest::Error>(bytes::Bytes::from("data: {\"a\":1}\n\n")),
            Err(error),
        ]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_line_reader(stream, tx, LineMode::SseDataOnly);

        assert_eq!(rx.recv().await.unwrap().unwrap(), r#"{"a":1}"#);
        assert!(matches!(rx.recv().await, Some(Err(_))));
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn empty_chunk_has_no_provider_payload() {
        let chunk = empty_chunk();
        assert!(chunk.text.is_none());
        assert!(chunk.tool_calls.is_empty());
        assert!(chunk.finish_reason.is_none());
        assert!(chunk.usage.is_none());
        assert!(chunk.model.is_none());
        assert!(chunk.reasoning.is_none());
        assert!(chunk.web_search.is_none());
        assert!(chunk.web_search_calls.is_empty());
        assert!(chunk.thinking_blocks.is_empty());
    }
}
