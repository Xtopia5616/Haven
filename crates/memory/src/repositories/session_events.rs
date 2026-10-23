//! Durable, append-only session event storage.
//!
//! `session_events` is the recovery authority for a session.  Producers own
//! the JSON payload and event type; this repository only provides ordering,
//! transactionality and timeline control.  A rollback never deletes history:
//! it appends a `timeline_rollback` marker and readers replay the active
//! timeline from the complete log.

use crate::Database;
use crate::repositories::messages::{
    Message, PendingInputDisposition, PendingSessionInput, last_projection_created_at,
    next_projection_created_at,
};
use crate::repositories::session_steps::{SessionStep, ToolStepOutcome, ToolStepWrite};
use crate::repositories::sessions::{Session, SessionCreation};
use crate::repositories::usage::{LlmUsageRecord, LlmUsageRecordInput, SessionUsage};
use chrono::{SecondsFormat, Utc};
use haven_common::lifecycle::SessionStatus;
use haven_common::types::{MessageAttachment, TranscriptMessageKind, is_canonical_id};
use rusqlite::OptionalExtension;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

mod branch_events;
mod event_append;
mod event_cursors;
mod event_history;
mod event_model;
mod event_replay;
mod forks;
mod ingress;
mod pending_inputs;
mod rollback;
mod session_authorization;
mod session_history;
mod session_records;
mod tool_steps;
mod transcript_commit;
mod usage_events;
use event_model::{
    BoundedByteCounter, MAX_SESSION_EVENT_REPLAY_PAGE_SIZE, SessionEventInputRef,
    session_event_from_row,
};
pub use event_model::{
    MAX_SESSION_ASSET_SEARCH_RESULTS, MAX_SESSION_HISTORY_QUERY_BYTES,
    MAX_SESSION_HISTORY_SEARCH_RESULTS, MAX_SESSION_LINKED_HISTORY_CURSOR_TIMESTAMP_BYTES,
    MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS, SessionEvent, SessionEventInput, SessionEventPage,
    SessionLinkedTranscriptHistoryCursor, SessionLinkedTranscriptHistoryMatch,
    SessionLinkedTranscriptHistorySearch, SessionTranscriptAsset, SessionTranscriptAssetMatch,
    SessionTranscriptAssetSearch, SessionTranscriptHistoryMatch, SessionTranscriptHistoryRecord,
    SessionTranscriptHistorySearch,
};
pub use session_history::{
    SessionHistoryFilter, SessionHistoryStatusFilter, SessionMessageText, SessionResumeMedia,
    SessionTitleGenerationContext,
};
pub use transcript_commit::{SessionCommitResult, SessionCommitted};
#[cfg(test)]
use transcript_commit::{SessionCommittedEvent, SessionProjectionIntent};

pub const TRANSCRIPT_EVENT_TYPE: &str = "transcript";
/// Stable session execution context survives transcript rollback and compaction.
/// The payload remains Agent-owned; Memory only preserves its latest event.
pub const SESSION_EXECUTION_WORKSPACE_EVENT_TYPE: &str =
    "session_context.execution_workspace_bound";
/// Durable session execution preset. Like the workspace binding, this is
/// stable context and survives timeline rollback and compaction.
pub const SESSION_EXECUTION_MODE_EVENT_TYPE: &str = "session_context.execution_mode_bound";
/// Agent-owned durable signal for memory work. Memory stores the event
/// without depending on Agent payload types.
pub const MEMORY_TRIGGER_EVENT_TYPE: &str = "memory_trigger";
const BRANCH_POINT_EVENT_TYPE: &str = "branch_point";
pub const TIMELINE_ROLLBACK_EVENT_TYPE: &str = "timeline_rollback";
/// Durable usage domain events. Their payload is the complete
/// [`LlmUsageRecord`] value; `llm_usage` and `session_usage` are projections.
pub const USAGE_RECORDED_EVENT_TYPE: &str = "usage_recorded";
pub const USAGE_DISCARDED_EVENT_TYPE: &str = "usage_discarded";
/// Session-local interaction lifecycle events. The payload is an Agent-owned
/// `InteractionRequest` or a small `{ "request_ids": [...] }` object; Memory
/// only orders and durably stores the event.
pub const INTERACTION_REQUESTED_EVENT_TYPE: &str = "interaction_requested";
pub const INTERACTION_RESOLVED_EVENT_TYPE: &str = "interaction_resolved";
pub const INTERACTION_CLEARED_EVENT_TYPE: &str = "interaction_cleared";
/// Agent-owned ordered plan for the full ReAct tool batch paused by one or
/// more confirmation requests. Memory stores the payload without interpreting
/// its fields.
pub const CONFIRMATION_BATCH_PLANNED_EVENT_TYPE: &str = "confirmation_batch_planned";
/// Two-phase marker for recovery-only partial persistence. It is deliberately
/// an append-only control event so an interrupted repair remains observable
/// even when materialized projection rows are incomplete.
const RECOVERY_PERSISTENCE_EVENT_TYPE: &str = "recovery_persistence";
/// Hard ceiling for one session-event append transaction, including pure
/// domain-event batches. ReAct context/tool batches are smaller; this protects
/// the persistence boundary if a caller bypasses those planners.
pub(crate) const MAX_SESSION_EVENT_BATCH_EVENTS: usize = 128;
pub(crate) const MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS: usize = 256;
/// Hard ceiling for serialized session-event batches and the complete
/// transcript commit payload, including materialized projection text.
pub const MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn ensure_session_id(session_id: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(session_id, "ses"),
        "session_id must be a canonical ses-* ID"
    );
    Ok(())
}

fn ensure_message_or_step_id(message_or_step_id: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(message_or_step_id, "msg") || is_canonical_id(message_or_step_id, "step"),
        "{field} must be a canonical msg-* or step-* ID"
    );
    Ok(())
}

fn ensure_message_id(message_id: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(message_id, "msg"),
        "{field} must be a canonical msg-* ID"
    );
    Ok(())
}

fn ensure_step_id(step_id: &str, field: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_canonical_id(step_id, "step"),
        "{field} must be a canonical step-* ID"
    );
    Ok(())
}

fn ensure_domain_event_type(event_type: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        event_type != TRANSCRIPT_EVENT_TYPE,
        "transcript events must use the transcript commit boundary"
    );
    Ok(())
}

fn ensure_domain_event_types(events: &[SessionEventInput]) -> anyhow::Result<()> {
    for event in events {
        ensure_domain_event_type(&event.event_type)?;
    }
    Ok(())
}

/// Latest active rollback metadata for one session step.
///
/// The durable branch-point event sequence, transcript cursor, step identity,
/// and message projection timestamp are separate clocks/identities; keep them
/// named instead of passing them positionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveBranchPoint {
    pub event_sequence: i64,
    pub event_cursor: usize,
    pub step_number: u32,
    pub last_msg_at: Option<String>,
}

/// The assistant partial kinds that recovery may persist outside the
/// canonical transcript event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryPartialKind {
    Thought,
    Reasoning,
}

impl RecoveryPartialKind {
    const fn as_message_type(self) -> TranscriptMessageKind {
        match self {
            Self::Thought => TranscriptMessageKind::Text,
            Self::Reasoning => TranscriptMessageKind::Reasoning,
        }
    }
}

/// The only cursor view exposed to the Agent layer.
///
/// `session_events` owns the event sequence, while messages and steps are
/// materialized projections.  Callers must not derive one clock from another;
/// this value is read from the same SQLite connection so a recovery boundary
/// observes one coherent point in time.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SessionCursor {
    pub event_sequence: i64,
    pub event_cursor: usize,
    pub message_ingress_seq: i64,
    pub step_seq: i64,
    pub last_msg_at: Option<String>,
}

/// The complete durable input required to rebuild an Agent session.
///
/// The event store returns the active transcript and branch metadata together
/// with the projection clocks from one read boundary. Callers must not load
/// these pieces independently and then try to reconcile their timestamps.
#[derive(Debug, Clone)]
pub struct SessionReplayState {
    pub transcript: Vec<SessionEvent>,
    pub branch_points: Vec<ActiveBranchPoint>,
    pub cursor: SessionCursor,
}

/// The projection side of a rollback boundary. `inclusive` is true for a
/// user-message rollback (the selected message is removed) and false for a
/// failed/agent step rollback (the branch-point message is retained).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionCutoff {
    pub created_at: String,
    pub inclusive: bool,
}

/// Describes how a rollback's materialized projection boundary is resolved.
/// The SessionStore resolves either boundary inside the rollback transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackProjectionBoundary {
    /// Remove the selected message and all projection rows at or after it.
    UserMessage { message_id: String },
    /// Use the active branch point's exclusive message cutoff. When no branch
    /// point exists, use the latest user message timestamp as the cutoff.
    BranchPoint,
}

/// The durable and projection boundary captured by the Agent before rollback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackRequest {
    pub expected_event_sequence: i64,
    pub transcript_cursor: usize,
    pub target_step: u32,
    pub projection_boundary: RollbackProjectionBoundary,
}

#[derive(Debug, Clone)]
pub struct RollbackResult {
    pub marker: SessionEvent,
    pub replacement_events: Vec<SessionEvent>,
    pub cursor: SessionCursor,
    pub to_sequence: i64,
}

/// Stage results carried by a recovery-persistence control event. Keeping the
/// status as a named value prevents the event API from becoming a positional
/// boolean list as the protocol evolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPersistenceStatus {
    pub branch_point: bool,
    pub partial_messages: bool,
    pub projection: bool,
    pub event_boundary: bool,
}

impl SessionEventInput {
    pub fn new(event_type: impl Into<String>, payload: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            payload: payload.into(),
            run_id: None,
            step_number: None,
        }
    }

    pub fn transcript(payload: impl Into<String>, run_id: u64, step_number: u32) -> Self {
        Self {
            event_type: TRANSCRIPT_EVENT_TYPE.into(),
            payload: payload.into(),
            run_id: Some(run_id),
            step_number: Some(step_number),
        }
    }
}

/// A cloneable persistence boundary used by the Agent layer.  The store is
/// deliberately independent of Agent types so the memory crate remains a
/// stable persistence dependency and can replay events without loading the
/// ReAct implementation.
#[derive(Clone)]
pub struct SessionStore {
    db: Arc<Database>,
    live_tx: tokio::sync::broadcast::Sender<SessionEvent>,
}

/// SQLite failures that indicate writes could not reach the local database
/// storage. `DiskFull` is the precise `SQLITE_FULL` signal; an I/O failure is
/// deliberately broader because the operating system may not identify its
/// underlying cause as exhausted space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteStorageWriteFailure {
    DiskFull,
    IoFailure,
}

/// Existing read models needed to build the App's session-resume response.
///
/// The transcript/read-model fields do not imply a shared database snapshot.
/// `active_events` and `event_sequence_watermark` are captured together so the
/// frontend can reject late events from a branch that has since been rolled back.
#[derive(Debug, Clone)]
pub struct SessionResumeProjection {
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
    pub usage: Option<SessionUsage>,
    pub llm_usage: Vec<LlmUsageRecord>,
    /// Active append-only event stream used by Agent-owned resume reducers.
    pub active_events: Vec<SessionEvent>,
    pub active_domain_events: Vec<SessionEvent>,
    /// Highest durable sequence observed in the same snapshot as active events.
    pub event_sequence_watermark: i64,
}

/// Agent-prepared prefix used to create an independent child session.
///
/// Event payloads contain the active transcript prefix and its rollback
/// boundaries. Projection rows are re-keyed by Agent and copied with new
/// child-owned identities; usage, authorization grants, pending inputs, and
/// interaction state are omitted.
#[derive(Debug, Clone)]
pub struct SessionForkSnapshot {
    pub parent_session_id: String,
    pub source_message_id: String,
    pub source_message_created_at: String,
    pub child_source_message_id: String,
    pub expected_event_sequence: i64,
    pub expected_projection_created_at: Option<String>,
    pub input_text: String,
    pub transcript_events: Vec<SessionEventInput>,
    pub branch_points: Vec<SessionForkBranchPoint>,
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
}

/// A parent rollback boundary inside the forked transcript prefix. Its
/// projection cutoff is translated to the child's clock during the write.
#[derive(Debug, Clone)]
pub struct SessionForkBranchPoint {
    pub event_cursor: usize,
    pub step_number: u32,
    pub source_last_msg_at: Option<String>,
}

const MAX_FORK_TRANSCRIPT_EVENTS: usize = 8_192;
const MAX_FORK_PROJECTION_ROWS: usize = 32_768;
const MAX_FORK_EVENT_PAYLOAD_BYTES: usize = 32 * 1024 * 1024;

fn rollback_after_sqlite_failure(conn: &rusqlite::Connection, operation: &'static str) {
    if conn.is_autocommit() {
        return;
    }
    if let Err(error) = conn.execute_batch("ROLLBACK") {
        tracing::error!(
            operation,
            error = %haven_common::error::sanitize_error_text(&error.to_string()),
            "failed to roll back SQLite transaction after an operation failure"
        );
    }
}

fn commit_sqlite_transaction(
    conn: &rusqlite::Connection,
    operation: &'static str,
) -> anyhow::Result<()> {
    if let Err(error) = conn.execute_batch("COMMIT") {
        rollback_after_sqlite_failure(conn, operation);
        return Err(error.into());
    }
    Ok(())
}

impl SessionStore {
    #[cfg(feature = "test-support")]
    pub fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(db: Arc<Database>) -> Self {
        Self::from_database(db)
    }

    fn from_database(db: Arc<Database>) -> Self {
        let (live_tx, _) = tokio::sync::broadcast::channel(256);
        Self { db, live_tx }
    }

