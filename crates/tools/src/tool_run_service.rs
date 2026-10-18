use haven_common::ToolRunCompletionPayload;
use haven_common::lifecycle::ToolRunStatus;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{RwLock, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
#[cfg(test)]
use tracing::Instrument;

use crate::tool_run_completion::ToolRunCompletionBus;
pub use crate::tool_run_completion::{
    BackgroundToolRunCompletion, ToolRunCompletion, ToolRunCompletionReceiver,
};
#[cfg(test)]
use crate::tool_run_output::ToolRunOutputPort;
#[cfg(test)]
use crate::tool_run_output::ToolRunOutputTail;
#[cfg(test)]
#[cfg(test)]
use crate::tool_run_retry_policy::ToolRunStoreRetryPolicy;
use crate::tool_run_retry_policy::{RetryDecision, RetrySignal, ToolRunPersistenceRetryPolicy};
use crate::tool_run_terminal::{
    TerminalPayload, TerminalTimestamps, TerminalTransitionGuard, ToolRunState, can_claim_terminal,
};
use crate::{
    ToolRunLifecycle, ToolRunLifecycleEvent, ToolRunLifecyclePayload, ToolRunLifecycleState,
    ToolRunLifecycleUpdate, ToolRunSessionAttachedPayload,
};
use haven_memory::{ToolRunCompletionOutboxRow, ToolRunStore};

#[cfg(test)]
use crate::output::sanitize_shell_output;
use crate::output::summarize_error;
#[cfg(test)]
use crate::process::read_stream_text_capped;
#[cfg(not(test))]
use crate::shell_runtime::cleanup_shell_output_logs_after_restart;
#[cfg(test)]
use crate::shell_runtime::{build_shell_command, collect_byte_cap};
use crate::shell_runtime::{
    remove_orphaned_tool_run_output_logs, remove_tool_run_output_log, write_output_log,
};

mod background;
mod views;

#[cfg(test)]
pub(crate) use background::BackgroundShellRequest;

pub use views::{
    ToolRunHistoryQuery, ToolRunListView, ToolRunStateView, ToolRunStatusView, ToolRunView,
};
use views::{
    list_view_started_at, project_background_completion_payload, project_board_tool_run,
    project_persisted_background_board_row, project_persisted_tool_run, tool_run_lifecycle_state,
};

/// Optional typed sink for the closed ToolRun lifecycle event contract.
pub use crate::tool_run_lifecycle::ToolRunLifecycleEventSink;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolRunKind {
    Background,
}

impl ToolRunKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
        }
    }
}

/// Counts previous-process background ToolRuns marked failed during restore.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolRunRestoreSummary {
    pub interrupted_runs_marked_failed: usize,
}

