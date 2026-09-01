use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Context as _;
use haven_common::prompts::{
    COMPACTED_SUMMARY_PREFIX, CONTRADICTION_ARBITRATE_SYSTEM_PROMPT, FACT_EXTRACTION_SYSTEM_PROMPT,
    predicate_merge_system_prompt,
};
use haven_llm::{EndpointRole, LlmRouter};
use haven_memory::Database;
use haven_memory::recall::{MemoryKind, MemoryQuery, MemoryRecall, MemoryRetriever};
use haven_memory::repositories::facts::{
    CANONICAL_MERGE_TARGETS, Fact, FactSourceRef, is_canonical_merge_target, is_sensitive_object,
    is_sensitive_predicate, is_single_valued_predicate,
};
use tokio::sync::{Notify, Semaphore};

use crate::fact_extraction::{
    FactDraft, LlmFact, extract_json_array, normalize_predicate, sanitize_fact_field, sanitize_tags,
};
use crate::fact_inference::{
    ContradictionDemoteProposal, PredicateMergeProposal, build_extraction_window,
    build_numbered_transcript, format_contradiction_groups, gate_contradiction_demote,
    gate_predicate_merge, resolve_source_message,
};
use crate::memory_index::MemoryEmbeddingIndex;

pub struct InferenceEngine {
    db: Arc<Database>,
    router: Arc<LlmRouter>,
    /// Cap (chars) for transcripts sent to the BalancedModel for fact
    /// extraction. Prevents unbounded token cost on long conversations.
    max_transcript_chars: usize,
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
    /// Provider-facing embedding/index lifecycle, kept outside fact
    /// extraction and maintenance policy.
    embedding_index: MemoryEmbeddingIndex,
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
            db: db.clone(),
            router: router.clone(),
            max_transcript_chars,
            max_known_facts,
            sanitize_max_chars,
            fact_extraction_min_interval_secs,
            inference_semaphore: Arc::new(Semaphore::new(1)),
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
            embedding_index: MemoryEmbeddingIndex::new(
                db.clone(),
                router.clone(),
                embed_chunk_size,
            ),
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
    /// persisted, and the cursor stays put so a later run can retry the same
    /// messages. An empty `Ok([])` from
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
            let last_run = match self
                .db
                .run_blocking({
                    let key = last_key.clone();
                    move |db| db.get_kv(&key)
                })
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        "fact inference throttle read failed for session {}: {}",
                        session_id,
                        error
                    );
                    return;
                }
            };
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
            let session_id_for_db = session_id.to_string();
            match db
                .run_blocking(move |db| {
                    let messages = db.get_session_messages(&session_id_for_db)?;
                    let steps = db.get_session_steps(&session_id_for_db)?;
                    Ok::<_, anyhow::Error>((messages, steps))
                })
                .await
            {
                Ok(pair) => pair,
                Err(error) => {
                    tracing::warn!(
                        "fact inference: failed to load transcript for session {}: {}",
                        session_id,
                        error
                    );
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
        let cursor = match self
            .db
            .run_blocking({
                let key = cursor_key.clone();
                move |db| db.get_kv(&key)
            })
            .await
        {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    "fact inference cursor read failed for session {}: {}",
                    session_id,
                    error
                );
                return;
            }
        };
        let window = build_extraction_window(&messages, cursor.as_deref(), &steps);
        if window.messages.is_empty() {
            tracing::debug!("fact inference: no new messages since cursor");
            // Still advance when the only new rows were low-trust (peer
            // kickoff / cross-session) so extraction does not stall forever.
            if let Some(last) = window.cursor_last {
                let db = self.db.clone();
                let key = cursor_key.clone();
                if let Err(error) = db
                    .run_blocking(move |db| {
                        db.set_kv(&key, &last)?;
                        Ok::<(), anyhow::Error>(())
                    })
                    .await
                {
                    tracing::warn!(
                        "fact inference cursor advance failed for session {}: {}",
                        session_id,
                        error
                    );
                }
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
            if let Err(error) = db
                .run_blocking(move |db| {
                    db.set_kv(&key, &now)?;
                    Ok::<(), anyhow::Error>(())
                })
                .await
            {
                tracing::warn!(
                    "fact inference throttle stamp failed for session {}; skipping LLM call: {}",
                    session_id,
                    error
                );
                return;
            }
        }

        let extraction_succeeded = match self.infer_facts_with_llm(&window.messages).await {
            Ok(facts) if !facts.is_empty() => {
                match self.persist_facts(&facts, &window.messages).await {
                    Ok(wrote) => {
                        if wrote {
                            self.mark_memory_dirty(session_id);
                        }
                        true
                    }
                    Err(error) => {
                        tracing::warn!(
                            "fact persistence failed for session {}, keeping extraction cursor unchanged: {}",
                            session_id,
                            error
                        );
                        false
                    }
                }
            }
            Ok(_) => {
                tracing::debug!("LLM found no facts in session {}", session_id);
                true
            }
            Err(e) => {
                tracing::warn!(
                    "LLM fact extraction failed for session {}, keeping extraction cursor unchanged: {}",
                    session_id,
                    e
                );
                false
            }
        };

        if !extraction_succeeded {
            return;
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

    /// Full memory maintenance pass, independent of any extraction: collapse
    /// duplicate facts, purge sensitive facts, flush stale low-confidence
    /// facts, and prune embeddings whose source rows were deleted, then catch
    /// up on vector indexing (facts + episodes, incl. compaction summaries).
    /// Runs the rule-based contradiction engine (X5), then optionally proposes
    /// LLM predicate merges (M6) and residual contradiction arbitration when
    /// SmallModel is configured. Intended for the app-level scheduler (and
    /// explicit admin paths) — not the ReAct hot path, which only runs
    /// [`Self::infer_session`].
    ///
    /// Returns the sum of rows touched by dedup / sensitive / flush / prune /
    /// contradiction demotes / predicate rewrites (cursor cleanup and embed
    /// catch-up are best-effort and not counted).
    pub async fn run_memory_maintenance(&self) -> anyhow::Result<u64> {
        let db = self.db.clone();
        let cleaned = db
            .run_blocking(move |db| {
                let mut total = 0u64;
                let mut failures = Vec::new();
                match db.dedup_facts() {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::warn!("memory maintenance: dedup_facts failed: {}", e);
                        failures.push(format!("dedup_facts: {e}"));
                    }
                }
                match db.delete_sensitive_facts() {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::error!("memory maintenance: delete_sensitive_facts failed: {}", e);
                        failures.push(format!("delete_sensitive_facts: {e}"));
                    }
                }
                // X5: demote recent polarity / single-valued losers that
                // slipped past upsert (age-capped), before low-confidence
                // flush can delete them in the same pass.
                match db.resolve_contradictions() {
                    Ok(n) => {
                        if n > 0 {
                            tracing::info!(
                                "memory maintenance: resolved {} contradictory fact(s)",
                                n
                            );
                        }
                        total += n;
                    }
                    Err(e) => {
                        tracing::warn!("memory maintenance: resolve_contradictions failed: {}", e);
                        failures.push(format!("resolve_contradictions: {e}"));
                    }
                }
                match db.flush_low_confidence(0.3) {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::warn!("memory maintenance: flush_low_confidence failed: {}", e);
                        failures.push(format!("flush_low_confidence: {e}"));
                    }
                }
                match db.prune_orphaned_embeddings() {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::warn!(
                            "memory maintenance: prune_orphaned_embeddings failed: {}",
                            e
                        );
                        failures.push(format!("prune_orphaned_embeddings: {e}"));
                    }
                }
                if let Err(e) = db.cleanup_orphan_extraction_cursors() {
                    tracing::warn!(
                        "memory maintenance: cleanup_orphan_extraction_cursors failed: {}",
                        e
                    );
                    failures.push(format!("cleanup_orphan_extraction_cursors: {e}"));
                }
                // provenance_item_id is FK ON DELETE SET NULL; opaque
                // provenance_record_id values are intentional transcript refs.
                // Still normalize empty record ids.
                match db.cleanup_orphan_source_refs() {
                    Ok(n) => total += n,
                    Err(e) => {
                        tracing::warn!(
                            "memory maintenance: cleanup_orphan_source_refs failed: {}",
                            e
                        );
                        failures.push(format!("cleanup_orphan_source_refs: {e}"));
                    }
                }
                if failures.is_empty() {
                    Ok(total)
                } else {
                    Err(anyhow::anyhow!(
                        "memory maintenance failed: {}",
                        failures.join("; ")
                    ))
                }
            })
            .await?;
        let merged = self.merge_predicates_with_llm().await;
        // Alias merges can create new single-valued multi-object conflicts;
        // re-run the rule keeper before LLM arbitration so merge-created
        // pairs get the same user>inferred / confidence treatment.
        let resolved_after_merge = if merged > 0 {
            let db = self.db.clone();
            db.run_blocking(move |db| db.resolve_contradictions())
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        "memory maintenance: post-merge resolve_contradictions failed: {}",
                        e
                    );
                    0
                })
        } else {
            0
        };
        let arbitrated = self.arbitrate_contradictions_with_llm().await;
        // Catch up on vector indexing too, so memory that accumulated while
        // the embedding model was unconfigured gets indexed once it is set up.
        // Rebuild LSH only when the side table lags the embedding rows (M5).
        self.embedding_index.embed_new_memory().await;
        self.embedding_index.rebuild_lsh_if_lagging().await;
        Ok(cleaned
            .saturating_add(merged)
            .saturating_add(resolved_after_merge)
            .saturating_add(arbitrated))
    }

    /// Maintenance LLM pass (X5): residual contradiction groups after the
    /// rule engine, with `source_ref` snippets as evidence. LLM runs outside
    /// the DB lock; demotes are gated then applied in a separate blocking
    /// call. Returns rows demoted.
    async fn arbitrate_contradictions_with_llm(&self) -> u64 {
        if !self
            .router
            .is_role_configured(EndpointRole::SmallModel)
            .await
        {
            return 0;
        }
        let db = self.db.clone();
        let groups = match db
            .run_blocking(move |db| db.list_ambiguous_contradictions())
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: list_ambiguous_contradictions failed: {}",
                    e
                );
                return 0;
            }
        };
        let groups: Vec<_> = groups
            .into_iter()
            .filter_map(|mut group| {
                group.facts.retain(MemoryRetriever::visible_fact);
                (!group.facts.is_empty()).then_some(group)
            })
            .collect();
        if groups.is_empty() {
            return 0;
        }
        let listing = format_contradiction_groups(&groups);
        let user_content = format!(
            "Conflict groups (JSON):\n{listing}\n\nPropose demotions for residual contradictions."
        );

        let _permit = match self.inference_semaphore.acquire().await {
            Ok(p) => p,
            Err(_) => return 0,
        };
        let response = match self
            .router
            .chat_with_prompt(
                EndpointRole::SmallModel,
                CONTRADICTION_ARBITRATE_SYSTEM_PROMPT,
                &user_content,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: contradiction arbitrate LLM failed: {}",
                    e
                );
                return 0;
            }
        };
        if response.text.trim().is_empty() {
            return 0;
        }
        let json_str = extract_json_array(&response.text);
        let proposals: Vec<ContradictionDemoteProposal> = match serde_json::from_str(&json_str) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "memory maintenance: failed to parse contradiction demote JSON: {}",
                    e
                );
                return 0;
            }
        };

        let allowed: HashMap<String, &Fact> = groups
            .iter()
            .flat_map(|g| g.facts.iter().map(|f| (f.id.clone(), f)))
            .collect();
        let mut demote_ids: Vec<String> = Vec::new();
        for p in proposals.into_iter().take(20) {
            if let Some(id) = gate_contradiction_demote(&p, &allowed, &groups)
                && !demote_ids.iter().any(|x| x == &id)
            {
                demote_ids.push(id);
            }
        }
        if demote_ids.is_empty() {
            return 0;
        }
        let db = self.db.clone();
        db.run_blocking(move |db| {
            let n = db.demote_fact_ids(demote_ids)?;
            if n > 0 {
                tracing::info!(
                    "memory maintenance: LLM demoted {} contradictory fact(s)",
                    n
                );
            }
            Ok::<u64, anyhow::Error>(n)
        })
        .await
        .unwrap_or(0)
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
        let counts = match db.run_blocking(move |db| db.list_predicate_counts()).await {
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
    /// `embedding_model` slot when configured (embed the query, then fuse
    /// cosine candidates with keyword candidates); otherwise falls back to
    /// keyword search. Retrieval and sensitive filtering are owned by
    /// `haven_memory`; this method only acquires the optional provider vector
    /// and moves the blocking read off the async runtime.
    pub async fn recall_memory(
        &self,
        query: &str,
        kind: &str,
        limit: usize,
    ) -> anyhow::Result<MemoryRecall> {
        let kind = MemoryKind::parse(kind)?;
        let query = MemoryQuery::new(query, kind, limit)?;
        self.recall_memory_query(query).await
    }

    /// Execute a fully-scoped typed recall request. Callers that already own
    /// a `MemoryQuery` must use this entry point so session and subject scope
    /// cannot be silently discarded at an adapter boundary.
    pub async fn recall_memory_query(&self, query: MemoryQuery) -> anyhow::Result<MemoryRecall> {
        let vector_hits = self.embedding_index.search(&query).await?;
        let db = self.db.clone();
        db.run_blocking(move |db| MemoryRetriever::new(db).retrieve(&query, vector_hits))
            .await
    }

    /// Persist a batch of LLM-extracted facts. `messages` is the extraction
    /// window (may include assistant+user pairs); `message_index` resolves to
    /// a user line when possible for `FactSourceRef` (M1).
    /// Returns whether persistence completed and whether it changed memory.
    async fn persist_facts(
        &self,
        facts: &[LlmFact],
        messages: &[haven_memory::repositories::messages::Message],
    ) -> anyhow::Result<bool> {
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
    async fn persist_fact_batch(&self, facts: Vec<FactDraft>) -> anyhow::Result<bool> {
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
            for (
                subject_raw,
                predicate_raw,
                object_raw,
                confidence_raw,
                tags_raw,
                src_ref,
                durability_raw,
            ) in facts
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
                    tracing::debug!("fact inference: dropping sensitive fact '{}'", predicate);
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
            let (existing_triples, existing_pairs) = db.facts_exist_batch(&subjects)?;
            for (subject, predicate, object, confidence, tags_raw, src_ref, durability) in
                candidates
            {
                let is_new_fact = !existing_triples.contains(&(
                    subject.clone(),
                    predicate.clone(),
                    object.clone(),
                ));
                // A single-valued predicate that already has a stored value
                // (for a DIFFERENT object) is a user correction/update, not
                // a brand-new fact: the floor must not drop it, or the
                // latest value the user stated would never replace the
                // stale one.
                let is_single_valued_update = is_single_valued_predicate(&predicate)
                    && existing_pairs.contains(&(subject.clone(), predicate.clone()));
                if is_new_fact && !is_single_valued_update && confidence < PERSIST_CONFIDENCE_FLOOR
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
                        return Err(e).context(format!(
                            "failed to persist fact '{} {} {}'",
                            subject, predicate, object
                        ));
                    }
                }
            }
            Ok::<bool, anyhow::Error>(wrote)
        })
        .await
        .map_err(|error| anyhow::anyhow!("fact batch persistence failed: {error}"))
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
        for fact in facts
            .iter()
            .filter(|fact| MemoryRetriever::visible_fact(fact))
            .take(self.max_known_facts)
        {
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
        self.embedding_index.embed_new_memory().await;
    }

    /// Pause-path variant: bypasses the extraction time throttle so a
    /// same-step interval infer cannot starve the fresher post-pause pass.
    pub async fn infer_session_on_pause(&self, session_id: &str) {
        self.infer_facts_on_pause(session_id).await;
        self.embedding_index.embed_new_memory().await;
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
                    SummaryExtractOutcome::Throttled { wait_secs }
                    | SummaryExtractOutcome::Retryable { wait_secs } => {
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
    /// message cursor. Throttle and transient failures return without advancing
    /// the episode cursor so the caller can retry.
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
        if !MemoryRetriever::visible_text(summary) {
            tracing::debug!(
                session_id,
                episode_id,
                "skipping sensitive compaction summary before LLM extraction"
            );
            return SummaryExtractOutcome::Done;
        }
        let episode_cursor_key = format!("fact_extraction_episode.{}", session_id);
        let last_episode = match self
            .db
            .run_blocking({
                let key = episode_cursor_key.clone();
                move |db| db.get_kv(&key)
            })
            .await
        {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    "summary fact extraction cursor read failed for session {}: {}",
                    session_id,
                    error
                );
                return SummaryExtractOutcome::Retryable { wait_secs: 1 };
            }
        };
        if last_episode.as_deref() == Some(episode_id) {
            return SummaryExtractOutcome::Done;
        }
        // Share the wall-clock throttle with normal extraction so compaction
        // cannot bypass the interval and spam the balanced model.
        if self.fact_extraction_min_interval_secs > 0 {
            let last_key = format!("fact_extraction_last_run.{}", session_id);
            let last_run = match self
                .db
                .run_blocking({
                    let key = last_key.clone();
                    move |db| db.get_kv(&key)
                })
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        "summary fact extraction throttle read failed for session {}: {}",
                        session_id,
                        error
                    );
                    return SummaryExtractOutcome::Retryable { wait_secs: 1 };
                }
            };
            if let Some(ts) = last_run
                && let Ok(prev) = chrono::DateTime::parse_from_rfc3339(&ts)
            {
                let elapsed = (chrono::Utc::now() - prev.with_timezone(&chrono::Utc)).num_seconds();
                let min = self.fact_extraction_min_interval_secs as i64;
                if elapsed < min {
                    return SummaryExtractOutcome::Throttled {
                        wait_secs: (min - elapsed).max(1) as u64,
                    };
                }
            }
            let db = self.db.clone();
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(error) = db
                .run_blocking(move |db| {
                    db.set_kv(&last_key, &now)?;
                    Ok::<(), anyhow::Error>(())
                })
                .await
            {
                tracing::warn!(
                    "summary fact extraction throttle stamp failed for session {}: {}",
                    session_id,
                    error
                );
                return SummaryExtractOutcome::Retryable { wait_secs: 1 };
            }
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

        match self
            .infer_facts_with_llm(std::slice::from_ref(&synthetic))
            .await
        {
            Ok(facts) if !facts.is_empty() => {
                let wrote = match self
                    .persist_facts(&facts, std::slice::from_ref(&synthetic))
                    .await
                {
                    Ok(wrote) => wrote,
                    Err(error) => {
                        tracing::warn!(
                            "summary fact persistence failed for session {}, keeping episode cursor unchanged: {}",
                            session_id,
                            error
                        );
                        return SummaryExtractOutcome::Retryable { wait_secs: 1 };
                    }
                };
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
                    "LLM summary fact extraction failed for session {}, keeping episode cursor unchanged: {}",
                    session_id,
                    e
                );
                return SummaryExtractOutcome::Retryable { wait_secs: 1 };
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
            return SummaryExtractOutcome::Retryable { wait_secs: 1 };
        }
        SummaryExtractOutcome::Done
    }
}

