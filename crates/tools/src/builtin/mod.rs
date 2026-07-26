pub mod audio;
pub mod clipboard;
pub mod env_var;
pub mod file_op;
pub mod load_mcp;
pub mod load_skill;
pub mod network;
pub mod power;
pub mod process;
pub mod registry;
pub mod search;
pub mod shell;
pub mod system;
pub mod window;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::mcp::McpManager;
use crate::skills::SkillsEngine;
use crate::skills::runner::SkillRunner;
use crate::ToolBox;

pub async fn register_builtin_tools(
    tools: &mut Vec<ToolBox>,
    skills_engine: &SkillsEngine,
    skill_runner: &Arc<RwLock<SkillRunner>>,
    mcp_manager: &Arc<McpManager>,
    server_configs: &Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
) {
    tools.push(Arc::new(audio::AudioTool));
    tools.push(Arc::new(file_op::FileOpTool));
    tools.push(Arc::new(process::ProcessTool));
    tools.push(Arc::new(clipboard::ClipboardTool));
    tools.push(Arc::new(shell::ShellTool));
    tools.push(Arc::new(system::SystemInfoTool));
    tools.push(Arc::new(env_var::EnvTool));
    tools.push(Arc::new(window::WindowTool));
    tools.push(Arc::new(registry::RegistryTool));
    tools.push(Arc::new(network::NetworkTool));
    tools.push(Arc::new(search::SearchTool));
    tools.push(Arc::new(power::PowerTool));
    tools.push(Arc::new(load_skill::LoadSkillTool {
        skills_engine: skills_engine.clone(),
        skill_runner: skill_runner.clone(),
    }));
    tools.push(Arc::new(load_mcp::LoadMcpTool {
        mcp_manager: mcp_manager.clone(),
        server_configs: server_configs.clone(),
    }));
}
