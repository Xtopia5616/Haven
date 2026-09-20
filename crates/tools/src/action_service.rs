use haven_common::ActionStatus;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::{RwLock, broadcast, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::ActionLifecycle;
use haven_memory::repositories::action_completion_outbox::ActionCompletionOutboxRow;

fn lock_or_recover<'a, T>(lock: &'a Mutex<T>, name: &'static str) -> MutexGuard<'a, T> {
    lock.lock().unwrap_or_else(|poisoned| {
        tracing::error!(
            lock = name,
            "background action lock poisoned; recovering state"
        );
        poisoned.into_inner()
    })
}

use crate::output::{append_windows_diagnostics, sanitize_shell_output, summarize_error};
use crate::process::{kill_process_tree, read_stream_capped, take_tail_if_changed};
use crate::shell_runtime::{build_shell_command, collect_byte_cap, write_output_log};
use haven_memory::Database;

const ACTION_DB_RETRY_ATTEMPTS: usize = 3;
const ACTION_DB_RETRY_DELAY: Duration = Duration::from_millis(50);
const SCHEDULED_FIRE_LEASE: Duration = Duration::from_secs(15 * 60);

/// A background action that has reached a terminal state, surfaced to a consumer
/// (the agent layer) so the owning session can be auto-notified of the result
/// instead of the model having to poll `status`.
#[derive(Clone, Debug)]
pub struct BackgroundActionCompletion {
    pub action_id: String,
    /// Stable identity of the terminal result. It remains the same when the
    /// broadcast is replayed or the owning session queue retries delivery.
    pub action_result_id: String,
    pub session_id: Option<String>,
    /// Canonical terminal lifecycle status.
    pub status: ActionStatus,
    /// The action's status JSON (same shape `status()` returns for terminal
    /// states), carrying the output/error payload.
    pub status_json: Value,
}

/// A scheduled action that reached its durable `Waiting -> Running` trigger
/// transition. The agent must acknowledge the actual work with
/// [`ActionService::complete_scheduled`] or [`ActionService::fail_scheduled`].
#[derive(Clone, Debug, serde::Serialize)]
pub struct ScheduledActionFired {
    pub action_id: String,
    pub title: String,
    pub body: String,
    pub mode: crate::builtin::scheduled_action::ScheduleMode,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_args: Option<Value>,
    pub prompt: Option<String>,
}

/// One completion stream for every action kind.
#[derive(Clone, Debug)]
pub enum ActionCompletion {
    Background(BackgroundActionCompletion),
    Scheduled(ScheduledActionFired),
}

/// Receiver for the unified action completion stream.
pub struct ActionCompletionReceiver {
    rx: broadcast::Receiver<ActionCompletion>,
    /// Scheduled fire claim/lease ownership is deliberately shared by all
    /// receivers: local de-duplication cannot prevent two scheduled consumers
    /// from executing the same fire.
    pending_scheduled_fires: Arc<RwLock<HashMap<String, ScheduledActionFired>>>,
    scheduled_fire_claims: Arc<RwLock<HashMap<String, ScheduledFireClaim>>>,
}

#[derive(Clone, Copy, Debug)]
struct ScheduledFireClaim {
    expires_at: Instant,
}

async fn claim_scheduled_fire(
    pending_scheduled_fires: &RwLock<HashMap<String, ScheduledActionFired>>,
    scheduled_fire_claims: &RwLock<HashMap<String, ScheduledFireClaim>>,
    action_id: &str,
) -> Option<ScheduledActionFired> {
    // Claim and lookup use the same lock order everywhere. This makes the
    // claim check atomic from the perspective of concurrent receivers while
    // allowing an abandoned consumer to be recovered after the lease expires.
    let mut claims = scheduled_fire_claims.write().await;
    let now = Instant::now();
    if let Some(claim) = claims.get(action_id)
        && claim.expires_at > now
    {
        return None;
    }
    let fired = pending_scheduled_fires
        .read()
        .await
        .get(action_id)
        .cloned()?;
    claims.insert(
        action_id.to_string(),
        ScheduledFireClaim {
            expires_at: now + SCHEDULED_FIRE_LEASE,
        },
    );
    Some(fired)
}

impl ActionCompletionReceiver {
    pub async fn recv(&mut self) -> Option<ActionCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(ActionCompletion::Scheduled(fired)) => {
                    if let Some(fired) = claim_scheduled_fire(
                        &self.pending_scheduled_fires,
                        &self.scheduled_fire_claims,
                        &fired.action_id,
                    )
                    .await
                    {
                        return Some(ActionCompletion::Scheduled(fired));
                    }
                }
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "action completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Receive background completions without claiming scheduled fires. The
    /// agent's background consumer uses this so a second receiver cannot steal
    /// a scheduled trigger before the dedicated scheduled consumer sees it.
    pub async fn recv_background(&mut self) -> Option<ActionCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(ActionCompletion::Background(completion)) => {
                    return Some(ActionCompletion::Background(completion));
                }
                Ok(ActionCompletion::Scheduled(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "action completion receiver lagged")
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Receive a background completion from either the transient broadcast or
    /// the durable outbox. The outbox is checked after every broadcast lag and
    /// on a bounded interval so a completion that was never published still
    /// wakes the owning session. Delivery claims expire if the consumer dies.
    pub async fn recv_background_with_recovery(
        &mut self,
        service: &ActionService,
    ) -> Option<ActionCompletion> {
        let mut reconcile = tokio::time::interval(ACTION_COMPLETION_RECONCILE_INTERVAL);
        reconcile.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Reconcile once immediately at startup; subsequent checks are
        // bounded by the interval while the transient channel remains the
        // fast path for newly completed actions.
        reconcile.tick().await;
        loop {
            if let Some(completion) = service.claim_pending_background_completion().await {
                return Some(ActionCompletion::Background(completion));
            }
            match tokio::select! {
                result = self.rx.recv() => result,
                _ = reconcile.tick() => continue,
            } {
                Ok(ActionCompletion::Background(completion)) => {
                    return Some(ActionCompletion::Background(completion));
                }
                Ok(ActionCompletion::Scheduled(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(
                        skipped,
                        "action completion receiver lagged; reconciling durable outbox"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return service
                        .claim_pending_background_completion()
                        .await
                        .map(ActionCompletion::Background);
                }
            }
        }
    }

    /// Receive a scheduled trigger with recovery for broadcast lag. The
    /// scheduled action remains in an in-memory unacknowledged set until the
    /// actual work is acknowledged, so a lagged receiver can replay it rather
    /// than silently losing the trigger.
    pub async fn recv_scheduled_with_recovery(
        &mut self,
        service: &ActionService,
    ) -> Option<ActionCompletion> {
        loop {
            // A fire can have been retained after a send with no consumer. A
            // receiver created later must drain that recovery source before
            // waiting on the transient broadcast channel.
            if let Some(fired) = service.pending_scheduled_fire().await {
                return Some(ActionCompletion::Scheduled(fired));
            }
            match self.rx.recv().await {
                Ok(ActionCompletion::Scheduled(fired)) => {
                    if let Some(fired) = claim_scheduled_fire(
                        &self.pending_scheduled_fires,
                        &self.scheduled_fire_claims,
                        &fired.action_id,
                    )
                    .await
                    {
                        return Some(ActionCompletion::Scheduled(fired));
                    }
                }
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "action completion receiver lagged");
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

const ACTION_COMPLETION_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);

/// Optional sink for action lifecycle events surfaced to the UI. The
/// sink is called with `(event, payload)` where event is one of:
/// - `action:created`  — an action was admitted
///   `{ action_id, status: "running"|"waiting", kind, started_at|due_at }`
/// - `action:updated`  — a background action was bound to a session
///   `{ action_id, session_id }`, or a scheduled action changed its live state
///   (`Waiting ↔ Running`, including no-consumer rollback)
/// - `action:output`   — live output preview while the action runs
///   `{ action_id, status: "running", output }` (bounded tail, emitted periodically)
/// - `action:finished` — the action reached a terminal state (full status
///   JSON, which already carries `action_id`, `status`, and the output/error
///   payload)
///
/// Scheduled actions use the same callback shape and sink.
pub use crate::action_lifecycle::EventSink;

#[derive(Clone, Debug)]
enum ActionState {
    /// A timer/dependency action is admitted but has not started its fire
    /// transition yet.
    Waiting {
        due_at: String,
    },
    Running {
        started_at: String,
    },
    Completed {
        output: String,
        exit_code: Option<i32>,
        truncated: bool,
        /// Path to the full-output log file (written when output was capped).
        log_path: Option<String>,
        started_at: String,
        finished_at: String,
    },
    Failed {
        error: String,
        error_reason: String,
        /// Path to the full-output log file (always written for failures so
        /// the root cause survives the condensed `error_reason`).
        log_path: Option<String>,
        exit_code: Option<i32>,
        started_at: String,
        finished_at: String,
    },
    Cancelled {
        started_at: String,
        finished_at: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActionKind {
    Background,
    Scheduled,
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduledActionEntry {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) due_at: String,
    pub(crate) mode: crate::builtin::scheduled_action::ScheduleMode,
    pub(crate) session_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_args: Option<Value>,
    pub(crate) prompt: Option<String>,
    pub(crate) watch_action_id: Option<String>,
}

impl ActionState {
    fn status(&self) -> ActionStatus {
        match self {
            Self::Waiting { .. } => ActionStatus::Waiting,
            Self::Running { .. } => ActionStatus::Running,
            Self::Completed { .. } => ActionStatus::Completed,
            Self::Failed { .. } => ActionStatus::Failed,
            Self::Cancelled { .. } => ActionStatus::Cancelled,
        }
    }

    fn is_waiting(&self) -> bool {
        matches!(self, Self::Waiting { .. })
    }

    fn can_transition_to(&self, next: ActionStatus) -> bool {
        self.status().can_transition_to(next)
    }

    fn is_terminal(&self) -> bool {
        self.status().is_terminal()
    }
}

struct ActionEntry {
    kind: ActionKind,
    session_id: Option<String>,
    state: ActionState,
    /// Kill signal for the running child process.
    kill: Option<oneshot::Sender<()>>,
    /// Bounded tail of the combined live output, for `action:output` preview
    /// events while the action runs. `None` for terminal entries.
    tail: Option<Arc<Mutex<String>>>,
    /// The shell command this action is executing (surfaced in running status so
    /// the agent can see what the action is doing right now).
    command: String,
    /// Interpreter the command runs under ("cmd", "powershell", "bash", ...).
    shell: String,
    /// Timer/dependency spec for scheduled actions.  Both process and timer
    /// actions live in the same map; only their worker-specific spec differs.
    scheduled: Option<ScheduledActionEntry>,
}

/// True when a terminal entry has outlived the configured terminal-action TTL
/// (running entries are never stale). Entries with an unparseable
/// `finished_at` are kept (never wrongly reaped).
fn terminal_entry_stale(entry: &ActionEntry, ttl: Duration) -> bool {
    let finished = match &entry.state {
        ActionState::Completed { finished_at, .. }
        | ActionState::Failed { finished_at, .. }
        | ActionState::Cancelled { finished_at, .. } => finished_at,
        ActionState::Running { .. } | ActionState::Waiting { .. } => return false,
    };
    let finished_ts = match chrono::DateTime::parse_from_rfc3339(finished) {
        Ok(t) => t.with_timezone(&chrono::Utc),
        Err(e) => {
            tracing::warn!(
                "terminal_entry_stale: unparseable finished_at '{}': {}",
                finished,
                e
            );
            return false;
        }
    };
    let Ok(ttl) = chrono::Duration::from_std(ttl) else {
        // An unrepresentable duration is effectively infinite from the
        // action registry's perspective; retain the entry rather than panic
        // during cleanup.
        return false;
    };
    chrono::Utc::now() - finished_ts > ttl
}

/// Unified state machine and runtime registry for every action kind.
///
/// A process action is spawned with `spawn_shell`, runs detached from the ReAct
/// loop, and is polled with `status`. Timer and dependency actions enter the
/// same map in `Waiting`, then use the same cancellation, ownership, lifecycle
/// and completion paths.
///
/// When an action reaches a terminal state, its typed completion is sent on the
/// unified completion bus so the agent layer can auto-inject the result into
/// the owning session's context without model polling.
pub struct ActionService {
    actions: RwLock<HashMap<String, ActionEntry>>,
    /// Serializes spawn admission and durable registration. An action is not
    /// visible to cancellation until its `running` row is durable, avoiding
    /// orphaned DB rows or processes across the spawn failure window.
    spawn_gate: tokio::sync::Mutex<()>,
    /// One bus for process completions and timer fires. Consumers may filter
    /// their subscription by variant, but no action kind owns a second bus.
    completion_tx: broadcast::Sender<ActionCompletion>,
    /// Scheduled triggers remain here until the agent acknowledges the actual
    /// work. This is the recovery source when the transient broadcast receiver
    /// falls behind.
    pending_scheduled_fires: Arc<RwLock<HashMap<String, ScheduledActionFired>>>,
    /// Service-wide scheduled-fire claims. A receiver owns a fire until it
    /// acknowledges it or its lease expires, so multiple receivers cannot
    /// execute the same trigger concurrently.
    scheduled_fire_claims: Arc<RwLock<HashMap<String, ScheduledFireClaim>>>,
    /// At most one retry worker is allowed for each scheduled action whose
    /// terminal DB write failed. The worker is cancelled with the service and
    /// stops once the durable transition succeeds.
    terminal_persistence_retries: RwLock<HashSet<String>>,
    /// At most one quarantine retry task is allowed per malformed action row.
    quarantine_persistence_retries: RwLock<HashSet<String>>,
    /// Max concurrent *running* actions (from `context_limits.background_max_actions`).
    max_actions: RwLock<usize>,
    /// Live-output tail cap (chars) for `action:output` preview events (from
    /// `context_limits.background_job_tail_max_chars`).
    job_tail_max_chars: RwLock<usize>,
    /// Cadence of `action:output` events while a action produces output (from
    /// `context_limits.background_job_output_emit_interval_ms`).
    job_output_emit_interval: RwLock<Duration>,
    /// Terminal actions stay on the board this long, then are reaped (from
    /// `context_limits.terminal_job_ttl_secs`).
    terminal_job_ttl: RwLock<Duration>,
    /// Max pending timer/dependency actions.
    max_scheduled_actions: RwLock<usize>,
    /// Upper bound for absolute timer schedules.
    max_due_horizon_secs: RwLock<i64>,
    /// Optional UI event sink (see `EventSink`). Wired by the desktop shell
    /// to forward lifecycle events as Tauri events.
    event_sink: ActionLifecycle,
    /// Persistent store; `None` in headless/test builds (in-memory only).
    /// Terminal action rows stay here as history even after the in-memory board
    /// reaps them (`TERMINAL_JOB_TTL`), so results survive app restarts.
    db: RwLock<Option<Arc<Database>>>,
    /// Cancels process runners, output preview loops and scheduled timers
    /// during application teardown.
    shutdown_token: CancellationToken,
    shutting_down: AtomicBool,
}

impl Default for ActionService {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionService {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            actions: RwLock::new(HashMap::new()),
            spawn_gate: tokio::sync::Mutex::new(()),
            completion_tx: tx,
            pending_scheduled_fires: Arc::new(RwLock::new(HashMap::new())),
            scheduled_fire_claims: Arc::new(RwLock::new(HashMap::new())),
            terminal_persistence_retries: RwLock::new(HashSet::new()),
            quarantine_persistence_retries: RwLock::new(HashSet::new()),
            max_actions: RwLock::new(64),
            job_tail_max_chars: RwLock::new(2000),
            job_output_emit_interval: RwLock::new(Duration::from_millis(1500)),
            terminal_job_ttl: RwLock::new(Duration::from_secs(600)),
            max_scheduled_actions: RwLock::new(32),
            max_due_horizon_secs: RwLock::new(365 * 24 * 3600),
            event_sink: ActionLifecycle::default(),
            db: RwLock::new(None),
            shutdown_token: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Unified completion receiver consumed by the agent layer.
    pub fn take_action_receiver(&self) -> Option<ActionCompletionReceiver> {
        Some(ActionCompletionReceiver {
            rx: self.completion_tx.subscribe(),
            pending_scheduled_fires: Arc::clone(&self.pending_scheduled_fires),
            scheduled_fire_claims: Arc::clone(&self.scheduled_fire_claims),
        })
    }

    async fn pending_scheduled_fire(&self) -> Option<ScheduledActionFired> {
        let ids = self
            .pending_scheduled_fires
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for action_id in ids {
            if let Some(fired) = claim_scheduled_fire(
                &self.pending_scheduled_fires,
                &self.scheduled_fire_claims,
                &action_id,
            )
            .await
            {
                return Some(fired);
            }
        }
        None
    }

    async fn claim_pending_background_completion(&self) -> Option<BackgroundActionCompletion> {
        let db = self.db.read().await.clone()?;
        match db.run_blocking(|db| db.claim_action_completion()).await {
            Ok(Some(ActionCompletionOutboxRow {
                action_id,
                action_result_id,
                session_id,
                status,
                status_json,
            })) => Some(BackgroundActionCompletion {
                action_id,
                action_result_id,
                session_id,
                status,
                status_json,
            }),
            Ok(None) => None,
            Err(error) => {
                tracing::debug!("action completion outbox reconcile failed: {error}");
                None
            }
        }
    }

    /// Acknowledge a completion after the agent's transcript/event projection
    /// is durable. Queue admission alone is deliberately insufficient: a
    /// session may become terminal and clear its actor queue immediately after
    /// admission.
    pub async fn acknowledge_background_completion(&self, action_result_id: &str) {
        let Some(db) = self.db.read().await.clone() else {
            return;
        };
        let action_result_id = action_result_id.to_string();
        let action_result_id_for_log = action_result_id.clone();
        if let Err(error) = db
            .run_blocking(move |db| db.acknowledge_action_completion(&action_result_id))
            .await
        {
            tracing::warn!(
                action_result_id = %action_result_id_for_log,
                "failed to acknowledge durable action completion: {error}"
            );
        }
    }

    /// Install the UI event sink (called once by the desktop shell).
    pub fn set_event_sink(&self, sink: EventSink) {
        self.event_sink.set_event_sink(sink);
    }

    /// Forward a lifecycle event to the installed sink (no-op without one).
    fn emit(&self, event: &str, payload: Value) {
        self.event_sink.emit(event, payload);
    }

    /// Replace the unified context limits (background action concurrency cap,
    /// live-output tail size, output-event cadence, terminal-action TTL).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_actions.write().await = limits.background_max_actions;
        *self.job_tail_max_chars.write().await = limits.background_job_tail_max_chars;
        *self.job_output_emit_interval.write().await =
            Duration::from_millis(limits.background_job_output_emit_interval_ms);
        *self.terminal_job_ttl.write().await = Duration::from_secs(limits.terminal_job_ttl_secs);
        *self.max_scheduled_actions.write().await = limits.scheduled_actions_max;
        *self.max_due_horizon_secs.write().await = limits.scheduled_actions_due_horizon_secs;
    }

    /// Attach the database used for persistence. Wired by the desktop shell;
    /// headless tests skip it.
    pub async fn set_db(&self, db: Option<Arc<Database>>) {
        *self.db.write().await = db;
    }

    /// Try to move a malformed persisted waiting row to terminal history.
    /// Returns `Ok(false)` when another path already removed it from the
    /// waiting set. Restore must never leave a row that the pending query
    /// returns but the runtime cannot parse.
    async fn try_quarantine_invalid_scheduled_row(
        &self,
        id: &str,
        reason: &str,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        let Some(db) = self.db.read().await.clone() else {
            return Ok(true);
        };
        let mut last_error = None;
        for attempt in 0..ACTION_DB_RETRY_ATTEMPTS {
            let action_id = id.to_string();
            let reason = reason.to_string();
            let finished_at_for_db = finished_at.to_string();
            match db
                .run_blocking(move |db| {
                    db.fail_waiting_scheduled_action(&action_id, &reason, &finished_at_for_db)
                })
                .await
            {
                Ok(changed) => return Ok(changed),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < ACTION_DB_RETRY_ATTEMPTS {
                        tokio::time::sleep(ACTION_DB_RETRY_DELAY).await;
                    }
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("malformed scheduled action quarantine failed")))
    }

    /// Move a malformed row out of the pending set. If the short inline retry
    /// budget is exhausted, retain a single per-row background retry with
    /// exponential backoff. This prevents a transient DB outage from turning
    /// a durable `waiting` row into a permanently invisible action.
    async fn quarantine_invalid_scheduled_row(self: &Arc<Self>, id: &str, reason: &str) {
        let finished_at = chrono::Utc::now().to_rfc3339();
        match self
            .try_quarantine_invalid_scheduled_row(id, reason, &finished_at)
            .await
        {
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(
                    action_id = %id,
                    "failed to quarantine malformed scheduled action; scheduling retry: {error}"
                );
            }
        }

        if !self
            .quarantine_persistence_retries
            .write()
            .await
            .insert(id.to_string())
        {
            return;
        }
        let service = Arc::clone(self);
        let action_id = id.to_string();
        let reason = reason.to_string();
        tokio::spawn(async move {
            let mut delay = Duration::from_secs(1);
            loop {
                tokio::select! {
                    _ = service.shutdown_token.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                match service
                    .try_quarantine_invalid_scheduled_row(&action_id, &reason, &finished_at)
                    .await
                {
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(
                            action_id = %action_id,
                            "malformed scheduled action quarantine retry failed: {error}"
                        );
                        delay = (delay * 2).min(Duration::from_secs(30));
                    }
                }
            }
            service
                .quarantine_persistence_retries
                .write()
                .await
                .remove(&action_id);
        });
    }

    async fn clear_scheduled_fire_claim(&self, id: &str) {
        // Keep the same claim -> pending lock order as claim_scheduled_fire.
        self.scheduled_fire_claims.write().await.remove(id);
        self.pending_scheduled_fires.write().await.remove(id);
    }

    async fn rollback_background_registration(&self, action_id: &str) {
        let Some(db) = self.db.read().await.clone() else {
            self.actions.write().await.remove(action_id);
            return;
        };

        let mut delete_error = None;
        let mut deleted = false;
        for attempt in 0..ACTION_DB_RETRY_ATTEMPTS {
            let id = action_id.to_string();
            match db.run_blocking(move |db| db.delete_action(&id)).await {
                Ok(true) | Ok(false) => {
                    deleted = true;
                    break;
                }
                Err(error) => {
                    delete_error = Some(error);
                    if attempt + 1 < ACTION_DB_RETRY_ATTEMPTS {
                        tokio::time::sleep(ACTION_DB_RETRY_DELAY).await;
                    }
                }
            }
        }

        if deleted {
            self.actions.write().await.remove(action_id);
            return;
        }

        // The process/containment admission failed, so leaving the durable row
        // as `running` would create a restart-only ghost. Preserve a terminal
        // failure as a last-resort compensation; restore_after_restart can then
        // safely treat the row as history even if the delete path is unavailable.
        let finished_at = chrono::Utc::now().to_rfc3339();
        let id = action_id.to_string();
        let reason = "background action failed before its process was admitted";
        let mut fallback_error = None;
        for attempt in 0..ACTION_DB_RETRY_ATTEMPTS {
            let id = id.clone();
            let finished_at_for_db = finished_at.clone();
            match db
                .run_blocking(move |db| {
                    db.finish_action(
                        &id,
                        ActionStatus::Failed,
                        None,
                        Some(reason),
                        Some(reason),
                        None,
                        None,
                        &finished_at_for_db,
                    )
                })
                .await
            {
                Ok(()) => {
                    fallback_error = None;
                    break;
                }
                Err(error) => {
                    fallback_error = Some(error);
                    if attempt + 1 < ACTION_DB_RETRY_ATTEMPTS {
                        tokio::time::sleep(ACTION_DB_RETRY_DELAY).await;
                    }
                }
            }
        }
        if let Some(error) = fallback_error {
            tracing::warn!(
                action_id,
                delete_error = ?delete_error,
                fallback_error = %error,
                "failed to compensate background action registration rollback"
            );
        }

        let mut actions = self.actions.write().await;
        if let Some(entry) = actions.get_mut(action_id) {
            entry.kill = None;
            entry.tail = None;
            entry.state = ActionState::Failed {
                error: reason.to_string(),
                error_reason: reason.to_string(),
                log_path: None,
                exit_code: None,
                started_at: match &entry.state {
                    ActionState::Running { started_at } => started_at.clone(),
                    _ => finished_at.clone(),
                },
                finished_at,
            };
        }
    }

    /// Stop action workers owned by the application.
    ///
    /// Running background processes are cancelled and become terminal so the
    /// action board cannot retain a ghost `running` row. Pending scheduled rows
    /// are left durable and are only stopped in memory; they can be restored by
    /// the next process instead of being silently cancelled on a normal app
    /// exit.
    pub async fn shutdown(&self) {
        if self
            .shutting_down
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        // Serialize shutdown with action admission. This closes the window in
        // which a process could be published after the application has begun
        // teardown.
        let _spawn_gate = self.spawn_gate.lock().await;
        self.shutdown_token.cancel();
        let ids = {
            let actions = self.actions.read().await;
            actions
                .iter()
                .filter(|(_, entry)| {
                    matches!(
                        (entry.kind, &entry.state),
                        (
                            ActionKind::Background | ActionKind::Scheduled,
                            ActionState::Running { .. }
                        )
                    )
                })
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        drop(_spawn_gate);

        for id in ids {
            let _ = self.cancel(&id).await;
        }
    }

    /// Post-restart cleanup: action rows a previous process left `running` are
    /// stale (their child processes died with the app), so mark them failed.
    /// Called once from the agent layer startup. Returns the number of rows
    /// marked. Idempotent.
    pub async fn restore_after_restart(&self) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        db.run_blocking(|db| db.mark_interrupted_actions())
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("restore_after_restart: failed to mark interrupted actions: {e}");
                0
            })
    }

    /// Persist a terminal action row (its status payload + owning session) so the
    /// result survives the in-memory board's TTL and app restarts. No-op
    /// without a database. Must run outside the `actions` lock is not required
    /// (the DB is a separate lock); callers may hold either.
    async fn persist_terminal(&self, action_id: &str, state: &ActionState, status_json: &Value) {
        let Some(db) = self.db.read().await.clone() else {
            return;
        };
        let (output, error, error_reason, log_path, exit_code, finished_at) = match state {
            ActionState::Completed {
                output,
                exit_code,
                log_path,
                finished_at,
                ..
            } => (
                Some(output.as_str()),
                None,
                None,
                log_path.as_deref(),
                *exit_code,
                finished_at.as_str(),
            ),
            ActionState::Failed {
                error,
                error_reason,
                log_path,
                exit_code,
                finished_at,
                ..
            } => (
                None,
                Some(error.as_str()),
                Some(error_reason.as_str()),
                log_path.as_deref(),
                *exit_code,
                finished_at.as_str(),
            ),
            ActionState::Cancelled { finished_at, .. } => {
                (None, None, None, None, None, finished_at.as_str())
            }
            ActionState::Running { .. } | ActionState::Waiting { .. } => return,
        };
        let action_id = action_id.to_string();
        let status = state.status();
        let output = output.map(str::to_string);
        let error = error.map(str::to_string);
        let error_reason = error_reason.map(str::to_string);
        let log_path = log_path.map(str::to_string);
        let finished_at = finished_at.to_string();
        let action_id_for_db = action_id.clone();
        let status_json = serde_json::to_string(status_json).unwrap_or_else(|_| "{}".into());
        if let Err(e) = db
            .run_blocking(move |db| {
                if matches!(status, ActionStatus::Completed | ActionStatus::Failed) {
                    db.finish_action_with_completion(
                        &action_id_for_db,
                        status,
                        output.as_deref(),
                        error.as_deref(),
                        error_reason.as_deref(),
                        log_path.as_deref(),
                        exit_code,
                        &finished_at,
                        &status_json,
                    )
                } else {
                    db.finish_action(
                        &action_id_for_db,
                        status,
                        output.as_deref(),
                        error.as_deref(),
                        error_reason.as_deref(),
                        log_path.as_deref(),
                        exit_code,
                        &finished_at,
                    )
                }
            })
            .await
        {
            tracing::warn!(action_id = %action_id, "failed to persist action result: {e}");
        }
    }

    /// Emit a completion notification for a action (if it has a terminal state),
    /// reading the owning session_id from the entry. Called from `mark_finished`,
    /// `mark_cancelled`, and `attach_session` (the latter to close the race where
    /// a action finishes before its session binding is recorded). Also persists the
    /// terminal row so the result survives restarts.
    async fn notify_completion(
        &self,
        action_id: &str,
        state: ActionState,
        session_id: Option<String>,
    ) {
        if !state.is_terminal() {
            return;
        }
        let status = match &state {
            ActionState::Completed { .. } => ActionStatus::Completed,
            ActionState::Failed { .. } => ActionStatus::Failed,
            ActionState::Cancelled { .. } => ActionStatus::Cancelled,
            ActionState::Running { .. } | ActionState::Waiting { .. } => return,
        };
        let status_json = render_status_json(action_id, &state);
        self.persist_terminal(action_id, &state, &status_json).await;
        self.emit("action:finished", status_json.clone());
        if let Err(error) =
            self.completion_tx
                .send(ActionCompletion::Background(BackgroundActionCompletion {
                    action_id: action_id.to_string(),
                    action_result_id: action_id.to_string(),
                    session_id,
                    status,
                    status_json,
                }))
        {
            tracing::debug!(
                action_id = %action_id,
                error = %error,
                "no action completion subscriber is currently attached"
            );
        }
    }

    /// Board view of every action: one entry per action with status, timestamps,
    /// owning session id, and a bounded output/error preview. Surfaces the full
    /// action set to the UI (the per-session variant `list_for_session` serves the
    /// agent). Order: oldest first.
    pub async fn board(&self) -> Vec<Value> {
        let actions = self.actions.read().await;
        let mut rows = Vec::new();
        for (id, entry) in actions.iter() {
            if entry.kind != ActionKind::Background {
                if let Some(schedule) = &entry.scheduled
                    && entry.state.status().is_live()
                {
                    rows.push(scheduled_status_json(id, schedule, &entry.state, None));
                }
                continue;
            }
            let mut row = match &entry.state {
                ActionState::Running { .. } => running_status_json(id, entry),
                _ => render_status_json(id, &entry.state),
            };
            if let Some(tid) = &entry.session_id {
                row["session_id"] = json!(tid);
            }
            row["kind"] = json!("background");
            attach_preview(&mut row);
            rows.push(row);
        }
        rows.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
        rows
    }

    /// Board view of every action owned by `session_id`: one entry per action with
    /// status, timestamps, and a bounded output/error preview. Lets the model
    /// see all background work of a session in a single call instead of polling
    /// `status` action by action. Order: oldest first.
    pub async fn list_for_session(&self, session_id: &str) -> Vec<Value> {
        let actions = self.actions.read().await;
        let mut rows = Vec::new();
        for (id, entry) in actions.iter() {
            if entry.kind != ActionKind::Background {
                if let Some(schedule) = &entry.scheduled
                    && entry.state.status().is_live()
                    && schedule.session_id.as_deref() == Some(session_id)
                {
                    rows.push(scheduled_status_json(
                        id,
                        schedule,
                        &entry.state,
                        Some(session_id),
                    ));
                }
                continue;
            }
            if entry.session_id.as_deref() != Some(session_id) {
                continue;
            }
            let mut row = match &entry.state {
                ActionState::Running { .. } => running_status_json(id, entry),
                _ => render_status_json(id, &entry.state),
            };
            if let Some(tid) = &entry.session_id {
                row["session_id"] = json!(tid);
            }
            row["kind"] = json!("background");
            attach_preview(&mut row);
            rows.push(row);
        }
        rows.sort_by(|a, b| a["started_at"].as_str().cmp(&b["started_at"].as_str()));
        rows
    }

    /// Spawn a shell command as a background action. Returns the action id; the
    /// command keeps running after this function returns. `cwd` overrides the
    /// default Temp working directory when provided.
    pub async fn spawn_shell(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
    ) -> anyhow::Result<String> {
        self.spawn_shell_for_session(command, shell, max_chars, cwd, None)
            .await
    }

    /// Spawn a background action with its owner bound before the process is
    /// published. Agent calls should use this variant so session shutdown can
    /// cancel a process even if it exits during the tool-result projection.
    pub async fn spawn_shell_for_session(
        self: &Arc<Self>,
        command: &str,
        shell: &str,
        max_chars: usize,
        cwd: Option<std::path::PathBuf>,
        session_id: Option<&str>,
    ) -> anyhow::Result<String> {
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("action service is shutting down");
        }
        if command.trim().is_empty() {
            anyhow::bail!("command is required");
        }
        // Unpredictable action id: a sequential counter would let any
        // session's agent enumerate and read other sessions' background outputs
        // through status (which is RiskLevel::Safe).
        let id = haven_common::types::new_id("act");
        let started_at = chrono::Utc::now().to_rfc3339();
        let (kill_tx, kill_rx) = oneshot::channel();
        let tail = Arc::new(Mutex::new(String::new()));
        let tail_max_chars = *self.job_tail_max_chars.read().await;
        let emit_interval = *self.job_output_emit_interval.read().await;
        let terminal_ttl = *self.terminal_job_ttl.read().await;
        let max_actions = *self.max_actions.read().await;
        let _spawn_gate = self.spawn_gate.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("action service is shutting down");
        }
        {
            let mut actions = self.actions.write().await;
            // Reap terminal entries first: their results were already
            // delivered via the completion channel, so they must not occupy
            // the cap forever (64 lifetime actions would otherwise brick the
            // feature for long-lived sessions). Terminal entries older than
            // the configured terminal-action TTL are dropped the same way (the
            // UI panel and the persisted log files remain the record after
            // that).
            actions.retain(|_, e| !terminal_entry_stale(e, terminal_ttl));
            let running = actions
                .values()
                .filter(|e| matches!(e.state, ActionState::Running { .. }))
                .count();
            if running >= max_actions {
                anyhow::bail!(
                    "too many running background actions (limit {})",
                    max_actions
                );
            }
        }

        // Persist before publishing the action to the in-memory board or
        // starting a process. A failed database write therefore cannot leave a
        // process that restore_after_restart does not know how to clean up.
        if let Some(db) = self.db.read().await.clone() {
            let action_id = id.clone();
            let command_for_db = command.to_string();
            let started_at_for_db = started_at.clone();
            let session_id_for_db = session_id.map(str::to_owned);
            if let Err(error) = db
                .run_blocking(move |db| {
                    db.save_action(
                        &action_id,
                        session_id_for_db.as_deref(),
                        &command_for_db,
                        &started_at_for_db,
                    )
                })
                .await
            {
                tracing::warn!(action_id = %id, "failed to persist action spawn: {error}");
                return Err(error);
            }
        }

        self.actions.write().await.insert(
            id.clone(),
            ActionEntry {
                kind: ActionKind::Background,
                session_id: session_id.map(str::to_owned),
                state: ActionState::Running {
                    started_at: started_at.clone(),
                },
                kill: Some(kill_tx),
                tail: Some(tail.clone()),
                command: command.to_string(),
                shell: shell.to_string(),
                scheduled: None,
            },
        );

        let mut std_cmd = build_shell_command(shell, command);
        if let Some(cwd) = cwd {
            std_cmd.current_dir(cwd);
        }

        let containment = match haven_common::process_containment::ProcessContainment::new() {
            Ok(containment) => containment,
            Err(error) => {
                self.rollback_background_registration(&id).await;
                return Err(error.into());
            }
        };
        let mut child = match tokio::process::Command::from(std_cmd)
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                // Spawn failed: remove the entry so the action is not left
                // dangling as "running".
                self.rollback_background_registration(&id).await;
                return Err(e.into());
            }
        };
        let Some(pid) = child.id() else {
            self.rollback_background_registration(&id).await;
            return Err(anyhow::anyhow!(
                "background shell child did not expose a process id"
            ));
        };
        if let Err(error) = containment.attach(pid) {
            let _ = child.kill().await;
            self.rollback_background_registration(&id).await;
            return Err(anyhow::anyhow!(
                "failed to attach background shell to process containment: {}",
                haven_common::error::sanitize_error_text(&error.to_string())
            ));
        }

        let me = self.clone();
        let action_id = id.clone();
        let shell_owned = shell.to_string();
        let command_owned = command.to_string();
        self.emit(
            "action:created",
            json!({
                "action_id": action_id,
                "kind": "background",
                "status": "running",
                "started_at": started_at,
            }),
        );
        // The direct child pid is captured before `run` moves `child`; on
        // Windows, cancelling must kill the whole process tree, not just the
        // cmd.exe/powershell.exe wrapper.
        let child_pid = child.id();
        // The action runner outlives its spawner: give it a action-level span so
        // every log line emitted while the action runs/cancels (output-log
        // writes, completion) carries the action id — parallel background actions
        // stay distinguishable in logs.
        let action_span = tracing::info_span!("bg_action", action_id = %action_id);
        let runner_tail = tail.clone();
        let emit_action_id = action_id.clone();
        let shutdown_token = self.shutdown_token.clone();
        tokio::spawn(async move {
            // Keep the Job Object alive for the entire action. Its
            // kill-on-close flag then cleans up descendants on cancellation
            // or application shutdown.
            let _containment = containment;
            // The action outlives this session: when `run` is dropped (kill signal
            // received), kill_on_drop terminates the child.
            let max_collect = collect_byte_cap(max_chars);
            let stdout_tail = runner_tail.clone();
            let stderr_tail = runner_tail.clone();
            let stdout_fut = read_stream_capped(
                child.stdout.take(),
                max_collect,
                Some(stdout_tail),
                tail_max_chars,
            );
            let stderr_fut = read_stream_capped(
                child.stderr.take(),
                max_collect,
                Some(stderr_tail),
                tail_max_chars,
            );
            let run = async {
                let ((stdout, stdout_overflow), (stderr, stderr_overflow)) =
                    tokio::join!(stdout_fut, stderr_fut);
                let status = child.wait().await;
                let mut combined = stdout;
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }
                // Strip PowerShell's NativeCommandError/CLIXML formatting so
                // the payload carries the real message, not the noise.
                combined = sanitize_shell_output(&combined, &shell_owned);
                let exit_code = status.as_ref().ok().and_then(|s| s.code());
                let success = matches!(status, Ok(s) if s.success());
                let truncated = stdout_overflow || stderr_overflow;
                (combined, success, exit_code, truncated)
            };
            tokio::pin!(run);
            tokio::select! {
                _ = kill_rx => {
                    // Dropping `run` drops the pipes and the child
                    // (kill_on_drop), terminating the command.
                    if let Some(pid) = child_pid {
                        kill_process_tree(pid).await;
                    }
                    me.mark_cancelled(&action_id, &started_at).await;
                }
                _ = shutdown_token.cancelled() => {
                    if let Some(pid) = child_pid {
                        kill_process_tree(pid).await;
                    }
                    me.mark_cancelled(&action_id, &started_at).await;
                }
                (combined, success, exit_code, truncated) = &mut run => {
                    me.mark_finished(&action_id, &started_at, &shell_owned, &command_owned, combined, success, exit_code, truncated).await;
                }
            }
        }.instrument(action_span));

        // Live-output preview: emit `action:output` when the bounded tail
        // changes (by value — length alone freezes once the window is full).
        let emit_me = self.clone();
        let emit_tail = tail;
        let shutdown_token = self.shutdown_token.clone();
        tokio::spawn(async move {
            let mut last_output = String::new();
            loop {
                tokio::select! {
                    _ = shutdown_token.cancelled() => return,
                    _ = tokio::time::sleep(emit_interval) => {}
                }
                if emit_me.status(&emit_action_id).await["status"].as_str() != Some("running") {
                    return;
                }
                if take_tail_if_changed(&emit_tail, &mut last_output) {
                    emit_me.emit(
                        "action:output",
                        json!({
                            "action_id": emit_action_id,
                            "status": "running",
                            "output": last_output.as_str(),
                        }),
                    );
                }
            }
        });

        Ok(id)
    }

    /// Report the current status of a action as JSON.
    pub async fn status(&self, action_id: &str) -> Value {
        let actions = self.actions.read().await;
        let Some(entry) = actions.get(action_id) else {
            return json!({"action_id": action_id, "status": "not_found"});
        };
        if entry.kind == ActionKind::Scheduled {
            return entry
                .scheduled
                .as_ref()
                .map(|schedule| {
                    if !entry.state.is_waiting() {
                        json!({"action_id": action_id, "status": entry.state.status().as_str()})
                    } else {
                        scheduled_status_json(action_id, schedule, &entry.state, None)
                    }
                })
                .unwrap_or_else(|| json!({"action_id": action_id, "status": "not_found"}));
        }
        Self::render_action_status(action_id, entry)
    }

    /// Status lookup scoped to the owning session. Agent-facing callers must
    /// never be able to enumerate another session's action by guessing its id.
    pub async fn status_for_session(&self, action_id: &str, session_id: &str) -> Value {
        let actions = self.actions.read().await;
        let Some(entry) = actions.get(action_id) else {
            return json!({"action_id": action_id, "status": "not_found"});
        };
        if entry.kind == ActionKind::Scheduled {
            let Some(schedule) = entry.scheduled.as_ref() else {
                return json!({"action_id": action_id, "status": "not_found"});
            };
            if schedule.session_id.as_deref() != Some(session_id) || !entry.state.status().is_live()
            {
                return json!({"action_id": action_id, "status": "not_found"});
            }
            return scheduled_status_json(action_id, schedule, &entry.state, Some(session_id));
        }
        if entry.session_id.as_deref() != Some(session_id) {
            return json!({"action_id": action_id, "status": "not_found"});
        }
        Self::render_action_status(action_id, entry)
    }

    fn render_action_status(action_id: &str, entry: &ActionEntry) -> Value {
        match &entry.state {
            ActionState::Running { .. } => {
                let body = running_status_json(action_id, entry);
                let mut ordered = haven_common::tools::background_wait_object(
                    "The action is still running. END YOUR TURN if you have nothing else useful to do — do not poll. The result is auto-pushed and the session is auto-woken when it finishes.",
                );
                // Move fields (including the live output tail) — do not clone
                // the potentially large `output` string just to reorder keys.
                if let Value::Object(obj) = body {
                    for (k, val) in obj {
                        ordered.insert(k, val);
                    }
                }
                Value::Object(ordered)
            }
            _ => render_status_json(action_id, &entry.state),
        }
    }

    /// Associate a action with its owning session. Called by the session executor
    /// after a background tool call so `cancel_for_session` can clean it up.
    ///
    /// Also closes a race: a short-lived action may finish (and call
    /// `mark_finished`/`mark_cancelled`) before this binding is recorded, in
    /// which case the completion notification carried `session_id: None` and was
    /// dropped by the consumer. If the action is already terminal here, re-fire
    /// the notification with the now-known session_id so the owning session still
    /// receives the result.
    pub async fn attach_session(&self, action_id: &str, session_id: &str) {
        let (terminal_state, session_id) = {
            let mut actions = self.actions.write().await;
            let Some(entry) = actions.get_mut(action_id) else {
                return;
            };
            if entry.kind != ActionKind::Background {
                return;
            }
            if let Some(existing) = entry.session_id.as_deref() {
                if existing == session_id {
                    return;
                }
                tracing::warn!(
                    action_id,
                    existing_session_id = existing,
                    requested_session_id = session_id,
                    "refusing to rebind background action to another session"
                );
                return;
            }
            entry.session_id = Some(session_id.to_string());
            (
                entry.state.is_terminal().then(|| entry.state.clone()),
                session_id.to_string(),
            )
        };
        self.emit(
            "action:updated",
            json!({
                "action_id": action_id,
                "session_id": session_id,
            }),
        );
        // Record the owning session in the persisted row too, so terminal
        // history keeps its owner (spawn rows start with session_id NULL).
        if let Some(db) = self.db.read().await.clone() {
            let action_id = action_id.to_string();
            let action_id_for_db = action_id.clone();
            let session_id_for_db = session_id.clone();
            if let Err(e) = db
                .run_blocking(move |db| {
                    db.update_action_session(&action_id_for_db, &session_id_for_db)
                })
                .await
            {
                tracing::warn!(action_id = %action_id, "failed to persist action session binding: {e}");
            }
        }
        if let Some(state) = terminal_state {
            self.notify_completion(action_id, state, Some(session_id))
                .await;
        }
    }

    /// Cancel a single live action (kept for inspection afterwards).
    /// Returns false when the action does not exist, is not live, or its
    /// durable cancellation could not be committed.
    pub async fn cancel(&self, action_id: &str) -> bool {
        let mut actions = self.actions.write().await;
        let Some(entry) = actions.get_mut(action_id) else {
            return false;
        };
        if entry.kind == ActionKind::Scheduled {
            drop(actions);
            return self.cancel_scheduled(action_id, None).await;
        }
        if !matches!(entry.state, ActionState::Running { .. }) {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel a single action only when it belongs to `session_id`.
    pub async fn cancel_for_session(&self, action_id: &str, session_id: &str) -> bool {
        let mut actions = self.actions.write().await;
        let Some(entry) = actions.get_mut(action_id) else {
            return false;
        };
        if entry.kind == ActionKind::Scheduled {
            drop(actions);
            return self.cancel_scheduled(action_id, Some(session_id)).await;
        }
        if entry.session_id.as_deref() != Some(session_id)
            || !matches!(entry.state, ActionState::Running { .. })
        {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel only when the caller's kind discriminator matches the owned
    /// action. The app IPC layer must not be able to cancel a scheduled row by
    /// presenting it as a background action (or vice versa).
    pub async fn cancel_for_kind(&self, action_id: &str, kind: &str) -> bool {
        let expected = match kind {
            "background" | "scheduled" => kind,
            _ => return false,
        };
        let matches = self
            .actions
            .read()
            .await
            .get(action_id)
            .is_some_and(|entry| {
                matches!(
                    (expected, entry.kind),
                    ("background", ActionKind::Background) | ("scheduled", ActionKind::Scheduled)
                )
            });
        matches && self.cancel(action_id).await
    }

    /// Delete terminal action history through the same owner that controls
    /// live action state. Waiting/running rows, kind mismatches and absent rows
    /// are all rejected and return `false`.
    pub async fn delete(&self, action_id: &str, kind: &str) -> anyhow::Result<bool> {
        let expected = match kind {
            "background" => ActionKind::Background,
            "scheduled" => ActionKind::Scheduled,
            _ => return Ok(false),
        };
        let _mutation = self.spawn_gate.lock().await;
        let memory_terminal = {
            let actions = self.actions.read().await;
            match actions.get(action_id) {
                Some(entry) if entry.kind == expected => entry.state.is_terminal(),
                Some(_) => return Ok(false),
                None => false,
            }
        };
        if !memory_terminal {
            if let Some(db) = self.db.read().await.clone() {
                let id = action_id.to_string();
                let row = db.run_blocking(move |db| db.get_action(&id)).await?;
                let Some(row) = row else {
                    return Ok(false);
                };
                if row.kind != kind || !row.status.is_terminal() {
                    return Ok(false);
                }
                let id = action_id.to_string();
                if !db.run_blocking(move |db| db.delete_action(&id)).await? {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        } else if let Some(db) = self.db.read().await.clone() {
            let id = action_id.to_string();
            if let Some(row) = db
                .run_blocking({
                    let id = id.clone();
                    move |db| db.get_action(&id)
                })
                .await?
            {
                if row.kind != kind || !row.status.is_terminal() {
                    return Ok(false);
                }
                if !db.run_blocking(move |db| db.delete_action(&id)).await? {
                    return Ok(false);
                }
            }
        }
        self.actions.write().await.remove(action_id);
        Ok(true)
    }

    /// Delete terminal history when the IPC surface does not need to expose a
    /// kind discriminator. The actual persisted/runtime kind is resolved first
    /// and then passed through the guarded kind-aware delete path.
    pub async fn delete_terminal(&self, action_id: &str) -> anyhow::Result<bool> {
        let memory_kind = {
            let actions = self.actions.read().await;
            actions.get(action_id).map(|entry| match entry.kind {
                ActionKind::Background => "background",
                ActionKind::Scheduled => "scheduled",
            })
        };
        if let Some(kind) = memory_kind {
            return self.delete(action_id, kind).await;
        }
        let Some(db) = self.db.read().await.clone() else {
            return Ok(false);
        };
        let id = action_id.to_string();
        let Some(row) = db.run_blocking(move |db| db.get_action(&id)).await? else {
            return Ok(false);
        };
        self.delete(action_id, &row.kind).await
    }

    /// Cancel and drop every background action owned by `session_id`.
    ///
    /// Running actions are killed, marked cancelled, persisted, and surfaced to
    /// the UI via `action:finished` before leaving the board — otherwise the
    /// titlebar panel keeps a ghost "running" row that cannot be stopped.
    pub async fn cancel_owned_background_by_session(&self, session_id: &str) {
        let ids: Vec<String> = {
            let actions = self.actions.read().await;
            actions
                .iter()
                .filter(|(_, e)| {
                    e.kind == ActionKind::Background && e.session_id.as_deref() == Some(session_id)
                })
                .map(|(id, _)| id.clone())
                .collect()
        };
        for id in ids {
            let Some(mut entry) = self.actions.write().await.remove(&id) else {
                continue;
            };
            if let Some(tx) = entry.kill.take() {
                let _ = tx.send(());
            }
            entry.tail = None;
            if let ActionState::Running { started_at } = &entry.state {
                entry.state = ActionState::Cancelled {
                    started_at: started_at.clone(),
                    finished_at: chrono::Utc::now().to_rfc3339(),
                };
                self.notify_completion(&id, entry.state.clone(), entry.session_id.clone())
                    .await;
            } else if entry.state.is_terminal() {
                // UI-only: the board is dropping a row whose agent completion
                // already reached a terminal state (or never needed one).
                // Re-sending completion_tx
                // would risk duplicate inject on an ending session.
                let mut status_json = render_status_json(&id, &entry.state);
                if let Some(tid) = &entry.session_id {
                    status_json["session_id"] = json!(tid);
                }
                self.emit("action:finished", status_json);
            }
        }
    }

    /// Cancel all action kinds owned by `session_id`. This is used by explicit
    /// session end/deletion; application shutdown uses the background-only
    /// variant so durable scheduled work remains waiting.
    pub async fn cancel_owned_by_session(&self, session_id: &str) {
        self.cancel_owned_background_by_session(session_id).await;
        self.cancel_owned_scheduled_by_session(session_id).await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn mark_finished(
        &self,
        id: &str,
        started_at: &str,
        shell: &str,
        command: &str,
        combined: String,
        success: bool,
        exit_code: Option<i32>,
        truncated: bool,
    ) {
        let (state, session_id) = {
            let mut actions = self.actions.write().await;
            let Some(entry) = actions.get_mut(id) else {
                return;
            };
            let next = if success {
                ActionStatus::Completed
            } else {
                ActionStatus::Failed
            };
            if entry.state.is_terminal() || !entry.state.can_transition_to(next) {
                return;
            }
            entry.kill = None;
            entry.tail = None;
            let finished_at = chrono::Utc::now().to_rfc3339();
            entry.state = if success {
                ActionState::Completed {
                    output: combined.clone(),
                    exit_code,
                    truncated,
                    // When the collected output was capped, the log file keeps
                    // the full transcript for inspection.
                    log_path: truncated.then(|| {
                        write_output_log("action-logs", id, &combined)
                            .to_string_lossy()
                            .into_owned()
                    }),
                    started_at: started_at.to_string(),
                    finished_at,
                }
            } else {
                // The failure payload must not drown the model (or the user) in
                // progress-bar spam: `error` keeps the sanitized output for full
                // inspection, `error_reason` carries a short tail of the most
                // likely error lines plus a Windows-trap hint when one matches.
                // The full output always lands in a log file so the root cause
                // is recoverable even when the summary misses it.
                let diagnosed = append_windows_diagnostics(shell, command, &combined);
                ActionState::Failed {
                    error: combined.clone(),
                    error_reason: summarize_error(&diagnosed, 1200),
                    log_path: Some(
                        write_output_log("action-logs", id, &combined)
                            .to_string_lossy()
                            .into_owned(),
                    ),
                    exit_code,
                    started_at: started_at.to_string(),
                    finished_at,
                }
            };
            (entry.state.clone(), entry.session_id.clone())
        };
        self.notify_completion(id, state, session_id).await;
    }

    async fn mark_cancelled(&self, id: &str, started_at: &str) {
        let (state, session_id) = {
            let mut actions = self.actions.write().await;
            let Some(entry) = actions.get_mut(id) else {
                return;
            };
            if entry.state.is_terminal() || !entry.state.can_transition_to(ActionStatus::Cancelled)
            {
                return;
            }
            entry.kill = None;
            entry.tail = None;
            entry.state = ActionState::Cancelled {
                started_at: started_at.to_string(),
                finished_at: chrono::Utc::now().to_rfc3339(),
            };
            (entry.state.clone(), entry.session_id.clone())
        };
        self.notify_completion(id, state, session_id).await;
    }
}

impl ActionService {
    /// Schedule a timer or an action dependency in the same state map as
    /// background processes. `Waiting` is the explicit pre-fire state; fire
    /// is one idempotent transition that publishes the work item. The consumer
    /// owns the terminal acknowledgement so tool/session failures remain
    /// durable as `failed`.
    pub async fn set(
        self: &Arc<Self>,
        spec: crate::builtin::scheduled_action::ScheduledActionSpec,
    ) -> anyhow::Result<String> {
        use crate::builtin::scheduled_action::ScheduleMode;

        let crate::builtin::scheduled_action::ScheduledActionSpec {
            due_at,
            delay_secs,
            watch_action_id,
            title,
            body,
            mode,
            session_id,
            tool_name,
            tool_args,
            prompt,
        } = spec;
        let watch_action_id = watch_action_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if watch_action_id.is_some() && (due_at.is_some() || delay_secs.is_some()) {
            anyhow::bail!("watch_action_id cannot be combined with due_at or delay_secs");
        }

        let now = chrono::Utc::now();
        let (due, remaining) = if watch_action_id.is_some() {
            // Keep the dependency pending even if the producer is not in this
            // process anymore. The watcher resolves that case as `not_found`
            // instead of leaving a durable schedule that can never fire.
            (None, 0_i64)
        } else {
            match (due_at.as_deref(), delay_secs) {
                (Some(_), Some(_)) => {
                    anyhow::bail!("use exactly one of due_at or delay_secs, not both")
                }
                (Some(value), None) => {
                    let parsed = chrono::DateTime::parse_from_rfc3339(value.trim())
                        .map_err(|_| anyhow::anyhow!("due_at must be an ISO 8601 timestamp"))?
                        .with_timezone(&chrono::Utc);
                    let remaining = (parsed - now).num_seconds();
                    if remaining <= 0 {
                        anyhow::bail!("due_at must be in the future");
                    }
                    if remaining > *self.max_due_horizon_secs.read().await {
                        anyhow::bail!("due_at is more than 365 days in the future");
                    }
                    (Some(parsed), remaining)
                }
                (None, Some(delay)) if (1..=86_400).contains(&delay) => (
                    Some(now + chrono::Duration::seconds(delay as i64)),
                    delay as i64,
                ),
                (None, Some(_)) => anyhow::bail!("delay_secs must be between 1 and 86400"),
                (None, None) => {
                    anyhow::bail!("either due_at, delay_secs or watch_action_id is required")
                }
            }
        };

        let body = body.trim().to_string();
        if body.is_empty() {
            anyhow::bail!("body is required");
        }
        let title = match title.trim() {
            "" => "Haven".to_string(),
            value => value.to_string(),
        };
        let tool_name = tool_name
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let prompt = prompt
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if mode == ScheduleMode::Tool && tool_name.is_none() {
            anyhow::bail!("tool_name is required when mode is 'tool'");
        }
        if mode == ScheduleMode::Continue && prompt.is_none() && watch_action_id.is_none() {
            anyhow::bail!("prompt is required when mode is 'continue'");
        }
        if let Some(args) = &tool_args
            && !args.is_object()
        {
            anyhow::bail!("tool_args must be a JSON object");
        }
        if mode != ScheduleMode::Tool && (tool_name.is_some() || tool_args.is_some()) {
            anyhow::bail!("tool_name and tool_args require mode 'tool'");
        }

        let id = haven_common::types::new_id("act");
        let due_at = due.map(|value| value.to_rfc3339()).unwrap_or_default();
        let _mutation = self.spawn_gate.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            anyhow::bail!("action service is shutting down");
        }
        let max_pending = *self.max_scheduled_actions.read().await;
        let mut actions = self.actions.write().await;
        // Terminal schedules are already durable history and must not consume
        // the in-memory pending budget. Reap them at the next admission just
        // like terminal process entries are reaped by the process worker.
        actions
            .retain(|_, entry| !(entry.kind == ActionKind::Scheduled && !entry.state.is_waiting()));
        let pending = actions
            .values()
            .filter(|entry| entry.kind == ActionKind::Scheduled && entry.state.is_waiting())
            .count();
        drop(actions);
        if pending >= max_pending {
            anyhow::bail!(
                "too many pending scheduled tasks (limit {}); cancel some first",
                max_pending
            );
        }

        // `watch_action_id` dependencies are deliberately process-local: they
        // reference the in-memory producer registry and are not written to the
        // durable action table. Cross-restart dependency recovery needs a
        // separate durable producer/idempotency contract (ADR 0172).
        if watch_action_id.is_none()
            && let Some(db) = self.db.read().await.clone()
        {
            let args_json = tool_args.as_ref().map(Value::to_string);
            let id_for_db = id.clone();
            let due_for_db = due_at.clone();
            let mode_for_db = mode.as_str().to_string();
            let title_for_db = title.clone();
            let body_for_db = body.clone();
            let session_for_db = session_id.clone();
            let tool_for_db = tool_name.clone();
            let prompt_for_db = prompt.clone();
            db.run_blocking(move |db| {
                db.save_scheduled_action(
                    &id_for_db,
                    &due_for_db,
                    &title_for_db,
                    &body_for_db,
                    &mode_for_db,
                    session_for_db.as_deref(),
                    tool_for_db.as_deref(),
                    args_json.as_deref(),
                    prompt_for_db.as_deref(),
                )
            })
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to persist scheduled task '{}': {error}", id)
            })?;
        }

        let entry = ScheduledActionEntry {
            title: title.clone(),
            body: body.clone(),
            due_at: due_at.clone(),
            mode,
            session_id: session_id.clone(),
            tool_name: tool_name.clone(),
            tool_args: tool_args.clone(),
            prompt: prompt.clone(),
            watch_action_id: watch_action_id.clone(),
        };
        self.actions.write().await.insert(
            id.clone(),
            ActionEntry {
                kind: ActionKind::Scheduled,
                session_id: session_id.clone(),
                // The schedule-specific state is authoritative for timer
                // actions; this placeholder keeps the worker fields uniform.
                state: ActionState::Waiting {
                    due_at: due_at.clone(),
                },
                kill: None,
                tail: None,
                command: String::new(),
                shell: String::new(),
                scheduled: Some(entry),
            },
        );
        self.emit(
            "action:created",
            json!({
                "id": id,
                "action_id": id,
                "kind": "scheduled",
                "status": "waiting",
                "title": title,
                "body": body,
                "mode": mode.as_str(),
                "session_id": session_id,
                "tool_name": tool_name,
                "watch_action_id": watch_action_id,
                "due_at": due_at,
            }),
        );

        let service = self.clone();
        let fired_id = id.clone();
        let shutdown_token = self.shutdown_token.clone();
        if let Some(watched_id) = watch_action_id {
            tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown_token.cancelled() => {}
                    _ = service.watch_action_timer(fired_id, watched_id) => {}
                }
            });
        } else {
            tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown_token.cancelled() => {}
                    _ = async {
                        tokio::time::sleep(Duration::from_secs(remaining.max(0) as u64)).await;
                        service.fire_scheduled(&fired_id).await;
                    } => {}
                }
            });
        }
        Ok(id)
    }

    pub async fn list(&self) -> Vec<Value> {
        self.list_scheduled_scoped(None).await
    }

    pub async fn list_scheduled_for_session(&self, session_id: &str) -> Vec<Value> {
        self.list_scheduled_scoped(Some(session_id)).await
    }

    async fn list_scheduled_scoped(&self, owner: Option<&str>) -> Vec<Value> {
        let actions = self.actions.read().await;
        let mut rows: Vec<_> = actions
            .iter()
            .filter_map(|(id, entry)| {
                let schedule = entry.scheduled.as_ref()?;
                if entry.kind != ActionKind::Scheduled
                    || !entry.state.status().is_live()
                    || owner.is_some_and(|value| schedule.session_id.as_deref() != Some(value))
                {
                    return None;
                }
                Some(scheduled_status_json(id, schedule, &entry.state, owner))
            })
            .collect();
        rows.sort_by(|left, right| right["due_at"].as_str().cmp(&left["due_at"].as_str()));
        rows
    }

    async fn fire_scheduled(self: &Arc<Self>, id: &str) {
        let _mutation = self.spawn_gate.lock().await;
        let started_at = chrono::Utc::now().to_rfc3339();
        let schedule = {
            let actions = self.actions.read().await;
            let Some(action) = actions.get(id) else {
                return;
            };
            if !matches!(action.state, ActionState::Waiting { .. }) {
                return;
            }
            let Some(schedule) = action.scheduled.as_ref() else {
                return;
            };
            schedule.clone()
        };
        if schedule.watch_action_id.is_none()
            && let Some(db) = self.db.read().await.clone()
        {
            let action_id = id.to_string();
            let started_at_for_db = started_at.clone();
            match db
                .run_blocking(move |db| db.start_scheduled_action(&action_id, &started_at_for_db))
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(action_id = %id, "scheduled action was no longer waiting in durable storage");
                    return;
                }
                Err(error) => {
                    tracing::warn!(action_id = %id, "failed to persist scheduled action trigger: {error}");
                    // The one-shot timer has already exited. Keep the in-memory
                    // action waiting and mount a fresh worker so a transient DB
                    // outage cannot permanently lose the schedule.
                    self.arm_scheduled_worker(id.to_string(), &schedule);
                    return;
                }
            }
        }
        let payload = ScheduledActionFired {
            action_id: id.to_string(),
            title: schedule.title.clone(),
            body: schedule.body.clone(),
            mode: schedule.mode,
            session_id: schedule.session_id.clone(),
            tool_name: schedule.tool_name.clone(),
            tool_args: schedule.tool_args.clone(),
            prompt: schedule.prompt.clone(),
        };
        let started_at_for_event = started_at.clone();
        {
            let mut actions = self.actions.write().await;
            let Some(action) = actions.get_mut(id) else {
                return;
            };
            if !matches!(action.state, ActionState::Waiting { .. }) {
                return;
            }
            action.state = ActionState::Running { started_at };
        }
        self.emit(
            "action:updated",
            scheduled_status_json(
                id,
                &schedule,
                &ActionState::Running {
                    started_at: started_at_for_event,
                },
                None,
            ),
        );
        self.pending_scheduled_fires
            .write()
            .await
            .insert(id.to_string(), payload.clone());
        if self
            .completion_tx
            .send(ActionCompletion::Scheduled(payload.clone()))
            .is_err()
        {
            // No consumer exists. Roll the durable and in-memory claim back to
            // `waiting` and put the timer worker back. If the durable rollback
            // itself fails, retain the fire in the recovery map so a receiver
            // can still acknowledge it later instead of silently losing work.
            self.clear_scheduled_fire_claim(id).await;
            let mut requeued = true;
            if let Some(db) = self.db.read().await.clone()
                && schedule.watch_action_id.is_none()
            {
                let mut last_error = None;
                for attempt in 0..ACTION_DB_RETRY_ATTEMPTS {
                    let action_id = id.to_string();
                    match db
                        .run_blocking(move |db| db.requeue_scheduled_action(&action_id))
                        .await
                    {
                        Ok(true) => {
                            last_error = None;
                            break;
                        }
                        Ok(false) => {
                            requeued = false;
                            tracing::warn!(action_id = %id, "undelivered scheduled action was not running in durable storage");
                            break;
                        }
                        Err(error) => {
                            last_error = Some(error);
                            if attempt + 1 < ACTION_DB_RETRY_ATTEMPTS {
                                tokio::time::sleep(ACTION_DB_RETRY_DELAY).await;
                            }
                        }
                    }
                }
                if let Some(error) = last_error {
                    requeued = false;
                    tracing::warn!(action_id = %id, "failed to requeue undelivered scheduled action: {error}");
                }
            }
            if !requeued {
                self.pending_scheduled_fires
                    .write()
                    .await
                    .insert(id.to_string(), payload);
                return;
            }
            if let Some(action) = self.actions.write().await.get_mut(id)
                && matches!(action.state, ActionState::Running { .. })
            {
                action.state = ActionState::Waiting {
                    due_at: schedule.due_at.clone(),
                };
            }
            self.emit(
                "action:updated",
                scheduled_status_json(
                    id,
                    &schedule,
                    &ActionState::Waiting {
                        due_at: schedule.due_at.clone(),
                    },
                    None,
                ),
            );
            self.arm_scheduled_worker(id.to_string(), &schedule);
        }
    }

    async fn watch_action_timer(self: &Arc<Self>, id: String, watched_id: String) {
        loop {
            tokio::time::sleep(Duration::from_millis(1000)).await;
            let status = self.status(&watched_id).await;
            if status["status"] == "running" {
                continue;
            }
            let prompt = action_finished_prompt(&watched_id, &status);
            {
                let mut actions = self.actions.write().await;
                let Some(action) = actions.get_mut(&id) else {
                    return;
                };
                if !matches!(action.state, ActionState::Waiting { .. }) {
                    return;
                }
                let Some(schedule) = action.scheduled.as_mut() else {
                    return;
                };
                schedule.prompt = Some(prompt);
            }
            self.fire_scheduled(&id).await;
            return;
        }
    }

    /// Start the worker for a scheduled action that was returned to `Waiting`.
    /// This is intentionally shared by normal admission/recovery paths and the
    /// no-consumer compensation path: a timer task that has already fired is
    /// not reusable after `fire_scheduled` returns.
    fn arm_scheduled_worker(self: &Arc<Self>, id: String, schedule: &ScheduledActionEntry) {
        let service = Arc::clone(self);
        let shutdown_token = self.shutdown_token.clone();
        if let Some(watched_id) = schedule.watch_action_id.clone() {
            tokio::spawn(async move {
                tokio::select! {
                    _ = shutdown_token.cancelled() => {}
                    _ = service.watch_action_timer(id, watched_id) => {}
                }
            });
            return;
        }

        let remaining = match chrono::DateTime::parse_from_rfc3339(&schedule.due_at) {
            Ok(due) => (due.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_seconds(),
            Err(error) => {
                tracing::warn!(action_id = %id, "cannot re-arm scheduled action with invalid due_at: {error}");
                return;
            }
        };
        tokio::spawn(async move {
            tokio::select! {
                _ = shutdown_token.cancelled() => {}
                _ = async {
                    // An undelivered overdue fire is retried with a small
                    // floor to avoid a tight broadcast-failure loop.
                    tokio::time::sleep(Duration::from_secs(remaining.max(1) as u64)).await;
                    service.fire_scheduled(&id).await;
                } => {}
            }
        });
    }

    pub async fn complete_scheduled(self: &Arc<Self>, id: &str) -> anyhow::Result<bool> {
        self.finish_scheduled(id, ActionStatus::Completed, None)
            .await
    }

    pub async fn fail_scheduled(self: &Arc<Self>, id: &str, reason: &str) -> anyhow::Result<bool> {
        self.finish_scheduled(id, ActionStatus::Failed, Some(reason))
            .await
    }

    async fn finish_scheduled_in_memory(
        &self,
        id: &str,
        schedule: &ScheduledActionEntry,
        state: ActionState,
    ) -> bool {
        let mut actions = self.actions.write().await;
        let Some(action) = actions.get_mut(id) else {
            return false;
        };
        if !matches!(action.state, ActionState::Running { .. }) {
            return false;
        }
        action.state = state.clone();
        self.clear_scheduled_fire_claim(id).await;
        drop(actions);
        self.emit_scheduled_finished(id, schedule, &state);
        true
    }

    async fn persist_scheduled_terminal(
        &self,
        id: &str,
        status: ActionStatus,
        error_reason: Option<&str>,
        finished_at: &str,
    ) -> anyhow::Result<bool> {
        let Some(db) = self.db.read().await.clone() else {
            return Ok(true);
        };
        let mut last_error = None;
        for attempt in 0..ACTION_DB_RETRY_ATTEMPTS {
            let action_id = id.to_string();
            let reason = error_reason.map(str::to_owned);
            let finished_at_for_db = finished_at.to_string();
            match db
                .run_blocking(move |db| {
                    db.finish_scheduled_action(
                        &action_id,
                        status,
                        reason.as_deref(),
                        &finished_at_for_db,
                    )
                })
                .await
            {
                Ok(changed) => return Ok(changed),
                Err(error) => {
                    last_error = Some(error);
                    if attempt + 1 < ACTION_DB_RETRY_ATTEMPTS {
                        tokio::time::sleep(ACTION_DB_RETRY_DELAY).await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("scheduled terminal persistence failed")))
    }

    async fn retry_scheduled_terminal_persistence(
        self: &Arc<Self>,
        id: String,
        schedule: ScheduledActionEntry,
        started_at: String,
        status: ActionStatus,
        error_reason: Option<String>,
        finished_at: String,
    ) {
        if !self
            .terminal_persistence_retries
            .write()
            .await
            .insert(id.clone())
        {
            return;
        }
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let mut delay = Duration::from_secs(1);
            loop {
                tokio::select! {
                    _ = service.shutdown_token.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                match service
                    .persist_scheduled_terminal(&id, status, error_reason.as_deref(), &finished_at)
                    .await
                {
                    Ok(true) => {
                        let state = match status {
                            ActionStatus::Completed => ActionState::Completed {
                                output: String::new(),
                                exit_code: None,
                                truncated: false,
                                log_path: None,
                                started_at: started_at.clone(),
                                finished_at: finished_at.clone(),
                            },
                            ActionStatus::Failed => ActionState::Failed {
                                error: error_reason.clone().unwrap_or_default(),
                                error_reason: error_reason.clone().unwrap_or_default(),
                                log_path: None,
                                exit_code: None,
                                started_at: started_at.clone(),
                                finished_at: finished_at.clone(),
                            },
                            _ => break,
                        };
                        service
                            .finish_scheduled_in_memory(&id, &schedule, state)
                            .await;
                        break;
                    }
                    Ok(false) => {
                        tracing::warn!(
                            action_id = %id,
                            "scheduled terminal retry found no running durable row"
                        );
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(
                            action_id = %id,
                            "scheduled terminal persistence retry failed: {error}"
                        );
                        delay = (delay * 2).min(Duration::from_secs(30));
                    }
                }
            }
            service
                .terminal_persistence_retries
                .write()
                .await
                .remove(&id);
        });
    }

    async fn finish_scheduled(
        self: &Arc<Self>,
        id: &str,
        status: ActionStatus,
        error_reason: Option<&str>,
    ) -> anyhow::Result<bool> {
        let _mutation = self.spawn_gate.lock().await;
        let (schedule, started_at) = {
            let actions = self.actions.read().await;
            let Some(action) = actions.get(id) else {
                return Ok(false);
            };
            let ActionState::Running { started_at } = &action.state else {
                return Ok(false);
            };
            let Some(schedule) = action.scheduled.as_ref() else {
                return Ok(false);
            };
            (schedule.clone(), started_at.clone())
        };
        let finished_at = chrono::Utc::now().to_rfc3339();
        if schedule.watch_action_id.is_none() {
            match self
                .persist_scheduled_terminal(id, status, error_reason, &finished_at)
                .await
            {
                Ok(true) => {}
                Ok(false) => return Ok(false),
                Err(error) => {
                    self.retry_scheduled_terminal_persistence(
                        id.to_string(),
                        schedule.clone(),
                        started_at.clone(),
                        status,
                        error_reason.map(str::to_owned),
                        finished_at.clone(),
                    )
                    .await;
                    return Err(anyhow::anyhow!(
                        "failed to persist scheduled action terminal state: {error}"
                    ));
                }
            }
        }
        let state = match status {
            ActionStatus::Completed => ActionState::Completed {
                output: String::new(),
                exit_code: None,
                truncated: false,
                log_path: None,
                started_at,
                finished_at,
            },
            ActionStatus::Failed => ActionState::Failed {
                error: error_reason.unwrap_or_default().to_string(),
                error_reason: error_reason.unwrap_or_default().to_string(),
                log_path: None,
                exit_code: None,
                started_at,
                finished_at,
            },
            _ => return Ok(false),
        };
        Ok(self.finish_scheduled_in_memory(id, &schedule, state).await)
    }

    fn emit_scheduled_finished(&self, id: &str, entry: &ScheduledActionEntry, state: &ActionState) {
        self.emit("action:finished", scheduled_finished_json(id, entry, state));
    }

    async fn cancel_scheduled(&self, id: &str, owner: Option<&str>) -> bool {
        let _mutation = self.spawn_gate.lock().await;
        let (schedule, started_at) = {
            let actions = self.actions.read().await;
            let Some(action) = actions.get(id) else {
                return false;
            };
            if action.kind != ActionKind::Scheduled
                || !action.state.status().is_live()
                || owner.is_some_and(|value| action.session_id.as_deref() != Some(value))
            {
                return false;
            }
            let Some(schedule) = action.scheduled.as_ref() else {
                return false;
            };
            let started_at = match &action.state {
                // A waiting schedule has not started. Keep this empty in the
                // in-memory terminal projection; the durable repository leaves
                // `started_at` NULL for the same reason.
                ActionState::Waiting { .. } => String::new(),
                ActionState::Running { started_at } => started_at.clone(),
                _ => return false,
            };
            (schedule.clone(), started_at)
        };
        let finished_at = chrono::Utc::now().to_rfc3339();
        if schedule.watch_action_id.is_none()
            && let Some(db) = self.db.read().await.clone()
        {
            let action_id = id.to_string();
            let finished_at_for_db = finished_at.clone();
            let mut last_error = None;
            for attempt in 0..ACTION_DB_RETRY_ATTEMPTS {
                let action_id = action_id.clone();
                let finished_at_for_db = finished_at_for_db.clone();
                match db
                    .run_blocking(move |db| {
                        db.cancel_scheduled_action(&action_id, &finished_at_for_db)
                    })
                    .await
                {
                    Ok(true) => {
                        last_error = None;
                        break;
                    }
                    Ok(false) => return false,
                    Err(error) => {
                        last_error = Some(error);
                        if attempt + 1 < ACTION_DB_RETRY_ATTEMPTS {
                            tokio::time::sleep(ACTION_DB_RETRY_DELAY).await;
                        }
                    }
                }
            }
            if let Some(error) = last_error {
                tracing::warn!(
                    action_id = %id,
                    "failed to persist scheduled action cancellation after retries: {error}"
                );
                // Keep both the waiting/running memory state and its timer. A
                // false result is deliberately not a cancellation claim; the
                // caller must keep showing the live action and may retry.
                return false;
            }
        }
        let state = ActionState::Cancelled {
            started_at,
            finished_at,
        };
        let mut actions = self.actions.write().await;
        let Some(action) = actions.get_mut(id) else {
            return false;
        };
        if !action.state.status().is_live() {
            return false;
        }
        action.state = state.clone();
        self.clear_scheduled_fire_claim(id).await;
        drop(actions);
        self.emit_scheduled_finished(id, &schedule, &state);
        true
    }

    async fn cancel_owned_scheduled_by_session(&self, session_id: &str) {
        let ids: Vec<_> = {
            let actions = self.actions.read().await;
            actions
                .iter()
                .filter_map(|(id, entry)| {
                    let schedule = entry.scheduled.as_ref()?;
                    if entry.kind == ActionKind::Scheduled
                        && entry.state.status().is_live()
                        && schedule.session_id.as_deref() == Some(session_id)
                    {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect()
        };
        for id in ids {
            let _ = self.cancel_scheduled(&id, Some(session_id)).await;
        }
    }

    /// Restore every persisted action family through one entry point.
    pub async fn restore(self: &Arc<Self>) -> (usize, usize) {
        let scheduled = Arc::clone(self).restore_pending().await;
        let interrupted = self.restore_after_restart().await;
        (scheduled, interrupted)
    }

    /// Re-arm persisted timers. Process rows are restored by
    /// [`restore_after_restart`], but both are deliberately exposed through
    /// this service rather than separate registries.
    pub async fn restore_pending(self: &Arc<Self>) -> usize {
        let Some(db) = self.db.read().await.clone() else {
            return 0;
        };
        let rows = match db
            .run_blocking(|db| db.list_pending_scheduled_actions())
            .await
        {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!("restore_pending: failed to load scheduled actions: {error}");
                return 0;
            }
        };
        let now = chrono::Utc::now();
        let mut overdue = 0;
        for row in rows {
            if self.actions.read().await.contains_key(&row.id) {
                continue;
            }
            let due = match chrono::DateTime::parse_from_rfc3339(&row.due_at) {
                Ok(value) => value.with_timezone(&chrono::Utc),
                Err(error) => {
                    tracing::warn!(action_id = %row.id, "skipping scheduled action with invalid due_at: {error}");
                    self.quarantine_invalid_scheduled_row(
                        &row.id,
                        "定时任务 due_at 无效，已隔离为失败",
                    )
                    .await;
                    continue;
                }
            };
            let Some(mode) = crate::builtin::scheduled_action::ScheduleMode::parse(&row.mode)
            else {
                tracing::warn!(action_id = %row.id, "skipping scheduled action with invalid mode");
                self.quarantine_invalid_scheduled_row(&row.id, "定时任务 mode 无效，已隔离为失败")
                    .await;
                continue;
            };
            let tool_args = match row.tool_args.as_deref() {
                Some(value) => match serde_json::from_str(value) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        tracing::warn!(action_id = %row.id, "skipping scheduled action with invalid tool_args: {error}");
                        self.quarantine_invalid_scheduled_row(
                            &row.id,
                            "定时任务 tool_args 无效，已隔离为失败",
                        )
                        .await;
                        continue;
                    }
                },
                None => None,
            };
            let valid_payload = match mode {
                crate::builtin::scheduled_action::ScheduleMode::Tool => row
                    .tool_name
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty()),
                crate::builtin::scheduled_action::ScheduleMode::Continue => {
                    row.session_id
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                        && row
                            .prompt
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty())
                }
            };
            if !valid_payload {
                tracing::warn!(
                    action_id = %row.id,
                    mode = %row.mode,
                    "skipping scheduled action with missing mode-specific payload"
                );
                self.quarantine_invalid_scheduled_row(
                    &row.id,
                    "定时任务缺少 mode 所需载荷，已隔离为失败",
                )
                .await;
                continue;
            }
            let entry = ScheduledActionEntry {
                title: row.title,
                body: row.body,
                due_at: row.due_at,
                mode,
                session_id: row.session_id,
                tool_name: row.tool_name,
                tool_args,
                prompt: row.prompt,
                watch_action_id: None,
            };
            let timer_entry = entry.clone();
            let id = row.id;
            self.actions.write().await.insert(
                id.clone(),
                ActionEntry {
                    kind: ActionKind::Scheduled,
                    session_id: entry.session_id.clone(),
                    state: ActionState::Waiting {
                        due_at: entry.due_at.clone(),
                    },
                    kill: None,
                    tail: None,
                    command: String::new(),
                    shell: String::new(),
                    scheduled: Some(entry),
                },
            );
            let remaining = (due - now).num_seconds();
            if remaining <= 0 {
                self.fire_scheduled(&id).await;
                overdue += 1;
            } else {
                self.arm_scheduled_worker(id.clone(), &timer_entry);
            }
        }
        overdue
    }
}