/// Result of a compaction-summary extraction attempt (M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryExtractOutcome {
    Done,
    Throttled { wait_secs: u64 },
    Retryable { wait_secs: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fact_inference::{
        EXTRACTION_TOOL_CONTENT_CHARS, build_extraction_window, build_numbered_transcript,
    };
    use crate::memory_index::embedding_batch_size;
    use async_trait::async_trait;
    use haven_common::types::CanonicalMessage;
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmResponse, StreamChunk};
    use haven_memory::repositories::facts::{ContradictionCandidate, ContradictionKind};
    use haven_memory::repositories::messages::Message;
    use haven_memory::repositories::session_steps::SessionStep;
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

    #[test]
    fn embedding_batch_size_caps_provider_limit_and_rejects_zero() {
        assert_eq!(embedding_batch_size(64), 10);
        assert_eq!(embedding_batch_size(5), 5);
        assert_eq!(embedding_batch_size(0), 1);
    }

    #[tokio::test]
    async fn known_fact_context_excludes_legacy_sensitive_rows() {
        let db = temp_db();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "hunter2", "inferred", 1.0, &[])
            .unwrap();
        let engine = make_engine(db);

        let known = engine.load_known_facts().await;

        assert!(known.contains("likes=Rust"));
        assert!(!known.contains("hunter2"));
    }

    fn make_engine(db: Arc<Database>) -> InferenceEngine {
        let router = mock_router("[]");
        InferenceEngine {
            db: db.clone(),
            router: router.clone(),
            max_transcript_chars: 4_000,
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
            embedding_index: MemoryEmbeddingIndex::new(db.clone(), router, 64),
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
        let window =
            build_extraction_window(&[a1, a2.clone(), a3.clone(), user.clone()], None, &[]);
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
        let window = build_extraction_window(&[ask.clone(), tool.clone(), user.clone()], None, &[]);
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
            action_index: 0,
            thought: None,
            action_tool: Some("shell".into()),
            action_input: None,
            tool_call_id: None,
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

    fn fresh_fact(id: &str, predicate: &str, object: &str, source: &str, confidence: f64) -> Fact {
        let now = chrono::Utc::now().to_rfc3339();
        Fact {
            id: id.into(),
            subject: "user".into(),
            predicate: predicate.into(),
            object: object.into(),
            source: source.into(),
            confidence,
            tags: vec![],
            created_at: now.clone(),
            mention_count: 0,
            last_seen_at: Some(now),
            source_ref: None,
            durability: 1.0,
        }
    }

    #[test]
    fn gate_contradiction_demote_accepts_high_confidence_member() {
        let keep = fresh_fact("fact-aaaa", "works_at", "Acme", "inferred", 0.9);
        let drop = fresh_fact("fact-bbbb", "works_at", "Beta", "inferred", 0.85);
        let groups = vec![ContradictionCandidate {
            kind: ContradictionKind::SingleValued,
            facts: vec![keep.clone(), drop.clone()],
        }];
        let allowed: HashMap<String, &Fact> = groups
            .iter()
            .flat_map(|g| g.facts.iter().map(|f| (f.id.clone(), f)))
            .collect();
        let ok = ContradictionDemoteProposal {
            demote_id: drop.id.clone(),
            confidence: 0.9,
        };
        assert_eq!(
            gate_contradiction_demote(&ok, &allowed, &groups),
            Some(drop.id.clone())
        );
        // Keeper must never be demoted (always leave ≥1 survivor).
        let kill_keeper = ContradictionDemoteProposal {
            demote_id: keep.id.clone(),
            confidence: 1.0,
        };
        assert_eq!(
            gate_contradiction_demote(&kill_keeper, &allowed, &groups),
            None
        );
        let weak = ContradictionDemoteProposal {
            demote_id: drop.id.clone(),
            confidence: 0.5,
        };
        assert_eq!(gate_contradiction_demote(&weak, &allowed, &groups), None);
        let unknown = ContradictionDemoteProposal {
            demote_id: "fact-zzzz".into(),
            confidence: 1.0,
        };
        assert_eq!(gate_contradiction_demote(&unknown, &allowed, &groups), None);
    }

    #[test]
    fn gate_contradiction_demote_protects_user_over_inferred() {
        let user_job = fresh_fact("fact-user", "works_at", "Acme", "user", 1.0);
        let inferred = fresh_fact("fact-inf", "works_at", "Beta", "inferred", 0.9);
        let groups = vec![ContradictionCandidate {
            kind: ContradictionKind::SingleValued,
            facts: vec![user_job.clone(), inferred.clone()],
        }];
        let allowed: HashMap<String, &Fact> = groups
            .iter()
            .flat_map(|g| g.facts.iter().map(|f| (f.id.clone(), f)))
            .collect();
        let attack = ContradictionDemoteProposal {
            demote_id: user_job.id.clone(),
            confidence: 1.0,
        };
        assert_eq!(gate_contradiction_demote(&attack, &allowed, &groups), None);
        let ok = ContradictionDemoteProposal {
            demote_id: inferred.id.clone(),
            confidence: 0.9,
        };
        assert_eq!(
            gate_contradiction_demote(&ok, &allowed, &groups),
            Some(inferred.id.clone())
        );
    }

    #[test]
    fn gate_contradiction_demote_rejects_stale() {
        let mut keep = fresh_fact("fact-aaaa", "works_at", "Acme", "inferred", 0.9);
        let mut drop = fresh_fact("fact-bbbb", "works_at", "Beta", "inferred", 0.85);
        let stale = (chrono::Utc::now() - chrono::Duration::days(30)).to_rfc3339();
        keep.created_at = stale.clone();
        keep.last_seen_at = Some(stale.clone());
        drop.created_at = stale.clone();
        drop.last_seen_at = Some(stale);
        let groups = vec![ContradictionCandidate {
            kind: ContradictionKind::SingleValued,
            facts: vec![keep, drop.clone()],
        }];
        let allowed: HashMap<String, &Fact> = groups
            .iter()
            .flat_map(|g| g.facts.iter().map(|f| (f.id.clone(), f)))
            .collect();
        let ok = ContradictionDemoteProposal {
            demote_id: drop.id.clone(),
            confidence: 1.0,
        };
        assert_eq!(gate_contradiction_demote(&ok, &allowed, &groups), None);
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
        let db = temp_db();
        let router = mock_router("[]");
        let engine = InferenceEngine {
            db: db.clone(),
            router: router.clone(),
            max_transcript_chars: 4_000,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            fact_extraction_min_interval_secs: 3_600,
            inference_semaphore: Arc::new(Semaphore::new(1)),
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
            embedding_index: MemoryEmbeddingIndex::new(db.clone(), router, 64),
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
        let router = mock_router("[]");
        let engine = InferenceEngine {
            db: db.clone(),
            router: router.clone(),
            max_transcript_chars: 4_000,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            fact_extraction_min_interval_secs: 3_600,
            inference_semaphore: Arc::new(Semaphore::new(1)),
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
            embedding_index: MemoryEmbeddingIndex::new(db.clone(), router, 64),
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
    async fn infer_facts_llm_failure_keeps_cursor_for_retry() {
        // Balanced model reply is not valid JSON -> extraction fails. The
        // failure is non-fatal, but the cursor stays behind so a later run can
        // retry instead of silently losing the message window.
        let db = temp_db();
        let session = db.create_session("t1", "").unwrap();
        let _m1 = db
            .add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let router = mock_router("not a json array");
        let engine = InferenceEngine {
            db: db.clone(),
            router: router.clone(),
            max_transcript_chars: 4_000,
            max_known_facts: 40,
            sanitize_max_chars: 256,
            fact_extraction_min_interval_secs: 0,
            inference_semaphore: Arc::new(Semaphore::new(1)),
            outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
            embedding_index: MemoryEmbeddingIndex::new(db.clone(), router, 64),
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
        assert_eq!(cursor, None);
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
