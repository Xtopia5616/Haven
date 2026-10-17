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
use crate::repositories::sessions::{Session, SessionOrigin};
use crate::repositories::usage::{LlmUsageRecord, LlmUsageRecordInput, SessionUsage};
use chrono::{SecondsFormat, Utc};
use haven_common::lifecycle::SessionStatus;
use haven_common::types::{MessageAttachment, TranscriptMessageKind, is_canonical_id};
use rusqlite::OptionalExtension;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

mod session_authorization;
mod session_history;
mod session_records;
pub use session_history::{
    SessionHistoryFilter, SessionHistoryStatusFilter, SessionMessageText, SessionResumeMedia,
    SessionTitleGenerationContext,
};

pub const TRANSCRIPT_EVENT_TYPE: &str = "transcript";
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

fn session_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionEvent> {
    let session_id: String = row.get(0)?;
    if !is_canonical_id(&session_id, "ses") {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            "invalid persisted session_events.session_id".into(),
        ));
    }
    let run_id = row
        .get::<_, Option<i64>>(5)?
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Integer,
                    error.into(),
                )
            })
        })
        .transpose()?;
    Ok(SessionEvent {
        session_id,
        sequence: row.get(1)?,
        event_type: row.get(2)?,
        payload: row.get(3)?,
        created_at: row.get(4)?,
        run_id,
        step_number: row.get(6)?,
    })
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub sequence: i64,
    pub event_type: String,
    pub payload: String,
    pub created_at: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
}

/// One bounded page from the append-only session event log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventPage {
    pub events: Vec<SessionEvent>,
    /// Sequence to pass as `after_sequence` for the next page. Remains the
    /// input cursor when this page contains no events.
    pub next_cursor: i64,
    pub has_more: bool,
}

/// Maximum number of durable events returned by a single replay page.
const MAX_SESSION_EVENT_REPLAY_PAGE_SIZE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionEventInput {
    pub event_type: String,
    pub payload: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
}

#[derive(serde::Serialize)]
struct SessionEventInputRef<'a> {
    event_type: &'a str,
    payload: &'a str,
    run_id: Option<u64>,
    step_number: Option<u32>,
}

impl<'a> From<&'a SessionEventInput> for SessionEventInputRef<'a> {
    fn from(event: &'a SessionEventInput) -> Self {
        Self {
            event_type: &event.event_type,
            payload: &event.payload,
            run_id: event.run_id,
            step_number: event.step_number,
        }
    }
}

#[derive(Default)]
struct BoundedByteCounter {
    bytes: usize,
    exceeded: bool,
}

impl std::io::Write for BoundedByteCounter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next_bytes = self.bytes.saturating_add(buffer.len());
        if next_bytes > MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES {
            self.exceeded = true;
            return Err(std::io::Error::other(
                "session event payload budget exceeded",
            ));
        }
        self.bytes = next_bytes;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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

/// One transcript event submitted for durable commit.
///
/// Transcript payloads are serialized by Agent because their canonical event
/// model belongs to ReAct. The store fixes the event type and owns sequence
/// allocation, transaction boundaries, and materialized projections.
#[derive(Debug, Clone, serde::Serialize)]
struct SessionCommittedEvent {
    payload: String,
    run_id: u64,
    step_number: u32,
}

impl SessionCommittedEvent {
    fn new(payload: impl Into<String>, run_id: u64, step_number: u32) -> Self {
        Self {
            payload: payload.into(),
            run_id,
            step_number,
        }
    }
}

/// Domain projection intents that accompany a transcript commit.
///
/// These describe session facts rather than database columns: assistant chat
/// content, a thought execution step, or a tool action step. SessionStore
/// translates them into the existing materialized `messages` and
/// `session_steps` rows.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SessionProjectionIntent {
    AcknowledgePendingUserInput {
        message_id: String,
    },
    AssistantMessage {
        message_id: String,
        content: String,
        message_type: Option<TranscriptMessageKind>,
    },
    ThoughtStep {
        message_id: String,
        step_number: u32,
    },
    ToolStep {
        step_id: String,
        step_number: u32,
        tool_index: u32,
        tool_name: String,
        tool_input: String,
        tool_call_id: Option<String>,
        is_high_risk: bool,
        silent: bool,
    },
}

/// One authoritative transcript commit submitted by Agent to SessionStore.
///
/// Events are appended first inside the same SQLite transaction that
/// materializes projection intents. A failed projection rolls back the event
/// too, so no committed UI sequence can refer to an uncommitted fact.
#[derive(Debug, Clone, Default)]
pub struct SessionCommitted {
    events: Vec<SessionCommittedEvent>,
    projections: Vec<SessionProjectionIntent>,
    /// Domain control events appended after transcript events in the same
    /// transaction. Used when a transcript result also closes a lifecycle
    /// gate, so recovery cannot observe one without the other.
    trailing_events: Vec<SessionEventInput>,
}

#[derive(serde::Serialize)]
struct SessionCommittedPayload<'a> {
    events: &'a [SessionCommittedEvent],
    projections: &'a [SessionProjectionIntent],
    trailing_events: &'a [SessionEventInput],
}

impl SessionCommitted {
    pub fn transcript(payload: impl Into<String>, run_id: u64, step_number: u32) -> Self {
        Self {
            events: vec![SessionCommittedEvent::new(payload, run_id, step_number)],
            projections: Vec::new(),
            trailing_events: Vec::new(),
        }
    }

    pub fn push_transcript(&mut self, payload: impl Into<String>, run_id: u64, step_number: u32) {
        self.events
            .push(SessionCommittedEvent::new(payload, run_id, step_number));
    }

    /// Combine another staged commit into this batch while preserving order
    /// within each component. Store internals remain hidden from callers.
    pub fn append_commit(&mut self, commit: Self) {
        self.events.extend(commit.events);
        self.projections.extend(commit.projections);
        self.trailing_events.extend(commit.trailing_events);
    }

    /// Append a domain event after this commit's transcript events, atomically
    /// with their projections.
    pub fn push_trailing_event(
        &mut self,
        event_type: impl Into<String>,
        payload: impl Into<String>,
    ) {
        self.trailing_events
            .push(SessionEventInput::new(event_type, payload));
    }

    /// Clear durable ingress recovery state in the same transaction as the
    /// `UserInject` event that proves delivery.
    pub fn acknowledge_pending_user_input(&mut self, message_id: impl Into<String>) {
        self.projections
            .push(SessionProjectionIntent::AcknowledgePendingUserInput {
                message_id: message_id.into(),
            });
    }

    pub fn project_assistant_message(
        &mut self,
        message_id: impl Into<String>,
        content: impl Into<String>,
        message_type: Option<TranscriptMessageKind>,
    ) {
        self.projections
            .push(SessionProjectionIntent::AssistantMessage {
                message_id: message_id.into(),
                content: content.into(),
                message_type,
            });
    }

