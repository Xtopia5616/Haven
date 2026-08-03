pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod openai_responses;

pub use anthropic::AnthropicAdapter;
pub use openai::OpenAiAdapter;

use futures_util::FutureExt;
use futures_util::StreamExt;
use haven_common::config::ModelEndpoint;
use tokio::sync::mpsc;

use crate::client::LlmClient;

/// Resolve the wire protocol style for an endpoint. An explicit `api_style`
/// wins; otherwise the style is derived from `provider`.
pub fn api_style_for(endpoint: &ModelEndpoint) -> &str {
    if let Some(style) = &endpoint.api_style
        && !style.is_empty()
    {
        return style;
    }
    match endpoint.provider.as_str() {
        "anthropic" => "anthropic",
        "google" | "gemini" => "gemini",
        _ => "openai-chat",
    }
}

/// Build the protocol adapter for an endpoint.
///
/// Dispatch happens on the resolved `api_style` (see `api_style_for`):
/// - `openai-chat`: OpenAI-compatible `/chat/completions` (OpenAI, Ollama,
///   vLLM, DeepSeek, and most third-party gateways)
/// - `openai-responses`: OpenAI Responses API
/// - `anthropic`: Anthropic Messages API
/// - `gemini`: Google Gemini API
pub fn adapter_for(endpoint: &ModelEndpoint) -> Box<dyn LlmClient> {
    match api_style_for(endpoint) {
        "anthropic" => Box::new(anthropic::AnthropicAdapter::new(endpoint.clone())),
        "gemini" => Box::new(gemini::GeminiAdapter::new(endpoint.clone())),
        "openai-responses" => Box::new(openai_responses::OpenAiResponsesAdapter::new(
            endpoint.clone(),
        )),
        _ => Box::new(openai::OpenAiAdapter::new(endpoint.clone())),
    }
}

// ---------------------------------------------------------------------------
// Shared streaming response reader
// ---------------------------------------------------------------------------

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

/// Spawn a task that reads an HTTP response byte stream line-by-line and
/// forwards parsed payloads on `tx`. Handles SSE (`data: …`) and raw-JSON-lines
/// formats in one pass; the interpretation is selected via `mode`.
pub(crate) fn spawn_line_reader<S>(
    byte_stream: S,
    tx: mpsc::UnboundedSender<String>,
    mode: LineMode,
) where
    S: futures_util::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Unpin + Send + 'static,
{
    tokio::spawn(async move {
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
                                        if tx.send(payload).is_err() {
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
                                    if tx.send(payload).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    Some(Err(_)) | None => {
                        // Flush any remaining buffered data before EOF.
                        let remaining = buf.trim().to_string();
                        if !remaining.is_empty() && remaining != "[DONE]" {
                            match mode {
                                LineMode::SseDataOnly => {
                                    if let Some(payload) = remaining.strip_prefix("data: ") {
                                        let payload = payload.trim().to_string();
                                        if !payload.is_empty() && payload != "[DONE]" {
                                            let _ = tx.send(payload);
                                        }
                                    }
                                }
                                LineMode::SseOrRaw => {
                                    let _ = tx.send(remaining);
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
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_style_explicit_wins() {
        let ep = ModelEndpoint {
            provider: "openai".into(),
            api_style: Some("anthropic".into()),
            ..Default::default()
        };
        assert_eq!(api_style_for(&ep), "anthropic");
    }

    #[test]
    fn api_style_derived_from_provider() {
        let anthropic = ModelEndpoint {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&anthropic), "anthropic");
        let google = ModelEndpoint {
            provider: "google".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&google), "gemini");
        let gemini = ModelEndpoint {
            provider: "gemini".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&gemini), "gemini");
        let openai = ModelEndpoint {
            provider: "openai".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&openai), "openai-chat");
        let unknown = ModelEndpoint {
            provider: "ollama".into(),
            ..Default::default()
        };
        assert_eq!(api_style_for(&unknown), "openai-chat");
    }

    #[test]
    fn adapter_for_dispatches_by_style() {
        let anthropic = ModelEndpoint {
            provider: "anthropic".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&anthropic).style(), "anthropic");
        let gemini = ModelEndpoint {
            provider: "google".into(),
            ..Default::default()
        };
        assert_eq!(adapter_for(&gemini).style(), "gemini");
        let responses = ModelEndpoint {
            api_style: Some("openai-responses".into()),
            ..Default::default()
        };
        assert_eq!(adapter_for(&responses).style(), "openai-responses");
        let openai = ModelEndpoint::default();
        assert_eq!(adapter_for(&openai).style(), "openai-chat");
    }

    #[tokio::test]
    async fn line_reader_sse_data_only_skips_event_lines() {
        use futures_util::stream;

        let body = "event: message_start\ndata: {\"a\":1}\n\n: comment\n\
                    event: content_block_delta\ndata: {\"b\":2}\n";
        let stream = stream::iter(vec![Ok::<_, reqwest::Error>(bytes::Bytes::from(body))]);
        let (tx, mut rx) = mpsc::unbounded_channel();
        spawn_line_reader(stream, tx, LineMode::SseDataOnly);
        let mut got = Vec::new();
        while let Some(p) = rx.recv().await {
            got.push(p);
        }
        assert_eq!(got, vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }
}
