use crate::is_dangling_boundary;
use haven_common::prompts::CONVERSATION_SUMMARY_PROMPT;
use haven_common::types::{CanonicalMessage, ContentPart};
use haven_llm::{EndpointRole, LlmError, LlmRouter, ToolDefinition};
use std::sync::Arc;
use std::sync::LazyLock;
use tiktoken_rs::o200k_base;
use tokio_util::sync::CancellationToken;

static TOKENIZER: LazyLock<tiktoken_rs::CoreBPE> =
    LazyLock::new(|| o200k_base().expect("failed to initialize o200k_base tokenizer"));

/// Token estimation using o200k_base tokenizer for accurate counts.
pub fn estimate_tokens(text: &str) -> u32 {
    TOKENIZER.encode_with_special_tokens(text).len() as u32
}

/// Estimate the provider-visible token cost of one canonical message.
///
/// The estimator intentionally stays provider-neutral. It covers the content
/// and the extra fields that can be echoed into a request; protocol framing is
/// accounted for by the provider adapters and the small safety reserve in the
/// compactor.
fn estimate_message_token_cost(msg: &CanonicalMessage) -> u32 {
    let mut total = 0u32;
    for part in &msg.content {
        match part {
            ContentPart::Text(t) => total = total.saturating_add(estimate_tokens(t)),
            ContentPart::Image { .. } => total = total.saturating_add(200), // rough image token cost
            ContentPart::Audio { .. } => total = total.saturating_add(500), // rough audio token cost
        }
    }
    // Reasoning (thinking-mode) is echoed back to the provider on every
    // request and can dwarf the message text itself (a single turn's
    // reasoning routinely reaches 10k chars). It must count toward the
    // context budget or compaction never triggers on reasoning-heavy
    // conversations, the request body explodes, and providers stall.
    if let Some(r) = &msg.reasoning {
        total = total.saturating_add(estimate_tokens(r));
    }
    // Anthropic thinking blocks and built-in search items are echoed as raw
    // JSON. Count their complete serialized form, including signatures and
    // provider metadata, rather than only the visible thinking text.
    for block in &msg.thinking_blocks {
        total = total.saturating_add(estimate_serialized_tokens(block));
    }
    if !msg.web_search_calls.is_empty() {
        total = total.saturating_add(estimate_serialized_tokens(&msg.web_search_calls));
    }
    if let Some(calls) = &msg.tool_calls {
        // Tool arguments can be large (for example a generated patch). The
        // old fixed 50-token allowance missed that provider-visible payload.
        for call in calls {
            total = total
                .saturating_add(estimate_serialized_tokens(call))
                .saturating_add(10);
        }
    }
    if let Some(tool_call_id) = &msg.tool_call_id {
        total = total.saturating_add(estimate_tokens(tool_call_id));
    }
    total
}

fn estimate_serialized_tokens<T: serde::Serialize>(value: &T) -> u32 {
    serde_json::to_string(value)
        .ok()
        .map(|json| estimate_tokens(&json))
        .unwrap_or_default()
}

/// Estimate tokens in a list of canonical messages by summing each message's
/// provider-visible cost.
pub fn estimate_message_tokens(messages: &[CanonicalMessage]) -> u32 {
    messages
        .iter()
        .map(estimate_message_token_cost)
        .fold(0, u32::saturating_add)
}

/// Estimate the serialized schema cost once per request. Tool definitions are
/// not part of the durable transcript, but they occupy the same provider
/// context window as messages.
pub fn estimate_tool_tokens(tools: &[ToolDefinition]) -> u32 {
    serde_json::to_string(tools)
        .ok()
        .map(|json| estimate_tokens(&json))
        .unwrap_or_default()
}

/// Provider framing and adapter-side fields are not represented in canonical
/// messages. Keep one conservative allowance shared by compaction and the
/// dynamic output-cap calculation.
pub const PROVIDER_REQUEST_OVERHEAD_TOKENS: u32 = 256;

/// Estimate the request that will actually reach a provider. Sanitization is
/// applied to a clone so durable transcript state remains authoritative while
/// dangling tool-call repairs still participate in the budget.
pub fn estimate_provider_request_tokens(
    messages: &[CanonicalMessage],
    tools: &[ToolDefinition],
) -> u32 {
    estimate_provider_request_tokens_with_message_estimate(
        messages,
        tools,
        estimate_message_tokens(messages),
    )
}

/// Cache-friendly variant used by the ReAct preflight. Healthy canonical
/// arrays reuse the incremental message estimate; only malformed tool
/// boundaries need a sanitized clone and a fresh message pass.
pub fn estimate_provider_request_tokens_with_message_estimate(
    messages: &[CanonicalMessage],
    tools: &[ToolDefinition],
    cached_message_tokens: u32,
) -> u32 {
    let message_tokens = if crate::canonical::canonical_pairing_healthy(messages) {
        cached_message_tokens
    } else {
        let mut provider_messages = messages.to_vec();
        crate::sanitize_canonical(&mut provider_messages);
        estimate_message_tokens(&provider_messages)
    };
    message_tokens
        .saturating_add(estimate_tool_tokens(tools))
        .saturating_add(PROVIDER_REQUEST_OVERHEAD_TOKENS)
}

