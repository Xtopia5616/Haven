use super::*;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

fn fixture() -> (Arc<SessionSupervisor>, Arc<Database>) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let supervisor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        Arc::new(ToolsFacade::new()),
        1,
    ));
    (supervisor, db)
}

async fn child_of(supervisor: &Arc<SessionSupervisor>, parent_id: &str) -> String {
    let child = supervisor.create_session("cascade child").await.unwrap();
    supervisor
        .messaging_service
        .register_with_profile(&child.session_id, &[], None, None, Some(parent_id))
        .unwrap();
    supervisor.mark_has_children(parent_id).await;
    child.session_id
}

fn reject_completion(db: &Database, session_id: &str) {
    assert!(haven_common::types::is_canonical_id(session_id, "ses"));
    db.conn()
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER reject_child_completion
             BEFORE UPDATE OF status ON sessions
             WHEN NEW.id = '{session_id}' AND NEW.status = 'completed'
             BEGIN SELECT RAISE(ABORT, 'injected child completion failure'); END;"
        ))
        .unwrap();
}

fn allow_completion(db: &Database) {
    db.conn()
        .unwrap()
        .execute_batch("DROP TRIGGER reject_child_completion;")
        .unwrap();
}

#[derive(Default)]
struct CountingOverlay {
    calls: AtomicUsize,
    block_once: AtomicBool,
    entered: Notify,
}

#[async_trait::async_trait]
impl SessionToolOverlayPort for CountingOverlay {
    async fn unregister_session(&self, _: &str) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.block_once.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            std::future::pending::<()>().await;
        }
    }

    async fn register_mcp_for_session(&self, _: &str, _: &str, _: Option<&[String]>) -> bool {
        false
    }

    async fn load_skill_for_session(&self, _: &str, _: Vec<String>) -> bool {
        false
    }

    async fn load_builtin_operations_for_session(
        &self,
        _: &str,
        _: Option<Vec<String>>,
        _: Option<Vec<String>>,
    ) -> bool {
        false
    }
}

fn counting_fixture() -> (Arc<SessionSupervisor>, Arc<Database>, Arc<CountingOverlay>) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let overlay = Arc::new(CountingOverlay::default());
    let mut supervisor =
        SessionSupervisor::new_for_test(db.clone(), Arc::new(ToolsFacade::new()), 1);
    supervisor.session_tool_overlay_port = overlay.clone();
    (Arc::new(supervisor), db, overlay)
}

async fn wait_for_removed(supervisor: &SessionSupervisor, session_id: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while supervisor.actor_for(session_id).await.is_some() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("cleanup must complete after the transient fault is removed");
}

#[tokio::test]
async fn repeated_parent_end_retries_child_after_transient_completion_failure() {
    let (supervisor, db) = fixture();
    let parent = supervisor.create_session("cascade parent").await.unwrap();
    let child = supervisor.create_session("cascade child").await.unwrap();
    supervisor
        .messaging_service
        .register(&parent.session_id, &[])
        .unwrap();
    supervisor
        .messaging_service
        .register_with_profile(&child.session_id, &[], None, None, Some(&parent.session_id))
        .unwrap();
    supervisor.mark_has_children(&parent.session_id).await;

    reject_completion(&db, &child.session_id);

    let first_result = supervisor.end_session(&parent.session_id).await;
    assert_eq!(first_result.as_ref().unwrap(), &SessionStatus::Completed);
    assert_eq!(
        db.get_session(&child.session_id).unwrap().unwrap().status,
        SessionStatus::Paused,
        "the injected failure must prevent the child's terminal commit"
    );
    allow_completion(&db);

    let retry_result = supervisor.end_session(&parent.session_id).await;
    assert!(retry_result.is_ok(), "repeated parent End must be accepted");
    assert_eq!(
        db.get_session(&child.session_id).unwrap().unwrap().status,
        SessionStatus::Completed,
        "retry must finish the child after the transient failure; first End: {first_result:?}, retry: {retry_result:?}, parent actor retained: {}, queued retries: {}",
        supervisor.actor_for(&parent.session_id).await.is_some(),
        supervisor.terminal_cleanup_retry_queue.pending_count(),
    );
}

