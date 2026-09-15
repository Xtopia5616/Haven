use crate::desktop::DesktopShell;
use crate::events::AppBootstrapEvent;
use crate::runtime::{ApplicationRuntime, RuntimeServices};
use haven_agent::AgentLayer;
use haven_agent::SessionSupervisor;
use haven_common::config::{ConfigLoader, ConfigService, LogLevel};
use haven_input::InputPipeline;
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use haven_memory::Database;
use haven_tools::ToolsManager;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

struct ReloadLogLevelPort {
    handles: Vec<reload::Handle<EnvFilter, Registry>>,
}

impl haven_tools::LogLevelPort for ReloadLogLevelPort {
    fn set_level(&self, level: &LogLevel) -> anyhow::Result<()> {
        for handle in &self.handles {
            if let Err(error) = handle.modify(|filter| {
                *filter = EnvFilter::new(format!("haven={}", level.as_str()));
            }) {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    Loading,
    Ready,
}

/// A renderer-triggered MCP/skill invocation waiting in the same confirmation
/// queue as agent actions. Raw arguments stay backend-only until the request is
/// resolved and are never part of an IPC error payload.
pub(crate) enum UiConfirmationAction {
    Mcp {
        client: String,
        tool: String,
        args: serde_json::Value,
    },
    Skill {
        name: String,
        params: serde_json::Value,
    },
    Admin {
        request: Box<haven_tools::AdminRequest>,
    },
}

pub(crate) struct UiConfirmationPending {
    pub session_id: String,
    pub tool_name: String,
    pub permission_key: String,
    pub risk_level: haven_common::types::RiskLevel,
    pub summary: String,
    pub receipt: haven_tools::ConfirmationReceipt,
    pub action: UiConfirmationAction,
}

impl BootstrapStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
        }
    }
}

pub struct AppState {
    pub(crate) runtime: Arc<ApplicationRuntime>,
    /// The `rec-{uuid}` id of the in-flight voice recording. Set when a
    /// recording starts (button or hotkey), consumed by
    /// `finalize_transcription`, and shared by every event of the same
    /// recording (`recording:started` / `transcription:*` events) so the
    /// frontend can correlate them by id.
    pub recording_session: Arc<std::sync::Mutex<Option<haven_common::types::SessionId>>>,
    /// LLM-backed ingress transcription usage waiting for the frontend to
    /// submit the transcript to its concrete conversation session. The
    /// `rec-*` key is deliberately kept separate from durable `ses-*` ids.
    pub(crate) pending_recording_usage:
        Arc<std::sync::Mutex<HashMap<String, Vec<haven_llm::LlmCallUsage>>>>,
    /// True once deferred startup (MCP discover + skills scan + audio
    /// prewarm) has finished. The UI polls / listens so the status chip can
    /// show 加载中 → 就绪 without blocking window creation.
    bootstrap_ready: Arc<AtomicBool>,
    pub(crate) ui_confirmations: Arc<tokio::sync::Mutex<HashMap<String, UiConfirmationPending>>>,
}

impl Deref for AppState {
    type Target = ApplicationRuntime;

    fn deref(&self) -> &Self::Target {
        &self.runtime
    }
}

