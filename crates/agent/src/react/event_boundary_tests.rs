use super::*;
use crate::react::ReActState;
use crate::session::SessionSupervisor;
use haven_memory::Database;
use std::collections::HashMap;
use std::sync::Arc;

fn test_engine(db: Arc<Database>) -> ReActEngine {
    let router = Arc::new(haven_llm::LlmRouter::new(
        haven_common::config::RouterConfig::default(),
    ));
    let executor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        Arc::new(haven_tools::ToolsFacade::new()),
        2,
    ));
    ReActEngine::new(
        router,
        crate::react::test_tool_catalog_port(&executor),
        executor,
        haven_memory::MemoryStore::new(db.clone()),
        10,
        haven_common::config::ContextLimitsConfig::default(),
    )
}

#[tokio::test]
async fn event_boundary_cursor_read_succeeds_without_changing_the_event_log() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("event boundary success").unwrap();
    let engine = test_engine(db.clone());
    engine
        .event_store
        .append(&session.id, "boundary_test", "{}", Some(1), Some(1))
        .unwrap();
    let events_before = engine.event_store.read_all(&session.id).unwrap();
    let state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());

    assert!(engine.ensure_event_boundary(&session.id, &state, 1).await);

    assert_eq!(
        engine.event_store.read_all(&session.id).unwrap(),
        events_before
    );
    let metrics = engine.metrics.snapshot();
    assert_eq!(metrics.counters.snapshot_failures, 0);
    assert_eq!(metrics.phase(MetricsPhase::Snapshot).count, 1);
}

#[tokio::test]
async fn event_boundary_cursor_read_failure_returns_false_and_records_failure() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("event boundary failure").unwrap();
    db.conn()
        .execute_batch("DROP TABLE session_events")
        .unwrap();
    let engine = test_engine(db);
    let state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());

    assert!(!engine.ensure_event_boundary(&session.id, &state, 1).await);

    let metrics = engine.metrics.snapshot();
    assert_eq!(metrics.counters.snapshot_failures, 1);
    assert_eq!(metrics.phase(MetricsPhase::Snapshot).count, 1);
}
