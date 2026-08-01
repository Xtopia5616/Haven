use std::sync::Arc;

use haven_common::types::ContentPart;
use haven_llm::types::{LlmMessage, LlmRole};
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use tokio::sync::Semaphore;

use crate::session::SessionManager;

/// Maximum characters of transcript sent to the BalancedModel for fact
/// extraction. Prevents unbounded token cost on long sessions.
const MAX_TRANSCRIPT_CHARS: usize = 4000;

/// A fact extracted by the LLM, deserialized from the model's JSON response.
#[derive(serde::Deserialize)]
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
    pub fn new(db: Arc<Database>, sessions: Arc<SessionManager>, router: Arc<LlmRouter>) -> Self {
        let _ = sessions; // Session ID is now passed explicitly to infer methods.
        Self {
            db,
            router,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    /// Extract facts from the specified session's user messages.
    ///
    /// Takes an explicit `session_id` to avoid a race where the global
    /// current-session pointer has already switched to the next task by
    /// the time this fire-and-forget task runs.
    ///
    /// Tries LLM-assisted extraction via the BalancedModel first. On any
    /// failure (network error, circuit breaker open, bad JSON), falls back
    /// to the rule-based extractor so inference is never silently skipped.
    /// An empty `Ok([])` from the LLM is treated as a valid "no facts found"
    /// response and does NOT trigger the fallback.
    pub async fn infer_facts(&self, session_id: &str) {
        let messages = match self.db.get_session_messages(session_id) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("fact inference: failed to load messages: {}", e);
                return;
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
                for f in &facts {
                    let tags: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                    let _ = self.db.insert_fact(
                        &sanitize_fact_field(&f.subject),
                        &sanitize_fact_field(&f.predicate),
                        &sanitize_fact_field(&f.object),
                        "inferred",
                        f.confidence,
                        &tags,
                    );
                }
                let _ = self.db.dedup_facts();
                let _ = self.db.flush_low_confidence(0.3);
            }
            Ok(_) => {
                tracing::debug!("LLM found no facts in session {}", session_id);
            }
            Err(e) => {
                tracing::warn!("LLM fact extraction failed ({}), falling back to rules", e);
                let inferred = self.db.infer_facts_from_messages(&user_messages);
                for f in &inferred {
                    let tags: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                    let _ = self.db.insert_fact(
                        &f.subject,
                        &f.predicate,
                        &f.object,
                        "inferred",
                        f.confidence,
                        &tags,
                    );
                }
                let _ = self.db.dedup_facts();
                let _ = self.db.flush_low_confidence(0.3);
            }
        }
    }

    /// Send the conversation transcript to the BalancedModel and ask it to
    /// extract user facts as a JSON array.
    async fn infer_facts_with_llm(
        &self,
        user_messages: &[haven_memory::repositories::messages::Message],
    ) -> anyhow::Result<Vec<LlmFact>> {
        let transcript = build_truncated_transcript(user_messages, MAX_TRANSCRIPT_CHARS);

        let system_prompt = "\
Extract factual information about the user from the conversation. \
Return a JSON array where each element has: \
\"subject\" (always \"user\"), \
\"predicate\" (short key: name, likes, dislikes, uses, works_at, project_path, etc.), \
\"object\" (the value), \
\"tags\" (array of: identity, preference, workspace, project), \
\"confidence\" (0.5-1.0). \
Only extract clear, explicit facts the user stated. \
If no facts found, return []. \
Respond with ONLY the JSON array, no markdown, no explanation.";

        let messages = vec![
            LlmMessage {
                role: LlmRole::System,
                content: vec![ContentPart::text(system_prompt)],
                tool_call_id: None,
                tool_calls: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: vec![ContentPart::text(&transcript)],
                tool_call_id: None,
                tool_calls: None,
            },
        ];

        let _permit = self
            .inference_semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("inference semaphore closed: {}", e))?;

        let response = self
            .router
            .chat(EndpointRole::BalancedModel, messages)
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

    /// Run cross-session preference inference.
    pub fn infer_preferences(&self, session_id: &str) {
        if let Ok(messages) = self.db.get_session_messages(session_id) {
            let inferred = self.db.infer_preferences_from_messages(&messages);
            let _ = self.db.save_inferred_preferences(&inferred);
        }
    }

    /// Run both fact and preference inference (common exit point in the ReAct loop).
    pub async fn infer_all(&self, session_id: &str) {
        self.infer_facts(session_id).await;
        self.infer_preferences(session_id);
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
            session_id: "s1".into(),
            role: "user".into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            is_compacted: false,
            compaction_id: None,
            parent_message_id: None,
            attachments: vec![],
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
