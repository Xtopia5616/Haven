use super::*;

/// One transcript event submitted for durable commit.
///
/// Transcript payloads are serialized by Agent because their canonical event
/// model belongs to ReAct. The store fixes the event type and owns sequence
/// allocation, transaction boundaries, and materialized projections.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct SessionCommittedEvent {
    pub(super) payload: String,
    pub(super) run_id: u64,
    pub(super) step_number: u32,
}

impl SessionCommittedEvent {
    pub(super) fn new(payload: impl Into<String>, run_id: u64, step_number: u32) -> Self {
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
pub(super) enum SessionProjectionIntent {
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
    pub(super) events: Vec<SessionCommittedEvent>,
    pub(super) projections: Vec<SessionProjectionIntent>,
    /// Domain control events appended after transcript events in the same
    /// transaction. Used when a transcript result also closes a lifecycle
    /// gate, so recovery cannot observe one without the other.
    pub(super) trailing_events: Vec<SessionEventInput>,
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

    pub(super) fn event_inputs(&self) -> Vec<SessionEventInput> {
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

impl SessionStore {
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

        let conn = self.db.try_conn()?;
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

    pub(super) fn validate_inputs(events: &[SessionEventInput]) -> anyhow::Result<()> {
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

    pub(super) fn validate_single_event(
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

    pub(super) fn validate_committed_event_batch(
        committed: &SessionCommitted,
    ) -> anyhow::Result<()> {
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

    pub(super) fn validate_committed_batch(committed: &SessionCommitted) -> anyhow::Result<()> {
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

    pub(super) fn validate_event_input_refs(
        events: &[SessionEventInputRef<'_>],
    ) -> anyhow::Result<()> {
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

    pub(super) fn validate_projections(
        projections: &[SessionProjectionIntent],
    ) -> anyhow::Result<()> {
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

    pub(super) fn payload_bytes(committed: &SessionCommitted) -> anyhow::Result<usize> {
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

    pub(super) fn append_batch_in_transaction(
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