    /// Project the id shared by the source's transcript content and thought
    /// step: assistant thought uses `step-*`, while a supplement uses `msg-*`.
    pub fn project_thought_step(&mut self, message_id: impl Into<String>, step_number: u32) {
        self.projections.push(SessionProjectionIntent::ThoughtStep {
            message_id: message_id.into(),
            step_number,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn project_tool_call_step(
        &mut self,
        step_id: impl Into<String>,
        step_number: u32,
        tool_index: u32,
        tool_name: impl Into<String>,
        tool_input: impl Into<String>,
        tool_call_id: Option<String>,
        is_high_risk: bool,
        silent: bool,
    ) {
        self.projections.push(SessionProjectionIntent::ToolStep {
            step_id: step_id.into(),
            step_number,
            tool_index,
            tool_name: tool_name.into(),
            tool_input: tool_input.into(),
            tool_call_id,
            is_high_risk,
            silent,
        });
    }

    fn event_inputs(&self) -> Vec<SessionEventInput> {
        let mut inputs = self
            .events
            .iter()
            .map(|event| {
                SessionEventInput::transcript(
                    event.payload.clone(),
                    event.run_id,
                    event.step_number,
                )
            })
            .collect::<Vec<_>>();
        inputs.extend(self.trailing_events.iter().cloned());
        inputs
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionCommitResult {
    pub events: Vec<SessionEvent>,
    /// Created-at values for materialized message rows, in insertion order.
    pub message_created_at: Vec<String>,
    /// Time spent entering the immediate SQLite transaction, including any
    /// writer-lock wait before the event/projection batch could begin.
    pub lock_wait_ms: u64,
    /// All session clocks after this batch committed.
    pub cursor: SessionCursor,
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
/// The projection groups the current queries behind one SessionStore port;
/// its fields do not imply a shared database snapshot or transaction.
#[derive(Debug, Clone)]
pub struct SessionResumeProjection {
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
    pub usage: Option<SessionUsage>,
    pub llm_usage: Vec<LlmUsageRecord>,
    /// Active append-only event stream used by Agent-owned resume reducers.
    pub active_events: Vec<SessionEvent>,
    pub active_domain_events: Vec<SessionEvent>,
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

    /// Ensure a pending action-step row exists with the supplied durable
    /// invocation identity and confirmation decision.
    pub async fn ensure_tool_step(
        &self,
        write: ToolStepWrite,
        confirmed: Option<bool>,
    ) -> anyhow::Result<()> {
        ensure_session_id(&write.session_id)?;
        ensure_step_id(&write.step_id, "step_id")?;
        self.db
            .run_blocking(move |db| {
                db.ensure_tool_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.tool_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )
            })
            .await
    }

    /// Ensure an action-step row and mark it running as one blocking-pool
    /// operation. Keeping both Database calls in this closure preserves the
    /// existing ordering and avoids an interleaving window between them.
    pub async fn ensure_and_start_tool_step(
        &self,
        write: ToolStepWrite,
        confirmed: Option<bool>,
    ) -> anyhow::Result<bool> {
        ensure_session_id(&write.session_id)?;
        ensure_step_id(&write.step_id, "step_id")?;
        self.db
            .run_blocking(move |db| {
                db.ensure_tool_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.tool_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )?;
                db.start_tool_step(&write.step_id)
            })
            .await
    }

    /// Ensure an action-step row and record its final observation/outcome as
    /// one blocking-pool operation, retaining the existing Database order.
    pub async fn ensure_and_finish_tool_step(
        &self,
        write: ToolStepWrite,
        confirmed: Option<bool>,
        observation: String,
        outcome: ToolStepOutcome,
    ) -> anyhow::Result<bool> {
        ensure_session_id(&write.session_id)?;
        ensure_step_id(&write.step_id, "step_id")?;
        self.db
            .run_blocking(move |db| {
                db.ensure_tool_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.tool_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )?;
                db.finish_tool_step(&write.step_id, &observation, outcome)
            })
            .await
    }

    /// Load the existing read models used by App session resume on SQLite's
    /// blocking pool. Queries run in the established order and retain their
    /// independent read semantics; this port does not promise one snapshot.
    /// Dropping this future cannot interrupt a query already running on
    /// Tokio's blocking pool.
    pub async fn session_resume_projection(
        &self,
        session_id: &str,
    ) -> anyhow::Result<SessionResumeProjection> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let store = self.clone();
        self.db
            .run_blocking(move |db| {
                let messages = db.list_session_messages(&session_id)?;
                let steps = db.list_session_steps(&session_id)?;
                let usage = db.get_session_usage(&session_id)?;
                let llm_usage = db.list_session_llm_usage(&session_id)?;
                let active_events = store.read_active(&session_id)?;
                let active_domain_events = active_events
                    .iter()
                    .filter(|event| event.event_type != TRANSCRIPT_EVENT_TYPE)
                    .cloned()
                    .collect();
                Ok(SessionResumeProjection {
                    messages,
                    steps,
                    usage,
                    llm_usage,
                    active_events,
                    active_domain_events,
                })
            })
            .await
    }

    /// Atomically create an independent child session from an Agent-prepared
    /// active transcript prefix and its message/step UI projections.
    pub async fn create_fork_session(
        &self,
        snapshot: SessionForkSnapshot,
    ) -> anyhow::Result<Session> {
        ensure_session_id(&snapshot.parent_session_id)?;
        ensure_message_id(&snapshot.source_message_id, "source_message_id")?;
        ensure_message_id(&snapshot.child_source_message_id, "child_source_message_id")?;
        anyhow::ensure!(
            snapshot.expected_event_sequence >= 0,
            "fork event sequence cannot be negative"
        );
        let store = self.clone();
        self.db
            .run_blocking(move |_| store.create_fork_session_sync(snapshot))
            .await
    }

    fn create_fork_session_sync(&self, snapshot: SessionForkSnapshot) -> anyhow::Result<Session> {
        ensure_session_id(&snapshot.parent_session_id)?;
        ensure_message_id(&snapshot.source_message_id, "source_message_id")?;
        ensure_message_id(&snapshot.child_source_message_id, "child_source_message_id")?;
        anyhow::ensure!(
            !snapshot.transcript_events.is_empty(),
            "fork transcript prefix cannot be empty"
        );
        anyhow::ensure!(
            snapshot
                .transcript_events
                .len()
                .saturating_add(snapshot.branch_points.len())
                <= MAX_FORK_TRANSCRIPT_EVENTS,
            "fork transcript and rollback metadata exceed {MAX_FORK_TRANSCRIPT_EVENTS} events"
        );
        anyhow::ensure!(
            snapshot.messages.len().saturating_add(snapshot.steps.len())
                <= MAX_FORK_PROJECTION_ROWS,
            "fork projection exceeds {MAX_FORK_PROJECTION_ROWS} rows"
        );
        let event_payload_bytes = snapshot
            .transcript_events
            .iter()
            .fold(0_usize, |total, event| {
                total.saturating_add(event.payload.len())
            })
            .saturating_add(snapshot.branch_points.iter().fold(0_usize, |total, point| {
                total.saturating_add(
                    serde_json::json!({
                        "event_cursor": point.event_cursor,
                        "step_number": point.step_number,
                        "last_msg_at": point.source_last_msg_at.as_deref(),
                    })
                    .to_string()
                    .len(),
                )
            }));
        anyhow::ensure!(
            event_payload_bytes <= MAX_FORK_EVENT_PAYLOAD_BYTES,
            "fork transcript exceeds {} payload bytes",
            MAX_FORK_EVENT_PAYLOAD_BYTES
        );
        for event in &snapshot.transcript_events {
            anyhow::ensure!(
                event.event_type == TRANSCRIPT_EVENT_TYPE,
                "fork snapshot may contain transcript events only"
            );
        }
        let selected_child_message = snapshot
            .messages
            .iter()
            .find(|message| message.message_id == snapshot.child_source_message_id);
        anyhow::ensure!(
            selected_child_message.is_some_and(|message| {
                message.role == haven_common::types::CanonicalRole::User
                    && message.content == snapshot.input_text
            }),
            "fork projection must include the selected user message"
        );
        for message in &snapshot.messages {
            anyhow::ensure!(
                message.session_id == snapshot.parent_session_id,
                "fork message projection belongs to a different parent session"
            );
            for input in &message.media_inputs {
                input.validate_asset_identities()?;
            }
        }
        for step in &snapshot.steps {
            anyhow::ensure!(
                step.session_id == snapshot.parent_session_id,
                "fork step projection belongs to a different parent session"
            );
            anyhow::ensure!(
                is_canonical_id(&step.step_id, "step")
                    || (step.tool_name.is_none() && is_canonical_id(&step.step_id, "msg")),
                "fork step projection has an invalid identity"
            );
        }

        let session_id = haven_common::types::new_id("ses");
        let origin = SessionOrigin::Fork {
            parent_session_id: snapshot.parent_session_id.clone(),
            source_message_id: snapshot.source_message_id.clone(),
        };
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let write_result = (|| -> anyhow::Result<Vec<SessionEvent>> {
            let source = conn
                .query_row(
                    "SELECT content, created_at FROM messages
                     WHERE session_id = ?1 AND id = ?2 AND role = 'user'",
                    rusqlite::params![snapshot.parent_session_id, snapshot.source_message_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?
                .ok_or_else(|| anyhow::anyhow!("fork source user message no longer exists"))?;
            anyhow::ensure!(
                source.0 == snapshot.input_text && source.1 == snapshot.source_message_created_at,
                "fork source message changed while creating the child session"
            );
            let parent_event_sequence: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
                rusqlite::params![snapshot.parent_session_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                parent_event_sequence == snapshot.expected_event_sequence,
                "parent session event stream changed while creating the fork"
            );
            anyhow::ensure!(
                last_projection_created_at(&conn, &snapshot.parent_session_id)?
                    == snapshot.expected_projection_created_at,
                "parent session projections changed while creating the fork"
            );

            conn.execute(
                "INSERT INTO sessions (
                    id, input_text, status, created_at, updated_at, origin,
                    parent_session_id, fork_source_message_id
                 ) VALUES (?1, ?2, 'pending', ?3, ?3, 'fork', ?4, ?5)",
                rusqlite::params![
                    session_id,
                    snapshot.input_text,
                    now,
                    snapshot.parent_session_id,
                    snapshot.source_message_id
                ],
            )?;

            let mut stored_events = Vec::with_capacity(
                snapshot
                    .transcript_events
                    .len()
                    .saturating_add(snapshot.branch_points.len()),
            );
            for batch in snapshot
                .transcript_events
                .chunks(MAX_SESSION_EVENT_BATCH_EVENTS)
            {
                stored_events.extend(Self::append_batch_in_transaction(
                    &conn,
                    &session_id,
                    batch,
                )?);
            }

            let mut projection_clocks =
                Vec::with_capacity(snapshot.messages.len().saturating_add(snapshot.steps.len()));
            let mut projection_order =
                Vec::with_capacity(snapshot.messages.len().saturating_add(snapshot.steps.len()));
            projection_order.extend(
                snapshot
                    .messages
                    .iter()
                    .enumerate()
                    .map(|(index, message)| (message.created_at.as_str(), 0_u8, index)),
            );
            projection_order.extend(
                snapshot
                    .steps
                    .iter()
                    .enumerate()
                    .map(|(index, step)| (step.created_at.as_str(), 1_u8, index)),
            );
            projection_order.sort();

            for (_, kind, index) in projection_order {
                if kind == 0 {
                    let message = &snapshot.messages[index];
                    let inserted = Database::insert_message_row_in_transaction(
                        &conn,
                        &session_id,
                        message.role,
                        &message.content,
                        message.message_type,
                        message.tool_call_id.as_deref(),
                        &message.attachments,
                        message.voice,
                        &message.message_id,
                        &message.media_inputs,
                    )?;
                    projection_clocks.push((message.created_at.clone(), inserted.created_at));
                } else {
                    let step = &snapshot.steps[index];
                    let tool_name = step.tool_name.as_deref().unwrap_or("thought");
                    let tool_input = step.tool_input.as_deref().unwrap_or(&step.step_id);
                    let output = step.observation.as_deref().unwrap_or("{}");
                    let created_at = next_projection_created_at(&conn, &session_id, None)?;
                    projection_clocks.push((step.created_at.clone(), created_at.clone()));
                    conn.execute(
                        "INSERT INTO session_steps (
                            id, session_id, step_number, tool_index, tool_name, input, output,
                            status, is_high_risk, confirmed, started_at, completed_at, created_at,
                            silent, thought, tool_call_name, tool_call_input, tool_call_id, observation
                         ) VALUES (
                            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                            ?14, ?15, ?16, ?17, ?18, ?19
                         )",
                        rusqlite::params![
                            step.step_id,
                            session_id,
                            step.step_number,
                            step.tool_index,
                            tool_name,
                            tool_input,
                            output,
                            step.status.as_str(),
                            step.is_high_risk as i32,
                            step.confirmed.map(|confirmed| confirmed as i32),
                            step.started_at,
                            step.completed_at,
                            created_at,
                            step.silent as i32,
                            step.thought,
                            step.tool_name,
                            step.tool_input,
                            step.tool_call_id,
                            step.observation
                        ],
                    )?;
                    conn.execute(
                        "INSERT INTO session_step_cursors (session_id, last_step_seq)
                         VALUES (?1, 1)
                         ON CONFLICT(session_id) DO UPDATE SET
                            last_step_seq = session_step_cursors.last_step_seq + 1",
                        rusqlite::params![session_id],
                    )?;
                }
            }

            let mut branch_events = Vec::with_capacity(snapshot.branch_points.len());
            for point in &snapshot.branch_points {
                anyhow::ensure!(
                    point.event_cursor <= snapshot.transcript_events.len(),
                    "fork rollback boundary is outside the copied transcript prefix"
                );
                let last_msg_at = point.source_last_msg_at.as_deref().and_then(|cutoff| {
                    projection_clocks
                        .iter()
                        .take_while(|(source_created_at, _)| source_created_at.as_str() <= cutoff)
                        .last()
                        .map(|(_, child_created_at)| child_created_at.clone())
                });
                let payload = serde_json::json!({
                    "event_cursor": point.event_cursor,
                    "step_number": point.step_number,
                    "last_msg_at": last_msg_at,
                });
                let mut event =
                    SessionEventInput::new(BRANCH_POINT_EVENT_TYPE, payload.to_string());
                event.step_number = Some(point.step_number);
                branch_events.push(event);
            }
            for batch in branch_events.chunks(MAX_SESSION_EVENT_BATCH_EVENTS) {
                stored_events.extend(Self::append_batch_in_transaction(
                    &conn,
                    &session_id,
                    batch,
                )?);
            }
            Ok(stored_events)
        })();

        let stored_events = match write_result {
            Ok(events) => {
                commit_sqlite_transaction(&conn, "create fork session")?;
                events
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "create fork session");
                return Err(error);
            }
        };
        drop(conn);
        self.db.cache_invalidate_messages(&session_id);
        self.db.cache_invalidate_session_history_page();
        for event in stored_events {
            let _ = self.live_tx.send(event);
        }
        Ok(Session {
            session_id,
            input_text: snapshot.input_text,
            title: None,
            status: SessionStatus::Pending,
            run_end_reason: None,
            created_at: now.clone(),
            updated_at: now,
            origin,
        })
    }

    /// Load the materialized session usage through the usage store boundary.
    /// The caller never needs to borrow the raw Database just to seed a live
    /// usage tracker.
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

    /// Create the materialized thought step for a committed transcript row.
    /// The message row remains the sole content authority; this method only
    /// writes the execution-state projection.
    pub async fn create_thought_step(
        &self,
        session_id: &str,
        step_number: u32,
        message_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        ensure_step_id(message_id, "thought message_id")?;
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.create_thought_step(&session_id, step_number, &message_id)?;
                Ok::<(), anyhow::Error>(())
            })
            .await
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

