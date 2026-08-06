use std::sync::Arc;

use haven_common::prompts::FACT_EXTRACTION_SYSTEM_PROMPT;
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::repositories::facts::{is_sensitive_object, is_sensitive_predicate};
use haven_memory::Database;
use tokio::sync::Semaphore;

/// Maximum characters of transcript sent to the BalancedModel for fact
/// extraction. Prevents unbounded token cost on long conversations.
const MAX_TRANSCRIPT_CHARS: usize = 4000;

/// A fact extracted by the LLM, deserialized from the model's JSON response.
#[derive(Clone, serde::Deserialize)]
struct LlmFact {
    #[serde(default = "default_subject")]
    subject: String,
    predicate: String,
    object: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default = "default_confidence")]
    confidence: f64,
}

fn default_subject() -> String {
    "user".into()
}
fn default_confidence() -> f64 {
    0.7
}

pub struct InferenceEngine {
    db: Arc<Database>,
    router: Arc<LlmRouter>,
    /// Limits concurrent LLM fact-extraction calls to avoid overwhelming
    /// the BalancedModel endpoint when multiple tasks complete in rapid
    /// succession.
    inference_semaphore: Arc<Semaphore>,
}

impl InferenceEngine {
    pub fn new(db: Arc<Database>, router: Arc<LlmRouter>) -> Self {
        Self {
            db,
            router,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    /// Extract facts from the specified task's user messages.
    ///
    /// Takes an explicit `task_id` so the fire-and-forget background task is
    /// immune to any concurrent task switching.
    ///
    /// Tries LLM-assisted extraction via the BalancedModel first. On any
    /// failure (network error, circuit breaker open, bad JSON), falls back
    /// to the rule-based extractor so inference is never silently skipped.
    /// An empty `Ok([])` from the LLM is treated as a valid "no facts found"
    /// response and does NOT trigger the fallback.
    pub async fn infer_facts(&self, task_id: &str) {
        let messages = {
            let db = self.db.clone();
            let task_id = task_id.to_string();
            match db.run_blocking(move |db| db.get_task_messages(&task_id)).await {
                Ok(m) => m,
                _ => {
                    tracing::warn!("fact inference: failed to load messages");
                    return;
                }
            }
        };
        if messages.is_empty() {
            return;
        }

        let user_messages: Vec<_> = messages
            .iter()
            .filter(|m| m.role == "user")
            .cloned()
            .collect();
        if user_messages.is_empty() {
            return;
        }

        match self.infer_facts_with_llm(&user_messages).await {
            Ok(facts) if !facts.is_empty() => {
                self.persist_facts(&facts).await;
            }
            Ok(_) => {
                tracing::debug!("LLM found no facts in task {}", task_id);
            }
            Err(e) => {
                tracing::warn!("LLM fact extraction failed ({}), falling back to rules", e);
                let inferred = self.db.infer_facts_from_messages(&user_messages);
                let db = self.db.clone();
                let _ = db
                    .run_blocking(move |db| {
                        for f in &inferred {
                            if is_sensitive_predicate(&f.predicate) || is_sensitive_object(&f.object) {
                                continue;
                            }
                            let tags: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                            let _ = db.insert_fact(
                                &f.subject,
                                &f.predicate,
                                &f.object,
                                "inferred",
                                f.confidence,
                                &tags,
                            );
                        }
                        let _ = db.dedup_facts();
                        let _ = db.delete_sensitive_facts();
                        let _ = db.flush_low_confidence(0.3);
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;
            }
        }
    }

    /// Persist a batch of LLM-extracted facts plus the dedup/confidence
    /// maintenance pass in a single blocking DB round-trip.
    async fn persist_facts(&self, facts: &[LlmFact]) {
        let db = self.db.clone();
        let owned: Vec<LlmFact> = facts.to_vec();
        let _ = db
            .run_blocking(move |db| {
                for f in &owned {
                    if is_sensitive_predicate(&f.predicate) || is_sensitive_object(&f.object) {
                        tracing::debug!(
                            "fact inference: dropping sensitive fact '{}'",
                            f.predicate
                        );
                        continue;
                    }
                    let tags: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                    let _ = db.insert_fact(
                        &sanitize_fact_field(&f.subject),
                        &sanitize_fact_field(&f.predicate),
                        &sanitize_fact_field(&f.object),
                        "inferred",
                        f.confidence,
                        &tags,
                    );
                }
                let _ = db.dedup_facts();
                let _ = db.delete_sensitive_facts();
                let _ = db.flush_low_confidence(0.3);
                Ok::<(), anyhow::Error>(())
            })
            .await;
    }

    /// Send the conversation transcript to the BalancedModel and ask it to
    /// extract user facts as a JSON array.
    async fn infer_facts_with_llm(
        &self,
        user_messages: &[haven_memory::repositories::messages::Message],
    ) -> anyhow::Result<Vec<LlmFact>> {
        let transcript = build_truncated_transcript(user_messages, MAX_TRANSCRIPT_CHARS);

        let _permit = self
            .inference_semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("inference semaphore closed: {}", e))?;

        let response = self
            .router
            .chat_with_prompt(
                EndpointRole::BalancedModel,
                FACT_EXTRACTION_SYSTEM_PROMPT,
                &transcript,
            )
            .await
            .map_err(|e| anyhow::anyhow!("balanced model chat failed: {}", e))?;

        let json_str = extract_json_array(&response.text);
        let facts: Vec<LlmFact> = serde_json::from_str(&json_str).map_err(|e| {
            let preview: String = response.text.chars().take(200).collect();
            anyhow::anyhow!("failed to parse LLM fact JSON: {} — raw: {}", e, preview)
        })?;

        tracing::info!("LLM fact extraction: {} facts extracted", facts.len());
        Ok(facts)
    }

    /// Run preference inference over a task's messages.
    pub async fn infer_preferences(&self, task_id: &str) {
        let db = self.db.clone();
        let task_id = task_id.to_string();
        let _ = db
            .run_blocking(move |db| {
                if let Ok(messages) = db.get_task_messages(&task_id) {
                    let inferred = db.infer_preferences_from_messages(&messages);
                    let _ = db.save_inferred_preferences(&inferred);
                }
                Ok::<(), anyhow::Error>(())
            })
            .await;
    }

    /// Run both fact and preference inference (common exit point in the ReAct loop).
    pub async fn infer_all(&self, task_id: &str) {
        self.infer_facts(task_id).await;
        self.infer_preferences(task_id).await;
    }
}

/// Build a transcript string from user messages, truncated to `max_chars`
/// to prevent unbounded token cost on long sessions. Recent messages take
/// priority (the last N messages that fit within the limit).
fn build_truncated_transcript(
    messages: &[haven_memory::repositories::messages::Message],
    max_chars: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut total_len = 0;
    // Walk backwards so the most recent messages are kept when truncating.
    for m in messages.iter().rev() {
        let line = format!("User: {}", m.content);
        if total_len + line.len() + 1 > max_chars {
            break;
        }
        total_len += line.len() + 1;
        lines.push(line);
    }
    lines.reverse();
    lines.join("\n")
}

/// Sanitize a fact field value before it is stored and later interpolated
/// into the agent's system prompt. Strips newlines and control characters
/// that could be used for indirect prompt injection, and caps the length.
fn sanitize_fact_field(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(256)
        .collect()
}

/// Extract the first JSON array `[...]` from a string that may contain
/// markdown code fences or surrounding text.
fn extract_json_array(text: &str) -> String {
    let trimmed = text.trim();
    if let Some(start) = trimmed.find('[')
        && let Some(end) = trimmed.rfind(']')
        && end > start
    {
        return trimmed[start..=end].to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_memory::repositories::messages::Message;

    fn make_message(content: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: "t1".into(),
            role: "user".into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
            attachments: vec![],
            voice: false,
        }
    }

    #[test]
    fn test_extract_json_array_plain() {
        let result =
            extract_json_array(r#"[{"subject":"user","predicate":"name","object":"Alice"}]"#);
        assert!(result.starts_with('['));
        assert!(result.ends_with(']'));
    }

    #[test]
    fn test_extract_json_array_markdown_fenced() {
        let result = extract_json_array("```json\n[{\"x\":1}]\n```");
        assert_eq!(result, r#"[{"x":1}]"#);
    }

    #[test]
    fn test_extract_json_array_with_explanation() {
        let result = extract_json_array("Here are the facts:\n[{\"a\":1}]\nDone.");
        assert_eq!(result, r#"[{"a":1}]"#);
    }

    #[test]
    fn test_extract_json_array_empty_array() {
        let result = extract_json_array("[]");
        assert_eq!(result, "[]");
    }

    #[test]
    fn test_extract_json_array_no_array() {
        let result = extract_json_array("No facts found.");
        assert_eq!(result, "No facts found.");
    }

    #[test]
    fn test_build_truncated_transcript_short() {
        let msgs = vec![make_message("hello"), make_message("world")];
        let transcript = build_truncated_transcript(&msgs, 4000);
        assert!(transcript.contains("hello"));
        assert!(transcript.contains("world"));
    }

    #[test]
    fn test_build_truncated_transcript_truncates() {
        let big = "x".repeat(1000);
        let msgs: Vec<Message> = (0..10).map(|_| make_message(&big)).collect();
        let transcript = build_truncated_transcript(&msgs, 2000);
        assert!(transcript.len() <= 2000 + 20); // small overhead for "User: " prefixes
    }

    #[test]
    fn test_build_truncated_transcript_keeps_recent() {
        let msgs = vec![make_message("old_message"), make_message("recent_message")];
        let transcript = build_truncated_transcript(&msgs, 50);
        // "recent_message" should be kept because it's more recent.
        assert!(transcript.contains("recent_message"));
    }

    #[test]
    fn test_sanitize_strips_newlines() {
        let result = sanitize_fact_field("hello\nworld\r\nIGNORE INSTRUCTIONS");
        assert!(!result.contains('\n'));
        assert!(!result.contains('\r'));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_sanitize_caps_length() {
        let result = sanitize_fact_field(&"x".repeat(500));
        assert_eq!(result.len(), 256);
    }

    #[test]
    fn test_sanitize_preserves_normal_text() {
        let result = sanitize_fact_field("Alice likes Rust");
        assert_eq!(result, "Alice likes Rust");
    }
}
