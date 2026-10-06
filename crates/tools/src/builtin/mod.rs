pub mod admin;
mod admin_support;
pub mod ask;
pub mod checklist;
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
mod media_audio;
pub mod memory;
pub mod messaging;
pub mod notify;
mod operation_contract;
mod power;
pub mod preferences;
pub mod process;
mod registry;
pub mod scheduled_tool_run;
pub mod shell;
pub mod system;
pub mod tool_catalog;
pub mod tool_runs;
pub mod window;

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use self::operation_contract::{OperationContract, operation_contract};
use crate::ToolRegistry;
use crate::ToolRunService;
use crate::operation_view::{
    OperationPolicyRule, OperationSpec, OperationViewTool, split_operation_schema,
    split_scope_operation_schema,
};
use crate::prompts as tool_prompts;
use crate::registry::{DeferredToolCatalog, SessionToolOverlay};
use crate::skill_runner::SkillRunner;
use crate::{
    ConfirmationRequirement, OperationIdempotency, OperationPolicy, ToolBox, ToolConcurrency,
    ToolOperationScope,
};
use haven_common::config::RequestKind;
use haven_common::tools::{ToolCatalogGroup, ToolPresentation, ToolPrompt};
use haven_common::types::RiskLevel;
use haven_mcp::McpManager;
use haven_skills::SkillsEngine;

/// Resolve capability truth for the model-backed media operations. A
/// configured role is not enough: routing may fall back to another role and
/// the selected adapter may explicitly reject the representation.
pub(crate) async fn resolve_media_capabilities(
    router: Option<&Arc<haven_llm::LlmRouter>>,
    dedicated_stt_available: bool,
) -> media::MediaCapabilities {
    let Some(router) = router else {
        return media::MediaCapabilities {
            transcribe: dedicated_stt_available,
            ..media::MediaCapabilities::default()
        };
    };

    let vision_available = router.is_request_configured(RequestKind::Vision).await
        && router
            .capability_profile_for_request(RequestKind::Vision)
            .image
            .is_supported();

    let transcribe_available = if router
        .is_request_configured(RequestKind::Transcription)
        .await
    {
        let configured = router
            .is_request_configured(RequestKind::Transcription)
            .await;
        let profile = router.capability_profile_for_request(RequestKind::Transcription);
        let style = {
            let config = router.config().await;
            config
                .route(RequestKind::Transcription)
                .map(|model| haven_llm::adapters::api_style_for(&model.endpoint))
                .unwrap_or("openai-chat")
        };
        let unknown_custom_route = profile.audio == haven_common::media::CapabilitySupport::Unknown
            && !haven_llm::adapters::is_known_api_style(style);
        configured
            && (profile.audio.is_supported()
                || haven_llm::is_stt_only_style(style)
                || unknown_custom_route)
    } else {
        false
    };

    media::MediaCapabilities {
        describe: vision_available,
        transcribe: transcribe_available || dedicated_stt_available,
        ..media::MediaCapabilities::default()
    }
}

pub use crate::tool_runtime::{MemoryRecallPort, MemoryRecallSlot, new_memory_recall_slot};
pub use admin::{
    AdminCapability, AdminContext, AdminOperationError, AdminRequest, AdminSurfaces,
    ConfigAdminContext, ConfigAdminOperation, ConfigAdminTool, ConfigOperationArgs,
    ConfigOperationError, ConfigOperationOutput, ConfigViewOutput, DiagnosticsOperationArgs,
    LogLevelOutput, McpOperationArgs, McpRefreshAction, McpRefreshPlan, McpRefreshTarget,
    NativeMcpOperationArgs, SkillsOperationArgs, ToolsOperationArgs,
};
pub use media::{MediaTranscriptionResult, MediaTranscriptionStatus};
pub use memory::MemoryTool;
pub use messaging::AgentTool;
pub use scheduled_tool_run::ScheduleTool;

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

/// Provider and host dependencies used by media producers/consumers. Media
/// tools are rebuilt as one generation so a settings update cannot combine a
/// new router with stale specialized clients.
pub struct MediaDeps {
    pub router: Option<Arc<haven_llm::LlmRouter>>,
    pub audio_pipeline: Option<Arc<haven_input::InputPipeline>>,
    pub stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    pub ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    pub image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    pub tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
    pub config: haven_common::config::MediaConfig,
    pub(crate) capabilities: media::MediaCapabilities,
}

/// Long-running action dependencies. Background processes and scheduled
/// timers are admitted and transitioned by the same ToolRunService state
/// machine; only their short-lived workers differ.
pub struct ToolRunDeps {
    pub live_outputs: Arc<crate::live_output::LiveOutputHub>,
    pub service: Arc<ToolRunService>,
}

/// Complete dependency object for constructing the builtin catalog. Keeping
/// this boundary as a value object makes additions explicit and prevents the
/// constructor from growing another positional argument.
pub struct BuiltinContext {
    pub skills_engine: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub mcp_manager: Arc<McpManager>,
    pub server_configs: Arc<RwLock<HashMap<String, haven_common::McpServerConfig>>>,
    pub registry: ToolRegistry,
    pub session_tool_overlay: SessionToolOverlay,
    pub deferred_catalog: DeferredToolCatalog,
    pub settings: HashMap<String, haven_common::config::ToolConfig>,
    pub limits: haven_common::config::ContextLimitsConfig,
    pub default_shell: haven_common::types::ShellChoice,
    pub clipboard_history: Arc<clipboard::ClipboardHistory>,
    pub admin_context: Option<AdminContext>,
    pub messaging_service: Arc<haven_messaging::MessagingService>,
    pub memory_recall: MemoryRecallSlot,
    pub managed_assets: crate::ManagedAssetRegistry,
    pub media: MediaDeps,
    pub tool_runs: ToolRunDeps,
}

