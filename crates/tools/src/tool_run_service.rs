use haven_common::ToolRunStatus;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{RwLock, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::tool_run_completion::ToolRunCompletionBus;
pub use crate::tool_run_completion::{
    BackgroundToolRunCompletion, ScheduledToolRunResultCompletion, ToolRunCompletion,
    ToolRunCompletionReceiver,
};
use crate::tool_run_output::{
    ToolRunOutputPort, ToolRunOutputTail, ToolRunTailFactory, ToolRunTailSnapshot,
};
use crate::tool_run_retry_policy::{
    RetryDecision, RetrySignal, ToolRunPersistenceRetryPolicy, ToolRunStoreRetryPolicy,
};
use crate::tool_run_terminal::{
    TerminalPayload, TerminalSource, TerminalTimestamps, TerminalTransitionGuard, ToolRunState,
    can_claim_terminal,
};
use crate::tool_run_trigger_policy::{ScheduledTrigger, ScheduledTriggerRequest};
use crate::tool_run_types::{ScheduleMode, ScheduledToolRunFired, ScheduledToolRunSpec};
use crate::{
    ToolRunLifecycle, ToolRunLifecycleEvent, ToolRunLifecyclePayload, ToolRunLifecycleState,
    ToolRunLifecycleUpdate, ToolRunOutputPayload, ToolRunSessionAttachedPayload,
};
use haven_memory::{ToolRunCompletionOutboxRow, ToolRunRow, ToolRunStore};

use crate::output::{append_windows_diagnostics, sanitize_shell_output, summarize_error};
use crate::process::{kill_process_tree, read_stream_capped};
use crate::shell_runtime::{build_shell_command, collect_byte_cap, write_output_log};

mod background;
mod scheduled;
mod views;

pub(crate) use background::BackgroundShellRequest;

#[cfg(test)]
use scheduled::tool_run_finished_prompt;
pub use views::{
    ScheduledToolRunView, ToolRunListView, ToolRunStateView, ToolRunStatusView, ToolRunView,
};
use views::{
    list_view_started_at, project_board_tool_run, render_background_status_json,
    render_status_json, scheduled_lifecycle_payload, scheduled_tool_run_view,
    tool_run_lifecycle_state,
};

/// Optional typed sink for the closed ToolRun lifecycle event contract.
pub use crate::tool_run_lifecycle::EventSink;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRunKind {
    Background,
    Scheduled,
}

/// Counts produced by restoring process-local ToolRun state after startup.
/// The first field counts overdue scheduled rows passed to the fire path;
/// the second counts previous-process running rows marked failed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolRunRestoreSummary {
    pub overdue_scheduled_runs: usize,
    pub interrupted_runs_marked_failed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DependencyStatus {
    Waiting,
    Running,
    Completed(Option<String>),
    Failed(Option<String>),
    Cancelled,
    NotFound,
}

impl DependencyStatus {
    fn from_state(state: &ToolRunState) -> Self {
        match state {
            ToolRunState::Waiting => Self::Waiting,
            ToolRunState::Running { .. } => Self::Running,
            ToolRunState::Completed { output, .. } => {
                Self::Completed(non_empty_result(Some(output.as_str())))
            }
            ToolRunState::Failed {
                error,
                error_reason,
                ..
            } => Self::Failed(non_empty_result(Some(if error_reason.is_empty() {
                error
            } else {
                error_reason
            }))),
            ToolRunState::Cancelled { .. } => Self::Cancelled,
        }
    }

    fn from_durable(row: haven_memory::ToolRunDependencyRow) -> Self {
        match row.status {
            ToolRunStatus::Waiting => Self::Waiting,
            ToolRunStatus::Running => Self::Running,
            ToolRunStatus::Completed => Self::Completed(non_empty_result(row.result.as_deref())),
            ToolRunStatus::Failed => Self::Failed(non_empty_result(row.result.as_deref())),
            ToolRunStatus::Cancelled => Self::Cancelled,
        }
    }
}

fn non_empty_result(result: Option<&str>) -> Option<String> {
    result
        .map(str::trim)
        .filter(|result| !result.is_empty())
        .map(str::to_string)
}

#[derive(Clone, Debug)]
pub(crate) struct ScheduledToolRunEntry {
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) due_at: String,
    pub(crate) mode: ScheduleMode,
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_args: Option<Value>,
    pub(crate) prompt: Option<String>,
    pub(crate) watch_tool_run_id: Option<String>,
}

struct ScheduledTerminalRetry {
    id: String,
    schedule: ScheduledToolRunEntry,
    started_at: String,
    status: ToolRunStatus,
    result_summary: Option<String>,
    error_reason: Option<String>,
    finished_at: String,
}

fn scheduled_terminal_state(
    status: ToolRunStatus,
    result_summary: Option<&str>,
    error_reason: Option<&str>,
    timestamps: TerminalTimestamps,
) -> Option<ToolRunState> {
    let payload = match status {
        ToolRunStatus::Completed => TerminalPayload::Completed {
            output: result_summary.unwrap_or_default().to_string(),
            exit_code: None,
            truncated: false,
            log_path: None,
        },
        ToolRunStatus::Failed => {
            let reason = error_reason.unwrap_or_default().to_string();
            TerminalPayload::Failed {
                error: reason.clone(),
                error_reason: reason,
                log_path: None,
                exit_code: None,
            }
        }
        ToolRunStatus::Waiting | ToolRunStatus::Running | ToolRunStatus::Cancelled => return None,
    };
    Some(timestamps.build(payload))
}

struct ToolRunEntry {
    kind: ToolRunKind,
    session_id: Option<String>,
    source_step_id: Option<String>,
    state: ToolRunState,
    /// Kill signal for the running child process.
    kill: Option<oneshot::Sender<()>>,
    /// Bounded tail of the combined live output, for `tool_run:output` preview
    /// events while the ToolRun runs. `None` for terminal entries.
    tail: Option<ToolRunOutputTail>,
    /// The shell command this ToolRun is executing (surfaced in running status so
    /// the agent can see what the ToolRun is doing right now).
    command: String,
    /// Interpreter the command runs under ("cmd", "powershell", "bash", ...).
    shell: String,
    /// Timer/dependency spec for scheduled ToolRuns. Background and scheduled
    /// ToolRuns share this map; only their worker-specific spec differs.
    scheduled: Option<ScheduledToolRunEntry>,
}

#[derive(Default)]
struct OwnedToolRunSelection {
    live_ids: Vec<String>,
    terminal_ids: Vec<String>,
}

/// True when a terminal entry has outlived the configured terminal ToolRun TTL
/// (running entries are never stale). Entries with an unparseable
/// `finished_at` are kept (never wrongly reaped).
fn terminal_entry_stale(entry: &ToolRunEntry, ttl: Duration) -> bool {
    let finished = match &entry.state {
        ToolRunState::Completed { finished_at, .. }
        | ToolRunState::Failed { finished_at, .. }
        | ToolRunState::Cancelled { finished_at, .. } => finished_at,
        ToolRunState::Running { .. } | ToolRunState::Waiting => return false,
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
        // ToolRun registry's perspective; retain the entry rather than panic
        // during cleanup.
        return false;
    };
    chrono::Utc::now() - finished_ts > ttl
}

