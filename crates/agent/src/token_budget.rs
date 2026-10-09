//! Shared token accounting and text-budget operations for Agent requests.

use haven_common::types::{CanonicalMessage, ContentPart};
use haven_llm::LlmToolDefinition;
use std::sync::{LazyLock, OnceLock};
use tiktoken_rs::o200k_base;

use crate::canonical::{canonical_pairing_healthy, sanitize_canonical};

static TOKENIZER: LazyLock<Result<tiktoken_rs::CoreBPE, String>> =
    LazyLock::new(|| o200k_base().map_err(|error| error.to_string()));
static TOKENIZER_WARNING_LOGGED: OnceLock<()> = OnceLock::new();

/// Estimate text tokens with o200k_base, falling back to a conservative
/// character estimate if tokenizer initialization fails.
pub(crate) fn estimate_tokens(text: &str) -> u32 {
    match &*TOKENIZER {
        Ok(tokenizer) => tokenizer.encode_with_special_tokens(text).len() as u32,
        Err(error) => {
            TOKENIZER_WARNING_LOGGED.get_or_init(|| {
                tracing::error!(error = %error, "failed to initialize tokenizer; using conservative character estimate");
            });
            text.chars().count().div_ceil(4) as u32
        }
    }
}

/// Estimate the provider-visible token cost of one canonical message.
///
/// The estimator intentionally stays provider-neutral. It covers content and
/// extra fields echoed into requests; protocol framing is accounted for by
/// provider adapters and the request overhead below.
pub(crate) fn estimate_message_token_cost(message: &CanonicalMessage) -> u32 {
    let mut total = 0u32;
    for part in &message.content {
        match part {
            ContentPart::Text(text) => total = total.saturating_add(estimate_tokens(text)),
            ContentPart::Image { .. } => total = total.saturating_add(200), // rough image token cost
            ContentPart::Audio { .. } => total = total.saturating_add(500), // rough audio token cost
            ContentPart::Video { .. } => total = total.saturating_add(1_500), // rough video token cost
        }
    }
    // Reasoning is echoed back to the provider on every request and can dwarf
    // visible message text, so it must participate in context budgeting.
    if let Some(reasoning) = &message.reasoning {
        total = total.saturating_add(estimate_tokens(reasoning));
    }
    // Provider thinking blocks and search items are echoed as raw JSON;
    // count the complete payload, including signatures and metadata.
    for block in &message.thinking_blocks {
        total = total.saturating_add(estimate_serialized_tokens(block));
    }
    if !message.web_search_calls.is_empty() {
        total = total.saturating_add(estimate_serialized_tokens(&message.web_search_calls));
    }
    if let Some(calls) = &message.tool_calls {
        // Tool arguments can be large (for example a generated patch).
        for call in calls {
            total = total
                .saturating_add(estimate_serialized_tokens(call))
                .saturating_add(10);
        }
    }
    if let Some(tool_call_id) = &message.tool_call_id {
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

/// Estimate the provider-visible token cost of canonical messages.
pub(crate) fn estimate_message_tokens(messages: &[CanonicalMessage]) -> u32 {
    messages
        .iter()
        .map(estimate_message_token_cost)
        .fold(0, u32::saturating_add)
}

/// Estimate the serialized schema cost of tool definitions for one request.
pub(crate) fn estimate_tool_tokens(tools: &[LlmToolDefinition]) -> u32 {
    serde_json::to_string(tools)
        .ok()
        .map(|json| estimate_tokens(&json))
        .unwrap_or_default()
}

/// Conservative token allowance for provider framing and adapter-side fields.
pub(crate) const PROVIDER_REQUEST_OVERHEAD_TOKENS: u32 = 256;

/// Estimate a provider request from current messages and tool definitions.
pub(crate) fn estimate_provider_request_tokens(
    messages: &[CanonicalMessage],
    tools: &[LlmToolDefinition],
) -> u32 {
    estimate_provider_request_tokens_with_estimates(
        messages,
        estimate_message_tokens(messages),
        estimate_tool_tokens(tools),
    )
}

/// Use version-scoped cached estimates when both request inputs are unchanged.
/// Malformed tool-call boundaries are sanitized on a clone before counting;
/// durable transcript state remains authoritative.
pub(crate) fn estimate_provider_request_tokens_with_estimates(
    messages: &[CanonicalMessage],
    cached_message_tokens: u32,
    tool_token_estimate: u32,
) -> u32 {
    let message_tokens = if canonical_pairing_healthy(messages) {
        cached_message_tokens
    } else {
        let mut provider_messages = messages.to_vec();
        sanitize_canonical(&mut provider_messages);
        estimate_message_tokens(&provider_messages)
    };
    message_tokens
        .saturating_add(tool_token_estimate)
        .saturating_add(PROVIDER_REQUEST_OVERHEAD_TOKENS)
}

/// Keep the longest prefix whose estimated token count fits the budget.
pub(crate) fn truncate_prefix_to_token_budget(text: &str, max_tokens: u32) -> String {
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

/// Keep the longest suffix whose estimated token count fits the budget.
pub(crate) fn truncate_suffix_to_token_budget(text: &str, max_tokens: u32) -> String {
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
