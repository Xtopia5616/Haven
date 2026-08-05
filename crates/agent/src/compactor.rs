use crate::is_dangling_boundary;
use haven_common::prompts::CONVERSATION_SUMMARY_PROMPT;
use haven_common::types::{CanonicalMessage, ContentPart};
use haven_llm::{EndpointRole, LlmRouter};
use std::sync::Arc;
use std::sync::LazyLock;
use tiktoken_rs::o200k_base;

static TOKENIZER: LazyLock<tiktoken_rs::CoreBPE> =
    LazyLock::new(|| o200k_base().expect("failed to initialize o200k_base tokenizer"));

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
                ContentPart::Audio { .. } => total += 500, // rough audio token cost
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
/// after each ReAct step. When the estimate exceeds `context_window *
/// trigger_ratio` (clamped to leave room for the model's response and a
/// retry buffer), it compresses the oldest messages into a single summary
/// via the DefaultModel.
///
/// `trigger_ratio` defaults to 0.75 — i.e. compact when 75% of the context
/// window is consumed. The previous behaviour (`context_window -
/// reserve_tokens`) was too conservative and triggered compaction only when
/// the model was already close to overflowing, forcing an expensive
/// retry-and-resummarize cycle.
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
        let estimated = estimate_message_tokens(messages);
        let ratio_threshold = (self.context_window as f64 * self.trigger_ratio as f64) as u32;
        let headroom_threshold = self.context_window.saturating_sub(self.reserve_tokens);
        let threshold = ratio_threshold.min(headroom_threshold.max(1));
        estimated > threshold
    }

    /// Build a summarization prompt from the oldest messages (up to `max_summary_messages`).
    fn build_summary_prompt(prefix: &[CanonicalMessage]) -> String {
        let mut text = String::from(CONVERSATION_SUMMARY_PROMPT);
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
        let end_idx = Self::safe_end_idx(messages, system_count + summarize_count);
        let prefix = &messages[..end_idx];
        let suffix = &messages[end_idx..];

        let tokens_before = estimate_message_tokens(messages);

        let prompt = Self::build_summary_prompt(prefix);

        match router
            .chat_with_prompt(EndpointRole::DefaultModel, "", &prompt)
            .await
        {
            Ok(response) => {
                let summary = response.text.trim().to_string();
                if summary.is_empty() {
                    return None;
                }

                let mut compacted: Vec<CanonicalMessage> = messages[..system_count].to_vec();
                compacted.push(CanonicalMessage::assistant(
                    vec![ContentPart::text(format!(
                        "[Compacted summary of previous messages]: {}",
                        summary
                    ))],
                    None,
                    None,
                ));
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
            reasoning: None,
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
            haven_common::config::LlmConfig::default(),
        ));
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

    fn make_tool_result(text: &str) -> CanonicalMessage {
        CanonicalMessage {
            role: CanonicalRole::Tool,
            content: vec![ContentPart::text(text)],
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            parent_message_id: None,
            reasoning: None,
        }
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
}