/// Unified state machine and runtime registry for every ToolRun kind.
///
/// A process ToolRun is spawned with `spawn_shell`, runs detached from the ReAct
/// loop, and is polled with `status`. Timer-triggered and dependency-triggered ToolRuns enter the
/// same map in `Waiting`, then use the same cancellation, ownership, lifecycle
/// and completion paths.
///
/// When a ToolRun reaches a terminal state, its typed completion is sent on the
/// unified completion bus so the agent layer can auto-inject the result into
/// the owning session's context without model polling.
pub struct ToolRunService {
    tool_runs: RwLock<HashMap<String, ToolRunEntry>>,
    /// Serializes spawn admission and durable registration. An ToolRun is not
    /// visible to cancellation until its `running` row is durable, avoiding
    /// orphaned DB rows or processes across the spawn failure window.
    spawn_gate: Arc<tokio::sync::Mutex<()>>,
    /// Serializes terminal arbitration for both ToolRun kinds. The database CAS
    /// remains authoritative across service instances; this gate makes
    /// in-memory transitions first-wins while a durable transition is in flight.
    terminal_transition: TerminalTransitionGuard,
    /// Accepted ScheduledToolRun execution claims. This fast in-memory view is
    /// paired with the durable `scheduled_execution_claim.<tool_run_id>` kv row
    /// so cancellation and approval also arbitrate across service instances.
    scheduled_execution_claims: RwLock<HashMap<String, String>>,
    /// Transient completion transport and scheduled-fire recovery claims.
    completion_bus: ToolRunCompletionBus,
    /// At most one retry worker is allowed for each scheduled ToolRun whose
    /// terminal DB write failed. The worker is cancelled with the service and
    /// stops once the durable transition succeeds.
    terminal_persistence_retries: RwLock<HashSet<String>>,
    /// At most one retry worker per background ToolRun whose terminal commit
    /// failed. The worker retains the complete candidate until the durable CAS
    /// commits or another terminal state wins.
    background_terminal_retries: RwLock<HashSet<String>>,
    /// At most one quarantine retry task is allowed per malformed ToolRun row.
    quarantine_persistence_retries: RwLock<HashSet<String>>,
    /// Max concurrent *running* tool_runs (from `context_limits.background_max_tool_runs`).
    max_tool_runs: RwLock<usize>,
    /// Unique owner of bounded ToolRun-output tail policy for lifecycle and
    /// foreground tool-card previews.
    output_port: ToolRunOutputPort,
    /// Cadence of `tool_run:output` events while a ToolRun produces output (from
    /// `context_limits.background_tool_run_output_emit_interval_ms`).
    tool_run_output_emit_interval: RwLock<Duration>,
    /// Terminal ToolRuns stay on the board this long, then are reaped (from
    /// `context_limits.tool_run_terminal_ttl_secs`).
    tool_run_terminal_ttl: RwLock<Duration>,
    /// Max pending timer/dependency tool_runs.
    max_scheduled_tool_runs: RwLock<usize>,
    /// Upper bound for absolute timer schedules.
    max_due_horizon_secs: RwLock<i64>,
    /// Optional UI event sink (see `EventSink`). Wired by the desktop shell
    /// to forward lifecycle events as Tauri events.
    event_sink: ToolRunLifecycle,
    /// Persistent ToolRun store; `None` in headless/test builds (in-memory only).
    /// Terminal ToolRun rows stay here as history even after the in-memory board
    /// reaps them (`TERMINAL_JOB_TTL`), so results survive app restarts.
    tool_run_store: RwLock<Option<ToolRunStore>>,
    /// Cancels process runners, output preview loops and scheduled timers
    /// during application teardown.
    shutdown_token: CancellationToken,
    shutting_down: AtomicBool,
}

impl Default for ToolRunService {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRunService {
    pub fn new() -> Self {
        Self {
            tool_runs: RwLock::new(HashMap::new()),
            spawn_gate: Arc::new(tokio::sync::Mutex::new(())),
            terminal_transition: TerminalTransitionGuard::default(),
            scheduled_execution_claims: RwLock::new(HashMap::new()),
            completion_bus: ToolRunCompletionBus::new(),
            terminal_persistence_retries: RwLock::new(HashSet::new()),
            background_terminal_retries: RwLock::new(HashSet::new()),
            quarantine_persistence_retries: RwLock::new(HashSet::new()),
            max_tool_runs: RwLock::new(64),
            output_port: ToolRunOutputPort::new(),
            tool_run_output_emit_interval: RwLock::new(Duration::from_millis(1500)),
            tool_run_terminal_ttl: RwLock::new(Duration::from_secs(600)),
            max_scheduled_tool_runs: RwLock::new(32),
            max_due_horizon_secs: RwLock::new(365 * 24 * 3600),
            event_sink: ToolRunLifecycle::default(),
            tool_run_store: RwLock::new(None),
            shutdown_token: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Unified completion receiver consumed by the agent layer.
    pub fn take_tool_run_receiver(&self) -> Option<ToolRunCompletionReceiver> {
        Some(self.completion_bus.subscribe())
    }

    pub(crate) fn output_tail_factory(&self) -> ToolRunTailFactory {
        self.output_port.tail_factory()
    }

    pub(crate) async fn pending_scheduled_fire(&self) -> Option<ScheduledToolRunFired> {
        self.completion_bus.pending_scheduled_fire().await
    }

    pub(crate) async fn claim_pending_tool_run_result(&self) -> Option<ToolRunCompletion> {
        let store = self.tool_run_store.read().await.clone()?;
        match store.claim_pending_completion().await {
            Ok(Some(ToolRunCompletionOutboxRow {
                tool_run_id,
                tool_run_result_id,
                kind,
                session_id,
                status,
                status_json,
            })) => match kind.as_str() {
                "background" => Some(ToolRunCompletion::Background(BackgroundToolRunCompletion {
                    tool_run_id,
                    tool_run_result_id,
                    session_id,
                    status,
                    status_json,
                })),
                "scheduled" => Some(ToolRunCompletion::ScheduledResult(
                    ScheduledToolRunResultCompletion {
                        tool_run_id,
                        tool_run_result_id,
                        session_id,
                        status,
                        status_json,
                    },
                )),
                _ => {
                    tracing::warn!(
                        tool_run_id,
                        kind,
                        "ignoring ToolRun result with unknown kind"
                    );
                    None
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::debug!("ToolRun completion outbox reconcile failed: {error}");
                None
            }
        }
    }

    /// Acknowledge an ToolRun result after the agent's transcript/event projection
    /// is durable. Queue admission alone is deliberately insufficient: a
    /// session may become terminal and clear its actor queue immediately after
    /// admission.
    pub async fn acknowledge_tool_run_completion(&self, tool_run_result_id: &str) {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return;
        };
        let tool_run_result_id = tool_run_result_id.to_string();
        let tool_run_result_id_for_log = tool_run_result_id.clone();
        if let Err(error) = store.acknowledge_completion(tool_run_result_id).await {
            tracing::warn!(
                tool_run_result_id = %tool_run_result_id_for_log,
                "failed to acknowledge durable ToolRun completion: {error}"
            );
        }
    }

    /// Acknowledge a completion with no owning session, guarded against a
    /// concurrent or subsequent late session binding.
    pub async fn acknowledge_unowned_tool_run_completion(&self, tool_run_result_id: &str) {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return;
        };
        let tool_run_result_id = tool_run_result_id.to_string();
        if let Err(error) = store
            .acknowledge_unowned_completion(tool_run_result_id.clone())
            .await
        {
            tracing::warn!(
                tool_run_result_id = %tool_run_result_id,
                "failed to acknowledge unowned durable ToolRun completion: {error}"
            );
        }
    }

    /// Install the UI event sink (called once by the desktop shell).
    pub fn set_event_sink(&self, sink: EventSink) {
        self.event_sink.set_event_sink(sink);
    }

    /// Forward a lifecycle event to the installed sink (no-op without one).
    fn emit(&self, event: ToolRunLifecycleEvent) {
        self.event_sink.emit(event);
    }

    /// Replace the unified context limits (background ToolRun concurrency cap,
    /// live-output tail size, output-event cadence, terminal ToolRun TTL).
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_tool_runs.write().await = limits.background_max_tool_runs;
        self.output_port
            .set_tail_max_chars(limits.background_tool_run_tail_max_chars)
            .await;
        *self.tool_run_output_emit_interval.write().await =
            Duration::from_millis(limits.background_tool_run_output_emit_interval_ms);
        *self.tool_run_terminal_ttl.write().await =
            Duration::from_secs(limits.tool_run_terminal_ttl_secs);
        *self.max_scheduled_tool_runs.write().await = limits.scheduled_tool_runs_max;
        *self.max_due_horizon_secs.write().await = limits.scheduled_tool_runs_due_horizon_secs;
    }

