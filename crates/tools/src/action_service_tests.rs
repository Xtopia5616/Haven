use crate::process::{append_tail, read_stream_capped, take_tail_if_changed};

use super::*;
use std::time::Duration;

/// Poll `status` until it is no longer "running" (or timeout).
async fn wait_terminal(actions: &ActionService, id: &str, timeout_secs: u64) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let v = actions.status(id).await;
        if v["status"] != "running" || std::time::Instant::now() > deadline {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn recv_background(rx: &mut ActionCompletionReceiver) -> BackgroundActionCompletion {
    loop {
        match rx.recv().await {
            Some(ActionCompletion::Background(completion)) => return completion,
            Some(ActionCompletion::Scheduled(_)) => continue,
            None => panic!("action completion channel closed"),
        }
    }
}
/// Spawn the two fixture echo actions (`action-a` / `action-b`) and attach them to
/// `ses-1` / `ses-2`. Shared by the board and scoped-list tests.
async fn spawn_two_echo_actions(actions: &Arc<ActionService>) -> (String, String) {
    let id_a = actions
        .spawn_shell("echo action-a", "cmd", 20_000, None)
        .await
        .unwrap();
    let id_b = actions
        .spawn_shell("echo action-b", "cmd", 20_000, None)
        .await
        .unwrap();
    actions.attach_session(&id_a, "ses-1").await;
    actions.attach_session(&id_b, "ses-2").await;
    (id_a, id_b)
}

#[cfg(windows)]
#[tokio::test]
async fn test_completion_notified_on_finish() {
    let actions = Arc::new(ActionService::new());
    let mut rx = actions.take_action_receiver().expect("receiver available");
    // Attach the session BEFORE the action finishes (normal path): the
    // completion must carry the session_id.
    let id = actions
        .spawn_shell("echo done", "cmd", 20_000, None)
        .await
        .unwrap();
    actions.attach_session(&id, "ses-A").await;
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "completed");
    let comp = tokio::time::timeout(Duration::from_secs(2), recv_background(&mut rx))
        .await
        .expect("completion received");
    assert_eq!(comp.action_id, id);
    assert_eq!(comp.status, haven_common::ActionStatus::Completed);
    assert_eq!(comp.session_id.as_deref(), Some("ses-A"));
    assert!(
        comp.status_json["output"]
            .as_str()
            .unwrap()
            .contains("done")
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_for_session_binds_owner_before_completion() {
    let actions = Arc::new(ActionService::new());
    let mut rx = actions.take_action_receiver().expect("receiver available");
    let id = actions
        .spawn_shell_for_session("echo prebound", "cmd", 20_000, None, Some("ses-owner"))
        .await
        .unwrap();

    let completion = tokio::time::timeout(Duration::from_secs(10), recv_background(&mut rx))
        .await
        .expect("completion received");
    assert_eq!(completion.action_id, id);
    assert_eq!(completion.session_id.as_deref(), Some("ses-owner"));
    assert_eq!(
        actions.status_for_session(&id, "ses-owner").await["status"],
        "completed"
    );
    actions.attach_session(&id, "ses-owner").await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), recv_background(&mut rx))
            .await
            .is_err()
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_action_result_persisted_to_db() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let db = Arc::new(Database::open(&dir.path().join("test.db")).expect("temp db"));
    let actions = Arc::new(ActionService::new());
    actions.set_db(Some(db.clone())).await;

    let id = actions
        .spawn_shell(
            "echo live-line & ping -n 4 127.0.0.1 >nul",
            "cmd",
            20_000,
            None,
        )
        .await
        .unwrap();
    actions.attach_session(&id, "ses-DB").await;
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "completed");

    // The status flips to completed before the terminal row is persisted
    // (mark_finished → notify_completion → persist_terminal); poll the
    // DB instead of reading it immediately.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let row = loop {
        let rows = db.list_actions(Some("background")).unwrap();
        if let Some(row) = rows
            .iter()
            .find(|r| r.id == id && r.status == haven_common::ActionStatus::Completed)
        {
            break row.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "action row never persisted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(row.kind, "background");
    assert_eq!(row.status, haven_common::ActionStatus::Completed);
    assert_eq!(row.session_id.as_deref(), Some("ses-DB"));
    assert!(row.output.as_deref().unwrap().contains("live-line"));
    assert_eq!(row.exit_code, Some(0));
    assert!(row.finished_at.is_some());
}

