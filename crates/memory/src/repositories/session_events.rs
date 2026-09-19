//! Durable, append-only session event storage.
//!
//! `session_events` is the recovery authority for a session.  Producers own
//! the JSON payload and event type; this repository only provides ordering,
//! transactionality and timeline control.  A rollback never deletes history:
//! it appends a `timeline_rollback` marker and readers replay the active
//! timeline from the complete log.

use crate::Database;
use chrono::{SecondsFormat, Utc};
use rusqlite::OptionalExtension;
use std::sync::Arc;

pub const TRANSCRIPT_EVENT_TYPE: &str = "transcript";
pub const BRANCH_POINT_EVENT_TYPE: &str = "branch_point";
pub const TIMELINE_ROLLBACK_EVENT_TYPE: &str = "timeline_rollback";
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEventInput {
    pub event_type: String,
    pub payload: String,
    pub run_id: Option<u64>,
    pub step_number: Option<u32>,
}

/// Projection rows written together with a live transcript batch.
///
/// These types intentionally contain only the columns needed by the ReAct
/// transcript boundary.  The memory crate does not depend on Agent types, and
/// the batch writer therefore remains a stable persistence contract rather
/// than a second transcript model.
#[derive(Debug, Clone, Default)]
pub struct TranscriptBatch {
    pub events: Vec<SessionEventInput>,
    pub messages: Vec<TranscriptMessageProjection>,
    pub thought_steps: Vec<TranscriptThoughtStepProjection>,
    pub action_steps: Vec<TranscriptActionStepProjection>,
}

#[derive(Debug, Clone)]
pub struct TranscriptMessageProjection {
    pub id: String,
    pub role: String,
    pub content: String,
    pub message_type: Option<String>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TranscriptThoughtStepProjection {
    pub id: String,
    pub step_number: i32,
}

#[derive(Debug, Clone)]
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
}

/// Stage results carried by a recovery-persistence control event. Keeping the
/// status as a named value prevents the event API from becoming a positional
/// boolean list as the protocol evolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryPersistenceStatus {
    pub branch_point: bool,
    pub partial_messages: bool,
    pub projection: bool,
    pub recovery_snapshot: bool,
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
pub struct SessionEventStore {
    db: Arc<Database>,
    live_tx: tokio::sync::broadcast::Sender<SessionEvent>,
}

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

impl SessionEventStore {
    pub fn new(db: Arc<Database>) -> Self {
        let (live_tx, _) = tokio::sync::broadcast::channel(256);
        Self { db, live_tx }
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
        anyhow::ensure!(
            batch.messages.len() + batch.thought_steps.len() + batch.action_steps.len()
                <= MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS,
            "transcript batch exceeds {} projection rows",
            MAX_TRANSCRIPT_BATCH_PROJECTION_ROWS
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
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> anyhow::Result<TranscriptBatchResult> {
            let events = Self::append_batch_in_transaction(&conn, session_id, &batch.events)?;
            let message_created_at = self
                .db
                .write_transcript_projections(&conn, session_id, batch)?;
            Ok(TranscriptBatchResult {
                events,
                message_created_at,
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
            "recovery_snapshot": status.recovery_snapshot,
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
        Ok(self
            .read_all(session_id)?
            .into_iter()
            .rev()
            .find(|event| event.event_type == RECOVERY_PERSISTENCE_EVENT_TYPE))
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
        let mut active = Vec::new();
        for event in self.read_all(session_id)? {
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
        Ok(active)
    }

    pub fn read_active_transcript(&self, session_id: &str) -> anyhow::Result<Vec<SessionEvent>> {
        Ok(self
            .read_active(session_id)?
            .into_iter()
            .filter(|event| event.event_type == TRANSCRIPT_EVENT_TYPE)
            .collect())
    }

    /// Return the latest active branch point for each step. Branch points are
    /// control events and are intentionally kept separate from transcript
    /// replay, but they share the same rollback cursor and audit log.
    pub fn read_active_branch_points(
        &self,
        session_id: &str,
    ) -> anyhow::Result<Vec<StoredBranchPoint>> {
        let mut points = Vec::new();
        for event in self.read_active(session_id)? {
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
        let events = self.read_active_transcript(session_id)?;
        Ok(transcript_cursor
            .checked_sub(1)
            .and_then(|index| events.get(index).map(|event| event.sequence))
            .unwrap_or(0))
    }

    /// Seed an event stream exactly once when importing a snapshot cache.  A
    /// non-empty stream is never rewritten, even if a stale cache is passed.
    pub fn seed_if_empty(
        &self,
        session_id: &str,
        events: &[SessionEventInput],
    ) -> anyhow::Result<Vec<SessionEvent>> {
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
                     tool_call_id, attachments, voice, ingress_seq, media_inputs)
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

    fn store() -> (Arc<Database>, SessionEventStore, String) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("input", "input").unwrap();
        let store = SessionEventStore::new(db.clone());
        (db, store, session.id)
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
        store.append_rollback(&session_id, 1, 2, Some(2)).unwrap();
        store
            .append_transcript(&session_id, r#"{"type":"replacement"}"#, 2, 2)
            .unwrap();

        let active = store.read_active_transcript(&session_id).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].sequence, 1);
        assert_eq!(active[1].payload, r#"{"type":"replacement"}"#);
        assert_eq!(store.read_all(&session_id).unwrap().len(), 4);
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
                    recovery_snapshot: false,
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
