use crate::interaction::InteractionRequest;
pub use haven_common::lifecycle::SessionStatus;
use haven_common::types::MessageAttachment;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::sessions::Session as DbSession;
use haven_tools::{AuthorizationDecision, ToolResult, ToolsManager, is_silent_action};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::sync::{Mutex, broadcast, watch};
use tokio_util::sync::CancellationToken;

/// Last-resort ceiling for [`SessionSupervisor::await_run_finished`]. The
/// normal path is a true oneshot join on handler exit; this bound only
/// guards against a stuck handler so rollback cannot hang forever.
const RUN_EXIT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// User-queue payload (steering or follow-up). Defined in `haven-common`;
/// re-exported so session code uses the canonical queue type.
pub use haven_common::types::FollowUp;

/// Runner invoked by the dispatcher for each picked session. The closure must
/// perform the ReAct loop for `session_id` and return `Ok(())` on completion.
/// It is responsible for acquiring no permits (dispatcher already does) but
/// is expected to update the session status on completion/error.
pub type RunHandler =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

type DirectRunWaiters = HashMap<String, Vec<(usize, CancellationToken)>>;

/// Process-local history-purge admission block with cancellation-safe cleanup.
/// The block is held while runs quiesce and durable rows are removed; dropping
/// it always wakes the dispatcher so a cancelled cleanup cannot strand the
/// whole supervisor in fail-closed mode forever.
pub(crate) struct LifecycleBlockGuard {
    blocked: Arc<std::sync::atomic::AtomicBool>,
    dispatch_tx: watch::Sender<u64>,
}

impl Drop for LifecycleBlockGuard {
    fn drop(&mut self) {
        self.blocked
            .store(false, std::sync::atomic::Ordering::Release);
        self.dispatch_tx.send_modify(|counter| *counter += 1);
    }
}

/// Process-local close marker with cancellation-safe cleanup. A destructive
/// lifecycle operation must block new admissions while it quiesces a session,
/// but a cancelled caller must not leave that session permanently closed.
pub(crate) struct SessionClosingGuard {
    sessions: Arc<StdMutex<HashSet<String>>>,
    session_id: String,
}

impl Drop for SessionClosingGuard {
    fn drop(&mut self) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session_id);
    }
}

/// Absolute fail-closed ceiling for an unanswered **scheduled** confirmation
/// (R2). The interactive UI countdown (120s) starts when the dialog is
/// **shown**, not when the request arrives — so queued confirms behind a
/// visible dialog are not starved. This longer backend timer only covers the
/// closed-UI / crashed-frontend case so pending entries cannot live forever.
pub(crate) const SCHEDULED_CONFIRM_ABSOLUTE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub input: String,
    /// LLM-produced one-line summary used as the ReAct session description
    /// when the dispatcher runs the session. Defaults to `input` when no
    /// classifier summary is available.
    pub summary: String,
    /// LLM-generated short title for display. Set automatically after the
    /// first ReAct loop completes, or manually by the user.
    pub title: Option<String>,
    pub status: SessionStatus,
    pub steps: Vec<StepInfo>,
    pub created_at: String,
    pub updated_at: String,
}

