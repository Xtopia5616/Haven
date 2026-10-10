use crate::interaction::{InteractionEnvelope, InteractionRequest};
use haven_common::SessionStepStatus;
pub use haven_common::lifecycle::SessionStatus;
pub use haven_common::lifecycle::SessionWaitingReason;
use haven_common::types::{
    CapabilityScope, MessageAttachment, PermissionEffect, PermissionTarget, RiskLevel,
};
use haven_memory::repositories::sessions::Session as DbSession;
#[cfg(test)]
use haven_memory::{Database, ToolRunStore};
use haven_memory::{SessionAuthorizationGrant, SessionStore};
#[cfg(test)]
use haven_tools::ToolsFacade;
use haven_tools::{
    AuthorizationDecision, AuthorizationEngine, ToolResult, ToolRunService, is_silent_tool_call,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
#[cfg(test)]
use tokio::sync::Notify;
use tokio::sync::{Mutex, broadcast, mpsc, watch};
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
pub type SessionRunHandler =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> + Send + Sync>;

/// A direct SessionRun blocked on admission capacity. The id unregisters only
/// this waiter; lifecycle operations cancel its token to wake the capacity wait.
struct DirectSessionRunAdmissionWaiter {
    waiter_id: usize,
    cancellation: CancellationToken,
}

type DirectSessionRunAdmissionWaiters = HashMap<String, Vec<DirectSessionRunAdmissionWaiter>>;

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
    sessions: Arc<StdMutex<HashMap<String, SessionClosingMode>>>,
    cascade_overrides: Arc<StdMutex<HashMap<String, bool>>>,
    retry_queue: Arc<TerminalCleanupRetryQueue>,
    session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionClosingMode {
    EndPreparing { cascade: bool },
    EndCommitted { cascade: bool },
    Destructive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalCleanupRetry {
    pub cascade: Option<bool>,
    pub remove_error_actor: bool,
}

impl Drop for SessionClosingGuard {
    fn drop(&mut self) {
        let retry = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session_id)
            .and_then(|mode| match mode {
                SessionClosingMode::EndCommitted { cascade } => Some(TerminalCleanupRetry {
                    cascade: Some(cascade),
                    remove_error_actor: true,
                }),
                SessionClosingMode::Destructive => Some(TerminalCleanupRetry {
                    cascade: None,
                    remove_error_actor: true,
                }),
                SessionClosingMode::EndPreparing { .. } => None,
            });
        if let Some(retry) = retry {
            self.retry_queue.enqueue(&self.session_id, retry);
        }
    }
}

impl SessionClosingGuard {
    /// Promote an End marker only after its Completed transition has committed.
    /// Callers hold `lifecycle_guard()` so run-exit observes either phase.
    pub(crate) fn mark_end_committed(&self) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match sessions.get_mut(&self.session_id) {
            Some(mode @ SessionClosingMode::EndPreparing { .. }) => {
                let SessionClosingMode::EndPreparing { cascade } = *mode else {
                    unreachable!();
                };
                *mode = SessionClosingMode::EndCommitted { cascade };
                self.cascade_overrides
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(self.session_id.clone(), cascade);
                true
            }
            Some(SessionClosingMode::EndCommitted { .. }) => true,
            _ => false,
        }
    }
}

/// Admission lease for the terminal run-exit cleanup window. It prevents a
/// Continue/rollback from reopening an actor after cleanup has claimed the
/// terminal state but before its owner has cleared projections and registry
/// state.
pub(crate) struct TerminalCleanupGuard {
    sessions: Arc<StdMutex<HashSet<String>>>,
    closing_sessions: Arc<StdMutex<HashMap<String, SessionClosingMode>>>,
    retry_queue: Arc<TerminalCleanupRetryQueue>,
    session_id: String,
    completed: bool,
    retry: TerminalCleanupRetry,
}

impl Drop for TerminalCleanupGuard {
    fn drop(&mut self) {
        self.sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.session_id);
        if !self.completed {
            let cascade =
                self.closing_sessions
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&self.session_id)
                    .and_then(|mode| match mode {
                        SessionClosingMode::EndCommitted { cascade } => Some(*cascade),
                        SessionClosingMode::EndPreparing { .. }
                        | SessionClosingMode::Destructive => None,
                    });
            self.retry_queue.enqueue(
                &self.session_id,
                TerminalCleanupRetry {
                    cascade: cascade.or(self.retry.cascade),
                    remove_error_actor: self.retry.remove_error_actor,
                },
            );
        }
    }
}

impl TerminalCleanupGuard {
    pub(crate) fn mark_complete(&mut self) {
        self.completed = true;
    }

    pub(crate) fn set_retry_policy(&mut self, retry: TerminalCleanupRetry) {
        self.retry = retry;
    }
}

/// De-duplicated retry wakeups for interrupted terminal cleanup. The worker is
/// tied to the session dispatcher lifetime; retries always re-read Actor state
/// under the lifecycle gate before claiming cleanup again.
pub(crate) struct TerminalCleanupRetryQueue {
    queued: StdMutex<HashMap<String, TerminalCleanupRetry>>,
    sender: mpsc::UnboundedSender<String>,
}

impl TerminalCleanupRetryQueue {
    fn enqueue(&self, session_id: &str, retry: TerminalCleanupRetry) {
        let mut queued = self
            .queued
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = queued.get_mut(session_id) {
            if existing.cascade.is_none() && retry.cascade.is_some() {
                existing.cascade = retry.cascade;
            }
            existing.remove_error_actor |= retry.remove_error_actor;
        } else {
            queued.insert(session_id.to_string(), retry);
            if self.sender.send(session_id.to_string()).is_err() {
                queued.remove(session_id);
            }
        }
    }

    pub(crate) fn take(&self, session_id: &str) -> Option<TerminalCleanupRetry> {
        self.queued
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id)
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
    /// Runtime/UI projection explaining why `status == Paused`. It is not
    /// persisted and is recomputed from interactions and live tool_runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_reason: Option<SessionWaitingReason>,
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
            summary: record.input_text.clone(),
            title: record.title.clone(),
            status: record.status,
            waiting_reason: None,
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
    pub status: SessionStepStatus,
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
    pub status: crate::interaction::InteractionStatus,
}

/// Typed side effects emitted by the supervisor. Consumers subscribe to this
/// stream; no subsystem installs mutable one-shot callbacks on the runtime.
#[derive(Debug, Clone)]
pub enum SessionSupervisorEvent {
    InteractionRequested {
        envelope: Box<InteractionEnvelope>,
    },
    /// The final pending confirmation woke a paused session.
    /// `AgentLayer` projects this through the existing session lifecycle event.
    SessionResumed {
        session_id: String,
    },
    /// A direct run was cancelled with its caller and has been reconciled to
    /// a retryable paused state.
    SessionRunPaused {
        session_id: String,
    },
    /// An explicit end attempt failed after durably pausing a retryable
    /// session. The UI keeps the session visible and can retry the end.
    SessionEndPaused {
        session_id: String,
    },
    ScheduledConfirmOutcome {
        tool_run_id: String,
        session_id: Option<String>,
        title: String,
        body: String,
    },
    SessionCleanup {
        session_id: String,
    },
    CascadeCompleted {
        session_id: String,
        title: String,
    },
    SessionError {
        session_id: String,
        reason: String,
    },
}

/// Result of a safety-gated tool execution: the tool result plus the
/// risk level and confirmation state recorded for the step.
pub struct ToolExecution {
    pub result: ToolResult,
    pub risk_level: RiskLevel,
    pub confirmed: Option<bool>,
}

pub struct SessionSupervisor {
    /// The single durable session event boundary shared by all actors and the
    /// ReAct turn runner. Keeping one store instance also makes the live event
    /// broadcast observe interaction/control events, not just transcript rows.
    store: SessionStore,
    execution: Arc<dyn tool_ports::ToolExecutionPort>,
    tool_authorization: Arc<dyn tool_ports::ToolAuthorizationPort>,
    #[cfg(test)]
    tool_catalog: Arc<dyn crate::react::ToolCatalogPort>,
    /// Live authorization capability shared with the ToolsFacade execution
    /// boundary. Session lifecycle code accesses this narrow capability
    /// directly instead of exposing a process-service bundle.
    authorization: Arc<AuthorizationEngine>,
    /// ToolCall lifecycle capability used for session-owned cancellation and
    /// terminal ToolRun reconciliation.
    tool_runs: Arc<ToolRunService>,
    /// Agent-owned boundary for restoring and clearing per-session tool
    /// registrations. Live loading remains owned by the tool execution path.
    session_tool_overlay_port: Arc<dyn SessionToolOverlayPort>,
    /// Agent-owned lifecycle boundary for session-scoped managed asset leases.
    managed_asset_lease_port: Arc<dyn ManagedAssetLeasePort>,
    /// Agent-owned read boundary for formatting completed tool observations.
    observation_port: Arc<dyn ToolObservationPort>,
    /// The sole cross-session registry. A session's mutable runtime state is
    /// owned by its actor and is never protected by a shared per-session lock.
    actors: Arc<Mutex<HashMap<String, actor::SessionActorHandle>>>,
    /// Admission gate for session runs. Unlike a dynamically resized Tokio
    /// semaphore, the gate tracks active runs explicitly, so lowering and
    /// raising the limit while work is in flight cannot leak permits.
    admission: Arc<dispatcher::SessionRunAdmission>,
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
    closing_sessions: Arc<StdMutex<HashMap<String, SessionClosingMode>>>,
    /// Run-exit and status transitions share one terminal cleanup owner. The
    /// marker also closes resume/rollback admission while that owner performs
    /// cleanup outside the global lifecycle gate.
    terminal_cleanup_sessions: Arc<StdMutex<HashSet<String>>>,
    /// An accepted End's cascade decision must survive after the close marker
    /// drops while a run is still unwinding, until run-exit cleanup consumes it.
    terminal_cleanup_cascade_overrides: Arc<StdMutex<HashMap<String, bool>>>,
    terminal_cleanup_retry_queue: Arc<TerminalCleanupRetryQueue>,
    terminal_cleanup_retry_rx: StdMutex<Option<mpsc::UnboundedReceiver<String>>>,
    /// Direct SessionRun admissions waiting for a run slot can be cancelled by
    /// delete/clear instead of waiting for unrelated work to release capacity.
    direct_session_run_admission_waiters: Arc<Mutex<DirectSessionRunAdmissionWaiters>>,
    direct_session_run_admission_waiter_id: AtomicUsize,
    /// A direct-run owner remains reserved until its exit reconciliation has
    /// completed. Actor `running` may clear slightly earlier, so this marker
    /// prevents same-session re-admission during that cleanup handoff.
    direct_session_run_leases: StdMutex<HashMap<String, usize>>,
    direct_session_run_lease_id: AtomicUsize,
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
    /// Deterministic failure seam for the pending-recovery retry regression.
    #[cfg(test)]
    pending_session_recovery_failures: AtomicUsize,
    #[cfg(test)]
    pending_session_recovery_attempts: AtomicUsize,
    #[cfg(test)]
    pending_session_recovery_backoff_started: Notify,
    /// Scheduled confirmations are not session state (some are headless), so
    /// they live in an owner-local registry keyed by ToolRun ID. Resolve and
    /// expiry must also match the confirmation request ID stored in the entry.
    scheduled_confirms: Arc<Mutex<HashMap<String, InteractionRequest>>>,
    /// Serializes one-shot, permanent, and session-scoped resolution paths so
    /// a stale concurrent click cannot commit trust after another decision
    /// already removed and woke the pending confirmation.
    confirmation_resolution_gate: Arc<Mutex<()>>,
    /// Coordinated lifecycle for checkpointed stream text (checkpoint /
    /// promote / discard), shared with the agent loop and the end/rollback
    /// paths.
    pub partials: Arc<crate::partial::PartialStore>,
    event_tx: broadcast::Sender<SessionSupervisorEvent>,
    message_tx: watch::Sender<u64>,
    /// Notification body truncation for scheduled-tool outcomes (matches
    /// `ContextLimitsConfig::notification_summary_chars`).
    pub notification_summary_chars: AtomicUsize,
}

mod actor;
mod dispatcher;
mod queues;
mod run_engine;
mod status;
mod tool_ports;
mod tool_runner;
pub(crate) use dispatcher::DirectSessionRunLease;
pub use tool_ports::SessionToolPorts;
pub use tool_ports::{
    ManagedAssetLeasePort, SessionToolOverlayPort, ToolAuthorizationPort, ToolExecutionContext,
    ToolExecutionPort, ToolObservationPort,
};
pub(crate) use tool_runner::{ToolStepMetadata, ToolStepPersistenceError};

