use super::*;

fn fixture() -> (Arc<SessionSupervisor>, Arc<Database>) {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let supervisor = Arc::new(SessionSupervisor::new_for_test(
        db.clone(),
        Arc::new(ToolsFacade::new()),
        1,
    ));
    (supervisor, db)
}

fn corrupt_registry(supervisor: &SessionSupervisor) -> (PathBuf, Vec<u8>) {
    let path = supervisor
        .messaging_root
        .as_ref()
        .unwrap()
        .path()
        .join("agents.json");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"injected invalid registry JSON").unwrap();
    (path, original)
}

async fn drain(supervisor: &Arc<SessionSupervisor>) {
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    supervisor
        .clone()
        .run_terminal_cleanup_worker(final_drain)
        .await;
}

#[derive(Default)]
struct CountingOverlay(AtomicUsize);

#[async_trait::async_trait]
impl SessionToolOverlayPort for CountingOverlay {
    async fn unregister_session(&self, _: &str) {
        self.0.fetch_add(1, Ordering::SeqCst);
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

#[tokio::test]
async fn terminal_inbox_normal_worker_backs_off_and_recovers() {
    let db = Arc::new(Database::open_in_memory().unwrap());
    let overlay = Arc::new(CountingOverlay::default());
    let mut supervisor = SessionSupervisor::new_for_test(db, Arc::new(ToolsFacade::new()), 1);
    supervisor.session_tool_overlay_port = overlay.clone();
    let supervisor = Arc::new(supervisor);
    let session = supervisor.create_session("normal retry").await.unwrap();
    supervisor
        .messaging_service
        .register(&session.session_id, &[])
        .unwrap();
    let (path, original) = corrupt_registry(&supervisor);
    supervisor.end_session(&session.session_id).await.unwrap();
    let final_drain = CancellationToken::new();
    let worker = tokio::spawn(
        supervisor
            .clone()
            .run_terminal_cleanup_worker(final_drain.clone()),
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while overlay.0.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("normal worker must retry failed registry cleanup");
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        overlay.0.load(Ordering::SeqCst),
        2,
        "failed registry cleanup must back off"
    );
    std::fs::write(path, original).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while supervisor.actor_for(&session.session_id).await.is_some() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("normal worker must recover after registry repair");
    final_drain.cancel();
    worker.await.unwrap();
    assert!(
        supervisor
            .messaging_service
            .list_agents()
            .unwrap()
            .is_empty()
    );
}

async fn resident_inbox_failure_recovers(child: bool) {
    let (supervisor, db) = fixture();
    let session = supervisor.create_session("inbox cleanup").await.unwrap();
    let parent_id = child.then(|| haven_common::types::new_id("ses"));
    supervisor
        .messaging_service
        .register_with_profile(&session.session_id, &[], None, None, parent_id.as_deref())
        .unwrap();
    let (path, original) = corrupt_registry(&supervisor);
    assert_eq!(
        supervisor
            .end_session_with_cascade(&session.session_id, false)
            .await
            .unwrap(),
        SessionStatus::Completed
    );
    assert_eq!(
        db.get_session(&session.session_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
    assert!(
        supervisor.actor_for(&session.session_id).await.is_some(),
        "failed inbox cleanup must retain its actor owner"
    );
    assert_eq!(
        supervisor
            .terminal_cleanup_retry_queue
            .pending_retry(&session.session_id),
        Some(TerminalCleanupRetry {
            cascade: Some(false),
            remove_error_actor: true
        })
    );
    // A failed final round must retain work rather than claim completion.
    drain(&supervisor).await;
    assert!(supervisor.actor_for(&session.session_id).await.is_some());
    std::fs::write(path, original).unwrap();
    drain(&supervisor).await;
    assert!(supervisor.actor_for(&session.session_id).await.is_none());
    let agents = supervisor.messaging_service.list_agents().unwrap();
    if child {
        let entry = agents
            .iter()
            .find(|entry| entry.session_id == session.session_id)
            .unwrap();
        assert_eq!(entry.parent_session_id, parent_id);
        assert_eq!(entry.status, haven_messaging::AgentStatus::Offline);
    } else {
        assert!(
            !agents
                .iter()
                .any(|entry| entry.session_id == session.session_id)
        );
    }
}

#[tokio::test]
async fn terminal_inbox_root_failure_retains_work_and_recovers() {
    resident_inbox_failure_recovers(false).await;
}

#[tokio::test]
async fn terminal_inbox_child_failure_retries_mark_offline_and_preserves_parent() {
    resident_inbox_failure_recovers(true).await;
}

#[tokio::test]
async fn terminal_inbox_failure_revokes_trust_and_preserves_no_cascade() {
    let (supervisor, db) = fixture();
    let parent = supervisor.create_session("parent").await.unwrap();
    let child = supervisor.create_session("child").await.unwrap();
    supervisor
        .messaging_service
        .register(&parent.session_id, &[])
        .unwrap();
    supervisor
        .messaging_service
        .register_with_profile(&child.session_id, &[], None, None, Some(&parent.session_id))
        .unwrap();
    supervisor.mark_has_children(&parent.session_id).await;
    let mut request = haven_tools::AuthorizationRequest::new(
        Some(&parent.session_id),
        "files.write",
        serde_json::json!({"path": "notes.txt"}),
        haven_tools::OperationPolicy::native(
            "files.write",
            CapabilityScope::try_new("files.write").unwrap(),
            RiskLevel::High,
            haven_common::tools::NetworkAccess::None,
        ),
    );
    request
        .resources
        .push(haven_tools::DeclaredResource::LocalPath {
            path: PathBuf::from("notes.txt"),
            access: haven_tools::LocalPathAccess::Write,
        });
    let scope = supervisor
        .authorization
        .resource_scope_for_request(
            &request,
            haven_common::types::PermissionResourceTarget::Exact,
        )
        .await
        .unwrap();
    supervisor
        .grant_session_permission(
            &parent.session_id,
            CapabilityScope::try_new("files.write").unwrap(),
            PermissionTarget::Operation,
            PermissionEffect::Allow,
            scope,
        )
        .await
        .unwrap();
    assert!(matches!(
        supervisor.authorization.authorize(&request).await,
        AuthorizationDecision::AutoApproved
    ));
    let (path, original) = corrupt_registry(&supervisor);
    supervisor
        .end_session_with_cascade(&parent.session_id, false)
        .await
        .unwrap();
    assert!(
        matches!(
            supervisor.authorization.authorize(&request).await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ),
        "registry failure must not retain session trust"
    );
    std::fs::write(path, original).unwrap();
    drain(&supervisor).await;
    assert_eq!(
        db.get_session(&child.session_id).unwrap().unwrap().status,
        SessionStatus::Pending
    );
    assert!(supervisor.actor_for(&parent.session_id).await.is_none());
}

#[tokio::test]
async fn terminal_inbox_actorless_failure_reports_postcommit_error_and_explicit_retry() {
    let (supervisor, db) = fixture();
    let session = supervisor.create_session("actorless").await.unwrap();
    supervisor
        .messaging_service
        .register(&session.session_id, &[])
        .unwrap();
    {
        let _lifecycle = supervisor.lifecycle_guard().await;
        supervisor.remove_actor_locked(&session.session_id).await;
    }
    let (path, original) = corrupt_registry(&supervisor);
    let mut events = supervisor.subscribe_events();
    assert!(
        supervisor.end_session(&session.session_id).await.is_err(),
        "actorless End must report incomplete registry cleanup"
    );
    assert_eq!(
        db.get_session(&session.session_id).unwrap().unwrap().status,
        SessionStatus::Completed
    );
    while let Ok(event) = events.try_recv() {
        assert!(
            !matches!(event, SessionSupervisorEvent::SessionEndPaused { .. }),
            "postcommit cleanup failure must not report Paused"
        );
    }
    std::fs::write(path, original).unwrap();
    assert_eq!(
        supervisor.end_session(&session.session_id).await.unwrap(),
        SessionStatus::Completed
    );
    assert!(
        supervisor
            .messaging_service
            .list_agents()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        supervisor.end_session(&session.session_id).await.unwrap(),
        SessionStatus::Completed
    );
}