pub async fn register_builtin_tools(
    tools: &mut Vec<ToolBox>,
    context: BuiltinContext,
) -> Option<Arc<admin::AdminSurfaces>> {
    let BuiltinContext {
        skills_engine,
        skill_runner,
        mcp_manager,
        server_configs,
        registry,
        session_tool_overlay,
        deferred_catalog,
        settings,
        limits,
        default_shell,
        clipboard_history,
        admin_context,
        messaging_service,
        memory_recall,
        managed_assets,
        media:
            MediaDeps {
                router,
                audio_pipeline,
                stt_client,
                ocr_client,
                image_gen_client,
                tts_client,
                config: media_config,
                capabilities,
            },
        tool_runs:
            ToolRunDeps {
                live_outputs,
                service: tool_run_service,
            },
    } = context;
    let settings = &settings;
    let limits = &limits;
    let mut admin_surfaces: Option<Arc<admin::AdminSurfaces>> = None;
    let audio_runtime = Arc::new(
        media_audio::AudioRuntime::with_tts(audio_pipeline, tts_client)
            .with_managed_assets(managed_assets.clone()),
    );
    tools.push(Arc::new(ask::typed_adapter()));
    tools.push(Arc::new(tool_catalog::ToolCatalogTool {
        deferred_catalog: deferred_catalog.clone(),
        registry: registry.clone(),
        session_tool_overlay: session_tool_overlay.clone(),
        max_tools_per_request: limits.max_tools_per_request.max(1),
        mcp_manager: mcp_manager.clone(),
        server_configs: server_configs.clone(),
    }));
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
        .with_stt_timeout_secs(media_config.stt.timeout_secs)
        .with_capabilities(capabilities)
        .with_audio_runtime(audio_runtime),
    );
    add_operation_views(tools, media_tool.clone(), settings, MEDIA_OPERATION_VIEWS);
    let files_tool: ToolBox = Arc::new(
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
        .with_max_write_bytes(limits.file_max_byte_read)
        .with_media_tool(media_tool.clone()),
    );
    for contract in operation_specs(limits.search_max_results) {
        if !contract.name.starts_with("files.") {
            continue;
        }
        tools.push(OperationViewTool::new(files_tool.clone(), contract));
    }
    add_operation_views(tools, files_tool.clone(), settings, FILE_OPERATION_VIEWS);
    let process_tool: ToolBox = Arc::new(process::ProcessTool {
        max_output_chars: tool_output_cap(settings, "process", limits.max_observation_chars),
    });
    let clipboard_tool: ToolBox = Arc::new(
        clipboard::ClipboardTool::new(
            clipboard_history,
            tool_output_cap(settings, "clipboard", limits.max_observation_chars),
            limits.clipboard_history_entries,
            limits.clipboard_history_max_entries,
            limits.clipboard_entry_max_chars,
        )
        .with_managed_assets(managed_assets.clone()),
    );
    tools.push(Arc::new(shell::ShellTool {
        tool_runs: tool_run_service.clone(),
        live_outputs,
        max_output_chars: tool_output_cap(settings, "shell", limits.max_observation_chars),
        default_shell: default_shell.as_str().into(),
    }));
    let tool_runs_tool: ToolBox = Arc::new(tool_runs::ToolRunsTool {
        tool_runs: tool_run_service.clone(),
    });
    let input_tool: ToolBox = Arc::new(input::InputTool);
    let schedule_tool: ToolBox = Arc::new(scheduled_tool_run::ScheduleTool {
        service: tool_run_service,
        // Weak registry probe so `set` can validate tool_name / risk at
        // schedule time; taken before the registry is shared with admin services.
        registry: Some(registry.probe()),
    });
    let preferences_tool: ToolBox = Arc::new(preferences::PreferencesTool::default());
    let checklist_tool: ToolBox = Arc::new(checklist::ChecklistTool::default());
    let window_tool: ToolBox =
        Arc::new(window::WindowTool::new(managed_assets).with_media_tool(media_tool));
    add_operation_views(tools, process_tool, settings, PROCESS_OPERATION_VIEWS);
    add_operation_views(tools, clipboard_tool, settings, CLIPBOARD_OPERATION_VIEWS);
    add_operation_views(tools, input_tool, settings, INPUT_OPERATION_VIEWS);
    add_operation_views(tools, window_tool, settings, WINDOW_OPERATION_VIEWS);
    let system_tool: ToolBox = Arc::new(system::SystemTool::default().with_max_output_chars(
        tool_output_cap(settings, "system", limits.max_observation_chars),
    ));
    let contract = operation_specs(limits.search_max_results)
        .into_iter()
        .find(|contract| contract.name == "system.info")
        .expect("system.info operation view contract");
    tools.push(OperationViewTool::new(system_tool.clone(), contract));
    add_system_scope_operation_views(tools, system_tool.clone(), settings);
    let contract = operation_spec(
        &system_tool,
        "system.display",
        tool_prompts::operation_text("system.display").description,
        vec![("scope".into(), serde_json::json!("display"))],
        system_display_schema(),
        "system",
        "monitor",
        tool_prompts::operation_text("system.display").when_to_use,
    );
    tools.push(OperationViewTool::new(system_tool, contract));
    tools.push(Arc::new(http::HttpTool {
        max_retries: limits.network_max_retries,
        backoff_base_secs: limits.network_backoff_base_secs,
        max_body_bytes: limits.network_max_body_bytes,
        allowed_domains: settings
            .get("http")
            .map(|config| config.allowed_domains.clone())
            .unwrap_or_default(),
    }));
    tools.push(Arc::new(notify::typed_adapter()));
    // Cross-session messaging / peer collab: one aggregate implementation over
    // the service-owned transport and session mailbox. Agents lazily register
    // on first call; the desktop runtime is an optional typed service port.
    let agent_tool: ToolBox = Arc::new(messaging::AgentTool::new(messaging_service));
    add_operation_views(tools, agent_tool, settings, AGENT_OPERATION_VIEWS);
    let max_tools = limits.max_tools_per_request.max(1);
    // Skills are executable adapters in the deferred catalog. They become
    // provider-visible only after the model explicitly loads one or more.
    let skill_runner = skill_runner.read().await.clone();
    for skill in skills_engine
        .list_skills()
        .await
        .into_iter()
        .filter(|skill| skill.enabled() && skill.has_script())
    {
        tools.push(Arc::new(crate::SkillToolAdapter::new(
            Arc::new(skill),
            skill_runner.clone(),
        )));
    }
    // Keep the control-plane surface stable even when no optional Skill or
    // MCP provider is configured. The loader returns a precise unavailable
    // result; the model should not have to infer capability availability from
    // a changing provider tool list.
    tools.push(Arc::new(load_skill::LoadSkillTool {
        deferred_catalog: deferred_catalog.clone(),
        registry: registry.clone(),
        session_tool_overlay: session_tool_overlay.clone(),
        max_tools_per_request: max_tools,
    }));
    tools.push(Arc::new(load_mcp::LoadMcpTool {
        mcp_manager: mcp_manager.clone(),
        server_configs: server_configs.clone(),
        registry: registry.clone(),
        session_tool_overlay,
        max_tools_per_request: max_tools,
    }));
    if let Some(ctx) = admin_context {
        // The composition root provides this typed capability explicitly;
        // headless builds can omit it without exposing the database here.
        let memory_facts = ctx.memory_facts.clone();
        let memory_tool: ToolBox = Arc::new(memory::MemoryTool::new(memory_facts, memory_recall));
        add_operation_views(tools, memory_tool, settings, MEMORY_OPERATION_VIEWS);
        let surfaces = Arc::new(admin::AdminSurfaces::new(
            ctx,
            skills_engine.clone(),
            mcp_manager.clone(),
            server_configs.clone(),
            registry.clone(),
            limits.self_tool_max_instructions_bytes,
            limits.self_tool_max_script_bytes,
        ));
        for capability_tool in surfaces.tools() {
            let capability = capability_tool.name();
            add_admin_operation_views(tools, capability_tool, settings, &capability);
        }
        admin_surfaces = Some(surfaces);
    }
    add_tool_run_views(tools, tool_runs_tool, settings);
    add_operation_views(tools, schedule_tool, settings, SCHEDULE_OPERATION_VIEWS);
    add_operation_views(
        tools,
        preferences_tool,
        settings,
        PREFERENCE_OPERATION_VIEWS,
    );
    add_operation_views(tools, checklist_tool, settings, CHECKLIST_OPERATION_VIEWS);
    admin_surfaces
}

