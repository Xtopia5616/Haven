use haven_common::hooks::OnceHandler;
use haven_common::types::MessageAttachment;
use haven_common::types::RiskLevel;
use haven_memory::Database;
use haven_memory::repositories::sessions::Session as DbSession;
use haven_tools::{ConfirmationResult, ToolResult, ToolsManager, is_silent_action};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, oneshot, watch};
use tokio_util::sync::CancellationToken;

/// Last-resort ceiling for [`SessionExecutor::await_run_finished`]. The
/// normal path is a true oneshot join on handler exit; this bound only
/// guards against a stuck handler so rollback cannot hang forever.
const RUN_EXIT_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Per-session gate: created when a run is claimed, signaled when the
/// dispatcher handler fully exits (`unmark_running` → `cleanup_session_maps`).
/// Rollback takes the receiver so cancel→restore ordering is deterministic.
struct RunExitGate {
    tx: oneshot::Sender<()>,
    rx: Option<oneshot::Receiver<()>>,
}

/// User-queue payload (steering or follow-up). Defined in `haven-common`;
/// re-exported so session code keeps using `crate::session::Supplement` /
/// [`FollowUp`].
pub use haven_common::types::{FollowUp, Supplement};

/// Runner invoked by the dispatcher for each picked session. The closure must
/// perform the ReAct loop for `session_id` and return `Ok(())` on completion.
/// It is responsible for acquiring no permits (dispatcher already does) but
/// is expected to update the session status on completion/error.
pub type RunHandler =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

const DISPATCH_LOG_INTERVAL: u64 = 200; // log every ~20s instead of every 100ms

/// Absolute fail-closed ceiling for an unanswered **scheduled** confirmation
/// (R2). The interactive UI countdown (120s) starts when the dialog is
/// **shown**, not when the request arrives — so queued confirms behind a
/// visible dialog are not starved. This longer backend timer only covers the
/// closed-UI / crashed-frontend case so pending entries cannot live forever.
pub(crate) const SCHEDULED_CONFIRM_ABSOLUTE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Pending,
    Running,
    Paused,
    /// Paused because the `ask` tool is awaiting a human answer. Background-action
    /// completions must NOT auto-wake this state: the model is blocked on the
    /// user, not on action results, and resuming would let the agent continue
    /// (and run tools) without the user's consent. Persisted distinctly as
    /// `paused_awaiting_answer` (Phase 4 / F2) so restart restores the ask
    /// gate without JSON heuristics.
    PausedAwaitingAnswer,
    /// Paused because a safety-gated tool needs user confirmation (Phase 5 / E3).
    /// Background-action completions must NOT auto-wake — same gate as ask.
    /// Persisted as `paused_awaiting_confirm`.
    PausedAwaitingConfirm,
    Completed,
    Error,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Pending => "pending",
            SessionStatus::Running => "running",
            SessionStatus::Paused => "paused",
            SessionStatus::PausedAwaitingAnswer => "paused_awaiting_answer",
            SessionStatus::PausedAwaitingConfirm => "paused_awaiting_confirm",
            SessionStatus::Completed => "completed",
            SessionStatus::Error => "error",
        }
    }

    pub fn from_status_str(s: &str) -> Self {
        match s {
            "pending" => SessionStatus::Pending,
            "running" => SessionStatus::Running,
            "paused" => SessionStatus::Paused,
            "paused_awaiting_answer" => SessionStatus::PausedAwaitingAnswer,
            "paused_awaiting_confirm" => SessionStatus::PausedAwaitingConfirm,
            "completed" => SessionStatus::Completed,
            "error" => SessionStatus::Error,
            // Unknown/corrupt DB statuses must not silently map to Pending:
            // that would auto-resurrect the session on the next dispatcher
            // reload. Error is the safe interpretation (visible, inert).
            other => {
                tracing::warn!(
                    "unknown session status string {:?}; mapping to Error",
                    other
                );
                SessionStatus::Error
            }
        }
    }

    /// True for every pause flavor: scheduling, ask-awaiting, confirm-awaiting.
    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            SessionStatus::Paused
                | SessionStatus::PausedAwaitingAnswer
                | SessionStatus::PausedAwaitingConfirm
        )
    }

    /// True when the pause is blocked on a human answer to an `ask` tool.
    pub fn is_awaiting_answer(&self) -> bool {
        matches!(self, SessionStatus::PausedAwaitingAnswer)
    }

    /// True when the pause is blocked on a safety confirmation (Phase 5 / E3).
    pub fn is_awaiting_confirm(&self) -> bool {
        matches!(self, SessionStatus::PausedAwaitingConfirm)
    }

    /// True when background-action completions must not auto-wake the session.
    pub fn blocks_auto_wake(&self) -> bool {
        matches!(
            self,
            SessionStatus::PausedAwaitingAnswer | SessionStatus::PausedAwaitingConfirm
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionStatus::Completed | SessionStatus::Error)
    }
}

