pub mod actions;
pub mod admin;
mod admin_support;
pub mod ask;
pub mod audio;
pub mod clipboard;
mod env;
mod file_outline;
pub mod file_search;
pub mod files;
pub mod http;
pub mod input;
pub mod load_mcp;
pub mod load_skill;
pub mod media;
pub mod memory;
pub mod messaging;
pub mod notify;
mod power;
pub mod process;
mod registry;
pub mod scheduled_action;
pub mod self_tool;
pub mod shell;
pub mod system;
pub mod window;

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::BackgroundActions;
use crate::ToolBox;
use crate::ToolRegistry;
use crate::registry::SessionCatalog;
use crate::skill_runner::SkillRunner;
use haven_mcp::McpManager;
use haven_skills::SkillsEngine;

/// Resolve capability truth for the model-backed media operations. A
/// configured role is not enough: routing may fall back to another role and
/// the selected adapter may explicitly reject the representation.
pub(crate) async fn resolve_media_capabilities(
    router: Option<&Arc<haven_llm::LlmRouter>>,
    dedicated_stt_available: bool,
) -> (bool, bool) {
    let Some(router) = router else {
        return (false, dedicated_stt_available);
    };

    let vision_role = router.vision_role().await;
    let vision_available = router.is_role_configured(vision_role).await
        && router.capability_profile(vision_role).image.is_supported();

    let transcribe_available = if let Some(role) = router.stt_role().await {
        let configured = router.is_role_configured(role).await;
        let profile = router.capability_profile(role);
        let style = {
            let config = router.config().await;
            haven_llm::adapters::api_style_for(config.endpoint(role))
        };
        configured && (profile.audio.is_supported() || haven_llm::is_stt_only_style(style))
    } else {
        false
    };

    (
        vision_available,
        transcribe_available || dedicated_stt_available,
    )
}

