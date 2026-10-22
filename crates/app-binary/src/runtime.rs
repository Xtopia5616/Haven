//! Application-wide service ownership and lifecycle control.
//!
//! `ApplicationRuntime` is the lifecycle owner for long-lived service handles
//! and app-scoped asynchronous work. `AppState::new` is the composition root
//! that creates and injects those services. Components may still own their
//! domain workers (for example an action process or an MCP monitor), but the
//! application gives them one cancellation boundary and tears them down in a
//! deterministic order.

use crate::chatgpt_plan_auth::ConfigChatGptPlanAuthPort;
use crate::config_runtime::RuntimeConfigCoordinator;
use crate::desktop::DesktopShell;
use haven_agent::{AgentLayer, MemoryStartup, PendingSessionRecovery, SessionSupervisor};
use haven_common::config::ConfigService;
use haven_common::error::sanitize_error_text;
use haven_input::InputPipeline;
use haven_memory::{MemoryFactProposalStore, MemoryFactStore, SessionStore};
use haven_tools::{
    AuthorizationPort, LiveOutputSinkPort, ManagedAssetLifecyclePort, McpRuntimePort,
    ScheduleAgentCapability, ScheduleManagementCapability, SkillCatalogPort, SkillExecutionPort,
    ToolRunManagementCapability, ToolServices, ToolsFacade,
};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

/// The application-wide lifecycle owner.
///
/// The service fields are deliberately kept here rather than in several
/// independent state containers. `AppState` retains only Tauri-specific
/// transient state and exposes this runtime through its explicit `runtime`
/// field.
pub struct ApplicationRuntime {
    pub(crate) session_store: SessionStore,
    pub(crate) memory_fact_store: MemoryFactStore,
    pub(crate) memory_fact_proposal_store: MemoryFactProposalStore,
    pub(crate) tools: Arc<ToolsFacade>,
    /// App-facing domain capabilities provided by the Tools composition root.
    pub(crate) services: AppServices,
    pub(crate) executor: Arc<SessionSupervisor>,
    pub(crate) agent: Arc<AgentLayer>,
    pub(crate) memory_startup: MemoryStartup,
    pub(crate) input_pipeline: Arc<InputPipeline>,
    pub(crate) shell: Arc<DesktopShell>,
    pub(crate) log_filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    pub(crate) config_service: Arc<ConfigService>,
    pub(crate) chatgpt_plan_auth: Arc<ConfigChatGptPlanAuthPort>,
    /// Serializes settings and model config commit-plus-apply operations so a
    /// later snapshot cannot publish before an earlier runtime update ends.
    pub(crate) config_runtime_coordinator: RuntimeConfigCoordinator,
    agent_startup_started: AtomicBool,
    shutdown_token: CancellationToken,
    shutting_down: AtomicBool,
    tasks: Mutex<JoinSet<()>>,
    runtime_handle: tokio::runtime::Handle,
}

const TASK_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub(crate) struct RuntimeServices {
    pub(crate) session_store: SessionStore,
    pub(crate) memory_fact_store: MemoryFactStore,
    pub(crate) memory_fact_proposal_store: MemoryFactProposalStore,
    pub(crate) tools: Arc<ToolsFacade>,
    pub(crate) tool_services: ToolServices,
    pub(crate) executor: Arc<SessionSupervisor>,
    pub(crate) agent: Arc<AgentLayer>,
    pub(crate) memory_startup: MemoryStartup,
    pub(crate) input_pipeline: Arc<InputPipeline>,
    pub(crate) shell: Arc<DesktopShell>,
    pub(crate) log_filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    pub(crate) config_service: Arc<ConfigService>,
    pub(crate) chatgpt_plan_auth: Arc<ConfigChatGptPlanAuthPort>,
    pub(crate) config_apply_gate: Arc<tokio::sync::Mutex<()>>,
}

pub(crate) struct AppServices {
    pub(crate) mcp: Arc<dyn McpRuntimePort>,
    pub(crate) mcp_tool_caller: Arc<dyn haven_llm::McpToolCaller>,
    pub(crate) skills: Arc<dyn SkillCatalogPort>,
    pub(crate) skill_execution: Arc<dyn SkillExecutionPort>,
    pub(crate) authorization: Arc<dyn AuthorizationPort>,
    pub(crate) tool_runs: Arc<dyn ToolRunManagementCapability>,
    pub(crate) schedules: Arc<dyn ScheduleAgentCapability>,
    pub(crate) schedule_management: Arc<dyn ScheduleManagementCapability>,
    pub(crate) live_output: Arc<dyn LiveOutputSinkPort>,
    pub(crate) managed_assets: Arc<dyn ManagedAssetLifecyclePort>,
}