fn scheduled_status_json(
    id: &str,
    entry: &ScheduledActionEntry,
    state: &ActionState,
    _owner: Option<&str>,
) -> Value {
    let mut value = json!({
        "id": id,
        "action_id": id,
        "kind": "scheduled",
        "status": state.status().as_str(),
        "title": entry.title,
        "body": entry.body,
        "mode": entry.mode.as_str(),
        "session_id": entry.session_id,
        "tool_name": entry.tool_name,
        "tool_args": entry.tool_args,
        "prompt": entry.prompt,
        "watch_action_id": entry.watch_action_id,
        "due_at": entry.due_at,
    });
    if let ActionState::Running { started_at } = state {
        value["started_at"] = json!(started_at);
    }
    value
}

fn scheduled_finished_json(id: &str, entry: &ScheduledActionEntry, state: &ActionState) -> Value {
    let mut value = json!({
        "id": id,
        "action_id": id,
        "kind": "scheduled",
        "status": state.status().as_str(),
        "title": entry.title,
        "body": entry.body,
        "mode": entry.mode.as_str(),
        "session_id": entry.session_id,
        "due_at": entry.due_at,
    });
    match state {
        ActionState::Completed {
            started_at,
            finished_at,
            ..
        }
        | ActionState::Cancelled {
            started_at,
            finished_at,
        }
        | ActionState::Failed {
            started_at,
            finished_at,
            ..
        } => {
            if !started_at.is_empty() {
                value["started_at"] = json!(started_at);
            }
            value["finished_at"] = json!(finished_at);
        }
        _ => {}
    }
    if let ActionState::Failed { error_reason, .. } = state {
        value["error_reason"] = json!(error_reason);
    }
    value
}

