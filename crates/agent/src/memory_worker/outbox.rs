use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use haven_memory::MemoryStore;
use haven_memory::repositories::kv_store::{
    FactExtractionMarker, MAX_MEMORY_OUTBOX_PAGE_SIZE, SummaryExtractionMarker,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use super::SummaryExtractOutcome;

const OUTBOX_RETRY_MAX_SECS: u64 = 30;
pub(super) const OUTBOX_PAGE_SIZE: usize = MAX_MEMORY_OUTBOX_PAGE_SIZE;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FactExtractionJob {
    event_sequence: i64,
    bypass_throttle: bool,
}

#[derive(Default)]
struct OutboxPassResult {
    made_progress: bool,
    next_due_at_ms: Option<i64>,
}

impl OutboxPassResult {
    fn note_due(&mut self, due_at_ms: i64) {
        self.next_due_at_ms = Some(
            self.next_due_at_ms
                .map_or(due_at_ms, |current| current.min(due_at_ms)),
        );
    }
}

pub(super) fn current_epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn retry_due_after_secs(wait_secs: u64) -> i64 {
    let wait_ms = wait_secs.saturating_mul(1000).min(i64::MAX as u64) as i64;
    current_epoch_millis().saturating_add(wait_ms)
}

#[cfg(test)]
impl FactExtractionJob {
    fn merge(&mut self, newer: Self) {
        self.event_sequence = self.event_sequence.max(newer.event_sequence);
        self.bypass_throttle |= newer.bypass_throttle;
    }
}

#[async_trait::async_trait]
pub(super) trait MemoryExtractionHandler: Send + Sync {
    async fn infer_session(&self, session_id: &str) -> bool;

    async fn infer_session_on_pause(&self, session_id: &str) -> bool;

    async fn infer_facts_from_summary(
        &self,
        session_id: &str,
        episode_id: &str,
        summary: &str,
    ) -> SummaryExtractOutcome;
}

pub(super) struct MemoryOutbox {
    memory_store: MemoryStore,
    #[cfg(test)]
    outbox: Mutex<std::collections::HashMap<String, FactExtractionJob>>,
    #[cfg(test)]
    summary_outbox: Mutex<std::collections::HashMap<String, String>>,
    outbox_notify: Notify,
    /// Serializes worker startup with shutdown to prevent restart past the boundary.
    outbox_lifecycle: Mutex<()>,
    /// Shared root cancellation token; prompt prefetch and scanner lifecycle linearize together.
    shutdown_token: CancellationToken,
    outbox_worker_started: AtomicBool,
}

impl MemoryOutbox {
    pub(super) fn new(memory_store: MemoryStore, shutdown_token: CancellationToken) -> Self {
        Self {
            memory_store,
            #[cfg(test)]
            outbox: Mutex::new(std::collections::HashMap::new()),
            #[cfg(test)]
            summary_outbox: Mutex::new(std::collections::HashMap::new()),
            outbox_notify: Notify::new(),
            outbox_lifecycle: Mutex::new(()),
            shutdown_token,
            outbox_worker_started: AtomicBool::new(false),
        }
    }

    /// Durably enqueue extraction before waking the bounded page scanner.
    /// Persistence failures are returned and do not publish a wake-up.
    pub(super) async fn enqueue_infer_durable(
        self: &Arc<Self>,
        session_id: &str,
        bypass_throttle: bool,
        event_sequence: i64,
        cancellation: &CancellationToken,
        handler: Arc<dyn MemoryExtractionHandler>,
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
            event_sequence,
            bypass_throttle,
            handler,
        );
        Ok(())
    }

    /// Start the shared durable-marker scanner. This no longer hydrates an
    /// in-memory queue; the worker reads at most one bounded page per class.
    pub(super) async fn start_outbox_worker(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
        handler: Arc<dyn MemoryExtractionHandler>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.shutdown_token.is_cancelled(),
            "memory worker is shut down"
        );
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "fact extraction outbox restore cancelled"
        );
        #[cfg(test)]
        self.seed_test_outbox_projection(cancellation).await?;
        self.ensure_outbox_worker(handler);
        self.outbox_notify.notify_one();
        Ok(())
    }

    /// Stop background work owned by this worker. Pending extraction markers
    /// are intentionally left in durable storage for the next process start.
    pub(super) fn shutdown(&self) {
        {
            let _lifecycle = self
                .outbox_lifecycle
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.shutdown_token.cancel();
        }
        self.outbox_notify.notify_waiters();
    }

    #[cfg(test)]
    pub(super) fn suspend_outbox_worker_for_test(&self) {
        self.outbox_worker_started.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn pending_outbox_value_for_test(&self, session_id: &str) -> Option<(i64, bool)> {
        self.outbox
            .lock()
            .ok()?
            .get(session_id)
            .map(|job| (job.event_sequence, job.bypass_throttle))
    }

    #[cfg(test)]
    pub(super) fn pending_summary_outbox_value_for_test(&self, episode_id: &str) -> Option<String> {
        self.summary_outbox.lock().ok()?.get(episode_id).cloned()
    }

    #[cfg(test)]
    pub(super) fn worker_started_for_test(&self) -> bool {
        self.outbox_worker_started.load(Ordering::Acquire)
    }

    fn enqueue_memory(
        self: &Arc<Self>,
        session_id: String,
        event_sequence: i64,
        bypass_throttle: bool,
        handler: Arc<dyn MemoryExtractionHandler>,
    ) {
        #[cfg(test)]
        if let Ok(mut pending) = self.outbox.lock() {
            let job = FactExtractionJob {
                event_sequence,
                bypass_throttle,
            };
            pending
                .entry(session_id.clone())
                .and_modify(|existing| existing.merge(job))
                .or_insert(job);
        }
        #[cfg(not(test))]
        let _ = (session_id, event_sequence, bypass_throttle);
        self.ensure_outbox_worker(handler);
        self.outbox_notify.notify_one();
    }

    fn enqueue_summary_memory(
        self: &Arc<Self>,
        session_id: String,
        episode_id: String,
        handler: Arc<dyn MemoryExtractionHandler>,
    ) {
        #[cfg(test)]
        if let Ok(mut pending) = self.summary_outbox.lock() {
            pending.insert(episode_id.clone(), session_id);
        }
        #[cfg(not(test))]
        let _ = (session_id, episode_id);
        self.ensure_outbox_worker(handler);
        self.outbox_notify.notify_one();
    }

    /// Wake the live projection after a producer has atomically persisted the
    /// episode and its durable summary-extraction marker.
    pub(super) fn wake_summary_extract(
        self: &Arc<Self>,
        session_id: &str,
        episode_id: &str,
        handler: Arc<dyn MemoryExtractionHandler>,
    ) {
        self.enqueue_summary_memory(session_id.to_owned(), episode_id.to_owned(), handler);
    }

    #[cfg(test)]
    async fn seed_test_outbox_projection(
        &self,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        if let Some(high_water) = self
            .memory_store
            .fact_extraction_high_water_cancellable(cancellation)
            .await?
        {
            let page = self
                .memory_store
                .pending_fact_extractions_page_cancellable(
                    None,
                    high_water,
                    OUTBOX_PAGE_SIZE,
                    cancellation,
                )
                .await?;
            for marker in page {
                if let Ok(state) = marker.state {
                    self.outbox
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .entry(marker.session_id)
                        .and_modify(|existing| {
                            existing.merge(FactExtractionJob {
                                event_sequence: state.event_sequence,
                                bypass_throttle: state.bypass_throttle,
                            })
                        })
                        .or_insert(FactExtractionJob {
                            event_sequence: state.event_sequence,
                            bypass_throttle: state.bypass_throttle,
                        });
                }
            }
        }
        if let Some(high_water) = self
            .memory_store
            .summary_extraction_high_water_cancellable(cancellation)
            .await?
        {
            let page = self
                .memory_store
                .pending_summary_extractions_page_cancellable(
                    None,
                    high_water,
                    OUTBOX_PAGE_SIZE,
                    cancellation,
                )
                .await?;
            for marker in page {
                self.summary_outbox
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(marker.episode_id, marker.session_id);
            }
        }
        Ok(())
    }

    async fn scan_outbox_pass(
        &self,
        cancellation: &CancellationToken,
        handler: &dyn MemoryExtractionHandler,
    ) -> anyhow::Result<OutboxPassResult> {
        let fact_high_water = self
            .memory_store
            .fact_extraction_high_water_cancellable(cancellation)
            .await?;
        let summary_high_water = self
            .memory_store
            .summary_extraction_high_water_cancellable(cancellation)
            .await?;
        let mut fact_after = None;
        let mut summary_after = None;
        let mut result = OutboxPassResult::default();

        loop {
            anyhow::ensure!(!cancellation.is_cancelled(), "memory outbox scan cancelled");
            let fact_page = match fact_high_water.as_ref() {
                Some(high_water) => {
                    self.memory_store
                        .pending_fact_extractions_page_cancellable(
                            fact_after.clone(),
                            high_water.clone(),
                            OUTBOX_PAGE_SIZE,
                            cancellation,
                        )
                        .await?
                }
                None => Vec::new(),
            };
            let summary_page = match summary_high_water.as_ref() {
                Some(high_water) => {
                    self.memory_store
                        .pending_summary_extractions_page_cancellable(
                            summary_after.clone(),
                            high_water.clone(),
                            OUTBOX_PAGE_SIZE,
                            cancellation,
                        )
                        .await?
                }
                None => Vec::new(),
            };
            if fact_page.is_empty() && summary_page.is_empty() {
                break;
            }
            if let Some(last) = fact_page.last() {
                fact_after = Some(last.key.clone());
            }
            if let Some(last) = summary_page.last() {
                summary_after = Some(last.key.clone());
            }

            let paired_len = fact_page.len().max(summary_page.len());
            for index in 0..paired_len {
                if cancellation.is_cancelled() {
                    anyhow::bail!("memory outbox scan cancelled");
                }
                if let Some(marker) = fact_page.get(index) {
                    match &marker.state {
                        Ok(state) if state.next_attempt_at_ms > current_epoch_millis() => {
                            result.note_due(state.next_attempt_at_ms);
                        }
                        Ok(state) => {
                            result.made_progress |= self
                                .process_fact_marker(
                                    marker.clone(),
                                    state.clone(),
                                    cancellation,
                                    handler,
                                )
                                .await?;
                        }
                        Err(error) => tracing::warn!(
                            key = %marker.key,
                            error = %error,
                            "skipping malformed fact extraction marker"
                        ),
                    }
                }
                if let Some(marker) = summary_page.get(index) {
                    match &marker.state {
                        Ok(state) if state.next_attempt_at_ms > current_epoch_millis() => {
                            result.note_due(state.next_attempt_at_ms);
                        }
                        Ok(state) => {
                            result.made_progress |= self
                                .process_summary_marker(
                                    marker.clone(),
                                    state.clone(),
                                    cancellation,
                                    handler,
                                )
                                .await?;
                        }
                        Err(error) => {
                            match self
                                .memory_store
                                .repair_summary_extraction_marker_if_current_cancellable(
                                    marker.key.clone(),
                                    marker.value.clone(),
                                    cancellation,
                                )
                                .await
                            {
                                Ok(true) => {
                                    result.made_progress = true;
                                    tracing::warn!(
                                        key = %marker.key,
                                        error = %error,
                                        "quarantined malformed summary marker and requeued its episode"
                                    );
                                }
                                Ok(false) => tracing::warn!(
                                    key = %marker.key,
                                    error = %error,
                                    "skipping malformed summary extraction marker"
                                ),
                                Err(repair_error) => tracing::warn!(
                                    key = %marker.key,
                                    error = %error,
                                    repair_error = %repair_error,
                                    "failed to repair malformed summary extraction marker"
                                ),
                            }
                        }
                    }
                }
            }
        }
        Ok(result)
    }

    async fn process_fact_marker(
        &self,
        marker: FactExtractionMarker,
        state: haven_memory::repositories::kv_store::FactExtractionMarkerState,
        cancellation: &CancellationToken,
        handler: &dyn MemoryExtractionHandler,
    ) -> anyhow::Result<bool> {
        let completed = tokio::select! {
            biased;
            _ = cancellation.cancelled() => anyhow::bail!("memory outbox cancelled"),
            completed = async {
                if state.bypass_throttle {
                    handler.infer_session_on_pause(&marker.session_id).await
                } else {
                    handler.infer_session(&marker.session_id).await
                }
            } => completed,
        };
        if cancellation.is_cancelled() {
            anyhow::bail!("memory outbox cancelled");
        }
        if completed {
            match self
                .memory_store
                .clear_fact_extraction_marker_if_current_cancellable(
                    marker.key.clone(),
                    marker.value.clone(),
                    cancellation,
                )
                .await
            {
                Ok(true) => {
                    #[cfg(test)]
                    self.outbox
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&marker.session_id);
                    return Ok(true);
                }
                Ok(false) => return Ok(true),
                Err(error) if cancellation.is_cancelled() => return Err(error),
                Err(error) => tracing::warn!(
                    session = %marker.session_id,
                    error = %error,
                    "fact extraction durable acknowledgement failed"
                ),
            }
        }
        let mut attempt = state.attempt;
        let wait_secs = next_outbox_retry_secs(&mut attempt, 0);
        let due = retry_due_after_secs(wait_secs);
        self.memory_store
            .update_fact_extraction_retry_if_current_cancellable(
                marker.key,
                marker.value,
                attempt,
                due,
                cancellation,
            )
            .await?;
        Ok(true)
    }

    async fn process_summary_marker(
        &self,
        marker: SummaryExtractionMarker,
        state: haven_memory::repositories::kv_store::SummaryExtractionMarkerState,
        cancellation: &CancellationToken,
        handler: &dyn MemoryExtractionHandler,
    ) -> anyhow::Result<bool> {
        let summary = match self
            .memory_store
            .episode_text_cancellable(&marker.episode_id, cancellation)
            .await
        {
            Ok(Some(summary)) => summary,
            Ok(None) => {
                let acknowledged = self
                    .memory_store
                    .clear_summary_extraction_if_current_cancellable(
                        marker.key.clone(),
                        marker.value.clone(),
                        cancellation,
                    )
                    .await?;
                if acknowledged {
                    #[cfg(test)]
                    self.summary_outbox
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&marker.episode_id);
                }
                return Ok(true);
            }
            Err(error) if cancellation.is_cancelled() => return Err(error),
            Err(error) => {
                tracing::warn!(
                    session = %marker.session_id,
                    episode = %marker.episode_id,
                    error = %error,
                    "summary extraction episode read failed"
                );
                self.defer_summary_marker(&marker, &state, 0, cancellation)
                    .await?;
                return Ok(true);
            }
        };
        if cancellation.is_cancelled() {
            anyhow::bail!("memory outbox cancelled");
        }

        let outcome = tokio::select! {
            biased;
            _ = cancellation.cancelled() => anyhow::bail!("memory outbox cancelled"),
            outcome = handler.infer_facts_from_summary(
                &marker.session_id,
                &marker.episode_id,
                &summary,
            ) => outcome,
        };
        if cancellation.is_cancelled() {
            anyhow::bail!("memory outbox cancelled");
        }
        match outcome {
            SummaryExtractOutcome::Done => {
                match self
                    .memory_store
                    .clear_summary_extraction_if_current_cancellable(
                        marker.key.clone(),
                        marker.value.clone(),
                        cancellation,
                    )
                    .await
                {
                    Ok(true) => {
                        #[cfg(test)]
                        self.summary_outbox
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&marker.episode_id);
                    }
                    Ok(false) => {}
                    Err(error) if cancellation.is_cancelled() => return Err(error),
                    Err(error) => {
                        tracing::warn!(
                            session = %marker.session_id,
                            episode = %marker.episode_id,
                            error = %error,
                            "summary extraction durable acknowledgement failed"
                        );
                        self.defer_summary_marker(&marker, &state, 0, cancellation)
                            .await?;
                    }
                }
            }
            SummaryExtractOutcome::Throttled { wait_secs }
            | SummaryExtractOutcome::Retryable { wait_secs } => {
                self.defer_summary_marker(&marker, &state, wait_secs, cancellation)
                    .await?;
            }
        }
        Ok(true)
    }

    async fn defer_summary_marker(
        &self,
        marker: &SummaryExtractionMarker,
        state: &haven_memory::repositories::kv_store::SummaryExtractionMarkerState,
        requested_wait_secs: u64,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        let mut attempt = state.attempt;
        let wait_secs = next_outbox_retry_secs(&mut attempt, requested_wait_secs);
        let due = retry_due_after_secs(wait_secs);
        self.memory_store
            .update_summary_extraction_retry_if_current_cancellable(
                marker.key.clone(),
                marker.value.clone(),
                attempt,
                due,
                cancellation,
            )
            .await?;
        tracing::debug!(
            session = %marker.session_id,
            episode = %marker.episode_id,
            wait_secs,
            "summary extraction deferred with durable retry deadline"
        );
        Ok(())
    }

    pub(super) fn ensure_outbox_worker(
        self: &Arc<Self>,
        handler: Arc<dyn MemoryExtractionHandler>,
    ) {
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
            loop {
                if cancellation.is_cancelled() {
                    return;
                }
                let notified = engine.outbox_notify.notified();
                tokio::pin!(notified);
                match engine
                    .scan_outbox_pass(&cancellation, handler.as_ref())
                    .await
                {
                    Ok(result) if result.made_progress => continue,
                    Ok(result) => {
                        let sleep_for = result.next_due_at_ms.map(|due| {
                            Duration::from_millis(
                                due.saturating_sub(current_epoch_millis()).max(0) as u64
                            )
                        });
                        tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => return,
                            _ = &mut notified => {}
                            _ = async {
                                if let Some(duration) = sleep_for {
                                    tokio::time::sleep(duration).await;
                                } else {
                                    std::future::pending::<()>().await;
                                }
                            } => {}
                        }
                    }
                    Err(error) if cancellation.is_cancelled() => {
                        tracing::debug!(error = %error, "memory outbox scan stopped during cancellation");
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "memory outbox page scan failed");
                        tokio::select! {
                            biased;
                            _ = cancellation.cancelled() => return,
                            _ = &mut notified => {}
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                    }
                }
            }
        });
    }

    #[cfg(test)]
    pub(super) fn projection_len_for_test(&self) -> usize {
        self.outbox.lock().map_or(0, |pending| pending.len())
    }
}

