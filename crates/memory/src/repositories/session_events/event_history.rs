use super::*;
use std::collections::VecDeque;

mod media;

const MAX_SESSION_HISTORY_SCAN_EVENTS: usize = 50_000;
const MAX_SESSION_HISTORY_SCAN_BYTES: usize = 64 * 1024 * 1024;
const MAX_SESSION_HISTORY_EXCERPT_BYTES: usize = 2 * 1024;
const MAX_SESSION_HISTORY_RECORD_BYTES: usize = 32 * 1024;

impl SessionStore {
    /// Search the current session's transcript history. Compaction roots and
    /// private reasoning fields are omitted, while earlier original events
    /// remain searchable unless a rollback marker removed them from the
    /// current lineage. Results are newest-first and the event/byte scans have
    /// fixed ceilings.
    pub async fn search_transcript_history_cancellable(
        &self,
        session_id: &str,
        query: &str,
        before_sequence: Option<i64>,
        limit: usize,
        cancel: CancellationToken,
    ) -> anyhow::Result<SessionTranscriptHistorySearch> {
        ensure_session_id(session_id)?;
        let query = query.trim();
        anyhow::ensure!(!query.is_empty(), "history query must not be empty");
        anyhow::ensure!(
            query.len() <= MAX_SESSION_HISTORY_QUERY_BYTES,
            "history query exceeds {MAX_SESSION_HISTORY_QUERY_BYTES} bytes"
        );
        if let Some(before_sequence) = before_sequence {
            anyhow::ensure!(before_sequence >= 1, "before_sequence must be positive");
        }
        anyhow::ensure!(
            (1..=MAX_SESSION_HISTORY_SEARCH_RESULTS).contains(&limit),
            "history search limit must be between 1 and {MAX_SESSION_HISTORY_SEARCH_RESULTS}"
        );

        let store = self.clone();
        let session_id = session_id.to_owned();
        let query = query.to_owned();
        self.db
            .run_blocking_cancellable(cancel.clone(), move |_| {
                store.search_transcript_history_sync(
                    &session_id,
                    &query,
                    before_sequence,
                    limit,
                    &cancel,
                )
            })
            .await
    }

    /// Read one transcript event by sequence if it remains on the current
    /// lineage. Provider reasoning fields are removed and payload output is
    /// capped to protect the model context budget.
    pub async fn read_transcript_history_event_cancellable(
        &self,
        session_id: &str,
        sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<Option<SessionTranscriptHistoryRecord>> {
        ensure_session_id(session_id)?;
        anyhow::ensure!(sequence >= 1, "sequence must be positive");

        let store = self.clone();
        let session_id = session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel.clone(), move |_| {
                store.read_transcript_history_event_sync(&session_id, sequence, &cancel)
            })
            .await
    }

    fn search_transcript_history_sync(
        &self,
        session_id: &str,
        query: &str,
        before_sequence: Option<i64>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> anyhow::Result<SessionTranscriptHistorySearch> {
        ensure_not_cancelled(cancel)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            let latest_sequence = latest_sequence_in_connection(&conn, session_id)?;
            let (active_sequences, scanned_events, scanned_bytes) =
                active_transcript_sequences(&conn, session_id, latest_sequence, cancel)?;
            let query_lower = query.to_lowercase();
            let mut selected = VecDeque::with_capacity(limit);
            let mut matching_count = 0usize;
            let sql = if before_sequence.is_some() {
                "SELECT session_id, sequence, event_type, payload,
                        created_at, run_id, step_number
                 FROM session_events
                 WHERE session_id = ?1 AND event_type = ?2
                   AND sequence <= ?3 AND sequence < ?4
                 ORDER BY sequence ASC"
            } else {
                "SELECT session_id, sequence, event_type, payload,
                        created_at, run_id, step_number
                 FROM session_events
                 WHERE session_id = ?1 AND event_type = ?2 AND sequence <= ?3
                 ORDER BY sequence ASC"
            };
            {
                let mut statement = conn.prepare(sql)?;
                let mut rows = match before_sequence {
                    Some(before_sequence) => statement.query(rusqlite::params![
                        session_id,
                        TRANSCRIPT_EVENT_TYPE,
                        latest_sequence,
                        before_sequence
                    ])?,
                    None => statement.query(rusqlite::params![
                        session_id,
                        TRANSCRIPT_EVENT_TYPE,
                        latest_sequence
                    ])?,
                };
                while let Some(row) = rows.next()? {
                    ensure_not_cancelled(cancel)?;
                    let event = session_event_from_row(row)?;
                    if active_sequences.binary_search(&event.sequence).is_err()
                        || is_compaction_root(&event.payload)
                        || !is_retrievable_transcript_event(&event.payload)
                    {
                        continue;
                    }
                    let (visible_payload, redacted_fields) =
                        model_visible_history_payload(&event.payload);
                    if !visible_payload.to_lowercase().contains(&query_lower) {
                        continue;
                    }
                    matching_count += 1;
                    selected.push_back(history_match(
                        &event,
                        query,
                        &visible_payload,
                        redacted_fields,
                    ));
                    if selected.len() > limit {
                        selected.pop_front();
                    }
                }
            }
            let mut matches = selected.into_iter().rev().collect::<Vec<_>>();
            if let Some(before_sequence) = before_sequence {
                matches.retain(|entry| entry.sequence < before_sequence);
            }
            Ok(SessionTranscriptHistorySearch {
                has_more: matching_count > limit,
                matches,
                latest_sequence,
                before_sequence,
                scanned_events,
                scanned_bytes,
            })
        })();

