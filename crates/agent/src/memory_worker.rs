mod maintenance;
use maintenance::MemoryMaintenancePass;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use haven_common::prompts::{COMPACTED_SUMMARY_PREFIX, FACT_EXTRACTION_SYSTEM_PROMPT};
use haven_common::retry::{BackoffPolicy, RecoveryDecision, RecoveryPolicy, RecoverySignal};
#[cfg(test)]
use haven_llm::LlmRouter;
#[cfg(test)]
use haven_memory::Database;
use haven_memory::recall::MemoryRetriever;
use haven_memory::repositories::facts::{
    FactSourceRef, is_sensitive_object, is_sensitive_predicate, is_single_valued_predicate,
};
use haven_memory::{
    MemoryFactExtractionStore, MemoryFactStore, MemoryFactWrite, MemoryMaintenanceStore,
    MemoryStore,
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::fact_extraction::FactDraft;
use crate::fact_extraction::{
    LlmFact, extract_json_array, normalize_predicate, sanitize_fact_field, sanitize_tags,
};
#[cfg(test)]
use crate::fact_inference::{
    ContradictionDemoteProposal, PredicateMergeProposal, gate_contradiction_demote,
    gate_predicate_merge,
};
use crate::fact_inference::{
    build_extraction_window, build_numbered_transcript, resolve_source_message,
};
use crate::memory_inference::MemoryInferencePort;
#[cfg(test)]
use crate::memory_inference::RouterMemoryInferencePort;
use crate::memory_service::MemoryService;
#[cfg(test)]
use haven_memory::repositories::facts::Fact;

const OUTBOX_RETRY_MAX_SECS: u64 = 30;
const PERSIST_CONFIDENCE_FLOOR: f64 = 0.55;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FactExtractionJob {
    event_sequence: i64,
    bypass_throttle: bool,
}

impl FactExtractionJob {
    fn merge(&mut self, newer: Self) {
        self.event_sequence = self.event_sequence.max(newer.event_sequence);
        self.bypass_throttle |= newer.bypass_throttle;
    }
}

/// Background memory worker: fact extraction, maintenance, outbox draining,
/// and embedding catch-up. Prompt assembly does not depend on this type.
pub struct MemoryWorker {
    memory: Arc<MemoryService>,
    memory_store: MemoryStore,
    fact_store: MemoryFactStore,
    fact_extraction_store: MemoryFactExtractionStore,
    maintenance_store: MemoryMaintenanceStore,
    inference: Arc<dyn MemoryInferencePort>,
    /// Cap (chars) for transcripts sent to the SmallModel for fact
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
    /// the SmallModel endpoint when multiple sessions complete in rapid
    /// succession.
    inference_semaphore: Arc<Semaphore>,
    /// Pending extraction jobs keyed by session_id. Coalesce the latest event
    /// generation and OR the bypass flag so an older in-flight job cannot
    /// acknowledge a newer committed trigger.
    outbox: Mutex<HashMap<String, FactExtractionJob>>,
    /// Pending compaction-summary extraction jobs keyed by episode id. Each
    /// marker is durable in `kv_store`; this map is only the live wake-up
    /// projection.
    summary_outbox: Mutex<HashMap<String, String>>,
    outbox_notify: Notify,
    /// Serializes worker startup with shutdown so a late enqueue cannot spawn
    /// a replacement worker after the shutdown boundary.
    outbox_lifecycle: Mutex<()>,
    /// Shared shutdown signal for the detached outbox worker and prompt
    /// prefetches. Durable markers remain authoritative when it is cancelled.
    shutdown_token: CancellationToken,
    /// Lazy worker start so `AgentLayer::build` stays usable outside a Tokio
    /// runtime (unit tests that only construct the layer).
    outbox_worker_started: AtomicBool,
    /// Sessions whose MEMORY fence should be refreshed at the next
    /// `before_step` (M2). Set after a successful fact write; cleared by
    /// [`Self::take_memory_dirty_throttled`].
    memory_dirty: Mutex<HashMap<String, Instant>>,
    /// Last successful mid-run MEMORY patch per session (throttle key).
    memory_patch_last: Mutex<HashMap<String, Instant>>,
    /// At most one initial prompt-memory prefetch runs per session. The
    /// cancellation token is also used by session cleanup so a provider call
    /// started for a finished session does not outlive its owner indefinitely.
    prompt_prefetches: Mutex<HashMap<String, CancellationToken>>,
    /// Keep prompt prefetches from competing with the main model for an
    /// unbounded number of provider permits when several sessions start at
    /// once.
    prompt_prefetch_slots: Arc<Semaphore>,
}

impl MemoryWorker {
    #[cfg(test)]
    pub(crate) fn uses_memory_service_for_test(&self, memory: &Arc<MemoryService>) -> bool {
        Arc::ptr_eq(&self.memory, memory)
    }

    #[cfg(test)]
    pub(crate) fn new(
        db: Arc<Database>,
        router: Arc<LlmRouter>,
        max_transcript_chars: usize,
        embed_chunk_size: usize,
        max_known_facts: usize,
        sanitize_max_chars: usize,
        fact_extraction_min_interval_secs: u64,
    ) -> Self {
        let inference = Arc::new(RouterMemoryInferencePort::new(router.clone()));
        let memory = Arc::new(MemoryService::new(
            db,
            Some(router.clone()),
            embed_chunk_size,
        ));
        let fact_store = memory.memory_fact_store();
        Self::new_with_inference(
            memory,
            fact_store,
            inference,
            max_transcript_chars,
            max_known_facts,
            sanitize_max_chars,
            fact_extraction_min_interval_secs,
        )
    }

    pub(crate) fn new_with_inference(
        memory: Arc<MemoryService>,
        fact_store: MemoryFactStore,
        inference: Arc<dyn MemoryInferencePort>,
        max_transcript_chars: usize,
        max_known_facts: usize,
        sanitize_max_chars: usize,
        fact_extraction_min_interval_secs: u64,
    ) -> Self {
        let memory_store = memory.memory_store();
        let fact_extraction_store = memory.memory_fact_extraction_store();
        let maintenance_store = memory.memory_maintenance_store();
        Self {
            memory,
            memory_store,
            fact_store,
            fact_extraction_store,
            maintenance_store,
            inference,
            max_transcript_chars,
            max_known_facts,
            sanitize_max_chars,
            fact_extraction_min_interval_secs,
            inference_semaphore: Arc::new(Semaphore::new(1)),
            outbox: Mutex::new(HashMap::new()),
            summary_outbox: Mutex::new(HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_lifecycle: Mutex::new(()),
            shutdown_token: CancellationToken::new(),
            outbox_worker_started: AtomicBool::new(false),
            memory_dirty: Mutex::new(HashMap::new()),
            memory_patch_last: Mutex::new(HashMap::new()),
            prompt_prefetches: Mutex::new(HashMap::new()),
            prompt_prefetch_slots: Arc::new(Semaphore::new(2)),
        }
    }

    /// Prefetch semantic prompt memory without delaying the first model turn.
    ///
    /// The result is placed in `MemoryService`'s bounded cache. Once it is
    /// ready, the existing MEMORY-fence patch path consumes that cache on the
    /// next turn. This is deliberately best-effort: a provider failure leaves
    /// the first-turn prompt valid and the normal keyword fallback available
    /// to a later refresh.
    pub fn prefetch_prompt_memory(self: &Arc<Self>, session_id: &str, description: &str) {
        if session_id.trim().is_empty()
            || description.trim().is_empty()
            || self.shutdown_token.is_cancelled()
            || tokio::runtime::Handle::try_current().is_err()
        {
            return;
        }
        let session_id = session_id.to_string();
        let description = description.to_string();
        let cancellation = self.shutdown_token.child_token();
        {
            let mut prefetches = self.prompt_prefetches.lock().unwrap();
            if self.shutdown_token.is_cancelled() || prefetches.contains_key(&session_id) {
                return;
            }
            prefetches.insert(session_id.clone(), cancellation.clone());
        }
        // A new run is about to install a fresh no-memory system prompt. Any
        // stale dirty marker must not cause before_step to repeat an old query.
        self.memory_dirty.lock().unwrap().remove(&session_id);

        let worker = self.clone();
        let memory = self.memory.clone();
        let slots = self.prompt_prefetch_slots.clone();
        tokio::spawn(async move {
            let result = tokio::select! {
                _ = cancellation.cancelled() => return,
                permit = slots.acquire_owned() => {
                    let Ok(_permit) = permit else { return };
                    tokio::select! {
                        _ = cancellation.cancelled() => return,
                        result = memory.prompt_candidates(&description, Some(&session_id)) => result,
                    }
                }
            };
            worker.prompt_prefetches.lock().unwrap().remove(&session_id);
            if cancellation.is_cancelled() {
                return;
            }
            match result {
                Ok(_) => {
                    // Only schedule a prompt patch when recall populated the
                    // cache. Embedding failures are intentionally not cached;
                    // marking the fence dirty in that case would make the
                    // first before_step retry the provider synchronously.
                    if memory
                        .has_cached_prompt_candidates(&description, Some(&session_id))
                        .await
                    {
                        worker.mark_memory_dirty(&session_id);
                    }
                }
                Err(error) => tracing::debug!(
                    session_id = %session_id,
                    "initial prompt memory prefetch failed: {error}"
                ),
            }
        });
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

    /// Durably enqueue extraction before exposing it to the existing
    /// in-memory outbox and worker. Persistence failures are returned and do
    /// not enqueue an in-memory job.
    pub(crate) async fn enqueue_infer_durable(
        self: &Arc<Self>,
        session_id: &str,
        bypass_throttle: bool,
        event_sequence: i64,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        self.memory_store
            .enqueue_fact_extraction_cancellable(
                session_id,
                bypass_throttle,
                event_sequence,
                cancellation,
            )
            .await?;
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "fact extraction enqueue cancelled after durable write"
        );
        self.enqueue_memory(
            session_id.to_owned(),
            FactExtractionJob {
                event_sequence,
                bypass_throttle,
            },
        );
        Ok(())
    }

    /// Restore durable fact and compaction-summary extraction jobs into the
    /// existing in-memory outboxes and ensure their shared worker is running.
    /// Durable markers remain the authority until each job completes.
    pub(crate) async fn restore_pending_outbox(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<usize> {
        anyhow::ensure!(
            !self.shutdown_token.is_cancelled(),
            "memory worker is shut down"
        );
        let pending = self
            .memory_store
            .pending_fact_extractions_cancellable(cancellation)
            .await?;
        let pending_summaries = self
            .memory_store
            .pending_summary_extractions_cancellable(cancellation)
            .await?;
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "fact extraction outbox restore cancelled"
        );
        let restored_count = pending.len() + pending_summaries.len();
        for (session_id, bypass_throttle, event_sequence) in pending {
            anyhow::ensure!(
                !cancellation.is_cancelled() && !self.shutdown_token.is_cancelled(),
                "fact extraction outbox restore cancelled"
            );
            self.enqueue_memory(
                session_id,
                FactExtractionJob {
                    event_sequence,
                    bypass_throttle,
                },
            );
        }
        for (session_id, episode_id) in pending_summaries {
            anyhow::ensure!(
                !cancellation.is_cancelled() && !self.shutdown_token.is_cancelled(),
                "summary extraction outbox restore cancelled"
            );
            self.enqueue_summary_memory(session_id, episode_id);
        }
        anyhow::ensure!(
            !cancellation.is_cancelled() && !self.shutdown_token.is_cancelled(),
            "fact extraction outbox restore cancelled"
        );
        self.ensure_outbox_worker();
        self.outbox_notify.notify_one();
        Ok(restored_count)
    }

    /// Stop background work owned by this worker. Pending extraction markers
    /// are intentionally left in durable storage for the next process start.
    pub(crate) fn shutdown(&self) {
        {
            let _lifecycle = self
                .outbox_lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.shutdown_token.cancel();
        }
        self.outbox_notify.notify_waiters();

        let prefetches = {
            let mut prefetches = self
                .prompt_prefetches
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *prefetches)
        };
        for cancellation in prefetches.into_values() {
            cancellation.cancel();
        }
    }

    #[cfg(test)]
    pub(crate) fn suspend_outbox_worker_for_test(&self) {
        self.outbox_worker_started.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn pending_outbox_value_for_test(&self, session_id: &str) -> Option<(i64, bool)> {
        self.outbox
            .lock()
            .ok()?
            .get(session_id)
            .map(|job| (job.event_sequence, job.bypass_throttle))
    }

    #[cfg(test)]
    pub(crate) fn pending_summary_outbox_value_for_test(&self, episode_id: &str) -> Option<String> {
        self.summary_outbox.lock().ok()?.get(episode_id).cloned()
    }

    fn enqueue_memory(self: &Arc<Self>, session_id: String, job: FactExtractionJob) {
        if let Ok(mut pending) = self.outbox.lock() {
            pending
                .entry(session_id)
                .and_modify(|existing| existing.merge(job))
                .or_insert(job);
        }
        self.ensure_outbox_worker();
        self.outbox_notify.notify_one();
    }

    fn enqueue_summary_memory(self: &Arc<Self>, session_id: String, episode_id: String) {
        if let Ok(mut pending) = self.summary_outbox.lock() {
            pending.insert(episode_id, session_id);
        }
        self.ensure_outbox_worker();
        self.outbox_notify.notify_one();
    }

    /// Wake the live projection after a producer has atomically persisted the
    /// episode and its durable summary-extraction marker.
    pub(crate) fn wake_summary_extract(self: &Arc<Self>, session_id: &str, episode_id: &str) {
        self.enqueue_summary_memory(session_id.to_owned(), episode_id.to_owned());
    }

    fn ensure_outbox_worker(self: &Arc<Self>) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let _lifecycle = self
            .outbox_lifecycle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.shutdown_token.is_cancelled() || self.outbox_worker_started.load(Ordering::Acquire)
        {
            return;
        }
        self.outbox_worker_started.store(true, Ordering::Release);
        let engine = self.clone();
        tokio::spawn(async move {
            let cancellation = engine.shutdown_token.clone();
            // Restore jobs that were enqueued by the previous process. Jobs
            // stay durable until successful completion; the extraction cursor
            // makes a replay after a crash idempotent.
            match engine
                .memory_store
                .pending_fact_extractions_cancellable(&cancellation)
                .await
            {
                Ok(restored) => {
                    if let Ok(mut pending) = engine.outbox.lock() {
                        for (session_id, bypass_throttle, event_sequence) in restored {
                            pending
                                .entry(session_id)
                                .and_modify(|existing| {
                                    existing.merge(FactExtractionJob {
                                        event_sequence,
                                        bypass_throttle,
                                    })
                                })
                                .or_insert(FactExtractionJob {
                                    event_sequence,
                                    bypass_throttle,
                                });
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!("fact extraction durable outbox restore failed: {}", error);
                }
            }
            if cancellation.is_cancelled() {
                return;
            }
            match engine
                .memory_store
                .pending_summary_extractions_cancellable(&cancellation)
                .await
            {
                Ok(restored) => {
                    if let Ok(mut pending) = engine.summary_outbox.lock() {
                        for (session_id, episode_id) in restored {
                            pending.insert(episode_id, session_id);
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "summary extraction durable outbox restore failed: {}",
                        error
                    );
                }
            }
            if cancellation.is_cancelled() {
                return;
            }
            let mut fact_retry_attempts = HashMap::<String, u32>::new();
            let mut summary_retry_attempts = HashMap::<String, u32>::new();
            loop {
                if cancellation.is_cancelled() {
                    return;
                }
                let batch: Vec<(String, FactExtractionJob)> = {
                    let mut pending = engine.outbox.lock().unwrap_or_else(|e| e.into_inner());
                    if pending.is_empty() {
                        Vec::new()
                    } else {
                        pending.drain().collect()
                    }
                };
                let summary_batch: Vec<(String, String)> = {
                    let mut pending = engine
                        .summary_outbox
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    pending
                        .drain()
                        .map(|(episode_id, session_id)| (session_id, episode_id))
                        .collect()
                };
                if batch.is_empty() && summary_batch.is_empty() {
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        _ = engine.outbox_notify.notified() => {}
                    }
                    continue;
                }
                for (session_id, job) in batch {
                    if cancellation.is_cancelled() {
                        return;
                    }
                    let completed = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        completed = async {
                            if job.bypass_throttle {
                                engine.infer_session_on_pause(&session_id).await
                            } else {
                                engine.infer_session(&session_id).await
                            }
                        } => completed,
                    };
                    if cancellation.is_cancelled() {
                        return;
                    }
                    if completed {
                        match engine
                            .memory_store
                            .clear_pending_fact_extraction_if_current_cancellable(
                                &session_id,
                                job.event_sequence,
                                job.bypass_throttle,
                                &cancellation,
                            )
                            .await
                        {
                            Ok(_) => {
                                fact_retry_attempts.remove(&session_id);
                            }
                            Err(error) => {
                                if cancellation.is_cancelled() {
                                    return;
                                }
                                let wait_secs = next_outbox_retry_secs(
                                    fact_retry_attempts.entry(session_id.clone()).or_default(),
                                    0,
                                );
                                tracing::warn!(
                                    "fact extraction durable completion failed for session {}: {}; retrying in {}s",
                                    session_id,
                                    error,
                                    wait_secs
                                );
                                engine.enqueue_memory(session_id, job);
                                if !wait_for_outbox_retry(&cancellation, wait_secs).await {
                                    return;
                                }
                            }
                        }
                    } else {
                        let wait_secs = next_outbox_retry_secs(
                            fact_retry_attempts.entry(session_id.clone()).or_default(),
                            0,
                        );
                        tracing::debug!(
                            session = %session_id,
                            wait_secs,
                            "fact extraction deferred; durable marker retained"
                        );
                        engine.enqueue_memory(session_id, job);
                        if !wait_for_outbox_retry(&cancellation, wait_secs).await {
                            return;
                        }
                    }
                }
                for (session_id, episode_id) in summary_batch {
                    if cancellation.is_cancelled() {
                        return;
                    }
                    let summary = match engine
                        .memory_store
                        .episode_text_cancellable(&episode_id, &cancellation)
                        .await
                    {
                        Ok(Some(summary)) if !cancellation.is_cancelled() => summary,
                        Ok(Some(_)) => return,
                        Ok(None) => {
                            if cancellation.is_cancelled() {
                                return;
                            }
                            tracing::debug!(
                                session = %session_id,
                                episode = %episode_id,
                                "dropping summary extraction job for missing episode"
                            );
                            let clear_result = engine
                                .memory_store
                                .clear_summary_extraction_cancellable(
                                    &session_id,
                                    &episode_id,
                                    &cancellation,
                                )
                                .await;
                            if cancellation.is_cancelled() {
                                return;
                            }
                            if let Err(error) = clear_result {
                                let wait_secs = next_outbox_retry_secs(
                                    summary_retry_attempts
                                        .entry(episode_id.clone())
                                        .or_default(),
                                    0,
                                );
                                tracing::warn!(
                                    session = %session_id,
                                    episode = %episode_id,
                                    wait_secs,
                                    "missing summary marker cleanup failed: {error}"
                                );
                                engine.enqueue_summary_memory(session_id, episode_id);
                                if !wait_for_outbox_retry(&cancellation, wait_secs).await {
                                    return;
                                }
                            } else {
                                summary_retry_attempts.remove(&episode_id);
                            }
                            continue;
                        }
                        Err(error) if !cancellation.is_cancelled() => {
                            tracing::warn!(
                                session = %session_id,
                                episode = %episode_id,
                                "summary extraction episode read failed: {error}"
                            );
                            let wait_secs = next_outbox_retry_secs(
                                summary_retry_attempts
                                    .entry(episode_id.clone())
                                    .or_default(),
                                0,
                            );
                            engine.enqueue_summary_memory(session_id, episode_id);
                            if !wait_for_outbox_retry(&cancellation, wait_secs).await {
                                return;
                            }
                            continue;
                        }
                        Err(_) => return,
                    };
                    if cancellation.is_cancelled() {
                        return;
                    }
                    let outcome = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        outcome = engine.infer_facts_from_summary(&session_id, &episode_id, &summary) => outcome,
                    };
                    if cancellation.is_cancelled() {
                        return;
                    }
                    match outcome {
                        SummaryExtractOutcome::Done => {
                            match engine
                                .memory_store
                                .clear_summary_extraction_cancellable(
                                    &session_id,
                                    &episode_id,
                                    &cancellation,
                                )
                                .await
                            {
                                Ok(()) => {
                                    summary_retry_attempts.remove(&episode_id);
                                }
                                Err(error) => {
                                    if cancellation.is_cancelled() {
                                        return;
                                    }
                                    let wait_secs = next_outbox_retry_secs(
                                        summary_retry_attempts
                                            .entry(episode_id.clone())
                                            .or_default(),
                                        0,
                                    );
                                    tracing::warn!(
                                        session = %session_id,
                                        episode = %episode_id,
                                        wait_secs,
                                        "summary extraction durable completion failed: {error}"
                                    );
                                    engine.enqueue_summary_memory(session_id, episode_id);
                                    if !wait_for_outbox_retry(&cancellation, wait_secs).await {
                                        return;
                                    }
                                }
                            }
                        }
                        SummaryExtractOutcome::Throttled { wait_secs }
                        | SummaryExtractOutcome::Retryable { wait_secs } => {
                            let wait_secs = next_outbox_retry_secs(
                                summary_retry_attempts
                                    .entry(episode_id.clone())
                                    .or_default(),
                                wait_secs,
                            );
                            tracing::debug!(
                                session = %session_id,
                                episode = %episode_id,
                                wait_secs,
                                "summary fact inference deferred"
                            );
                            engine.enqueue_summary_memory(session_id, episode_id);
                            if !wait_for_outbox_retry(&cancellation, wait_secs).await {
                                return;
                            }
                        }
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
    /// Tries LLM-assisted extraction via the SmallModel. On any
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
    pub async fn infer_facts(&self, session_id: &str) -> bool {
        self.infer_facts_inner(session_id, false).await
    }

    /// Pause-path extraction: bypasses the time throttle so a same-step
    /// interval infer cannot starve the post-pause pass that has the
    /// fresher transcript (Phase 3 / G2).
    pub async fn infer_facts_on_pause(&self, session_id: &str) -> bool {
        self.infer_facts_inner(session_id, true).await
    }

    async fn infer_facts_inner(&self, session_id: &str, bypass_throttle: bool) -> bool {
        // Time throttle: at most one LLM extraction per interval per session.
        // kv_store key `fact_extraction_last_run.<session_id>` = RFC3339 of
        // the last run that actually called the model. The cleanup routine
        // treats the ordinary cursor, throttle stamp, and summary completion
        // markers as one session-scoped state family and removes them with
        // dead sessions.
        if !bypass_throttle && self.fact_extraction_min_interval_secs > 0 {
            let last_run = match self
                .fact_extraction_store
                .shared_extraction_last_attempt_timestamp(session_id)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(
                        "fact inference throttle read failed for session {}: {}",
                        session_id,
                        error
                    );
                    return false;
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
                return false;
            }
        }

        let transcript = match self
            .fact_extraction_store
            .load_ordinary_transcript(session_id)
            .await
        {
            Ok(transcript) => transcript,
            Err(error) => {
                tracing::warn!(
                    "fact inference: failed to load transcript for session {}: {}",
                    session_id,
                    error
                );
                return false;
            }
        };
        let messages = transcript.messages;
        let steps = transcript.steps;
        if messages.is_empty() {
            return true;
        }

        // Incremental window (M1+M4): cursor tracks user message ids; each new
        // user turn may include a bounded slice of preceding assistant/tool
        // context so short confirmations and tool-grounded replies stay
        // aligned with the model's recent vision — not a full transcript.
        let cursor = match self
            .fact_extraction_store
            .ordinary_extraction_cursor(session_id)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    "fact inference cursor read failed for session {}: {}",
                    session_id,
                    error
                );
                return false;
            }
        };
        let window = build_extraction_window(&messages, cursor.as_deref(), &steps);
        if window.messages.is_empty() {
            tracing::debug!("fact inference: no new messages since cursor");
            // Still advance when the only new rows were low-trust (peer
            // kickoff / cross-session) so extraction does not stall forever.
            if let Some(last) = window.cursor_last
                && let Err(error) = self
                    .fact_store
                    .commit_ordinary_extraction(
                        Vec::new(),
                        PERSIST_CONFIDENCE_FLOOR,
                        session_id,
                        &last,
                    )
                    .await
            {
                tracing::warn!(
                    "fact inference cursor commit failed for session {}: {}",
                    session_id,
                    error
                );
                return false;
            }
            return true;
        }

        // Stamp the run timestamp BEFORE calling the model: the throttle
        // guards "no more than one LLM call per interval", so even a failed
        // call counts as a run (otherwise a persistent failure would retry
        // every turn despite the cursor advancing).
        if self.fact_extraction_min_interval_secs > 0 {
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(error) = self
                .fact_extraction_store
                .stamp_shared_extraction_attempt(session_id, &now)
                .await
            {
                tracing::warn!(
                    "fact inference throttle stamp failed for session {}; skipping LLM call: {}",
                    session_id,
                    error
                );
                return false;
            }
        }

        let wrote = match self.infer_facts_with_llm(&window.messages).await {
            Ok(facts) => {
                if facts.is_empty() {
                    tracing::debug!("LLM found no facts in session {}", session_id);
                }
                let writes = self.prepare_fact_writes(&facts, &window.messages);
                let Some(last_message_id) = window.cursor_last.as_deref() else {
                    return true;
                };
                match self
                    .fact_store
                    .commit_ordinary_extraction(
                        writes,
                        PERSIST_CONFIDENCE_FLOOR,
                        session_id,
                        last_message_id,
                    )
                    .await
                {
                    Ok(wrote) => wrote,
                    Err(_) => {
                        tracing::warn!(
                            session_id = %session_id,
                            fact_count = facts.len(),
                            "ordinary fact extraction commit failed; keeping extraction cursor unchanged"
                        );
                        return false;
                    }
                }
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %session_id,
                    "LLM fact extraction failed; keeping extraction cursor unchanged"
                );
                return false;
            }
        };
        if wrote {
            self.mark_memory_dirty(session_id);
        }
        true
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
    /// Returns the sum of rows touched by dedup / sensitive / rule-based
    /// contradiction demotes / flush / embedding prune / predicate rewrites
    /// / LLM arbitration (cursor cleanup and embedding catch-up are best-effort
    /// and not counted).
    pub async fn run_memory_maintenance(&self) -> anyhow::Result<u64> {
        MemoryMaintenancePass::new(
            &self.maintenance_store,
            self.inference.as_ref(),
            self.inference_semaphore.as_ref(),
            self.memory.as_ref(),
        )
        .run(None)
        .await
    }

    pub(crate) async fn run_memory_maintenance_cancellable(
        &self,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<u64> {
        MemoryMaintenancePass::new(
            &self.maintenance_store,
            self.inference.as_ref(),
            self.inference_semaphore.as_ref(),
            self.memory.as_ref(),
        )
        .run(Some(cancellation))
        .await
    }

    /// Apply extraction policy and prepare facts for one atomic extraction
    /// commit. `message_index` resolves to a user line when possible for the
    /// persisted provenance reference (M1).
    fn prepare_fact_writes(
        &self,
        facts: &[LlmFact],
        messages: &[haven_memory::repositories::messages::Message],
    ) -> Vec<MemoryFactWrite> {
        let mut writes = Vec::with_capacity(facts.len());
        for fact in facts {
            let subject = sanitize_fact_field(&fact.subject, self.sanitize_max_chars);
            let predicate = normalize_predicate(&fact.predicate);
            let object = sanitize_fact_field(&fact.object, self.sanitize_max_chars);
            if predicate.is_empty() || subject.is_empty() || object.is_empty() {
                tracing::debug!(
                    "fact inference: dropping degenerate fact (empty subject/predicate/object)"
                );
                continue;
            }
            if is_sensitive_predicate(&predicate) || is_sensitive_object(&object) {
                tracing::debug!("fact inference: dropping sensitive fact");
                continue;
            }
            // Clamp to the documented range so an over-eager model
            // (e.g. 1.2) does not skew decay/ordering.
            let confidence = fact.confidence.clamp(0.5, 1.0);
            let tags = sanitize_tags(&fact.tags);
            let source_ref = fact
                .message_index
                .and_then(|idx| resolve_source_message(messages, idx))
                .map(|message| FactSourceRef::from_message(&message.id, &message.content));
            writes.push(MemoryFactWrite {
                subject,
                is_single_valued_predicate: is_single_valued_predicate(&predicate),
                predicate,
                object,
                confidence,
                tags,
                source_ref,
                durability: fact.durability.unwrap_or(0.6).clamp(0.1, 1.0),
            });
        }
        writes
    }

    #[cfg(test)]
    async fn persist_fact_batch(&self, facts: Vec<FactDraft>) -> anyhow::Result<bool> {
        let mut writes = Vec::with_capacity(facts.len());
        for (subject, predicate, object, confidence, tags, source_ref, durability) in facts {
            let subject = sanitize_fact_field(&subject, self.sanitize_max_chars);
            let predicate = normalize_predicate(&predicate);
            let object = sanitize_fact_field(&object, self.sanitize_max_chars);
            if subject.is_empty()
                || predicate.is_empty()
                || object.is_empty()
                || is_sensitive_predicate(&predicate)
                || is_sensitive_object(&object)
            {
                continue;
            }
            writes.push(MemoryFactWrite {
                subject,
                is_single_valued_predicate: is_single_valued_predicate(&predicate),
                predicate,
                object,
                confidence: confidence.clamp(0.5, 1.0),
                tags: sanitize_tags(&tags),
                source_ref,
                durability: durability.clamp(0.1, 1.0),
            });
        }
        self.fact_store
            .persist_inferred_batch(writes, PERSIST_CONFIDENCE_FLOOR)
            .await
    }

    /// Send the conversation transcript to the SmallModel and ask it to
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
            .inference
            .fast_chat(FACT_EXTRACTION_SYSTEM_PROMPT, &user_content)
            .await
            .map_err(|_| anyhow::anyhow!("small model chat failed"))?;

        if response.trim().is_empty() {
            tracing::debug!("LLM fact extraction: empty model response, treating as no facts");
            return Ok(Vec::new());
        }

        let json_str = extract_json_array(&response);
        let facts: Vec<LlmFact> = serde_json::from_str(&json_str)
            .map_err(|_| anyhow::anyhow!("failed to parse LLM fact JSON"))?;

        tracing::info!("LLM fact extraction: {} facts extracted", facts.len());
        Ok(facts)
    }

    /// Compact list of the stored facts (effective-confidence order, all
    /// subjects) to hand the extraction model as context. Cross-subject facts
    /// (projects, tools, other entities) carry their subject prefix so the
    /// model can re-confirm or update them with the same subject instead of
    /// collapsing everything onto "user".
    async fn load_known_facts(&self) -> String {
        let facts = match self
            .fact_store
            .list_recent_visible_facts_limited(self.max_known_facts)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("load_known_facts: list_facts failed: {}", e);
                Vec::new()
            }
        };
        let mut lines: Vec<String> = Vec::new();
        for fact in &facts {
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
    pub async fn infer_session(&self, session_id: &str) -> bool {
        let completed = self.infer_facts(session_id).await;
        self.memory.embed_new_memory().await;
        completed
    }

    /// Pause-path variant: bypasses the extraction time throttle so a
    /// same-step interval infer cannot starve the fresher post-pause pass.
    pub async fn infer_session_on_pause(&self, session_id: &str) -> bool {
        let completed = self.infer_facts_on_pause(session_id).await;
        self.memory.embed_new_memory().await;
        completed
    }

    /// Drop mid-run MEMORY patch bookkeeping for a finished session.
    pub fn clear_session(&self, session_id: &str) {
        self.memory_dirty.lock().unwrap().remove(session_id);
        self.memory_patch_last.lock().unwrap().remove(session_id);
        if let Some(cancellation) = self.prompt_prefetches.lock().unwrap().remove(session_id) {
            cancellation.cancel();
        }
    }

    /// Light extraction from a CompactSummary episode (M3). Respects the
    /// shared extraction time throttle and a per-episode completion marker;
    /// never touches the user-message cursor. Facts and the completion marker
    /// commit together so an outbox-ack retry cannot reinforce them again.
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
        let already_completed = match self
            .fact_store
            .summary_extraction_completed(session_id, episode_id)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    "summary fact extraction completion read failed for session {} episode {}: {}",
                    session_id,
                    episode_id,
                    error
                );
                return SummaryExtractOutcome::Retryable { wait_secs: 1 };
            }
        };
        if already_completed {
            return SummaryExtractOutcome::Done;
        }
        // Share the wall-clock throttle with normal extraction so compaction
        // cannot bypass the interval and spam the small model.
        if self.fact_extraction_min_interval_secs > 0 {
            let last_run = match self
                .fact_extraction_store
                .shared_extraction_last_attempt_timestamp(session_id)
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
            let now = chrono::Utc::now().to_rfc3339();
            if let Err(error) = self
                .fact_extraction_store
                .stamp_shared_extraction_attempt(session_id, &now)
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
            media_inputs: vec![],
            voice: false,
            ingress_seq: 0,
        };

        let writes = match self
            .infer_facts_with_llm(std::slice::from_ref(&synthetic))
            .await
        {
            Ok(facts) => {
                if facts.is_empty() {
                    tracing::debug!(
                        "LLM found no facts in compaction summary for session {}",
                        session_id
                    );
                }
                self.prepare_fact_writes(&facts, std::slice::from_ref(&synthetic))
            }
            Err(_) => {
                tracing::warn!(
                    session_id = %session_id,
                    episode_id = %episode_id,
                    "LLM summary fact extraction failed; leaving episode retryable"
                );
                return SummaryExtractOutcome::Retryable { wait_secs: 1 };
            }
        };

        match self
            .fact_store
            .commit_summary_extraction(writes, PERSIST_CONFIDENCE_FLOOR, session_id, episode_id)
            .await
        {
            Ok(wrote) => {
                if wrote {
                    self.mark_memory_dirty(session_id);
                }
            }
            Err(error) => {
                tracing::warn!(
                    session_id = %session_id,
                    episode_id = %episode_id,
                    error = %error,
                    "summary fact extraction commit failed; leaving episode retryable"
                );
                return SummaryExtractOutcome::Retryable { wait_secs: 1 };
            }
        }
        SummaryExtractOutcome::Done
    }
}

