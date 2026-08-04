use crate::desktop::DesktopShell;
use haven_agent::AgentLayer;
use haven_common::config::ConfigLoader;
use haven_input::InputPipeline;
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use haven_memory::Database;
use haven_task::TaskExecutor;
use haven_tools::ToolsManager;
use std::sync::Arc;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

pub struct AppState {
    pub db: Arc<Database>,
    pub tools: Arc<ToolsManager>,
    pub executor: Arc<TaskExecutor>,
    pub agent: Arc<AgentLayer>,
    pub pipeline: Arc<InputPipeline>,
    pub shell: Arc<DesktopShell>,
    pub log_filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
    pub config_loader: Arc<std::sync::Mutex<ConfigLoader>>,
}

impl AppState {
    pub async fn new(
        db_path: &std::path::Path,
        filter_handles: Vec<reload::Handle<EnvFilter, Registry>>,
        config_loader: ConfigLoader,
    ) -> anyhow::Result<Self> {
        let db = Arc::new(Database::open(db_path)?);

        let db_finalize = db.clone();
        tokio::spawn(async move {
            // The previous process is gone, so any task left `running` can
            // never resume — mark it errored immediately so the user sees the
            // interrupted state and can retry via the continue flow. This runs
            // before any UI fetches the task list.
            if let Ok(n) = db_finalize.finalize_orphaned_running_tasks()
                && n > 0
            {
                tracing::info!("finalized {} orphaned running task(s) from previous run", n);
            }
        });

        let cfg = config_loader.config().clone();
        let llm_config = cfg.llm.clone();
        let router = Arc::new(LlmRouter::new(llm_config));
        let max_steps = cfg.task.max_steps;
        let conversation_window_size = cfg.memory.session_window_size;
        let max_observation_chars = cfg.task.max_observation_chars;

        let tools = Arc::new(ToolsManager::new());

        let executor = Arc::new(TaskExecutor::new(db.clone(), tools.clone(), 3));

        let agent = Arc::new(AgentLayer::new(
            db.clone(),
            executor.clone(),
            router.clone(),
            max_steps,
            conversation_window_size,
            max_observation_chars,
        ));

        let pipeline = Arc::new(InputPipeline::new());

        // Start the long-lived audio capture thread now so the microphone
        // stream is already playing when the user first records — the first
        // words after a click are never lost to device-init latency.
        pipeline.prewarm().await;

        let stt_config = &cfg.stt;

        // Build the STT client for the configured provider and wire it into
        // the input pipeline. On error (e.g. `mcp` provider with no server)
        // or `none`, the pipeline gets no client so transcription is disabled.
        let mcp_caller: std::sync::Arc<dyn haven_llm::McpToolCaller> =
            std::sync::Arc::new(tools.mcp_manager.clone());
        match build_stt_client(router.clone(), Some(mcp_caller), stt_config) {
            Ok(client) => {
                pipeline.set_stt_client(client).await;
            }
            Err(e) => {
                tracing::warn!("STT client build failed, transcription disabled: {e}");
                pipeline.set_stt_client(None).await;
            }
        }

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
                if let Ok(n) = db_retention.delete_old_tasks(days)
                    && n > 0
                {
                    tracing::info!("cleaned up {} task(s) older than {} days", n, days);
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
                    && let Ok(n) = db_clone.delete_old_tasks(retention)
                    && n > 0
                {
                    tracing::info!("background cleanup: removed {} old task(s)", n);
                }
            }
        });

        // Start the task dispatcher
        agent.clone().start();

        // Pre-warm the LLM HTTP connection pool so the first user message
        // doesn't pay TCP+TLS handshake latency (~50-200ms).
        let router_warm = router.clone();
        tokio::spawn(async move {
            match router_warm
                .health_check(haven_llm::EndpointRole::DefaultModel)
                .await
            {
                Ok(()) => tracing::info!("LLM connection pre-warmed"),
                Err(e) => {
                    tracing::warn!("LLM pre-warm failed (will retry on first request): {}", e)
                }
            }
        });

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

        Ok(Self {
            db,
            tools,
            executor,
            agent,
            pipeline,
            shell,
            log_filter_handles: filter_handles,
            config_loader: config_loader_arc,
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
        assert!(state.tools.get_tool("file").await.is_some());
        assert!(state.tools.get_tool("shell").await.is_some());

        // The default config is loaded and accessible via the mutex.
        let cfg = state.config_loader.lock().unwrap().config().clone();
        assert!(cfg.task.max_steps > 0);
        assert_eq!(cfg.stt.provider, "mcp");
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
            .task
            .max_steps = 42;
        state.config_loader.lock().unwrap().save().unwrap();
        drop(state);

        let loader2 = ConfigLoader::load_from(&cfg_path).unwrap();
        let state2 = AppState::new(&db_path, vec![], loader2).await.unwrap();
        assert_eq!(
            state2.config_loader.lock().unwrap().config().task.max_steps,
            42
        );
    }
}