        finish_read_transaction(&conn, result)
    }

    fn read_transcript_history_event_sync(
        &self,
        session_id: &str,
        sequence: i64,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Option<SessionTranscriptHistoryRecord>> {
        ensure_not_cancelled(cancel)?;
        let conn = self.db.conn();
        conn.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            let latest_sequence = latest_sequence_in_connection(&conn, session_id)?;
            if sequence > latest_sequence {
                return Ok(None);
            }
            let (active_sequences, _, _) =
                active_transcript_sequences(&conn, session_id, latest_sequence, cancel)?;
            if active_sequences.binary_search(&sequence).is_err() {
                return Ok(None);
            }
            ensure_not_cancelled(cancel)?;
            let event = conn
                .query_row(
                    "SELECT session_id, sequence, event_type, payload,
                            created_at, run_id, step_number
                     FROM session_events
                     WHERE session_id = ?1 AND sequence = ?2 AND event_type = ?3",
                    rusqlite::params![session_id, sequence, TRANSCRIPT_EVENT_TYPE],
                    session_event_from_row,
                )
                .optional()?;
            let Some(event) = event else {
                return Ok(None);
            };
            if is_compaction_root(&event.payload)
                || !is_retrievable_transcript_event(&event.payload)
            {
                return Ok(None);
            }
            let (visible_payload, redacted_fields) = model_visible_history_payload(&event.payload);
            let (payload, payload_truncated) =
                truncate_utf8(&visible_payload, MAX_SESSION_HISTORY_RECORD_BYTES);
            Ok(Some(SessionTranscriptHistoryRecord {
                sequence: event.sequence,
                created_at: event.created_at,
                run_id: event.run_id,
                step_number: event.step_number,
                redacted_fields,
                payload,
                payload_source_bytes: event.payload.len(),
                payload_truncated,
            }))
        })();

        finish_read_transaction(&conn, result)
    }
}

fn latest_sequence_in_connection(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> anyhow::Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(sequence), 0) FROM session_events WHERE session_id = ?1",
        rusqlite::params![session_id],
        |row| row.get(0),
    )?)
}

fn active_transcript_sequences(
    conn: &rusqlite::Connection,
    session_id: &str,
    latest_sequence: i64,
    cancel: &CancellationToken,
) -> anyhow::Result<(Vec<i64>, usize, usize)> {
    active_transcript_sequences_with_limits(
        conn,
        session_id,
        latest_sequence,
        MAX_SESSION_HISTORY_SCAN_EVENTS,
        MAX_SESSION_HISTORY_SCAN_BYTES,
        cancel,
    )
}

