use super::*;
use haven_memory::{Database, ToolRunStore};
use std::time::Duration;

/// Poll `status` until it is no longer "running" (or timeout).
async fn wait_terminal(tool_runs: &ToolRunService, tool_run_id: &str, timeout_secs: u64) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let v = tool_runs.status_view(tool_run_id).await.to_json(true);
        if v["status"] != "running" || std::time::Instant::now() > deadline {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn recv_background(rx: &mut ToolRunCompletionReceiver) -> BackgroundToolRunCompletion {
    match rx.recv_background().await {
        Some(ToolRunCompletion::Background(completion)) => completion,
        None => panic!("ToolRun completion channel closed"),
    }
}

async fn terminal_test_service() -> (
    Arc<ToolRunService>,
    Arc<Database>,
    String,
    tempfile::TempDir,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(Database::open(&dir.path().join("tool_runs.db")).unwrap());
    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let tool_run_id = haven_common::types::new_id("toolrun");
    insert_running_background(&service, &db, &tool_run_id, None).await;
    (service, db, tool_run_id, dir)
}

fn capture_tool_run_events(
    service: &ToolRunService,
) -> Arc<std::sync::Mutex<Vec<(String, Value)>>> {
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = Arc::clone(&events);
    service.set_lifecycle_event_sink(Arc::new(move |event| {
        sink_events.lock().unwrap().push(event.into_test_parts());
    }));
    events
}

fn terminal_event_count(events: &std::sync::Mutex<Vec<(String, Value)>>) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "tool_run:finished")
        .count()
}

fn session_test_tool_run_entry(
    kind: ToolRunKind,
    session_id: Option<&str>,
    state: ToolRunState,
    kill: Option<tokio::sync::oneshot::Sender<()>>,
) -> ToolRunEntry {
    ToolRunEntry {
        tool_name: "shell".into(),
        kind,
        session_id: session_id.map(str::to_string),
        source_step_id: None,
        state,
        kill,
        tail: None,
        command: "echo session cancellation".into(),
        shell: "test".into(),
    }
}

async fn assert_no_background_completion(rx: &mut ToolRunCompletionReceiver) {
    assert!(
        tokio::time::timeout(Duration::from_millis(75), recv_background(rx))
            .await
            .is_err(),
        "unexpected duplicate or uncommitted background completion"
    );
}

#[tokio::test]
async fn background_terminal_race_publishes_only_the_database_cas_winner() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(3));

    let complete_service = Arc::clone(&service);
    let complete_barrier = Arc::clone(&barrier);
    let complete_tool_run_id = tool_run_id.clone();
    let complete = tokio::spawn(async move {
        complete_barrier.wait().await;
        complete_service
            .mark_finished(
                &complete_tool_run_id,
                "2026-09-24T10:00:00Z",
                "completed output".into(),
                true,
                false,
            )
            .await;
    });

    let cancel_service = Arc::clone(&service);
    let cancel_barrier = Arc::clone(&barrier);
    let cancel_tool_run_id = tool_run_id.clone();
    let cancel = tokio::spawn(async move {
        cancel_barrier.wait().await;
        cancel_service
            .mark_cancelled(&cancel_tool_run_id, "2026-09-24T10:00:00Z")
            .await;
    });

    barrier.wait().await;
    complete.await.unwrap();
    cancel.await.unwrap();

    let row = db.get_tool_run(&tool_run_id).unwrap().unwrap();
    assert!(matches!(
        row.status,
        ToolRunStatus::Completed | ToolRunStatus::Cancelled
    ));
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        row.status.as_str()
    );
    assert_eq!(terminal_event_count(&events), 1);
    let completion = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("the CAS winner publishes one completion");
    assert_eq!(completion.status, row.status);
    assert_no_background_completion(&mut rx).await;

    let outbox_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM tool_run_completion_outbox WHERE tool_run_id = ?1",
            [&tool_run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        outbox_count,
        i64::from(row.status == ToolRunStatus::Completed)
    );
}

#[tokio::test]
async fn background_terminal_cas_loser_reconciles_without_publishing() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    let completion_payload = ToolRunCompletionPayload {
        tool_run_id: tool_run_id.clone(),
        tool_name: "shell".into(),
        status: ToolRunStatus::Completed,
        output: Some("external winner".to_owned()),
        error: None,
        error_reason: None,
        log_path: None,
        started_at: None,
        finished_at: Some("2026-09-24T11:00:00Z".to_owned()),
        source_step_id: None,
        truncated: false,
    };
    assert!(
        db.finish_tool_run_with_completion(&completion_payload)
            .unwrap()
    );

    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            "local loser".into(),
            true,
            false,
        )
        .await;

    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "completed"
    );
    assert_eq!(
        db.get_tool_run(&tool_run_id)
            .unwrap()
            .unwrap()
            .output
            .as_deref(),
        Some("external winner")
    );
    assert_eq!(terminal_event_count(&events), 0);
    assert_no_background_completion(&mut rx).await;
}

#[tokio::test]
async fn background_terminal_storage_error_stays_running_then_retries_once() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    db.conn()
        .execute_batch(&format!(
            "CREATE TRIGGER block_background_completion
             BEFORE INSERT ON tool_run_completion_outbox
             WHEN NEW.tool_run_id = '{tool_run_id}'
             BEGIN SELECT RAISE(ABORT, 'injected outbox failure'); END;"
        ))
        .unwrap();

    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            "retry output".into(),
            true,
            false,
        )
        .await;

    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "running"
    );
    assert_eq!(
        db.get_tool_run(&tool_run_id).unwrap().unwrap().status,
        ToolRunStatus::Running
    );
    assert_eq!(terminal_event_count(&events), 0);
    assert_no_background_completion(&mut rx).await;

    db.conn()
        .execute_batch("DROP TRIGGER block_background_completion")
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while service.status_view(&tool_run_id).await.to_json(true)["status"] == "running" {
        assert!(
            std::time::Instant::now() < deadline,
            "terminal retry did not commit"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "completed"
    );
    assert_eq!(terminal_event_count(&events), 1);
    let completion = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("retry publishes after its database commit");
    assert_eq!(completion.payload.output.as_deref(), Some("retry output"));
    assert_no_background_completion(&mut rx).await;

    let outbox_count: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM tool_run_completion_outbox WHERE tool_run_id = ?1",
            [&tool_run_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outbox_count, 1);
}

