//! Ordered processing and bounded live/replay recovery for memory events.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use haven_memory::{
    CURRENT_EVENT_VERSION, MAX_SESSION_EVENT_REPLAY_PAGE_SIZE, MEMORY_TRIGGER_EVENT_TYPE,
    SessionEvent, SessionStore,
};
use tokio::sync::broadcast;
use tokio::time::{Instant, sleep, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::memory_trigger::MemoryTriggerPayload;
use crate::memory_worker::MemoryWorker;

const INITIAL_RETRY_BACKOFF: Duration = Duration::from_millis(250);
const MAX_RETRY_BACKOFF: Duration = Duration::from_secs(30);
const MEMORY_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Result of processing one committed event for a target session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryEventProcessOutcome {
    /// The event belongs to another session and had no effect.
    IgnoredOtherSession,
    /// The event was already covered by the durable memory event cursor.
    AlreadyProcessed,
    /// The event was handled and its sequence was checkpointed.
    Checkpointed { enqueued: bool },
}

/// Processes committed memory trigger events using the existing extraction
/// outbox. Transcript contents are never read from event payloads.
pub struct MemoryRuntime {
    session_store: SessionStore,
    memory_worker: Arc<MemoryWorker>,
}

impl MemoryRuntime {
    pub fn new(session_store: SessionStore, memory_worker: Arc<MemoryWorker>) -> Self {
        Self {
            session_store,
            memory_worker,
        }
    }

    /// Subscribe before taking the startup session snapshot, baseline only
    /// absent cursors for sessions in that snapshot, restore the durable fact
    /// outbox, and replay visible sessions before returning the live receiver.
    ///
    /// Initialization failures are logged and retried with cancellable
    /// backoff while keeping the original broadcast receiver and session
    /// snapshot. A present cursor at zero is deliberately not baselined.
    pub async fn prepare_start(
        &self,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<broadcast::Receiver<SessionEvent>> {
        let live = self.session_store.subscribe();
        let mut retry_backoff = INITIAL_RETRY_BACKOFF;
        let startup_session_ids = loop {
            if cancellation.is_cancelled() {
                anyhow::bail!("memory runtime startup cancelled");
            }
            match self
                .session_store
                .all_session_ids_cancellable(cancellation.clone())
                .await
            {
                Ok(session_ids) => break session_ids,
                Err(error) if cancellation.is_cancelled() => {
                    return Err(error).context("memory runtime startup cancelled");
                }
                Err(error) => {
                    tracing::warn!("memory runtime session snapshot failed: {}", error);
                    if !wait_for_retry(cancellation, retry_backoff).await {
                        anyhow::bail!("memory runtime startup cancelled");
                    }
                    retry_backoff = next_retry_backoff(retry_backoff);
                }
            }
        };

        loop {
            match self
                .baseline_missing_startup_cursors(&startup_session_ids, cancellation)
                .await
            {
                Ok(()) => break,
                Err(error) if cancellation.is_cancelled() => {
                    return Err(error).context("memory runtime startup cancelled");
                }
                Err(error) => {
                    tracing::warn!("memory runtime cursor baseline failed: {}", error);
                    if !wait_for_retry(cancellation, retry_backoff).await {
                        anyhow::bail!("memory runtime startup cancelled");
                    }
                    retry_backoff = next_retry_backoff(retry_backoff);
                }
            }
        }

        loop {
            match self
                .memory_worker
                .restore_pending_outbox(cancellation)
                .await
            {
                Ok(_) => break,
                Err(error) if cancellation.is_cancelled() => {
                    return Err(error).context("memory runtime startup cancelled");
                }
                Err(error) => {
                    tracing::warn!("memory runtime fact outbox restore failed: {}", error);
                    if !wait_for_retry(cancellation, retry_backoff).await {
                        anyhow::bail!("memory runtime startup cancelled");
                    }
                    retry_backoff = next_retry_backoff(retry_backoff);
                }
            }
        }

        loop {
            if cancellation.is_cancelled() {
                anyhow::bail!("memory runtime startup cancelled");
            }
            match self.recover_visible_sessions(cancellation).await {
                Ok(()) if cancellation.is_cancelled() => {
                    anyhow::bail!("memory runtime startup cancelled");
                }
                Ok(()) => return Ok(live),
                Err(error) if cancellation.is_cancelled() => {
                    return Err(error).context("memory runtime startup cancelled");
                }
                Err(error) => {
                    tracing::warn!("memory runtime startup replay failed: {}", error);
                    if !wait_for_retry(cancellation, retry_backoff).await {
                        anyhow::bail!("memory runtime startup cancelled");
                    }
                    retry_backoff = next_retry_backoff(retry_backoff);
                }
            }
        }
    }

    async fn baseline_missing_startup_cursors(
        &self,
        startup_session_ids: &[String],
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        for session_id in startup_session_ids {
            if self
                .session_store
                .memory_event_cursor_optional_cancellable(session_id, cancellation.clone())
                .await?
                .is_some()
            {
                continue;
            }
            let latest_sequence = self
                .session_store
                .latest_sequence_cancellable(session_id, cancellation.clone())
                .await?;
            self.session_store
                .initialize_memory_event_cursor_if_absent_cancellable(
                    session_id,
                    latest_sequence,
                    cancellation.clone(),
                )
                .await?;
        }
        Ok(())
    }

    /// Process a live event, filling any sequence gap from bounded durable
    /// replay before retrying the received event. Duplicate overlap is
    /// delegated to `process_event`'s durable cursor check.
    pub async fn process_live_event(
        &self,
        target_session_id: &str,
        event: &SessionEvent,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<MemoryEventProcessOutcome> {
        if event.session_id != target_session_id {
            return self
                .process_event(target_session_id, event, cancellation)
                .await;
        }
        let cursor = self
            .session_store
            .memory_event_cursor_cancellable(target_session_id, cancellation.clone())
            .await
            .context("read memory cursor before live event")?;
        if event.sequence > cursor.saturating_add(1) {
            self.recover_session_through(target_session_id, event.sequence - 1, cancellation)
                .await
                .context("recover memory event sequence gap")?;
        }
        self.process_event(target_session_id, event, cancellation)
            .await
    }

    /// Recover one session through its current durable high-water mark using
    /// pages no larger than `MAX_SESSION_EVENT_REPLAY_PAGE_SIZE`.
    pub async fn recover_session(
        &self,
        session_id: &str,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<usize> {
        let latest_sequence = self
            .session_store
            .latest_sequence_cancellable(session_id, cancellation.clone())
            .await
            .context("read latest memory recovery sequence")?;
        self.recover_session_through(session_id, latest_sequence, cancellation)
            .await
    }

    async fn recover_session_through(
        &self,
        session_id: &str,
        through_sequence: i64,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<usize> {
        anyhow::ensure!(!session_id.trim().is_empty(), "session id is required");
        anyhow::ensure!(
            through_sequence >= 0,
            "memory recovery sequence cannot be negative"
        );
        let mut cursor = self
            .session_store
            .memory_event_cursor_cancellable(session_id, cancellation.clone())
            .await?;
        let mut recovered = 0;

        while cursor < through_sequence {
            anyhow::ensure!(
                !cancellation.is_cancelled(),
                "memory event recovery cancelled"
            );
            let page = self
                .session_store
                .replay_page_cancellable(
                    session_id,
                    cursor,
                    MAX_SESSION_EVENT_REPLAY_PAGE_SIZE,
                    cancellation.clone(),
                )
                .await
                .context("read bounded memory event replay page")?;
            let mut advanced = false;
            for event in page.events {
                if event.sequence > through_sequence {
                    break;
                }
                self.process_event(session_id, &event, cancellation)
                    .await
                    .context("process replayed memory event")?;
                let next_cursor = self
                    .session_store
                    .memory_event_cursor_cancellable(session_id, cancellation.clone())
                    .await
                    .context("read replayed memory event cursor")?;
                if next_cursor > cursor {
                    cursor = next_cursor;
                    recovered += 1;
                    advanced = true;
                }
                if cursor >= through_sequence {
                    break;
                }
            }
            anyhow::ensure!(
                advanced,
                "memory event replay made no progress for session {} through sequence {}",
                session_id,
                through_sequence
            );
        }
        Ok(recovered)
    }

    async fn recover_visible_sessions(
        &self,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<()> {
        let session_ids = self
            .session_store
            .all_session_ids_cancellable(cancellation.clone())
            .await
            .context("list sessions for memory recovery")?;
        for session_id in session_ids {
            self.recover_session(&session_id, cancellation)
                .await
                .with_context(|| format!("recover memory events for session {session_id}"))?;
        }
        Ok(())
    }

    /// Run the live memory event consumer until cancellation or broadcast
    /// closure. A lost broadcast range is recovered from each visible
    /// session's durable event pages.
    pub async fn run_until_cancelled(&self, cancellation: &CancellationToken) {
        let live = match self.prepare_start(cancellation).await {
            Ok(live) => live,
            Err(_) if cancellation.is_cancelled() => return,
            Err(error) => {
                tracing::error!("memory runtime failed to prepare: {}", error);
                return;
            }
        };
        self.run_prepared(live, cancellation).await;
    }

    /// Own the periodic maintenance schedule while leaving task ownership and
    /// cancellation joining to the application runtime. Manual maintenance
    /// commands still use `AgentLayer::run_memory_maintenance` directly.
    pub async fn run_maintenance_until_cancelled(&self, cancellation: &CancellationToken) {
        let mut ticker = tokio::time::interval(MEMORY_MAINTENANCE_INTERVAL);
        loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return,
                _ = ticker.tick() => {
                    if let Err(error) = self.memory_worker.run_memory_maintenance().await {
                        tracing::warn!("periodic memory maintenance failed: {}", error);
                    }
                }
            }
        }
    }

    /// Run the live consumer using the receiver returned by `prepare_start`.
    /// Agent startup calls this only after the recovery preparation has
    /// completed, then opens the session dispatcher.
    pub(crate) async fn run_prepared(
        &self,
        mut live: broadcast::Receiver<SessionEvent>,
        cancellation: &CancellationToken,
    ) {
        let mut retry_backoff = INITIAL_RETRY_BACKOFF;
        let mut recovery_deadline = None;

        loop {
            let recovery_wait = async {
                match recovery_deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = cancellation.cancelled() => return,
                _ = recovery_wait => {
                    recovery_deadline = None;
                    match self.recover_visible_sessions(cancellation).await {
                        Ok(()) => retry_backoff = INITIAL_RETRY_BACKOFF,
                        Err(_error) if cancellation.is_cancelled() => return,
                        Err(error) => {
                            tracing::warn!("memory runtime recovery failed: {}", error);
                            recovery_deadline = Some(Instant::now() + retry_backoff);
                            retry_backoff = next_retry_backoff(retry_backoff);
                        }
                    }
                }
                received = live.recv() => match received {
                    Ok(event) => {
                        if let Err(error) = self
                            .process_live_event(&event.session_id, &event, cancellation)
                            .await
                        {
                            if cancellation.is_cancelled() {
                                return;
                            }
                            tracing::warn!(
                                "memory runtime event processing failed for session {} sequence {}: {}",
                                event.session_id,
                                event.sequence,
                                error
                            );
                            if recovery_deadline.is_none() {
                                recovery_deadline = Some(Instant::now() + retry_backoff);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            "memory runtime broadcast lagged; recovering after {} skipped events",
                            skipped
                        );
                        match self.recover_visible_sessions(cancellation).await {
                            Ok(()) => {
                                recovery_deadline = None;
                                retry_backoff = INITIAL_RETRY_BACKOFF;
                            }
                            Err(_error) if cancellation.is_cancelled() => return,
                            Err(error) => {
                                tracing::warn!("memory runtime lag recovery failed: {}", error);
                                recovery_deadline = Some(Instant::now() + retry_backoff);
                                retry_backoff = next_retry_backoff(retry_backoff);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }
    }

    /// Process one event belonging to `target_session_id`.
    ///
    /// Callers must serialize events per session. A gap is returned as an
    /// error so the caller can refill it from durable replay before retrying.
    /// SQLite cursor reads and writes run on the blocking pool and observe
    /// cancellation. A trigger's durable outbox write always completes before
    /// its event cursor checkpoint begins.
    pub async fn process_event(
        &self,
        target_session_id: &str,
        event: &SessionEvent,
        cancellation: &CancellationToken,
    ) -> anyhow::Result<MemoryEventProcessOutcome> {
        self.process_event_after_enqueue(target_session_id, event, cancellation, || {})
            .await
    }

    async fn process_event_after_enqueue<F>(
        &self,
        target_session_id: &str,
        event: &SessionEvent,
        cancellation: &CancellationToken,
        after_enqueue: F,
    ) -> anyhow::Result<MemoryEventProcessOutcome>
    where
        F: FnOnce(),
    {
        if event.session_id != target_session_id {
            return Ok(MemoryEventProcessOutcome::IgnoredOtherSession);
        }
        anyhow::ensure!(
            !target_session_id.trim().is_empty(),
            "target session id is required"
        );
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "memory event processing cancelled"
        );

        let cursor = self
            .session_store
            .memory_event_cursor_cancellable(target_session_id, cancellation.clone())
            .await
            .context("read memory event cursor")?;
        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "memory event processing cancelled after cursor read"
        );
        if event.sequence <= cursor {
            return Ok(MemoryEventProcessOutcome::AlreadyProcessed);
        }

        let expected_sequence = cursor
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("memory event sequence overflow"))?;
        anyhow::ensure!(
            event.sequence == expected_sequence,
            "memory event sequence gap for session {}: expected {}, received {}",
            target_session_id,
            expected_sequence,
            event.sequence
        );

        let enqueued = if event.event_type == MEMORY_TRIGGER_EVENT_TYPE {
            anyhow::ensure!(
                event.event_version == CURRENT_EVENT_VERSION,
                "unsupported memory_trigger event version {}",
                event.event_version
            );
            let payload: MemoryTriggerPayload =
                serde_json::from_str(&event.payload).context("invalid memory_trigger payload")?;
            payload
                .validate()
                .context("invalid memory_trigger fields")?;
            // These optional fields are validated at the wire boundary but
            // intentionally are not used to reconstruct transcript content.
            let _metadata = (payload.run_id, payload.step_number, payload.pause_reason);
            self.memory_worker
                .enqueue_infer_durable(target_session_id, payload.bypass_throttle, cancellation)
                .await
                .context("durably enqueue memory inference")?;
            after_enqueue();
            anyhow::ensure!(
                !cancellation.is_cancelled(),
                "memory event processing cancelled after durable enqueue"
            );
            true
        } else {
            false
        };

        anyhow::ensure!(
            !cancellation.is_cancelled(),
            "memory event processing cancelled before cursor checkpoint"
        );
        self.session_store
            .checkpoint_memory_event_cursor_cancellable(
                target_session_id,
                event.sequence,
                cancellation.clone(),
            )
            .await
            .context("checkpoint memory event cursor")?;

        Ok(MemoryEventProcessOutcome::Checkpointed { enqueued })
    }
}

async fn wait_for_retry(cancellation: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        _ = cancellation.cancelled() => false,
        _ = sleep(delay) => true,
    }
}

fn next_retry_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_RETRY_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory_inference::MemoryInferencePort;
    use crate::memory_service::MemoryService;
    use async_trait::async_trait;
    use haven_memory::{Database, SessionEvent, SessionStore};
    use std::sync::Arc;

    struct StubInference;

    #[async_trait]
    impl MemoryInferencePort for StubInference {
        async fn is_fast_chat_configured(&self) -> bool {
            true
        }

        async fn fast_chat(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> anyhow::Result<String> {
            Ok("[]".to_owned())
        }
    }

    fn fixture() -> (Arc<Database>, String, MemoryRuntime) {
        let db = Arc::new(Database::open_in_memory().expect("open in-memory database"));
        let session = db.create_session("memory runtime test").unwrap();
        let service = Arc::new(MemoryService::new(db.clone(), None, 16));
        let inference: Arc<dyn MemoryInferencePort> = Arc::new(StubInference);
        let worker = Arc::new(MemoryWorker::new_with_inference(
            service, inference, 4_000, 64, 256, 0,
        ));
        worker.suspend_outbox_worker_for_test();
        let runtime = MemoryRuntime::new(SessionStore::new(db.clone()), worker);
        (db, session.id, runtime)
    }

    fn event(session_id: &str, sequence: i64, event_type: &str, payload: String) -> SessionEvent {
        SessionEvent {
            session_id: session_id.to_owned(),
            sequence,
            event_type: event_type.to_owned(),
            event_version: CURRENT_EVENT_VERSION,
            payload,
            created_at: "2026-09-24T00:00:00Z".to_owned(),
            run_id: None,
            step_number: None,
        }
    }

    fn trigger(kind: &str, bypass: bool) -> String {
        serde_json::json!({
            "trigger_kind": kind,
            "bypass_throttle": bypass,
            "run_id": 7,
            "step_number": 3,
            "pause_reason": if kind == "pause" { Some("turn_end") } else { None },
        })
        .to_string()
    }

    async fn cursor(db: &Arc<Database>, session_id: &str) -> i64 {
        let session_id = session_id.to_owned();
        db.clone()
            .run_blocking(move |db| db.memory_event_cursor(&session_id))
            .await
            .unwrap()
    }

    async fn pending(db: &Arc<Database>) -> Vec<(String, bool)> {
        db.clone()
            .run_blocking(|db| db.pending_fact_extractions())
            .await
            .unwrap()
    }

    fn cancellation() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn maintenance_schedule_stops_before_first_tick_when_cancelled() {
        let (_, _, runtime) = fixture();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        runtime.run_maintenance_until_cancelled(&cancellation).await;
    }

    #[tokio::test]
    async fn startup_baselines_old_sessions_and_processes_a_new_session_from_one() {
        let (db, old_session_id, runtime) = fixture();
        runtime
            .session_store
            .append(&old_session_id, "usage_recorded", "{}", None, None)
            .unwrap();
        runtime
            .session_store
            .append(
                &old_session_id,
                MEMORY_TRIGGER_EVENT_TYPE,
                &trigger("pause", true),
                None,
                None,
            )
            .unwrap();

        let mut live = runtime.prepare_start(&cancellation()).await.unwrap();
        assert_eq!(cursor(&db, &old_session_id).await, 2);
        assert!(pending(&db).await.is_empty());

        let new_session = db.create_session("created after runtime startup").unwrap();
        let event = runtime
            .session_store
            .append(
                &new_session.id,
                MEMORY_TRIGGER_EVENT_TYPE,
                &trigger("step_interval", false),
                None,
                None,
            )
            .unwrap();
        let received = live.recv().await.unwrap();
        assert_eq!(received, event);
        assert_eq!(
            runtime
                .process_live_event(&received.session_id, &received, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::Checkpointed { enqueued: true }
        );
        assert_eq!(cursor(&db, &new_session.id).await, 1);
        assert_eq!(pending(&db).await, vec![(new_session.id, false)]);
    }

    #[tokio::test]
    async fn startup_replays_from_an_existing_zero_cursor() {
        let (db, session_id, runtime) = fixture();
        db.checkpoint_memory_event_cursor(&session_id, 0).unwrap();
        let old_event = runtime
            .session_store
            .append(
                &session_id,
                MEMORY_TRIGGER_EVENT_TYPE,
                &trigger("step_interval", false),
                None,
                None,
            )
            .unwrap();

        let _live = runtime.prepare_start(&cancellation()).await.unwrap();
        assert_eq!(cursor(&db, &session_id).await, old_event.sequence);
        assert_eq!(
            runtime
                .recover_session(&session_id, &cancellation())
                .await
                .unwrap(),
            0
        );
        assert_eq!(cursor(&db, &session_id).await, old_event.sequence);
        assert_eq!(pending(&db).await, vec![(session_id, false)]);
    }

    #[tokio::test]
    async fn prepare_start_replays_trigger_after_existing_cursor() {
        let (db, session_id, runtime) = fixture();
        let before_cursor = runtime
            .session_store
            .append(&session_id, "usage_recorded", "{}", None, None)
            .unwrap();
        db.checkpoint_memory_event_cursor(&session_id, before_cursor.sequence)
            .unwrap();
        let trigger_event = runtime
            .session_store
            .append(
                &session_id,
                MEMORY_TRIGGER_EVENT_TYPE,
                &trigger("step_interval", false),
                None,
                None,
            )
            .unwrap();

        let _live = runtime.prepare_start(&cancellation()).await.unwrap();

        assert_eq!(cursor(&db, &session_id).await, trigger_event.sequence);
        assert_eq!(pending(&db).await, vec![(session_id.clone(), false)]);
        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            Some(false)
        );
    }

    #[tokio::test]
    async fn prepare_start_calls_durable_pending_outbox_restore() {
        let (db, session_id, runtime) = fixture();
        db.enqueue_fact_extraction(&session_id, true).unwrap();

        let _live = runtime.prepare_start(&cancellation()).await.unwrap();

        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            Some(true)
        );
        assert_eq!(pending(&db).await, vec![(session_id, true)]);
    }

    #[tokio::test]
    async fn live_gap_recovery_pages_before_retrying_current_event() {
        let (db, session_id, runtime) = fixture();
        let mut last_event = None;
        for sequence in 1..=600 {
            let (event_type, payload) = if sequence == 300 {
                (MEMORY_TRIGGER_EVENT_TYPE, trigger("pause", true))
            } else {
                ("usage_recorded", format!(r#"{{"sequence":{sequence}}}"#))
            };
            last_event = Some(
                runtime
                    .session_store
                    .append(&session_id, event_type, &payload, None, None)
                    .unwrap(),
            );
        }
        let current_event = last_event.expect("at least one stored event");

        assert_eq!(
            runtime
                .process_live_event(&session_id, &current_event, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::Checkpointed { enqueued: false }
        );
        assert_eq!(cursor(&db, &session_id).await, 600);
        assert_eq!(pending(&db).await, vec![(session_id, true)]);
    }

    #[tokio::test]
    async fn replay_and_live_overlap_is_idempotently_skipped() {
        let (db, session_id, runtime) = fixture();
        let trigger_event = runtime
            .session_store
            .append(
                &session_id,
                MEMORY_TRIGGER_EVENT_TYPE,
                &trigger("step_interval", false),
                None,
                None,
            )
            .unwrap();
        runtime
            .session_store
            .append(&session_id, "usage_recorded", "{}", None, None)
            .unwrap();

        assert_eq!(
            runtime
                .recover_session(&session_id, &cancellation())
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            runtime
                .process_live_event(&session_id, &trigger_event, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::AlreadyProcessed
        );
        assert_eq!(cursor(&db, &session_id).await, 2);
        assert_eq!(pending(&db).await, vec![(session_id, false)]);
    }

    #[tokio::test]
    async fn non_trigger_advances_only_its_session_cursor() {
        let (db, session_id, runtime) = fixture();
        let event = event(
            &session_id,
            1,
            "usage_recorded",
            "not a trigger payload".into(),
        );

        assert_eq!(
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::Checkpointed { enqueued: false }
        );
        assert_eq!(cursor(&db, &session_id).await, 1);
        assert!(pending(&db).await.is_empty());
    }

    #[tokio::test]
    async fn valid_trigger_durably_enqueues_before_advancing_cursor() {
        let (db, session_id, runtime) = fixture();
        let event = event(
            &session_id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("step_interval", false),
        );

        assert_eq!(
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::Checkpointed { enqueued: true }
        );
        assert_eq!(pending(&db).await, vec![(session_id.clone(), false)]);
        assert_eq!(cursor(&db, &session_id).await, 1);
        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            Some(false)
        );
    }

    #[tokio::test]
    async fn duplicate_event_is_skipped_without_a_second_enqueue() {
        let (db, session_id, runtime) = fixture();
        let event = event(
            &session_id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("step_interval", false),
        );
        runtime
            .process_event(&session_id, &event, &cancellation())
            .await
            .unwrap();

        let trigger_sql = format!(
            "CREATE TRIGGER reject_duplicate_memory_enqueue BEFORE INSERT ON kv_store
             WHEN NEW.key = 'fact_extraction_pending.{session_id}'
             BEGIN SELECT RAISE(ABORT, 'duplicate enqueue'); END;"
        );
        db.clone()
            .run_blocking(move |db| {
                db.conn().execute_batch(&trigger_sql)?;
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::AlreadyProcessed
        );
        assert_eq!(pending(&db).await, vec![(session_id.clone(), false)]);
        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            Some(false)
        );
    }

    #[tokio::test]
    async fn bypass_trigger_upgrades_existing_ordinary_job() {
        let (db, session_id, runtime) = fixture();
        for (sequence, kind, bypass) in [(1, "step_interval", false), (2, "pause", true)] {
            let event = event(
                &session_id,
                sequence,
                MEMORY_TRIGGER_EVENT_TYPE,
                trigger(kind, bypass),
            );
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .unwrap();
        }

        assert_eq!(pending(&db).await, vec![(session_id.clone(), true)]);
        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            Some(true)
        );
        assert_eq!(cursor(&db, &session_id).await, 2);
    }

    #[tokio::test]
    async fn malformed_trigger_does_not_advance_cursor_or_enqueue() {
        let (db, session_id, runtime) = fixture();
        let event = event(
            &session_id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            r#"{"trigger_kind":"step_interval"}"#.into(),
        );

        assert!(
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .is_err()
        );
        assert_eq!(cursor(&db, &session_id).await, 0);
        assert!(pending(&db).await.is_empty());
        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            None
        );
    }

    #[tokio::test]
    async fn durable_outbox_failure_does_not_advance_cursor_or_enqueue_memory() {
        let (db, session_id, runtime) = fixture();
        let trigger_sql = format!(
            "CREATE TRIGGER reject_memory_enqueue BEFORE INSERT ON kv_store
             WHEN NEW.key = 'fact_extraction_pending.{session_id}'
             BEGIN SELECT RAISE(ABORT, 'outbox unavailable'); END;"
        );
        db.clone()
            .run_blocking(move |db| {
                db.conn().execute_batch(&trigger_sql)?;
                Ok(())
            })
            .await
            .unwrap();
        let event = event(
            &session_id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("step_interval", false),
        );

        assert!(
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .is_err()
        );
        assert_eq!(cursor(&db, &session_id).await, 0);
        assert!(pending(&db).await.is_empty());
        assert_eq!(
            runtime
                .memory_worker
                .pending_outbox_value_for_test(&session_id),
            None
        );
    }

    #[tokio::test]
    async fn checkpoint_failure_leaves_durable_outbox_for_replay() {
        let (db, session_id, runtime) = fixture();
        let trigger_sql = format!(
            "CREATE TRIGGER reject_memory_checkpoint BEFORE INSERT ON kv_store
             WHEN NEW.key = 'memory_event_cursor.{session_id}'
             BEGIN SELECT RAISE(ABORT, 'checkpoint unavailable'); END;"
        );
        db.clone()
            .run_blocking(move |db| {
                db.conn().execute_batch(&trigger_sql)?;
                Ok(())
            })
            .await
            .unwrap();
        let event = event(
            &session_id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("step_interval", false),
        );

        assert!(
            runtime
                .process_event(&session_id, &event, &cancellation())
                .await
                .is_err()
        );
        assert_eq!(pending(&db).await, vec![(session_id.clone(), false)]);
        assert_eq!(cursor(&db, &session_id).await, 0);
    }

    #[tokio::test]
    async fn sequence_gap_is_reported_without_fast_forward_or_enqueue() {
        let (db, session_id, runtime) = fixture();
        let event = event(
            &session_id,
            2,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("step_interval", false),
        );

        let error = runtime
            .process_event(&session_id, &event, &cancellation())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("sequence gap"));
        assert_eq!(cursor(&db, &session_id).await, 0);
        assert!(pending(&db).await.is_empty());
    }

    #[tokio::test]
    async fn event_from_another_session_is_ignored() {
        let (db, target_id, runtime) = fixture();
        let other = db.create_session("other session").unwrap();
        let event = event(
            &other.id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("pause", true),
        );

        assert_eq!(
            runtime
                .process_event(&target_id, &event, &cancellation())
                .await
                .unwrap(),
            MemoryEventProcessOutcome::IgnoredOtherSession
        );
        assert_eq!(cursor(&db, &target_id).await, 0);
        assert_eq!(cursor(&db, &other.id).await, 0);
        assert!(pending(&db).await.is_empty());
    }

    #[tokio::test]
    async fn cancellation_after_durable_enqueue_does_not_checkpoint_event() {
        let (db, session_id, runtime) = fixture();
        let event = event(
            &session_id,
            1,
            MEMORY_TRIGGER_EVENT_TYPE,
            trigger("step_interval", false),
        );
        let cancellation = cancellation();
        let cancel_after_enqueue = cancellation.clone();

        let result = runtime
            .process_event_after_enqueue(&session_id, &event, &cancellation, move || {
                cancel_after_enqueue.cancel();
            })
            .await;
        assert!(result.is_err());
        assert_eq!(cursor(&db, &session_id).await, 0);
        assert_eq!(pending(&db).await, vec![(session_id, false)]);
    }
}