struct ToolRunEntry {
    kind: ToolRunKind,
    tool_name: String,
    session_id: Option<String>,
    source_step_id: Option<String>,
    state: ToolRunState,
    /// Cancels the detached tool invocation.
    kill: Option<oneshot::Sender<()>>,
    /// Test-only output tail used by the shell lifecycle fixture.
    #[cfg(test)]
    tail: Option<ToolRunOutputTail>,
    #[cfg(test)]
    #[allow(dead_code)]
    command: String,
    #[cfg(test)]
    #[allow(dead_code)]
    shell: String,
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
        ToolRunState::Running { .. } => return false,
    };
    let finished_ts = match chrono::DateTime::parse_from_rfc3339(finished) {
        Ok(t) => t.with_timezone(&chrono::Utc),
        Err(_e) => {
            tracing::warn!("terminal ToolRun has an invalid completion timestamp");
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

/// State machine and runtime registry for detached background ToolRuns.
///
/// A background ToolRun invokes one already-authorized tool through the Tools
/// execution facade and continues independently of the ReAct loop.
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
    /// Serializes terminal arbitration for background ToolRuns. The database CAS
    /// remains authoritative across service instances; this gate makes
    /// in-memory transitions first-wins while a durable transition is in flight.
    terminal_transition: TerminalTransitionGuard,
    /// Transient completion transport for delivered background results.
    completion_bus: ToolRunCompletionBus,
    /// At most one retry worker per background ToolRun whose terminal commit
    /// failed. The worker retains the complete candidate until the durable CAS
    /// commits or another terminal state wins.
    background_terminal_retries: RwLock<HashSet<String>>,
    /// Maximum background ToolRuns in the running state.
    max_running_background_tool_runs: RwLock<usize>,
    /// Unique owner of bounded ToolRun-output tail policy for lifecycle and
    /// foreground tool-card previews.
    #[cfg(test)]
    output_port: ToolRunOutputPort,
    /// Terminal ToolRuns stay on the board this long, then are reaped (from
    /// `context_limits.tool_run_terminal_ttl_secs`).
    tool_run_terminal_ttl: RwLock<Duration>,
    /// Optional UI sink for ToolRun lifecycle events. Wired by the desktop
    /// shell to forward lifecycle events as Tauri events.
    lifecycle_events: ToolRunLifecycle,
    /// Persistent ToolRun store; `None` in headless/test builds (in-memory only).
    /// Terminal ToolRun rows stay here as history even after the in-memory board
    /// reaps them according to `tool_run_terminal_ttl_secs`, so results survive
    /// app restarts.
    tool_run_store: RwLock<Option<ToolRunStore>>,
    /// Cancels detached tool invocations
    /// during application teardown. Every service-owned task is tracked here
    /// so shutdown can wait until its future and destructor have completed.
    task_tracker: TaskTracker,
    /// Makes concurrent shutdown calls share one drain operation.
    shutdown_gate: tokio::sync::Mutex<()>,
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
            completion_bus: ToolRunCompletionBus::new(),
            background_terminal_retries: RwLock::new(HashSet::new()),
            max_running_background_tool_runs: RwLock::new(64),
            #[cfg(test)]
            output_port: ToolRunOutputPort::new(),
            tool_run_terminal_ttl: RwLock::new(Duration::from_secs(600)),
            lifecycle_events: ToolRunLifecycle::default(),
            tool_run_store: RwLock::new(None),
            task_tracker: TaskTracker::new(),
            shutdown_gate: tokio::sync::Mutex::new(()),
            shutdown_token: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
        }
    }

    /// Spawn a service-owned task. Admission is serialized by `spawn_gate` at
    /// every external entry point; children spawned by a tracked task remain
    /// covered until their parent exits, including during shutdown drain.
    fn spawn_owned_task<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.task_tracker.spawn(future)
    }

    /// Unified completion receiver consumed by the agent layer.
    pub fn take_tool_run_receiver(&self) -> Option<ToolRunCompletionReceiver> {
        Some(self.completion_bus.subscribe())
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
                payload,
            })) if kind == "background" => {
                Some(ToolRunCompletion::Background(BackgroundToolRunCompletion {
                    tool_run_id,
                    tool_run_result_id,
                    session_id,
                    status,
                    payload,
                }))
            }
            Ok(Some(row)) => {
                tracing::warn!(tool_run_id = %row.tool_run_id, "ignoring ToolRun result with unknown kind");
                None
            }
            Ok(None) => None,
            Err(error) => {
                tracing::debug!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "ToolRun completion outbox reconcile failed"
                );
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
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to acknowledge durable ToolRun completion"
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
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to acknowledge unowned durable ToolRun completion"
            );
        }
    }

    /// Install the UI sink for ToolRun lifecycle events (called by the desktop shell).
    pub fn set_lifecycle_event_sink(&self, sink: ToolRunLifecycleEventSink) {
        self.lifecycle_events.install_event_sink(sink);
    }

    /// Forward a lifecycle event to the installed sink (no-op without one).
    fn emit(&self, event: ToolRunLifecycleEvent) {
        self.lifecycle_events.emit(event);
    }

    /// Replace the context limits for background ToolRun concurrency and
    /// terminal ToolRun retention.
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.max_running_background_tool_runs.write().await =
            limits.max_running_background_tool_runs;
        *self.tool_run_terminal_ttl.write().await =
            Duration::from_secs(limits.tool_run_terminal_ttl_secs);
    }

    /// Attach the ToolRun persistence port. Headless/test builds leave it unset.
    pub async fn set_tool_run_store(&self, tool_run_store: Option<ToolRunStore>) {
        *self.tool_run_store.write().await = tool_run_store;
    }

    /// Load terminal background history for board hydration and project it into
    /// the safe application-facing ToolRun view.
    pub async fn list_board_history(&self) -> anyhow::Result<Vec<ToolRunView>> {
        let store = self
            .tool_run_store
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ToolRunService ToolRun store is not configured"))?;
        store
            .list_all_terminal_background_tool_runs()
            .await?
            .into_iter()
            .map(project_persisted_background_board_row)
            .collect()
    }

    /// Query terminal history through the configured store and return only the
    /// safe fields required by App's IPC projection. The query owns its page cap.
    pub async fn list_history(
        &self,
        query: ToolRunHistoryQuery,
    ) -> anyhow::Result<Vec<ToolRunView>> {
        let store = self
            .tool_run_store
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ToolRunService ToolRun store is not configured"))?;
        store
            .list_terminal_tool_runs(
                query.kind.map(ToolRunKind::as_str).map(str::to_owned),
                query.session_id,
                query.limit,
            )
            .await?
            .into_iter()
            .map(project_persisted_tool_run)
            .collect()
    }

    #[cfg(test)]
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
        let finished_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let tool_run_id = tool_run_id.to_string();
        let reason = "background ToolRun failed before its process was admitted";
        let mut fallback_error = None;
        for attempt in 1..=retry_policy.max_attempts() {
            match store
                .finish_background_tool_run(
                    tool_run_id.clone(),
                    ToolRunStatus::Failed,
                    None,
                    Some(reason.to_string()),
                    Some(reason.to_string()),
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
                delete_failed = delete_error.is_some(),
                fallback_error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "failed to compensate background ToolRun registration rollback"
            );
        }

        let mut tool_runs = self.tool_runs.write().await;
        if let Some(entry) = tool_runs.get_mut(&tool_run_id) {
            entry.kill = None;
            #[cfg(test)]
            {
                entry.tail = None;
            }
            let started_at = match &entry.state {
                ToolRunState::Running { started_at } => started_at.clone(),
                _ => finished_at.clone(),
            };
            entry.state =
                TerminalTimestamps::new(started_at, finished_at).build(TerminalPayload::Failed {
                    error: reason.to_string(),
                    error_reason: reason.to_string(),
                    log_path: None,
                });
        }
    }

    /// Stop ToolRun workers owned by the application.
    ///
    /// Running background processes are cancelled and become terminal so the
    /// ToolRun board cannot retain a ghost `running` row.
    pub async fn shutdown(&self) {
        let _shutdown = self.shutdown_gate.lock().await;
        if self.task_tracker.is_closed() && self.task_tracker.is_empty() {
            return;
        }

        // Serialize shutdown with ToolRun admission. This closes the window in
        // which a process could be published after the application has begun
        // teardown. Set the terminal flag only after acquiring the same gate,
        // so an admission already holding it can finish registering its task.
        let _spawn_gate = self.spawn_gate.lock().await;
        self.shutting_down.store(true, Ordering::Release);
        self.shutdown_token.cancel();
        let ids = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs
                .iter()
                .filter(|(_, entry)| matches!(entry.state, ToolRunState::Running { .. }))
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>()
        };
        drop(_spawn_gate);

        for id in ids {
            let _ = self.cancel(&id).await;
        }
        self.task_tracker.close();
        self.task_tracker.wait().await;
    }

    /// Post-restart cleanup: ToolRuns left `running` are failed because their
    /// in-process side effects cannot be known or safely replayed.
    /// Called once from the agent layer startup. Returns the number of rows
    /// marked failed. Idempotent.
    pub async fn restore_after_restart(&self) -> usize {
        #[cfg(not(test))]
        cleanup_shell_output_logs_after_restart();
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return 0;
        };
        match store.list_tool_run_output_log_ids().await {
            Ok(ids) => {
                let retained = ids.into_iter().collect::<HashSet<_>>();
                match remove_orphaned_tool_run_output_logs(&retained) {
                    Ok(0) => {}
                    Ok(removed) => tracing::debug!(
                        removed,
                        "removed orphaned ToolRun output logs after restart"
                    ),
                    Err(error) => tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "restore_after_restart: failed to clean orphaned ToolRun output logs"
                    ),
                }
            }
            Err(error) => tracing::warn!(
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "restore_after_restart: failed to load ToolRun output log references"
            ),
        }
        store
            .mark_interrupted_tool_runs()
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&e.to_string()),
                    "restore_after_restart: failed to mark interrupted tool_runs"
                );
                0
            })
    }

    /// Persist a terminal ToolRun row and completion outbox record. `Ok(false)`
    /// means another terminal transition already won the durable CAS. A
    /// service without an ToolRun store uses its in-memory transition as the
    /// commit.
    async fn persist_terminal(&self, payload: &ToolRunCompletionPayload) -> anyhow::Result<bool> {
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(true);
        };
        match payload.status {
            ToolRunStatus::Completed | ToolRunStatus::Failed => {
                store
                    .finish_background_tool_run_with_completion(payload.clone())
                    .await
            }
            ToolRunStatus::Cancelled => {
                store
                    .cancel_background_tool_run(
                        payload.tool_run_id.clone(),
                        payload.finished_at.clone().ok_or_else(|| {
                            anyhow::anyhow!("cancelled ToolRun completion requires finished_at")
                        })?,
                    )
                    .await
            }
            ToolRunStatus::Running => {
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
        let terminal_owner = {
            let tool_runs = self.tool_runs.read().await;
            tool_runs.get(tool_run_id).and_then(|entry| {
                (entry.kind == ToolRunKind::Background
                    && can_claim_terminal(entry.state.status(), state.status()))
                .then(|| {
                    (
                        entry.session_id.clone(),
                        entry.source_step_id.clone(),
                        entry.tool_name.clone(),
                    )
                })
            })
        };
        let Some((session_id, source_step_id, tool_name)) = terminal_owner else {
            if remove_after_commit {
                self.tool_runs.write().await.remove(tool_run_id);
            }
            return Ok(false);
        };

        let payload = project_background_completion_payload(
            tool_run_id,
            &tool_name,
            state,
            source_step_id.as_deref(),
        );
        match self.persist_terminal(&payload).await {
            Ok(true) => {
                {
                    let mut tool_runs = self.tool_runs.write().await;
                    if let Some(entry) = tool_runs.get_mut(tool_run_id)
                        && entry.kind == ToolRunKind::Background
                    {
                        entry.kill = None;
                        #[cfg(test)]
                        {
                            entry.tail = None;
                        }
                        entry.state = state.clone();
                    }
                }
                self.publish_committed_terminal(
                    tool_run_id,
                    state.clone(),
                    session_id,
                    source_step_id,
                )
                .await;
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
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to read ToolRun after losing terminal CAS"
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
                truncated: row.log_path.is_some(),
                log_path: row.log_path,
            }),
            ToolRunStatus::Failed => timestamps.clone().build(TerminalPayload::Failed {
                error: row.error.unwrap_or_default(),
                error_reason: row.error_reason.unwrap_or_default(),
                log_path: row.log_path,
            }),
            ToolRunStatus::Cancelled => timestamps.build(TerminalPayload::Cancelled),
            ToolRunStatus::Running => return,
        };
        let mut tool_runs = self.tool_runs.write().await;
        if let Some(entry) = tool_runs.get_mut(tool_run_id)
            && entry.kind == ToolRunKind::Background
        {
            entry.kill = None;
            #[cfg(test)]
            {
                entry.tail = None;
            }
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
        self.spawn_owned_task(async move {
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
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "background terminal persistence retry failed"
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
    async fn publish_committed_terminal(
        &self,
        tool_run_id: &str,
        state: ToolRunState,
        session_id: Option<String>,
        source_step_id: Option<String>,
    ) {
        debug_assert!(state.is_terminal());
        let Some(tool_name) = self
            .tool_runs
            .read()
            .await
            .get(tool_run_id)
            .map(|entry| entry.tool_name.clone())
        else {
            tracing::warn!(
                tool_run_id,
                "cannot publish ToolRun completion without its target tool identity"
            );
            return;
        };
        let mut event = ToolRunLifecyclePayload::new(
            ToolRunKind::Background,
            tool_run_id,
            tool_name.clone(),
            tool_run_lifecycle_state(&state),
        );
        event.source_step_id = source_step_id.clone();
        match &state {
            ToolRunState::Completed { output, .. } => {
                event.output = Some(output.clone());
            }
            ToolRunState::Failed {
                error,
                error_reason,
                ..
            } => {
                event.error = Some(error.clone());
                event.error_reason = Some(error_reason.clone());
            }
            ToolRunState::Cancelled { .. } => {}
            ToolRunState::Running { .. } => {
                unreachable!("terminal publication requires a terminal ToolRun state")
            }
        }
        self.emit(ToolRunLifecycleEvent::Finished(event));
        self.publish_background_completion(
            tool_run_id,
            tool_name,
            state,
            session_id,
            source_step_id,
        );
    }

    fn publish_background_completion(
        &self,
        tool_run_id: &str,
        tool_name: String,
        state: ToolRunState,
        session_id: Option<String>,
        source_step_id: Option<String>,
    ) {
        let status = match &state {
            ToolRunState::Completed { .. } => ToolRunStatus::Completed,
            ToolRunState::Failed { .. } => ToolRunStatus::Failed,
            ToolRunState::Cancelled { .. } => ToolRunStatus::Cancelled,
            ToolRunState::Running { .. } => return,
        };
        let payload = project_background_completion_payload(
            tool_run_id,
            &tool_name,
            &state,
            source_step_id.as_deref(),
        );
        if let Err(error) =
            self.completion_bus
                .send(ToolRunCompletion::Background(BackgroundToolRunCompletion {
                    tool_run_id: tool_run_id.to_string(),
                    tool_run_result_id: tool_run_id.to_string(),
                    session_id,
                    status,
                    payload,
                }))
        {
            tracing::debug!(
                tool_run_id = %tool_run_id,
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
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
            rows.push(project_board_tool_run(id, entry));
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
            if entry.session_id.as_deref() != Some(session_id) {
                continue;
            }
            let state = ToolRunStateView::from_entry(entry);
            let projection = ToolRunStatusView::Background {
                tool_run_id: id.clone(),
                tool_name: entry.tool_name.clone(),
                source_step_id: entry.source_step_id.clone(),
                state: state.clone(),
            };
            rows.push(ToolRunListView {
                status: state.status(),
                projection,
                kind: ToolRunKind::Background,
                tool_name: entry.tool_name.clone(),
                session_id: entry.session_id.clone(),
                preview: state.preview(),
            });
        }
        rows.sort_by(|left, right| list_view_started_at(left).cmp(&list_view_started_at(right)));
        rows
    }

    /// Typed unscoped status projection.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn status_view(&self, tool_run_id: &str) -> ToolRunStatusView {
        let tool_runs = self.tool_runs.read().await;
        let Some(entry) = tool_runs.get(tool_run_id) else {
            return ToolRunStatusView::NotFound {
                tool_run_id: tool_run_id.to_string(),
            };
        };
        ToolRunStatusView::Background {
            tool_run_id: tool_run_id.to_string(),
            tool_name: entry.tool_name.clone(),
            source_step_id: entry.source_step_id.clone(),
            state: ToolRunStateView::from_entry(entry),
        }
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
        if entry.session_id.as_deref() != Some(session_id) {
            return ToolRunStatusView::NotFound {
                tool_run_id: tool_run_id.to_string(),
            };
        }
        ToolRunStatusView::Background {
            tool_run_id: tool_run_id.to_string(),
            tool_name: entry.tool_name.clone(),
            source_step_id: entry.source_step_id.clone(),
            state: ToolRunStateView::from_entry(entry),
        }
    }

    /// Request cancellation of a live background ToolRun (kept for inspection afterwards).
    pub async fn cancel(&self, tool_run_id: &str) -> bool {
        let mut tool_runs = self.tool_runs.write().await;
        let Some(entry) = tool_runs.get_mut(tool_run_id) else {
            return false;
        };
        if !matches!(entry.state, ToolRunState::Running { .. }) {
            return false;
        }
        if let Some(tx) = entry.kill.take() {
            let _ = tx.send(());
        }
        true
    }

    /// Request cancellation only when the ToolRun belongs to `session_id`.
    pub async fn cancel_for_session(&self, tool_run_id: &str, session_id: &str) -> bool {
        let mut tool_runs = self.tool_runs.write().await;
        let Some(entry) = tool_runs.get_mut(tool_run_id) else {
            return false;
        };
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

    /// Delete terminal ToolRun history through the same owner that controls
    /// live ToolRun state. Waiting/running rows and absent rows are rejected.
    async fn delete(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        let _mutation = self.spawn_gate.lock().await;
        let memory_terminal = {
            let tool_runs = self.tool_runs.read().await;
            match tool_runs.get(tool_run_id) {
                Some(entry) => entry.state.is_terminal(),
                None => false,
            }
        };
        if !memory_terminal {
            if let Some(store) = self.tool_run_store.read().await.clone() {
                let row = store.get_tool_run(tool_run_id.to_string()).await?;
                let Some(row) = row else {
                    return Ok(false);
                };
                if row.kind != ToolRunKind::Background.as_str() || !row.status.is_terminal() {
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
            if row.kind != ToolRunKind::Background.as_str() || !row.status.is_terminal() {
                return Ok(false);
            }
            if !store.delete_tool_run(tool_run_id.to_string()).await? {
                return Ok(false);
            }
        }
        self.tool_runs.write().await.remove(tool_run_id);
        remove_tool_run_output_log(tool_run_id);
        Ok(true)
    }

    /// Delete terminal background ToolRun history.
    pub async fn delete_terminal(&self, tool_run_id: &str) -> anyhow::Result<bool> {
        if self.tool_runs.read().await.contains_key(tool_run_id) {
            return self.delete(tool_run_id).await;
        }
        let Some(store) = self.tool_run_store.read().await.clone() else {
            return Ok(false);
        };
        let Some(row) = store.get_tool_run(tool_run_id.to_string()).await? else {
            return Ok(false);
        };
        if row.kind != ToolRunKind::Background.as_str() {
            return Ok(false);
        }
        self.delete(tool_run_id).await
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
            drop(tool_runs);
            for id in &deleted_ids {
                remove_tool_run_output_log(id);
            }
        }
        Ok(deleted_ids.len() as u64)
    }

    /// Cancel session-owned background ToolRuns. Schedule cancellation belongs
    /// to ScheduleService and is coordinated by the Agent session lifecycle.
    pub async fn cancel_owned_background_by_session_checked(
        self: &Arc<Self>,
        session_id: &str,
    ) -> anyhow::Result<()> {
        let _mutation = self.spawn_gate.lock().await;
        if self.shutting_down.load(Ordering::Acquire) {
            return Ok(());
        }
        self.cancel_owned_background_by_session_locked(session_id)
            .await;
        Ok(())
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
                if entry.session_id.as_deref() != Some(session_id) {
                    continue;
                }
                if entry.state.status().is_live() {
                    selection.live_ids.push(id.clone());
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

    /// Restore process-local background ToolRun state after application start.
    pub async fn restore(self: &Arc<Self>) -> ToolRunRestoreSummary {
        let interrupted_runs_marked_failed = self.restore_after_restart().await;
        ToolRunRestoreSummary {
            interrupted_runs_marked_failed,
        }
    }
}

#[cfg(test)]
#[path = "tool_run_service_tests.rs"]
mod tests;