fn files_read_text_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string", "minLength": 1, "description": "File path to read" },
            "asset_id": { "type": "string", "minLength": 1, "description": "Opaque id of a user attachment" },
            "offset": { "type": "integer", "minimum": 0, "description": "Byte offset" },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum bytes" },
            "start_line": { "type": "integer", "minimum": 1, "description": "1-based first line" },
            "end_line": { "type": "integer", "minimum": 0, "description": "1-based last line; omit for the default span" },
            "focus": { "type": "string", "description": "Optional focus for a rich media source" }
        },
        "oneOf": [{ "required": ["path"] }, { "required": ["asset_id"] }]
    })
}

fn files_outline_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string", "minLength": 1, "description": "Source or Markdown file path" },
            "start_line": { "type": "integer", "minimum": 1, "description": "1-based line to continue from next_start_line" },
            "max_symbols": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum headings/declarations" }
        },
        "required": ["path"]
    })
}

fn files_summary_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string", "minLength": 1, "description": "Text file path" },
            "asset_id": { "type": "string", "minLength": 1, "description": "Opaque id of a user attachment" },
            "start_line": { "type": "integer", "minimum": 1 },
            "end_line": { "type": "integer", "minimum": 0 },
            "focus": { "type": "string", "maxLength": 2000 },
            "max_chars": { "type": "integer", "minimum": 1, "description": "Maximum source characters" }
        },
        "oneOf": [{ "required": ["path"] }, { "required": ["asset_id"] }]
    })
}

fn files_search_schema(max_results: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "root": { "type": "string", "minLength": 1, "description": "Directory or file path to search" },
            "pattern": { "type": "string", "minLength": 1, "description": "Filename glob or content regex" },
            "mode": { "type": "string", "enum": ["filename", "content"], "description": "Filename matching or text matching" },
            "max_depth": { "type": "integer", "minimum": 0 },
            "max_results": { "type": "integer", "minimum": 1, "maximum": max_results },
            "ignore_hidden": { "type": "boolean" },
            "max_file_size": { "type": "integer", "minimum": 0 },
            "start_line": { "type": "integer", "minimum": 1 },
            "end_line": { "type": "integer", "minimum": 0 }
        },
        "required": ["root", "pattern"]
    })
}

fn system_info_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "category": { "type": "string", "enum": ["overview", "cpu", "memory", "disk", "os", "network", "user", "locale", "all"] }
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn operation_spec(
    inner: &ToolBox,
    name: &'static str,
    description: &'static str,
    fixed: Vec<(String, Value)>,
    schema: Value,
    renderer: &'static str,
    icon: &'static str,
    prompt: &'static str,
) -> OperationSpec {
    let metadata = operation_contract(name);
    let policy_input = Value::Object(fixed.iter().cloned().collect());
    // Freeze the aggregate call shape. Idempotency and effect come from the
    // contract, independently of concurrency; sensitive/network reads still
    // require disclosure confirmation.
    let mut risk_level = inner.risk_level(&policy_input);
    if let Some(override_risk) = metadata.risk_override {
        risk_level = override_risk;
    }
    let concurrency = inner.concurrency(&policy_input);
    let (effect, data_sensitivity, network_access) =
        operation_policy_attributes(&metadata, concurrency.clone());
    let policy = OperationPolicy {
        risk_level,
        capability: name.into(),
        confirmation: crate::tool_contract::confirmation_for(
            risk_level,
            matches!(effect, crate::OperationEffect::ReadOnly),
        ),
        idempotency: metadata.idempotency,
        scope: inner.operation_scope(&policy_input),
        concurrency,
        effect,
        data_sensitivity,
        network_access,
    };
    OperationSpec {
        name: name.into(),
        description: description.into(),
        fixed,
        schema,
        policy,
        policy_rule: None,
        catalog_group: metadata.catalog_group,
        presentation: ToolPresentation {
            label: metadata.label.into(),
            renderer: renderer.into(),
            icon: icon.into(),
            represented_source: haven_common::tools::ToolSource::Builtin,
        },
        prompt: ToolPrompt {
            when_to_use: prompt.into(),
            when_not_to_use:
                "Use only for this named operation; its operation/scope fields are fixed.".into(),
            key_operations: vec![name.into()],
        },
        identity: None,
    }
}