#[tokio::test]
async fn background_cancel_storage_error_does_not_publish_before_retry_commit() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    db.conn()
        .execute_batch(&format!(
            "CREATE TRIGGER block_background_cancel
             BEFORE UPDATE OF status ON tool_runs
             WHEN NEW.id = '{tool_run_id}' AND NEW.status = 'cancelled'
             BEGIN SELECT RAISE(ABORT, 'injected cancel failure'); END;"
        ))
        .unwrap();

    service
        .mark_cancelled(&tool_run_id, "2026-09-24T10:00:00Z")
        .await;
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "running"
    );
    assert_eq!(terminal_event_count(&events), 0);
    assert_no_background_completion(&mut rx).await;

    db.conn()
        .execute_batch("DROP TRIGGER block_background_cancel")
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while service.status_view(&tool_run_id).await.to_json(true)["status"] == "running" {
        assert!(
            std::time::Instant::now() < deadline,
            "cancel retry did not commit"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "cancelled"
    );
    assert_eq!(
        db.get_tool_run(&tool_run_id).unwrap().unwrap().status,
        ToolRunStatus::Cancelled
    );
    assert_eq!(terminal_event_count(&events), 1);
    let completion = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("cancellation publishes only after durable commit");
    assert_eq!(completion.status, ToolRunStatus::Cancelled);
    assert_no_background_completion(&mut rx).await;
}

#[tokio::test]
async fn repeated_background_completion_is_idempotent_and_publishes_once() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    for output in ["first output", "duplicate output"] {
        service
            .mark_finished(
                &tool_run_id,
                "2026-09-24T10:00:00Z",
                output.into(),
                true,
                false,
            )
            .await;
    }

    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["output"],
        "first output"
    );
    assert_eq!(
        db.get_tool_run(&tool_run_id)
            .unwrap()
            .unwrap()
            .output
            .as_deref(),
        Some("first output")
    );
    assert_eq!(terminal_event_count(&events), 1);
    let completion = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("first terminal commit published");
    assert_eq!(completion.payload.output.as_deref(), Some("first output"));
    assert_no_background_completion(&mut rx).await;
}

#[tokio::test]
async fn persistent_late_attach_updates_outbox_without_republishing_completion() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            "late owner output".into(),
            true,
            false,
        )
        .await;
    let initial = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("committed completion published");
    assert!(initial.session_id.is_none());

    let session_id = haven_common::types::new_id("ses");
    service.attach_session(&tool_run_id, &session_id).await;
    assert_no_background_completion(&mut rx).await;
    assert_eq!(terminal_event_count(&events), 1);

    let outbox = db.claim_tool_run_completion().unwrap().unwrap();
    assert_eq!(outbox.tool_run_id, tool_run_id);
    assert_eq!(outbox.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(outbox.payload.output.as_deref(), Some("late owner output"));
}

#[tokio::test]
async fn late_attach_reopens_completion_after_unowned_ack() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let mut rx = service.take_tool_run_receiver().unwrap();
    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            "late owner output".into(),
            true,
            false,
        )
        .await;
    let initial = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("committed completion published");
    assert!(initial.session_id.is_none());

    service
        .acknowledge_unowned_tool_run_completion(&initial.tool_run_result_id)
        .await;
    service
        .attach_session(&tool_run_id, "ses-1b5db32ce2b2e978b8f9a3cabf01f712")
        .await;

    let pending = service
        .claim_pending_tool_run_result()
        .await
        .expect("late binding must reopen the durable completion");
    let ToolRunCompletion::Background(pending) = pending;
    assert_eq!(
        pending.session_id.as_deref(),
        Some("ses-1b5db32ce2b2e978b8f9a3cabf01f712")
    );
    assert!(!service.delete_terminal(&tool_run_id).await.unwrap());
    assert!(db.get_tool_run(&tool_run_id).unwrap().is_some());
}

#[tokio::test]
async fn session_cleanup_keeps_running_tool_run_until_cancel_commit() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    let session_id = haven_common::types::new_id("ses");
    {
        let mut tool_runs = service.tool_runs.write().await;
        tool_runs.get_mut(&tool_run_id).unwrap().session_id = Some(session_id.clone());
    }
    db.update_tool_run_session(&tool_run_id, &session_id)
        .unwrap();
    let events = capture_tool_run_events(&service);
    let mut rx = service.take_tool_run_receiver().unwrap();
    db.conn()
        .execute_batch(&format!(
            "CREATE TRIGGER block_cleanup_cancel
             BEFORE UPDATE OF status ON tool_runs
             WHEN NEW.id = '{tool_run_id}' AND NEW.status = 'cancelled'
             BEGIN SELECT RAISE(ABORT, 'injected cleanup failure'); END;"
        ))
        .unwrap();

    service
        .cancel_owned_background_by_session(&session_id)
        .await;
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "running"
    );
    assert_eq!(
        db.get_tool_run(&tool_run_id).unwrap().unwrap().status,
        ToolRunStatus::Running
    );
    assert_eq!(terminal_event_count(&events), 0);
    assert_no_background_completion(&mut rx).await;

    db.conn()
        .execute_batch("DROP TRIGGER block_cleanup_cancel")
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while service.status_view(&tool_run_id).await.to_json(true)["status"] != "not_found" {
        assert!(
            std::time::Instant::now() < deadline,
            "cleanup retry did not commit and remove the board entry"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        db.get_tool_run(&tool_run_id).unwrap().unwrap().status,
        ToolRunStatus::Cancelled
    );
    assert_eq!(terminal_event_count(&events), 1);
    let completion = tokio::time::timeout(Duration::from_secs(1), recv_background(&mut rx))
        .await
        .expect("cleanup publishes after the cancellation commits");
    assert_eq!(completion.status, ToolRunStatus::Cancelled);
    assert_eq!(completion.session_id.as_deref(), Some(session_id.as_str()));
    assert_no_background_completion(&mut rx).await;
}

#[tokio::test]
async fn persisted_tool_run_query_requires_a_bound_store() {
    let service = ToolRunService::new();

    let error = service
        .list_history(ToolRunHistoryQuery::new(None, None, 20))
        .await
        .expect_err("an unbound service must not report empty history");

    assert!(
        error
            .to_string()
            .contains("ToolRun store is not configured")
    );
}

/// Spawn the two fixture echo ToolRuns (`tool-run-a` / `tool-run-b`) and attach them to
/// Attach two echo ToolRuns to canonical sessions. Shared by the board and scoped-list tests.
async fn spawn_two_echo_tool_runs(tool_runs: &Arc<ToolRunService>) -> (String, String) {
    let first_tool_run_id = tool_runs
        .spawn_shell("echo tool-run-a", "cmd", 20_000, None)
        .await
        .unwrap();
    let second_tool_run_id = tool_runs
        .spawn_shell("echo tool-run-b", "cmd", 20_000, None)
        .await
        .unwrap();
    tool_runs
        .attach_session(&first_tool_run_id, "ses-09df36791e54c51f7be063867e10b9af")
        .await;
    tool_runs
        .attach_session(&second_tool_run_id, "ses-9faf101bcfff72cb42cfe3bcfe2a11e1")
        .await;
    (first_tool_run_id, second_tool_run_id)
}