/// Prefix sums let the compaction planner compare many candidate boundaries
/// without re-tokenizing the same messages for every candidate.
fn message_token_prefixes(messages: &[CanonicalMessage]) -> Vec<u32> {
    let mut prefixes: Vec<u32> = Vec::with_capacity(messages.len() + 1);
    prefixes.push(0);
    for message in messages {
        let next = prefixes
            .last()
            .copied()
            .unwrap_or_default()
            .saturating_add(estimate_message_token_cost(message));
        prefixes.push(next);
    }
    prefixes
}

fn range_token_cost(prefixes: &[u32], start: usize, end: usize) -> u32 {
    prefixes
        .get(end)
        .copied()
        .unwrap_or_default()
        .saturating_sub(prefixes.get(start).copied().unwrap_or_default())
}

/// Keep summary requests bounded even when a long-running session has a very
/// large middle region. The planner may summarize more messages than fit in
/// this input window; this renderer keeps both edges and explicitly marks the
/// omitted middle so the summary model sees the shape of the missing region.
const SUMMARY_INPUT_TOKEN_BUDGET: u32 = 16_000;
/// The summary message is deliberately smaller than the input budget. A
/// little framing overhead is reserved by the range planner below.
const SUMMARY_TEXT_TOKEN_BUDGET: u32 = 768;
const SUMMARY_MESSAGE_TOKEN_BUDGET: u32 = 1_024;
const SUMMARY_REQUEST_OVERHEAD_TOKENS: u32 = PROVIDER_REQUEST_OVERHEAD_TOKENS;