impl AppState {
    pub async fn new(
        db_path: &std::path::Path,
        filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
        config_loader: ConfigLoader,
    ) -> anyhow::Result<Self> {
        let t0 = std::time::Instant::now();
        let db = Arc::new(Database::open(db_path)?);
        tracing::debug!(
            "AppState::new phase=db elapsed={}ms",
            t0.elapsed().as_millis()
        );

        let config_service = Arc::new(ConfigService::new(config_loader));
        let cfg = config_service.snapshot()?.config;
        let context_limits = cfg.context_limits.clone();
        let context_limits_clone = context_limits.clone();
        let llm_config = cfg.llm.materialize(
            Some(context_limits.max_response_tokens),
            Some(context_limits.reasoning_echo_max_chars),
        );
        let router = Arc::new(LlmRouter::with_default_context_window(
            llm_config,
            context_limits.default_context_window,
        ));
        let max_steps = cfg.session.max_steps;
        let session_max_steps = cfg.session.session_max_steps;
        let conversation_window_size = cfg.memory.session_window_size;

        let tools = Arc::new(ToolsManager::new());

        let executor = Arc::new(SessionSupervisor::new(
            db.clone(),
            tools.clone(),
            cfg.session.max_concurrent.max(1),
        ));

        let agent = Arc::new(AgentLayer::new(
            db.clone(),
            executor.clone(),
            router.clone(),
            max_steps,
            conversation_window_size,
            context_limits,
        ));
        agent.set_media_strategy(cfg.media.input_strategy);
        agent.set_session_max_steps(session_max_steps);

        // Bind one typed messaging runtime. It supplies both the in-process
        // SessionActor mailbox and peer lifecycle operations, so the catalog
        // never needs a mutable spawn/controller callback pair.
        tools.bind_messaging_runtime(agent.clone())?;
        // `memory` recall shares History/`InferenceEngine::recall_memory`
        // through a typed capability port. The port is immutable after this
        // composition step, so catalog rebuilds cannot retain a stale closure.
        tools.bind_memory_recall(agent.clone())?;

        let pipeline = Arc::new(InputPipeline::new());
        pipeline.set_limits(&context_limits_clone);
        let shell = Arc::new(DesktopShell::new());
        let runtime = Arc::new(ApplicationRuntime::new(RuntimeServices {
            db: db.clone(),
            tools: tools.clone(),
            executor: executor.clone(),
            agent: agent.clone(),
            pipeline: pipeline.clone(),
            shell: shell.clone(),
            log_filter_handles: filter_handles.clone(),
            config_service: config_service.clone(),
        }));

        // Periodic memory maintenance: fact decay, dedup, sensitive purge and
        // embedding pruning. Hot-path infer only extracts + bounded-embeds, so
        // this scheduler owns the full sweep — run once at startup, then every
        // 6 hours. (`interval` yields immediately on the first `tick`; we use
        // that as the startup pass instead of discarding it.)
        {
            let agent = agent.clone();
            runtime.spawn_with_child_token("memory-maintenance", move |cancel| async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
                loop {
                    tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = ticker.tick() => {}
                    }
                    if let Err(error) = agent.run_memory_maintenance().await {
                        tracing::warn!("periodic memory maintenance failed: {}", error);
                    }
                }
            });
        }

        let stt_config = &cfg.media.stt;

        // Build the STT client for the configured provider and wire it into
        // the input pipeline. On error (e.g. `mcp` provider with no server)
        // or `none`, the pipeline gets no client so transcription is disabled.
        let mcp_caller: std::sync::Arc<dyn haven_llm::McpToolCaller> =
            std::sync::Arc::new(tools.mcp_manager().clone());
        let stt_client: Option<std::sync::Arc<dyn haven_llm::SttClient>> = match build_stt_client(
            router.clone(),
            Some(mcp_caller),
            stt_config,
            &cfg.llm.providers,
        ) {
            Ok(client) => client.map(std::sync::Arc::from),
            Err(e) => {
                tracing::warn!("STT client build failed, transcription disabled: {e}");
                None
            }
        };
        pipeline.set_stt_client(stt_client.clone()).await;
        // `provider == "llm"`: hotkey transcription uses the same
        // `LlmRouter::transcribe_audio` path as the model-facing media tool.
        if stt_config.provider == "llm" {
            pipeline.set_stt_router(Some(router.clone())).await;
        } else {
            pipeline.set_stt_router(None).await;
        }

        // Build the TTS client once for the model-facing `media.speak` operation
        // while startup wiring is assembled below. A failed optional
        // capability degrades only that capability and remains observable in
        // the log.
        let tts: Option<std::sync::Arc<dyn haven_llm::TtsClient>> =
            match haven_llm::build_tts_client(&cfg.media.tts, &cfg.llm.providers) {
                Ok(c) => c.map(std::sync::Arc::from),
                Err(e) => {
                    tracing::warn!("TTS client build failed, TTS disabled: {e}");
                    None
                }
            };

        // Dedicated media providers are wired directly into the canonical
        // model-facing `media` tool. Attachments remain raw managed assets;
        // no hidden ingress extraction or generation runs before ReAct.
        let ocr_client: Option<std::sync::Arc<dyn haven_llm::OcrClient>> =
            match haven_llm::build_ocr_client(&cfg.media.ocr) {
                Ok(c) => c.map(std::sync::Arc::from),
                Err(e) => {
                    tracing::warn!("OCR client build failed, OCR disabled: {e}");
                    None
                }
            };
        let image_gen_client: Option<std::sync::Arc<dyn haven_llm::ImageGenClient>> =
            match haven_llm::build_image_gen_client(&cfg.media.image_gen, &cfg.llm.providers) {
                Ok(c) => c.map(std::sync::Arc::from),
                Err(e) => {
                    tracing::warn!(
                        "image generation client build failed, image generation disabled: {e}"
                    );
                    None
                }
            };

        // The previous process is gone, so any session left `running` can
        // never resume — mark it errored immediately so the user sees the
        // interrupted state and can retry via the continue flow. This runs
        // before any UI fetches the session list.
        {
            let db_finalize = db.clone();
            runtime.spawn("finalize-orphaned-sessions", async move {
                match db_finalize.finalize_orphaned_running_sessions() {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            "finalized {} orphaned running session(s) from previous run",
                            n
                        );
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(error = %error, "failed to finalize orphaned running sessions");
                    }
                }
            });
        }

        // Retention-based cleanup: deferred to background (non-critical).
        let retention_days = cfg.memory.history_retention_days;
        if retention_days > 0 {
            let db_retention = db.clone();
            let days = retention_days;
            runtime.spawn("retention-cleanup", async move {
                match db_retention.delete_old_sessions(days) {
                    Ok(n) if n > 0 => {
                        tracing::info!("cleaned up {} session(s) older than {} days", n, days);
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "deferred session retention cleanup failed"
                    ),
                }
            });

            let upload_root = haven_common::default_work_dir().join("uploads");
            let upload_ttl = std::time::Duration::from_secs(
                u64::from(retention_days).saturating_mul(24 * 60 * 60),
            );
            let upload_registry = tools.managed_assets().clone();
            let db_upload_cleanup = db.clone();
            runtime.spawn("upload-retention-cleanup", async move {
                let referenced_paths = match db_upload_cleanup.list_managed_attachment_paths() {
                    Ok(paths) => paths,
                    Err(error) => {
                        tracing::warn!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "deferred upload cleanup skipped: could not read attachment references"
                        );
                        return;
                    }
                };
                match crate::commands::recording::cleanup_stale_upload_batches_with_references(
                    upload_root,
                    upload_ttl,
                    upload_registry,
                    Some(referenced_paths),
                )
                .await
                {
                    Ok(n) if n > 0 => {
                        tracing::info!("cleaned up {} stale upload batch(es)", n);
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error),
                        "deferred upload retention cleanup failed"
                    ),
                }
            });
        }

        // Crash leftovers in private upload staging directories are temporary
        // state, so their cleanup is independent from history retention.
        let staging_root = haven_common::default_work_dir().join("uploads");
        let generated_root = haven_common::config::default_generated_media_dir();
        let generated_registry = tools.managed_assets().clone();
        runtime.spawn("stale-upload-cleanup", async move {
            match crate::commands::recording::cleanup_stale_upload_staging(staging_root).await {
                Ok(n) if n > 0 => {
                    tracing::info!("cleaned up {} stale upload staging director(ies)", n);
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&error),
                    "deferred upload staging cleanup failed"
                ),
            }
            match crate::commands::recording::cleanup_stale_generated_media(
                generated_root,
                generated_registry,
            )
            .await
            {
                Ok(n) if n > 0 => {
                    tracing::info!("cleaned up {} expired generated media file(s)", n);
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&error),
                    "deferred generated media cleanup failed"
                ),
            }
        });

        // Spawn background cleanup every 24 hours
        let db_clone = db.clone();
        let retention = retention_days;
        let upload_root = haven_common::default_work_dir().join("uploads");
        let upload_ttl = std::time::Duration::from_secs(
            u64::from(retention_days.max(1)).saturating_mul(24 * 60 * 60),
        );
        let upload_registry = tools.managed_assets().clone();
        let generated_root = haven_common::config::default_generated_media_dir();
        let generated_registry = tools.managed_assets().clone();
        runtime.spawn_with_child_token("daily-cleanup", move |cancel| async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = interval.tick() => {}
                }
                if retention > 0 {
                    match db_clone.delete_old_sessions(retention) {
                        Ok(n) if n > 0 => {
                            tracing::info!("background cleanup: removed {} old session(s)", n);
                        }
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "background session retention cleanup failed"
                        ),
                    }
                }
                if retention > 0 {
                    match db_clone.list_managed_attachment_paths() {
                        Ok(referenced_paths) => {
                            match crate::commands::recording::cleanup_stale_upload_batches_with_references(
                                upload_root.clone(),
                                upload_ttl,
                                upload_registry.clone(),
                                Some(referenced_paths),
                            )
                            .await
                            {
                                Ok(n) if n > 0 => {
                                    tracing::info!(
                                        "background cleanup: removed {} stale upload batch(es)",
                                        n
                                    );
                                }
                                Ok(_) => {}
                                Err(error) => tracing::warn!(
                                    error = %haven_common::error::sanitize_error_text(&error),
                                    "background upload retention cleanup failed"
                                ),
                            }
                        }
                        Err(error) => tracing::warn!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "background upload cleanup skipped: could not read attachment references"
                        ),
                    }
                }
                match crate::commands::recording::cleanup_stale_upload_staging(upload_root.clone())
                    .await
                {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            "background cleanup: removed {} stale upload staging director(ies)",
                            n
                        );
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error),
                        "background upload staging cleanup failed"
                    ),
                }
                match crate::commands::recording::cleanup_stale_generated_media(
                    generated_root.clone(),
                    generated_registry.clone(),
                )
                .await
                {
                    Ok(n) if n > 0 => {
                        tracing::info!(
                            "background cleanup: removed {} expired generated media file(s)",
                            n
                        );
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error),
                        "background generated media cleanup failed"
                    ),
                }
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

        // Wire the five typed admin surfaces: the assistant can read status,
        // change typed config, toggle skills/tools/MCP servers, tail logs, and
        // switch the runtime log level (via the tracing reload handles).
        let log_path = cfg.log.file_enabled.then(|| {
            cfg.log
                .file_path
                .clone()
                .unwrap_or_else(haven_common::config::LogConfig::default_log_path)
        });
        let log_level = Some(Arc::new(ReloadLogLevelPort {
            handles: filter_handles.clone(),
        }) as Arc<dyn haven_tools::LogLevelPort>);
        let admin_context = haven_tools::AdminContext {
            config_service: Some(config_service.clone()),
            db: Some(db.clone()),
            router: Some(router.clone()),
            log_path,
            log_level,
            // The admin tool's tool_enable/tool_disable ops apply the runtime
            // change through the running ToolsManager after persisting config.
            tool_control: Some(tools.tool_control_port()),
        };

        // Single catalog rebuild for all startup wiring (settings / shell /
        // limits / router / audio pipeline / admin surface). Previously each
        // setter rebuilt the catalog and delayed window creation.
        tools
            .wire_startup(haven_tools::StartupWiring {
                tool_settings: cfg.tool_settings.clone(),
                default_shell: cfg.default_shell,
                context_limits: context_limits_clone,
                security: cfg.security.clone(),
                router: router.clone(),
                media_config: cfg.media.clone(),
                audio_pipeline: Some(pipeline.clone()),
                stt_client: stt_client.clone(),
                ocr_client,
                image_gen_client,
                tts_client: tts,
                admin_context,
            })
            .await;

        tracing::debug!(
            "AppState::new phase=done elapsed={}ms",
            t0.elapsed().as_millis()
        );

        Ok(Self {
            runtime,
            recording_session: Arc::new(std::sync::Mutex::new(None)),
            pending_recording_usage: Arc::new(std::sync::Mutex::new(HashMap::new())),
            bootstrap_ready: Arc::new(AtomicBool::new(false)),
            ui_confirmations: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
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
        let tools = self.tools.clone();
        let pipeline = self.pipeline.clone();
        let agent = self.agent.clone();
        let bootstrap_ready = self.bootstrap_ready.clone();
        let cfg = match self.config_service.snapshot() {
            Ok(snapshot) => snapshot.config,
            Err(error) => {
                tracing::error!("cannot read config for background init: {error}");
                return;
            }
        };
        let mcp_servers = cfg.mcp_servers.clone();
        let mcp_discovery = cfg.mcp_discovery.clone();
        let skills_cfg_root = cfg.skills.root.clone();
        let skills_cfg_enabled = cfg.skills.enabled.clone();

        emit(AppBootstrapEvent {
            status: BootstrapStatus::Loading.as_str().to_string(),
        });

        self.runtime
            .spawn_with_child_token("app-bootstrap", move |cancel| async move {
            // Audio engine + VAD worker: first recording must not pay spawn
            // latency, but window creation should not wait for it either.
            pipeline.prewarm().await;

            // Start new conversations immediately. Recovery of sessions left
            // Pending by a previous process is deferred until the catalog is
            // ready below, so restart semantics do not race an empty catalog.
            agent
                .clone()
                .start_without_pending_recovery_with_cancellation(cancel.clone());

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
                        if let Err(e) = tools
                            .skills_engine()
                            .set_config(skills_cfg_root, skills_cfg_enabled)
                            .await
                        {
                            tracing::warn!("skills engine initial scan failed: {e}");
                        }
                        tools.rebuild_catalog().await;
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

            match agent.recover_pending_sessions().await {
                Ok(reloaded) if reloaded > 0 => {
                    tracing::info!(
                        "deferred dispatcher recovery reloaded {} pending session(s)",
                        reloaded
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::error!(
                    error = %error,
                    "deferred dispatcher recovery failed: pending sessions could not be loaded"
                ),
            }

            if cancel.is_cancelled() {
                return;
            }

            bootstrap_ready.store(true, Ordering::Release);
            emit(AppBootstrapEvent {
                status: BootstrapStatus::Ready.as_str().to_string(),
            });
            tracing::info!("app bootstrap ready (MCP/skills/audio prewarm finished)");
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn new_initializes_core_components_with_default_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let db_path = dir.path().join("test.db");

        // Missing config file → created with defaults.
        let loader = ConfigLoader::load_from(&cfg_path).unwrap();
        let state = AppState::new(&db_path, vec![], loader).await.unwrap();

        // Builtin tools are registered synchronously before new() returns.
        assert!(state.tools.get_tool("files.read").await.is_some());
        assert!(state.tools.get_tool("shell").await.is_some());

        // The default config is loaded and accessible via the mutex.
        let cfg = state.config_service.snapshot().unwrap().config;
        assert!(cfg.session.max_steps > 0);
        assert_eq!(cfg.media.stt.provider, "llm");
        assert_eq!(state.bootstrap_status(), BootstrapStatus::Loading);
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
        let state = AppState::new(&dir.path().join("test.db"), vec![], loader)
            .await
            .unwrap();
        let runtime = state.runtime.clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let dropped_task = dropped.clone();

        assert!(runtime.spawn("runtime-test-task", async move {
            let _probe = DropProbe(dropped_task);
            std::future::pending::<()>().await;
        }));

        tokio::task::yield_now().await;
        runtime.shutdown().await;

        assert!(runtime.cancellation_token().is_cancelled());
        assert!(dropped.load(Ordering::Acquire));
        assert!(!runtime.spawn("late-task", async {}));

        // The second call is intentionally a no-op: teardown is safe to call
        // from both an exit hook and an owning test fixture.
        runtime.teardown().await;
    }

    #[tokio::test]
    async fn new_uses_existing_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let db_path = dir.path().join("test.db");
        let loader = ConfigLoader::load_from(&cfg_path).unwrap();
        let state = AppState::new(&db_path, vec![], loader).await.unwrap();
        let mut config = state.config_service.snapshot().unwrap().config;
        config.session.max_steps = 42;
        state
            .config_service
            .edit(|current| {
                *current = config;
                Ok(())
            })
            .unwrap();
        drop(state);

        let loader2 = ConfigLoader::load_from(&cfg_path).unwrap();
        let state2 = AppState::new(&db_path, vec![], loader2).await.unwrap();
        assert_eq!(
            state2
                .config_service
                .snapshot()
                .unwrap()
                .config
                .session
                .max_steps,
            42
        );
    }
}