impl ApplicationRuntime {
    pub(crate) fn new(runtime_services: RuntimeServices) -> Self {
        let tool_services = runtime_services.tool_services;
        let services = AppServices {
            mcp: tool_services.mcp,
            mcp_tool_caller: tool_services.mcp_tool_caller,
            skills: tool_services.skills,
            skill_execution: tool_services.skill_execution,
            authorization: tool_services.authorization,
            tool_runs: tool_services.tool_run_management,
            schedules: tool_services.schedule_agent,
            schedule_management: tool_services.schedule_management,
            live_output: tool_services.live_output,
            managed_assets: tool_services.managed_assets,
        };
        let config_apply_gate = runtime_services.config_apply_gate;
        Self {
            session_store: runtime_services.session_store,
            memory_fact_store: runtime_services.memory_fact_store,
            memory_fact_proposal_store: runtime_services.memory_fact_proposal_store,
            tools: runtime_services.tools,
            services,
            executor: runtime_services.executor,
            agent: runtime_services.agent,
            memory_startup: runtime_services.memory_startup,
            input_pipeline: runtime_services.input_pipeline,
            shell: runtime_services.shell,
            log_filter_handles: runtime_services.log_filter_handles,
            config_service: runtime_services.config_service,
            chatgpt_plan_auth: runtime_services.chatgpt_plan_auth,
            config_runtime_coordinator: RuntimeConfigCoordinator::with_shared_gate(
                config_apply_gate,
            ),
            agent_startup_started: AtomicBool::new(false),
            shutdown_token: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
            tasks: Mutex::new(JoinSet::new()),
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    pub(crate) fn services(&self) -> &AppServices {
        &self.services
    }

    /// Return the root token shared by app-scoped workers and domain startup
    /// consumers. Child tokens are handed to individual tasks so a task can
    /// also pass cancellation to a nested operation without exposing the
    /// runtime itself.
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn task_count_for_test(&self) -> usize {
        self.tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Register an app-scoped task. The task is cancelled and joined by
    /// [`Self::shutdown`]. A task submitted after shutdown has begun is
    /// rejected and never detached.
    pub(crate) fn spawn<F>(&self, name: &'static str, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.spawn_with_child_token(name, |_| future)
    }

    /// Register an app-scoped task and provide it with a child of the runtime
    /// cancellation token. The child is cancelled when the runtime shuts down.
    pub(crate) fn spawn_with_child_token<F, Fut>(&self, name: &'static str, task: F) -> bool
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }

        let token = self.cancellation_token().child_token();
        let future = task(token.clone());
        let wrapped = async move {
            tokio::select! {
                biased;
                _ = token.cancelled() => {
                    tracing::debug!(task = name, "application task cancelled");
                }
                _ = future => {}
            }
        };

        self.register_task(wrapped)
    }

    /// Register a cancellation-aware task without an outer select that would
    /// drop its future. Use for tasks whose own cancellation path must finish
    /// before their join handle resolves.
    pub(crate) fn spawn_cancellable_with_child_token<F, Fut>(
        &self,
        name: &'static str,
        task: F,
    ) -> bool
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let token = self.cancellation_token().child_token();
        self.register_cancellation_aware_task(name, task(token))
    }

    /// Register a future that already owns its cancellation token and
    /// performs its own orderly shutdown before resolving.
    pub(crate) fn spawn_cancellable<F>(&self, name: &'static str, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.register_cancellation_aware_task(name, future)
    }

    fn register_cancellation_aware_task<F>(&self, name: &'static str, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.register_task(async move {
            tracing::trace!(task = name, "application cancellation-aware task started");
            future.await;
        })
    }

    fn register_task<F>(&self, future: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }

        let mut tasks = self.tasks.lock().unwrap_or_else(|poisoned| {
            tracing::error!("application task registry lock poisoned; recovering");
            poisoned.into_inner()
        });
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        // Short-lived callbacks can register many tasks over the lifetime of
        // the app. Reap completed results on each registration while retaining
        // their join diagnostics; active work remains owned until shutdown.
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result
                && !error.is_cancelled()
            {
                tracing::warn!(
                    error = %sanitize_error_text(&error.to_string()),
                    "application task failed before shutdown"
                );
            }
        }
        tasks.spawn_on(future, &self.runtime_handle);
        true
    }

    /// Register app-owned memory preparation and live consumption before
    /// opening the Agent dispatcher. The startup task is single-shot and the
    /// live consumer receives its own task handle for shutdown joining.
    pub(crate) fn start_agent_after_memory_ready(
        self: &Arc<Self>,
        recovery: PendingSessionRecovery,
    ) -> bool {
        if self
            .agent_startup_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::warn!("Agent memory startup was requested more than once");
            return false;
        }

        let runtime = self.clone();
        self.spawn_cancellable_with_child_token(
            "memory-runtime-startup",
            move |cancellation| async move {
                let prepared = match runtime.memory_startup.prepare_start(&cancellation).await {
                    Ok(prepared) => prepared,
                    Err(_error) if cancellation.is_cancelled() => {
                        tracing::debug!("memory runtime startup stopped by cancellation");
                        return;
                    }
                    Err(error) => {
                        tracing::error!(
                            error = %sanitize_error_text(&error.to_string()),
                            "memory runtime failed to prepare; session dispatcher startup aborted"
                        );
                        return;
                    }
                };

                if cancellation.is_cancelled() {
                    tracing::debug!("memory runtime became ready after startup cancellation");
                    return;
                }

                let live_consumer_handoff = runtime
                    .memory_startup
                    .prepare_live_consumer(prepared, runtime.cancellation_token().child_token());
                let Some(readiness) = live_consumer_handoff.register_consumer_with(|live_future| {
                    runtime
                        .spawn_cancellable("memory-runtime-live-consumer", live_future)
                        .then_some(())
                }) else {
                    return;
                };
                if cancellation.is_cancelled() {
                    return;
                }

                runtime
                    .agent
                    .clone()
                    .start_after_memory_ready(readiness, recovery, cancellation);
            },
        )
    }

    /// Stop all application work and release domain resources in dependency
    /// order. This method is idempotent so both the Tauri exit hook and test
    /// teardown can call it safely.
    pub(crate) async fn shutdown(&self) {
        if self
            .shutting_down
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        self.shutdown_token.cancel();
        // Stop the shared memory worker after the cancellation boundary so it
        // cannot acknowledge unfinished durable jobs during teardown.
        self.memory_startup.shutdown_background_workers();
        self.services().schedules.shutdown().await;

        // Stop producers before consumers and resource owners. In particular,
        // this prevents a final recording or session run from starting while
        // its backing process/network resources are being torn down.
        if let Err(error) = self.input_pipeline.shutdown().await {
            tracing::warn!(
                error = %sanitize_error_text(&error.to_string()),
                "input pipeline shutdown failed"
            );
        }
        self.agent.shutdown_background_tasks().await;
        if let Err(error) = self
            .executor
            .clear_session_runtime_state_for_shutdown()
            .await
        {
            tracing::warn!(
                error = %sanitize_error_text(&error.to_string()),
                "session shutdown did not quiesce every run"
            );
        }
        self.executor.join_dispatcher().await;
        self.services().tool_runs.shutdown().await;
        self.services().mcp.shutdown().await;

        let mut tasks = {
            let mut registered = self.tasks.lock().unwrap_or_else(|poisoned| {
                tracing::error!("application task registry lock poisoned; recovering");
                poisoned.into_inner()
            });
            std::mem::take(&mut *registered)
        };
        while !tasks.is_empty() {
            match tokio::time::timeout(TASK_SHUTDOWN_GRACE, tasks.join_next()).await {
                Ok(Some(Ok(()))) => {}
                Ok(Some(Err(error))) if error.is_cancelled() => {}
                Ok(Some(Err(error))) => {
                    tracing::warn!(
                        error = %sanitize_error_text(&error.to_string()),
                        "application task join failed during shutdown"
                    );
                }
                Ok(None) => break,
                Err(_) => {
                    tracing::warn!(
                        "application task did not stop within {:?}; aborting",
                        TASK_SHUTDOWN_GRACE
                    );
                    tasks.abort_all();
                    while tasks.join_next().await.is_some() {}
                    break;
                }
            }
        }
    }

    /// Bridge the synchronous Tauri exit callback to the async teardown path.
    /// Tauri normally invokes the callback while the app runtime is active;
    /// the fallback also handles a callback delivered from a plain host
    /// thread.
    pub(crate) fn teardown_blocking(&self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            let handle = self.runtime_handle.clone();
            tokio::task::block_in_place(|| handle.block_on(self.shutdown()));
        } else {
            self.runtime_handle.block_on(self.shutdown());
        }
    }
}

impl Drop for ApplicationRuntime {
    fn drop(&mut self) {
        // Drop is only a last-resort safety net; normal exits must call the
        // async shutdown path so domain resources are awaited and diagnostics
        // are preserved. Registered task handles are aborted as a last resort.
        self.shutdown_token.cancel();
        if let Ok(tasks) = self.tasks.get_mut() {
            tasks.abort_all();
        }
    }
}