#[cfg(windows)]
#[tokio::test]
async fn test_completion_refired_after_late_attach() {
    // Race path: the action finishes before attach_session is called. The
    // completion first fires with session_id=None; attach_session must re-fire
    // with the session_id so the owning session still gets notified.
    let actions = Arc::new(ActionService::new());
    let mut rx = actions.take_action_receiver().expect("receiver available");
    let id = actions
        .spawn_shell("echo fast", "cmd", 20_000, None)
        .await
        .unwrap();
    // Wait for the action to finish BEFORE attaching (simulate the race).
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "completed");
    // Drain the session_id=None completion fired by mark_finished.
    let none_comp = recv_background(&mut rx).await;
    assert!(none_comp.session_id.is_none());
    // Now attach: should re-fire with the session_id.
    actions.attach_session(&id, "ses-B").await;
    let comp = tokio::time::timeout(Duration::from_secs(2), recv_background(&mut rx))
        .await
        .expect("refired completion received");
    assert_eq!(comp.session_id.as_deref(), Some("ses-B"));
    assert_eq!(comp.status, haven_common::ActionStatus::Completed);
}

#[tokio::test]
async fn test_completion_skipped_for_running() {
    let actions = Arc::new(ActionService::new());
    // No actions → no completion. Just confirm the receiver is taken.
    let _rx = actions.take_action_receiver().expect("receiver available");
    // status on not_found doesn't notify.
    assert_eq!(actions.status("nope").await["status"], "not_found");
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_completes_with_output() {
    let actions = Arc::new(ActionService::new());
    let id = actions
        .spawn_shell("echo bg-hello", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "completed", "got: {}", v);
    assert!(v["output"].as_str().unwrap().contains("bg-hello"));
    assert!(v["finished_at"].as_str().is_some());
}

#[cfg(windows)]
#[tokio::test]
async fn test_running_status_includes_command_and_live_output() {
    let actions = Arc::new(ActionService::new());
    let id = actions
        .spawn_shell(
            "echo live-line & ping -n 3 127.0.0.1 >nul",
            "cmd",
            20_000,
            None,
        )
        .await
        .unwrap();
    // While the action runs, status must carry the command line it executes.
    let v = actions.status(&id).await;
    assert_eq!(v["status"], "running", "got: {}", v);
    assert_eq!(v["shell"], "cmd");
    assert!(
        v["command"].as_str().unwrap().contains("live-line"),
        "running status must include the command: {v}"
    );
    // And the live output tail once the command has produced something.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let v = actions.status(&id).await;
        if v["output"].as_str().unwrap_or("").contains("live-line") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "live output never arrived: {v}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The running row of the board carries the same command + output.
    let board = actions.board().await;
    let row = board
        .iter()
        .find(|r| r["action_id"] == id)
        .expect("on board");
    assert!(row["command"].as_str().unwrap().contains("live-line"));
    assert!(row["preview"].as_str().unwrap_or("").contains("live-line"));
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_failure_reported() {
    let actions = Arc::new(ActionService::new());
    let id = actions
        .spawn_shell("exit 7", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "failed", "got: {}", v);
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_stderr_captured() {
    let actions = Arc::new(ActionService::new());
    let id = actions
        .spawn_shell("echo err-msg 1>&2", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "completed", "got: {}", v);
    assert!(v["output"].as_str().unwrap().contains("err-msg"));
}

#[cfg(windows)]
#[tokio::test]
async fn test_spawn_shell_cancelled() {
    let actions = Arc::new(ActionService::new());
    let id = actions
        .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
        .await
        .unwrap();
    assert_eq!(actions.status(&id).await["status"], "running");
    assert!(actions.cancel(&id).await, "cancel must report success");
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "cancelled", "got: {}", v);
}

#[cfg(windows)]
#[tokio::test]
async fn test_cancel_for_session_cleans_up() {
    let actions = Arc::new(ActionService::new());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = events.clone();
    actions.set_event_sink(Arc::new(move |name, payload| {
        sink_events.lock().unwrap().push((name, payload));
    }));
    let id = actions
        .spawn_shell("ping -n 30 127.0.0.1", "cmd", 20_000, None)
        .await
        .unwrap();
    actions.attach_session(&id, "ses-1").await;
    assert_eq!(actions.status(&id).await["status"], "running");
    actions.cancel_owned_by_session("ses-1").await;
    assert_eq!(actions.status(&id).await["status"], "not_found");
    let evs = events.lock().unwrap();
    let finished = evs
        .iter()
        .find(|(n, _)| n == "action:finished")
        .expect("cancel_for_session must emit action:finished so the UI drops the ghost");
    assert_eq!(finished.1["action_id"], id);
    assert_eq!(finished.1["status"], "cancelled");
}

#[tokio::test]
async fn test_status_not_found() {
    let actions = Arc::new(ActionService::new());
    assert_eq!(actions.status("action-nope").await["status"], "not_found");
}

#[tokio::test]
async fn test_cancel_unknown_action() {
    let actions = Arc::new(ActionService::new());
    assert!(!actions.cancel("action-nope").await);
}

#[tokio::test]
async fn test_spawn_empty_command_rejected() {
    let actions = Arc::new(ActionService::new());
    assert!(
        actions
            .spawn_shell("  ", "cmd", 20_000, None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn test_read_stream_capped_under_cap() {
    let (text, overflowed) = read_stream_capped(Some(&b"hello"[..]), 8192, None, 2000).await;
    assert_eq!(text, "hello");
    assert!(!overflowed);
}

#[tokio::test]
async fn test_read_stream_capped_none() {
    let (text, overflowed) = read_stream_capped::<&[u8]>(None, 8192, None, 2000).await;
    assert_eq!(text, "");
    assert!(!overflowed);
}

#[tokio::test]
async fn test_read_stream_capped_over_cap() {
    let data = vec![b'x'; 1000];
    let (text, overflowed) = read_stream_capped(Some(&data[..]), 100, None, 2000).await;
    assert_eq!(text.len(), 100);
    assert!(overflowed);
}

#[tokio::test]
async fn test_read_stream_capped_appends_tail() {
    let tail = Arc::new(Mutex::new(String::new()));
    let (text, _) =
        read_stream_capped(Some(&b"hello tail"[..]), 8192, Some(tail.clone()), 2000).await;
    assert_eq!(text, "hello tail");
    assert_eq!(*tail.lock().unwrap(), "hello tail");
    // A second chunk appends (multi-chunk tee).
    read_stream_capped(Some(&b" more"[..]), 8192, Some(tail.clone()), 2000).await;
    assert_eq!(*tail.lock().unwrap(), "hello tail more");
}

#[tokio::test]
async fn test_read_stream_capped_tail_carries_split_multibyte() {
    // 8191 ASCII + a 3-byte UTF-8 char: the first 8192-byte read splits the
    // char (lead byte only), the second read finishes it. The live tail must
    // still show the char intact, not GBK-fallback mojibake.
    let tail = Arc::new(Mutex::new(String::new()));
    let mut content = "a".repeat(8191);
    content.push('中');
    read_stream_capped(Some(content.as_bytes()), 10_000, Some(tail.clone()), 2000).await;
    let t = tail.lock().unwrap();
    assert!(
        t.ends_with('中'),
        "tail must keep the split char intact, got: {:?}",
        &t[t.len().saturating_sub(40)..]
    );
    assert!(!t.contains('\u{FFFD}'), "no replacement chars in tail");
}

#[test]
fn test_append_tail_bounded() {
    let tail = Mutex::new(String::new());
    // A single oversized chunk is truncated to the last max chars.
    let max_chars = 2000usize;
    let big = "x".repeat(max_chars + 500);
    append_tail(&tail, big.as_bytes(), max_chars);
    assert_eq!(tail.lock().unwrap().len(), max_chars);
    // Subsequent chunks drop the front.
    append_tail(&tail, "tail-end".as_bytes(), max_chars);
    let t = tail.lock().unwrap();
    assert!(
        t.ends_with("tail-end"),
        "got tail: {}",
        &t[t.len().saturating_sub(40)..]
    );
}

#[test]
fn test_take_tail_if_changed_detects_sliding_window() {
    let tail = Mutex::new("a".repeat(100));
    let mut last = String::new();
    assert!(take_tail_if_changed(&tail, &mut last));
    assert_eq!(last.len(), 100);
    assert!(!take_tail_if_changed(&tail, &mut last));
    // Same length, different content (capped-window slide).
    *tail.lock().unwrap() = "b".repeat(100);
    assert!(take_tail_if_changed(&tail, &mut last));
    assert_eq!(last, "b".repeat(100));
}

#[test]
fn test_terminal_entry_stale_ttl() {
    let now = chrono::Utc::now();
    let entry = |finished: chrono::DateTime<chrono::Utc>, running: bool| ActionEntry {
        kind: ActionKind::Background,
        session_id: None,
        state: if running {
            ActionState::Running {
                started_at: now.to_rfc3339(),
            }
        } else {
            ActionState::Completed {
                output: String::new(),
                exit_code: None,
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
        scheduled: None,
    };
    assert!(
        terminal_entry_stale(
            &entry(now - chrono::Duration::minutes(20), false),
            Duration::from_secs(600)
        ),
        "20-minute-old terminal action must be stale"
    );
    assert!(
        !terminal_entry_stale(
            &entry(now - chrono::Duration::minutes(5), false),
            Duration::from_secs(600)
        ),
        "fresh terminal action must be kept"
    );
    assert!(
        !terminal_entry_stale(&entry(now, true), Duration::from_secs(600)),
        "running action is never stale"
    );
}

// ── list_for_session (actions board) ────────────────────────────────────────

#[cfg(windows)]
#[tokio::test]
async fn test_event_sink_receives_lifecycle() {
    let actions = Arc::new(ActionService::new());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = events.clone();
    actions.set_event_sink(Arc::new(move |name, payload| {
        sink_events.lock().unwrap().push((name, payload));
    }));
    let id = actions
        .spawn_shell("echo bg-event", "cmd", 20_000, None)
        .await
        .unwrap();
    actions.attach_session(&id, "ses-evt").await;
    wait_terminal(&actions, &id, 10).await;

    let evs = events.lock().unwrap();
    let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
    assert!(names.contains(&"action:created"), "got: {names:?}");
    assert!(names.contains(&"action:updated"), "got: {names:?}");
    assert!(names.contains(&"action:finished"), "got: {names:?}");
    let created = evs
        .iter()
        .find(|(n, _)| n == "action:created")
        .expect("created event");
    assert_eq!(created.1["action_id"], id);
    assert_eq!(created.1["status"], "running");
    assert_eq!(created.1["kind"], "background");
    let term = evs
        .iter()
        .find(|(n, _)| n == "action:finished")
        .expect("terminal event");
    assert_eq!(term.1["status"], "completed");
    assert_eq!(term.1["action_id"], id);
}

#[cfg(windows)]
#[tokio::test]
async fn test_board_lists_all_jobs_with_session() {
    let actions = Arc::new(ActionService::new());
    let (id_a, id_b) = spawn_two_echo_actions(&actions).await;
    wait_terminal(&actions, &id_a, 10).await;
    wait_terminal(&actions, &id_b, 10).await;

    let rows = actions.board().await;
    assert_eq!(rows.len(), 2, "all actions on board: {rows:?}");
    let by_id: HashMap<_, _> = rows
        .iter()
        .map(|r| (r["action_id"].as_str().unwrap(), r))
        .collect();
    assert_eq!(by_id[&id_a.as_str()]["session_id"], "ses-1");
    assert_eq!(by_id[&id_b.as_str()]["session_id"], "ses-2");
    assert_eq!(by_id[&id_a.as_str()]["status"], "completed");
    assert!(
        by_id[&id_a.as_str()]["preview"]
            .as_str()
            .unwrap()
            .contains("action-a"),
        "preview expected, got: {rows:?}"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn test_action_output_preview_emitted() {
    let actions = Arc::new(ActionService::new());
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = events.clone();
    actions.set_event_sink(Arc::new(move |name, payload| {
        sink_events.lock().unwrap().push((name, payload));
    }));
    // A action that keeps running past one emit interval while producing
    // output (ping lasts ~3s), so the preview event has time to fire.
    let id = actions
        .spawn_shell(
            "echo preview-line-123 && ping -n 4 127.0.0.1 > nul",
            "cmd",
            20_000,
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(2500)).await;
    {
        let evs = events.lock().unwrap();
        let output_evt = evs
            .iter()
            .find(|(n, _)| n == "action:output")
            .expect("action:output event must be emitted while running");
        assert_eq!(output_evt.1["action_id"], id);
        assert!(
            output_evt.1["output"]
                .as_str()
                .unwrap()
                .contains("preview-line-123"),
            "preview must carry the echoed line, got: {:?}",
            output_evt.1["output"]
        );
    }
    let _ = actions.cancel(&id).await;
}

#[cfg(windows)]
#[tokio::test]
async fn test_list_for_session_scopes_to_owning_session() {
    let actions = Arc::new(ActionService::new());
    let (id_a, id_b) = spawn_two_echo_actions(&actions).await;
    wait_terminal(&actions, &id_a, 10).await;
    wait_terminal(&actions, &id_b, 10).await;

    let rows = actions.list_for_session("ses-1").await;
    assert_eq!(rows.len(), 1, "only ses-1's actions: {rows:?}");
    assert_eq!(rows[0]["action_id"], id_a);
    assert_eq!(rows[0]["status"], "completed");
    assert!(
        rows[0]["preview"].as_str().unwrap().contains("action-a"),
        "preview expected, got: {rows:?}"
    );

    let all = actions.list_for_session("ses-2").await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["action_id"], id_b);
}

#[cfg(windows)]
#[tokio::test]
async fn test_failed_action_reports_exit_code_and_reason() {
    let actions = Arc::new(ActionService::new());
    let id = actions
        .spawn_shell("echo progress... && exit 42", "cmd", 20_000, None)
        .await
        .unwrap();
    let v = wait_terminal(&actions, &id, 10).await;
    assert_eq!(v["status"], "failed", "got: {v}");
    assert_eq!(v["exit_code"], 42, "exit code must be captured, got: {v}");
    assert!(
        v["error_reason"].as_str().is_some_and(|s| !s.is_empty()),
        "error_reason must be present, got: {v}"
    );
}

#[tokio::test]
async fn test_unified_service_owns_scheduled_state_and_cancel() {
    let service = Arc::new(ActionService::new());
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(3600),
            watch_action_id: None,
            title: "Unified".into(),
            body: "still waiting".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Continue,
            session_id: Some("ses-unified".into()),
            tool_name: None,
            tool_args: None,
            prompt: Some("continue later".into()),
        })
        .await
        .unwrap();

    let board = service.board().await;
    assert_eq!(board.len(), 1);
    assert_eq!(board[0]["action_id"], id);
    assert_eq!(board[0]["kind"], "scheduled");
    assert_eq!(board[0]["status"], "waiting");
    assert_eq!(
        service.status_for_session(&id, "ses-unified").await["status"],
        "waiting"
    );

    assert!(!service.cancel_for_session(&id, "ses-other").await);
    assert!(service.cancel_for_session(&id, "ses-unified").await);
    assert!(service.board().await.is_empty());
    assert_eq!(service.status(&id).await["status"], "cancelled");
}

#[tokio::test]
async fn test_unified_completion_bus_emits_scheduled_transition() {
    let service = Arc::new(ActionService::new());
    let mut rx = service
        .take_action_receiver()
        .expect("unified receiver available");
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(1),
            watch_action_id: None,
            title: "Bus".into(),
            body: "fire".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        })
        .await
        .unwrap();

    let event = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("scheduled event received")
        .expect("unified bus open");
    match event {
        ActionCompletion::Scheduled(fired) => assert_eq!(fired.action_id, id),
        ActionCompletion::Background(_) => panic!("scheduled fire used the background variant"),
    }
    assert_eq!(service.status(&id).await["status"], "running");
    service.complete_scheduled(&id).await.unwrap();
    assert_eq!(service.status(&id).await["status"], "completed");
}

#[tokio::test]
async fn test_scheduled_fire_without_receiver_is_requeued_durably() {
    let (db, _dir) = {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Arc::new(haven_memory::Database::open(&dir.path().join("test.db")).unwrap());
        (db, dir)
    };
    let service = Arc::new(ActionService::new());
    service.set_db(Some(db.clone())).await;
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(1),
            watch_action_id: None,
            title: "No receiver".into(),
            body: "keep waiting".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Continue,
            session_id: None,
            tool_name: None,
            tool_args: None,
            prompt: Some("keep waiting".into()),
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;

    assert_eq!(service.status(&id).await["status"], "waiting");
    let pending = db.list_pending_scheduled_actions().unwrap();
    assert_eq!(pending.iter().filter(|row| row.id == id).count(), 1);
    assert_eq!(
        pending.iter().find(|row| row.id == id).unwrap().status,
        haven_common::ActionStatus::Waiting
    );
}

#[tokio::test]
async fn test_scheduled_fire_recovers_after_completion_bus_lag() {
    let service = Arc::new(ActionService::new());
    let mut rx = service.take_action_receiver().expect("receiver available");
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(1),
            watch_action_id: None,
            title: "Lag recovery".into(),
            body: "replay me".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(2)).await;
    for index in 0..=256 {
        let _ =
            service
                .completion_tx
                .send(ActionCompletion::Background(BackgroundActionCompletion {
                    action_id: format!("act-noise-{index}"),
                    session_id: None,
                    status: haven_common::ActionStatus::Completed,
                    status_json: serde_json::json!({"status": "completed"}),
                }));
    }

    let event = tokio::time::timeout(
        Duration::from_secs(2),
        rx.recv_scheduled_with_recovery(service.as_ref()),
    )
    .await
    .expect("lagged scheduled trigger must be recovered")
    .expect("completion bus open");
    match event {
        ActionCompletion::Scheduled(fired) => assert_eq!(fired.action_id, id),
        ActionCompletion::Background(_) => panic!("lag recovery returned a background event"),
    }
    assert_eq!(service.status(&id).await["status"], "running");
    service.complete_scheduled(&id).await.unwrap();
    assert_eq!(service.status(&id).await["status"], "completed");
}

#[tokio::test]
async fn test_scheduled_terminal_event_reuses_persisted_timestamps() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(haven_memory::Database::open(&dir.path().join("test.db")).unwrap());
    let service = Arc::new(ActionService::new());
    service.set_db(Some(db.clone())).await;
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_events = events.clone();
    service.set_event_sink(Arc::new(move |name, payload| {
        sink_events.lock().unwrap().push((name, payload));
    }));
    let mut rx = service.take_action_receiver().expect("receiver available");
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(1),
            watch_action_id: None,
            title: "Timestamp".into(),
            body: "same clock".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        })
        .await
        .unwrap();
    let fired = tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(fired, ActionCompletion::Scheduled(_)));
    service.complete_scheduled(&id).await.unwrap();

    let event = events
        .lock()
        .unwrap()
        .iter()
        .find(|(name, payload)| name == "action:finished" && payload["id"] == id)
        .map(|(_, payload)| payload.clone())
        .expect("scheduled completion event");
    let row = db.get_action(&id).unwrap().expect("scheduled history row");
    assert_eq!(event["started_at"].as_str(), row.started_at.as_deref());
    assert_eq!(event["finished_at"].as_str(), row.finished_at.as_deref());

    let cancel_id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(3600),
            watch_action_id: None,
            title: "Cancel timestamp".into(),
            body: "same clock".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        })
        .await
        .unwrap();
    assert!(service.cancel(&cancel_id).await);
    let cancel_event = events
        .lock()
        .unwrap()
        .iter()
        .find(|(name, payload)| name == "action:finished" && payload["id"] == cancel_id)
        .map(|(_, payload)| payload.clone())
        .expect("scheduled cancellation event");
    let cancel_row = db
        .get_action(&cancel_id)
        .unwrap()
        .expect("cancelled scheduled history row");
    assert_eq!(
        cancel_event["started_at"].as_str(),
        cancel_row.started_at.as_deref()
    );
    assert_eq!(
        cancel_event["finished_at"].as_str(),
        cancel_row.finished_at.as_deref()
    );
}

