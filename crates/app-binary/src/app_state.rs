use crate::chatgpt_plan_auth::ConfigChatGptPlanAuthPort;
use crate::chatgpt_plan_login::ChatGptPlanLoginOwner;
use crate::commands::confirmation::AppConfirmationOwner;
use crate::config_runtime::apply_log_level_to_handles;
use crate::desktop::DesktopShell;
use crate::events::AppBootstrapEvent;
use crate::failed_transcriptions::FailedTranscriptionStore;
use crate::recording_lifecycle::RecordingLifecycleOwner;
use crate::router_media_builder::build_router_media;
use crate::runtime::{ApplicationRuntime, RuntimeServices};
use haven_agent::SessionSupervisor;
use haven_agent::{AgentLayer, MemoryService, PendingSessionRecovery};
#[cfg(test)]
use haven_common::config::InMemoryCredentialStore;
use haven_common::config::{ConfigLoader, ConfigService, CredentialStore, LogLevel};
use haven_input::InputPipeline;
use haven_memory::{MemoryPersistence, SessionStore};
use haven_platform::credentials::PlatformCredentialStore;
use haven_tools::ToolsFacade;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

struct ReloadLogLevelPort {
    handles: Vec<reload::Handle<EnvFilter, Registry>>,
}

impl haven_tools::LogLevelPort for ReloadLogLevelPort {
    fn set_level(&self, level: &LogLevel) -> anyhow::Result<()> {
        for handle in &self.handles {
            if let Err(error) = apply_log_level_to_handles(std::slice::from_ref(handle), level) {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to apply runtime log level"
                );
            }
        }
        Ok(())
    }
}

/// Cold-start progress exposed to the UI status chip.
/// `loading` while MCP/skills/audio prewarm finish in the background;
/// `ready` once that deferred work completes (or immediately when there is
/// nothing deferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapStatus {
    Loading,
    Ready,
}

#[derive(Clone)]
struct CleanupRoots {
    uploads: PathBuf,
    generated_media: PathBuf,
}

impl CleanupRoots {
    fn production() -> Self {
        Self {
            uploads: haven_common::config::default_uploads_root(),
            generated_media: haven_common::config::default_generated_media_root(),
        }
    }

    #[cfg(test)]
    fn isolated(data_root: &Path) -> Self {
        Self {
            uploads: data_root.join("uploads"),
            generated_media: data_root.join("media").join("generated"),
        }
    }
}