impl serde::Serialize for SessionStatus {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for SessionStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_status_str(&s))
    }
}

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
    /// Follow-up queue (Phase 4 / D1): post-pause user injects and ask
    /// answers (`is_answer`). Historical field name was `supplement_queue`.
    pub follow_up_queue: Vec<FollowUp>,
    /// Steering queue: mid-run user interjections injected before the next
    /// LLM call (step boundary; tools already in flight still finish unless
    /// cancelled — see D3).
    pub steering_queue: Vec<FollowUp>,
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
            status: SessionStatus::from_status_str(&record.status),
            steps: Vec::new(),
            follow_up_queue: Vec::new(),
            steering_queue: Vec::new(),
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

type ConfirmRequestCallback = OnceHandler<
    dyn Fn(
            haven_common::types::ConfirmId,
            String,
            String,
            RiskLevel,
            Value,
            Option<String>,
            u32,
            Option<String>,
        ) + Send
        + Sync,
>;

/// Terminal-failure callback: invoked when the dispatcher marks a session as
/// Error on a path that bypasses the ReAct loop's normal error emission
/// (handler panic / abort). The app layer wires it to emit `session:error` and
/// the `session:updated` secondary broadcast so the UI never misses a terminal
/// transition (busy indicators, status chip, session list refresh).
type SessionErrorCallback = OnceHandler<dyn Fn(String, String) + Send + Sync>;

/// Outcome of resolving a confirm request — enough for the app layer to
/// record a permission grant (tool key + session scope).
#[derive(Debug, Clone)]
pub struct ConfirmResolution {
    pub session_id: Option<String>,
    pub tool_name: String,
    pub tool_input: Value,
}

/// Non-blocking scheduled-tool confirmation pending (R2). Keyed by `conf-*`
/// in `SessionExecutor::scheduled_confirms`. The fired-action consumer emits
/// `confirm:requested` and continues; `resolve_confirmation` (or the
/// `SCHEDULED_CONFIRM_TIMEOUT` timer) later executes or skips the tool.
struct ScheduledConfirmPending {
    /// Owning session for trust-recording; `None` for headless fires.
    session_id: Option<String>,
    tool_name: String,
    tool_args: Value,
    /// Notification title from the scheduled action (outcome toast).
    title: String,
}

/// Outcome toast for a resolved scheduled confirm (`title`, `body`).
type ScheduledConfirmOutcomeCallback = OnceHandler<dyn Fn(String, String) + Send + Sync>;

/// Fired from [`SessionExecutor::cleanup_session_maps`] so sidecars (inference
/// MEMORY dirty maps, etc.) can drop per-session state.
type SessionCleanupCallback = OnceHandler<dyn Fn(String) + Send + Sync>;

/// Cascade-ended child sessions (parent terminal path) that never go through
/// the Tauri `end_session` command — the app layer wires this to emit
/// `session:completed` (+ secondary `session:updated`) so busy chips / lists
/// clear for descendants too. Args: `(session_id, title)`.
type CascadeCompletedCallback = OnceHandler<dyn Fn(String, String) + Send + Sync>;

