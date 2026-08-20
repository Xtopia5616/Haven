use crate::desktop::DesktopShell;
use haven_agent::AgentLayer;
use haven_agent::SessionExecutor;
use haven_common::config::ConfigLoader;
use haven_input::InputPipeline;
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use haven_memory::Database;
use haven_tools::ToolsManager;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

/// Cold-start progress exposed to the UI status chip.
/// `loading` while MCP/skills/audio prewarm finish in the background;
/// `ready` once that deferred work completes (or immediately when there is
/// nothing deferred).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapStatus {
    Loading,
    Ready,
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
    pub db: Arc<Database>,
    pub tools: Arc<ToolsManager>,
    pub executor: Arc<SessionExecutor>,
    pub agent: Arc<AgentLayer>,
    pub pipeline: Arc<InputPipeline>,
    pub shell: Arc<DesktopShell>,
    pub log_filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    pub config_loader: Arc<std::sync::Mutex<ConfigLoader>>,
    /// The `rec-{uuid}` id of the in-flight voice recording. Set when a
    /// recording starts (button or hotkey), consumed by
    /// `finalize_transcription`, and shared by every event of the same
    /// recording (`recording:started` / `transcription:*` events) so the
    /// frontend can correlate them by id.
    pub recording_session: Arc<std::sync::Mutex<Option<haven_common::types::SessionId>>>,
    /// True once deferred startup (MCP discover + skills scan + audio
    /// prewarm) has finished. The UI polls / listens so the status chip can
    /// show 加载中 → 就绪 without blocking window creation.
    bootstrap_ready: Arc<AtomicBool>,
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

        let db_finalize = db.clone();
        tokio::spawn(async move {
            // The previous process is gone, so any session left `running` can
            // never resume — mark it errored immediately so the user sees the
            // interrupted state and can retry via the continue flow. This runs
            // before any UI fetches the session list.
            if let Ok(n) = db_finalize.finalize_orphaned_running_sessions()
                && n > 0
            {
                tracing::info!(
                    "finalized {} orphaned running session(s) from previous run",
                    n
                );
            }
        });

        let cfg = config_loader.config().clone();
        let llm_config = cfg.llm.materialize(
            Some(cfg.context_limits.max_response_tokens),
            Some(cfg.context_limits.reasoning_echo_max_chars),
        );
        let router = Arc::new(LlmRouter::new(llm_config));
        let max_steps = cfg.session.max_steps;
        let conversation_window_size = cfg.memory.session_window_size;
        let context_limits = cfg.context_limits.clone();
        let context_limits_clone = context_limits.clone();

        let tools = Arc::new(ToolsManager::new());

        let executor = Arc::new(SessionExecutor::new(
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

        // Plan A multi-agent: `agent_spawn` creates real peer sessions through
        // the agent layer (tools crate cannot depend on haven-agent).
        {
            let agent_for_spawn = agent.clone();
            tools
                .set_agent_spawner(std::sync::Arc::new(move |req| {
                    let agent = agent_for_spawn.clone();
                    Box::pin(async move { agent.spawn_peer_session(req).await })
                }))
                .await;
        }

        let pipeline = Arc::new(InputPipeline::new());
        pipeline.set_limits(&context_limits_clone);

        // Periodic memory maintenance: fact decay, dedup, sensitive purge and
        // embedding pruning. Hot-path infer only extracts + bounded-embeds, so
        // this scheduler owns the full sweep — run once at startup, then every
        // 6 hours. (`interval` yields immediately on the first `tick`; we use
        // that as the startup pass instead of discarding it.)
        {
            let agent = agent.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
                loop {
                    ticker.tick().await;
                    let _ = agent.run_memory_maintenance().await;
                }
            });
        }

        let stt_config = &cfg.media.stt;

        // Build the STT client for the configured provider and wire it into
        // the input pipeline. On error (e.g. `mcp` provider with no server)
        // or `none`, the pipeline gets no client so transcription is disabled.
        let mcp_caller: std::sync::Arc<dyn haven_llm::McpToolCaller> =
            std::sync::Arc::new(tools.mcp_manager.clone());
        let stt_client: Option<std::sync::Arc<dyn haven_llm::SttClient>> =
            match build_stt_client(router.clone(), Some(mcp_caller), stt_config) {
                Ok(client) => client.map(std::sync::Arc::from),
                Err(e) => {
                    tracing::warn!("STT client build failed, transcription disabled: {e}");
                    None
                }
            };
        pipeline.set_stt_client(stt_client.clone()).await;

        // Media gateway: dedicated OCR / TTS / image-generation clients plus
        // the shared STT client. The gateway pre-processes attachments
        // (extract → OCR/ASR with main-model fallback) and handles pure-text
        // generation requests (TTS / text-to-image) before they reach the
        // ReAct loop. A capability build error disables only that capability
        // (fail-open: the main model still handles the media).
        let gateway = {
            let ocr: Option<std::sync::Arc<dyn haven_llm::OcrClient>> =
                match haven_llm::build_ocr_client(&cfg.media.ocr) {
                    Ok(c) => c.map(std::sync::Arc::from),
                    Err(e) => {
                        tracing::warn!("OCR client build failed, OCR disabled: {e}");
                        None
                    }
                };
            let tts: Option<std::sync::Arc<dyn haven_llm::TtsClient>> =
                match haven_llm::build_tts_client(&cfg.media.tts) {
                    Ok(c) => c.map(std::sync::Arc::from),
                    Err(e) => {
                        tracing::warn!("TTS client build failed, TTS disabled: {e}");
                        None
                    }
                };
            let image_gen: Option<std::sync::Arc<dyn haven_llm::ImageGenClient>> =
                match haven_llm::build_image_gen_client(&cfg.media.image_gen) {
                    Ok(c) => c.map(std::sync::Arc::from),
                    Err(e) => {
                        tracing::warn!(
                            "image generation client build failed, image generation disabled: {e}"
                        );
                        None
                    }
                };
            std::sync::Arc::new(haven_llm::media::MediaGateway::new(
                router.clone(),
                stt_client,
                ocr,
                tts,
                image_gen,
                cfg.media.clone(),
            ))
        };
        agent.set_gateway(Some(gateway)).await;

        let shell = Arc::new(DesktopShell::new());

        // Retention-based cleanup: deferred to background (non-critical).
        let retention_days = cfg.memory.history_retention_days;
        if retention_days > 0 {
            let db_retention = db.clone();
            let days = retention_days;
            tokio::spawn(async move {
                if let Ok(n) = db_retention.delete_old_sessions(days)
                    && n > 0
                {
                    tracing::info!("cleaned up {} session(s) older than {} days", n, days);
                }
            });
        }

        // Spawn background cleanup every 24 hours
        let db_clone = db.clone();
        let retention = retention_days;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(86400));
            loop {
                interval.tick().await;
                if retention > 0
                    && let Ok(n) = db_clone.delete_old_sessions(retention)
                    && n > 0
                {
                    tracing::info!("background cleanup: removed {} old session(s)", n);
                }
            }
        });

        // Pre-warm LLM HTTP pools in the background so the first chat request
        // does not pay TCP+TLS. The session dispatcher is started later from
        // `spawn_background_init` only after MCP/skills are loaded — otherwise
        // pending-session resume can race an empty MCP catalog.
        let router_warm = router.clone();
        tokio::spawn(async move {
            // Bound the prewarm: a slow/unreachable endpoint's health check
            // must not hold the runtime. On timeout the endpoint fails fast
            // on its first real request instead.
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), router_warm.prewarm_all())
                    .await;
        });
        tracing::debug!(
            "AppState::new phase=agent elapsed={}ms",
            t0.elapsed().as_millis()
        );

        let config_loader_arc = Arc::new(std::sync::Mutex::new(config_loader));

        // Wire the `self` management tool: the assistant can read its own
        // status, change config, toggle skills/MCP servers, tail logs, and
        // switch the runtime log level (via the tracing reload handles).
        let log_path = cfg.log.file_enabled.then(|| {
            cfg.log
                .file_path
                .clone()
                .unwrap_or_else(haven_common::config::LogConfig::default_log_path)
        });
        let log_handles = filter_handles.clone();
        let set_log_level = Some(Arc::new(move |level: String| {
            for handle in &log_handles {
                let _ = handle.modify(|filter| {
                    *filter = EnvFilter::new(format!("haven={}", level));
                });
            }
        }) as Arc<dyn Fn(String) + Send + Sync>);
        let self_ctx = haven_tools::SelfToolContext {
            config_loader: Some(config_loader_arc.clone()),
            db: Some(db.clone()),
            router: Some(router.clone()),
            log_path,
            set_log_level,
            // The self tool's tool_enable/tool_disable ops apply the runtime
            // change through the running ToolsManager after persisting config.
            tools_weak: Some(Arc::downgrade(&tools)),
        };

        // Single catalog rebuild for all startup wiring (settings / shell /
        // limits / router / audio pipeline / self tool). Previously each
        // setter rebuilt the catalog and delayed window creation.
        tools
            .wire_startup(
                cfg.tool_settings.clone(),
                cfg.default_shell,
                context_limits_clone,
                cfg.security.min_risk_level,
                router.clone(),
                Some(pipeline.clone()),
                self_ctx,
            )
            .await;

        tracing::debug!(
            "AppState::new phase=done elapsed={}ms",
            t0.elapsed().as_millis()
        );

        Ok(Self {
            db,
            tools,
            executor,
            agent,
            pipeline,
            shell,
            log_filter_handles: filter_handles,
            config_loader: config_loader_arc,
            recording_session: Arc::new(std::sync::Mutex::new(None)),
            bootstrap_ready: Arc::new(AtomicBool::new(false)),
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
    /// that blocks window creation, then start the session dispatcher.
    /// Emits `app:bootstrap` (`loading` / `ready`) through `emit` so the
    /// status chip can track progress even when the frontend mounts mid-flight.
    pub fn spawn_background_init<F>(&self, emit: F)
    where
        F: Fn(&str, serde_json::Value) + Send + Sync + 'static,
    {
        let tools = self.tools.clone();
        let pipeline = self.pipeline.clone();
        let agent = self.agent.clone();
        let bootstrap_ready = self.bootstrap_ready.clone();
        let cfg = self.config_loader.lock().unwrap().config().clone();
        let mcp_servers = cfg.mcp_servers.clone();
        let mcp_discovery = cfg.mcp_discovery.clone();
        let skills_cfg_root = cfg.skills.root.clone();
        let skills_cfg_enabled = cfg.skills.enabled.clone();

        emit(
            "app:bootstrap",
            serde_json::json!({ "status": BootstrapStatus::Loading.as_str() }),
        );

        tokio::spawn(async move {
            // Audio engine + VAD worker: first recording must not pay spawn
            // latency, but window creation should not wait for it either.
            pipeline.prewarm().await;

            // MCP discover + skills scan run in a task so a hung server cannot
            // block session resume forever. `load_mcp` / restore already wait
            // briefly for tools when the catalog is still warming.
            let tools_bg = tools.clone();
            let mut catalog = tokio::spawn(async move {
                tools_bg.discover_all(&mcp_servers, &mcp_discovery).await;
                if let Err(e) = tools_bg
                    .skills_engine
                    .set_config(skills_cfg_root, skills_cfg_enabled)
                    .await
                {
                    tracing::warn!("skills engine initial scan failed: {e}");
                }
                tools_bg.rebuild_catalog().await;
            });

            // Head-start window: prefer a live catalog for restore, then start
            // the dispatcher regardless so pending sessions are not stuck idle.
            let catalog_finished = tokio::select! {
                r = &mut catalog => {
                    if let Err(e) = r {
                        tracing::warn!("bootstrap catalog task panicked: {e}");
                    }
                    true
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {
                    tracing::warn!(
                        "MCP/skills bootstrap timed out after 10s; starting session dispatcher anyway"
                    );
                    false
                }
            };

            agent.start();

            if !catalog_finished
                && let Err(e) = catalog.await
            {
                tracing::warn!("bootstrap catalog task panicked: {e}");
            }

            bootstrap_ready.store(true, Ordering::Release);
            emit(
                "app:bootstrap",
                serde_json::json!({ "status": BootstrapStatus::Ready.as_str() }),
            );
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
        assert!(state.tools.get_tool("files").await.is_some());
        assert!(state.tools.get_tool("shell").await.is_some());

        // The default config is loaded and accessible via the mutex.
        let cfg = state.config_loader.lock().unwrap().config().clone();
        assert!(cfg.session.max_steps > 0);
        assert_eq!(cfg.media.stt.provider, "mcp");
        assert_eq!(state.bootstrap_status(), BootstrapStatus::Loading);
    }

    #[tokio::test]
    async fn new_uses_existing_config() {
        let dir = tempdir().unwrap();
        let cfg_path = dir.path().join("config.toml");
        let db_path = dir.path().join("test.db");
        let loader = ConfigLoader::load_from(&cfg_path).unwrap();
        let state = AppState::new(&db_path, vec![], loader).await.unwrap();
        state
            .config_loader
            .lock()
            .unwrap()
            .config_mut()
            .session
            .max_steps = 42;
        state.config_loader.lock().unwrap().save().unwrap();
        drop(state);

        let loader2 = ConfigLoader::load_from(&cfg_path).unwrap();
        let state2 = AppState::new(&db_path, vec![], loader2).await.unwrap();
        assert_eq!(
            state2
                .config_loader
                .lock()
                .unwrap()
                .config()
                .session
                .max_steps,
            42
        );
    }
}