#[cfg(windows)]
#[tokio::test]
async fn test_completion_notified_on_finish() {
    let tool_runs = Arc::new(ToolRunService::new());
    let mut rx = tool_runs
        .take_tool_run_receiver()
        .expect("receiver available");
    // Attach the session BEFORE the ToolRun finishes (normal path): the
    // completion must carry the session_id.
    let tool_run_id = tool_runs
        .spawn_shell("echo done", "cmd", 20_000, None)
        .await
        .unwrap();
    tool_runs
        .attach_session(&tool_run_id, "ses-92f863021d24c9259b49c176ea2845df")
        .await;
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "completed");
    let comp = tokio::time::timeout(Duration::from_secs(2), recv_background(&mut rx))
        .await
        .expect("completion received");
    assert_eq!(comp.tool_run_id, tool_run_id);
    assert_eq!(
        comp.status,
        haven_common::lifecycle::ToolRunStatus::Completed
    );
    assert_eq!(
        comp.session_id.as_deref(),
        Some("ses-92f863021d24c9259b49c176ea2845df")
    );
    assert!(comp.payload.output.as_deref().unwrap().contains("done"));
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_for_session_binds_owner_before_completion() {
    let tool_runs = Arc::new(ToolRunService::new());
    let mut rx = tool_runs
        .take_tool_run_receiver()
        .expect("receiver available");
    let tool_run_id = tool_runs
        .spawn_shell_for_session(
            "echo prebound",
            "cmd",
            20_000,
            None,
            Some("ses-c9727d934ca3dbc6a3ef095ac246a427"),
        )
        .await
        .unwrap();

    let completion = tokio::time::timeout(Duration::from_secs(10), recv_background(&mut rx))
        .await
        .expect("completion received");
    assert_eq!(completion.tool_run_id, tool_run_id);
    assert_eq!(
        completion.session_id.as_deref(),
        Some("ses-c9727d934ca3dbc6a3ef095ac246a427")
    );
    assert_eq!(
        tool_runs
            .status_for_session_view(&tool_run_id, "ses-c9727d934ca3dbc6a3ef095ac246a427")
            .await
            .to_json(true)["status"],
        "completed"
    );
    tool_runs
        .attach_session(&tool_run_id, "ses-c9727d934ca3dbc6a3ef095ac246a427")
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), recv_background(&mut rx))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_background_admission_waiting_on_cleanup_gate_is_not_published() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Arc::new(Database::open(&dir.path().join("cancelled-admission.db")).unwrap());
    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let held_gate = service.spawn_gate.lock().await;
    let cancel = tokio_util::sync::CancellationToken::new();
    let spawn_service = Arc::clone(&service);
    let spawn_cancel = cancel.clone();
    let spawn = tokio::spawn(async move {
        spawn_service
            .spawn_shell_for_session_with_source_and_cancel(
                super::BackgroundShellRequest {
                    command: "echo must-not-start",
                    shell: "cmd",
                    max_chars: 20_000,
                    cwd: None,
                    session_id: Some("ses-25c77666e61c3a1f45188a11a91e7485"),
                    source_step_id: None,
                    execution_authorization: None,
                },
                &spawn_cancel,
            )
            .await
    });
    tokio::task::yield_now().await;
    cancel.cancel();

    let error = tokio::time::timeout(Duration::from_secs(1), spawn)
        .await
        .expect("cancelled admission must not wait for cleanup gate")
        .expect("spawn task should join")
        .expect_err("a cancelled tool must not admit a background ToolRun");
    assert!(error.to_string().contains("cancelled"));
    assert!(db.list_tool_runs(Some("background")).unwrap().is_empty());
    assert!(service.tool_runs.read().await.is_empty());
    drop(held_gate);
}

#[tokio::test]
async fn cleanup_cancels_durable_background_admission_without_board_entry() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Arc::new(Database::open(&dir.path().join("orphan-background.db")).unwrap());
    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let session_id = "ses-a865b5f239cd7498542e1f90f3f00a37";
    let tool_run_id = haven_common::types::new_id("toolrun");
    db.save_tool_run_with_source(
        &tool_run_id,
        Some(session_id),
        "echo admission worker completed after caller cancellation",
        "2026-10-05T00:00:00Z",
        Some("step-00000000000000000000000000000007"),
    )
    .unwrap();

    service
        .cancel_owned_background_by_session_checked(session_id)
        .await
        .unwrap();

    let row = db.get_tool_run(&tool_run_id).unwrap().unwrap();
    assert_eq!(
        row.status,
        haven_common::lifecycle::ToolRunStatus::Cancelled
    );
    assert!(service.tool_runs.read().await.is_empty());
}

#[cfg(windows)]
#[cfg(windows)]
#[tokio::test]
async fn test_tool_run_result_persisted_to_db() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
    let tool_runs = Arc::new(ToolRunService::new());
    tool_runs
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;

    let tool_run_id = tool_runs
        .spawn_shell(
            "echo live-line & ping -n 4 127.0.0.1 >nul",
            "cmd",
            20_000,
            None,
        )
        .await
        .unwrap();
    tool_runs
        .attach_session(&tool_run_id, "ses-8427891e09582e8e6b03246b585c69b5")
        .await;
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "completed");

    // The status flips to completed before the terminal row is persisted
    // (mark_finished → notify_completion → persist_terminal); poll the
    // DB instead of reading it immediately.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let row = loop {
        let rows = db.list_tool_runs(Some("background")).unwrap();
        if let Some(row) = rows.iter().find(|r| {
            r.tool_run_id == tool_run_id
                && r.status == haven_common::lifecycle::ToolRunStatus::Completed
        }) {
            break row.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ToolRun row never persisted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(row.kind, "background");
    assert_eq!(
        row.status,
        haven_common::lifecycle::ToolRunStatus::Completed
    );
    assert_eq!(
        row.session_id.as_deref(),
        Some("ses-8427891e09582e8e6b03246b585c69b5")
    );
    assert!(row.output.as_deref().unwrap().contains("live-line"));
    assert!(row.finished_at.is_some());
}