/// Keep data/effect policy independent from scheduling concurrency. A shared
/// resource can describe either a reader or a writer; the operation contract
/// decides which effect applies, while network access remains separately
/// classified for disclosure and network policy.
fn operation_policy_attributes(
    contract: &OperationContract,
    concurrency: ToolConcurrency,
) -> (
    crate::OperationEffect,
    crate::DataSensitivity,
    crate::NetworkAccess,
) {
    let (concurrency_effect, data_sensitivity, inferred_network_access) =
        crate::tool_contract::operation_attributes(contract.name, concurrency);
    let effect = if contract.read_only {
        crate::OperationEffect::ReadOnly
    } else {
        concurrency_effect
    };
    let network_access = contract
        .network_access_override
        .unwrap_or(inferred_network_access);
    (effect, data_sensitivity, network_access)
}

/// The operation-view catalog is the backend source of truth for the model
/// schema, execution policy and the cross-boundary UI/prompt identifiers.
fn operation_specs(max_results: usize) -> Vec<OperationSpec> {
    vec![
        OperationSpec {
            name: "files.read".into(),
            description: tool_prompts::operation_text("files.read").description.into(),
            fixed: vec![("operation".into(), serde_json::json!("read"))],
            schema: files_read_text_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: "files.read".into(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::None,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::System,
            presentation: ToolPresentation {
                label: operation_contract("files.read").label.into(),
                renderer: "files".into(),
                icon: "file".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.read").when_to_use.into(),
                when_not_to_use: "Use a different operation view for another action; do not add an operation field.".into(),
                key_operations: vec!["files.read".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "files.outline".into(),
            description: tool_prompts::operation_text("files.outline").description.into(),
            fixed: vec![("operation".into(), serde_json::json!("outline"))],
            schema: files_outline_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: "files.outline".into(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::None,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::System,
            presentation: ToolPresentation {
                label: operation_contract("files.outline").label.into(),
                renderer: "files".into(),
                icon: "fileSearch".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.outline").when_to_use.into(),
                when_not_to_use: "Use a different operation view for another action; do not add an operation field.".into(),
                key_operations: vec!["files.outline".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "files.summary".into(),
            description: tool_prompts::operation_text("files.summary").description.into(),
            fixed: vec![("operation".into(), serde_json::json!("summary"))],
            schema: files_summary_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: "files.summary".into(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::Public,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::System,
            presentation: ToolPresentation {
                label: operation_contract("files.summary").label.into(),
                renderer: "files".into(),
                icon: "file".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.summary").when_to_use.into(),
                when_not_to_use: "Use a different operation view for another action; do not add an operation field.".into(),
                key_operations: vec!["files.summary".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "files.search".into(),
            description: tool_prompts::operation_text("files.search").description.into(),
            fixed: vec![("operation".into(), serde_json::json!("search"))],
            schema: files_search_schema(max_results),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: "files.search".into(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::None,
            },
            policy_rule: Some(OperationPolicyRule::ContentSearchMedium),
            catalog_group: ToolCatalogGroup::System,
            presentation: ToolPresentation {
                label: operation_contract("files.search").label.into(),
                renderer: "files.search".into(),
                icon: "search".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.search").when_to_use.into(),
                when_not_to_use: "Use a different operation view for another action; do not add an operation field.".into(),
                key_operations: vec!["files.search".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "system.info".into(),
            description: tool_prompts::operation_text("system.info").description.into(),
            fixed: vec![("scope".into(), serde_json::json!("info"))],
            schema: system_info_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Safe,
                capability: "system.info".into(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Global,
                concurrency: ToolConcurrency::ReadOnly,
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::None,
                network_access: crate::NetworkAccess::None,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::System,
            presentation: ToolPresentation {
                label: operation_contract("system.info").label.into(),
                renderer: "system".into(),
                icon: "cpu".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("system.info").when_to_use.into(),
                when_not_to_use: "Use a different operation view for another action; do not add an operation field.".into(),
                key_operations: vec!["system.info".into()],
            },
            identity: None,
        },
    ]
}

#[derive(Debug, Clone, Copy)]
struct SplitOperationSpec {
    name: &'static str,
    operation: &'static str,
    renderer: &'static str,
    icon: &'static str,
}

macro_rules! split_spec {
    ($name:literal, $operation:literal, $renderer:literal, $icon:literal) => {
        SplitOperationSpec {
            name: $name,
            operation: $operation,
            renderer: $renderer,
            icon: $icon,
        }
    };
}

const FILE_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("files.inspect", "inspect", "files", "fileSearch"),
    split_spec!("files.stat", "stat", "files", "fileSearch"),
    split_spec!("files.hash", "hash", "files", "fileSearch"),
    split_spec!("files.write", "write", "files", "file"),
    split_spec!("files.create_dir", "create_dir", "files", "folder"),
    split_spec!("files.edit", "edit", "files", "edit"),
    split_spec!("files.patch", "patch", "files", "edit"),
    split_spec!("files.copy", "copy", "files", "copy"),
    split_spec!("files.move", "move", "files", "move"),
    split_spec!("files.delete", "delete", "files", "delete"),
    split_spec!("files.list", "list", "files", "folder"),
];

const PROCESS_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("process.list", "list", "process", "activity"),
    split_spec!("process.kill", "kill", "process", "activity"),
];

const CLIPBOARD_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("clipboard.read", "read", "clipboard", "clipboard"),
    split_spec!("clipboard.write", "write", "clipboard", "clipboard"),
    split_spec!("clipboard.history", "history", "clipboard", "clipboard"),
];

const INPUT_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("input.type", "type", "input", "keyboard"),
    split_spec!("input.type_element", "type_element", "input", "keyboard"),
    split_spec!("input.key", "key", "input", "keyboard"),
    split_spec!("input.click", "click", "input", "mouse"),
    split_spec!("input.click_element", "click_element", "input", "mouse"),
    split_spec!("input.move", "move", "input", "mouse"),
    split_spec!("input.scroll", "scroll", "input", "mouse"),
];

const WINDOW_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("window.list", "list", "window", "monitor"),
    split_spec!("window.foreground", "foreground", "window", "monitor"),
    split_spec!("window.focus", "focus", "window", "monitor"),
    split_spec!("window.close", "close", "window", "monitor"),
    split_spec!("window.screenshot", "screenshot", "window", "image"),
    split_spec!("window.ocr", "ocr", "window", "image"),
    split_spec!("window.ui_tree", "ui_tree", "window", "account_tree"),
    split_spec!("window.observe", "observe", "window", "image"),
    split_spec!("window.invoke", "invoke", "window", "play"),
    split_spec!("window.set_value", "set_value", "window", "edit"),
    split_spec!("window.toggle", "toggle", "window", "settings"),
    split_spec!("window.select", "select", "window", "list"),
    split_spec!("window.wait", "wait", "window", "hourglass"),
];

const MEDIA_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("media.inspect", "inspect", "media", "image"),
    split_spec!("media.describe", "describe", "media", "image"),
    split_spec!("media.ocr", "ocr", "media", "image"),
    split_spec!("media.transcribe", "transcribe", "media", "mic"),
    split_spec!("media.extract", "extract", "media", "fileText"),
    split_spec!("media.render", "render", "media", "fileText"),
    split_spec!("media.generate", "generate", "media", "image"),
    split_spec!("media.record", "record", "media", "mic"),
    split_spec!("media.play", "play", "media", "volumeUp"),
    split_spec!("media.speak", "speak", "media", "volumeUp"),
    split_spec!("media.volume_get", "volume_get", "media", "volumeUp"),
    split_spec!("media.volume_set", "volume_set", "media", "volumeUp"),
    split_spec!("media.mute_get", "mute_get", "media", "volumeOff"),
    split_spec!("media.mute_set", "mute_set", "media", "volumeOff"),
];

const MEMORY_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("memory.search", "search", "memory", "memory"),
    split_spec!("memory.list", "list", "memory", "memory"),
    split_spec!("memory.remember", "remember", "memory", "memory"),
    split_spec!("memory.forget", "forget", "memory", "memory"),
    split_spec!("memory.recall", "recall", "memory", "memory"),
];

const AGENT_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("agent.list", "list", "agent", "users"),
    split_spec!("agent.children", "children", "agent", "users"),
    split_spec!("agent.history", "history", "agent", "users"),
    split_spec!("agent.inbox", "inbox", "agent", "users"),
    split_spec!("agent.ack", "ack", "agent", "users"),
    split_spec!("agent.send", "send", "agent", "users"),
    split_spec!("agent.reply", "reply", "agent", "users"),
    split_spec!("agent.profile", "profile", "agent", "users"),
    split_spec!("agent.request", "request", "agent", "users"),
    split_spec!("agent.spawn", "spawn", "agent", "users"),
    split_spec!("agent.status", "status", "agent", "users"),
    split_spec!("agent.join", "join", "agent", "hourglass"),
    split_spec!("agent.wait", "wait", "agent", "users"),
    split_spec!("agent.stop", "stop", "agent", "users"),
    split_spec!("agent.collect", "collect", "agent", "fileText"),
];

const ACTION_OPERATION_VIEWS: &[SplitOperationSpec] = &[split_spec!(
    "tool_runs.cancel",
    "cancel",
    "tool_runs",
    "clock"
)];

const SCHEDULE_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("schedule.set", "set", "schedule", "bell"),
    split_spec!("schedule.list", "list", "schedule", "bell"),
    split_spec!("schedule.cancel", "cancel", "schedule", "bell"),
];

const PREFERENCE_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("preferences.get", "get", "preferences", "settings"),
    split_spec!("preferences.set", "set", "preferences", "settings"),
    split_spec!("preferences.clear", "clear", "preferences", "settings"),
    split_spec!("preferences.list", "list", "preferences", "settings"),
];

const CHECKLIST_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("checklist.list", "list", "checklist", "checklist"),
    split_spec!("checklist.add", "add", "checklist", "checklist"),
    split_spec!("checklist.update", "update", "checklist", "checklist"),
    split_spec!("checklist.remove", "remove", "checklist", "checklist"),
    split_spec!("checklist.clear", "clear", "checklist", "checklist"),
];

const SYSTEM_SCOPE_OPERATION_VIEWS: &[(&str, &str, &str, &str, &str)] = &[
    ("system.env.list", "env", "list", "system", "terminal"),
    ("system.env.get", "env", "get", "system", "terminal"),
    ("system.env.set", "env", "set", "system", "terminal"),
    ("system.env.unset", "env", "unset", "system", "terminal"),
    (
        "system.registry.list",
        "registry",
        "list",
        "system",
        "settings",
    ),
    (
        "system.registry.get",
        "registry",
        "get",
        "system",
        "settings",
    ),
    (
        "system.registry.set",
        "registry",
        "set",
        "system",
        "settings",
    ),
    (
        "system.registry.delete_value",
        "registry",
        "delete_value",
        "system",
        "settings",
    ),
    (
        "system.registry.delete_key",
        "registry",
        "delete_key",
        "system",
        "settings",
    ),
    (
        "system.power.status",
        "power",
        "status",
        "system",
        "battery",
    ),
    ("system.power.lock", "power", "lock", "system", "lock"),
    ("system.power.sleep", "power", "sleep", "system", "sleep"),
    (
        "system.power.hibernate",
        "power",
        "hibernate",
        "system",
        "sleep",
    ),
];

const ADMIN_OPERATION_VIEWS: &[(&str, &str, &str, &str, &str)] = &[
    (
        "haven.diagnostics.status",
        "haven_diagnostics",
        "status",
        "settings",
        "settings",
    ),
    (
        "haven.diagnostics.logs_tail",
        "haven_diagnostics",
        "logs_tail",
        "settings",
        "settings",
    ),
    (
        "haven.diagnostics.sessions",
        "haven_diagnostics",
        "sessions",
        "settings",
        "settings",
    ),
    (
        "haven.diagnostics.errors",
        "haven_diagnostics",
        "errors",
        "settings",
        "settings",
    ),
    (
        "haven.config.config_get",
        "haven_config",
        "config_get",
        "settings",
        "settings",
    ),
    (
        "haven.config.logs_level",
        "haven_config",
        "logs_level",
        "settings",
        "settings",
    ),
    (
        "haven.skills.skills_list",
        "haven_skills",
        "skills_list",
        "settings",
        "sparkles",
    ),
    (
        "haven.skills.skill_enable",
        "haven_skills",
        "skill_enable",
        "settings",
        "sparkles",
    ),
    (
        "haven.skills.skill_disable",
        "haven_skills",
        "skill_disable",
        "settings",
        "sparkles",
    ),
    (
        "haven.skills.skill_create",
        "haven_skills",
        "skill_create",
        "settings",
        "sparkles",
    ),
    (
        "haven.tools.tool_enable",
        "haven_tools",
        "tool_enable",
        "settings",
        "settings",
    ),
    (
        "haven.tools.tool_disable",
        "haven_tools",
        "tool_disable",
        "settings",
        "settings",
    ),
    (
        "haven.mcp.mcp_list",
        "haven_mcp",
        "mcp_list",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_connect",
        "haven_mcp",
        "mcp_connect",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_disconnect",
        "haven_mcp",
        "mcp_disconnect",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_add",
        "haven_mcp",
        "mcp_add",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_update",
        "haven_mcp",
        "mcp_update",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_toggle",
        "haven_mcp",
        "mcp_toggle",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_remove",
        "haven_mcp",
        "mcp_remove",
        "settings",
        "network",
    ),
    (
        "haven.mcp.mcp_reload",
        "haven_mcp",
        "mcp_reload",
        "settings",
        "network",
    ),
];

fn add_operation_views(
    tools: &mut Vec<ToolBox>,
    inner: ToolBox,
    _settings: &HashMap<String, haven_common::config::ToolConfig>,
    specs: &[SplitOperationSpec],
) {
    let schema = inner.input_schema();
    for spec in specs {
        let Some(view_schema) = split_operation_schema(&schema, spec.operation) else {
            continue;
        };
        let text = tool_prompts::operation_text(spec.name);
        let contract = operation_spec(
            &inner,
            spec.name,
            text.description,
            vec![("operation".into(), serde_json::json!(spec.operation))],
            view_schema,
            spec.renderer,
            spec.icon,
            text.when_to_use,
        );
        tools.push(OperationViewTool::new(inner.clone(), contract));
    }
}

fn add_system_scope_operation_views(
    tools: &mut Vec<ToolBox>,
    inner: ToolBox,
    _settings: &HashMap<String, haven_common::config::ToolConfig>,
) {
    let schema = inner.input_schema();
    for (name, scope, operation, renderer, icon) in SYSTEM_SCOPE_OPERATION_VIEWS {
        let Some(view_schema) = split_scope_operation_schema(&schema, scope, operation) else {
            continue;
        };
        let text = tool_prompts::operation_text(name);
        let fixed = vec![
            ("scope".into(), serde_json::json!(scope)),
            ("operation".into(), serde_json::json!(operation)),
        ];
        let contract = operation_spec(
            &inner,
            name,
            text.description,
            fixed,
            view_schema,
            renderer,
            icon,
            text.when_to_use,
        );
        tools.push(OperationViewTool::new(inner.clone(), contract));
    }
}

fn tool_run_list_schema(inspect: bool) -> Value {
    if inspect {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "tool_run_id": { "type": "string", "minLength": 1 } },
            "required": ["tool_run_id"]
        })
    } else {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": { "status": { "type": "string", "enum": ["running", "completed", "failed", "cancelled"] } }
        })
    }
}

