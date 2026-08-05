pub mod ask;
pub mod audio;
pub mod clipboard;
pub mod env_var;
pub mod file;
pub mod input;
pub mod job_status;
pub mod load_mcp;
pub mod load_skill;
pub mod network;
pub mod notify;
pub mod power;
pub mod process;
pub mod registry;
pub mod reminder;
pub mod search;
pub mod self_tool;
pub mod shell;
pub mod system;
pub mod window;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::ToolBox;
use crate::ToolRegistry;
use crate::bg::BackgroundJobs;
use crate::mcp::McpManager;
use crate::skills::SkillsEngine;
use crate::skills::runner::SkillRunner;

pub use reminder::{ReminderCenter, ReminderFired, ReminderMode, ReminderTool};
pub use self_tool::{SelfTool, SelfToolContext};

#[allow(clippy::too_many_arguments)]
pub async fn register_builtin_tools(
    tools: &mut Vec<ToolBox>,
    skills_engine: &SkillsEngine,
    skill_runner: &Arc<RwLock<SkillRunner>>,
    mcp_manager: &Arc<McpManager>,
    server_configs: &Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
    router: Option<Arc<haven_llm::LlmRouter>>,
    background_jobs: Arc<BackgroundJobs>,
    reminders: Arc<ReminderCenter>,
    self_context: Option<SelfToolContext>,
    registry: ToolRegistry,
) {
    tools.push(Arc::new(audio::AudioTool));
    tools.push(Arc::new(ask::AskTool));
    tools.push(Arc::new(file::FileOpTool::new(router)));
    tools.push(Arc::new(process::ProcessTool));
    tools.push(Arc::new(clipboard::ClipboardTool));
    tools.push(Arc::new(shell::ShellTool {
        jobs: background_jobs.clone(),
    }));
    tools.push(Arc::new(job_status::JobStatusTool {
        jobs: background_jobs,
    }));
    tools.push(Arc::new(input::InputTool));
    tools.push(Arc::new(reminder::ReminderTool { center: reminders }));
    tools.push(Arc::new(system::SystemInfoTool));
    tools.push(Arc::new(env_var::EnvTool));
    tools.push(Arc::new(window::WindowTool));
    tools.push(Arc::new(registry::RegistryTool));
    tools.push(Arc::new(network::NetworkTool));
    tools.push(Arc::new(notify::NotifyTool));
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
    if let Some(ctx) = self_context {
        tools.push(Arc::new(self_tool::SelfTool::new(
            ctx,
            skills_engine.clone(),
            mcp_manager.clone(),
            server_configs.clone(),
            registry,
        )));
    }
}