async fn run_cleanup_pass(
    executor: &SessionSupervisor,
    session_store: &SessionStore,
    roots: &CleanupRoots,
    registry: &std::sync::Arc<dyn haven_tools::ManagedAssetLifecyclePort>,
    retention_days: u32,
    context: &'static str,
) {
    if retention_days > 0 {
        match executor.delete_old_sessions(retention_days).await {
            Ok(n) if n > 0 => tracing::info!(
                cleanup = context,
                count = n,
                retention_days,
                "removed expired sessions"
            ),
            Ok(_) => {}
            Err(error) => tracing::warn!(
                cleanup = context,
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "session retention cleanup failed"
            ),
        }
    }

    match crate::commands::managed_media::cleanup_unreferenced_managed_media(
        roots.uploads.clone(),
        roots.generated_media.clone(),
        Arc::clone(registry),
        session_store,
    )
    .await
    {
        Ok(counts)
            if counts.removed_upload_batches > 0 || counts.removed_generated_media_files > 0 =>
        {
            tracing::info!(
                cleanup = context,
                uploads = counts.removed_upload_batches,
                generated = counts.removed_generated_media_files,
                "removed unreferenced managed media"
            );
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(
            cleanup = context,
            error = %haven_common::error::sanitize_error_text(&error),
            "managed media cleanup failed"
        ),
    }

    match crate::commands::managed_media::cleanup_stale_upload_staging(roots.uploads.clone()).await
    {
        Ok(n) if n > 0 => tracing::info!(
            cleanup = context,
            count = n,
            "removed stale upload staging directories"
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            cleanup = context,
            error = %haven_common::error::sanitize_error_text(&error),
            "stale upload staging cleanup failed"
        ),
    }
}

fn daily_cleanup_interval(period: std::time::Duration) -> tokio::time::Interval {
    let first_tick = tokio::time::Instant::now() + period;
    tokio::time::interval_at(first_tick, period)
}

pub struct AppState {
    pub(crate) runtime: Arc<ApplicationRuntime>,
    pub(crate) chatgpt_plan_login: ChatGptPlanLoginOwner,
    /// App-owned voice recording lifecycle and event identity. It is separate
    /// from timed `media.record` captures, which share the input pipeline but
    /// do not produce app recording events.
    pub(crate) recording_lifecycle: RecordingLifecycleOwner,
    /// LLM-backed ingress transcription usage waiting for the frontend to
    /// submit the transcript to its concrete session. The
    /// `rec-*` key is deliberately kept separate from durable `ses-*` ids.
    pub(crate) pending_recording_usage:
        Arc<std::sync::Mutex<HashMap<String, Vec<haven_llm::LlmCallUsage>>>>,
    /// Bounded durable source audio for explicit retry after STT failure.
    pub(crate) failed_transcriptions: FailedTranscriptionStore,
    /// Bound simultaneous provider work; overflow is retained for explicit
    /// retry instead of spawning an unbounded pile of audio tasks.
    pub(crate) recording_transcription_slots: Arc<tokio::sync::Semaphore>,
    /// True once deferred startup (MCP discover + skills scan + audio
    /// prewarm) has finished. The UI polls / listens so the status chip can
    /// show 加载中 → 就绪 without blocking window creation.
    bootstrap_ready: Arc<AtomicBool>,
    #[cfg(test)]
    fail_initial_pending_recovery_once: AtomicBool,
    #[cfg(test)]
    pending_recovery_retry_scheduled: Arc<AtomicBool>,
    /// Suppresses the global recording shortcut while the renderer is
    /// capturing a replacement key binding.
    pub(crate) hotkey_capture_active: Arc<AtomicBool>,
    pub(crate) app_confirmations: AppConfirmationOwner,
    /// Last config version whose complete Security settings phase succeeded.
    /// One-off permission edits layer on top of this baseline.
    pub(crate) last_fully_applied_security_config_version: AtomicU64,
}

impl AppState {
    pub async fn new(
        db_path: &std::path::Path,
        filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
        config_loader: ConfigLoader,
        file_logging_enabled: bool,
    ) -> anyhow::Result<Self> {
        Self::new_with_cleanup_roots(
            db_path,
            filter_handles,
            config_loader,
            file_logging_enabled,
            CleanupRoots::production(),
            Arc::new(PlatformCredentialStore),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn new_for_test(
        db_path: &Path,
        filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
        config_loader: ConfigLoader,
        test_data_root: &Path,
    ) -> anyhow::Result<Self> {
        let file_logging_enabled = config_loader.config().log.file_enabled;
        Self::new_with_cleanup_roots(
            db_path,
            filter_handles,
            config_loader,
            file_logging_enabled,
            CleanupRoots::isolated(test_data_root),
            Arc::new(InMemoryCredentialStore::default()),
        )
        .await
    }

    async fn new_with_cleanup_roots(
        db_path: &Path,
        filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
        config_loader: ConfigLoader,
        file_logging_enabled: bool,
        cleanup_roots: CleanupRoots,
        credential_store: Arc<dyn CredentialStore>,
    ) -> anyhow::Result<Self> {
        let t0 = std::time::Instant::now();
        let memory_persistence = MemoryPersistence::open(db_path)?;
        let session_store = memory_persistence.session_store();
        // Keep the supervisor's live event channel separate from the app's
        // command/read store, as it was before the constructor accepted stores.
        let supervisor_session_store = memory_persistence.session_store();
        let memory_stores = memory_persistence.memory_stores();
        let memory_fact_store = memory_stores.fact_store();
        let memory_fact_proposal_store = memory_stores.fact_proposal_store();
        tracing::debug!(
            "AppState::new phase=db elapsed={}ms",
            t0.elapsed().as_millis()
        );

        let config_service = Arc::new(ConfigService::new_with_credential_store(
            config_loader,
            credential_store,
        )?);
        let chatgpt_plan_auth = Arc::new(ConfigChatGptPlanAuthPort::new(config_service.clone()));
        let config_apply_gate = Arc::new(tokio::sync::Mutex::new(()));
        let initial_config = config_service.snapshot()?;
        let last_fully_applied_security_config_version = initial_config.version;
        let config_version = initial_config.version;
        let cfg = initial_config.config;
        let context_limits = cfg.context_limits.clone();
        let context_limits_clone = context_limits.clone();
        let tools = Arc::new(ToolsFacade::new());
        let tool_services = tools.share_services();
        let agent_tool_ports = crate::agent_tool_adapters::agent_tool_ports_from_capabilities(
            Arc::clone(&tools),
            Arc::clone(&tool_services.authorization),
            Arc::clone(&tool_services.skills),
            Arc::clone(&tool_services.tool_run_agent),
            Arc::clone(&tool_services.schedule_agent),
        );
        let mcp_caller: Arc<dyn haven_llm::McpToolCaller> =
            Arc::clone(&tool_services.mcp_tool_caller);
        let router_media_build =
            build_router_media(&cfg, Some(mcp_caller), Some(chatgpt_plan_auth.clone()));
        let router = Arc::clone(&router_media_build.router);

        // Dedicated media providers degrade independently at startup, then
        // join the same immutable model/media generation as the router.
        let stt_client = match router_media_build.stt_client {
            Ok(client) => client,
            Err(_) => {
                tracing::warn!(
                    failure_kind = "stt_client_build",
                    "STT client build failed, transcription disabled"
                );
                None
            }
        };
        let tts_client = match router_media_build.tts_client {
            Ok(client) => client,
            Err(_) => {
                tracing::warn!(
                    failure_kind = "tts_client_build",
                    "TTS client build failed, TTS disabled"
                );
                None
            }
        };
        let ocr_client = match router_media_build.ocr_client {
            Ok(client) => client,
            Err(_) => {
                tracing::warn!(
                    failure_kind = "ocr_client_build",
                    "OCR client build failed, OCR disabled"
                );
                None
            }
        };
        let image_gen_client = match router_media_build.image_gen_client {
            Ok(client) => client,
            Err(_) => {
                tracing::warn!(
                    failure_kind = "image_gen_client_build",
                    "image generation client build failed, image generation disabled"
                );
                None
            }
        };
        let (model_media_runtime, model_media_publisher) = haven_llm::ModelMediaRuntime::new(
            haven_llm::ModelMediaSnapshot::new(
                config_version,
                router.clone(),
                router_media_build.media,
            )
            .with_clients(stt_client, ocr_client, image_gen_client, tts_client),
        );
        let max_steps_per_run = cfg.session.max_steps_per_run;
        let max_steps_per_session = cfg.session.max_steps_per_session;
        let session_prompt_history_limit = cfg.session.prompt_history_limit;

        let executor = Arc::new(SessionSupervisor::new(
            supervisor_session_store,
            agent_tool_ports.session_ports(),
            cfg.session.max_concurrent,
        ));

        let memory_service = Arc::new(MemoryService::new_with_model_media_runtime(
            memory_stores,
            model_media_runtime.clone(),
            context_limits.embedding_chunk_size,
        ));
        let agent_startup = AgentLayer::build(
            memory_service,
            executor.clone(),
            agent_tool_ports,
            router.clone(),
            max_steps_per_run,
            session_prompt_history_limit,
            context_limits,
        );
        let agent = Arc::new(agent_startup.agent);
        agent.set_fact_inference_enabled(cfg.memory.fact_inference_enabled);
        let memory_startup = agent_startup.memory_startup;
        agent.set_media_strategy(cfg.media.input_strategy)?;
        agent.set_max_steps_per_session(max_steps_per_session)?;

        let input_pipeline = Arc::new(InputPipeline::new());
        input_pipeline
            .set_ring_buffer_capacity_secs(context_limits_clone.input_ring_buffer_secs)?;
        let shell = Arc::new(DesktopShell::new());
        let runtime = Arc::new(ApplicationRuntime::new(RuntimeServices {
            session_store: session_store.clone(),
            memory_fact_store: memory_fact_store.clone(),
            memory_fact_proposal_store: memory_fact_proposal_store.clone(),
            tools: tools.clone(),
            tool_services,
            executor: executor.clone(),
            agent: agent.clone(),
            memory_startup,
            input_pipeline: input_pipeline.clone(),
            shell: shell.clone(),
            log_filter_handles: filter_handles.clone(),
            config_service: config_service.clone(),
            chatgpt_plan_auth: chatgpt_plan_auth.clone(),
            config_apply_gate: config_apply_gate.clone(),
            model_media_publisher,
        }));

        // ApplicationRuntime owns task registration, cancellation, and join;
        // MemoryStartup retains the six-hour schedule policy.
        {
            let memory_startup = runtime.memory_startup.clone();
            runtime.spawn_with_child_token("memory-maintenance", move |cancel| async move {
                memory_startup
                    .run_maintenance_until_cancelled(&cancel)
                    .await;
            });
        }

        // The previous process is gone, so any session left `running` can
        // never resume — mark it errored immediately so the user sees the
        // interrupted state and can retry via the continue flow. This runs
        // before any UI fetches the session list.
        {
            let session_store = session_store.clone();
            runtime.spawn("finalize-orphaned-sessions", async move {
                match session_store.finalize_orphaned_running_sessions().await {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            "finalized {} orphaned running session(s) from previous run",
                            n
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "failed to finalize orphaned running sessions"
                        );
                    }
                }
            });
        }

        // Run one initial cleanup pass, then repeat it daily. Keeping the pass
        // in one worker prevents the startup and interval paths from racing to
        // delete the same session media. Retention-disabled runs still sweep
        // orphaned media, and staging keeps its independent TTL.
        let retention_days = cfg.session.history_retention_days;
        let cleanup_executor = executor.clone();
        let cleanup_store = session_store.clone();
        let cleanup_roots = cleanup_roots.clone();
        let cleanup_registry = Arc::clone(&runtime.services.managed_assets);
        runtime.spawn_with_child_token("daily-cleanup", move |cancel| async move {
            let period = std::time::Duration::from_secs(86400);
            run_cleanup_pass(
                &cleanup_executor,
                &cleanup_store,
                &cleanup_roots,
                &cleanup_registry,
                retention_days,
                "startup",
            )
            .await;
            let mut interval = daily_cleanup_interval(period);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {}
                }
                run_cleanup_pass(
                    &cleanup_executor,
                    &cleanup_store,
                    &cleanup_roots,
                    &cleanup_registry,
                    retention_days,
                    "daily",
                )
                .await;
            }
        });

        // Pre-warm LLM HTTP pools in the background so the first chat request
        // does not pay TCP+TLS. The session dispatcher is started from
        // `spawn_background_init` as soon as the event bus is installed; only
        // durable pending-session recovery waits for the MCP/Skills catalog.
        let router_warm = router.clone();
        runtime.spawn("llm-prewarm", async move {
            // Bound the prewarm: a slow/unreachable endpoint's health check
            // must not hold the runtime. On timeout the endpoint fails fast
            // on its first real request instead.
            if tokio::time::timeout(std::time::Duration::from_secs(2), router_warm.prewarm_all())
                .await
                .is_err()
            {
                tracing::debug!("LLM HTTP prewarm timed out; first request will warm lazily");
            }
        });
        tracing::debug!(
            "AppState::new phase=agent elapsed={}ms",
            t0.elapsed().as_millis()
        );

        // Wire the five typed admin operations: the assistant can read status,
        // change typed config, toggle skills/tools/MCP servers, tail logs, and
        // switch the runtime log level (via the tracing reload handles).
        let log_path = Some(
            cfg.log
                .file_path
                .clone()
                .unwrap_or_else(haven_common::config::LogConfig::default_log_path),
        );
        let log_level = Some(Arc::new(ReloadLogLevelPort {
            handles: filter_handles.clone(),
        }) as Arc<dyn haven_tools::LogLevelPort>);
        let admin_context = haven_tools::AdminContext {
            config_service: Some(config_service.clone()),
            config_apply_gate: Some(config_apply_gate),
            session_store: Some(session_store),
            session_working_document: Some(
                crate::agent_tool_adapters::session_working_document_port(executor.clone()),
            ),
            memory_facts: Some(memory_fact_store),
            model_media_runtime: Some(model_media_runtime.clone()),
            log_path,
            file_logging_enabled,
            log_level,
            // The admin tool's tool_enable/tool_disable ops apply the runtime
            // change through the running ToolsFacade after persisting config.
            tool_control: Some(tools.tool_control_port()),
        };

        // Single catalog rebuild for all startup wiring (messaging, memory, settings / shell /
        // limits / router / audio pipeline / admin surface). Previously each
        // setter rebuilt the catalog and delayed window creation.
        tools
            .wire_startup(haven_tools::StartupWiring {
                tool_run_store: Some(memory_persistence.tool_run_store()),
                schedule_store: Some(memory_persistence.schedule_store()),
                tool_settings: cfg.tool_settings.clone(),
                default_shell: cfg.default_shell,
                context_limits: context_limits_clone,
                security: cfg.security.clone(),
                model_media_runtime: model_media_runtime.clone(),
                input_pipeline: Some(input_pipeline.clone()),
                admin_context,
                messaging_runtime: agent.clone(),
                memory_recall: agent.clone(),
            })
            .await?;

        tracing::debug!(
            "AppState::new phase=done elapsed={}ms",
            t0.elapsed().as_millis()
        );

        Ok(Self {
            runtime,
            chatgpt_plan_login: ChatGptPlanLoginOwner::default(),
            recording_lifecycle: RecordingLifecycleOwner::default(),
            pending_recording_usage: Arc::new(std::sync::Mutex::new(HashMap::new())),
            failed_transcriptions: FailedTranscriptionStore::production(),
            recording_transcription_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            bootstrap_ready: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            fail_initial_pending_recovery_once: AtomicBool::new(false),
            #[cfg(test)]
            pending_recovery_retry_scheduled: Arc::new(AtomicBool::new(false)),
            hotkey_capture_active: Arc::new(AtomicBool::new(false)),
            app_confirmations: AppConfirmationOwner::default(),
            last_fully_applied_security_config_version: AtomicU64::new(
                last_fully_applied_security_config_version,
            ),
        })
    }

    pub fn bootstrap_status(&self) -> BootstrapStatus {
        if self.bootstrap_ready.load(Ordering::Acquire) {
            BootstrapStatus::Ready
        } else {
            BootstrapStatus::Loading
        }
    }

    /// Run MCP discover + skills scan + audio prewarm off the critical path
    /// that blocks window creation. The dispatcher starts before that work so
    /// fresh conversations do not wait for the catalog; durable pending
    /// sessions are reloaded after the catalog is ready.
    /// Emits typed `app:bootstrap` (`loading` / `ready`) payloads so the status
    /// chip can track progress even when the frontend mounts mid-flight.
    pub fn spawn_background_init<F>(&self, emit: F)
    where
        F: Fn(AppBootstrapEvent) + Send + Sync + 'static,
    {
        let tools = self.runtime.tools.clone();
        let input_pipeline = self.runtime.input_pipeline.clone();
        let agent = self.runtime.agent.clone();
        let runtime = self.runtime.clone();
        let bootstrap_ready = self.bootstrap_ready.clone();
        #[cfg(test)]
        let fail_initial_pending_recovery_once = self
            .fail_initial_pending_recovery_once
            .swap(false, Ordering::AcqRel);
        #[cfg(test)]
        let pending_recovery_retry_scheduled = self.pending_recovery_retry_scheduled.clone();
        let cfg = match self.runtime.config_service.snapshot() {
            Ok(snapshot) => snapshot.config,
            Err(error) => {
                tracing::error!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "cannot read config for background init"
                );
                return;
            }
        };
        let mcp_servers = cfg.mcp_servers.clone();
        let mcp_discovery = cfg.mcp_discovery.clone();
        let skills_cfg_root = cfg.skills.root.clone();
        let skills_cfg_enabled = cfg.skills.enabled.clone();
        let skills_catalog = Arc::clone(&runtime.services.skills);

        emit(AppBootstrapEvent {
            status: BootstrapStatus::Loading,
        });

        let bootstrap_runtime = runtime.clone();
        runtime
            .spawn_with_child_token("app-bootstrap", move |cancel| async move {
            // Audio engine + VAD worker: first recording must not pay spawn
            // latency, but window creation should not wait for it either.
            input_pipeline.prewarm().await;

            // Start new conversations immediately. Recovery of sessions left
            // Pending by a previous process is deferred until the catalog is
            // ready below, so restart semantics do not race an empty catalog.
            bootstrap_runtime.start_agent_after_memory_ready(
                PendingSessionRecovery::DeferUntilCatalogReady,
            );

            // MCP discover + skills scan run behind an explicit deadline so a
            // hung server cannot block session resume forever. This task is
            // directly owned by ApplicationRuntime; no detached child join
            // handle survives shutdown.
            let catalog_finished = tokio::select! {
                _ = cancel.cancelled() => return,
                result = tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    async {
                        tools.discover_all(&mcp_servers, &mcp_discovery).await;
                        if skills_catalog
                            .set_config(skills_cfg_root, skills_cfg_enabled)
                            .await
                            .is_err()
                        {
                            tracing::warn!(
                                failure_kind = "skill_registry_initial_scan",
                                "skill registry initial scan failed"
                            );
                        }
                        if tools.rebuild_catalog().await.is_err() {
                            tracing::warn!(
                                failure_kind = "tool_catalog_rebuild",
                                "initial tool catalog rebuild failed"
                            );
                        }
                    }
                ) => {
                    if result.is_err() {
                        tracing::warn!(
                            "MCP/skills bootstrap timed out after 10s; starting session dispatcher anyway"
                        );
                        false
                    } else {
                        true
                    }
                }
            };

            if cancel.is_cancelled() {
                return;
            }

            if !catalog_finished {
                // The timed operation was dropped above. Keep the branch
                // explicit so the readiness transition remains observable.
                tracing::debug!(
                    "continuing bootstrap after the MCP/skills catalog deadline"
                );
            }

            if cancel.is_cancelled() {
                return;
            }

            let (recovery_result_tx, recovery_result_rx) = tokio::sync::oneshot::channel();
            let recovery_agent = agent.clone();
            let recovery_scheduled = bootstrap_runtime.spawn_cancellable_with_child_token(
                "pending-session-initial-recovery",
                move |_recovery_cancel| async move {
                    #[cfg(test)]
                    let result = if fail_initial_pending_recovery_once {
                        Err(anyhow::anyhow!("injected initial pending recovery failure"))
                    } else {
                        recovery_agent.recover_pending_sessions().await
                    };
                    #[cfg(not(test))]
                    let result = recovery_agent.recover_pending_sessions().await;
                    let _ = recovery_result_tx.send(result);
                },
            );
            if recovery_scheduled {
                match recovery_result_rx.await {
                    Ok(Ok(reloaded)) if reloaded > 0 => tracing::info!(
                        "deferred dispatcher recovery reloaded {} pending session(s)",
                        reloaded
                    ),
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => {
                        tracing::error!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "deferred dispatcher recovery failed; scheduling a cancellable retry"
                        );
                        let scheduled = schedule_pending_session_recovery_retry(
                            bootstrap_runtime.as_ref(),
                            agent.clone(),
                            &cancel,
                        );
                        #[cfg(test)]
                        pending_recovery_retry_scheduled.store(scheduled, Ordering::Release);
                        #[cfg(not(test))]
                        let _ = scheduled;
                    }
                    Err(error) => {
                        tracing::error!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "initial pending session recovery task ended without a result; scheduling a retry"
                        );
                        let scheduled = schedule_pending_session_recovery_retry(
                            bootstrap_runtime.as_ref(),
                            agent.clone(),
                            &cancel,
                        );
                        #[cfg(test)]
                        pending_recovery_retry_scheduled.store(scheduled, Ordering::Release);
                        #[cfg(not(test))]
                        let _ = scheduled;
                    }
                }
            } else if !cancel.is_cancelled() {
                tracing::error!(
                    "application runtime rejected the initial pending session recovery task"
                );
            }

            if cancel.is_cancelled() {
                return;
            }

            bootstrap_ready.store(true, Ordering::Release);
            emit(AppBootstrapEvent {
                status: BootstrapStatus::Ready,
            });
            tracing::info!("app bootstrap ready (MCP/skills/audio prewarm finished)");
            });
    }
}

