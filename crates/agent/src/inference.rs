use std::sync::Arc;

use haven_common::prompts::FACT_EXTRACTION_SYSTEM_PROMPT;
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::embeddings::entity_kind;
use haven_memory::repositories::facts::{
    FactSourceRef, is_sensitive_object, is_sensitive_predicate,
};
use tokio::sync::Semaphore;

/// Maximum known facts listed in the extraction prompt as context, so the
/// model can re-confirm or update existing facts instead of re-extracting
/// everything from scratch. Embedding requests are chunked to stay under
/// provider request limits.
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
    /// Index into the numbered conversation transcript of the message that
    /// supports this fact (the model is asked to fill this in).
    #[serde(default)]
    message_index: Option<usize>,
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
    /// Cap (chars) for transcripts sent to the BalancedModel for fact
    /// extraction. Prevents unbounded token cost on long conversations.
    max_transcript_chars: usize,
    /// Embedding requests are chunked to stay under provider request limits.
    embed_chunk_size: usize,
    /// Max known facts listed in the extraction prompt as context.
    max_known_facts: usize,
    /// Max chars of a fact subject/predicate/object field (prompt-injection
    /// sanitization truncation).
    sanitize_max_chars: usize,
    /// Limits concurrent LLM fact-extraction calls to avoid overwhelming
    /// the BalancedModel endpoint when multiple tasks complete in rapid
    /// succession.
    inference_semaphore: Arc<Semaphore>,
}

