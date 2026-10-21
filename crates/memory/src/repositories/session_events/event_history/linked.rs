use super::*;
use std::cmp::Ordering;

impl SessionStore {
    /// Search original transcript events from sessions directly linked to the
    /// current session. Links are symmetric; results exclude the current
    /// session and remain scoped to each linked session's active lineage.
    pub async fn search_linked_transcript_history_cancellable(
        &self,
        session_id: &str,
        query: &str,
        before: Option<SessionLinkedTranscriptHistoryCursor>,
        limit: usize,
        cancel: CancellationToken,
    ) -> anyhow::Result<SessionLinkedTranscriptHistorySearch> {
        ensure_session_id(session_id)?;
        let query = query.trim();
        anyhow::ensure!(!query.is_empty(), "history query must not be empty");
        anyhow::ensure!(
            query.len() <= MAX_SESSION_HISTORY_QUERY_BYTES,
            "history query exceeds {MAX_SESSION_HISTORY_QUERY_BYTES} bytes"
        );
        anyhow::ensure!(
            (1..=MAX_SESSION_HISTORY_SEARCH_RESULTS).contains(&limit),
            "history search limit must be between 1 and {MAX_SESSION_HISTORY_SEARCH_RESULTS}"
        );
        if let Some(cursor) = &before {
            ensure_session_id(&cursor.session_id)?;
            anyhow::ensure!(cursor.sequence >= 1, "cursor sequence must be positive");
            anyhow::ensure!(
                !cursor.created_at.is_empty()
                    && cursor.created_at.len() <= MAX_SESSION_LINKED_HISTORY_CURSOR_TIMESTAMP_BYTES,
                "cursor created_at must be between 1 and {MAX_SESSION_LINKED_HISTORY_CURSOR_TIMESTAMP_BYTES} bytes"
            );
            anyhow::ensure!(
                chrono::DateTime::parse_from_rfc3339(&cursor.created_at).is_ok(),
                "cursor created_at must be an RFC3339 timestamp"
            );
        }

        let store = self.clone();
        let session_id = session_id.to_owned();
        let query = query.to_owned();
        self.db
            .run_blocking_cancellable(cancel.clone(), move |_| {
                store.search_linked_transcript_history_sync(
                    &session_id,
                    &query,
                    before.as_ref(),
                    limit,
                    &cancel,
                )
            })
            .await
    }

    /// Read one transcript event from a directly linked session. The link is
    /// checked in the same read transaction as the event lookup.
    pub async fn read_linked_transcript_history_event_cancellable(
        &self,
        session_id: &str,
        related_session_id: &str,
        sequence: i64,
        cancel: CancellationToken,
    ) -> anyhow::Result<Option<SessionTranscriptHistoryRecord>> {
        ensure_session_id(session_id)?;
        ensure_session_id(related_session_id)?;
        anyhow::ensure!(sequence >= 1, "sequence must be positive");
        anyhow::ensure!(
            session_id != related_session_id,
            "session is not linked to itself"
        );

        let store = self.clone();
        let session_id = session_id.to_owned();
        let related_session_id = related_session_id.to_owned();
        self.db
            .run_blocking_cancellable(cancel.clone(), move |_| {
                store.read_linked_transcript_history_event_sync(
                    &session_id,
                    &related_session_id,
                    sequence,
                    &cancel,
                )
            })
            .await
    }