#[tokio::test]
async fn test_action_kind_and_terminal_delete_guards() {
    let service = Arc::new(ActionService::new());
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(3600),
            watch_action_id: None,
            title: "Delete guard".into(),
            body: "pending".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: None,
            prompt: None,
        })
        .await
        .unwrap();

    assert!(!service.cancel_for_kind(&id, "background").await);
    assert!(!service.cancel_for_kind(&id, "invalid").await);
    assert!(!service.delete(&id, "scheduled").await.unwrap());
    assert!(service.cancel_for_kind(&id, "scheduled").await);
    assert!(service.delete_terminal(&id).await.unwrap());
    assert_eq!(service.status(&id).await["status"], "not_found");
}

#[tokio::test]
async fn test_background_registration_rollback_removes_durable_row() {
    let dir = tempfile::TempDir::new().unwrap();
    let db = Arc::new(haven_memory::Database::open(&dir.path().join("test.db")).unwrap());
    let service = Arc::new(ActionService::new());
    service.set_db(Some(db.clone())).await;
    let id = haven_common::types::new_id("act");
    db.save_action(
        &id,
        Some("ses-rollback"),
        "echo rollback",
        "2026-09-19T00:00:00Z",
    )
    .unwrap();
    service.actions.write().await.insert(
        id.clone(),
        ActionEntry {
            kind: ActionKind::Background,
            session_id: Some("ses-rollback".into()),
            state: ActionState::Running {
                started_at: "2026-09-19T00:00:00Z".into(),
            },
            kill: None,
            tail: None,
            command: "echo rollback".into(),
            shell: "cmd".into(),
            scheduled: None,
        },
    );

    service.rollback_background_registration(&id).await;

    assert!(db.get_action(&id).unwrap().is_none());
    assert_eq!(service.status(&id).await["status"], "not_found");
}