#[cfg(windows)]
#[tokio::test]
async fn test_completion_refired_after_late_attach() {
    // Race path: the ToolRun finishes before attach_session is called. The
    // completion first fires with session_id=None; attach_session must re-fire
    // with the session_id so the owning session still gets notified.
    let tool_runs = Arc::new(ToolRunService::new());
    let mut rx = tool_runs
        .take_tool_run_receiver()
        .expect("receiver available");
    let tool_run_id = tool_runs
        .spawn_shell("echo fast", "cmd", 20_000, None)
        .await
        .unwrap();
    // Wait for the ToolRun to finish BEFORE attaching (simulate the race).
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "completed");
    // Drain the session_id=None completion fired by mark_finished.
    let none_comp = recv_background(&mut rx).await;
    assert!(none_comp.session_id.is_none());
    // Now attach: should re-fire with the session_id.
    tool_runs
        .attach_session(&tool_run_id, "ses-ee19b99e43f404f74b13f81e4b3dffcf")
        .await;
    let comp = tokio::time::timeout(Duration::from_secs(2), recv_background(&mut rx))
        .await
        .expect("refired completion received");
    assert_eq!(
        comp.session_id.as_deref(),
        Some("ses-ee19b99e43f404f74b13f81e4b3dffcf")
    );
    assert_eq!(
        comp.status,
        haven_common::lifecycle::ToolRunStatus::Completed
    );
}

#[tokio::test]
async fn test_completion_skipped_for_running() {
    let tool_runs = Arc::new(ToolRunService::new());
    // No tool_runs → no completion. Just confirm the receiver is taken.
    let _rx = tool_runs
        .take_tool_run_receiver()
        .expect("receiver available");
    // status on not_found doesn't notify.
    assert_eq!(
        tool_runs.status_view("nope").await.to_json(true)["status"],
        "not_found"
    );
}

#[tokio::test]
async fn test_background_completion_reconciles_after_broadcast_loss() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(Database::open(&dir.path().join("reconcile.db")).unwrap());
    db.create_session("durable completion").unwrap();
    db.save_tool_run(
        "toolrun-0000000000000000000000000000000b",
        Some("ses-fe54dfaa28bbc2e34df6b1bb5b110224"),
        "echo durable",
        "2026-09-24T10:00:00Z",
    )
    .unwrap();
    db.finish_tool_run(
        "toolrun-0000000000000000000000000000000b",
        haven_common::lifecycle::ToolRunStatus::Completed,
        Some("durable output"),
        None,
        None,
        None,
        "2026-09-24T11:00:00Z",
    )
    .unwrap();

    // No broadcast was sent to this service. The receiver must rebuild the
    // completion from terminal ToolRun history and keep it pending until the
    // transcript consumer acknowledges it.
    let tool_runs = Arc::new(ToolRunService::new());
    tool_runs
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let mut rx = tool_runs.take_tool_run_receiver().unwrap();
    let completion = tokio::time::timeout(
        Duration::from_secs(2),
        rx.recv_tool_run_result_with_recovery(tool_runs.as_ref()),
    )
    .await
    .expect("durable completion should be reconciled")
    .expect("completion bus should remain open");
    let ToolRunCompletion::Background(completion) = completion;
    assert_eq!(
        completion.tool_run_id,
        "toolrun-0000000000000000000000000000000b"
    );
    assert_eq!(
        completion.session_id.as_deref(),
        Some("ses-fe54dfaa28bbc2e34df6b1bb5b110224")
    );
    assert_eq!(completion.payload.output.as_deref(), Some("durable output"));

    // History deletion is rejected while the durable completion has not
    // crossed the transcript boundary.
    assert!(
        !tool_runs
            .delete_terminal("toolrun-0000000000000000000000000000000b")
            .await
            .unwrap()
    );
    assert!(
        db.get_tool_run("toolrun-0000000000000000000000000000000b")
            .unwrap()
            .is_some()
    );

    // A claimed row is not delivered twice before the transcript boundary is
    // durable. Once that boundary is acknowledged, recovery is quiescent.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            rx.recv_tool_run_result_with_recovery(tool_runs.as_ref())
        )
        .await
        .is_err()
    );
    tool_runs
        .acknowledge_tool_run_completion(&completion.tool_run_result_id)
        .await;
    assert!(
        tool_runs
            .delete_terminal("toolrun-0000000000000000000000000000000b")
            .await
            .unwrap()
    );
    assert!(
        db.get_tool_run("toolrun-0000000000000000000000000000000b")
            .unwrap()
            .is_none()
    );
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            rx.recv_tool_run_result_with_recovery(tool_runs.as_ref())
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn terminal_history_delete_and_completion_ack_are_atomic() {
    let (service, db, tool_run_id, _dir) = terminal_test_service().await;
    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            "race output".into(),
            true,
            false,
        )
        .await;

    let ack_store = ToolRunStore::new(db.clone());
    let (delete_result, ack_result) = tokio::join!(
        service.delete_terminal(&tool_run_id),
        ack_store.acknowledge_completion(tool_run_id.clone()),
    );
    let deleted = delete_result.unwrap();
    let acknowledged = ack_result.unwrap();

    // If delete wins the SQLite writer race, acknowledgement must have won
    // first; otherwise the ToolRun and its pending completion remain intact.
    if deleted {
        assert!(acknowledged);
        assert!(db.get_tool_run(&tool_run_id).unwrap().is_none());
    } else {
        assert!(db.get_tool_run(&tool_run_id).unwrap().is_some());
        let pending: i64 = db
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM tool_run_completion_outbox
                 WHERE tool_run_id = ?1 AND delivered_at IS NULL",
                [&tool_run_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, i64::from(!acknowledged));
    }
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_completes_with_output() {
    let tool_runs = Arc::new(ToolRunService::new());
    let tool_run_id = tool_runs
        .spawn_shell("echo bg-hello", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "completed", "got: {}", v);
    assert!(v["output"].as_str().unwrap().contains("bg-hello"));
    assert!(v["finished_at"].as_str().is_some());
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_failure_reported() {
    let tool_runs = Arc::new(ToolRunService::new());
    let tool_run_id = tool_runs
        .spawn_shell("exit 7", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "failed", "got: {}", v);
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_stderr_captured() {
    let tool_runs = Arc::new(ToolRunService::new());
    let tool_run_id = tool_runs
        .spawn_shell("echo err-msg 1>&2", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "completed", "got: {}", v);
    assert!(v["output"].as_str().unwrap().contains("err-msg"));
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_cancelled() {
    let tool_runs = Arc::new(ToolRunService::new());
    let tool_run_id = tool_runs
        .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
        .await
        .unwrap();
    assert_eq!(
        tool_runs.status_view(&tool_run_id).await.to_json(true)["status"],
        "running"
    );
    assert!(
        tool_runs.cancel(&tool_run_id).await,
        "cancel must report success"
    );
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "cancelled", "got: {}", v);
}

#[cfg(windows)]
#[tokio::test]
async fn test_cancel_for_session_cleans_up() {
    let tool_runs = Arc::new(ToolRunService::new());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = events.clone();
    tool_runs.set_lifecycle_event_sink(Arc::new(move |event| {
        sink_events.lock().unwrap().push(event.into_test_parts());
    }));
    let tool_run_id = tool_runs
        .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
        .await
        .unwrap();
    tool_runs
        .attach_session(&tool_run_id, "ses-09df36791e54c51f7be063867e10b9af")
        .await;
    assert_eq!(
        tool_runs.status_view(&tool_run_id).await.to_json(true)["status"],
        "running"
    );
    tool_runs
        .cancel_owned_background_by_session_checked("ses-09df36791e54c51f7be063867e10b9af")
        .await
        .unwrap();
    assert_eq!(
        tool_runs.status_view(&tool_run_id).await.to_json(true)["status"],
        "not_found"
    );
    let evs = events.lock().unwrap();
    let finished = evs
        .iter()
        .find(|(n, _)| n == "tool_run:finished")
        .expect("cancel_for_session must emit tool_run:finished so the UI drops the ghost");
    assert_eq!(finished.1["tool_run_id"], tool_run_id);
    assert_eq!(finished.1["status"], "cancelled");
}

#[tokio::test]
async fn test_status_not_found() {
    let tool_runs = Arc::new(ToolRunService::new());
    assert_eq!(
        tool_runs
            .status_view("toolrun-00000000000000000000000000000fff")
            .await
            .to_json(true)["status"],
        "not_found"
    );
}

#[tokio::test]
async fn test_cancel_unknown_tool_run() {
    let tool_runs = Arc::new(ToolRunService::new());
    assert!(
        !tool_runs
            .cancel("toolrun-00000000000000000000000000000fff")
            .await
    );
}

#[tokio::test]
async fn test_spawn_empty_command_rejected() {
    let tool_runs = Arc::new(ToolRunService::new());
    assert!(
        tool_runs
            .spawn_shell("  ", "cmd", 20_000, None)
            .await
            .is_err()
    );
}

// ── list_for_session (tool_runs board) ────────────────────────────────────────

#[cfg(windows)]
#[tokio::test]
async fn lifecycle_event_sink_receives_lifecycle_events() {
    let tool_runs = Arc::new(ToolRunService::new());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = events.clone();
    tool_runs.set_lifecycle_event_sink(Arc::new(move |event| {
        sink_events.lock().unwrap().push(event.into_test_parts());
    }));
    let tool_run_id = tool_runs
        .spawn_shell("echo bg-event", "cmd", 20_000, None)
        .await
        .unwrap();
    tool_runs
        .attach_session(&tool_run_id, "ses-a07db86895e9387dbc8e4517a8bae2da")
        .await;
    wait_terminal(&tool_runs, &tool_run_id, 10).await;

    let evs = events.lock().unwrap();
    let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"tool_run:created"), "got: {names:?}");
    assert!(names.contains(&"tool_run:updated"), "got: {names:?}");
    assert!(names.contains(&"tool_run:finished"), "got: {names:?}");
    let created = evs
        .iter()
        .find(|(n, _)| n == "tool_run:created")
        .expect("created event");
    assert_eq!(created.1["tool_run_id"], tool_run_id);
    assert_eq!(created.1["status"], "running");
    assert_eq!(created.1["kind"], "background");
    let term = evs
        .iter()
        .find(|(n, _)| n == "tool_run:finished")
        .expect("terminal event");
    assert_eq!(term.1["status"], "completed");
    assert_eq!(term.1["tool_run_id"], tool_run_id);
}

