pub mod action_status;
pub mod actions;
pub mod ask;
pub mod audio;
pub mod clipboard;
pub mod env_var;
pub mod facts;
pub mod file;
pub mod file_search;
pub mod input;
pub mod load_mcp;
pub mod load_skill;
pub mod network;
pub mod notify;
pub mod power;
pub mod process;
pub mod registry;
pub mod scheduled_action;
pub mod self_tool;
pub mod shell;
pub mod system;
pub mod window;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::ToolBox;
use crate::ToolRegistry;
use crate::bg::BackgroundActions;
use crate::mcp::McpManager;
use crate::skills::SkillsEngine;
use crate::skills::runner::SkillRunner;

pub use facts::FactsTool;
pub use scheduled_action::{
    ScheduleMode, ScheduleTool, ScheduledActionCenter, ScheduledActionFired,
};
pub use self_tool::{SelfTool, SelfToolContext};

/// Effective output cap for a tool: the per-tool `tool_settings` override
/// when set, else the global observation budget
/// `context_limits.max_observation_chars`. The observation budget is the
/// ONLY output limit: tools truncate their own output to it, and the loop
/// applies the same budget as a safety net for adapters, so there is no
/// double truncation with different values.
fn tool_output_cap(
    settings: &HashMap<String, haven_common::config::ToolConfig>,
    name: &str,
    default_cap: usize,
) -> usize {
    settings
        .get(name)
        .and_then(|c| c.max_output_chars)
        .unwrap_or(default_cap)
}

#[allow(clippy::too_many_arguments)]
pub async fn register_builtin_tools(
    tools: &mut Vec<ToolBox>,
    skills_engine: &SkillsEngine,
    skill_runner: &Arc<RwLock<SkillRunner>>,
    mcp_manager: &Arc<McpManager>,
    server_configs: &Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
    router: Option<Arc<haven_llm::LlmRouter>>,
    background_actions: Arc<BackgroundActions>,
    scheduled_actions: Arc<ScheduledActionCenter>,
    self_context: Option<SelfToolContext>,
    registry: ToolRegistry,
    clipboard_history: Arc<clipboard::ClipboardHistory>,
    settings: &HashMap<String, haven_common::config::ToolConfig>,
    limits: &haven_common::config::ContextLimitsConfig,
    default_shell: haven_common::types::ShellChoice,
    audio_pipeline: Option<Arc<haven_input::InputPipeline>>,
) {
    tools.push(Arc::new(audio::AudioTool::new(audio_pipeline)));
    tools.push(Arc::new(ask::AskTool));
    tools.push(Arc::new(file::FilesTool::new(
        router,
        tool_output_cap(settings, "files", limits.max_observation_chars),
        limits.file_read_max_chars,
        limits.file_line_span,
        limits.file_max_line_chars,
        limits.file_summary_input_chars,
        limits.file_max_list_entries,
        limits.file_max_byte_read,
        limits.file_vision_max_bytes,
        limits.file_summary_timeout_secs,
        file_search::FileSearchEngine::new(
            limits.search_snippet_chars,
            limits.search_max_results,
            limits.search_max_file_size_bytes,
            limits.search_window_bytes,
        ),
    )));
    tools.push(Arc::new(process::ProcessTool {
        max_output_chars: tool_output_cap(settings, "process", limits.max_observation_chars),
    }));
    tools.push(Arc::new(clipboard::ClipboardTool::new(
        clipboard_history,
        tool_output_cap(settings, "clipboard", limits.max_observation_chars),
        limits.clipboard_history_entries,
        limits.clipboard_history_max_entries,
        limits.clipboard_entry_max_chars,
    )));
    tools.push(Arc::new(shell::ShellTool {
        actions: background_actions.clone(),
        max_output_chars: tool_output_cap(settings, "shell", limits.max_observation_chars),
        default_shell: default_shell.as_str().into(),
    }));
    tools.push(Arc::new(action_status::ActionStatusTool {
        actions: background_actions.clone(),
    }));
    tools.push(Arc::new(actions::ActionsTool {
        actions: background_actions,
    }));
    tools.push(Arc::new(input::InputTool));
    tools.push(Arc::new(scheduled_action::ScheduleTool {
        center: scheduled_actions,
        // Weak registry probe so `set` can validate tool_name / risk at
        // schedule time; taken before `registry` is moved into SelfTool.
        registry: Some(registry.probe()),
    }));
    tools.push(Arc::new(system::SystemInfoTool));
    tools.push(Arc::new(env_var::EnvTool {
        max_output_chars: tool_output_cap(settings, "env", limits.max_observation_chars),
    }));
    tools.push(Arc::new(window::WindowTool));
    tools.push(Arc::new(registry::RegistryTool));
    tools.push(Arc::new(network::NetworkTool {
        max_retries: limits.network_max_retries,
        backoff_base_secs: limits.network_backoff_base_secs,
        max_body_bytes: limits.network_max_body_bytes,
    }));
    tools.push(Arc::new(notify::NotifyTool));
    tools.push(Arc::new(power::PowerTool));
    tools.push(Arc::new(load_skill::LoadSkillTool {
        skills_engine: skills_engine.clone(),
        skill_runner: skill_runner.clone(),
    }));
    tools.push(Arc::new(load_mcp::LoadMcpTool {
        mcp_manager: mcp_manager.clone(),
        server_configs: server_configs.clone(),
    }));
    if let Some(ctx) = self_context {
        // Facts memory needs the DB; like SelfTool it only registers once the
        // desktop shell wires the app context (headless builds skip it).
        tools.push(Arc::new(facts::FactsTool::new(ctx.db.clone())));
        tools.push(Arc::new(self_tool::SelfTool::new(
            ctx,
            skills_engine.clone(),
            mcp_manager.clone(),
            server_configs.clone(),
            registry,
            limits.self_tool_max_instructions_bytes,
            limits.self_tool_max_script_bytes,
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::ToolConfig;

    fn settings_with(tool: &str, cap: Option<usize>) -> HashMap<String, ToolConfig> {
        let mut map = HashMap::new();
        map.insert(
            tool.to_string(),
            ToolConfig {
                max_output_chars: cap,
                ..Default::default()
            },
        );
        map
    }

    #[test]
    fn tool_output_cap_falls_back_to_global_default() {
        assert_eq!(tool_output_cap(&HashMap::new(), "shell", 8_000), 8_000);
        assert_eq!(tool_output_cap(&HashMap::new(), "files", 5_000), 5_000);
    }

    #[test]
    fn tool_output_cap_prefers_per_tool_override() {
        let settings = settings_with("shell", Some(1_000));
        assert_eq!(tool_output_cap(&settings, "shell", 8_000), 1_000);
        // Tools without a settings entry still get the global default.
        assert_eq!(tool_output_cap(&settings, "files", 8_000), 8_000);
    }

    #[test]
    fn tool_output_cap_none_inherits_global() {
        let settings = settings_with("shell", None);
        assert_eq!(tool_output_cap(&settings, "shell", 5_000), 5_000);
    }
}