fn next_outbox_retry_secs(attempt: &mut u32, requested_wait_secs: u64) -> u64 {
    let completed_attempts = attempt.saturating_add(1);
    *attempt = completed_attempts;
    let policy = RecoveryPolicy::new(
        None,
        None,
        BackoffPolicy::new(
            Duration::from_secs(1),
            2,
            Duration::from_secs(OUTBOX_RETRY_MAX_SECS),
        ),
    );
    match policy.decide(
        completed_attempts,
        RecoverySignal::Retryable {
            retry_after: Some(Duration::from_secs(requested_wait_secs)),
        },
        Instant::now(),
        0,
    ) {
        RecoveryDecision::Retry { delay, .. } => delay.as_secs(),
        RecoveryDecision::Stop { .. } => requested_wait_secs.max(OUTBOX_RETRY_MAX_SECS),
    }
}

async fn wait_for_outbox_retry(cancellation: &CancellationToken, wait_secs: u64) -> bool {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => false,
        _ = tokio::time::sleep(Duration::from_secs(wait_secs)) => true,
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
    use super::maintenance::MemoryMaintenancePass;
    use super::*;
    use crate::fact_inference::{
        EXTRACTION_TOOL_CONTENT_CHARS, build_extraction_window, build_numbered_transcript,
    };
    use crate::memory_index::embedding_batch_size;
    use async_trait::async_trait;
    use haven_common::types::CanonicalMessage;
    use haven_llm::client::LlmClient;
    use haven_llm::types::{FinishReason, LlmError, LlmResponse, StreamChunk};
    use haven_memory::repositories::facts::{
        ContradictionCandidate, ContradictionKind, FactSourceRef,
    };
    use haven_memory::repositories::messages::Message;
    use haven_memory::repositories::session_steps::SessionStep;
    use std::collections::VecDeque;
    use std::pin::Pin;
    use std::sync::atomic::AtomicUsize;
    use tokio::sync::{mpsc, oneshot};

    struct FixedMemoryInference {
        fast_chat_configured: bool,
        response: String,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl MemoryInferencePort for FixedMemoryInference {
        async fn is_fast_chat_configured(&self) -> bool {
            self.fast_chat_configured
        }

        async fn fast_chat(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.response.clone())
        }
    }

    struct BlockingMemoryInference {
        started: Notify,
    }

    #[async_trait]
    impl MemoryInferencePort for BlockingMemoryInference {
        async fn is_fast_chat_configured(&self) -> bool {
            true
        }

        async fn fast_chat(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> anyhow::Result<String> {
            self.started.notify_one();
            std::future::pending().await
        }
    }

    struct GatedMemoryInference {
        started: mpsc::UnboundedSender<usize>,
        releases: Mutex<VecDeque<oneshot::Receiver<()>>>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl MemoryInferencePort for GatedMemoryInference {
        async fn is_fast_chat_configured(&self) -> bool {
            true
        }

        async fn fast_chat(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> anyhow::Result<String> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            self.started.send(call).map_err(|error| {
                anyhow::anyhow!("test inference start receiver closed: {error}")
            })?;
            let release = self
                .releases
                .lock()
                .unwrap()
                .pop_front()
                .expect("each gated inference call has a release receiver");
            release.await.map_err(|error| {
                anyhow::anyhow!("test inference release sender closed: {error}")
            })?;
            Ok("[]".to_owned())
        }
    }

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
        async fn chat_with_output_cap(
            &self,
            messages: Vec<CanonicalMessage>,
            _max_output_tokens: Option<u32>,
        ) -> Result<LlmResponse, LlmError> {
            self.chat(messages).await
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
        async fn chat_stream_with_tools_output_cap_shared(
            &self,
            _messages: std::sync::Arc<[CanonicalMessage]>,
            _tools: std::sync::Arc<[haven_llm::types::ToolDefinition]>,
            _max_output_tokens: Option<u32>,
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
            media_inputs: vec![],
            voice: false,
            ingress_seq: 0,
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
    async fn known_fact_context_preserves_order_format_limit_and_sensitive_filtering() {
        let db = temp_db();
        db.insert_fact("project: Haven", "uses", "Rust", "user", 0.95, &[])
            .unwrap();
        db.insert_fact("user", "likes", "tea", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "hunter2", "user", 1.0, &[])
            .unwrap();
        db.insert_fact("user", "uses", "sk-hidden-token", "user", 0.98, &[])
            .unwrap();
        let engine = MemoryWorker::new(db, mock_router("[]"), 4_000, 64, 2, 256, 0);

        let known = engine.load_known_facts().await;

        assert_eq!(
            known,
            "- [project: Haven] uses=Rust (95%)\n- likes=tea (90%)"
        );
    }

    fn make_engine(db: Arc<Database>) -> MemoryWorker {
        let router = mock_router("[]");
        MemoryWorker::new(db, router, 4_000, 64, 40, 256, 0)
    }

    fn make_engine_with_inference(
        db: Arc<Database>,
        inference: Arc<dyn MemoryInferencePort>,
    ) -> MemoryWorker {
        make_engine_with_inference_and_interval(db, inference, 0)
    }

    fn make_engine_with_inference_and_interval(
        db: Arc<Database>,
        inference: Arc<dyn MemoryInferencePort>,
        fact_extraction_min_interval_secs: u64,
    ) -> MemoryWorker {
        let memory = Arc::new(MemoryService::new(db, None, 64));
        MemoryWorker::new_with_inference(
            memory.clone(),
            memory.memory_fact_store(),
            inference,
            4_000,
            64,
            256,
            fact_extraction_min_interval_secs,
        )
    }

    fn insert_noncanonical_predicate(db: &Database, predicate: &str, object: &str) -> String {
        let fact = db
            .insert_fact("user", "likes", object, "inferred", 0.9, &[])
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET predicate = ?1 WHERE id = ?2",
                [predicate, fact.id.as_str()],
            )
            .unwrap();
        fact.id
    }

    fn insert_orphan_embedding(db: &Database) {
        db.conn()
            .execute(
                "INSERT INTO memory_embeddings
                    (entity_type, entity_id, model, vector, text)
                 VALUES ('fact', 'fact-orphan', 'model', x'00000000', 'orphan')",
                [],
            )
            .unwrap();
        db.conn()
            .execute(
                "INSERT INTO embedding_lsh (entity_type, entity_id, model, bucket)
                 VALUES ('fact', 'fact-orphan', 'model', 1)",
                [],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn memory_maintenance_returns_sum_of_deterministic_success_counts() {
        let db = temp_db();
        db.insert_fact("user", "likes", "Go", "user", 0.5, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Go", "user", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "api_key", "sk-test-secret", "inferred", 0.9, &[])
            .unwrap();
        let stale = db
            .insert_fact("user", "likes", "Python", "inferred", 0.1, &[])
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET created_at = '2000-01-01T00:00:00Z',
                                  last_seen_at = '2000-01-01T00:00:00Z'
                 WHERE id = ?1",
                [&stale.id],
            )
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.9, &[])
            .unwrap();
        db.insert_fact("user", "dislikes", "Rust", "user", 1.0, &[])
            .unwrap();
        insert_orphan_embedding(&db);
        let worker = make_engine(db);

        assert_eq!(worker.run_memory_maintenance().await.unwrap(), 5);
    }

    #[tokio::test]
    async fn memory_maintenance_runs_contradiction_keeper_before_low_confidence_flush() {
        let db = temp_db();
        let loser_id = insert_noncanonical_predicate(&db, "language", "Go");
        db.conn()
            .execute(
                "UPDATE facts SET confidence = 0.5 WHERE id = ?1",
                [&loser_id],
            )
            .unwrap();
        let keeper_id = insert_noncanonical_predicate(&db, "language", "Rust");
        db.conn()
            .execute(
                "UPDATE facts SET source = 'user', confidence = 1.0 WHERE id = ?1",
                [&keeper_id],
            )
            .unwrap();
        let thirty_six_hours_ago = (chrono::Utc::now() - chrono::Duration::hours(36)).to_rfc3339();
        db.conn()
            .execute(
                "UPDATE facts SET created_at = ?1, last_seen_at = ?1 WHERE id IN (?2, ?3)",
                [
                    thirty_six_hours_ago.as_str(),
                    loser_id.as_str(),
                    keeper_id.as_str(),
                ],
            )
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: false,
            response: String::new(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db.clone(), inference);

        assert_eq!(worker.run_memory_maintenance().await.unwrap(), 2);
        assert!(db.get_fact_by_id(&keeper_id).unwrap().is_some());
        assert!(
            db.get_fact_by_id(&loser_id).unwrap().is_none(),
            "rule demotion must happen before the low-confidence purge"
        );
    }

    #[tokio::test]
    async fn memory_maintenance_continues_after_one_store_operation_fails() {
        let db = temp_db();
        let fact = db
            .insert_fact("user", "likes", "Go", "inferred", 0.8, &[])
            .unwrap();
        db.conn()
            .execute(
                "UPDATE facts SET provenance_record_id = '  ' WHERE id = ?1",
                [&fact.id],
            )
            .unwrap();
        db.set_kv("fact_extraction.ses-deadbeef", "msg-deadbeef")
            .unwrap();
        db.conn()
            .execute_batch("DROP TABLE memory_embeddings")
            .unwrap();
        let worker = make_engine(db.clone());

        let error = worker.run_memory_maintenance().await.unwrap_err();

        let message = error.to_string();
        assert!(message.contains("memory maintenance failed"));
        assert!(message.contains("prune_orphaned_embeddings"));
        assert!(!message.contains("cleanup_orphan_extraction_cursors"));
        assert!(!message.contains("cleanup_orphan_source_refs"));
        assert_eq!(
            db.get_kv("fact_extraction.ses-deadbeef").unwrap(),
            None,
            "cursor cleanup after the failed prune must still run"
        );
        assert!(
            db.get_fact_by_id(&fact.id)
                .unwrap()
                .unwrap()
                .source_ref
                .is_none(),
            "source ref cleanup after the failed prune must still run"
        );
    }

    #[tokio::test]
    async fn memory_maintenance_cancellation_stops_before_first_database_operation() {
        let db = temp_db();
        let first = db
            .insert_fact("user", "likes", "Rust", "user", 0.8, &[])
            .unwrap();
        db.insert_fact("user", "likes", "Rust", "user", 0.7, &[])
            .unwrap();
        let worker = make_engine(db.clone());
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = worker
            .run_memory_maintenance_cancellable(&cancellation)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        assert_eq!(db.get_facts("user").unwrap().len(), 2);
        assert!(db.get_fact_by_id(&first.id).unwrap().is_some());
    }

    #[tokio::test]
    async fn predicate_llm_rewrites_keep_gates_and_accumulate_each_store_result() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        for object in ["D:/one", "D:/two", "D:/three"] {
            insert_noncanonical_predicate(&db, "workspace", object);
        }
        for object in ["Rust", "Go"] {
            insert_noncanonical_predicate(&db, "fav_lang", object);
        }
        let canonical = db
            .insert_fact("user", "likes", "tea", "user", 1.0, &[])
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response: r#"[
                {"from":"workspace","to":"project_path","confidence":0.95},
                {"from":"fav_lang","to":"language","confidence":0.95},
                {"from":"likes","to":"dislikes","confidence":1.0}
            ]"#
            .into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db.clone(), inference.clone());

        let rewritten = MemoryMaintenancePass::new(
            &worker.maintenance_store,
            worker.inference.as_ref(),
            worker.inference_semaphore.as_ref(),
            worker.memory.as_ref(),
        )
        .merge_predicates_with_llm()
        .await;

        assert_eq!(
            rewritten, 5,
            "counts from separate rewrites are accumulated"
        );
        assert_eq!(inference.calls.load(Ordering::Relaxed), 1);
        let facts = db.get_facts("user").unwrap();
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.predicate == "project_path")
                .count(),
            3
        );
        assert_eq!(
            facts
                .iter()
                .filter(|fact| fact.predicate == "language")
                .count(),
            2
        );
        assert!(
            facts
                .iter()
                .any(|fact| fact.id == canonical.id && fact.predicate == "likes")
        );
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact.predicate.as_str(), "workspace" | "fav_lang"))
        );
    }

    #[tokio::test]
    async fn contradiction_llm_arbitration_uses_store_after_policy_gate() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.insert_fact("user", "likes", "Rust", "user", 1.0, &[])
            .unwrap();
        let inferred = db
            .insert_fact("user", "dislikes", "Rust", "inferred", 0.8, &[])
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response: format!(r#"[{{"demote_id":"{}","confidence":0.95}}]"#, inferred.id),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db.clone(), inference.clone());

        let demoted = MemoryMaintenancePass::new(
            &worker.maintenance_store,
            worker.inference.as_ref(),
            worker.inference_semaphore.as_ref(),
            worker.memory.as_ref(),
        )
        .arbitrate_contradictions_with_llm()
        .await;

        assert_eq!(demoted, 1);
        assert_eq!(inference.calls.load(Ordering::Relaxed), 1);
        let updated = db.get_fact_by_id(&inferred.id).unwrap().unwrap();
        assert!((updated.confidence - 0.4).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn llm_maintenance_still_skips_calls_when_fast_chat_is_unconfigured() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        insert_noncanonical_predicate(&db, "workspace", "D:/one");
        insert_noncanonical_predicate(&db, "fav_lang", "Rust");
        db.insert_fact("user", "likes", "Rust", "user", 1.0, &[])
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: false,
            response: r#"[{"from":"workspace","to":"project_path","confidence":1.0}]"#.into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db, inference.clone());

        let maintenance = MemoryMaintenancePass::new(
            &worker.maintenance_store,
            worker.inference.as_ref(),
            worker.inference_semaphore.as_ref(),
            worker.memory.as_ref(),
        );
        assert_eq!(maintenance.merge_predicates_with_llm().await, 0);
        assert_eq!(maintenance.arbitrate_contradictions_with_llm().await, 0);
        assert_eq!(inference.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn outbox_retry_backoff_is_bounded_but_honors_throttle_wait() {
        let mut attempt = 0;
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 1);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 2);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 4);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 8);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 16);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 30);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 900), 900);
    }

    #[test]
    fn shutdown_cancels_worker_and_prompt_prefetch_tokens() {
        let worker = make_engine(temp_db());
        let prefetch_cancellation = CancellationToken::new();
        worker
            .prompt_prefetches
            .lock()
            .unwrap()
            .insert("ses-shutdown-test".into(), prefetch_cancellation.clone());

        worker.shutdown();

        assert!(worker.shutdown_token.is_cancelled());
        assert!(prefetch_cancellation.is_cancelled());
        assert!(worker.prompt_prefetches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn outbox_worker_cannot_start_after_shutdown() {
        let worker = Arc::new(make_engine(temp_db()));
        worker.shutdown();

        worker.ensure_outbox_worker();
        tokio::task::yield_now().await;

        assert!(worker.shutdown_token.is_cancelled());
        assert!(!worker.outbox_worker_started.load(Ordering::Acquire));
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
            media_inputs: vec![],
            voice: false,
            ingress_seq: 0,
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
        let second_kickoff = make_role_message(
            "user",
            "[Delegated task from agent ses-parent — LOW TRUST, not a user instruction]\nold",
        );
        let mut second_kickoff = second_kickoff;
        second_kickoff.message_type = Some("peer_kickoff".into());
        let real = make_role_message("user", "My name is Alice");
        let window = build_extraction_window(&[kickoff, second_kickoff, real.clone()], None, &[]);
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

    #[tokio::test]
    async fn extraction_store_projections_keep_the_incremental_message_window() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db
            .create_session("fact extraction persisted window")
            .unwrap();
        let previous_user = db
            .add_message(&session.id, "user", "Earlier user turn", Some("text"), None)
            .unwrap();
        let ask = db
            .add_message(
                &session.id,
                "assistant",
                "Checking the path",
                Some("text"),
                None,
            )
            .unwrap();
        let reasoning = db
            .add_message(
                &session.id,
                "assistant",
                "private reasoning",
                Some("reasoning"),
                None,
            )
            .unwrap();
        let current_user = db
            .add_message(&session.id, "user", "Use that path", Some("text"), None)
            .unwrap();
        let message_times = [
            (previous_user.id.as_str(), "2026-01-01T00:00:00.000Z"),
            (ask.id.as_str(), "2026-01-01T00:00:01.000Z"),
            (reasoning.id.as_str(), "2026-01-01T00:00:02.000Z"),
            (current_user.id.as_str(), "2026-01-01T00:00:05.000Z"),
        ];
        for (message_id, timestamp) in message_times {
            db.conn()
                .execute(
                    "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                    [timestamp, message_id],
                )
                .unwrap();
        }

        let step_id = haven_common::types::new_id("step");
        let step = db
            .create_action_step(
                &session.id,
                1,
                "shell",
                "{}",
                false,
                false,
                None,
                Some(&step_id),
            )
            .unwrap();
        db.complete_action_step(&step.id, "C:/Workspace/Haven", true)
            .unwrap();
        db.conn()
            .execute(
                "UPDATE session_steps
                 SET created_at = ?1, started_at = ?2, completed_at = ?3
                 WHERE id = ?4",
                [
                    "2026-01-01T00:00:03.000Z",
                    "2026-01-01T00:00:03.000Z",
                    "2026-01-01T00:00:04.000Z",
                    step.id.as_str(),
                ],
            )
            .unwrap();

        let transcript = MemoryFactExtractionStore::new(db)
            .load_ordinary_transcript(&session.id)
            .await
            .unwrap();
        let window = build_extraction_window(
            &transcript.messages,
            Some(&previous_user.id),
            &transcript.steps,
        );

        assert_eq!(window.messages.len(), 3);
        assert_eq!(window.messages[0].id, ask.id);
        assert_eq!(window.messages[1].role, "tool");
        assert!(window.messages[1].content.contains("tool(shell):"));
        assert!(window.messages[1].content.contains("C:/Workspace/Haven"));
        assert_eq!(window.messages[2].id, current_user.id);
        assert_eq!(
            window.cursor_last.as_deref(),
            Some(current_user.id.as_str())
        );
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
        let engine = MemoryWorker::new(db.clone(), router, 4_000, 64, 40, 256, 3_600);
        engine.mark_memory_dirty("ses-b");
        assert!(engine.take_memory_dirty_throttled("ses-b"));
        engine.mark_memory_dirty("ses-b");
        assert!(!engine.take_memory_dirty_throttled("ses-b"));
    }

    #[tokio::test]
    async fn prompt_prefetch_is_deduplicated_and_cancelled_on_cleanup() {
        let worker = Arc::new(make_engine(temp_db()));

        worker.prefetch_prompt_memory("ses-prefetch", "workspace task");
        worker.prefetch_prompt_memory("ses-prefetch", "workspace task");
        assert_eq!(worker.prompt_prefetches.lock().unwrap().len(), 1);

        worker.clear_session("ses-prefetch");
        assert!(worker.prompt_prefetches.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn restore_pending_summary_extraction_rehydrates_live_projection() {
        let db = temp_db();
        let session = db.create_session("summary restore").unwrap();
        let episode_id = "msg-summary-restore";
        db.add_episode_with_pending_extraction(
            &session.id,
            "A durable summary restored after process restart.",
            episode_id,
            true,
        )
        .unwrap();
        let worker = Arc::new(make_engine(db));
        worker.suspend_outbox_worker_for_test();

        let restored = worker
            .restore_pending_outbox(&CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(restored, 1);
        assert_eq!(
            worker.summary_outbox.lock().unwrap().get(episode_id),
            Some(&session.id)
        );
    }

    #[tokio::test]
    async fn restored_fact_and_summary_jobs_acknowledge_successful_durable_markers() {
        let db = temp_db();
        let session = db.create_session("outbox acknowledgements").unwrap();
        db.enqueue_fact_extraction(&session.id, false, 1).unwrap();
        db.add_episode_with_pending_extraction(
            &session.id,
            "A durable summary with enough text for the extraction path.",
            "msg-summary-ack-success",
            true,
        )
        .unwrap();
        let inference: Arc<dyn MemoryInferencePort> = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response: "[]".to_owned(),
            calls: AtomicUsize::new(0),
        });
        let memory = Arc::new(MemoryService::new(db.clone(), None, 64));
        let worker = Arc::new(MemoryWorker::new_with_inference(
            memory.clone(),
            memory.memory_fact_store(),
            inference,
            4_000,
            64,
            256,
            0,
        ));

        assert_eq!(
            worker
                .restore_pending_outbox(&CancellationToken::new())
                .await
                .unwrap(),
            2
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if db.pending_fact_extractions().unwrap().is_empty()
                    && db.pending_summary_extractions().unwrap().is_empty()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("successful jobs should acknowledge their durable markers");
        worker.shutdown();
    }

    #[tokio::test]
    async fn failed_fact_marker_ack_keeps_marker_and_requeues_live_job() {
        let db = temp_db();
        let session = db.create_session("outbox fact retry").unwrap();
        db.enqueue_fact_extraction(&session.id, false, 1).unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE marker_ack_attempts (kind TEXT NOT NULL);
                 CREATE TRIGGER reject_fact_marker_ack
                 BEFORE DELETE ON kv_store
                 WHEN old.key LIKE 'fact_extraction_pending.%'
                 BEGIN
                    INSERT INTO marker_ack_attempts (kind) VALUES ('fact');
                    SELECT RAISE(FAIL, 'fact marker acknowledgement unavailable');
                 END;",
            )
            .unwrap();
        let worker = Arc::new(make_engine(db.clone()));

        worker
            .restore_pending_outbox(&CancellationToken::new())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let attempts: i64 = db
                    .conn()
                    .query_row(
                        "SELECT COUNT(*) FROM marker_ack_attempts WHERE kind = 'fact'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                if attempts > 0
                    && worker.pending_outbox_value_for_test(&session.id) == Some((1, false))
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("failed acknowledgement should requeue the live job");

        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), false, 1)]
        );
        worker.shutdown();
    }

    #[tokio::test]
    async fn newer_same_bypass_trigger_survives_an_in_flight_fact_ack() {
        let db = temp_db();
        let session = db.create_session("outbox generation race").unwrap();
        let first_message = db
            .add_message(&session.id, "user", "I prefer Rust.", Some("text"), None)
            .unwrap();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();
        let (release_second_tx, release_second_rx) = oneshot::channel();
        let inference = Arc::new(GatedMemoryInference {
            started: started_tx,
            releases: Mutex::new(VecDeque::from(vec![release_first_rx, release_second_rx])),
            calls: AtomicUsize::new(0),
        });
        let memory = Arc::new(MemoryService::new(db.clone(), None, 64));
        let worker = Arc::new(MemoryWorker::new_with_inference(
            memory.clone(),
            memory.memory_fact_store(),
            inference.clone(),
            4_000,
            64,
            256,
            0,
        ));

        worker
            .enqueue_infer_durable(&session.id, true, 1, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), started_rx.recv())
                .await
                .expect("first extraction should enter model inference"),
            Some(1)
        );

        let second_message = db
            .add_message(
                &session.id,
                "user",
                "I also use Windows.",
                Some("text"),
                None,
            )
            .unwrap();
        worker
            .enqueue_infer_durable(&session.id, true, 2, &CancellationToken::new())
            .await
            .unwrap();

        release_first_tx
            .send(())
            .expect("first extraction should still be waiting");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), started_rx.recv())
                .await
                .expect("second extraction should run after the first ack"),
            Some(2)
        );
        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 2)],
            "the old in-flight generation must not clear the newer same-bypass marker"
        );
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .as_deref(),
            Some(first_message.id.as_str()),
            "the first inference commits only the transcript window it read before waiting"
        );

        release_second_tx
            .send(())
            .expect("second extraction should still be waiting");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if db.pending_fact_extractions().unwrap().is_empty()
                    && worker.pending_outbox_value_for_test(&session.id).is_none()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the newer generation should be acknowledged after its own extraction");
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .as_deref(),
            Some(second_message.id.as_str())
        );
        assert_eq!(inference.calls.load(Ordering::Relaxed), 2);
        worker.shutdown();
    }

    #[tokio::test]
    async fn failed_summary_marker_ack_keeps_marker_and_requeues_live_job() {
        let db = temp_db();
        let session = db.create_session("outbox summary retry").unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.7, &[])
            .unwrap();
        let episode_id = "msg-summary-ack-retry";
        db.add_episode_with_pending_extraction(
            &session.id,
            "A durable summary with enough text for a successful empty extraction.",
            episode_id,
            true,
        )
        .unwrap();
        db.conn()
            .execute_batch(
                "CREATE TABLE marker_ack_attempts (kind TEXT NOT NULL);
                 CREATE TRIGGER reject_first_summary_marker_ack
                 BEFORE DELETE ON kv_store
                 WHEN old.key LIKE 'fact_extraction_episode_pending.%'
                 BEGIN
                    INSERT INTO marker_ack_attempts (kind) VALUES ('summary');
                    SELECT RAISE(FAIL, 'summary marker acknowledgement unavailable')
                    WHERE (SELECT COUNT(*) FROM marker_ack_attempts WHERE kind = 'summary') = 1;
                 END;",
            )
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response:
                r#"[{"subject":"user","predicate":"likes","object":"Rust","confidence":0.9}]"#
                    .into(),
            calls: AtomicUsize::new(0),
        });
        let memory = Arc::new(MemoryService::new(db.clone(), None, 64));
        let worker = Arc::new(MemoryWorker::new_with_inference(
            memory.clone(),
            memory.memory_fact_store(),
            inference.clone(),
            4_000,
            64,
            256,
            0,
        ));

        worker
            .restore_pending_outbox(&CancellationToken::new())
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let attempts: i64 = db
                    .conn()
                    .query_row(
                        "SELECT COUNT(*) FROM marker_ack_attempts WHERE kind = 'summary'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                if attempts >= 2
                    && db.pending_summary_extractions().unwrap().is_empty()
                    && worker
                        .pending_summary_outbox_value_for_test(episode_id)
                        .is_none()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the summary job should retry acknowledgement after inference commit");

        assert_eq!(inference.calls.load(Ordering::Relaxed), 1);
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].mention_count, 1);
        worker.shutdown();
    }

    #[tokio::test]
    async fn cancelling_worker_during_inference_leaves_fact_marker_for_restore() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("outbox cancellation").unwrap();
        db.add_message(&session.id, "user", "I prefer Rust.", Some("text"), None)
            .unwrap();
        db.enqueue_fact_extraction(&session.id, true, 1).unwrap();
        let inference = Arc::new(BlockingMemoryInference {
            started: Notify::new(),
        });
        let memory = Arc::new(MemoryService::new(db.clone(), None, 64));
        let worker = Arc::new(MemoryWorker::new_with_inference(
            memory.clone(),
            memory.memory_fact_store(),
            inference.clone(),
            4_000,
            64,
            256,
            0,
        ));

        worker
            .restore_pending_outbox(&CancellationToken::new())
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            inference.started.notified(),
        )
        .await
        .expect("worker should enter inference before cancellation");
        worker.shutdown();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert_eq!(
            db.pending_fact_extractions().unwrap(),
            vec![(session.id.clone(), true, 1)]
        );
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap(),
            None,
            "cancellation during inference must leave the message cursor behind"
        );
    }

    #[tokio::test]
    async fn infer_facts_advances_cursor_once() {
        let db = temp_db();
        let session = db.create_session("t1").unwrap();
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
    async fn infer_facts_uses_injected_memory_inference_port() {
        let db = temp_db();
        let session = db.create_session("injected inference").unwrap();
        db.add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response: r#"[{"subject":"user","predicate":"likes","object":"Rust","confidence":0.9,"durability":0.8,"message_index":1}]"#.into(),
            calls: AtomicUsize::new(0),
        });
        let memory = Arc::new(MemoryService::new(db.clone(), None, 64));
        let worker = MemoryWorker::new_with_inference(
            memory.clone(),
            memory.memory_fact_store(),
            inference.clone(),
            4_000,
            64,
            256,
            0,
        );

        assert!(worker.infer_facts(&session.id).await);

        let facts = db.get_facts("user").unwrap();
        assert!(
            facts
                .iter()
                .any(|fact| { fact.predicate == "likes" && fact.object == "Rust" })
        );
        assert_eq!(inference.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn ordinary_fact_writes_roll_back_when_cursor_commit_fails() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("ordinary atomic extraction").unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.7, &[])
            .unwrap();
        let message = db
            .add_message(&session.id, "user", "I prefer Rust.", Some("text"), None)
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response:
                r#"[{"subject":"user","predicate":"likes","object":"Rust","confidence":0.9}]"#
                    .into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db.clone(), inference.clone());
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER fail_ordinary_cursor
                 BEFORE INSERT ON kv_store
                 WHEN NEW.key = 'fact_extraction.{}'
                 BEGIN SELECT RAISE(ABORT, 'injected ordinary cursor failure'); END;",
                session.id
            ))
            .unwrap();

        assert!(!worker.infer_facts(&session.id).await);
        assert_eq!(db.get_facts("user").unwrap()[0].mention_count, 0);
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap(),
            None
        );

        db.conn()
            .execute_batch("DROP TRIGGER fail_ordinary_cursor")
            .unwrap();
        assert!(worker.infer_facts(&session.id).await);
        assert_eq!(
            db.get_kv(&format!("fact_extraction.{}", session.id))
                .unwrap()
                .as_deref(),
            Some(message.id.as_str())
        );
        let facts = db.get_facts("user").unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].mention_count, 1);
        assert_eq!(inference.calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn prepared_fact_batch_keeps_agent_confidence_policy_and_source_reference() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        db.insert_fact("user", "likes", "Rust", "inferred", 0.65, &[])
            .unwrap();
        db.insert_fact(
            "user",
            "project_path",
            "D:/old-project",
            "inferred",
            0.8,
            &[],
        )
        .unwrap();
        let worker = MemoryWorker::new(db.clone(), mock_router("[]"), 4_000, 64, 40, 256, 0);

        let wrote = worker
            .persist_fact_batch(vec![
                (
                    "user".into(),
                    "likes".into(),
                    "Rust".into(),
                    0.4,
                    vec!["Preference".into()],
                    Some(FactSourceRef {
                        message_id: "msg-reconfirmed-rust".into(),
                        snippet: "I still use Rust.".into(),
                    }),
                    0.8,
                ),
                (
                    "user".into(),
                    "likes".into(),
                    "Coffee".into(),
                    0.549,
                    vec![],
                    None,
                    0.6,
                ),
                (
                    "user".into(),
                    "likes".into(),
                    "SQLite".into(),
                    0.55,
                    vec![],
                    None,
                    0.6,
                ),
                (
                    "user".into(),
                    "project_path".into(),
                    "D:/new-project".into(),
                    0.5,
                    vec![],
                    None,
                    0.6,
                ),
            ])
            .await
            .unwrap();

        assert!(wrote);
        let facts = db.get_facts("user").unwrap();
        let rust = facts.iter().find(|fact| fact.object == "Rust").unwrap();
        assert_eq!(
            rust.mention_count, 1,
            "existing facts bypass the new-fact floor"
        );
        assert!(rust.confidence >= 0.65);
        assert_eq!(rust.tags, ["preference"]);
        assert_eq!(
            rust.source_ref.as_ref().unwrap().message_id,
            "msg-reconfirmed-rust"
        );
        assert!(facts.iter().all(|fact| fact.object != "Coffee"));
        assert!(facts.iter().any(|fact| fact.object == "SQLite"));
        assert!(
            facts
                .iter()
                .any(|fact| fact.predicate == "project_path" && fact.object == "D:/new-project"),
            "a low-confidence update to a stored single-valued pair remains eligible"
        );
        assert!(
            facts
                .iter()
                .find(|fact| fact.object == "D:/old-project")
                .unwrap()
                .confidence
                < 0.8
        );

        assert!(!worker.persist_fact_batch(Vec::new()).await.unwrap());
    }

    #[tokio::test]
    async fn summary_extraction_skips_an_episode_with_its_completion_marker() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("summary completion duplicate").unwrap();
        db.set_kv(
            &format!(
                "fact_extraction_episode_done.{}.msg-summary-already-processed",
                session.id
            ),
            &session.id,
        )
        .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response: r#"[{"subject":"user","predicate":"likes","object":"Rust"}]"#.into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db, inference.clone());

        let outcome = worker
            .infer_facts_from_summary(
                &session.id,
                "msg-summary-already-processed",
                "The user prefers Rust for personal projects and tooling.",
            )
            .await;

        assert_eq!(outcome, SummaryExtractOutcome::Done);
        assert_eq!(inference.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn summary_facts_and_completion_marker_commit_atomically() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("summary persistence retry").unwrap();
        db.insert_fact("user", "likes", "Rust", "inferred", 0.7, &[])
            .unwrap();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER fail_summary_completion_marker
                 BEFORE INSERT ON kv_store
                 WHEN NEW.key LIKE 'fact_extraction_episode_done.%'
                 BEGIN SELECT RAISE(ABORT, 'injected summary completion marker failure'); END;",
            )
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response:
                r#"[{"subject":"user","predicate":"likes","object":"Rust","confidence":0.9}]"#
                    .into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db.clone(), inference.clone());
        let episode_id = "msg-summary-persist-after-success";
        let summary = "The user prefers Rust for personal projects and tooling.";

        assert!(matches!(
            worker
                .infer_facts_from_summary(&session.id, episode_id, summary)
                .await,
            SummaryExtractOutcome::Retryable { .. }
        ));
        assert_eq!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.{}",
                session.id, episode_id
            ))
            .unwrap(),
            None,
            "a failed fact transaction must leave the episode retryable"
        );
        assert_eq!(db.get_facts("user").unwrap()[0].mention_count, 0);

        db.conn()
            .execute_batch("DROP TRIGGER fail_summary_completion_marker")
            .unwrap();
        assert_eq!(
            worker
                .infer_facts_from_summary(&session.id, episode_id, summary)
                .await,
            SummaryExtractOutcome::Done
        );
        assert_eq!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .as_deref(),
            Some(session.id.as_str())
        );
        assert!(
            db.get_facts("user")
                .unwrap()
                .iter()
                .any(|fact| fact.predicate == "likes" && fact.object == "Rust")
        );
        assert_eq!(inference.calls.load(Ordering::Relaxed), 2);

        assert_eq!(
            worker
                .infer_facts_from_summary(&session.id, episode_id, summary)
                .await,
            SummaryExtractOutcome::Done
        );
        assert_eq!(inference.calls.load(Ordering::Relaxed), 2);
        assert_eq!(db.get_facts("user").unwrap()[0].mention_count, 1);
    }

    #[tokio::test]
    async fn older_summary_retry_after_newer_episode_does_not_reinforce_twice() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("summary out of order retry").unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response:
                r#"[{"subject":"user","predicate":"likes","object":"Rust","confidence":0.9}]"#
                    .into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference(db.clone(), inference.clone());
        let summary = "The user prefers Rust for personal projects and tooling.";

        assert_eq!(
            worker
                .infer_facts_from_summary(&session.id, "msg-newer-episode", summary)
                .await,
            SummaryExtractOutcome::Done
        );
        assert_eq!(
            worker
                .infer_facts_from_summary(&session.id, "msg-older-episode", summary)
                .await,
            SummaryExtractOutcome::Done
        );
        assert_eq!(db.get_facts("user").unwrap()[0].mention_count, 1);

        assert_eq!(
            worker
                .infer_facts_from_summary(&session.id, "msg-older-episode", summary)
                .await,
            SummaryExtractOutcome::Done
        );
        assert_eq!(db.get_facts("user").unwrap()[0].mention_count, 1);
        assert_eq!(inference.calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn failed_summary_throttle_stamp_does_not_block_a_later_retry() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("summary throttle stamp retry").unwrap();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER fail_summary_throttle_insert
                 BEFORE INSERT ON kv_store
                 WHEN NEW.key LIKE 'fact_extraction_last_run.%'
                 BEGIN SELECT RAISE(ABORT, 'injected summary throttle stamp failure'); END;",
            )
            .unwrap();
        let inference = Arc::new(FixedMemoryInference {
            fast_chat_configured: true,
            response: "[]".into(),
            calls: AtomicUsize::new(0),
        });
        let worker = make_engine_with_inference_and_interval(db.clone(), inference.clone(), 3_600);
        let episode_id = "msg-summary-throttle-retry";
        let summary = "The user prefers Rust for personal projects and tooling.";

        assert!(matches!(
            worker
                .infer_facts_from_summary(&session.id, episode_id, summary)
                .await,
            SummaryExtractOutcome::Retryable { .. }
        ));
        assert_eq!(inference.calls.load(Ordering::Relaxed), 0);
        assert_eq!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.{}",
                session.id, episode_id
            ))
            .unwrap(),
            None
        );

        db.conn()
            .execute_batch("DROP TRIGGER fail_summary_throttle_insert")
            .unwrap();
        assert_eq!(
            worker
                .infer_facts_from_summary(&session.id, episode_id, summary)
                .await,
            SummaryExtractOutcome::Done
        );
        assert_eq!(inference.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            db.get_kv(&format!(
                "fact_extraction_episode_done.{}.{}",
                session.id, episode_id
            ))
            .unwrap()
            .as_deref(),
            Some(session.id.as_str())
        );
    }

    #[tokio::test]
    async fn infer_facts_processes_only_new_messages() {
        let db = temp_db();
        let session = db.create_session("t1").unwrap();
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
        let session = db.create_session("t1").unwrap();
        let m1 = db
            .add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let router = mock_router("[]");
        let engine = MemoryWorker::new(db.clone(), router, 4_000, 64, 40, 256, 3_600);
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
        // Small model reply is not valid JSON -> extraction fails. The
        // failure is non-fatal, but the cursor stays behind so a later run can
        // retry instead of silently losing the message window.
        let db = temp_db();
        let session = db.create_session("t1").unwrap();
        let _m1 = db
            .add_message(&session.id, "user", "I like Rust.", Some("text"), None)
            .unwrap();
        let router = mock_router("not a json array");
        let engine = MemoryWorker::new(db.clone(), router, 4_000, 64, 40, 256, 0);
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

    #[tokio::test]
    async fn infer_facts_transcript_store_errors_are_nonfatal_and_keep_cursor() {
        for missing_table in ["messages", "session_steps"] {
            let db = Arc::new(Database::open_in_memory().unwrap());
            let session = db.create_session("missing extraction projection").unwrap();
            db.add_message(&session.id, "user", "I prefer Rust.", Some("text"), None)
                .unwrap();
            db.conn()
                .execute_batch(&format!("DROP TABLE {missing_table}"))
                .unwrap();
            let worker = MemoryWorker::new(db.clone(), mock_router("[]"), 4_000, 64, 40, 256, 0);

            assert!(
                !worker.infer_facts(&session.id).await,
                "missing {missing_table} must preserve the existing non-fatal extraction failure"
            );
            assert_eq!(
                db.get_kv(&format!("fact_extraction.{}", session.id))
                    .unwrap(),
                None,
                "missing {missing_table} must not advance the extraction cursor"
            );
        }
    }

    #[tokio::test]
    async fn enqueue_infer_durable_coalesces_bypass_flag() {
        let db = temp_db();
        let first = db.create_session("a").unwrap();
        let second = db.create_session("b").unwrap();
        let engine = Arc::new(make_engine(db.clone()));
        engine.suspend_outbox_worker_for_test();
        let cancellation = CancellationToken::new();
        engine
            .enqueue_infer_durable(&first.id, false, 1, &cancellation)
            .await
            .unwrap();
        engine
            .enqueue_infer_durable(&first.id, true, 2, &cancellation)
            .await
            .unwrap();
        engine
            .enqueue_infer_durable(&second.id, false, 1, &cancellation)
            .await
            .unwrap();
        let pending = engine.outbox.lock().unwrap();
        assert_eq!(
            pending.get(&first.id),
            Some(&FactExtractionJob {
                event_sequence: 2,
                bypass_throttle: true,
            })
        );
        assert_eq!(
            pending.get(&second.id),
            Some(&FactExtractionJob {
                event_sequence: 1,
                bypass_throttle: false,
            })
        );
        assert_eq!(pending.len(), 2);
        let durable = db.pending_fact_extractions().unwrap();
        assert!(durable.contains(&(first.id, true, 2)));
        assert!(durable.contains(&(second.id, false, 1)));
    }

    #[tokio::test]
    async fn enqueue_infer_durable_writes_marker_before_memory_projection() {
        let db = temp_db();
        let session = db.create_session("async").unwrap();
        let engine = Arc::new(make_engine(db.clone()));
        // Keep this test focused on enqueue ordering; a real outbox worker
        // would immediately consume and clear the marker after success.
        engine.suspend_outbox_worker_for_test();

        engine
            .enqueue_infer_durable(&session.id, true, 1, &CancellationToken::new())
            .await
            .unwrap();

        let pending = db
            .run_blocking(|db| db.pending_fact_extractions())
            .await
            .unwrap();
        let in_memory = engine
            .outbox
            .lock()
            .unwrap()
            .get(&session.id)
            .map(|job| (job.event_sequence, job.bypass_throttle));
        assert!(pending.contains(&(session.id.clone(), true, 1)));
        assert_eq!(in_memory, Some((1, true)));
    }
}