#[cfg(windows)]
#[tokio::test]
async fn test_board_lists_all_tool_runs_by_session() {
    let tool_runs = Arc::new(ToolRunService::new());
    let (first_tool_run_id, second_tool_run_id) = spawn_two_echo_tool_runs(&tool_runs).await;
    wait_terminal(&tool_runs, &first_tool_run_id, 10).await;
    wait_terminal(&tool_runs, &second_tool_run_id, 10).await;

    let rows = tool_runs.board().await;
    assert_eq!(rows.len(), 2, "all tool_runs on board: {rows:?}");
    let by_id: HashMap<_, _> = rows
        .iter()
        .map(|row| (row.tool_run_id.as_str(), row))
        .collect();
    assert_eq!(
        by_id[&first_tool_run_id.as_str()].session_id.as_deref(),
        Some("ses-09df36791e54c51f7be063867e10b9af")
    );
    assert_eq!(
        by_id[&second_tool_run_id.as_str()].session_id.as_deref(),
        Some("ses-9faf101bcfff72cb42cfe3bcfe2a11e1")
    );
    assert_eq!(
        by_id[&first_tool_run_id.as_str()].status,
        ToolRunStatus::Completed
    );
    assert!(
        by_id[&first_tool_run_id.as_str()]
            .preview
            .as_deref()
            .unwrap()
            .contains("tool-run-a"),
        "preview expected, got: {rows:?}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_list_for_session_scopes_to_owning_session() {
    let tool_runs = Arc::new(ToolRunService::new());
    let (first_tool_run_id, second_tool_run_id) = spawn_two_echo_tool_runs(&tool_runs).await;
    wait_terminal(&tool_runs, &first_tool_run_id, 10).await;
    wait_terminal(&tool_runs, &second_tool_run_id, 10).await;

    let rows = tool_runs
        .list_for_session_views("ses-09df36791e54c51f7be063867e10b9af")
        .await
        .into_iter()
        .map(|row| row.to_json())
        .collect::<Vec<_>>();
    assert_eq!(
        rows.len(),
        1,
        "only ses-09df36791e54c51f7be063867e10b9af's tool_runs: {rows:?}"
    );
    assert_eq!(rows[0]["tool_run_id"], first_tool_run_id);
    assert_eq!(rows[0]["status"], "completed");
    assert!(
        rows[0]["preview"].as_str().unwrap().contains("tool-run-a"),
        "preview expected, got: {rows:?}"
    );

    let all = tool_runs
        .list_for_session_views("ses-9faf101bcfff72cb42cfe3bcfe2a11e1")
        .await
        .into_iter()
        .map(|row| row.to_json())
        .collect::<Vec<_>>();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["tool_run_id"], second_tool_run_id);
}

#[cfg(windows)]
#[tokio::test]
async fn test_failed_tool_run_reports_reason_without_shell_exit_code() {
    let tool_runs = Arc::new(ToolRunService::new());
    let tool_run_id = tool_runs
        .spawn_shell("echo progress... && exit 42", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&tool_runs, &tool_run_id, 10).await;
    assert_eq!(v["status"], "failed", "got: {v}");
    assert!(
        v.get("exit_code").is_none(),
        "ToolRun is tool-oriented: {v}"
    );
    assert!(
        v["error_reason"].as_str().is_some_and(|s| !s.is_empty()),
        "error_reason must be present, got: {v}"
    );
}

