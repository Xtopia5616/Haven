//! Ordered processing core for committed session events that drive memory.
//!
//! This module intentionally does not subscribe to events or own application
//! startup. Its caller supplies events for a target session in sequence order.

use std::sync::Arc;

use anyhow::Context as _;
use haven_memory::{CURRENT_EVENT_VERSION, MEMORY_TRIGGER_EVENT_TYPE, SessionEvent, SessionStore};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::memory_worker::MemoryWorker;

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
            let MemoryTriggerPayload {
                trigger_kind,
                bypass_throttle,
                run_id,
                step_number,
                pause_reason,
            } = payload;
            // These optional fields are validated at the wire boundary but
            // intentionally are not used to reconstruct transcript content.
            let _metadata = (run_id, step_number, pause_reason);
            match trigger_kind {
                MemoryTriggerKind::StepInterval => anyhow::ensure!(
                    !bypass_throttle,
                    "step_interval memory trigger cannot bypass throttle"
                ),
                MemoryTriggerKind::Pause => {
                    anyhow::ensure!(bypass_throttle, "pause memory trigger must bypass throttle")
                }
            }
            self.memory_worker
                .enqueue_infer_durable(target_session_id, bypass_throttle, cancellation)
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryTriggerPayload {
    trigger_kind: MemoryTriggerKind,
    bypass_throttle: bool,
    #[serde(default)]
    run_id: Option<u64>,
    #[serde(default)]
    step_number: Option<u32>,
    #[serde(default)]
    pause_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemoryTriggerKind {
    StepInterval,
    Pause,
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
