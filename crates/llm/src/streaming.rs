use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use haven_common::types::{CanonicalMessage, ContentPart};
use tokio::sync::RwLock;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::client::{LlmClient, retry_delay};
use crate::request_pipeline::RetryPolicy;
use crate::stream_rules::{StreamRule, StreamRuleMode, check_stream_rules};
use crate::types::{FinishReason, LlmError, LlmResponse, StreamChunk, ToolDefinition};

/// Budget for the FIRST chunk of a stream, applied before any data has
/// arrived: providers run server-side "thinking" and may delay the first
/// delta well beyond the data-gap idle timeout. A stream that stays silent
/// past this is treated as dead (the total-duration deadline still bounds
/// everything).
const FIRST_CHUNK_GRACE: Duration = Duration::from_secs(60);

/// Extra data-gap idle budget granted per ~1k estimated prompt tokens.
/// Providers decode against the whole context: on long conversations the
/// gap between deltas legitimately grows (server-side thinking, slow
/// decode), so a fixed `stream_idle_timeout_secs` aborts slow-but-alive
/// streams mid-answer and the UI looks frozen ("streaming stuck"). The idle
/// window is therefore scaled
/// with the request size and capped so a genuinely dead stream still
/// surfaces within a bounded window.
const IDLE_EXTRA_SECS_PER_1K_TOKENS: u64 = 2;

/// Hard cap on the scaled data-gap idle window (base + context extra).
pub(crate) const IDLE_SCALE_CAP_SECS: u64 = 90;

/// Conversation data shared by repeated streaming attempts.
#[derive(Clone, Copy)]
pub(crate) struct StreamContext<'a> {
    pub(crate) messages: &'a [CanonicalMessage],
    pub(crate) tools: &'a [ToolDefinition],
    pub(crate) max_output_tokens: Option<u32>,
}

/// Rough prompt-size estimate in tokens (text chars / 4, ~1k per image or
/// audio part, tool-call arguments and echoed reasoning included). Only
/// used to scale stream idle timeouts — exact counting is the provider's
/// action.
pub(crate) fn estimate_prompt_tokens(messages: &[CanonicalMessage]) -> u64 {
    let mut total: u64 = 0;
    for m in messages {
        for part in &m.content {
            match part {
                ContentPart::Text(t) => total += (t.chars().count() as u64) / 4,
                ContentPart::Image { .. } | ContentPart::Audio { .. } => total += 1_000,
            }
        }
        if let Some(reasoning) = &m.reasoning {
            total += (reasoning.chars().count() as u64) / 4;
        }
        // Anthropic thinking text is carried as raw `thinking_blocks` when the
        // redundant `reasoning` copy is dropped; count it either way.
        for block in &m.thinking_blocks {
            if let Some(t) = block.get("thinking").and_then(serde_json::Value::as_str) {
                total += (t.chars().count() as u64) / 4;
            }
        }
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                // Serialize into a counting sink: the arguments are JSON
                // `Value`s and this runs per step, so a temporary String is
                // pure allocation for a length probe.
                let mut counter = CountingWriter(0);
                let _ = serde_json::to_writer(&mut counter, &c.arguments);
                total += (counter.0 as u64) / 4;
            }
        }
    }
    total
}

/// Byte-counting `io::Write` sink used by [`estimate_prompt_tokens`] to
/// measure serialized JSON length without allocating.
struct CountingWriter(usize);

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Scale a base stream idle timeout by the estimated prompt size of the
/// request. Identity for small/empty prompts; grows 2s per ~1k tokens up to
/// `IDLE_SCALE_CAP_SECS` total. The first-chunk grace already tolerates a
/// slow prefill, so this targets the mid-stream data gaps that long
/// contexts make slower.
pub(crate) fn scale_stream_idle(base: Duration, messages: &[CanonicalMessage]) -> Duration {
    let base_secs = base.as_secs();
    let est_tokens = estimate_prompt_tokens(messages);
    let extra_secs = (est_tokens / 1_000).saturating_mul(IDLE_EXTRA_SECS_PER_1K_TOKENS);
    let cap_extra = IDLE_SCALE_CAP_SECS.saturating_sub(base_secs);
    Duration::from_secs(base_secs.saturating_add(extra_secs.min(cap_extra)).max(1))
}

