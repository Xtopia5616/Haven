use std::sync::Arc;

use haven_common::prompts::FACT_EXTRACTION_SYSTEM_PROMPT;
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::embeddings::entity_kind;
use haven_memory::repositories::facts::{
    FactSourceRef, is_sensitive_object, is_sensitive_predicate, is_single_valued_predicate,
};
use serde::Deserialize;
use tokio::sync::Semaphore;

/// Maximum known facts listed in the extraction prompt as context, so the
/// model can re-confirm or update existing facts instead of re-extracting
/// everything from scratch. Embedding requests are chunked to stay under
/// provider request limits.
/// A fact extracted by the LLM, deserialized from the model's JSON response.
#[derive(Clone, serde::Deserialize)]
struct LlmFact {
    #[serde(default = "default_subject", deserialize_with = "coerce_to_string")]
    subject: String,
    #[serde(deserialize_with = "coerce_to_string")]
    predicate: String,
    #[serde(deserialize_with = "coerce_to_string")]
    object: String,
    #[serde(default, deserialize_with = "coerce_string_array")]
    tags: Vec<String>,
    #[serde(default = "default_confidence")]
    confidence: f64,
    /// 0..1 rating of how long this fact stays useful. Missing/unsure falls
    /// back to 0.6 (moderately durable) so an omitted field does not make a
    /// fact immortal by defaulting to 1.0.
    #[serde(default)]
    durability: Option<f64>,
    /// Index into the numbered conversation transcript of the message that
    /// supports this fact (the model is asked to fill this in).
    #[serde(default)]
    message_index: Option<usize>,
}

fn default_subject() -> String {
    "user".into()
}

/// Deserialize any JSON value into a string. The extraction model sometimes
/// emits booleans or numbers for fact fields (e.g. `"object": true`), which
/// would otherwise hard-fail the whole batch; coerce them to their string
/// form instead of dropping the fact.
/// Coerce any JSON value to its string form for a fact field. The extraction
/// model sometimes emits booleans or numbers (e.g. `"object": true`), which
/// would otherwise hard-fail the whole batch; coerce them instead of dropping
/// the fact. Single shared implementation used by both the scalar and array
/// deserializers so the coercion policy cannot drift.
fn coerce_value_to_string(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s,
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn coerce_to_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(coerce_value_to_string(value))
}

/// Deserialize an array of arbitrary JSON values into strings, coercing each
/// element the same way `coerce_to_string` does.
fn coerce_string_array<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<serde_json::Value>::deserialize(deserializer)?;
    Ok(values.into_iter().map(coerce_value_to_string).collect())
}
fn default_confidence() -> f64 {
    0.7
}

/// One extracted fact ready for the shared persistence path:
/// (subject, predicate, object, confidence, tags, source reference, durability).
type FactDraft = (
    String,
    String,
    String,
    f64,
    Vec<String>,
    Option<FactSourceRef>,
    f64,
);

/// Fact tags allowed to enter long-term memory. The extraction prompt asks
/// the model to stick to these, but it may still emit arbitrary values; this
/// whitelist keeps the prompt-side grouping (`tags.first()`) clean and stops
/// tag drift from polluting the facts index.
const ALLOWED_FACT_TAGS: &[&str] = &["identity", "preference", "workspace", "project"];

/// Keep only tags from the allowed set, normalized to lowercase, capped in
/// number and length so a stray model output cannot inflate the tag column.
fn sanitize_tags(tags: &[String]) -> Vec<String> {
    tags.iter()
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| ALLOWED_FACT_TAGS.contains(&t.as_str()))
        .take(4)
        .collect()
}

