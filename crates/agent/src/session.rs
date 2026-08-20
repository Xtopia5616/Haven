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
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, watch};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// A user message queued for injection into the ReAct loop (supplement or
/// steering). Defined in `haven-common` (the shared types layer); re-exported
/// here so session code keeps using `crate::session::Supplement`.
pub use haven_common::types::Supplement;

/// Runner invoked by the dispatcher for each picked session. The closure must
/// perform the ReAct loop for `session_id` and return `Ok(())` on completion.
/// It is responsible for acquiring no permits (dispatcher already does) but
/// is expected to update the session status on completion/error.
pub type RunHandler =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

const DISPATCH_LOG_INTERVAL: u64 = 200; // log every ~20s instead of every 100ms

/// Bounded wait for a user confirmation. A pending confirmation whose
/// frontend answer never arrives (window closed, dialog lost, scheduled action fired
/// with no UI attached) must fail CLOSED instead of blocking the session — or,
/// for the scheduled-action path, the whole sequential scheduled-action consumer — for an
/// unbounded time. The bound is short enough that a headless queue recovers
/// quickly, yet still gives an interactive user a comfortable window to
/// approve/deny a dialog.
const CONFIRM_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq)]
pub enum SessionStatus {
    Pending,
    Running,
    Paused,
    /// Paused because the `ask` tool is awaiting a human answer. Background-action
    /// completions must NOT auto-wake this state: the model is blocked on the
    /// user, not on action results, and resuming would let the agent continue
    /// (and run tools) without the user's consent. Serialized as "paused" so
    /// the wire/DB format is unchanged.
    PausedAwaitingAnswer,
    Completed,
    Error,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Pending => "pending",
            SessionStatus::Running => "running",
            SessionStatus::Paused | SessionStatus::PausedAwaitingAnswer => "paused",
            SessionStatus::Completed => "completed",
            SessionStatus::Error => "error",
        }
    }

    pub fn from_status_str(s: &str) -> Self {
        match s {
            "pending" => SessionStatus::Pending,
            "running" => SessionStatus::Running,
            "paused" => SessionStatus::Paused,
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

    /// True for both pause flavors: scheduling pause and ask-awaiting pause.
    pub fn is_paused(&self) -> bool {
        matches!(
            self,
            SessionStatus::Paused | SessionStatus::PausedAwaitingAnswer
        )
    }

    /// True when the pause is blocked on a human answer to an `ask` tool.
    pub fn is_awaiting_answer(&self) -> bool {
        matches!(self, SessionStatus::PausedAwaitingAnswer)
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
    pub supplement_queue: Vec<Supplement>,
    /// Steering queue: items that should interrupt the current tool sequence
    /// and be injected as context immediately (refine 搂1.2).
    pub steering_queue: Vec<Supplement>,
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
            supplement_queue: Vec::new(),
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

type ConfirmRequestCallback =
    OnceHandler<dyn Fn(haven_common::types::ConfirmId, String, String, RiskLevel) + Send + Sync>;

/// Terminal-failure callback: invoked when the dispatcher marks a session as
/// Error on a path that bypasses the ReAct loop's normal error emission
/// (handler panic / abort). The app layer wires it to emit `session:error` and
/// the `session:updated` secondary broadcast so the UI never misses a terminal
/// transition (busy indicators, status chip, session list refresh).
type SessionErrorCallback = OnceHandler<dyn Fn(String, String) + Send + Sync>;

/// A pending safety-gateway confirmation wait, keyed by a generated step id
/// in `SessionExecutor::confirm_waits`. The executing session blocks on the oneshot
/// receiver until the frontend resolves the confirmation via
/// `resolve_confirmation` (or the session is cancelled).
struct ConfirmWait {
    risk_level: RiskLevel,
    /// The session (if any) the tool call belonged to. Resolving the wait
    /// returns it so the app layer can record a "trust for this conversation"
    /// approval against the right session id.
    session_id: Option<String>,
    tx: tokio::sync::oneshot::Sender<bool>,
}

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
    /// Per-session buffer of completed background-action results, delivered to the
    /// ReAct loop as context at the next step start. Kept separate from the
    /// steering queue so action output is never mistaken for a user reply (the
    /// `ask` pause path keys resume off the steering queue, which now holds
    /// only genuine user interjections).
    action_completions: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Pending user confirmations for safety-gated tool calls, keyed by the
    /// generated step id reported in the `confirm:requested` event.
    confirm_waits: Arc<Mutex<HashMap<haven_common::types::ConfirmId, ConfirmWait>>>,
    /// Coordinated lifecycle for checkpointed stream text (checkpoint /
    /// promote / discard), shared with the agent loop and the end/rollback
    /// paths.
    pub partials: Arc<crate::partial::PartialStore>,
    pub on_confirm_request: ConfirmRequestCallback,
    pub on_session_error: SessionErrorCallback,
}

impl SessionExecutor {
    pub fn new(db: Arc<Database>, tools: Arc<ToolsManager>, max_concurrent: usize) -> Self {
        Self {
            partials: Arc::new(crate::partial::PartialStore::new(db.clone())),
            db,
            tools,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            running_sessions: Arc::new(Mutex::new(HashSet::new())),
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            max_concurrent: std::sync::atomic::AtomicUsize::new(max_concurrent),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            session_permits: Arc::new(Mutex::new(HashMap::new())),
            session_cancellations: Arc::new(Mutex::new(HashMap::new())),
            status_tx: Arc::new(Mutex::new(HashMap::new())),
            dispatch_tx: watch::channel(0).0,
            action_completions: Arc::new(Mutex::new(HashMap::new())),
            confirm_waits: Arc::new(Mutex::new(HashMap::new())),
            on_confirm_request: OnceHandler::new(),
            on_session_error: OnceHandler::new(),
        }
    }

    pub async fn create_session(&self, input: &str) -> anyhow::Result<SessionInfo> {
        self.create_session_with_summary(input, input).await
    }

    pub async fn create_session_with_summary(
        &self,
        input: &str,
        summary: &str,
    ) -> anyhow::Result<SessionInfo> {
        let record = self.db.create_session(input, input)?;
        let mut session = SessionInfo::from_db_record(&record);
        // The DB record was created with `input` as its transcript, but the
        // caller may have a distinct classifier-generated summary — overlay
        // it after construction so we keep the constructor single-purpose.
        session.summary = summary.into();
        let mut sessions = self.sessions.lock().await;
        sessions.insert(session.id.clone(), Arc::new(Mutex::new(session.clone())));

        // FIFO dispatch: queue the session before waking so the dispatcher's
        // first claim finds it at the tail, in submission order.
        self.enqueue_pending(&session.id).await;

        // Wake the dispatcher so it picks up this Pending session immediately.
        self.wake_dispatcher();
        Ok(session)
    }

    /// Bump the dispatcher wake counter. Level-triggered: a bump that lands
    /// between a failed claim and the dispatcher's wait resolves `changed()`
    /// immediately, so no transition is ever lost.
    fn wake_dispatcher(&self) {
        self.dispatch_tx.send_modify(|c| *c += 1);
    }

    /// Enqueue a session id at the tail of the FIFO dispatch queue (idempotent:
    /// a session already queued is not duplicated).
    async fn enqueue_pending(&self, session_id: &str) {
        let mut q = self.pending_queue.lock().await;
        if !q.iter().any(|t| t == session_id) {
            q.push_back(session_id.to_string());
        }
    }

    /// Remove a session id from the FIFO dispatch queue (no-op when absent).
    async fn dequeue_pending(&self, session_id: &str) {
        let mut q = self.pending_queue.lock().await;
        q.retain(|t| t != session_id);
    }

    /// Subscribe to dispatch wake signals (a `watch` receiver on the wake
    /// counter). The receiver resolves as soon as the counter moved past the
    /// version it has seen, so it must be created before the first claim.
    pub fn subscribe_dispatch(&self) -> watch::Receiver<u64> {
        self.dispatch_tx.subscribe()
    }

    /// Adjust the session concurrency ceiling at runtime (settings save). The
    /// semaphore permit count is updated by the delta:
    /// - Raising: `add_permits` grows the cap immediately; queued sessions start
    ///   as soon as a permit is free.
    /// - Lowering: unused permits are reclaimed best-effort. Permits held by
    ///   in-flight sessions cannot be revoked (they finish and release naturally),
    ///   so the effective concurrency may stay above the new target until the
    ///   current sessions complete — never forcibly cancelled.
    pub fn set_max_concurrent(&self, new_max: usize) {
        let new_max = new_max.max(1);
        let cur = self
            .max_concurrent
            .load(std::sync::atomic::Ordering::Relaxed);
        if new_max == cur {
            return;
        }
        if new_max > cur {
            self.semaphore.add_permits(new_max - cur);
        } else {
            let mut reclaimed = 0usize;
            while reclaimed < cur - new_max {
                match self.semaphore.clone().try_acquire_owned() {
                    Ok(p) => {
                        // `forget` (not `drop`): dropping a permit returns it
                        // to the semaphore, which would make the reclaim loop
                        // a no-op and let the ceiling drift (a later raise
                        // would then overshoot by the stale delta).
                        p.forget();
                        reclaimed += 1;
                    }
                    Err(_) => break,
                }
            }
            if reclaimed < cur - new_max {
                tracing::warn!(
                    "set_max_concurrent: reclaimed {}/{} permits; in-flight sessions keep \
                     the effective concurrency above the new target until they finish",
                    reclaimed,
                    cur - new_max
                );
            }
        }
        self.max_concurrent
            .store(new_max, std::sync::atomic::Ordering::Relaxed);
        tracing::info!("session concurrency ceiling: {} -> {}", cur, new_max);
    }

    /// Remove the session from the cross-session messaging registry (graceful
    /// shutdown: `agents_list` no longer shows it). Mailboxes and archives
    /// are kept, so late messages remain deliverable (reported offline) and
    /// the history survives a later re-registration. Fire-and-forget.
    fn unregister_from_inbox(session_id: &str) {
        let sid = session_id.to_string();
        let sid_err = sid.clone();
        tokio::spawn(async move {
            let bus = haven_tools::inbox::InboxBus::default_root();
            if let Ok(Err(e)) = tokio::task::spawn_blocking(move || bus.unregister(&sid)).await {
                tracing::debug!("messaging unregister failed for {sid_err}: {e}");
            }
        });
    }

    /// Persist a session status to the DB with a small number of retries. SQLite
    /// writes through the blocking pool can transiently fail with SQLITE_BUSY;
    /// a short retry turns that into extra latency instead of a diverged
    /// memory/DB state. Returns the last failure after exhausting the retries.
    async fn persist_status(
        db: &Arc<Database>,
        session_id: &str,
        status: &str,
    ) -> anyhow::Result<()> {
        let mut last_err = None;
        for attempt in 0..3 {
            let db = db.clone();
            let tid = session_id.to_string();
            let st = status.to_string();
            match db
                .run_blocking(move |db| db.update_session_status(&tid, &st))
                .await
            {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last_err = Some(e);
                    if attempt < 2 {
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("status persist failed")))
    }

    /// Spawn the background dispatcher. Whenever a semaphore permit is free
    /// and a `Pending` session exists, the dispatcher calls `handler(session_id)`.
    /// The handler must perform the ReAct loop and finalize the session status.
    pub fn start_dispatcher(self: Arc<Self>, handler: RunHandler) {
        let exec = self.clone();
        tokio::spawn(async move {
            // Pick up sessions that were still Pending when the app stopped so
            // queued work survives a restart instead of being stranded in
            // the DB (the in-memory working set is empty on a fresh start).
            let reloaded = exec.load_pending_sessions().await;
            if reloaded > 0 {
                tracing::info!(
                    "dispatcher reloaded {} pending session(s) from previous run",
                    reloaded
                );
            }
            // Subscribe BEFORE the first claim so a Pending transition that
            // lands between a failed claim and the wait below is never lost:
            // `changed()` resolves immediately when the counter moved.
            let mut dispatch_rx = exec.subscribe_dispatch();
            let mut log_counter: u64 = 0;
            loop {
                log_counter += 1;
                if log_counter.is_multiple_of(DISPATCH_LOG_INTERVAL) {
                    tracing::debug!("dispatcher heartbeat (iter {})", log_counter);
                }
                let permit = match exec.semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::error!("session semaphore closed");
                        return;
                    }
                };

                let session_id = exec.try_claim_pending().await;
                let Some(session_id) = session_id else {
                    drop(permit);
                    // Wait for the next Pending transition, then re-claim.
                    let _ = dispatch_rx.changed().await;
                    continue;
                };

                // Register the permit so pause/cancel can release it.
                {
                    let mut permits = exec.session_permits.lock().await;
                    permits.insert(session_id.clone(), permit);
                }
                // Create a cancellation token for this session. Use entry() so a
                // token already created (and possibly cancelled) by end_session
                // during the claim window is never clobbered with a fresh one.
                {
                    let mut cancels = exec.session_cancellations.lock().await;
                    cancels
                        .entry(session_id.clone())
                        .or_insert_with(CancellationToken::new);
                }

                let exec_inner = exec.clone();
                let handler_inner = handler.clone();
                tracing::info!(session_id = %session_id, "dispatcher spawning handler");
                // Run the handler on a nested session so a panic in the ReAct
                // loop is contained: the JoinHandle turns it into an Err and
                // the cleanup below still runs. Without this, a panicked
                // handler would skip the Error marking and unmark_running,
                // leaving the session stuck in Running (memory + DB) forever.
                //
                // The handler runs inside a ses-level span so every log line
                // emitted by the ReAct loop (agent, compactor, title,
                // inference) carries the session_id even when the call site does
                // not name it — parallel sessions stay distinguishable in logs.
                let session_span = tracing::info_span!("run_session", session_id = %session_id);
                tokio::spawn(async move {
                    let result =
                        tokio::spawn(handler_inner(session_id.clone()).instrument(session_span))
                            .await;
                    let failed = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(e)) => Some(format!("handler failed: {}", e)),
                        Err(join_err) if join_err.is_panic() => {
                            Some(format!("handler panicked: {}", join_err))
                        }
                        Err(join_err) => Some(format!("handler aborted: {}", join_err)),
                    };
                    if let Some(reason) = failed {
                        tracing::error!(session_id = %session_id, "dispatcher session {} {}", session_id, reason);
                        let _ = exec_inner
                            .update_session_status(&session_id, SessionStatus::Error)
                            .await;
                        // The ReAct loop errored out: kill any background actions
                        // the session spawned so their children cannot leak.
                        exec_inner.cancel_session_actions(&session_id).await;
                        // The ReAct loop never emitted a terminal event for
                        // this failure (panic bypasses its error path), so
                        // surface it through the wired callback — otherwise
                        // the UI keeps the session in its busy set and the chip
                        // would stay stuck on "waiting" forever.
                        if let Some(cb) = exec_inner.on_session_error.snap() {
                            cb(session_id.clone(), reason);
                        }
                    }
                    exec_inner.unmark_running(&session_id).await;
                });
            }
        });
    }

    /// Claim the oldest `Pending` session from the FIFO dispatch queue, flip it
    /// to `Running` (memory + DB) and insert it into the running set. Returns
    /// the claimed session id, or `None` if nothing is dispatchable.
    ///
    /// Stale queue entries are skipped without re-queuing: a session whose
    /// status moved away from Pending (paused, cancelled, ended) or whose
    /// handler is still alive (a supplement flipped it Paused → Pending —
    /// its own loop picks up the supplement via the status watcher, and only
    /// the dispatcher inserts into `running_sessions`, so a re-claim would be a
    /// double-dispatch) must not be started again.
    ///
    /// The status flip happens under the session's own entry lock (never under
    /// the map lock), so a slow transition of another session cannot block the
    /// claim. The DB write precedes the memory flip: on a persistent DB
    /// failure the claim is aborted before memory and the running set diverge,
    /// keeping the memory/DB error policy consistent with `update_session_status`.
    /// The session is re-queued at the tail so it is not lost.
    async fn try_claim_pending(&self) -> Option<String> {
        loop {
            let session_id = {
                let mut q = self.pending_queue.lock().await;
                match q.pop_front() {
                    Some(id) => id,
                    None => return None,
                }
            };
            let entry = { self.sessions.lock().await.get(&session_id).cloned() };
            let Some(entry) = entry else {
                // Session removed between enqueue and claim (end_session /
                // remove_session / terminal cleanup): stale queue entry.
                continue;
            };
            let mut session = entry.lock().await;
            if session.status != SessionStatus::Pending {
                // Status moved (paused, ended, errored) while queued: no
                // longer dispatchable, and the transition path already
                // re-enqueued it if it became Pending again.
                continue;
            }
            // The `running_sessions` check prevents double-dispatch: a session whose
            // handler is still alive (e.g. blocked in a pause-wait after a
            // supplement flipped it Paused → Pending) must not be claimed
            // again — its own loop picks up the supplement via the status
            // watcher. Only the dispatcher inserts into this set, so the
            // check-then-insert below cannot race.
            if self.running_sessions.lock().await.contains(&session_id) {
                continue;
            }
            if let Err(e) = Self::persist_status(&self.db, &session_id, "running").await {
                tracing::error!(
                    "try_claim_pending: DB persist failed for session {}; re-queuing it: {}",
                    session_id,
                    e
                );
                self.pending_queue.lock().await.push_back(session_id);
                return None;
            }
            session.status = SessionStatus::Running;
            session.updated_at = chrono::Utc::now().to_rfc3339();
            self.running_sessions
                .lock()
                .await
                .insert(session_id.clone());
            tracing::debug!("try_claim_pending: claimed session {}", session_id);
            return Some(session_id);
        }
    }

    /// Remove a session from the running set. Terminal status updates are
    /// performed by the handler / agent loop. Also removes terminal-status
    /// sessions from the in-memory list so `try_claim_pending` only counts
    /// active (Pending / Running) sessions.
    async fn unmark_running(&self, session_id: &str) {
        self.cleanup_session_maps(session_id).await;
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return;
        };
        let status = entry.lock().await.status.clone();
        if status == SessionStatus::Error || status == SessionStatus::Completed {
            tracing::debug!(
                "session {} unmark_running: {:?}, removing from list",
                session_id,
                status
            );
            self.dequeue_pending(session_id).await;
            self.sessions.lock().await.remove(session_id);
        } else {
            // The handler has exited (unmark_running runs after the handler
            // future completes). A session left Pending here is claimable again
            // — re-enqueue it, or it would strand forever: the FIFO claim
            // consumed its queue entry when it skipped it while the handler
            // was still alive, and no later Pending transition re-queues it.
            // The alive-handler case is safe: a session whose handler is truly
            // still running is claimed only after `running_sessions` re-check.
            if status == SessionStatus::Pending {
                self.enqueue_pending(session_id).await;
                // The dispatcher may be parked on its wake channel after a
                // failed claim; re-queueing alone would not wake it.
                self.wake_dispatcher();
            }
            tracing::debug!(
                "session {} unmark_running: {:?}, keeping in list",
                session_id,
                status
            );
        }
    }

    pub async fn running_count(&self) -> usize {
        self.running_sessions.lock().await.len()
    }

    /// Return a list of currently running session IDs. Used by rollback to wait
    /// until a stopped session's handler has fully released its slot.
    pub async fn running_actions_list(&self) -> Vec<String> {
        self.running_sessions.lock().await.iter().cloned().collect()
    }

    pub async fn add_supplement(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_supplement_with_attachments(session_id, text, &[], None)
            .await
    }

    /// `message_id` is the id of the persisted user message row this
    /// supplement's words were stored under (persisted at submit time by
    /// `process_input`). The ReAct loop's `push_user_context` creates the
    /// anchoring thought-step row under that same id, so review/rollback
    /// resolve the step by id. `None` when no row was persisted.
    pub async fn add_supplement_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.push_supplement(session_id, text, attachments, false, message_id)
            .await
    }

    /// Queue a supplement that is the user's reply to a pending `ask`
    /// question. Injected as a paired answer on resume so the model no
    /// longer sees the old question as open.
    pub async fn add_answer_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        self.push_supplement(session_id, text, attachments, true, message_id)
            .await
    }

    async fn push_supplement(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        is_answer: bool,
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            anyhow::bail!("session '{}' not found", session_id)
        };
        let mut session = entry.lock().await;
        let supplement = if is_answer {
            Supplement::answer_with_message_id(text, attachments.to_vec(), message_id)
        } else {
            Supplement::new_with_message_id(text, attachments.to_vec(), message_id)
        };
        session.supplement_queue.push(supplement);
        tracing::debug!(
            "session {} {} added ({} chars, {} attachments)",
            session_id,
            if is_answer { "answer" } else { "supplement" },
            text.len(),
            attachments.len()
        );
        Ok(())
    }

    pub async fn get_supplements(&self, session_id: &str) -> Vec<Supplement> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return Vec::new();
        };
        entry.lock().await.supplement_queue.drain(..).collect()
    }

    /// Add a steering item: interrupts the current tool sequence and is
    /// injected as context immediately (refine 搂1.2).
    pub async fn add_steering(&self, session_id: &str, text: &str) -> anyhow::Result<()> {
        self.add_steering_with_attachments(session_id, text, &[], None)
            .await
    }

    pub async fn add_steering_with_attachments(
        &self,
        session_id: &str,
        text: &str,
        attachments: &[MessageAttachment],
        message_id: Option<String>,
    ) -> anyhow::Result<()> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            anyhow::bail!("session '{}' not found", session_id)
        };
        let mut session = entry.lock().await;
        session.steering_queue.push(Supplement::with_message_id(
            text,
            attachments.to_vec(),
            message_id,
        ));
        tracing::debug!(
            "session {} steering added ({} chars, {} attachments)",
            session_id,
            text.len(),
            attachments.len()
        );
        Ok(())
    }

    /// Drain the steering queue for a session.
    pub async fn get_steering(&self, session_id: &str) -> Vec<Supplement> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            return Vec::new();
        };
        entry.lock().await.steering_queue.drain(..).collect()
    }

    /// Buffer a completed background-action result for a session. It is delivered
    /// to the ReAct loop as context at the next step start (drained by
    /// `drain_action_completions`), separate from the user-driven steering queue.
    pub async fn add_action_completion(&self, session_id: &str, text: &str) {
        let mut actions = self.action_completions.lock().await;
        actions
            .entry(session_id.to_string())
            .or_default()
            .push(text.to_string());
    }

    /// Drain buffered background-action completions for a session.
    pub async fn drain_action_completions(&self, session_id: &str) -> Vec<String> {
        self.action_completions
            .lock()
            .await
            .remove(session_id)
            .unwrap_or_default()
    }

    /// Drain all pending user-facing context for a session in one lock pass:
    /// supplements (paused-session replies / `ask` answers), steering (mid-run
    /// user interjections) and buffered background-action results. The ReAct loop
    /// calls this once per step instead of three separate queue drains (three
    /// global ses-map lock acquisitions per step), so the three batches can
    /// never drift apart either.
    pub async fn drain_pending_context(
        &self,
        session_id: &str,
    ) -> (Vec<Supplement>, Vec<Supplement>, Vec<String>) {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let (supplements, steering) = match entry {
            Some(entry) => {
                let mut session = entry.lock().await;
                (
                    session.supplement_queue.drain(..).collect(),
                    session.steering_queue.drain(..).collect(),
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let action_results = self
            .action_completions
            .lock()
            .await
            .remove(session_id)
            .unwrap_or_default();
        (supplements, steering, action_results)
    }

    /// Non-draining check for pending user-facing context (supplements or
    /// steering). Resume uses it to decide whether post-snapshot inputs must
    /// be recovered from the DB: when the queues still hold the inputs (the
    /// pause → answer flow in the same process), the ReAct loop injects them
    /// and the DB copy must NOT be re-queued; after a restart the queues are
    /// empty and the DB is the only source.
    pub async fn has_pending_context(&self, session_id: &str) -> bool {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        match entry {
            Some(entry) => {
                let session = entry.lock().await;
                !session.supplement_queue.is_empty() || !session.steering_queue.is_empty()
            }
            None => false,
        }
    }

    /// End a session. Since the user explicitly asked to end it, the session is
    /// always marked as Completed —regardless of whether it was still
    /// Running (forced stop) or Paused (naturally finished). Clean up
    /// resources either way. Called from the frontend "结束任务" button.
    pub async fn end_session(&self, session_id: &str) -> anyhow::Result<SessionStatus> {
        // Cancel the running token first to interrupt any active ReAct loop.
        // Ensure a real token exists even when the dispatcher hasn't created
        // one yet (race window between try_claim_pending and token insertion);
        // otherwise cancel() would fire on a default token nobody observes.
        let cancel = {
            let mut cancels = self.session_cancellations.lock().await;
            cancels
                .entry(session_id.to_string())
                .or_insert_with(CancellationToken::new)
                .clone()
        };
        cancel.cancel();
        // Kill any background actions the session spawned; they would otherwise
        // keep running (and leak child processes) after the session is gone.
        self.cancel_session_actions(session_id).await;
        // Promote checkpointed stream text into history (skip when a real
        // message already supersedes it). Runs BEFORE the session is torn down;
        // the PartialStore's generation bump also invalidates any in-flight
        // checkpoint so it cannot re-create the row afterwards.
        if let Err(e) = self.partials.promote(session_id).await {
            tracing::warn!(
                "end_session: failed to promote partial reply for session {}: {}",
                session_id,
                e
            );
        }
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            // Session not in memory (e.g. after restart) —end it regardless of
            // its DB state; the user asked to finish it.
            if let Err(e) = Self::persist_status(&self.db, session_id, "completed").await {
                tracing::error!(
                    "end_session: DB persist failed for session {}: {}",
                    session_id,
                    e
                );
                return Err(e);
            }
            return Ok(SessionStatus::Completed);
        };
        {
            let mut session = entry.lock().await;
            if let Err(e) = Self::persist_status(&self.db, session_id, "completed").await {
                tracing::error!(
                    "end_session: DB persist failed for session {}: {}",
                    session_id,
                    e
                );
                return Err(e);
            }
            session.status = SessionStatus::Completed;
            session.updated_at = chrono::Utc::now().to_rfc3339();
        }
        // Wake any ReAct-loop status waiter before tearing down the rest of
        // the per-session state.
        if let Some(tx) = self.status_tx.lock().await.remove(session_id) {
            let _ = tx.send(SessionStatus::Completed);
        }
        self.dequeue_pending(session_id).await;
        self.cleanup_session_maps(session_id).await;
        self.sessions.lock().await.remove(session_id);
        Self::unregister_from_inbox(session_id);
        // The conversation is over — its trusted risk levels must not outlive
        // it (a later conversation must ask again).
        self.tools
            .safety_gateway
            .clear_session_trust(session_id)
            .await;
        Ok(SessionStatus::Completed)
    }

    /// Remove a session entirely from the in-memory state.
    /// This does NOT delete from DB —the caller handles that.
    /// Succeeds even if the session is not in memory (e.g. after restart).
    pub async fn remove_session(&self, session_id: &str) {
        self.tools
            .safety_gateway
            .clear_session_trust(session_id)
            .await;
        self.cancel_session_actions(session_id).await;
        self.sessions.lock().await.remove(session_id);
        self.dequeue_pending(session_id).await;
        self.cleanup_session_maps(session_id).await;
        self.status_tx.lock().await.remove(session_id);
        self.action_completions.lock().await.remove(session_id);
    }

    pub async fn update_session_title(&self, session_id: &str, title: &str) {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        if let Some(entry) = entry {
            entry.lock().await.title = Some(title.into());
        }
    }

    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let entries: Vec<Arc<Mutex<SessionInfo>>> =
            self.sessions.lock().await.values().cloned().collect();
        let mut sessions: Vec<SessionInfo> = Vec::with_capacity(entries.len());
        for entry in entries {
            sessions.push(entry.lock().await.clone());
        }
        // Preserve the insertion-order semantics of the former Vec storage
        // (the map itself is unordered).
        sessions.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        sessions
    }

    /// Remove all sessions from memory and clean up running state.
    /// Used when the user clears history —the DB is already wiped.
    pub async fn clear_all_sessions(&self) {
        self.tools.safety_gateway.clear_all_trust().await;
        self.sessions.lock().await.clear();
        self.pending_queue.lock().await.clear();
        self.running_sessions.lock().await.clear();
        self.session_permits.lock().await.clear();
        self.session_cancellations.lock().await.clear();
        self.status_tx.lock().await.clear();
        self.action_completions.lock().await.clear();
    }

    /// Subscribe to a session's status changes. Level-triggered: the receiver
    /// holds the CURRENT status, so a transition that happened before the
    /// subscription is visible immediately, and `changed()` resolves as soon
    /// as the status moves after the receiver's last observed value. Callers
    /// must re-read the authoritative state after waking (the watch value is
    /// a hint, not a lock-free source of truth).
    pub async fn subscribe_status(&self, session_id: &str) -> watch::Receiver<SessionStatus> {
        // Initial value: the session's current status so a receiver created
        // after a transition observes it; Pending when the session is absent
        // (the caller re-checks state after waking anyway).
        let initial = {
            let entry = self.sessions.lock().await.get(session_id).cloned();
            match entry {
                Some(e) => e.lock().await.status.clone(),
                None => SessionStatus::Pending,
            }
        };
        self.status_tx
            .lock()
            .await
            .entry(session_id.to_string())
            .or_insert_with(|| watch::channel(initial).0)
            .subscribe()
    }

    /// Transition a session's status through the centralized state machine.
    ///
    /// Ordering guarantees (all under the session's own entry lock, so
    /// transitions of different sessions never serialize on a global lock):
    /// 1. The transition is validated against `can_transition`; illegal
    ///    transitions (e.g. mutating a terminal state) are rejected with a
    ///    warning and leave the state untouched.
    /// 2. The DB write happens BEFORE the memory flip, with a short retry, so
    ///    a persistent DB failure aborts the transition with memory/DB
    ///    consistent (the DB is the source of truth across restarts).
    /// 3. The status watcher is notified and, for Pending transitions, the
    ///    dispatcher is woken — outside the entry lock.
    /// 4. Terminal transitions run cleanup (maps, per-session tools, working
    ///    set) after the wake so a waiter observing the terminal status
    ///    always sees the session still resolvable.
    pub async fn update_session_status(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        self.update_session_status_inner(session_id, status, true).await
    }

    /// Transition a session's status in MEMORY ONLY, without persisting it to
    /// the DB. Used by the history-review reopen flow: a merely VIEWED
    /// completed/errored session must be made resumable for the current run
    /// (Paused) without resurrecting it in the DB — otherwise the ended
    /// conversation would be auto-restored on every app start and shown as an
    /// active conversation everywhere. Once the user actually continues it,
    /// the normal transitions persist again.
    pub async fn update_session_status_memory_only(
        &self,
        session_id: &str,
        status: SessionStatus,
    ) -> anyhow::Result<()> {
        self.update_session_status_inner(session_id, status, false).await
    }

    async fn update_session_status_inner(
        &self,
        session_id: &str,
        status: SessionStatus,
        persist: bool,
    ) -> anyhow::Result<()> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        let Some(entry) = entry else {
            // Session not in memory (e.g. already removed): no-op, matching the
            // historical behavior of silently succeeding.
            return Ok(());
        };
        let mut session = entry.lock().await;
        let old_status = session.status.clone();
        if old_status == status {
            // Same-status refresh: still wake the dispatcher so a session
            // re-registered as Pending (e.g. `create_session_with_first_message`)
            // is picked up even though its status did not change.
            if status == SessionStatus::Pending {
                self.enqueue_pending(session_id).await;
                self.wake_dispatcher();
            }
            return Ok(());
        }
        if !Self::can_transition(&old_status, &status) {
            tracing::warn!(
                "update_session_status: rejected illegal transition session={} {:?} -> {:?}",
                session_id,
                old_status,
                status
            );
            return Ok(());
        }
        if persist
            && let Err(e) = Self::persist_status(&self.db, session_id, status.as_str()).await
        {
            tracing::error!(
                "update_session_status: DB persist failed for session {}; transition {:?} -> {:?} aborted: {}",
                session_id,
                old_status,
                status,
                e
            );
            return Err(e);
        }
        session.status = status.clone();
        session.updated_at = chrono::Utc::now().to_rfc3339();
        tracing::info!(
            "update_session_status: session={} {} -> {}",
            session_id,
            old_status.as_str(),
            status.as_str()
        );
        let is_pending = status == SessionStatus::Pending;
        let is_terminal = status.is_terminal();
        drop(session);
        drop(entry);
        // Level-triggered wake: send on the existing watcher channel (or
        // lazily create one) so the ReAct loop's pause-wait resolves.
        let tx = {
            let mut map = self.status_tx.lock().await;
            map.entry(session_id.to_string())
                .or_insert_with(|| watch::channel(status.clone()).0)
                .clone()
        };
        let _ = tx.send(status.clone());
        if is_pending {
            self.enqueue_pending(session_id).await;
            self.wake_dispatcher();
        }
        if is_terminal {
            self.dequeue_pending(session_id).await;
            self.cleanup_session_maps(session_id).await;
            self.tools.unregister_session(session_id).await;
            Self::unregister_from_inbox(session_id);
            // The conversation ended — drop its trusted risk levels too (the
            // ReAct loop / dispatcher-panic path reaches terminal status
            // through here, not `end_session`, so this must happen on every
            // terminal transition or the per-session trust map leaks).
            self.tools
                .safety_gateway
                .clear_session_trust(session_id)
                .await;
            if let Some(tx) = self.status_tx.lock().await.remove(session_id) {
                let _ = tx.send(status);
            }
            self.sessions.lock().await.remove(session_id);
        }
        Ok(())
    }

    /// Centralized transition validation. Only transitions reachable from
    /// real call sites are allowed; anything else (notably any mutation of a
    /// terminal state except the explicit reopen/continue flows) is a bug and
    /// is rejected.
    fn can_transition(from: &SessionStatus, to: &SessionStatus) -> bool {
        use SessionStatus::*;
        match (from, to) {
            // Claim by the dispatcher.
            (Pending, Running) => true,
            // Park / finish a queued session without dispatching it.
            (Pending, Paused) | (Pending, PausedAwaitingAnswer) => true,
            (Pending, Completed) | (Pending, Error) => true,
            // Pause for a user reply / scheduling / budget checkpoint.
            (Running, Paused) | (Running, PausedAwaitingAnswer) => true,
            // Immediate resume: the ask was answered in the same turn
            // (pause_turn → Pending while the handler is still alive).
            (Running, Pending) => true,
            // Natural completion / failure.
            (Running, Completed) | (Running, Error) => true,
            // Resume paths (user message, action completion, continue flow).
            (Paused, Pending) | (PausedAwaitingAnswer, Pending) => true,
            // Re-pause with an answer requirement.
            (Paused, PausedAwaitingAnswer) => true,
            // Defensive force-resume when a tool executes on a paused session
            // (see `execute_step`; kept from the pre-validation era).
            (Paused, Running) | (PausedAwaitingAnswer, Running) => true,
            // Finish / fail a paused session (end_session's own path also exists,
            // but explicit transitions are kept valid).
            (Paused, Completed) | (Paused, Error) => true,
            (PausedAwaitingAnswer, Completed) | (PausedAwaitingAnswer, Error) => true,
            // User-driven exceptions: reopen a finished session for review
            // (history flow), retry an errored session from its snapshot.
            (Completed, Paused) | (Error, Paused) => true,
            (Error, Pending) => true,
            _ => false,
        }
    }

    /// Return (and, on first call, register) the cancellation token for a
    /// session. Register-on-miss mirrors `end_session`/the dispatcher's `entry()`
    /// pattern: a caller that cancels a directly-run session (tests, or code that
    /// runs the handler without the dispatcher claim path) must observe the
    /// same token the loop watches, otherwise `cancel()` would fire on a
    /// default token nobody listens to. `entry()` never clobbers an existing
    /// (possibly already-cancelled) token.
    pub async fn cancellation_token(&self, session_id: &str) -> CancellationToken {
        self.session_cancellations
            .lock()
            .await
            .entry(session_id.to_string())
            .or_insert_with(CancellationToken::new)
            .clone()
    }

    /// Remove `session_id` from the three per-session maps (`running_sessions`,
    /// `session_permits`, `session_cancellations`). Centralizes the three-line
    /// triplet that used to be copy-pasted at every cleanup site.
    /// Does NOT touch `sessions` (working set) or `status_tx` — those have
    /// ordering-sensitive callers (`update_session_status`, `unmark_running`)
    /// that need to remain in the lock-order path.
    pub async fn cleanup_session_maps(&self, session_id: &str) {
        self.running_sessions.lock().await.remove(session_id);
        self.session_permits.lock().await.remove(session_id);
        self.session_cancellations.lock().await.remove(session_id);
    }

    /// Look up an in-memory `SessionInfo` by id (O(1), per-session lock only).
    pub async fn get_session(&self, session_id: &str) -> Option<SessionInfo> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        Some(entry?.lock().await.clone())
    }

    /// Load a session from the database into the in-memory list if it is not
    /// already there (e.g. after an app restart). Used by `process_input`
    /// so that follow-up messages can reach sessions that were paused before
    /// the restart and never re-entered the executor's working set.
    pub async fn ensure_session_loaded(&self, session_id: &str) -> anyhow::Result<()> {
        {
            let sessions = self.sessions.lock().await;
            if sessions.contains_key(session_id) {
                return Ok(());
            }
        }
        let record = self
            .db
            .get_session(session_id)?
            .ok_or_else(|| anyhow::anyhow!("session '{}' not found in database", session_id))?;
        let session = SessionInfo::from_db_record(&record);
        let mut sessions = self.sessions.lock().await;
        // Re-check: another thread may have inserted this session between the
        // check above and the DB query.
        if !sessions.contains_key(session_id) {
            sessions.insert(session_id.to_string(), Arc::new(Mutex::new(session)));
        }
        Ok(())
    }

    /// Reload sessions that are still `Pending` in the database into the
    /// in-memory working set and wake the dispatcher. Called at dispatcher
    /// startup so queued work from a previous run is picked up after an app
    /// restart. Returns the number of sessions reloaded.
    pub async fn load_pending_sessions(&self) -> usize {
        let pending =
            match self
                .db
                .search_sessions_filtered(None, Some("pending"), None, None, -1, 0)
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("load_pending_sessions: pending-session query failed: {}", e);
                    Vec::new()
                }
            };
        let mut loaded = 0;
        let mut queued = Vec::new();
        {
            let mut sessions = self.sessions.lock().await;
            for record in pending {
                if sessions.contains_key(&record.id) {
                    continue;
                }
                // Force Pending: this loader only ever rehydrates sessions
                // whose DB status is already "pending" (the SQL filter
                // guarantees that), so the override is a no-op but keeps
                // the invariant explicit at the call site.
                let mut info = SessionInfo::from_db_record(&record);
                info.status = SessionStatus::Pending;
                sessions.insert(record.id.clone(), Arc::new(Mutex::new(info)));
                queued.push(record.id);
                loaded += 1;
            }
        }
        // FIFO: enqueue after releasing the working-set lock (the queue lock
        // is never held across the map lock to keep the order acyclic).
        for id in queued {
            self.enqueue_pending(&id).await;
        }
        if loaded > 0 {
            self.wake_dispatcher();
        }
        loaded
    }

    /// Current in-memory status of a session, or `None` when the session is not in
    /// the working set (removed on terminal cleanup / `end_session` / restart).
    /// Deliberately does NOT conflate "absent" with `Error`: callers that
    /// previously probed for `Error` to detect removal must check for `None`.
    pub async fn get_session_state(&self, session_id: &str) -> Option<SessionStatus> {
        let entry = { self.sessions.lock().await.get(session_id).cloned() };
        Some(entry?.lock().await.status.clone())
    }

    pub fn get_tools(&self) -> Arc<ToolsManager> {
        self.tools.clone()
    }

    pub fn db(&self) -> &Arc<Database> {
        &self.db
    }

    /// Cancel and drop all background actions owned by a session, and cancel its
    /// pending scheduled_actions. Called when the session ends, is removed, or is
    /// rolled back so child processes cannot leak past their session and no
    /// scheduled action fires against a session that no longer exists.
    pub async fn cancel_session_actions(&self, session_id: &str) {
        self.tools
            .background_actions
            .cancel_for_session(session_id)
            .await;
        self.tools
            .scheduled_actions
            .cancel_for_session(session_id)
            .await;
    }

    /// Persist a pending `session_steps` row under the pre-minted `step-*` id
    /// at Action-emit time — before the tool runs. Interrupted / cancelled
    /// tools never reach `execute_step`'s post-completion write, so without
    /// this the live card is the only copy and drops on every DB rebuild
    /// (Continue resync, session switch, app restart).
    pub async fn begin_action_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        step_id: &str,
    ) {
        let risk_level = self
            .tools
            .get_risk_level(Some(session_id), tool_name, input)
            .await;
        let silent = is_silent_action(tool_name, input);
        let step_number = step_num as i32;
        let session_id = session_id.to_string();
        let tool_name = tool_name.to_string();
        let tool_input = input.to_string();
        let step_id_owned = step_id.to_string();
        if let Err(e) = self
            .db
            .run_blocking(move |db| {
                db.ensure_action_step(
                    &session_id,
                    step_number,
                    &tool_name,
                    &tool_input,
                    risk_level != RiskLevel::Safe,
                    silent,
                    None,
                    &step_id_owned,
                )
            })
            .await
        {
            tracing::warn!("begin_action_step failed for step {}: {}", step_id, e);
        }
    }

    /// Persist an Interrupted observation onto the pending step row (creating
    /// it if Action-time begin failed). Keeps the review badge aligned with
    /// the live Interrupted card across resume/resync.
    pub async fn finish_interrupted_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: &Value,
        step_num: u32,
        step_id: &str,
        observation: &str,
    ) {
        let risk_level = self
            .tools
            .get_risk_level(Some(session_id), tool_name, input)
            .await;
        let silent = is_silent_action(tool_name, input);
        let step_number = step_num as i32;
        let session_id = session_id.to_string();
        let tool_name = tool_name.to_string();
        let tool_input = input.to_string();
        let step_id_owned = step_id.to_string();
        let observation = observation.to_string();
        if let Err(e) = self
            .db
            .run_blocking(move |db| {
                db.ensure_action_step(
                    &session_id,
                    step_number,
                    &tool_name,
                    &tool_input,
                    risk_level != RiskLevel::Safe,
                    silent,
                    None,
                    &step_id_owned,
                )?;
                db.complete_action_step(&step_id_owned, &observation, false)
            })
            .await
        {
            tracing::warn!(
                "finish_interrupted_step failed for step {}: {}",
                step_id,
                e
            );
        }
    }

    /// Execute a tool step. `step_id` is the pre-minted `step-*` id the frontend's
    /// live tool card already uses; the persisted step row reuses it so the live
    /// card and the review badge are one entity. The pending row is normally
    /// created by [`Self::begin_action_step`] at Action emit; this method
    /// ensures + completes it after the tool finishes.
    #[allow(clippy::too_many_arguments)]
    pub async fn execute_step(
        &self,
        session_id: &str,
        tool_name: &str,
        input: Value,
        step_num: u32,
        step_id: &str,
    ) -> anyhow::Result<ToolResult> {
        tracing::info!(
            "execute_step: session={} tool={} input={:?}",
            session_id,
            tool_name,
            input
        );
        {
            let entry = { self.sessions.lock().await.get(session_id).cloned() };
            if let Some(entry) = entry {
                let mut session = entry.lock().await;
                let prev = session.status.clone();
                if prev.is_terminal() {
                    // Executing a tool on a Completed/Error session is always a
                    // stale call from a racing loop (end/rollback already
                    // finished this session). Failing fast here prevents the
                    // force below from resurrecting a dead session. Complete
                    // the pending Action-time row so review doesn't keep an
                    // empty badge for a tool that never ran.
                    let err = format!(
                        "execute_step: session {} is {}; refusing to execute tool '{}'",
                        session_id,
                        prev.as_str(),
                        tool_name
                    );
                    self.finish_interrupted_step(
                        session_id,
                        tool_name,
                        &input,
                        step_num,
                        step_id,
                        &err,
                    )
                    .await;
                    return Err(anyhow::anyhow!(err));
                }
                if prev != SessionStatus::Running {
                    // The ReAct loop runs with status Pending (the dispatcher
                    // never flips it to Running), and scheduled scheduled_actions fire
                    // on Paused sessions; both are legitimate tool-call moments.
                    session.status = SessionStatus::Running;
                    tracing::warn!(
                        "execute_step: session {} was {} before tool call, forcing Running",
                        session_id,
                        prev.as_str()
                    );
                }
            }
        }

        let cancel = self.cancellation_token(session_id).await;
        let gated = match self
            .execute_gated(Some(session_id), tool_name, input.clone(), cancel)
            .await
        {
            Ok(gated) => gated,
            Err(e) => {
                // Pending row was created at Action emit; record the failure
                // so resume/resync does not rebuild an empty tool badge.
                self.finish_interrupted_step(
                    session_id,
                    tool_name,
                    &input,
                    step_num,
                    step_id,
                    &e.to_string(),
                )
                .await;
                return Err(e);
            }
        };
        let ToolExecution {
            result,
            risk_level,
            confirmed,
        } = gated;
        tracing::info!(
            "execute_step result: tool={} success={}",
            tool_name,
            result.success
        );

        // Apply the tool's declared per-session side effects (skill/MCP adapter
        // registration) instead of name-matching load_skill/load_mcp here —
        // a new tool with a side effect declares it via `Tool::registrations`
        // and nothing in this executor needs to change. Background-action
        // bindings are applied after the running-set guard below (a action
        // spawned in a concurrently-rolled-back step must not attach past
        // the cleanup sweep). `registrations` is extracted ONCE: calling it
        // twice could yield divergent results for stateful tools, and the
        // variant split below is explicit rather than silently partitioned.
        let registrations = if result.success {
            self.tools
                .get_tool_for_session(Some(session_id), tool_name)
                .await
                .map(|t| t.registrations(&result.output))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for reg in &registrations {
            match reg {
                haven_tools::ToolRegistration::Skill(name) => {
                    self.tools
                        .register_skill_for_session(session_id, name)
                        .await;
                }
                haven_tools::ToolRegistration::McpServer(name) => {
                    self.tools.register_mcp_for_session(session_id, name).await;
                }
                // Action is applied after the running-set guard.
                haven_tools::ToolRegistration::Action(_) => {}
            }
        }
        let step_number = step_num as i32;
        // Guard against rollback/cancel: if the session has been removed from the
        // running set while the tool was executing (e.g. rollback_session marked
        // it Error and restored a snapshot), skip persisting step records that
        // would otherwise corrupt the restored state.
        if !self.running_sessions.lock().await.contains(session_id) {
            tracing::warn!(
                "execute_step: session {} left running set during tool execution; skipping step record",
                session_id
            );
            return Ok(result);
        }
        // Tie a background action to its session so end/rollback can clean it up.
        // Applied only AFTER the running-set guard above passed (a rollback
        // racing this step may have removed the session); the registrations were
        // extracted once, before the guard.
        for reg in &registrations {
            if let haven_tools::ToolRegistration::Action(action_id) = reg {
                self.tools
                    .background_actions
                    .attach_session(action_id, session_id)
                    .await;
            }
        }
        let obs = result.summary_text();
        let success = result.success;
        let persist_step_id = step_id.to_string();
        let tool_name_owned = tool_name.to_string();
        // The in-memory StepInfo reuses the persisted step row's id so the
        // live session state and the review history reference the same step.
        if let Some(entry) = self.sessions.lock().await.get(session_id).cloned() {
            let mut session = entry.lock().await;
            session.steps.push(StepInfo {
                id: persist_step_id.clone(),
                step_number,
                tool_name: tool_name_owned.clone(),
                input: input.clone(),
                output: Some(result.output.clone()),
                status: if success {
                    "completed".into()
                } else {
                    "failed".into()
                },
                risk_level,
                confirmed,
            });
            session.updated_at = chrono::Utc::now().to_rfc3339();
        }
        // Row was normally created at Action emit; ensure + complete covers
        // direct execute_step callers (tests) and races where begin failed.
        let session_id_owned = session_id.to_string();
        let tool_input = input.to_string();
        let silent = is_silent_action(tool_name, &input);
        self.db
            .run_blocking(move |db| {
                db.ensure_action_step(
                    &session_id_owned,
                    step_number,
                    &tool_name_owned,
                    &tool_input,
                    risk_level != RiskLevel::Safe,
                    silent,
                    confirmed,
                    &persist_step_id,
                )?;
                db.complete_action_step(&persist_step_id, &obs, success)
            })
            .await?;
        Ok(result)
    }

    /// Execute a tool through the safety gateway. The tool's risk level is
    /// checked against the configured threshold BEFORE anything runs; an
    /// operation at/above the threshold blocks on the user's confirmation
    /// (`confirm:requested` event + `resolve_confirmation`), and is aborted
    /// when the user declines or the session is cancelled. Returns a failed
    /// `ToolResult` for declined operations so the ReAct loop sees a normal
    /// tool failure the model can react to.
    pub async fn execute_gated(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolExecution> {
        let risk_level = self
            .tools
            .get_risk_level(session_id, tool_name, &input)
            .await;
        let mut confirmed: Option<bool> = None;
        match self
            .tools
            .safety_gateway
            .check(session_id, tool_name, &input, risk_level)
            .await
        {
            ConfirmationResult::AutoApproved => {}
            ConfirmationResult::Blocked => {
                return Ok(ToolExecution {
                    result: ToolResult {
                        success: false,
                        output: Value::Null,
                        error: Some(format!(
                            "operation '{}' is blocked by the security policy. Do NOT retry it — ask the user what to do instead or choose a different approach.",
                            tool_name
                        )),
                        truncated: false,
                        signals: haven_tools::ToolSignals::default(),
                    },
                    risk_level,
                    confirmed: Some(false),
                });
            }
            ConfirmationResult::RequiresConfirmation { .. } => {
                if !self
                    .await_confirmation(session_id, tool_name, risk_level)
                    .await
                {
                    return Ok(ToolExecution {
                        result: ToolResult {
                            success: false,
                            output: Value::Null,
                            error: Some(format!(
                                "The user REJECTED the operation '{}' (confirmation declined). Do NOT retry it — ask the user what to do instead or choose a different approach.",
                                tool_name
                            )),
                            truncated: false,
                            signals: haven_tools::ToolSignals::default(),
                        },
                        risk_level,
                        confirmed: Some(false),
                    });
                }
                confirmed = Some(true);
            }
        }
        let result = self
            .tools
            .execute_tool(session_id, tool_name, input, cancel)
            .await?;
        Ok(ToolExecution {
            result,
            risk_level,
            confirmed,
        })
    }

    /// Request user confirmation for a safety-gated tool call and wait for
    /// the answer. Emits `confirm:requested` through the wired callback and
    /// blocks until `resolve_confirmation` resolves the generated step id, or
    /// the session's cancellation token fires (end/rollback/stop). Returns
    /// `true` when the user approved.
    async fn await_confirmation(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        risk_level: RiskLevel,
    ) -> bool {
        let step_id: haven_common::types::ConfirmId = haven_common::types::new_id("conf").into();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.confirm_waits.lock().await.insert(
            step_id.clone(),
            ConfirmWait {
                risk_level,
                session_id: session_id.map(str::to_string),
                tx,
            },
        );
        let tid = session_id.unwrap_or("action").to_string();
        // No confirmation callback wired (unit tests, degraded startup):
        // there is no UI that could ever answer — fail closed so the tool
        // never runs without approval, instead of blocking the session forever.
        if self.on_confirm_request.snap().is_none() {
            self.confirm_waits.lock().await.remove(&step_id);
            tracing::info!(
                "confirmation for tool '{}' on session {} rejected: no confirmation channel wired",
                tool_name,
                tid
            );
            return false;
        }
        if let Some(cb) = self.on_confirm_request.snap() {
            cb(
                step_id.clone(),
                tid.clone(),
                tool_name.to_string(),
                risk_level,
            );
        }
        let cancel = self.cancellation_token(&tid).await;
        let decision = tokio::select! {
            r = rx => r.ok(),
            _ = cancel.cancelled() => None,
            // Bounded, fail-closed fallback: an unanswered confirmation (e.g.
            // the app window is closed when a scheduled action fires) must
            // not wedge the session — or the sequential scheduled-action consumer —
            // forever.
            _ = tokio::time::sleep(CONFIRM_WAIT_TIMEOUT) => {
                tracing::warn!(
                    "confirmation for tool '{}' on session {} timed out after {:?}; treating as rejected",
                    tool_name,
                    tid,
                    CONFIRM_WAIT_TIMEOUT
                );
                None
            }
        };
        self.confirm_waits.lock().await.remove(&step_id);
        match decision {
            Some(true) => true,
            Some(false) | None => {
                tracing::info!(
                    "confirmation for tool '{}' on session {} not approved (answer={:?})",
                    tool_name,
                    tid,
                    decision
                );
                false
            }
        }
    }

    /// Resolve a pending safety-gateway confirmation and return the risk level
    /// and the owning session id, so the caller can trust the level for the
    /// right conversation. The approval/denial itself is persisted on the real
    /// `session_steps` row when `execute_step` completes the pending step (via
    /// the `confirmed` returned by `execute_gated`); this method only unblocks
    /// the ReAct loop waiting on the oneshot. Every step id handed here comes
    /// from a `confirm:requested` payload, which is only emitted by
    /// `await_confirmation` — so an id not present in `confirm_waits` is stale
    /// (already resolved or cancelled); there is no legacy path.
    pub async fn resolve_confirmation(
        &self,
        step_id: &haven_common::types::ConfirmId,
        confirmed: bool,
    ) -> anyhow::Result<Option<(RiskLevel, Option<String>)>> {
        if let Some(wait) = self.confirm_waits.lock().await.remove(step_id) {
            let level = wait.risk_level;
            let session_id = wait.session_id;
            let _ = wait.tx.send(confirmed);
            return Ok(Some((level, session_id)));
        }
        Ok(None)
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

    /// A Pending session whose handler is still alive (present in the running
    /// set, e.g. blocked in a pause-wait after Paused → Pending) must not be
    /// claimed again —otherwise the dispatcher spawns a duplicate ReAct loop.
    /// The stale queue entry is consumed on the skip: the alive handler picks
    /// up the supplement via the status watcher itself, and a later transition
    /// to Pending re-enqueues the session if it ever becomes claimable again.
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
        let loaded = exec2.load_pending_sessions().await;
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
        let loaded = exec2.load_pending_sessions().await;
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
        // The wire/DB form stays "paused".
        assert_eq!(state.as_str(), "paused");

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
        // Both pause flavors serialize to the same wire string.
        assert_eq!(SessionStatus::PausedAwaitingAnswer.as_str(), "paused");
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