fn schedule_pending_session_recovery_retry(
    runtime: &ApplicationRuntime,
    agent: Arc<AgentLayer>,
    bootstrap_cancellation: &tokio_util::sync::CancellationToken,
) -> bool {
    let scheduled = runtime.spawn_cancellable_with_child_token(
        "pending-session-recovery",
        move |recovery_cancel| async move {
            match agent
                .retry_pending_session_recovery_after_failure(recovery_cancel)
                .await
            {
                Some(reloaded) => tracing::info!(
                    reloaded,
                    "deferred pending session recovery completed after retry"
                ),
                None => tracing::debug!(
                    "deferred pending session recovery retry stopped by cancellation"
                ),
            }
        },
    );
    if !scheduled && !bootstrap_cancellation.is_cancelled() {
        tracing::error!("application runtime rejected the pending session recovery retry task");
    }
    scheduled
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::McpServerConfig;
    use tempfile::tempdir;

    #[tokio::test]
    async fn daily_cleanup_interval_delays_first_tick_by_one_period() {
        let period = std::time::Duration::from_millis(100);
        let mut interval = daily_cleanup_interval(period);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), interval.tick())
                .await
                .is_err(),
            "the first periodic cleanup tick must not run at startup"
        );
        tokio::time::timeout(
            period + std::time::Duration::from_millis(100),
            interval.tick(),
        )
        .await
        .expect("the first daily tick should arrive after its period");
    }

    #[tokio::test]
    async fn new_initializes_core_components_with_default_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let db_path = dir.path().join("test.db");

        // Missing config file → created with defaults.
        let loader = ConfigLoader::load_from(&cfg_path).unwrap();
        let state = AppState::new_for_test(&db_path, vec![], loader, dir.path())
            .await
            .unwrap();

        // Builtin tools are registered synchronously before new() returns.
        assert!(state.runtime.tools.get_tool("files.read").await.is_some());
        assert!(state.runtime.tools.get_tool("shell").await.is_some());

        // The default config is loaded and accessible via the mutex.
        let cfg = state.runtime.config_service.snapshot().unwrap().config;
        assert!(cfg.session.max_steps_per_run > 0);
        assert_eq!(cfg.media.stt.provider_name, "llm");
        assert_eq!(state.bootstrap_status(), BootstrapStatus::Loading);
    }

    #[tokio::test]
    async fn deferred_pending_recovery_failure_registers_retry_and_keeps_bootstrap_ready() {
        let dir = tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let state = AppState::new_for_test(&dir.path().join("test.db"), vec![], loader, dir.path())
            .await
            .unwrap();
        state
            .fail_initial_pending_recovery_once
            .store(true, Ordering::Release);
        let retry_scheduled = state.pending_recovery_retry_scheduled.clone();
        let runtime = state.runtime.clone();

        state.spawn_background_init(|_| {});

        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while state.bootstrap_status() != BootstrapStatus::Ready
                || !retry_scheduled.load(Ordering::Acquire)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("bootstrap should become Ready after registering the runtime-owned recovery retry");

        assert_eq!(state.bootstrap_status(), BootstrapStatus::Ready);
        runtime.shutdown().await;
        assert_eq!(
            runtime.task_count_for_test(),
            0,
            "runtime shutdown must join bootstrap and recovery tasks"
        );
    }

    #[tokio::test]
    async fn completed_runtime_tasks_are_reaped_without_detaching_live_work() {
        struct DropProbe(Arc<AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let dir = tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let state = AppState::new_for_test(&dir.path().join("test.db"), vec![], loader, dir.path())
            .await
            .unwrap();
        let runtime = state.runtime.clone();
        let existing_tasks = runtime.task_count_for_test();
        let completed = Arc::new(tokio::sync::Notify::new());

        for _ in 0..128 {
            let task_completed = Arc::clone(&completed);
            let completion = completed.notified();
            assert!(runtime.spawn("short-lived-callback", async move {
                task_completed.notify_one();
            }));
            completion.await;
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_task = Arc::clone(&dropped);
        let started = Arc::new(tokio::sync::Notify::new());
        let task_started = Arc::clone(&started);
        assert!(runtime.spawn("live-callback", async move {
            let _probe = DropProbe(dropped_task);
            task_started.notify_one();
            std::future::pending::<()>().await;
        }));
        started.notified().await;

        let registered_tasks = runtime.task_count_for_test();
        assert!(
            (1..=existing_tasks + 1).contains(&registered_tasks),
            "completed callback handles must be reaped before the next registration; registered {registered_tasks}, baseline {existing_tasks}"
        );
        runtime.shutdown().await;
        assert!(dropped.load(Ordering::Acquire));
        assert_eq!(runtime.task_count_for_test(), 0);
    }

    #[tokio::test]
    async fn runtime_shutdown_cancels_owned_tasks_and_is_idempotent() {
        struct DropProbe(Arc<AtomicBool>);

        impl Drop for DropProbe {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let dir = tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let db_path = dir.path().join("test.db");
        let state = AppState::new_for_test(&db_path, vec![], loader, dir.path())
            .await
            .unwrap();
        let runtime = state.runtime.clone();

        let session = runtime
            .executor
            .create_session("app-owned memory runtime")
            .await
            .unwrap();
        let usage_event = runtime
            .session_store
            .append(&session.session_id, "usage_recorded", "{}", None, None)
            .unwrap();

        // The UI maintenance command still calls the Agent's single-pass
        // worker entry point rather than the periodic schedule.
        runtime.agent.run_memory_maintenance().await.unwrap();

        assert!(
            runtime.start_agent_after_memory_ready(PendingSessionRecovery::DeferUntilCatalogReady,)
        );
        let startup_deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let cursor = runtime
                .session_store
                .memory_event_cursor_optional_cancellable(
                    &session.session_id,
                    tokio_util::sync::CancellationToken::new(),
                )
                .await
                .unwrap();
            if cursor == Some(usage_event.sequence) {
                break;
            }
            assert!(
                std::time::Instant::now() < startup_deadline,
                "memory live consumer did not durably advance the session cursor"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_task = dropped.clone();

        assert!(runtime.spawn("runtime-test-task", async move {
            let _probe = DropProbe(dropped_task);
            std::future::pending::<()>().await;
        }));

        runtime
            .tools
            .load_mcp_from_config(&[McpServerConfig {
                name: "disabled-shutdown-test".into(),
                enabled: false,
                ..McpServerConfig::default()
            }])
            .await;

        let keep_reading = Arc::new(AtomicBool::new(true));
        let read_started = Arc::new(tokio::sync::Notify::new());
        let reader_tools = Arc::clone(&runtime.tools);
        let reader_flag = Arc::clone(&keep_reading);
        let reader_started = Arc::clone(&read_started);
        let reader = tokio::spawn(async move {
            let mut signalled = false;
            loop {
                let _ = reader_tools.runtime_capabilities().await;
                assert!(reader_tools.build_mcp_index().await.is_empty());
                if !signalled {
                    reader_started.notify_one();
                    signalled = true;
                }
                if !reader_flag.load(Ordering::Acquire) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        tokio::task::yield_now().await;
        read_started.notified().await;
        runtime.shutdown().await;
        keep_reading.store(false, Ordering::Release);
        reader
            .await
            .expect("capability reads must survive shutdown");

        assert!(runtime.cancellation_token().is_cancelled());
        assert!(
            runtime.task_count_for_test() == 0,
            "shutdown must join the memory startup and live consumer tasks"
        );
        assert!(dropped.load(Ordering::Acquire));
        assert!(!runtime.spawn("late-task", async {}));
        let after_shutdown = runtime.tools.runtime_capabilities().await;
        assert!(after_shutdown.recording);
        assert!(matches!(
            after_shutdown.web_search,
            haven_tools::WebSearchAvailability::Unavailable
        ));

        // The second call is intentionally a no-op: teardown is safe to call
        // from both an exit hook and an owning test fixture.
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn runtime_shutdown_preserves_session_owned_schedule_plans() {
        let dir = tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let db_path = dir.path().join("test.db");
        let state = AppState::new_for_test(&db_path, vec![], loader, dir.path())
            .await
            .unwrap();
        let session = state
            .runtime
            .executor
            .create_session("scheduled owner")
            .await
            .unwrap();
        let services = state.runtime.tools.share_services();
        let plan = services
            .schedule_management
            .create(haven_memory::SchedulePlanInput {
                owner_session_id: session.session_id.clone(),
                schedule_type: haven_memory::ScheduleType::Once,
                title: "Keep after exit".into(),
                body: "restore me".into(),
                mode: haven_common::schedule::ScheduleMode::Continue,
                due_at: Some(
                    (chrono::Utc::now() + chrono::Duration::hours(1))
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                ),
                watch_tool_run_id: None,
                tool_name: None,
                tool_args: None,
                prompt: Some("continue later".into()),
                interval_seconds: None,
            })
            .await
            .unwrap();

        state.runtime.shutdown().await;

        let plans = services
            .schedule_management
            .list_for_session(&session.session_id)
            .await
            .unwrap();
        assert!(
            plans
                .iter()
                .any(|row| row.schedule_id == plan.schedule_id && row.enabled)
        );
    }

    #[tokio::test]
    async fn new_uses_existing_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let db_path = dir.path().join("test.db");
        let loader = ConfigLoader::load_from(&cfg_path).unwrap();
        let state = AppState::new_for_test(&db_path, vec![], loader, dir.path())
            .await
            .unwrap();
        let mut config = state.runtime.config_service.snapshot().unwrap().config;
        config.session.max_steps_per_run = 42;
        state
            .runtime
            .config_service
            .edit(|current| {
                *current = config;
                Ok(())
            })
            .unwrap();
        drop(state);

        let loader2 = ConfigLoader::load_from(&cfg_path).unwrap();
        let state2 = AppState::new_for_test(&db_path, vec![], loader2, dir.path())
            .await
            .unwrap();
        assert_eq!(
            state2
                .runtime
                .config_service
                .snapshot()
                .unwrap()
                .config
                .session
                .max_steps_per_run,
            42
        );
    }
}
