use super::*;
use haven_memory::Database;

fn test_worker(db: Arc<Database>) -> Arc<crate::MemoryWorker> {
    let router = Arc::new(LlmRouter::new(haven_common::config::RouterConfig::default()));
    let worker = Arc::new(crate::MemoryWorker::new(db, router, 4_000, 64, 64, 256, 0));
    worker.suspend_outbox_worker_for_test();
    worker
}

fn test_engine(db: Arc<Database>, worker: Arc<crate::MemoryWorker>) -> ReActEngine {
    let router = Arc::new(LlmRouter::new(haven_common::config::RouterConfig::default()));
    let executor = Arc::new(SessionSupervisor::new(
        db.clone(),
        Arc::new(haven_tools::ToolsManager::new()),
        2,
    ));
    ReActEngine::new(
        router,
        test_tool_catalog_port(&executor),
        executor,
        MemoryStore::new(db),
        10,
        ContextLimitsConfig::default(),
    )
    .with_memory_worker(worker)
}

#[tokio::test]
async fn compaction_summary_persists_trimmed_episode_marker_then_wakes_worker() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("compaction summary").unwrap();
    let worker = test_worker(db.clone());
    let engine = test_engine(db.clone(), worker.clone());
    let episode_id = haven_common::types::new_id("msg");
    let summary = "  A durable compaction summary that exceeds the extraction threshold.  ";

    engine
        .persist_compaction_summary(&session.id, summary, &episode_id)
        .await;

    assert_eq!(
        db.episode_text(&episode_id).unwrap().as_deref(),
        Some(summary.trim())
    );
    assert_eq!(
        db.pending_summary_extractions().unwrap(),
        vec![(session.id.clone(), episode_id.clone())]
    );
    assert_eq!(
        worker.pending_summary_outbox_value_for_test(&episode_id),
        Some(session.id)
    );
}

#[tokio::test]
async fn compaction_summary_below_threshold_persists_without_marker_and_empty_summary_is_skipped() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("compaction threshold").unwrap();
    let worker = test_worker(db.clone());
    let engine = test_engine(db.clone(), worker.clone());
    let short_episode_id = haven_common::types::new_id("msg");
    let threshold_episode_id = haven_common::types::new_id("msg");
    let empty_episode_id = haven_common::types::new_id("msg");
    let below_threshold = "x".repeat(23);
    let at_threshold = "y".repeat(24);

    engine
        .persist_compaction_summary(&session.id, &below_threshold, &short_episode_id)
        .await;
    engine
        .persist_compaction_summary(&session.id, &at_threshold, &threshold_episode_id)
        .await;
    engine
        .persist_compaction_summary(&session.id, "  \n\t  ", &empty_episode_id)
        .await;

    assert_eq!(
        db.episode_text(&short_episode_id).unwrap().as_deref(),
        Some(below_threshold.as_str())
    );
    assert_eq!(
        db.episode_text(&threshold_episode_id).unwrap().as_deref(),
        Some(at_threshold.as_str())
    );
    assert!(db.episode_text(&empty_episode_id).unwrap().is_none());
    assert_eq!(
        db.pending_summary_extractions().unwrap(),
        vec![(session.id.clone(), threshold_episode_id.clone())]
    );
    assert!(
        worker
            .pending_summary_outbox_value_for_test(&short_episode_id)
            .is_none()
    );
    assert!(
        worker
            .pending_summary_outbox_value_for_test(&empty_episode_id)
            .is_none()
    );
    assert_eq!(
        worker.pending_summary_outbox_value_for_test(&threshold_episode_id),
        Some(session.id)
    );
}

#[tokio::test]
async fn failed_compaction_summary_does_not_wake_worker_or_prevent_later_success() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let session = db.create_session("compaction failure").unwrap();
    let failed_episode_id = haven_common::types::new_id("msg");
    db.add_episode_with_id(&session.id, "original summary", &failed_episode_id)
        .unwrap();

    let worker = test_worker(db.clone());
    let engine = test_engine(db.clone(), worker.clone());
    engine
        .persist_compaction_summary(
            &session.id,
            "a conflicting summary that is long enough to enqueue extraction",
            &failed_episode_id,
        )
        .await;

    assert_eq!(
        db.episode_text(&failed_episode_id).unwrap().as_deref(),
        Some("original summary")
    );
    assert!(db.pending_summary_extractions().unwrap().is_empty());
    assert!(
        worker
            .pending_summary_outbox_value_for_test(&failed_episode_id)
            .is_none()
    );

    let succeeding_episode_id = haven_common::types::new_id("msg");
    let succeeding_summary = "A later compaction summary still persists and wakes extraction.";
    engine
        .persist_compaction_summary(&session.id, succeeding_summary, &succeeding_episode_id)
        .await;
    assert_eq!(
        db.pending_summary_extractions().unwrap(),
        vec![(session.id.clone(), succeeding_episode_id.clone())]
    );
    assert_eq!(
        worker.pending_summary_outbox_value_for_test(&succeeding_episode_id),
        Some(session.id)
    );
}
