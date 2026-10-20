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
        haven_memory::MemoryOutboxStore::new(db.clone()),
        10,
        haven_common::config::ContextLimitsConfig::default(),
    )
}

#[test]
fn pause_memory_trigger_payload_uses_the_event_boundary_reason() {
    for (reason, wire_reason) in [
        (PauseReason::TurnEnd, "turn_end"),
        (PauseReason::Ask, "ask"),
        (PauseReason::Confirm, "confirm"),
        (PauseReason::Budget, "budget"),
        (PauseReason::External, "external"),
    ] {
        assert_eq!(
            pause_memory_trigger_payload(7, 25, reason),
            crate::memory_trigger::MemoryTriggerPayload::pause(7, 25, wire_reason)
        );
    }
}

#[test]
fn only_a_normal_turn_end_has_a_completed_run_kind() {
    assert_eq!(
        run_end_kind_for_pause(PauseReason::TurnEnd),
        Some(haven_common::lifecycle::SessionRunEndKind::TurnCompleted)
    );
    for reason in [
        PauseReason::Ask,
        PauseReason::Confirm,
        PauseReason::Budget,
        PauseReason::External,
    ] {
        assert_eq!(run_end_kind_for_pause(reason), None);
    }
}

#[tokio::test]
async fn event_boundary_cursor_read_succeeds_without_changing_the_event_log() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("event boundary success").unwrap();
    let engine = test_engine(db.clone());
    engine
        .event_store
        .append(&session.session_id, "boundary_test", "{}", Some(1), Some(1))
        .unwrap();
    let events_before = engine.event_store.read_all(&session.session_id).unwrap();
    let state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());

    assert!(
        engine
            .ensure_event_boundary(&session.session_id, &state, 1)
            .await
    );

    assert_eq!(
        engine.event_store.read_all(&session.session_id).unwrap(),
        events_before
    );
    let metrics = engine.metrics.snapshot();
    assert_eq!(metrics.counters.event_boundary_check_failures, 0);
    assert_eq!(metrics.phase(MetricsPhase::EventBoundaryCheck).count, 1);
}

#[tokio::test]
async fn cancelled_run_still_verifies_its_final_event_boundary() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("cancelled event boundary").unwrap();
    let engine = test_engine(db.clone());
    engine
        .event_store
        .append(&session.session_id, "boundary_test", "{}", Some(1), Some(1))
        .unwrap();
    let mut state = ReActState::new(Vec::new(), Vec::new(), HashMap::new());
    let cancellation = tokio_util::sync::CancellationToken::new();
    cancellation.cancel();
    state.turn_cancel = Some(cancellation);

    assert!(matches!(
        engine.exit_cancelled(&session.session_id, &state, 1).await,
        crate::react::LoopExit::Cancelled
    ));

    let metrics = engine.metrics.snapshot();
    assert_eq!(metrics.counters.event_boundary_check_failures, 0);
    assert_eq!(metrics.phase(MetricsPhase::EventBoundaryCheck).count, 1);
    assert_eq!(
        engine
            .event_store
            .read_all(&session.session_id)
            .unwrap()
            .len(),
        1
    );
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

    assert!(
        !engine
            .ensure_event_boundary(&session.session_id, &state, 1)
            .await
    );

    let metrics = engine.metrics.snapshot();
    assert_eq!(metrics.counters.event_boundary_check_failures, 1);
    assert_eq!(metrics.phase(MetricsPhase::EventBoundaryCheck).count, 1);
}
