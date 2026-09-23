//! Composition boundary for concrete builtin providers.
//!
//! Builtin implementations live under [`crate::builtin`]. This object owns
//! their discovery/runtime dependencies so `ToolsManager` does not become a
//! second service locator for MCP and Skills.

use crate::builtin::{ActionDeps, BuiltinContext, MediaDeps};
use crate::skill_runner::SkillRunner;
use crate::tool_core::ToolCore;
use crate::tool_runtime::ToolRuntime;
use haven_common::config::{McpServerConfig, SkillsExecConfig};
use haven_common::types::ShellChoice;
use haven_mcp::McpManager;
use haven_skills::{SkillsEngine, VenvManager};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(crate) struct ToolBuiltins {
    pub(crate) mcp_manager: McpManager,
    pub(crate) mcp_server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub(crate) skills_engine: SkillsEngine,
    pub(crate) skill_runner: Arc<RwLock<SkillRunner>>,
    pub(crate) default_shell: RwLock<ShellChoice>,
}

impl ToolBuiltins {
    pub(crate) fn new(exec_config: SkillsExecConfig) -> Self {
        Self {
            mcp_manager: McpManager::new(),
            mcp_server_configs: Arc::new(RwLock::new(HashMap::new())),
            skills_engine: SkillsEngine::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
            default_shell: RwLock::new(ShellChoice::default()),
        }
    }

    /// Assemble the concrete builtin dependency graph from the three tool
    /// boundaries. Registration remains atomic in `ToolRegistry`; this method
    /// only creates a value object and does not mutate the catalog.
    pub(crate) async fn build_context(
        &self,
        core: &ToolCore,
        runtime: &ToolRuntime,
    ) -> BuiltinContext {
        let platform = runtime.platform().await;
        let settings = core.tool_settings.read().await.clone();
        let limits = core.context_limits.read().await.clone();
        let router = platform.router.clone();
        let admin_context = platform.admin_context.clone();
        let audio_pipeline = platform.audio_pipeline.clone();
        let stt_client = platform.stt_client.clone();
        let ocr_client = platform.ocr_client.clone();
        let image_gen_client = platform.image_gen_client.clone();
        let media_config = platform.media_config.clone();
        let tts_client = platform.tts_client.clone();

        BuiltinContext {
            skills_engine: self.skills_engine.clone(),
            skill_runner: self.skill_runner.clone(),
            mcp_manager: Arc::new(self.mcp_manager.clone()),
            server_configs: self.mcp_server_configs.clone(),
            registry: core.operations.installed.clone(),
            session_catalog: core.operations.sessions.clone(),
            deferred_catalog: core.operations.deferred.clone(),
            settings,
            limits,
            default_shell: *self.default_shell.read().await,
            clipboard_history: runtime.clipboard_history.clone(),
            admin_context,
            messaging_service: runtime.messaging_service.clone(),
            memory_recall: runtime.memory_recall.clone(),
            managed_assets: runtime.managed_assets.clone(),
            media: MediaDeps {
                router,
                audio_pipeline,
                stt_client,
                ocr_client,
                image_gen_client,
                tts_client,
                config: media_config,
            },
            actions: ActionDeps {
                live_outputs: runtime.live_outputs.clone(),
                service: runtime.action_service.clone(),
            },
        }
    }
}
