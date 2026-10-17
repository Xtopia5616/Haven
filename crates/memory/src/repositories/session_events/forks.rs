use super::*;

impl SessionStore {
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
}
