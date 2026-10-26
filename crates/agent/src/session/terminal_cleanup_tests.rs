use super::*;
use std::sync::atomic::AtomicUsize;
use tokio::sync::{Notify, Semaphore};

struct BlockingOverlay {
    entered: Notify,
    release: Semaphore,
    active: AtomicUsize,
    maximum: AtomicUsize,
    calls: AtomicUsize,
    allowed: StdMutex<HashSet<String>>,
}

impl Default for BlockingOverlay {
    fn default() -> Self {
        Self {
            entered: Notify::new(),
            release: Semaphore::new(0),
            active: AtomicUsize::new(0),
            maximum: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
            allowed: StdMutex::new(HashSet::new()),
        }
    }
}

struct ActiveCall<'a>(&'a AtomicUsize);

impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl SessionToolOverlayPort for BlockingOverlay {
    async fn unregister_session(&self, session_id: &str) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _active = ActiveCall(&self.active);
        self.maximum.fetch_max(active, Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        let allowed = self.allowed.lock().unwrap().contains(session_id);
        if !allowed {
            self.release.acquire().await.unwrap().forget();
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

fn supervisor(overlay: Arc<BlockingOverlay>) -> Arc<SessionSupervisor> {
    Arc::new(SessionSupervisor::new_with_session_tool_overlay_port(
        SessionStore::new(Arc::new(Database::open_in_memory().unwrap())),
        Arc::new(ToolsFacade::new()),
        1,
        overlay,
    ))
}

async fn queue_terminal(supervisor: &Arc<SessionSupervisor>, cascade: bool) -> String {
    let session = supervisor.create_session("cleanup retry").await.unwrap();
    supervisor
        .actor_for(&session.session_id)
        .await
        .unwrap()
        .transition(SessionStatus::Error, true)
        .await
        .unwrap();
    supervisor.terminal_cleanup_retry_queue.enqueue(
        &session.session_id,
        TerminalCleanupRetry {
            cascade: Some(cascade),
            remove_error_actor: true,
        },
    );
    session.session_id
}

#[tokio::test]
async fn blocked_terminal_cleanup_does_not_block_run_admission() {
    let overlay = Arc::new(BlockingOverlay::default());
    let supervisor = supervisor(overlay.clone());
    queue_terminal(&supervisor, false).await;
    let ran = Arc::new(Notify::new());
    let handler: SessionRunHandler = {
        let ran = ran.clone();
        Arc::new(move |_, _| {
            let ran = ran.clone();
            Box::pin(async move {
                ran.notify_one();
                Ok(())
            })
        })
    };
    supervisor
        .clone()
        .start_dispatcher_without_recovery(handler);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        overlay.entered.notified(),
    )
    .await
    .unwrap();
    let session = supervisor.create_session("new run").await.unwrap();
    supervisor.enqueue_pending(&session.session_id).await;
    supervisor.wake_dispatcher();
    let admitted = tokio::time::timeout(std::time::Duration::from_secs(1), ran.notified()).await;
    overlay.release.add_permits(10);
    supervisor.join_dispatcher().await;
    assert!(
        admitted.is_ok(),
        "cleanup must not occupy the admission loop"
    );
}

#[tokio::test]
async fn terminal_cleanup_abort_before_cleanup_guard_preserves_complete_policy() {
    let overlay = Arc::new(BlockingOverlay::default());
    let supervisor = supervisor(overlay);
    let session_id = queue_terminal(&supervisor, false).await;
    let lifecycle = supervisor.lifecycle_guard().await;
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(
        supervisor
            .clone()
            .run_terminal_cleanup_worker(CancellationToken::new()),
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while supervisor.terminal_cleanup_retry_queue.pending_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker must own the popped item while waiting for the lifecycle gate");
    tasks.abort_all();
    assert!(tasks.join_next().await.unwrap().unwrap_err().is_cancelled());
    assert_eq!(
        supervisor
            .terminal_cleanup_retry_queue
            .pending_retry(&session_id),
        Some(TerminalCleanupRetry {
            cascade: Some(false),
            remove_error_actor: true,
        })
    );
    drop(lifecycle);
}

#[tokio::test]
async fn terminal_cleanup_final_drain_excludes_late_entries_and_processes_multiple_batches() {
    let overlay = Arc::new(BlockingOverlay::default());
    let supervisor = supervisor(overlay.clone());
    let initial_count = terminal_cleanup::CLEANUP_BATCH_SIZE * 2 + 3;
    let mut initial = Vec::new();
    for _ in 0..initial_count {
        initial.push(queue_terminal(&supervisor, false).await);
    }
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(supervisor.clone().run_terminal_cleanup_worker(final_drain));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        overlay.entered.notified(),
    )
    .await
    .unwrap();
    let late = queue_terminal(&supervisor, true).await;
    overlay.release.add_permits(initial_count);
    tokio::time::timeout(std::time::Duration::from_secs(5), tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(overlay.calls.load(Ordering::SeqCst), initial_count);
    assert_eq!(overlay.maximum.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.terminal_cleanup_retry_queue.pending_count(), 1);
    assert!(supervisor.actor_for(&late).await.is_some());
    for session_id in initial {
        assert!(supervisor.actor_for(&session_id).await.is_none());
    }
}

#[tokio::test]
async fn terminal_cleanup_final_timeout_preserves_false_cascade_without_starving_followers() {
    let overlay = Arc::new(BlockingOverlay::default());
    let supervisor = supervisor(overlay.clone());
    let blocked = queue_terminal(&supervisor, false).await;
    let second = queue_terminal(&supervisor, true).await;
    let third = queue_terminal(&supervisor, true).await;
    overlay
        .allowed
        .lock()
        .unwrap()
        .extend([second.clone(), third.clone()]);
    let final_drain = CancellationToken::new();
    final_drain.cancel();
    tokio::time::timeout(
        std::time::Duration::from_secs(8),
        supervisor.clone().run_terminal_cleanup_worker(final_drain),
    )
    .await
    .unwrap();
    assert_eq!(overlay.calls.load(Ordering::SeqCst), 3);
    assert_eq!(overlay.maximum.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.terminal_cleanup_retry_queue.pending_count(), 1);
    assert_eq!(
        supervisor
            .terminal_cleanup_retry_queue
            .pending_retry(&blocked),
        Some(TerminalCleanupRetry {
            cascade: Some(false),
            remove_error_actor: true,
        })
    );
    assert!(supervisor.actor_for(&blocked).await.is_some());
    assert!(supervisor.actor_for(&second).await.is_none());
    assert!(supervisor.actor_for(&third).await.is_none());
    assert!(!supervisor.is_session_closing(&blocked));
}

#[tokio::test]
async fn terminal_cleanup_shutdown_drains_retry_enqueued_by_exiting_run_wrapper() {
    let overlay = Arc::new(BlockingOverlay::default());
    overlay.release.add_permits(1);
    let supervisor = supervisor(overlay.clone());
    let entered = Arc::new(Notify::new());
    let finish = Arc::new(Notify::new());
    let queued = Arc::new(StdMutex::new(None));
    let handler: SessionRunHandler = {
        let supervisor = supervisor.clone();
        let entered = entered.clone();
        let finish = finish.clone();
        let queued = queued.clone();
        Arc::new(move |_, _| {
            let supervisor = supervisor.clone();
            let entered = entered.clone();
            let finish = finish.clone();
            let queued = queued.clone();
            Box::pin(async move {
                entered.notify_one();
                finish.notified().await;
                let session_id = queue_terminal(&supervisor, false).await;
                *queued.lock().unwrap() = Some(session_id);
                Ok(())
            })
        })
    };
    supervisor
        .create_session("running before shutdown")
        .await
        .unwrap();
    supervisor
        .clone()
        .start_dispatcher_without_recovery(handler);
    tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
        .await
        .unwrap();
    let mut joins = tokio::task::JoinSet::new();
    let joining = supervisor.clone();
    joins.spawn(async move { joining.join_dispatcher().await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !supervisor
            .dispatcher_cancellation
            .lock()
            .unwrap()
            .as_ref()
            .is_none_or(CancellationToken::is_cancelled)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    finish.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), joins.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let queued = queued.lock().unwrap().clone().unwrap();
    assert!(supervisor.actor_for(&queued).await.is_none());
    assert_eq!(overlay.calls.load(Ordering::SeqCst), 1);
    assert_eq!(supervisor.terminal_cleanup_retry_queue.pending_count(), 0);
}

#[tokio::test]
async fn terminal_cleanup_shutdown_deadline_aborts_owned_worker_and_retains_every_policy() {
    let overlay = Arc::new(BlockingOverlay::default());
    let supervisor = supervisor(overlay.clone());
    let mut policies = Vec::new();
    // Eight five-second final attempts exceed the existing thirty-second
    // dispatcher deadline. No test-only timeout or alternate runtime is used.
    for index in 0..8 {
        let cascade = index % 2 == 0;
        policies.push((queue_terminal(&supervisor, cascade).await, cascade));
    }
    let handler: SessionRunHandler = Arc::new(|_, _| Box::pin(async { Ok(()) }));
    supervisor
        .clone()
        .start_dispatcher_without_recovery(handler);
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        overlay.entered.notified(),
    )
    .await
    .unwrap();
    let started = std::time::Instant::now();
    tokio::time::timeout(
        std::time::Duration::from_secs(35),
        supervisor.join_dispatcher(),
    )
    .await
    .expect("owned cleanup must obey the dispatcher shutdown deadline");
    assert!(started.elapsed() >= std::time::Duration::from_secs(29));
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while overlay.active.load(Ordering::SeqCst) != 0
            || supervisor.terminal_cleanup_retry_queue.pending_count() != policies.len()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("aborting the dispatcher must also cancel its owned async cleanup worker");
    assert_eq!(overlay.maximum.load(Ordering::SeqCst), 1);
    assert!(
        supervisor
            .terminal_cleanup_sessions
            .lock()
            .unwrap()
            .is_empty()
    );
    for (session_id, cascade) in policies {
        assert_eq!(
            supervisor
                .terminal_cleanup_retry_queue
                .pending_retry(&session_id),
            Some(TerminalCleanupRetry {
                cascade: Some(cascade),
                remove_error_actor: true,
            })
        );
        assert!(!supervisor.is_session_closing(&session_id));
    }
}
