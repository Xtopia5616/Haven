//! Durable, append-only session event storage.
//!
//! `session_events` is the recovery authority for a session.  Producers own
//! the JSON payload and event type; this repository only provides ordering,
//! transactionality and timeline control.  A rollback never deletes history:
//! it appends a `timeline_rollback` marker and readers replay the active
//! timeline from the complete log.

use crate::Database;
use crate::repositories::messages::{Message, now_rfc3339_millis, undelivered_recovery_since};
use crate::repositories::session_steps::{ActionStepOutcome, ActionStepWrite, SessionStep};
use crate::repositories::sessions::Session;
use crate::repositories::usage::{LlmCallUsage, LlmCallUsageInput, SessionUsage};
use chrono::{SecondsFormat, Utc};
use haven_common::SessionStatus;
use haven_common::media::MediaInput;
use haven_common::types::MessageAttachment;
use rusqlite::OptionalExtension;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

pub const TRANSCRIPT_EVENT_TYPE: &str = "transcript";
/// Agent-owned durable signal for memory work. Memory stores the event
/// without depending on Agent payload types.
pub const MEMORY_TRIGGER_EVENT_TYPE: &str = "memory_trigger";
pub const BRANCH_POINT_EVENT_TYPE: &str = "branch_point";
pub const TIMELINE_ROLLBACK_EVENT_TYPE: &str = "timeline_rollback";
/// Durable usage domain events. Their payload is the complete
/// [`LlmCallUsage`] value; `llm_usage` and `session_usage` are projections.
pub const USAGE_RECORDED_EVENT_TYPE: &str = "usage_recorded";
pub const USAGE_DISCARDED_EVENT_TYPE: &str = "usage_discarded";
/// Session-local interaction lifecycle events. The payload is an Agent-owned
/// `InteractionRequest` or a small `{ "ids": [...] }` object; Memory only
/// orders and durably stores the event.
pub const INTERACTION_REQUESTED_EVENT_TYPE: &str = "interaction_requested";
pub const INTERACTION_RESOLVED_EVENT_TYPE: &str = "interaction_resolved";
pub const INTERACTION_CLEARED_EVENT_TYPE: &str = "interaction_cleared";
/// Two-phase marker for recovery-only partial persistence. It is deliberately
/// an append-only control event so an interrupted repair remains observable
/// even when the snapshot cache is stale or unreadable.
pub const RECOVERY_PERSISTENCE_EVENT_TYPE: &str = "recovery_persistence";
pub const CURRENT_EVENT_VERSION: i64 = 1;
/// Hard ceiling for one live transcript transaction. ReAct context/tool
/// batches are smaller; this protects the persistence boundary if a future
/// caller constructs a batch without going through those planners.
pub const MAX_TRANSCRIPT_BATCH_EVENTS: usize = 128;
pub const MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS: usize = 256;
/// Hard ceiling for the total variable-width payload persisted by one live
/// transcript transaction. This includes event JSON and projection text so a
/// bounded row count cannot be bypassed with one oversized tool input.
pub const MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;