    /// Attach the ToolRun persistence port. Headless/test builds leave it unset.
    pub async fn set_tool_run_store(&self, tool_run_store: Option<ToolRunStore>) {
        *self.tool_run_store.write().await = tool_run_store;
    }

    /// List persisted ToolRun rows through the configured ToolRun store. Callers
    /// must treat a missing binding as a configuration error rather than empty
    /// history.
    pub async fn list_persisted_tool_runs(
        &self,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        let store = self
            .tool_run_store
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ToolRunService ToolRun store is not configured"))?;
        store.list_tool_runs(kind.map(str::to_owned)).await
    }

    /// List persisted ToolRuns owned by one session for its conversation
    /// timeline. This remains a read-only projection through the ToolRun store.
    pub async fn list_persisted_tool_runs_for_session(
        &self,
        session_id: &str,
        kind: Option<&str>,
    ) -> anyhow::Result<Vec<ToolRunRow>> {
        let store = self
            .tool_run_store
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ToolRunService ToolRun store is not configured"))?;
        store
            .list_tool_runs_for_session(session_id.to_string(), kind.map(str::to_owned))
            .await
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
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(true);
        };
        let mut last_error = None;
        let retry_policy = ToolRunStoreRetryPolicy::inline_store();
        for attempt in 1..=retry_policy.max_attempts() {
            match store
                .quarantine_waiting_scheduled_tool_run(
                    id.to_string(),
                    reason.to_string(),
                    finished_at.to_string(),
                )
                .await
            {
                Ok(changed) => return Ok(changed),
                Err(error) => {
                    last_error = Some(error);
                    match retry_policy.decide(attempt, true) {
                        RetryDecision::Retry { delay, .. } => tokio::time::sleep(delay).await,
                        RetryDecision::Stop { .. } => break,
                    }
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("malformed scheduled ToolRun quarantine failed")))
    }