pub(crate) async fn aggregate_stream_with_retry_before_output(
    client: Arc<dyn LlmClient>,
    context: StreamContext<'_>,
    on_chunk: Arc<StdMutex<impl FnMut(&StreamChunk) + Send + 'static>>,
    cancel: CancellationToken,
    stream_rules: &RwLock<Vec<StreamRule>>,
    idle_timeout: Duration,
    retry: RetryPolicy,
) -> Result<LlmResponse, LlmError> {
    for attempt in 0..=retry.max_retries {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let emitted = Arc::new(AtomicBool::new(false));
        let callback = {
            let on_chunk = on_chunk.clone();
            let emitted = emitted.clone();
            Arc::new(StdMutex::new(move |chunk: &StreamChunk| {
                emitted.store(true, Ordering::SeqCst);
                let mut callback = on_chunk.lock().unwrap();
                callback(chunk);
            }))
        };
        let result = aggregate_stream_cancellable(
            client.clone(),
            context.messages.to_vec(),
            context.tools.to_vec(),
            callback,
            cancel.clone(),
            stream_rules,
            idle_timeout,
            context.max_output_tokens,
        )
        .await;
        let Err(err) = result else {
            return result;
        };
        if !err.is_retryable() || emitted.load(Ordering::SeqCst) || attempt == retry.max_retries {
            return Err(err);
        }
        let delay = retry_delay(
            retry.base_secs,
            retry.factor,
            retry.max_secs,
            retry.jitter,
            attempt,
            err.retry_after(),
        );
        tracing::debug!(
            "stream attempt {}/{} failed before output, retrying after {:?}: {}",
            attempt + 1,
            retry.max_retries + 1,
            delay,
            err
        );
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        }
    }
    Err(LlmError::Unknown("stream retry loop exhausted".into()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn aggregate_stream_cancellable(
    client: Arc<dyn LlmClient>,
    messages: Vec<CanonicalMessage>,
    tools: Vec<ToolDefinition>,
    on_chunk: Arc<StdMutex<impl FnMut(&StreamChunk) + Send + 'static>>,
    cancel: CancellationToken,
    stream_rules: &RwLock<Vec<StreamRule>>,
    idle_timeout: Duration,
    max_output_tokens: Option<u32>,
) -> Result<LlmResponse, LlmError> {
    // Long contexts make providers slower between deltas; grant extra
    // data-gap budget proportional to the request size so a slow-but-alive
    // stream is not aborted mid-answer (see `scale_stream_idle`).
    let idle_timeout = scale_stream_idle(idle_timeout, &messages);
    // Code-fence abort only applies when the model has tools available —
    // without tools, dumping a code sample is legitimate assistant output.
    let enforce_stream_rules = !tools.is_empty();
    // Stream creation includes the provider request and response-header wait.
    // Keep that phase cancellable too: otherwise pressing the UI interrupt
    // button cannot stop a provider that accepted the connection but has not
    // returned headers yet, and the caller waits for the transport timeout.
    let mut stream = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(LlmError::Cancelled),
        result = client.chat_stream_with_tools_output_cap(messages, tools, max_output_tokens) => result?,
    };
    tracing::debug!("aggregate_stream_cancellable start");

    // Channel decouples the stream loop from callback execution.
    // The consumer session (spawned below) calls on_chunk asynchronously;
    // the stream loop only does O(1) try_send and never blocks.
    let (chunk_tx, mut chunk_rx) = mpsc::channel::<StreamChunk>(128);
    let consumer = tokio::spawn(async move {
        while let Some(chunk) = chunk_rx.recv().await {
            let mut guard = on_chunk.lock().unwrap();
            guard(&chunk);
        }
    });

    // The first chunk may lag far behind the request (providers run
    // server-side "thinking" before the first delta). A dead stream must
    // still surface quickly, so the first chunk gets a longer budget than
    // the data-gap idle timeout; after data starts flowing, `idle_timeout`
    // applies to every subsequent gap.
    let first_chunk_timeout = idle_timeout.max(FIRST_CHUNK_GRACE);
    let mut received_any = false;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut finish_reason: Option<FinishReason> = None;
    let mut usage: Option<crate::types::Usage> = None;
    let mut model: Option<String> = None;
    let mut reasoning = String::new();
    let mut web_search_calls = Vec::new();
    let mut thinking_blocks = Vec::new();

    // Unified exit so every path drops `chunk_tx` and awaits the consumer
    // (avoids dual-consumer races when abort/retry reuses `on_chunk`).
    let outcome: Result<LlmResponse, LlmError> = async {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    return Err(LlmError::Cancelled);
                }
                item = tokio::time::timeout(
                    if received_any { idle_timeout } else { first_chunk_timeout },
                    stream.next(),
                ) => {
                    let stream_item = match item {
                        Ok(v) => v,
                        Err(_) => {
                            // No chunk arrived within the window: the server
                            // accepted the request but the body is stalled (half-open
                            // connection, provider-side hang). Abort as a retryable
                            // timeout instead of blocking until the overall
                            // `max_total_duration_secs` deadline — a hung stream
                            // must surface within the idle window, not minutes later.
                            if received_any {
                                tracing::warn!(
                                    "stream idle timeout after {}s with no chunk; aborting stream",
                                    idle_timeout.as_secs()
                                );
                                return Err(LlmError::Timeout(format!(
                                    "stream idle timeout after {}s with no data",
                                    idle_timeout.as_secs()
                                )));
                            }
                            tracing::warn!(
                                "stream first chunk timeout after {}s with no chunk; aborting stream",
                                first_chunk_timeout.as_secs()
                            );
                            return Err(LlmError::Timeout(format!(
                                "stream first chunk timeout after {}s with no data",
                                first_chunk_timeout.as_secs()
                            )));
                        }
                    };
                    match stream_item {
                        Some(Ok(chunk)) => {
                            received_any = true;
                            if let Some(ref delta) = chunk.text {
                                text.push_str(delta);
                            }
                            if let Some(ref r) = chunk.reasoning {
                                reasoning.push_str(r);
                            }
                            if !chunk.tool_calls.is_empty() {
                                tool_calls.extend(chunk.tool_calls.clone());
                            }
                            if !chunk.web_search_calls.is_empty() {
                                web_search_calls.extend(chunk.web_search_calls.clone());
                            }
                            if !chunk.thinking_blocks.is_empty() {
                                thinking_blocks.extend(chunk.thinking_blocks.clone());
                            }
                            if chunk.finish_reason.is_some() {
                                finish_reason = chunk.finish_reason;
                            }
                            if chunk.usage.is_some() {
                                usage = chunk.usage.clone();
                            }
                            if chunk.model.is_some() {
                                model = chunk.model.clone();
                            }

                            // Evaluate rules BEFORE forwarding so an aborting
                            // fence chunk never reaches the UI / partial_thought
                            // (retry reuses the same on_chunk).
                            if enforce_stream_rules && !text.is_empty() {
                                let rules = stream_rules.read().await;
                                if let Some(match_result) = check_stream_rules(&rules, &text) {
                                    drop(rules);
                                    match match_result.mode {
                                        StreamRuleMode::Warn => {
                                            tracing::warn!(
                                                "stream rule '{}' triggered (warn): matched '{}'",
                                                match_result.rule_name, match_result.matched_text
                                            );
                                        }
                                        StreamRuleMode::Abort => {
                                            tracing::warn!(
                                                "stream rule '{}' triggered (abort): matched '{}'",
                                                match_result.rule_name, match_result.matched_text
                                            );
                                            return Err(LlmError::StreamAborted(
                                                match_result.rule_name,
                                                match_result.inject,
                                            ));
                                        }
                                    }
                                }
                            }

                            // Non-blocking: consumer session calls on_chunk asynchronously
                            if let Err(e) = chunk_tx.try_send(chunk) {
                                tracing::warn!(
                                    "chunk consumer channel full, dropping chunk: {}",
                                    e
                                );
                            }
                        }
                        Some(Err(e)) => return Err(e),
                        None => break,
                    }
                }
            }
        }

        Ok(LlmResponse {
            text,
            tool_calls,
            finish_reason,
            usage: usage.unwrap_or_default(),
            model,
            reasoning: if reasoning.is_empty() {
                None
            } else {
                Some(reasoning)
            },
            web_search_calls,
            thinking_blocks,
        })
    }
    .await;

    drop(chunk_tx);
    if let Err(e) = consumer.await {
        tracing::warn!("stream chunk consumer action panicked: {}", e);
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::LlmClient;
    use crate::types::{Embedding, SttResult};
    use async_trait::async_trait;
    use std::pin::Pin;

    struct PendingStreamClient;

    #[async_trait]
    impl LlmClient for PendingStreamClient {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::Unknown("test client does not chat".into()))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Ok(Box::pin(futures_util::stream::empty()))
        }

        async fn chat_stream_with_tools_output_cap(
            &self,
            _messages: Vec<CanonicalMessage>,
            _tools: Vec<ToolDefinition>,
            _max_output_tokens: Option<u32>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            std::future::pending().await
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }

        async fn embed(&self, _input: Vec<String>) -> Result<Embedding, LlmError> {
            Err(LlmError::UnsupportedCapability("test".into()))
        }

        async fn transcribe(&self, _wav_data: &[u8]) -> Result<SttResult, LlmError> {
            Err(LlmError::UnsupportedCapability("test".into()))
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_provider_stream_creation() {
        let cancel = CancellationToken::new();
        let rules = Arc::new(RwLock::new(Vec::new()));
        let on_chunk = Arc::new(StdMutex::new(|_chunk: &StreamChunk| {}));
        let client: Arc<dyn LlmClient> = Arc::new(PendingStreamClient);
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            aggregate_stream_cancellable(
                client,
                Vec::new(),
                Vec::new(),
                on_chunk,
                task_cancel,
                rules.as_ref(),
                Duration::from_secs(30),
                None,
            )
            .await
        });

        tokio::task::yield_now().await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("stream creation should be cancellable")
            .expect("stream task should not panic");
        assert!(matches!(result, Err(LlmError::Cancelled)));
    }
}
