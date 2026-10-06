//! Composition boundary for concrete builtin providers.
//!
//! Builtin implementations live under [`crate::builtin`]. This object owns
//! their discovery/runtime dependencies so `ToolsFacade` does not become a
//! second service locator for MCP and Skills.

use crate::builtin::{BuiltinContext, MediaDeps, ToolRunDeps};
use crate::runtime_capabilities::ToolCapabilitySnapshot;
use crate::skill_runner::SkillRunner;
use crate::tool_core::ToolCore;
use crate::tool_runtime::{PlatformRuntime, ToolRuntime};
use haven_common::config::{McpServerConfig, SkillsExecConfig};
use haven_mcp::McpManager;
use haven_skills::{SkillRegistry, VenvManager};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(crate) struct ToolBuiltins {
    pub(crate) mcp_manager: McpManager,
    pub(crate) mcp_server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub(crate) skill_registry: SkillRegistry,
    pub(crate) skill_runner: Arc<RwLock<SkillRunner>>,
}

impl ToolBuiltins {
    pub(crate) fn new(exec_config: SkillsExecConfig) -> Self {
        Self {
            mcp_manager: McpManager::new(),
            mcp_server_configs: Arc::new(RwLock::new(HashMap::new())),
            skill_registry: SkillRegistry::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
        }
    }

    /// Assemble the concrete builtin dependency graph from the three tool
    /// boundaries. Registration remains atomic in `ToolRegistry`; this method
    /// only creates a value object and does not mutate the catalog.
    pub(crate) fn build_context(
        &self,
        core: &ToolCore,
        runtime: &ToolRuntime,
        platform: Arc<PlatformRuntime>,
        capabilities: ToolCapabilitySnapshot,
    ) -> BuiltinContext {
        let settings = platform.tool_settings.clone();
        let limits = platform.context_limits.clone();
        let router = platform.router.clone();
        let admin_context = platform.admin_context.clone();
        let audio_pipeline = platform.audio_pipeline.clone();
        let stt_client = platform.stt_client.clone();
        let ocr_client = platform.ocr_client.clone();
        let image_gen_client = platform.image_gen_client.clone();
        let media_config = platform.media_config.clone();
        let tts_client = platform.tts_client.clone();

        BuiltinContext {
            skill_registry: self.skill_registry.clone(),
            skill_runner: self.skill_runner.clone(),
            mcp_manager: Arc::new(self.mcp_manager.clone()),
            server_configs: self.mcp_server_configs.clone(),
            registry: core.operations.installed.clone(),
            session_tool_overlay: core.operations.session_tool_overlay.clone(),
            deferred_catalog: core.operations.deferred.clone(),
            settings,
            limits,
            default_shell: platform.default_shell,
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
                capabilities: capabilities.media,
            },
            tool_runs: ToolRunDeps {
                live_outputs: runtime.live_outputs.clone(),
                service: runtime.tool_run_service.clone(),
            },
        }
    }
}