    /// Persist a new session's crash-safe user seed before it is exposed to
    /// the dispatcher. The seed type is derived from the durable session
    /// origin; later input must use the pending-input and `UserInject` path.
    pub async fn persist_ingress_user_seed(
        &self,
        session_id: &str,
        content: &str,
        attachments: &[MessageAttachment],
        voice: bool,
    ) -> anyhow::Result<Message> {
        ensure_session_id(session_id)?;
        let origin = self
            .load_session_record(session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {session_id} not found"))?
            .origin;
        self.persist_message_row(
            session_id,
            haven_common::types::CanonicalRole::User,
            content,
            Some(origin.first_user_message_type()),
            attachments,
            voice,
            None,
            None,
            None,
        )
        .await
    }

    /// Persist a recovery-only error partial. It intentionally stays outside
    /// `session_events` and remains bounded by the existing `last_msg_at`
    /// rollback clock.
    pub async fn persist_recovery_partial(
        &self,
        session_id: &str,
        content: &str,
        kind: RecoveryPartialKind,
        message_id: &str,
    ) -> anyhow::Result<Message> {
        ensure_message_id(message_id, "recovery partial message_id")?;
        self.persist_message_row(
            session_id,
            haven_common::types::CanonicalRole::Assistant,
            content,
            Some(kind.as_message_type()),
            &[],
            false,
            Some(message_id),
            None,
            None,
        )
        .await
    }

    /// Persist a terminal ToolRun result when there is no live Agent loop to
    /// commit it. The stable message id keeps delivery retries idempotent.
    pub async fn persist_terminal_tool_run_result(
        &self,
        session_id: &str,
        content: &str,
        message_id: &str,
    ) -> anyhow::Result<Message> {
        ensure_message_id(message_id, "terminal ToolRun result message_id")?;
        self.persist_message_row(
            session_id,
            haven_common::types::CanonicalRole::User,
            content,
            Some(TranscriptMessageKind::Text),
            &[],
            false,
            Some(message_id),
            None,
            None,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn persist_message_row(
        &self,
        session_id: &str,
        role: haven_common::types::CanonicalRole,
        content: &str,
        message_type: Option<TranscriptMessageKind>,
        attachments: &[MessageAttachment],
        voice: bool,
        message_id: Option<&str>,
        tool_call_id: Option<&str>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Message> {
        ensure_session_id(session_id)?;
        if let Some(message_id) = message_id {
            ensure_message_or_step_id(message_id, "message_id")?;
        }
        let session_id = session_id.to_owned();
        let content = content.to_owned();
        let attachments = attachments.to_vec();
        let message_id = message_id.map(str::to_owned);
        let tool_call_id = tool_call_id.map(str::to_owned);
        let persist = move |db: &Database| {
            if let Some(message_id) = message_id.as_deref()
                && let Some(existing) = db.get_message_by_id(&session_id, message_id)?
            {
                if existing.role != role
                    || existing.content != content
                    || existing.message_type != message_type
                    || existing.tool_call_id.as_deref() != tool_call_id.as_deref()
                {
                    anyhow::bail!(
                        "message idempotency conflict for session {} message {}",
                        session_id,
                        message_id
                    );
                }
                return Ok(existing);
            }
            db.add_message_full(
                &session_id,
                role,
                &content,
                message_type,
                tool_call_id.as_deref(),
                &attachments,
                voice,
                message_id.as_deref(),
            )
        };

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
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

    /// Read every pending session for dispatcher recovery, newest first, on
    /// SQLite's blocking pool. Cancellation interrupts a query already
    /// waiting on a SQLite connection.
    ///
    /// This is the semantic equivalent of the unbounded pending-session query:
    /// no text/date filters, `limit = -1`, and `offset = 0`. The generic search
    /// surface remains an implementation detail of the Database repository.
    pub async fn pending_session_records_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<Vec<Session>> {
        anyhow::ensure!(
            !cancel.is_cancelled(),
            "pending session read cancelled before start"
        );
        self.db
            .run_blocking_cancellable(cancel, |db| {
                db.search_sessions_filtered(
                    None,
                    Some(SessionStatus::Pending.as_str()),
                    None,
                    None,
                    -1,
                    0,
                )
            })
            .await
    }

    /// Mark pending/running action steps for a failed session as `unknown`.
    ///
    /// The existing session-steps repository owns the update semantics; this
    /// port only moves its SQLite work onto the blocking pool.
    pub async fn fail_pending_tool_run_steps(
        &self,
        session_id: &str,
        observation: &str,
    ) -> anyhow::Result<usize> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let observation = observation.to_owned();
        self.db
            .run_blocking(move |db| db.fail_pending_tool_run_steps(&session_id, &observation))
            .await
    }

    /// Subscribe to events committed through this store. A consumer that also
    /// reads durable history must subscribe first, then replay and deduplicate
    /// overlapping `(session_id, sequence)` values. A lagged receiver must
    /// perform the same replay again.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.live_tx.subscribe()
    }

    /// Initialize a memory-event cursor synchronously for fixtures and tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn initialize_memory_event_cursor_if_absent(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<bool> {
        ensure_session_id(session_id)?;
        self.db
            .initialize_memory_event_cursor_if_absent(session_id, sequence)
    }

    /// Read the durable memory event cursor. This clock is independent from
    /// the fact-extraction message-id cursor and projection cursors.
    fn memory_event_cursor(&self, session_id: &str) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        self.db.memory_event_cursor(session_id)
    }

    /// Read the memory event cursor on SQLite's blocking pool with
    /// cancellation for an in-flight database operation.
    pub async fn memory_event_cursor_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| store.memory_event_cursor(&session_id))
            .await
    }

    /// Read the durable cursor while preserving a missing key as `None`.
    pub async fn memory_event_cursor_optional_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<Option<i64>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |db| {
                db.memory_event_cursor_optional(&session_id)
            })
            .await
    }

    pub async fn baseline_missing_memory_event_cursors_to_latest_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<usize> {
        self.db
            .run_blocking_cancellable(cancel, |db| {
                db.baseline_missing_memory_event_cursors_to_latest()
            })
            .await
    }

    /// Advance the durable memory event cursor monotonically.
    fn checkpoint_memory_event_cursor(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        self.db.checkpoint_memory_event_cursor(session_id, sequence)
    }

    /// Persist a memory event checkpoint on SQLite's blocking pool with
    /// cancellation for an in-flight database operation.
    pub async fn checkpoint_memory_event_cursor_cancellable(
        &self,
        session_id: &str,
        sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| {
                store.checkpoint_memory_event_cursor(&session_id, sequence)
            })
            .await
    }

    /// Clear one session's durable memory event cursor.
    #[cfg(test)]
    fn clear_memory_event_cursor(&self, session_id: &str) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        self.db.clear_memory_event_cursor(session_id)
    }

    /// Synchronous clock inspection for Memory unit tests.
    #[cfg(test)]
    pub(super) fn cursor(&self, session_id: &str) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let conn = self.db.conn();
        Self::cursor_in_connection(&conn, session_id)
    }

    /// Read every persisted input that has not yet been acknowledged by a
    /// committed `UserInject` event. The durable marker, not message age or
    /// step-row shape, determines whether it is recoverable.
    pub async fn pending_session_inputs(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<PendingSessionInput>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.list_pending_session_inputs(&session_id))
            .await
    }

    /// Drop recovery state for an input explicitly rejected by ingress, such
    /// as a message routed to a terminal session whose history row cannot be
    /// deleted. Normal delivery uses the atomic transcript acknowledgement.
    pub async fn discard_pending_user_input(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        ensure_message_id(message_id, "pending message_id")?;
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let conn = db.conn();
                Database::acknowledge_pending_session_input(&conn, &session_id, &message_id)
            })
            .await
    }

    /// Persist a newly accepted input with its recovery disposition and marker.
    /// Repeating a supplied message id is idempotent and does not recreate a
    /// marker already acknowledged by a transcript event.
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_pending_user_input(
        &self,
        session_id: &str,
        content: &str,
        message_type: Option<TranscriptMessageKind>,
        attachments: &[MessageAttachment],
        voice: bool,
        message_id: Option<&str>,
        disposition: PendingInputDisposition,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<PendingSessionInput> {
        ensure_session_id(session_id)?;
        if let Some(message_id) = message_id {
            ensure_message_id(message_id, "pending message_id")?;
        }
        let session_id = session_id.to_owned();
        let content = content.to_owned();
        let attachments = attachments.to_vec();
        let message_id = message_id.map(str::to_owned);
        let persist = move |db: &Database| {
            if let Some(message_id) = message_id.as_deref()
                && let Some(existing) = db.get_message_by_id(&session_id, message_id)?
            {
                if existing.role != haven_common::types::CanonicalRole::User
                    || existing.content != content
                    || existing.message_type != message_type
                {
                    anyhow::bail!(
                        "message idempotency conflict for session {} message {}",
                        session_id,
                        message_id
                    );
                }
                return Ok(db
                    .get_pending_session_input(&session_id, message_id)?
                    .unwrap_or(PendingSessionInput {
                        message: existing,
                        disposition: PendingInputDisposition::FollowUp,
                    }));
            }
            db.add_pending_user_input(
                &session_id,
                &content,
                message_type,
                &attachments,
                voice,
                message_id.as_deref(),
                disposition,
            )
        };

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
    }

    /// Load the active transcript, branch points and all projection clocks
    /// from one SQLite read boundary. This is the only recovery read surface
    /// exposed to Agent resume/rollback code.
    fn load_replay_state(&self, session_id: &str) -> anyhow::Result<Option<SessionReplayState>> {
        ensure_session_id(session_id)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN")?;
        let result = (|| -> anyhow::Result<Option<SessionReplayState>> {
            let latest_sequence: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )?;
            if latest_sequence == 0 {
                return Ok(None);
            }
            Ok(Some(SessionReplayState {
                transcript: Self::read_active_in_connection(&conn, session_id)?
                    .into_iter()
                    .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
                    .collect(),
                branch_points: Self::read_active_branch_points_in_connection(&conn, session_id)?,
                cursor: Self::cursor_in_connection(&conn, session_id)?,
            }))
        })();
        match result {
            Ok(state) => {
                conn.execute_batch("COMMIT")?;
                Ok(state)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Load the durable replay state on SQLite's blocking pool. Agent keeps
    /// transcript decoding and ReAct projection policy outside Memory.
    pub async fn load_replay_state_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionReplayState>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.load_replay_state(&session_id))
            .await
    }

    /// Read the durable replay cursor used to verify an Agent event boundary.
    /// A session without an event log keeps the same default cursor as an
    /// absent replay state. Agent owns the boundary policy; this port only
    /// schedules the existing replay read and preserves optional cancellation.
    pub async fn event_boundary_cursor(
        &self,
        session_id: &str,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let read = move |_db: &Database| {
            Ok(store
                .load_replay_state(&session_id)?
                .map(|replay| replay.cursor)
                .unwrap_or_default())
        };

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, read).await,
            None => self.db.run_blocking(read).await,
        }
    }

    /// Append a recovery protocol marker on SQLite's blocking pool. Synthetic
    /// Agent tests may use a session id without a durable session row; retain
    /// their historical successful no-op behavior inside this boundary.
    pub async fn append_recovery_persistence_if_session_exists(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let phase = phase.to_owned();
        self.db
            .run_blocking(move |db| {
                if db.get_session(&session_id)?.is_none() {
                    return Ok(());
                }
                store.append_recovery_persistence_sync(
                    &session_id,
                    run_id,
                    step_number,
                    &phase,
                    status,
                )?;
                Ok(())
            })
            .await
    }

    /// Append a branch point using the projection clocks observed under its
    /// existing transaction. The durable-session check and append stay within
    /// one blocking-pool operation, preserving the caller's not-found error.
    pub async fn append_branch_point_from_projection_for_existing_session(
        &self,
        session_id: &str,
        step_number: u32,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                if db.get_session(&session_id)?.is_none() {
                    anyhow::bail!(
                        "session '{}' disappeared before branch-point append",
                        session_id
                    );
                }
                let (_, cursor) =
                    store.append_branch_point_from_projection(&session_id, step_number, run_id)?;
                Ok(cursor)
            })
            .await
    }

    fn append_sync(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
        run_id: Option<u64>,
        step_number: Option<u32>,
    ) -> anyhow::Result<SessionEvent> {
        ensure_session_id(session_id)?;
        Self::validate_single_event(event_type, payload, run_id, step_number)?;
        let input = SessionEventInput {
            event_type: event_type.into(),
            payload: payload.into(),
            run_id,
            step_number,
        };
        self.append_batch(session_id, std::slice::from_ref(&input))
            .map(|mut events| events.remove(0))
    }

    /// Synchronous raw event append for fixtures and unit tests. Production
    /// callers use the typed async event APIs.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
        run_id: Option<u64>,
        step_number: Option<u32>,
    ) -> anyhow::Result<SessionEvent> {
        self.append_sync(session_id, event_type, payload, run_id, step_number)
    }

    /// Append a non-transcript domain event through the session persistence
    /// boundary. The caller owns the domain payload and policy; this port only
    /// schedules the existing append operation on SQLite's blocking pool.
    /// Domain events have no run or step association. The existing append
    /// implementation continues to broadcast the committed event to live
    /// subscribers.
    pub async fn append_domain_event(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
    ) -> anyhow::Result<SessionEvent> {
        ensure_session_id(session_id)?;
        ensure_domain_event_type(event_type)?;
        Self::validate_single_event(event_type, payload, None, None)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let event_type = event_type.to_owned();
        let payload = payload.to_owned();
        self.db
            .run_blocking(move |_| {
                store.append_sync(&session_id, &event_type, &payload, None, None)
            })
            .await
    }

    /// Append non-transcript domain events and move the owning session to a new
    /// status in one SQLite transaction. The status compare-and-set and every
    /// event commit together, and live subscribers only see committed events.
    pub async fn append_domain_event_batch_with_session_status(
        &self,
        session_id: &str,
        expected_status: SessionStatus,
        next_status: SessionStatus,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        anyhow::ensure!(!events.is_empty(), "domain event batch cannot be empty");
        Self::validate_inputs(events)?;
        ensure_domain_event_types(events)?;
        anyhow::ensure!(
            events
                .iter()
                .all(|event| event.run_id.is_none() && event.step_number.is_none()),
            "domain event batches cannot include transcript coordinates"
        );
        let store = self.clone();
        let session_id = session_id.to_owned();
        let events = events.to_vec();
        self.db
            .run_blocking(move |_| {
                store.append_domain_event_batch_with_session_status_sync(
                    &session_id,
                    expected_status,
                    next_status,
                    &events,
                )
            })
            .await
    }

    fn append_domain_event_batch_with_session_status_sync(
        &self,
        session_id: &str,
        expected_status: SessionStatus,
        next_status: SessionStatus,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        ensure_domain_event_types(events)?;
        Self::validate_inputs(events)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| {
            let changed = conn.execute(
                "UPDATE sessions SET status = ?1, updated_at = ?2,
                    run_end_reason = CASE WHEN ?1 IN ('pending', 'running') THEN NULL ELSE run_end_reason END
                 WHERE id = ?3 AND status = ?4",
                rusqlite::params![
                    next_status.as_str(),
                    now,
                    session_id,
                    expected_status.as_str()
                ],
            )?;
            anyhow::ensure!(
                changed == 1,
                "session '{}' status changed before confirmation batch commit",
                session_id
            );
            let stored = Self::append_batch_in_transaction(&conn, session_id, events)?;
            commit_sqlite_transaction(&conn, "append confirmation batch and pause session")?;
            Ok(stored)
        })();

        match result {
            Ok(stored) => {
                self.db.cache_invalidate_session_history_page();
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append confirmation batch and pause session");
                Err(error)
            }
        }
    }

    /// Append a batch in one SQLite transaction.  Sequence allocation happens
    /// under `BEGIN IMMEDIATE`, so concurrent sessions and concurrent writers
    /// cannot produce duplicate per-session cursors.
    pub(super) fn append_batch(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        Self::validate_inputs(events)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = Self::append_batch_in_transaction(&conn, session_id, events);
        match result {
            Ok(stored) => {
                commit_sqlite_transaction(&conn, "append batch")?;
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append batch");
                Err(error)
            }
        }
    }

    /// Commit an Agent transcript intent on the blocking pool, with optional
    /// cooperative cancellation for the SQLite operation.
    ///
    /// The session foreign key rejects writes after session deletion; no
    /// transcript is silently discarded for a missing session.
    pub async fn commit_transcript_cancellable(
        &self,
        session_id: &str,
        committed: SessionCommitted,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<SessionCommitResult> {
        ensure_session_id(session_id)?;
        Self::validate_committed_batch(&committed)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let write = move |_db: &Database| store.commit_transcript_sync(&session_id, &committed);

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, write).await,
            None => self.db.run_blocking(write).await,
        }
    }

    /// Commit transcript events and their materialized rows in one SQLite
    /// transaction. Events are allocated and inserted before the projection
    /// intents are applied; projection failure rolls the whole commit back.
    /// Live broadcasts happen only after COMMIT, so subscribers never observe
    /// an event that was rolled back.
    fn commit_transcript_sync(
        &self,
        session_id: &str,
        committed: &SessionCommitted,
    ) -> anyhow::Result<SessionCommitResult> {
        ensure_session_id(session_id)?;
        Self::validate_committed_batch(committed)?;
        let events = committed.event_inputs();
        Self::validate_inputs(&events)?;
        if committed.events.is_empty() {
            anyhow::ensure!(
                committed.projections.is_empty() && committed.trailing_events.is_empty(),
                "transcript projection batch must have an event"
            );
            return Ok(SessionCommitResult::default());
        }

        let conn = self.db.conn();
        let lock_wait_started = Instant::now();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let lock_wait_ms = lock_wait_started.elapsed().as_millis() as u64;
        let result = (|| -> anyhow::Result<SessionCommitResult> {
            let events = Self::append_batch_in_transaction(&conn, session_id, &events)?;
            let message_created_at = self
                .db
                .write_session_projections(&conn, session_id, committed)?;
            let cursor = Self::cursor_in_connection(&conn, session_id)?;
            Ok(SessionCommitResult {
                events,
                message_created_at,
                lock_wait_ms,
                cursor,
            })
        })();
        match result {
            Ok(result) => {
                commit_sqlite_transaction(&conn, "transcript commit")?;
                if !result.message_created_at.is_empty() {
                    self.db.cache_invalidate_messages(session_id);
                }
                for event in &result.events {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(result)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "transcript transaction");
                Err(error)
            }
        }
    }

    /// Synchronous commit access for fixture and unit tests. Production
    /// callers use `commit_transcript_cancellable` so SQLite work stays on
    /// the blocking boundary.
    #[cfg(any(test, feature = "test-support"))]
    pub fn commit_transcript(
        &self,
        session_id: &str,
        committed: &SessionCommitted,
    ) -> anyhow::Result<SessionCommitResult> {
        self.commit_transcript_sync(session_id, committed)
    }

    fn validate_inputs(events: &[SessionEventInput]) -> anyhow::Result<()> {
        anyhow::ensure!(
            events.len() <= MAX_SESSION_EVENT_BATCH_EVENTS,
            "session event batch exceeds {} events",
            MAX_SESSION_EVENT_BATCH_EVENTS
        );
        let event_refs = events
            .iter()
            .map(SessionEventInputRef::from)
            .collect::<Vec<_>>();
        Self::validate_event_input_refs(&event_refs)
    }

    fn validate_single_event(
        event_type: &str,
        payload: &str,
        run_id: Option<u64>,
        step_number: Option<u32>,
    ) -> anyhow::Result<()> {
        let event = SessionEventInputRef {
            event_type,
            payload,
            run_id,
            step_number,
        };
        Self::validate_event_input_refs(std::slice::from_ref(&event))
    }

    fn validate_committed_event_batch(committed: &SessionCommitted) -> anyhow::Result<()> {
        let event_count = committed
            .events
            .len()
            .checked_add(committed.trailing_events.len())
            .ok_or_else(|| anyhow::anyhow!("session event batch event count overflow"))?;
        anyhow::ensure!(
            event_count <= MAX_SESSION_EVENT_BATCH_EVENTS,
            "session event batch exceeds {} events",
            MAX_SESSION_EVENT_BATCH_EVENTS
        );
        let mut event_refs = Vec::with_capacity(event_count);
        event_refs.extend(committed.events.iter().map(|event| SessionEventInputRef {
            event_type: TRANSCRIPT_EVENT_TYPE,
            payload: &event.payload,
            run_id: Some(event.run_id),
            step_number: Some(event.step_number),
        }));
        event_refs.extend(
            committed
                .trailing_events
                .iter()
                .map(SessionEventInputRef::from),
        );
        Self::validate_event_input_refs(&event_refs)
    }

    fn validate_committed_batch(committed: &SessionCommitted) -> anyhow::Result<()> {
        Self::validate_committed_event_batch(committed)?;
        anyhow::ensure!(
            committed.projections.len() <= MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS,
            "transcript batch exceeds {} projection rows",
            MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS
        );
        Self::validate_projections(&committed.projections)?;
        Self::payload_bytes(committed)?;
        Ok(())
    }

    fn validate_event_input_refs(events: &[SessionEventInputRef<'_>]) -> anyhow::Result<()> {
        anyhow::ensure!(
            events.len() <= MAX_SESSION_EVENT_BATCH_EVENTS,
            "session event batch exceeds {} events",
            MAX_SESSION_EVENT_BATCH_EVENTS
        );
        let mut byte_counter = BoundedByteCounter::default();
        if let Err(error) = serde_json::to_writer(&mut byte_counter, events) {
            if byte_counter.exceeded {
                anyhow::bail!(
                    "session event batch exceeds {} payload bytes",
                    MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES
                );
            }
            return Err(error.into());
        }
        for event in events {
            anyhow::ensure!(
                !event.event_type.trim().is_empty(),
                "session event type must not be empty"
            );
            anyhow::ensure!(
                serde_json::from_str::<serde_json::Value>(event.payload).is_ok(),
                "session event payload must be valid JSON"
            );
        }
        Ok(())
    }

    fn validate_projections(projections: &[SessionProjectionIntent]) -> anyhow::Result<()> {
        for projection in projections {
            match projection {
                SessionProjectionIntent::AcknowledgePendingUserInput { message_id } => {
                    ensure_message_id(message_id, "pending message_id")?;
                }
                SessionProjectionIntent::AssistantMessage { message_id, .. } => {
                    ensure_message_or_step_id(message_id, "message_id")?;
                }
                SessionProjectionIntent::ThoughtStep { message_id, .. } => {
                    ensure_message_or_step_id(message_id, "thought message_id")?;
                }
                SessionProjectionIntent::ToolStep { step_id, .. } => {
                    ensure_step_id(step_id, "step_id")?;
                }
            }
        }
        Ok(())
    }

    fn payload_bytes(committed: &SessionCommitted) -> anyhow::Result<usize> {
        // The guard is defined over the exact JSON batch representation rather
        // than a hand-maintained sum of selected string fields. This accounts
        // for object/array keys, separators, numeric/boolean fields and JSON
        // escaping, which are all part of the variable-width persistence
        // payload at this boundary. Keep the staging object itself
        // non-serializable to callers; this private view preserves its exact
        // field order and representation for byte accounting.
        let payload = SessionCommittedPayload {
            events: &committed.events,
            projections: &committed.projections,
            trailing_events: &committed.trailing_events,
        };
        let mut byte_counter = BoundedByteCounter::default();
        if let Err(error) = serde_json::to_writer(&mut byte_counter, &payload) {
            if byte_counter.exceeded {
                anyhow::bail!(
                    "transcript batch exceeds {} payload bytes",
                    MAX_SESSION_EVENT_BATCH_PAYLOAD_BYTES
                );
            }
            return Err(error.into());
        }
        Ok(byte_counter.bytes)
    }

    fn append_batch_in_transaction(
        conn: &rusqlite::Connection,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        Self::validate_inputs(events)?;
        let mut sequence: i64 = conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )?;
        let mut stored = Vec::with_capacity(events.len());
        for event in events {
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("session event sequence overflow"))?;
            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let run_id = event
                .run_id
                .map(i64::try_from)
                .transpose()
                .map_err(|_| anyhow::anyhow!("run_id does not fit SQLite INTEGER"))?;
            conn.execute(
                "INSERT INTO session_events
                    (session_id, sequence, event_type, payload,
                     created_at, run_id, step_number)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    session_id,
                    sequence,
                    event.event_type,
                    event.payload,
                    created_at,
                    run_id,
                    event.step_number.map(i64::from),
                ],
            )?;
            stored.push(SessionEvent {
                session_id: session_id.into(),
                sequence,
                event_type: event.event_type.clone(),
                payload: event.payload.clone(),
                created_at,
                run_id: event.run_id,
                step_number: event.step_number,
            });
        }
        Ok(stored)
    }

    fn cursor_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<SessionCursor> {
        ensure_session_id(session_id)?;
        let event_sequence = conn.query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )?;
        let event_cursor = Self::read_active_in_connection(conn, session_id)?
            .into_iter()
            .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
            .count();
        let message_ingress_seq = conn
            .query_row(
                "SELECT COALESCE(last_ingress_seq, 0)
                 FROM message_ingress_cursors WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let step_seq = conn
            .query_row(
                "SELECT COALESCE(last_step_seq, 0)
                 FROM session_step_cursors WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let last_msg_at = last_projection_created_at(conn, session_id)?;
        Ok(SessionCursor {
            event_sequence,
            event_cursor,
            message_ingress_seq,
            step_seq,
            last_msg_at,
        })
    }

    fn append_transcript_sync(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<SessionEvent> {
        self.append_sync(
            session_id,
            TRANSCRIPT_EVENT_TYPE,
            payload,
            Some(run_id),
            Some(step_number),
        )
    }

    /// Synchronous append access for fixture and unit tests. Production
    /// callers use `append_transcript_async`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_transcript(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<SessionEvent> {
        self.append_transcript_sync(session_id, payload, run_id, step_number)
    }

    /// Append one transcript event on SQLite's blocking pool and return its
    /// durable sequence. The session foreign key rejects writes for a missing
    /// session instead of silently returning sequence zero.
    pub async fn append_transcript_async(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        let payload = payload.to_owned();
        self.db
            .run_blocking(move |_| {
                Ok(store
                    .append_transcript_sync(&session_id, &payload, run_id, step_number)?
                    .sequence)
            })
            .await
    }

    /// Append a raw rollback marker for audit/import tooling.
    ///
    /// Live Agent rollback must use [`Self::rollback_to`], which resolves the
    /// active cursor and updates projections in the same transaction. This
    /// narrow primitive remains available for importing an already-materialized
    /// historical marker without pretending that it also repairs projections.
    #[cfg(test)]
    fn append_rollback(
        &self,
        session_id: &str,
        to_sequence: i64,
        target_step: u32,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionEvent> {
        anyhow::ensure!(to_sequence >= 0, "rollback sequence must not be negative");
        let payload = serde_json::json!({
            "to_sequence": to_sequence,
            "target_step": target_step,
        });
        self.append(
            session_id,
            TIMELINE_ROLLBACK_EVENT_TYPE,
            &payload.to_string(),
            run_id,
            Some(target_step),
        )
    }

    /// Read the exact persisted message selected as a rollback target.
    ///
    /// The session-scoped lookup and its not-found error live at the session
    /// persistence boundary; Agent still decides whether the message is a
    /// user message and how it relates to the restored transcript. Dropping
    /// this future cannot interrupt a query already running on Tokio's
    /// blocking pool.
    pub async fn load_rollback_target_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<Message> {
        ensure_session_id(session_id)?;
        ensure_message_id(message_id, "rollback target message_id")?;
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.get_message_by_id(&session_id, &message_id)?
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "rollback target message '{}' not found in session messages",
                            message_id
                        )
                    })
            })
            .await
    }

    /// Roll back one session timeline and all of its materialized projections
    /// in one SQLite transaction. `transcript_cursor` is an index into the
    /// active transcript returned by [`Self::load_replay_state`]; this method
    /// resolves the corresponding append-only event sequence internally and
    /// fails if the event high-water changed or the cursor is outside the
    /// active transcript. The projection boundary is resolved from the same
    /// transaction snapshot.
    /// Discarded event history is retained for audit.
    fn rollback_to(
        &self,
        session_id: &str,
        request: &RollbackRequest,
        replacement_transcript: &[SessionEventInput],
        run_id: Option<u64>,
    ) -> anyhow::Result<RollbackResult> {
        ensure_session_id(session_id)?;
        if let RollbackProjectionBoundary::UserMessage { message_id } = &request.projection_boundary
        {
            ensure_message_id(message_id, "rollback target message_id")?;
        }
        Self::validate_inputs(replacement_transcript)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<RollbackResult> {
            let current_event_sequence: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(
                current_event_sequence == request.expected_event_sequence,
                "session event boundary changed during rollback (expected {}, found {})",
                request.expected_event_sequence,
                current_event_sequence
            );
            let branch_points = Self::read_active_branch_points_in_connection(&conn, session_id)?;
            let mapped_sequence = Self::sequence_for_transcript_cursor_in_connection(
                &conn,
                session_id,
                request.transcript_cursor,
            )?;
            let to_sequence = branch_points
                .iter()
                .find(|point| {
                    point.step_number == request.target_step
                        && point.event_cursor == request.transcript_cursor
                })
                .map_or(mapped_sequence, |point| point.event_sequence);
            let projection_cutoff = match &request.projection_boundary {
                RollbackProjectionBoundary::UserMessage { message_id } => {
                    let created_at = conn
                        .query_row(
                            "SELECT created_at FROM messages
                             WHERE session_id = ?1 AND id = ?2",
                            rusqlite::params![session_id, message_id],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "rollback target message '{}' not found in session messages",
                                message_id
                            )
                        })?;
                    Some(ProjectionCutoff {
                        created_at,
                        inclusive: true,
                    })
                }
                RollbackProjectionBoundary::BranchPoint => {
                    match branch_points
                        .iter()
                        .find(|point| point.step_number == request.target_step)
                    {
                        Some(ActiveBranchPoint {
                            last_msg_at: Some(created_at),
                            ..
                        }) => Some(ProjectionCutoff {
                            created_at: created_at.clone(),
                            inclusive: false,
                        }),
                        Some(_) => None,
                        None => conn
                            .query_row(
                                "SELECT created_at FROM messages
                                 WHERE session_id = ?1 AND role = 'user'
                                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                                rusqlite::params![session_id],
                                |row| row.get::<_, String>(0),
                            )
                            .optional()?
                            .map(|created_at| ProjectionCutoff {
                                created_at,
                                inclusive: false,
                            }),
                    }
                }
            };
            let payload = serde_json::json!({
                "to_sequence": to_sequence,
                "target_step": request.target_step,
            });
            let input = SessionEventInput {
                event_type: TIMELINE_ROLLBACK_EVENT_TYPE.into(),
                payload: payload.to_string(),
                run_id,
                step_number: Some(request.target_step),
            };
            if let Some(cutoff) = projection_cutoff.as_ref() {
                Self::truncate_session_projections_in_transaction(
                    &conn,
                    session_id,
                    &cutoff.created_at,
                    cutoff.inclusive,
                )?;
            }
            let mut events = Self::append_batch_in_transaction(&conn, session_id, &[input])?;
            let event = events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("rollback append returned no event"))?;
            let replacement = if replacement_transcript.is_empty() {
                Vec::new()
            } else {
                Self::append_batch_in_transaction(&conn, session_id, replacement_transcript)?
            };
            let cursor = Self::cursor_in_connection(&conn, session_id)?;
            Ok(RollbackResult {
                marker: event,
                replacement_events: replacement,
                cursor,
                to_sequence,
            })
        })();
        match result {
            Ok(result) => {
                commit_sqlite_transaction(&conn, "session rollback")?;
                self.db.cache_invalidate_messages(session_id);
                let _ = self.live_tx.send(result.marker.clone());
                for event in &result.replacement_events {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(result)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "session rollback");
                Err(error)
            }
        }
    }

    /// Run the complete rollback transaction on SQLite's blocking pool,
    /// including its marker, projection changes, and optional replacement
    /// transcript. Dropping this future cannot interrupt a transaction
    /// already running on Tokio's blocking pool.
    pub async fn rollback_to_async(
        &self,
        session_id: &str,
        request: RollbackRequest,
        replacement_transcript: Vec<SessionEventInput>,
        run_id: Option<u64>,
    ) -> anyhow::Result<RollbackResult> {
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| {
                store.rollback_to(&session_id, &request, &replacement_transcript, run_id)
            })
            .await
    }

    /// Truncate only materialized projections through the session store.
    /// Continue/retry uses this boundary without moving the active event
    /// timeline.
    ///
    /// The active branch point and its projection cutoff are resolved inside
    /// the same immediate transaction as the projection deletion. A missing
    /// branch point or timestamp is a safe no-op.
    #[cfg(test)]
    fn truncate_projection_after_step(
        &self,
        session_id: &str,
        step_number: u32,
    ) -> anyhow::Result<()> {
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result =
            Self::truncate_projection_after_step_in_transaction(&conn, session_id, step_number);
        match result {
            Ok(events) => {
                commit_sqlite_transaction(&conn, "truncate transcript projection")?;
                if let Some(events) = events {
                    self.db.cache_invalidate_messages(session_id);
                    for event in events {
                        let _ = self.live_tx.send(event);
                    }
                }
                Ok(())
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "truncate transcript projection");
                Err(error)
            }
        }
    }

    /// Use the newest recovery marker in append-only history to decide whether
    /// continue may remove projections. Marker lookup, phase validation,
    /// active branch-point resolution, projection deletion, usage
    /// compensation, and aggregate rebuilding share one write transaction.
    /// A marker outside the active replay still participates in the decision.
    fn truncate_projection_after_latest_committed_recovery(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Option<Vec<SessionEvent>>> {
            let Some(marker) = Self::latest_recovery_persistence_in_connection(&conn, session_id)?
            else {
                return Ok(None);
            };
            let phase = serde_json::from_str::<serde_json::Value>(&marker.payload)
                .ok()
                .and_then(|payload| payload.get("phase")?.as_str().map(str::to_owned));
            if phase.as_deref() != Some("committed") {
                return Ok(None);
            }

            let step_number = marker.step_number.ok_or_else(|| {
                anyhow::anyhow!(
                    "committed recovery event at sequence {} is missing step_number",
                    marker.sequence
                )
            })?;
            Self::truncate_projection_after_step_in_transaction(&conn, session_id, step_number)
        })();
        match result {
            Ok(events) => {
                commit_sqlite_transaction(&conn, "truncate committed recovery projection")?;
                if let Some(events) = events {
                    self.db.cache_invalidate_messages(session_id);
                    for event in events {
                        let _ = self.live_tx.send(event);
                    }
                }
                Ok(())
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "truncate committed recovery projection");
                Err(error)
            }
        }
    }

    /// Apply the committed-recovery projection cutoff on SQLite's blocking
    /// pool by reusing the existing single-transaction operation. Dropping
    /// this future cannot interrupt a transaction already running on Tokio's
    /// blocking pool.
    pub async fn truncate_projection_after_latest_committed_recovery_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| {
                store.truncate_projection_after_latest_committed_recovery(&session_id)
            })
            .await
    }

    fn truncate_projection_after_step_in_transaction(
        conn: &rusqlite::Connection,
        session_id: &str,
        step_number: u32,
    ) -> anyhow::Result<Option<Vec<SessionEvent>>> {
        ensure_session_id(session_id)?;
        let cutoff = Self::read_active_branch_points_in_connection(conn, session_id)?
            .into_iter()
            .find(|point| point.step_number == step_number)
            .and_then(|point| point.last_msg_at)
            .map(|created_at| ProjectionCutoff {
                created_at,
                inclusive: false,
            });
        let Some(cutoff) = cutoff else {
            return Ok(None);
        };

        let mut statement =
            conn.prepare("SELECT id FROM llm_usage WHERE session_id = ?1 AND created_at > ?2")?;
        let usage_ids = statement
            .query_map(rusqlite::params![session_id, cutoff.created_at], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);

        Self::truncate_session_projections_in_transaction(
            conn,
            session_id,
            &cutoff.created_at,
            false,
        )?;
        let discard_inputs = usage_ids
            .iter()
            .map(|usage_id| {
                SessionEventInput::new(
                    USAGE_DISCARDED_EVENT_TYPE,
                    serde_json::json!({ "usage_id": usage_id }).to_string(),
                )
            })
            .collect::<Vec<_>>();
        Self::append_batch_in_transaction(conn, session_id, &discard_inputs).map(Some)
    }

    #[cfg(test)]
    fn truncate_projection_after(
        &self,
        session_id: &str,
        cutoff: &ProjectionCutoff,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Vec<SessionEvent>> {
            let op = if cutoff.inclusive { ">=" } else { ">" };
            let usage_sql =
                format!("SELECT id FROM llm_usage WHERE session_id = ?1 AND created_at {op} ?2");
            let mut statement = conn.prepare(&usage_sql)?;
            let usage_ids = statement
                .query_map(rusqlite::params![session_id, cutoff.created_at], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            Self::truncate_session_projections_in_transaction(
                &conn,
                session_id,
                &cutoff.created_at,
                cutoff.inclusive,
            )?;
            let discard_inputs = usage_ids
                .iter()
                .map(|usage_id| {
                    SessionEventInput::new(
                        USAGE_DISCARDED_EVENT_TYPE,
                        serde_json::json!({ "usage_id": usage_id }).to_string(),
                    )
                })
                .collect::<Vec<_>>();
            if discard_inputs.is_empty() {
                return Ok(Vec::new());
            }
            Self::append_batch_in_transaction(&conn, session_id, &discard_inputs)
        })();
        match result {
            Ok(events) => {
                commit_sqlite_transaction(&conn, "discard rolled back usage")?;
                self.db.cache_invalidate_messages(session_id);
                for event in events {
                    let _ = self.live_tx.send(event);
                }
                Ok(())
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "discard rolled back usage");
                Err(error)
            }
        }
    }

    /// Append usage domain events and project them into the usage tables in
    /// the same transaction. This is the only live usage write boundary for
    /// Agent-owned, tool-owned and media-owned model calls. The synchronous
    /// form exists only for fixtures; production uses
    /// `append_usage_batch_cancellable`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_usage(
        &self,
        session_id: &str,
        input: &LlmUsageRecordInput,
    ) -> anyhow::Result<LlmUsageRecord> {
        let mut records = self.append_usage_batch(session_id, std::slice::from_ref(input))?;
        records
            .pop()
            .ok_or_else(|| anyhow::anyhow!("usage append returned no record"))
    }

    fn append_usage_batch(
        &self,
        session_id: &str,
        inputs: &[LlmUsageRecordInput],
    ) -> anyhow::Result<Vec<LlmUsageRecord>> {
        ensure_session_id(session_id)?;
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let mut records = inputs
            .iter()
            .map(|input| {
                LlmUsageRecord::from_input(
                    haven_common::types::new_id("usage"),
                    session_id,
                    input,
                    String::new(),
                )
            })
            .collect::<Vec<_>>();
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(Vec<LlmUsageRecord>, Vec<SessionEvent>)> {
            let mut previous_created_at: Option<String> = None;
            for record in &mut records {
                let created_at =
                    next_projection_created_at(&conn, session_id, previous_created_at.as_deref())?;
                record.created_at = created_at.clone();
                previous_created_at = Some(created_at);
            }
            let events = records
                .iter()
                .map(|record| {
                    Ok(SessionEventInput {
                        event_type: USAGE_RECORDED_EVENT_TYPE.into(),
                        payload: serde_json::to_string(record)?,
                        run_id: None,
                        step_number: record.step_number,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let stored_events = Self::append_batch_in_transaction(&conn, session_id, &events)?;
            for record in &records {
                Database::project_llm_call_usage_conn(&conn, record)?;
            }
            Ok((std::mem::take(&mut records), stored_events))
        })();
        match result {
            Ok((records, stored_events)) => {
                commit_sqlite_transaction(&conn, "append usage batch")?;
                for event in stored_events {
                    let _ = self.live_tx.send(event);
                }
                Ok(records)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append usage batch");
                Err(error)
            }
        }
    }

    /// Append usage records on the blocking pool while keeping cancellation
    /// and SQLite interrupt handling inside the memory boundary.
    pub async fn append_usage_batch_cancellable(
        &self,
        session_id: &str,
        inputs: Vec<LlmUsageRecordInput>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Vec<LlmUsageRecord>> {
        ensure_session_id(session_id)?;
        let session_id = session_id.to_owned();
        let store = self.clone();
        let persist = move |_db: &Database| store.append_usage_batch(&session_id, &inputs);
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, persist).await,
            None => self.db.run_blocking(persist).await,
        }
    }

    /// Record a compensating domain event when an in-flight usage write loses
    /// a rollback epoch race. Deleting only the projection would leave the
    /// usage event active and allow a later event replay to resurrect it.
    fn discard_usage(&self, session_id: &str, usage_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            is_canonical_id(session_id, "ses"),
            "session_id must be canonical"
        );
        anyhow::ensure!(
            is_canonical_id(usage_id, "usage"),
            "usage_id must be canonical"
        );
        let payload = serde_json::json!({ "usage_id": usage_id }).to_string();
        let input = SessionEventInput::new(USAGE_DISCARDED_EVENT_TYPE, payload);
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<SessionEvent> {
            let mut events = Self::append_batch_in_transaction(&conn, session_id, &[input])?;
            let event = events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("usage discard append returned no event"))?;
            conn.execute(
                "DELETE FROM llm_usage WHERE session_id = ?1 AND id = ?2",
                rusqlite::params![session_id, usage_id],
            )?;
            Database::rebuild_session_usage_from_calls_conn(&conn, session_id)?;
            Ok(event)
        })();
        match result {
            Ok(event) => {
                commit_sqlite_transaction(&conn, "discard usage")?;
                let _ = self.live_tx.send(event.clone());
                Ok(())
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "discard usage");
                Err(error)
            }
        }
    }

    /// Append a compensating usage-discard event on the blocking pool after
    /// a rollback epoch invalidates an already-written usage record.
    pub async fn discard_usage_cancellable(
        &self,
        session_id: &str,
        usage_id: &str,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<()> {
        ensure_session_id(session_id)?;
        anyhow::ensure!(
            is_canonical_id(usage_id, "usage"),
            "usage_id must be a canonical usage-* ID"
        );
        let session_id = session_id.to_owned();
        let usage_id = usage_id.to_owned();
        let store = self.clone();
        let discard = move |_db: &Database| store.discard_usage(&session_id, &usage_id);
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, discard).await,
            None => self.db.run_blocking(discard).await,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn append_branch_point_sync(
        &self,
        session_id: &str,
        event_cursor: usize,
        step_number: u32,
        last_msg_at: Option<&str>,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionEvent> {
        let payload = serde_json::json!({
            "event_cursor": event_cursor,
            "step_number": step_number,
            "last_msg_at": last_msg_at,
        });
        self.append_sync(
            session_id,
            BRANCH_POINT_EVENT_TYPE,
            &payload.to_string(),
            run_id,
            Some(step_number),
        )
    }

    /// Synchronous branch-point append for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_branch_point(
        &self,
        session_id: &str,
        event_cursor: usize,
        step_number: u32,
        last_msg_at: Option<&str>,
        run_id: Option<u64>,
    ) -> anyhow::Result<SessionEvent> {
        self.append_branch_point_sync(session_id, event_cursor, step_number, last_msg_at, run_id)
    }

    /// Append a branch point using the projection clocks observed under the
    /// same SQLite write transaction. ReAct callers receive the clocks as
    /// data; they never read `messages` or derive an event cursor locally.
    fn append_branch_point_from_projection(
        &self,
        session_id: &str,
        step_number: u32,
        run_id: Option<u64>,
    ) -> anyhow::Result<(SessionEvent, SessionCursor)> {
        ensure_session_id(session_id)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(SessionEvent, SessionCursor)> {
            let cursor = Self::cursor_in_connection(&conn, session_id)?;
            let payload = serde_json::json!({
                "event_cursor": cursor.event_cursor,
                "step_number": step_number,
                "last_msg_at": cursor.last_msg_at,
            });
            let input = SessionEventInput {
                event_type: BRANCH_POINT_EVENT_TYPE.into(),
                payload: payload.to_string(),
                run_id,
                step_number: Some(step_number),
            };
            let mut events = Self::append_batch_in_transaction(&conn, session_id, &[input])?;
            let event = events
                .pop()
                .ok_or_else(|| anyhow::anyhow!("branch point append returned no event"))?;
            Ok((event, cursor))
        })();
        match result {
            Ok(result) => {
                commit_sqlite_transaction(&conn, "append branch point")?;
                let _ = self.live_tx.send(result.0.clone());
                Ok(result)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "append branch point");
                Err(error)
            }
        }
    }

    /// Record one phase of the recovery-only persistence protocol. The
    /// marker is not part of transcript replay; it only makes a partially
    /// completed repair durable and auditable.
    fn append_recovery_persistence_sync(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<SessionEvent> {
        ensure_session_id(session_id)?;
        let payload = serde_json::json!({
            "phase": phase,
            "branch_point": status.branch_point,
            "partial_messages": status.partial_messages,
            "projection": status.projection,
            "event_boundary": status.event_boundary,
        });
        self.append_sync(
            session_id,
            RECOVERY_PERSISTENCE_EVENT_TYPE,
            &payload.to_string(),
            Some(run_id),
            Some(step_number),
        )
    }

    /// Synchronous recovery-marker append for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn append_recovery_persistence(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<SessionEvent> {
        self.append_recovery_persistence_sync(session_id, run_id, step_number, phase, status)
    }

    /// Return the latest recovery protocol marker, including failed markers
    /// outside the active timeline. Resume diagnostics must not lose this
    /// state merely because a later rollback moved the active cursor.
    #[cfg(any(test, feature = "test-support"))]
    fn latest_recovery_persistence_sync(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        let conn = self.db.conn();
        Self::latest_recovery_persistence_in_connection(&conn, session_id)
    }

    /// Synchronous recovery-marker lookup for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn latest_recovery_persistence(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        self.latest_recovery_persistence_sync(session_id)
    }

    fn latest_recovery_persistence_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        ensure_session_id(session_id)?;
        conn.query_row(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND event_type = ?2
             ORDER BY sequence DESC LIMIT 1",
            rusqlite::params![session_id, RECOVERY_PERSISTENCE_EVENT_TYPE],
            session_event_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    fn latest_sequence_sync(&self, session_id: &str) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        Ok(self.db.conn().query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )?)
    }

    /// Synchronous sequence inspection for fixtures and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn latest_sequence(&self, session_id: &str) -> anyhow::Result<i64> {
        self.latest_sequence_sync(session_id)
    }

    /// Read the latest durable event sequence on SQLite's blocking pool.
    pub async fn latest_sequence_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<i64> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| store.latest_sequence_sync(&session_id))
            .await
    }

    #[cfg(any(test, feature = "test-support"))]
    fn read_all_sync(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_from_sync(session_id, 0)
    }

    /// Read the complete append-only stream synchronously in fixtures/tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read_all(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_all_sync(session_id)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn read_from_sync(
        &self,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let conn = self.db.conn();
        Self::read_from_in_connection(&conn, session_id, after_sequence)
    }

    /// Read one bounded page of durable events strictly after
    /// `after_sequence`. Rows are ordered by ascending sequence. The store
    /// owns the fixed page capacity.
    fn replay_page_sync(
        &self,
        session_id: &str,
        after_sequence: i64,
        limit: usize,
    ) -> anyhow::Result<SessionEventPage> {
        ensure_session_id(session_id)?;
        anyhow::ensure!(
            after_sequence >= 0,
            "event replay cursor cannot be negative"
        );
        anyhow::ensure!(
            (1..=MAX_SESSION_EVENT_REPLAY_PAGE_SIZE).contains(&limit),
            "event replay page size must be between 1 and {MAX_SESSION_EVENT_REPLAY_PAGE_SIZE}"
        );

        let conn = self.db.conn();
        let mut stmt = conn.prepare(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, after_sequence, (limit + 1) as i64],
            session_event_from_row,
        )?;
        let mut events = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = events.len() > limit;
        if has_more {
            events.pop();
        }
        let next_cursor = events
            .last()
            .map(|event| event.sequence)
            .unwrap_or(after_sequence);
        Ok(SessionEventPage {
            events,
            next_cursor,
            has_more,
        })
    }

    /// Read one bounded event page on SQLite's blocking pool.
    pub async fn replay_page_cancellable(
        &self,
        session_id: &str,
        after_sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<SessionEventPage> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| {
                store.replay_page_sync(
                    &session_id,
                    after_sequence,
                    MAX_SESSION_EVENT_REPLAY_PAGE_SIZE,
                )
            })
            .await
    }

    fn read_from_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let mut stmt = conn.prepare(
            "SELECT session_id, sequence, event_type, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, after_sequence],
            session_event_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Replay the current timeline. Rollback markers only move the active
    /// cursor; the underlying append-only rows remain available for audit and
    /// future branch tooling.
    fn read_active(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        let conn = self.db.conn();
        Self::read_active_in_connection(&conn, session_id)
    }

    /// Read the complete active event stream on SQLite's blocking pool. Agent
    /// recovery reducers use this when a control projection must be reconciled
    /// with canonical transcript events after an interrupted commit sequence.
    pub async fn read_active_events_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.read_active(&session_id))
            .await
    }

    fn read_active_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let started = Instant::now();
        // A compact_summary is a durable active-root marker.  The audit log
        // before it remains readable through read_all, but normal recovery
        // only needs the root and its suffix.  Fall back to a full replay if
        // an unusual late rollback targets before that root; correctness wins
        // over the optimization for that branch.
        let after_sequence = Self::active_replay_boundary_in_connection(conn, session_id)?;
        let mut active = Vec::new();
        for event in Self::read_from_in_connection(conn, session_id, after_sequence)? {
            if event.event_type == TIMELINE_ROLLBACK_EVENT_TYPE {
                let target = serde_json::from_str::<serde_json::Value>(&event.payload)?
                    .get("to_sequence")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or_else(|| anyhow::anyhow!("rollback event has no to_sequence"))?;
                active.retain(|candidate: &SessionEvent| candidate.sequence <= target);
            } else if event.event_type == TRANSCRIPT_EVENT_TYPE
                && serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()
                    .and_then(|payload| payload.get("type").cloned())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .as_deref()
                    == Some("compact_summary")
            {
                // Compaction intentionally replaces the active transcript
                // root. Older rows remain append-only history, but they no
                // longer belong to the replayable timeline.
                active.clear();
                active.push(event);
            } else {
                active.push(event);
            }
        }
        tracing::debug!(
            session_id,
            after_sequence,
            active_events = active.len(),
            scan_ms = started.elapsed().as_millis() as u64,
            "active session event replay scanned durable suffix"
        );
        Ok(active)
    }

    fn active_replay_boundary_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<i64> {
        let root = conn.query_row(
            "SELECT MAX(sequence) FROM session_events
                 WHERE session_id = ?1 AND event_type = ?2
                   AND json_extract(payload, '$.type') = 'compact_summary'",
            rusqlite::params![session_id, TRANSCRIPT_EVENT_TYPE],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let Some(root) = root else {
            return Ok(0);
        };

        let mut statement = conn.prepare(
            "SELECT payload FROM session_events
             WHERE session_id = ?1 AND sequence > ?2 AND event_type = ?3
             ORDER BY sequence ASC",
        )?;
        let rollback_payloads = statement.query_map(
            rusqlite::params![session_id, root, TIMELINE_ROLLBACK_EVENT_TYPE],
            |row| row.get::<_, String>(0),
        )?;
        for payload in rollback_payloads {
            let payload = payload?;
            let target = serde_json::from_str::<serde_json::Value>(&payload)?
                .get("to_sequence")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("rollback event has no to_sequence"))?;
            if target < root {
                return Ok(0);
            }
        }
        Ok(root.saturating_sub(1))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn read_active_transcript(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .read_active(session_id)?
            .into_iter()
            .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
            .collect())
    }

    /// Return active non-transcript domain events for the session.  ReAct
    /// recovery uses this for interaction state; messages and steps are never
    /// consulted to reconstruct the actor.
    fn read_active_domain_events_inner(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .read_active(session_id)?
            .into_iter()
            .filter(|event| event.event_type != TRANSCRIPT_EVENT_TYPE)
            .collect())
    }

    /// Synchronous active-event projection for fixture and unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn read_active_domain_events(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_active_domain_events_inner(session_id)
    }

    /// Read active non-transcript domain events through the session
    /// persistence boundary on SQLite's blocking pool.
    pub async fn read_active_domain_events_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.read_active_domain_events_inner(&session_id))
            .await
    }

    /// Return the latest active branch point for each step. Branch points are
    /// control events and are intentionally kept separate from transcript
    /// replay, but they share the same rollback cursor and audit log.
    #[cfg(test)]
    fn read_active_branch_points(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<ActiveBranchPoint>> {
        let conn = self.db.conn();
        Self::read_active_branch_points_in_connection(&conn, session_id)
    }

    fn read_active_branch_points_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<ActiveBranchPoint>> {
        let mut points = Vec::new();
        for event in Self::read_active_in_connection(conn, session_id)? {
            if event.event_type != BRANCH_POINT_EVENT_TYPE {
                continue;
            }
            let payload: serde_json::Value = serde_json::from_str(&event.payload)?;
            let event_cursor = payload
                .get("event_cursor")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| anyhow::anyhow!("branch point event has no event_cursor"))?;
            let step_number = payload
                .get("step_number")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| anyhow::anyhow!("branch point event has no step_number"))?;
            let last_msg_at = payload
                .get("last_msg_at")
                .and_then(serde_json::Value::as_str)
                .map(String::from);
            let point = ActiveBranchPoint {
                event_sequence: event.sequence,
                event_cursor,
                step_number,
                last_msg_at,
            };
            if let Some(index) = points
                .iter()
                .position(|point: &ActiveBranchPoint| point.step_number == step_number)
            {
                points[index] = point;
            } else {
                points.push(point);
            }
        }
        Ok(points)
    }

    fn sequence_for_transcript_cursor_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
        transcript_cursor: usize,
    ) -> anyhow::Result<i64> {
        let events = Self::read_active_in_connection(conn, session_id)?;
        if transcript_cursor == 0 {
            return Ok(0);
        }
        events
            .iter()
            .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
            .nth(transcript_cursor - 1)
            .map(|event| event.sequence)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "transcript cursor {} is outside the active session timeline",
                    transcript_cursor
                )
            })
    }

    /// Seed an event stream exactly once from the initial transcript. A
    /// non-empty stream is never rewritten, even if a stale seed is passed.
    fn seed_if_empty(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        Self::validate_inputs(events)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<Option<Vec<SessionEvent>>> {
            let latest_sequence: i64 = conn.query_row(
                "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )?;
            if latest_sequence != 0 {
                return Ok(None);
            }
            Ok(Some(Self::append_batch_in_transaction(
                &conn, session_id, events,
            )?))
        })();
        match result {
            Ok(Some(stored)) => {
                commit_sqlite_transaction(&conn, "seed session events")?;
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Ok(None) => {
                commit_sqlite_transaction(&conn, "seed session events")?;
                drop(conn);
                self.read_active(session_id)
            }
            Err(error) => {
                rollback_after_sqlite_failure(&conn, "seed session events");
                Err(error)
            }
        }
    }

    /// Seed an event stream once on SQLite's blocking pool, reusing the
    /// synchronous transaction, validation, and post-commit broadcast path.
    pub async fn seed_if_empty_async(
        &self,
        session_id: &str,
        events: Vec<SessionEventInput>,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        ensure_session_id(session_id)?;
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.seed_if_empty(&session_id, &events))
            .await
    }

    fn truncate_session_projections_in_transaction(
        conn: &rusqlite::Connection,
        session_id: &str,
        cutoff: &str,
        inclusive: bool,
    ) -> anyhow::Result<()> {
        let op = if inclusive { ">=" } else { ">" };
        let messages_sql =
            format!("DELETE FROM messages WHERE session_id = ?1 AND created_at {op} ?2");
        let steps_sql =
            format!("DELETE FROM session_steps WHERE session_id = ?1 AND created_at {op} ?2");
        let usage_sql =
            format!("DELETE FROM llm_usage WHERE session_id = ?1 AND created_at {op} ?2");
        conn.execute(&messages_sql, rusqlite::params![session_id, cutoff])?;
        conn.execute(&steps_sql, rusqlite::params![session_id, cutoff])?;
        conn.execute(&usage_sql, rusqlite::params![session_id, cutoff])?;
        Database::rebuild_session_usage_from_calls_conn(conn, session_id)
    }
}

