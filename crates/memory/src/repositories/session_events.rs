//! Durable, append-only session event storage.
//!
//! `session_events` is the recovery authority for a session.  Producers own
//! the JSON payload and event type; this repository only provides ordering,
//! transactionality and timeline control.  A rollback never deletes history:
//! it appends a `timeline_rollback` marker and readers replay the active
//! timeline from the complete log.

use crate::Database;
use crate::repositories::messages::{Message, now_rfc3339_millis, undelivered_recovery_since};
use crate::repositories::sessions::Session;
use crate::repositories::usage::{LlmCallUsage, LlmCallUsageInput};
use chrono::{SecondsFormat, Utc};
use haven_common::SessionStatus;
use haven_common::types::MessageAttachment;
use rusqlite::OptionalExtension;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

pub const TRANSCRIPT_EVENT_TYPE: &str = "transcript";
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

/// Projection rows written together with a live transcript batch.
///
/// These types intentionally contain only the columns needed by the ReAct
/// transcript boundary.  The memory crate does not depend on Agent types, and
/// the batch writer therefore remains a stable persistence contract rather
/// than a second transcript model.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct TranscriptBatch {
    pub events: Vec<SessionEventInput>,
    pub messages: Vec<TranscriptMessageProjection>,
    pub thought_steps: Vec<TranscriptThoughtStepProjection>,
    pub action_steps: Vec<TranscriptActionStepProjection>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptMessageProjection {
    pub id: String,
    pub role: String,
    pub content: String,
    pub message_type: Option<String>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptThoughtStepProjection {
    pub id: String,
    pub step_number: i32,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TranscriptActionStepProjection {
    pub id: String,
    pub step_number: i32,
    pub action_index: i32,
    pub tool_name: String,
    pub tool_input: String,
    pub tool_call_id: Option<String>,
    pub is_high_risk: bool,
    pub silent: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptBatchResult {
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

    /// Read a persisted session record by id for actor installation.
    ///
    /// A missing record remains `Ok(None)` so lifecycle callers can preserve
    /// their existing not-found behavior.
    pub fn session_record(&self, session_id: &str) -> anyhow::Result<Option<Session>> {
        self.db.get_session(session_id)
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

    /// Append a transcript batch on the blocking pool, with optional
    /// cooperative cancellation for the SQLite operation.
    ///
    /// A missing session row historically produced an empty result for live
    /// Agent transcript writes. Keep that compatibility check inside this
    /// boundary; all event and projection SQL remains owned by
    /// [`Self::append_transcript_batch`].
    pub async fn append_transcript_batch_cancellable(
        &self,
        session_id: &str,
        batch: TranscriptBatch,
        cancel: Option<CancellationToken>,
    ) -> anyhow::Result<TranscriptBatchResult> {
        let store = self.clone();
        let session_id = session_id.to_owned();
        let write = move |db: &Database| {
            if db.get_session(&session_id)?.is_none() {
                return Ok(TranscriptBatchResult::default());
            }
            store.append_transcript_batch(&session_id, &batch)
        };

        match cancel {
            Some(cancel) => self.db.run_blocking_cancellable(cancel, write).await,
            None => self.db.run_blocking(write).await,
        }
    }

    /// Append transcript events and their materialized rows in one SQLite
    /// transaction.  The durable event rows are inserted first; projection
    /// failure rolls the whole batch back.  Live broadcasts happen only after
    /// COMMIT, so subscribers never observe an event that was rolled back.
    pub fn append_transcript_batch(
        &self,
        session_id: &str,
        batch: &TranscriptBatch,
    ) -> anyhow::Result<TranscriptBatchResult> {
        Self::validate_inputs(&batch.events)?;
        anyhow::ensure!(
            batch.events.len() <= MAX_TRANSCRIPT_BATCH_EVENTS,
            "transcript batch exceeds {} events",
            MAX_TRANSCRIPT_BATCH_EVENTS
        );
        Self::validate_transcript_events(&batch.events)?;
        anyhow::ensure!(
            batch.messages.len() + batch.thought_steps.len() + batch.action_steps.len()
                <= MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS,
            "transcript batch exceeds {} projection rows",
            MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS
        );
        let payload_bytes = Self::payload_bytes(batch)?;
        anyhow::ensure!(
            payload_bytes <= MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            "transcript batch exceeds {} payload bytes ({} bytes)",
            MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            payload_bytes
        );
        if batch.events.is_empty() {
            anyhow::ensure!(
                batch.messages.is_empty()
                    && batch.thought_steps.is_empty()
                    && batch.action_steps.is_empty(),
                "transcript projection batch must have an event"
            );
            return Ok(TranscriptBatchResult::default());
        }

        let conn = self.db.conn();
        let lock_wait_started = Instant::now();
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let lock_wait_ms = lock_wait_started.elapsed().as_millis() as u64;
        let result = (|| -> anyhow::Result<TranscriptBatchResult> {
            let events = Self::append_batch_in_transaction(&conn, session_id, &batch.events)?;
            let message_created_at = self
                .db
                .write_transcript_projections(&conn, session_id, batch)?;
            let cursor = Self::cursor_in_connection(&conn, session_id)?;
            Ok(TranscriptBatchResult {
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
        let batch = TranscriptBatch {
            events: events.to_vec(),
            ..TranscriptBatch::default()
        };
        let payload_bytes = Self::payload_bytes(&batch)?;
        anyhow::ensure!(
            payload_bytes <= MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            "transcript batch exceeds {} payload bytes ({} bytes)",
            MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES,
            payload_bytes
        );
        Ok(())
    }

    fn payload_bytes(batch: &TranscriptBatch) -> anyhow::Result<usize> {
        // The guard is defined over the exact JSON batch representation rather
        // than a hand-maintained sum of selected string fields. This accounts
        // for object/array keys, separators, numeric/boolean fields and JSON
        // escaping, which are all part of the variable-width persistence
        // payload at this boundary.
        Ok(serde_json::to_vec(batch)?.len())
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
    fn write_transcript_projections(
        &self,
        conn: &rusqlite::Connection,
        session_id: &str,
        batch: &TranscriptBatch,
    ) -> anyhow::Result<Vec<String>> {
        let mut message_created_at = Vec::with_capacity(batch.messages.len());
        let mut last_created_at = conn
            .query_row(
                "SELECT created_at FROM messages
                 WHERE session_id = ?1 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                rusqlite::params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        for message in &batch.messages {
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
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, 0, ?8, NULL)",
                rusqlite::params![
                    message.id,
                    session_id,
                    message.role,
                    message.content,
                    message.message_type,
                    created_at,
                    message.tool_call_id,
                    ingress_seq,
                ],
            )?;
            last_created_at = Some(created_at.clone());
            message_created_at.push(created_at);
        }

        for step in &batch.thought_steps {
            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            conn.execute(
                "INSERT INTO session_steps
                    (id, session_id, step_number, tool_name, input, thought,
                     status, is_high_risk, created_at)
                 VALUES (?1, ?2, ?3, 'thought', ?1, NULL, 'completed', 0, ?4)",
                rusqlite::params![step.id, session_id, step.step_number, created_at],
            )?;
            bump_step_sequence(conn, session_id)?;
        }

        for step in &batch.action_steps {
            let created_at = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
            let changed = conn.execute(
                "INSERT OR IGNORE INTO session_steps
                    (id, session_id, step_number, action_index, tool_name, input,
                     action_tool, action_input, tool_call_id, status, is_high_risk,
                     created_at, silent, confirmed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?6, ?7, 'pending', ?8, ?9, ?10, NULL)",
                rusqlite::params![
                    step.id,
                    session_id,
                    step.step_number,
                    step.action_index,
                    step.tool_name,
                    step.tool_input,
                    step.tool_call_id,
                    step.is_high_risk as i32,
                    created_at,
                    step.silent as i32,
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

    fn usage_input(step_number: i32, total_tokens: u32) -> LlmCallUsageInput {
        LlmCallUsageInput {
            step_number: Some(step_number),
            request_kind: RequestKind::Chat,
            call_kind: "agent".into(),
            model: Some("test-model".into()),
            prompt_tokens: total_tokens,
            completion_tokens: 0,
            total_tokens,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            cache_miss_tokens: 0,
            cache_accounting: "unknown".into(),
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
    /// active-log reads on the same database so the numbers expose the
    /// compaction boundary rather than setup or filesystem noise.
    #[test]
    fn active_replay_boundary_benchmark_1k_10k_100k() {
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
            let full_started = Instant::now();
            let full = store.read_all(&session_id).unwrap();
            let full_elapsed_ms = full_started.elapsed().as_millis();
            let active_started = Instant::now();
            let active = store.read_active(&session_id).unwrap();
            let active_elapsed_ms = active_started.elapsed().as_millis();
            tracing::info!(
                count,
                full_elapsed_ms,
                active_elapsed_ms,
                speedup = if active_elapsed_ms == 0 {
                    0.0
                } else {
                    full_elapsed_ms as f64 / active_elapsed_ms as f64
                },
                full = full.len(),
                active = active.len(),
                "replay benchmark baseline"
            );
            assert_eq!(full.len(), count + 2);
            assert_eq!(active.len(), 2);
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
    fn transcript_batch_commits_events_and_projections_together() {
        let (db, store, session_id) = store();
        let mut receiver = store.subscribe();
        let message_id = "step-batch-thought".to_string();
        let action_id = "step-batch-action".to_string();
        let result = store
            .append_transcript_batch(
                &session_id,
                &TranscriptBatch {
                    events: vec![SessionEventInput::transcript(
                        r#"{"type":"thought","message_id":"step-batch-thought"}"#,
                        2,
                        3,
                    )],
                    messages: vec![TranscriptMessageProjection {
                        id: message_id.clone(),
                        role: "assistant".into(),
                        content: "thinking".into(),
                        message_type: Some("text".into()),
                        tool_call_id: None,
                    }],
                    thought_steps: vec![TranscriptThoughtStepProjection {
                        id: message_id.clone(),
                        step_number: 3,
                    }],
                    action_steps: vec![TranscriptActionStepProjection {
                        id: action_id.clone(),
                        step_number: 3,
                        action_index: 0,
                        tool_name: "echo".into(),
                        tool_input: "{}".into(),
                        tool_call_id: Some("call-batch".into()),
                        is_high_risk: false,
                        silent: false,
                    }],
                },
            )
            .unwrap();
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
    async fn cancellable_transcript_batch_port_writes_with_existing_store_semantics() {
        let (db, store, session_id) = store();
        let batch = TranscriptBatch {
            events: vec![SessionEventInput::transcript(r#"{"type":"port"}"#, 2, 3)],
            messages: vec![TranscriptMessageProjection {
                id: "step-port-thought".into(),
                role: "assistant".into(),
                content: "port write".into(),
                message_type: Some("text".into()),
                tool_call_id: None,
            }],
            ..TranscriptBatch::default()
        };

        let result = store
            .append_transcript_batch_cancellable(&session_id, batch, Some(CancellationToken::new()))
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
    async fn cancellable_transcript_batch_port_preserves_empty_batch_result() {
        let (_db, store, session_id) = store();

        let result = store
            .append_transcript_batch_cancellable(&session_id, TranscriptBatch::default(), None)
            .await
            .unwrap();

        assert!(result.events.is_empty());
        assert!(result.message_created_at.is_empty());
        assert_eq!(result.lock_wait_ms, 0);
        assert_eq!(result.cursor, SessionCursor::default());
    }

    #[tokio::test]
    async fn cancellable_transcript_batch_port_returns_empty_for_missing_session() {
        let (_db, store, _session_id) = store();
        let missing_session_id = haven_common::types::new_id("ses");
        let batch = TranscriptBatch {
            events: vec![SessionEventInput::transcript(
                r#"{"type":"missing-session"}"#,
                1,
                1,
            )],
            ..TranscriptBatch::default()
        };

        let result = store
            .append_transcript_batch_cancellable(
                &missing_session_id,
                batch,
                Some(CancellationToken::new()),
            )
            .await
            .unwrap();

        assert!(result.events.is_empty());
        assert!(result.message_created_at.is_empty());
        assert_eq!(result.cursor, SessionCursor::default());
        assert!(store.read_all(&missing_session_id).unwrap().is_empty());
    }

    #[test]
    fn transcript_batch_rolls_back_event_when_projection_fails() {
        let (db, store, session_id) = store();
        let error = store
            .append_transcript_batch(
                &session_id,
                &TranscriptBatch {
                    events: vec![SessionEventInput::transcript(
                        r#"{"type":"rollback"}"#,
                        1,
                        1,
                    )],
                    messages: vec![TranscriptMessageProjection {
                        id: "not-a-valid-role-row".into(),
                        role: "invalid".into(),
                        content: "must fail".into(),
                        message_type: None,
                        tool_call_id: None,
                    }],
                    ..TranscriptBatch::default()
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("CHECK") || error.to_string().contains("constraint"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
        assert!(db.get_session_messages(&session_id).unwrap().is_empty());
    }

    #[test]
    fn transcript_batch_budget_includes_serialization_overhead() {
        let (_db, store, session_id) = store();
        let content = "x".repeat(MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES - 128);
        let batch = TranscriptBatch {
            events: vec![SessionEventInput::transcript(
                format!(r#"{{"type":"overhead","content":"{content}"}}"#),
                1,
                1,
            )],
            ..TranscriptBatch::default()
        };
        let string_fields = batch.events[0].event_type.len() + batch.events[0].payload.len();
        assert!(string_fields < MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);
        assert!(serde_json::to_vec(&batch).unwrap().len() > MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);

        let error = store
            .append_transcript_batch(&session_id, &batch)
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
    fn transcript_batch_rejects_oversized_event_and_projection_payloads() {
        let (db, store, session_id) = store();
        let oversized = "x".repeat(MAX_TRANSCRIPT_BATCH_PAYLOAD_BYTES);
        let error = store
            .append_transcript_batch(
                &session_id,
                &TranscriptBatch {
                    events: vec![SessionEventInput::transcript(
                        format!(r#"{{"type":"oversized","content":"{oversized}"}}"#),
                        1,
                        1,
                    )],
                    ..TranscriptBatch::default()
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());

        let error = store
            .append_transcript_batch(
                &session_id,
                &TranscriptBatch {
                    events: vec![SessionEventInput::transcript(r#"{"type":"action"}"#, 1, 1)],
                    action_steps: vec![TranscriptActionStepProjection {
                        id: "step-oversized".into(),
                        step_number: 1,
                        action_index: 0,
                        tool_name: "tool".into(),
                        tool_input: oversized,
                        tool_call_id: None,
                        is_high_risk: false,
                        silent: false,
                    }],
                    ..TranscriptBatch::default()
                },
            )
            .unwrap_err();
        assert!(error.to_string().contains("payload bytes"));
        assert!(store.read_all(&session_id).unwrap().is_empty());
        assert!(db.get_session_steps(&session_id).unwrap().is_empty());
    }

    #[test]
    fn transcript_batch_handles_a_64_event_burst_in_order() {
        let (_db, store, session_id) = store();
        let batch = TranscriptBatch {
            events: (0..64)
                .map(|index| {
                    SessionEventInput::transcript(
                        format!(r#"{{"type":"burst","index":{index}}}"#),
                        9,
                        index,
                    )
                })
                .collect(),
            ..TranscriptBatch::default()
        };

        let result = store
            .append_transcript_batch(&session_id, &batch)
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
    fn transcript_batch_accepts_128_events_and_rejects_129() {
        let (_db, at_limit_store, session_id) = store();
        let at_limit = TranscriptBatch {
            events: (0..MAX_TRANSCRIPT_BATCH_EVENTS)
                .map(|index| {
                    SessionEventInput::transcript(
                        format!(r#"{{"type":"boundary","index":{index}}}"#),
                        1,
                        index as u32,
                    )
                })
                .collect(),
            ..TranscriptBatch::default()
        };
        assert_eq!(
            at_limit_store
                .append_transcript_batch(&session_id, &at_limit)
                .unwrap()
                .events
                .len(),
            MAX_TRANSCRIPT_BATCH_EVENTS
        );

        let (_db, store, session_id) = store();
        let over_limit = TranscriptBatch {
            events: (0..=MAX_TRANSCRIPT_BATCH_EVENTS)
                .map(|index| {
                    SessionEventInput::transcript(
                        format!(r#"{{"type":"boundary","index":{index}}}"#),
                        1,
                        index as u32,
                    )
                })
                .collect(),
            ..TranscriptBatch::default()
        };
        let error = store
            .append_transcript_batch(&session_id, &over_limit)
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
