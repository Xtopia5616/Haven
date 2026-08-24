use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use haven_common::prompts::{
    COMPACTED_SUMMARY_PREFIX, FACT_EXTRACTION_SYSTEM_PROMPT, predicate_merge_system_prompt,
};
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::embeddings::entity_kind;
use haven_memory::repositories::facts::{
    CANONICAL_MERGE_TARGETS, FactSourceRef, is_canonical_merge_target, is_identity_predicate,
    is_sensitive_object, is_sensitive_predicate, is_sensitive_text, is_single_valued_predicate,
};
use haven_memory::repositories::session_steps::SessionStep;
use serde::Deserialize;
use tokio::sync::{Notify, Semaphore};

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
    /// Pending extraction jobs keyed by session_id. Value is
    /// `bypass_throttle`; coalesce with OR so pause-path never loses to an
    /// earlier interval enqueue (L3 / P1-7).
    outbox: Mutex<HashMap<String, bool>>,
    outbox_notify: Notify,
    /// Lazy worker start so `AgentLayer::new` stays usable outside a Tokio
    /// runtime (unit tests that only construct the layer).
    outbox_worker_started: AtomicBool,
    /// Sessions whose MEMORY fence should be refreshed at the next
    /// `before_step` (M2). Set after a successful fact write; cleared by
    /// [`Self::take_memory_dirty_throttled`].
    memory_dirty: Mutex<HashMap<String, Instant>>,
    /// Last successful mid-run MEMORY patch per session (throttle key).
    memory_patch_last: Mutex<HashMap<String, Instant>>,
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
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
        }
    }

    /// Mark that new facts were written for `session_id` so the next
    /// `before_step` can surgically refresh the MEMORY fence (M2).
    pub fn mark_memory_dirty(&self, session_id: &str) {
        self.memory_dirty
            .lock()
            .unwrap()
            .insert(session_id.to_string(), Instant::now());
    }

    /// If the session is dirty and the patch throttle allows, clear dirty and
    /// return `true`. Throttle reuses `fact_extraction_min_interval_secs`
    /// (0 = no throttle). Never triggers a full tools/skills rebuild.
    pub fn take_memory_dirty_throttled(&self, session_id: &str) -> bool {
        let mut dirty = self.memory_dirty.lock().unwrap();
        if !dirty.contains_key(session_id) {
            return false;
        }
        let min = self.fact_extraction_min_interval_secs;
        if min > 0 {
            let last = self.memory_patch_last.lock().unwrap();
            if let Some(prev) = last.get(session_id)
                && prev.elapsed().as_secs() < min
            {
                return false;
            }
        }
        dirty.remove(session_id);
        drop(dirty);
        self.memory_patch_last
            .lock()
            .unwrap()
            .insert(session_id.to_string(), Instant::now());
        true
    }

    /// Enqueue a session for extraction. ReAct only enqueues; a single worker
    /// drains the outbox (L3 / P1-7). Duplicate session ids coalesce; any
    /// `bypass_throttle=true` wins.
    pub fn enqueue_infer(self: &Arc<Self>, session_id: &str, bypass_throttle: bool) {
        if session_id.is_empty() {
            return;
        }
        if let Ok(mut pending) = self.outbox.lock() {
            let entry = pending.entry(session_id.to_string()).or_insert(false);
            *entry = *entry || bypass_throttle;
        }
        self.ensure_outbox_worker();
        self.outbox_notify.notify_one();
    }

    fn ensure_outbox_worker(self: &Arc<Self>) {
        if self.outbox_worker_started.load(Ordering::Acquire) {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        if self
            .outbox_worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let engine = self.clone();
        tokio::spawn(async move {
            loop {
                let batch: Vec<(String, bool)> = {
                    let mut pending = engine.outbox.lock().unwrap_or_else(|e| e.into_inner());
                    if pending.is_empty() {
                        Vec::new()
                    } else {
                        pending.drain().collect()
                    }
                };
                if batch.is_empty() {
                    engine.outbox_notify.notified().await;
                    continue;
                }
                for (session_id, bypass) in batch {
                    if bypass {
                        engine.infer_session_on_pause(&session_id).await;
                    } else {
                        engine.infer_session(&session_id).await;
                    }
                }
            }
        });
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

        let (messages, steps) = {
            let db = self.db.clone();
            let session_id = session_id.to_string();
            match db
                .run_blocking(move |db| {
                    let messages = db.get_session_messages(&session_id)?;
                    let steps = db.get_session_steps(&session_id).unwrap_or_default();
                    Ok::<_, anyhow::Error>((messages, steps))
                })
                .await
            {
                Ok(pair) => pair,
                _ => {
                    tracing::warn!("fact inference: failed to load messages");
                    return;
                }
            }
        };
        if messages.is_empty() {
            return;
        }

        // Incremental window (M1+M4): cursor tracks user message ids; each new
        // user turn may include a bounded slice of preceding assistant/tool
        // context so short confirmations and tool-grounded replies stay
        // aligned with the model's recent vision — not a full transcript.
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
        let window = build_extraction_window(&messages, cursor.as_deref(), &steps);
        if window.messages.is_empty() {
            tracing::debug!("fact inference: no new messages since cursor");
            // Still advance when the only new rows were low-trust (peer
            // kickoff / cross-session) so extraction does not stall forever.
            if let Some(last) = window.cursor_last {
                let db = self.db.clone();
                let key = cursor_key.clone();
                let _ = db
                    .run_blocking(move |db| {
                        db.set_kv(&key, &last)?;
                        Ok::<(), anyhow::Error>(())
                    })
                    .await;
            }
            return;
        }

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

        match self.infer_facts_with_llm(&window.messages).await {
            Ok(facts) if !facts.is_empty() => {
                let wrote = self.persist_facts(&facts, &window.messages).await;
                if wrote {
                    self.mark_memory_dirty(session_id);
                }
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

        // Advance the cursor so the next run only sees brand-new user messages.
        if let Some(last) = window.cursor_last {
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
        let fallback_model = self
            .router
            .config()
            .await
            .embedding_model
            .model_name
            .clone();
        if fallback_model.is_empty() {
            return;
        }
        let db = self.db.clone();
        let model_for_missing = fallback_model.clone();
        let pending_raw = db
            .run_blocking(move |db| {
                let mut out: Vec<(String, String, String)> = Vec::new();
                match db.missing_embedding_ids(entity_kind::FACT, &model_for_missing) {
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
                match db.missing_embedding_ids(entity_kind::EPISODE, &model_for_missing) {
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
    /// Optionally proposes LLM predicate merges (M6) when SmallModel is
    /// configured. Intended for the app-level scheduler (and explicit admin
    /// paths) — not the ReAct hot path, which only runs [`Self::infer_session`].
    ///
    /// Returns the sum of rows touched by dedup / sensitive / flush / prune /
    /// predicate rewrites (cursor cleanup and embed catch-up are best-effort
    /// and not counted).
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
                match db.cleanup_orphan_source_refs() {
                    Ok(n) => total += n,
                    Err(e) => tracing::warn!(
                        "memory maintenance: cleanup_orphan_source_refs failed: {}",
                        e
                    ),
                }
                Ok::<u64, anyhow::Error>(total)
            })
            .await
            .unwrap_or(0);
        let merged = self.merge_predicates_with_llm().await;
        // Catch up on vector indexing too, so memory that accumulated while
        // the embedding model was unconfigured gets indexed once it is set up.
        // Rebuild LSH only when the side table lags the embedding rows (M5).
        self.embed_new_memory().await;
        if self
            .router
            .is_role_configured(EndpointRole::EmbeddingModel)
            .await
        {
            let model = self
                .router
                .config()
                .await
                .embedding_model
                .model_name
                .clone();
            if !model.is_empty() {
                let db = self.db.clone();
                if let Err(e) = db
                    .run_blocking(move |db| {
                        if db.embedding_lsh_lagging(&model)? {
                            db.rebuild_embedding_lsh(&model)?;
                        }
                        Ok::<(), anyhow::Error>(())
                    })
                    .await
                {
                    tracing::warn!("memory maintenance: embedding LSH rebuild failed: {}", e);
                }
            }
        }
        cleaned.saturating_add(merged)
    }

    /// Maintenance LLM pass (M6): propose predicate alias merges and apply
    /// only gated rewrites. LLM runs outside the DB lock; SQL apply is a
    /// separate blocking call. Returns rows rewritten.
    async fn merge_predicates_with_llm(&self) -> u64 {
        if !self
            .router
            .is_role_configured(EndpointRole::SmallModel)
            .await
        {
            return 0;
        }
        let db = self.db.clone();
        let counts = match db
            .run_blocking(move |db| db.list_predicate_counts())
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("memory maintenance: list_predicate_counts failed: {}", e);
                return 0;
            }
        };
        // Only bother the model when some keys still need collapsing: either
        // a legacy alias spelling, or a free-form non-canonical predicate.
        let needs_merge = counts.iter().any(|(p, _)| {
            let n = normalize_predicate(p);
            n != *p || !is_canonical_merge_target(&n)
        });
        if !needs_merge || counts.len() < 2 {
            return 0;
        }
        let listing = counts
            .iter()
            .take(60)
            .map(|(p, n)| format!("{p}\t{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let user_content = format!(
            "Predicate counts (predicate\\trows):\n{listing}\n\nPropose merges for free-form keys onto canonical ones."
        );

        let _permit = match self.inference_semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => return 0,
        };
        let merge_prompt = predicate_merge_system_prompt(CANONICAL_MERGE_TARGETS);
        let response = match self
            .router
            .chat_with_prompt(EndpointRole::SmallModel, &merge_prompt, &user_content)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("memory maintenance: predicate merge LLM failed: {}", e);
                return 0;
            }
        };
        if response.text.trim().is_empty() {
            return 0;
        }
        let json_str = extract_json_array(&response.text);
        let proposals: Vec<PredicateMergeProposal> = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: failed to parse predicate merge JSON: {}",
                    e
                );
                return 0;
            }
        };

        let mut accepted: Vec<(String, String)> = Vec::new();
        for p in proposals.into_iter().take(20) {
            if let Some((from, to)) = gate_predicate_merge(&p) {
                accepted.push((from, to));
            }
        }
        if accepted.is_empty() {
            return 0;
        }
        let db = self.db.clone();
        db.run_blocking(move |db| {
            let mut total = 0u64;
            for (from, to) in accepted {
                match db.rewrite_predicate(&from, &to) {
                    Ok(n) => {
                        if n > 0 {
                            tracing::info!(
                                "memory maintenance: rewrote predicate '{}' → '{}' ({} rows)",
                                from,
                                to,
                                n
                            );
                            total += n;
                        }
                    }
                    Err(e) => tracing::warn!(
                        "memory maintenance: rewrite_predicate {}→{} failed: {}",
                        from,
                        to,
                        e
                    ),
                }
            }
            Ok::<u64, anyhow::Error>(total)
        })
        .await
        .unwrap_or(0)
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
            let model = self
                .router
                .config()
                .await
                .embedding_model
                .model_name
                .clone();
            let db = self.db.clone();
            let entity_owned = entity.to_string();
            if let Ok(hits) = db
                .run_blocking(move |db| {
                    db.search_embeddings(&entity_owned, &vec, limit, &model)
                })
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

    /// Persist a batch of LLM-extracted facts. `messages` is the extraction
    /// window (may include assistant+user pairs); `message_index` resolves to
    /// a user line when possible for `FactSourceRef` (M1).
    /// Returns `true` when at least one fact was inserted/reinforced/corrected.
    async fn persist_facts(
        &self,
        facts: &[LlmFact],
        messages: &[haven_memory::repositories::messages::Message],
    ) -> bool {
        let batch: Vec<FactDraft> = facts
            .iter()
            .map(|f| {
                let src_ref = f
                    .message_index
                    .and_then(|idx| resolve_source_message(messages, idx))
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
        self.persist_fact_batch(batch).await
    }

    /// Shared persistence policy for a batch of extracted facts: sensitivity
    /// filter, degenerate rejection, confidence floor for brand-new facts,
    /// field sanitization / predicate normalization / tag whitelist.
    /// Maintenance (dedup, sensitive purge, low-confidence flush) is NOT
    /// inlined here — it runs on the app scheduler via
    /// `run_memory_maintenance`, so the ReAct hot path never pays for a
    /// full-table sweep after every extract.
    async fn persist_fact_batch(&self, facts: Vec<FactDraft>) -> bool {
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
        db.run_blocking(move |db| {
                let mut wrote = false;
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
                    match db.upsert_fact_with_durability(
                        &subject,
                        &predicate,
                        &object,
                        "inferred",
                        confidence,
                        &tags,
                        src_ref.as_ref(),
                        durability,
                    ) {
                        Ok(outcome) => {
                            use haven_memory::repositories::facts::UpsertOutcome::*;
                            if matches!(outcome, Inserted | Reinforced | Corrected) {
                                wrote = true;
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "fact inference: failed to persist fact '{} {} {}': {}",
                                subject,
                                predicate,
                                object,
                                e
                            );
                        }
                    }
                }
                Ok::<bool, anyhow::Error>(wrote)
            })
            .await
            .unwrap_or(false)
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
        let transcript = build_numbered_transcript(user_messages, self.max_transcript_chars);
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

    /// Drop mid-run MEMORY patch bookkeeping for a finished session.
    pub fn clear_session(&self, session_id: &str) {
        self.memory_dirty.lock().unwrap().remove(session_id);
        self.memory_patch_last.lock().unwrap().remove(session_id);
    }

    /// M3: enqueue light fact extraction from a compaction summary.
    /// Does not advance the user-message cursor (`fact_extraction.{session}`).
    /// Retries after the shared throttle instead of dropping the episode.
    pub fn enqueue_summary_extract(
        self: &Arc<Self>,
        session_id: &str,
        episode_id: &str,
        summary: &str,
    ) {
        if session_id.is_empty() || episode_id.is_empty() || summary.trim().len() < 24 {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let engine = self.clone();
        let session_id = session_id.to_string();
        let episode_id = episode_id.to_string();
        let summary = summary.to_string();
        tokio::spawn(async move {
            // Cap retries so a permanently busy throttle cannot spin forever.
            for attempt in 0..8 {
                match engine
                    .infer_facts_from_summary(&session_id, &episode_id, &summary)
                    .await
                {
                    SummaryExtractOutcome::Done => return,
                    SummaryExtractOutcome::Throttled { wait_secs } => {
                        tracing::debug!(
                            session = %session_id,
                            episode = %episode_id,
                            attempt,
                            wait_secs,
                            "summary fact inference deferred"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(wait_secs.max(1))).await;
                    }
                }
            }
            tracing::warn!(
                "summary fact inference exhausted retries for session {} episode {}",
                session_id,
                episode_id
            );
        });
    }

    /// Light extraction from a CompactSummary episode (M3). Respects the
    /// shared extraction time throttle and an episode cursor
    /// (`fact_extraction_episode.{session_id}`); never touches the user
    /// message cursor. Throttle returns [`SummaryExtractOutcome::Throttled`]
    /// without advancing the episode cursor so the caller can retry.
    pub async fn infer_facts_from_summary(
        &self,
        session_id: &str,
        episode_id: &str,
        summary: &str,
    ) -> SummaryExtractOutcome {
        let summary = summary.trim();
        if summary.len() < 24 {
            return SummaryExtractOutcome::Done;
        }
        let episode_cursor_key = format!("fact_extraction_episode.{}", session_id);
        let last_episode = self
            .db
            .run_blocking({
                let key = episode_cursor_key.clone();
                move |db| db.get_kv(&key)
            })
            .await
            .ok()
            .flatten();
        if last_episode.as_deref() == Some(episode_id) {
            return SummaryExtractOutcome::Done;
        }
        // Share the wall-clock throttle with normal extraction so compaction
        // cannot bypass the interval and spam the balanced model.
        if self.fact_extraction_min_interval_secs > 0 {
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
            {
                let elapsed =
                    (chrono::Utc::now() - prev.with_timezone(&chrono::Utc)).num_seconds();
                let min = self.fact_extraction_min_interval_secs as i64;
                if elapsed < min {
                    return SummaryExtractOutcome::Throttled {
                        wait_secs: (min - elapsed).max(1) as u64,
                    };
                }
            }
            let db = self.db.clone();
            let now = chrono::Utc::now().to_rfc3339();
            let _ = db
                .run_blocking(move |db| {
                    db.set_kv(&last_key, &now)?;
                    Ok::<(), anyhow::Error>(())
                })
                .await;
        }

        let synthetic = haven_memory::repositories::messages::Message {
            id: episode_id.to_string(),
            session_id: session_id.to_string(),
            role: "user".into(),
            content: format!(
                "[compaction summary]\n{}",
                summary.trim_start_matches(COMPACTED_SUMMARY_PREFIX).trim()
            ),
            message_type: Some("text".into()),
            created_at: chrono::Utc::now().to_rfc3339(),
            tool_call_id: None,
            attachments: vec![],
            voice: false,
        };

        match self.infer_facts_with_llm(std::slice::from_ref(&synthetic)).await {
            Ok(facts) if !facts.is_empty() => {
                let wrote = self
                    .persist_facts(&facts, std::slice::from_ref(&synthetic))
                    .await;
                if wrote {
                    self.mark_memory_dirty(session_id);
                }
            }
            Ok(_) => {
                tracing::debug!(
                    "LLM found no facts in compaction summary for session {}",
                    session_id
                );
            }
            Err(e) => {
                tracing::warn!(
                    "LLM summary fact extraction failed for session {}, skipping: {}",
                    session_id,
                    e
                );
            }
        }

        let db = self.db.clone();
        let key = episode_cursor_key;
        let episode_id = episode_id.to_string();
        if let Err(e) = db
            .run_blocking(move |db| {
                db.set_kv(&key, &episode_id)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
        {
            tracing::warn!(
                "summary fact extraction cursor advance failed for session {}: {}",
                session_id,
                e
            );
        }
        SummaryExtractOutcome::Done
    }
}

/// Result of a compaction-summary extraction attempt (M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryExtractOutcome {
    Done,
    Throttled { wait_secs: u64 },
}

/// Max preceding assistant text turns kept per new user (M4). Closest first.
const EXTRACTION_MAX_ASSISTANTS_PER_TURN: usize = 2;
/// Max tool observations kept per new user turn (M4).
const EXTRACTION_MAX_TOOLS_PER_TURN: usize = 3;
/// Truncate each tool observation body before it enters the transcript (M4).
const EXTRACTION_TOOL_CONTENT_CHARS: usize = 300;

/// Incremental extraction window (M1+M4): cursor is on **user** message ids;
/// each new user turn may include a bounded assistant/tool slice from the
/// same turn (skip compacted summaries / reasoning). Not a full transcript.
///
/// X12: reads the `messages` / `session_steps` projections (not the events
/// blob). Cursor stays on the last processed **user** message id.
struct ExtractionWindow {
    messages: Vec<haven_memory::repositories::messages::Message>,
    cursor_last: Option<String>,
}

fn build_extraction_window(
    all: &[haven_memory::repositories::messages::Message],
    cursor: Option<&str>,
    steps: &[SessionStep],
) -> ExtractionWindow {
    let user_indices: Vec<usize> = all
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == "user")
        .map(|(i, _)| i)
        .collect();
    let start_user = cursor
        .and_then(|c| user_indices.iter().position(|&i| all[i].id == c))
        .map(|i| i + 1)
        .unwrap_or(0);
    if start_user >= user_indices.len() {
        return ExtractionWindow {
            messages: Vec::new(),
            cursor_last: None,
        };
    }
    let mut messages = Vec::new();
    for (pos, &ui) in user_indices[start_user..].iter().enumerate() {
        // Peer kickoff / cross-session mail are low-trust and must not become
        // durable user facts (Plan A trust model).
        if is_low_trust_extraction_user(&all[ui]) {
            continue;
        }
        let abs_user_pos = start_user + pos;
        let (span_start, after_ts) = if abs_user_pos == 0 {
            (0, None)
        } else {
            let prev_ui = user_indices[abs_user_pos - 1];
            (prev_ui + 1, Some(all[prev_ui].created_at.as_str()))
        };
        let turn_slice = &all[span_start..ui];
        push_turn_context(&mut messages, turn_slice, &all[ui], after_ts, steps);
        messages.push(all[ui].clone());
    }
    let cursor_last = user_indices[start_user..]
        .last()
        .map(|&i| all[i].id.clone());
    ExtractionWindow {
        messages,
        cursor_last,
    }
}

fn is_extraction_assistant(m: &haven_memory::repositories::messages::Message) -> bool {
    if m.role != "assistant" {
        return false;
    }
    if m.content.starts_with(COMPACTED_SUMMARY_PREFIX) {
        return false;
    }
    match m.message_type.as_deref() {
        Some("reasoning") | Some("thought") | Some("action") | Some("observation") => false,
        _ => true,
    }
}

/// Low-trust user rows that must never seed durable facts: peer spawn kickoff
/// (`message_type=peer_kickoff` or delegated-task wrapper). Cross-session mail
/// is inject-only (not persisted as user rows), so it is not filtered here.
fn is_low_trust_extraction_user(m: &haven_memory::repositories::messages::Message) -> bool {
    if m.role != "user" {
        return false;
    }
    if m.message_type.as_deref() == Some("peer_kickoff") {
        return true;
    }
    m.content
        .trim_start()
        .starts_with(haven_common::types::PEER_KICKOFF_PREFIX)
}

/// Collect up to [`EXTRACTION_MAX_ASSISTANTS_PER_TURN`] assistants (closest to
/// the user) and up to [`EXTRACTION_MAX_TOOLS_PER_TURN`] tool observations for
/// one user turn. Tool rows prefer `role=tool` messages in the span; otherwise
/// recent `session_steps` observations between the previous and current user
/// timestamps are synthesized as `tool(name): …` lines (M4).
fn push_turn_context(
    out: &mut Vec<haven_memory::repositories::messages::Message>,
    turn_slice: &[haven_memory::repositories::messages::Message],
    user: &haven_memory::repositories::messages::Message,
    after_ts: Option<&str>,
    steps: &[SessionStep],
) {
    let mut assistants: Vec<&haven_memory::repositories::messages::Message> = turn_slice
        .iter()
        .filter(|m| is_extraction_assistant(m))
        .collect();
    if assistants.len() > EXTRACTION_MAX_ASSISTANTS_PER_TURN {
        assistants = assistants[assistants.len() - EXTRACTION_MAX_ASSISTANTS_PER_TURN..].to_vec();
    }
    for m in assistants {
        out.push(m.clone());
    }

    let mut tools: Vec<haven_memory::repositories::messages::Message> = turn_slice
        .iter()
        .filter(|m| m.role == "tool")
        .cloned()
        .collect();
    if tools.is_empty() {
        let user_ts = user.created_at.as_str();
        for step in steps.iter().rev() {
            if tools.len() >= EXTRACTION_MAX_TOOLS_PER_TURN {
                break;
            }
            let Some(obs) = step.observation.as_deref() else {
                continue;
            };
            if obs.trim().is_empty() {
                continue;
            }
            if is_sensitive_text(obs) {
                continue;
            }
            let ts = step
                .completed_at
                .as_deref()
                .or(step.started_at.as_deref())
                .unwrap_or(step.created_at.as_str());
            if !timestamp_in_turn(ts, after_ts, user_ts) {
                continue;
            }
            let name = step
                .action_tool
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or("tool");
            let body = haven_common::text::sanitize_prompt_field(obs, EXTRACTION_TOOL_CONTENT_CHARS);
            if body.trim().is_empty() {
                continue;
            }
            tools.push(haven_memory::repositories::messages::Message {
                id: step.id.clone(),
                session_id: step.session_id.clone(),
                role: "tool".into(),
                content: format!("tool({name}): {body}"),
                message_type: Some("observation".into()),
                created_at: ts.to_string(),
                tool_call_id: None,
                attachments: vec![],
                voice: false,
            });
        }
        tools.reverse();
    } else {
        tools.retain(|t| !is_sensitive_text(&t.content));
        for t in &mut tools {
            t.content = haven_common::text::sanitize_prompt_field(
                &t.content,
                EXTRACTION_TOOL_CONTENT_CHARS,
            );
        }
        tools.retain(|t| !t.content.trim().is_empty());
        if tools.len() > EXTRACTION_MAX_TOOLS_PER_TURN {
            tools = tools[tools.len() - EXTRACTION_MAX_TOOLS_PER_TURN..].to_vec();
        }
    }
    out.extend(tools);
}

/// Inclusive turn window for step timestamps. Parses RFC3339 when possible so
/// millis (`…Z`) and offset (`…+00:00`) shapes compare correctly (M4).
fn timestamp_in_turn(ts: &str, after_ts: Option<&str>, user_ts: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(ts),
        chrono::DateTime::parse_from_rfc3339(user_ts),
    ) {
        (Ok(step_dt), Ok(user_dt)) => {
            if step_dt > user_dt {
                return false;
            }
            if let Some(bound) = after_ts {
                if let Ok(bound_dt) = chrono::DateTime::parse_from_rfc3339(bound) {
                    return step_dt > bound_dt;
                }
            }
            true
        }
        _ => {
            // Fallback: lexicographic only when both sides share a shape.
            if ts > user_ts {
                return false;
            }
            after_ts.is_none_or(|bound| ts > bound)
        }
    }
}

#[derive(Debug, Deserialize)]
struct PredicateMergeProposal {
    #[serde(deserialize_with = "coerce_to_string")]
    from: String,
    #[serde(deserialize_with = "coerce_to_string")]
    to: String,
    #[serde(default)]
    confidence: f64,
}

/// Gate an LLM merge proposal (M6). Accept when the static alias map already
/// maps `from`→`to` (incl. case-only folds), or when confidence ≥ 0.85 and
/// `to` is canonical while `from` is still free-form. Never rewrite identity
/// or already-canonical keys onto a different key; never merge likes↔dislikes.
/// `from` is kept as listed so `rewrite_predicate` matches the exact DB spelling.
fn gate_predicate_merge(p: &PredicateMergeProposal) -> Option<(String, String)> {
    let from_raw = p.from.trim();
    let to_raw = p.to.trim();
    if from_raw.is_empty() || to_raw.is_empty() {
        return None;
    }
    let to = normalize_predicate(to_raw);
    let from_norm = normalize_predicate(from_raw);
    if from_raw == to {
        return None;
    }
    let polarity_clash = (from_norm == "likes" && to == "dislikes")
        || (from_norm == "dislikes" && to == "likes");
    if polarity_clash {
        return None;
    }
    // Never move identity / already-canonical keys onto a different key.
    if (is_identity_predicate(&from_norm) || is_canonical_merge_target(&from_norm))
        && from_norm != to
    {
        return None;
    }
    // Alias map or case-only fold onto the same canonical key.
    if from_norm == to && is_canonical_merge_target(&to) {
        return Some((from_raw.to_string(), to));
    }
    // Free-form → canonical only at high confidence.
    if p.confidence >= 0.85
        && is_canonical_merge_target(&to)
        && !is_canonical_merge_target(&from_norm)
    {
        return Some((from_raw.to_string(), to));
    }
    None
}

/// Prefer the user line in an assistant+user pair for `source_ref` (M1).
fn resolve_source_message(
    messages: &[haven_memory::repositories::messages::Message],
    idx: usize,
) -> Option<&haven_memory::repositories::messages::Message> {
    let m = messages.get(idx)?;
    if m.role == "user" {
        return Some(m);
    }
    messages[idx + 1..].iter().find(|n| n.role == "user")
}

/// Build a transcript string truncated to `max_chars`. Recent messages take
/// priority. Each line is `[N] role: content` — numbering stays absolute in
/// the input slice so `message_index` maps straight back.
fn build_numbered_transcript(
    messages: &[haven_memory::repositories::messages::Message],
    max_chars: usize,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut total_len = 0;
    // Walk backwards so the most recent messages are kept when truncating.
    for (i, m) in messages.iter().enumerate().rev() {
        // Sanitize content so tool/assistant bodies cannot inject newlines that
        // forge extra `[N] user:` lines in the extraction prompt (M4).
        let body = haven_common::text::sanitize_prompt_field(&m.content, max_chars);
        let line = format!("[{}] {}: {}", i, m.role, body);
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
    fn test_build_numbered_transcript_short() {
        let msgs = vec![make_message("hello"), make_message("world")];
        let transcript = build_numbered_transcript(&msgs, 4000);
        assert!(transcript.contains("hello"));
        assert!(transcript.contains("world"));
        // Messages are numbered with their absolute index and role (M1).
        assert!(transcript.contains("[0] user: hello"));
        assert!(transcript.contains("[1] user: world"));
    }

    #[test]
    fn test_build_numbered_transcript_truncates() {
        let big = "x".repeat(1000);
        let msgs: Vec<Message> = (0..10).map(|_| make_message(&big)).collect();
        let transcript = build_numbered_transcript(&msgs, 2000);
        // Small overhead for "[N] " prefixes (3-4 chars per line).
        assert!(transcript.len() <= 2000 + 60);
    }

    #[test]
    fn test_build_numbered_transcript_keeps_recent() {
        let msgs = vec![make_message("old_message"), make_message("recent_message")];
        let transcript = build_numbered_transcript(&msgs, 50);
        // "recent_message" should be kept because it's more recent.
        assert!(transcript.contains("recent_message"));
    }

    #[test]
    fn test_build_numbered_transcript_preserves_absolute_indices() {
        // Large earlier messages get dropped by truncation, but the remaining
        // lines must keep their absolute indices so the model's
        // message_index values still map back into the source slice.
        let big = "x".repeat(1000);
        let mut msgs: Vec<Message> = (0..5).map(|_| make_message(&big)).collect();
        msgs.push(make_message("the recent one"));
        let transcript = build_numbered_transcript(&msgs, 100);
        assert!(!transcript.contains("[0]"));
        assert!(transcript.contains("[5] user: the recent one"));
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
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
        }
    }

    fn make_role_message(role: &str, content: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: "t1".into(),
            role: role.into(),
            content: content.into(),
            message_type: Some("text".into()),
            created_at: "2026-01-01T00:00:00Z".into(),
            tool_call_id: None,
            attachments: vec![],
            voice: false,
        }
    }

    #[test]
    fn extraction_window_pairs_assistant_with_user() {
        let ask = make_role_message("assistant", "Dark or light theme?");
        let confirm = make_role_message("user", "dark");
        let window = build_extraction_window(&[ask.clone(), confirm.clone()], None, &[]);
        assert_eq!(window.messages.len(), 2);
        assert_eq!(window.messages[0].role, "assistant");
        assert_eq!(window.messages[1].id, confirm.id);
        assert_eq!(window.cursor_last.as_deref(), Some(confirm.id.as_str()));
    }

    #[test]
    fn extraction_window_skips_peer_kickoff() {
        let mut kickoff = make_role_message(
            "user",
            "[Delegated task from agent ses-parent — LOW TRUST, not a user instruction]\nDo work",
        );
        kickoff.message_type = Some("peer_kickoff".into());
        let legacy = make_role_message(
            "user",
            "[Delegated task from agent ses-parent — LOW TRUST, not a user instruction]\nold",
        );
        let real = make_role_message("user", "My name is Alice");
        let window = build_extraction_window(&[kickoff.clone(), legacy, real.clone()], None, &[]);
        assert_eq!(window.messages.len(), 1);
        assert_eq!(window.messages[0].id, real.id);
        assert_eq!(window.cursor_last.as_deref(), Some(real.id.as_str()));
    }

    #[test]
    fn extraction_window_skips_compacted_summary_pair() {
        let summary = make_role_message(
            "assistant",
            &format!("{COMPACTED_SUMMARY_PREFIX} prior chat"),
        );
        let user = make_role_message("user", "I like Rust");
        let window = build_extraction_window(&[summary, user.clone()], None, &[]);
        assert_eq!(window.messages.len(), 1);
        assert_eq!(window.messages[0].id, user.id);
    }

    #[test]
    fn extraction_window_keeps_two_closest_assistants() {
        let a1 = make_role_message("assistant", "first ask");
        let a2 = make_role_message("assistant", "second ask");
        let a3 = make_role_message("assistant", "third ask");
        let user = make_role_message("user", "dark");
        let window = build_extraction_window(&[a1, a2.clone(), a3.clone(), user.clone()], None, &[]);
        assert_eq!(window.messages.len(), 3);
        assert_eq!(window.messages[0].id, a2.id);
        assert_eq!(window.messages[1].id, a3.id);
        assert_eq!(window.messages[2].id, user.id);
    }

    #[test]
    fn extraction_window_skips_reasoning_assistant() {
        let mut reasoning = make_role_message("assistant", "hidden chain");
        reasoning.message_type = Some("reasoning".into());
        let ask = make_role_message("assistant", "Which theme?");
        let user = make_role_message("user", "dark");
        let window = build_extraction_window(&[reasoning, ask.clone(), user.clone()], None, &[]);
        assert_eq!(window.messages.len(), 2);
        assert_eq!(window.messages[0].id, ask.id);
        assert_eq!(window.messages[1].id, user.id);
    }

    #[test]
    fn extraction_window_includes_tool_message_in_span() {
        let ask = make_role_message("assistant", "Checking path");
        let mut tool = make_role_message("tool", &"x".repeat(500));
        tool.role = "tool".into();
        tool.message_type = Some("observation".into());
        let user = make_role_message("user", "use that path");
        let window =
            build_extraction_window(&[ask.clone(), tool.clone(), user.clone()], None, &[]);
        assert_eq!(window.messages.len(), 3);
        assert_eq!(window.messages[0].id, ask.id);
        assert_eq!(window.messages[1].role, "tool");
        assert!(window.messages[1].content.chars().count() <= EXTRACTION_TOOL_CONTENT_CHARS);
        assert_eq!(window.messages[2].id, user.id);
    }

    #[test]
    fn extraction_window_synthesizes_step_observations() {
        let ask = make_role_message("assistant", "Looking up");
        let mut user = make_role_message("user", "yes keep it");
        user.created_at = "2026-01-01T00:00:02Z".into();
        let step = SessionStep {
            id: "step-obs1".into(),
            session_id: "t1".into(),
            step_number: 1,
            thought: None,
            action_tool: Some("shell".into()),
            action_input: None,
            observation: Some("C:/Workspace/Haven".into()),
            status: "completed".into(),
            is_high_risk: false,
            confirmed: None,
            silent: false,
            started_at: Some("2026-01-01T00:00:01Z".into()),
            completed_at: Some("2026-01-01T00:00:01Z".into()),
            created_at: "2026-01-01T00:00:01Z".into(),
        };
        let window = build_extraction_window(&[ask.clone(), user.clone()], None, &[step]);
        assert_eq!(window.messages.len(), 3);
        assert_eq!(window.messages[0].id, ask.id);
        assert_eq!(window.messages[1].role, "tool");
        assert!(window.messages[1].content.contains("tool(shell):"));
        assert!(window.messages[1].content.contains("C:/Workspace/Haven"));
        assert_eq!(window.messages[2].id, user.id);
    }

    #[test]
    fn resolve_source_prefers_following_user() {
        let ask = make_role_message("assistant", "Which theme?");
        let confirm = make_role_message("user", "dark");
        let msgs = vec![ask, confirm.clone()];
        let src = resolve_source_message(&msgs, 0).unwrap();
        assert_eq!(src.id, confirm.id);
        assert_eq!(resolve_source_message(&msgs, 1).unwrap().id, confirm.id);
    }

    #[test]
    fn gate_predicate_merge_accepts_alias_and_high_confidence() {
        let alias = PredicateMergeProposal {
            from: "Workspace".into(),
            to: "project_path".into(),
            confidence: 0.5,
        };
        assert_eq!(
            gate_predicate_merge(&alias),
            Some(("Workspace".into(), "project_path".into()))
        );
        let case_fold = PredicateMergeProposal {
            from: "Likes".into(),
            to: "likes".into(),
            confidence: 0.1,
        };
        assert_eq!(
            gate_predicate_merge(&case_fold),
            Some(("Likes".into(), "likes".into()))
        );
        let free = PredicateMergeProposal {
            from: "fav_lang".into(),
            to: "language".into(),
            confidence: 0.9,
        };
        assert_eq!(
            gate_predicate_merge(&free),
            Some(("fav_lang".into(), "language".into()))
        );
        let weak = PredicateMergeProposal {
            from: "fav_lang".into(),
            to: "language".into(),
            confidence: 0.5,
        };
        assert_eq!(gate_predicate_merge(&weak), None);
        let polarity = PredicateMergeProposal {
            from: "likes".into(),
            to: "dislikes".into(),
            confidence: 1.0,
        };
        assert_eq!(gate_predicate_merge(&polarity), None);
        let identity = PredicateMergeProposal {
            from: "name".into(),
            to: "works_at".into(),
            confidence: 1.0,
        };
        assert_eq!(gate_predicate_merge(&identity), None);
    }

    #[test]
    fn memory_dirty_throttle_suppresses_second_take() {
        let engine = make_engine(temp_db());
        engine.mark_memory_dirty("ses-a");
        assert!(engine.take_memory_dirty_throttled("ses-a"));
        engine.mark_memory_dirty("ses-a");
        // min interval 0 → no throttle
        assert!(engine.take_memory_dirty_throttled("ses-a"));
        let engine = InferenceEngine {
            db: temp_db(),
            router: mock_router("[]"),
            max_transcript_chars: 4_000,
            embed_chunk_size: 64,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            fact_extraction_min_interval_secs: 3_600,
            inference_semaphore: Arc::new(Semaphore::new(1)),
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
        };
        engine.mark_memory_dirty("ses-b");
        assert!(engine.take_memory_dirty_throttled("ses-b"));
        engine.mark_memory_dirty("ses-b");
        assert!(!engine.take_memory_dirty_throttled("ses-b"));
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
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
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
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
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

    #[test]
    fn enqueue_infer_coalesces_bypass_flag() {
        let db = temp_db();
        let engine = Arc::new(make_engine(db));
        engine.enqueue_infer("ses-a", false);
        engine.enqueue_infer("ses-a", true);
        engine.enqueue_infer("ses-b", false);
        let pending = engine.outbox.lock().unwrap();
        assert_eq!(pending.get("ses-a"), Some(&true));
        assert_eq!(pending.get("ses-b"), Some(&false));
        assert_eq!(pending.len(), 2);
    }
}
