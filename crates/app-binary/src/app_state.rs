use haven_agent::AgentLayer;
use haven_common::config::ConfigLoader;
use haven_desktop::DesktopShell;
use haven_input::InputPipeline;
use haven_llm::LlmRouter;
use haven_memory::Database;
use haven_task::TaskExecutor;
use haven_tools::ToolsManager;
use haven_tools::stt::{LlmSttAdapter, McpSttClient};
use std::sync::Arc;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;
use tracing_subscriber::Registry;

pub struct AppState {
    pub db: Arc<Database>,
    pub tools: Arc<ToolsManager>,
    pub executor: Arc<TaskExecutor>,
    pub agent: Arc<AgentLayer>,
    pub pipeline: Arc<InputPipeline>,
    pub shell: Arc<DesktopShell>,
    pub log_filter_handle: reload::Handle<EnvFilter, Registry>,
    #[allow(dead_code)]
    pub router: Arc<LlmRouter>,
    pub config_loader: Arc<std::sync::Mutex<ConfigLoader>>,
}

impl AppState {
    pub async fn new(
        db_path: &std::path::Path,
        filter_handle: reload::Handle<EnvFilter, Registry>,
        config_loader: ConfigLoader,
    ) -> anyhow::Result<Self> {
        let db = Arc::new(Database::open(db_path)?);

        let db_finalize = db.clone();
        tokio::spawn(async move {
            if let Ok(n) = db_finalize.finalize_stale_tasks(10)
                && n > 0
            {
                tracing::info!("finalized {} stale task(s) from previous run", n);
            }
        });

        let tools = Arc::new(ToolsManager::new());
        let executor = Arc::new(TaskExecutor::new(db.clone(), tools.clone(), 3));

        let cfg = config_loader.config().clone();
        let llm_config = cfg.llm.clone();
        let router = Arc::new(LlmRouter::new(llm_config));
        let max_steps = cfg.task.max_steps;
        let session_window_size = cfg.memory.session_window_size;
        let max_observation_chars = cfg.task.max_observation_chars;

        let agent = Arc::new(AgentLayer::new(
            db.clone(),
            executor.clone(),
            router.clone(),
            max_steps,
            session_window_size,
            max_observation_chars,
        ));

        let pipeline = Arc::new(InputPipeline::new());

        let stt_config = &cfg.stt;

        // Load MCP servers from config + start health monitors.
        let mcp_servers = cfg.mcp_servers.clone();
        let mcp_discovery = cfg.mcp_discovery.clone();
        let skills_cfg_root = cfg.skills.root.clone();
        let skills_cfg_enabled = cfg.skills.enabled.clone();
        let tools_skills = tools.clone();
        tokio::spawn(async move {
            tools_skills.discover_all(&mcp_servers, &mcp_discovery).await;
            if let Err(e) = tools_skills
                .skills_engine
                .set_config(skills_cfg_root, skills_cfg_enabled)
                .await
            {
                tracing::warn!("skills engine initial scan failed: {e}");
            }
            tools_skills.rebuild_catalog().await;
        });

        if stt_config.provider == "mcp" {
            if let Some(server_name) = &stt_config.mcp_server {
                let client = McpSttClient::new(
                    tools.mcp_manager.clone(),
                    server_name,
                    stt_config.timeout_secs,
                );
                pipeline.set_stt_client(Box::new(client)).await;
            }
        } else {
            pipeline.set_stt_client(Box::new(LlmSttAdapter)).await;
        }

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

        // Session recovery: deferred to background (non-critical).
        let db_session = db.clone();
        let agent_session = agent.clone();
        tokio::spawn(async move {
            let session_id = agent_session.ensure_session();
            if let Ok(Some(session)) = db_session.get_session(&session_id)
                && session.ended_at.is_none()
            {
                tracing::info!("session '{}' recovered from previous run", session_id);
            }
        });

        let config_loader_arc = Arc::new(std::sync::Mutex::new(config_loader));

        Ok(Self {
            db,
            tools,
            executor,
            agent,
            pipeline,
            shell,
            log_filter_handle: filter_handle,
            router,
            config_loader: config_loader_arc,
        })
    }
}