fn action_finished_prompt(action_id: &str, status: &Value) -> String {
    let state = status["status"].as_str().unwrap_or("unknown");
    if state == "not_found" {
        return format!("Background action {action_id} not found.");
    }
    let payload = status["output"]
        .as_str()
        .or_else(|| status["error_reason"].as_str())
        .or_else(|| status["error"].as_str())
        .unwrap_or_default();
    format!("Background action {action_id} {state}.\nOutput:\n{payload}")
}

/// Attach a bounded `preview` (first 200 chars of output, else error) to a
/// status row. Shared by the board and scoped-list views.
fn attach_preview(row: &mut Value) {
    let preview = row
        .get("output")
        .and_then(|v| v.as_str())
        .or_else(|| row.get("error").and_then(|v| v.as_str()))
        .unwrap_or("");
    row["preview"] = json!(preview.chars().take(200).collect::<String>());
}

/// Render the running-state row for a action: the command line it is executing
/// and the bounded live-output tail, so the agent sees what the action is doing
/// right now instead of only "running". `output` is omitted while empty (the
/// command has not produced anything yet).
fn running_status_json(action_id: &str, entry: &ActionEntry) -> Value {
    let mut v = json!({
        "action_id": action_id,
        "status": "running",
        "command": entry.command,
        "shell": entry.shell,
    });
    if let ActionState::Running { started_at } = &entry.state {
        v["started_at"] = json!(started_at);
    }
    if let Some(tail) = &entry.tail {
        let out = lock_or_recover(tail, "action_output_tail");
        if !out.is_empty() {
            v["output"] = json!(out.as_str());
        }
    }
    v
}