pub(crate) use actor::{
    CONTEXT_BATCH_MAX_CHARS, CONTEXT_BATCH_MAX_ITEMS, MessagingTitle, ReActContextBatch,
};
pub(crate) use queues::PendingInteractionGates;
pub use run_engine::SessionRunEngine;

impl SessionSupervisor {
    pub fn new(store: SessionStore, ports: SessionToolPorts, max_concurrent: usize) -> Self {
        let SessionToolPorts {
            execution,
            tool_authorization,
            catalog: _catalog,
            authorization,
            tool_runs,
            session_tool_overlay,
            managed_asset_leases,
            observations,
        } = ports;
        let (event_tx, _) = broadcast::channel(256);
        let (retry_tx, retry_rx) = mpsc::unbounded_channel();
        Self {
            partials: Arc::new(crate::partial::PartialStore::new(store.clone())),
            store,
            execution,
            tool_authorization,
            #[cfg(test)]
            tool_catalog: _catalog,
            authorization,
            tool_runs,
            session_tool_overlay_port: session_tool_overlay,
            observation_port: observations,
            managed_asset_lease_port: managed_asset_leases,
            actors: Arc::new(Mutex::new(HashMap::new())),
            admission: Arc::new(dispatcher::SessionRunAdmission::new(max_concurrent.max(1))),
            lifecycle_gate: Arc::new(Mutex::new(())),
            lifecycle_blocked: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            closing_sessions: Arc::new(StdMutex::new(HashMap::new())),
            terminal_cleanup_sessions: Arc::new(StdMutex::new(HashSet::new())),
            terminal_cleanup_cascade_overrides: Arc::new(StdMutex::new(HashMap::new())),
            terminal_cleanup_retry_queue: Arc::new(TerminalCleanupRetryQueue {
                queued: StdMutex::new(HashMap::new()),
                sender: retry_tx,
            }),
            terminal_cleanup_retry_rx: StdMutex::new(Some(retry_rx)),
            direct_session_run_admission_waiters: Arc::new(Mutex::new(HashMap::new())),
            direct_session_run_admission_waiter_id: AtomicUsize::new(0),
            direct_session_run_leases: StdMutex::new(HashMap::new()),
            direct_session_run_lease_id: AtomicUsize::new(0),
            dispatcher_started: std::sync::atomic::AtomicBool::new(false),
            pending_queue: Arc::new(Mutex::new(VecDeque::new())),
            dispatch_tx: watch::channel(0).0,
            #[cfg(test)]
            pending_session_recovery_failures: AtomicUsize::new(0),
            #[cfg(test)]
            pending_session_recovery_attempts: AtomicUsize::new(0),
            #[cfg(test)]
            pending_session_recovery_backoff_started: Notify::new(),
            scheduled_confirms: Arc::new(Mutex::new(HashMap::new())),
            confirmation_resolution_gate: Arc::new(Mutex::new(())),
            event_tx,
            message_tx: watch::channel(0).0,
            notification_summary_chars: AtomicUsize::new(800),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_session_tool_overlay_port(
        store: SessionStore,
        tools: Arc<ToolsFacade>,
        max_concurrent: usize,
        session_tool_overlay_port: Arc<dyn SessionToolOverlayPort>,
    ) -> Self {
        let ports = tool_ports::SessionToolPorts::from_tools_facade(tools)
            .with_session_tool_overlay(session_tool_overlay_port);
        Self::new(store, ports, max_concurrent)
    }

    /// Build an executor for unit tests while keeping the production
    /// constructor on the typed session persistence boundary.
    #[cfg(test)]
    pub(crate) fn new_for_test(
        db: Arc<Database>,
        tools: Arc<ToolsFacade>,
        max_concurrent: usize,
    ) -> Self {
        Self::new(
            SessionStore::new(db),
            tool_ports::SessionToolPorts::from_tools_facade(tools),
            max_concurrent,
        )
    }

    pub(crate) fn register_managed_assets_for_session(
        &self,
        session_id: &str,
        attachments: &[MessageAttachment],
    ) {
        self.managed_asset_lease_port
            .register_for_session(session_id, attachments);
    }

    pub(crate) fn release_managed_assets_for_session(&self, session_id: &str) {
        self.managed_asset_lease_port
            .release_for_session(session_id);
    }

    pub(crate) async fn unregister_session_tool_overlay(&self, session_id: &str) {
        self.session_tool_overlay_port
            .unregister_session(session_id)
            .await;
    }

    /// Persist a session grant before adding it to the live authorization
    /// engine. If SQLite rejects the write, the decision is not applied in
    /// memory and cannot silently disappear on restart.
    pub async fn grant_session_permission(
        &self,
        session_id: &str,
        capability: CapabilityScope,
        target: PermissionTarget,
        effect: PermissionEffect,
    ) -> anyhow::Result<()> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        if self.is_session_closing(session_id) {
            anyhow::bail!("session '{}' is closing; retry after deletion", session_id);
        }
        let grant = SessionAuthorizationGrant::session(capability, target, effect);
        self.store
            .save_session_authorization_grant(session_id, grant.clone())
            .await?;
        self.authorization
            .grant(
                Some(session_id),
                grant.capability,
                grant.effect,
                grant.scope,
            )
            .await;
        Ok(())
    }

    /// Reconcile the process-local session map with all durable grants. This
    /// replaces stale grants after a live security-policy update or retention
    /// cleanup while preserving grants for sessions that still exist.
    pub async fn restore_session_authorization_grants(&self) -> anyhow::Result<usize> {
        let _lifecycle = self.lifecycle_guard().await;
        self.ensure_lifecycle_open()?;
        // Clear first so a failed read cannot leave stale or partially
        // restored trust active. Callers surface the error and remain
        // fail-closed until reconciliation succeeds.
        self.authorization.clear_all_trust().await;
        let grants = self.store.all_session_authorization_grants().await?;
        let count = grants.len();
        for stored in grants {
            self.authorization
                .grant(
                    Some(&stored.session_id),
                    stored.grant.capability,
                    stored.grant.effect,
                    stored.grant.scope,
                )
                .await;
        }
        Ok(count)
    }

    /// Reload one session's durable grants when a terminal session is reopened
    /// in the same process and still has an idle actor. A failed read clears
    /// that session's live trust first, preserving fail-closed behavior.
    pub async fn restore_session_authorization_grants_for(
        &self,
        session_id: &str,
    ) -> anyhow::Result<usize> {
        let _lifecycle = self.lifecycle_guard().await;
        self.restore_session_authorization_grants_for_locked(session_id)
            .await
    }

    async fn restore_session_authorization_grants_for_locked(
        &self,
        session_id: &str,
    ) -> anyhow::Result<usize> {
        self.ensure_lifecycle_open()?;
        if self.is_session_closing(session_id) {
            anyhow::bail!("session '{}' is closing; retry after deletion", session_id);
        }
        self.authorization.clear_session_trust(session_id).await;
        let grants = self.store.session_authorization_grants(session_id).await?;
        let count = grants.len();
        for grant in grants {
            self.authorization
                .grant(
                    Some(session_id),
                    grant.capability,
                    grant.effect,
                    grant.scope,
                )
                .await;
        }
        Ok(count)
    }

    pub(crate) async fn register_mcp_tool_overlay(
        &self,
        session_id: &str,
        server_name: &str,
        tool_names: Option<&[String]>,
    ) -> bool {
        self.session_tool_overlay_port
            .register_mcp_for_session(session_id, server_name, tool_names)
            .await
    }

    pub(crate) async fn load_skill_tool_overlay(
        &self,
        session_id: &str,
        names: Vec<String>,
    ) -> bool {
        self.session_tool_overlay_port
            .load_skill_for_session(session_id, names)
            .await
    }

    pub(crate) async fn load_builtin_operations_tool_overlay(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> bool {
        self.session_tool_overlay_port
            .load_builtin_operations_for_session(session_id, operations, roots)
            .await
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionSupervisorEvent> {
        self.event_tx.subscribe()
    }

    pub(crate) async fn actor_for(&self, session_id: &str) -> Option<actor::SessionActorHandle> {
        self.actors.lock().await.get(session_id).cloned()
    }

    /// Non-blocking actor lookup for cancellation-safe guard cleanup.  It is
    /// intentionally only a fast path; lifecycle operations continue to use
    /// the awaited registry lock.
    pub(crate) fn actor_for_now(&self, session_id: &str) -> Option<actor::SessionActorHandle> {
        self.actors.try_lock().ok()?.get(session_id).cloned()
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
            .contains_key(session_id)
            || self
                .terminal_cleanup_sessions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains(session_id)
    }

    /// Start terminal cleanup while the caller holds `lifecycle_guard()`.
    /// Returns `None` when another end/run-exit path already owns it.
    pub(crate) fn begin_terminal_cleanup_locked(
        &self,
        session_id: &str,
    ) -> Option<TerminalCleanupGuard> {
        let closing_mode = self
            .closing_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
            .copied();
        if matches!(
            closing_mode,
            Some(SessionClosingMode::EndPreparing { .. } | SessionClosingMode::Destructive)
        ) {
            return None;
        }
        let inserted = self
            .terminal_cleanup_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(session_id.to_string());
        inserted.then(|| TerminalCleanupGuard {
            sessions: self.terminal_cleanup_sessions.clone(),
            closing_sessions: self.closing_sessions.clone(),
            retry_queue: self.terminal_cleanup_retry_queue.clone(),
            session_id: session_id.to_string(),
            completed: false,
            retry: TerminalCleanupRetry {
                cascade: None,
                remove_error_actor: false,
            },
        })
    }

    /// Return whether run-exit cleanup should cascade while holding the
    /// lifecycle gate. A destructive close or an End still preparing its
    /// durable terminal state is not eligible for run-exit handoff.
    pub(crate) fn terminal_cleanup_cascade_locked(&self, session_id: &str) -> Option<bool> {
        match self
            .closing_sessions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(session_id)
        {
            None => Some(
                self.terminal_cleanup_cascade_overrides
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(session_id)
                    .copied()
                    .unwrap_or(true),
            ),
            Some(SessionClosingMode::EndCommitted { cascade }) => Some(*cascade),
            Some(SessionClosingMode::EndPreparing { .. } | SessionClosingMode::Destructive) => None,
        }
    }

    pub(crate) async fn register_direct_session_run_admission_waiter(
        &self,
        session_id: &str,
        cancellation: CancellationToken,
    ) -> usize {
        let waiter_id = self
            .direct_session_run_admission_waiter_id
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.direct_session_run_admission_waiters
            .lock()
            .await
            .entry(session_id.to_string())
            .or_default()
            .push(DirectSessionRunAdmissionWaiter {
                waiter_id,
                cancellation,
            });
        waiter_id
    }

    pub(crate) async fn unregister_direct_session_run_admission_waiter(
        &self,
        session_id: &str,
        waiter_id: usize,
    ) {
        let mut waiters = self.direct_session_run_admission_waiters.lock().await;
        let Some(session_waiters) = waiters.get_mut(session_id) else {
            return;
        };
        session_waiters.retain(|waiter| waiter.waiter_id != waiter_id);
        if session_waiters.is_empty() {
            waiters.remove(session_id);
        }
    }

    pub(crate) async fn cancel_direct_session_run_admission_waiters(&self, session_id: &str) {
        let waiters = self
            .direct_session_run_admission_waiters
            .lock()
            .await
            .remove(session_id);
        if let Some(waiters) = waiters {
            for waiter in waiters {
                waiter.cancellation.cancel();
            }
        }
    }

    async fn install_actor(
        self: &Arc<Self>,
        info: SessionInfo,
    ) -> anyhow::Result<actor::SessionActorHandle> {
        // Interaction state is authoritative in the active durable event
        // stream. Do not apply this session's grants or register an actor
        // until that state has been reconstructed successfully.
        let interactions = actor::load_interactions(&self.store, &info.id).await?;
        let grants = self.store.session_authorization_grants(&info.id).await?;
        for grant in grants {
            self.authorization
                .grant(Some(&info.id), grant.capability, grant.effect, grant.scope)
                .await;
        }
        let handle = actor::spawn(self.store.clone(), info, interactions.clone());
        self.actors
            .lock()
            .await
            .insert(handle.id.clone(), handle.clone());
        for request in &interactions {
            self.schedule_confirmation_expiry(request);
        }
        Ok(handle)
    }

    /// Remove an actor when the caller already owns [`lifecycle_guard`].
    pub(crate) async fn remove_actor_locked(
        &self,
        session_id: &str,
    ) -> Option<actor::SessionActorHandle> {
        self.terminal_cleanup_cascade_overrides
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(session_id);
        self.actors.lock().await.remove(session_id)
    }

    /// Return the in-process mailbox port used by `MessagingService`.
    pub(crate) fn messaging_mailbox(self: &Arc<Self>) -> Arc<dyn haven_messaging::SessionMailbox> {
        self.clone()
    }

    /// A service view for the ReAct inbox. It shares this supervisor's actor
    /// registry while retaining the JSONL fallback for external processes.
    pub(crate) fn messaging_service(self: &Arc<Self>) -> Arc<haven_messaging::MessagingService> {
        Arc::new(haven_messaging::MessagingService::with_session_mailbox(
            self.messaging_mailbox(),
        ))
    }

    pub(crate) fn emit_event(&self, event: SessionSupervisorEvent) {
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

impl haven_messaging::SessionMailbox for SessionSupervisor {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.message_tx.subscribe()
    }

    fn deliver(
        &self,
        to: &str,
        envelope: &haven_messaging::inbox::Envelope,
    ) -> anyhow::Result<Option<haven_messaging::inbox::SendOutcome>> {
        let actor = self.actors.blocking_lock().get(to).cloned();
        let Some(actor) = actor else {
            return Ok(None);
        };
        actor.deliver_message(envelope.clone())?;
        self.message_tx.send_modify(|counter| *counter += 1);
        Ok(Some(haven_messaging::inbox::SendOutcome {
            to: to.to_string(),
            delivered: true,
            status: haven_messaging::inbox::AgentStatus::Online,
        }))
    }

    fn claim(
        &self,
        recipient: &str,
    ) -> anyhow::Result<Option<Vec<haven_messaging::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(recipient).cloned();
        Ok(actor.map(|actor| actor.claim_messages()))
    }

    fn try_claim(
        &self,
        recipient: &str,
    ) -> anyhow::Result<Option<Vec<haven_messaging::inbox::Envelope>>> {
        self.claim(recipient)
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
    ) -> anyhow::Result<Option<Option<haven_messaging::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| actor.last_received_message()))
    }