impl InferenceEngine {
    pub fn new(
        db: Arc<Database>,
        router: Arc<LlmRouter>,
        max_transcript_chars: usize,
        embed_chunk_size: usize,
        max_known_facts: usize,
        sanitize_max_chars: usize,
    ) -> Self {
        Self {
            db,
            router,
            max_transcript_chars,
            embed_chunk_size,
            max_known_facts,
            sanitize_max_chars,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    /// Extract facts from the specified task's user messages.
    ///
    /// Takes an explicit `task_id` so the fire-and-forget background task is
    /// immune to any concurrent task switching.
    ///
    /// Extraction is incremental: a per-task cursor (stored in the internal
    /// kv_store as `fact_extraction.<task_id>` = last processed user-message
    /// id) makes re-runs process only the messages that arrived since the previous
    /// extraction instead of re-scanning the whole conversation. This keeps
    /// cost bounded on long tasks and makes fact decay meaningful 鈥?a fact's
    /// `last_seen_at` refreshes only when it is actually re-observed, not when
    /// the same old messages are re-scanned.
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
            match db
                .run_blocking(move |db| db.get_task_messages(&task_id))
                .await
            {
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

        // Incremental window: only the messages after the last-processed one.
        let cursor_key = format!("fact_extraction.{}", task_id);
        let cursor = self
            .db
            .run_blocking({
                let key = cursor_key.clone();
                move |db| db.get_kv(&key)
            })
            .await
            .ok()
            .flatten();
        let start = cursor
            .as_deref()
            .and_then(|c| user_messages.iter().position(|m| m.id == c))
            .map(|i| i + 1)
            .unwrap_or(0);
        if start >= user_messages.len() {
            tracing::debug!("fact inference: no new messages since cursor");
            // Even with no new user messages, catch up on pending indexing
            // (compaction summaries, facts added from other paths).
            self.embed_new_memory().await;
            return;
        }
        let new_messages = &user_messages[start..];

        let cursor_last = new_messages.last().map(|m| m.id.clone());

        match self.infer_facts_with_llm(new_messages).await {
            Ok(facts) if !facts.is_empty() => {
                self.persist_facts(&facts, new_messages).await;
            }
            Ok(_) => {
                tracing::debug!("LLM found no facts in task {}", task_id);
            }
            Err(e) => {
                tracing::warn!("LLM fact extraction failed ({}), falling back to rules", e);
                let inferred = self.db.infer_facts_from_messages(new_messages);
                let db = self.db.clone();
                let _ = db
                    .run_blocking(move |db| {
                        for f in &inferred {
                            if is_sensitive_predicate(&f.predicate)
                                || is_sensitive_object(&f.object)
                            {
                                continue;
                            }
                            let tags: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                            let _ = db.upsert_fact(
                                &f.subject,
                                &f.predicate,
                                &f.object,
                                "inferred",
                                f.confidence,
                                &tags,
                                f.source_ref.as_ref(),
                            );
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;
            }
        }

        // Advance the cursor so the next run only sees brand-new messages.
        if let Some(last) = cursor_last {
            let db = self.db.clone();
            let key = cursor_key.clone();
            let _ = db
                .run_blocking(move |db| {
                    db.set_kv(&key, &last)?;
                    Ok::<(), anyhow::Error>(())
                })
                .await;
        }

        // Index the new facts + conversation events into vector memory.
        self.embed_new_memory().await;
    }

    /// True when the vector index holds embeddings from a different model
    /// than the currently configured `embedding_model`. Vectors from another
    /// model are not comparable (dimension mismatch 鈫?cosine similarity
    /// degenerates to 0), so the index must be rebuilt.
    async fn embedding_model_changed(&self) -> bool {
        let current = self
            .router
            .config()
            .await
            .embedding_model
            .model_name
            .clone();
        if current.is_empty() {
            return false;
        }
        let db = self.db.clone();
        let stored: Vec<String> = db
            .run_blocking(move |db| db.list_embedding_models())
            .await
            .unwrap_or_default();
        !stored.is_empty() && stored.iter().any(|m| m != &current)
    }

    /// Embed any facts or conversation events (user messages, compaction
    /// summaries) that do not yet have a stored vector. No-op when the
    /// `embedding_model` slot is unconfigured, so the feature degrades
    /// gracefully to keyword-only retrieval.
    async fn embed_new_memory(&self) {
        if !self
            .router
            .is_role_configured(EndpointRole::EmbeddingModel)
            .await
        {
            tracing::debug!("embedding_model unconfigured; skipping vector indexing");
            return;
        }
        // Model switched since the last index? Drop the stale vectors so the
        // rebuild below starts from a clean, dimension-consistent index.
        if self.embedding_model_changed().await {
            let db = self.db.clone();
            let _ = db.run_blocking(move |db| db.clear_embeddings()).await;
            tracing::info!("embedding model changed: cleared vector index for rebuild");
        }
        let db = self.db.clone();
        let pending: Vec<(String, String, String)> = db
            .run_blocking(move |db| {
                let mut out: Vec<(String, String, String)> = Vec::new();
                for id in db
                    .missing_embedding_ids(entity_kind::FACT)
                    .unwrap_or_default()
                {
                    if let Ok(Some(text)) = db.fact_text_by_id(&id) {
                        out.push((entity_kind::FACT.to_string(), id, text));
                    }
                }
                for id in db
                    .missing_embedding_ids(entity_kind::EPISODE)
                    .unwrap_or_default()
                {
                    if let Ok(Some(text)) = db.episode_text(&id) {
                        out.push((entity_kind::EPISODE.to_string(), id, text));
                    }
                }
                Ok::<Vec<(String, String, String)>, anyhow::Error>(out)
            })
            .await
            .unwrap_or_default();
        if pending.is_empty() {
            return;
        }
        let fallback_model = self
            .router
            .config()
            .await
            .embedding_model
            .model_name
            .clone();
        tracing::info!("embedding {} memory items", pending.len());
        for chunk in pending.chunks(self.embed_chunk_size) {
            let texts: Vec<String> = chunk.iter().map(|(_, _, t)| t.clone()).collect();
            match self.router.embed(texts).await {
                Ok(emb) => {
                    let model = emb.model.clone().unwrap_or_else(|| fallback_model.clone());
                    let owned: Vec<(String, String, String, Vec<f32>)> = chunk
                        .iter()
                        .zip(emb.vectors)
                        .filter(|(_, v)| !v.is_empty())
                        .map(|((kind, id, text), v)| (kind.clone(), id.clone(), text.clone(), v))
                        .collect();
                    let db = self.db.clone();
                    let _ = db
                        .run_blocking(move |db| {
                            for (kind, id, text, vector) in owned {
                                let _ = db.save_embedding(&kind, &id, &model, &vector, &text);
                            }
                            Ok::<(), anyhow::Error>(())
                        })
                        .await;
                }
                Err(e) => {
                    tracing::warn!("embedding batch failed: {}", e);
                    return;
                }
            }
        }
    }

    /// Full memory maintenance pass, independent of any extraction: collapse
    /// duplicate facts, purge sensitive facts, flush stale low-confidence
    /// facts, and prune embeddings whose source rows were deleted. Run after
    /// each extraction AND periodically via the app-level scheduler so decay
    /// never depends on inference having happened.
    pub async fn run_memory_maintenance(&self) {
        let db = self.db.clone();
        let _ = db
            .run_blocking(move |db| {
                let _ = db.dedup_facts();
                let _ = db.delete_sensitive_facts();
                let _ = db.flush_low_confidence(0.3);
                let _ = db.prune_orphaned_embeddings();
                let _ = db.cleanup_orphan_extraction_cursors();
                Ok::<(), anyhow::Error>(())
            })
            .await;
        // Catch up on vector indexing too, so memory that accumulated while
        // the embedding model was unconfigured gets indexed once it is set up.
        self.embed_new_memory().await;
    }

    /// Retrieve the memory items most relevant to `query`. Uses the
    /// `embedding_model` slot when configured (embed the query, then cosine
    /// search over the stored vectors); otherwise falls back to keyword
    /// search. `kind` is `"fact"` or `"episode"`. Returns a JSON-friendly
    /// list of `{ entity_id, text, score, model }` objects.
    pub async fn recall_memory(
        &self,
        query: &str,
        kind: &str,
        limit: usize,
    ) -> Vec<serde_json::Value> {
        let entity = if kind == entity_kind::EPISODE {
            entity_kind::EPISODE
        } else {
            entity_kind::FACT
        };
        let limit = limit.clamp(1, 20);

        // Vector path: embed the query and cosine-search the index. Skipped
        // when the index still holds vectors from a previous model (they are
        // dimension-incompatible; maintenance rebuilds the index).
        if self
            .router
            .is_role_configured(EndpointRole::EmbeddingModel)
            .await
            && !self.embedding_model_changed().await
            && let Ok(vec) = self.router.embed_text(query).await
            && !vec.is_empty()
        {
            let db = self.db.clone();
            let entity_owned = entity.to_string();
            if let Ok(hits) = db
                .run_blocking(move |db| db.search_embeddings(&entity_owned, &vec, limit))
                .await
            {
                return hits
                    .into_iter()
                    .map(|(e, score)| {
                        serde_json::json!({
                            "entity_id": e.entity_id,
                            "text": e.text,
                            "score": score,
                            "model": e.model,
                        })
                    })
                    .collect();
            }
        }

        // Keyword fallback.
        let db = self.db.clone();
        let query_owned = query.to_string();
        db.run_blocking(move |db| {
            let hits: Vec<serde_json::Value> = if entity == entity_kind::EPISODE {
                let terms: Vec<&str> = query_owned.split_whitespace().collect();
                db.search_episodes_by_keywords(&terms, limit)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|text| serde_json::json!({ "entity_id": "", "text": text, "score": 0.0, "model": "" }))
                    .collect()
            } else {
                db.search_facts(&query_owned)
                    .unwrap_or_default()
                    .into_iter()
                    .take(limit)
                    .map(|f| {
                        serde_json::json!({
                            "entity_id": f.id,
                            "text": format!("{}={}", f.predicate, f.object),
                            "score": f.confidence,
                            "model": "",
                        })
                    })
                    .collect()
            };
            Ok::<Vec<serde_json::Value>, anyhow::Error>(hits)
        })
        .await
        .unwrap_or_default()
    }

    /// Persist a batch of LLM-extracted facts plus the maintenance pass in a
    /// single blocking DB round-trip. `user_messages` resolves each fact's
    /// `message_index` into a `FactSourceRef` for traceability.
    async fn persist_facts(
        &self,
        facts: &[LlmFact],
        user_messages: &[haven_memory::repositories::messages::Message],
    ) {
        let refs: Vec<Option<FactSourceRef>> = facts
            .iter()
            .map(|f| {
                f.message_index
                    .and_then(|idx| user_messages.get(idx))
                    .map(|m| FactSourceRef::from_message(&m.id, &m.content))
            })
            .collect();
        let owned: Vec<(LlmFact, Option<FactSourceRef>)> =
            facts.iter().cloned().zip(refs).collect();
        let db = self.db.clone();
        let sanitize_max = self.sanitize_max_chars;
        let _ = db
            .run_blocking(move |db| {
                for (f, src_ref) in &owned {
                    if is_sensitive_predicate(&f.predicate) || is_sensitive_object(&f.object) {
                        tracing::debug!(
                            "fact inference: dropping sensitive fact '{}'",
                            f.predicate
                        );
                        continue;
                    }
                    let tags: Vec<&str> = f.tags.iter().map(|s| s.as_str()).collect();
                    let _ = db.upsert_fact(
                        &sanitize_fact_field(&f.subject, sanitize_max),
                        &sanitize_fact_field(&f.predicate, sanitize_max),
                        &sanitize_fact_field(&f.object, sanitize_max),
                        "inferred",
                        f.confidence,
                        &tags,
                        src_ref.as_ref(),
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
    /// extract user facts as a JSON array. The transcript numbers each user
    /// message (`[N] ...`) and is prefixed with the already-stored facts, so
    /// the model can re-confirm or update existing memory instead of only
    /// emitting brand-new facts.
    async fn infer_facts_with_llm(
        &self,
        user_messages: &[haven_memory::repositories::messages::Message],
    ) -> anyhow::Result<Vec<LlmFact>> {
        let transcript = build_truncated_transcript(user_messages, self.max_transcript_chars);
        let known_facts = self.load_known_facts().await;
        let user_content = if known_facts.is_empty() {
            transcript
        } else {
            format!(
                "Known user facts (already stored; re-confirming one is fine, output a new value if the user changed it):\n{}\n\nConversation (each message is numbered as [N] 鈥?set \"message_index\" to the number supporting each fact):\n{}",
                known_facts, transcript
            )
        };

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
                &user_content,
            )
            .await
            .map_err(|e| anyhow::anyhow!("balanced model chat failed: {}", e))?;

        let json_str = extract_json_array(&response.text);
        let facts: Vec<LlmFact> = serde_json::from_str(&json_str).map_err(|e| {
            let preview: String = response.text.chars().take(200).collect();
            anyhow::anyhow!("failed to parse LLM fact JSON: {} 鈥?raw: {}", e, preview)
        })?;

        tracing::info!("LLM fact extraction: {} facts extracted", facts.len());
        Ok(facts)
    }

    /// Compact list of the stored user facts (effective-confidence order) to
    /// hand the extraction model as context.
    async fn load_known_facts(&self) -> String {
        let db = self.db.clone();
        let facts = db
            .run_blocking(move |db| db.get_facts("user"))
            .await
            .unwrap_or_default();
        let mut lines: Vec<String> = Vec::new();
        for fact in facts.iter().take(self.max_known_facts) {
            lines.push(format!(
                "- {}={} ({:.0}%)",
                sanitize_fact_field(&fact.predicate, self.sanitize_max_chars),
                sanitize_fact_field(&fact.object, self.sanitize_max_chars),
                haven_memory::repositories::facts::fact_effective_confidence(fact) * 100.0
            ));
        }
        lines.join("\n")
    }

    /// Run fact inference (the single memory channel — preferences are facts
    /// tagged `preference`), followed by the maintenance pass so stale facts
    /// are flushed even when extraction found nothing new.
    pub async fn infer_all(&self, task_id: &str) {
        self.infer_facts(task_id).await;
        self.run_memory_maintenance().await;
    }
}

/// Build a transcript string from user messages, truncated to `max_chars`
/// to prevent unbounded token cost on long sessions. Recent messages take
/// priority (the last N messages that fit within the limit). Each line is
/// prefixed with its absolute index `[N]` in the input slice 鈥?truncation
/// may drop old lines, but the numbering stays stable so the model's
/// `message_index` values map straight back into the slice.
fn build_truncated_transcript(
    messages: &[haven_memory::repositories::messages::Message],
    max_chars: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut total_len = 0;
    // Walk backwards so the most recent messages are kept when truncating.
    for (i, m) in messages.iter().enumerate().rev() {
        let line = format!("[{}] {}", i, m.content);
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
fn sanitize_fact_field(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|c| !c.is_control() || *c == ' ')
        .take(max_chars)
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
    use async_trait::async_trait;
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmMessage, LlmResponse, StreamChunk};
    use haven_memory::repositories::messages::Message;
    use std::pin::Pin;

    /// Mock whose chat answers with a fixed JSON fact array.
    struct FakeLlm {
        reply: String,
    }

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(&self, _: Vec<LlmMessage>) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: self.reply.clone(),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: haven_llm::types::Usage::default(),
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
            })
        }
        async fn chat_stream(
            &self,
            _: Vec<LlmMessage>,
        ) -> Result<
            Pin<Box<dyn futures_util::Stream<Item = Result<StreamChunk, LlmError>> + Send>>,
            LlmError,
        > {
            Err(LlmError::Unknown("mock: no stream".into()))
        }
        async fn health_check(&self) -> Result<(), LlmError> {
            Ok(())
        }
    }

    fn mock_router(reply: &str) -> Arc<LlmRouter> {
        let client: Arc<dyn LlmClient> = Arc::new(FakeLlm {
            reply: reply.to_string(),
        });
        Arc::new(LlmRouter::new_with_clients_full(
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client.clone(),
            client,
        ))
    }

    fn temp_db() -> Arc<Database> {
        let dir =
            std::env::temp_dir().join(format!("haven_inference_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(Database::open(&dir.join("test.db")).unwrap())
    }

    fn make_message(content: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: "t1".into(),
            role: "user".into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
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
        // Messages are numbered with their absolute index.
        assert!(transcript.contains("[0] hello"));
        assert!(transcript.contains("[1] world"));
    }

    #[test]
    fn test_build_truncated_transcript_truncates() {
        let big = "x".repeat(1000);
        let msgs: Vec<Message> = (0..10).map(|_| make_message(&big)).collect();
        let transcript = build_truncated_transcript(&msgs, 2000);
        // Small overhead for "[N] " prefixes (3-4 chars per line).
        assert!(transcript.len() <= 2000 + 60);
    }

    #[test]
    fn test_build_truncated_transcript_keeps_recent() {
        let msgs = vec![make_message("old_message"), make_message("recent_message")];
        let transcript = build_truncated_transcript(&msgs, 50);
        // "recent_message" should be kept because it's more recent.
        assert!(transcript.contains("recent_message"));
    }

    #[test]
    fn test_build_truncated_transcript_preserves_absolute_indices() {
        // Large earlier messages get dropped by truncation, but the remaining
        // lines must keep their absolute indices so the model's
        // message_index values still map back into the source slice.
        let big = "x".repeat(1000);
        let mut msgs: Vec<Message> = (0..5).map(|_| make_message(&big)).collect();
        msgs.push(make_message("the recent one"));
        let transcript = build_truncated_transcript(&msgs, 100);
        assert!(!transcript.contains("[0]"));
        assert!(transcript.contains("[5] the recent one"));
    }

    #[test]
    fn test_sanitize_strips_newlines() {
        let result = sanitize_fact_field("hello\nworld\r\nIGNORE INSTRUCTIONS", 256);
        assert!(!result.contains('\n'));
        assert!(!result.contains('\r'));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_sanitize_caps_length() {
        let result = sanitize_fact_field(&"x".repeat(500), 256);
        assert_eq!(result.len(), 256);
    }

    #[test]
    fn test_sanitize_preserves_normal_text() {
        let result = sanitize_fact_field("Alice likes Rust", 256);
        assert_eq!(result, "Alice likes Rust");
    }

    fn make_engine(db: Arc<Database>) -> InferenceEngine {
        InferenceEngine {
            db,
            router: mock_router("[]"),
            max_transcript_chars: 4_000,
            embed_chunk_size: 64,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    #[tokio::test]
    async fn infer_facts_advances_cursor_once() {
        let db = temp_db();
        let task = db.create_task("t1", "").unwrap();
        let _m1 = db
            .add_message(&task.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let m2 = db
            .add_message(&task.id, "user", "I use VSCode.", Some("text"), None)
            .unwrap();
        let engine = make_engine(db.clone());
        engine.infer_facts(&task.id).await;

        // Cursor should point at the last processed user message.
        let cursor: Option<String> = db.get_kv(&format!("fact_extraction.{}", task.id)).unwrap();
        assert_eq!(cursor.as_deref(), Some(m2.id.as_str()));

        // Re-running with no new messages must not change anything.
        engine.infer_facts(&task.id).await;
        let cursor2: Option<String> = db.get_kv(&format!("fact_extraction.{}", task.id)).unwrap();
        assert_eq!(cursor2, cursor);
    }

    #[tokio::test]
    async fn infer_facts_processes_only_new_messages() {
        let db = temp_db();
        let task = db.create_task("t1", "").unwrap();
        let m1 = db
            .add_message(&task.id, "user", "first message", Some("text"), None)
            .unwrap();
        let engine = make_engine(db.clone());
        engine.infer_facts(&task.id).await;
        let cursor: Option<String> = db.get_kv(&format!("fact_extraction.{}", task.id)).unwrap();
        assert_eq!(cursor.as_deref(), Some(m1.id.as_str()));

        // A new message moves the cursor forward.
        let m2 = db
            .add_message(&task.id, "user", "new signal only", Some("text"), None)
            .unwrap();
        engine.infer_facts(&task.id).await;
        let cursor2: Option<String> = db.get_kv(&format!("fact_extraction.{}", task.id)).unwrap();
        assert_eq!(cursor2.as_deref(), Some(m2.id.as_str()));
    }

    #[tokio::test]
    async fn infer_facts_rule_fallback_persists_and_indexes() {
        // Balanced model reply is not valid JSON -> falls back to the rule
        // extractor, which must still persist facts.
        let db = temp_db();
        let task = db.create_task("t1", "").unwrap();
        let _ = db
            .add_message(&task.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let engine = InferenceEngine {
            db: db.clone(),
            router: mock_router("not a json array"),
            max_transcript_chars: 4_000,
            embed_chunk_size: 64,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        };
        engine.infer_facts(&task.id).await;
        let facts = db.get_facts("user").unwrap();
        assert!(
            facts
                .iter()
                .any(|f| f.predicate == "likes" && f.object == "Rust"),
            "rule fallback must extract likes=Rust"
        );
        let cursor: Option<String> = db.get_kv(&format!("fact_extraction.{}", task.id)).unwrap();
        assert!(cursor.is_some());
    }
}
