//! File-backed capacity and SQLITE_FULL probes for the durable session log.

use super::session_events::{
    MAX_TRANSCRIPT_BATCH_EVENTS, SessionCommitted, SessionEventInput, SessionEventStore,
};
use crate::db::Database;
use std::sync::Arc;

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn sqlite_sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    value.into()
}

fn pragma_i64(db: &Database, pragma: &str) -> i64 {
    db.conn()
        .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get(0))
        .unwrap()
}

fn truncate_wal(db: &Database) -> (i64, i64, i64) {
    db.conn()
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
}

#[test]
fn sqlite_full_transcript_commit_rolls_back_and_can_be_retried() {
    let test_root = std::env::temp_dir().join(format!(
        "haven-sqlite-full-{}",
        haven_common::types::new_id("file")
    ));
    std::fs::create_dir_all(&test_root).unwrap();
    let db_path = test_root.join("haven.db");
    let db = Arc::new(Database::open_single_connection_for_test(&db_path).unwrap());
    let session = db.create_session("sqlite-full-test").unwrap();
    let store = SessionEventStore::new(db.clone());
    let mut live = store.subscribe();

    let page_count = pragma_i64(&db, "page_count");
    {
        let conn = db.conn();
        conn.pragma_update(None, "max_page_count", page_count)
            .unwrap();
        assert_eq!(
            conn.query_row("PRAGMA max_page_count", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            page_count
        );
    }

    let oversized_for_test = format!(
        r#"{{"type":"transcript","text":"{}"}}"#,
        "x".repeat(1024 * 1024)
    );
    let message_id = haven_common::types::new_id("msg");
    let mut committed = SessionCommitted::transcript(oversized_for_test, 1, 1);
    committed.project_assistant_message(message_id, "committed", None);
    let commit_error = store
        .commit_transcript(&session.id, &committed)
        .unwrap_err();
    assert!(commit_error.to_string().to_lowercase().contains("full"));
    assert_eq!(
        SessionEventStore::sqlite_storage_write_failure(&commit_error),
        Some(crate::repositories::session_events::SqliteStorageWriteFailure::DiskFull)
    );
    assert!(store.read_all(&session.id).unwrap().is_empty());
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params![session.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(matches!(
        live.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    {
        let conn = db.conn();
        conn.pragma_update(None, "max_page_count", 2_147_483_647_i64)
            .unwrap();
    }
    let recovered = store.commit_transcript(&session.id, &committed).unwrap();
    assert_eq!(recovered.events[0].sequence, 1);
    assert_eq!(store.read_all(&session.id).unwrap(), recovered.events);
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params![session.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(live.try_recv().unwrap().sequence, 1);

    drop(store);
    drop(db);
    std::fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn failed_transcript_commit_rolls_back_and_can_be_retried() {
    let test_root = std::env::temp_dir().join(format!(
        "haven-session-commit-failure-{}",
        haven_common::types::new_id("file")
    ));
    std::fs::create_dir_all(&test_root).unwrap();
    let db_path = test_root.join("haven.db");
    let db = Arc::new(Database::open_single_connection_for_test(&db_path).unwrap());
    let session = db.create_session("commit-failure-test").unwrap();
    let store = SessionEventStore::new(db.clone());
    let mut live = store.subscribe();

    db.conn()
        .execute_batch(
            "CREATE TABLE commit_failure_guard (
                session_id TEXT NOT NULL REFERENCES sessions(id) DEFERRABLE INITIALLY DEFERRED
            );
            CREATE TRIGGER fail_session_event_commit
            AFTER INSERT ON session_events
            BEGIN
                INSERT INTO commit_failure_guard(session_id) VALUES ('missing-session');
            END;",
        )
        .unwrap();

    let message_id = haven_common::types::new_id("msg");
    let mut committed = SessionCommitted::transcript(
        r#"{"type":"transcript","text":"committed only after COMMIT"}"#,
        1,
        1,
    );
    committed.project_assistant_message(message_id, "assistant", None);
    let commit_error = store
        .commit_transcript(&session.id, &committed)
        .unwrap_err();
    assert!(commit_error.to_string().contains("FOREIGN KEY"));
    assert!(db.conn().is_autocommit());
    assert!(store.read_all(&session.id).unwrap().is_empty());
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params![session.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        0
    );
    assert!(matches!(
        live.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));

    db.conn()
        .execute_batch("DROP TRIGGER fail_session_event_commit; DROP TABLE commit_failure_guard;")
        .unwrap();
    let recovered = store.commit_transcript(&session.id, &committed).unwrap();
    assert_eq!(recovered.events[0].sequence, 1);
    assert_eq!(store.read_all(&session.id).unwrap(), recovered.events);
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = ?1",
                rusqlite::params![session.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1
    );
    assert_eq!(live.try_recv().unwrap().sequence, 1);

    drop(store);
    drop(db);
    std::fs::remove_dir_all(test_root).unwrap();
}

#[test]
#[ignore = "manual file-backed capacity profile writes a temporary SQLite fixture"]
fn session_event_disk_capacity_profile() {
    const EVENTS_PER_PROFILE: usize = 1_000;
    const PAYLOAD_SIZES: [usize; 3] = [512, 4 * 1024, 16 * 1024];

    let test_root = std::env::temp_dir().join(format!(
        "haven-session-capacity-{}",
        haven_common::types::new_id("file")
    ));
    std::fs::create_dir_all(&test_root).unwrap();

    for payload_size in PAYLOAD_SIZES {
        let db_path = test_root.join(format!("events-{payload_size}.db"));
        let wal_path = sqlite_sidecar(&db_path, "-wal");
        let db = Arc::new(Database::open(&db_path).unwrap());
        let session = db.create_session("session-event-capacity-profile").unwrap();
        let store = SessionEventStore::new(db.clone());
        truncate_wal(&db);
        let baseline_db_bytes = file_len(&db_path);

        let prefix = r#"{"type":"transcript","text":""#;
        let suffix = r#""}"#;
        let text_len = payload_size - prefix.len() - suffix.len();
        let payload = format!("{prefix}{}{suffix}", "x".repeat(text_len));
        assert_eq!(payload.len(), payload_size);
        let event = SessionEventInput::transcript(payload, 1, 1);
        let events = vec![event; EVENTS_PER_PROFILE];
        let mut wal_peak_bytes = 0;
        for batch in events.chunks(MAX_TRANSCRIPT_BATCH_EVENTS) {
            store.append_batch(&session.id, batch).unwrap();
            wal_peak_bytes = wal_peak_bytes.max(file_len(&wal_path));
        }

        let (checkpoint_busy, checkpointed_frames, wal_frames) = truncate_wal(&db);
        assert_eq!(checkpoint_busy, 0);
        let db_after_append_bytes = file_len(&db_path);
        let wal_after_append_checkpoint_bytes = file_len(&wal_path);
        let page_size = pragma_i64(&db, "page_size");
        let page_count = pragma_i64(&db, "page_count");
        assert_eq!(checkpointed_frames, wal_frames);
        assert_eq!(wal_after_append_checkpoint_bytes, 0);

        db.conn()
            .execute(
                "UPDATE sessions SET created_at = '2000-01-01T00:00:00+00:00' WHERE id = ?1",
                rusqlite::params![session.id],
            )
            .unwrap();
        assert_eq!(db.delete_old_sessions(90).unwrap(), 1);
        let (cleanup_busy, cleanup_checkpointed, cleanup_wal_frames) = truncate_wal(&db);
        assert_eq!(cleanup_busy, 0);
        assert_eq!(cleanup_checkpointed, cleanup_wal_frames);
        let db_after_retention_bytes = file_len(&db_path);
        let freelist_after_retention = pragma_i64(&db, "freelist_count");
        let wal_after_retention_checkpoint_bytes = file_len(&wal_path);

        db.conn().execute_batch("VACUUM").unwrap();
        let (vacuum_busy, vacuum_checkpointed, vacuum_wal_frames) = truncate_wal(&db);
        assert_eq!(vacuum_busy, 0);
        assert_eq!(vacuum_checkpointed, vacuum_wal_frames);
        let db_after_vacuum_bytes = file_len(&db_path);
        let wal_after_vacuum_bytes = file_len(&wal_path);

        println!(
            "STORAGE_PROFILE area=session_events os=windows fixture=file_sqlite payload_bytes={payload_size} events={EVENTS_PER_PROFILE} page_size={page_size} page_count_after_append={page_count} baseline_db_bytes={baseline_db_bytes} db_after_append_bytes={db_after_append_bytes} wal_peak_bytes={wal_peak_bytes} db_after_retention_checkpoint_bytes={db_after_retention_bytes} freelist_after_retention_pages={freelist_after_retention} wal_after_retention_checkpoint_bytes={wal_after_retention_checkpoint_bytes} db_after_vacuum_bytes={db_after_vacuum_bytes} wal_after_vacuum_bytes={wal_after_vacuum_bytes}"
        );
    }

    std::fs::remove_dir_all(test_root).unwrap();
}

#[test]
fn sqlite_storage_failures_keep_full_and_io_errors_distinct() {
    let io_error = anyhow::Error::new(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR_WRITE),
        Some("disk I/O error".into()),
    ));
    assert_eq!(
        SessionEventStore::sqlite_storage_write_failure(&io_error),
        Some(crate::repositories::session_events::SqliteStorageWriteFailure::IoFailure)
    );

    let constraint_error = anyhow::Error::new(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
        Some("foreign key constraint failed".into()),
    ));
    assert_eq!(
        SessionEventStore::sqlite_storage_write_failure(&constraint_error),
        None
    );
}

#[test]
fn age_retention_deletes_whole_sessions_and_cascades_their_events() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let expired = db.create_session("expired-session").unwrap();
    let retained = db.create_session("retained-session").unwrap();
    let store = SessionEventStore::new(db.clone());
    store
        .append_transcript(&expired.id, r#"{"type":"thought","text":"old"}"#, 1, 1)
        .unwrap();
    store
        .append_transcript(&retained.id, r#"{"type":"thought","text":"recent"}"#, 1, 1)
        .unwrap();
    db.conn()
        .execute(
            "UPDATE sessions SET created_at = '2000-01-01T00:00:00+00:00' WHERE id = ?1",
            rusqlite::params![expired.id],
        )
        .unwrap();

    assert_eq!(db.delete_old_sessions(90).unwrap(), 1);
    let event_count = |session_id: &str| {
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM session_events WHERE session_id = ?1",
                rusqlite::params![session_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
    };
    assert_eq!(event_count(&expired.id), 0);
    assert_eq!(event_count(&retained.id), 1);

    drop(store);
    drop(db);
}

/// The event names and field layouts mirror the production TranscriptRecord
/// variants in haven-agent. Text lengths and the mix are synthetic scenarios,
/// not measurements sampled from Haven users.
fn representative_transcript_payload(index: usize, step_number: u32) -> String {
    let step_id = haven_common::types::new_id("step");
    let message_id = haven_common::types::new_id("msg");
    let value = match index % 100 {
        0..=24 => serde_json::json!({
            "type": "thought",
            "step_number": step_number,
            "text": "x".repeat(512),
            "message_id": step_id,
        }),
        25..=39 => serde_json::json!({
            "type": "reasoning",
            "step_number": step_number,
            "text": "x".repeat(2 * 1024),
            "message_id": message_id,
        }),
        40..=54 => serde_json::json!({
            "type": "user_inject",
            "step_number": step_number,
            "source": "steering",
            "text": "x".repeat(1024),
            "media_inputs": [],
            "message_id": message_id,
        }),
        55..=74 => serde_json::json!({
            "type": "tool_call",
            "step_number": step_number,
            "text": "Searching local notes",
            "tool_calls": [{
                "id": format!("call-{step_number}"),
                "name": "files.search",
                "arguments": { "query": "x".repeat(1024), "limit": 20 }
            }],
            "reasoning": "x".repeat(512),
            "web_search_calls": [],
            "thinking_blocks": [],
        }),
        75..=94 => serde_json::json!({
            "type": "tool_result",
            "step_number": step_number,
            "tool_index": 0,
            "step_id": step_id,
            "canonical_observation": "x".repeat(1024),
            "history_observation": "x".repeat(2 * 1024),
            "tool_call_id": format!("call-{step_number}"),
            "action": {
                "tool_name": "files.search",
                "tool_input": { "query": "x".repeat(512), "limit": 20 },
                "is_final": false,
                "tool_call_id": format!("call-{step_number}"),
            },
        }),
        _ => serde_json::json!({
            "type": "compact_summary",
            "compacted": [{ "role": "assistant", "content": ["x".repeat(512)] }],
            "media_inputs": [],
            "summary": "x".repeat(8 * 1024),
            "tokens_before": 32000,
            "tokens_after": 4000,
            "episode_id": message_id,
            "degraded": false,
        }),
    };
    serde_json::to_string(&value).unwrap()
}

#[test]
#[ignore = "manual file-backed mixed-event capacity profile writes a temporary SQLite fixture"]
fn session_event_representative_mixture_capacity_profile() {
    const EVENTS: usize = 1_000;
    let test_root = std::env::temp_dir().join(format!(
        "haven-session-event-mix-{}",
        haven_common::types::new_id("file")
    ));
    std::fs::create_dir_all(&test_root).unwrap();

    let db_path = test_root.join("representative-events.db");
    let wal_path = sqlite_sidecar(&db_path, "-wal");
    let db = Arc::new(Database::open(&db_path).unwrap());
    let session = db
        .create_session("session-event-representative-profile")
        .unwrap();
    let store = SessionEventStore::new(db.clone());
    truncate_wal(&db);
    let baseline_db_bytes = file_len(&db_path);

    let events = (0..EVENTS)
        .map(|index| {
            let step_number = u32::try_from(index + 1).unwrap();
            SessionEventInput::transcript(
                representative_transcript_payload(index, step_number),
                1,
                step_number,
            )
        })
        .collect::<Vec<_>>();
    let mut payload_sizes = events
        .iter()
        .map(|event| event.payload.len())
        .collect::<Vec<_>>();
    payload_sizes.sort_unstable();
    let payload_bytes = payload_sizes.iter().sum::<usize>();
    let min_payload_bytes = payload_sizes[0];
    let median_payload_bytes = (payload_sizes[EVENTS / 2 - 1] + payload_sizes[EVENTS / 2]) / 2;
    let p95_payload_bytes = payload_sizes[(EVENTS * 95 / 100).saturating_sub(1)];
    let max_payload_bytes = payload_sizes[EVENTS - 1];

    let mut wal_peak_bytes = 0;
    for batch in events.chunks(MAX_TRANSCRIPT_BATCH_EVENTS) {
        store.append_batch(&session.id, batch).unwrap();
        wal_peak_bytes = wal_peak_bytes.max(file_len(&wal_path));
    }

    let (checkpoint_busy, checkpointed_frames, wal_frames) = truncate_wal(&db);
    assert_eq!(checkpoint_busy, 0);
    assert_eq!(checkpointed_frames, wal_frames);
    assert_eq!(file_len(&wal_path), 0);
    let db_after_append_bytes = file_len(&db_path);
    let db_growth_bytes = db_after_append_bytes.saturating_sub(baseline_db_bytes);
    let page_size = pragma_i64(&db, "page_size");
    let page_count = pragma_i64(&db, "page_count");

    db.conn()
        .execute(
            "UPDATE sessions SET created_at = '2000-01-01T00:00:00+00:00' WHERE id = ?1",
            rusqlite::params![session.id],
        )
        .unwrap();
    assert_eq!(db.delete_old_sessions(90).unwrap(), 1);
    let (cleanup_busy, cleanup_checkpointed, cleanup_wal_frames) = truncate_wal(&db);
    assert_eq!(cleanup_busy, 0);
    assert_eq!(cleanup_checkpointed, cleanup_wal_frames);
    let db_after_retention_bytes = file_len(&db_path);
    let freelist_after_retention = pragma_i64(&db, "freelist_count");

    println!(
        "STORAGE_PROFILE area=session_events os=windows fixture=file_sqlite scenario=synthetic_production_shape_mix events={EVENTS} mix=thought:25%,reasoning:15%,user_inject:15%,tool_call:20%,tool_result:20%,compact_summary:5% payload_total_bytes={payload_bytes} payload_min_bytes={min_payload_bytes} payload_median_bytes={median_payload_bytes} payload_p95_bytes={p95_payload_bytes} payload_max_bytes={max_payload_bytes} page_size={page_size} page_count_after_append={page_count} baseline_db_bytes={baseline_db_bytes} db_after_append_bytes={db_after_append_bytes} db_growth_bytes={db_growth_bytes} wal_peak_bytes={wal_peak_bytes} db_after_retention_checkpoint_bytes={db_after_retention_bytes} freelist_after_retention_pages={freelist_after_retention}"
    );

    drop(store);
    drop(db);
    std::fs::remove_dir_all(test_root).unwrap();
}