    fn search_linked_transcript_history_sync(
        &self,
        session_id: &str,
        query: &str,
        before: Option<&SessionLinkedTranscriptHistoryCursor>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> anyhow::Result<SessionLinkedTranscriptHistorySearch> {
        ensure_not_cancelled(cancel)?;
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            let linked_session_ids = linked_session_ids_in_connection(&conn, session_id)?;
            if let Some(cursor) = before {
                anyhow::ensure!(
                    linked_session_ids
                        .iter()
                        .any(|linked_id| linked_id == &cursor.session_id),
                    "cursor session_id is no longer directly linked"
                );
            }

            let query_lower = query.to_lowercase();
            let mut matches = Vec::with_capacity(limit);
            let mut matching_count = 0usize;
            let mut scanned_events = 0usize;
            let mut scanned_bytes = 0usize;

            for linked_session_id in &linked_session_ids {
                ensure_not_cancelled(cancel)?;
                let latest_sequence = latest_sequence_in_connection(&conn, linked_session_id)?;
                let remaining_events = MAX_SESSION_HISTORY_SCAN_EVENTS - scanned_events;
                let remaining_bytes = MAX_SESSION_HISTORY_SCAN_BYTES - scanned_bytes;
                let (active_sequences, session_events_scanned, session_bytes_scanned) =
                    active_transcript_sequences_with_limits(
                        &conn,
                        linked_session_id,
                        latest_sequence,
                        remaining_events,
                        remaining_bytes,
                        cancel,
                    )?;
                scanned_events += session_events_scanned;
                scanned_bytes += session_bytes_scanned;

                let mut statement = conn.prepare(
                    "SELECT session_id, sequence, event_type, payload,
                            created_at, run_id, step_number
                     FROM session_events
                     WHERE session_id = ?1 AND event_type = ?2 AND sequence <= ?3
                     ORDER BY sequence ASC",
                )?;
                let mut rows = statement.query(rusqlite::params![
                    linked_session_id,
                    TRANSCRIPT_EVENT_TYPE,
                    latest_sequence
                ])?;
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
                    if before.is_some_and(|cursor| {
                        compare_history_key(
                            &event.created_at,
                            linked_session_id,
                            event.sequence,
                            &cursor.created_at,
                            &cursor.session_id,
                            cursor.sequence,
                        ) != Ordering::Less
                    }) {
                        continue;
                    }

                    matching_count = matching_count.saturating_add(1);
                    let transcript =
                        history_match(&event, query, &visible_payload, redacted_fields);
                    matches.push(SessionLinkedTranscriptHistoryMatch {
                        session_id: linked_session_id.clone(),
                        transcript,
                    });
                    matches.sort_by(compare_linked_matches);
                    if matches.len() > limit {
                        matches.remove(0);
                    }
                }
            }

            matches.sort_by(|left, right| compare_linked_matches(right, left));
            let has_more = matching_count > limit;
            let next_before = has_more
                .then(|| {
                    matches
                        .last()
                        .map(|entry| SessionLinkedTranscriptHistoryCursor {
                            created_at: entry.transcript.created_at.clone(),
                            session_id: entry.session_id.clone(),
                            sequence: entry.transcript.sequence,
                        })
                })
                .flatten();
            Ok(SessionLinkedTranscriptHistorySearch {
                matches,
                linked_sessions_searched: linked_session_ids.len(),
                next_before,
                scanned_events,
                scanned_bytes,
                has_more,
            })
        })();

        finish_read_transaction(&conn, result)
    }

    fn read_linked_transcript_history_event_sync(
        &self,
        session_id: &str,
        related_session_id: &str,
        sequence: i64,
        cancel: &CancellationToken,
    ) -> anyhow::Result<Option<SessionTranscriptHistoryRecord>> {
        ensure_not_cancelled(cancel)?;
        let conn = self.db.try_conn()?;
        conn.execute_batch("BEGIN DEFERRED")?;
        let result = (|| {
            let linked_session_ids = linked_session_ids_in_connection(&conn, session_id)?;
            anyhow::ensure!(
                linked_session_ids
                    .iter()
                    .any(|linked_id| linked_id == related_session_id),
                "session is not directly linked to the current session"
            );
            read_transcript_history_event_in_connection(&conn, related_session_id, sequence, cancel)
        })();
        finish_read_transaction(&conn, result)
    }
}

fn linked_session_ids_in_connection(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> anyhow::Result<Vec<String>> {
    let mut statement = conn.prepare(
        "WITH linked_sessions AS (
             SELECT CASE WHEN session_id = ?1 THEN related_session_id ELSE session_id END AS linked_session_id,
                    created_at
             FROM session_links
             WHERE session_id = ?1 OR related_session_id = ?1
         )
         SELECT linked_sessions.linked_session_id
         FROM linked_sessions
         JOIN sessions ON sessions.id = linked_sessions.linked_session_id
         WHERE linked_sessions.linked_session_id <> ?1
         ORDER BY linked_sessions.created_at DESC, linked_sessions.linked_session_id ASC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(
        rusqlite::params![
            session_id,
            (MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS + 1) as i64
        ],
        |row| row.get::<_, String>(0),
    )?;
    let linked_session_ids = rows.collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        linked_session_ids.len() <= MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS,
        "linked history search supports at most {MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS} direct sessions"
    );
    for linked_session_id in &linked_session_ids {
        ensure_session_id(linked_session_id)?;
    }
    Ok(linked_session_ids)
}

fn compare_linked_matches(
    left: &SessionLinkedTranscriptHistoryMatch,
    right: &SessionLinkedTranscriptHistoryMatch,
) -> Ordering {
    compare_history_key(
        &left.transcript.created_at,
        &left.session_id,
        left.transcript.sequence,
        &right.transcript.created_at,
        &right.session_id,
        right.transcript.sequence,
    )
}

fn compare_history_key(
    left_created_at: &str,
    left_session_id: &str,
    left_sequence: i64,
    right_created_at: &str,
    right_session_id: &str,
    right_sequence: i64,
) -> Ordering {
    compare_timestamps(left_created_at, right_created_at)
        .then_with(|| left_session_id.cmp(right_session_id))
        .then_with(|| left_sequence.cmp(&right_sequence))
}

