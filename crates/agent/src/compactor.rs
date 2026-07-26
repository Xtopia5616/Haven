use haven_common::types::{CanonicalMessage, ContentPart};
use haven_llm::{EndpointRole, LlmMessage, LlmRole, LlmRouter};
use std::sync::LazyLock;
use std::sync::Arc;
use tiktoken_rs::o200k_base;

static TOKENIZER: LazyLock<tiktoken_rs::CoreBPE> = LazyLock::new(|| {
    o200k_base().expect("failed to initialize o200k_base tokenizer")
});

/// Token estimation using o200k_base tokenizer for accurate counts.
pub fn estimate_tokens(text: &str) -> u32 {
    TOKENIZER.encode_with_special_tokens(text).len() as u32
}

/// Estimate tokens in a list of canonical messages by summing the content text.
pub fn estimate_message_tokens(messages: &[CanonicalMessage]) -> u32 {
    let mut total = 0u32;
    for msg in messages {
        for part in &msg.content {
            match part {
                ContentPart::Text(t) => total += estimate_tokens(t),
                ContentPart::Image { .. } => total += 200, // rough image token cost
            }
        }
        if msg.tool_calls.is_some() {
            total += 50;
        }
    }
    total
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
}

/// Context window pressure monitor and auto-compactor.
///
/// Monitors the total estimated token count of the canonical message list
/// after each ReAct step. When the estimate exceeds `context_window - reserve_tokens`,
/// it compresses the oldest messages into a single summary via the DefaultModel.
pub struct ContextCompactor {
    /// Soft limit: total context window in tokens (from model config).
    pub context_window: u32,
    /// Tokens to reserve for the response.
    pub reserve_tokens: u32,
}

impl ContextCompactor {
    pub fn new(context_window: u32, reserve_tokens: u32) -> Self {
        Self {
            context_window,
            reserve_tokens,
        }
    }

    /// Returns true when the message list exceeds the compact threshold,
    /// meaning compaction should be triggered before the next LLM call.
    pub fn needs_compaction(&self, messages: &[CanonicalMessage]) -> bool {
        let estimated = estimate_message_tokens(messages);
        estimated > self.context_window.saturating_sub(self.reserve_tokens)
    }

    /// Build a summarization prompt from the oldest messages (up to `max_summary_messages`).
    fn build_summary_prompt(prefix: &[CanonicalMessage]) -> String {
        let mut text = String::from(
            "Summarize this conversation. Keep key facts, decisions, and context:\n\n"
        );
        for msg in prefix {
            let role = match msg.role {
                haven_common::types::CanonicalRole::System => "system",
                haven_common::types::CanonicalRole::User => "user",
                haven_common::types::CanonicalRole::Assistant => "assistant",
                haven_common::types::CanonicalRole::Tool => "tool",
            };
            for part in &msg.content {
                if let ContentPart::Text(t) = part {
                    text.push_str(&format!("[{}] {}\n", role, t));
                }
            }
        }
        text.push_str("\n---\nSummary:");
        text
    }

    /// Compress the message list: take the oldest half (up to `max_summary_messages`)
    /// and replace them with a DefaultModel-generated summary.
    ///
    /// Returns `None` when compaction fails (e.g. LLM call fails) or there's nothing
    /// to compact (fewer than 4 messages).
    pub async fn compact(
        &self,
        messages: &[CanonicalMessage],
        router: &Arc<LlmRouter>,
    ) -> Option<CompactionResult> {
        // Need at least 4 messages to make compaction worthwhile
        if messages.len() < 4 {
            return None;
        }

        // Compact roughly the oldest half (but keep the system prompt if present)
        let system_count = messages
            .iter()
            .take_while(|m| matches!(m.role, haven_common::types::CanonicalRole::System))
            .count();

        let compactable = messages.len() - system_count;
        if compactable < 3 {
            return None;
        }

        let summarize_count = (compactable / 2).max(2);
        let end_idx = system_count + summarize_count;
        let prefix = &messages[..end_idx];
        let suffix = &messages[end_idx..];

        let tokens_before = estimate_message_tokens(messages);

        let prompt = Self::build_summary_prompt(prefix);
        let llm_messages = vec![
            LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text(prompt)],
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        match router.chat(EndpointRole::DefaultModel, llm_messages).await {
            Ok(response) => {
                let summary = response.text.trim().to_string();
                if summary.is_empty() {
                    return None;
                }

                let mut compacted: Vec<CanonicalMessage> = messages[..system_count].to_vec();
                compacted.push(CanonicalMessage {
                    role: haven_common::types::CanonicalRole::Assistant,
                    content: vec![ContentPart::text(format!(
                        "[Compacted summary of previous messages]: {}",
                        summary
                    ))],
                    tool_calls: None,
                    tool_call_id: None,
                    parent_message_id: None,
                });
                compacted.extend_from_slice(suffix);

                let tokens_after = estimate_message_tokens(&compacted);

                Some(CompactionResult {
                    compacted,
                    summarized_count: summarize_count,
                    summary,
                    tokens_before,
                    tokens_after,
                })
            }
            Err(e) => {
                tracing::warn!("Compaction LLM call failed: {}", e);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::types::CanonicalRole;

    fn make_msg(role: CanonicalRole, text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: None,
            parent_message_id: None,
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
        let text = "This is a long conversation history that should exceed the compaction threshold. ".repeat(10);
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
    fn compact_returns_none_for_few_messages() {
        let router = Arc::new(haven_llm::LlmRouter::new(haven_common::config::LlmConfig::default()));
        let compactor = ContextCompactor::new(4096, 512);
        let msgs = vec![
            make_msg(CanonicalRole::System, "You are Haven."),
            make_msg(CanonicalRole::User, "Hi"),
        ];
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(compactor.compact(&msgs, &router));
        assert!(result.is_none());
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
}