#[tokio::test]
async fn cascade_worker_retries_failed_child_without_another_parent_end() {
    let (supervisor, db) = fixture();
    let parent = supervisor.create_session("worker parent").await.unwrap();
    let child_id = child_of(&supervisor, &parent.session_id).await;
    reject_completion(&db, &child_id);
    assert_eq!(
        supervisor.end_session(&parent.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    assert!(supervisor.actor_for(&parent.session_id).await.is_some());
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Paused
    );
    allow_completion(&db);

    let final_drain = CancellationToken::new();
    final_drain.cancel();
    supervisor
        .clone()
        .run_terminal_cleanup_worker(final_drain)
        .await;
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
    assert!(supervisor.actor_for(&parent.session_id).await.is_none());
}

#[tokio::test]
async fn cascade_failure_does_not_skip_healthy_siblings() {
    let (supervisor, db) = fixture();
    let parent = supervisor.create_session("siblings parent").await.unwrap();
    let failed_child = child_of(&supervisor, &parent.session_id).await;
    let healthy_child = child_of(&supervisor, &parent.session_id).await;
    reject_completion(&db, &failed_child);
    assert_eq!(
        supervisor.end_session(&parent.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    assert_eq!(
        db.get_session(&healthy_child).unwrap().unwrap().status,
        SessionStatus::Completed,
        "a failing sibling must not prevent other descendants from ending"
    );
    assert_eq!(
        db.get_session(&failed_child).unwrap().unwrap().status,
        SessionStatus::Paused
    );
    allow_completion(&db);
    supervisor.end_session(&parent.session_id).await.unwrap();
    assert_eq!(
        db.get_session(&failed_child).unwrap().unwrap().status,
        SessionStatus::Completed
    );
}

#[tokio::test]
async fn cascade_retry_worker_preserves_explicit_false_policy_with_children() {
    let (supervisor, db) = fixture();
    let parent = supervisor
        .create_session("no cascade parent")
        .await
        .unwrap();
    let child_id = child_of(&supervisor, &parent.session_id).await;
    supervisor
        .actor_for(&parent.session_id)
        .await
        .unwrap()
        .transition(SessionStatus::Completed, true)
        .await
        .unwrap();
    supervisor.terminal_cleanup_retry_queue.enqueue(
        &parent.session_id,
        TerminalCleanupRetry {
            cascade: Some(false),
            remove_error_actor: true,
        },
    );
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    supervisor
        .clone()
        .run_terminal_cleanup_worker(final_drain)
        .await;
    assert!(supervisor.actor_for(&parent.session_id).await.is_none());
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Pending,
        "explicit cascade=false must survive retry processing"
    );
}

#[tokio::test]
async fn cascade_normal_worker_backs_off_failure_and_recovers() {
    let (supervisor, db, overlay) = counting_fixture();
    let parent = supervisor.create_session("backoff parent").await.unwrap();
    let child_id = child_of(&supervisor, &parent.session_id).await;
    reject_completion(&db, &child_id);
    assert_eq!(
        supervisor.end_session(&parent.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    assert_eq!(overlay.calls.load(Ordering::SeqCst), 1);
    let final_drain = CancellationToken::new();
    let worker = tokio::spawn(
        supervisor
            .clone()
            .run_terminal_cleanup_worker(final_drain.clone()),
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        while overlay.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        overlay.calls.load(Ordering::SeqCst),
        2,
        "ordinary cascade failure must back off instead of hot-looping"
    );
    assert!(supervisor.actor_for(&parent.session_id).await.is_some());
    allow_completion(&db);
    wait_for_removed(&supervisor, &parent.session_id).await;
    final_drain.cancel();
    worker.await.unwrap();
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
}

#[tokio::test]
async fn cascade_failed_final_drain_retains_actor_hint_and_retry_policy() {
    let (supervisor, db) = fixture();
    let parent = supervisor
        .create_session("final failure parent")
        .await
        .unwrap();
    let child_id = child_of(&supervisor, &parent.session_id).await;
    reject_completion(&db, &child_id);
    assert_eq!(
        supervisor.end_session(&parent.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    supervisor
        .clone()
        .run_terminal_cleanup_worker(final_drain)
        .await;
    let actor = supervisor.actor_for(&parent.session_id).await.unwrap();
    assert!(actor.has_children().await);
    assert_eq!(
        supervisor
            .terminal_cleanup_retry_queue
            .pending_retry(&parent.session_id),
        Some(TerminalCleanupRetry {
            cascade: Some(true),
            remove_error_actor: true
        })
    );
    assert_eq!(
        db.get_session(&parent.session_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
    allow_completion(&db);
}

#[tokio::test]
async fn cascade_worker_cancellation_during_cleanup_retains_retry_for_next_worker() {
    let (supervisor, db, overlay) = counting_fixture();
    let parent = supervisor.create_session("cancel parent").await.unwrap();
    let child_id = child_of(&supervisor, &parent.session_id).await;
    reject_completion(&db, &child_id);
    assert_eq!(
        supervisor.end_session(&parent.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    overlay.block_once.store(true, Ordering::SeqCst);
    let final_drain = CancellationToken::new();
    let worker = tokio::spawn(
        supervisor
            .clone()
            .run_terminal_cleanup_worker(final_drain.clone()),
    );
    tokio::time::timeout(Duration::from_secs(2), overlay.entered.notified())
        .await
        .unwrap();
    final_drain.cancel();
    tokio::time::timeout(Duration::from_secs(2), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(
        supervisor
            .actor_for(&parent.session_id)
            .await
            .unwrap()
            .has_children()
            .await
    );
    assert_eq!(
        supervisor
            .terminal_cleanup_retry_queue
            .pending_retry(&parent.session_id),
        Some(TerminalCleanupRetry {
            cascade: Some(true),
            remove_error_actor: true
        })
    );
    allow_completion(&db);
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    supervisor
        .clone()
        .run_terminal_cleanup_worker(final_drain)
        .await;
    assert!(supervisor.actor_for(&parent.session_id).await.is_none());
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
}

#[tokio::test]
async fn cascade_descendant_enumeration_failure_retains_work_until_registry_recovers() {
    let (supervisor, db) = fixture();
    let parent = supervisor
        .create_session("registry failure parent")
        .await
        .unwrap();
    let child_id = child_of(&supervisor, &parent.session_id).await;
    let registry_path = supervisor
        .messaging_root
        .as_ref()
        .unwrap()
        .path()
        .join("agents.json");
    let registry = std::fs::read(&registry_path).unwrap();
    std::fs::write(&registry_path, b"injected invalid registry JSON").unwrap();
    assert_eq!(
        supervisor.end_session(&parent.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    assert!(
        supervisor
            .actor_for(&parent.session_id)
            .await
            .unwrap()
            .has_children()
            .await
    );
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Pending
    );
    assert_eq!(
        supervisor
            .terminal_cleanup_retry_queue
            .pending_retry(&parent.session_id),
        Some(TerminalCleanupRetry {
            cascade: Some(true),
            remove_error_actor: true
        })
    );
    std::fs::write(&registry_path, registry).unwrap();
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    supervisor
        .clone()
        .run_terminal_cleanup_worker(final_drain)
        .await;
    assert!(supervisor.actor_for(&parent.session_id).await.is_none());
    assert_eq!(
        db.get_session(&child_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
}