fn active_transcript_sequences_with_limits(
    conn: &rusqlite::Connection,
    session_id: &str,
    latest_sequence: i64,
    max_events: usize,
    max_bytes: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<(Vec<i64>, usize, usize)> {
    let mut active = Vec::new();
    let mut scanned_events = 0usize;
    let mut scanned_bytes = 0usize;
    let mut statement = conn.prepare(
        "SELECT sequence, event_type, payload
         FROM session_events
         WHERE session_id = ?1 AND sequence <= ?2
         ORDER BY sequence ASC
         LIMIT ?3",
    )?;
    let mut rows = statement.query(rusqlite::params![
        session_id,
        latest_sequence,
        (max_events.saturating_add(1)) as i64
    ])?;
    while let Some(row) = rows.next()? {
        ensure_not_cancelled(cancel)?;
        scanned_events += 1;
        anyhow::ensure!(
            scanned_events <= max_events,
            "session history exceeds the {max_events}-event search limit"
        );
        let sequence: i64 = row.get(0)?;
        let event_type: String = row.get(1)?;
        let payload: String = row.get(2)?;
        scanned_bytes = scanned_bytes.saturating_add(payload.len());
        anyhow::ensure!(
            scanned_bytes <= max_bytes,
            "session history exceeds the {}-byte search scan limit",
            max_bytes
        );
        if event_type == TIMELINE_ROLLBACK_EVENT_TYPE {
            let target = serde_json::from_str::<serde_json::Value>(&payload)?
                .get("to_sequence")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| anyhow::anyhow!("rollback event has no to_sequence"))?;
            anyhow::ensure!(target >= 0, "rollback event has a negative to_sequence");
            while active
                .last()
                .is_some_and(|active_sequence| *active_sequence > target)
            {
                active.pop();
            }
        } else if event_type == TRANSCRIPT_EVENT_TYPE {
            active.push(sequence);
        }
    }
    Ok((active, scanned_events, scanned_bytes))
}

fn history_match(
    event: &SessionEvent,
    query: &str,
    visible_payload: &str,
    redacted_fields: Vec<String>,
) -> SessionTranscriptHistoryMatch {
    let (payload_excerpt, payload_offset_bytes, payload_truncated) =
        excerpt_around_query(visible_payload, query);
    SessionTranscriptHistoryMatch {
        sequence: event.sequence,
        created_at: event.created_at.clone(),
        run_id: event.run_id,
        step_number: event.step_number,
        redacted_fields,
        payload_excerpt,
        payload_offset_bytes,
        payload_source_bytes: event.payload.len(),
        payload_truncated,
    }
}

fn model_visible_history_payload(payload: &str) -> (String, Vec<String>) {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return (payload.to_owned(), Vec::new());
    };
    let Some(object) = value.as_object_mut() else {
        return (payload.to_owned(), Vec::new());
    };
    let mut redacted_fields = Vec::new();
    for field in ["reasoning", "thinking_blocks"] {
        if object.remove(field).is_some() {
            redacted_fields.push(field.to_owned());
        }
    }
    let visible_payload = serde_json::to_string(&value).unwrap_or_else(|_| payload.to_owned());
    (visible_payload, redacted_fields)
}

fn excerpt_around_query(payload: &str, query: &str) -> (String, usize, bool) {
    if payload.len() <= MAX_SESSION_HISTORY_EXCERPT_BYTES {
        return (payload.to_owned(), 0, false);
    }

    let offset = payload
        .to_ascii_lowercase()
        .find(&query.to_ascii_lowercase())
        .or_else(|| payload.find(query))
        .unwrap_or(0);
    let mut start = offset.saturating_sub(MAX_SESSION_HISTORY_EXCERPT_BYTES / 3);
    while !payload.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = start
        .saturating_add(MAX_SESSION_HISTORY_EXCERPT_BYTES)
        .min(payload.len());
    while !payload.is_char_boundary(end) {
        end -= 1;
    }
    (payload[start..end].to_owned(), start, true)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_owned(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_owned(), true)
}

fn is_compaction_root(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("compact_summary")
}

fn is_retrievable_transcript_event(payload: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|event_type| {
            matches!(
                event_type.as_str(),
                "user_inject" | "tool_call" | "tool_result" | "media_plan"
            )
        })
}

fn ensure_not_cancelled(cancel: &CancellationToken) -> anyhow::Result<()> {
    anyhow::ensure!(!cancel.is_cancelled(), "cancelled");
    Ok(())
}

