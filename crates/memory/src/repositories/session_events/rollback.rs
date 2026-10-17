use super::*;

impl SessionStore {
    /// Append a raw rollback marker for audit/import tooling.
    ///
    /// Live Agent rollback must use [`Self::rollback_to`], which resolves the
    /// active cursor and updates projections in the same transaction. This
    /// narrow primitive remains available for importing an already-materialized
    /// historical marker without pretending that it also repairs projections.
    #[cfg(test)]
    pub(super) fn append_rollback(
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
    pub(super) fn rollback_to(
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
    pub(super) fn truncate_projection_after_step(
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
    pub(super) fn truncate_projection_after_latest_committed_recovery(
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

    pub(super) fn truncate_projection_after_step_in_transaction(
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
    pub(super) fn truncate_projection_after(
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

    pub(super) fn truncate_session_projections_in_transaction(
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