pub use admin::{
    AdminCapability, AdminCapabilityTool, AdminOperationMetadata, ConfigAdminContext,
    ConfigAdminOperation, ConfigAdminTool, ConfigOperationArgs, ConfigOperationError,
    ConfigOperationOutput, ConfigViewOutput, LogLevelOutput,
};
pub use memory::{MemoryRecallFn, MemoryRecallSlot, MemoryTool, new_memory_recall_slot};
pub use messaging::{
    AgentSpawnRequest, AgentSpawnResult, AgentSpawner, AgentSpawnerSlot, new_agent_spawner_slot,
};
pub use scheduled_action::{
    ScheduleMode, ScheduledActionCenter, ScheduledActionFired, ScheduledActionTool,
};
pub use self_tool::{SelfOperation, SelfParams, SelfTool, SelfToolContext};

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
    live_outputs: Arc<crate::live_output::LiveOutputHub>,
    scheduled_actions: Arc<ScheduledActionCenter>,
    self_context: Option<SelfToolContext>,
    registry: ToolRegistry,
    clipboard_history: Arc<clipboard::ClipboardHistory>,
    settings: &HashMap<String, haven_common::config::ToolConfig>,
    limits: &haven_common::config::ContextLimitsConfig,
    default_shell: haven_common::types::ShellChoice,
    audio_pipeline: Option<Arc<haven_input::InputPipeline>>,
    stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
    media_config: haven_common::config::MediaConfig,
    session_catalog: SessionCatalog,
    agent_spawner: messaging::AgentSpawnerSlot,
    memory_recall: memory::MemoryRecallSlot,
    managed_assets: crate::ManagedAssetRegistry,
) -> Option<Arc<self_tool::SelfTool>> {
    let mut self_tool_arc: Option<Arc<self_tool::SelfTool>> = None;
    let (vision_available, transcribe_available) =
        resolve_media_capabilities(router.as_ref(), stt_client.is_some()).await;
    let record_available = if let Some(pipeline) = audio_pipeline.as_ref() {
        pipeline.recording_configured().await
    } else {
        false
    };
    let has_enabled_skills = skills_engine.list().await.iter().any(|skill| skill.enabled);
    let has_enabled_mcp = server_configs
        .read()
        .await
        .values()
        .any(|server| server.enabled);
    tools.push(Arc::new(ask::AskTool));
    // One media runtime serves every producer/consumer boundary. `files` and
    // `window` only create assets; interpretation and generation always land
    // in this same instance and therefore share provider routing, limits,
    // capability pruning, and managed-asset lifecycle.
    let media_tool = Arc::new(
        media::MediaTool::new(
            router.clone(),
            managed_assets.clone(),
            limits.file_vision_max_bytes,
            limits.file_summary_timeout_secs,
            tool_output_cap(settings, "media", limits.max_observation_chars),
        )
        .with_stt_client(stt_client.clone())
        .with_ocr_client(ocr_client)
        .with_image_gen_client(image_gen_client)
        .with_confidence_thresholds(
            media_config.ocr.min_confidence,
            media_config.stt.min_confidence,
        )
        .with_capabilities(vision_available, transcribe_available),
    );
    tools.push(media_tool.clone());
    tools.push(Arc::new(
        audio::AudioTool::with_tts(audio_pipeline, tts_client)
            .with_managed_assets(managed_assets.clone())
            .with_capabilities(record_available, transcribe_available)
            .with_media_tool(media_tool.clone()),
    ));
    tools.push(Arc::new(
        files::FilesTool::new(
            router.clone(),
            tool_output_cap(settings, "files", limits.max_observation_chars),
            limits.file_read_max_chars,
            limits.file_line_span,
            limits.file_max_line_chars,
            limits.file_summary_input_chars,
            limits.file_max_list_entries,
            limits.file_max_byte_read,
            limits.file_summary_timeout_secs,
            file_search::FileSearchEngine::new(
                limits.search_snippet_chars,
                limits.search_max_results,
                limits.search_max_file_size_bytes,
                limits.search_window_bytes,
            ),
            managed_assets.clone(),
        )
        .with_media_tool(media_tool.clone()),
    ));
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
        live_outputs,
        max_output_chars: tool_output_cap(settings, "shell", limits.max_observation_chars),
        default_shell: default_shell.as_str().into(),
    }));
    tools.push(Arc::new(actions::ActionsTool {
        actions: background_actions,
    }));
    tools.push(Arc::new(input::InputTool));
    tools.push(Arc::new(scheduled_action::ScheduledActionTool {
        center: scheduled_actions,
        // Weak registry probe so `set` can validate tool_name / risk at
        // schedule time; taken before `registry` is moved into SelfTool.
        registry: Some(registry.probe()),
    }));
    tools.push(Arc::new(system::SystemTool {
        max_output_chars: tool_output_cap(settings, "system", limits.max_observation_chars),
    }));
    tools.push(Arc::new(
        window::WindowTool::new(managed_assets).with_media_tool(media_tool),
    ));
    tools.push(Arc::new(http::HttpTool {
        max_retries: limits.network_max_retries,
        backoff_base_secs: limits.network_backoff_base_secs,
        max_body_bytes: limits.network_max_body_bytes,
    }));
    tools.push(Arc::new(notify::NotifyTool));
    // Cross-session messaging / peer collab: single `agent` tool over the
    // shared file bus. Agents lazily register on first call; spawn needs the
    // desktop-wired spawner slot (None in headless → tool errors clearly).
    let messaging_service = Arc::new(crate::messaging_service::MessagingService::default_root());
    tools.push(Arc::new(messaging::AgentTool::new(
        messaging_service,
        agent_spawner,
    )));
    let max_tools = limits.max_tools_per_request.max(1);
    if has_enabled_skills {
        tools.push(Arc::new(load_skill::LoadSkillTool {
            skills_engine: skills_engine.clone(),
            skill_runner: skill_runner.clone(),
            registry: registry.clone(),
            session_catalog: session_catalog.clone(),
            max_tools_per_request: max_tools,
        }));
    }
    if has_enabled_mcp {
        tools.push(Arc::new(load_mcp::LoadMcpTool {
            mcp_manager: mcp_manager.clone(),
            server_configs: server_configs.clone(),
            registry: registry.clone(),
            session_catalog,
            max_tools_per_request: max_tools,
        }));
    }
    if let Some(ctx) = self_context {
        let config_admin = Arc::new(admin::new_config_admin_tool(admin::ConfigAdminContext {
            config_service: ctx.config_service.clone(),
            set_log_level: ctx.set_log_level.clone(),
        }));
        // Facts memory needs the DB; like SelfTool it only registers once the
        // desktop shell wires the app context (headless builds skip it).
        tools.push(Arc::new(memory::MemoryTool::new(
            ctx.db.clone(),
            memory_recall,
        )));
        let tool = Arc::new(self_tool::SelfTool::new(
            ctx,
            skills_engine.clone(),
            mcp_manager.clone(),
            server_configs.clone(),
            registry,
            limits.self_tool_max_instructions_bytes,
            limits.self_tool_max_script_bytes,
        ));
        self_tool_arc = Some(tool.clone());
        // The broad native surface is retained only for app commands. The
        // model receives capability-scoped adapters, each with its own
        // schema and AuthorizationEngine permission key.
        for capability in admin::AdminCapability::ALL {
            if capability == admin::AdminCapability::Config {
                tools.push(config_admin.clone());
            } else {
                tools.push(Arc::new(admin::AdminCapabilityTool::new(
                    tool.clone(),
                    capability,
                )));
            }
        }
    }
    self_tool_arc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tool;
    use haven_common::config::ToolConfig;
    use serde_json::json;

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

    #[test]
    fn operation_schemas_reject_cross_operation_arguments() {
        let clipboard = clipboard::ClipboardTool::new(
            Arc::new(clipboard::ClipboardHistory::new(8)),
            2_000,
            4,
            8,
            500,
        );
        let audio = audio::AudioTool::new(None);
        let process = process::ProcessTool::default();
        let http = http::HttpTool::default();
        let input = input::InputTool;
        let system = system::SystemTool::default();
        let window = window::WindowTool::new(crate::ManagedAssetRegistry::default());
        let schedule = scheduled_action::ScheduledActionTool {
            center: Arc::new(scheduled_action::ScheduledActionCenter::new()),
            registry: None,
        };
        let cases: Vec<(&dyn Tool, serde_json::Value, serde_json::Value)> = vec![
            (
                &audio,
                json!({"operation": "volume_set", "volume": 0.5}),
                json!({"operation": "volume_set"}),
            ),
            (
                &process,
                json!({"operation": "kill", "pid": 1}),
                json!({"operation": "kill", "command": "taskkill"}),
            ),
            (
                &clipboard,
                json!({"operation": "write", "content": "x"}),
                json!({"operation": "write"}),
            ),
            (
                &http,
                json!({"url": "https://example.com"}),
                json!({"method": "POST", "url": "https://example.com", "body": 1}),
            ),
            (
                &input,
                json!({"operation": "click", "x": 1, "y": 2}),
                json!({"operation": "click", "x": 1}),
            ),
            (
                &system,
                json!({"scope": "env", "operation": "set", "name": "HAVEN_TEST", "value": "x"}),
                json!({"scope": "env", "operation": "set", "name": "HAVEN_TEST"}),
            ),
            (
                &window,
                json!({"operation": "wait", "condition": "title_contains", "text": "Haven"}),
                json!({"operation": "focus"}),
            ),
            (
                &schedule,
                json!({"operation": "set", "delay_secs": 5, "body": "check"}),
                json!({"operation": "cancel"}),
            ),
        ];
        for (tool, valid, invalid) in cases {
            assert!(
                tool.validate_input(&valid).is_ok(),
                "{} rejected {valid}",
                tool.name()
            );
            assert!(
                tool.validate_input(&invalid).is_err(),
                "{} accepted {invalid}",
                tool.name()
            );
        }
    }
}