#[tokio::test]
async fn delete_terminal_rejects_unknown_persisted_tool_run_kind() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let tool_run_id = haven_common::types::new_id("toolrun");
    let now = chrono::Utc::now().to_rfc3339();
    db.save_tool_run(&tool_run_id, None, "echo terminal-test", &now)
        .unwrap();
    assert!(db.cancel_background_tool_run(&tool_run_id, &now).unwrap());
    let connection = db.conn();
    connection
        .pragma_update(None, "ignore_check_constraints", true)
        .unwrap();
    connection
        .execute(
            "UPDATE tool_runs SET kind = 'unknown' WHERE id = ?1",
            [&tool_run_id],
        )
        .unwrap();
    connection
        .pragma_update(None, "ignore_check_constraints", false)
        .unwrap();
    drop(connection);

    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;

    assert!(!service.delete_terminal(&tool_run_id).await.unwrap());
    assert!(db.get_tool_run(&tool_run_id).unwrap().is_some());
}

#[tokio::test]
async fn clear_terminal_history_removes_only_terminal_persisted_rows() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(Database::open(&dir.path().join("clear-history.db")).unwrap());
    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let terminal_id = haven_common::types::new_id("toolrun");
    let running_id = haven_common::types::new_id("toolrun");
    let now = "2026-10-06T00:00:00Z";
    db.save_tool_run(&terminal_id, None, "echo done", now)
        .unwrap();
    assert!(db.cancel_background_tool_run(&terminal_id, now).unwrap());
    db.save_tool_run(&running_id, None, "echo running", now)
        .unwrap();

    assert_eq!(service.clear_terminal_history().await.unwrap(), 1);
    assert!(db.get_tool_run(&terminal_id).unwrap().is_none());
    assert!(db.get_tool_run(&running_id).unwrap().is_some());
}

#[tokio::test]
async fn test_shutdown_cancels_and_joins_background_runner_before_returning() {
    let service = Arc::new(ToolRunService::new());
    let tool_run_id = service
        .spawn_shell("ping -n 30 127.0.0.1 >NUL", "cmd", 1024, None)
        .await
        .unwrap();

    service.shutdown().await;

    assert!(service.task_tracker.is_empty());
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "cancelled"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_shutdown_terminates_background_shell_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("background-shell-descendant.pid");
    let mut descendant =
        crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
    let marker = pid_file.to_string_lossy().replace('\'', "''");
    let command = format!(
        "$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 60' -PassThru; [System.IO.File]::WriteAllText('{marker}', [string]$child.Id); Start-Sleep -Seconds 60"
    );
    let service = Arc::new(ToolRunService::new());
    let tool_run_id = service
        .spawn_shell(
            &command,
            "powershell",
            1024,
            Some(temp.path().to_path_buf()),
        )
        .await
        .unwrap();

    let marker_ready = tokio::time::timeout(Duration::from_secs(20), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok();
    let descendant_pid = marker_ready.then(|| descendant.pid()).transpose();
    service.shutdown().await;

    assert!(marker_ready, "background Shell must start its descendant");
    assert!(service.task_tracker.is_empty());
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "cancelled"
    );
    let descendant_pid = descendant_pid
        .expect("descendant PID marker should be readable")
        .expect("descendant PID should be numeric");
    descendant
        .wait_for_exit(descendant_pid, Duration::from_secs(5))
        .expect("ToolRun shutdown must close the Job Object");
}

#[cfg(windows)]
#[tokio::test]
async fn test_policy_revocation_terminates_background_shell_descendants() {
    let temp = tempfile::tempdir().unwrap();
    let pid_file = temp.path().join("revoked-background-shell-descendant.pid");
    let mut descendant =
        crate::process_test_support::DescendantProcessMarker::new(pid_file.clone());
    let marker = pid_file.to_string_lossy().replace('\'', "''");
    let command = format!(
        "$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList '-NoProfile -NonInteractive -Command Start-Sleep -Seconds 60' -PassThru; [System.IO.File]::WriteAllText('{marker}', [string]$child.Id); Start-Sleep -Seconds 60"
    );
    let (execution_authorization, authorization) =
        crate::security::ExecutionPolicyAuthorization::for_test_with_engine();
    let service = Arc::new(ToolRunService::new());
    let admission_cancel = tokio_util::sync::CancellationToken::new();
    let tool_run_id = service
        .spawn_shell_for_session_with_source_and_cancel(
            super::BackgroundShellRequest {
                command: &command,
                shell: "powershell",
                max_chars: 1024,
                cwd: Some(temp.path().to_path_buf()),
                session_id: None,
                source_step_id: None,
                execution_authorization: Some(execution_authorization),
            },
            &admission_cancel,
        )
        .await
        .unwrap();

    let marker_ready = tokio::time::timeout(Duration::from_secs(20), async {
        while !pid_file.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok();
    assert!(marker_ready, "background Shell must start its descendant");
    let descendant_pid = descendant.pid().expect("descendant PID should be numeric");

    authorization
        .set_permission_mode(haven_common::types::PermissionMode::Plan)
        .await;
    let terminal = wait_terminal(&service, &tool_run_id, 10).await;
    assert_eq!(terminal["status"], "cancelled");
    descendant
        .wait_for_exit(descendant_pid, Duration::from_secs(5))
        .expect("policy revocation must close the background Job Object");
}

#[tokio::test]
async fn test_concurrent_shutdown_calls_wait_for_the_same_task_drain() {
    let service = Arc::new(ToolRunService::new());
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (child_started_tx, child_started_rx) = tokio::sync::oneshot::channel();
    let (release_child_tx, release_child_rx) = tokio::sync::oneshot::channel();
    let service_for_child = service.clone();
    service.spawn_owned_task(async move {
        let _ = started_tx.send(());
        let _ = release_rx.await;
        service_for_child.spawn_owned_task(async move {
            let _ = child_started_tx.send(());
            let _ = release_child_rx.await;
        });
    });
    started_rx.await.unwrap();

    let mut first_shutdown = {
        let service = service.clone();
        tokio::spawn(async move { service.shutdown().await })
    };
    tokio::task::yield_now().await;
    assert!(!first_shutdown.is_finished());

    let second_shutdown = {
        let service = service.clone();
        tokio::spawn(async move { service.shutdown().await })
    };
    tokio::task::yield_now().await;
    assert!(!second_shutdown.is_finished());

    release_tx.send(()).unwrap();
    child_started_rx
        .await
        .expect("tracked task should admit its terminal cleanup child");
    assert!(!first_shutdown.is_finished());
    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut first_shutdown)
            .await
            .is_err(),
        "first shutdown must wait while the nested cleanup task is active"
    );
    release_child_tx.send(()).unwrap();
    // A nested task is counted before its parent exits, so neither shutdown
    // call can observe a transient empty tracker and return early.
    tokio::time::timeout(Duration::from_secs(1), &mut first_shutdown)
        .await
        .expect("shutdown should complete after both tracked tasks exit")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), second_shutdown)
        .await
        .expect("second shutdown should wait for the same drain")
        .unwrap();
    assert!(service.task_tracker.is_closed());
    assert!(service.task_tracker.is_empty());
}