fn compare_timestamps(left: &str, right: &str) -> Ordering {
    match (
        chrono::DateTime::parse_from_rfc3339(left),
        chrono::DateTime::parse_from_rfc3339(right),
    ) {
        (Ok(left_timestamp), Ok(right_timestamp)) => left_timestamp.cmp(&right_timestamp),
        _ => left.cmp(right),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn linked_history_cursor_orders_timestamp_instants_across_precision_formats() {
        assert_eq!(
            compare_timestamps("2026-10-21T08:00:00+00:00", "2026-10-21T08:00:00.100+00:00"),
            Ordering::Less
        );
    }

    fn transcript(text: &str) -> SessionEventInput {
        SessionEventInput::new(
            TRANSCRIPT_EVENT_TYPE,
            json!({
                "type": "tool_result",
                "canonical_observation": text
            })
            .to_string(),
        )
    }

    async fn append(store: &SessionStore, session_id: &str, text: &str) -> i64 {
        store
            .append_if_session_exists(session_id, transcript(text), CancellationToken::new())
            .await
            .unwrap()
            .unwrap()
            .sequence
    }

    #[tokio::test]
    async fn linked_history_search_reads_direct_symmetric_links_and_pages_with_source_ids() {
        let db = Arc::new(crate::Database::open_in_memory().unwrap());
        let current = db.create_session("current").unwrap();
        let first = db.create_session("first linked").unwrap();
        let second = db.create_session("second linked").unwrap();
        let unrelated = db.create_session("unrelated").unwrap();
        let store = SessionStore::new(db);
        store
            .link_sessions(&current.session_id, &first.session_id)
            .await
            .unwrap();
        store
            .link_sessions(&second.session_id, &current.session_id)
            .await
            .unwrap();
        store
            .link_sessions(&first.session_id, &unrelated.session_id)
            .await
            .unwrap();

        append(&store, &current.session_id, "needle in current").await;
        append(&store, &first.session_id, "needle in first linked session").await;
        append(
            &store,
            &second.session_id,
            "needle in second linked session",
        )
        .await;
        append(
            &store,
            &unrelated.session_id,
            "needle only in a transitive session",
        )
        .await;

        let page = store
            .search_linked_transcript_history_cancellable(
                &current.session_id,
                "needle",
                None,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(page.linked_sessions_searched, 2);
        assert_eq!(page.matches.len(), 1);
        assert!(page.has_more);
        let cursor = page.next_before.clone().unwrap();
        let first_session_id = page.matches[0].session_id.clone();
        assert_ne!(first_session_id, current.session_id);
        assert_ne!(first_session_id, unrelated.session_id);
        assert!(
            page.matches[0]
                .transcript
                .payload_excerpt
                .contains("needle")
        );

        let next_page = store
            .search_linked_transcript_history_cancellable(
                &current.session_id,
                "needle",
                Some(cursor),
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(next_page.matches.len(), 1);
        assert!(!next_page.has_more);
        assert_ne!(first_session_id, next_page.matches[0].session_id);

        let record = store
            .read_linked_transcript_history_event_cancellable(
                &current.session_id,
                &first_session_id,
                page.matches[0].transcript.sequence,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(record.payload.contains("needle in"));
    }

    #[tokio::test]
    async fn linked_history_read_rejects_sessions_without_a_direct_link() {
        let db = Arc::new(crate::Database::open_in_memory().unwrap());
        let current = db.create_session("current").unwrap();
        let unrelated = db.create_session("unrelated").unwrap();
        let store = SessionStore::new(db);

        let error = store
            .read_linked_transcript_history_event_cancellable(
                &current.session_id,
                &unrelated.session_id,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not directly linked"));
    }

    #[tokio::test]
    async fn linked_history_search_excludes_rolled_back_transcript_events() {
        let db = Arc::new(crate::Database::open_in_memory().unwrap());
        let current = db.create_session("current").unwrap();
        let linked = db.create_session("linked").unwrap();
        let store = SessionStore::new(db);
        store
            .link_sessions(&current.session_id, &linked.session_id)
            .await
            .unwrap();
        append(&store, &linked.session_id, "needle on abandoned branch").await;
        store
            .append_if_session_exists(
                &linked.session_id,
                SessionEventInput::new(
                    TIMELINE_ROLLBACK_EVENT_TYPE,
                    json!({ "to_sequence": 0 }).to_string(),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();

        let result = store
            .search_linked_transcript_history_cancellable(
                &current.session_id,
                "needle",
                None,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.matches.is_empty());
        assert!(!result.has_more);
    }

    #[tokio::test]
    async fn linked_history_search_enforces_direct_session_ceiling() {
        let db = Arc::new(crate::Database::open_in_memory().unwrap());
        let current = db.create_session("current").unwrap();
        let store = SessionStore::new(db.clone());
        for _ in 0..=MAX_SESSION_LINKED_HISTORY_SEARCH_SESSIONS {
            let linked = db.create_session("linked").unwrap();
            store
                .link_sessions(&current.session_id, &linked.session_id)
                .await
                .unwrap();
        }

        let error = store
            .search_linked_transcript_history_cancellable(
                &current.session_id,
                "needle",
                None,
                1,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("at most 32 direct sessions"));
    }
}