/// Result of a safety-gated tool execution: the tool result plus the
/// risk level and confirmation state recorded for the step.
pub struct ToolExecution {
    pub result: ToolResult,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

pub struct SessionExecutor {
    db: Arc<Database>,
    tools: Arc<ToolsManager>,
    /// Per-session working set. Keyed by session id; each entry is behind its own
    /// mutex so a slow transition of one session (DB write under the entry lock)
    /// never serializes the other sessions' operations on a global lock. The map
    /// lock itself is only held for lookup/insert/remove (never while
    /// awaiting an entry lock), keeping the lock order acyclic.
    sessions: Arc<Mutex<HashMap<String, Arc<Mutex<SessionInfo>>>>>,
    running_sessions: Arc<Mutex<HashSet<String>>>,
    /// Completion gates for in-flight dispatcher runs. Inserted in
    /// `try_claim_pending` (covers the claim→spawn window) and signaled from
    /// `cleanup_session_maps` when the handler releases the running slot.
    run_exit: Arc<Mutex<HashMap<String, RunExitGate>>>,
    semaphore: Arc<Semaphore>,
    /// Current configured session concurrency ceiling. Kept separate from the
    /// semaphore's live permit count so `set_max_concurrent` can compute the
    /// delta when the user changes the setting at runtime.
    max_concurrent: std::sync::atomic::AtomicUsize,
    /// Tracks the semaphore permit held by each running session's handler.
    /// When a session is paused, its permit is dropped so the dispatcher slot
    /// is freed. On resume the dispatcher re-acquires a permit.
    session_permits: Arc<Mutex<HashMap<String, OwnedSemaphorePermit>>>,
    /// FIFO dispatch queue: session ids in the order they became `Pending`
    /// (insertion order ≈ creation order for fresh sessions). The dispatcher
    /// claims from the front, so queued sessions run in submission order instead
    /// of the nondeterministic `HashMap` iteration order a full scan would
    /// produce. Entries are (re-)enqueued on every transition to Pending and
    /// removed on terminal states / claims / explicit removal.
    pending_queue: Arc<Mutex<VecDeque<String>>>,
    /// Cancellation tokens for each session, used to abort in-flight LLM calls.
    session_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    /// Per-session level-triggered status watchers: the ReAct loop blocks on the
    /// receiver (`subscribe_status`) instead of polling, and a transition
    /// that lands between a state read and the wait is never lost (unlike the
    /// edge-triggered Notify it replaced, the stored value makes `changed()`
    /// resolve immediately when the value moved).
    status_tx: Arc<Mutex<HashMap<String, watch::Sender<SessionStatus>>>>,
    /// Dispatch wake counter: incremented on every transition to Pending.
    /// The dispatcher waits on a receiver of this watch, so a session that
    /// becomes Pending right after a failed claim still wakes it (no missed
    /// notification, no polling fallback).
    dispatch_tx: watch::Sender<u64>,
    /// Per-session buffer of completed background-action results (system
    /// inject / action_results — not a user queue). Delivered to the ReAct
    /// loop as context at the next step start.
    action_completions: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Explicit ask-awaiting flag per session (Phase 4 / C5). Mirrored into
    /// `ReActSnapshot.awaiting_answer` on pause and restored on resume.
    awaiting_answer: Arc<Mutex<HashMap<String, crate::types::AskPending>>>,
    /// Explicit confirm-awaiting batch per session (Phase 5 / E3). Mirrored
    /// into `ReActSnapshot.awaiting_confirm` on pause and restored on resume.
    awaiting_confirm: Arc<Mutex<HashMap<String, crate::types::ConfirmPending>>>,
    /// Pending scheduled-tool confirmations (R2), keyed by the `conf-*` id
    /// reported in `confirm:requested`. ReAct sessions use `awaiting_confirm`
    /// (pause/continue) instead — tool futures never block.
    scheduled_confirms:
        Arc<Mutex<HashMap<haven_common::types::ConfirmId, ScheduledConfirmPending>>>,
    /// Coordinated lifecycle for checkpointed stream text (checkpoint /
    /// promote / discard), shared with the agent loop and the end/rollback
    /// paths.
    pub partials: Arc<crate::partial::PartialStore>,
    pub on_confirm_request: ConfirmRequestCallback,
    /// Wired by [`crate::layer::AgentLayer::start`] to surface scheduled
    /// confirm outcomes as notifications.
    pub on_scheduled_confirm_outcome: ScheduledConfirmOutcomeCallback,
    /// Wired by [`crate::layer::AgentLayer::start`] to clear inference
    /// mid-run MEMORY bookkeeping when a session leaves the working set.
    pub on_session_cleanup: SessionCleanupCallback,
    /// Wired by [`crate::layer::AgentLayer::start`] for cascade child ends.
    pub on_cascade_completed: CascadeCompletedCallback,
    /// Session ids that have successfully spawned at least one peer child in
    /// this process. Used to skip inbox registry I/O on terminal cleanup for
    /// the common leaf-session path.
    sessions_with_children: Mutex<HashSet<String>>,
    /// Notification body truncation for scheduled-tool outcomes (matches
    /// `ContextLimitsConfig::notification_summary_chars`).
    pub notification_summary_chars: AtomicUsize,
    pub on_session_error: SessionErrorCallback,
}

mod dispatcher;
mod queues;
mod status;
mod tool_runner;
pub(crate) use tool_runner::ActionStepPersistenceError;

pub(crate) use queues::ReactContextBatch;

impl SessionExecutor {
    pub fn new(db: Arc<Database>, tools: Arc<ToolsManager>, max_concurrent: usize) -> Self {
        Self {
            partials: Arc::new(crate::partial::PartialStore::new(db.clone())),
            db,
            tools,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            running_sessions: Arc::new(Mutex::new(HashSet::new())),
            run_exit: Arc::new(Mutex::new(HashMap::new())),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent: std::sync::atomic::AtomicUsize::new(max_concurrent),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            session_permits: Arc::new(Mutex::new(HashMap::new())),
            session_cancellations: Arc::new(Mutex::new(HashMap::new())),
            status_tx: Arc::new(Mutex::new(HashMap::new())),
            dispatch_tx: watch::channel(0).0,
            action_completions: Arc::new(Mutex::new(HashMap::new())),
            awaiting_answer: Arc::new(Mutex::new(HashMap::new())),
            awaiting_confirm: Arc::new(Mutex::new(HashMap::new())),
            scheduled_confirms: Arc::new(Mutex::new(HashMap::new())),
            on_confirm_request: OnceHandler::new(),
            on_scheduled_confirm_outcome: OnceHandler::new(),
            on_session_cleanup: OnceHandler::new(),
            on_cascade_completed: OnceHandler::new(),
            sessions_with_children: Mutex::new(HashSet::new()),
            notification_summary_chars: AtomicUsize::new(800),
            on_session_error: OnceHandler::new(),
        }
    }