pub type StoredBranchPoint = (SessionEvent, usize, u32, Option<String>);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionEvent {
    pub session_id: String,
    pub sequence: i64,
    pub event_type: String,
    pub event_version: i64,
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
pub const MAX_SESSION_EVENT_REPLAY_PAGE_SIZE: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SessionEventInput {
    pub event_type: String,
    pub payload: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
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
    pub branch_points: Vec<StoredBranchPoint>,
    pub cursor: SessionCursor,
}

/// The projection side of a rollback boundary. `inclusive` is true for a
/// user-message rollback (the selected message is removed) and false for a
/// failed/agent step rollback (the branch-point message is retained).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCutoff {
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
    /// point exists, retain the legacy snapshot fallback at the latest user
    /// message timestamp.
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
pub struct SessionCommittedEvent {
    pub payload: String,
    pub run_id: u64,
    pub step_number: u32,
}

impl SessionCommittedEvent {
    pub fn new(payload: impl Into<String>, run_id: u64, step_number: u32) -> Self {
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
pub enum SessionProjectionIntent {
    AssistantMessage {
        message_id: String,
        content: String,
        message_type: Option<String>,
    },
    ThoughtStep {
        message_id: String,
        step_number: u32,
    },
    ActionStep {
        step_id: String,
        step_number: u32,
        action_index: u32,
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
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SessionCommitted {
    pub events: Vec<SessionCommittedEvent>,
    pub projections: Vec<SessionProjectionIntent>,
}

impl SessionCommitted {
    pub fn transcript(payload: impl Into<String>, run_id: u64, step_number: u32) -> Self {
        Self {
            events: vec![SessionCommittedEvent::new(payload, run_id, step_number)],
            projections: Vec::new(),
        }
    }

    pub fn push_transcript(&mut self, payload: impl Into<String>, run_id: u64, step_number: u32) {
        self.events
            .push(SessionCommittedEvent::new(payload, run_id, step_number));
    }

    pub fn project_assistant_message(
        &mut self,
        message_id: impl Into<String>,
        content: impl Into<String>,
        message_type: Option<String>,
    ) {
        self.projections
            .push(SessionProjectionIntent::AssistantMessage {
                message_id: message_id.into(),
                content: content.into(),
                message_type,
            });
    }

    pub fn project_thought_step(&mut self, message_id: impl Into<String>, step_number: u32) {
        self.projections.push(SessionProjectionIntent::ThoughtStep {
            message_id: message_id.into(),
            step_number,
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub fn project_action_step(
        &mut self,
        step_id: impl Into<String>,
        step_number: u32,
        action_index: u32,
        tool_name: impl Into<String>,
        tool_input: impl Into<String>,
        tool_call_id: Option<String>,
        is_high_risk: bool,
        silent: bool,
    ) {
        self.projections.push(SessionProjectionIntent::ActionStep {
            step_id: step_id.into(),
            step_number,
            action_index,
            tool_name: tool_name.into(),
            tool_input: tool_input.into(),
            tool_call_id,
            is_high_risk,
            silent,
        });
    }

    fn event_inputs(&self) -> Vec<SessionEventInput> {
        self.events
            .iter()
            .map(|event| {
                SessionEventInput::transcript(
                    event.payload.clone(),
                    event.run_id,
                    event.step_number,
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct SessionCommitResult {
    pub events: Vec<SessionEvent>,
    /// Created-at values for message rows, in insertion order.  Agent uses
    /// the last value to advance its in-memory `last_msg_at` sidecar.
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

/// Typed filters for app-facing session history queries.
///
/// `limit` and `offset` are required so each caller keeps ownership of its
/// existing defaults (the history page and export have different limits).
#[derive(Debug, Clone)]
pub struct SessionHistoryFilter {
    pub query: Option<String>,
    pub status: Option<String>,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

/// The role and text needed to assemble a fresh-run conversation window.
/// Agent keeps ownership of its `ConversationMessage` prompt type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessageText {
    pub role: String,
    pub content: String,
}

/// The materialized media needed to initialize a session run.
///
/// This read model does not define resume authority or register/lease managed
/// assets. Durable event replay and Agent-owned asset lifecycle stay separate.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionResumeMedia {
    pub initial_message_id: Option<String>,
    pub initial_attachments: Vec<MessageAttachment>,
    pub initial_media_inputs: Vec<MediaInput>,
    pub all_attachments: Vec<MessageAttachment>,
}

/// User-only transcript context used to generate a session title.
///
/// The store checks session existence and the persisted title before loading
/// the latest ten title-eligible messages, then applies the existing role
/// filter while preserving chronological order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTitleGenerationContext {
    pub user_messages: Vec<String>,
}

const TITLE_GENERATION_MESSAGE_LIMIT: usize = 10;

/// Existing read models needed to build the App's session-resume response.
///
/// The projection groups the current queries behind one SessionStore port;
/// its fields do not imply a shared database snapshot or transaction.
#[derive(Debug, Clone)]
pub struct SessionResumeProjection {
    pub messages: Vec<Message>,
    pub steps: Vec<SessionStep>,
    pub usage: Option<SessionUsage>,
    pub llm_usage: Vec<LlmCallUsage>,
    pub active_domain_events: Vec<SessionEvent>,
}

/// Transitional name for code that only consumes the append-only event API.
/// New ownership code should use [`SessionStore`].
pub type SessionEventStore = SessionStore;

/// A race-safe handoff from durable replay to live events.
///
/// The receiver is subscribed before the database replay starts. Events
/// committed during that replay may therefore occur in both collections; a
/// consumer must advance by `(session_id, sequence)` and ignore duplicates.
#[derive(Debug)]
pub struct SessionEventSubscription {
    pub replay: Vec<SessionEvent>,
    pub live: tokio::sync::broadcast::Receiver<SessionEvent>,
}

impl SessionStore {
    pub fn new(db: Arc<Database>) -> Self {
        let (live_tx, _) = tokio::sync::broadcast::channel(256);
        Self { db, live_tx }
    }

    /// Mark orphaned running sessions as errored through the typed session
    /// persistence boundary. The underlying Database operation retains its
    /// partial-message promotion and cache invalidation semantics.
    pub async fn finalize_orphaned_running_sessions(&self) -> anyhow::Result<usize> {
        self.db
            .run_blocking(|db| db.finalize_orphaned_running_sessions())
            .await
    }

    /// Delete sessions older than the supplied retention window through the
    /// typed session boundary. The repository operation remains responsible
    /// for session-scoped memory cleanup and cache invalidation.
    pub async fn delete_old_sessions(&self, retention_days: u32) -> anyhow::Result<usize> {
        self.db
            .run_blocking(move |db| db.delete_old_sessions(retention_days))
            .await
    }

    /// Return host-managed attachment paths still referenced by messages.
    /// This is a read-only typed port used by media retention cleanup.
    pub async fn list_managed_attachment_paths(&self) -> anyhow::Result<Vec<PathBuf>> {
        self.db
            .run_blocking(|db| db.list_managed_attachment_paths())
            .await
    }

    /// Persist a session lifecycle status on SQLite's blocking pool.
    ///
    /// The caller owns lifecycle retry and in-memory transition policy; this
    /// port only schedules the existing Database write. Dropping this future
    /// cannot interrupt a write already running on Tokio's blocking pool.
    pub async fn update_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.update_session_status(&session_id, status))
            .await
    }

    /// Ensure a pending action-step row exists with the supplied durable
    /// invocation identity and confirmation decision.
    pub async fn ensure_action_step(
        &self,
        write: ActionStepWrite,
        confirmed: Option<bool>,
    ) -> anyhow::Result<()> {
        self.db
            .run_blocking(move |db| {
                db.ensure_action_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.action_index,
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
    pub async fn ensure_and_start_action_step(
        &self,
        write: ActionStepWrite,
        confirmed: Option<bool>,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.ensure_action_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.action_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )?;
                db.start_action_step(&write.step_id)
            })
            .await
    }

    /// Ensure an action-step row and record its final observation/outcome as
    /// one blocking-pool operation, retaining the existing Database order.
    pub async fn ensure_and_finish_action_step(
        &self,
        write: ActionStepWrite,
        confirmed: Option<bool>,
        observation: String,
        outcome: ActionStepOutcome,
    ) -> anyhow::Result<bool> {
        self.db
            .run_blocking(move |db| {
                db.ensure_action_step_with_identity(
                    &write.session_id,
                    write.step_number,
                    write.action_index,
                    &write.tool_name,
                    &write.tool_input,
                    write.tool_call_id.as_deref(),
                    write.is_high_risk,
                    write.silent,
                    confirmed,
                    &write.step_id,
                )?;
                db.finish_action_step(&write.step_id, &observation, outcome)
            })
            .await
    }

    /// Delete one durable session through the session persistence boundary.
    ///
    /// The existing Database method remains responsible for cascades,
    /// session-scoped cleanup, and cache invalidation. Dropping this future
    /// cannot interrupt a write already running on Tokio's blocking pool.
    pub async fn delete_session(&self, session_id: &str) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.delete_session(&session_id))
            .await
    }

    /// Delete a persisted message through the session persistence boundary.
    ///
    /// The existing Database method remains responsible for message cache
    /// invalidation. This port only schedules that operation on the blocking
    /// pool and returns its result unchanged.
    pub async fn delete_message_by_id(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| db.delete_message_by_id(&session_id, &message_id))
            .await
    }

    /// Atomically clear durable sessions through the session persistence
    /// boundary and return the number of deleted session rows.
    ///
    /// The existing Database method remains responsible for the transaction,
    /// session-scoped cleanup, and cache invalidation. Dropping this future
    /// cannot interrupt a write already running on Tokio's blocking pool.
    pub async fn clear_sessions(&self) -> anyhow::Result<usize> {
        self.db.clone().run_blocking(|db| db.clear_sessions()).await
    }

    /// Load the latest textual messages for a fresh-run conversation window.
    ///
    /// The underlying query preserves its existing message-type filter,
    /// chronological result order, and `limit` behavior. Dropping this future
    /// cannot interrupt a `run_blocking` query already running on Tokio's
    /// blocking pool.
    pub async fn conversation_window(
        &self,
        session_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SessionMessageText>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                Ok(db
                    .get_session_messages_limit(&session_id, limit)?
                    .into_iter()
                    .map(|message| SessionMessageText {
                        role: message.role,
                        content: message.content,
                    })
                    .collect())
            })
            .await
    }

    /// Load the ordered message media needed to initialize a session run.
    ///
    /// Messages are read and aggregated in one blocking-pool closure using
    /// the existing `get_session_messages` ordering. The first user message
    /// supplies the initial input media; all message attachments are flattened
    /// in message order for the caller's asset registration. This read model
    /// does not replay events or register/lease managed assets.
    pub async fn session_resume_media(
        &self,
        session_id: &str,
    ) -> anyhow::Result<SessionResumeMedia> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let messages = db.get_session_messages(&session_id)?;
                let all_attachments = messages
                    .iter()
                    .flat_map(|message| message.attachments.iter().cloned())
                    .collect();
                let initial_message = messages.iter().find(|message| message.role == "user");
                Ok(SessionResumeMedia {
                    initial_message_id: initial_message.map(|message| message.id.clone()),
                    initial_attachments: initial_message
                        .map(|message| message.attachments.clone())
                        .unwrap_or_default(),
                    initial_media_inputs: initial_message
                        .map(|message| message.media_inputs.clone())
                        .unwrap_or_default(),
                    all_attachments,
                })
            })
            .await
    }

    /// Load the user messages used for title generation on SQLite's blocking
    /// pool. Missing sessions and sessions that already have a title return
    /// `None`; an existing untitled session returns its user-only context,
    /// which may be empty. The original limit, message eligibility, role
    /// filtering, and chronological order are preserved. Dropping this future
    /// cannot interrupt a query already running on Tokio's blocking pool.
    pub async fn title_generation_context(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionTitleGenerationContext>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                let Some(session) = db.get_session(&session_id)? else {
                    return Ok(None);
                };
                if session.title.is_some() {
                    return Ok(None);
                }

                let user_messages = db
                    .get_session_messages_limit(&session_id, TITLE_GENERATION_MESSAGE_LIMIT)?
                    .into_iter()
                    .filter(|message| message.role == "user")
                    .map(|message| message.content)
                    .collect();
                Ok(Some(SessionTitleGenerationContext { user_messages }))
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
        let session_id = session_id.to_owned();
        let store = self.clone();
        self.db
            .run_blocking(move |db| {
                let messages = db.get_session_messages(&session_id)?;
                let steps = db.get_session_steps(&session_id)?;
                let usage = db.get_session_usage(&session_id)?;
                let llm_usage = db.get_session_llm_usage(&session_id)?;
                let active_domain_events = store.read_active_domain_events(&session_id)?;
                Ok(SessionResumeProjection {
                    messages,
                    steps,
                    usage,
                    llm_usage,
                    active_domain_events,
                })
            })
            .await
    }

    /// List session history on SQLite's blocking pool.
    ///
    /// This delegates to the existing database query, including its first
    /// page cache behavior and `created_at DESC` ordering. Dropping the
    /// returned future cannot interrupt a `run_blocking` query already
    /// running on Tokio's blocking pool.
    pub async fn list_history(&self, limit: i64, offset: i64) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| db.list_sessions(limit, offset))
            .await
    }

    /// Read the most recently created session using the existing history
    /// ordering and first-row behavior. Dropping this future cannot interrupt
    /// a query already running on Tokio's blocking pool.
    pub async fn latest_session_record(&self) -> anyhow::Result<Option<Session>> {
        Ok(self.list_history(1, 0).await?.into_iter().next())
    }

    /// Count persisted sessions on SQLite's blocking pool.
    ///
    /// Dropping the returned future cannot interrupt a `run_blocking` query
    /// already running on Tokio's blocking pool.
    pub async fn count_history(&self) -> anyhow::Result<i64> {
        self.db.run_blocking(|db| db.count_sessions()).await
    }

    /// Search and page session history using the existing database predicate
    /// and ordering. Dropping the returned future cannot interrupt a
    /// `run_blocking` query already running on Tokio's blocking pool.
    pub async fn search_history_paginated(
        &self,
        query: String,
        limit: i64,
        offset: i64,
    ) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| db.search_sessions_paginated(&query, limit, offset))
            .await
    }

    /// Count matches using the existing database search predicate.
    /// Dropping the returned future cannot interrupt a `run_blocking` query
    /// already running on Tokio's blocking pool.
    pub async fn count_history_search(&self, query: String) -> anyhow::Result<i64> {
        self.db
            .run_blocking(move |db| db.count_sessions_search(&query))
            .await
    }

    /// Search the first 50 session history matches using the existing
    /// database predicate and ordering. Dropping the returned future cannot
    /// interrupt a `run_blocking` query already running on Tokio's blocking
    /// pool.
    pub async fn search_history(&self, query: String) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| db.search_sessions(&query))
            .await
    }

    /// Apply typed filters through the existing database query, preserving
    /// its empty-filter handling, date conversion, cache behavior, predicate
    /// and ordering. Dropping the returned future cannot interrupt a
    /// `run_blocking` query already running on Tokio's blocking pool.
    pub async fn search_history_filtered(
        &self,
        filter: SessionHistoryFilter,
    ) -> anyhow::Result<Vec<Session>> {
        self.db
            .run_blocking(move |db| {
                db.search_sessions_filtered(
                    filter.query.as_deref(),
                    filter.status.as_deref(),
                    filter.start_date.as_deref(),
                    filter.end_date.as_deref(),
                    filter.limit,
                    filter.offset,
                )
            })
            .await
    }

    /// Read all persisted session ids on SQLite's blocking pool.
    pub async fn all_session_ids_cancellable(
        &self,
        cancel: CancellationToken,
    ) -> anyhow::Result<Vec<String>> {
        self.db
            .run_blocking_cancellable(cancel, |db| db.all_session_ids())
            .await
    }

    /// Create a durable session record through the session persistence
    /// boundary. Actor installation, lifecycle gates, and dispatch remain
    /// owned by the Agent layer.
    pub async fn create_session(&self, input_text: &str) -> anyhow::Result<Session> {
        let input_text = input_text.to_owned();
        self.db
            .run_blocking(move |db| db.create_session(&input_text))
            .await
    }

    /// Load the materialized session usage through the usage store boundary.
    /// The caller never needs to borrow the raw Database just to seed a live
    /// usage tracker.
    pub async fn load_session_usage(
        &self,
        session_id: &str,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Option<SessionUsage>> {
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
        let session_id = session_id.to_owned();
        let message_id = message_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.create_thought_step(&session_id, step_number as i32, &message_id)?;
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

    /// Read a persisted session record by id for actor installation.
    ///
    /// A missing record remains `Ok(None)` so lifecycle callers can preserve
    /// their existing not-found behavior.
    pub fn session_record(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        self.db.get_session(session_id)
    }

    /// Pause all running sessions during the synchronous application exit
    /// callback. This delegates to the existing repository operation so its
    /// status transition, update count, and crash recovery behavior stay the
    /// same.
    pub fn pause_running_sessions(&self) -> anyhow::Result<usize> {
        self.db.pause_running_sessions()
    }

    /// Read a persisted session record by id on SQLite's blocking pool.
    ///
    /// The synchronous `session_record` remains available to Agent lifecycle
    /// callers that require a synchronous lookup. Missing records remain
    /// `Ok(None)` for either API.
    pub async fn load_session_record(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.get_session(&session_id))
            .await
    }

    /// Read only the title needed by the messaging heartbeat projection.
    /// Keeping this narrow avoids exposing the raw Database to Agent context
    /// assembly merely for a presentation field.
    pub async fn session_title(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| Ok(db.get_session(&session_id)?.and_then(|s| s.title)))
            .await
    }

    /// Read the title shown when ending a session, falling back to its input
    /// text when no title has been assigned. Missing sessions remain `None`.
    /// SQLite work runs on the blocking pool; dropping this future cannot
    /// interrupt a query that has already started.
    pub async fn session_display_title(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                Ok(db
                    .get_session(&session_id)?
                    .map(|session| session.title.unwrap_or(session.input_text)))
            })
            .await
    }

    /// Persist a session title on SQLite's blocking pool.
    ///
    /// This delegates to the existing Database operation so its cache
    /// invalidation and error behavior remain authoritative. Dropping this
    /// future cannot interrupt a write already running on Tokio's blocking
    /// pool.
    pub async fn update_session_title(&self, session_id: &str, title: &str) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        let title = title.to_owned();
        self.db
            .run_blocking(move |db| db.update_session_title(&session_id, &title))
            .await
    }

    /// Persist an ingress or recovery message through the session boundary.
    ///
    /// SQLite work runs on the blocking pool and may be cooperatively
    /// cancelled. A supplied message id preserves the Agent's retry
    /// idempotency behavior; without one, the messages repository mints a new
    /// id through `add_message_full`.
    #[allow(clippy::too_many_arguments)]
    pub async fn persist_session_message(
        &self,
        session_id: &str,
        role: &str,
        content: &str,
        message_type: Option<&str>,
        attachments: &[MessageAttachment],
        voice: bool,
        message_id: Option<&str>,
        tool_call_id: Option<&str>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Message> {
        let session_id = session_id.to_owned();
        let role = role.to_owned();
        let content = content.to_owned();
        let message_type = message_type.map(str::to_owned);
        let attachments = attachments.to_vec();
        let message_id = message_id.map(str::to_owned);
        let tool_call_id = tool_call_id.map(str::to_owned);
        let persist = move |db: &Database| {
            if let Some(message_id) = message_id.as_deref()
                && let Some(existing) = db.get_message_by_id(&session_id, message_id)?
            {
                if existing.role != role
                    || existing.content != content
                    || existing.message_type.as_deref() != message_type.as_deref()
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
                &role,
                &content,
                message_type.as_deref(),
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
    /// projection `last_msg_at` clock. Promotion delegates to the existing
    /// Database operation, retaining its single-statement take and timestamp
    /// guard.
    pub async fn upsert_partial_stream(
        &self,
        session_id: &str,
        content: &str,
    ) -> anyhow::Result<()> {
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
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.promote_partial_message(&session_id))
            .await
    }

    /// Remove an unpromoted stream checkpoint. Missing rows remain a no-op.
    pub async fn discard_partial_stream(&self, session_id: &str) -> anyhow::Result<()> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| db.delete_partial_message(&session_id))
            .await
    }

    /// Read every pending session for dispatcher recovery, newest first.
    ///
    /// This is the semantic equivalent of the unbounded pending-session query:
    /// no text/date filters, `limit = -1`, and `offset = 0`. The generic search
    /// surface remains an implementation detail of the Database repository.
    pub fn pending_session_records(&self) -> anyhow::Result<Vec<Session>> {
        self.db.search_sessions_filtered(
            None,
            Some(SessionStatus::Pending.as_str()),
            None,
            None,
            -1,
            0,
        )
    }

    /// Mark pending/running action steps for a failed session as `unknown`.
    ///
    /// The existing session-steps repository owns the update semantics; this
    /// port only moves its SQLite work onto the blocking pool.
    pub async fn fail_pending_action_steps(
        &self,
        session_id: &str,
        observation: &str,
    ) -> anyhow::Result<usize> {
        let session_id = session_id.to_owned();
        let observation = observation.to_owned();
        self.db
            .run_blocking(move |db| db.fail_pending_action_steps(&session_id, &observation))
            .await
    }

    /// Subscribe to events committed through this store. Use
    /// [`Self::subscribe_from`] when a consumer needs a replay without a gap.
    /// A lagged receiver must perform the same replay again.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<SessionEvent> {
        self.live_tx.subscribe()
    }

    /// Subscribe before replaying the requested cursor so commits cannot fall
    /// between the replay and subscription steps. The replay and live stream
    /// can overlap at the boundary; consumers deduplicate by sequence.
    pub fn subscribe_from(
        &self,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<SessionEventSubscription> {
        let live = self.subscribe();
        let replay = self.read_from(session_id, after_sequence)?;
        Ok(SessionEventSubscription { replay, live })
    }

    /// Read the durable memory event cursor. This clock is independent from
    /// the fact-extraction message-id cursor and projection cursors.
    pub fn memory_event_cursor(&self, session_id: &str) -> anyhow::Result<i64> {
        self.db.memory_event_cursor(session_id)
    }

    /// Read the memory event cursor on SQLite's blocking pool with
    /// cancellation for an in-flight database operation.
    pub async fn memory_event_cursor_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<i64> {
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
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |db| {
                db.memory_event_cursor_optional(&session_id)
            })
            .await
    }

    /// Set a startup baseline only if no cursor key has ever been written.
    pub async fn initialize_memory_event_cursor_if_absent_cancellable(
        &self,
        session_id: &str,
        sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<bool> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |db| {
                db.initialize_memory_event_cursor_if_absent(&session_id, sequence)
            })
            .await
    }

    /// Advance the durable memory event cursor monotonically.
    pub fn checkpoint_memory_event_cursor(
        &self,
        session_id: &str,
        sequence: i64,
    ) -> anyhow::Result<()> {
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
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| {
                store.checkpoint_memory_event_cursor(&session_id, sequence)
            })
            .await
    }

    /// Clear one session's durable memory event cursor.
    pub fn clear_memory_event_cursor(&self, session_id: &str) -> anyhow::Result<()> {
        self.db.clear_memory_event_cursor(session_id)
    }

    /// Read all durable session clocks through one persistence boundary.
    ///
    /// This is intentionally the only Agent-facing API for `event_cursor`,
    /// `message_ingress_seq`, `step_seq` and `last_msg_at`.  The values are
    /// projection metadata, never a source for reconstructing the transcript.
    pub fn cursor(&self, session_id: &str) -> anyhow::Result<SessionCursor> {
        let conn = self.db.conn();
        Self::cursor_in_connection(&conn, session_id)
    }

    /// Read messages whose durable ingress sequence is newer than the
    /// checkpoint cursor. This query intentionally returns all message roles
    /// and types; callers apply the same recovery filtering as before.
    pub async fn messages_after_ingress_cursor(
        &self,
        session_id: &str,
        ingress_cursor: i64,
    ) -> anyhow::Result<Vec<Message>> {
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |db| {
                db.get_session_messages_since_ingress_seq(&session_id, ingress_cursor)
            })
            .await
    }

    /// Read recent user messages that have no session-step anchor. The
    /// existing two-day recovery window, first-user exclusion, ID filtering,
    /// ingress ordering and message projections are owned by the messages
    /// repository query.
    pub async fn recent_unanchored_user_messages(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<Message>> {
        let session_id = session_id.to_owned();
        let since_created_at = undelivered_recovery_since();
        self.db
            .run_blocking(move |db| {
                db.get_undelivered_user_messages_since(&session_id, &since_created_at)
            })
            .await
    }

    /// Load the active transcript, branch points and all projection clocks
    /// from one SQLite read boundary. This is the only recovery read surface
    /// exposed to Agent resume/rollback code.
    pub fn load_replay_state(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionReplayState>> {
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
        let store = self.clone();
        let session_id = session_id.to_owned();
        let phase = phase.to_owned();
        self.db
            .run_blocking(move |db| {
                if db.get_session(&session_id)?.is_none() {
                    return Ok(());
                }
                store.append_recovery_persistence(
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

    /// Return the latest user-message projection clock without exposing the
    /// underlying `messages` repository to Agent recovery code.
    pub fn last_user_message_at(&self, session_id: &str) -> anyhow::Result<Option<String>> {
        self.db.last_user_message_ts(session_id)
    }

    pub fn append(
        &self,
        session_id: &str,
        event_type: &str,
        payload: &str,
        run_id: Option<u64>,
        step_number: Option<u32>,
    ) -> anyhow::Result<SessionEvent> {
        let input = SessionEventInput {
            event_type: event_type.into(),
            payload: payload.into(),
            run_id,
            step_number,
        };
        self.append_batch(session_id, std::slice::from_ref(&input))
            .map(|mut events| events.remove(0))
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
        let store = self.clone();
        let session_id = session_id.to_owned();
        let event_type = event_type.to_owned();
        let payload = payload.to_owned();
        self.db
            .run_blocking(move |_| store.append(&session_id, &event_type, &payload, None, None))
            .await
    }

    /// Append a batch in one SQLite transaction.  Sequence allocation happens
    /// under `BEGIN IMMEDIATE`, so concurrent sessions and concurrent writers
    /// cannot produce duplicate per-session cursors.
    pub fn append_batch(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Self::validate_inputs(events)?;
        Self::validate_transcript_events(events)?;
        if events.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = Self::append_batch_in_transaction(&conn, session_id, events);
        match result {
            Ok(stored) => {
                conn.execute_batch("COMMIT")?;
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
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
        let store = self.clone();
        let session_id = session_id.to_owned();
        let write = move |_db: &Database| store.commit_transcript(&session_id, &committed);

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
    pub fn commit_transcript(
        &self,
        session_id: &str,
        committed: &SessionCommitted,
    ) -> anyhow::Result<SessionCommitResult> {
        let events = committed.event_inputs();
        Self::validate_inputs(&events)?;
        anyhow::ensure!(
            committed.events.len() <= MAX_TRANSCRIPT_BATCH_EVENTS,
            "transcript batch exceeds {} events",
            MAX_TRANSCRIPT_BATCH_EVENTS
        );
        Self::validate_transcript_events(&events)?;
        anyhow::ensure!(
            committed.projections.len() <= MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS,
            "transcript batch exceeds {} projection rows",
            MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS
        );
        let payload_bytes = Self::payload_bytes(committed)?;
        anyhow::ensure!(
            payload_bytes <= MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            "transcript batch exceeds {} payload bytes ({} bytes)",
            MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            payload_bytes
        );
        if committed.events.is_empty() {
            anyhow::ensure!(
                committed.projections.is_empty(),
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
                conn.execute_batch("COMMIT")?;
                if !result.message_created_at.is_empty() {
                    self.db.cache_invalidate_messages(session_id);
                }
                for event in &result.events {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(result)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn validate_inputs(events: &[SessionEventInput]) -> anyhow::Result<()> {
        for event in events {
            anyhow::ensure!(
                !event.event_type.trim().is_empty(),
                "session event type must not be empty"
            );
            anyhow::ensure!(
                serde_json::from_str::<serde_json::Value>(&event.payload).is_ok(),
                "session event payload must be valid JSON"
            );
        }
        Ok(())
    }

    /// Keep the generic append paths subject to the same hard ceiling as the
    /// projection-aware transcript writer. Control events remain on the
    /// generic path, but a batch containing transcript records must not be a
    /// back door around the transcript budget.
    fn validate_transcript_events(events: &[SessionEventInput]) -> anyhow::Result<()> {
        if !events
            .iter()
            .any(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
        {
            return Ok(());
        }
        anyhow::ensure!(
            events.len() <= MAX_TRANSCRIPT_BATCH_EVENTS,
            "transcript batch exceeds {} events",
            MAX_TRANSCRIPT_BATCH_EVENTS
        );
        let payload_bytes = serde_json::to_vec(events)?.len();
        anyhow::ensure!(
            payload_bytes <= MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            "transcript batch exceeds {} payload bytes ({} bytes)",
            MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            payload_bytes
        );
        Ok(())
    }

    fn payload_bytes(committed: &SessionCommitted) -> anyhow::Result<usize> {
        // The guard is defined over the exact JSON batch representation rather
        // than a hand-maintained sum of selected string fields. This accounts
        // for object/array keys, separators, numeric/boolean fields and JSON
        // escaping, which are all part of the variable-width persistence
        // payload at this boundary.
        Ok(serde_json::to_vec(committed)?.len())
    }

    fn append_batch_in_transaction(
        conn: &rusqlite::Connection,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
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
                    (session_id, sequence, event_type, event_version, payload,
                     created_at, run_id, step_number)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    session_id,
                    sequence,
                    event.event_type,
                    CURRENT_EVENT_VERSION,
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
                event_version: CURRENT_EVENT_VERSION,
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
        let last_msg_at = conn
            .query_row(
                "SELECT created_at FROM messages
                 WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                rusqlite::params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(SessionCursor {
            event_sequence,
            event_cursor,
            message_ingress_seq,
            step_seq,
            last_msg_at,
        })
    }

    pub fn append_transcript(
        &self,
        session_id: &str,
        payload: &str,
        run_id: u64,
        step_number: u32,
    ) -> anyhow::Result<SessionEvent> {
        self.append(
            session_id,
            TRANSCRIPT_EVENT_TYPE,
            payload,
            Some(run_id),
            Some(step_number),
        )
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
        let store = self.clone();
        let session_id = session_id.to_owned();
        let payload = payload.to_owned();
        self.db
            .run_blocking(move |_| {
                Ok(store
                    .append_transcript(&session_id, &payload, run_id, step_number)?
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
    pub fn append_rollback(
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
    pub fn rollback_to(
        &self,
        session_id: &str,
        request: &RollbackRequest,
        replacement_transcript: &[SessionEventInput],
        run_id: Option<u64>,
    ) -> anyhow::Result<RollbackResult> {
        Self::validate_inputs(replacement_transcript)?;
        Self::validate_transcript_events(replacement_transcript)?;
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
                .find(|(_, cursor, step, _)| {
                    *step == request.target_step && *cursor == request.transcript_cursor
                })
                .map_or(mapped_sequence, |(event, _, _, _)| event.sequence);
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
                        .find(|(_, _, step, _)| *step == request.target_step)
                    {
                        Some((_, _, _, Some(created_at))) => Some(ProjectionCutoff {
                            created_at: created_at.clone(),
                            inclusive: false,
                        }),
                        Some((_, _, _, None)) => None,
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
                conn.execute_batch("COMMIT")?;
                self.db.cache_invalidate_messages(session_id);
                let _ = self.live_tx.send(result.marker.clone());
                for event in &result.replacement_events {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(result)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
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
    pub fn truncate_projection_after_step(
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
                conn.execute_batch("COMMIT")?;
                if let Some(events) = events {
                    self.db.cache_invalidate_messages(session_id);
                    for event in events {
                        let _ = self.live_tx.send(event);
                    }
                }
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Use the newest recovery marker in append-only history to decide whether
    /// continue may remove projections. Marker lookup, phase validation,
    /// active branch-point resolution, projection deletion, usage
    /// compensation, and aggregate rebuilding share one write transaction.
    /// A marker outside the active replay still participates in the decision.
    pub fn truncate_projection_after_latest_committed_recovery(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
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

            // Preserve continue_session's legacy missing-step behavior: an
            // absent step is interpreted as zero, which safely no-ops unless
            // an active step-zero branch point exists.
            Self::truncate_projection_after_step_in_transaction(
                &conn,
                session_id,
                marker.step_number.unwrap_or_default(),
            )
        })();
        match result {
            Ok(events) => {
                conn.execute_batch("COMMIT")?;
                if let Some(events) = events {
                    self.db.cache_invalidate_messages(session_id);
                    for event in events {
                        let _ = self.live_tx.send(event);
                    }
                }
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
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
        let cutoff = Self::read_active_branch_points_in_connection(conn, session_id)?
            .into_iter()
            .find(|(_, _, step, _)| *step == step_number)
            .and_then(|(_, _, _, last_msg_at)| last_msg_at)
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

    pub fn truncate_projection_after(
        &self,
        session_id: &str,
        cutoff: &ProjectionCutoff,
    ) -> anyhow::Result<()> {
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
                conn.execute_batch("COMMIT")?;
                self.db.cache_invalidate_messages(session_id);
                for event in events {
                    let _ = self.live_tx.send(event);
                }
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Resolve a branch point's projection cutoff without exposing the event
    /// payload or timestamp lookup to Agent recovery code.
    pub fn projection_cutoff_for_step(
        &self,
        session_id: &str,
        step_number: u32,
    ) -> anyhow::Result<Option<ProjectionCutoff>> {
        Ok(self
            .branch_point_for_step(session_id, step_number)?
            .and_then(|(_, _, last_msg_at)| {
                last_msg_at.map(|created_at| ProjectionCutoff {
                    created_at,
                    inclusive: false,
                })
            }))
    }

    /// Append usage domain events and project them into the usage tables in
    /// the same transaction. This is the only live usage write boundary for
    /// Agent-owned, tool-owned and media-owned model calls.
    pub fn append_usage(
        &self,
        session_id: &str,
        input: &LlmCallUsageInput,
    ) -> anyhow::Result<LlmCallUsage> {
        let mut records = self.append_usage_batch(session_id, std::slice::from_ref(input))?;
        records
            .pop()
            .ok_or_else(|| anyhow::anyhow!("usage append returned no record"))
    }

    pub fn append_usage_batch(
        &self,
        session_id: &str,
        inputs: &[LlmCallUsageInput],
    ) -> anyhow::Result<Vec<LlmCallUsage>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let records = inputs
            .iter()
            .map(|input| {
                let step_number = input
                    .step_number
                    .map(|value| {
                        u32::try_from(value)
                            .map_err(|_| anyhow::anyhow!("usage step_number must not be negative"))
                    })
                    .transpose();
                step_number.map(|step_number| {
                    let mut record = LlmCallUsage::from_input(
                        haven_common::types::new_id("usage"),
                        session_id,
                        input,
                        now_rfc3339_millis(),
                    );
                    record.step_number = step_number.map(|value| value as i32);
                    record
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let events = records
            .iter()
            .map(|record| {
                Ok(SessionEventInput {
                    event_type: USAGE_RECORDED_EVENT_TYPE.into(),
                    payload: serde_json::to_string(record)?,
                    run_id: None,
                    step_number: record
                        .step_number
                        .and_then(|value| u32::try_from(value).ok()),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let conn = self.db.conn();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<(Vec<LlmCallUsage>, Vec<SessionEvent>)> {
            let stored_events = Self::append_batch_in_transaction(&conn, session_id, &events)?;
            for record in &records {
                Database::project_llm_call_usage_conn(&conn, record)?;
            }
            Ok((records, stored_events))
        })();
        match result {
            Ok((records, stored_events)) => {
                conn.execute_batch("COMMIT")?;
                for event in stored_events {
                    let _ = self.live_tx.send(event);
                }
                Ok(records)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Append usage records on the blocking pool while keeping cancellation
    /// and SQLite interrupt handling inside the memory boundary.
    pub async fn append_usage_batch_cancellable(
        &self,
        session_id: &str,
        inputs: Vec<LlmCallUsageInput>,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<Vec<LlmCallUsage>> {
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
    pub fn discard_usage(&self, session_id: &str, usage_id: &str) -> anyhow::Result<()> {
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
                conn.execute_batch("COMMIT")?;
                let _ = self.live_tx.send(event.clone());
                Ok(())
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
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
        let session_id = session_id.to_owned();
        let usage_id = usage_id.to_owned();
        let store = self.clone();
        let discard = move |_db: &Database| store.discard_usage(&session_id, &usage_id);
        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, discard).await,
            None => self.db.run_blocking(discard).await,
        }
    }

    pub fn append_branch_point(
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
        self.append(
            session_id,
            BRANCH_POINT_EVENT_TYPE,
            &payload.to_string(),
            run_id,
            Some(step_number),
        )
    }

    /// Append a branch point using the projection clocks observed under the
    /// same SQLite write transaction. ReAct callers receive the clocks as
    /// data; they never read `messages` or derive an event cursor locally.
    pub fn append_branch_point_from_projection(
        &self,
        session_id: &str,
        step_number: u32,
        run_id: Option<u64>,
    ) -> anyhow::Result<(SessionEvent, SessionCursor)> {
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
                conn.execute_batch("COMMIT")?;
                let _ = self.live_tx.send(result.0.clone());
                Ok(result)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    /// Record one phase of the recovery-only persistence protocol. The
    /// marker is not part of transcript replay; it only makes a partially
    /// completed repair durable and auditable.
    pub fn append_recovery_persistence(
        &self,
        session_id: &str,
        run_id: u64,
        step_number: u32,
        phase: &str,
        status: RecoveryPersistenceStatus,
    ) -> anyhow::Result<SessionEvent> {
        let payload = serde_json::json!({
            "phase": phase,
            "branch_point": status.branch_point,
            "partial_messages": status.partial_messages,
            "projection": status.projection,
            "event_boundary": status.event_boundary,
        });
        self.append(
            session_id,
            RECOVERY_PERSISTENCE_EVENT_TYPE,
            &payload.to_string(),
            Some(run_id),
            Some(step_number),
        )
    }

    /// Return the latest recovery protocol marker, including failed markers
    /// outside the active timeline. Resume diagnostics must not lose this
    /// state merely because a later rollback moved the active cursor.
    pub fn latest_recovery_persistence(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        let conn = self.db.conn();
        Self::latest_recovery_persistence_in_connection(&conn, session_id)
    }

    fn latest_recovery_persistence_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Option<SessionEvent>> {
        conn.query_row(
            "SELECT session_id, sequence, event_type, event_version, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND event_type = ?2
             ORDER BY sequence DESC LIMIT 1",
            rusqlite::params![session_id, RECOVERY_PERSISTENCE_EVENT_TYPE],
            |row| {
                Ok(SessionEvent {
                    session_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    event_version: row.get(3)?,
                    payload: row.get(4)?,
                    created_at: row.get(5)?,
                    run_id: row
                        .get::<_, Option<i64>>(6)?
                        .and_then(|value| u64::try_from(value).ok()),
                    step_number: row
                        .get::<_, Option<i64>>(7)?
                        .and_then(|value| u32::try_from(value).ok()),
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn latest_sequence(&self, session_id: &str) -> anyhow::Result<i64> {
        Ok(self.db.conn().query_row(
            "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )?)
    }

    /// Read the latest durable event sequence on SQLite's blocking pool.
    pub async fn latest_sequence_cancellable(
        &self,
        session_id: &str,
        cancel: CancellationToken,
    ) -> anyhow::Result<i64> {
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| store.latest_sequence(&session_id))
            .await
    }

    pub fn read_all(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_from(session_id, 0)
    }

    pub fn read_from(
        &self,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let conn = self.db.conn();
        Self::read_from_in_connection(&conn, session_id, after_sequence)
    }

    /// Read one bounded page of durable events strictly after
    /// `after_sequence`. Rows are ordered by ascending sequence. The page
    /// limit must be between one and [`MAX_SESSION_EVENT_REPLAY_PAGE_SIZE`].
    pub fn replay_page(
        &self,
        session_id: &str,
        after_sequence: i64,
        limit: usize,
    ) -> anyhow::Result<SessionEventPage> {
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
            "SELECT session_id, sequence, event_type, event_version, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![session_id, after_sequence, (limit + 1) as i64],
            |row| {
                Ok(SessionEvent {
                    session_id: row.get(0)?,
                    sequence: row.get(1)?,
                    event_type: row.get(2)?,
                    event_version: row.get(3)?,
                    payload: row.get(4)?,
                    created_at: row.get(5)?,
                    run_id: row
                        .get::<_, Option<i64>>(6)?
                        .and_then(|value| u64::try_from(value).ok()),
                    step_number: row
                        .get::<_, Option<i64>>(7)?
                        .and_then(|value| u32::try_from(value).ok()),
                })
            },
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
        limit: usize,
        cancel: CancellationToken,
    ) -> anyhow::Result<SessionEventPage> {
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel, move |_| {
                store.replay_page(&session_id, after_sequence, limit)
            })
            .await
    }

    fn read_from_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let mut stmt = conn.prepare(
            "SELECT session_id, sequence, event_type, event_version, payload,
                    created_at, run_id, step_number
             FROM session_events
             WHERE session_id = ?1 AND sequence > ?2
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id, after_sequence], |row| {
            Ok(SessionEvent {
                session_id: row.get(0)?,
                sequence: row.get(1)?,
                event_type: row.get(2)?,
                event_version: row.get(3)?,
                payload: row.get(4)?,
                created_at: row.get(5)?,
                run_id: row
                    .get::<_, Option<i64>>(6)?
                    .and_then(|value| u64::try_from(value).ok()),
                step_number: row
                    .get::<_, Option<i64>>(7)?
                    .and_then(|value| u32::try_from(value).ok()),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Explicit replay name used by live/resume consumers. The returned rows
    /// are ordered strictly after `after_sequence`.
    pub fn replay_from(
        &self,
        session_id: &str,
        after_sequence: i64,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        self.read_from(session_id, after_sequence)
    }

    /// Replay the current timeline. Rollback markers only move the active
    /// cursor; the underlying append-only rows remain available for audit and
    /// future branch tooling.
    pub fn read_active(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        let conn = self.db.conn();
        Self::read_active_in_connection(&conn, session_id)
    }

    fn read_active_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let started = Instant::now();
        // A compact_summary is a durable active-root marker.  The audit log
        // before it remains readable through read_all, but normal recovery
        // only needs the root and its suffix.  Fall back to a full replay if
        // an unusual late rollback targets before that root; correctness wins
        // over the optimization for that branch.
        let after_sequence = Self::active_replay_boundary_in_connection(conn, session_id)?;
        let mut active = Vec::new();
        for event in Self::read_from_in_connection(conn, session_id, after_sequence)? {
            anyhow::ensure!(
                event.event_version == CURRENT_EVENT_VERSION,
                "unsupported session event version {} at sequence {}",
                event.event_version,
                event.sequence
            );
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
    pub fn read_active_domain_events(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .read_active(session_id)?
            .into_iter()
            .filter(|event| event.event_type != TRANSCRIPT_EVENT_TYPE)
            .collect())
    }

    /// Read active non-transcript domain events through the session
    /// persistence boundary on SQLite's blocking pool.
    pub async fn read_active_domain_events_async(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<SessionEvent>> {
        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking(move |_| store.read_active_domain_events(&session_id))
            .await
    }

    /// Return the latest active branch point for each step. Branch points are
    /// control events and are intentionally kept separate from transcript
    /// replay, but they share the same rollback cursor and audit log.
    pub fn read_active_branch_points(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<StoredBranchPoint>> {
        let conn = self.db.conn();
        Self::read_active_branch_points_in_connection(&conn, session_id)
    }

    fn read_active_branch_points_in_connection(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> anyhow::Result<Vec<StoredBranchPoint>> {
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
            if let Some(index) = points
                .iter()
                .position(|(_, _, step, _): &StoredBranchPoint| *step == step_number)
            {
                points[index] = (event, event_cursor, step_number, last_msg_at);
            } else {
                points.push((event, event_cursor, step_number, last_msg_at));
            }
        }
        Ok(points)
    }

    pub fn branch_point_for_step(
        &self,
        session_id: &str,
        step_number: u32,
    ) -> anyhow::Result<Option<(SessionEvent, usize, Option<String>)>> {
        Ok(self
            .read_active_branch_points(session_id)?
            .into_iter()
            .find(|(_, _, step, _)| *step == step_number)
            .map(|(event, cursor, _, last_msg_at)| (event, cursor, last_msg_at)))
    }

    /// Return the sequence immediately before the `transcript_cursor`-th
    /// active transcript event. Cursor zero is the beginning of the timeline.
    pub fn sequence_for_transcript_cursor(
        &self,
        session_id: &str,
        transcript_cursor: usize,
    ) -> anyhow::Result<i64> {
        let conn = self.db.conn();
        Self::sequence_for_transcript_cursor_in_connection(&conn, session_id, transcript_cursor)
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

    /// Seed an event stream exactly once when importing a snapshot cache.  A
    /// non-empty stream is never rewritten, even if a stale cache is passed.
    pub fn seed_if_empty(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
        Self::validate_inputs(events)?;
        Self::validate_transcript_events(events)?;
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
                conn.execute_batch("COMMIT")?;
                for event in &stored {
                    let _ = self.live_tx.send(event.clone());
                }
                Ok(stored)
            }
            Ok(None) => {
                conn.execute_batch("COMMIT")?;
                drop(conn);
                self.read_active(session_id)
            }
            Err(error) => {
                let _ = conn.execute_batch("ROLLBACK");
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
        let mut last_created_at = conn
            .query_row(
                "SELECT created_at FROM messages
                 WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                rusqlite::params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        // Keep materialization order stable: chat rows first, then thought
        // steps, then action steps. Branch-point `last_msg_at` and the UI
        // timeline rely on this established projection order.
        for projection in &committed.projections {
            let SessionProjectionIntent::AssistantMessage {
                message_id,
                content,
                message_type,
            } = projection
            else {
                continue;
            };
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let created_at = match last_created_at.as_deref() {
                Some(last) if last >= now.as_str() => bump_message_millis(last),
                _ => now,
            };
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
                    message_type,
                    created_at,
                    ingress_seq,
                ],
            )?;
            last_created_at = Some(created_at.clone());
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
            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            conn.execute(
                "INSERT INTO session_steps
                    (id, session_id, step_number, tool_name, input, thought,
                     status, is_high_risk, created_at)
                 VALUES (?1, ?2, ?3, 'thought', ?1, NULL, 'completed', 0, ?4)",
                rusqlite::params![message_id, session_id, *step_number as i32, created_at],
            )?;
            bump_step_sequence(conn, session_id)?;
        }

        for projection in &committed.projections {
            let SessionProjectionIntent::ActionStep {
                step_id,
                step_number,
                action_index,
                tool_name,
                tool_input,
                tool_call_id,
                is_high_risk,
                silent,
            } = projection
            else {
                continue;
            };
            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let changed = conn.execute(
                "INSERT OR IGNORE INTO session_steps
                    (id, session_id, step_number, action_index, tool_name, input,
                     action_tool, action_input, tool_call_id, status, is_high_risk,
                     created_at, silent, confirmed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, NULL)",
                rusqlite::params![
                    step_id,
                    session_id,
                    *step_number as i32,
                    *action_index as i32,
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

fn bump_message_millis(last: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(last) {
        Ok(timestamp) => (timestamp + chrono::Duration::milliseconds(1))
            .to_rfc3339_opts(SecondsFormat::Millis, true),
        Err(_) => Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::RequestKind;
    use haven_common::types::{CacheAccounting, LlmCallKind};

    fn usage_input(step_number: i32, total_tokens: u32) -> LlmCallUsageInput {
        LlmCallUsageInput {
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

    fn store() -> (Arc<Database>, SessionEventStore, String) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("input").unwrap();
        let store = SessionEventStore::new(db.clone());
        (db, store, session.id)
    }

    fn resume_test_attachment(filename: &str) -> MessageAttachment {
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some(haven_common::types::new_id("asset"));
        attachment.filename = Some(filename.to_owned());
        attachment
    }

    #[tokio::test]
    async fn session_store_cleanup_ports_preserve_session_semantics() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionStore::new(db.clone());
        let running = db.create_session("running").unwrap();
        db.update_session_status(&running.id, SessionStatus::Running)
            .unwrap();

        assert_eq!(store.finalize_orphaned_running_sessions().await.unwrap(), 1);
        assert_eq!(
            db.get_session(&running.id).unwrap().unwrap().status,
            SessionStatus::Error
        );

        let retained = db.create_session("retained before cleanup").unwrap();
        assert_eq!(store.delete_old_sessions(0).await.unwrap(), 2);
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(db.get_session(&retained.id).unwrap().is_none());
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

    fn action_step_write(session_id: &str, step_id: &str) -> ActionStepWrite {
        ActionStepWrite {
            session_id: session_id.into(),
            step_number: 7,
            action_index: 2,
            tool_name: "files.read".into(),
            tool_input: r#"{"path":"notes.txt"}"#.into(),
            tool_call_id: Some("provider-call-7".into()),
            is_high_risk: true,
            silent: false,
            step_id: step_id.into(),
        }
    }

    #[tokio::test]
    async fn session_store_action_step_ports_preserve_identity_confirmation_and_start() {
        let (db, store, session_id) = store();
        let write = action_step_write(&session_id, "step-store-start");

        store
            .ensure_action_step(write.clone(), Some(false))
            .await
            .unwrap();
        store
            .ensure_action_step(write.clone(), Some(true))
            .await
            .unwrap();

        let pending = db.get_session_steps(&session_id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, write.step_id);
        assert_eq!(pending[0].session_id, session_id);
        assert_eq!(pending[0].step_number, 7);
        assert_eq!(pending[0].action_index, 2);
        assert_eq!(pending[0].action_tool.as_deref(), Some("files.read"));
        assert_eq!(
            pending[0].action_input.as_deref(),
            Some(r#"{"path":"notes.txt"}"#)
        );
        assert_eq!(pending[0].tool_call_id.as_deref(), Some("provider-call-7"));
        assert!(pending[0].is_high_risk);
        assert_eq!(pending[0].confirmed, Some(true));
        assert_eq!(pending[0].status, "pending");

        assert!(
            store
                .ensure_and_start_action_step(write.clone(), None)
                .await
                .unwrap()
        );
        assert!(
            store
                .ensure_and_start_action_step(write, None)
                .await
                .unwrap()
        );

        let running = db.get_session_steps(&session_id).unwrap();
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].status, "running");
        assert!(running[0].started_at.is_some());
        assert_eq!(running[0].confirmed, Some(true));
    }

    #[tokio::test]
    async fn session_store_action_step_finish_port_preserves_outcome_and_observation() {
        let (db, store, session_id) = store();
        let write = action_step_write(&session_id, "step-store-finish");

        assert!(
            store
                .ensure_and_finish_action_step(
                    write.clone(),
                    Some(false),
                    "tool may have crossed a side-effect boundary".into(),
                    ActionStepOutcome::Unknown,
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .ensure_and_finish_action_step(
                    write,
                    Some(true),
                    "late completion must not overwrite the terminal row".into(),
                    ActionStepOutcome::Completed,
                )
                .await
                .unwrap()
        );

        let steps = db.get_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].status, "unknown");
        assert_eq!(steps[0].confirmed, Some(false));
        assert_eq!(
            steps[0].observation.as_deref(),
            Some("tool may have crossed a side-effect boundary")
        );
        assert!(steps[0].completed_at.is_some());
    }

    #[test]
    fn session_store_reads_session_records_and_preserves_missing_as_none() {
        let (_db, store, session_id) = store();
        assert_eq!(
            store.session_record(&session_id).unwrap().unwrap().id,
            session_id
        );

        let missing_session_id = haven_common::types::new_id("ses");

        assert!(store.session_record(&missing_session_id).unwrap().is_none());
    }

    #[test]
    fn session_store_pauses_running_sessions_and_returns_updated_count() {
        let (db, store, first_running_id) = store();
        db.update_session_status(&first_running_id, SessionStatus::Running)
            .unwrap();

        let second_running = db.create_session("second running").unwrap();
        db.update_session_status(&second_running.id, SessionStatus::Running)
            .unwrap();
        let already_paused = db.create_session("already paused").unwrap();
        db.update_session_status(&already_paused.id, SessionStatus::Paused)
            .unwrap();
        let still_pending = db.create_session("still pending").unwrap();
        let completed = db.create_session("completed").unwrap();
        db.update_session_status(&completed.id, SessionStatus::Completed)
            .unwrap();

        assert_eq!(store.pause_running_sessions().unwrap(), 2);
        assert_eq!(
            db.get_session(&first_running_id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&second_running.id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&already_paused.id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_session(&still_pending.id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        assert_eq!(
            db.get_session(&completed.id).unwrap().unwrap().status,
            SessionStatus::Completed
        );
        assert_eq!(store.pause_running_sessions().unwrap(), 0);
    }

    #[tokio::test]
    async fn session_store_updates_status_on_blocking_pool() {
        let (db, store, session_id) = store();

        store
            .update_session_status(&session_id, SessionStatus::Running)
            .await
            .unwrap();

        assert_eq!(
            db.get_session(&session_id).unwrap().unwrap().status,
            SessionStatus::Running
        );
    }

    #[tokio::test]
    async fn session_store_deletes_message_by_id_on_blocking_pool() {
        let (db, store, session_id) = store();
        let keep = db
            .add_message(&session_id, "user", "keep", Some("text"), None)
            .unwrap();
        let delete = db
            .add_message(&session_id, "user", "delete", Some("text"), None)
            .unwrap();

        store
            .delete_message_by_id(&session_id, &delete.id)
            .await
            .unwrap();

        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, keep.id);
    }

    #[tokio::test]
    async fn session_store_deletes_session_and_preserves_database_cleanup_and_error() {
        let (db, store, session_id) = store();
        db.add_message(
            &session_id,
            "user",
            "keep only until delete",
            Some("text"),
            None,
        )
        .unwrap();
        db.get_session_messages(&session_id).unwrap();
        let kv_key = format!("fact_extraction_pending.{session_id}");
        db.set_kv(&kv_key, "1").unwrap();

        store.delete_session(&session_id).await.unwrap();

        assert!(db.get_session(&session_id).unwrap().is_none());
        assert!(db.get_session_messages(&session_id).unwrap().is_empty());
        assert!(db.get_kv(&kv_key).unwrap().is_none());
        let error = store.delete_session(&session_id).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("session '{}' not found in database", session_id)
        );
    }

    #[tokio::test]
    async fn session_store_clears_sessions_and_returns_deleted_row_count() {
        let (db, store, first_session_id) = store();
        let second = db.create_session("second input").unwrap();
        db.add_message(
            &first_session_id,
            "user",
            "first message",
            Some("text"),
            None,
        )
        .unwrap();
        db.add_message(&second.id, "user", "second message", Some("text"), None)
            .unwrap();
        let kv_key = format!("fact_extraction_pending.{first_session_id}");
        db.set_kv(&kv_key, "1").unwrap();

        assert_eq!(store.clear_sessions().await.unwrap(), 2);

        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(
            db.get_session_messages(&first_session_id)
                .unwrap()
                .is_empty()
        );
        assert!(db.get_session_messages(&second.id).unwrap().is_empty());
        assert!(db.get_kv(&kv_key).unwrap().is_none());
        assert_eq!(store.clear_sessions().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn session_store_async_session_record_preserves_existing_and_missing_results() {
        let (_db, store, session_id) = store();
        assert_eq!(
            serde_json::to_value(store.load_session_record(&session_id).await.unwrap()).unwrap(),
            serde_json::to_value(store.session_record(&session_id).unwrap()).unwrap()
        );

        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .load_session_record(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_latest_record_preserves_recent_history_order_and_empty_result() {
        let (db, store, first_id) = store();
        let second = db.create_session("second").unwrap();
        let third = db.create_session("third").unwrap();
        let conn = db.conn();
        for (session_id, created_at) in [
            (&first_id, "2026-09-20T10:00:00.000Z"),
            (&second.id, "2026-09-21T10:00:00.000Z"),
            (&third.id, "2026-09-22T10:00:00.000Z"),
        ] {
            conn.execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![created_at, session_id],
            )
            .unwrap();
        }
        drop(conn);

        let latest = store.latest_session_record().await.unwrap();
        assert_eq!(
            latest.as_ref().map(|session| session.id.as_str()),
            Some(third.id.as_str())
        );
        assert_eq!(
            serde_json::to_value(latest).unwrap(),
            serde_json::to_value(db.list_sessions(1, 0).unwrap().into_iter().next()).unwrap()
        );

        let empty_db = Arc::new(Database::open_in_memory().unwrap());
        let empty_store = SessionStore::new(empty_db);
        assert!(empty_store.latest_session_record().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn session_store_reads_only_the_session_title_for_context_assembly() {
        let (db, store, session_id) = store();
        db.update_session_title(&session_id, "Context title")
            .unwrap();

        assert_eq!(
            store.session_title(&session_id).await.unwrap().as_deref(),
            Some("Context title")
        );
        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .session_title(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_loads_title_generation_context_with_original_filter_and_limit() {
        let (db, store, session_id) = store();
        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .title_generation_context(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );

        for index in 0..12 {
            let role = if index % 2 == 0 { "assistant" } else { "user" };
            db.add_message(
                &session_id,
                role,
                &format!("{role}-{index}"),
                Some("text"),
                None,
            )
            .unwrap();
        }

        let context = store
            .title_generation_context(&session_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            context,
            SessionTitleGenerationContext {
                user_messages: vec![
                    "user-3".into(),
                    "user-5".into(),
                    "user-7".into(),
                    "user-9".into(),
                    "user-11".into(),
                ],
            }
        );

        db.update_session_title(&session_id, "Already titled")
            .unwrap();
        assert!(
            store
                .title_generation_context(&session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_reads_display_title_with_input_text_fallback() {
        let (db, store, session_id) = store();

        assert_eq!(
            store
                .session_display_title(&session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("input")
        );

        db.update_session_title(&session_id, "Display title")
            .unwrap();
        assert_eq!(
            store
                .session_display_title(&session_id)
                .await
                .unwrap()
                .as_deref(),
            Some("Display title")
        );

        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .session_display_title(&missing_session_id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn session_store_updates_session_title_and_invalidates_history_cache() {
        let (db, store, session_id) = store();
        assert_eq!(db.list_sessions(50, 0).unwrap()[0].title, None);

        store
            .update_session_title(&session_id, "Updated through SessionStore")
            .await
            .unwrap();

        assert_eq!(
            store.list_history(50, 0).await.unwrap()[0].title.as_deref(),
            Some("Updated through SessionStore")
        );
    }

    #[tokio::test]
    async fn session_store_conversation_window_keeps_latest_limit_in_chronological_order() {
        let (db, store, session_id) = store();
        db.add_message(&session_id, "user", "old", Some("text"), None)
            .unwrap();
        db.add_message(&session_id, "assistant", "middle", Some("text"), None)
            .unwrap();
        db.add_message(&session_id, "user", "latest", Some("text"), None)
            .unwrap();

        let window = store.conversation_window(&session_id, 2).await.unwrap();

        assert_eq!(
            window,
            vec![
                SessionMessageText {
                    role: "assistant".into(),
                    content: "middle".into(),
                },
                SessionMessageText {
                    role: "user".into(),
                    content: "latest".into(),
                },
            ]
        );
        let missing_session_id = haven_common::types::new_id("ses");
        assert!(
            store
                .conversation_window(&missing_session_id, 2)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn session_store_session_resume_media_uses_first_user_and_message_order() {
        let (db, store, session_id) = store();
        let assistant_attachment = resume_test_attachment("assistant.png");
        db.add_message_full(
            &session_id,
            "assistant",
            "before input",
            Some("text"),
            None,
            std::slice::from_ref(&assistant_attachment),
            false,
            None,
        )
        .unwrap();
        let initial_attachment = resume_test_attachment("initial.png");
        let initial_message = db
            .add_message_full(
                &session_id,
                "user",
                "initial input",
                Some("text"),
                None,
                std::slice::from_ref(&initial_attachment),
                false,
                None,
            )
            .unwrap();
        let later_attachment = resume_test_attachment("later.png");
        db.add_message_full(
            &session_id,
            "user",
            "later input",
            Some("text"),
            None,
            std::slice::from_ref(&later_attachment),
            false,
            None,
        )
        .unwrap();

        let resume_media = store.session_resume_media(&session_id).await.unwrap();
        let persisted_messages = db.get_session_messages(&session_id).unwrap();

        assert_eq!(
            resume_media.initial_message_id.as_deref(),
            Some(initial_message.id.as_str())
        );
        assert_eq!(
            resume_media
                .initial_attachments
                .iter()
                .filter_map(|attachment| attachment.filename.as_deref())
                .collect::<Vec<_>>(),
            vec!["initial.png"]
        );
        assert_eq!(
            resume_media.initial_media_inputs,
            persisted_messages[1].media_inputs
        );
        assert_eq!(
            resume_media
                .all_attachments
                .iter()
                .filter_map(|attachment| attachment.filename.as_deref())
                .collect::<Vec<_>>(),
            vec!["assistant.png", "initial.png", "later.png"]
        );
    }

    #[tokio::test]
    async fn session_store_session_resume_media_returns_empty_media_and_isolates_sessions() {
        let (db, store, session_id) = store();
        let initial_message = db
            .add_message(&session_id, "user", "plain input", Some("text"), None)
            .unwrap();
        let other_session = db.create_session("other session").unwrap();
        let other_attachment = resume_test_attachment("other-session.png");
        db.add_message_full(
            &other_session.id,
            "user",
            "other input",
            Some("text"),
            None,
            std::slice::from_ref(&other_attachment),
            false,
            None,
        )
        .unwrap();

        let resume_media = store.session_resume_media(&session_id).await.unwrap();

        assert_eq!(
            resume_media.initial_message_id.as_deref(),
            Some(initial_message.id.as_str())
        );
        assert!(resume_media.initial_attachments.is_empty());
        assert!(resume_media.initial_media_inputs.is_empty());
        assert!(resume_media.all_attachments.is_empty());
    }

    #[tokio::test]
    async fn session_store_session_resume_projection_groups_existing_reads() {
        let (db, store, session_id) = store();
        let message = db
            .add_message(&session_id, "assistant", "resume message", None, None)
            .unwrap();
        let step = db.create_thought_step(&session_id, 1, &message.id).unwrap();
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
            serde_json::to_value(db.get_session_messages(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.steps).unwrap(),
            serde_json::to_value(db.get_session_steps(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.usage).unwrap(),
            serde_json::to_value(db.get_session_usage(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.llm_usage).unwrap(),
            serde_json::to_value(db.get_session_llm_usage(&session_id).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&projection.active_domain_events).unwrap(),
            serde_json::to_value(store.read_active_domain_events(&session_id).unwrap()).unwrap()
        );
        assert_eq!(projection.messages[0].id, message.id);
        assert_eq!(projection.steps[0].id, step.id);
        assert_eq!(projection.llm_usage[0].id, usage.id);
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
    async fn session_store_history_ports_preserve_database_query_semantics() {
        let (db, store, first_id) = store();
        let second = db.create_session("history needle two").unwrap();
        let third = db.create_session("history other three").unwrap();
        db.update_session_title(&second.id, "named needle").unwrap();

        let conn = db.conn();
        for (session_id, created_at) in [
            (&first_id, "2026-09-20T10:00:00.000Z"),
            (&second.id, "2026-09-21T10:00:00.000Z"),
            (&third.id, "2026-09-22T10:00:00.000Z"),
        ] {
            conn.execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![created_at, session_id],
            )
            .unwrap();
        }
        drop(conn);

        let as_json = |sessions: Vec<Session>| serde_json::to_value(sessions).unwrap();

        assert_eq!(
            as_json(store.list_history(2, 1).await.unwrap()),
            as_json(db.list_sessions(2, 1).unwrap())
        );
        assert_eq!(
            store.count_history().await.unwrap(),
            db.count_sessions().unwrap()
        );
        assert_eq!(
            as_json(
                store
                    .search_history_paginated("needle".into(), 1, 1)
                    .await
                    .unwrap()
            ),
            as_json(db.search_sessions_paginated("needle", 1, 1).unwrap())
        );
        assert_eq!(
            store.count_history_search("needle".into()).await.unwrap(),
            db.count_sessions_search("needle").unwrap()
        );
        assert_eq!(
            as_json(store.search_history("needle".into()).await.unwrap()),
            as_json(db.search_sessions("needle").unwrap())
        );

        let filter = SessionHistoryFilter {
            query: Some("needle".into()),
            status: Some("pending".into()),
            start_date: None,
            end_date: None,
            limit: 10,
            offset: 0,
        };
        assert_eq!(
            as_json(store.search_history_filtered(filter.clone()).await.unwrap()),
            as_json(
                db.search_sessions_filtered(
                    filter.query.as_deref(),
                    filter.status.as_deref(),
                    filter.start_date.as_deref(),
                    filter.end_date.as_deref(),
                    filter.limit,
                    filter.offset,
                )
                .unwrap()
            )
        );
    }

    #[tokio::test]
    async fn session_store_creates_session_without_appending_events() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let store = SessionEventStore::new(db.clone());

        let session = store.create_session("created through store").await.unwrap();

        assert!(!session.id.is_empty());
        assert_eq!(session.input_text, "created through store");
        assert_eq!(session.status, SessionStatus::Pending);
        assert!(store.session_record(&session.id).unwrap().is_some());
        assert_eq!(store.latest_sequence(&session.id).unwrap(), 0);
    }

    #[tokio::test]
    async fn session_store_async_domain_event_ports_preserve_append_and_live_replay() {
        let (_db, store, session_id) = store();
        let mut live = store.subscribe();
        let payload = r#"{"id":"step-interaction","status":"pending"}"#;

        let appended = store
            .append_domain_event(&session_id, INTERACTION_REQUESTED_EVENT_TYPE, payload)
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
    }

    #[test]
    fn session_store_lists_all_pending_records_newest_first_and_filters_other_states() {
        let (db, store, oldest_id) = store();
        let middle = db.create_session("middle").unwrap();
        let newest = db.create_session("newest").unwrap();
        let completed = db.create_session("completed").unwrap();
        db.update_session_status(&completed.id, SessionStatus::Completed)
            .unwrap();

        let conn = db.conn();
        for (session_id, created_at) in [
            (&oldest_id, "2026-09-20T10:00:00.000Z"),
            (&middle.id, "2026-09-21T10:00:00.000Z"),
            (&newest.id, "2026-09-22T10:00:00.000Z"),
            (&completed.id, "2026-09-23T10:00:00.000Z"),
        ] {
            conn.execute(
                "UPDATE sessions SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![created_at, session_id],
            )
            .unwrap();
        }
        drop(conn);

        let pending = store.pending_session_records().unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|session| session.id.as_str())
                .collect::<Vec<_>>(),
            [newest.id.as_str(), middle.id.as_str(), oldest_id.as_str()]
        );
        assert!(
            pending
                .iter()
                .all(|session| session.status == SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn session_store_fail_pending_action_steps_scopes_unfinished_steps() {
        let (db, store, session_id) = store();
        let other_session = db.create_session("other").unwrap();
        let pending = db
            .create_action_step(&session_id, 0, "shell", "{}", false, false, None, None)
            .unwrap();
        let running = db
            .create_action_step(&session_id, 1, "shell", "{}", false, false, None, None)
            .unwrap();
        assert!(db.start_action_step(&running.id).unwrap());
        let completed = db
            .create_action_step(&session_id, 2, "shell", "{}", false, false, None, None)
            .unwrap();
        db.complete_action_step(&completed.id, "already finished", true)
            .unwrap();
        let other_pending = db
            .create_action_step(
                &other_session.id,
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
            .get_session_steps(&session_id)
            .unwrap()
            .into_iter()
            .find(|step| step.id == completed.id)
            .unwrap()
            .completed_at;
        assert!(completed_at_before.is_some());
        let other_observation_before = db
            .get_session_steps(&other_session.id)
            .unwrap()
            .into_iter()
            .find(|step| step.id == other_pending.id)
            .unwrap()
            .observation;

        let changed = store
            .fail_pending_action_steps(&session_id, "session failed")
            .await
            .unwrap();

        assert_eq!(changed, 2);
        let target_steps = db.get_session_steps(&session_id).unwrap();
        for step_id in [&pending.id, &running.id] {
            let step = target_steps
                .iter()
                .find(|step| step.id == *step_id)
                .unwrap();
            assert_eq!(step.status, "unknown");
            assert_eq!(step.observation.as_deref(), Some("session failed"));
            assert!(step.completed_at.is_some());
        }
        let completed_after = target_steps
            .iter()
            .find(|step| step.id == completed.id)
            .unwrap();
        assert_eq!(completed_after.status, "completed");
        assert_eq!(
            completed_after.observation.as_deref(),
            Some("already finished")
        );
        assert_eq!(completed_after.completed_at, completed_at_before);

        let untouched = db.get_session_steps(&other_session.id).unwrap();
        let other_pending_after = untouched
            .iter()
            .find(|step| step.id == other_pending.id)
            .unwrap();
        assert_eq!(other_pending_after.status, "pending");
        assert_eq!(other_pending_after.observation, other_observation_before);
    }

    #[tokio::test]
    async fn session_store_persists_message_with_all_fields_without_id() {
        let (db, store, session_id) = store();
        let mut attachment = MessageAttachment::new("image/png", "aGVsbG8=");
        attachment.asset_id = Some("asset-test".into());
        attachment.filename = Some("photo.png".into());
        let attachments = [attachment.clone()];

        let inserted = store
            .persist_session_message(
                &session_id,
                "user",
                "describe this",
                Some("text"),
                &attachments,
                true,
                None,
                Some("call-test"),
                Some(CancellationToken::new()),
            )
            .await
            .unwrap();

        assert!(inserted.id.starts_with("msg-"));
        assert_eq!(inserted.role, "user");
        assert_eq!(inserted.content, "describe this");
        assert_eq!(inserted.message_type.as_deref(), Some("text"));
        assert_eq!(inserted.tool_call_id.as_deref(), Some("call-test"));
        assert_eq!(inserted.attachments, attachments);
        assert!(inserted.voice);

        let persisted = db
            .get_message_by_id(&session_id, &inserted.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            persisted.attachments[0].asset_id.as_deref(),
            Some("asset-test")
        );
        assert_eq!(
            persisted.attachments[0].filename.as_deref(),
            Some("photo.png")
        );
        assert!(persisted.voice);
        assert_eq!(persisted.role, "user");
        assert_eq!(persisted.content, "describe this");
        assert_eq!(persisted.message_type.as_deref(), Some("text"));
        assert_eq!(persisted.tool_call_id.as_deref(), Some("call-test"));
        assert_eq!(persisted.media_inputs.len(), 1);
    }

    #[tokio::test]
    async fn session_store_persist_message_returns_existing_for_same_id_and_content() {
        let (db, store, session_id) = store();
        let first = store
            .persist_session_message(
                &session_id,
                "user",
                "retry me",
                Some("text"),
                &[],
                false,
                Some("msg-retry"),
                None,
                None,
            )
            .await
            .unwrap();

        let retried = store
            .persist_session_message(
                &session_id,
                "user",
                "retry me",
                Some("text"),
                &[],
                false,
                Some("msg-retry"),
                None,
                None,
            )
            .await
            .unwrap();

        assert_eq!(retried.id, first.id);
        assert_eq!(retried.created_at, first.created_at);
        assert_eq!(db.get_session_messages(&session_id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn session_store_persist_message_rejects_idempotency_conflict() {
        let (db, store, session_id) = store();
        store
            .persist_session_message(
                &session_id,
                "user",
                "original",
                None,
                &[],
                false,
                Some("msg-conflict"),
                None,
                None,
            )
            .await
            .unwrap();

        let error = store
            .persist_session_message(
                &session_id,
                "user",
                "changed",
                None,
                &[],
                false,
                Some("msg-conflict"),
                None,
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!(
                "message idempotency conflict for session {} message msg-conflict",
                session_id
            )
        );
        assert_eq!(db.get_session_messages(&session_id).unwrap().len(), 1);
    }

    fn append_recovery_marker(store: &SessionEventStore, session_id: &str, phase: &str) {
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
    async fn session_store_reads_messages_after_ingress_cursor_with_projection() {
        let (db, store, session_id) = store();
        let first = db
            .add_message(&session_id, "user", "first", None, None)
            .unwrap();
        let attachment = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        let second = db
            .add_message_full(
                &session_id,
                "user",
                "second",
                None,
                None,
                std::slice::from_ref(&attachment),
                false,
                None,
            )
            .unwrap();
        let third = db
            .add_message(&session_id, "assistant", "third", None, None)
            .unwrap();

        let messages = store
            .messages_after_ingress_cursor(&session_id, first.ingress_seq)
            .await
            .unwrap();
        assert_eq!(
            messages
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            [second.id.as_str(), third.id.as_str()]
        );
        assert_eq!(messages[0].attachments[0].media_type, "image/png");
        assert_eq!(messages[0].media_inputs.len(), 1);
        assert!(matches!(
            messages[0].media_inputs[0].representations[0].payload,
            haven_common::media::MediaRepresentationPayload::ManagedFileRef { .. }
        ));
    }

    #[tokio::test]
    async fn session_store_reads_recent_unanchored_users_with_existing_window() {
        let (db, store, session_id) = store();
        db.add_message(&session_id, "user", "seed", None, None)
            .unwrap();
        let anchored = db
            .add_message(&session_id, "user", "delivered", None, None)
            .unwrap();
        db.create_thought_step(&session_id, 1, &anchored.id)
            .unwrap();
        let attachment = haven_common::types::MessageAttachment::new("image/png", "aGVsbG8=");
        let pending = db
            .add_message_full(
                &session_id,
                "user",
                "pending",
                None,
                None,
                std::slice::from_ref(&attachment),
                false,
                None,
            )
            .unwrap();
        db.add_message(&session_id, "assistant", "ignored", None, None)
            .unwrap();

        let messages = store
            .recent_unanchored_user_messages(&session_id)
            .await
            .unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, pending.id);
        assert_eq!(messages[0].attachments[0].media_type, "image/png");
        assert_eq!(messages[0].media_inputs.len(), 1);

        let old_session = db.create_session("old input").unwrap();
        let old_seed = db
            .add_message(&old_session.id, "user", "old seed", None, None)
            .unwrap();
        let old_pending = db
            .add_message(&old_session.id, "user", "old pending", None, None)
            .unwrap();
        let old_cutoff = (chrono::Utc::now() - chrono::Duration::days(3))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let seed_cutoff =
            (chrono::Utc::now() - chrono::Duration::days(3) - chrono::Duration::seconds(1))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let conn = db.conn();
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![seed_cutoff, old_seed.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE messages SET created_at = ?1 WHERE id = ?2",
            rusqlite::params![old_cutoff, old_pending.id],
        )
        .unwrap();
        drop(conn);
        assert!(
            store
                .recent_unanchored_user_messages(&old_session.id)
                .await
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
            .add_message(&session_id, "user", "selected", Some("text"), None)
            .unwrap();
        let other_session = db.create_session("other rollback target").unwrap();
        let foreign = db
            .add_message(&other_session.id, "user", "foreign", Some("text"), None)
            .unwrap();

        let loaded = store
            .load_rollback_target_message(&session_id, &target.id)
            .await
            .unwrap();
        assert_eq!(loaded.id, target.id);
        assert_eq!(loaded.session_id, session_id);
        assert_eq!(loaded.role, "user");

        for missing_id in [foreign.id.as_str(), "msg-missing"] {
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
            .add_message(&session_id, "assistant", "kept", None, None)
            .unwrap();
        store
            .append_branch_point(&session_id, 0, 2, Some(&kept.created_at), None)
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let discarded = db
            .add_message(&session_id, "assistant", "discarded", None, None)
            .unwrap();
        append_recovery_marker(&store, &session_id, "committed");

        store
            .truncate_projection_after_latest_committed_recovery_async(&session_id)
            .await
            .unwrap();

        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, kept.id);
        assert!(!messages.iter().any(|message| message.id == discarded.id));
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
        assert_eq!(store.read_from(&session_id, 1).unwrap(), vec![second]);
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

    #[test]
    fn replay_page_is_bounded_ordered_and_returns_next_cursor() {
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
                &other_session.id,
                "usage_recorded",
                r#"{"payload":"other"}"#,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store.subscribe_from(&session_id, 0).unwrap().replay.len(),
            5
        );

        let first = store.replay_page(&session_id, 0, 2).unwrap();
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
            .replay_page(&session_id, first.next_cursor, 2)
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
            .replay_page(&session_id, second.next_cursor, 2)
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

        let empty = store.replay_page(&session_id, 12, 2).unwrap();
        assert!(empty.events.is_empty());
        assert!(!empty.has_more);
        assert_eq!(empty.next_cursor, 12);
        assert_eq!(
            store
                .replay_page(&other_session.id, 0, 2)
                .unwrap()
                .events
                .len(),
            1
        );
        assert!(store.replay_page(&session_id, 0, 0).is_err());
        assert!(
            store
                .replay_page(&session_id, 0, MAX_SESSION_EVENT_REPLAY_PAGE_SIZE + 1)
                .is_err()
        );
        assert!(store.replay_page(&session_id, -1, 2).is_err());
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
            .add_message(&session_id, "assistant", "base", None, None)
            .unwrap();
        let first_step = db
            .create_thought_step(&session_id, 1, "step-first")
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
        db.add_message(&session_id, "assistant", "discarded", None, None)
            .unwrap();
        db.create_thought_step(&session_id, 2, "step-second")
            .unwrap();

        let active_usage_event = store
            .read_active_domain_events(&session_id)
            .unwrap()
            .into_iter()
            .find(|event| event.event_type == USAGE_RECORDED_EVENT_TYPE)
            .unwrap();
        let payload: LlmCallUsage = serde_json::from_str(&active_usage_event.payload).unwrap();
        assert_eq!(payload.id, first_usage.id);

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
        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(db.get_session_steps(&session_id).unwrap().len(), 1);
        let usage = db.get_session_llm_usage(&session_id).unwrap();
        assert_eq!(usage.len(), 1);
        assert_eq!(usage[0].id, first_usage.id);
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
            .add_message(&session_id, "user", "keep", Some("text"), None)
            .unwrap();
        let other_session = db.create_session("other session").unwrap();
        let foreign_message = db
            .add_message(&other_session.id, "user", "foreign", Some("text"), None)
            .unwrap();

        let result = store.rollback_to(
            &session_id,
            &rollback_request(
                1,
                1,
                1,
                RollbackProjectionBoundary::UserMessage {
                    message_id: foreign_message.id,
                },
            ),
            &[],
            None,
        );

        assert!(result.is_err());
        assert_eq!(
            db.get_session_messages(&session_id).unwrap()[0].id,
            message.id
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

        let usage = db.get_session_llm_usage(&session_id).unwrap();
        assert_eq!(
            usage.iter().map(|record| &record.id).collect::<Vec<_>>(),
            [&first.id]
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
            Some(second.id.as_str())
        );
    }

    #[test]
    fn projection_truncate_after_step_uses_active_branch_cutoff_and_publishes_discard() {
        let (db, store, session_id) = store();
        let kept_message = db
            .add_message(&session_id, "assistant", "kept", None, None)
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
        db.add_message(&session_id, "assistant", "discarded", None, None)
            .unwrap();
        db.create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();
        let discarded_usage = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();
        // Prime the message cache so the post-commit invalidation is covered.
        assert_eq!(db.get_session_messages(&session_id).unwrap().len(), 2);
        let mut live = store.subscribe();

        store
            .truncate_projection_after_step(&session_id, 2)
            .unwrap();

        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, kept_message.id);
        let steps = db.get_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, kept_step.id);
        let usage = db.get_session_llm_usage(&session_id).unwrap();
        assert_eq!(
            usage.iter().map(|record| &record.id).collect::<Vec<_>>(),
            [&kept_usage.id]
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
            Some(discarded_usage.id.as_str())
        );
        let published = live.try_recv().unwrap();
        assert_eq!(published, discarded[0]);
        assert!(live.try_recv().is_err());
    }

    #[test]
    fn projection_truncate_after_step_is_noop_without_branch_point_or_cutoff() {
        let (db, store, no_branch_point_session_id) = store();
        let no_cutoff_session_id = db.create_session("no cutoff").unwrap().id;

        for (session_id, branch_point) in [
            (&no_branch_point_session_id, false),
            (&no_cutoff_session_id, true),
        ] {
            db.add_message(session_id, "assistant", "kept", None, None)
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
                .get_session_messages(session_id)
                .unwrap()
                .into_iter()
                .map(|message| message.id)
                .collect::<Vec<_>>();
            let step_ids = db
                .get_session_steps(session_id)
                .unwrap()
                .into_iter()
                .map(|step| step.id)
                .collect::<Vec<_>>();
            let usage_ids = db
                .get_session_llm_usage(session_id)
                .unwrap()
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>();

            store.truncate_projection_after_step(session_id, 1).unwrap();

            assert_eq!(
                db.get_session_messages(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                message_ids
            );
            assert_eq!(
                db.get_session_steps(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|step| step.id)
                    .collect::<Vec<_>>(),
                step_ids
            );
            assert_eq!(
                db.get_session_llm_usage(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|record| record.id)
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
        db.add_message(&session_id, "assistant", "kept", None, None)
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
            .add_message(&session_id, "assistant", "discarded", None, None)
            .unwrap();
        db.create_thought_step(&session_id, 2, &haven_common::types::new_id("step"))
            .unwrap();
        let discarded_usage = store
            .append_usage(&session_id, &usage_input(2, 20))
            .unwrap();
        append_recovery_marker(&store, &session_id, "committed");
        assert_eq!(db.get_session_messages(&session_id).unwrap().len(), 2);
        let mut live = store.subscribe();

        store
            .truncate_projection_after_latest_committed_recovery(&session_id)
            .unwrap();

        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_ne!(messages[0].id, discarded_message.id);
        let steps = db.get_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].id, kept_step.id);
        let usage = db.get_session_llm_usage(&session_id).unwrap();
        assert_eq!(
            usage.iter().map(|record| &record.id).collect::<Vec<_>>(),
            [&kept_usage.id]
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
            discarded_usage.id
        );
        assert_eq!(live.try_recv().unwrap(), discard);
        assert!(live.try_recv().is_err());
    }

    #[test]
    fn latest_failed_marker_outside_active_replay_overrides_earlier_commit() {
        let (db, store, session_id) = store();
        db.add_message(&session_id, "assistant", "kept", None, None)
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
            .add_message(&session_id, "assistant", "must remain", None, None)
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

        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(
            messages
                .iter()
                .any(|message| message.id == discarded_message.id)
        );
        let steps = db.get_session_steps(&session_id).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(steps.iter().any(|step| step.id == kept_step.id));
        assert!(steps.iter().any(|step| step.id == discarded_step.id));
        let usage = db.get_session_llm_usage(&session_id).unwrap();
        assert_eq!(usage.len(), 2);
        assert!(usage.iter().any(|record| record.id == kept_usage.id));
        assert!(usage.iter().any(|record| record.id == discarded_usage.id));
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
        db.add_message(&session_id, "assistant", "kept", None, None)
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
            .add_message(&session_id, "assistant", "must remain", None, None)
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
            db.get_session_messages(&session_id)
                .unwrap()
                .iter()
                .any(|message| message.id == later_message.id)
        );
        assert!(
            db.get_session_steps(&session_id)
                .unwrap()
                .iter()
                .any(|step| step.id == later_step.id)
        );
        assert!(
            db.get_session_llm_usage(&session_id)
                .unwrap()
                .iter()
                .any(|record| record.id == later_usage.id)
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
        let no_branch_point = db.create_session("no branch point").unwrap().id;
        let no_cutoff = db.create_session("no cutoff").unwrap().id;

        for (session_id, has_marker, branch_point_cutoff) in [
            (no_marker.as_str(), false, Some(true)),
            (no_branch_point.as_str(), true, None),
            (no_cutoff.as_str(), true, Some(false)),
        ] {
            db.add_message(session_id, "assistant", "anchor", None, None)
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
            db.add_message(session_id, "assistant", "later", None, None)
                .unwrap();
            db.create_thought_step(session_id, 2, &haven_common::types::new_id("step"))
                .unwrap();
            store.append_usage(session_id, &usage_input(2, 20)).unwrap();
            if has_marker {
                append_recovery_marker(&store, session_id, "committed");
            }
            let message_ids = db
                .get_session_messages(session_id)
                .unwrap()
                .into_iter()
                .map(|message| message.id)
                .collect::<Vec<_>>();
            let step_ids = db
                .get_session_steps(session_id)
                .unwrap()
                .into_iter()
                .map(|step| step.id)
                .collect::<Vec<_>>();
            let usage_ids = db
                .get_session_llm_usage(session_id)
                .unwrap()
                .into_iter()
                .map(|record| record.id)
                .collect::<Vec<_>>();

            store
                .truncate_projection_after_latest_committed_recovery(session_id)
                .unwrap();

            assert_eq!(
                db.get_session_messages(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|message| message.id)
                    .collect::<Vec<_>>(),
                message_ids
            );
            assert_eq!(
                db.get_session_steps(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|step| step.id)
                    .collect::<Vec<_>>(),
                step_ids
            );
            assert_eq!(
                db.get_session_llm_usage(session_id)
                    .unwrap()
                    .into_iter()
                    .map(|record| record.id)
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
            for chunk in events.chunks(MAX_TRANSCRIPT_BATCH_EVENTS) {
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
        assert_eq!(points[0].1, 2);
        assert_eq!(points[0].3.as_deref(), Some("2026-01-01T00:00:01Z"));
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
        let message_id = "step-batch-thought".to_string();
        let action_id = "step-batch-action".to_string();
        let mut committed = SessionCommitted::transcript(
            r#"{"type":"thought","message_id":"step-batch-thought"}"#,
            2,
            3,
        );
        committed.project_assistant_message(message_id.clone(), "thinking", Some("text".into()));
        committed.project_thought_step(message_id.clone(), 3);
        committed.project_action_step(
            action_id.clone(),
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
        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(
            messages.iter().filter(|row| row.id == message_id).count(),
            1
        );
        let steps = db.get_session_steps(&session_id).unwrap();
        assert!(steps.iter().any(|step| step.id == action_id));
    }

    #[tokio::test]
    async fn cancellable_session_commit_port_writes_with_existing_store_semantics() {
        let (db, store, session_id) = store();
        let mut committed = SessionCommitted::transcript(r#"{"type":"port"}"#, 2, 3);
        committed.project_assistant_message("step-port-thought", "port write", Some("text".into()));

        let result = store
            .commit_transcript_cancellable(&session_id, committed, Some(CancellationToken::new()))
            .await
            .unwrap();

        assert_eq!(result.events.len(), 1);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 1);
        assert_eq!(
            db.get_session_messages(&session_id).unwrap()[0].content,
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
            .add_message(&session_id, "user", "keep", Some("text"), None)
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

        let message_id = "step-committed-thought";
        let mut committed = SessionCommitted::transcript(
            r#"{"type":"thought","step_number":1,"text":"keep thought","message_id":"step-committed-thought"}"#,
            6,
            1,
        );
        committed.project_assistant_message(message_id, "keep thought", Some("text".into()));
        let result = store.commit_transcript(&session_id, &committed).unwrap();
        store
            .create_thought_step(&session_id, 1, message_id)
            .await
            .unwrap();

        let messages = db.get_session_messages(&session_id).unwrap();
        let steps = db.get_session_steps(&session_id).unwrap();
        assert!(messages.iter().any(|message| message.id == message_id));
        assert!(steps.iter().any(|step| step.id == message_id));

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
        let messages = db.get_session_messages(&session_id).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, baseline.id);
        assert!(db.get_session_steps(&session_id).unwrap().is_empty());
    }

    #[test]
    fn session_commit_rolls_back_event_when_projection_fails() {
        let (db, store, session_id) = store();
        let mut live = store.subscribe();
        let conn = db.conn();
        conn.execute_batch(
            "CREATE TRIGGER fail_session_projection BEFORE INSERT ON messages
             WHEN NEW.id = 'msg-projection-fail'
             BEGIN SELECT RAISE(ABORT, 'projection failed'); END;",
        )
        .unwrap();
        drop(conn);
        let mut committed = SessionCommitted::transcript(r#"{"type":"rollback"}"#, 1, 1);
        committed.project_assistant_message("msg-projection-fail", "must fail", None);

        let error = store
            .commit_transcript(&session_id, &committed)
            .unwrap_err();

        assert!(error.to_string().contains("projection failed"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
        assert!(db.get_session_messages(&session_id).unwrap().is_empty());
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
        let mut committed = SessionCommitted::transcript(
            r#"{"type":"thought","message_id":"step-event-first"}"#,
            3,
            4,
        );
        committed.project_assistant_message(
            "step-event-first",
            "persist after event",
            Some("text".into()),
        );

        let result = store.commit_transcript(&session_id, &committed).unwrap();

        assert_eq!(result.events[0].sequence, 1);
        assert_eq!(
            db.get_session_messages(&session_id).unwrap()[0].id,
            "step-event-first"
        );
    }

    #[test]
    fn session_commit_budget_includes_serialization_overhead() {
        let (_db, store, session_id) = store();
        let content = "x".repeat(MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES - 80);
        let committed = SessionCommitted::transcript(
            format!(r#"{{"type":"overhead","content":"{content}"}}"#),
            1,
            1,
        );
        assert!(committed.events[0].payload.len() < MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);
        assert!(serde_json::to_vec(&committed).unwrap().len() > MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);

        let error = store
            .commit_transcript(&session_id, &committed)
            .unwrap_err();
        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
    }

    #[test]
    fn generic_transcript_append_cannot_bypass_batch_budget() {
        let (_db, store, session_id) = store();
        let oversized = "x".repeat(MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);
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
        let oversized = "x".repeat(MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);
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
        oversized_projection.project_action_step(
            "step-oversized",
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
        assert!(db.get_session_steps(&session_id).unwrap().is_empty());
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
    fn session_commit_accepts_128_events_and_rejects_129() {
        let (_db, at_limit_store, session_id) = store();
        let at_limit = SessionCommitted {
            events: (0..MAX_TRANSCRIPT_BATCH_EVENTS)
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
            MAX_TRANSCRIPT_BATCH_EVENTS
        );

        let (_db, store, session_id) = store();
        let over_limit = SessionCommitted {
            events: (0..=MAX_TRANSCRIPT_BATCH_EVENTS)
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
    fn subscribe_from_replays_and_keeps_the_live_receiver_open() {
        let (_db, store, session_id) = store();
        let first = store
            .append_transcript(&session_id, r#"{"type":"first"}"#, 1, 1)
            .unwrap();
        let mut subscription = store.subscribe_from(&session_id, 0).unwrap();
        assert_eq!(subscription.replay, vec![first]);

        let second = store
            .append_transcript(&session_id, r#"{"type":"second"}"#, 1, 2)
            .unwrap();
        assert_eq!(subscription.live.try_recv().unwrap(), second);
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