    /// Classify an SQLite error preserved in an `anyhow` source chain so the
    /// session boundary can give users storage-specific recovery guidance.
    /// The I/O class does not claim that free space is the cause.
    pub fn sqlite_storage_write_failure(
        error: &anyhow::Error,
    ) -> Option<SqliteStorageWriteFailure> {
        error.chain().find_map(|cause| {
            let sqlite_error = cause.downcast_ref::<rusqlite::Error>()?;
            let rusqlite::Error::SqliteFailure(code, _) = sqlite_error else {
                return None;
            };
            match code.code {
                rusqlite::ErrorCode::DiskFull => Some(SqliteStorageWriteFailure::DiskFull),
                rusqlite::ErrorCode::SystemIoFailure => Some(SqliteStorageWriteFailure::IoFailure),
                _ => None,
            }
        })
    }

    /// Load the existing read models used by App session resume on SQLite's
    /// blocking pool. Queries run in the established order and retain their
    /// independent read semantics; this port does not promise one snapshot.
    /// Dropping this future cannot interrupt a query already running on
    /// Tokio's blocking pool.
    pub async fn load_session_usage(
        &self,
        session_id: &str,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Option<SessionUsage>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let read = move |db: &Database| db.get_session_usage(&session_id);
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, read).await,
            None => self.db.run_blocking(read).await,
        }
    }

    /// Append one event only when the session still exists. This keeps
    /// best-effort producers from reaching through the SessionStore merely to
    /// perform a presence check before an append.
    pub async fn append_if_session_exists(
        &self,
        session_id: &str,
        input: SessionEventInput,
        cancel: CancellationToken,
    ) -> anyhow::Result<Option<SessionEvent>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let store = self.clone();
        self.db
            .run_blocking_cancellable(cancel, move |db| {
                if db.get_session(&session_id)?.is_none() {
                    return Ok(None);
                }
                store
                    .append_batch(&session_id, std::slice::from_ref(&input))?
                    .into_iter()
                    .next()
                    .map(Some)
                    .ok_or_else(|| anyhow::anyhow!("session event append returned no event"))
            })
            .await
    }

    /// Persist one checkpoint of an in-flight assistant stream.
    ///
    /// Partial stream rows remain scratch data and do not advance the
    /// projection `last_msg_at` clock. Promotion delegates to the Database
    /// transaction that compares message ingress sequences and atomically
    /// writes the message projection while consuming the checkpoint.
    pub async fn upsert_partial_stream(
        &self,
        session_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let content = content.to_owned();
        self.db
            .run_blocking(move |db| db.upsert_partial_message(&session_id, &content))
            .await
    }

    /// Consume and, when still current and non-empty, promote a checkpointed
    /// stream into the session transcript using the existing Database
    /// operation.
    pub async fn promote_partial_stream(&self, session_id: &str) -> anyhow::Result<bool> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.promote_partial_message(&session_id))
            .await
    }

    /// Remove an unpromoted stream checkpoint. Missing rows remain a no-op.
    pub async fn discard_partial_stream(&self, session_id: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.delete_partial_message(&session_id))
            .await
    }

    /// Subscribe to events committed through this store. A consumer that also
    /// reads durable history must subscribe first, then replay and deduplicate
    /// overlapping `(session_id, sequence)` values. A lagged receiver must
    /// perform the same replay again.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.live_tx.subscribe()
    }
}