fn truncate_to_token_budget(text: &str, max_tokens: u32) -> String {
    if max_tokens == 0 {
        return String::new();
    }
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let middle = (low + high).div_ceil(2);
        let candidate: String = chars[..middle].iter().collect();
        if estimate_tokens(&candidate) <= max_tokens {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    chars[..low].iter().collect()
}

fn truncate_from_end_to_token_budget(text: &str, max_tokens: u32) -> String {
    if max_tokens == 0 {
        return String::new();
    }
    if estimate_tokens(text) <= max_tokens {
        return text.to_string();
    }

    let chars: Vec<char> = text.chars().collect();
    let mut low = 0usize;
    let mut high = chars.len();
    while low < high {
        let take = (low + high).div_ceil(2);
        let start = chars.len().saturating_sub(take);
        let candidate: String = chars[start..].iter().collect();
        if estimate_tokens(&candidate) <= max_tokens {
            low = take;
        } else {
            high = take - 1;
        }
    }
    let start = chars.len().saturating_sub(low);
    chars[start..].iter().collect()
}

#[derive(Debug, Clone)]
pub struct CompactionResult {
    /// The compacted message list (summary + remaining messages)
    pub compacted: Vec<CanonicalMessage>,
    /// Number of messages that were summarized
    pub summarized_count: usize,
    /// The generated summary text
    pub summary: String,
    /// Token count before compaction
    pub tokens_before: u32,
    /// Token count after compaction
    pub tokens_after: u32,
    /// Stable `msg-*` shared by the summary bubble and `memory_items` (L1).
    pub episode_id: String,
    /// True when the deterministic older-context marker was used because the
    /// summary request failed, returned empty output, or could not fit.
    pub degraded: bool,
}

/// Context window pressure monitor and auto-compactor.
///
/// Monitors the total estimated token count of the canonical message list
/// after each ReAct step. When the estimate exceeds `context_window *
/// trigger_ratio` (clamped to leave room for the model's response and a
/// retry buffer), it compresses the oldest messages into a single summary
/// via the configured text/vision summary endpoint.
///
/// `trigger_ratio` defaults to 0.75 — i.e. compact when 75% of the context
/// window is consumed. The previous behaviour (`context_window -
/// reserve_tokens`) was too conservative and triggered compaction only when
/// the model was already close to overflowing, forcing an expensive
/// retry-and-resummarize cycle.
///
/// NOTE: production construction goes through
/// `ReActEngine::context_compactor`, which passes `context_limits` values
/// from `[context_limits]` in config.toml; these `new`/`with_ratio` defaults
/// must stay in sync with `ContextLimitsConfig` for tests and direct users.
pub struct ContextCompactor {
    /// Soft limit: total context window in tokens (from model config).
    pub context_window: u32,
    /// Tokens to reserve for the response.
    pub reserve_tokens: u32,
    /// Fraction of `context_window` at which to start compacting. Lower =
    /// more aggressive. Must be in (0, 1).
    pub trigger_ratio: f32,
}

impl ContextCompactor {
    pub fn new(context_window: u32, reserve_tokens: u32) -> Self {
        Self {
            context_window,
            reserve_tokens,
            trigger_ratio: 0.75,
        }
    }

    pub fn with_ratio(context_window: u32, reserve_tokens: u32, ratio: f32) -> Self {
        Self {
            context_window,
            reserve_tokens,
            trigger_ratio: ratio.clamp(0.1, 0.95),
        }
    }

    /// The token threshold at which compaction triggers: the *lower* of
    /// `context_window * trigger_ratio` (proactive cap) and
    /// `context_window - reserve_tokens` (response headroom floor).
    ///
    /// Split out of `needs_compaction` so callers that already hold an
    /// external estimate (e.g. the incremental per-session cache in
    /// `ReActEngine`) can compare without re-estimating the whole list.
    pub fn threshold_tokens(&self) -> u32 {
        let ratio_threshold = (self.context_window as f64 * self.trigger_ratio as f64) as u32;
        let headroom_threshold = self.context_window.saturating_sub(self.reserve_tokens);
        ratio_threshold.min(headroom_threshold.max(1))
    }

    /// Returns true when the message list exceeds the compact threshold.
    ///
    /// We use the *lower* of:
    /// - `context_window * trigger_ratio` (proactive cap)
    /// - `context_window - reserve_tokens` (response headroom floor)
    ///
    /// Whichever is smaller triggers compaction first. This preserves the
    /// original "leave room for response" guarantee while compacting
    /// earlier when the model has plenty of headroom.
    pub fn needs_compaction(&self, messages: &[CanonicalMessage]) -> bool {
        estimate_message_tokens(messages) > self.threshold_tokens()
    }

    /// Build a bounded prompt from the messages being removed. The source
    /// renderer keeps both edges when the middle is larger than the summary
    /// model's input budget.
    #[cfg_attr(not(test), allow(dead_code))]
    fn build_summary_prompt(messages: &[CanonicalMessage]) -> String {
        Self::build_summary_prompt_with_budget(messages, SUMMARY_INPUT_TOKEN_BUDGET)
    }

    fn build_summary_prompt_with_budget(
        messages: &[CanonicalMessage],
        input_budget: u32,
    ) -> String {
        use std::fmt::Write as _;
        let mut lines = Vec::with_capacity(messages.len());
        for msg in messages {
            let role = match msg.role {
                haven_common::types::CanonicalRole::System => "system",
                haven_common::types::CanonicalRole::User => "user",
                haven_common::types::CanonicalRole::Assistant => "assistant",
                haven_common::types::CanonicalRole::Tool => "tool",
            };
            let mut line = String::new();
            for part in &msg.content {
                match part {
                    ContentPart::Text(t) => {
                        let _ = writeln!(line, "[{}] {}", role, t);
                    }
                    ContentPart::Image { media_type, .. } => {
                        let _ = writeln!(
                            line,
                            "[{} image attachment media_type={} follows]",
                            role, media_type
                        );
                    }
                    ContentPart::Audio { media_type, .. } => {
                        let _ = writeln!(
                            line,
                            "[{} audio attachment media_type={} follows]",
                            role, media_type
                        );
                    }
                }
            }
            if let Some(calls) = &msg.tool_calls {
                for call in calls {
                    let _ = writeln!(
                        line,
                        "[assistant tool_call id={} name={} arguments={}]",
                        call.id, call.name, call.arguments
                    );
                }
            }
            if let Some(tool_call_id) = &msg.tool_call_id {
                let _ = writeln!(line, "[tool result id={}]", tool_call_id);
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }

        let marker = format!(
            "\n[... {} transcript entries omitted from this summary input ...]\n",
            lines.len().saturating_sub(2)
        );
        let suffix = "\n---\nSummary:";
        let fixed_tokens =
            estimate_tokens(CONVERSATION_SUMMARY_PROMPT).saturating_add(estimate_tokens(suffix));
        let available = input_budget.saturating_sub(fixed_tokens).max(1);
        let full_body = lines.concat();
        let body = if estimate_tokens(&full_body) <= available {
            full_body
        } else {
            let marker_tokens = estimate_tokens(&marker);
            let body_budget = available.saturating_sub(marker_tokens);
            let head_budget = body_budget / 2;
            let tail_budget = body_budget.saturating_sub(head_budget);
            let split = lines.len().div_ceil(2);
            let head = truncate_to_token_budget(&lines[..split].concat(), head_budget);
            let tail = truncate_from_end_to_token_budget(&lines[split..].concat(), tail_budget);
            truncate_to_token_budget(&format!("{head}{marker}{tail}"), available)
        };

        let mut text =
            String::with_capacity(CONVERSATION_SUMMARY_PROMPT.len() + body.len() + suffix.len());
        text.push_str(CONVERSATION_SUMMARY_PROMPT);
        text.push_str(&body);
        text.push_str(suffix);
        text
    }

    /// Build one multimodal summary request. Text is bounded independently so
    /// image/audio parts remain available to a vision-capable provider instead
    /// of being counted and silently discarded.
    fn build_summary_messages(
        messages: &[CanonicalMessage],
        input_budget: u32,
    ) -> Vec<CanonicalMessage> {
        let media: Vec<ContentPart> = messages
            .iter()
            .flat_map(|message| message.content.iter())
            .filter_map(|part| match part {
                ContentPart::Image { .. } | ContentPart::Audio { .. } => Some(part.clone()),
                ContentPart::Text(_) => None,
            })
            .collect();
        let media_tokens = estimate_message_tokens(&[CanonicalMessage::user(media.clone())]);
        let text_budget = input_budget.saturating_sub(media_tokens).max(1);
        let prompt = Self::build_summary_prompt_with_budget(messages, text_budget);
        let mut content = vec![ContentPart::text(prompt)];
        content.extend(media);
        vec![CanonicalMessage::user(content)]
    }

    /// Compute a safe cutoff index that never splits a tool-call/tool-result
    /// pair.
    ///
    /// Tool results (`role == Tool`) reference the assistant message that
    /// declared them via `tool_call_id`. Cutting between that assistant
    /// message and its `Tool` results leaves the suffix beginning with a
    /// dangling tool message, which providers reject with a 400. This slides
    /// `desired` forward past leading `Tool` messages AND past an assistant
    /// message that declares `tool_calls` (its results immediately follow it),
    /// so the suffix starts only at a clean boundary.
    fn safe_end_idx(messages: &[CanonicalMessage], mut desired: usize) -> usize {
        while desired < messages.len() && is_dangling_boundary(&messages[desired]) {
            desired += 1;
        }
        desired
    }

    /// Pick a summary start without cutting an assistant tool declaration away
    /// from its results. Unlike [`Self::safe_end_idx`], an assistant message
    /// with calls is safe to include in the summarized region. When the
    /// desired position lands inside tool results, rewind to that declaration
    /// so the whole round is summarized together.
    fn safe_start_idx(messages: &[CanonicalMessage], desired: usize) -> usize {
        if desired >= messages.len()
            || messages[desired].role != haven_common::types::CanonicalRole::Tool
        {
            return desired;
        }
        let mut index = desired;
        while index > 0 && messages[index].role == haven_common::types::CanonicalRole::Tool {
            index -= 1;
        }
        if messages[index].role == haven_common::types::CanonicalRole::Assistant
            && messages[index].tool_calls.is_some()
        {
            index
        } else {
            desired
        }
    }

    /// Pick the beginning of a recent suffix without leaving an orphaned
    /// tool result in that suffix. Unlike `safe_end_idx`, a tool result is
    /// rewound to its assistant declaration when the whole call round can be
    /// retained. This is used by the deterministic degraded path, whose
    /// contract is to keep recent rounds rather than summarize them away.
    fn safe_suffix_start_idx(messages: &[CanonicalMessage], desired: usize) -> usize {
        let mut candidate = desired.min(messages.len());
        while candidate < messages.len() {
            if crate::canonical::canonical_pairing_healthy(&messages[candidate..]) {
                return candidate;
            }
            if messages[candidate].role == haven_common::types::CanonicalRole::Tool {
                let rewound = Self::safe_start_idx(messages, candidate);
                if rewound < candidate
                    && crate::canonical::canonical_pairing_healthy(&messages[rewound..])
                {
                    return rewound;
                }
            }
            candidate += 1;
        }
        messages.len()
    }

    /// Keep the earliest non-system messages so provider message-prefix cache
    /// can survive compaction. Summarize the **middle**, not the head.
    const STICKY_PREFIX_MESSAGES: usize = 2;
    /// Deterministic degraded compaction keeps a few recent turns verbatim.
    /// Six messages cover a normal exchange plus one complete
    /// assistant-tool-result round without making the fallback unbounded.
    const DEGRADED_RECENT_MESSAGES: usize = 6;

    /// Choose `[start, end)` of the middle region to summarize.
    ///
    /// Layout: `[system*][sticky…][middle…)[suffix…]`. Sticky is a small
    /// head of the conversation (prompt-cache friendly); middle is selected by
    /// token cost; suffix is the largest recent tail that fits the post-
    /// compaction target.
    #[cfg_attr(not(test), allow(dead_code))]
    fn compaction_range(
        &self,
        messages: &[CanonicalMessage],
        token_prefixes: &[u32],
    ) -> Option<(usize, usize, usize)> {
        self.compaction_range_with_extra_tokens(messages, token_prefixes, 0)
    }

    fn compaction_range_with_extra_tokens(
        &self,
        messages: &[CanonicalMessage],
        token_prefixes: &[u32],
        extra_tokens: u32,
    ) -> Option<(usize, usize, usize)> {
        let system_count = messages
            .iter()
            .take_while(|m| matches!(m.role, haven_common::types::CanonicalRole::System))
            .count();

        let compactable = messages.len() - system_count;
        // Preserve the first user turn as the cache-routing anchor. Three
        // non-system messages are enough for a complete tool round
        // (user -> assistant tool call -> tool result): summarize the round
        // itself when a provider rejects the context, rather than failing and
        // leaving the session unrecoverable.
        if compactable < 3 {
            return None;
        }

        // Keep at least one anchor while leaving a two-message region for a
        // complete assistant-tool round. Longer transcripts retain the normal
        // cache-friendly sticky prefix.
        let sticky_target = Self::STICKY_PREFIX_MESSAGES.min(compactable.saturating_sub(2));
        let start_idx = Self::safe_start_idx(messages, system_count + sticky_target);

        // Compact to roughly half of the trigger threshold. This gives the
        // next few turns room to append without immediately rebuilding the
        // prompt again, while the sticky prefix keeps the provider cacheable
        // prefix intact.
        let target_tokens = self.threshold_tokens().saturating_mul(2) / 3;
        let fixed_tokens = token_prefixes
            .get(start_idx)
            .copied()
            .unwrap_or_default()
            .saturating_add(SUMMARY_MESSAGE_TOKEN_BUDGET)
            .saturating_add(extra_tokens);
        let mut end_idx = None;
        for desired_end in (start_idx + 2)..=messages.len() {
            let candidate = Self::safe_end_idx(messages, desired_end);
            if candidate <= start_idx {
                continue;
            }
            let suffix_tokens = range_token_cost(token_prefixes, candidate, messages.len());
            if fixed_tokens.saturating_add(suffix_tokens) <= target_tokens {
                end_idx = Some(candidate);
                break;
            }
        }
        let end_idx = end_idx.unwrap_or_else(|| {
            // If the normal token target cannot retain any suffix, keep the
            // latest few messages anyway. This is the bounded deterministic
            // fallback used when a summary request fails, and is preferable
            // to deleting the entire recent tail. The suffix helper keeps a
            // complete tool-call round together.
            let recent_start = Self::safe_suffix_start_idx(
                messages,
                messages
                    .len()
                    .saturating_sub(Self::DEGRADED_RECENT_MESSAGES),
            );
            if recent_start > start_idx + 1 {
                recent_start
            } else {
                messages.len()
            }
        });

        // A summary may legitimately replace the whole trailing tool round:
        // it is then the new clean tail. Otherwise a suffix must remain.
        if end_idx <= start_idx || end_idx - start_idx < 2 {
            return None;
        }

        Some((system_count, start_idx, end_idx))
    }

    /// Compress the message list: keep a sticky early prefix, summarize the
    /// token-selected middle, and retain the largest recent suffix that fits
    /// the post-compaction target (prompt-cache aware).
    ///
    /// Returns `None` when compaction fails (e.g. LLM call fails) or there is
    /// no complete turn/tool round to compact (fewer than 3 messages).
    pub async fn compact(
        &self,
        messages: &[CanonicalMessage],
        tools: &[ToolDefinition],
        router: &Arc<LlmRouter>,
        cancel: CancellationToken,
    ) -> Result<Option<CompactionResult>, LlmError> {
        // A user -> assistant tool-call -> tool-result round is the minimum
        // recoverable shape after a context-length failure.
        if messages.len() < 3 {
            return Ok(None);
        }

        let token_prefixes = message_token_prefixes(messages);
        let extra_tokens =
            estimate_tool_tokens(tools).saturating_add(PROVIDER_REQUEST_OVERHEAD_TOKENS);
        let Some((system_count, start_idx, end_idx)) =
            self.compaction_range_with_extra_tokens(messages, &token_prefixes, extra_tokens)
        else {
            return Ok(None);
        };
        let middle = &messages[start_idx..end_idx];
        let suffix = &messages[end_idx..];
        let summarized_count = end_idx - start_idx;

        let tokens_before = estimate_provider_request_tokens(messages, tools);
        let summary_role = if middle.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Image { .. }))
        }) {
            router.vision_role().await
        } else {
            EndpointRole::DefaultModel
        };
        let summary_window = router.context_window_for_role(summary_role).await;
        let summary_budget = SUMMARY_INPUT_TOKEN_BUDGET.min(
            summary_window
                .saturating_sub(SUMMARY_TEXT_TOKEN_BUDGET)
                .saturating_sub(SUMMARY_REQUEST_OVERHEAD_TOKENS)
                .max(1),
        );
        let summary_messages = Self::build_summary_messages(middle, summary_budget);
        let summary_input_tokens = estimate_provider_request_tokens(&summary_messages, &[]);
        let summary_fits = summary_input_tokens
            .saturating_add(1)
            .saturating_add(SUMMARY_REQUEST_OVERHEAD_TOKENS)
            <= summary_window;

        let summary = if summary_fits {
            let output_cap = router
                .effective_output_tokens(summary_role, summary_input_tokens)
                .await
                .clamp(1, SUMMARY_TEXT_TOKEN_BUDGET);
            match router
                .chat_messages_cancellable(summary_role, summary_messages, Some(output_cap), cancel)
                .await
            {
                Ok(response) => {
                    let text =
                        truncate_to_token_budget(response.text.trim(), SUMMARY_TEXT_TOKEN_BUDGET);
                    (!text.is_empty()).then_some(text)
                }
                Err(LlmError::Cancelled) => return Err(LlmError::Cancelled),
                Err(error) => {
                    tracing::warn!("compaction summary request failed; degrading: {error}");
                    None
                }
            }
        } else {
            tracing::warn!(
                "compaction summary request does not fit model window {}; degrading",
                summary_window
            );
            None
        };

        let degraded = summary.is_none();
        let summary = summary.unwrap_or_else(|| "[older context omitted]".into());
        let episode_id = haven_common::types::new_id("msg");
        let mut compacted: Vec<CanonicalMessage> =
            Vec::with_capacity(system_count + (start_idx - system_count) + 1 + suffix.len());
        // Both normal and degraded compaction retain system messages, a
        // cache-friendly early anchor, and a recent suffix beginning at a
        // complete tool-call boundary. Only the middle is removed.
        compacted.extend_from_slice(&messages[..system_count]);
        compacted.extend_from_slice(&messages[system_count..start_idx]);
        let mut summary_msg = CanonicalMessage::assistant(
            vec![ContentPart::text(format!(
                "{} {}",
                haven_common::prompts::COMPACTED_SUMMARY_PREFIX,
                summary
            ))],
            None,
            None,
            Vec::new(),
            Vec::new(),
        );
        summary_msg.id = Some(episode_id.clone());
        compacted.push(summary_msg);
        compacted.extend_from_slice(suffix);

        let tokens_after = estimate_provider_request_tokens(&compacted, tools);

        Ok(Some(CompactionResult {
            compacted,
            summarized_count,
            summary,
            tokens_before,
            tokens_after,
            episode_id,
            degraded,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::CanonicalRole;
    use haven_llm::{LlmClient, LlmResponse, StreamChunk};
    use std::pin::Pin;

    struct FailingSummaryClient;

    #[async_trait::async_trait]
    impl LlmClient for FailingSummaryClient {
        async fn chat(&self, _messages: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Err(LlmError::UnsupportedCapability(
                "summary disabled in test".into(),
            ))
        }

        async fn chat_stream(
            &self,
            _messages: Vec<CanonicalMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::UnsupportedCapability(
                "summary streaming disabled in test".into(),
            ))
        }

        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn make_msg(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    #[test]
    fn estimate_tokens_basic() {
        let count = estimate_tokens("Hello world, this is a test message.");
        assert!(count > 0);
    }

    #[test]
    fn estimate_message_tokens_counts_all_messages() {
        let msgs = vec![
            make_msg(CanonicalRole::System, "You are a helpful assistant."),
            make_msg(CanonicalRole::User, "Hello, can you help me?"),
        ];
        let count = estimate_message_tokens(&msgs);
        assert!(count > 0);
    }

    #[test]
    fn needs_compaction_returns_true_when_exceeded() {
        let compactor = ContextCompactor::new(100, 20);
        let text =
            "This is a long conversation history that should exceed the compaction threshold. "
                .repeat(10);
        let msgs = vec![
            make_msg(CanonicalRole::System, &text),
            make_msg(CanonicalRole::User, &text),
        ];
        assert!(compactor.needs_compaction(&msgs));
    }

    #[test]
    fn needs_compaction_returns_false_when_under() {
        let compactor = ContextCompactor::new(1000, 200);
        let msgs = vec![make_msg(CanonicalRole::User, "Hello")];
        assert!(!compactor.needs_compaction(&msgs));
    }

    #[test]
    fn needs_compaction_triggers_proactively_below_headroom_floor() {
        // 100K window, 8K reserve. The old behavior triggered at 92K
        // (headroom floor). With a 75% ratio it triggers at 75K — earlier,
        // leaving more room for the response + retry buffer.
        let compactor = ContextCompactor::with_ratio(100_000, 8_000, 0.75);
        let text = "This is a realistic English sentence used to estimate conversation \
                    tokens in the compaction test suite. ";
        let est_single = estimate_tokens(text);
        assert!(est_single > 0);
        // Target ~77K estimated: comfortably above the 75K ratio threshold
        // and comfortably below the 92K headroom floor.
        let count = (77_000 / est_single).max(1) as usize;
        let msgs: Vec<_> = (0..count)
            .map(|_| make_msg(CanonicalRole::User, text))
            .collect();
        let est = estimate_message_tokens(&msgs);
        assert!(
            est > 75_000 && est <= 92_000,
            "estimate {} must land between 75K (ratio) and 92K (headroom)",
            est
        );
        assert!(
            compactor.needs_compaction(&msgs),
            "should compact at 75% of window before hitting the 92K headroom floor"
        );
    }

    #[test]
    fn needs_compaction_respects_headroom_floor_when_ratio_is_loose() {
        // With a near-1.0 ratio the headroom floor (window - reserve) must
        // still win, so we never compact only when the model would overflow.
        let compactor = ContextCompactor::with_ratio(10_000, 500, 0.95);
        let text = "x".repeat(200);
        // ~6K estimated: below 9.5K ratio threshold AND below 9.5K headroom.
        let msgs: Vec<_> = (0..30)
            .map(|_| make_msg(CanonicalRole::User, &text))
            .collect();
        assert!(!compactor.needs_compaction(&msgs));
    }

    #[test]
    fn with_ratio_clamps_to_valid_range() {
        let compactor = ContextCompactor::with_ratio(1000, 100, 2.0);
        assert!(compactor.trigger_ratio <= 0.95);
        let compactor = ContextCompactor::with_ratio(1000, 100, 0.0);
        assert!(compactor.trigger_ratio >= 0.1);
    }

    #[test]
    fn compact_returns_none_for_few_messages() {
        let router = Arc::new(haven_llm::LlmRouter::new(
            haven_common::config::RouterConfig::default(),
        ));
        let compactor = ContextCompactor::new(4096, 512);
        let msgs = vec![
            make_msg(CanonicalRole::System, "You are Haven."),
            make_msg(CanonicalRole::User, "Hi"),
        ];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(compactor.compact(&msgs, &[], &router, CancellationToken::new()));
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn build_summary_prompt_contains_messages() {
        let msgs = vec![
            make_msg(CanonicalRole::User, "My name is Alice"),
            make_msg(CanonicalRole::Assistant, "Hello Alice!"),
        ];
        let prompt = ContextCompactor::build_summary_prompt(&msgs);
        assert!(prompt.contains("Alice"));
    }

    #[test]
    fn summary_prompt_preserves_tool_result_identity_and_media_shape() {
        let mut result = make_tool_result("contents");
        result.tool_call_id = Some("call-42".into());
        let image = CanonicalMessage {
            role: CanonicalRole::User,
            content: vec![ContentPart::Image {
                content_type: "image".into(),
                media_type: "image/png".into(),
                data: "encoded-image".into(),
            }],
            tool_calls: None,
            tool_call_id: None,
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        };
        let prompt = ContextCompactor::build_summary_prompt(&[result, image.clone()]);
        assert!(prompt.contains("tool result id=call-42"));
        assert!(prompt.contains("image attachment media_type=image/png"));

        let summary_messages =
            ContextCompactor::build_summary_messages(&[image], SUMMARY_INPUT_TOKEN_BUDGET);
        assert!(summary_messages[0].content.iter().any(|part| matches!(
            part,
            ContentPart::Image { data, .. } if data == "encoded-image"
        )));
    }

    fn make_tool_result(text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            reasoning: None,
            web_search_calls: Vec::new(),
            thinking_blocks: Vec::new(),
            source: None,
            id: None,
        }
    }

    #[test]
    fn compact_degrades_to_marker_and_retains_recent_tool_boundary() {
        let mut first_call = make_msg(CanonicalRole::Assistant, "old call");
        first_call.tool_calls = Some(vec![haven_common::types::CanonicalToolCall {
            id: "call-old".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "old.txt"}),
        }]);
        let mut first_result = make_tool_result("old result");
        first_result.tool_call_id = Some("call-old".into());

        let mut recent_call = make_msg(CanonicalRole::Assistant, "recent call");
        recent_call.tool_calls = Some(vec![haven_common::types::CanonicalToolCall {
            id: "call-recent".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "recent.txt"}),
        }]);
        let mut recent_result = make_tool_result("recent result");
        recent_result.tool_call_id = Some("call-recent".into());

        let messages = vec![
            make_msg(CanonicalRole::System, "stable system"),
            make_msg(CanonicalRole::User, "anchor"),
            make_msg(CanonicalRole::Assistant, "old answer"),
            first_call,
            first_result,
            make_msg(CanonicalRole::User, "old follow-up"),
            recent_call,
            recent_result,
            make_msg(CanonicalRole::User, "recent user"),
            make_msg(CanonicalRole::Assistant, "recent answer"),
            make_msg(CanonicalRole::User, "latest user"),
            make_msg(CanonicalRole::Assistant, "latest answer"),
        ];
        let failing: Arc<dyn LlmClient> = Arc::new(FailingSummaryClient);
        let router = Arc::new(haven_llm::LlmRouter::new_with_clients(
            failing.clone(),
            failing.clone(),
            failing.clone(),
            failing.clone(),
            failing,
        ));
        let compactor = ContextCompactor::new(100, 20);
        let result = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(compactor.compact(&messages, &[], &router, CancellationToken::new()))
            .unwrap()
            .expect("enough history to compact");

        assert!(result.degraded);
        assert_eq!(result.summary, "[older context omitted]");
        assert!(result.compacted.iter().any(|message| {
            message.content.iter().any(
                |part| matches!(part, ContentPart::Text(text) if text.contains("stable system")),
            )
        }));
        assert!(result.compacted.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Text(text) if text.contains("latest user")))
        }));
        assert!(result.compacted.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Text(text) if text == "recent call"))
        }));
        assert!(!result.compacted.iter().any(|message| {
            message
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::Text(text) if text == "old follow-up"))
        }));
        assert!(crate::canonical::canonical_pairing_healthy(
            &result.compacted
        ));
    }

    #[test]
    fn safe_end_idx_keeps_suffix_from_starting_with_tool() {
        // user, assistant(tool_calls), tool — a full pair. Any desired index
        // between the assistant and its tool result must be pushed to the end
        // of the tool-result block so the suffix never starts with a Tool msg.
        let msgs = vec![
            make_msg(CanonicalRole::User, "hello"),
            make_msg(CanonicalRole::Assistant, "let me check"),
            make_tool_result("result"),
            make_msg(CanonicalRole::User, "thanks"),
        ];
        // Desired index 2 points at the Tool message -> must slide to 3.
        assert_eq!(ContextCompactor::safe_end_idx(&msgs, 2), 3);
        // Desired index 1 points at the assistant-with-calls message -> safe.
        assert_eq!(ContextCompactor::safe_end_idx(&msgs, 1), 1);
        // Desired index 3 points at a User message -> safe.
        assert_eq!(ContextCompactor::safe_end_idx(&msgs, 3), 3);
    }

    #[test]
    fn safe_end_idx_pushes_past_multiple_tool_results() {
        let msgs = vec![
            make_msg(CanonicalRole::User, "a"),
            make_msg(CanonicalRole::Assistant, "call"),
            make_tool_result("r1"),
            make_tool_result("r2"),
            make_msg(CanonicalRole::User, "b"),
        ];
        // Desired index 2 (at Tool r1) -> slides past both results to index 4.
        assert_eq!(ContextCompactor::safe_end_idx(&msgs, 2), 4);
        assert_eq!(ContextCompactor::safe_end_idx(&msgs, 4), 4);
    }

    #[test]
    fn safe_end_idx_slides_past_assistant_with_tool_calls_plus_results() {
        // Cutting right AFTER the assistant-with-calls message leaves its tool
        // results dangling in the suffix (their assistant is summarized away),
        // which providers reject with a 400. The index must slide past the
        // assistant AND its tool-result block.
        let msgs = vec![
            make_msg(CanonicalRole::User, "a"),
            make_msg(CanonicalRole::Assistant, "call"),
            make_tool_result("r1"),
            make_tool_result("r2"),
            make_msg(CanonicalRole::User, "b"),
        ];
        let mut with_calls = msgs.clone();
        with_calls[1].tool_calls = Some(vec![haven_common::types::CanonicalToolCall {
            id: "call_1".into(),
            name: "tool".into(),
            arguments: serde_json::Value::Null,
        }]);
        // Desired index 1 points at the assistant-with-calls message -> the
        // split must slide past it and both results to index 4.
        assert_eq!(ContextCompactor::safe_end_idx(&with_calls, 1), 4);
        assert_eq!(ContextCompactor::safe_end_idx(&with_calls, 2), 4);
        // Desired index 4 (User) -> safe as-is.
        assert_eq!(ContextCompactor::safe_end_idx(&with_calls, 4), 4);
    }

    #[test]
    fn safe_suffix_start_keeps_complete_tool_round() {
        let mut assistant = make_msg(CanonicalRole::Assistant, "call");
        assistant.tool_calls = Some(vec![haven_common::types::CanonicalToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({"path": "notes.txt"}),
        }]);
        let mut result = make_tool_result("contents");
        result.tool_call_id = Some("call-1".into());
        let messages = vec![
            make_msg(CanonicalRole::User, "old"),
            assistant,
            result,
            make_msg(CanonicalRole::User, "recent"),
        ];

        // The desired position points at the tool result. The degraded tail
        // must rewind to the assistant declaration, not start with Tool.
        assert_eq!(ContextCompactor::safe_suffix_start_idx(&messages, 2), 1);
        assert!(crate::canonical::canonical_pairing_healthy(&messages[1..]));
    }

    #[test]
    fn compaction_range_keeps_sticky_prefix_and_suffix() {
        // system + 8 user turns: sticky keeps earliest turns, middle is summarized.
        let mut msgs = vec![make_msg(CanonicalRole::System, "sys")];
        for i in 0..8 {
            msgs.push(make_msg(CanonicalRole::User, &format!("u{i}")));
        }
        let compactor = ContextCompactor::new(10_000, 1_000);
        let prefixes = message_token_prefixes(&msgs);
        let (system_count, start, end) = compactor.compaction_range(&msgs, &prefixes).unwrap();
        assert_eq!(system_count, 1);
        assert!(start > system_count, "sticky prefix must be kept");
        assert!(end < msgs.len(), "recent suffix must remain");
        assert!(end - start >= 2, "middle must be worth summarizing");
        // Sticky target = min(2, 8/4)=2 → start at index 3 (after sys + u0 + u1).
        assert_eq!(start, 3);
    }

    #[test]
    fn compaction_range_keeps_session_anchor_on_minimal_recoverable_session() {
        let msgs = vec![
            make_msg(CanonicalRole::System, "sys"),
            make_msg(CanonicalRole::User, "a"),
            make_msg(CanonicalRole::Assistant, "b"),
            make_msg(CanonicalRole::User, "c"),
        ];
        let compactor = ContextCompactor::new(10_000, 1_000);
        let prefixes = message_token_prefixes(&msgs);
        let (system_count, start, end) = compactor.compaction_range(&msgs, &prefixes).unwrap();
        assert_eq!(system_count, 1);
        assert_eq!((start, end), (2, 4));
        assert!(matches!(
            &msgs[system_count].content[0],
            ContentPart::Text(text) if text == "a"
        ));
    }

    #[test]
    fn compaction_range_summarizes_complete_trailing_tool_round() {
        let mut assistant = make_msg(CanonicalRole::Assistant, "calling tool");
        assistant.tool_calls = Some(vec![haven_common::types::CanonicalToolCall {
            id: "call-1".into(),
            name: "read".into(),
            arguments: serde_json::json!({}),
        }]);
        let mut tool = make_msg(CanonicalRole::Tool, "tool result");
        tool.tool_call_id = Some("call-1".into());
        let msgs = vec![
            make_msg(CanonicalRole::System, "sys"),
            make_msg(CanonicalRole::User, "anchor"),
            assistant,
            tool,
        ];

        let compactor = ContextCompactor::new(10_000, 1_000);
        let prefixes = message_token_prefixes(&msgs);
        let (system_count, start, end) = compactor.compaction_range(&msgs, &prefixes).unwrap();
        assert_eq!(system_count, 1);
        assert_eq!((start, end), (2, 4));
    }

    #[test]
    fn compaction_range_uses_token_budget_for_recent_tail() {
        let compactor = ContextCompactor::with_ratio(5_000, 500, 0.75);
        let mut msgs = vec![
            make_msg(CanonicalRole::System, "stable system"),
            make_msg(CanonicalRole::User, "first intent"),
            make_msg(CanonicalRole::Assistant, "first answer"),
        ];
        msgs.extend((0..40).map(|index| {
            make_msg(
                CanonicalRole::User,
                &format!("recent-{index} {}", "detail ".repeat(80)),
            )
        }));

        let prefixes = message_token_prefixes(&msgs);
        let (_, start, end) = compactor.compaction_range(&msgs, &prefixes).unwrap();
        let target = compactor.threshold_tokens() * 2 / 3;
        let fixed = prefixes[start] + SUMMARY_MESSAGE_TOKEN_BUDGET;
        let suffix = range_token_cost(&prefixes, end, msgs.len());

        assert!(
            end > start + 2,
            "large recent tail needs token-based trimming"
        );
        assert!(
            fixed + suffix <= target,
            "retained context must fit the post-compaction target"
        );
        assert!(
            end < msgs.len(),
            "the planner should retain a recent suffix when the target allows it"
        );
    }

    #[test]
    fn summary_prompt_is_bounded_and_keeps_both_edges() {
        let messages: Vec<_> = (0..400)
            .map(|index| {
                make_msg(
                    CanonicalRole::User,
                    &format!("entry-{index} {}", "long detail ".repeat(80)),
                )
            })
            .collect();

        let prompt = ContextCompactor::build_summary_prompt(&messages);

        assert!(
            estimate_tokens(&prompt) <= SUMMARY_INPUT_TOKEN_BUDGET,
            "summary input must stay bounded, got {} tokens",
            estimate_tokens(&prompt)
        );
        assert!(prompt.contains("entry-0"));
        assert!(prompt.contains("entry-399"));
        assert!(prompt.contains("omitted from this summary input"));
    }

    #[test]
    fn summary_output_truncation_is_token_bounded() {
        let text = "摘要内容 ".repeat(2_000);
        let truncated = truncate_to_token_budget(&text, SUMMARY_TEXT_TOKEN_BUDGET);

        assert!(!truncated.is_empty());
        assert!(estimate_tokens(&truncated) <= SUMMARY_TEXT_TOKEN_BUDGET);
    }
}
