//! Shared memory-trigger wire payload and best-effort event producer.

use std::sync::Arc;

use anyhow::Context as _;
use haven_memory::{
    Database, MEMORY_TRIGGER_EVENT_TYPE, SessionEvent, SessionEventInput, SessionStore,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryTriggerKind {
    StepInterval,
    Pause,
}

/// Typed `memory_trigger` payload shared by the ReAct producer and runtime consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryTriggerPayload {
    pub(crate) trigger_kind: MemoryTriggerKind,
    pub(crate) bypass_throttle: bool,
    #[serde(default)]
    pub(crate) run_id: Option<u64>,
    #[serde(default)]
    pub(crate) step_number: Option<u32>,
    #[serde(default)]
    pub(crate) pause_reason: Option<String>,
}

impl MemoryTriggerPayload {
    pub(crate) fn step_interval(run_id: u64, step_number: u32) -> Self {
        Self {
            trigger_kind: MemoryTriggerKind::StepInterval,
            bypass_throttle: false,
            run_id: Some(run_id),
            step_number: Some(step_number),
            pause_reason: None,
        }
    }

    pub(crate) fn pause(run_id: u64, step_number: u32, reason: impl Into<String>) -> Self {
        Self {
            trigger_kind: MemoryTriggerKind::Pause,
            bypass_throttle: true,
            run_id: Some(run_id),
            step_number: Some(step_number),
            pause_reason: Some(reason.into()),
        }
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        match self.trigger_kind {
            MemoryTriggerKind::StepInterval => anyhow::ensure!(
                !self.bypass_throttle,
                "step_interval memory trigger cannot bypass throttle"
            ),
            MemoryTriggerKind::Pause => anyhow::ensure!(
                self.bypass_throttle,
                "pause memory trigger must bypass throttle"
            ),
        }
        if matches!(self.trigger_kind, MemoryTriggerKind::Pause) {
            anyhow::ensure!(
                self.pause_reason
                    .as_deref()
                    .is_some_and(|reason| !reason.is_empty()),
                "pause memory trigger requires a pause_reason"
            );
        }
        Ok(())
    }
}

/// Persist an interval trigger through the ReAct engine's shared event store.
/// A missing session is expected in synthetic loop tests and has no side effect.
async fn append_memory_trigger(
    db: Arc<Database>,
    store: SessionStore,
    session_id: &str,
    payload: MemoryTriggerPayload,
    cancellation: CancellationToken,
) -> anyhow::Result<Option<SessionEvent>> {
    anyhow::ensure!(
        !cancellation.is_cancelled(),
        "memory trigger append cancelled before persistence"
    );
    payload
        .validate()
        .context("invalid memory trigger payload")?;

    let event_input = SessionEventInput {
        event_type: MEMORY_TRIGGER_EVENT_TYPE.to_owned(),
        payload: serde_json::to_string(&payload).context("serialize memory trigger payload")?,
        run_id: payload.run_id,
        step_number: payload.step_number,
    };
    let session_id = session_id.to_owned();

    db.clone()
        .run_blocking_cancellable(cancellation, move |db| {
            if db.get_session(&session_id)?.is_none() {
                return Ok(None);
            }

            store
                .append_batch(&session_id, std::slice::from_ref(&event_input))?
                .into_iter()
                .next()
                .map(Some)
                .ok_or_else(|| anyhow::anyhow!("memory trigger append returned no event"))
        })
        .await
}

/// Memory scheduling is best effort from ReAct's perspective. The durable
/// event enables recovery when written, while its failure never changes the
/// provider turn's existing success/cancellation semantics.
pub(crate) async fn append_memory_trigger_nonfatal(
    db: Arc<Database>,
    store: SessionStore,
    session_id: &str,
    payload: MemoryTriggerPayload,
    cancellation: CancellationToken,
) {
    match append_memory_trigger(db, store, session_id, payload, cancellation.clone()).await {
        Ok(Some(event)) => tracing::debug!(
            session_id,
            sequence = event.sequence,
            "appended memory trigger"
        ),
        Ok(None) => {}
        Err(error) if cancellation.is_cancelled() => {
            tracing::debug!(session_id, "memory trigger append cancelled: {error}");
        }
        Err(error) => {
            tracing::warn!(session_id, "memory trigger append failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn persists_interval_payload_after_existing_durable_event() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("interval trigger test").unwrap();
        let store = SessionStore::new(db.clone());
        let prior = store
            .append(&session.id, "test_marker", "{}", None, None)
            .unwrap();

        let event = append_memory_trigger(
            db.clone(),
            store.clone(),
            &session.id,
            MemoryTriggerPayload::step_interval(17, 25),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(event.event_type, MEMORY_TRIGGER_EVENT_TYPE);
        assert_eq!(event.sequence, prior.sequence + 1);
        assert_eq!(event.run_id, Some(17));
        assert_eq!(event.step_number, Some(25));
        assert_eq!(
            serde_json::from_str::<MemoryTriggerPayload>(&event.payload).unwrap(),
            MemoryTriggerPayload::step_interval(17, 25)
        );
        assert_eq!(store.latest_sequence(&session.id).unwrap(), event.sequence);
    }

    #[tokio::test]
    async fn cancelled_best_effort_append_has_no_effect_and_is_nonfatal() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db
            .create_session("cancelled interval trigger test")
            .unwrap();
        let store = SessionStore::new(db.clone());
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        append_memory_trigger_nonfatal(
            db.clone(),
            store.clone(),
            &session.id,
            MemoryTriggerPayload::step_interval(3, 5),
            cancellation,
        )
        .await;

        assert_eq!(store.latest_sequence(&session.id).unwrap(), 0);
    }

    #[tokio::test]
    async fn append_failure_is_nonfatal_and_does_not_publish_a_partial_event() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("failed interval trigger test").unwrap();
        let store = SessionStore::new(db.clone());
        let prior = store
            .append(&session.id, "test_marker", "{}", None, None)
            .unwrap();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_memory_trigger BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'memory_trigger'
                 BEGIN SELECT RAISE(ABORT, 'test memory trigger failure'); END;",
            )
            .unwrap();

        append_memory_trigger_nonfatal(
            db,
            store.clone(),
            &session.id,
            MemoryTriggerPayload::step_interval(3, 5),
            CancellationToken::new(),
        )
        .await;

        assert_eq!(store.latest_sequence(&session.id).unwrap(), prior.sequence);
    }

    #[tokio::test]
    async fn missing_synthetic_session_does_not_append_event() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionStore::new(db.clone());

        assert!(
            append_memory_trigger(
                db,
                store,
                "ses-synthetic",
                MemoryTriggerPayload::step_interval(1, 25),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .is_none()
        );
    }
}