async fn insert_running_background(
    service: &ToolRunService,
    db: &Database,
    tool_run_id: &str,
    session_id: Option<&str>,
) {
    let started_at = chrono::Utc::now().to_rfc3339();
    db.save_tool_run(tool_run_id, session_id, "echo terminal-test", &started_at)
        .unwrap();
    service.tool_runs.write().await.insert(
        tool_run_id.to_string(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: session_id.map(str::to_string),
            source_step_id: None,
            state: ToolRunState::Running { started_at },
            kill: None,
            tail: None,
            command: "echo terminal-test".into(),
            shell: "test".into(),
        },
    );
}

#[tokio::test]
async fn missing_store_keeps_background_tool_runs_memory_only() {
    let service = Arc::new(ToolRunService::new());
    let mut receiver = service.take_tool_run_receiver().unwrap();
    let tool_run_id = haven_common::types::new_id("toolrun");
    let session_id = haven_common::types::new_id("ses");
    service.tool_runs.write().await.insert(
        tool_run_id.clone(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: Some(session_id.clone()),
            source_step_id: None,
            state: ToolRunState::Running {
                started_at: "2026-09-24T10:00:00Z".into(),
            },
            kill: None,
            tail: None,
            command: "echo memory-only".into(),
            shell: "test".into(),
        },
    );

    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            "memory result".into(),
            true,
            false,
        )
        .await;

    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "completed"
    );
    let completion = recv_background(&mut receiver).await;
    assert_eq!(completion.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(completion.payload.output.as_deref(), Some("memory result"));
}

#[tokio::test]
async fn terminal_projection_keeps_final_output_and_releases_live_tail() {
    let service = Arc::new(ToolRunService::new());
    let events = capture_tool_run_events(&service);
    let tool_run_id = haven_common::types::new_id("toolrun");
    let tail = service.output_port.new_tail().await;
    tail.append_text("live preview before exit");
    service.tool_runs.write().await.insert(
        tool_run_id.clone(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: Some("ses-44e40abbf954168db25d58ff3fc2b17f".into()),
            source_step_id: None,
            state: ToolRunState::Running {
                started_at: "2026-09-24T10:00:00Z".into(),
            },
            kill: None,
            tail: Some(tail),
            command: "echo terminal".into(),
            shell: "test".into(),
        },
    );

    let final_output = "authoritative terminal output";
    service
        .mark_finished(
            &tool_run_id,
            "2026-09-24T10:00:00Z",
            final_output.into(),
            true,
            false,
        )
        .await;

    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["output"],
        final_output
    );
    assert!(
        service.tool_runs.read().await[&tool_run_id].tail.is_none(),
        "terminal commit releases the live tail"
    );
    let finished = events
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _)| name == "tool_run:finished")
        .cloned()
        .expect("terminal snapshot is published once");
    assert_eq!(finished.1["tool_run_id"], tool_run_id);
    assert_eq!(finished.1["output"], final_output);
    assert_eq!(finished.1["status"], "completed");
}

#[tokio::test]
async fn cancellation_drops_live_output_without_projecting_it_to_terminal_state() {
    let service = Arc::new(ToolRunService::new());
    let events = capture_tool_run_events(&service);
    let tool_run_id = haven_common::types::new_id("toolrun");
    let sensitive_preview = "token=must-not-survive-cancel";
    let tail = service.output_port.new_tail().await;
    tail.append_text(sensitive_preview);
    service.tool_runs.write().await.insert(
        tool_run_id.clone(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: Some("ses-afa7085143df5a9c48ccc8eb8e9f02fc".into()),
            source_step_id: None,
            state: ToolRunState::Running {
                started_at: "2026-09-24T10:00:00Z".into(),
            },
            kill: None,
            tail: Some(tail),
            command: "echo token".into(),
            shell: "test".into(),
        },
    );

    service
        .mark_cancelled(&tool_run_id, "2026-09-24T10:00:00Z")
        .await;

    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "cancelled"
    );
    assert!(
        service
            .status_view(&tool_run_id)
            .await
            .to_json(true)
            .get("output")
            .is_none()
    );
    assert!(service.tool_runs.read().await[&tool_run_id].tail.is_none());
    let finished = events
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _)| name == "tool_run:finished")
        .cloned()
        .expect("cancellation publishes a terminal ToolRun event");
    assert!(!finished.1.to_string().contains(sensitive_preview));
    assert!(finished.1.get("output").is_none());
}

#[test]
fn test_terminal_entry_stale_ttl() {
    let now = chrono::Utc::now();
    let entry = |finished: chrono::DateTime<chrono::Utc>, running: bool| ToolRunEntry {
        tool_name: "shell".into(),
        kind: ToolRunKind::Background,
        session_id: None,
        source_step_id: None,
        state: if running {
            ToolRunState::Running {
                started_at: now.to_rfc3339(),
            }
        } else {
            ToolRunState::Completed {
                output: String::new(),
                truncated: false,
                log_path: None,
                started_at: now.to_rfc3339(),
                finished_at: finished.to_rfc3339(),
            }
        },
        kill: None,
        tail: None,
        command: String::new(),
        shell: "cmd".into(),
    };
    assert!(
        terminal_entry_stale(
            &entry(now - chrono::Duration::minutes(20), false),
            Duration::from_secs(600)
        ),
        "20-minute-old terminal ToolRun must be stale"
    );
    assert!(
        !terminal_entry_stale(
            &entry(now - chrono::Duration::minutes(5), false),
            Duration::from_secs(600)
        ),
        "fresh terminal ToolRun must be kept"
    );
    assert!(
        !terminal_entry_stale(&entry(now, true), Duration::from_secs(600)),
        "running ToolRun is never stale"
    );
}