#[tokio::test]
async fn test_shutdown_stops_scheduled_timers_and_rejects_new_work() {
    let service = Arc::new(ActionService::new());
    let mut rx = service
        .take_action_receiver()
        .expect("unified receiver available");
    let id = service
        .set(crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at: None,
            delay_secs: Some(1),
            watch_action_id: None,
            title: "Shutdown".into(),
            body: "must not fire after teardown".into(),
            mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
            session_id: None,
            tool_name: Some("notify".into()),
            tool_args: Some(serde_json::json!({})),
            prompt: None,
        })
        .await
        .unwrap();

    service.shutdown().await;
    service.shutdown().await;

    assert_eq!(service.status(&id).await["status"], "waiting");
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), rx.recv())
            .await
            .is_err(),
        "scheduled timer must not fire after action-service shutdown"
    );
    assert!(
        service
            .set(crate::builtin::scheduled_action::ScheduledActionSpec {
                due_at: None,
                delay_secs: Some(1),
                watch_action_id: None,
                title: "Rejected".into(),
                body: "not admitted".into(),
                mode: crate::builtin::scheduled_action::ScheduleMode::Tool,
                session_id: None,
                tool_name: Some("notify".into()),
                tool_args: Some(serde_json::json!({})),
                prompt: None,
            })
            .await
            .is_err(),
        "new scheduled work must be rejected after shutdown"
    );
}