impl SessionInfo {
    /// Build an in-memory `SessionInfo` from a freshly-loaded DB record. Centralizes
    /// the 10-field literal that used to be duplicated at every `load_*` site;
    /// `status` is taken from the record so callers that need a forced override
    /// (e.g. `load_pending_sessions`) can mutate it after construction.
    pub fn from_db_record(record: &DbSession) -> Self {
        Self {
            id: record.id.clone(),
            input: record.input_text.clone(),
            summary: record.transcript.clone(),
            title: record.title.clone(),
            status: record.status,
            steps: Vec::new(),
            created_at: record.created_at.clone(),
            updated_at: record.updated_at.clone(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StepInfo {
    pub id: String,
    pub step_number: i32,
    pub tool_name: String,
    pub input: Value,
    pub output: Option<Value>,
    pub status: String,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

/// Outcome of resolving a confirm request — enough for the app layer to
/// record a permission grant (tool key + session scope).
#[derive(Debug, Clone)]
pub struct ConfirmResolution {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub tool_input: Value,
}

/// Typed side effects emitted by the supervisor. Consumers subscribe to this
/// stream; no subsystem installs mutable one-shot callbacks on the runtime.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    InteractionRequested { request: Box<InteractionRequest> },
    ScheduledConfirmOutcome { title: String, body: String },
    SessionCleanup { session_id: String },
    CascadeCompleted { session_id: String, title: String },
    SessionError { session_id: String, reason: String },
}

/// Result of a safety-gated tool execution: the tool result plus the
/// risk level and confirmation state recorded for the step.
pub struct ToolExecution {
    pub result: ToolResult,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

pub struct SessionSupervisor {
    db: Arc<Database>,
    tools: Arc<ToolsManager>,
    /// The sole cross-session registry. A session's mutable runtime state is
    /// owned by its actor and is never protected by a shared per-session lock.
    actors: Arc<Mutex<HashMap<String, actor::SessionActorHandle>>>,
    /// Admission gate for session runs. Unlike a dynamically resized Tokio
    /// semaphore, the gate tracks active runs explicitly, so lowering and
    /// raising the limit while work is in flight cannot leak permits.
    admission: Arc<dispatcher::RunAdmission>,
    /// Serializes registry changes with the durable session mutations that
    /// accompany them. This closes load-vs-delete and create-vs-clear windows
    /// where a stale actor could otherwise be installed after its DB row was
    /// removed.
    lifecycle_gate: Arc<Mutex<()>>,
    /// Set only while the destructive history-clear operation is quiescing.
    /// New session creation/loading and dispatch admission fail closed until
    /// the durable purge has completed.
    lifecycle_blocked: Arc<std::sync::atomic::AtomicBool>,
    /// Session-scoped closing markers close the gap between quiescing one
    /// session and taking the registry gate. Direct resumes and dispatch
    /// claims must not start after a delete has linearized its close request.
    closing_sessions: Arc<StdMutex<HashSet<String>>>,
    /// Direct resumes waiting for a run slot can be cancelled by delete/clear
    /// instead of waiting for an unrelated session to release capacity.
    direct_run_waiters: Arc<Mutex<DirectRunWaiters>>,
    direct_waiter_id: AtomicUsize,
    /// The supervisor owns exactly one dispatcher. Duplicate starts would
    /// create competing lifecycle consumers and make recovery nondeterministic.
    dispatcher_started: std::sync::atomic::AtomicBool,
    /// FIFO dispatch queue: session ids in the order they became `Pending`
    /// (insertion order ≈ creation order for fresh sessions). The dispatcher
    /// claims from the front, so queued sessions run in submission order instead
    /// of the nondeterministic `HashMap` iteration order a full scan would
    /// produce. Entries are (re-)enqueued on every transition to Pending and
    /// removed on terminal states / claims / explicit removal.
    pending_queue: Arc<Mutex<VecDeque<String>>>,
    /// Dispatch wake counter: incremented on every transition to Pending.
    /// The dispatcher waits on a receiver of this watch, so a session that
    /// becomes Pending right after a failed claim still wakes it (no missed
    /// notification, no polling fallback).
    dispatch_tx: watch::Sender<u64>,
    /// Scheduled confirmations are not session state (some are headless), so
    /// they use a small owner-local list rather than another session map.
    scheduled_confirms: Arc<Mutex<Vec<InteractionRequest>>>,
    /// Coordinated lifecycle for checkpointed stream text (checkpoint /
    /// promote / discard), shared with the agent loop and the end/rollback
    /// paths.
    pub partials: Arc<crate::partial::PartialStore>,
    event_tx: broadcast::Sender<SessionEvent>,
    message_tx: watch::Sender<u64>,
    /// Notification body truncation for scheduled-tool outcomes (matches
    /// `ContextLimitsConfig::notification_summary_chars`).
    pub notification_summary_chars: AtomicUsize,
}

/// Test-only compatibility name kept inside the legacy unit-test module while
/// production callers use [`SessionSupervisor`] directly.
#[cfg(test)]
pub(crate) type SessionExecutor = SessionSupervisor;

mod actor;
mod dispatcher;
mod queues;
mod run_engine;
mod status;
mod tool_runner;
pub(crate) use dispatcher::DirectRunLease;
pub(crate) use tool_runner::{ActionStepMetadata, ActionStepPersistenceError};

pub(crate) use actor::{CONTEXT_BATCH_MAX_CHARS, CONTEXT_BATCH_MAX_ITEMS};
pub(crate) use queues::ReactContextBatch;
pub use run_engine::RunEngine;

impl SessionSupervisor {
    pub fn new(db: Arc<Database>, tools: Arc<ToolsManager>, max_concurrent: usize) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            partials: Arc::new(crate::partial::PartialStore::new(db.clone())),
            db,
            tools,
            actors: Arc::new(Mutex::new(HashMap::new())),
            admission: Arc::new(dispatcher::RunAdmission::new(max_concurrent.max(1))),
            lifecycle_gate: Arc::new(Mutex::new(())),
            lifecycle_blocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            closing_sessions: Arc::new(StdMutex::new(HashSet::new())),
            direct_run_waiters: Arc::new(Mutex::new(HashMap::new())),
            direct_waiter_id: AtomicUsize::new(0),
            dispatcher_started: std::sync::atomic::AtomicBool::new(false),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            dispatch_tx: watch::channel(0).0,
            scheduled_confirms: Arc::new(Mutex::new(Vec::new())),
            event_tx,
            message_tx: watch::channel(0).0,
            notification_summary_chars: AtomicUsize::new(800),
        }
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_tx.subscribe()
    }

    pub(crate) async fn actor_for(&self, session_id: &str) -> Option<actor::SessionActorHandle> {
        self.actors.lock().await.get(session_id).cloned()
    }

    /// Hold the lifecycle gate across a multi-step registry/DB operation.
    /// Callers holding this guard must use the corresponding `_locked` helper
    /// to avoid trying to acquire the same mutex recursively.
    pub(crate) async fn lifecycle_guard(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.lifecycle_gate.clone().lock_owned().await
    }

    pub(crate) fn is_session_closing(&self, session_id: &str) -> bool {
        self.closing_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(session_id)
    }

    pub(crate) async fn register_direct_waiter(
        &self,
        session_id: &str,
        cancellation: CancellationToken,
    ) -> usize {
        let waiter_id = self
            .direct_waiter_id
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.direct_run_waiters
            .lock()
            .await
            .entry(session_id.to_string())
            .or_default()
            .push((waiter_id, cancellation));
        waiter_id
    }

    pub(crate) async fn unregister_direct_waiter(&self, session_id: &str, waiter_id: usize) {
        let mut waiters = self.direct_run_waiters.lock().await;
        let Some(session_waiters) = waiters.get_mut(session_id) else {
            return;
        };
        session_waiters.retain(|(id, _)| *id != waiter_id);
        if session_waiters.is_empty() {
            waiters.remove(session_id);
        }
    }

    pub(crate) async fn cancel_direct_waiters(&self, session_id: &str) {
        let waiters = self.direct_run_waiters.lock().await.remove(session_id);
        if let Some(waiters) = waiters {
            for (_, cancellation) in waiters {
                cancellation.cancel();
            }
        }
    }

    async fn install_actor(&self, info: SessionInfo) -> actor::SessionActorHandle {
        let handle = actor::spawn(self.db.clone(), info);
        self.actors
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        handle
    }

    /// Remove an actor when the caller already owns [`lifecycle_guard`].
    pub(crate) async fn remove_actor_locked(
        &self,
        session_id: &str,
    ) -> Option<actor::SessionActorHandle> {
        self.actors.lock().await.remove(session_id)
    }

    /// Return the in-process mailbox port used by `MessagingService`.
    pub(crate) fn messaging_mailbox(self: &Arc<Self>) -> Arc<dyn haven_tools::SessionMailbox> {
        self.clone()
    }

    /// A service view for the ReAct inbox. It shares this supervisor's actor
    /// registry while retaining the JSONL fallback for external processes.
    pub(crate) fn messaging_service(self: &Arc<Self>) -> Arc<haven_tools::MessagingService> {
        Arc::new(haven_tools::MessagingService::with_session_mailbox(
            self.messaging_mailbox(),
        ))
    }

    pub(crate) fn emit_event(&self, event: SessionEvent) {
        let _ = self.event_tx.send(event);
    }

    pub fn set_notification_summary_chars(&self, chars: usize) {
        self.notification_summary_chars
            .store(chars.max(64), Ordering::Relaxed);
    }

    /// Record that `parent_session_id` spawned a peer child (in-process hint
    /// for cascade skip).
    pub async fn mark_has_children(&self, parent_session_id: &str) {
        if let Some(actor) = self.actor_for(parent_session_id).await {
            actor.set_has_children(true).await;
        }
    }

    async fn may_have_children(&self, session_id: &str) -> bool {
        match self.actor_for(session_id).await {
            Some(actor) => actor.has_children().await,
            None => false,
        }
    }

    async fn clear_has_children(&self, session_id: &str) {
        if let Some(actor) = self.actor_for(session_id).await {
            actor.set_has_children(false).await;
        }
    }
}

impl haven_tools::SessionMailbox for SessionSupervisor {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.message_tx.subscribe()
    }

    fn deliver(
        &self,
        to: &str,
        envelope: &haven_tools::inbox::Envelope,
    ) -> anyhow::Result<Option<haven_tools::inbox::SendOutcome>> {
        let actor = self.actors.blocking_lock().get(to).cloned();
        let Some(actor) = actor else {
            return Ok(None);
        };
        actor.deliver_message(envelope.clone())?;
        self.message_tx.send_modify(|counter| *counter += 1);
        Ok(Some(haven_tools::inbox::SendOutcome {
            to: to.to_string(),
            delivered: true,
            status: haven_tools::inbox::AgentStatus::Online,
        }))
    }

    fn claim(&self, recipient: &str) -> anyhow::Result<Option<Vec<haven_tools::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(recipient).cloned();
        Ok(actor.map(|actor| actor.claim_messages()))
    }

    fn ack(&self, recipient: &str, ids: &[String]) -> anyhow::Result<Option<()>> {
        let actor = self.actors.blocking_lock().get(recipient).cloned();
        let Some(actor) = actor else {
            return Ok(None);
        };
        actor.ack_messages(ids.to_vec());
        Ok(Some(()))
    }

    fn last_received(
        &self,
        name: &str,
    ) -> anyhow::Result<Option<Option<haven_tools::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| actor.last_received_message()))
    }

    fn find_message(
        &self,
        name: &str,
        id: &str,
    ) -> anyhow::Result<Option<Option<haven_tools::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| actor.find_message_by_id(id.to_string())))
    }

    fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Option<Vec<haven_tools::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| {
            actor.take_matching_replies_blocking(in_reply_to.to_string(), expected_from.to_string())
        }))
    }

    fn history(
        &self,
        name: &str,
        limit: usize,
    ) -> anyhow::Result<Option<Vec<haven_tools::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| actor.history_blocking(limit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_tools::inbox::MessageType;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        p
    }

    fn make_executor(max_concurrent: usize) -> Arc<SessionExecutor> {
        let path = temp_db_path();
        let db = Arc::new(Database::open(&path).unwrap());
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, max_concurrent));
        // Best-effort cleanup; failures are ignored since the OS will purge
        // temp files eventually.
        let _ = path;
        exec
    }

    /// A handler that panics must still release the running slot and mark the
    /// session Error —otherwise the session is stuck in Running forever.
    #[tokio::test]
    async fn dispatcher_panicked_handler_marks_error() {
        let exec = make_executor(1);
        let session = exec.create_session("t1").await.unwrap();

        let mut events = exec.subscribe_events();

        let handler: RunHandler = Arc::new(move |_id: String| {
            Box::pin(async move {
                panic!("simulated handler panic");
                #[allow(unreachable_code)]
                Ok(())
            })
        });
        exec.clone().start_dispatcher(handler);

        // Wait for the dispatcher to claim the session, run the panicking
        // handler, and mark it Error in the DB (pending → running → error).
        let mut db_status = SessionStatus::Pending;
        for _ in 0..100 {
            db_status = exec
                .db
                .get_session(&session.id)
                .unwrap()
                .map(|t| t.status)
                .unwrap_or(SessionStatus::Error);
            if db_status == SessionStatus::Error {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(db_status, SessionStatus::Error);
        // Terminal status removed the session from the working set and released
        // the running slot; the session is absent, not "error" in memory.
        //
        // NOTE: `update_session_status` persists the DB row BEFORE the
        // in-memory cleanup (`cleanup_session_maps` / `unmark_running`), so
        // seeing "error" in the DB does not guarantee the slot is released yet.
        // Under parallel test load the dispatcher action can be descheduled
        // between the two, so poll the memory side instead of asserting it
        // immediately (this test flaked under `cargo test --workspace`).
        let mut released = false;
        for _ in 0..100 {
            if !exec.is_run_in_flight(&session.id).await
                && exec.get_session_state(&session.id).await.is_none()
            {
                released = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(released, "running slot / working set must be released");
        // The typed failure event fires right after the DB write in the
        // dispatcher's spawned session.
        let mut seen = None;
        for _ in 0..100 {
            if let Ok(Ok(SessionEvent::SessionError { session_id, reason })) =
                tokio::time::timeout(std::time::Duration::from_millis(10), events.recv()).await
            {
                seen = Some((session_id, reason));
                break;
            }
        }
        let (seen_id, seen_reason) = seen.expect("session error event must fire");
        assert_eq!(seen_id, session.id);
        assert!(seen_reason.contains("panicked"), "reason: {seen_reason}");
    }

    /// `await_run_finished` must not resolve until the dispatcher handler
    /// has exited and released the running slot (F5: no timed poll).
    #[tokio::test]
    async fn await_run_finished_waits_until_handler_exits() {
        let exec = make_executor(1);
        let session = exec.create_session("await-exit").await.unwrap();

        let release = Arc::new(AtomicU32::new(0));
        let in_handler = Arc::new(AtomicU32::new(0));
        let exited = Arc::new(AtomicU32::new(0));

        let release_h = release.clone();
        let in_handler_h = in_handler.clone();
        let exited_h = exited.clone();
        let exec_ref = exec.clone();
        let handler: RunHandler = Arc::new(move |id: String| {
            let release_h = release_h.clone();
            let in_handler_h = in_handler_h.clone();
            let exited_h = exited_h.clone();
            let exec_ref = exec_ref.clone();
            Box::pin(async move {
                in_handler_h.store(1, Ordering::SeqCst);
                while release_h.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                let _ = exec_ref
                    .update_session_status(&id, SessionStatus::Paused)
                    .await;
                exited_h.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });
        exec.clone().start_dispatcher(handler);

        let mut seen = false;
        for _ in 0..100 {
            if in_handler.load(Ordering::SeqCst) == 1 && exec.is_run_in_flight(&session.id).await {
                seen = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(seen, "handler should be running and hold the slot");

        let exec_wait = exec.clone();
        let sid = session.id.clone();
        let wait_task = tokio::spawn(async move {
            let _ = exec_wait.await_run_finished(&sid).await;
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(exited.load(Ordering::SeqCst), 0);
        assert!(
            !wait_task.is_finished(),
            "await_run_finished must block while the handler is alive"
        );

        release.store(1, Ordering::SeqCst);

        tokio::time::timeout(std::time::Duration::from_secs(2), wait_task)
            .await
            .expect("await_run_finished should resolve after handler exit")
            .expect("wait task join");
        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert!(!exec.is_run_in_flight(&session.id).await);
    }

    /// `await_run_finished` is a no-op when the session was never claimed.
    #[tokio::test]
    async fn await_run_finished_noop_when_not_running() {
        let exec = make_executor(1);
        let session = exec.create_session("idle").await.unwrap();
        // Do not start a dispatcher — session stays Pending, no run_exit gate.
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            exec.await_run_finished(&session.id),
        )
        .await
        .expect("must return immediately when no run is in flight");
    }

    /// Dispatcher honors `max_concurrent` and drains all Pending sessions.
    #[tokio::test]
    async fn dispatcher_respects_max_concurrent() {
        let exec = make_executor(2);

        let current = Arc::new(AtomicU32::new(0));
        let peak = Arc::new(AtomicU32::new(0));
        let completed = Arc::new(AtomicU32::new(0));

        let cur = current.clone();
        let pk = peak.clone();
        let done = completed.clone();
        let exec_ref = exec.clone();
        let handler: RunHandler = Arc::new(move |id: String| {
            let cur = cur.clone();
            let pk = pk.clone();
            let done = done.clone();
            let exec_ref = exec_ref.clone();
            Box::pin(async move {
                let n = cur.fetch_add(1, Ordering::SeqCst) + 1;
                pk.fetch_max(n, Ordering::SeqCst);
                assert!(n <= 2, "concurrency exceeded max_concurrent=2: {}", n);
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                cur.fetch_sub(1, Ordering::SeqCst);
                let _ = exec_ref
                    .update_session_status(&id, SessionStatus::Completed)
                    .await;
                done.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
        });

        for i in 0..5 {
            exec.create_session(&format!("session {}", i))
                .await
                .unwrap();
        }

        exec.clone().start_dispatcher(handler);

        for _ in 0..200 {
            if completed.load(Ordering::SeqCst) == 5 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        assert_eq!(completed.load(Ordering::SeqCst), 5);
        assert!(
            peak.load(Ordering::SeqCst) >= 1 && peak.load(Ordering::SeqCst) <= 2,
            "peak concurrent out of expected range: {}",
            peak.load(Ordering::SeqCst)
        );
    }

    /// Claim is atomic: it flips the session to Running in memory + DB and
    /// inserts it into the running set, so a second claim returns nothing.
    #[tokio::test]
    async fn try_claim_pending_claims_once_and_persists() {
        let exec = make_executor(2);
        let session = exec.create_session("t1").await.unwrap();

        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(session.id.as_str()));

        let state = exec.get_session_state(&session.id).await;
        assert_eq!(state, Some(SessionStatus::Running));
        assert!(exec.is_run_in_flight(&session.id).await);
        let db_status = exec
            .db
            .get_session(&session.id)
            .unwrap()
            .map(|t| t.status)
            .unwrap_or(SessionStatus::Error);
        assert_eq!(db_status, SessionStatus::Running);

        // No second claim while the first handler holds the slot.
        assert!(exec.try_claim_pending().await.is_none());
    }

    /// A claimed session cannot be claimed again until its run finishes.
    #[tokio::test]
    async fn try_claim_pending_skips_session_already_in_running_set() {
        let exec = make_executor(2);
        let session = exec.create_session("t1").await.unwrap();
        assert_eq!(
            exec.try_claim_pending().await.as_deref(),
            Some(session.id.as_str())
        );
        assert!(exec.is_run_in_flight(&session.id).await);
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        exec.end_direct_run(&session.id).await;
        exec.enqueue_pending(&session.id).await;
        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(session.id.as_str()));
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Running)
        );
    }

    /// Claims follow FIFO submission order: the oldest Pending session is
    /// claimed first, not a HashMap-iteration lottery.
    #[tokio::test]
    async fn try_claim_pending_is_fifo_by_submission_order() {
        let exec = make_executor(1);
        let t1 = exec.create_session("first").await.unwrap();
        let t2 = exec.create_session("second").await.unwrap();
        let t3 = exec.create_session("third").await.unwrap();

        let c1 = exec.try_claim_pending().await;
        let c2 = exec.try_claim_pending().await;
        let c3 = exec.try_claim_pending().await;
        assert_eq!(c1.as_deref(), Some(t1.id.as_str()));
        assert_eq!(c2.as_deref(), Some(t2.id.as_str()));
        assert_eq!(c3.as_deref(), Some(t3.id.as_str()));
        assert!(exec.try_claim_pending().await.is_none());
    }

    /// `set_max_concurrent` must change the effective active-run ceiling
    /// exactly, even when resized while no work is running.
    #[tokio::test]
    async fn set_max_concurrent_reclaims_and_does_not_overshoot() {
        let exec = make_executor(4);
        exec.set_max_concurrent(1);
        // Idle pool: exactly one permit may be acquired without waiting.
        let first = exec.admission.try_acquire();
        assert!(
            first.is_some(),
            "one permit must be available after lowering to 1"
        );
        let second = exec.admission.try_acquire();
        assert!(
            second.is_none(),
            "lowering must reclaim unused permits (no-op reclaim would leave 3 free)"
        );
        drop(first.unwrap());
        // Raise back to 3: available permits must be 3, not 3 + stale 3.
        exec.set_max_concurrent(3);
        let mut held = Vec::new();
        for _ in 0..3 {
            match exec.admission.try_acquire() {
                Some(p) => held.push(p),
                None => break,
            }
        }
        assert_eq!(
            held.len(),
            3,
            "raise after lower must yield exactly 3 permits"
        );
        assert!(
            exec.admission.try_acquire().is_none(),
            "no extra permits may leak from the lower→raise cycle"
        );
        drop(held);
    }

    /// Resizing while every old slot is occupied must not leave the old
    /// capacity cached in returned permits. Once the four old runs finish, a
    /// limit of one still admits exactly one new run.
    #[tokio::test]
    async fn admission_resize_while_running_has_no_stale_capacity() {
        let admission = Arc::new(dispatcher::RunAdmission::new(4));
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(admission.try_acquire().expect("initial slot available"));
        }
        assert!(admission.try_acquire().is_none());

        admission.set_limit(1);
        drop(held);

        let one = admission.try_acquire();
        assert!(one.is_some());
        assert!(admission.try_acquire().is_none());
        drop(one);
    }

    #[tokio::test]
    async fn delete_session_removes_durable_row_and_actor_together() {
        let exec = make_executor(1);
        let session = exec.create_session("delete atomically").await.unwrap();

        exec.delete_session(&session.id).await.unwrap();

        assert!(exec.actor_for(&session.id).await.is_none());
        assert!(exec.db.get_session(&session.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_cancels_direct_resume_waiting_for_capacity() {
        let exec = make_executor(1);
        let session = exec.create_session("delete waiting resume").await.unwrap();
        let held = exec
            .admission
            .try_acquire()
            .expect("test run occupies the only slot");
        let exec_for_resume = exec.clone();
        let session_id = session.id.clone();
        let resume =
            tokio::spawn(async move { exec_for_resume.begin_direct_run(&session_id).await });

        for _ in 0..100 {
            if exec
                .direct_run_waiters
                .lock()
                .await
                .contains_key(&session.id)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            exec.direct_run_waiters
                .lock()
                .await
                .contains_key(&session.id),
            "direct resume should register its cancellable admission wait"
        );

        exec.delete_session(&session.id).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), resume)
            .await
            .expect("delete must wake a direct resume waiting for capacity")
            .unwrap();
        assert!(result.is_none());
        drop(held);
    }

    #[tokio::test]
    async fn closing_marker_is_released_when_delete_task_is_cancelled() {
        let exec = make_executor(1);
        let session = exec.create_session("cancelled delete").await.unwrap();
        let exec_for_delete = exec.clone();
        let session_id = session.id.clone();
        let delete = tokio::spawn(async move {
            let _closing = exec_for_delete
                .begin_session_closing(&session_id)
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });

        for _ in 0..100 {
            if exec.is_session_closing(&session.id) {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(exec.is_session_closing(&session.id));
        delete.abort();
        let _ = delete.await;
        assert!(!exec.is_session_closing(&session.id));
    }

    #[tokio::test]
    async fn duplicate_closing_admission_is_rejected() {
        let exec = make_executor(1);
        let session = exec.create_session("duplicate close").await.unwrap();
        let first = exec.begin_session_closing(&session.id).await.unwrap();
        let second = exec.begin_session_closing(&session.id).await;
        assert!(second.is_err());
        drop(first);
        assert!(!exec.is_session_closing(&session.id));
    }

    #[tokio::test]
    async fn lifecycle_block_is_released_when_cleanup_task_is_cancelled() {
        let exec = make_executor(1);
        let exec_for_cleanup = exec.clone();
        let cleanup = tokio::spawn(async move {
            let _block = exec_for_cleanup.begin_lifecycle_block().unwrap();
            std::future::pending::<()>().await;
        });

        for _ in 0..100 {
            if exec.ensure_lifecycle_open().is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(exec.ensure_lifecycle_open().is_err());
        cleanup.abort();
        let _ = cleanup.await;
        assert!(exec.ensure_lifecycle_open().is_ok());
    }

    #[tokio::test]
    async fn terminal_actor_removal_waits_for_lifecycle_gate() {
        let exec = make_executor(1);
        let session = exec.create_session("gated terminal removal").await.unwrap();
        let lifecycle = exec.lifecycle_guard().await;
        let exec_for_transition = exec.clone();
        let session_id = session.id.clone();
        let transition = tokio::spawn(async move {
            exec_for_transition
                .update_session_status(&session_id, SessionStatus::Completed)
                .await
                .unwrap();
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(
            exec.actor_for(&session.id).await.is_some(),
            "terminal cleanup must not remove an actor outside the lifecycle gate"
        );
        drop(lifecycle);
        tokio::time::timeout(std::time::Duration::from_secs(1), transition)
            .await
            .expect("terminal status transition should finish")
            .unwrap();
        assert!(exec.actor_for(&session.id).await.is_none());
    }

    /// A session terminated by end_session between the old find/mark window must
    /// not be resurrected by a late claim (no ghost execution).
    #[tokio::test]
    async fn try_claim_pending_respects_end_session() {
        let exec = make_executor(2);
        let session = exec.create_session("t1").await.unwrap();

        let status = exec.end_session(&session.id).await.unwrap();
        assert_eq!(status, SessionStatus::Completed);

        assert!(exec.try_claim_pending().await.is_none());
        assert!(!exec.is_run_in_flight(&session.id).await);
    }

    // ─── Data-layer tests (no dispatcher required) ───

    fn temp_db() -> Arc<Database> {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        Arc::new(Database::open(&p).unwrap())
    }

    #[tokio::test]
    async fn messaging_service_routes_full_lifecycle_through_actor_mailboxes() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 2));
        let sender = exec.create_session("sender").await.unwrap();
        let receiver = exec.create_session("receiver").await.unwrap();
        let service = exec.messaging_service();

        let request = {
            let service = service.clone();
            let from = sender.id.clone();
            let to = receiver.id.clone();
            tokio::task::spawn_blocking(move || {
                service
                    .request(&from, &to, "请处理", None, None, None)
                    .unwrap()
            })
            .await
            .unwrap()
        };

        let first_claim = {
            let service = service.clone();
            let recipient = receiver.id.clone();
            tokio::task::spawn_blocking(move || service.claim(&recipient).unwrap())
                .await
                .unwrap()
        };
        assert_eq!(first_claim.envelopes()[0].id, request.envelope.id);
        assert_eq!(first_claim.envelopes()[0].delivery_attempt, 1);
        tokio::task::spawn_blocking(move || first_claim.retry())
            .await
            .unwrap();

        let second_claim = {
            let service = service.clone();
            let recipient = receiver.id.clone();
            tokio::task::spawn_blocking(move || service.claim(&recipient).unwrap())
                .await
                .unwrap()
        };
        assert_eq!(second_claim.envelopes()[0].delivery_attempt, 2);
        tokio::task::spawn_blocking(move || second_claim.complete().unwrap())
            .await
            .unwrap();

        let receipt_claim = {
            let service = service.clone();
            let recipient = sender.id.clone();
            tokio::task::spawn_blocking(move || service.claim(&recipient).unwrap())
                .await
                .unwrap()
        };
        assert_eq!(receipt_claim.envelopes().len(), 1);
        assert_eq!(receipt_claim.envelopes()[0].r#type, MessageType::Receipt);
        tokio::task::spawn_blocking(move || receipt_claim.complete().unwrap())
            .await
            .unwrap();

        let reply_service = service.clone();
        let reply_to = request.envelope.id.clone();
        let from = receiver.id.clone();
        let to = sender.id.clone();
        let reply_task = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            tokio::task::spawn_blocking(move || {
                reply_service
                    .reply(&from, &to, &reply_to, "已完成", None, None, None)
                    .unwrap()
            })
            .await
            .unwrap();
        });
        let reply = service
            .wait_for_reply(
                &sender.id,
                &request.envelope.id,
                &receiver.id,
                std::time::Duration::from_secs(1),
                &tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap()
            .unwrap();
        reply_task.await.unwrap();
        assert_eq!(reply.text, "已完成");

        let reply_receipt = {
            let service = service.clone();
            let recipient = receiver.id.clone();
            tokio::task::spawn_blocking(move || service.claim(&recipient).unwrap())
                .await
                .unwrap()
        };
        assert_eq!(reply_receipt.envelopes().len(), 1);
        assert_eq!(reply_receipt.envelopes()[0].r#type, MessageType::Receipt);
        tokio::task::spawn_blocking(move || reply_receipt.complete().unwrap())
            .await
            .unwrap();

        let mut expired = haven_tools::inbox::Envelope::new(&sender.id, &receiver.id, "过期");
        expired.expires_at = Some("2000-01-01T00:00:00Z".into());
        let expired_id = expired.id.clone();
        {
            let service = service.clone();
            tokio::task::spawn_blocking(move || service.send(expired).unwrap())
                .await
                .unwrap();
        }
        let expired_claim = {
            let service = service.clone();
            let recipient = receiver.id.clone();
            tokio::task::spawn_blocking(move || service.claim(&recipient).unwrap())
                .await
                .unwrap()
        };
        assert!(expired_claim.is_empty());
        let history = {
            let service = service.clone();
            let recipient = receiver.id.clone();
            tokio::task::spawn_blocking(move || service.history(&recipient, 20).unwrap())
                .await
                .unwrap()
        };
        assert!(history.iter().any(|envelope| envelope.id == expired_id));
    }

    #[tokio::test]
    async fn constructor_creates_executor() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db.clone(), tools.clone(), 3);
        assert_eq!(exec.running_count().await, 0);
        assert!(exec.list_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn create_session_returns_pending_session() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("hello world").await.unwrap();
        assert_eq!(session.status, SessionStatus::Pending);
        assert_eq!(session.input, "hello world");
        assert!(!session.id.is_empty());
        assert!(!session.created_at.is_empty());
    }

    #[tokio::test]
    async fn create_session_with_summary_preserves_fields() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec
            .create_session_with_summary("raw input", "summary text")
            .await
            .unwrap();
        assert_eq!(session.input, "raw input");
        assert_eq!(session.summary, "summary text");
    }

    #[tokio::test]
    async fn end_session_running_marks_completed_and_triggers_token() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        // Set to Running so end_session also cancels the loop token.
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();
        let real_token = exec.cancellation_token(&session.id).await;
        assert!(!real_token.is_cancelled());
        let status = exec.end_session(&session.id).await.unwrap();
        assert_eq!(status, SessionStatus::Completed);
        assert!(real_token.is_cancelled());
        // end_session removes the session from the working set entirely.
        assert_eq!(exec.get_session_state(&session.id).await, None);
    }

    #[tokio::test]
    async fn end_session_waits_for_an_active_run_to_exit() {
        let exec = make_executor(1);
        let session = exec.create_session("active end").await.unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let cancellation_seen = Arc::new(AtomicUsize::new(0));
        let allow_exit = Arc::new(AtomicUsize::new(0));
        let exited = Arc::new(AtomicUsize::new(0));

        let started_handler = started.clone();
        let cancellation_seen_handler = cancellation_seen.clone();
        let allow_exit_handler = allow_exit.clone();
        let exited_handler = exited.clone();
        let exec_handler = exec.clone();
        let handler: RunHandler = Arc::new(move |session_id: String| {
            let started = started_handler.clone();
            let cancellation_seen = cancellation_seen_handler.clone();
            let allow_exit = allow_exit_handler.clone();
            let exited = exited_handler.clone();
            let exec = exec_handler.clone();
            Box::pin(async move {
                started.store(1, Ordering::SeqCst);
                let cancel = exec.cancellation_token(&session_id).await;
                cancel.cancelled().await;
                cancellation_seen.store(1, Ordering::SeqCst);
                while allow_exit.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                exited.store(1, Ordering::SeqCst);
                Ok(())
            })
        });
        exec.clone().start_dispatcher(handler);

        for _ in 0..100 {
            if started.load(Ordering::SeqCst) == 1 && exec.is_run_in_flight(&session.id).await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(started.load(Ordering::SeqCst), 1, "handler should start");
        assert!(
            exec.is_run_in_flight(&session.id).await,
            "run should be active"
        );

        let end = {
            let exec = exec.clone();
            let session_id = session.id.clone();
            tokio::spawn(async move { exec.end_session(&session_id).await })
        };

        for _ in 0..100 {
            if cancellation_seen.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            cancellation_seen.load(Ordering::SeqCst),
            1,
            "end_session should cancel the active run"
        );
        assert!(
            !end.is_finished(),
            "end_session must wait for the active run"
        );
        assert_eq!(exited.load(Ordering::SeqCst), 0);

        allow_exit.store(1, Ordering::SeqCst);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), end)
            .await
            .expect("end_session should finish after the run exits")
            .expect("end task should join")
            .expect("end_session should succeed");
        assert_eq!(result, SessionStatus::Completed);
        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert_eq!(exec.get_session_state(&session.id).await, None);
    }

    #[tokio::test]
    async fn interrupt_session_pauses_and_cancels_without_removing() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();

        let real_token = exec.cancellation_token(&session.id).await;

        assert!(exec.interrupt_session(&session.id).await.unwrap());
        assert!(real_token.is_cancelled());
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(exec.get_session(&session.id).await.is_some());
        assert!(!exec.interrupt_session(&session.id).await.unwrap());
    }

    #[tokio::test]
    async fn interrupt_session_waits_for_an_active_run_to_exit() {
        let exec = make_executor(1);
        let session = exec.create_session("active interrupt").await.unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let cancellation_seen = Arc::new(AtomicUsize::new(0));
        let allow_exit = Arc::new(AtomicUsize::new(0));
        let exited = Arc::new(AtomicUsize::new(0));

        let started_handler = started.clone();
        let cancellation_seen_handler = cancellation_seen.clone();
        let allow_exit_handler = allow_exit.clone();
        let exited_handler = exited.clone();
        let exec_handler = exec.clone();
        let handler: RunHandler = Arc::new(move |session_id: String| {
            let started = started_handler.clone();
            let cancellation_seen = cancellation_seen_handler.clone();
            let allow_exit = allow_exit_handler.clone();
            let exited = exited_handler.clone();
            let exec = exec_handler.clone();
            Box::pin(async move {
                started.store(1, Ordering::SeqCst);
                let cancel = exec.cancellation_token(&session_id).await;
                cancel.cancelled().await;
                cancellation_seen.store(1, Ordering::SeqCst);
                while allow_exit.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                exited.store(1, Ordering::SeqCst);
                Ok(())
            })
        });
        exec.clone().start_dispatcher(handler);

        for _ in 0..100 {
            if started.load(Ordering::SeqCst) == 1 && exec.is_run_in_flight(&session.id).await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(started.load(Ordering::SeqCst), 1, "handler should start");
        assert!(
            exec.is_run_in_flight(&session.id).await,
            "run should be active"
        );

        let interrupt = {
            let exec = exec.clone();
            let session_id = session.id.clone();
            tokio::spawn(async move { exec.interrupt_session(&session_id).await })
        };

        for _ in 0..100 {
            if cancellation_seen.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            cancellation_seen.load(Ordering::SeqCst),
            1,
            "interrupt_session should cancel the active run"
        );
        assert!(
            !interrupt.is_finished(),
            "interrupt_session must wait for the active run"
        );
        assert_eq!(exited.load(Ordering::SeqCst), 0);

        allow_exit.store(1, Ordering::SeqCst);
        let paused = tokio::time::timeout(std::time::Duration::from_secs(2), interrupt)
            .await
            .expect("interrupt_session should finish after the run exits")
            .expect("interrupt task should join")
            .expect("interrupt_session should succeed");
        assert!(paused);
        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn end_session_nonexistent_succeeds() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        // end_session on a nonexistent session updates DB directly.
        let result = exec.end_session("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn end_session_paused_marks_completed() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let status = exec.end_session(&session.id).await.unwrap();
        assert_eq!(status, SessionStatus::Completed);
    }

    #[tokio::test]
    async fn add_and_get_follow_ups() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        exec.add_follow_up(&session.id, "extra context 1")
            .await
            .unwrap();
        exec.add_follow_up(&session.id, "extra context 2")
            .await
            .unwrap();
        let drained: Vec<String> = exec
            .get_follow_ups(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["extra context 1", "extra context 2"]);
        assert!(exec.get_follow_ups(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn answer_supplement_carries_is_answer_flag() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        exec.add_answer_with_attachments(&session.id, "the answer", &[], None)
            .await
            .unwrap();
        exec.add_follow_up(&session.id, "plain context")
            .await
            .unwrap();
        let drained = exec.get_follow_ups(&session.id).await;
        assert_eq!(drained.len(), 2);
        assert!(drained[0].is_answer, "first message is an ask reply");
        assert_eq!(drained[0].text, "the answer");
        assert!(!drained[1].is_answer, "plain supplement is not an answer");
    }

    #[tokio::test]
    async fn add_and_get_follow_ups_with_attachments() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
        exec.add_follow_up_with_attachments(&session.id, "看图", std::slice::from_ref(&att), None)
            .await
            .unwrap();
        let drained = exec.get_follow_ups(&session.id).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text, "看图");
        assert_eq!(drained[0].attachments, vec![att]);
        assert!(exec.get_follow_ups(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn add_follow_up_nonexistent_session_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let result = exec.add_follow_up("nonexistent", "ctx").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn add_and_get_steering() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        exec.add_steering(&session.id, "steer 1").await.unwrap();
        let drained: Vec<String> = exec
            .get_steering(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["steer 1"]);
    }

    #[tokio::test]
    async fn list_actions_all_present() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);

        let _low = exec.create_session("low").await.unwrap();
        let _normal = exec.create_session("normal").await.unwrap();
        let _high = exec.create_session("high").await.unwrap();

        let sessions = exec.list_sessions().await;
        assert_eq!(sessions.len(), 3);
    }

    #[tokio::test]
    async fn get_session_state_returns_correct_status() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn get_session_state_nonexistent_returns_none() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        // Absent means "not in the working set", NOT Error.
        assert_eq!(exec.get_session_state("nonexistent").await, None);
    }

    #[tokio::test]
    async fn cancellation_token_returns_default_for_unknown_session() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let token = exec.cancellation_token("nonexistent").await;
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn load_pending_actions_reloads_after_restart() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db.clone(), tools.clone(), 3);
        let session = exec.create_session("queued before restart").await.unwrap();

        // Simulate a restart: fresh executor over the same DB with an empty
        // working set. The pending session must be reloaded and dispatchable.
        let exec2 = SessionExecutor::new(db.clone(), tools, 3);
        assert!(exec2.list_sessions().await.is_empty());
        let loaded = exec2.load_pending_sessions().await.unwrap();
        assert_eq!(loaded, 1);

        let claimed = exec2.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(session.id.as_str()));
        assert_eq!(
            exec2.get_session_state(&session.id).await,
            Some(SessionStatus::Running)
        );
    }

    #[tokio::test]
    async fn dispatcher_can_defer_pending_recovery_until_catalog_ready() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db.clone(), tools.clone(), 1));
        let session = exec.create_session("queued before catalog").await.unwrap();
        let exec2 = Arc::new(SessionExecutor::new(db, tools, 1));
        let handled = Arc::new(AtomicU32::new(0));
        let handled_by_runner = handled.clone();
        let exec_for_runner = exec2.clone();
        let handler: RunHandler = Arc::new(move |session_id: String| {
            let handled = handled_by_runner.clone();
            let exec = exec_for_runner.clone();
            Box::pin(async move {
                handled.fetch_add(1, Ordering::SeqCst);
                exec.update_session_status(&session_id, SessionStatus::Completed)
                    .await?;
                Ok(())
            })
        });

        exec2.clone().start_dispatcher_without_recovery(handler);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(handled.load(Ordering::SeqCst), 0);
        assert!(exec2.list_sessions().await.is_empty());
        assert_eq!(
            exec2
                .db
                .get_session(&session.id)
                .unwrap()
                .map(|record| record.status),
            Some(SessionStatus::Pending)
        );

        assert_eq!(exec2.load_pending_sessions().await.unwrap(), 1);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while handled.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("deferred recovery should dispatch after loading pending sessions");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while exec2.get_session_state(&session.id).await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("deferred recovery should release the completed session");
    }

    #[tokio::test]
    async fn load_pending_actions_skips_non_pending() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db.clone(), tools.clone(), 3);
        let done = exec.create_session("done").await.unwrap();
        exec.end_session(&done.id).await.unwrap();
        let paused = exec.create_session("paused").await.unwrap();
        exec.update_session_status(&paused.id, SessionStatus::Paused)
            .await
            .unwrap();

        // Restart: only the still-pending session is reloaded.
        let exec2 = SessionExecutor::new(db, tools, 3);
        let loaded = exec2.load_pending_sessions().await.unwrap();
        assert_eq!(loaded, 0);
        assert!(exec2.list_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn update_session_status_changes_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        // Terminal status removes the session from the in-memory working set.
        assert_eq!(exec.get_session_state(&session.id).await, None);
    }

    #[tokio::test]
    async fn update_session_status_completed_cleans_up() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        assert!(!exec.is_run_in_flight(&session.id).await);
        assert!(exec.get_session_state(&session.id).await.is_none());
    }

    #[tokio::test]
    async fn execute_step_unknown_tool_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        let result = exec
            .execute_step(
                &session.id,
                "nonexistent_tool",
                serde_json::json!({}),
                1,
                "step-any",
            )
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn execute_step_rejects_paused_without_forcing_running() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("paused tool").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let result = exec
            .execute_step(
                &session.id,
                "nonexistent_tool",
                serde_json::json!({}),
                1,
                "step-any",
            )
            .await;
        assert!(result.is_err());
        // Must not have forced memory back to Running.
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn execute_step_rejects_pending_without_forcing_running() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("pending tool").await.unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let result = exec
            .execute_step(
                &session.id,
                "nonexistent_tool",
                serde_json::json!({}),
                1,
                "step-any",
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn execute_step_rejects_missing_session_fail_closed() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let result = exec
            .execute_step(
                "ses-deadbeefdeadbeefdeadbeefdeadbeef",
                "nonexistent_tool",
                serde_json::json!({}),
                1,
                "step-any",
            )
            .await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not in working set"),
            "expected fail-closed missing-session error, got: {msg}"
        );
    }

    #[tokio::test]
    async fn paused_state_uses_interaction_registry() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("ask me").await.unwrap();

        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        exec.request_interaction(crate::interaction::InteractionRequest::ask(
            &session.id,
            "which file?",
            vec!["README.md".into()],
            vec!["step-0123456789abcdef0123456789abcdef".into()],
        ))
        .await
        .unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let pending = exec
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
            .await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].prompt, "which file?");

        // Reactivation (user answered → Pending) exits the awaiting state.
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn plain_pause_has_no_interaction() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("pause me").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let state = exec.get_session_state(&session.id).await.unwrap();
        assert!(state.is_paused());
        assert!(
            exec.pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
                .await
                .is_empty()
        );
        assert_eq!(state.as_str(), "paused");
    }

    #[tokio::test]
    async fn terminal_actions_need_explicit_reopen_to_reactivate() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("t").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        // The terminal session was removed from the working set; any later
        // update on the absent entry is a silent no-op, not a resurrection.
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        assert_eq!(exec.get_session_state(&session.id).await, None);
        // In-memory resurrection is only possible through the explicit
        // reopen path (Completed → Paused) after ensure_session_loaded.
        exec.ensure_session_loaded(&session.id).await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Paused)
        );
        // And from Paused the session resumes via the normal Paused → Pending
        // path (e.g. process_input / continue flow).
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn status_watch_wakes_waiter_on_transition() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("wait").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        // Waiter subscribes AFTER the pause (the level-triggered value must
        // still be visible) and wakes on the resume transition.
        let exec2 = exec.clone();
        let tid = session.id.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut rx = exec2.subscribe_status(&tid).await;
            let _ = rx.changed().await;
            let _ = done_tx.send(exec2.get_session_state(&tid).await);
        });

        // Give the waiter a moment to register, then transition.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();

        let state = tokio::time::timeout(std::time::Duration::from_secs(2), done_rx)
            .await
            .expect("waiter must wake within 2s")
            .unwrap();
        assert_eq!(state, Some(SessionStatus::Pending));
    }

    #[tokio::test]
    async fn same_status_pending_still_wakes_dispatcher() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("already pending").await.unwrap();

        // Re-registering as Pending (as `create_session_with_first_message`
        // does after `ensure_session_loaded`) must wake the dispatcher even
        // though the status did not change.
        let mut rx = exec.subscribe_dispatch();
        let before = *rx.borrow();
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            rx.changed().await.expect("dispatcher must wake");
        })
        .await
        .expect("dispatcher must be woken by a same-status Pending update");
        assert!(*rx.borrow() > before);
    }

    #[tokio::test]
    async fn illegal_terminal_transition_is_rejected_without_mutating_session() {
        let exec = make_executor(1);
        let session = exec.create_session("illegal transition").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();

        let error = exec
            .update_session_status(&session.id, SessionStatus::Completed)
            .await
            .expect_err("Error -> Completed must not be reported as success");
        assert!(error.to_string().contains("illegal session transition"));
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Error)
        );
    }

    #[tokio::test]
    async fn unknown_status_string_maps_to_error() {
        assert_eq!(
            SessionStatus::from_status_str("bogus"),
            SessionStatus::Error
        );
        assert_eq!(
            SessionStatus::from_status_str("cancelled"),
            SessionStatus::Error
        );
        assert_eq!(
            SessionStatus::from_status_str("paused"),
            SessionStatus::Paused
        );
    }

    #[tokio::test]
    async fn action_completions_buffered_and_drained() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("bg action").await.unwrap();

        assert!(exec.drain_action_completions(&session.id).await.is_empty());

        let _ = exec
            .add_action_completion(&session.id, "action-1 done")
            .await;
        let _ = exec
            .add_action_completion(&session.id, "action-2 failed")
            .await;

        let drained = exec.drain_action_completions(&session.id).await;
        assert_eq!(drained, vec!["action-1 done", "action-2 failed"]);
        assert!(exec.drain_action_completions(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn concurrent_steering_ingress_is_bounded_and_never_truncated() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = Arc::new(SessionExecutor::new(db, tools, 3));
        let session = exec.create_session("concurrent steering").await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..(crate::session::actor::CONTEXT_QUEUE_MAX_ITEMS + 16) {
            let exec = exec.clone();
            let session_id = session.id.clone();
            tasks.spawn(async move {
                exec.add_steering(&session_id, &format!("steering-{index}"))
                    .await
            });
        }
        let mut accepted = 0;
        let mut rejected = 0;
        while let Some(result) = tasks.join_next().await {
            match result.unwrap() {
                Ok(()) => accepted += 1,
                Err(error) => {
                    assert!(error.to_string().contains("deferred"));
                    rejected += 1;
                }
            }
        }
        assert_eq!(accepted, crate::session::actor::CONTEXT_QUEUE_MAX_ITEMS);
        assert_eq!(rejected, 16);
        assert_eq!(exec.get_steering(&session.id).await.len(), accepted);
    }

    #[tokio::test]
    async fn drain_react_context_prioritizes_steering_over_follow_ups() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("context priority").await.unwrap();

        exec.add_follow_up(&session.id, "follow-up").await.unwrap();
        exec.add_steering(&session.id, "steering").await.unwrap();
        let _ = exec
            .add_action_completion(&session.id, "action result")
            .await;

        let batch = exec.drain_react_context(&session.id).await;
        assert!(batch.follow_ups.is_empty());
        assert_eq!(
            batch
                .steering
                .into_iter()
                .map(|item| item.text)
                .collect::<Vec<_>>(),
            vec!["steering"]
        );
        assert_eq!(batch.action_results, vec!["action result"]);

        let batch = exec.drain_react_context(&session.id).await;
        assert_eq!(
            batch
                .follow_ups
                .into_iter()
                .map(|item| item.text)
                .collect::<Vec<_>>(),
            vec!["follow-up"]
        );
        assert!(batch.steering.is_empty());
        assert!(batch.action_results.is_empty());
    }

    /// Phase 7 / D2: re-queue by the same `message_id` must not double-inject.
    #[tokio::test]
    async fn follow_up_same_message_id_is_idempotent() {
        let exec = make_executor(1);
        let session = exec.create_session("dedup").await.unwrap();
        let mid = "msg-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();

        exec.add_follow_up_with_attachments(&session.id, "one", &[], Some(mid.clone()))
            .await
            .unwrap();
        exec.add_follow_up_with_attachments(&session.id, "two", &[], Some(mid.clone()))
            .await
            .unwrap();
        exec.add_answer_with_attachments(&session.id, "answer", &[], Some(mid))
            .await
            .unwrap();

        let follow_ups = exec.get_follow_ups(&session.id).await;
        assert_eq!(follow_ups.len(), 1, "duplicate message_id must be skipped");
        assert_eq!(follow_ups[0].text, "one");
    }

    #[tokio::test]
    async fn steering_same_message_id_is_idempotent() {
        let exec = make_executor(1);
        let session = exec.create_session("steer-dedup").await.unwrap();
        let mid = "msg-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();

        exec.add_steering_with_attachments(&session.id, "first", &[], Some(mid.clone()))
            .await
            .unwrap();
        exec.add_steering_with_attachments(&session.id, "second", &[], Some(mid))
            .await
            .unwrap();

        let steering = exec.get_steering(&session.id).await;
        assert_eq!(steering.len(), 1);
        assert_eq!(steering[0].text, "first");
    }

    #[tokio::test]
    async fn remove_session_clears_action_buffers_and_status_watcher() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("cleanup").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let _ = exec.add_action_completion(&session.id, "stranded").await;
        let rx = exec.subscribe_status(&session.id).await;
        let _ = rx; // a subscriber must not keep the session alive after removal

        exec.remove_session(&session.id).await.unwrap();
        assert_eq!(exec.get_session_state(&session.id).await, None);
        assert!(exec.drain_action_completions(&session.id).await.is_empty());
    }
}
