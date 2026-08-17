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
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

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
    /// recording (`recording:started` / `transcription:result` /
    /// `transcription:error`) so the frontend can correlate them by id.
    pub recording_session: Arc<std::sync::Mutex<Option<haven_common::types::SessionId>>>,
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

        // Load per-tool settings (timeouts, disabled tools) into the manager
        // so `rebuild_catalog` can apply them from the first rebuild.
        let tool_settings = cfg.tool_settings.clone();
        tools.set_tool_settings(tool_settings).await;
        // Default shell for the `shell` tool (cmd / powershell / pwsh) so the
        // catalog picks it up from the first rebuild.
        tools.set_default_shell(cfg.default_shell).await;
        // Unified context limits (compaction threshold, tool output cap, ...)
        // feed the catalog rebuild so tools pick up the global output cap.
        tools.set_context_limits(context_limits.clone()).await;
        // Apply the configured security threshold to the safety gateway NOW
        // (not only when settings are saved later), so a hand-edited
        // config.toml takes effect on startup. The gateway is what gates
        // every tool execution in the agent loop.
        tools
            .safety_gateway
            .set_min_risk_level(cfg.security.min_risk_level)
            .await;

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

        let pipeline = Arc::new(InputPipeline::new());
        pipeline.set_limits(&context_limits_clone);

        // Periodic memory maintenance: fact decay, dedup, sensitive purge and
        // embedding pruning run on a timer so stale memory is flushed even
        // when no inference has happened recently. The first tick fires
        // immediately (startup cleanup), then every 6 hours.
        {
            let agent = agent.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(std::time::Duration::from_secs(6 * 60 * 60));
                ticker.tick().await;
                loop {
                    ticker.tick().await;
                    agent.run_memory_maintenance().await;
                }
            });
        }

        // Start the long-lived audio capture thread now so the microphone
        // stream is already playing when the user first records — the first
        // words after a click are never lost to device-init latency.
        pipeline.prewarm().await;
        tracing::debug!(
            "AppState::new phase=prewarm elapsed={}ms",
            t0.elapsed().as_millis()
        );

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
            std::sync::Arc::new(haven_input::gateway::MediaGateway::new(
                router.clone(),
                stt_client,
                ocr,
                tts,
                image_gen,
                cfg.media.clone(),
            ))
        };
        agent.set_gateway(Some(gateway)).await;

        // Load MCP servers from config + start health monitors.
        let mcp_servers = cfg.mcp_servers.clone();
        let mcp_discovery = cfg.mcp_discovery.clone();
        let skills_cfg_root = cfg.skills.root.clone();
        let skills_cfg_enabled = cfg.skills.enabled.clone();
        let tools_skills = tools.clone();
        // Wire the router into the ToolsManager so the file `summary` and
        // image-understanding operations can route through it (image_model
        // for vision, small_model for summarization, with balanced-model
        // fallback). Also rebuilds the catalog.
        tools.set_router(router.clone()).await;
        // Wire the shared input pipeline into the ToolsManager so the `audio`
        // tool's `record` operation captures + transcribes through the same
        // engine/STT as user voice input.
        tools.set_audio_pipeline(Some(pipeline.clone())).await;
        tokio::spawn(async move {
            tools_skills
                .discover_all(&mcp_servers, &mcp_discovery)
                .await;
            if let Err(e) = tools_skills
                .skills_engine
                .set_config(skills_cfg_root, skills_cfg_enabled)
                .await
            {
                tracing::warn!("skills engine initial scan failed: {e}");
            }
            tools_skills.rebuild_catalog().await;
        });

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

        // Pre-warm LLM HTTP connection pools for all configured endpoints so
        // the first user message doesn't pay TCP+TLS handshake latency
        // (~50-200ms) on whichever model slot it hits, then start the session
        // dispatcher. Starting the dispatcher only after prewarm finishes means
        // a resumed session's first chat request reuses a warm connection instead
        // of racing the prewarm; dispatch is never blocked on the model slot
        // cold start the prewarm cannot warm (the provider's model load still
        // happens on the first real request). AppState::new returns immediately;
        // both prewarm and dispatch run in the background.
        let router_warm = router.clone();
        let agent_start = agent.clone();
        tokio::spawn(async move {
            // Bound the prewarm gate: a slow/unreachable endpoint's health
            // check (up to 7s per attempt, retried once) must not hold up session
            // dispatch for seconds. In the common case prewarm finishes in
            // ~200ms and the pool is warm before the first request; on timeout
            // the dispatcher still starts, leaving that endpoint to fail fast
            // (and retry) on its own.
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), router_warm.prewarm_all())
                    .await;
            agent_start.start();
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
        };
        tools.set_self_context(self_ctx).await;

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
        })
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