fn system_display_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false
    })
}

fn add_admin_operation_views(
    tools: &mut Vec<ToolBox>,
    inner: ToolBox,
    _settings: &HashMap<String, haven_common::config::ToolConfig>,
    capability_root: &str,
) {
    let schema = inner.input_schema();
    for (name, _root, operation, renderer, icon) in ADMIN_OPERATION_VIEWS
        .iter()
        .copied()
        .filter(|(_, root, ..)| *root == capability_root)
    {
        let Some(view_schema) = split_operation_schema(&schema, operation) else {
            continue;
        };
        let text = tool_prompts::operation_text(name);
        let contract = operation_spec(
            &inner,
            name,
            text.description,
            vec![("operation".into(), serde_json::json!(operation))],
            view_schema,
            renderer,
            icon,
            text.when_to_use,
        );
        tools.push(OperationViewTool::new(inner.clone(), contract));
    }
}

fn add_tool_run_views(
    tools: &mut Vec<ToolBox>,
    inner: ToolBox,
    _settings: &HashMap<String, haven_common::config::ToolConfig>,
) {
    for (name, schema) in [
        ("tool_runs.list", tool_run_list_schema(false)),
        ("tool_runs.inspect", tool_run_list_schema(true)),
    ] {
        let text = tool_prompts::operation_text(name);
        let contract = operation_spec(
            &inner,
            name,
            text.description,
            Vec::new(),
            schema,
            "tool_runs",
            "clock",
            text.when_to_use,
        );
        tools.push(OperationViewTool::new(inner.clone(), contract));
    }
    add_operation_views(tools, inner, _settings, ACTION_OPERATION_VIEWS);
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
    fn media_views_freeze_concurrency_without_a_per_call_asset() {
        let media = Arc::new(
            media::MediaTool::new(
                None,
                crate::ManagedAssetRegistry::default(),
                1024,
                10,
                2_000,
            )
            .with_capabilities(media::MediaCapabilities {
                describe: true,
                ocr: true,
                transcribe: true,
                generate: true,
                record: true,
                speak: true,
            }),
        );
        let mut tools = Vec::new();
        add_operation_views(&mut tools, media, &HashMap::new(), MEDIA_OPERATION_VIEWS);
        let policy = |name: &str| {
            tools
                .iter()
                .find(|tool| tool.name() == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .operation_policy(&json!({}))
                .concurrency
        };
        for name in [
            "media.inspect",
            "media.describe",
            "media.ocr",
            "media.transcribe",
            "media.extract",
            "media.render",
            "media.generate",
        ] {
            assert_eq!(
                policy(name),
                ToolConcurrency::Resource("media:unknown".into()),
                "{name}"
            );
        }
        for name in [
            "media.record",
            "media.play",
            "media.speak",
            "media.volume_get",
            "media.volume_set",
            "media.mute_get",
            "media.mute_set",
        ] {
            assert_eq!(
                policy(name),
                ToolConcurrency::Resource("media:audio-device".into()),
                "{name}"
            );
        }
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
        let media = media::MediaTool::new(
            None,
            crate::ManagedAssetRegistry::default(),
            1024,
            10,
            2_000,
        );
        let process = process::ProcessTool::default();
        let http = http::HttpTool::default();
        let input = input::InputTool;
        let system = system::SystemTool::default();
        let window = window::WindowTool::new(crate::ManagedAssetRegistry::default());
        let schedule = scheduled_tool_run::ScheduleTool {
            service: Arc::new(ToolRunService::new()),
            registry: None,
        };
        let cases: Vec<(&dyn Tool, serde_json::Value, serde_json::Value)> = vec![
            (
                &media,
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
                json!({"operation": "write", "format": "text", "text": "x"}),
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
                json!({"operation": "set", "delay_secs": 5, "body": "check", "mode": "tool", "tool_name": "notify"}),
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

    #[test]
    fn operation_specs_cover_policy_and_security_metadata() {
        let contracts = operation_specs(64);
        assert_eq!(
            contracts
                .iter()
                .map(|contract| contract.name.as_ref())
                .collect::<Vec<_>>(),
            vec![
                "files.read",
                "files.outline",
                "files.summary",
                "files.search",
                "system.info"
            ]
        );
        for contract in contracts {
            assert!(contract.schema.is_object(), "{} schema", contract.name);
            assert_eq!(contract.schema["additionalProperties"], json!(false));
            assert!(!contract.policy.capability.is_empty());
            assert!(!contract.presentation.renderer.is_empty());
            assert!(!contract.presentation.icon.is_empty());
            assert!(!contract.prompt.when_to_use.is_empty());

            let policy_input = json!(
                contract
                    .fixed
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<serde_json::Map<_, _>>()
            );
            assert_eq!(
                haven_common::types::permission_key(contract.name.as_ref(), &policy_input),
                contract.policy.capability.to_string(),
                "permission key drift for {}",
                contract.name
            );
            let matrix = crate::security::LOCAL_TOOL_SECURITY_MATRIX
                .iter()
                .find(|case| case.tool_name == contract.name && case.operation == contract.name)
                .unwrap_or_else(|| panic!("security matrix missing {}", contract.name));
            assert_eq!(matrix.risk_level, contract.policy.risk_level);
        }
    }

    #[test]
    fn admin_diagnostics_effect_and_network_policy_are_distinct() {
        let status = operation_contract("haven.diagnostics.status");
        let (status_effect, _, status_network) = operation_policy_attributes(
            &status,
            ToolConcurrency::SharedResource("haven:diagnostics".into()),
        );
        assert_eq!(status_effect, crate::OperationEffect::ReadOnly);
        assert_eq!(status_network, crate::NetworkAccess::Public);

        let errors = operation_contract("haven.diagnostics.errors");
        let (errors_effect, _, errors_network) = operation_policy_attributes(
            &errors,
            ToolConcurrency::SharedResource("haven:sessions".into()),
        );
        assert_eq!(errors_effect, crate::OperationEffect::ReadOnly);
        assert_eq!(errors_network, crate::NetworkAccess::None);
    }

    #[tokio::test]
    async fn diagnostics_status_confirms_network_access_but_errors_does_not() {
        use haven_common::types::PermissionMode;

        let authorization = crate::AuthorizationEngine::new();
        authorization
            .set_permission_mode(PermissionMode::Default)
            .await;

        let request_for = |name: &'static str| {
            let contract = operation_contract(name);
            let concurrency = ToolConcurrency::SharedResource(
                if name == "haven.diagnostics.status" {
                    "haven:diagnostics"
                } else {
                    "haven:sessions"
                }
                .into(),
            );
            let (effect, data_sensitivity, network_access) =
                operation_policy_attributes(&contract, concurrency.clone());
            let policy = OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: name.into(),
                confirmation: crate::tool_contract::confirmation_for(
                    RiskLevel::Low,
                    matches!(effect, crate::OperationEffect::ReadOnly),
                ),
                idempotency: contract.idempotency,
                scope: crate::ToolOperationScope::Global,
                concurrency,
                effect,
                data_sensitivity,
                network_access,
            };
            crate::AuthorizationRequest::new(None, name, json!({}), policy)
        };

        assert!(matches!(
            authorization
                .authorize(&request_for("haven.diagnostics.status"))
                .await,
            crate::AuthorizationDecision::RequiresConfirmation {
                reason_code: crate::AuthorizationReasonCode::SensitiveData,
                ..
            }
        ));
        assert!(matches!(
            authorization
                .authorize(&request_for("haven.diagnostics.errors"))
                .await,
            crate::AuthorizationDecision::AutoApproved
        ));
    }

    #[tokio::test]
    async fn model_mcp_connect_is_blocked_when_network_is_denied() {
        let name = "haven.mcp.mcp_connect";
        let contract = operation_contract(name);
        let concurrency = ToolConcurrency::Exclusive;
        let (effect, data_sensitivity, network_access) =
            operation_policy_attributes(&contract, concurrency.clone());
        assert_eq!(network_access, crate::NetworkAccess::Opaque);

        let request = crate::AuthorizationRequest::new(
            None,
            name,
            json!({ "operation": "mcp_connect", "name": "server-a" }),
            OperationPolicy {
                risk_level: contract.risk_override.unwrap_or(RiskLevel::Medium),
                capability: name.into(),
                confirmation: crate::tool_contract::confirmation_for(
                    contract.risk_override.unwrap_or(RiskLevel::Medium),
                    matches!(effect, crate::OperationEffect::ReadOnly),
                ),
                idempotency: contract.idempotency,
                scope: ToolOperationScope::Global,
                concurrency,
                effect,
                data_sensitivity,
                network_access,
            },
        );
        let engine = crate::AuthorizationEngine::new();
        for network_policy in [
            haven_common::types::NetworkPolicy::Deny,
            haven_common::types::NetworkPolicy::Restricted,
        ] {
            engine
                .set_boundaries(
                    haven_common::types::SandboxMode::FullAccess,
                    Vec::new(),
                    network_policy,
                )
                .await;

            assert!(matches!(
                engine.authorize(&request).await,
                crate::AuthorizationDecision::Blocked {
                    reason_code: crate::AuthorizationReasonCode::NetworkPolicy,
                    ..
                }
            ));
        }

        let native = admin::AdminRequest::Mcp(admin::McpOperationArgs::McpConnect {
            name: "server-a".into(),
        });
        assert_eq!(native.network_access(), network_access);
    }

    #[tokio::test]
    async fn model_mcp_operations_that_can_connect_are_blocked_by_restricted_policy() {
        use admin::{AdminRequest, McpOperationArgs};
        use haven_common::types::{McpTransportType, NetworkPolicy, SandboxMode};

        let cases = [
            (
                "haven.mcp.mcp_add",
                json!({
                    "operation": "mcp_add",
                    "name": "server-a",
                    "transport": "http",
                    "url": "https://example.invalid/mcp",
                    "enabled": true,
                    "auto_connect": true,
                }),
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "server-a".into(),
                    transport: McpTransportType::Http,
                    command: None,
                    url: Some("https://example.invalid/mcp".into()),
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd: None,
                    enabled: true,
                    auto_connect: true,
                }),
            ),
            (
                "haven.mcp.mcp_update",
                json!({
                    "operation": "mcp_update",
                    "name": "server-a",
                    "transport": "http",
                    "url": "https://example.invalid/mcp",
                    "enabled": true,
                }),
                AdminRequest::Mcp(McpOperationArgs::McpUpdate {
                    name: "server-a".into(),
                    transport: Some(McpTransportType::Http),
                    command: None,
                    url: Some("https://example.invalid/mcp".into()),
                    args: None,
                    env: None,
                    cwd: None,
                    enabled: Some(true),
                }),
            ),
            (
                "haven.mcp.mcp_toggle",
                json!({
                    "operation": "mcp_toggle",
                    "name": "server-a",
                    "enabled": true,
                }),
                AdminRequest::Mcp(McpOperationArgs::McpToggle {
                    name: "server-a".into(),
                    enabled: true,
                }),
            ),
            (
                "haven.mcp.mcp_reload",
                json!({"operation": "mcp_reload"}),
                AdminRequest::Mcp(McpOperationArgs::McpReload),
            ),
        ];

        let engine = crate::AuthorizationEngine::new();
        engine
            .set_boundaries(
                SandboxMode::FullAccess,
                Vec::new(),
                NetworkPolicy::Restricted,
            )
            .await;

        for (name, input, native) in cases {
            let contract = operation_contract(name);
            let concurrency = ToolConcurrency::Exclusive;
            let (effect, data_sensitivity, network_access) =
                operation_policy_attributes(&contract, concurrency.clone());
            assert_eq!(network_access, crate::NetworkAccess::Opaque, "{name}");
            assert_eq!(native.network_access(), network_access, "{name}");

            let risk_level = contract.risk_override.unwrap_or(RiskLevel::High);
            let request = crate::AuthorizationRequest::new(
                None,
                name,
                input,
                OperationPolicy {
                    risk_level,
                    capability: name.into(),
                    confirmation: crate::tool_contract::confirmation_for(
                        risk_level,
                        matches!(effect, crate::OperationEffect::ReadOnly),
                    ),
                    idempotency: contract.idempotency,
                    scope: ToolOperationScope::Global,
                    concurrency,
                    effect,
                    data_sensitivity,
                    network_access,
                },
            );

            assert!(
                matches!(
                    engine.authorize(&request).await,
                    crate::AuthorizationDecision::Blocked {
                        reason_code: crate::AuthorizationReasonCode::NetworkPolicy,
                        ..
                    }
                ),
                "{name} must be blocked before its handler can connect"
            );
        }

        for (name, native) in [
            (
                "haven.mcp.mcp_list",
                AdminRequest::Mcp(McpOperationArgs::McpList),
            ),
            (
                "haven.mcp.mcp_disconnect",
                AdminRequest::Mcp(McpOperationArgs::McpDisconnect {
                    name: "server-a".into(),
                }),
            ),
            (
                "haven.mcp.mcp_remove",
                AdminRequest::Mcp(McpOperationArgs::McpRemove {
                    name: "server-a".into(),
                }),
            ),
        ] {
            let (_, _, network_access) =
                operation_policy_attributes(&operation_contract(name), ToolConcurrency::Exclusive);
            assert_eq!(network_access, crate::NetworkAccess::None, "{name}");
            assert_eq!(native.network_access(), network_access, "{name}");
        }
    }

    #[test]
    fn file_read_views_share_the_files_resource_with_writers() {
        let specs = operation_specs(64);
        for name in [
            "files.read",
            "files.outline",
            "files.summary",
            "files.search",
        ] {
            let spec = specs
                .iter()
                .find(|spec| spec.name == name)
                .expect("file operation spec");
            assert_eq!(
                spec.policy.concurrency,
                ToolConcurrency::SharedResource("files".into()),
                "{name} must coordinate with files.write/edit/patch"
            );
        }
    }
}