    pub fn set_notification_summary_chars(&self, chars: usize) {
        self.notification_summary_chars
            .store(chars.max(64), Ordering::Relaxed);
    }

    /// Record that `parent_session_id` spawned a peer child (in-process hint
    /// for cascade skip).
    pub async fn mark_has_children(&self, parent_session_id: &str) {
        self.sessions_with_children
            .lock()
            .await
            .insert(parent_session_id.to_string());
    }

    async fn may_have_children(&self, session_id: &str) -> bool {
        self.sessions_with_children
            .lock()
            .await
            .contains(session_id)
    }

    async fn clear_has_children(&self, session_id: &str) {
        self.sessions_with_children.lock().await.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        // The panic path bypasses the ReAct loop's event emission, so the
        // wired on_session_error callback must fire — otherwise the UI would
        // never learn about the terminal transition.
        //
        // A `std::sync::Mutex` (not a tokio mutex) so the synchronous
        // callback can lock it directly; `try_lock().unwrap()` on a tokio
        // mutex panicked whenever the poll loop below happened to hold the
        // lock while the dispatcher fired the callback.
        let notified = Arc::new(std::sync::Mutex::new(None::<(String, String)>));
        let nt = notified.clone();
        exec.on_session_error
            .set(Arc::new(move |session_id: String, reason: String| {
                *nt.lock().unwrap() = Some((session_id, reason));
            }));

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
        let mut db_status = String::new();
        for _ in 0..100 {
            db_status = exec
                .db
                .get_session(&session.id)
                .unwrap()
                .map(|t| t.status)
                .unwrap_or_default();
            if db_status == "error" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert_eq!(db_status, "error");
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
            if !exec.running_sessions.lock().await.contains(&session.id)
                && exec.get_session_state(&session.id).await.is_none()
            {
                released = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(released, "running slot / working set must be released");
        // The wired failure callback fired with the session id and a panic
        // reason (the UI clears its busy set from this signal). Poll: the
        // callback runs right after the DB write in the dispatcher's spawned
        // session.
        let mut seen = None;
        for _ in 0..100 {
            seen = notified.lock().unwrap().clone();
            if seen.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let (seen_id, seen_reason) = seen.expect("on_session_error callback must fire");
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
            if in_handler.load(Ordering::SeqCst) == 1
                && exec.running_sessions.lock().await.contains(&session.id)
            {
                seen = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(seen, "handler should be running and hold the slot");

        let exec_wait = exec.clone();
        let sid = session.id.clone();
        let wait_task = tokio::spawn(async move {
            exec_wait.await_run_finished(&sid).await;
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
        assert!(!exec.running_sessions.lock().await.contains(&session.id));
    }

    /// `await_run_finished` is a no-op when the session was never claimed.
    #[tokio::test]
    async fn await_run_finished_noop_when_not_running() {
        let exec = make_executor(1);
        let session = exec.create_session("idle").await.unwrap();
        // Do not start a dispatcher — session stays Pending, no run_exit gate.
        tokio::time::timeout(
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
        assert!(exec.running_sessions.lock().await.contains(&session.id));
        let db_status = exec
            .db
            .get_session(&session.id)
            .unwrap()
            .map(|t| t.status)
            .unwrap_or_default();
        assert_eq!(db_status, "running");

        // No second claim while the first handler holds the slot.
        assert!(exec.try_claim_pending().await.is_none());
    }

    /// A Pending session already present in `running_sessions` (claim→spawn
    /// window) must not be claimed again — otherwise the dispatcher spawns a
    /// duplicate ReAct loop. The stale queue entry is consumed on the skip; a
    /// later Pending transition re-enqueues the session once the handler
    /// exits and `unmark_running` clears the set (pause is exit-based).
    #[tokio::test]
    async fn try_claim_pending_skips_session_already_in_running_set() {
        let exec = make_executor(2);
        let session = exec.create_session("t1").await.unwrap();
        exec.running_sessions
            .lock()
            .await
            .insert(session.id.clone());

        assert!(exec.try_claim_pending().await.is_none());

        // Once the handler releases the slot, the session only becomes claimable
        // again after it re-enters the FIFO queue (a fresh Pending transition).
        exec.running_sessions.lock().await.remove(&session.id);
        assert!(exec.try_claim_pending().await.is_none());
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

    /// `set_max_concurrent` must reclaim permits on lowering (not return them
    /// to the semaphore — that would be a no-op) and must not overshoot on a
    /// later raise. The effective ceiling is measured by how many concurrent
    /// dispatcher acquisitions succeed without blocking.
    #[tokio::test]
    async fn set_max_concurrent_reclaims_and_does_not_overshoot() {
        let exec = make_executor(4);
        exec.set_max_concurrent(1);
        // Idle pool: exactly one permit may be acquired without waiting.
        let first = exec.semaphore.clone().try_acquire_owned();
        assert!(
            first.is_ok(),
            "one permit must be available after lowering to 1"
        );
        let second = exec.semaphore.clone().try_acquire_owned();
        assert!(
            second.is_err(),
            "lowering must reclaim unused permits (no-op reclaim would leave 3 free)"
        );
        drop(first.unwrap());
        // Raise back to 3: available permits must be 3, not 3 + stale 3.
        exec.set_max_concurrent(3);
        let mut held = Vec::new();
        for _ in 0..3 {
            match exec.semaphore.clone().try_acquire_owned() {
                Ok(p) => held.push(p),
                Err(_) => break,
            }
        }
        assert_eq!(
            held.len(),
            3,
            "raise after lower must yield exactly 3 permits"
        );
        assert!(
            exec.semaphore.clone().try_acquire_owned().is_err(),
            "no extra permits may leak from the lower→raise cycle"
        );
        drop(held);
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
        assert!(!exec.running_sessions.lock().await.contains(&session.id));
    }

    // ─── Data-layer tests (no dispatcher required) ───

    fn temp_db() -> Arc<Database> {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        Arc::new(Database::open(&p).unwrap())
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
        // Insert a token as the dispatcher would, so end_session can trigger it
        let real_token = CancellationToken::new();
        let clone = real_token.clone();
        exec.session_cancellations
            .lock()
            .await
            .insert(session.id.clone(), clone);
        assert!(!real_token.is_cancelled());
        let status = exec.end_session(&session.id).await.unwrap();
        assert_eq!(status, SessionStatus::Completed);
        assert!(real_token.is_cancelled());
        // end_session removes the session from the working set entirely.
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

        let real_token = CancellationToken::new();
        let clone = real_token.clone();
        exec.session_cancellations
            .lock()
            .await
            .insert(session.id.clone(), clone);

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
    async fn add_and_get_supplements() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        exec.add_supplement(&session.id, "extra context 1")
            .await
            .unwrap();
        exec.add_supplement(&session.id, "extra context 2")
            .await
            .unwrap();
        let drained: Vec<String> = exec
            .get_supplements(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["extra context 1", "extra context 2"]);
        assert!(exec.get_supplements(&session.id).await.is_empty());
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
        exec.add_supplement(&session.id, "plain context")
            .await
            .unwrap();
        let drained = exec.get_supplements(&session.id).await;
        assert_eq!(drained.len(), 2);
        assert!(drained[0].is_answer, "first message is an ask reply");
        assert_eq!(drained[0].text, "the answer");
        assert!(!drained[1].is_answer, "plain supplement is not an answer");
    }

    #[tokio::test]
    async fn add_and_get_supplements_with_attachments() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("test").await.unwrap();
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
        exec.add_supplement_with_attachments(&session.id, "看图", std::slice::from_ref(&att), None)
            .await
            .unwrap();
        let drained = exec.get_supplements(&session.id).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text, "看图");
        assert_eq!(drained[0].attachments, vec![att]);
        assert!(exec.get_supplements(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn add_supplement_nonexistent_session_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let result = exec.add_supplement("nonexistent", "ctx").await;
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
        exec.running_sessions
            .lock()
            .await
            .insert(session.id.clone());
        let sem = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = sem.clone().acquire_owned().await.unwrap();
        exec.session_permits
            .lock()
            .await
            .insert(session.id.clone(), permit);
        exec.session_cancellations
            .lock()
            .await
            .insert(session.id.clone(), CancellationToken::new());

        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        assert!(!exec.running_sessions.lock().await.contains(&session.id));
        assert!(exec.session_permits.lock().await.get(&session.id).is_none());
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
    async fn awaiting_answer_pause_is_distinct_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("ask me").await.unwrap();

        // The ask pause path pauses in PausedAwaitingAnswer.
        exec.update_session_status(&session.id, SessionStatus::PausedAwaitingAnswer)
            .await
            .unwrap();
        assert_eq!(
            exec.get_session_state(&session.id).await,
            Some(SessionStatus::PausedAwaitingAnswer)
        );
        // Both pause flavors report is_paused; only the answer variant
        // reports is_awaiting_answer.
        let state = exec.get_session_state(&session.id).await.unwrap();
        assert!(state.is_paused());
        assert!(state.is_awaiting_answer());
        // Phase 4 / F2: awaiting persists as a distinct DB/wire status.
        assert_eq!(state.as_str(), "paused_awaiting_answer");

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
    async fn plain_pause_is_not_awaiting_answer() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("pause me").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let state = exec.get_session_state(&session.id).await.unwrap();
        assert!(state.is_paused());
        assert!(!state.is_awaiting_answer());
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
        assert_eq!(
            SessionStatus::from_status_str("paused_awaiting_answer"),
            SessionStatus::PausedAwaitingAnswer
        );
        assert_eq!(
            SessionStatus::PausedAwaitingAnswer.as_str(),
            "paused_awaiting_answer"
        );
    }

    #[tokio::test]
    async fn action_completions_buffered_and_drained() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("bg action").await.unwrap();

        assert!(exec.drain_action_completions(&session.id).await.is_empty());

        exec.add_action_completion(&session.id, "action-1 done")
            .await;
        exec.add_action_completion(&session.id, "action-2 failed")
            .await;

        let drained = exec.drain_action_completions(&session.id).await;
        assert_eq!(drained, vec!["action-1 done", "action-2 failed"]);
        assert!(exec.drain_action_completions(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn drain_react_context_prioritizes_steering_over_follow_ups() {
        let db = temp_db();
        let tools = Arc::new(ToolsManager::new());
        let exec = SessionExecutor::new(db, tools, 3);
        let session = exec.create_session("context priority").await.unwrap();

        exec.add_follow_up(&session.id, "follow-up").await.unwrap();
        exec.add_steering(&session.id, "steering").await.unwrap();
        exec.add_action_completion(&session.id, "action result")
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
        exec.update_session_status(&session.id, SessionStatus::PausedAwaitingAnswer)
            .await
            .unwrap();
        exec.add_action_completion(&session.id, "stranded").await;
        let rx = exec.subscribe_status(&session.id).await;
        let _ = rx; // a subscriber must not keep the session alive after removal

        exec.remove_session(&session.id).await;
        assert_eq!(exec.get_session_state(&session.id).await, None);
        assert!(exec.drain_action_completions(&session.id).await.is_empty());
    }
}