/// Normalize a predicate to its canonical form (trim + lowercase + alias
/// mapping). Delegates to the memory layer so the inference path and the
/// repository write paths share ONE normalization policy.
fn normalize_predicate(predicate: &str) -> String {
    haven_memory::repositories::facts::normalize_predicate(predicate)
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
    /// Min wall-clock seconds between LLM extraction calls per session
    /// (time-based throttle, complements the step-based react gate).
    fact_extraction_min_interval_secs: u64,
    /// Limits concurrent LLM fact-extraction calls to avoid overwhelming
    /// the BalancedModel endpoint when multiple sessions complete in rapid
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
        fact_extraction_min_interval_secs: u64,
    ) -> Self {
        Self {
            db,
            router,
            max_transcript_chars,
            embed_chunk_size,
            max_known_facts,
            sanitize_max_chars,
            fact_extraction_min_interval_secs,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    /// Extract facts from the specified session's user messages.
    ///
    /// Takes an explicit `session_id` so the fire-and-forget background session is
    /// immune to any concurrent session switching.
    ///
    /// Extraction is incremental: a per-session cursor (stored in the internal
    /// kv_store as `fact_extraction.<session_id>` = last processed user-message
    /// id) makes re-runs process only the messages that arrived since the previous
    /// extraction instead of re-scanning the whole conversation. This keeps
    /// cost bounded on long sessions and makes fact decay meaningful —a fact's
    /// `last_seen_at` refreshes only when it is actually re-observed, not when
    /// the same old messages are re-scanned.
    ///
    /// Tries LLM-assisted extraction via the BalancedModel first. On any
    /// failure (network error, circuit breaker open, bad JSON) the extraction
    /// is skipped for this window with a non-fatal warning — nothing is
    /// persisted, and the cursor still advances so a persistent failure does
    /// not re-analyze the same messages every turn. An empty `Ok([])` from
    /// the LLM is treated as a valid "no facts found" response.
    ///
    /// Extraction is also time-throttled: a run within
    /// `fact_extraction_min_interval_secs` of the previous one for the same
    /// session returns early WITHOUT touching the cursor, so the pending
    /// messages are still processed by the next allowed run (and by the
    /// maintenance pass regardless).
    pub async fn infer_facts(&self, session_id: &str) {
        self.infer_facts_inner(session_id, false).await;
    }

    /// Pause-path extraction: bypasses the time throttle so a same-step
    /// interval infer cannot starve the post-pause pass that has the
    /// fresher transcript (Phase 3 / G2).
    pub async fn infer_facts_on_pause(&self, session_id: &str) {
        self.infer_facts_inner(session_id, true).await;
    }

    async fn infer_facts_inner(&self, session_id: &str, bypass_throttle: bool) {
        // Time throttle: at most one LLM extraction per interval per session.
        // kv_store key `fact_extraction_last_run.<session_id>` = RFC3339 of
        // the last run that actually called the model. Note the underscore
        // namespace (NOT `fact_extraction.`): the orphan-cursor cleanup
        // matches `fact_extraction.%` and would wipe this stamp every
        // maintenance pass.
        if !bypass_throttle && self.fact_extraction_min_interval_secs > 0 {
            let last_key = format!("fact_extraction_last_run.{}", session_id);
            let last_run = self
                .db
                .run_blocking({
                    let key = last_key.clone();
                    move |db| db.get_kv(&key)
                })
                .await
                .ok()
                .flatten();
            if let Some(ts) = last_run
                && let Ok(prev) = chrono::DateTime::parse_from_rfc3339(&ts)
                && (chrono::Utc::now() - prev.with_timezone(&chrono::Utc)).num_seconds()
                    < self.fact_extraction_min_interval_secs as i64
            {
                tracing::debug!(
                    "fact inference: throttled (last run {} < {}s ago) for session {}",
                    ts,
                    self.fact_extraction_min_interval_secs,
                    session_id
                );
                return;
            }
        }

        let messages = {
            let db = self.db.clone();
            let session_id = session_id.to_string();
            match db
                .run_blocking(move |db| db.get_session_messages(&session_id))
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
        let cursor_key = format!("fact_extraction.{}", session_id);
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
            // Nothing new to extract; indexing catch-up happens in the
            // bounded hot-path embed after `infer_session`, or the full
            // maintenance pass the app scheduler runs.
            return;
        }
        let new_messages = &user_messages[start..];

        let cursor_last = new_messages.last().map(|m| m.id.clone());

        // Stamp the run timestamp BEFORE calling the model: the throttle
        // guards "no more than one LLM call per interval", so even a failed
        // call counts as a run (otherwise a persistent failure would retry
        // every turn despite the cursor advancing).
        if self.fact_extraction_min_interval_secs > 0 {
            let db = self.db.clone();
            let key = format!("fact_extraction_last_run.{}", session_id);
            let now = chrono::Utc::now().to_rfc3339();
            let _ = db
                .run_blocking(move |db| {
                    db.set_kv(&key, &now)?;
                    Ok::<(), anyhow::Error>(())
                })
                .await;
        }

        match self.infer_facts_with_llm(new_messages).await {
            Ok(facts) if !facts.is_empty() => {
                self.persist_facts(&facts, new_messages).await;
            }
            Ok(_) => {
                tracing::debug!("LLM found no facts in session {}", session_id);
            }
            Err(e) => {
                // Non-fatal: skip extraction for this window. The extraction
                // cursor is still advanced below so a persistent failure does
                // not re-analyze the same messages every turn; the maintenance
                // pass keeps memory consistent regardless.
                tracing::warn!(
                    "LLM fact extraction failed for session {}, skipping: {}",
                    session_id,
                    e
                );
            }
        }

        // Advance the cursor so the next run only sees brand-new messages.
        if let Some(last) = cursor_last {
            let db = self.db.clone();
            let key = cursor_key.clone();
            if let Err(e) = db
                .run_blocking(move |db| {
                    db.set_kv(&key, &last)?;
                    Ok::<(), anyhow::Error>(())
                })
                .await
            {
                tracing::warn!(
                    "fact extraction cursor advance failed for session {}: {}",
                    session_id,
                    e
                );
            }
        }
    }

    /// True when the vector index holds embeddings from a different model
    /// than the currently configured `embedding_model`. Vectors from another
    /// model are not comparable (dimension mismatch → cosine similarity
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
        let stored: Vec<String> = match db.run_blocking(move |db| db.list_embedding_models()).await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "embedding_model_changed: list_embedding_models failed: {}",
                    e
                );
                Vec::new()
            }
        };
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
            if let Err(e) = db.run_blocking(move |db| db.clear_embeddings()).await {
                tracing::error!(
                    "embedding model changed: failed to clear vector index: {}",
                    e
                );
            } else {
                tracing::info!("embedding model changed: cleared vector index for rebuild");
            }
        }
        let db = self.db.clone();
        let pending_raw = db
            .run_blocking(move |db| {
                let mut out: Vec<(String, String, String)> = Vec::new();
                match db.missing_embedding_ids(entity_kind::FACT) {
                    Ok(ids) => {
                        for id in ids {
                            match db.fact_text_by_id(&id) {
                                Ok(Some(text)) => {
                                    out.push((entity_kind::FACT.to_string(), id, text));
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        "embed_new_memory: fact_text_by_id failed for {}: {}",
                                        id,
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "embed_new_memory: missing_embedding_ids(fact) failed: {}",
                            e
                        );
                    }
                }
                match db.missing_embedding_ids(entity_kind::EPISODE) {
                    Ok(ids) => {
                        for id in ids {
                            match db.episode_text(&id) {
                                Ok(Some(text)) => {
                                    out.push((entity_kind::EPISODE.to_string(), id, text));
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    tracing::warn!(
                                        "embed_new_memory: episode_text failed for {}: {}",
                                        id,
                                        e
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "embed_new_memory: missing_embedding_ids(episode) failed: {}",
                            e
                        );
                    }
                }
                Ok::<Vec<(String, String, String)>, anyhow::Error>(out)
            })
            .await;
        let pending: Vec<(String, String, String)> = match pending_raw {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "embed_new_memory: failed to collect pending embedding items: {}",
                    e
                );
                Vec::new()
            }
        };
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
                    let batch_len = owned.len();
                    let _ = db
                        .run_blocking(move |db| {
                            let mut failures = 0usize;
                            for (kind, id, text, vector) in owned {
                                if let Err(e) =
                                    db.save_embedding(&kind, &id, &model, &vector, &text)
                                {
                                    failures += 1;
                                    if failures <= 3 {
                                        tracing::warn!(
                                            "save_embedding failed for {} {}: {}",
                                            kind,
                                            id,
                                            e
                                        );
                                    }
                                }
                            }
                            if failures > 0 {
                                tracing::warn!(
                                    "embedding batch: {} of {} items failed to save",
                                    failures,
                                    batch_len
                                );
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
    /// facts, and prune embeddings whose source rows were deleted, then catch
    /// up on vector indexing (facts + episodes, incl. compaction summaries).
    /// Intended for the app-level scheduler (and explicit admin paths) — not
    /// the ReAct hot path, which only runs [`Self::infer_session`].
    ///
    /// Returns the sum of rows touched by dedup / sensitive / flush / prune
    /// (cursor cleanup and embed catch-up are best-effort and not counted).
    pub async fn run_memory_maintenance(&self) -> u64 {
        let db = self.db.clone();
        let cleaned = db
            .run_blocking(move |db| {
                let mut total = 0u64;
                match db.dedup_facts() {
                    Ok(n) => total += n,
                    Err(e) => tracing::warn!("memory maintenance: dedup_facts failed: {}", e),
                }
                match db.delete_sensitive_facts() {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::error!("memory maintenance: delete_sensitive_facts failed: {}", e)
                    }
                }
                match db.flush_low_confidence(0.3) {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::warn!("memory maintenance: flush_low_confidence failed: {}", e)
                    }
                }
                match db.prune_orphaned_embeddings() {
                    Ok(n) => total += n,
                    Err(e) => tracing::warn!(
                        "memory maintenance: prune_orphaned_embeddings failed: {}",
                        e
                    ),
                }
                if let Err(e) = db.cleanup_orphan_extraction_cursors() {
                    tracing::warn!(
                        "memory maintenance: cleanup_orphan_extraction_cursors failed: {}",
                        e
                    );
                }
                Ok::<u64, anyhow::Error>(total)
            })
            .await
            .unwrap_or(0);
        // Catch up on vector indexing too, so memory that accumulated while
        // the embedding model was unconfigured gets indexed once it is set up.
        self.embed_new_memory().await;
        cleaned
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

        // Keyword fallback (CJK-aware terms for episodes; full query for FTS facts).
        let db = self.db.clone();
        let query_owned = query.to_string();
        db.run_blocking(move |db| {
            let hits: Vec<serde_json::Value> = if entity == entity_kind::EPISODE {
                let terms = haven_common::text::memory_recall_terms(&query_owned);
                let term_refs = haven_common::text::memory_recall_term_sample(&terms, 6);
                db.search_episodes_by_keywords(&term_refs, limit)
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

    /// Persist a batch of LLM-extracted facts. `user_messages` resolves each
    /// fact's `message_index` into a `FactSourceRef` for traceability.
    async fn persist_facts(
        &self,
        facts: &[LlmFact],
        user_messages: &[haven_memory::repositories::messages::Message],
    ) {
        let batch: Vec<FactDraft> = facts
            .iter()
            .map(|f| {
                let src_ref = f
                    .message_index
                    .and_then(|idx| user_messages.get(idx))
                    .map(|m| FactSourceRef::from_message(&m.id, &m.content));
                (
                    f.subject.clone(),
                    f.predicate.clone(),
                    f.object.clone(),
                    f.confidence,
                    f.tags.clone(),
                    src_ref,
                    f.durability.unwrap_or(0.6),
                )
            })
            .collect();
        self.persist_fact_batch(batch).await;
    }

    /// Shared persistence policy for a batch of extracted facts: sensitivity
    /// filter, degenerate rejection, confidence floor for brand-new facts,
    /// field sanitization / predicate normalization / tag whitelist.
    /// Maintenance (dedup, sensitive purge, low-confidence flush) is NOT
    /// inlined here — it runs on the app scheduler via
    /// `run_memory_maintenance`, so the ReAct hot path never pays for a
    /// full-table sweep after every extract.
    async fn persist_fact_batch(&self, facts: Vec<FactDraft>) {
        let db = self.db.clone();
        let sanitize_max = self.sanitize_max_chars;
        // Hard floor for NEW facts entering long-term memory. The extraction
        // prompt already asks for durable, generalizable facts; this rejects
        // whatever slips through with a borderline confidence so one-off
        // trivia does not linger for a year (the 365-day decay half-life
        // would otherwise keep a 0.5-confidence fact around for ~450 days).
        // Re-confirmations of an ALREADY-STORED triple must bypass the floor:
        // dropping them would skip the reinforcement (mention_count bump,
        // last_seen_at refresh, confidence boost) and let genuinely
        // re-confirmed facts keep decaying.
        const PERSIST_CONFIDENCE_FLOOR: f64 = 0.55;
        let _ = db
            .run_blocking(move |db| {
                // Phase 1: sanitize/validate every draft, collecting the
                // survivors' subjects so the existence check below runs as ONE
                // query for the whole batch instead of two per fact (each
                // query would re-checkout a pooled connection).
                let mut candidates: Vec<FactDraft> = Vec::new();
                for (subject_raw, predicate_raw, object_raw, confidence_raw, tags_raw, src_ref, durability_raw) in
                    facts
                {
                    let subject = sanitize_fact_field(&subject_raw, sanitize_max);
                    let predicate = normalize_predicate(&predicate_raw);
                    let object = sanitize_fact_field(&object_raw, sanitize_max);
                    if predicate.is_empty() || subject.is_empty() || object.is_empty() {
                        tracing::debug!(
                            "fact inference: dropping degenerate fact (empty subject/predicate/object)"
                        );
                        continue;
                    }
                    if is_sensitive_predicate(&predicate) || is_sensitive_object(&object) {
                        tracing::debug!(
                            "fact inference: dropping sensitive fact '{}'",
                            predicate
                        );
                        continue;
                    }
                    // Clamp to the documented range so an over-eager model
                    // (e.g. 1.2) does not skew decay/ordering.
                    candidates.push((
                        subject,
                        predicate,
                        object,
                        confidence_raw.clamp(0.5, 1.0),
                        tags_raw,
                        src_ref,
                        durability_raw.clamp(0.1, 1.0),
                    ));
                }
                let subjects: Vec<&str> = candidates
                    .iter()
                    .map(|(s, _, _, _, _, _, _)| s.as_str())
                    .collect();
                let (existing_triples, existing_pairs) = db
                    .facts_exist_batch(&subjects)
                    // Fail in the same direction as the per-fact queries they
                    // replace: on error, nothing exists -> the confidence
                    // floor applies.
                    .unwrap_or_default();
                for (subject, predicate, object, confidence, tags_raw, src_ref, durability) in candidates
                {
                    let is_new_fact =
                        !existing_triples.contains(&(subject.clone(), predicate.clone(), object.clone()));
                    // A single-valued predicate that already has a stored value
                    // (for a DIFFERENT object) is a user correction/update, not
                    // a brand-new fact: the floor must not drop it, or the
                    // latest value the user stated would never replace the
                    // stale one.
                    let is_single_valued_update = is_single_valued_predicate(&predicate)
                        && existing_pairs.contains(&(subject.clone(), predicate.clone()));
                    if is_new_fact
                        && !is_single_valued_update
                        && confidence < PERSIST_CONFIDENCE_FLOOR
                    {
                        tracing::debug!(
                            "fact inference: dropping low-confidence fact '{}' (confidence {})",
                            predicate,
                            confidence
                        );
                        continue;
                    }
                    let tags = sanitize_tags(&tags_raw);
                    let tags: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
                    if let Err(e) = db.upsert_fact_with_durability(
                        &subject,
                        &predicate,
                        &object,
                        "inferred",
                        confidence,
                        &tags,
                        src_ref.as_ref(),
                        durability,
                    ) {
                        tracing::warn!(
                            "fact inference: failed to persist fact '{} {} {}': {}",
                            subject,
                            predicate,
                            object,
                            e
                        );
                    }
                }
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
                "Known facts (already stored; re-confirming one is fine, output a new value if the user changed it):\n{}\n\nConversation (each message is numbered as [N]; set \"message_index\" to the number supporting each fact):\n{}",
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

        if response.text.trim().is_empty() {
            tracing::debug!("LLM fact extraction: empty model response, treating as no facts");
            return Ok(Vec::new());
        }

        let json_str = extract_json_array(&response.text);
        let facts: Vec<LlmFact> = serde_json::from_str(&json_str).map_err(|e| {
            let preview: String = response.text.chars().take(200).collect();
            anyhow::anyhow!("failed to parse LLM fact JSON: {} —raw: {}", e, preview)
        })?;

        tracing::info!("LLM fact extraction: {} facts extracted", facts.len());
        Ok(facts)
    }

    /// Compact list of the stored facts (effective-confidence order, all
    /// subjects) to hand the extraction model as context. Cross-subject facts
    /// (projects, tools, other entities) carry their subject prefix so the
    /// model can re-confirm or update them with the same subject instead of
    /// collapsing everything onto "user".
    async fn load_known_facts(&self) -> String {
        let db = self.db.clone();
        let facts = match db.run_blocking(move |db| db.list_facts()).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("load_known_facts: list_facts failed: {}", e);
                Vec::new()
            }
        };
        let mut lines: Vec<String> = Vec::new();
        for fact in facts.iter().take(self.max_known_facts) {
            let subject = if fact.subject == "user" {
                String::new()
            } else {
                format!(
                    "[{}] ",
                    sanitize_fact_field(&fact.subject, self.sanitize_max_chars)
                )
            };
            lines.push(format!(
                "- {}{}={} ({:.0}%)",
                subject,
                sanitize_fact_field(&fact.predicate, self.sanitize_max_chars),
                sanitize_fact_field(&fact.object, self.sanitize_max_chars),
                haven_memory::repositories::facts::fact_effective_confidence(fact) * 100.0
            ));
        }
        lines.join("\n")
    }

    /// Hot-path memory update for a session: extract new facts, then catch up
    /// a **bounded** embedding batch for newly written rows. Does **not** run
    /// full-table dedup / sensitive / flush — that stays on the scheduler via
    /// [`Self::run_memory_maintenance`].
    pub async fn infer_session(&self, session_id: &str) {
        self.infer_facts(session_id).await;
        self.embed_new_memory().await;
    }

    /// Pause-path variant: bypasses the extraction time throttle so a
    /// same-step interval infer cannot starve the fresher post-pause pass.
    pub async fn infer_session_on_pause(&self, session_id: &str) {
        self.infer_facts_on_pause(session_id).await;
        self.embed_new_memory().await;
    }
}

/// Build a transcript string from user messages, truncated to `max_chars`
/// to prevent unbounded token cost on long sessions. Recent messages take
/// priority (the last N messages that fit within the limit). Each line is
/// prefixed with its absolute index `[N]` in the input slice —truncation
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
/// Shared implementation lives in `haven_common::text` so the policy cannot
/// drift from prompt / tool index sanitization.
fn sanitize_fact_field(value: &str, max_chars: usize) -> String {
    haven_common::text::sanitize_prompt_field(value, max_chars)
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
    use haven_common::types::CanonicalMessage;
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmResponse, StreamChunk};
    use haven_memory::repositories::messages::Message;
    use std::pin::Pin;

    /// Mock whose chat answers with a fixed JSON fact array.
    struct FakeLlm {
        reply: String,
    }

    #[async_trait]
    impl LlmClient for FakeLlm {
        async fn chat(&self, _: Vec<CanonicalMessage>) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                text: self.reply.clone(),
                tool_calls: Vec::new(),
                finish_reason: Some(FinishReason::Stop),
                usage: haven_llm::types::Usage::default(),
                model: None,
                reasoning: None,
                web_search_calls: Vec::new(),
                thinking_blocks: Vec::new(),
            })
        }
        async fn chat_stream(
            &self,
            _: Vec<CanonicalMessage>,
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
            session_id: "t1".into(),
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
    fn test_llm_fact_coerces_non_string_fields() {
        let json = r#"[
            {"subject":"user","predicate":"has_pentest_mcp","object":true,"tags":["workspace"],"confidence":0.6,"message_index":0},
            {"subject":"user","predicate":"likes_count","object":3,"tags":["preference"],"confidence":0.8},
            {"subject":"user","predicate":"nickname","object":null,"tags":["identity"]},
            {"subject":"user","predicate":"name","object":"Alice","tags":[42],"confidence":0.9}
        ]"#;
        let facts: Vec<LlmFact> = serde_json::from_str(json).unwrap();
        assert_eq!(facts.len(), 4);
        assert_eq!(facts[0].object, "true");
        assert_eq!(facts[0].predicate, "has_pentest_mcp");
        assert_eq!(facts[1].object, "3");
        assert_eq!(facts[2].object, "");
        assert_eq!(facts[3].object, "Alice");
        assert_eq!(facts[3].tags, vec!["42"]);
    }

    #[test]
    fn test_llm_fact_durability_optional_with_default() {
        // Omitted durability → None (persistence maps to the 0.6 fallback).
        let no_dup: Vec<LlmFact> =
            serde_json::from_str(r#"[{"subject":"user","predicate":"name","object":"Alice"}]"#)
                .unwrap();
        assert!(no_dup[0].durability.is_none());
        // Explicit value round-trips; subject defaults to "user".
        let with_dup: Vec<LlmFact> = serde_json::from_str(
            r#"[{"subject":"haven","predicate":"project_path","object":"D:/w","durability":0.4}]"#,
        )
        .unwrap();
        assert_eq!(with_dup[0].durability, Some(0.4));
        assert_eq!(with_dup[0].subject, "haven");
        // Cross-subject facts deserialize without a subject field defaulting.
        let default_subj: Vec<LlmFact> =
            serde_json::from_str(r#"[{"predicate":"name","object":"A"}]"#).unwrap();
        assert_eq!(default_subj[0].subject, "user");
    }

    #[test]
    fn test_sanitize_tags_whitelists_and_lowercases() {
        assert_eq!(
            sanitize_tags(&["Workspace".into(), "Preference".into()]),
            vec!["workspace", "preference"]
        );
        // Out-of-set and empty tags are dropped.
        assert_eq!(
            sanitize_tags(&["hacker".into(), "".into()]),
            Vec::<String>::new()
        );
        // Mixed valid/invalid keeps only valid, capped to the allowed count.
        assert_eq!(
            sanitize_tags(&["identity".into(), "project".into(), "nonsense".into()]),
            vec!["identity", "project"]
        );
    }

    #[test]
    fn test_normalize_predicate_lowercases_and_trims() {
        assert_eq!(normalize_predicate("  Likes  "), "likes");
        assert_eq!(normalize_predicate("Works_at"), "works_at");
        assert_eq!(normalize_predicate("PROJECT_PATH"), "project_path");
    }

    #[test]
    fn test_normalize_predicate_maps_aliases() {
        // Alias mapping merges the same concept under different spellings so
        // single-valued constraints stay effective across sources.
        assert_eq!(normalize_predicate("Workspace"), "project_path");
        assert_eq!(normalize_predicate("workspace_path"), "project_path");
        assert_eq!(normalize_predicate("project_location"), "project_path");
        assert_eq!(normalize_predicate("employer"), "works_at");
        assert_eq!(normalize_predicate("favorite_language"), "language");
    }

    #[test]
    fn test_sanitize_tags_caps_count() {
        let many: Vec<String> = (0..10).map(|_| "identity".to_string()).collect();
        assert_eq!(sanitize_tags(&many).len(), 4);
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
            // 0 disables the time throttle; interval tests opt in explicitly.
            fact_extraction_min_interval_secs: 0,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        }
    }

    #[tokio::test]
    async fn infer_facts_advances_cursor_once() {
        let db = temp_db();
        let session = db.create_session("t1", "").unwrap();
        let _m1 = db
            .add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let m2 = db
            .add_message(&session.id, "user", "I use VSCode.", Some("text"), None)
            .unwrap();
        let engine = make_engine(db.clone());
        engine.infer_facts(&session.id).await;

        // Cursor should point at the last processed user message.
        let cursor: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(cursor.as_deref(), Some(m2.id.as_str()));

        // Re-running with no new messages must not change anything.
        engine.infer_facts(&session.id).await;
        let cursor2: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(cursor2, cursor);
    }

    #[tokio::test]
    async fn infer_facts_processes_only_new_messages() {
        let db = temp_db();
        let session = db.create_session("t1", "").unwrap();
        let m1 = db
            .add_message(&session.id, "user", "first message", Some("text"), None)
            .unwrap();
        let engine = make_engine(db.clone());
        engine.infer_facts(&session.id).await;
        let cursor: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(cursor.as_deref(), Some(m1.id.as_str()));

        // A new message moves the cursor forward.
        let m2 = db
            .add_message(&session.id, "user", "new signal only", Some("text"), None)
            .unwrap();
        engine.infer_facts(&session.id).await;
        let cursor2: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(cursor2.as_deref(), Some(m2.id.as_str()));
    }

    #[tokio::test]
    async fn infer_facts_throttled_within_interval_keeps_cursor() {
        // A second run inside the min interval must NOT call the model and
        // must NOT advance the cursor — the pending messages are processed by
        // the next allowed run (the maintenance pass catches up regardless).
        let db = temp_db();
        let session = db.create_session("t1", "").unwrap();
        let m1 = db
            .add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let engine = InferenceEngine {
            db: db.clone(),
            router: mock_router("[]"),
            max_transcript_chars: 4_000,
            embed_chunk_size: 64,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            fact_extraction_min_interval_secs: 3_600,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        };
        engine.infer_facts(&session.id).await;
        let cursor: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(cursor.as_deref(), Some(m1.id.as_str()));
        let last_run: Option<String> = db
            .get_kv(&format!("fact_extraction_last_run.{}", session.id))
            .unwrap();
        assert!(last_run.is_some(), "a model call must stamp last_run");

        // New message arrives within the interval: run is skipped entirely.
        let m2 = db
            .add_message(&session.id, "user", "I use VSCode.", Some("text"), None)
            .unwrap();
        engine.infer_facts(&session.id).await;
        let cursor2: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(
            cursor2.as_deref(),
            Some(m1.id.as_str()),
            "throttled run must not advance the cursor"
        );
        let last_run2: Option<String> = db
            .get_kv(&format!("fact_extraction_last_run.{}", session.id))
            .unwrap();
        assert_eq!(last_run2, last_run, "throttled run must not re-stamp");
        // The pending message is still unprocessed (not lost).
        let user_msgs: Vec<String> = db
            .get_session_messages(&session.id)
            .unwrap()
            .into_iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content)
            .collect();
        assert_eq!(user_msgs.len(), 2);
        let _ = m2;
    }

    #[tokio::test]
    async fn infer_facts_llm_failure_skips_and_advances_cursor() {
        // Balanced model reply is not valid JSON -> extraction fails. The
        // failure is non-fatal: nothing is persisted, and the cursor still
        // advances so the same messages are not re-analyzed next turn.
        let db = temp_db();
        let session = db.create_session("t1", "").unwrap();
        let m1 = db
            .add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let engine = InferenceEngine {
            db: db.clone(),
            router: mock_router("not a json array"),
            max_transcript_chars: 4_000,
            embed_chunk_size: 64,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            fact_extraction_min_interval_secs: 0,
            inference_semaphore: Arc::new(Semaphore::new(1)),
        };
        engine.infer_facts(&session.id).await;
        let facts = db.get_facts("user").unwrap();
        assert!(
            facts.is_empty(),
            "a failed extraction must not persist anything"
        );
        let cursor: Option<String> = db
            .get_kv(&format!("fact_extraction.{}", session.id))
            .unwrap();
        assert_eq!(cursor.as_deref(), Some(m1.id.as_str()));
    }
}