#[tokio::test]
async fn background_status_wait_feedback_carries_durable_provenance() {
    let service = ToolRunService::new();
    service.tool_runs.write().await.insert(
        "toolrun-00000000000000000000000000000019".into(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: Some("ses-8c9a571950f8b821954d297b0b911587".into()),
            source_step_id: Some("step-00000000000000000000000000000006".into()),
            state: ToolRunState::Running {
                started_at: "2026-09-24T10:00:00Z".into(),
            },
            kill: None,
            tail: None,
            command: "echo working".into(),
            shell: "test".into(),
        },
    );

    let status = service
        .status_view("toolrun-00000000000000000000000000000019")
        .await
        .to_json(true);
    assert_eq!(status["kind"], "background");
    assert_eq!(
        status["source_step_id"],
        "step-00000000000000000000000000000006"
    );
    assert_eq!(status["background_wait"]["kind"], "tool_run_result");
    assert_eq!(
        status["background_wait"]["tool_run_ids"],
        json!(["toolrun-00000000000000000000000000000019"])
    );
    assert_eq!(status["background_wait"]["delivery"], "automatic");
}

#[tokio::test]
async fn committed_background_completion_keeps_source_step_identity() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(Database::open(&dir.path().join("source.db")).unwrap());
    db.save_tool_run_with_source(
        "toolrun-00000000000000000000000000000005",
        Some("ses-a30354858b393b4ae5d0f5481880c3a3"),
        "echo committed",
        "2026-09-24T10:00:00Z",
        Some("step-00000000000000000000000000000005"),
    )
    .unwrap();

    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let events = capture_tool_run_events(&service);
    let mut receiver = service.take_tool_run_receiver().unwrap();
    service.tool_runs.write().await.insert(
        "toolrun-00000000000000000000000000000005".into(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: Some("ses-a30354858b393b4ae5d0f5481880c3a3".into()),
            source_step_id: Some("step-00000000000000000000000000000005".into()),
            state: ToolRunState::Running {
                started_at: "2026-09-24T10:00:00Z".into(),
            },
            kill: None,
            tail: None,
            command: "echo committed".into(),
            shell: "test".into(),
        },
    );

    service
        .mark_finished(
            "toolrun-00000000000000000000000000000005",
            "2026-09-24T10:00:00Z",
            "done".into(),
            true,
            false,
        )
        .await;

    let completion = recv_background(&mut receiver).await;
    assert_eq!(
        completion.payload.source_step_id.as_deref(),
        Some("step-00000000000000000000000000000005")
    );
    let finished = events
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _)| name == "tool_run:finished")
        .map(|(_, payload)| payload.clone())
        .expect("committed finish event is published");
    assert_eq!(
        finished["source_step_id"],
        "step-00000000000000000000000000000005"
    );
    assert_eq!(
        db.get_tool_run("toolrun-00000000000000000000000000000005")
            .unwrap()
            .unwrap()
            .source_step_id
            .as_deref(),
        Some("step-00000000000000000000000000000005")
    );
}

#[tokio::test]
async fn session_cleanup_leaves_non_owner_running_and_terminal_history_unchanged() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(Database::open(&dir.path().join("session-cancel.db")).unwrap());
    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let session_id = "ses-cf613f1e85e966dcae4e908f7754b2f9";
    let terminal_id = "toolrun-00000000000000000000000000000017";
    db.save_tool_run(
        terminal_id,
        Some(session_id),
        "echo terminal",
        "2026-09-24T10:00:00Z",
    )
    .unwrap();
    db.finish_tool_run(
        terminal_id,
        ToolRunStatus::Completed,
        Some("completed"),
        None,
        None,
        None,
        "2026-09-24T11:00:00Z",
    )
    .unwrap();
    let terminal_state = TerminalTimestamps::new("2026-09-24T10:00:00Z", "2026-09-24T11:00:00Z")
        .build(TerminalPayload::Completed {
            output: "completed".into(),
            truncated: false,
            log_path: None,
        });
    service.tool_runs.write().await.insert(
        terminal_id.into(),
        session_test_tool_run_entry(
            ToolRunKind::Background,
            Some(session_id),
            terminal_state,
            None,
        ),
    );

    let non_owner_id = "toolrun-00000000000000000000000000000009";
    db.save_tool_run(
        non_owner_id,
        Some("ses-b94f7e9be86c8c5069c9e5dce8b0834b"),
        "echo other",
        "2026-09-24T10:00:00Z",
    )
    .unwrap();
    let (non_owner_kill_tx, mut non_owner_kill_rx) = tokio::sync::oneshot::channel();
    service.tool_runs.write().await.insert(
        non_owner_id.into(),
        session_test_tool_run_entry(
            ToolRunKind::Background,
            Some("ses-b94f7e9be86c8c5069c9e5dce8b0834b"),
            ToolRunState::Running {
                started_at: "2026-09-24T10:00:00Z".into(),
            },
            Some(non_owner_kill_tx),
        ),
    );
    let events = capture_tool_run_events(&service);

    service
        .cancel_owned_background_by_session_checked(session_id)
        .await
        .unwrap();

    assert_eq!(
        db.get_tool_run(terminal_id).unwrap().unwrap().status,
        ToolRunStatus::Completed
    );
    assert_eq!(
        db.get_tool_run(non_owner_id).unwrap().unwrap().status,
        ToolRunStatus::Running
    );
    assert_eq!(
        service.status_view(non_owner_id).await.to_json(true)["status"],
        "running"
    );
    assert!(matches!(
        non_owner_kill_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(terminal_event_count(&events), 0);
}

#[tokio::test]
async fn test_background_registration_rollback_removes_durable_row() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(haven_memory::Database::open(&dir.path().join("test.db")).unwrap());
    let service = Arc::new(ToolRunService::new());
    service
        .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
        .await;
    let tool_run_id = haven_common::types::new_id("toolrun");
    db.save_tool_run(
        &tool_run_id,
        Some("ses-2bc2cb1b18f43fcb787d84f06a06a1e8"),
        "echo rollback",
        "2026-09-19T00:00:00Z",
    )
    .unwrap();
    service.tool_runs.write().await.insert(
        tool_run_id.clone(),
        ToolRunEntry {
            tool_name: "shell".into(),
            kind: ToolRunKind::Background,
            session_id: Some("ses-2bc2cb1b18f43fcb787d84f06a06a1e8".into()),
            source_step_id: None,
            state: ToolRunState::Running {
                started_at: "2026-09-19T00:00:00Z".into(),
            },
            kill: None,
            tail: None,
            command: "echo rollback".into(),
            shell: "cmd".into(),
        },
    );

    service.rollback_background_registration(&tool_run_id).await;

    assert!(db.get_tool_run(&tool_run_id).unwrap().is_none());
    assert_eq!(
        service.status_view(&tool_run_id).await.to_json(true)["status"],
        "not_found"
    );
}