    /// Move a malformed row out of the pending set. If the short inline retry
    /// budget is exhausted, retain a single per-row background retry with
    /// exponential backoff. This prevents a transient DB outage from turning
    /// a durable `waiting` row into a permanently invisible ToolRun.
    async fn quarantine_invalid_scheduled_row(self: &Arc<Self>, id: &str, reason: &str) {
        let finished_at = chrono::Utc::now().to_rfc3339();
        match self
            .try_quarantine_invalid_scheduled_row(id, reason, &finished_at)
            .await
        {
            Ok(_) => return,
            Err(error) => {
                tracing::warn!(
                    tool_run_id = %id,
                    "failed to quarantine malformed scheduled ToolRun; scheduling retry: {error}"
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
        let tool_run_id = id.to_string();
        let reason = reason.to_string();
        tokio::spawn(async move {
            let retry_policy = ToolRunPersistenceRetryPolicy::terminal_persistence();
            let mut completed_attempts = 1;
            while let RetryDecision::Retry {
                next_attempt,
                delay,
            } = retry_policy.decide(
                completed_attempts,
                RetrySignal::Failure { retryable: true },
                tokio::time::Instant::now(),
            ) {
                tokio::select! {
                    _ = service.shutdown_token.cancelled() => break,
                    _ = tokio::time::sleep(delay) => {}
                }
                completed_attempts = next_attempt;
                match service
                    .try_quarantine_invalid_scheduled_row(&tool_run_id, &reason, &finished_at)
                    .await
                {
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(
                            tool_run_id = %tool_run_id,
                            "malformed scheduled ToolRun quarantine retry failed: {error}"
                        );
                    }
                }
            }
            service
                .quarantine_persistence_retries
                .write()
                .await
                .remove(&tool_run_id);
        });
    }

    async fn clear_scheduled_fire_claim(&self, id: &str) {
        self.completion_bus.clear_scheduled_fire(id).await;
    }

    async fn rollback_background_registration(&self, tool_run_id: &str) {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            self.tool_runs.write().await.remove(tool_run_id);
            return;
        };

        let mut delete_error = None;
        let mut deleted = false;
        let retry_policy = ToolRunStoreRetryPolicy::inline_store();
        for attempt in 1..=retry_policy.max_attempts() {
            match store.delete_tool_run(tool_run_id.to_string()).await {
                Ok(true) | Ok(false) => {
                    deleted = true;
                    break;
                }
                Err(error) => {
                    delete_error = Some(error);
                    match retry_policy.decide(attempt, true) {
                        RetryDecision::Retry { delay, .. } => tokio::time::sleep(delay).await,
                        RetryDecision::Stop { .. } => break,
                    }
                }
            }
        }

        if deleted {
            self.tool_runs.write().await.remove(tool_run_id);
            return;
        }

        // The process/containment admission failed, so leaving the durable row
        // as `running` would create a restart-only ghost. Preserve a terminal
        // failure as a last-resort compensation; restore_after_restart can then
        // safely treat the row as history even if the delete path is unavailable.
        let finished_at = chrono::Utc::now().to_rfc3339();
        let id = tool_run_id.to_string();
        let reason = "background ToolRun failed before its process was admitted";
        let mut fallback_error = None;
        for attempt in 1..=retry_policy.max_attempts() {
            match store
                .finish_background_tool_run(
                    id.clone(),
                    ToolRunStatus::Failed,
                    None,
                    Some(reason.to_string()),
                    Some(reason.to_string()),
                    None,
                    None,
                    finished_at.clone(),
                )
                .await
            {
                Ok(_) => {
                    fallback_error = None;
                    break;
                }
                Err(error) => {
                    fallback_error = Some(error);
                    match retry_policy.decide(attempt, true) {
                        RetryDecision::Retry { delay, .. } => tokio::time::sleep(delay).await,
                        RetryDecision::Stop { .. } => break,
                    }
                }
            }
        }
        if let Some(error) = fallback_error {
            tracing::warn!(
                tool_run_id,
                delete_error = ?delete_error,
                fallback_error = %error,
                "failed to compensate background ToolRun registration rollback"
            );
        }

        let mut tool_runs = self.tool_runs.write().await;
        if let Some(entry) = tool_runs.get_mut(tool_run_id) {
            entry.kill = None;
            entry.tail = None;
            let started_at = match &entry.state {
                ToolRunState::Running { started_at } => started_at.clone(),
                _ => finished_at.clone(),
            };
            entry.state =
                TerminalTimestamps::new(started_at, finished_at).build(TerminalPayload::Failed {
                    error: reason.to_string(),
                    error_reason: reason.to_string(),
                    log_path: None,
                    exit_code: None,
                });
        }
    }

    /// Stop ToolRun workers owned by the application.
    ///
    /// Running background processes are cancelled and become terminal so the
    /// ToolRun board cannot retain a ghost `running` row. Pending scheduled rows
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

        // Serialize shutdown with ToolRun admission. This closes the window in
        // which a process could be published after the application has begun
        // teardown.
        let _spawn_gate = self.spawn_gate.lock().await;
        self.shutdown_token.cancel();
        let ids = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs
                .iter()
                .filter(|(_, entry)| {
                    matches!(
                        (entry.kind, &entry.state),
                        (
                            ToolRunKind::Background | ToolRunKind::Scheduled,
                            ToolRunState::Running { .. }
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

    /// Post-restart cleanup: ToolRun rows a previous process left `running` are
    /// stale (their child processes died with the app), so mark them failed.
    /// Called once from the agent layer startup. Returns the number of rows
    /// marked. Idempotent.
    pub async fn restore_after_restart(&self) -> usize {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return 0;
        };
        store
            .mark_interrupted_tool_runs()
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("restore_after_restart: failed to mark interrupted tool_runs: {e}");
                0
            })
    }

    /// Persist a terminal ToolRun row and completion outbox record. `Ok(false)`
    /// means another terminal transition already won the durable CAS. A
    /// service without an ToolRun store uses its in-memory transition as the
    /// commit.
    async fn persist_terminal(
        &self,
        tool_run_id: &str,
        state: &ToolRunState,
        status_json: &Value,
    ) -> anyhow::Result<bool> {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(true);
        };
        let (output, error, error_reason, log_path, exit_code, finished_at) = match state {
            ToolRunState::Completed {
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
            ToolRunState::Failed {
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
            ToolRunState::Cancelled { finished_at, .. } => {
                (None, None, None, None, None, finished_at.as_str())
            }
            ToolRunState::Running { .. } | ToolRunState::Waiting => {
                anyhow::bail!("cannot persist non-terminal background ToolRun status")
            }
        };
        let tool_run_id = tool_run_id.to_string();
        let status = state.status();
        let output = output.map(str::to_owned);
        let error = error.map(str::to_owned);
        let error_reason = error_reason.map(str::to_owned);
        let log_path = log_path.map(str::to_owned);
        let finished_at = finished_at.to_string();
        match status {
            ToolRunStatus::Completed | ToolRunStatus::Failed => {
                store
                    .finish_background_tool_run_with_completion(
                        tool_run_id,
                        status,
                        output,
                        error,
                        error_reason,
                        log_path,
                        exit_code,
                        finished_at,
                        status_json.clone(),
                    )
                    .await
            }
            ToolRunStatus::Cancelled => {
                store
                    .cancel_background_tool_run(tool_run_id, finished_at)
                    .await
            }
            ToolRunStatus::Waiting | ToolRunStatus::Running => {
                anyhow::bail!("cannot persist non-terminal background ToolRun status")
            }
        }
    }

    /// Try one terminal transition. Both memory-only transitions and durable
    /// transitions are first-wins; when a database is configured, its CAS must
    /// commit before the memory projection or either notification is published.
    async fn try_commit_background_terminal(
        self: &Arc<Self>,
        tool_run_id: &str,
        state: &ToolRunState,
        remove_after_commit: bool,
    ) -> anyhow::Result<bool> {
        if !state.is_terminal() {
            return Ok(false);
        }

        let _terminal = self.terminal_transition.lock().await;
        let session_id = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(tool_run_id).and_then(|entry| {
                (entry.kind == ToolRunKind::Background
                    && can_claim_terminal(
                        entry.state.status(),
                        state.status(),
                        TerminalSource::Live,
                    ))
                .then(|| (entry.session_id.clone(), entry.source_step_id.clone()))
            })
        };
        let Some((session_id, source_step_id)) = session_id else {
            if remove_after_commit {
                self.tool_runs.write().await.remove(tool_run_id);
            }
            return Ok(false);
        };

        let status_json =
            render_background_status_json(tool_run_id, state, source_step_id.as_deref());
        match self
            .persist_terminal(tool_run_id, state, &status_json)
            .await
        {
            Ok(true) => {
                {
                    let mut tool_runs = self.tool_runs.write().await;
                    if let Some(entry) = tool_runs.get_mut(tool_run_id)
                        && entry.kind == ToolRunKind::Background
                    {
                        entry.kill = None;
                        entry.tail = None;
                        entry.state = state.clone();
                    }
                }
                self.publish_committed_terminal(
                    tool_run_id,
                    state.clone(),
                    session_id,
                    source_step_id,
                );
                if remove_after_commit {
                    self.tool_runs.write().await.remove(tool_run_id);
                }
                Ok(true)
            }
            Ok(false) => {
                // The durable row is authoritative when this process lost the
                // CAS. Align its runtime projection, but do not publish: only
                // the transaction that changed `running` may notify.
                self.reconcile_background_terminal(tool_run_id).await;
                if remove_after_commit {
                    self.tool_runs.write().await.remove(tool_run_id);
                }
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    async fn reconcile_background_terminal(&self, tool_run_id: &str) {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return;
        };
        let row = match store.get_tool_run(tool_run_id.to_string()).await {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(
                    tool_run_id,
                    "failed to read ToolRun after losing terminal CAS: {error}"
                );
                return;
            }
        };
        let Some(row) = row else {
            return;
        };
        let timestamps = TerminalTimestamps::new(
            row.started_at.unwrap_or_default(),
            row.finished_at.unwrap_or_default(),
        );
        let state = match row.status {
            ToolRunStatus::Completed => timestamps.clone().build(TerminalPayload::Completed {
                output: row.output.unwrap_or_default(),
                exit_code: row.exit_code,
                truncated: row.log_path.is_some(),
                log_path: row.log_path,
            }),
            ToolRunStatus::Failed => timestamps.clone().build(TerminalPayload::Failed {
                error: row.error.unwrap_or_default(),
                error_reason: row.error_reason.unwrap_or_default(),
                log_path: row.log_path,
                exit_code: row.exit_code,
            }),
            ToolRunStatus::Cancelled => timestamps.build(TerminalPayload::Cancelled),
            ToolRunStatus::Waiting | ToolRunStatus::Running => return,
        };
        let mut tool_runs = self.tool_runs.write().await;
        if let Some(entry) = tool_runs.get_mut(tool_run_id)
            && entry.kind == ToolRunKind::Background
        {
            entry.kill = None;
            entry.tail = None;
            entry.state = state;
        }
    }

    async fn retry_background_terminal_persistence(
        self: &Arc<Self>,
        tool_run_id: &str,
        state: ToolRunState,
        remove_after_commit: bool,
    ) {
        if !self
            .background_terminal_retries
            .write()
            .await
            .insert(tool_run_id.to_string())
        {
            return;
        }
        let service = Arc::clone(self);
        let tool_run_id = tool_run_id.to_string();
        tokio::spawn(async move {
            let policy = ToolRunPersistenceRetryPolicy::terminal_persistence();
            let mut completed_attempts = 1;
            let mut signal = RetrySignal::Failure { retryable: true };
            while let RetryDecision::Retry {
                next_attempt,
                delay,
            } = policy.decide(completed_attempts, signal, tokio::time::Instant::now())
            {
                let should_retry = tokio::select! {
                    _ = service.shutdown_token.cancelled() => false,
                    _ = tokio::time::sleep(delay) => true,
                };
                if !should_retry {
                    signal = RetrySignal::Cancelled;
                    continue;
                }
                if policy
                    .can_start_attempt(next_attempt, tokio::time::Instant::now())
                    .is_err()
                {
                    break;
                }
                match service
                    .try_commit_background_terminal(&tool_run_id, &state, remove_after_commit)
                    .await
                {
                    Ok(true) => signal = RetrySignal::Succeeded,
                    Ok(false) => signal = RetrySignal::Terminal,
                    Err(error) => {
                        tracing::warn!(
                            tool_run_id = %tool_run_id,
                            "background terminal persistence retry failed: {error}"
                        );
                        completed_attempts = next_attempt;
                        signal = RetrySignal::Failure { retryable: true };
                    }
                }
            }
            service
                .background_terminal_retries
                .write()
                .await
                .remove(&tool_run_id);
        });
    }

    /// Publish only after the matching terminal write has committed. A failed
    /// transient broadcast is recoverable from the durable completion outbox.
    fn publish_committed_terminal(
        &self,
        tool_run_id: &str,
        state: ToolRunState,
        session_id: Option<String>,
        source_step_id: Option<String>,
    ) {
        debug_assert!(state.is_terminal());
        let mut event = ToolRunLifecyclePayload::new(
            ToolRunKind::Background,
            tool_run_id,
            tool_run_lifecycle_state(&state),
        );
        event.source_step_id = source_step_id.clone();
        match &state {
            ToolRunState::Completed {
                output, exit_code, ..
            } => {
                event.output = Some(output.clone());
                event.exit_code = *exit_code;
            }
            ToolRunState::Failed {
                error,
                error_reason,
                exit_code,
                ..
            } => {
                event.error = Some(error.clone());
                event.error_reason = Some(error_reason.clone());
                event.exit_code = *exit_code;
            }
            ToolRunState::Cancelled { .. } => {}
            ToolRunState::Waiting | ToolRunState::Running { .. } => {
                unreachable!("terminal publication requires a terminal ToolRun state")
            }
        }
        self.emit(ToolRunLifecycleEvent::Finished(event));
        self.publish_background_completion(tool_run_id, state, session_id, source_step_id);
    }

    fn publish_background_completion(
        &self,
        tool_run_id: &str,
        state: ToolRunState,
        session_id: Option<String>,
        source_step_id: Option<String>,
    ) {
        let status = match &state {
            ToolRunState::Completed { .. } => ToolRunStatus::Completed,
            ToolRunState::Failed { .. } => ToolRunStatus::Failed,
            ToolRunState::Cancelled { .. } => ToolRunStatus::Cancelled,
            ToolRunState::Running { .. } | ToolRunState::Waiting => return,
        };
        let status_json =
            render_background_status_json(tool_run_id, &state, source_step_id.as_deref());
        if let Err(error) =
            self.completion_bus
                .send(ToolRunCompletion::Background(BackgroundToolRunCompletion {
                    tool_run_id: tool_run_id.to_string(),
                    tool_run_result_id: tool_run_id.to_string(),
                    session_id,
                    status,
                    status_json,
                }))
        {
            tracing::debug!(
                tool_run_id = %tool_run_id,
                error = %error,
                "no ToolRun completion subscriber is currently attached"
            );
        }
    }

    /// Board view of every ToolRun: one entry per run with status, timestamps,
    /// owning session id, and a bounded output/error preview. Surfaces the full
    /// ToolRun set to the UI (the per-session variant `list_for_session` serves the
    /// agent). Order: oldest first.
    pub async fn board(&self) -> Vec<ToolRunView> {
        let tool_runs = self.tool_runs.read().await;
        let mut rows = Vec::new();
        for (id, entry) in tool_runs.iter() {
            match entry.kind {
                ToolRunKind::Background => rows.push(project_board_tool_run(id, entry)),
                ToolRunKind::Scheduled
                    if entry.state.status().is_live() && entry.scheduled.is_some() =>
                {
                    rows.push(project_board_tool_run(id, entry));
                }
                ToolRunKind::Scheduled => {}
            }
        }
        rows.sort_by(|a: &ToolRunView, b: &ToolRunView| {
            a.started_at.as_deref().cmp(&b.started_at.as_deref())
        });
        rows
    }

    /// Typed agent-facing board projection. JSON conversion is intentionally
    /// deferred until the ToolRuns tool has applied its status filter.
    pub async fn list_for_session_views(&self, session_id: &str) -> Vec<ToolRunListView> {
        let tool_runs = self.tool_runs.read().await;
        let mut rows = Vec::new();
        for (id, entry) in tool_runs.iter() {
            if entry.kind != ToolRunKind::Background {
                if let Some(schedule) = &entry.scheduled
                    && entry.state.status().is_live()
                    && entry.session_id.as_deref() == Some(session_id)
                {
                    let state = ToolRunStateView::from_entry(entry);
                    let projection = ToolRunStatusView::Scheduled {
                        tool_run_id: id.clone(),
                        session_id: entry.session_id.clone(),
                        schedule: Box::new(ScheduledToolRunView {
                            title: schedule.title.clone(),
                            body: schedule.body.clone(),
                            due_at: schedule.due_at.clone(),
                            mode: schedule.mode.as_str().to_string(),
                            tool_name: schedule.tool_name.clone(),
                            tool_args: schedule.tool_args.clone(),
                            prompt: schedule.prompt.clone(),
                            watch_tool_run_id: schedule.watch_tool_run_id.clone(),
                        }),
                        state,
                    };
                    rows.push(ToolRunListView {
                        status: projection.status().expect("scheduled view has status"),
                        projection,
                        kind: ToolRunKind::Scheduled,
                        session_id: entry.session_id.clone(),
                        preview: String::new(),
                    });
                }
                continue;
            }
            if entry.session_id.as_deref() != Some(session_id) {
                continue;
            }
            let state = ToolRunStateView::from_entry(entry);
            let projection = ToolRunStatusView::Background {
                tool_run_id: id.clone(),
                source_step_id: entry.source_step_id.clone(),
                state: state.clone(),
            };
            rows.push(ToolRunListView {
                status: state.status(),
                projection,
                kind: ToolRunKind::Background,
                session_id: entry.session_id.clone(),
                preview: state.preview(),
            });
        }
        rows.sort_by(|left, right| list_view_started_at(left).cmp(&list_view_started_at(right)));
        rows
    }

    /// Typed unscoped status projection.
    pub async fn status_view(&self, tool_run_id: &str) -> ToolRunStatusView {
        let tool_runs = self.tool_runs.read().await;
        let Some(entry) = tool_runs.get(tool_run_id) else {
            return ToolRunStatusView::NotFound {
                tool_run_id: tool_run_id.to_string(),
            };
        };
        if entry.kind == ToolRunKind::Scheduled {
            let Some(schedule) = entry.scheduled.as_ref() else {
                return ToolRunStatusView::NotFound {
                    tool_run_id: tool_run_id.to_string(),
                };
            };
            if !entry.state.is_waiting() {
                return ToolRunStatusView::ScheduledTerminal {
                    tool_run_id: tool_run_id.to_string(),
                    status: entry.state.status(),
                };
            }
            return ToolRunStatusView::Scheduled {
                tool_run_id: tool_run_id.to_string(),
                session_id: entry.session_id.clone(),
                schedule: Box::new(scheduled_tool_run_view(schedule)),
                state: ToolRunStateView::from_entry(entry),
            };
        }
        ToolRunStatusView::Background {
            tool_run_id: tool_run_id.to_string(),
            source_step_id: entry.source_step_id.clone(),
            state: ToolRunStateView::from_entry(entry),
        }
    }

    async fn dependency_status(&self, tool_run_id: &str) -> anyhow::Result<DependencyStatus> {
        let state = self
            .tool_runs
            .read()
            .await
            .get(tool_run_id)
            .map(|entry| entry.state.clone());
        if let Some(state) = state {
            return Ok(DependencyStatus::from_state(&state));
        }
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(DependencyStatus::NotFound);
        };
        Ok(store
            .get_tool_run_dependency(tool_run_id.to_string())
            .await?
            .map(DependencyStatus::from_durable)
            .unwrap_or(DependencyStatus::NotFound))
    }

    /// Typed status lookup scoped to the owning session.
    pub async fn status_for_session_view(
        &self,
        tool_run_id: &str,
        session_id: &str,
    ) -> ToolRunStatusView {
        let tool_runs = self.tool_runs.read().await;
        let Some(entry) = tool_runs.get(tool_run_id) else {
            return ToolRunStatusView::NotFound {
                tool_run_id: tool_run_id.to_string(),
            };
        };
        if entry.kind == ToolRunKind::Scheduled {
            let Some(schedule) = entry.scheduled.as_ref() else {
                return ToolRunStatusView::NotFound {
                    tool_run_id: tool_run_id.to_string(),
                };
            };
            if entry.session_id.as_deref() != Some(session_id) || !entry.state.status().is_live() {
                return ToolRunStatusView::NotFound {
                    tool_run_id: tool_run_id.to_string(),
                };
            }
            return ToolRunStatusView::Scheduled {
                tool_run_id: tool_run_id.to_string(),
                session_id: entry.session_id.clone(),
                schedule: Box::new(scheduled_tool_run_view(schedule)),
                state: ToolRunStateView::from_entry(entry),
            };
        }
        if entry.session_id.as_deref() != Some(session_id) {
            return ToolRunStatusView::NotFound {
                tool_run_id: tool_run_id.to_string(),
            };
        }
        ToolRunStatusView::Background {
            tool_run_id: tool_run_id.to_string(),
            source_step_id: entry.source_step_id.clone(),
            state: ToolRunStateView::from_entry(entry),
        }
    }

    /// Request cancellation of a live ToolRun (kept for inspection afterwards).
    /// A scheduled ToolRun can be cancelled only before execution is claimed;
    /// after approval or operation start, cancellation returns `false` and the
    /// claimed work reports its own terminal result. Background tool_runs retain
    /// their process-signal behavior.
    pub async fn cancel(&self, tool_run_id: &str) -> bool {
        let mut tool_runs = self.tool_runs.write().await;
        let Some(entry) = tool_runs.get_mut(tool_run_id) else {
            return false;
        };
        if entry.kind == ToolRunKind::Scheduled {
            drop(tool_runs);
            return self.cancel_scheduled(tool_run_id, None).await;
        }
        if !matches!(entry.state, ToolRunState::Running { .. }) {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Request cancellation only when the ToolRun belongs to `session_id`.
    /// Scheduled ToolRuns follow the same pre-execution-claim cancellation rule.
    pub async fn cancel_for_session(&self, tool_run_id: &str, session_id: &str) -> bool {
        let mut tool_runs = self.tool_runs.write().await;
        let Some(entry) = tool_runs.get_mut(tool_run_id) else {
            return false;
        };
        if entry.kind == ToolRunKind::Scheduled {
            drop(tool_runs);
            return self.cancel_scheduled(tool_run_id, Some(session_id)).await;
        }
        if entry.session_id.as_deref() != Some(session_id)
            || !matches!(entry.state, ToolRunState::Running { .. })
        {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Cancel only when the caller's kind discriminator matches the owned
    /// ToolRun. The app IPC layer must not be able to cancel a scheduled row by
    /// presenting it as a background ToolRun (or vice versa).
    pub async fn cancel_for_kind(&self, tool_run_id: &str, kind: &str) -> bool {
        let expected = match kind {
            "background" | "scheduled" => kind,
            _ => return false,
        };
        let matches = self
            .tool_runs
            .read()
            .await
            .get(tool_run_id)
            .is_some_and(|entry| {
                matches!(
                    (expected, entry.kind),
                    ("background", ToolRunKind::Background) | ("scheduled", ToolRunKind::Scheduled)
                )
            });
        matches && self.cancel(tool_run_id).await
    }

    /// Delete terminal ToolRun history through the same owner that controls
    /// live ToolRun state. Waiting/running rows, kind mismatches and absent rows
    /// are all rejected and return `false`.
    pub async fn delete(&self, tool_run_id: &str, kind: &str) -> anyhow::Result<bool> {
        let expected = match kind {
            "background" => ToolRunKind::Background,
            "scheduled" => ToolRunKind::Scheduled,
            _ => return Ok(false),
        };
        let _mutation = self.spawn_gate.lock().await;
        let memory_terminal = {
            let tool_runs = self.tool_runs.read().await;
            match tool_runs.get(tool_run_id) {
                Some(entry) if entry.kind == expected => entry.state.is_terminal(),
                Some(_) => return Ok(false),
                None => false,
            }
        };
        if !memory_terminal {
            if let Some(store) = self.tool_run_store.read().await.clone() {
                let row = store.get_tool_run(tool_run_id.to_string()).await?;
                let Some(row) = row else {
                    return Ok(false);
                };
                if row.kind != kind || !row.status.is_terminal() {
                    return Ok(false);
                }
                if !store.delete_tool_run(tool_run_id.to_string()).await? {
                    return Ok(false);
                }
            } else {
                return Ok(false);
            }
        } else if let Some(store) = self.tool_run_store.read().await.clone()
            && let Some(row) = store.get_tool_run(tool_run_id.to_string()).await?
        {
            if row.kind != kind || !row.status.is_terminal() {
                return Ok(false);
            }
            if !store.delete_tool_run(tool_run_id.to_string()).await? {
                return Ok(false);
            }
        }
        self.tool_runs.write().await.remove(tool_run_id);
        Ok(true)
    }

    /// Delete terminal history when the IPC surface does not need to expose a
    /// kind discriminator. The actual persisted/runtime kind is resolved first
    /// and then passed through the guarded kind-aware delete path.
    pub async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        let memory_kind = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(tool_run_id).map(|entry| match entry.kind {
                ToolRunKind::Background => "background",
                ToolRunKind::Scheduled => "scheduled",
            })
        };
        if let Some(kind) = memory_kind {
            return self.delete(tool_run_id, kind).await;
        }
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(false);
        };
        let Some(row) = store.get_tool_run(tool_run_id.to_string()).await? else {
            return Ok(false);
        };
        self.delete(tool_run_id, &row.kind).await
    }

    /// Clear persisted terminal ToolRun history without touching waiting or
    /// running work. The store performs the outbox-aware delete atomically;
    /// the in-memory board is then pruned for the exact rows that were removed.
    pub async fn clear_terminal_history(&self) -> anyhow::Result<u64> {
        let _mutation = self.spawn_gate.lock().await;
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(0);
        };
        let deleted_ids = store.clear_terminal_tool_runs().await?;
        if !deleted_ids.is_empty() {
            let mut tool_runs = self.tool_runs.write().await;
            for id in &deleted_ids {
                tool_runs.remove(id);
            }
        }
        Ok(deleted_ids.len() as u64)
    }

    /// Cancel all ToolRun kinds owned by `session_id`. This is used by explicit
    /// session end/deletion; application shutdown uses the background-only
    /// variant so durable scheduled work remains waiting.
    pub async fn cancel_owned_by_session(self: &Arc<Self>, session_id: &str) {
        if let Err(error) = self.cancel_owned_by_session_checked(session_id).await {
            tracing::warn!(session_id = %session_id, "failed to completely cancel session-owned ToolRuns: {error}");
        }
    }

    /// Cancel session-owned ToolRuns and return scheduled durable query/cancel
    /// failures so a destructive session lifecycle can leave the durable
    /// session in place for retry. Background cancellation remains best-effort
    /// and uses its bounded terminal-write retry. A scheduled execution claim
    /// that already won remains a legitimate first-wins outcome and is not an
    /// error.
    pub async fn cancel_owned_by_session_checked(
        self: &Arc<Self>,
        session_id: &str,
    ) -> anyhow::Result<()> {
        // Serialize both ToolRun kinds with admission. Background admission
        // must finish publishing its durable row and board entry before this
        // snapshot; scheduled admission already uses the same gate through its
        // durable write. Keep the established background-then-scheduled order.
        let _mutation = self.spawn_gate.lock().await;
        self.cancel_owned_background_by_session_locked(session_id)
            .await;
        self.cancel_owned_scheduled_by_session_checked_locked(session_id)
            .await
    }
}

impl ToolRunService {
    async fn cancel_owned_scheduled_by_session_checked_locked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let live_ids = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs
                .iter()
                .filter(|(_, entry)| {
                    entry.kind == ToolRunKind::Scheduled
                        && entry.session_id.as_deref() == Some(session_id)
                        && entry.state.status().is_live()
                        && entry.scheduled.is_some()
                })
                .map(|(id, _)| id.clone())
                .collect::<HashSet<_>>()
        };

        let mut first_error = None;
        for id in &live_ids {
            if let Err(error) = self.cancel_scheduled_locked(id, Some(session_id)).await {
                first_error.get_or_insert_with(|| {
                    error.context(format!("failed to cancel scheduled ToolRun {id}"))
                });
            }
        }

        if let Some(store) = self.tool_run_store.read().await.clone() {
            match store.list_live_scheduled_tool_runs().await {
                Ok(rows) => {
                    for row in rows.into_iter().filter(|row| {
                        row.session_id.as_deref() == Some(session_id) && !live_ids.contains(&row.id)
                    }) {
                        if let Err(error) =
                            self.cancel_untracked_scheduled_row(&store, &row.id).await
                        {
                            first_error.get_or_insert_with(|| {
                                error.context(format!(
                                    "failed to cancel untracked scheduled ToolRun {}",
                                    row.id
                                ))
                            });
                        }
                    }
                }
                Err(error) => {
                    first_error.get_or_insert_with(|| {
                        error.context("failed to list durable session-owned scheduled tool_runs")
                    });
                }
            }
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn cancel_untracked_scheduled_row(
        &self,
        store: &ToolRunStore,
        tool_run_id: &str,
    ) -> anyhow::Result<()> {
        let _terminal = self.terminal_transition.lock().await;
        let finished_at = chrono::Utc::now().to_rfc3339();
        let retry_policy = ToolRunStoreRetryPolicy::inline_store();
        let mut last_error = None;
        for attempt in 1..=retry_policy.max_attempts() {
            match store
                .cancel_scheduled_tool_run(tool_run_id.to_string(), finished_at.clone())
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) => {
                    last_error = Some(error);
                    match retry_policy.decide(attempt, true) {
                        RetryDecision::Retry { delay, .. } => tokio::time::sleep(delay).await,
                        RetryDecision::Stop { .. } => break,
                    }
                }
            }
        }
        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("scheduled ToolRun cancellation did not complete")))
    }

    /// Select once, then visit matching live owned ToolRuns sequentially without
    /// holding the board lock across a family-specific asynchronous callback.
    /// Terminal IDs are returned for background's existing board cleanup; they
    /// are never fed into cancellation callbacks. Callback return values remain
    /// intentionally ignored, so one false/error result cannot short-circuit
    /// the remaining ToolRuns; kind-specific logging and retries stay there.
    async fn cancel_owned_live_tool_runs<F, Fut, R>(
        &self,
        session_id: &str,
        kind: ToolRunKind,
        mut cancel: F,
    ) -> OwnedToolRunSelection
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = R>,
    {
        let selection = {
            let tool_runs = self.tool_runs.read().await;
            let mut selection = OwnedToolRunSelection::default();
            for (id, entry) in tool_runs.iter() {
                if entry.kind != kind || entry.session_id.as_deref() != Some(session_id) {
                    continue;
                }
                if entry.state.status().is_live() {
                    if kind != ToolRunKind::Scheduled || entry.scheduled.is_some() {
                        selection.live_ids.push(id.clone());
                    }
                } else if entry.state.is_terminal() {
                    selection.terminal_ids.push(id.clone());
                }
            }
            selection
        };

        for id in selection.live_ids.iter().cloned() {
            let _ = cancel(id).await;
        }
        selection
    }

    /// Restore every persisted ToolRun kind through one entry point.
    pub async fn restore(self: &Arc<Self>) -> ToolRunRestoreSummary {
        let interrupted_runs_marked_failed = self.restore_after_restart().await;
        let overdue_scheduled_runs = Arc::clone(self).restore_pending().await;
        ToolRunRestoreSummary {
            overdue_scheduled_runs,
            interrupted_runs_marked_failed,
        }
    }

    /// Re-arm persisted timers and dependency watchers. Running rows are first
    /// marked failed by [`restore_after_restart`]; recovery stays in this
    /// service rather than introducing a separate ToolRun registry.
    pub async fn restore_pending(self: &Arc<Self>) -> usize {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return 0;
        };
        let _mutation = self.spawn_gate.lock().await;
        let rows = match store.list_pending_scheduled_tool_runs().await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!("restore_pending: failed to load scheduled tool_runs: {error}");
                return 0;
            }
        };
        let now = chrono::Utc::now();
        let mut overdue_tool_run_ids = Vec::new();
        for row in rows {
            if self.tool_runs.read().await.contains_key(&row.id) {
                continue;
            }
            let watch_tool_run_id = row
                .watch_tool_run_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            if row.watch_tool_run_id.is_some() && watch_tool_run_id.is_none() {
                tracing::warn!(tool_run_id = %row.id, "skipping scheduled ToolRun with empty dependency id");
                self.quarantine_invalid_scheduled_row(
                    &row.id,
                    "定时任务依赖 ID 无效，已隔离为失败",
                )
                .await;
                continue;
            }
            let due = if watch_tool_run_id.is_some() {
                if !row.due_at.trim().is_empty() {
                    tracing::warn!(tool_run_id = %row.id, "skipping scheduled ToolRun with both timer and dependency triggers");
                    self.quarantine_invalid_scheduled_row(
                        &row.id,
                        "定时任务触发器配置冲突，已隔离为失败",
                    )
                    .await;
                    continue;
                }
                None
            } else {
                match chrono::DateTime::parse_from_rfc3339(&row.due_at) {
                    Ok(value) => Some(value.with_timezone(&chrono::Utc)),
                    Err(error) => {
                        tracing::warn!(tool_run_id = %row.id, "skipping scheduled ToolRun with invalid due_at: {error}");
                        self.quarantine_invalid_scheduled_row(
                            &row.id,
                            "定时任务 due_at 无效，已隔离为失败",
                        )
                        .await;
                        continue;
                    }
                }
            };
            let Some(mode) = ScheduleMode::parse(&row.mode) else {
                tracing::warn!(tool_run_id = %row.id, "skipping scheduled ToolRun with invalid mode");
                self.quarantine_invalid_scheduled_row(&row.id, "定时任务 mode 无效，已隔离为失败")
                    .await;
                continue;
            };
            let tool_args = match row.tool_args.as_deref() {
                Some(value) => match serde_json::from_str(value) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        tracing::warn!(tool_run_id = %row.id, "skipping scheduled ToolRun with invalid tool_args: {error}");
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
                ScheduleMode::Tool => {
                    watch_tool_run_id.is_none()
                        && row
                            .tool_name
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty())
                }
                ScheduleMode::Continue => {
                    row.session_id
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                        && (watch_tool_run_id.is_some()
                            || row
                                .prompt
                                .as_deref()
                                .is_some_and(|value| !value.trim().is_empty()))
                }
            };
            if !valid_payload {
                tracing::warn!(
                    tool_run_id = %row.id,
                    mode = %row.mode,
                    "skipping scheduled ToolRun with missing mode-specific payload"
                );
                self.quarantine_invalid_scheduled_row(
                    &row.id,
                    "定时任务缺少 mode 所需载荷，已隔离为失败",
                )
                .await;
                continue;
            }
            let entry = ScheduledToolRunEntry {
                title: row.title,
                body: row.body,
                due_at: row.due_at,
                mode,
                tool_name: row.tool_name,
                tool_args,
                prompt: row.prompt,
                watch_tool_run_id: watch_tool_run_id.clone(),
            };
            let timer_entry = entry.clone();
            let id = row.id;
            let session_id = row.session_id;
            self.tool_runs.write().await.insert(
                id.clone(),
                ToolRunEntry {
                    kind: ToolRunKind::Scheduled,
                    session_id,
                    source_step_id: None,
                    state: ToolRunState::Waiting,
                    kill: None,
                    tail: None,
                    command: String::new(),
                    shell: String::new(),
                    scheduled: Some(entry),
                },
            );
            if watch_tool_run_id.is_some() {
                self.arm_scheduled_worker(id, &timer_entry);
            } else {
                let remaining = (due.expect("timer trigger has due time") - now).num_seconds();
                if remaining <= 0 {
                    overdue_tool_run_ids.push(id);
                } else {
                    self.arm_scheduled_worker(id, &timer_entry);
                }
            }
        }
        drop(_mutation);
        let overdue = overdue_tool_run_ids.len();
        for id in overdue_tool_run_ids {
            self.fire_scheduled(&id).await;
        }
        overdue
    }
}

#[cfg(test)]
#[path = "tool_run_service_tests.rs"]
mod tests;