fn finish_read_transaction<T>(
    conn: &rusqlite::Connection,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn append_event(
        store: &SessionStore,
        session_id: &str,
        event_type: &str,
        payload: serde_json::Value,
    ) -> SessionEvent {
        store
            .append_if_session_exists(
                session_id,
                SessionEventInput::new(event_type, payload.to_string()),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap()
    }

    #[test]
    fn excerpt_keeps_the_match_near_the_center_and_respects_utf8_boundaries() {
        let payload = format!(
            "{}需要查找的内容{}",
            "前文🦀".repeat(700),
            "后文🎯".repeat(700)
        );
        let (excerpt, offset, truncated) = excerpt_around_query(&payload, "需要查找的内容");
        assert!(truncated);
        assert!(offset > 0);
        assert!(excerpt.contains("需要查找的内容"));
        assert!(excerpt.len() <= MAX_SESSION_HISTORY_EXCERPT_BYTES);
        assert!(payload.is_char_boundary(offset));
    }

    #[test]
    fn record_truncation_respects_utf8_boundaries() {
        let value = "🦀".repeat(10);
        let (truncated, did_truncate) = truncate_utf8(&value, 9);
        assert!(did_truncate);
        assert_eq!(truncated, "🦀🦀");
    }

    #[tokio::test]
    async fn history_search_returns_original_active_records_and_excludes_rolled_back_events() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("history").unwrap();
        let store = SessionStore::new(db);
        let session_id = session.session_id;
        let first = append_event(
            &store,
            &session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "user_inject", "text": "needle from user"}),
        )
        .await;
        let rolled_back = append_event(
            &store,
            &session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({
                "type": "tool_result",
                "tool_call": {"name": "shell", "arguments": "needle old branch"},
                "canonical_observation": "needle old result"
            }),
        )
        .await;
        append_event(
            &store,
            &session_id,
            TIMELINE_ROLLBACK_EVENT_TYPE,
            serde_json::json!({"to_sequence": first.sequence}),
        )
        .await;
        let current = append_event(
            &store,
            &session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({
                "type": "tool_call",
                "tool_calls": [{"name": "files.read", "arguments": "needle current branch"}],
                "reasoning": "needle private reasoning",
                "thinking_blocks": [{"text": "needle private reasoning block"}]
            }),
        )
        .await;
        let internal_reasoning = append_event(
            &store,
            &session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "reasoning", "text": "needle private reasoning"}),
        )
        .await;
        append_event(
            &store,
            &session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "compact_summary", "summary": "needle summary"}),
        )
        .await;

        let first_page = store
            .search_transcript_history_cancellable(
                &session_id,
                "needle",
                None,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(first_page.matches.len(), 1);
        assert_eq!(first_page.matches[0].sequence, current.sequence);
        assert!(first_page.matches[0].payload_excerpt.contains("files.read"));
        assert!(
            !first_page.matches[0]
                .payload_excerpt
                .contains("private reasoning")
        );
        assert_eq!(
            first_page.matches[0].redacted_fields,
            vec!["reasoning".to_owned(), "thinking_blocks".to_owned()]
        );
        assert!(first_page.has_more);

        let reasoning_search = store
            .search_transcript_history_cancellable(
                &session_id,
                "private reasoning",
                None,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(reasoning_search.matches.is_empty());

        let second_page = store
            .search_transcript_history_cancellable(
                &session_id,
                "needle",
                Some(current.sequence),
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(second_page.matches.len(), 1);
        assert_eq!(second_page.matches[0].sequence, first.sequence);
        assert!(!second_page.has_more);

        assert!(
            store
                .read_transcript_history_event_cancellable(
                    &session_id,
                    rolled_back.sequence,
                    CancellationToken::new(),
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .read_transcript_history_event_cancellable(
                    &session_id,
                    internal_reasoning.sequence,
                    CancellationToken::new(),
                )
                .await
                .unwrap()
                .is_none()
        );
        let current_record = store
            .read_transcript_history_event_cancellable(
                &session_id,
                current.sequence,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            current_record.redacted_fields,
            vec!["reasoning".to_owned(), "thinking_blocks".to_owned()]
        );
        assert!(!current_record.payload.contains("private reasoning"));
    }

    #[tokio::test]
    async fn history_search_rejects_an_already_cancelled_request() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("history").unwrap();
        let store = SessionStore::new(db);
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = store
            .search_transcript_history_cancellable(&session.session_id, "needle", None, 3, cancel)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn history_scan_enforces_event_and_payload_byte_limits() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let session = db.create_session("history").unwrap();
        let store = SessionStore::new(db);
        append_event(
            &store,
            &session.session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "user_inject", "text": "first"}),
        )
        .await;
        append_event(
            &store,
            &session.session_id,
            TRANSCRIPT_EVENT_TYPE,
            serde_json::json!({"type": "tool_result", "text": "second"}),
        )
        .await;

        let connection = store.db.conn();
        let latest = latest_sequence_in_connection(&connection, &session.session_id).unwrap();
        let event_limit_error = active_transcript_sequences_with_limits(
            &connection,
            &session.session_id,
            latest,
            1,
            MAX_SESSION_HISTORY_SCAN_BYTES,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(
            event_limit_error
                .to_string()
                .contains("1-event search limit")
        );

        let byte_limit_error = active_transcript_sequences_with_limits(
            &connection,
            &session.session_id,
            latest,
            MAX_SESSION_HISTORY_SCAN_EVENTS,
            1,
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert!(
            byte_limit_error
                .to_string()
                .contains("1-byte search scan limit")
        );
    }
}