fn bump_step_sequence(conn: &rusqlite::Connection, session_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO session_step_cursors (session_id, last_step_seq)
         VALUES (?1, 1)
         ON CONFLICT(session_id) DO UPDATE SET
            last_step_seq = session_step_cursors.last_step_seq + 1",
        rusqlite::params![session_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::sessions::SessionCreation;
    use haven_common::config::RequestKind;
    use haven_common::usage::{CacheAccounting, LlmCallKind};

    fn usage_input(step_number: u32, total_tokens: u32) -> LlmUsageRecordInput {
        LlmUsageRecordInput {
            step_number: Some(step_number),
            request_kind: RequestKind::Chat,
            call_kind: LlmCallKind::Agent,
            model: Some("test-model".into()),
            prompt_tokens: total_tokens,
            completion_tokens: 0,
            total_tokens,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cache_miss_tokens: 0,
            cache_accounting: CacheAccounting::Unknown,
            cache_diagnostics: None,
            cost_usd: 0.0,
            has_cost: false,
            duration_ms: None,
            context_tokens: 0,
            context_window: None,
        }
    }

    fn store() -> (Arc<Database>, SessionStore, String) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("input").unwrap();
        let store = SessionStore::new(db.clone());
        (db, store, session.session_id)
    }

    #[tokio::test]
    async fn session_store_event_boundary_cursor_defaults_or_matches_replay() {
        let (_db, store, session_id) = store();

        assert_eq!(
            store
                .event_boundary_cursor(&session_id, None)
                .await
                .unwrap(),
            SessionCursor::default()
        );

        store
            .seed_if_empty_async(
                &session_id,
                vec![SessionEventInput::transcript(
                    r#"{"type":"boundary"}"#,
                    4,
                    8,
                )],
            )
            .await
            .unwrap();
        let replay = store
            .load_replay_state_async(&session_id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            store
                .event_boundary_cursor(&session_id, Some(CancellationToken::new()))
                .await
                .unwrap(),
            replay.cursor
        );
    }

    #[tokio::test]
    async fn session_event_step_number_round_trips_u32_and_rejects_negative_storage() {
        let (db, store, session_id) = store();
        let step_number = u32::MAX;
        store
            .append_usage(&session_id, &usage_input(step_number, 10))
            .unwrap();

        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(usage[0].step_number, Some(step_number));
        assert_eq!(
            store.read_all(&session_id).unwrap()[0].step_number,
            Some(step_number)
        );

        db.conn().expect("test database connection checkout failed")
            .execute(
                "INSERT INTO session_events
                    (session_id, sequence, event_type, payload, created_at, run_id, step_number)
                 VALUES (?1, 2, 'test_corrupt_step_number', '{}', '2026-10-12T00:00:00Z', NULL, -1)",
                rusqlite::params![session_id],
            )
            .unwrap();

        assert!(store.read_all(&session_id).is_err());
    }

    #[tokio::test]
    async fn session_event_run_id_rejects_negative_storage() {
        let (db, store, session_id) = store();
        db.conn()
            .expect("test database connection checkout failed")
            .execute(
                "INSERT INTO session_events
                    (session_id, sequence, event_type, payload, created_at, run_id, step_number)
                 VALUES (?1, 1, 'test_corrupt_run_id', '{}', '2026-10-12T00:00:00Z', -1, NULL)",
                rusqlite::params![session_id],
            )
            .unwrap();

        assert!(store.read_all(&session_id).is_err());
    }

    #[tokio::test]
    async fn session_store_react_marker_ports_preserve_session_check_behavior() {
        let (_db, store, session_id) = store();
        let cursor = store
            .append_branch_point_from_projection_for_existing_session(&session_id, 3, Some(7))
            .await
            .unwrap();
        assert_eq!(cursor, SessionCursor::default());
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);

        let missing_session_id = haven_common::types::new_id("ses");
        let error = store
            .append_branch_point_from_projection_for_existing_session(
                &missing_session_id,
                4,
                Some(8),
            )
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("disappeared before branch-point append")
        );
        store
            .append_recovery_persistence_if_session_exists(
                &missing_session_id,
                8,
                4,
                "failed",
                RecoveryPersistenceStatus {
                    branch_point: false,
                    partial_messages: false,
                    projection: false,
                    event_boundary: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);

        store
            .append_recovery_persistence_if_session_exists(
                &session_id,
                8,
                4,
                "failed",
                RecoveryPersistenceStatus {
                    branch_point: true,
                    partial_messages: false,
                    projection: false,
                    event_boundary: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(store.read_all(&session_id).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn session_store_event_boundary_cursor_observes_cancellation() {
        let (db, store, session_id) = store();
        // In-memory databases have one pooled connection. Holding it keeps
        // the replay worker pending until the cancellable scheduler times out.
        let connection = db.conn().expect("test database connection checkout failed");
        let cancel = CancellationToken::new();
        cancel.cancel();

        let result = store.event_boundary_cursor(&session_id, Some(cancel)).await;
        drop(connection);

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn session_store_async_replay_seed_and_transcript_ports_preserve_event_order() {
        let (_db, store, session_id) = store();
        let mut live = store.subscribe();

        assert!(
            store
                .load_replay_state_async(&session_id)
                .await
                .unwrap()
                .is_none()
        );

        let seeded = store
            .seed_if_empty_async(
                &session_id,
                vec![SessionEventInput::transcript(r#"{"type":"first"}"#, 4, 8)],
            )
            .await
            .unwrap();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].sequence, 1);
        assert_eq!(live.try_recv().unwrap(), seeded[0]);

        let duplicate_seed = store
            .seed_if_empty_async(
                &session_id,
                vec![SessionEventInput::transcript(r#"{"type":"stale"}"#, 9, 9)],
            )
            .await
            .unwrap();
        assert_eq!(duplicate_seed, seeded);
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        assert_eq!(
            store
                .append_transcript_async(&session_id, r#"{"type":"second"}"#, 4, 9)
                .await
                .unwrap(),
            2
        );
        let appended = live.try_recv().unwrap();
        assert_eq!(appended.sequence, 2);
        assert_eq!(appended.run_id, Some(4));
        assert_eq!(appended.step_number, Some(9));

        let replay = store
            .load_replay_state_async(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay.transcript.len(), 2);
        assert_eq!(replay.transcript[0].payload, r#"{"type":"first"}"#);
        assert_eq!(replay.transcript[1], appended);
        assert_eq!(replay.cursor.event_sequence, 2);
        assert_eq!(replay.cursor.event_cursor, 2);
    }

    #[tokio::test]
    async fn session_store_async_transcript_append_errors_for_missing_session() {
        let (_db, store, _session_id) = store();
        let missing_session_id = haven_common::types::new_id("ses");
        let mut live = store.subscribe();

        assert!(
            store
                .append_transcript_async(&missing_session_id, r#"{"type":"synthetic"}"#, 1, 1,)
                .await
                .is_err()
        );
        assert!(store.read_all(&missing_session_id).unwrap().is_empty());
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn session_store_async_transcript_port_failure_has_no_durable_or_live_side_effect() {
        let (_db, store, session_id) = store();
        let mut live = store.subscribe();

        let error = store
            .append_transcript_async(&session_id, "not-json", 1, 1)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("valid JSON"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    fn tool_step_write(session_id: &str, step_id: &str) -> ToolStepWrite {
        ToolStepWrite {
            session_id: session_id.into(),
            step_number: 7,
            tool_index: 2,
            tool_name: "files.read".into(),
            tool_input: r#"{"path":"notes.txt"}"#.into(),
            tool_call_id: Some("provider-call-7".into()),
            is_high_risk: true,
            silent: false,
            step_id: step_id.into(),
        }
    }

    #[tokio::test]
    async fn session_store_tool_step_ports_preserve_identity_confirmation_and_start() {
        let (db, store, session_id) = store();
        let write = tool_step_write(&session_id, &haven_common::types::new_id("step"));

        store
            .ensure_tool_step(write.clone(), Some(false))
            .await
            .unwrap();
        store
            .ensure_tool_step(write.clone(), Some(true))
            .await
            .unwrap();

        let pending = db.list_session_steps(&session_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].step_id, write.step_id);
        assert_eq!(pending[0].session_id, session_id);
        assert_eq!(pending[0].step_number, 7);
        assert_eq!(pending[0].tool_index, 2);
        assert_eq!(pending[0].tool_name.as_deref(), Some("files.read"));
        assert_eq!(
            pending[0].tool_input.as_deref(),
            Some(r#"{"path":"notes.txt"}"#)
        );
        assert_eq!(pending[0].tool_call_id.as_deref(), Some("provider-call-7"));
        assert!(pending[0].is_high_risk);
        assert_eq!(pending[0].confirmed, Some(true));
        assert_eq!(
            pending[0].status,
            haven_common::lifecycle::SessionStepStatus::Pending
        );

        assert!(
            store
                .ensure_and_start_tool_step(write.clone(), None)
                .await
                .unwrap()
        );
        assert!(store.ensure_and_start_tool_step(write, None).await.unwrap());

        let running = db.list_session_steps(&session_id).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(
            running[0].status,
            haven_common::lifecycle::SessionStepStatus::Running
        );
        assert!(running[0].started_at.is_some());
        assert_eq!(running[0].confirmed, Some(true));
    }

    #[tokio::test]
    async fn session_store_tool_step_finish_port_preserves_outcome_and_observation() {
        let (db, store, session_id) = store();
        let write = tool_step_write(&session_id, &haven_common::types::new_id("step"));

        assert!(
            store
                .ensure_and_finish_tool_step(
                    write.clone(),
                    Some(false),
                    "tool may have crossed a side-effect boundary".into(),
                    ToolStepOutcome::Unknown,
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .ensure_and_finish_tool_step(
                    write,
                    Some(true),
                    "late completion must not overwrite the terminal row".into(),
                    ToolStepOutcome::Completed,
                )
                .await
                .unwrap()
        );

        let steps = db.list_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(
            steps[0].status,
            haven_common::lifecycle::SessionStepStatus::Unknown
        );
        assert_eq!(steps[0].confirmed, Some(false));
        assert_eq!(
            steps[0].observation.as_deref(),
            Some("tool may have crossed a side-effect boundary")
        );
        assert!(steps[0].completed_at.is_some());
    }

    #[tokio::test]
    async fn session_store_session_resume_projection_groups_existing_reads() {
        let (db, store, session_id) = store();
        let message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "resume message",
                None,
                None,
            )
            .unwrap();
        let step = db
            .create_thought_step(&session_id, 1, &message.message_id)
            .unwrap();
        let usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        let domain_event = store
            .append(
                &session_id,
                "resume_test",
                r#"{"ready":true}"#,
                Some(1),
                Some(2),
            )
            .unwrap();

        let projection = store.session_resume_projection(&session_id).await.unwrap();
        let clone = projection.clone();

        assert_eq!(
            serde_json::to_value(&projection.messages).unwrap(),
            serde_json::to_value(db.list_session_messages(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.steps).unwrap(),
            serde_json::to_value(db.list_session_steps(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.usage).unwrap(),
            serde_json::to_value(db.get_session_usage(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.llm_usage).unwrap(),
            serde_json::to_value(db.list_session_llm_usage(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.active_domain_events).unwrap(),
            serde_json::to_value(store.read_active_domain_events(&session_id).unwrap()).unwrap()
        );
        assert_eq!(projection.messages[0].message_id, message.message_id);
        assert_eq!(projection.steps[0].step_id, step.step_id);
        assert_eq!(projection.llm_usage[0].usage_id, usage.usage_id);
        assert_eq!(projection.event_sequence_watermark, domain_event.sequence);
        assert!(
            projection
                .active_domain_events
                .iter()
                .any(|event| event.sequence == domain_event.sequence)
        );
        assert_eq!(
            serde_json::to_value(clone.active_domain_events).unwrap(),
            serde_json::to_value(projection.active_domain_events).unwrap()
        );
    }

    #[tokio::test]
    async fn session_store_async_domain_event_ports_preserve_append_and_live_replay() {
        let (db, store, session_id) = store();
        let mut live = store.subscribe();
        let request_id = haven_common::types::new_id("step");
        let payload = format!(r#"{{"request_id":"{request_id}","status":"pending"}}"#);

        let appended = store
            .append_domain_event(&session_id, INTERACTION_REQUESTED_EVENT_TYPE, &payload)
            .await
            .unwrap();

        assert_eq!(appended.sequence, 1);
        assert_eq!(appended.event_type, INTERACTION_REQUESTED_EVENT_TYPE);
        assert_eq!(appended.payload, payload);
        assert_eq!(appended.run_id, None);
        assert_eq!(appended.step_number, None);
        assert_eq!(live.recv().await.unwrap(), appended);
        assert_eq!(
            store
                .read_active_domain_events_async(&session_id)
                .await
                .unwrap(),
            vec![appended]
        );

        assert!(
            store
                .append_domain_event(&session_id, TRANSCRIPT_EVENT_TYPE, "{}")
                .await
                .is_err()
        );
        assert!(
            store
                .append_domain_event_batch_with_session_status(
                    &session_id,
                    SessionStatus::Pending,
                    SessionStatus::Paused,
                    &[SessionEventInput::new(TRANSCRIPT_EVENT_TYPE, "{}")],
                )
                .await
                .is_err()
        );
        assert_eq!(store.latest_sequence(&session_id).unwrap(), 1);
        assert_eq!(
            db.get_session(&session_id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
    }

    #[tokio::test]
    async fn session_event_budget_applies_to_domain_batches_and_status_is_atomic() {
        let (db, event_store, session_id) = store();
        let mut at_limit = (0..MAX_SESSION_EVENT_BATCH_EVENTS)
            .map(|index| {
                SessionEventInput::new("test_domain_event", format!(r#"{{"index":{index}}}"#))
            })
            .collect::<Vec<_>>();
        let appended = event_store
            .append_domain_event_batch_with_session_status(
                &session_id,
                SessionStatus::Pending,
                SessionStatus::Paused,
                &at_limit,
            )
            .await
            .expect("a domain event batch at the shared limit should commit");
        assert_eq!(appended.len(), MAX_SESSION_EVENT_BATCH_EVENTS);
        assert_eq!(event_store.latest_sequence(&session_id).unwrap(), 128);
        assert_eq!(
            db.get_session(&session_id).unwrap().unwrap().status,
            SessionStatus::Paused
        );

        let (db, event_store, session_id) = store();
        let mut live = event_store.subscribe();
        at_limit.push(SessionEventInput::new(
            "test_domain_event",
            r#"{"index":128}"#,
        ));
        let error = event_store
            .append_domain_event_batch_with_session_status(
                &session_id,
                SessionStatus::Pending,
                SessionStatus::Paused,
                &at_limit,
            )
            .await
            .expect_err("a domain event batch over the shared limit must be rejected");

        assert!(error.to_string().contains("128 events"));
        assert_eq!(event_store.latest_sequence(&session_id).unwrap(), 0);
        assert_eq!(
            db.get_session(&session_id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn single_domain_event_respects_shared_payload_budget_before_append() {
        let (db, event_store, session_id) = store();
        let mut live = event_store.subscribe();
        let payload = format!(
            r#"{{"body":"{}"}}"#,
            "x".repeat(MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES)
        );

        let error = event_store
            .append_domain_event(&session_id, "test_domain_event", &payload)
            .await
            .expect_err("an oversized domain event must be rejected");

        assert!(error.to_string().contains("payload bytes"));
        assert_eq!(event_store.latest_sequence(&session_id).unwrap(), 0);
        assert_eq!(
            db.get_session(&session_id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn event_payload_budget_accepts_the_exact_serialized_limit() {
        let base_event = SessionEventInputRef {
            event_type: "test_domain_event",
            payload: "\"\"",
            run_id: None,
            step_number: None,
        };
        let base_bytes = serde_json::to_vec(std::slice::from_ref(&base_event))
            .unwrap()
            .len();
        let payload_chars = MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES - base_bytes;
        let payload = format!("\"{}\"", "x".repeat(payload_chars));

        SessionStore::validate_single_event("test_domain_event", &payload, None, None)
            .expect("the exact serialized payload limit should be accepted");
    }

    #[tokio::test]
    async fn session_store_lists_all_pending_records_newest_first_and_filters_other_states() {
        let (db, store, oldest_id) = store();
        let middle = db.create_session("middle").unwrap();
        let newest = db.create_session("newest").unwrap();
        let completed = db.create_session("completed").unwrap();
        db.update_session_status(&completed.session_id, SessionStatus::Completed)
            .unwrap();

        let conn = db.conn().expect("test database connection checkout failed");
        for (session_id, created_at) in [
            (&oldest_id, "2026-09-20T10:00:00.000Z"),
            (&middle.session_id, "2026-09-21T10:00:00.000Z"),
            (&newest.session_id, "2026-09-22T10:00:00.000Z"),
            (&completed.session_id, "2026-09-23T10:00:00.000Z"),
        ] {
            conn.execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![created_at, session_id],
            )
            .unwrap();
        }
        drop(conn);

        let pending = store
            .pending_session_records_cancellable(CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|session| session.session_id.as_str())
                .collect::<Vec<_>>(),
            [
                newest.session_id.as_str(),
                middle.session_id.as_str(),
                oldest_id.as_str()
            ]
        );
        assert!(
            pending
                .iter()
                .all(|session| session.status == SessionStatus::Pending)
        );

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            store
                .pending_session_records_cancellable(cancelled)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn session_store_fail_pending_tool_run_steps_scopes_unfinished_steps() {
        let (db, store, session_id) = store();
        let other_session = db.create_session("other").unwrap();
        let pending = db
            .create_tool_step(&session_id, 0, "shell", "{}", false, false, None, None)
            .unwrap();
        let running = db
            .create_tool_step(&session_id, 1, "shell", "{}", false, false, None, None)
            .unwrap();
        assert!(db.start_tool_step(&running.step_id).unwrap());
        let completed = db
            .create_tool_step(&session_id, 2, "shell", "{}", false, false, None, None)
            .unwrap();
        db.finish_tool_step(
            &completed.step_id,
            "already finished",
            ToolStepOutcome::Completed,
        )
        .unwrap();
        let other_pending = db
            .create_tool_step(
                &other_session.session_id,
                0,
                "shell",
                "{}",
                false,
                false,
                None,
                None,
            )
            .unwrap();

        let completed_at_before = db
            .list_session_steps(&session_id)
            .unwrap()
            .into_iter()
            .find(|step| step.step_id == completed.step_id)
            .unwrap()
            .completed_at;
        assert!(completed_at_before.is_some());
        let other_observation_before = db
            .list_session_steps(&other_session.session_id)
            .unwrap()
            .into_iter()
            .find(|step| step.step_id == other_pending.step_id)
            .unwrap()
            .observation;

        let changed = store
            .fail_pending_tool_run_steps(&session_id, "session failed")
            .await
            .unwrap();

        assert_eq!(changed, 2);
        let target_steps = db.list_session_steps(&session_id).unwrap();
        for step_id in [&pending.step_id, &running.step_id] {
            let step = target_steps
                .iter()
                .find(|step| step.step_id == *step_id)
                .unwrap();
            assert_eq!(
                step.status,
                haven_common::lifecycle::SessionStepStatus::Unknown
            );
            assert_eq!(step.observation.as_deref(), Some("session failed"));
            assert!(step.completed_at.is_some());
        }
        let completed_after = target_steps
            .iter()
            .find(|step| step.step_id == completed.step_id)
            .unwrap();
        assert_eq!(
            completed_after.status,
            haven_common::lifecycle::SessionStepStatus::Completed
        );
        assert_eq!(
            completed_after.observation.as_deref(),
            Some("already finished")
        );
        assert_eq!(completed_after.completed_at, completed_at_before);

        let untouched = db.list_session_steps(&other_session.session_id).unwrap();
        let other_pending_after = untouched
            .iter()
            .find(|step| step.step_id == other_pending.step_id)
            .unwrap();
        assert_eq!(
            other_pending_after.status,
            haven_common::lifecycle::SessionStepStatus::Pending
        );
        assert_eq!(other_pending_after.observation, other_observation_before);
    }

    #[tokio::test]
    async fn session_store_persists_ingress_seed_with_origin_type_and_attachments() {
        let (db, store, session_id) = store();
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some("asset-00000000000000000000000000000001".into());
        attachment.filename = Some("photo.png".into());
        let attachments = [attachment.clone()];

        let inserted = store
            .persist_ingress_user_seed(&session_id, "describe this", &attachments, true)
            .await
            .unwrap();

        assert!(inserted.message_id.starts_with("msg-"));
        assert_eq!(inserted.role, haven_common::types::CanonicalRole::User);
        assert_eq!(inserted.content, "describe this");
        assert_eq!(
            inserted.message_type,
            Some(haven_common::types::TranscriptMessageKind::Text)
        );
        assert_eq!(inserted.tool_call_id, None);
        assert_eq!(inserted.attachments, attachments);
        assert!(inserted.voice);

        let persisted = db
            .get_message_by_id(&session_id, &inserted.message_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted.attachments[0].asset_id.as_deref(),
            Some("asset-00000000000000000000000000000001")
        );
        assert_eq!(
            persisted.attachments[0].filename.as_deref(),
            Some("photo.png")
        );
        assert!(persisted.voice);
        assert_eq!(persisted.role, haven_common::types::CanonicalRole::User);
        assert_eq!(persisted.content, "describe this");
        assert_eq!(
            persisted.message_type,
            Some(haven_common::types::TranscriptMessageKind::Text)
        );
        assert_eq!(persisted.tool_call_id, None);
        assert_eq!(persisted.media_inputs.len(), 1);
    }

    #[tokio::test]
    async fn session_store_rejects_noncanonical_attachment_asset_ids_before_writing() {
        let (db, store, session_id) = store();
        let mut attachment = MessageAttachment::new("application/pdf", "");
        attachment.asset_id = Some("asset-test".into());
        let error = store
            .persist_ingress_user_seed(&session_id, "describe this", &[attachment], false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("canonical asset-* ID"));
        assert!(db.list_session_messages(&session_id).unwrap().is_empty());
        assert_eq!(db.get_last_message_ingress_seq(&session_id), 0);

        let mut attachment = MessageAttachment::new("application/pdf", "");
        attachment.asset_id = Some("asset-00000000000000000000000000000002".into());
        attachment
            .representations
            .push(haven_common::media::MediaRepresentation::available(
                haven_common::media::MediaRepresentationKind::ManagedFileRef,
                haven_common::media::MediaProvenance::Original,
                haven_common::media::MediaRepresentationPayload::ManagedFileRef {
                    asset_id: "asset-test".into(),
                    filename: None,
                },
            ));
        let error = store
            .persist_ingress_user_seed(&session_id, "describe this", &[attachment], false)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("managed media representation asset_id")
        );
        assert!(db.list_session_messages(&session_id).unwrap().is_empty());
        assert_eq!(db.get_last_message_ingress_seq(&session_id), 0);
    }

    #[tokio::test]
    async fn session_store_ingress_seed_type_comes_from_persisted_origin() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionStore::new(db);
        let parent = store.create_session("parent").await.unwrap();
        let user_session = store.create_session("user seed").await.unwrap();
        let kickoff_session = store
            .create_session_with_creation(
                "kickoff seed",
                SessionCreation::agent_spawn(parent.session_id),
            )
            .await
            .unwrap();

        let user_seed = store
            .persist_ingress_user_seed(&user_session.session_id, "hello", &[], false)
            .await
            .unwrap();
        let kickoff_seed = store
            .persist_ingress_user_seed(&kickoff_session.session_id, "delegate this", &[], false)
            .await
            .unwrap();

        assert_eq!(user_seed.role, haven_common::types::CanonicalRole::User);
        assert_eq!(
            user_seed.message_type,
            Some(haven_common::types::TranscriptMessageKind::Text)
        );
        assert_eq!(kickoff_seed.role, haven_common::types::CanonicalRole::User);
        assert_eq!(
            kickoff_seed.message_type,
            Some(haven_common::types::TranscriptMessageKind::PeerKickoff)
        );
    }

    #[tokio::test]
    async fn session_store_recovery_partial_fixes_role_and_type_and_is_idempotent() {
        let (db, store, session_id) = store();
        for (message_id, kind, expected_type) in [
            (
                haven_common::types::new_id("msg"),
                RecoveryPartialKind::Thought,
                TranscriptMessageKind::Text,
            ),
            (
                haven_common::types::new_id("msg"),
                RecoveryPartialKind::Reasoning,
                TranscriptMessageKind::Reasoning,
            ),
        ] {
            let first = store
                .persist_recovery_partial(&session_id, "partial", kind, &message_id)
                .await
                .unwrap();
            let retry = store
                .persist_recovery_partial(&session_id, "partial", kind, &message_id)
                .await
                .unwrap();

            assert_eq!(first.message_id, retry.message_id);
            assert_eq!(first.role, haven_common::types::CanonicalRole::Assistant);
            assert_eq!(first.message_type, Some(expected_type));
            assert!(first.attachments.is_empty());
            assert!(!first.voice);
        }
        assert_eq!(db.list_session_messages(&session_id).unwrap().len(), 2);
    }

    #[tokio::test]
    async fn session_store_private_message_row_returns_existing_for_same_id_and_content() {
        let (db, store, session_id) = store();
        let message_id = haven_common::types::new_id("msg");
        let first = store
            .persist_message_row(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "retry me",
                Some(haven_common::types::TranscriptMessageKind::Text),
                &[],
                false,
                Some(&message_id),
                None,
                None,
            )
            .await
            .unwrap();

        let retried = store
            .persist_message_row(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "retry me",
                Some(haven_common::types::TranscriptMessageKind::Text),
                &[],
                false,
                Some(&message_id),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(retried.message_id, first.message_id);
        assert_eq!(retried.created_at, first.created_at);
        assert_eq!(db.list_session_messages(&session_id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_store_private_message_row_rejects_idempotency_conflict() {
        let (db, store, session_id) = store();
        let message_id = haven_common::types::new_id("msg");
        store
            .persist_message_row(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "original",
                None,
                &[],
                false,
                Some(&message_id),
                None,
                None,
            )
            .await
            .unwrap();

        let error = store
            .persist_message_row(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "changed",
                None,
                &[],
                false,
                Some(&message_id),
                None,
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "message idempotency conflict for session {} message {}",
                session_id, message_id
            )
        );
        assert_eq!(db.list_session_messages(&session_id).unwrap().len(), 1);
    }

    fn append_recovery_marker(store: &SessionStore, session_id: &str, phase: &str) {
        store
            .append_recovery_persistence(
                session_id,
                1,
                2,
                phase,
                RecoveryPersistenceStatus {
                    branch_point: true,
                    partial_messages: true,
                    projection: true,
                    event_boundary: true,
                },
            )
            .unwrap();
    }

    #[tokio::test]
    async fn session_store_reads_pending_inputs_without_an_age_cutoff() {
        let (db, store, session_id) = store();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::User,
            "seed",
            None,
            None,
        )
        .unwrap();
        let anchored = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "delivered",
                None,
                None,
            )
            .unwrap();
        db.create_thought_step(&session_id, 1, &anchored.message_id)
            .unwrap();
        let attachment = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        let pending = db
            .add_pending_user_input(
                &session_id,
                "pending",
                None,
                std::slice::from_ref(&attachment),
                false,
                None,
                PendingInputDisposition::FollowUp,
            )
            .unwrap();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "ignored",
            None,
            None,
        )
        .unwrap();

        let messages = store.pending_session_inputs(&session_id).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message.message_id, pending.message.message_id);
        assert_eq!(messages[0].message.attachments[0].media_type, "image/png");
        assert_eq!(messages[0].message.media_inputs.len(), 1);
        assert_eq!(messages[0].disposition, PendingInputDisposition::FollowUp);

        let old_session = db.create_session("old input").unwrap();
        db.add_message(
            &old_session.session_id,
            haven_common::types::CanonicalRole::User,
            "old seed",
            None,
            None,
        )
        .unwrap();
        let old_pending = db
            .add_pending_user_input(
                &old_session.session_id,
                "old pending",
                None,
                &[],
                false,
                None,
                PendingInputDisposition::FollowUp,
            )
            .unwrap();
        let old_cutoff = (chrono::Utc::now() - chrono::Duration::days(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let conn = db.conn().expect("test database connection checkout failed");
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![old_cutoff, old_pending.message.message_id],
        )
        .unwrap();
        drop(conn);
        let old_recovered = store
            .pending_session_inputs(&old_session.session_id)
            .await
            .unwrap();
        assert_eq!(old_recovered.len(), 1);
        assert_eq!(
            old_recovered[0].message.message_id,
            old_pending.message.message_id
        );
    }

    #[tokio::test]
    async fn user_input_ack_is_atomic_with_transcript_commit() {
        let (db, store, session_id) = store();
        let pending = db
            .add_pending_user_input(
                &session_id,
                "pending",
                Some(haven_common::types::TranscriptMessageKind::Text),
                &[],
                false,
                None,
                PendingInputDisposition::FollowUp,
            )
            .unwrap();

        let mut failed = SessionCommitted::transcript("{}", 1, 1);
        failed.acknowledge_pending_user_input(pending.message.message_id.clone());
        failed.project_assistant_message(
            pending.message.message_id.clone(),
            "conflict",
            Some(haven_common::types::TranscriptMessageKind::Text),
        );
        assert!(store.commit_transcript(&session_id, &failed).is_err());
        assert_eq!(
            db.list_pending_session_inputs(&session_id).unwrap().len(),
            1
        );

        let mut committed = SessionCommitted::transcript("{}", 1, 1);
        committed.acknowledge_pending_user_input(pending.message.message_id.clone());
        store.commit_transcript(&session_id, &committed).unwrap();
        assert!(
            db.list_pending_session_inputs(&session_id)
                .unwrap()
                .is_empty()
        );
    }

    fn rollback_request(
        expected_event_sequence: i64,
        transcript_cursor: usize,
        target_step: u32,
        projection_boundary: RollbackProjectionBoundary,
    ) -> RollbackRequest {
        RollbackRequest {
            expected_event_sequence,
            transcript_cursor,
            target_step,
            projection_boundary,
        }
    }

    #[tokio::test]
    async fn session_store_loads_only_the_exact_session_rollback_target() {
        let (db, store, session_id) = store();
        let target = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "selected",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        let other_session = db.create_session("other rollback target").unwrap();
        let foreign = db
            .add_message(
                &other_session.session_id,
                haven_common::types::CanonicalRole::User,
                "foreign",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();

        let loaded = store
            .load_rollback_target_message(&session_id, &target.message_id)
            .await
            .unwrap();
        assert_eq!(loaded.message_id, target.message_id);
        assert_eq!(loaded.session_id, session_id);
        assert_eq!(loaded.role, haven_common::types::CanonicalRole::User);

        let missing_message_id = haven_common::types::new_id("msg");
        for missing_id in [foreign.message_id.as_str(), missing_message_id.as_str()] {
            let error = store
                .load_rollback_target_message(&session_id, missing_id)
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "rollback target message '{}' not found in session messages",
                    missing_id
                )
            );
        }
    }

    #[tokio::test]
    async fn session_store_async_rollback_keeps_replacement_in_the_atomic_timeline() {
        let (_db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"two"}"#, 1, 2)
            .unwrap();
        let mut live = store.subscribe();

        let result = store
            .rollback_to_async(
                &session_id,
                rollback_request(2, 1, 2, RollbackProjectionBoundary::BranchPoint),
                vec![SessionEventInput::transcript(
                    r#"{"type":"compact_summary","summary":"replacement root"}"#,
                    2,
                    2,
                )],
                Some(2),
            )
            .await
            .unwrap();

        assert_eq!(result.to_sequence, 1);
        assert_eq!(result.replacement_events.len(), 1);
        assert_eq!(
            result.replacement_events[0].sequence,
            result.marker.sequence + 1
        );
        assert_eq!(live.try_recv().unwrap(), result.marker);
        assert_eq!(live.try_recv().unwrap(), result.replacement_events[0]);
        let active = store.read_active_transcript(&session_id).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0], result.replacement_events[0]);
        assert_eq!(active[0].run_id, Some(2));
        assert_eq!(store.read_all(&session_id).unwrap().len(), 4);
    }

    #[tokio::test]
    async fn session_store_async_rollback_fails_closed_on_stale_event_cursor() {
        let (_db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"two"}"#, 1, 2)
            .unwrap();
        let before = store.read_all(&session_id).unwrap();
        let mut live = store.subscribe();

        let error = store
            .rollback_to_async(
                &session_id,
                rollback_request(1, 1, 1, RollbackProjectionBoundary::BranchPoint),
                Vec::new(),
                None,
            )
            .await
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("event boundary changed during rollback")
        );
        assert_eq!(store.read_all(&session_id).unwrap().len(), before.len());
        assert_eq!(store.read_active_transcript(&session_id).unwrap(), before);
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn session_store_async_recovery_truncate_applies_committed_projection_cutoff() {
        let (db, store, session_id) = store();
        let kept = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "kept",
                None,
                None,
            )
            .unwrap();
        store
            .append_branch_point(&session_id, 0, 2, Some(&kept.created_at), None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let discarded = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "discarded",
                None,
                None,
            )
            .unwrap();
        append_recovery_marker(&store, &session_id, "committed");

        store
            .truncate_projection_after_latest_committed_recovery_async(&session_id)
            .await
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, kept.message_id);
        assert!(
            !messages
                .iter()
                .any(|message| message.message_id == discarded.message_id)
        );
    }

    #[test]
    fn appends_monotonic_sequences_and_reads_after_cursor() {
        let (_db, store, session_id) = store();
        let first = store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 7, 1)
            .unwrap();
        let second = store
            .append_transcript(&session_id, r#"{"type":"two"}"#, 7, 2)
            .unwrap();
        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(store.read_from_sync(&session_id, 1).unwrap(), vec![second]);
    }

    #[test]
    fn memory_event_cursor_store_api_is_monotonic_and_clearable() {
        let (_db, store, session_id) = store();
        assert_eq!(store.memory_event_cursor(&session_id).unwrap(), 0);
        store
            .checkpoint_memory_event_cursor(&session_id, 3)
            .unwrap();
        assert_eq!(store.memory_event_cursor(&session_id).unwrap(), 3);
        assert!(
            store
                .checkpoint_memory_event_cursor(&session_id, 2)
                .is_err()
        );
        assert_eq!(store.memory_event_cursor(&session_id).unwrap(), 3);
        store.clear_memory_event_cursor(&session_id).unwrap();
        assert_eq!(store.memory_event_cursor(&session_id).unwrap(), 0);
    }

    #[tokio::test]
    async fn replay_page_is_bounded_ordered_and_returns_next_cursor() {
        let (db, store, session_id) = store();
        let other_session = db.create_session("other").unwrap();
        for index in 1..=5 {
            store
                .append(
                    &session_id,
                    "usage_recorded",
                    &format!(r#"{{"payload":"payload-{index}"}}"#),
                    None,
                    None,
                )
                .unwrap();
        }
        store
            .append(
                &other_session.session_id,
                "usage_recorded",
                r#"{"payload":"other"}"#,
                None,
                None,
            )
            .unwrap();
        let first = store.replay_page_sync(&session_id, 0, 2).unwrap();
        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [1, 2]
        );
        assert!(first.has_more);
        assert_eq!(first.next_cursor, 2);

        let second = store
            .replay_page_sync(&session_id, first.next_cursor, 2)
            .unwrap();
        assert_eq!(
            second
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [3, 4]
        );
        assert!(second.has_more);
        assert_eq!(second.next_cursor, 4);

        let third = store
            .replay_page_sync(&session_id, second.next_cursor, 2)
            .unwrap();
        assert_eq!(
            third
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            [5]
        );
        assert!(!third.has_more);
        assert_eq!(third.next_cursor, 5);

        let empty = store.replay_page_sync(&session_id, 12, 2).unwrap();
        assert!(empty.events.is_empty());
        assert!(!empty.has_more);
        assert_eq!(empty.next_cursor, 12);
        assert_eq!(
            store
                .replay_page_sync(&other_session.session_id, 0, 2)
                .unwrap()
                .events
                .len(),
            1
        );
        assert!(store.replay_page_sync(&session_id, 0, 0).is_err());
        assert!(
            store
                .replay_page_sync(&session_id, 0, MAX_SESSION_EVENT_REPLAY_PAGE_SIZE + 1)
                .is_err()
        );
        assert!(store.replay_page_sync(&session_id, -1, 2).is_err());
    }

    #[test]
    fn rollback_marker_changes_active_view_without_deleting_history() {
        let (_db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"two"}"#, 1, 2)
            .unwrap();
        let rollback = store
            .rollback_to(
                &session_id,
                &rollback_request(2, 1, 2, RollbackProjectionBoundary::BranchPoint),
                &[SessionEventInput::transcript(
                    r#"{"type":"replacement"}"#,
                    2,
                    2,
                )],
                Some(2),
            )
            .unwrap();
        assert_eq!(rollback.to_sequence, 1);

        let active = store.read_active_transcript(&session_id).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].sequence, 1);
        assert_eq!(active[1].payload, r#"{"type":"replacement"}"#);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 4);
        let replay = store.load_replay_state(&session_id).unwrap().unwrap();
        assert_eq!(replay.cursor.event_cursor, 2);
        assert_eq!(replay.transcript.len(), 2);
        assert!(replay.branch_points.is_empty());
    }

    #[test]
    fn usage_events_and_rollback_projection_share_one_transaction() {
        let (db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"base"}"#, 1, 1)
            .unwrap();
        let first_usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        let first_message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "base",
                None,
                None,
            )
            .unwrap();
        let first_step = db
            .create_thought_step(&session_id, 1, &haven_common::types::new_id("step"))
            .unwrap();
        let cutoff = first_message
            .created_at
            .clone()
            .max(first_step.created_at.clone());
        store
            .append_branch_point(&session_id, 1, 2, Some(&cutoff), None)
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(3));
        store
            .append_transcript(&session_id, r#"{"type":"discarded"}"#, 1, 2)
            .unwrap();
        store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "discarded",
            None,
            None,
        )
        .unwrap();
        db.create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();

        let active_usage_event = store
            .read_active_domain_events(&session_id)
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == USAGE_RECORDED_EVENT_TYPE)
            .unwrap();
        let wire: serde_json::Value = serde_json::from_str(&active_usage_event.payload).unwrap();
        assert_eq!(wire["usage_id"], first_usage.usage_id);
        assert!(wire.get("id").is_none());
        assert_eq!(wire["request_kind"], "chat");
        assert!(wire.get("role").is_none());
        let payload: LlmUsageRecord = serde_json::from_str(&active_usage_event.payload).unwrap();
        assert_eq!(payload.usage_id, first_usage.usage_id);
        assert_eq!(payload.request_kind, RequestKind::Chat);

        let expected_event_sequence = store
            .load_replay_state(&session_id)
            .unwrap()
            .unwrap()
            .cursor
            .event_sequence;
        store
            .rollback_to(
                &session_id,
                &rollback_request(
                    expected_event_sequence,
                    1,
                    2,
                    RollbackProjectionBoundary::BranchPoint,
                ),
                &[],
                None,
            )
            .unwrap();
        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(db.list_session_steps(&session_id).unwrap().len(), 1);
        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].usage_id, first_usage.usage_id);
        assert_eq!(
            db.get_session_usage(&session_id)
                .unwrap()
                .unwrap()
                .total_tokens,
            10
        );
        assert_eq!(store.read_all(&session_id).unwrap().len(), 6);
        assert_eq!(
            store.read_active_domain_events(&session_id).unwrap().len(),
            2
        );
    }

    #[test]
    fn rollback_rejects_unmappable_transcript_cursor_without_writes() {
        let (_db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"two"}"#, 1, 2)
            .unwrap();

        let result = store.rollback_to(
            &session_id,
            &rollback_request(2, 3, 2, RollbackProjectionBoundary::BranchPoint),
            &[],
            None,
        );

        assert!(result.is_err());
        assert_eq!(store.read_all(&session_id).unwrap().len(), 2);
        assert_eq!(store.read_active_transcript(&session_id).unwrap().len(), 2);
    }

    #[test]
    fn rollback_rejects_stale_event_boundary_even_when_cursor_still_maps() {
        let (_db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"two"}"#, 1, 2)
            .unwrap();
        let expected_event_sequence = store
            .load_replay_state(&session_id)
            .unwrap()
            .unwrap()
            .cursor
            .event_sequence;
        store
            .append_transcript(&session_id, r#"{"type":"three"}"#, 1, 3)
            .unwrap();

        let result = store.rollback_to(
            &session_id,
            &rollback_request(
                expected_event_sequence,
                1,
                1,
                RollbackProjectionBoundary::BranchPoint,
            ),
            &[],
            None,
        );

        assert!(result.is_err());
        assert_eq!(store.read_all(&session_id).unwrap().len(), 3);
        assert_eq!(store.read_active_transcript(&session_id).unwrap().len(), 3);
    }

    #[test]
    fn rollback_revalidates_target_message_inside_transaction() {
        let (db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"one"}"#, 1, 1)
            .unwrap();
        let message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "keep",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        let other_session = db.create_session("other session").unwrap();
        let foreign_message = db
            .add_message(
                &other_session.session_id,
                haven_common::types::CanonicalRole::User,
                "foreign",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();

        let result = store.rollback_to(
            &session_id,
            &rollback_request(
                1,
                1,
                1,
                RollbackProjectionBoundary::UserMessage {
                    message_id: foreign_message.message_id,
                },
            ),
            &[],
            None,
        );

        assert!(result.is_err());
        assert_eq!(
            db.list_session_messages(&session_id).unwrap()[0].message_id,
            message.message_id
        );
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);
    }

    #[test]
    fn projection_truncate_emits_usage_discard_compensation() {
        let (db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"base"}"#, 1, 1)
            .unwrap();
        let first = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let second = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();

        store
            .truncate_projection_after(
                &session_id,
                &ProjectionCutoff {
                    created_at: first.created_at,
                    inclusive: false,
                },
            )
            .unwrap();

        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(
            usage
                .iter()
                .map(|record| &record.usage_id)
                .collect::<Vec<_>>(),
            [&first.usage_id]
        );
        let discard = store
            .read_active_domain_events(&session_id)
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == USAGE_DISCARDED_EVENT_TYPE)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&discard.payload)
                .unwrap()
                .get("usage_id")
                .and_then(serde_json::Value::as_str),
            Some(second.usage_id.as_str())
        );
    }

    #[tokio::test]
    async fn usage_writes_reject_noncanonical_ids_before_appending_events() {
        let (db, store, session_id) = store();
        let usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        let canonical_events = store.read_all(&session_id).unwrap();

        let malformed_session_id = "ses-invalid";
        db.conn()
            .expect("test database connection checkout failed")
            .execute(
                "INSERT INTO sessions (id, input_text) VALUES (?1, 'fixture')",
                rusqlite::params![malformed_session_id],
            )
            .unwrap();

        assert!(
            store
                .append_usage_batch_cancellable(
                    malformed_session_id,
                    vec![usage_input(2, 20)],
                    None,
                )
                .await
                .is_err()
        );
        assert!(store.read_all(malformed_session_id).is_err());
        assert!(db.list_session_llm_usage(malformed_session_id).is_err());

        assert!(
            store
                .discard_usage_cancellable(&session_id, "usage-invalid", None)
                .await
                .is_err()
        );
        assert!(
            store
                .discard_usage_cancellable(malformed_session_id, &usage.usage_id, None)
                .await
                .is_err()
        );

        assert_eq!(store.read_all(&session_id).unwrap(), canonical_events);
        assert!(store.read_all(malformed_session_id).is_err());
        assert_eq!(
            db.list_session_llm_usage(&session_id)
                .unwrap()
                .iter()
                .map(|record| record.usage_id.as_str())
                .collect::<Vec<_>>(),
            [usage.usage_id.as_str()]
        );
    }

    #[tokio::test]
    async fn session_event_ports_reject_noncanonical_session_ids_before_io() {
        let (_db, store, session_id) = store();
        let invalid_session_id = "ses-short";

        assert!(
            store
                .update_session_status(invalid_session_id, SessionStatus::Paused)
                .await
                .is_err()
        );
        assert!(store.load_session_record(invalid_session_id).await.is_err());
        assert!(store.get_session_title(invalid_session_id).await.is_err());
        assert!(
            store
                .session_resume_projection(invalid_session_id)
                .await
                .is_err()
        );
        assert!(
            store
                .upsert_partial_stream(invalid_session_id, "partial")
                .await
                .is_err()
        );
        assert!(
            store
                .persist_pending_user_input(
                    invalid_session_id,
                    "pending",
                    None,
                    &[],
                    false,
                    Some("msg-short"),
                    PendingInputDisposition::FollowUp,
                    None,
                )
                .await
                .is_err()
        );
        assert!(
            store
                .list_session_ids_page_cancellable(
                    Some(invalid_session_id.to_owned()),
                    session_id.clone(),
                    CancellationToken::new(),
                )
                .await
                .is_err()
        );
        assert!(
            store
                .list_session_ids_page_cancellable(
                    None,
                    invalid_session_id.to_owned(),
                    CancellationToken::new(),
                )
                .await
                .is_err()
        );
        assert!(
            store
                .ensure_tool_step(
                    ToolStepWrite {
                        session_id: session_id.clone(),
                        step_number: 1,
                        tool_index: 0,
                        tool_name: "read".into(),
                        tool_input: "{}".into(),
                        tool_call_id: None,
                        is_high_risk: false,
                        silent: false,
                        step_id: "step-short".into(),
                    },
                    None,
                )
                .await
                .is_err()
        );

        assert!(
            store
                .append_domain_event(invalid_session_id, "memory_trigger", "{}")
                .await
                .is_err()
        );
        assert!(
            store
                .append_domain_event_batch_with_session_status(
                    invalid_session_id,
                    SessionStatus::Pending,
                    SessionStatus::Paused,
                    &[SessionEventInput::new("interaction_cleared", "{}")],
                )
                .await
                .is_err()
        );
        assert!(
            store
                .commit_transcript_cancellable(
                    invalid_session_id,
                    SessionCommitted::default(),
                    None,
                )
                .await
                .is_err()
        );
        assert!(
            store
                .seed_if_empty_async(invalid_session_id, Vec::new())
                .await
                .is_err()
        );
        assert!(
            store
                .append_transcript_async(invalid_session_id, "{}", 1, 1)
                .await
                .is_err()
        );
        assert!(
            store
                .load_replay_state_async(invalid_session_id)
                .await
                .is_err()
        );
        assert!(
            store
                .event_boundary_cursor(invalid_session_id, None)
                .await
                .is_err()
        );
        assert!(
            store
                .latest_sequence_cancellable(invalid_session_id, CancellationToken::new())
                .await
                .is_err()
        );
        assert!(
            store
                .replay_page_cancellable(invalid_session_id, 0, CancellationToken::new())
                .await
                .is_err()
        );
        assert!(
            store
                .read_active_events_async(invalid_session_id)
                .await
                .is_err()
        );
        assert!(store.read_all(invalid_session_id).is_err());
        assert!(store.read_all(&session_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn commit_rejects_noncanonical_projection_ids_before_event_write() {
        let (db, store, session_id) = store();
        let mut assistant_message = SessionCommitted::transcript("{}", 1, 1);
        assistant_message.project_assistant_message("msg-short", "assistant", None);
        let mut thought_step = SessionCommitted::transcript("{}", 1, 1);
        thought_step.project_thought_step("step-short", 1);
        let mut wrong_prefix_thought = SessionCommitted::transcript("{}", 1, 1);
        wrong_prefix_thought.project_thought_step("fact-00000000000000000000000000000001", 1);
        let mut tool_step = SessionCommitted::transcript("{}", 1, 1);
        tool_step.project_tool_call_step("step-short", 1, 0, "read", "{}", None, false, false);
        let mut wrong_prefix_tool_step = SessionCommitted::transcript("{}", 1, 1);
        wrong_prefix_tool_step.project_tool_call_step(
            "msg-00000000000000000000000000000001",
            1,
            0,
            "read",
            "{}",
            None,
            false,
            false,
        );
        let mut pending_input = SessionCommitted::transcript("{}", 1, 1);
        pending_input.acknowledge_pending_user_input("msg-short");

        for committed in [
            assistant_message,
            thought_step,
            wrong_prefix_thought,
            tool_step,
            wrong_prefix_tool_step,
            pending_input,
        ] {
            assert!(
                store
                    .commit_transcript_cancellable(&session_id, committed, None)
                    .await
                    .is_err()
            );
            assert!(store.read_all(&session_id).unwrap().is_empty());
            assert!(db.list_session_messages(&session_id).unwrap().is_empty());
            assert!(db.list_session_steps(&session_id).unwrap().is_empty());
        }

        let message_id = haven_common::types::new_id("msg");
        let mut supplement_thought = SessionCommitted::transcript("{}", 1, 1);
        supplement_thought.project_thought_step(message_id.clone(), 1);
        store
            .commit_transcript(&session_id, &supplement_thought)
            .unwrap();
        assert!(
            db.list_session_steps(&session_id)
                .unwrap()
                .iter()
                .any(|step| step.step_id == message_id)
        );
    }

    #[tokio::test]
    async fn rollback_rejects_noncanonical_user_message_id_before_mutation() {
        let (_db, store, session_id) = store();
        store
            .append_transcript_async(&session_id, "{}", 1, 1)
            .await
            .unwrap();

        let result = store
            .rollback_to_async(
                &session_id,
                RollbackRequest {
                    expected_event_sequence: 1,
                    transcript_cursor: 1,
                    target_step: 1,
                    projection_boundary: RollbackProjectionBoundary::UserMessage {
                        message_id: "step-00000000000000000000000000000001".into(),
                    },
                },
                Vec::new(),
                None,
            )
            .await;

        assert!(result.is_err());
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);
    }

    #[test]
    fn session_event_row_mapper_rejects_noncanonical_session_identity() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let invalid = conn
            .query_row(
                "SELECT 'ses-short', 1, 'event', '{}', 'now', NULL, NULL",
                [],
                session_event_from_row,
            )
            .unwrap_err();
        assert!(invalid.to_string().contains("session_events.session_id"));

        let session_id = haven_common::types::new_id("ses");
        let event = conn
            .query_row(
                "SELECT ?1, 1, 'event', '{}', 'now', NULL, NULL",
                [&session_id],
                session_event_from_row,
            )
            .unwrap();
        assert_eq!(event.session_id, session_id);
    }

    #[test]
    fn projection_truncate_after_step_uses_active_branch_cutoff_and_publishes_discard() {
        let (db, store, session_id) = store();
        let kept_message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "kept",
                None,
                None,
            )
            .unwrap();
        let kept_step = db
            .create_thought_step(&session_id, 1, &haven_common::types::new_id("step"))
            .unwrap();
        let kept_usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        store
            .append_branch_point(&session_id, 2, 2, Some(&kept_usage.created_at), None)
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "discarded",
            None,
            None,
        )
        .unwrap();
        db.create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();
        let discarded_usage = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();
        // Prime the message cache so the post-commit invalidation is covered.
        assert_eq!(db.list_session_messages(&session_id).unwrap().len(), 2);
        let mut live = store.subscribe();

        store
            .truncate_projection_after_step(&session_id, 2)
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, kept_message.message_id);
        let steps = db.list_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step_id, kept_step.step_id);
        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(
            usage
                .iter()
                .map(|record| &record.usage_id)
                .collect::<Vec<_>>(),
            [&kept_usage.usage_id]
        );
        assert_eq!(
            db.get_session_usage(&session_id)
                .unwrap()
                .unwrap()
                .total_tokens,
            10
        );

        let discarded = store
            .read_active_domain_events(&session_id)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == USAGE_DISCARDED_EVENT_TYPE)
            .collect::<Vec<_>>();
        assert_eq!(discarded.len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&discarded[0].payload)
                .unwrap()
                .get("usage_id")
                .and_then(serde_json::Value::as_str),
            Some(discarded_usage.usage_id.as_str())
        );
        let published = live.try_recv().unwrap();
        assert_eq!(published, discarded[0]);
        assert!(live.try_recv().is_err());
    }

    #[test]
    fn projection_truncate_after_step_is_noop_without_branch_point_or_cutoff() {
        let (db, store, no_branch_point_session_id) = store();
        let no_cutoff_session_id = db.create_session("no cutoff").unwrap().session_id;

        for (session_id, branch_point) in [
            (&no_branch_point_session_id, false),
            (&no_cutoff_session_id, true),
        ] {
            db.add_message(
                session_id,
                haven_common::types::CanonicalRole::Assistant,
                "kept",
                None,
                None,
            )
            .unwrap();
            let step_id = haven_common::types::new_id("step");
            db.create_thought_step(session_id, 1, &step_id).unwrap();
            store.append_usage(session_id, &usage_input(1, 10)).unwrap();
            if branch_point {
                store
                    .append_branch_point(session_id, 1, 2, None, None)
                    .unwrap();
            }

            let message_ids = db
                .list_session_messages(session_id)
                .unwrap()
                .into_iter()
                .map(|message| message.message_id)
                .collect::<Vec<_>>();
            let step_ids = db
                .list_session_steps(session_id)
                .unwrap()
                .into_iter()
                .map(|step| step.step_id)
                .collect::<Vec<_>>();
            let usage_ids = db
                .list_session_llm_usage(session_id)
                .unwrap()
                .into_iter()
                .map(|record| record.usage_id)
                .collect::<Vec<_>>();

            store.truncate_projection_after_step(session_id, 1).unwrap();

            assert_eq!(
                db.list_session_messages(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|message| message.message_id)
                    .collect::<Vec<_>>(),
                message_ids
            );
            assert_eq!(
                db.list_session_steps(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|step| step.step_id)
                    .collect::<Vec<_>>(),
                step_ids
            );
            assert_eq!(
                db.list_session_llm_usage(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|record| record.usage_id)
                    .collect::<Vec<_>>(),
                usage_ids
            );
            assert!(
                store
                    .read_active_domain_events(session_id)
                    .unwrap()
                    .iter()
                    .all(|event| event.event_type != USAGE_DISCARDED_EVENT_TYPE)
            );
        }
    }

    #[test]
    fn committed_recovery_marker_truncates_projection_and_compensates_usage() {
        let (db, store, session_id) = store();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "kept",
            None,
            None,
        )
        .unwrap();
        let kept_step = db
            .create_thought_step(&session_id, 1, &haven_common::types::new_id("step"))
            .unwrap();
        let kept_usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        store
            .append_branch_point(&session_id, 1, 2, Some(&kept_usage.created_at), None)
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        let discarded_message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "discarded",
                None,
                None,
            )
            .unwrap();
        db.create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();
        let discarded_usage = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();
        append_recovery_marker(&store, &session_id, "committed");
        assert_eq!(db.list_session_messages(&session_id).unwrap().len(), 2);
        let mut live = store.subscribe();

        store
            .truncate_projection_after_latest_committed_recovery(&session_id)
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_ne!(messages[0].message_id, discarded_message.message_id);
        let steps = db.list_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step_id, kept_step.step_id);
        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(
            usage
                .iter()
                .map(|record| &record.usage_id)
                .collect::<Vec<_>>(),
            [&kept_usage.usage_id]
        );
        assert_eq!(
            db.get_session_usage(&session_id)
                .unwrap()
                .unwrap()
                .total_tokens,
            10
        );
        let discard = store
            .read_active_domain_events(&session_id)
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == USAGE_DISCARDED_EVENT_TYPE)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&discard.payload).unwrap()["usage_id"],
            discarded_usage.usage_id
        );
        assert_eq!(live.try_recv().unwrap(), discard);
        assert!(live.try_recv().is_err());
    }

    #[test]
    fn latest_failed_marker_outside_active_replay_overrides_earlier_commit() {
        let (db, store, session_id) = store();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "kept",
            None,
            None,
        )
        .unwrap();
        let kept_step = db
            .create_thought_step(&session_id, 1, &haven_common::types::new_id("step"))
            .unwrap();
        let kept_usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        let branch_point = store
            .append_branch_point(&session_id, 1, 2, Some(&kept_usage.created_at), None)
            .unwrap();
        let committed = store
            .append_recovery_persistence(
                &session_id,
                1,
                2,
                "committed",
                RecoveryPersistenceStatus {
                    branch_point: true,
                    partial_messages: true,
                    projection: true,
                    event_boundary: true,
                },
            )
            .unwrap();
        append_recovery_marker(&store, &session_id, "failed");
        // Hide the failed marker from active replay while keeping the earlier
        // committed marker active. Recovery authorization must use full log
        // order, not the current active view.
        store
            .append_rollback(&session_id, committed.sequence, 2, None)
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(5));
        let discarded_message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "must remain",
                None,
                None,
            )
            .unwrap();
        let discarded_step = db
            .create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();
        let discarded_usage = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();
        let active_markers = store
            .read_active_domain_events(&session_id)
            .unwrap()
            .into_iter()
            .filter(|event| event.event_type == RECOVERY_PERSISTENCE_EVENT_TYPE)
            .collect::<Vec<_>>();
        assert_eq!(active_markers.len(), 1);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&active_markers[0].payload).unwrap()["phase"],
            "committed"
        );
        let latest = store
            .latest_recovery_persistence(&session_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&latest.payload).unwrap()["phase"],
            "failed"
        );

        store
            .truncate_projection_after_latest_committed_recovery(&session_id)
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .any(|message| message.message_id == discarded_message.message_id)
        );
        let steps = db.list_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps.iter().any(|step| step.step_id == kept_step.step_id));
        assert!(
            steps
                .iter()
                .any(|step| step.step_id == discarded_step.step_id)
        );
        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(usage.len(), 2);
        assert!(
            usage
                .iter()
                .any(|record| record.usage_id == kept_usage.usage_id)
        );
        assert!(
            usage
                .iter()
                .any(|record| record.usage_id == discarded_usage.usage_id)
        );
        assert_eq!(
            db.get_session_usage(&session_id)
                .unwrap()
                .unwrap()
                .total_tokens,
            30
        );
        assert!(
            store
                .read_active_domain_events(&session_id)
                .unwrap()
                .iter()
                .all(|event| event.event_type != USAGE_DISCARDED_EVENT_TYPE)
        );
        assert!(
            store
                .read_all(&session_id)
                .unwrap()
                .iter()
                .any(|event| event.sequence == branch_point.sequence)
        );
    }

    #[test]
    fn latest_malformed_marker_outside_active_replay_blocks_earlier_commit() {
        let (db, store, session_id) = store();
        db.add_message(
            &session_id,
            haven_common::types::CanonicalRole::Assistant,
            "kept",
            None,
            None,
        )
        .unwrap();
        let kept_usage = store
            .append_usage(&session_id, &usage_input(1, 10))
            .unwrap();
        store
            .append_branch_point(&session_id, 1, 2, Some(&kept_usage.created_at), None)
            .unwrap();
        let committed = store
            .append_recovery_persistence(
                &session_id,
                1,
                2,
                "committed",
                RecoveryPersistenceStatus {
                    branch_point: true,
                    partial_messages: true,
                    projection: true,
                    event_boundary: true,
                },
            )
            .unwrap();
        store
            .append(
                &session_id,
                RECOVERY_PERSISTENCE_EVENT_TYPE,
                r#"{"phase":false}"#,
                Some(1),
                Some(2),
            )
            .unwrap();
        store
            .append_rollback(&session_id, committed.sequence, 2, None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let later_message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "must remain",
                None,
                None,
            )
            .unwrap();
        let later_step = db
            .create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();
        let later_usage = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();

        store
            .truncate_projection_after_latest_committed_recovery(&session_id)
            .unwrap();

        assert!(
            db.list_session_messages(&session_id)
                .unwrap()
                .iter()
                .any(|message| message.message_id == later_message.message_id)
        );
        assert!(
            db.list_session_steps(&session_id)
                .unwrap()
                .iter()
                .any(|step| step.step_id == later_step.step_id)
        );
        assert!(
            db.list_session_llm_usage(&session_id)
                .unwrap()
                .iter()
                .any(|record| record.usage_id == later_usage.usage_id)
        );
        assert!(
            store
                .read_active_domain_events(&session_id)
                .unwrap()
                .iter()
                .all(|event| event.event_type != USAGE_DISCARDED_EVENT_TYPE)
        );
    }

    #[test]
    fn recovery_truncation_is_noop_without_marker_or_valid_cutoff() {
        let (db, store, first_session_id) = store();
        let no_marker = first_session_id;
        let no_branch_point = db.create_session("no branch point").unwrap().session_id;
        let no_cutoff = db.create_session("no cutoff").unwrap().session_id;

        for (session_id, has_marker, branch_point_cutoff) in [
            (no_marker.as_str(), false, Some(true)),
            (no_branch_point.as_str(), true, None),
            (no_cutoff.as_str(), true, Some(false)),
        ] {
            db.add_message(
                session_id,
                haven_common::types::CanonicalRole::Assistant,
                "anchor",
                None,
                None,
            )
            .unwrap();
            db.create_thought_step(session_id, 1, &haven_common::types::new_id("step"))
                .unwrap();
            let anchor_usage = store.append_usage(session_id, &usage_input(1, 10)).unwrap();
            if let Some(has_cutoff) = branch_point_cutoff {
                store
                    .append_branch_point(
                        session_id,
                        1,
                        2,
                        has_cutoff.then_some(anchor_usage.created_at.as_str()),
                        None,
                    )
                    .unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            db.add_message(
                session_id,
                haven_common::types::CanonicalRole::Assistant,
                "later",
                None,
                None,
            )
            .unwrap();
            db.create_thought_step(session_id, 2, &haven_common::types::new_id("step"))
                .unwrap();
            store.append_usage(session_id, &usage_input(2, 20)).unwrap();
            if has_marker {
                append_recovery_marker(&store, session_id, "committed");
            }
            let message_ids = db
                .list_session_messages(session_id)
                .unwrap()
                .into_iter()
                .map(|message| message.message_id)
                .collect::<Vec<_>>();
            let step_ids = db
                .list_session_steps(session_id)
                .unwrap()
                .into_iter()
                .map(|step| step.step_id)
                .collect::<Vec<_>>();
            let usage_ids = db
                .list_session_llm_usage(session_id)
                .unwrap()
                .into_iter()
                .map(|record| record.usage_id)
                .collect::<Vec<_>>();

            store
                .truncate_projection_after_latest_committed_recovery(session_id)
                .unwrap();

            assert_eq!(
                db.list_session_messages(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|message| message.message_id)
                    .collect::<Vec<_>>(),
                message_ids
            );
            assert_eq!(
                db.list_session_steps(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|step| step.step_id)
                    .collect::<Vec<_>>(),
                step_ids
            );
            assert_eq!(
                db.list_session_llm_usage(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|record| record.usage_id)
                    .collect::<Vec<_>>(),
                usage_ids
            );
            assert!(
                store
                    .read_active_domain_events(session_id)
                    .unwrap()
                    .iter()
                    .all(|event| event.event_type != USAGE_DISCARDED_EVENT_TYPE)
            );
        }
    }

    #[test]
    fn compact_summary_is_a_durable_active_replay_boundary() {
        let (_db, store, session_id) = store();
        store
            .append_transcript(&session_id, r#"{"type":"old","n":1}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"old","n":2}"#, 1, 2)
            .unwrap();
        let root = store
            .append_transcript(
                &session_id,
                r#"{"type":"compact_summary","summary":"root"}"#,
                1,
                3,
            )
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"new","n":4}"#, 1, 4)
            .unwrap();

        let active = store.read_active_transcript(&session_id).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].sequence, root.sequence);
        assert_eq!(active[1].payload, r#"{"type":"new","n":4}"#);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 4);
    }

    #[test]
    fn session_execution_workspace_survives_rollback_and_compaction() {
        let (_db, store, session_id) = store();
        let binding = store
            .append(
                &session_id,
                SESSION_EXECUTION_WORKSPACE_EVENT_TYPE,
                r#"{"root":"C:\\Haven\\Workspace"}"#,
                None,
                None,
            )
            .unwrap();
        let mode_binding = store
            .append(
                &session_id,
                SESSION_EXECUTION_MODE_EVENT_TYPE,
                r#"{"mode":"unattended"}"#,
                None,
                None,
            )
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"first"}"#, 1, 1)
            .unwrap();
        let rollback = store
            .append(
                &session_id,
                TIMELINE_ROLLBACK_EVENT_TYPE,
                r#"{"to_sequence":0}"#,
                None,
                None,
            )
            .unwrap();
        assert_eq!(rollback.event_type, TIMELINE_ROLLBACK_EVENT_TYPE);
        let active_after_rollback = store.read_active(&session_id).unwrap();
        assert!(active_after_rollback.iter().any(|event| {
            event.sequence == binding.sequence
                && event.event_type == SESSION_EXECUTION_WORKSPACE_EVENT_TYPE
        }));
        assert!(active_after_rollback.iter().any(|event| {
            event.sequence == mode_binding.sequence
                && event.event_type == SESSION_EXECUTION_MODE_EVENT_TYPE
        }));
        assert!(
            active_after_rollback.iter().all(|event| matches!(
                event.event_type.as_str(),
                SESSION_EXECUTION_WORKSPACE_EVENT_TYPE | SESSION_EXECUTION_MODE_EVENT_TYPE
            )),
            "rollback markers are audit history and do not appear in the active replay"
        );

        let compact = store
            .append_transcript(
                &session_id,
                r#"{"type":"compact_summary","summary":"root"}"#,
                2,
                1,
            )
            .unwrap();
        let active_after_compaction = store.read_active(&session_id).unwrap();
        assert!(active_after_compaction.iter().any(|event| {
            event.sequence == binding.sequence
                && event.event_type == SESSION_EXECUTION_WORKSPACE_EVENT_TYPE
        }));
        assert!(active_after_compaction.iter().any(|event| {
            event.sequence == mode_binding.sequence
                && event.event_type == SESSION_EXECUTION_MODE_EVENT_TYPE
        }));
        assert!(
            active_after_compaction
                .iter()
                .any(|event| event.sequence == compact.sequence)
        );
    }

    #[test]
    fn event_cursor_matches_active_transcript_across_rollbacks_and_compaction() {
        let (_db, store, session_id) = store();

        let assert_cursor_matches_active = || {
            let expected = store.read_active_transcript(&session_id).unwrap().len();
            assert_eq!(store.cursor(&session_id).unwrap().event_cursor, expected);
        };

        store
            .append_transcript(&session_id, r#"{"type":"first"}"#, 1, 1)
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"second"}"#, 1, 2)
            .unwrap();
        assert_cursor_matches_active();

        store
            .append(
                &session_id,
                TIMELINE_ROLLBACK_EVENT_TYPE,
                r#"{"to_sequence":1}"#,
                None,
                None,
            )
            .unwrap();
        assert_cursor_matches_active();

        store
            .append_transcript(&session_id, r#"{"type":"third"}"#, 1, 3)
            .unwrap();
        let compact_root = store
            .append_transcript(
                &session_id,
                r#"{"type":"compact_summary","summary":"root"}"#,
                1,
                4,
            )
            .unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"after_compaction"}"#, 1, 5)
            .unwrap();
        assert_cursor_matches_active();

        // A late rollback before the compact root forces full replay. The
        // cursor-only path must retain the same compaction and rollback rules.
        let rollback_payload = format!(r#"{{"to_sequence":{}}}"#, compact_root.sequence - 1);
        store
            .append(
                &session_id,
                TIMELINE_ROLLBACK_EVENT_TYPE,
                &rollback_payload,
                None,
                None,
            )
            .unwrap();
        assert_cursor_matches_active();
    }

    /// Replay benchmark for the recovery boundary. Keep full-log and
    /// active-log reads on the same database so the samples expose the
    /// compaction boundary without including fixture setup or filesystem I/O.
    #[test]
    fn active_replay_boundary_benchmark_1k_10k_100k() {
        const WARMUP_READS: usize = 2;
        const MEASURED_PAIRS: usize = 21;

        fn percentile(samples: &[u128], percentile: usize) -> u128 {
            let mut sorted = samples.to_vec();
            sorted.sort_unstable();
            let rank = (percentile * sorted.len()).div_ceil(100);
            sorted[rank - 1]
        }

        for count in [1_000usize, 10_000, 100_000] {
            let (_db, store, session_id) = store();
            let mut events = Vec::with_capacity(count);
            for index in 0..count {
                events.push(SessionEventInput::transcript(
                    format!(r#"{{"type":"old","n":{index}}}"#),
                    1,
                    1,
                ));
            }
            // Transcript batches are deliberately bounded for live writes;
            // build the long history through the same bounded append path.
            for chunk in events.chunks(MAX_SESSION_EVENT_BATCH_EVENTS) {
                store.append_batch(&session_id, chunk).unwrap();
            }
            store
                .append_transcript(
                    &session_id,
                    r#"{"type":"compact_summary","summary":"root"}"#,
                    1,
                    2,
                )
                .unwrap();
            store
                .append_transcript(&session_id, r#"{"type":"new"}"#, 1, 3)
                .unwrap();

            for _ in 0..WARMUP_READS {
                assert_eq!(store.read_all(&session_id).unwrap().len(), count + 2);
                assert_eq!(store.read_active(&session_id).unwrap().len(), 2);
            }

            let mut full_samples_us = Vec::with_capacity(MEASURED_PAIRS);
            let mut active_samples_us = Vec::with_capacity(MEASURED_PAIRS);
            for pair in 0..MEASURED_PAIRS {
                // Alternate which read goes first to reduce a fixed order
                // bias after the shared in-memory fixture has been warmed.
                let read_full = || {
                    let started = Instant::now();
                    let events = store.read_all(&session_id).unwrap();
                    let elapsed_us = started.elapsed().as_micros();
                    assert_eq!(events.len(), count + 2);
                    elapsed_us
                };
                let read_active = || {
                    let started = Instant::now();
                    let events = store.read_active(&session_id).unwrap();
                    let elapsed_us = started.elapsed().as_micros();
                    assert_eq!(events.len(), 2);
                    elapsed_us
                };

                let (full_elapsed_us, active_elapsed_us) = if pair % 2 == 0 {
                    (read_full(), read_active())
                } else {
                    let active_elapsed_us = read_active();
                    let full_elapsed_us = read_full();
                    (full_elapsed_us, active_elapsed_us)
                };
                full_samples_us.push(full_elapsed_us);
                active_samples_us.push(active_elapsed_us);
            }

            println!(
                "PERF_BASELINE area=session_event_replay profile=test fixture=in_memory_sqlite history_events={count} active_events=2 warmup_reads_per_mode={WARMUP_READS} measured_pairs={MEASURED_PAIRS} unit=us full_p50_us={} full_p95_us={} active_p50_us={} active_p95_us={}",
                percentile(&full_samples_us, 50),
                percentile(&full_samples_us, 95),
                percentile(&active_samples_us, 50),
                percentile(&active_samples_us, 95),
            );
        }
    }

    #[test]
    fn rejects_invalid_payload() {
        let (_db, store, session_id) = store();
        let error = store
            .append(&session_id, "transcript", "not-json", None, None)
            .unwrap_err();
        assert!(error.to_string().contains("valid JSON"));
    }

    #[test]
    fn branch_points_replay_as_latest_control_metadata() {
        let (_db, store, session_id) = store();
        store
            .append_branch_point(&session_id, 1, 3, Some("2026-01-01T00:00:00Z"), None)
            .unwrap();
        store
            .append_branch_point(&session_id, 2, 3, Some("2026-01-01T00:00:01Z"), None)
            .unwrap();
        let points = store.read_active_branch_points(&session_id).unwrap();
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].event_cursor, 2);
        assert_eq!(points[0].step_number, 3);
        assert_eq!(
            points[0].last_msg_at.as_deref(),
            Some("2026-01-01T00:00:01Z")
        );
    }

    #[test]
    fn committed_events_are_published_after_commit() {
        let (_db, store, session_id) = store();
        let mut receiver = store.subscribe();
        let written = store
            .append_transcript(&session_id, r#"{"type":"live"}"#, 4, 9)
            .unwrap();
        let published = receiver.try_recv().unwrap();
        assert_eq!(published, written);
    }

    #[test]
    fn session_committed_commits_events_and_projection_intents_together() {
        let (db, store, session_id) = store();
        let mut receiver = store.subscribe();
        let message_id = haven_common::types::new_id("step");
        let tool_run_id = haven_common::types::new_id("step");
        let mut committed = SessionCommitted::transcript(
            format!(r#"{{"type":"thought","message_id":"{message_id}"}}"#),
            2,
            3,
        );
        committed.project_assistant_message(
            message_id.clone(),
            "thinking",
            Some(haven_common::types::TranscriptMessageKind::Text),
        );
        committed.project_thought_step(message_id.clone(), 3);
        committed.project_tool_call_step(
            tool_run_id.clone(),
            3,
            0,
            "echo",
            "{}",
            Some("call-batch".into()),
            false,
            false,
        );

        let result = store.commit_transcript(&session_id, &committed).unwrap();

        assert_eq!(result.events.len(), 1);
        assert!(result.lock_wait_ms < 1_000);
        assert_eq!(receiver.try_recv().unwrap(), result.events[0]);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);
        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|row| row.message_id == message_id)
                .count(),
            1
        );
        let steps = db.list_session_steps(&session_id).unwrap();
        assert!(steps.iter().any(|step| step.step_id == tool_run_id));
    }

    #[tokio::test]
    async fn cancellable_session_commit_port_writes_with_existing_store_semantics() {
        let (db, store, session_id) = store();
        let message_id = haven_common::types::new_id("step");
        let mut committed = SessionCommitted::transcript(r#"{"type":"port"}"#, 2, 3);
        committed.project_assistant_message(
            message_id,
            "port write",
            Some(haven_common::types::TranscriptMessageKind::Text),
        );

        let result = store
            .commit_transcript_cancellable(&session_id, committed, Some(CancellationToken::new()))
            .await
            .unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);
        assert_eq!(
            db.list_session_messages(&session_id).unwrap()[0].content,
            "port write"
        );
    }

    #[tokio::test]
    async fn cancellable_session_commit_port_preserves_empty_result() {
        let (_db, store, session_id) = store();

        let result = store
            .commit_transcript_cancellable(&session_id, SessionCommitted::default(), None)
            .await
            .unwrap();

        assert!(result.events.is_empty());
        assert!(result.message_created_at.is_empty());
        assert_eq!(result.lock_wait_ms, 0);
        assert_eq!(result.cursor, SessionCursor::default());
    }

    #[tokio::test]
    async fn cancellable_session_commit_port_errors_for_missing_session() {
        let (_db, store, _session_id) = store();
        let missing_session_id = haven_common::types::new_id("ses");
        let committed = SessionCommitted::transcript(r#"{"type":"missing-session"}"#, 1, 1);

        let result = store
            .commit_transcript_cancellable(
                &missing_session_id,
                committed,
                Some(CancellationToken::new()),
            )
            .await;

        assert!(result.is_err());
        assert!(store.read_all(&missing_session_id).unwrap().is_empty());
    }

    #[tokio::test]
    async fn committed_thought_message_and_step_share_id_and_rollback_clocks() {
        let (db, store, session_id) = store();
        let baseline = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "keep",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        let (_, branch_cursor) = store
            .append_branch_point_from_projection(&session_id, 1, Some(5))
            .unwrap();
        assert_eq!(branch_cursor.event_cursor, 0);
        assert_eq!(
            branch_cursor.last_msg_at.as_deref(),
            Some(baseline.created_at.as_str())
        );
        std::thread::sleep(std::time::Duration::from_millis(5));

        let message_id = haven_common::types::new_id("step");
        let mut committed = SessionCommitted::transcript(
            format!(
                r#"{{"type":"thought","step_number":1,"text":"keep thought","message_id":"{message_id}"}}"#
            ),
            6,
            1,
        );
        committed.project_assistant_message(
            message_id.clone(),
            "keep thought",
            Some(haven_common::types::TranscriptMessageKind::Text),
        );
        let result = store.commit_transcript(&session_id, &committed).unwrap();
        store
            .create_thought_step(&session_id, 1, &message_id)
            .await
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        let steps = db.list_session_steps(&session_id).unwrap();
        assert!(
            messages
                .iter()
                .any(|message| message.message_id == message_id)
        );
        assert!(steps.iter().any(|step| step.step_id == message_id));

        let rollback = store
            .rollback_to_async(
                &session_id,
                RollbackRequest {
                    expected_event_sequence: result.cursor.event_sequence,
                    transcript_cursor: branch_cursor.event_cursor,
                    target_step: 1,
                    projection_boundary: RollbackProjectionBoundary::BranchPoint,
                },
                Vec::new(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(rollback.to_sequence, 1);
        assert_eq!(store.read_active_transcript(&session_id).unwrap().len(), 0);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 3);
        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, baseline.message_id);
        assert!(db.list_session_steps(&session_id).unwrap().is_empty());
    }

    #[test]
    fn branch_projection_watermark_covers_messages_steps_and_usage() {
        let (db, store, session_id) = store();
        let baseline = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "baseline",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        let before_step = db
            .create_tool_step(&session_id, 1, "read_file", "{}", false, false, None, None)
            .unwrap();
        let before_usage = store.append_usage(&session_id, &usage_input(1, 5)).unwrap();
        let (_, branch_cursor) = store
            .append_branch_point_from_projection(&session_id, 2, None)
            .unwrap();
        let cutoff = branch_cursor.last_msg_at.as_deref().unwrap();
        assert_eq!(cutoff, before_usage.created_at);
        assert!(cutoff >= baseline.created_at.as_str());
        assert!(cutoff >= before_step.created_at.as_str());
        assert!(cutoff >= before_usage.created_at.as_str());

        let after_step = db
            .create_tool_step(&session_id, 2, "shell", "{}", false, false, None, None)
            .unwrap();
        let after_usage = store.append_usage(&session_id, &usage_input(2, 7)).unwrap();
        let after_message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::Assistant,
                "after branch",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();
        assert!(after_step.created_at.as_str() > cutoff);
        assert!(after_usage.created_at.as_str() > cutoff);
        assert!(after_message.created_at.as_str() > cutoff);

        store
            .truncate_projection_after_step(&session_id, 2)
            .unwrap();

        let messages = db.list_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, baseline.message_id);
        let steps = db.list_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].step_id, before_step.step_id);
        let usage = db.list_session_llm_usage(&session_id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].usage_id, before_usage.usage_id);
    }

    #[test]
    fn projection_watermark_rejects_noncanonical_timestamps() {
        let (db, store, session_id) = store();
        let message = db
            .add_message(
                &session_id,
                haven_common::types::CanonicalRole::User,
                "baseline",
                Some(haven_common::types::TranscriptMessageKind::Text),
                None,
            )
            .unwrap();

        for timestamp in [
            "2099-03-04T05:06:07.123456789+00:00",
            "2099-03-04T05:06:07.123+00:00",
            "2099-03-04T05:06:07Z",
        ] {
            db.conn()
                .expect("test database connection checkout failed")
                .execute(
                    "UPDATE messages SET created_at = ?1 WHERE id = ?2",
                    rusqlite::params![timestamp, message.message_id],
                )
                .unwrap();
            assert!(
                store
                    .append_branch_point_from_projection(&session_id, 2, None)
                    .is_err(),
                "accepted noncanonical timestamp {timestamp}"
            );
        }
    }

    #[test]
    fn session_commit_rolls_back_event_when_projection_fails() {
        let (db, store, session_id) = store();
        let mut live = store.subscribe();
        let failed_message_id = haven_common::types::new_id("msg");
        let conn = db.conn().expect("test database connection checkout failed");
        conn.execute_batch(&format!(
            "CREATE TRIGGER fail_session_projection BEFORE INSERT ON messages
             WHEN NEW.id = '{failed_message_id}'
             BEGIN SELECT RAISE(ABORT, 'projection failed'); END;"
        ))
        .unwrap();
        drop(conn);
        let mut committed = SessionCommitted::transcript(r#"{"type":"rollback"}"#, 1, 1);
        committed.project_assistant_message(&failed_message_id, "must fail", None);

        let error = store
            .commit_transcript(&session_id, &committed)
            .unwrap_err();

        assert!(error.to_string().contains("projection failed"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
        assert!(db.list_session_messages(&session_id).unwrap().is_empty());
        assert!(matches!(
            live.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn session_commit_writes_event_before_assistant_projection() {
        let (db, store, session_id) = store();
        let conn = db.conn().expect("test database connection checkout failed");
        conn.execute_batch(
            "CREATE TRIGGER assert_transcript_before_message BEFORE INSERT ON messages
             WHEN NOT EXISTS (
                 SELECT 1 FROM session_events
                 WHERE session_id = NEW.session_id AND event_type = 'transcript'
                   AND json_extract(payload, '$.type') = 'thought'
             )
             BEGIN SELECT RAISE(ABORT, 'transcript event missing before message projection'); END;",
        )
        .unwrap();
        drop(conn);
        let message_id = haven_common::types::new_id("step");
        let mut committed = SessionCommitted::transcript(
            format!(r#"{{"type":"thought","message_id":"{message_id}"}}"#),
            3,
            4,
        );
        committed.project_assistant_message(
            &message_id,
            "persist after event",
            Some(haven_common::types::TranscriptMessageKind::Text),
        );

        let result = store.commit_transcript(&session_id, &committed).unwrap();

        assert_eq!(result.events[0].sequence, 1);
        assert_eq!(
            db.list_session_messages(&session_id).unwrap()[0].message_id,
            message_id
        );
    }

    #[test]
    fn session_commit_budget_includes_serialization_overhead() {
        let (_db, store, session_id) = store();
        let content = "x".repeat(MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES - 80);
        let committed = SessionCommitted::transcript(
            format!(r#"{{"type":"overhead","content":"{content}"}}"#),
            1,
            1,
        );
        assert!(committed.events[0].payload.len() < MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES);
        assert!(
            SessionStore::payload_bytes(&committed)
                .unwrap_err()
                .to_string()
                .contains("payload bytes")
        );

        let error = store
            .commit_transcript(&session_id, &committed)
            .unwrap_err();
        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
    }

    #[test]
    fn generic_transcript_append_cannot_bypass_batch_budget() {
        let (_db, store, session_id) = store();
        let oversized = "x".repeat(MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES);
        let event = SessionEventInput::transcript(
            format!(r#"{{"type":"oversized","content":"{oversized}"}}"#),
            1,
            1,
        );

        let error = store.append_batch(&session_id, &[event]).unwrap_err();

        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
    }

    #[test]
    fn session_commit_rejects_oversized_event_and_projection_payloads() {
        let (db, store, session_id) = store();
        let oversized = "x".repeat(MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES);
        let oversized_event = SessionCommitted::transcript(
            format!(r#"{{"type":"oversized","content":"{oversized}"}}"#),
            1,
            1,
        );
        let error = store
            .commit_transcript(&session_id, &oversized_event)
            .unwrap_err();
        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());

        let mut oversized_projection = SessionCommitted::transcript(r#"{"type":"action"}"#, 1, 1);
        oversized_projection.project_tool_call_step(
            haven_common::types::new_id("step"),
            1,
            0,
            "tool",
            oversized,
            None,
            false,
            false,
        );
        let error = store
            .commit_transcript(&session_id, &oversized_projection)
            .unwrap_err();
        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
        assert!(db.list_session_steps(&session_id).unwrap().is_empty());
    }

    #[test]
    fn session_commit_handles_a_64_event_burst_in_order() {
        let (_db, store, session_id) = store();
        let committed = SessionCommitted {
            events: (0..64)
                .map(|index| {
                    SessionCommittedEvent::new(
                        format!(r#"{{"type":"burst","index":{index}}}"#),
                        9,
                        index,
                    )
                })
                .collect(),
            ..SessionCommitted::default()
        };

        let result = store
            .commit_transcript(&session_id, &committed)
            .expect("bounded transcript bursts should commit");
        assert_eq!(result.events.len(), 64);
        assert_eq!(
            result
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            (1..=64).collect::<Vec<_>>()
        );
    }

    #[test]
    fn session_committed_append_preserves_event_projection_and_trailing_event_order() {
        let first_id = haven_common::types::new_id("step");
        let second_id = haven_common::types::new_id("step");
        let mut first = SessionCommitted::transcript("first", 3, 1);
        first.project_thought_step(first_id.clone(), 1);
        let mut second = SessionCommitted::transcript("second", 3, 2);
        second.project_assistant_message(
            second_id.clone(),
            "second thought",
            Some(TranscriptMessageKind::Text),
        );
        second.push_trailing_event(INTERACTION_CLEARED_EVENT_TYPE, r#"{"request_ids":[]}"#);

        first.append_commit(second);

        assert_eq!(
            first
                .events
                .iter()
                .map(|event| event.payload.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(matches!(
            first.projections.as_slice(),
            [
                SessionProjectionIntent::ThoughtStep { message_id, .. },
                SessionProjectionIntent::AssistantMessage { message_id: assistant_id, .. }
            ] if message_id == &first_id && assistant_id == &second_id
        ));
        assert_eq!(
            first
                .trailing_events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            [INTERACTION_CLEARED_EVENT_TYPE]
        );
    }

    #[test]
    fn trailing_interaction_clear_rolls_back_with_tool_result_event() {
        let (db, store, session_id) = store();
        db.conn()
            .expect("test database connection checkout failed")
            .execute_batch(
                "CREATE TRIGGER reject_confirmation_clear
                 BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'interaction_cleared'
                 BEGIN SELECT RAISE(ABORT, 'test confirmation clear failure'); END;",
            )
            .unwrap();
        let mut committed = SessionCommitted::transcript(r#"{"type":"tool_result"}"#, 1, 2);
        committed.push_trailing_event(
            INTERACTION_CLEARED_EVENT_TYPE,
            r#"{"request_ids":["conf-00000000000000000000000000000001"]}"#,
        );

        let error = store
            .commit_transcript(&session_id, &committed)
            .expect_err("the injected clear failure must abort both events");

        assert!(
            error
                .to_string()
                .contains("test confirmation clear failure")
        );
        assert!(store.read_all(&session_id).unwrap().is_empty());

        db.conn()
            .expect("test database connection checkout failed")
            .execute_batch("DROP TRIGGER reject_confirmation_clear;")
            .unwrap();
        let result = store.commit_transcript(&session_id, &committed).unwrap();
        assert_eq!(
            result
                .events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            [TRANSCRIPT_EVENT_TYPE, INTERACTION_CLEARED_EVENT_TYPE]
        );
    }

    #[test]
    fn session_commit_accepts_128_events_and_rejects_129() {
        let (_db, at_limit_store, session_id) = store();
        let at_limit = SessionCommitted {
            events: (0..MAX_SESSION_EVENT_BATCH_EVENTS)
                .map(|index| {
                    SessionCommittedEvent::new(
                        format!(r#"{{"type":"boundary","index":{index}}}"#),
                        1,
                        index as u32,
                    )
                })
                .collect(),
            ..SessionCommitted::default()
        };
        assert_eq!(
            at_limit_store
                .commit_transcript(&session_id, &at_limit)
                .unwrap()
                .events
                .len(),
            MAX_SESSION_EVENT_BATCH_EVENTS
        );

        let (_db, store, session_id) = store();
        let over_limit = SessionCommitted {
            events: (0..=MAX_SESSION_EVENT_BATCH_EVENTS)
                .map(|index| {
                    SessionCommittedEvent::new(
                        format!(r#"{{"type":"boundary","index":{index}}}"#),
                        1,
                        index as u32,
                    )
                })
                .collect(),
            ..SessionCommitted::default()
        };
        let error = store
            .commit_transcript(&session_id, &over_limit)
            .unwrap_err();
        assert!(error.to_string().contains("128 events"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
    }

    #[test]
    fn seed_is_one_way_and_does_not_publish_stale_cache_rows() {
        let (_db, store, session_id) = store();
        assert!(store.seed_if_empty(&session_id, &[]).unwrap().is_empty());
        let first = store
            .seed_if_empty(
                &session_id,
                &[SessionEventInput::new("transcript", r#"{"type":"first"}"#)],
            )
            .unwrap();
        let second = store
            .seed_if_empty(
                &session_id,
                &[SessionEventInput::new("transcript", r#"{"type":"stale"}"#)],
            )
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(second, first);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn stable_workspace_configuration_does_not_block_initial_transcript_seed() {
        let (_db, store, session_id) = store();
        store
            .append_domain_event(
                &session_id,
                SESSION_EXECUTION_WORKSPACE_EVENT_TYPE,
                r#"{"root":null}"#,
            )
            .await
            .unwrap();
        store
            .append_domain_event(
                &session_id,
                SESSION_EXECUTION_MODE_EVENT_TYPE,
                r#"{"mode":"collaborative"}"#,
            )
            .await
            .unwrap();

        assert!(
            store
                .load_replay_state_async(&session_id)
                .await
                .unwrap()
                .is_none()
        );
        let seeded = store
            .seed_if_empty_async(
                &session_id,
                vec![SessionEventInput::transcript(r#"{"type":"first"}"#, 1, 1)],
            )
            .await
            .unwrap();
        assert_eq!(seeded.len(), 1);
        let events = store.read_all(&session_id).unwrap();
        assert_eq!(
            events.len(),
            3,
            "stable session context events are preserved beside the seed"
        );
        assert_eq!(events[0].event_type, SESSION_EXECUTION_WORKSPACE_EVENT_TYPE);
        assert_eq!(events[1].event_type, SESSION_EXECUTION_MODE_EVENT_TYPE);
        assert_eq!(events[2].event_type, TRANSCRIPT_EVENT_TYPE);
        let replay = store
            .load_replay_state_async(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(replay.transcript.len(), 1);
    }

    #[test]
    fn event_rows_cannot_be_updated() {
        let (db, store, session_id) = store();
        let event = store
            .append_transcript(&session_id, r#"{"type":"immutable"}"#, 1, 1)
            .unwrap();
        let error = db
            .conn()
            .expect("test database connection checkout failed")
            .execute(
                "UPDATE session_events SET payload = ?1
                 WHERE session_id = ?2 AND sequence = ?3",
                rusqlite::params![r#"{"type":"changed"}"#, session_id, event.sequence],
            )
            .unwrap_err();
        assert!(error.to_string().contains("append-only"));
    }

    #[test]
    fn recovery_persistence_markers_are_durable_control_events() {
        let (_db, store, session_id) = store();
        store
            .append_recovery_persistence(
                &session_id,
                3,
                4,
                "failed",
                RecoveryPersistenceStatus {
                    branch_point: true,
                    partial_messages: false,
                    projection: false,
                    event_boundary: false,
                },
            )
            .unwrap();
        let marker = store
            .latest_recovery_persistence(&session_id)
            .unwrap()
            .expect("marker should be readable after commit");
        assert_eq!(marker.event_type, RECOVERY_PERSISTENCE_EVENT_TYPE);
        assert_eq!(marker.step_number, Some(4));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&marker.payload).unwrap()["phase"],
            "failed"
        );
        assert!(
            store
                .read_active_transcript(&session_id)
                .unwrap()
                .is_empty()
        );
    }
}