/// Render the terminal status JSON for a action (mirrors `status()` output for
/// completed/failed/cancelled states), used in completion notifications.
fn render_status_json(action_id: &str, state: &ActionState) -> Value {
    match state {
        ActionState::Completed {
            output,
            exit_code,
            truncated,
            log_path,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "action_id": action_id,
                "status": "completed",
                "output": output,
                "started_at": started_at,
                "finished_at": finished_at,
            });
            if let Some(code) = exit_code {
                v["exit_code"] = json!(code);
            }
            if *truncated {
                v["truncated"] = json!(true);
            }
            if let Some(p) = log_path {
                v["log_path"] = json!(p);
            }
            v
        }
        ActionState::Failed {
            error,
            error_reason,
            log_path,
            exit_code,
            started_at,
            finished_at,
        } => {
            let mut v = json!({
                "action_id": action_id,
                "status": "failed",
                "error": error,
                "error_reason": error_reason,
                "started_at": started_at,
                "finished_at": finished_at,
            });
            if let Some(code) = exit_code {
                v["exit_code"] = json!(code);
            }
            if let Some(p) = log_path {
                v["log_path"] = json!(p);
            }
            v
        }
        ActionState::Cancelled {
            started_at,
            finished_at,
        } => json!({
            "action_id": action_id,
            "status": "cancelled",
            "started_at": started_at,
            "finished_at": finished_at,
        }),
        ActionState::Waiting { due_at } => {
            json!({ "action_id": action_id, "status": "waiting", "due_at": due_at })
        }
        ActionState::Running { .. } => {
            json!({ "action_id": action_id, "status": "running" })
        }
    }
}

#[cfg(test)]
#[path = "action_service_tests.rs"]
mod tests;