impl Database {
    fn write_session_projections(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
        committed: &SessionCommitted,
    ) -> anyhow::Result<Vec<String>> {
        let message_count = committed
            .projections
            .iter()
            .filter(|projection| {
                matches!(projection, SessionProjectionIntent::AssistantMessage { .. })
            })
            .count();
        let mut message_created_at = Vec::with_capacity(message_count);
        // Keep materialization order stable: chat rows first, then thought
        // steps, then action steps. Branch-point `last_msg_at` and the UI
        // timeline rely on this established projection order.
        for projection in &committed.projections {
            if let SessionProjectionIntent::AcknowledgePendingUserInput { message_id } = projection
            {
                Database::acknowledge_pending_session_input(conn, session_id, message_id)?;
            }
        }

        for projection in &committed.projections {
            let SessionProjectionIntent::AssistantMessage {
                message_id,
                content,
                message_type,
            } = projection
            else {
                continue;
            };
            let created_at = next_projection_created_at(conn, session_id, None)?;
            conn.execute(
                "INSERT INTO message_ingress_cursors (session_id, last_ingress_seq)
                 VALUES (?1, 1)
                 ON CONFLICT(session_id) DO UPDATE SET
                    last_ingress_seq = message_ingress_cursors.last_ingress_seq + 1",
                rusqlite::params![session_id],
            )?;
            let ingress_seq: i64 = conn.query_row(
                "SELECT last_ingress_seq FROM message_ingress_cursors WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )?;
            conn.execute(
                "INSERT INTO messages
                    (id, session_id, role, content, message_type, created_at,
                     tool_call_id, ui_metadata, voice, ingress_seq, media_inputs)
                 VALUES (?1, ?2, 'assistant', ?3, ?4, ?5, NULL, NULL, 0, ?6, NULL)",
                rusqlite::params![
                    message_id,
                    session_id,
                    content,
                    (*message_type).map(TranscriptMessageKind::as_str),
                    created_at,
                    ingress_seq,
                ],
            )?;
            message_created_at.push(created_at);
        }

        for projection in &committed.projections {
            let SessionProjectionIntent::ThoughtStep {
                message_id,
                step_number,
            } = projection
            else {
                continue;
            };
            let created_at = next_projection_created_at(conn, session_id, None)?;
            conn.execute(
                "INSERT INTO session_steps
                    (id, session_id, step_number, tool_name, input, thought,
                     status, is_high_risk, created_at)
                 VALUES (?1, ?2, ?3, 'thought', ?1, NULL, 'completed', 0, ?4)",
                rusqlite::params![message_id, session_id, *step_number, created_at],
            )?;
            bump_step_sequence(conn, session_id)?;
        }

        for projection in &committed.projections {
            let SessionProjectionIntent::ToolStep {
                step_id,
                step_number,
                tool_index,
                tool_name,
                tool_input,
                tool_call_id,
                is_high_risk,
                silent,
            } = projection
            else {
                continue;
            };
            let created_at = next_projection_created_at(conn, session_id, None)?;
            let changed = conn.execute(
                "INSERT OR IGNORE INTO session_steps
                    (id, session_id, step_number, tool_index, tool_name, input,
                     tool_call_name, tool_call_input, tool_call_id, status, is_high_risk,
                     created_at, silent, confirmed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, NULL)",
                rusqlite::params![
                    step_id,
                    session_id,
                    *step_number,
                    *tool_index as i32,
                    tool_name,
                    tool_input,
                    tool_call_id,
                    *is_high_risk as i32,
                    created_at,
                    *silent as i32,
                ],
            )?;
            if changed > 0 {
                bump_step_sequence(conn, session_id)?;
            }
        }

        Ok(message_created_at)
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
    use crate::repositories::sessions::SessionOrigin;
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

        db.conn()
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
        let connection = db.conn();
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

        let conn = db.conn();
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
        db.complete_tool_step(&completed.step_id, "already finished", true)
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
            .create_session_with_origin(
                "kickoff seed",
                SessionOrigin::AgentSpawn {
                    parent_session_id: parent.session_id,
                },
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
        let conn = db.conn();
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
        let conn = db.conn();
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
        let conn = db.conn();
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

    #[test]
    fn event_rows_cannot_be_updated() {
        let (db, store, session_id) = store();
        let event = store
            .append_transcript(&session_id, r#"{"type":"immutable"}"#, 1, 1)
            .unwrap();
        let error = db
            .conn()
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