    fn find_message(
        &self,
        name: &str,
        id: &str,
    ) -> anyhow::Result<Option<Option<haven_messaging::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| actor.find_message_by_id(id.to_string())))
    }

    fn take_matching_replies(
        &self,
        name: &str,
        in_reply_to: &str,
        expected_from: &str,
    ) -> anyhow::Result<Option<Vec<haven_messaging::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| {
            actor.take_matching_replies_blocking(in_reply_to.to_string(), expected_from.to_string())
        }))
    }

    fn history(
        &self,
        name: &str,
        limit: usize,
    ) -> anyhow::Result<Option<Vec<haven_messaging::inbox::Envelope>>> {
        let actor = self.actors.blocking_lock().get(name).cloned();
        Ok(actor.map(|actor| actor.history_blocking(limit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_messaging::inbox::MessageType;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_db_path() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("haven_agent_test_{}.db", uuid::Uuid::new_v4()));
        p
    }

    fn make_executor_with_db(max_concurrent: usize) -> (Arc<SessionSupervisor>, Arc<Database>) {
        let path = temp_db_path();
        let db = Arc::new(Database::open(&path).unwrap());
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools,
            max_concurrent,
        ));
        // Best-effort cleanup; failures are ignored since the OS will purge
        // temp files eventually.
        let _ = path;
        (exec, db)
    }

    fn make_executor(max_concurrent: usize) -> Arc<SessionSupervisor> {
        make_executor_with_db(max_concurrent).0
    }

    #[tokio::test]
    async fn constructor_uses_the_injected_session_store() {
        let db = Arc::new(Database::open(&temp_db_path()).unwrap());
        let store = SessionStore::new(db);
        let ports = tool_ports::SessionToolPorts::from_tools_facade(Arc::new(ToolsFacade::new()));
        let exec = Arc::new(SessionSupervisor::new(store.clone(), ports, 1));

        let session = exec.create_session("typed constructor").await.unwrap();
        let persisted = store
            .load_session_record(&session.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(persisted.id, session.id);
        assert_eq!(persisted.input_text, "typed constructor");
    }

    fn high_risk_session_request(session_id: &str) -> haven_tools::AuthorizationRequest {
        let capability = CapabilityScope::try_new("files.write").unwrap();
        haven_tools::AuthorizationRequest::new(
            Some(session_id),
            "files.write",
            serde_json::json!({}),
            haven_tools::OperationPolicy::native(
                "files.write",
                capability,
                RiskLevel::High,
                haven_tools::NetworkAccess::None,
            ),
        )
    }

    #[tokio::test]
    async fn session_grant_persists_and_restores_when_session_actor_is_reloaded() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let record = db.create_session("durable session permission").unwrap();
        let first = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        first.ensure_session_loaded(&record.id).await.unwrap();
        first
            .grant_session_permission(
                &record.id,
                CapabilityScope::try_new("files.write").unwrap(),
                PermissionTarget::Operation,
                PermissionEffect::Allow,
            )
            .await
            .unwrap();
        assert!(matches!(
            first
                .authorization
                .authorize(&high_risk_session_request(&record.id))
                .await,
            AuthorizationDecision::AutoApproved
        ));
        assert!(matches!(
            first
                .authorization
                .authorize(&high_risk_session_request(
                    "ses-11111111111111111111111111111111"
                ))
                .await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        // Security settings apply invalidates in-memory trust first, then the
        // supervisor restores the durable per-session set without widening it
        // to a global grant.
        let mut changed_security = haven_common::config::SecurityConfig::default();
        changed_security.network_policy = haven_common::types::NetworkPolicy::Open;
        first.authorization.apply_security(&changed_security).await;
        first.restore_session_authorization_grants().await.unwrap();
        assert!(matches!(
            first
                .authorization
                .authorize(&high_risk_session_request(&record.id))
                .await,
            AuthorizationDecision::AutoApproved
        ));

        // Simulate process shutdown: runtime actors and in-memory grants go
        // away while the persisted session and its grant remain available.
        first
            .clear_session_runtime_state_for_shutdown()
            .await
            .unwrap();
        let restarted = Arc::new(SessionSupervisor::new_for_test(
            db,
            Arc::new(ToolsFacade::new()),
            1,
        ));
        restarted.ensure_session_loaded(&record.id).await.unwrap();
        assert!(matches!(
            restarted
                .authorization
                .authorize(&high_risk_session_request(&record.id))
                .await,
            AuthorizationDecision::AutoApproved
        ));
    }

    #[tokio::test]
    async fn failed_session_grant_persistence_does_not_change_live_authorization() {
        let exec = make_executor(1);
        let missing_session = "ses-00000000000000000000000000000000";
        let error = exec
            .grant_session_permission(
                missing_session,
                CapabilityScope::try_new("files.write").unwrap(),
                PermissionTarget::Operation,
                PermissionEffect::Allow,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("FOREIGN KEY"));
        assert!(matches!(
            exec.authorization
                .authorize(&high_risk_session_request(missing_session))
                .await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn terminal_error_actor_reloads_durable_grants_before_continue() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let record = db
            .create_session("continue durable session permission")
            .unwrap();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db,
            Arc::new(ToolsFacade::new()),
            1,
        ));
        exec.ensure_session_loaded(&record.id).await.unwrap();
        exec.grant_session_permission(
            &record.id,
            CapabilityScope::try_new("files.write").unwrap(),
            PermissionTarget::Operation,
            PermissionEffect::Allow,
        )
        .await
        .unwrap();

        // Error keeps an idle actor for Continue, while finish_ended_session
        // clears its process-local grant map. ensure_session_loaded must reload
        // the durable decision even though it does not need to respawn actor.
        exec.update_session_status(&record.id, SessionStatus::Error)
            .await
            .unwrap();
        assert!(matches!(
            exec.authorization
                .authorize(&high_risk_session_request(&record.id))
                .await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));
        exec.ensure_session_loaded(&record.id).await.unwrap();
        assert!(matches!(
            exec.authorization
                .authorize(&high_risk_session_request(&record.id))
                .await,
            AuthorizationDecision::AutoApproved
        ));
    }

    #[tokio::test]
    async fn run_exit_rechecks_status_after_continue_wins_the_idle_window() {
        let exec = make_executor(1);
        let session = exec.create_session("continue wins run exit").await.unwrap();
        let actor = exec.actor_for(&session.id).await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();
        assert!(actor.try_mark_direct_run_active().await);

        // ReAct marks the failed run terminal before the dispatcher clears
        // the actor's running bit. A Continue can commit after that bit clears
        // but before the run-exit continuation is scheduled.
        exec.update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();
        assert!(actor.finish_run().await.is_some());
        {
            let _lifecycle = exec.lifecycle_guard().await;
            assert!(!exec.is_session_closing(&session.id));
            let transition = actor
                .transition_if(SessionStatus::Error, SessionStatus::Pending, true)
                .await
                .unwrap();
            assert!(transition.changed);
            exec.enqueue_pending(&session.id).await;
        }

        exec.reconcile_run_exit(&session.id, &actor).await;

        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let current = exec.actor_for(&session.id).await.unwrap();
        assert!(actor.same_instance(&current));
        assert!(
            exec.pending_queue
                .lock()
                .await
                .iter()
                .any(|id| id == &session.id)
        );
    }

    #[tokio::test]
    async fn terminal_cleanup_lease_blocks_reopen_and_new_closing_owner() {
        let exec = make_executor(1);
        let session = exec.create_session("cleanup lease").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Error)
            .await
            .unwrap();
        let cleanup = {
            let _lifecycle = exec.lifecycle_guard().await;
            exec.begin_terminal_cleanup_locked(&session.id)
                .expect("idle terminal cleanup should be claimable")
        };

        let reopen_error = exec
            .ensure_session_loaded(&session.id)
            .await
            .expect_err("reopen must wait for the cleanup owner");
        assert!(reopen_error.to_string().contains("closing"));
        assert!(
            exec.begin_session_closing(&session.id, SessionClosingMode::Destructive)
                .await
                .is_err()
        );
        drop(cleanup);
        exec.ensure_session_loaded(&session.id).await.unwrap();
    }

    #[tokio::test]
    async fn run_exit_only_takes_over_after_end_commit_and_not_during_delete() {
        let exec = make_executor(1);
        let ending = exec.create_session("end cleanup handoff").await.unwrap();
        let ending_actor = exec.actor_for(&ending.id).await.unwrap();
        ending_actor
            .transition(SessionStatus::Error, true)
            .await
            .unwrap();
        let ending_marker = exec
            .begin_session_closing(
                &ending.id,
                SessionClosingMode::EndPreparing { cascade: false },
            )
            .await
            .unwrap();

        // A run-exit racing the initial End snapshot/Paused write must leave
        // the Actor and durable state to the End owner.
        exec.reconcile_run_exit(&ending.id, &ending_actor).await;
        assert!(exec.actor_for(&ending.id).await.is_some());
        assert!(
            !exec
                .terminal_cleanup_sessions
                .lock()
                .unwrap()
                .contains(&ending.id)
        );

        {
            let _lifecycle = exec.lifecycle_guard().await;
            ending_actor
                .transition(SessionStatus::Paused, true)
                .await
                .unwrap();
            ending_actor
                .transition(SessionStatus::Completed, true)
                .await
                .unwrap();
            assert!(ending_marker.mark_end_committed());
        }
        exec.reconcile_run_exit(&ending.id, &ending_actor).await;
        assert!(exec.actor_for(&ending.id).await.is_none());

        let deleting = exec
            .create_session("delete cleanup ownership")
            .await
            .unwrap();
        let deleting_actor = exec.actor_for(&deleting.id).await.unwrap();
        deleting_actor
            .transition(SessionStatus::Error, true)
            .await
            .unwrap();
        let delete_marker = exec
            .begin_session_closing(&deleting.id, SessionClosingMode::Destructive)
            .await
            .unwrap();
        exec.reconcile_run_exit(&deleting.id, &deleting_actor).await;
        assert!(exec.actor_for(&deleting.id).await.is_some());
        assert!(
            !exec
                .terminal_cleanup_sessions
                .lock()
                .unwrap()
                .contains(&deleting.id)
        );
        drop(delete_marker);
        exec.reconcile_run_exit(&deleting.id, &deleting_actor).await;
        assert!(exec.actor_for(&deleting.id).await.is_none());
    }

    #[tokio::test]
    async fn interrupted_terminal_cleanup_is_retried_by_dispatcher_worker() {
        let exec = make_executor(1);
        let session = exec
            .create_session("terminal cleanup retry worker")
            .await
            .unwrap();
        let actor = exec.actor_for(&session.id).await.unwrap();
        actor.transition(SessionStatus::Error, true).await.unwrap();
        let mut cleanup = {
            let _lifecycle = exec.lifecycle_guard().await;
            exec.begin_terminal_cleanup_locked(&session.id)
                .expect("terminal cleanup should be claimable")
        };
        cleanup.set_retry_policy(TerminalCleanupRetry {
            cascade: Some(true),
            remove_error_actor: true,
        });

        // Dropping an incomplete owner models task cancellation between the
        // lease claim and cleanup completion.
        drop(cleanup);
        let cancellation = CancellationToken::new();
        let handler: SessionRunHandler = Arc::new(|_| Box::pin(async { Ok(()) }));
        exec.clone()
            .start_dispatcher_without_recovery_with_cancellation(handler, cancellation.clone());

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while exec.actor_for(&session.id).await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the dispatcher retry worker should reclaim idle terminal cleanup");
        cancellation.cancel();
    }

    #[tokio::test]
    async fn end_committed_close_drop_retries_terminal_cleanup_with_cascade_policy() {
        let exec = make_executor(1);
        let session = exec
            .create_session("end marker retry worker")
            .await
            .unwrap();
        let actor = exec.actor_for(&session.id).await.unwrap();
        let closing = exec
            .begin_session_closing(
                &session.id,
                SessionClosingMode::EndPreparing { cascade: false },
            )
            .await
            .unwrap();
        {
            let _lifecycle = exec.lifecycle_guard().await;
            actor.transition(SessionStatus::Paused, true).await.unwrap();
            actor
                .transition(SessionStatus::Completed, true)
                .await
                .unwrap();
            assert!(closing.mark_end_committed());
        }
        drop(closing);
        assert_eq!(
            exec.terminal_cleanup_retry_queue
                .queued
                .lock()
                .unwrap()
                .get(&session.id),
            Some(&TerminalCleanupRetry {
                cascade: Some(false),
                remove_error_actor: true,
            }),
            "retry must preserve the End cascade decision"
        );

        let cancellation = CancellationToken::new();
        let handler: SessionRunHandler = Arc::new(|_| Box::pin(async { Ok(()) }));
        exec.clone()
            .start_dispatcher_without_recovery_with_cancellation(handler, cancellation.clone());
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while exec.actor_for(&session.id).await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("an aborted End must retry its committed terminal cleanup");
        cancellation.cancel();
    }

    #[tokio::test]
    async fn memory_only_terminal_status_cannot_remove_uncommitted_session() {
        let exec = make_executor(1);
        let session = exec
            .create_session("memory only terminal guard")
            .await
            .unwrap();
        let actor = exec.actor_for(&session.id).await.unwrap();
        let durable_before = exec
            .store
            .session_record(&session.id)
            .unwrap()
            .unwrap()
            .status;

        let error = exec
            .update_session_status_memory_only(&session.id, SessionStatus::Completed)
            .await
            .expect_err("terminal status must have a durable commit before cleanup");
        assert!(error.to_string().contains("memory-only"));
        assert!(actor.same_instance(&exec.actor_for(&session.id).await.unwrap()));
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
        assert_eq!(
            exec.store
                .session_record(&session.id)
                .unwrap()
                .unwrap()
                .status,
            durable_before
        );
    }

    /// A handler that panics must still release the running slot and mark the
    /// session Error —otherwise the session is stuck in Running forever.
    #[tokio::test]
    async fn dispatcher_panicked_handler_marks_error() {
        let (exec, db) = make_executor_with_db(1);
        let session = exec.create_session("t1").await.unwrap();

        let mut events = exec.subscribe_events();

        let handler: SessionRunHandler = Arc::new(move |_id: String| {
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
            db_status = db
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
                && exec.get_active_session_status(&session.id).await.is_none()
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
            if let Ok(Ok(SessionSupervisorEvent::SessionError { session_id, reason })) =
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
        let handler: SessionRunHandler = Arc::new(move |id: String| {
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
        let handler: SessionRunHandler = Arc::new(move |id: String| {
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
        let (exec, db) = make_executor_with_db(2);
        let session = exec.create_session("t1").await.unwrap();

        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(session.id.as_str()));

        let state = exec.get_active_session_status(&session.id).await;
        assert_eq!(state, Some(SessionStatus::Running));
        assert!(exec.is_run_in_flight(&session.id).await);
        let db_status = db
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
        let actor = exec.actor_for(&session.id).await.unwrap();
        exec.end_direct_session_run(&session.id, &actor).await;
        exec.enqueue_pending(&session.id).await;
        let claimed = exec.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(session.id.as_str()));
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
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
        let admission = Arc::new(dispatcher::SessionRunAdmission::new(4));
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
        let (exec, db) = make_executor_with_db(1);
        let session = exec.create_session("delete atomically").await.unwrap();
        db.add_message(
            &session.id,
            haven_common::types::CanonicalRole::User,
            "delete with session",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
        )
        .unwrap();
        db.set_kv(
            &format!("fact_extraction_pending.{}", session.id),
            "0:1:0:0",
        )
        .unwrap();

        exec.delete_session(&session.id).await.unwrap();

        assert!(exec.actor_for(&session.id).await.is_none());
        assert!(db.get_session(&session.id).unwrap().is_none());
        assert!(db.list_session_messages(&session.id).unwrap().is_empty());
        assert!(
            db.get_kv(&format!("fact_extraction_pending.{}", session.id))
                .unwrap()
                .is_none()
        );
        let error = exec.delete_session(&session.id).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("session '{}' not found in database", session.id)
        );
    }

    #[tokio::test]
    async fn explicit_history_deletion_releases_managed_asset_leases() {
        let path = temp_db_path();
        let db = Arc::new(Database::open(&path).unwrap());
        let tools = Arc::new(ToolsFacade::new());
        let registry = tools.share_services().assets;
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 1));
        let assets_root = tempfile::TempDir::new().unwrap();

        let deleted = exec
            .create_session("delete leased attachment")
            .await
            .unwrap();
        let deleted_path = assets_root.path().join("deleted.pdf");
        std::fs::write(&deleted_path, b"deleted").unwrap();
        assert!(registry.register_under_root_for_session(
            &deleted.id,
            assets_root.path(),
            "asset-deleted",
            deleted_path,
            Some("deleted.pdf".into()),
            "application/pdf",
        ));
        assert_eq!(registry.leased_paths().len(), 1);

        exec.delete_session(&deleted.id).await.unwrap();
        assert!(registry.leased_paths().is_empty());

        let cleared = exec
            .create_session("clear leased attachment")
            .await
            .unwrap();
        let cleared_path = assets_root.path().join("cleared.pdf");
        std::fs::write(&cleared_path, b"cleared").unwrap();
        assert!(registry.register_under_root_for_session(
            &cleared.id,
            assets_root.path(),
            "asset-cleared",
            cleared_path,
            Some("cleared.pdf".into()),
            "application/pdf",
        ));
        assert_eq!(registry.leased_paths().len(), 1);

        exec.delete_all_sessions().await.unwrap();
        assert!(registry.leased_paths().is_empty());
    }

    #[tokio::test]
    async fn retention_deletion_releases_runtime_grants_and_asset_leases() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let tools = Arc::new(ToolsFacade::new());
        let registry = tools.share_services().assets;
        let exec = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 1));
        let session = exec
            .create_session("expire session-owned state")
            .await
            .unwrap();
        exec.grant_session_permission(
            &session.id,
            CapabilityScope::try_new("files.write").unwrap(),
            PermissionTarget::Operation,
            PermissionEffect::Allow,
        )
        .await
        .unwrap();

        let assets_root = tempfile::TempDir::new().unwrap();
        let asset_path = assets_root.path().join("expired.pdf");
        std::fs::write(&asset_path, b"expired").unwrap();
        assert!(registry.register_under_root_for_session(
            &session.id,
            assets_root.path(),
            "asset-expired",
            asset_path,
            Some("expired.pdf".into()),
            "application/pdf",
        ));

        let conn = db.conn();
        conn.execute(
            "UPDATE sessions SET created_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
            [&session.id],
        )
        .unwrap();
        drop(conn);

        assert_eq!(exec.delete_old_sessions(1).await.unwrap(), 1);
        assert!(db.get_session(&session.id).unwrap().is_none());
        assert!(
            db.session_authorization_grants(&session.id)
                .unwrap()
                .is_empty()
        );
        assert!(exec.actor_for(&session.id).await.is_none());
        assert!(registry.leased_paths().is_empty());
    }

    #[tokio::test]
    async fn delete_all_sessions_clears_runtime_state_and_returns_deleted_count() {
        let (exec, db) = make_executor_with_db(1);
        let first = exec.create_session("first to clear").await.unwrap();
        let second = exec.create_session("second to clear").await.unwrap();
        db.add_message(
            &first.id,
            haven_common::types::CanonicalRole::User,
            "first message",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
        )
        .unwrap();
        db.add_message(
            &second.id,
            haven_common::types::CanonicalRole::User,
            "second message",
            Some(haven_common::types::TranscriptMessageKind::Text),
            None,
        )
        .unwrap();
        let kv_key = format!("fact_extraction_pending.{}", first.id);
        db.set_kv(&kv_key, "1").unwrap();

        let deleted = exec.delete_all_sessions().await.unwrap();
        assert_eq!(deleted.len(), 2);
        assert!(deleted.contains(&first.id));
        assert!(deleted.contains(&second.id));

        assert!(exec.actors.lock().await.is_empty());
        assert!(exec.pending_queue.lock().await.is_empty());
        assert!(
            exec.direct_session_run_admission_waiters
                .lock()
                .await
                .is_empty()
        );
        assert_eq!(db.count_sessions().unwrap(), 0);
        assert!(db.get_session(&first.id).unwrap().is_none());
        assert!(db.get_session(&second.id).unwrap().is_none());
        assert!(db.list_session_messages(&first.id).unwrap().is_empty());
        assert!(db.list_session_messages(&second.id).unwrap().is_empty());
        assert!(db.get_kv(&kv_key).unwrap().is_none());
        assert!(exec.delete_all_sessions().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_all_sessions_cancels_tool_runs_for_unloaded_sessions() {
        let db = temp_db();
        let session = db.create_session("unloaded session to clear").unwrap();
        let tool_run_id = "toolrun-unloaded-clear";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "Unloaded clear",
            "must be cancelled",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;

        assert_eq!(
            exec.delete_all_sessions().await.unwrap(),
            vec![session.id.clone()]
        );

        assert!(db.get_session(&session.id).unwrap().is_none());
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );
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
            tokio::spawn(
                async move { exec_for_resume.begin_direct_session_run(&session_id).await },
            );

        for _ in 0..100 {
            if exec
                .direct_session_run_admission_waiters
                .lock()
                .await
                .contains_key(&session.id)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(
            exec.direct_session_run_admission_waiters
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
    async fn direct_session_run_admission_promotes_paused_session_before_execution() {
        let exec = make_executor(1);
        let session = exec.create_session("direct run from paused").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(
            !exec.is_session_closing(&session.id),
            "a paused idle session should not retain a closing or cleanup lease"
        );
        assert!(!exec.is_run_in_flight(&session.id).await);

        let mut lease = exec
            .begin_direct_session_run(&session.id)
            .await
            .expect("paused direct run should acquire an explicit run lease");
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Running),
            "the actor must persist Running before a direct run can emit errors"
        );
        lease.finish().await;
    }

    #[tokio::test]
    async fn cancelled_direct_session_run_admission_unregisters_waiter() {
        let exec = make_executor(1);
        let occupying = exec.create_session("occupying direct run").await.unwrap();
        let waiting = exec
            .create_session("cancelled direct waiter")
            .await
            .unwrap();
        let mut occupying_lease = exec
            .begin_direct_session_run(&occupying.id)
            .await
            .expect("first direct run should acquire the only permit");

        let waiting_exec = exec.clone();
        let waiting_id = waiting.id.clone();
        let admission =
            tokio::spawn(async move { waiting_exec.begin_direct_session_run(&waiting_id).await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if exec
                    .direct_session_run_admission_waiters
                    .lock()
                    .await
                    .contains_key(&waiting.id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("waiting admission should register its cancellation handle");

        admission.abort();
        let error = match admission.await {
            Ok(_) => panic!("cancelled admission unexpectedly completed"),
            Err(error) => error,
        };
        assert!(error.is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if !exec
                    .direct_session_run_admission_waiters
                    .lock()
                    .await
                    .contains_key(&waiting.id)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled admission should unregister its waiter");

        exec.update_session_status(&occupying.id, SessionStatus::Paused)
            .await
            .unwrap();
        occupying_lease.finish().await;
        drop(occupying_lease);
    }

    #[tokio::test]
    async fn stale_direct_session_run_cleanup_does_not_finish_reloaded_actor() {
        let exec = make_executor(1);
        let session = exec.create_session("stale direct run owner").await.unwrap();
        let old_lease = exec
            .begin_direct_session_run(&session.id)
            .await
            .expect("first actor should admit a direct run");
        let old_actor = old_lease.actor.clone();

        // Remove and reload the session while the old caller still retains its
        // lease handle. The old run exits before its delayed guard cleanup, as
        // it would when deletion quiesces an in-flight direct caller.
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        old_actor.release_run_now();
        exec.remove_session(&session.id).await.unwrap();
        assert!(exec.actor_for(&session.id).await.is_none());
        drop(old_lease);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let active = exec
                    .direct_session_run_leases
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .contains_key(&session.id);
                if !active {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("stale actor cleanup should release its same-session reservation");

        exec.ensure_session_loaded(&session.id).await.unwrap();
        let mut new_lease = exec
            .begin_direct_session_run(&session.id)
            .await
            .expect("reloaded actor should admit a new direct run");
        let new_actor = exec.actor_for(&session.id).await.unwrap();
        assert!(!new_actor.same_instance(&old_actor));
        assert!(new_actor.is_running().await);

        exec.end_direct_session_run(&session.id, &old_actor).await;

        assert!(
            new_actor.is_running().await,
            "stale cleanup must not clear the reloaded actor's run bit"
        );
        new_lease.finish().await;
    }

    #[tokio::test]
    async fn closing_marker_is_released_when_delete_task_is_cancelled() {
        let exec = make_executor(1);
        let session = exec.create_session("cancelled delete").await.unwrap();
        let exec_for_delete = exec.clone();
        let session_id = session.id.clone();
        let delete = tokio::spawn(async move {
            let _closing = exec_for_delete
                .begin_session_closing(&session_id, SessionClosingMode::Destructive)
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
        let first = exec
            .begin_session_closing(
                &session.id,
                SessionClosingMode::EndPreparing { cascade: false },
            )
            .await
            .unwrap();
        let second = exec
            .begin_session_closing(&session.id, SessionClosingMode::Destructive)
            .await;
        assert!(second.is_err());
        {
            let _lifecycle = exec.lifecycle_guard().await;
            assert_eq!(
                exec.terminal_cleanup_cascade_locked(&session.id),
                None,
                "a duplicate close attempt must not replace the current End owner"
            );
            assert!(
                exec.closing_sessions
                    .lock()
                    .unwrap()
                    .get(&session.id)
                    .is_some_and(|mode| matches!(
                        mode,
                        SessionClosingMode::EndPreparing { cascade: false }
                    ))
            );
        }
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
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 2));
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

        let mut expired = haven_messaging::inbox::Envelope::new(&sender.id, &receiver.id, "过期");
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
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools.clone(),
            3,
        ));
        assert_eq!(exec.running_count().await, 0);
        assert!(exec.list_runtime_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn create_session_returns_pending_session() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("hello world").await.unwrap();
        assert_eq!(session.status, SessionStatus::Pending);
        assert_eq!(session.input, "hello world");
        assert!(!session.id.is_empty());
        assert!(!session.created_at.is_empty());
    }

    #[tokio::test]
    async fn create_session_with_summary_preserves_fields() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
        assert_eq!(exec.get_active_session_status(&session.id).await, None);
    }

    #[tokio::test]
    async fn repeated_end_reclaims_cleanup_for_an_idle_completed_actor() {
        let exec = make_executor(1);
        let session = exec.create_session("retry terminal cleanup").await.unwrap();
        let actor = exec.actor_for(&session.id).await.unwrap();

        // Simulate cancellation after Completed committed but before the
        // original cleanup owner removed the actor.
        actor
            .transition(SessionStatus::Completed, true)
            .await
            .unwrap();
        assert!(exec.actor_for(&session.id).await.is_some());

        assert_eq!(
            exec.end_session(&session.id).await.unwrap(),
            SessionStatus::Completed
        );
        assert!(exec.actor_for(&session.id).await.is_none());
    }

    #[tokio::test]
    async fn end_session_returns_before_a_stuck_run_exits() {
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
        let handler: SessionRunHandler = Arc::new(move |session_id: String| {
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
        let result = tokio::time::timeout(std::time::Duration::from_millis(500), end)
            .await
            .expect("end_session must return while the active run is unwinding")
            .expect("end task should join")
            .expect("end_session should succeed");
        assert_eq!(result, SessionStatus::Completed);
        assert_eq!(exited.load(Ordering::SeqCst), 0);
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Completed)
        );
        assert!(exec.is_run_in_flight(&session.id).await);

        allow_exit.store(1, Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while exec.actor_for(&session.id).await.is_some() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("terminal cleanup should finish after the run exits");
        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert_eq!(exec.get_active_session_status(&session.id).await, None);
    }

    #[tokio::test]
    async fn failed_end_pauses_stuck_run_and_retry_wins_over_late_run_error() {
        let (exec, db) = make_executor_with_db(1);
        let session = exec
            .create_session("failed end while run is stuck")
            .await
            .unwrap();
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;
        let tool_run_id = "toolrun-stuck-end-retry";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "Stuck end retry",
            "must be cancelled on retry",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER block_stuck_end_cancel
                 BEFORE UPDATE OF status ON tool_runs
                 WHEN NEW.id = '{tool_run_id}' AND NEW.status = 'cancelled'
                 BEGIN SELECT RAISE(ABORT, 'injected stuck cancellation failure'); END;"
            ))
            .unwrap();

        let started = Arc::new(AtomicU32::new(0));
        let cancellation_seen = Arc::new(AtomicU32::new(0));
        let allow_exit = Arc::new(AtomicU32::new(0));
        let exited = Arc::new(AtomicU32::new(0));
        let started_handler = started.clone();
        let cancellation_seen_handler = cancellation_seen.clone();
        let allow_exit_handler = allow_exit.clone();
        let exited_handler = exited.clone();
        let exec_handler = exec.clone();
        let handler: SessionRunHandler = Arc::new(move |session_id: String| {
            let started = started_handler.clone();
            let cancellation_seen = cancellation_seen_handler.clone();
            let allow_exit = allow_exit_handler.clone();
            let exited = exited_handler.clone();
            let exec = exec_handler.clone();
            Box::pin(async move {
                started.store(1, Ordering::SeqCst);
                exec.cancellation_token(&session_id).await.cancelled().await;
                cancellation_seen.store(1, Ordering::SeqCst);
                while allow_exit.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                exited.store(1, Ordering::SeqCst);
                anyhow::bail!("provider failed after end cancellation")
            })
        });
        let mut events = exec.subscribe_events();
        exec.clone().start_dispatcher(handler);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while started.load(Ordering::SeqCst) == 0 || !exec.is_run_in_flight(&session.id).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the run should start");

        let end = {
            let exec = exec.clone();
            let session_id = session.id.clone();
            tokio::spawn(async move { exec.end_session(&session_id).await })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while cancellation_seen.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("end should cancel the active run");
        let error = tokio::time::timeout(std::time::Duration::from_millis(500), end)
            .await
            .expect("failed end must not wait for a stuck provider")
            .expect("end task should join")
            .expect_err("durable cleanup failure must be reported");
        assert!(format!("{error:#}").contains("injected stuck cancellation failure"));
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(exec.is_run_in_flight(&session.id).await);
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Waiting
        );

        allow_exit.store(1, Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while exec.is_run_in_flight(&session.id).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("late run error should release its slot");
        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Paused),
            "a cancelled run error must not overwrite the accepted pause"
        );
        assert!(
            !std::iter::from_fn(|| events.try_recv().ok())
                .any(|event| matches!(event, SessionSupervisorEvent::SessionError { .. })),
            "a late cancelled-run error must not publish a second terminal event"
        );

        db.conn()
            .execute_batch("DROP TRIGGER block_stuck_end_cancel")
            .unwrap();
        assert_eq!(
            exec.end_session(&session.id).await.unwrap(),
            SessionStatus::Completed
        );
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Completed
        );
    }

    #[tokio::test]
    async fn delete_session_cancels_active_run_and_waits_for_its_exit() {
        let (exec, db) = make_executor_with_db(1);
        let session = exec.create_session("active delete").await.unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let cancellation_seen = Arc::new(AtomicUsize::new(0));
        let allow_exit = Arc::new(AtomicUsize::new(0));
        let exited = Arc::new(AtomicUsize::new(0));

        let started_handler = started.clone();
        let cancellation_seen_handler = cancellation_seen.clone();
        let allow_exit_handler = allow_exit.clone();
        let exited_handler = exited.clone();
        let exec_handler = exec.clone();
        let handler: SessionRunHandler = Arc::new(move |session_id: String| {
            let started = started_handler.clone();
            let cancellation_seen = cancellation_seen_handler.clone();
            let allow_exit = allow_exit_handler.clone();
            let exited = exited_handler.clone();
            let exec = exec_handler.clone();
            Box::pin(async move {
                started.store(1, Ordering::SeqCst);
                exec.cancellation_token(&session_id).await.cancelled().await;
                cancellation_seen.store(1, Ordering::SeqCst);
                while allow_exit.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
                exited.store(1, Ordering::SeqCst);
                Ok(())
            })
        });
        exec.clone().start_dispatcher(handler);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while started.load(Ordering::SeqCst) == 0 || !exec.is_run_in_flight(&session.id).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("dispatcher should start the session run");

        let mut delete = {
            let exec = exec.clone();
            let session_id = session.id.clone();
            tokio::spawn(async move { exec.delete_session(&session_id).await })
        };

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while cancellation_seen.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("delete should cancel the active run");

        assert!(exec.is_run_in_flight(&session.id).await);
        assert!(db.get_session(&session.id).unwrap().is_some());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut delete,)
                .await
                .is_err(),
            "delete must wait until the active run has unwound"
        );
        assert_eq!(exited.load(Ordering::SeqCst), 0);

        allow_exit.store(1, Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(2), delete)
            .await
            .expect("delete should finish after the run exits")
            .expect("delete task should join")
            .expect("delete should succeed");

        assert_eq!(exited.load(Ordering::SeqCst), 1);
        assert!(!exec.is_run_in_flight(&session.id).await);
        assert!(exec.actor_for(&session.id).await.is_none());
        assert!(db.get_session(&session.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn interrupt_session_pauses_and_cancels_without_removing() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();

        let real_token = exec.cancellation_token(&session.id).await;

        assert!(exec.interrupt_session(&session.id).await.unwrap());
        assert!(real_token.is_cancelled());
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(exec.get_session(&session.id).await.is_some());
        assert!(!exec.interrupt_session(&session.id).await.unwrap());
    }

    #[tokio::test]
    async fn interrupt_session_returns_before_a_stuck_run_exits() {
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
        let handler: SessionRunHandler = Arc::new(move |session_id: String| {
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
        let paused = tokio::time::timeout(std::time::Duration::from_millis(500), interrupt)
            .await
            .expect("interrupt_session must return while the active run is unwinding")
            .expect("interrupt task should join")
            .expect("interrupt_session should succeed");
        assert!(paused);
        assert_eq!(exited.load(Ordering::SeqCst), 0);
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(exec.is_run_in_flight(&session.id).await);

        allow_exit.store(1, Ordering::SeqCst);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while exec.is_run_in_flight(&session.id).await {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("run slot should be released after the handler exits");
        assert_eq!(exited.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn end_session_nonexistent_succeeds() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        // end_session on a nonexistent session updates DB directly.
        let result = exec.end_session("nonexistent").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn end_session_paused_marks_completed() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let status = exec.end_session(&session.id).await.unwrap();
        assert_eq!(status, SessionStatus::Completed);
    }

    #[tokio::test]
    async fn add_and_drain_follow_ups() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.add_follow_up(&session.id, "extra context 1")
            .await
            .unwrap();
        exec.add_follow_up(&session.id, "extra context 2")
            .await
            .unwrap();
        let drained: Vec<String> = exec
            .drain_follow_ups(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["extra context 1", "extra context 2"]);
        assert!(exec.drain_follow_ups(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn answer_supplement_carries_is_answer_flag() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.add_answer_with_attachments(&session.id, "the answer", &[], None)
            .await
            .unwrap();
        exec.add_follow_up(&session.id, "plain context")
            .await
            .unwrap();
        let drained = exec.drain_follow_ups(&session.id).await;
        assert_eq!(drained.len(), 2);
        assert!(drained[0].is_answer, "first message is an ask reply");
        assert_eq!(drained[0].text, "the answer");
        assert!(!drained[1].is_answer, "plain supplement is not an answer");
    }

    #[tokio::test]
    async fn add_and_drain_follow_ups_with_attachments() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        let att = MessageAttachment::new("image/png", "aGVsbG8=");
        exec.add_follow_up_with_attachments(&session.id, "看图", std::slice::from_ref(&att), None)
            .await
            .unwrap();
        let drained = exec.drain_follow_ups(&session.id).await;
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].text, "看图");
        assert_eq!(drained[0].attachments, vec![att]);
        assert!(exec.drain_follow_ups(&session.id).await.is_empty());
    }

    #[tokio::test]
    async fn add_follow_up_nonexistent_session_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let result = exec.add_follow_up("nonexistent", "ctx").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn add_and_drain_steering() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.add_steering(&session.id, "steer 1").await.unwrap();
        let drained: Vec<String> = exec
            .drain_steering(&session.id)
            .await
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(drained, vec!["steer 1"]);
    }

    #[tokio::test]
    async fn list_tool_runs_all_present() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));

        let _low = exec.create_session("low").await.unwrap();
        let _normal = exec.create_session("normal").await.unwrap();
        let _high = exec.create_session("high").await.unwrap();

        let sessions = exec.list_runtime_sessions().await;
        assert_eq!(sessions.len(), 3);
    }

    #[tokio::test]
    async fn get_active_session_status_returns_correct_status() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn get_active_session_status_nonexistent_returns_none() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        // Absent means "not in the working set", NOT Error.
        assert_eq!(exec.get_active_session_status("nonexistent").await, None);
    }

    #[tokio::test]
    async fn cancellation_token_returns_default_for_unknown_session() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let token = exec.cancellation_token("nonexistent").await;
        assert!(!token.is_cancelled());
    }

    #[tokio::test]
    async fn load_pending_tool_runs_reloads_after_restart() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools.clone(),
            3,
        ));
        let session = exec.create_session("queued before restart").await.unwrap();

        // Simulate a restart: fresh executor over the same DB with an empty
        // working set. The pending session must be reloaded and dispatchable.
        let exec2 = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 3));
        assert!(exec2.list_runtime_sessions().await.is_empty());
        let loaded = exec2.load_pending_sessions().await.unwrap();
        assert_eq!(loaded, 1);

        let claimed = exec2.try_claim_pending().await;
        assert_eq!(claimed.as_deref(), Some(session.id.as_str()));
        assert_eq!(
            exec2.get_active_session_status(&session.id).await,
            Some(SessionStatus::Running)
        );
    }

    #[tokio::test]
    async fn pending_recovery_skips_interaction_replay_failure_without_installing_actor() {
        use haven_memory::INTERACTION_REQUESTED_EVENT_TYPE;

        let db = temp_db();
        // Pending records are loaded newest first, so create the healthy
        // session first to prove a later broken record does not stop the batch.
        let healthy = db.create_session("healthy pending session").unwrap();
        let broken = db.create_session("broken pending session").unwrap();
        let grant = SessionAuthorizationGrant::session(
            CapabilityScope::try_new("files.write").unwrap(),
            PermissionTarget::Operation,
            PermissionEffect::Allow,
        );
        db.save_session_authorization_grant(&healthy.id, &grant)
            .unwrap();
        db.save_session_authorization_grant(&broken.id, &grant)
            .unwrap();
        SessionStore::new(db.clone())
            .append_domain_event(&broken.id, INTERACTION_REQUESTED_EVENT_TYPE, "{}")
            .await
            .unwrap();

        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));

        assert_eq!(exec.load_pending_sessions().await.unwrap(), 1);
        assert!(exec.actor_for(&healthy.id).await.is_some());
        assert!(exec.actor_for(&broken.id).await.is_none());
        assert!(matches!(
            exec.authorization
                .authorize(&high_risk_session_request(&healthy.id))
                .await,
            AuthorizationDecision::AutoApproved
        ));
        assert!(matches!(
            exec.authorization
                .authorize(&high_risk_session_request(&broken.id))
                .await,
            AuthorizationDecision::RequiresConfirmation { .. }
        ));

        // A failed install is not cached: a later explicit load attempts the
        // durable replay again and still fails closed while the event is bad.
        assert!(exec.ensure_session_loaded(&broken.id).await.is_err());
        assert!(exec.actor_for(&broken.id).await.is_none());
        assert_eq!(
            db.session_authorization_grants(&broken.id).unwrap(),
            vec![grant]
        );
    }

    #[tokio::test]
    async fn pending_recovery_requeues_an_existing_pending_actor() {
        let exec = make_executor(1);
        let session = exec
            .create_session("pending actor recovery retry")
            .await
            .unwrap();
        exec.dequeue_pending(&session.id).await;
        let mut wake = exec.subscribe_dispatch();

        assert_eq!(exec.load_pending_sessions().await.unwrap(), 0);
        tokio::time::timeout(std::time::Duration::from_millis(100), wake.changed())
            .await
            .expect("recovery should wake the dispatcher even when queue insertion is deduped")
            .expect("dispatcher wake channel should remain open");
        assert_eq!(
            exec.try_claim_pending().await.as_deref(),
            Some(session.id.as_str()),
            "an already-installed Pending actor must be requeued by recovery"
        );
    }

    #[tokio::test]
    async fn pending_recovery_retries_batch_failure_and_dispatches_once() {
        let db = temp_db();
        let original = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = original
            .create_session("pending recovery transient failure")
            .await
            .unwrap();

        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        exec.pending_session_recovery_failures
            .store(1, Ordering::SeqCst);
        let handled = Arc::new(AtomicU32::new(0));
        let handled_by_runner = handled.clone();
        let exec_for_runner = exec.clone();
        let handler: SessionRunHandler = Arc::new(move |session_id: String| {
            let handled = handled_by_runner.clone();
            let exec = exec_for_runner.clone();
            Box::pin(async move {
                handled.fetch_add(1, Ordering::SeqCst);
                exec.update_session_status(&session_id, SessionStatus::Completed)
                    .await?;
                Ok(())
            })
        });
        let cancellation = CancellationToken::new();

        exec.clone()
            .start_dispatcher_with_cancellation(handler, cancellation.clone());

        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while handled.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dispatcher should dispatch the pending session after recovery");
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while db
                .get_session(&session.id)
                .unwrap()
                .map(|record| record.status)
                != Some(SessionStatus::Completed)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the recovered pending session should complete");
        cancellation.cancel();

        assert_eq!(handled.load(Ordering::SeqCst), 1);
        assert_eq!(
            exec.pending_session_recovery_attempts
                .load(Ordering::SeqCst),
            2,
            "one injected batch failure should be followed by one successful batch read"
        );
        assert_eq!(
            db.get_session(&session.id)
                .unwrap()
                .map(|record| record.status),
            Some(SessionStatus::Completed)
        );
    }

    #[tokio::test]
    async fn pending_recovery_retry_cancels_during_backoff() {
        let exec = make_executor(1);
        exec.pending_session_recovery_failures
            .store(usize::MAX, Ordering::SeqCst);
        let cancellation = CancellationToken::new();
        let backoff_started = exec.pending_session_recovery_backoff_started.notified();
        let recovery = {
            let exec = exec.clone();
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                exec.recover_pending_sessions_with_retry(&cancellation, 0)
                    .await
            })
        };

        tokio::time::timeout(std::time::Duration::from_secs(2), backoff_started)
            .await
            .expect("cancellation test should observe the retry backoff");
        cancellation.cancel();

        assert_eq!(recovery.await.unwrap(), None);
        assert_eq!(
            exec.pending_session_recovery_attempts
                .load(Ordering::SeqCst),
            1,
            "cancellation during backoff must prevent another read"
        );
    }

    #[tokio::test]
    async fn pending_recovery_stops_after_an_empty_successful_batch() {
        let exec = make_executor(1);
        exec.pending_session_recovery_failures
            .store(1, Ordering::SeqCst);

        let loaded = exec
            .recover_pending_sessions_with_retry(&CancellationToken::new(), 0)
            .await;

        assert_eq!(loaded, Some(0));
        assert_eq!(
            exec.pending_session_recovery_attempts
                .load(Ordering::SeqCst),
            2,
            "an empty successful batch ends retry even when nothing was newly loaded"
        );
    }

    #[tokio::test]
    async fn dispatcher_can_defer_pending_recovery_until_catalog_ready() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools.clone(),
            1,
        ));
        let session = exec.create_session("queued before catalog").await.unwrap();
        let exec2 = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 1));
        let handled = Arc::new(AtomicU32::new(0));
        let handled_by_runner = handled.clone();
        let exec_for_runner = exec2.clone();
        let handler: SessionRunHandler = Arc::new(move |session_id: String| {
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
        assert!(exec2.list_runtime_sessions().await.is_empty());
        assert_eq!(
            db.get_session(&session.id)
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
            while exec2.get_active_session_status(&session.id).await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("deferred recovery should release the completed session");
    }

    #[tokio::test]
    async fn load_pending_tool_runs_skips_non_pending() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools.clone(),
            3,
        ));
        let done = exec.create_session("done").await.unwrap();
        exec.end_session(&done.id).await.unwrap();
        let paused = exec.create_session("paused").await.unwrap();
        exec.update_session_status(&paused.id, SessionStatus::Paused)
            .await
            .unwrap();

        // Restart: only the still-pending session is reloaded.
        let exec2 = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let loaded = exec2.load_pending_sessions().await.unwrap();
        assert_eq!(loaded, 0);
        assert!(exec2.list_runtime_sessions().await.is_empty());
    }

    #[tokio::test]
    async fn update_session_status_changes_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        // Terminal status removes the session from the in-memory working set.
        assert_eq!(exec.get_active_session_status(&session.id).await, None);
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Completed
        );
    }

    #[tokio::test]
    async fn status_persistence_retries_and_failed_transition_keeps_actor_state() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 3));
        let session = exec
            .create_session("retry status persistence")
            .await
            .unwrap();

        db.conn()
            .execute_batch(
                "CREATE TABLE status_update_attempts (attempt INTEGER NOT NULL);
                 CREATE TRIGGER fail_session_status_update
                 BEFORE UPDATE OF status ON sessions
                 BEGIN
                     INSERT INTO status_update_attempts VALUES (1);
                     SELECT RAISE(FAIL, 'forced status write failure');
                 END;",
            )
            .unwrap();

        let error = exec
            .update_session_status(&session.id, SessionStatus::Running)
            .await
            .expect_err("a failed durable write must reject the transition");
        assert!(format!("{error:#}").contains("forced status write failure"));
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Pending),
            "the actor state must not change before persistence succeeds"
        );
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        let attempts: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM status_update_attempts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(attempts, 3, "status persistence retries three times");
    }

    #[tokio::test]
    async fn end_session_persists_completed_when_actor_is_not_loaded() {
        let db = temp_db();
        let session = db.create_session("unloaded session").unwrap();
        let tool_run_id = "toolrun-actorless-end";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "Actorless end",
            "must be cancelled",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            3,
        ));
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;

        assert_eq!(
            exec.end_session(&session.id).await.unwrap(),
            SessionStatus::Completed
        );
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Completed
        );
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn actorless_end_failure_pauses_and_retry_cancels_remaining_tool_run() {
        let db = temp_db();
        let session = db.create_session("actorless end retry").unwrap();
        let tool_run_id = "toolrun-actorless-end-retry";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "Actorless end retry",
            "must remain retryable",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;
        let mut events = exec.subscribe_events();
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER block_end_retry_cancel
                 BEFORE UPDATE OF status ON tool_runs
                 WHEN NEW.id = '{tool_run_id}' AND NEW.status = 'cancelled'
                 BEGIN SELECT RAISE(ABORT, 'injected end cancellation failure'); END;"
            ))
            .unwrap();

        let error = exec
            .end_session(&session.id)
            .await
            .expect_err("durable ToolRun cleanup failure must reject end");
        assert!(format!("{error:#}").contains("injected end cancellation failure"));
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Waiting
        );
        assert!(matches!(
            events.try_recv(),
            Ok(SessionSupervisorEvent::SessionEndPaused { session_id }) if session_id == session.id
        ));

        db.conn()
            .execute_batch("DROP TRIGGER block_end_retry_cancel")
            .unwrap();
        assert_eq!(
            exec.end_session(&session.id).await.unwrap(),
            SessionStatus::Completed
        );
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn resident_end_partial_failure_keeps_paused_actor_and_retry_completes() {
        let (exec, db) = make_executor_with_db(1);
        let session = exec.create_session("resident end retry").await.unwrap();
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;
        let blocked_id = "toolrun-resident-blocked";
        let cancelled_id = "toolrun-resident-cancelled";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        for tool_run_id in [blocked_id, cancelled_id] {
            db.save_scheduled_tool_run(
                tool_run_id,
                &due_at,
                "Resident end retry",
                "partial cleanup must converge",
                "tool",
                Some(&session.id),
                Some("notify"),
                None,
                None,
                None,
            )
            .unwrap();
        }
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER block_resident_end_cancel
                 BEFORE UPDATE OF status ON tool_runs
                 WHEN NEW.id = '{blocked_id}' AND NEW.status = 'cancelled'
                 BEGIN SELECT RAISE(ABORT, 'injected resident cancellation failure'); END;"
            ))
            .unwrap();

        let error = exec
            .end_session(&session.id)
            .await
            .expect_err("partial durable cleanup must leave end retryable");
        assert!(format!("{error:#}").contains("injected resident cancellation failure"));
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert_eq!(
            exec.waiting_reason(&session.id).await,
            Some(SessionWaitingReason::EndIncomplete)
        );
        assert_eq!(
            db.get_tool_run(blocked_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Waiting
        );
        assert_eq!(
            db.get_tool_run(cancelled_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );

        db.conn()
            .execute_batch("DROP TRIGGER block_resident_end_cancel")
            .unwrap();
        assert_eq!(
            exec.end_session(&session.id).await.unwrap(),
            SessionStatus::Completed
        );
        assert!(exec.actor_for(&session.id).await.is_none());
        assert_eq!(
            db.get_tool_run(blocked_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn end_status_write_failure_does_not_cancel_run_or_owned_tool_runs() {
        let (exec, db) = make_executor_with_db(1);
        let session = exec.create_session("end status write retry").await.unwrap();
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;
        let tool_run_id = "toolrun-end-status-write-retry";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "End status write retry",
            "must not be cancelled before pause commits",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_end_pause
                 BEFORE UPDATE OF status ON sessions
                 BEGIN SELECT RAISE(ABORT, 'injected pause persistence failure'); END;",
            )
            .unwrap();
        let run_token = exec.cancellation_token(&session.id).await;

        let error = exec
            .end_session(&session.id)
            .await
            .expect_err("end must fail before cancelling when Paused is not durable");
        assert!(format!("{error:#}").contains("injected pause persistence failure"));
        assert_eq!(
            exec.get_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
        assert!(!run_token.is_cancelled());
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Waiting
        );

        db.conn()
            .execute_batch("DROP TRIGGER reject_end_pause")
            .unwrap();
        assert_eq!(
            exec.end_session(&session.id).await.unwrap(),
            SessionStatus::Completed
        );
    }

    #[tokio::test]
    async fn actorless_delete_cancels_unrestored_scheduled_tool_run_before_deleting_session() {
        let db = temp_db();
        let session = db.create_session("unloaded delete session").unwrap();
        let tool_run_id = "toolrun-actorless-delete";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "Actorless delete",
            "must be cancelled before delete",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            3,
        ));
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;

        exec.delete_session(&session.id).await.unwrap();

        assert!(db.get_session(&session.id).unwrap().is_none());
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn session_delete_stays_open_when_durable_tool_run_cancellation_fails() {
        let db = temp_db();
        let session = db.create_session("delete fail closed").unwrap();
        let tool_run_id = "toolrun-delete-fail-closed";
        let due_at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        db.save_scheduled_tool_run(
            tool_run_id,
            &due_at,
            "Delete failure",
            "remain attached until cleanup works",
            "tool",
            Some(&session.id),
            Some("notify"),
            None,
            None,
            None,
        )
        .unwrap();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            3,
        ));
        exec.tool_run_service()
            .set_tool_run_store(Some(ToolRunStore::new(db.clone())))
            .await;
        db.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER block_session_tool_run_cancel
                 BEFORE UPDATE OF status ON tool_runs
                 WHEN NEW.id = '{tool_run_id}' AND NEW.status = 'cancelled'
                 BEGIN SELECT RAISE(ABORT, 'injected cancellation failure'); END;"
            ))
            .unwrap();

        let error = exec
            .delete_session(&session.id)
            .await
            .expect_err("a session must remain if its durable ToolRun cannot be cancelled");
        assert!(format!("{error:#}").contains("injected cancellation failure"));
        assert!(db.get_session(&session.id).unwrap().is_some());
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Waiting
        );

        db.conn()
            .execute_batch("DROP TRIGGER block_session_tool_run_cancel")
            .unwrap();
        exec.delete_session(&session.id).await.unwrap();
        assert!(db.get_session(&session.id).unwrap().is_none());
        assert_eq!(
            db.get_tool_run(tool_run_id).unwrap().unwrap().status,
            haven_common::ToolRunStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn update_session_status_completed_cleans_up() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("test").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        assert!(!exec.is_run_in_flight(&session.id).await);
        assert!(exec.get_active_session_status(&session.id).await.is_none());
    }

    #[tokio::test]
    async fn execute_step_unknown_tool_errors() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
    }

    #[tokio::test]
    async fn execute_step_rejects_pending_without_forcing_running() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("pending tool").await.unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
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
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn execute_step_rejects_missing_session_fail_closed() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
    async fn interaction_events_persist_and_replay_into_reloaded_actor() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools.clone(),
            3,
        ));
        let session = exec.create_session("ask me").await.unwrap();

        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let request = crate::interaction::InteractionRequest::ask(
            &session.id,
            vec!["README.md".into()],
            vec!["step-0123456789abcdef0123456789abcdef".into()],
        );
        let mut runtime_events = exec.subscribe_events();
        exec.request_interaction(request.clone()).await.unwrap();
        let delivery = runtime_events.recv().await.unwrap();
        let SessionSupervisorEvent::InteractionRequested { envelope } = delivery else {
            panic!("expected interaction runtime envelope, got {delivery:?}");
        };
        assert_eq!(
            envelope.owner,
            crate::interaction::InteractionOwner::Session {
                session_id: session.id.clone(),
            }
        );
        assert_eq!(envelope.request, request);
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );

        let events = exec
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].event_type,
            haven_memory::INTERACTION_REQUESTED_EVENT_TYPE
        );
        assert_eq!(events[0].run_id, None);
        assert_eq!(events[0].step_number, None);
        let persisted_payload: Value = serde_json::from_str(&events[0].payload).unwrap();
        assert_eq!(persisted_payload["session_id"], session.id);
        assert!(persisted_payload.get("owner").is_none());
        assert_eq!(
            serde_json::from_str::<crate::interaction::InteractionRequest>(&events[0].payload)
                .unwrap(),
            request
        );

        // A fresh supervisor reconstructs actor interaction state from the
        // durable domain event through SessionStore.
        let reloaded = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            tools.clone(),
            3,
        ));
        reloaded.ensure_session_loaded(&session.id).await.unwrap();
        let pending = reloaded
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
            .await;
        assert_eq!(pending.len(), 1);

        reloaded
            .clear_interactions(&session.id, Some(crate::interaction::InteractionKind::Ask))
            .await
            .unwrap();
        let confirm = crate::interaction::InteractionRequest::confirm(
            &session.id,
            1,
            "test.operation".into(),
            serde_json::json!({}),
            "call-test".into(),
            "step-confirm".into(),
            0,
            haven_common::types::RiskLevel::Safe,
            Some(haven_tools::ConfirmationReceipt {
                confirmation_id: haven_common::types::new_id("conf").into(),
                capability: haven_common::types::CapabilityScope::try_new("test.operation")
                    .unwrap(),
                canonical_input_hash: String::new(),
                effective_risk: haven_common::types::RiskLevel::Safe,
                policy_revision: 1,
                expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
            }),
        );
        reloaded.request_interaction(confirm.clone()).await.unwrap();
        let resolved = reloaded
            .resolve_interaction(&session.id, &confirm.id, serde_json::json!(true), false)
            .await
            .unwrap()
            .expect("confirmation should resolve");
        assert_eq!(
            resolved.status,
            crate::interaction::InteractionStatus::Resolved
        );

        let events = reloaded
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[1].event_type,
            haven_memory::INTERACTION_CLEARED_EVENT_TYPE
        );
        assert_eq!(
            events[2].event_type,
            haven_memory::INTERACTION_REQUESTED_EVENT_TYPE
        );
        assert_eq!(
            events[3].event_type,
            haven_memory::INTERACTION_RESOLVED_EVENT_TYPE
        );
        assert_eq!(
            serde_json::from_str::<crate::interaction::InteractionRequest>(&events[3].payload)
                .unwrap()
                .status,
            crate::interaction::InteractionStatus::Resolved
        );

        // A subsequent actor reload observes the resolved request as cleared.
        let final_reload = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        final_reload
            .ensure_session_loaded(&session.id)
            .await
            .unwrap();
        assert!(
            final_reload
                .pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
                .await
                .is_empty()
        );
        assert!(
            final_reload
                .pending_interactions(&session.id, crate::interaction::InteractionKind::Confirm)
                .await
                .is_empty()
        );

        // Resolving the last pending confirmation reactivates the session.
        assert_eq!(
            reloaded.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn final_confirmation_event_and_session_resume_commit_atomically() {
        let db = temp_db();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = exec
            .create_session("atomic confirmation resume")
            .await
            .unwrap();
        let make_request = |step_id: &str| {
            crate::interaction::InteractionRequest::confirm(
                &session.id,
                1,
                "test.operation".into(),
                serde_json::json!({}),
                format!("call-{step_id}"),
                step_id.into(),
                0,
                haven_common::types::RiskLevel::High,
                Some(haven_tools::ConfirmationReceipt {
                    confirmation_id: haven_common::types::new_id("conf").into(),
                    capability: haven_common::types::CapabilityScope::try_new("test.operation")
                        .unwrap(),
                    canonical_input_hash: String::new(),
                    effective_risk: haven_common::types::RiskLevel::High,
                    policy_revision: 1,
                    expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
                }),
            )
        };
        let requests = vec![
            make_request("step-atomic-first"),
            make_request("step-atomic-last"),
        ];
        exec.request_confirm_batch(&session.id, requests.clone())
            .await
            .unwrap();

        let mut runtime_events = exec.subscribe_events();
        let mut dispatch_wake = exec.subscribe_dispatch();
        let wake_before = *dispatch_wake.borrow_and_update();

        // Resolving a non-final confirmation persists only that decision and
        // leaves the batch gated in Paused without waking the dispatcher.
        let first = exec
            .resolve_interaction(&session.id, &requests[0].id, serde_json::json!(true), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            first.status,
            crate::interaction::InteractionStatus::Resolved
        );
        let first_event = runtime_events.recv().await.unwrap();
        assert!(matches!(
            first_event,
            SessionSupervisorEvent::InteractionRequested { ref envelope }
                if envelope.request.id == requests[0].id
        ));
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        assert!(!dispatch_wake.has_changed().unwrap());

        db.conn()
            .execute_batch(
                "CREATE TRIGGER fail_confirmation_resume_status
                 BEFORE UPDATE OF status ON sessions
                 WHEN OLD.status = 'paused' AND NEW.status = 'pending'
                 BEGIN SELECT RAISE(ABORT, 'forced confirmation resume status failure'); END;",
            )
            .unwrap();
        let error = exec
            .resolve_interaction(&session.id, &requests[1].id, serde_json::json!(true), false)
            .await
            .expect_err("failed status CAS must reject the final decision commit");
        assert!(format!("{error:#}").contains("forced confirmation resume status failure"));
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Paused
        );
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let pending = exec
            .pending_interactions(&session.id, crate::interaction::InteractionKind::Confirm)
            .await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, requests[1].id);
        let failed_events = exec
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(
            failed_events
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_RESOLVED_EVENT_TYPE)
                .count(),
            1,
            "the failed final resolve must not append its decision event"
        );
        assert!(!dispatch_wake.has_changed().unwrap());
        assert!(runtime_events.try_recv().is_err());

        db.conn()
            .execute_batch("DROP TRIGGER fail_confirmation_resume_status;")
            .unwrap();
        let retried = exec
            .resolve_interaction(&session.id, &requests[1].id, serde_json::json!(true), false)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            retried.status,
            crate::interaction::InteractionStatus::Resolved
        );
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
        let resumed = runtime_events.recv().await.unwrap();
        assert!(
            matches!(resumed, SessionSupervisorEvent::SessionResumed { ref session_id } if session_id == &session.id)
        );
        let resolved_event = runtime_events.recv().await.unwrap();
        assert!(matches!(
            resolved_event,
            SessionSupervisorEvent::InteractionRequested { ref envelope }
                if envelope.request.id == requests[1].id
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), dispatch_wake.changed())
            .await
            .expect("successful final confirmation must wake the dispatcher")
            .unwrap();
        assert_eq!(*dispatch_wake.borrow(), wake_before + 1);
        assert_eq!(
            exec.try_claim_pending().await.as_deref(),
            Some(session.id.as_str())
        );
        assert!(exec.try_claim_pending().await.is_none());
        let committed_events = exec
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(
            committed_events
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_RESOLVED_EVENT_TYPE)
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn expiring_final_confirmation_atomically_resumes_session() {
        let db = temp_db();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = exec
            .create_session("atomic confirmation expiry")
            .await
            .unwrap();
        let confirmation = crate::interaction::InteractionRequest::confirm(
            &session.id,
            1,
            "test.operation".into(),
            serde_json::json!({}),
            "call-step-expiring".into(),
            "step-expiring".into(),
            0,
            haven_common::types::RiskLevel::High,
            Some(haven_tools::ConfirmationReceipt {
                confirmation_id: haven_common::types::new_id("conf").into(),
                capability: haven_common::types::CapabilityScope::try_new("test.operation")
                    .unwrap(),
                canonical_input_hash: String::new(),
                effective_risk: haven_common::types::RiskLevel::High,
                policy_revision: 1,
                expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
            }),
        );
        exec.request_confirm_batch(&session.id, vec![confirmation.clone()])
            .await
            .unwrap();

        let mut runtime_events = exec.subscribe_events();
        let mut dispatch_wake = exec.subscribe_dispatch();
        let wake_before = *dispatch_wake.borrow_and_update();
        let expired = exec
            .resolve_interaction(&session.id, &confirmation.id, serde_json::Value::Null, true)
            .await
            .unwrap()
            .expect("final confirmation expiry should commit");
        assert_eq!(
            expired.status,
            crate::interaction::InteractionStatus::Expired
        );
        assert_eq!(
            db.get_session(&session.id).unwrap().unwrap().status,
            SessionStatus::Pending
        );
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
        assert!(matches!(
            runtime_events.recv().await.unwrap(),
            SessionSupervisorEvent::SessionResumed { ref session_id } if session_id == &session.id
        ));
        assert!(matches!(
            runtime_events.recv().await.unwrap(),
            SessionSupervisorEvent::InteractionRequested { ref envelope }
                if envelope.request.id == confirmation.id
                    && envelope.request.status == crate::interaction::InteractionStatus::Expired
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), dispatch_wake.changed())
            .await
            .expect("final confirmation expiry must wake the dispatcher")
            .unwrap();
        assert_eq!(*dispatch_wake.borrow(), wake_before + 1);
        assert_eq!(
            exec.try_claim_pending().await.as_deref(),
            Some(session.id.as_str())
        );
        assert!(exec.try_claim_pending().await.is_none());
    }

    #[tokio::test]
    async fn invalid_confirmation_batch_does_not_pause_or_partially_register() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db.clone(), tools, 1));
        let session = exec
            .create_session("atomic confirmation batch")
            .await
            .unwrap();

        let make_request = |step_id: &str, receipt: Option<haven_tools::ConfirmationReceipt>| {
            crate::interaction::InteractionRequest::confirm(
                &session.id,
                1,
                "test.operation".into(),
                serde_json::json!({}),
                format!("call-{step_id}"),
                step_id.into(),
                0,
                haven_common::types::RiskLevel::Safe,
                receipt,
            )
        };
        let receipt = || haven_tools::ConfirmationReceipt {
            confirmation_id: haven_common::types::new_id("conf").into(),
            capability: haven_tools::CapabilityScope::try_new("test.operation").unwrap(),
            canonical_input_hash: String::new(),
            effective_risk: haven_common::types::RiskLevel::Safe,
            policy_revision: 1,
            expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
        };

        let valid = make_request("step-batch-valid", Some(receipt()));
        let invalid = make_request("step-batch-invalid", None);
        assert!(
            exec.request_confirm_batch(&session.id, vec![valid, invalid])
                .await
                .is_err()
        );
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending),
            "an invalid member must be rejected before the session is paused"
        );
        assert!(
            exec.pending_interactions(&session.id, crate::interaction::InteractionKind::Confirm)
                .await
                .is_empty()
        );
        assert!(
            exec.store
                .read_active_domain_events_async(&session.id)
                .await
                .unwrap()
                .is_empty()
        );

        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_confirmation_batch_event
                 BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'interaction_requested'
                 BEGIN SELECT RAISE(ABORT, 'test confirmation append failure'); END;",
            )
            .unwrap();
        assert!(
            exec.request_confirm_batch(
                &session.id,
                vec![
                    make_request("step-batch-failed-first", Some(receipt())),
                    make_request("step-batch-failed-second", Some(receipt())),
                ],
            )
            .await
            .is_err()
        );
        db.conn()
            .execute_batch("DROP TRIGGER reject_confirmation_batch_event;")
            .unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending),
            "status and interaction events must roll back together on append failure"
        );
        assert!(
            exec.pending_interactions(&session.id, crate::interaction::InteractionKind::Confirm)
                .await
                .is_empty()
        );
        assert!(
            exec.store
                .read_active_domain_events_async(&session.id)
                .await
                .unwrap()
                .is_empty()
        );

        exec.request_confirm_batch(
            &session.id,
            vec![
                make_request("step-batch-first", Some(receipt())),
                make_request("step-batch-second", Some(receipt())),
            ],
        )
        .await
        .unwrap();
        let persisted = exec
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(
            persisted
                .iter()
                .filter(|event| event.event_type == haven_memory::INTERACTION_REQUESTED_EVENT_TYPE)
                .count(),
            2,
            "a valid confirmation batch must persist all requests together"
        );
    }

    #[tokio::test]
    async fn confirmation_plan_and_real_requests_pause_atomically() {
        let db = temp_db();
        let exec = Arc::new(SessionSupervisor::new_for_test(
            db.clone(),
            Arc::new(ToolsFacade::new()),
            1,
        ));
        let session = exec
            .create_session("confirmation continuation")
            .await
            .unwrap();
        let input = serde_json::json!({"path": "notes.txt"});
        let step_id = "step-confirmation-plan";
        let request = crate::interaction::InteractionRequest::confirm(
            &session.id,
            4,
            "files.write".into(),
            input.clone(),
            "call-confirmation-plan".into(),
            step_id.into(),
            2,
            haven_common::types::RiskLevel::High,
            Some(haven_tools::ConfirmationReceipt {
                confirmation_id: haven_common::types::new_id("conf").into(),
                capability: haven_common::types::CapabilityScope::try_new("files.write").unwrap(),
                canonical_input_hash: String::new(),
                effective_risk: haven_common::types::RiskLevel::High,
                policy_revision: 1,
                expires_at: chrono::Utc::now().timestamp().max(0) as u64 + 300,
            }),
        );
        let plan = crate::react::tool_batch_plan::ConfirmationBatchPlan {
            step_number: 4,
            tools: vec![crate::react::tool_batch_plan::ConfirmationBatchTool {
                step_id: step_id.into(),
                tool_index: 2,
                tool_call_id: Some("call-confirmation-plan".into()),
                confirmation_request_id: Some(request.id.clone()),
            }],
        };

        db.conn()
            .execute_batch(
                "CREATE TRIGGER reject_confirmation_plan
                 BEFORE INSERT ON session_events
                 WHEN NEW.event_type = 'confirmation_batch_planned'
                 BEGIN SELECT RAISE(ABORT, 'test confirmation plan append failure'); END;",
            )
            .unwrap();
        assert!(
            exec.request_confirm_batch_with_plan(&session.id, vec![request.clone()], plan.clone())
                .await
                .is_err()
        );
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
        assert!(exec.interaction_requests(&session.id).await.is_empty());
        assert!(
            exec.store
                .read_active_domain_events_async(&session.id)
                .await
                .unwrap()
                .is_empty()
        );

        db.conn()
            .execute_batch("DROP TRIGGER reject_confirmation_plan;")
            .unwrap();
        exec.request_confirm_batch_with_plan(&session.id, vec![request.clone()], plan)
            .await
            .unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        let events = exec
            .store
            .read_active_domain_events_async(&session.id)
            .await
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].event_type,
            haven_memory::INTERACTION_REQUESTED_EVENT_TYPE
        );
        assert_eq!(
            events[1].event_type,
            haven_memory::CONFIRMATION_BATCH_PLANNED_EVENT_TYPE
        );
    }

    #[tokio::test]
    async fn plain_pause_has_no_interaction() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("pause me").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let state = exec.get_active_session_status(&session.id).await.unwrap();
        assert!(state.is_paused());
        assert!(
            exec.pending_interactions(&session.id, crate::interaction::InteractionKind::Ask)
                .await
                .is_empty()
        );
        assert_eq!(state.as_str(), "paused");
    }

    #[tokio::test]
    async fn terminal_tool_runs_need_explicit_reopen_to_reactivate() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("t").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Completed)
            .await
            .unwrap();
        // The terminal session was removed from the working set; any later
        // update on the absent entry is a silent no-op, not a resurrection.
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        assert_eq!(exec.get_active_session_status(&session.id).await, None);
        // In-memory resurrection is only possible through the explicit
        // reopen path (Completed → Paused) after ensure_session_loaded.
        exec.ensure_session_loaded(&session.id).await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Paused)
        );
        // And from Paused the session resumes via the normal Paused → Pending
        // path (e.g. process_input / continue flow).
        exec.update_session_status(&session.id, SessionStatus::Pending)
            .await
            .unwrap();
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Pending)
        );
    }

    #[tokio::test]
    async fn status_watch_wakes_waiter_on_transition() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("wait").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();

        let late_subscriber = exec.subscribe_status(&session.id).await;
        assert_eq!(
            *late_subscriber.borrow(),
            SessionStatus::Paused,
            "a late watch subscriber must see the current actor status"
        );

        // Waiter subscribes AFTER the pause (the level-triggered value must
        // still be visible) and wakes on the resume transition.
        let exec2 = exec.clone();
        let tid = session.id.clone();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut rx = exec2.subscribe_status(&tid).await;
            let _ = rx.changed().await;
            let _ = done_tx.send(exec2.get_active_session_status(&tid).await);
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
    async fn conditional_wake_does_not_rewind_a_claimed_run() {
        let exec = make_executor(1);
        let session = exec.create_session("conditional wake").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Running)
            .await
            .unwrap();

        // A stale pause observer must not turn an already claimed run back
        // into Pending. This is the transition that used to trip the
        // run_react_loop entry assertion after resume/ToolRun wake-up.
        let changed = exec
            .update_session_status_if(&session.id, SessionStatus::Paused, SessionStatus::Pending)
            .await
            .unwrap();

        assert!(!changed);
        assert_eq!(
            exec.get_active_session_status(&session.id).await,
            Some(SessionStatus::Running)
        );
    }

    #[tokio::test]
    async fn same_status_pending_still_wakes_dispatcher() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
    async fn tool_run_completions_buffered_and_drained() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("background ToolRun").await.unwrap();

        assert!(
            exec.drain_tool_run_completions(&session.id)
                .await
                .is_empty()
        );

        let _ = exec
            .add_tool_run_completion(&session.id, "toolrun-1", "toolrun-1 done")
            .await;
        let _ = exec
            .add_tool_run_completion(&session.id, "toolrun-2", "toolrun-2 failed")
            .await;

        let drained = exec.drain_tool_run_completions(&session.id).await;
        assert_eq!(drained, vec!["toolrun-1 done", "toolrun-2 failed"]);
        assert!(
            exec.drain_tool_run_completions(&session.id)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn concurrent_steering_ingress_is_bounded_and_never_truncated() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
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
        assert_eq!(exec.drain_steering(&session.id).await.len(), accepted);
    }

    #[tokio::test]
    async fn drain_react_context_prioritizes_steering_over_follow_ups() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("context priority").await.unwrap();

        exec.add_follow_up(&session.id, "follow-up").await.unwrap();
        exec.add_steering(&session.id, "steering").await.unwrap();
        let _ = exec
            .add_tool_run_completion(&session.id, "toolrun-result", "ToolRun result")
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
        assert_eq!(
            batch
                .tool_run_results
                .iter()
                .map(|item| item.text.as_str())
                .collect::<Vec<_>>(),
            vec!["ToolRun result"]
        );

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
        assert!(batch.tool_run_results.is_empty());
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

        let follow_ups = exec.drain_follow_ups(&session.id).await;
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

        let steering = exec.drain_steering(&session.id).await;
        assert_eq!(steering.len(), 1);
        assert_eq!(steering[0].text, "first");
    }

    #[tokio::test]
    async fn remove_session_clears_tool_run_buffers_and_status_watcher() {
        let db = temp_db();
        let tools = Arc::new(ToolsFacade::new());
        let exec = Arc::new(SessionSupervisor::new_for_test(db, tools, 3));
        let session = exec.create_session("cleanup").await.unwrap();
        exec.update_session_status(&session.id, SessionStatus::Paused)
            .await
            .unwrap();
        let _ = exec
            .add_tool_run_completion(&session.id, "toolrun-stranded", "stranded")
            .await;
        let rx = exec.subscribe_status(&session.id).await;
        let _ = rx; // a subscriber must not keep the session alive after removal

        exec.remove_session(&session.id).await.unwrap();
        assert_eq!(exec.get_active_session_status(&session.id).await, None);
        assert!(
            exec.drain_tool_run_completions(&session.id)
                .await
                .is_empty()
        );
    }
}
