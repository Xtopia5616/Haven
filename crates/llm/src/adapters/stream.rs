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

/// Largest provider-controlled SSE/JSON-lines frame accepted by the reader.
const MAX_STREAM_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// Bound queued parsed events so slow adapters apply backpressure to the body.
pub(crate) const STREAM_LINE_QUEUE_CAPACITY: usize = 4;

pub(crate) fn line_payload_channel() -> (mpsc::Sender<LinePayload>, mpsc::Receiver<LinePayload>) {
    mpsc::channel(STREAM_LINE_QUEUE_CAPACITY)
}

/// An empty `StreamChunk` — the "no payload" baseline emitted by every
/// adapter's stream unfolding.
pub(crate) fn empty_chunk() -> StreamChunk {
    StreamChunk {
        text: None,
        tool_calls: Vec::new(),
        tool_call_updates: Vec::new(),
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

fn frame_append_fits(current_len: usize, segment_len: usize) -> bool {
    current_len <= MAX_STREAM_FRAME_BYTES && segment_len <= MAX_STREAM_FRAME_BYTES - current_len
}

fn append_frame_segment(frame: &mut Vec<u8>, segment: &[u8]) -> Result<(), LlmError> {
    if !frame_append_fits(frame.len(), segment.len()) {
        return Err(LlmError::InvalidResponse(format!(
            "stream frame exceeds the {} MiB limit",
            MAX_STREAM_FRAME_BYTES / (1024 * 1024)
        )));
    }
    frame.extend_from_slice(segment);
    Ok(())
}

async fn forward_line(line: &[u8], tx: &mpsc::Sender<LinePayload>, mode: LineMode) -> bool {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if line.is_empty() || line.starts_with(':') {
        return true;
    }

    let payload = match mode {
        LineMode::SseDataOnly => {
            let Some(payload) = line.strip_prefix("data: ") else {
                return true;
            };
            payload.trim()
        }
        LineMode::SseOrRaw => line.strip_prefix("data: ").map(str::trim).unwrap_or(line),
    };
    if payload.is_empty() || payload == "[DONE]" {
        return true;
    }

    tracing::trace!("stream payload: {} chars", payload.len());
    tx.send(Ok(payload.to_string())).await.is_ok()
}

/// Spawn a task that reads an HTTP response body as bounded lines and forwards
/// parsed payloads on a bounded queue. A slow consumer stops body polling until
/// it drains the queue; an oversized line fails the stream and drops the body.
pub(crate) fn spawn_line_reader<S>(
    mut byte_stream: S,
    tx: mpsc::Sender<LinePayload>,
    mode: LineMode,
) where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let panic_tx = tx.clone();
        let result = std::panic::AssertUnwindSafe(async {
            let mut frame = Vec::new();
            loop {
                match byte_stream.next().await {
                    Some(Ok(bytes)) => {
                        let mut start = 0;
                        for (end, byte) in bytes.iter().enumerate() {
                            if *byte != b'\n' {
                                continue;
                            }
                            if let Err(error) = append_frame_segment(&mut frame, &bytes[start..end])
                            {
                                drop(byte_stream);
                                let _ = tx.send(Err(error)).await;
                                return;
                            }
                            if !forward_line(&frame, &tx, mode).await {
                                return;
                            }
                            frame.clear();
                            start = end + 1;
                        }
                        if let Err(error) = append_frame_segment(&mut frame, &bytes[start..]) {
                            drop(byte_stream);
                            let _ = tx.send(Err(error)).await;
                            return;
                        }
                    }
                    Some(Err(error)) => {
                        let _ = tx.send(Err(LlmError::from(error))).await;
                        break;
                    }
                    None => {
                        if !frame.is_empty() && !forward_line(&frame, &tx, mode).await {
                            return;
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
            let _ = panic_tx
                .send(Err(LlmError::Unknown("byte stream reader panicked".into())))
                .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn sse_data_only_skips_event_lines_and_comments() {
        let body = "event: message_start\ndata: {\"a\":1}\n\n: comment\n\
                    event: content_block_delta\ndata: {\"b\":2}\n";
        let stream =
            futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(body))]);
        let (tx, mut rx) = line_payload_channel();
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
        let (tx, mut rx) = line_payload_channel();
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
        let (tx, mut rx) = line_payload_channel();
        spawn_line_reader(stream, tx, LineMode::SseDataOnly);

        assert_eq!(rx.recv().await.unwrap().unwrap(), r#"{"a":1}"#);
        assert!(matches!(rx.recv().await, Some(Err(_))));
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn frame_budget_accepts_exact_limit_and_rejects_overflow() {
        assert!(frame_append_fits(MAX_STREAM_FRAME_BYTES, 0));
        assert!(frame_append_fits(MAX_STREAM_FRAME_BYTES - 1, 1));
        assert!(!frame_append_fits(MAX_STREAM_FRAME_BYTES, 1));
        assert!(!frame_append_fits(usize::MAX, 1));
    }

    #[tokio::test]
    async fn oversized_unterminated_frame_returns_an_error() {
        let bytes = bytes::Bytes::from(vec![b'x'; MAX_STREAM_FRAME_BYTES + 1]);
        let stream = futures_util::stream::iter(vec![Ok::<_, reqwest::Error>(bytes)]);
        let (tx, mut rx) = line_payload_channel();
        spawn_line_reader(stream, tx, LineMode::SseOrRaw);

        let error = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("reader should report the oversized frame")
            .expect("reader should send one error")
            .expect_err("oversized frame must not be forwarded");
        assert!(matches!(
            error,
            LlmError::InvalidResponse(message) if message.contains("2 MiB limit")
        ));
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn bounded_queue_backpressures_the_body_reader() {
        let polled_chunks = Arc::new(AtomicUsize::new(0));
        let body = futures_util::stream::iter(std::iter::from_fn({
            let polled_chunks = Arc::clone(&polled_chunks);
            move || {
                let index = polled_chunks.fetch_add(1, Ordering::SeqCst);
                Some(Ok::<_, reqwest::Error>(bytes::Bytes::from(format!(
                    "{index}\n"
                ))))
            }
        }));
        let (tx, rx) = line_payload_channel();
        spawn_line_reader(body, tx, LineMode::SseOrRaw);

        tokio::time::timeout(Duration::from_secs(1), async {
            while polled_chunks.load(Ordering::SeqCst) < STREAM_LINE_QUEUE_CAPACITY + 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader should fill the bounded queue");
        assert_eq!(rx.len(), STREAM_LINE_QUEUE_CAPACITY);
        assert_eq!(
            polled_chunks.load(Ordering::SeqCst),
            STREAM_LINE_QUEUE_CAPACITY + 1,
            "reader may hold one in-flight frame while waiting on a full queue"
        );

        drop(rx);
        tokio::task::yield_now().await;
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
