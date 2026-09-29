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