pub(super) fn next_outbox_retry_secs(attempt: &mut u32, requested_wait_secs: u64) -> u64 {
    let completed_attempts = attempt.saturating_add(1);
    *attempt = completed_attempts;
    let policy = haven_common::retry::RecoveryPolicy::new(
        None,
        None,
        haven_common::retry::BackoffPolicy::new(
            Duration::from_secs(1),
            2,
            Duration::from_secs(OUTBOX_RETRY_MAX_SECS),
        ),
    );
    match policy.decide(
        completed_attempts,
        haven_common::retry::RecoverySignal::Retryable {
            retry_after: Some(Duration::from_secs(requested_wait_secs)),
        },
        Instant::now(),
        0,
    ) {
        haven_common::retry::RecoveryDecision::Retry { delay, .. } => delay.as_secs(),
        haven_common::retry::RecoveryDecision::Stop { .. } => {
            requested_wait_secs.max(OUTBOX_RETRY_MAX_SECS)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::next_outbox_retry_secs;

    #[test]
    fn retry_backoff_is_bounded_but_honors_throttle_wait() {
        let mut attempt = 0;
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 1);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 2);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 4);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 8);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 16);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 0), 30);
        assert_eq!(next_outbox_retry_secs(&mut attempt, 900), 900);
    }
}
