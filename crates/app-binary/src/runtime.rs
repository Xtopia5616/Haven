//! Application-wide service ownership and lifecycle control.
//!
//! `ApplicationRuntime` is the composition-root owner for long-lived service
//! handles and app-scoped asynchronous work.  Components may still own their
//! domain workers (for example an action process or an MCP monitor), but the
//! application gives them one cancellation boundary and tears them down in a
//! deterministic order.

use crate::desktop::DesktopShell;
use haven_agent::{AgentLayer, SessionSupervisor};
use haven_common::config::ConfigService;
use haven_input::InputPipeline;
use haven_memory::Database;
use haven_tools::ToolsManager;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

/// The application composition root.
///
/// The service fields are deliberately kept here rather than in several
/// independent state containers. `AppState` dereferences to this type for
/// command compatibility while retaining only Tauri-specific transient state
/// of its own.
pub struct ApplicationRuntime {
    pub(crate) db: Arc<Database>,
    pub(crate) tools: Arc<ToolsManager>,
    pub(crate) executor: Arc<SessionSupervisor>,
    pub(crate) agent: Arc<AgentLayer>,
    pub(crate) pipeline: Arc<InputPipeline>,
    pub(crate) shell: Arc<DesktopShell>,
    pub(crate) log_filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    pub(crate) config_service: Arc<ConfigService>,
    shutdown_token: CancellationToken,
    shutting_down: AtomicBool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    runtime_handle: tokio::runtime::Handle,
}

const TASK_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

pub(crate) struct RuntimeServices {
    pub(crate) db: Arc<Database>,
    pub(crate) tools: Arc<ToolsManager>,
    pub(crate) executor: Arc<SessionSupervisor>,
    pub(crate) agent: Arc<AgentLayer>,
    pub(crate) pipeline: Arc<InputPipeline>,
    pub(crate) shell: Arc<DesktopShell>,
    pub(crate) log_filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    pub(crate) config_service: Arc<ConfigService>,
}

impl ApplicationRuntime {
    pub(crate) fn new(services: RuntimeServices) -> Self {
        Self {
            db: services.db,
            tools: services.tools,
            executor: services.executor,
            agent: services.agent,
            pipeline: services.pipeline,
            shell: services.shell,
            log_filter_handles: services.log_filter_handles,
            config_service: services.config_service,
            shutdown_token: CancellationToken::new(),
            shutting_down: AtomicBool::new(false),
            tasks: Mutex::new(Vec::new()),
            runtime_handle: tokio::runtime::Handle::current(),
        }
    }

    /// Return the root token shared by app-scoped workers and domain startup
    /// consumers. Child tokens are handed to individual tasks so a task can
    /// also pass cancellation to a nested operation without exposing the
    /// runtime itself.
    pub(crate) fn cancellation_token(&self) -> CancellationToken {
        self.shutdown_token.clone()
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

        let mut tasks = self.tasks.lock().unwrap_or_else(|poisoned| {
            tracing::error!("application task registry lock poisoned; recovering");
            poisoned.into_inner()
        });
        if self.shutting_down.load(Ordering::Acquire) {
            return false;
        }
        tasks.push(self.runtime_handle.spawn(wrapped));
        true
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

        // Stop producers before consumers and resource owners. In particular,
        // this prevents a final recording or session run from starting while
        // its backing process/network resources are being torn down.
        if let Err(error) = self.pipeline.shutdown().await {
            tracing::warn!(error = %error, "input pipeline shutdown failed");
        }
        self.executor.clear_all_sessions().await;
        self.tools.action_service().shutdown().await;
        self.tools.mcp_manager().shutdown_all().await;

        let tasks = {
            let mut registered = self.tasks.lock().unwrap_or_else(|poisoned| {
                tracing::error!("application task registry lock poisoned; recovering");
                poisoned.into_inner()
            });
            std::mem::take(&mut *registered)
        };
        for mut task in tasks {
            match tokio::time::timeout(TASK_SHUTDOWN_GRACE, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => {
                    tracing::warn!(error = %error, "application task join failed during shutdown");
                }
                Err(_) => {
                    tracing::warn!(
                        "application task did not stop within {:?}; aborting",
                        TASK_SHUTDOWN_GRACE
                    );
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }

    /// Explicit teardown entry point used by setup-failure and test paths.
    /// Keeping it as an alias makes the lifecycle boundary discoverable without
    /// introducing a second cleanup implementation.
    pub(crate) async fn teardown(&self) {
        self.shutdown().await;
    }

    /// Bridge the synchronous Tauri exit callback to the async teardown path.
    /// Tauri normally invokes the callback while the app runtime is active;
    /// the fallback also handles a callback delivered from a plain host
    /// thread.
    pub(crate) fn teardown_blocking(&self) {
        if tokio::runtime::Handle::try_current().is_ok() {
            let handle = self.runtime_handle.clone();
            tokio::task::block_in_place(|| handle.block_on(self.teardown()));
        } else {
            self.runtime_handle.block_on(self.teardown());
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
            for task in tasks {
                task.abort();
            }
        }
    }
}
