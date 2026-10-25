pub mod admin;
mod admin_support;
pub mod ask;
pub mod clipboard;
pub(crate) mod discover_tools;
mod env;
mod everything_search;
mod file_line_reader;
mod file_outline;
pub mod file_search;
pub mod files;
mod git;
pub mod http;
mod http_content;
pub mod input;
pub(crate) mod load_mcp;
pub(crate) mod load_skill;
mod lsp;
pub mod media;
mod media_audio;
pub mod memory;
pub mod messaging;
mod name_list;
pub mod notify;
mod operation_contract;
mod power;
mod registry;
pub mod schedule;
pub mod session;
pub mod shell;
pub mod system;
pub mod tool_runs;
mod tool_schema;
pub mod web_search;
pub mod window;

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use self::operation_contract::{OperationContract, operation_contract};
use self::tool_schema::canonical_entity_id_schema;
use crate::ToolRegistry;
use crate::ToolRunService;
use crate::operation_view::{
    OperationIdentity, OperationPolicyRule, OperationSpec, OperationViewTool,
    split_operation_schema, split_scope_operation_schema,
};
use crate::prompts as tool_prompts;
use crate::registry::{DeferredToolCatalog, SessionToolOverlay};
use crate::skill_runner::SkillRunner;
use crate::{
    ConfirmationRequirement, OperationIdempotency, OperationPolicy, ToolConcurrency, ToolHandle,
    ToolOperationScope,
};
use haven_common::config::{MAX_TOOLS_PER_REQUEST, MIN_MAX_TOOLS_PER_REQUEST};
use haven_common::tools::{ToolCatalogGroup, ToolPresentation, ToolPrompt, ToolSource};
use haven_common::types::RiskLevel;
use haven_mcp::McpManager;
use haven_skills::SkillRegistry;

use crate::tool_runtime::MemoryRecallSlot;
pub(crate) use admin::AdminOperationSet;
pub use admin::{
    AdminContext, AdminOperationError, AdminRequest, ConfigOperationArgs, ConfigOperationError,
    ConfigOperationOutput, ConfigViewOutput, DiagnosticsOperationArgs, LogLevelOutput,
    McpOperationArgs, McpRefreshAction, McpRefreshPlan, McpRefreshTarget, NativeMcpOperationArgs,
    SkillsOperationArgs, ToolsOperationArgs,
};
pub use media::{MediaTranscriptionResult, MediaTranscriptionStatus};

/// Effective output cap for a tool: the per-tool `tool_settings` override
/// when set, else the global observation budget
/// `context_limits.max_observation_chars`. The observation budget is the
/// ONLY output limit: tools truncate their own output to it, and the loop
/// applies the same budget as a safety net for adapters, so there is no
/// double truncation with different values.
pub(crate) fn tool_output_cap(
    settings: &HashMap<String, haven_common::config::ToolConfig>,
    name: &str,
    default_cap: usize,
) -> usize {
    settings
        .get(name)
        .and_then(|c| c.max_output_chars)
        .map(|cap| cap.min(default_cap))
        .unwrap_or(default_cap)
}

/// Provider and host dependencies used by media producers/consumers. The
/// shared runtime handle lets each media operation capture the active model
/// and provider clients from the same immutable generation.
pub(crate) struct MediaDeps {
    pub model_media_runtime: Option<haven_llm::ModelMediaRuntime>,
    pub model_media_snapshot: Option<Arc<haven_llm::ModelMediaSnapshot>>,
    pub input_pipeline: Option<Arc<haven_input::InputPipeline>>,
    pub(crate) capabilities: media::MediaCapabilities,
}

/// Long-running action dependencies for detached background ToolRuns.
pub(crate) struct ToolRunDeps {
    pub(crate) live_outputs: Arc<crate::live_output::LiveOutputHub>,
    pub(crate) service: Arc<ToolRunService>,
}

/// Complete dependency object for constructing the builtin catalog. Keeping
/// this boundary as a value object makes additions explicit and prevents the
/// constructor from growing another positional argument.
pub(crate) struct BuiltinContext {
    pub skill_registry: SkillRegistry,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub mcp_manager: Arc<McpManager>,
    pub mcp_server_config_source: crate::McpServerConfigSource,
    pub lsp_server_config_source: crate::lsp_server_config::LspServerConfigSource,
    pub everything_config_source: crate::everything_config::EverythingConfigSource,
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
    pub schedule_service: Arc<crate::ScheduleService>,
    pub managed_assets: crate::ManagedAssetRegistry,
    pub authorization: Arc<crate::security::AuthorizationEngine>,
    pub media: MediaDeps,
    pub tool_runs: ToolRunDeps,
}

pub(crate) async fn register_builtin_tools(
    tools: &mut Vec<ToolHandle>,
    context: BuiltinContext,
) -> Option<Arc<admin::AdminOperationSet>> {
    let BuiltinContext {
        skill_registry,
        skill_runner,
        mcp_manager,
        mcp_server_config_source,
        lsp_server_config_source,
        everything_config_source,
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
        schedule_service,
        managed_assets,
        authorization,
        media:
            MediaDeps {
                model_media_runtime,
                model_media_snapshot,
                input_pipeline,
                capabilities,
            },
        tool_runs:
            ToolRunDeps {
                live_outputs,
                service: tool_run_service,
            },
    } = context;
    let router = model_media_snapshot
        .as_ref()
        .map(|snapshot| snapshot.router().clone());
    let stt_client = model_media_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.stt_client().cloned());
    let ocr_client = model_media_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.ocr_client().cloned());
    let image_gen_client = model_media_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.image_gen_client().cloned());
    let tts_client = model_media_snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.tts_client().cloned());
    let media_config = model_media_snapshot
        .as_ref()
        .map(|snapshot| snapshot.media_config().clone())
        .unwrap_or_default();
    let execution_workspace = authorization.execution_workspace().await;
    let settings = &settings;
    let limits = &limits;
    let http_tool = Arc::new(http::HttpTool {
        max_retries: limits.network_max_retries,
        backoff_base_secs: limits.network_backoff_base_secs,
        max_body_bytes: limits.network_max_body_bytes,
        allowed_domains: settings
            .get("http")
            .map(|config| config.resource_policy.network.allowed_domains.clone())
            .unwrap_or_default(),
        authorization: Some(authorization.clone()),
    });
    let mut admin_operations: Option<Arc<admin::AdminOperationSet>> = None;
    let audio_runtime = Arc::new(
        media_audio::AudioRuntime::with_tts(input_pipeline, tts_client)
            .with_managed_assets(managed_assets.clone()),
    );
    tools.push(Arc::new(ask::typed_adapter()));
    tools.push(Arc::new(discover_tools::DiscoverToolsTool {
        deferred_catalog: deferred_catalog.clone(),
        registry: registry.clone(),
        session_tool_overlay: session_tool_overlay.clone(),
        max_tools_per_request: limits
            .max_tools_per_request
            .clamp(MIN_MAX_TOOLS_PER_REQUEST, MAX_TOOLS_PER_REQUEST),
        mcp_manager: mcp_manager.clone(),
        mcp_server_config_source: mcp_server_config_source.clone(),
    }));
    if settings
        .get("web_search_api")
        .map(|setting| setting.enabled)
        .unwrap_or(true)
        && let Some(search_router) = router.as_ref()
        && search_router
            .independent_web_search_available(haven_common::config::RequestKind::Chat)
            .await
    {
        tools.push(Arc::new(web_search::typed_adapter(
            search_router.clone(),
            http_tool.clone(),
            model_media_runtime.clone(),
        )));
    }
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
        .with_audio_runtime(audio_runtime)
        .with_model_media_runtime(model_media_runtime.clone())
        .with_authorization_engine(authorization.clone()),
    );
    add_operation_views(tools, media_tool.clone(), settings, MEDIA_OPERATION_VIEWS);
    let files_tool: ToolHandle = Arc::new(
        files::FilesTool::new(
            router.clone(),
            tool_output_cap(settings, "files", limits.max_observation_chars),
            limits.file_max_full_file_bytes,
            limits.file_max_patch_bytes,
            limits.file_line_span,
            limits.file_line_max_bytes,
            limits.file_summary_input_chars,
            limits.file_max_list_entries,
            limits.file_byte_read_max_bytes,
            limits.file_summary_timeout_secs,
            file_search::FileSearchEngine::new(
                limits.search_snippet_chars,
                limits.search_max_results,
                limits.search_max_file_size_bytes,
                limits.search_window_bytes,
            ),
            everything_config_source.clone(),
            managed_assets.clone(),
        )
        .with_model_media_runtime(model_media_runtime.clone())
        .with_max_write_bytes(limits.file_write_max_bytes)
        .with_authorization_engine(authorization.clone())
        .with_media_tool(media_tool.clone()),
    );
    for contract in operation_specs(limits.search_max_results) {
        if !contract.name.starts_with("files.") {
            continue;
        }
        tools.push(OperationViewTool::new(files_tool.clone(), contract));
    }
    add_operation_views(tools, files_tool.clone(), settings, FILE_OPERATION_VIEWS);
    let lsp_tool: ToolHandle = Arc::new(lsp::LspTool::new(
        lsp_server_config_source,
        authorization.clone(),
    ));
    for contract in operation_specs(limits.search_max_results) {
        if contract.name.starts_with("lsp.") {
            tools.push(OperationViewTool::new(lsp_tool.clone(), contract));
        }
    }
    let clipboard_tool: ToolHandle = Arc::new(
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
        live_outputs,
        max_output_chars: tool_output_cap(settings, "shell", limits.max_observation_chars),
        default_shell: default_shell.as_str().into(),
        execution_workspace,
        #[cfg(test)]
        test_output_log_root: None,
    }));
    tools.push(Arc::new(git::GitTool::new(
        authorization.clone(),
        limits.max_observation_chars,
    )));
    let tool_runs_tool: ToolHandle = Arc::new(tool_runs::ToolRunsTool {
        tool_runs: tool_run_service.clone(),
    });
    let input_tool: ToolHandle = Arc::new(input::InputTool);
    let schedule_tool: ToolHandle = Arc::new(schedule::ScheduleTool {
        service: schedule_service,
        // Weak registry probe so `set` can validate tool_name / risk at
        // schedule time; taken before the registry is shared with admin services.
        registry: Some(registry.probe()),
    });
    let window_tool: ToolHandle =
        Arc::new(window::WindowTool::new(managed_assets).with_media_tool(media_tool));
    add_operation_views(tools, clipboard_tool, settings, CLIPBOARD_OPERATION_VIEWS);
    add_operation_views(tools, input_tool, settings, INPUT_OPERATION_VIEWS);
    add_operation_views(tools, window_tool, settings, WINDOW_OPERATION_VIEWS);
    let system_tool: ToolHandle = Arc::new(system::SystemTool::default().with_max_output_chars(
        tool_output_cap(settings, "system", limits.max_observation_chars),
    ));
    let contract = operation_specs(limits.search_max_results)
        .into_iter()
        .find(|contract| contract.name == "system.get_info")
        .expect("system.get_info operation view contract");
    tools.push(OperationViewTool::new(system_tool.clone(), contract));
    add_system_scope_operation_views(tools, system_tool.clone(), settings);
    let contract = operation_spec(
        &system_tool,
        "system.list_displays",
        tool_prompts::operation_text("system.list_displays").description,
        vec![("scope".into(), serde_json::json!("display"))],
        system_display_schema(),
        "system",
        "monitor",
        tool_prompts::operation_text("system.list_displays").when_to_use,
    );
    tools.push(OperationViewTool::new(system_tool, contract));
    tools.push(http_tool);
    tools.push(Arc::new(notify::typed_adapter()));
    // Cross-session messaging / peer collab: one aggregate implementation over
    // the service-owned transport and session mailbox. Agents lazily register
    // on first call; the desktop runtime is an optional typed service port.
    let session_store = admin_context
        .as_ref()
        .and_then(|context| context.session_store.clone());
    let session_tool: ToolHandle = Arc::new(
        session::SessionTool::new(messaging_service.clone(), session_store)
            .with_working_document_port(
                admin_context
                    .as_ref()
                    .and_then(|context| context.session_working_document.clone()),
            ),
    );
    add_operation_views(tools, session_tool, settings, SESSION_OPERATION_VIEWS);
    let agent_tool: ToolHandle = Arc::new(messaging::AgentTool::new(messaging_service));
    add_operation_views(tools, agent_tool, settings, AGENT_OPERATION_VIEWS);
    let max_tools = limits
        .max_tools_per_request
        .clamp(MIN_MAX_TOOLS_PER_REQUEST, MAX_TOOLS_PER_REQUEST);
    // Skills are executable adapters in the deferred catalog. They become
    // provider-visible only after the model explicitly loads one or more.
    let skill_runner = skill_runner.read().await.clone();
    for skill in skill_registry
        .list_skills()
        .await
        .into_iter()
        .filter(|skill| skill.enabled() && skill.has_script())
    {
        let adapter = crate::adapters::SkillToolAdapter::new(Arc::new(skill), skill_runner.clone())
            .with_authorization_engine(authorization.clone());
        tools.push(Arc::new(adapter));
    }
    // Keep the control-plane surface stable even when no optional Skill or
    // MCP provider is configured. The loader returns a precise unavailable
    // result; the model should not have to infer capability availability from
    // a changing provider tool list.
    tools.push(Arc::new(load_skill::LoadSkillTool {
        deferred_catalog: deferred_catalog.clone(),
        registry: registry.clone(),
        skill_registry: skill_registry.clone(),
        session_tool_overlay: session_tool_overlay.clone(),
        max_tools_per_request: max_tools,
        max_observation_chars: tool_output_cap(
            settings,
            "load_skill",
            limits.max_observation_chars,
        ),
    }));
    tools.push(Arc::new(load_mcp::LoadMcpTool {
        mcp_manager: mcp_manager.clone(),
        mcp_server_config_source: mcp_server_config_source.clone(),
        authorization: authorization.clone(),
        registry: registry.clone(),
        session_tool_overlay,
        max_tools_per_request: max_tools,
    }));
    if let Some(ctx) = admin_context {
        // The composition root provides this typed capability explicitly;
        // headless builds can omit it without exposing the database here.
        let memory_facts = ctx.memory_facts.clone();
        let memory_tool: ToolHandle =
            Arc::new(memory::MemoryTool::new(memory_facts, memory_recall));
        add_operation_views(tools, memory_tool, settings, MEMORY_OPERATION_VIEWS);
        let operation_set = Arc::new(admin::AdminOperationSet::new(
            ctx,
            skill_registry.clone(),
            mcp_manager.clone(),
            mcp_server_config_source.clone(),
            registry.clone(),
            limits.self_tool_max_instructions_bytes,
            limits.self_tool_max_script_bytes,
        ));
        for capability_tool in operation_set.tools() {
            let capability = capability_tool.name();
            add_admin_operation_views(tools, capability_tool, settings, &capability);
        }
        admin_operations = Some(operation_set);
    }
    add_tool_run_views(tools, tool_runs_tool, settings);
    add_operation_views(tools, schedule_tool, settings, SCHEDULE_OPERATION_VIEWS);
    admin_operations
}

fn files_read_text_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "path": { "type": "string", "minLength": 1, "description": "Local file path to read" },
            "asset_id": canonical_entity_id_schema("asset", Some("Managed attachment id returned by an earlier tool result")),
            "offset": { "type": "integer", "minimum": 0, "default": 0, "description": "Zero-based byte offset; defaults to 0. Use byte arguments instead of line arguments" },
            "limit": { "type": "integer", "minimum": 1, "description": "Maximum bytes to read from offset; omit for the configured default span. Do not combine with line arguments" },
            "start_line": { "type": "integer", "minimum": 1, "default": 1, "description": "1-based first line; defaults to 1. Use line arguments instead of byte arguments" },
            "end_line": { "type": "integer", "minimum": 1, "description": "1-based inclusive last line; omit for the default span" },
            "focus": { "type": "string", "description": "Optional question or region of interest for a rich media source" }
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
            "path": { "type": "string", "minLength": 1, "description": "Local text file path to summarize" },
            "asset_id": canonical_entity_id_schema("asset", Some("Managed attachment id returned by an earlier tool result")),
            "start_line": { "type": "integer", "minimum": 1, "default": 1, "description": "1-based first line; defaults to 1" },
            "end_line": { "type": "integer", "minimum": 0, "description": "1-based last line; 0 or omitted means through EOF" },
            "focus": { "type": "string", "maxLength": 2000, "description": "Optional question or topic to focus the summary on" },
            "max_chars": { "type": "integer", "minimum": 1, "description": "Maximum source characters sent for summarization" }
        },
        "oneOf": [{ "required": ["path"] }, { "required": ["asset_id"] }]
    })
}

fn files_search_schema(max_results: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "root": { "type": "string", "minLength": 1, "description": "Directory or file path to search under" },
            "pattern": { "type": "string", "minLength": 1, "description": "Filename glob in filename mode; regular expression in content mode" },
            "mode": { "type": "string", "enum": ["filename", "content"], "default": "filename", "description": "filename matches names (default); content searches text and returns matching line snippets" },
            "max_depth": { "type": "integer", "minimum": 0, "description": "Maximum directory depth; 0 means unlimited" },
            "max_results": { "type": "integer", "minimum": 1, "maximum": max_results, "description": "Maximum matches to return" },
            "ignore_hidden": { "type": "boolean", "description": "Whether to skip hidden files and directories" },
            "max_file_size": { "type": "integer", "minimum": 0, "description": "Skip content-search files larger than this many bytes" },
            "start_line": { "type": "integer", "minimum": 1, "description": "Content-mode first line to include" },
            "end_line": { "type": "integer", "minimum": 0, "description": "Content-mode last line; 0 or omitted means through EOF" }
        },
        "required": ["root", "pattern"]
    })
}

fn system_info_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "category": { "type": "string", "enum": ["overview", "cpu", "memory", "disk", "os", "network", "user", "locale", "all"], "default": "overview", "description": "Information to return; overview is compact, all is broad, and locale includes local/UTC time and timezone" }
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn operation_spec(
    inner: &ToolHandle,
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
    let attributes = operation_policy_attributes(&metadata, concurrency.clone());
    let policy = OperationPolicy {
        risk_level,
        capability: haven_common::types::CapabilityScope::try_new(name)
            .expect("builtin tool name is a valid capability"),
        confirmation: crate::tool_contract::confirmation_for(
            risk_level,
            matches!(attributes.effect, crate::OperationEffect::ReadOnly),
        ),
        idempotency: metadata.idempotency,
        scope: inner.operation_scope(&policy_input),
        concurrency,
        effect: attributes.effect,
        data_sensitivity: attributes.data_sensitivity,
        network_access: attributes.network_access,
        execution_boundary: attributes.execution_boundary,
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
            when_not_to_use: String::new(),
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
) -> crate::tool_contract::OperationAttributes {
    let mut attributes = crate::tool_contract::operation_attributes(contract.name, concurrency);
    attributes.effect = if contract.read_only {
        crate::OperationEffect::ReadOnly
    } else {
        attributes.effect
    };
    attributes.network_access = contract
        .network_access_override
        .unwrap_or(attributes.network_access);
    attributes
}

/// The operation-view catalog is the backend source of truth for the model
/// schema, execution policy and the cross-boundary UI/prompt identifiers.
fn operation_specs(max_results: usize) -> Vec<OperationSpec> {
    let mut specs = vec![
        OperationSpec {
            name: "files.read".into(),
            description: tool_prompts::operation_text("files.read")
                .description
                .into(),
            fixed: vec![("operation".into(), serde_json::json!("read"))],
            schema: files_read_text_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: haven_common::types::CapabilityScope::try_new("files.read").unwrap(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::None,
                execution_boundary: crate::ExecutionBoundary::InProcess,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::Files,
            presentation: ToolPresentation {
                label: operation_contract("files.read").label.into(),
                renderer: "files".into(),
                icon: "file".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.read")
                    .when_to_use
                    .into(),
                when_not_to_use: String::new(),
                key_operations: vec!["files.read".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "files.get_outline".into(),
            description: tool_prompts::operation_text("files.get_outline")
                .description
                .into(),
            fixed: vec![("operation".into(), serde_json::json!("outline"))],
            schema: files_outline_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: haven_common::types::CapabilityScope::try_new("files.get_outline")
                    .unwrap(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::None,
                execution_boundary: crate::ExecutionBoundary::InProcess,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::Files,
            presentation: ToolPresentation {
                label: operation_contract("files.get_outline").label.into(),
                renderer: "files".into(),
                icon: "fileSearch".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.get_outline")
                    .when_to_use
                    .into(),
                when_not_to_use: String::new(),
                key_operations: vec!["files.get_outline".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "files.summarize".into(),
            description: tool_prompts::operation_text("files.summarize")
                .description
                .into(),
            fixed: vec![("operation".into(), serde_json::json!("summary"))],
            schema: files_summary_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: haven_common::types::CapabilityScope::try_new("files.summarize")
                    .unwrap(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::Public,
                execution_boundary: crate::ExecutionBoundary::InProcess,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::Files,
            presentation: ToolPresentation {
                label: operation_contract("files.summarize").label.into(),
                renderer: "files".into(),
                icon: "file".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.summarize")
                    .when_to_use
                    .into(),
                when_not_to_use: String::new(),
                key_operations: vec!["files.summarize".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "files.search".into(),
            description: tool_prompts::operation_text("files.search")
                .description
                .into(),
            fixed: vec![("operation".into(), serde_json::json!("search"))],
            schema: files_search_schema(max_results),
            policy: OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: haven_common::types::CapabilityScope::try_new("files.search").unwrap(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::SharedResource("files".into()),
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::UserData,
                network_access: crate::NetworkAccess::None,
                execution_boundary: crate::ExecutionBoundary::InProcess,
            },
            policy_rule: Some(OperationPolicyRule::ContentSearchMedium),
            catalog_group: ToolCatalogGroup::Files,
            presentation: ToolPresentation {
                label: operation_contract("files.search").label.into(),
                renderer: "files.search".into(),
                icon: "search".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("files.search")
                    .when_to_use
                    .into(),
                when_not_to_use: String::new(),
                key_operations: vec!["files.search".into()],
            },
            identity: None,
        },
        OperationSpec {
            name: "system.get_info".into(),
            description: tool_prompts::operation_text("system.get_info")
                .description
                .into(),
            fixed: vec![("scope".into(), serde_json::json!("info"))],
            schema: system_info_schema(),
            policy: OperationPolicy {
                risk_level: RiskLevel::Safe,
                capability: haven_common::types::CapabilityScope::try_new("system.get_info")
                    .unwrap(),
                confirmation: ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Idempotent,
                scope: ToolOperationScope::Global,
                concurrency: ToolConcurrency::ReadOnly,
                effect: crate::OperationEffect::ReadOnly,
                data_sensitivity: crate::DataSensitivity::None,
                network_access: crate::NetworkAccess::None,
                execution_boundary: crate::ExecutionBoundary::InProcess,
            },
            policy_rule: None,
            catalog_group: ToolCatalogGroup::System,
            presentation: ToolPresentation {
                label: operation_contract("system.get_info").label.into(),
                renderer: "system".into(),
                icon: "cpu".into(),
                represented_source: haven_common::tools::ToolSource::Builtin,
            },
            prompt: ToolPrompt {
                when_to_use: tool_prompts::operation_text("system.get_info")
                    .when_to_use
                    .into(),
                when_not_to_use: String::new(),
                key_operations: vec!["system.get_info".into()],
            },
            identity: None,
        },
    ];
    specs.extend(lsp::operation_specs());
    specs
}

#[derive(Debug, Clone, Copy)]
struct SplitOperationSpec {
    name: &'static str,
    operation: &'static str,
    renderer: &'static str,
    icon: &'static str,
    catalog_root: Option<&'static str>,
}

macro_rules! split_spec {
    ($name:literal, $operation:literal, $renderer:literal, $icon:literal) => {
        SplitOperationSpec {
            name: $name,
            operation: $operation,
            renderer: $renderer,
            icon: $icon,
            catalog_root: None,
        }
    };
}

macro_rules! split_spec_in_root {
    ($root:literal, $name:literal, $operation:literal, $renderer:literal, $icon:literal) => {
        SplitOperationSpec {
            name: $name,
            operation: $operation,
            renderer: $renderer,
            icon: $icon,
            catalog_root: Some($root),
        }
    };
}

const FILE_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("files.inspect", "inspect", "files", "fileSearch"),
    split_spec!("files.write", "write", "files", "file"),
    split_spec!("files.create_directory", "create_dir", "files", "folder"),
    split_spec!("files.patch", "patch", "files", "edit"),
    split_spec!("files.copy", "copy", "files", "copy"),
    split_spec!("files.move", "move", "files", "move"),
    split_spec!("files.delete", "delete", "files", "delete"),
    split_spec!("files.list", "list", "files", "folder"),
];

const CLIPBOARD_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("clipboard.read", "read", "clipboard", "clipboard"),
    split_spec!("clipboard.write", "write", "clipboard", "clipboard"),
    split_spec!(
        "clipboard.list_history",
        "history",
        "clipboard",
        "clipboard"
    ),
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
    split_spec!("window.get_foreground", "foreground", "window", "monitor"),
    split_spec!("window.focus", "focus", "window", "monitor"),
    split_spec!("window.close", "close", "window", "monitor"),
    split_spec!("window.capture_screenshot", "screenshot", "window", "image"),
    split_spec!("window.get_ui_tree", "ui_tree", "window", "account_tree"),
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
    split_spec!("media.run_ocr", "ocr", "media", "image"),
    split_spec!("media.transcribe", "transcribe", "media", "mic"),
    split_spec!("media.extract", "extract", "media", "fileText"),
    split_spec!("media.render", "render", "media", "fileText"),
    split_spec!("media.generate", "generate", "media", "image"),
    split_spec!("media.record", "record", "media", "mic"),
    split_spec!("media.play", "play", "media", "volumeUp"),
    split_spec!("media.speak", "speak", "media", "volumeUp"),
    split_spec!("media.get_volume", "volume_get", "media", "volumeUp"),
    split_spec!("media.set_volume", "volume_set", "media", "volumeUp"),
    split_spec!("media.get_mute", "mute_get", "media", "volumeOff"),
    split_spec!("media.set_mute", "mute_set", "media", "volumeOff"),
];

const MEMORY_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec_in_root!("haven", "memory.search", "search", "memory", "memory"),
    split_spec_in_root!("haven", "memory.list", "list", "memory", "memory"),
    split_spec_in_root!("haven", "memory.remember", "remember", "memory", "memory"),
    split_spec_in_root!("haven", "memory.forget", "forget", "memory", "memory"),
    split_spec_in_root!("haven", "memory.recall", "recall", "memory", "memory"),
];

const AGENT_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("agent.list", "list", "agent", "users"),
    split_spec!("agent.list_children", "children", "agent", "users"),
    split_spec!("agent.read_history", "history", "agent", "users"),
    split_spec!("agent.list_inbox", "inbox", "agent", "users"),
    split_spec!("agent.acknowledge", "ack", "agent", "users"),
    split_spec!("agent.send", "send", "agent", "users"),
    split_spec!("agent.reply", "reply", "agent", "users"),
    split_spec!("agent.get_profile", "profile", "agent", "users"),
    split_spec!("agent.request", "request", "agent", "users"),
    split_spec!("agent.get_status", "status", "agent", "users"),
    split_spec!("agent.wait", "wait", "agent", "users"),
    split_spec!("agent.stop", "stop", "agent", "users"),
    split_spec!("agent.collect", "collect", "agent", "fileText"),
];

const SESSION_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("session.create", "create", "session", "users"),
    split_spec!("session.link", "link", "session", "link"),
    split_spec!("session.unlink", "unlink", "session", "link"),
    split_spec!("session.list_links", "links", "session", "link"),
    split_spec!(
        "session.read_notes",
        "working_document_read",
        "session",
        "fileText"
    ),
    split_spec!(
        "session.replace_notes",
        "working_document_replace",
        "session",
        "fileText"
    ),
    split_spec!(
        "session.search_history",
        "history_search",
        "session",
        "search"
    ),
    split_spec!(
        "session.read_history",
        "history_read",
        "session",
        "fileText"
    ),
    split_spec!(
        "session.search_linked_history",
        "linked_history_search",
        "session",
        "search"
    ),
    split_spec!(
        "session.read_linked_history",
        "linked_history_read",
        "session",
        "fileText"
    ),
    split_spec!("session.search_assets", "asset_search", "session", "file"),
];

const ACTION_OPERATION_VIEWS: &[SplitOperationSpec] =
    &[split_spec!("toolrun.cancel", "cancel", "toolrun", "clock")];

const SCHEDULE_OPERATION_VIEWS: &[SplitOperationSpec] = &[
    split_spec!("schedule.set", "set", "schedule", "bell"),
    split_spec!("schedule.list", "list", "schedule", "bell"),
    split_spec!("schedule.cancel", "cancel", "schedule", "bell"),
];

const SYSTEM_SCOPE_OPERATION_VIEWS: &[(&str, &str, &str, &str, &str)] = &[
    ("system.env.list", "env", "list", "system", "terminal"),
    ("system.env.get", "env", "get", "system", "terminal"),
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
        "system.power.get_status",
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
        "haven.diagnostics.get_status",
        "haven_diagnostics",
        "get_status",
        "settings",
        "settings",
    ),
    (
        "haven.diagnostics.tail_logs",
        "haven_diagnostics",
        "tail_logs",
        "settings",
        "settings",
    ),
    (
        "haven.diagnostics.list_sessions",
        "haven_diagnostics",
        "list_sessions",
        "settings",
        "settings",
    ),
    (
        "haven.diagnostics.list_errors",
        "haven_diagnostics",
        "list_errors",
        "settings",
        "settings",
    ),
    (
        "haven.config.get",
        "haven_config",
        "get",
        "settings",
        "settings",
    ),
    (
        "haven.config.set_log_level",
        "haven_config",
        "set_log_level",
        "settings",
        "settings",
    ),
    (
        "haven.skills.list",
        "haven_skills",
        "list",
        "settings",
        "sparkles",
    ),
    (
        "haven.skills.enable",
        "haven_skills",
        "enable",
        "settings",
        "sparkles",
    ),
    (
        "haven.skills.disable",
        "haven_skills",
        "disable",
        "settings",
        "sparkles",
    ),
    (
        "haven.skills.create",
        "haven_skills",
        "create",
        "settings",
        "sparkles",
    ),
    (
        "haven.tools.enable",
        "haven_tools",
        "enable",
        "settings",
        "settings",
    ),
    (
        "haven.tools.disable",
        "haven_tools",
        "disable",
        "settings",
        "settings",
    ),
    ("haven.mcp.list", "haven_mcp", "list", "settings", "network"),
    (
        "haven.mcp.connect",
        "haven_mcp",
        "connect",
        "settings",
        "network",
    ),
    (
        "haven.mcp.disconnect",
        "haven_mcp",
        "disconnect",
        "settings",
        "network",
    ),
    ("haven.mcp.add", "haven_mcp", "add", "settings", "network"),
    (
        "haven.mcp.update",
        "haven_mcp",
        "update",
        "settings",
        "network",
    ),
    (
        "haven.mcp.toggle",
        "haven_mcp",
        "toggle",
        "settings",
        "network",
    ),
    (
        "haven.mcp.remove",
        "haven_mcp",
        "remove",
        "settings",
        "network",
    ),
    (
        "haven.mcp.reload",
        "haven_mcp",
        "reload",
        "settings",
        "network",
    ),
];

fn add_operation_views(
    tools: &mut Vec<ToolHandle>,
    inner: ToolHandle,
    _settings: &HashMap<String, haven_common::config::ToolConfig>,
    specs: &[SplitOperationSpec],
) {
    let schema = inner.input_schema();
    for spec in specs {
        let Some(view_schema) = split_operation_schema(&schema, spec.operation) else {
            continue;
        };
        let text = tool_prompts::operation_text(spec.name);
        let mut contract = operation_spec(
            &inner,
            spec.name,
            text.description,
            vec![("operation".into(), serde_json::json!(spec.operation))],
            view_schema,
            spec.renderer,
            spec.icon,
            text.when_to_use,
        );
        if let Some(root) = spec.catalog_root {
            let operation = spec
                .name
                .strip_prefix(&format!("{root}."))
                .unwrap_or(spec.name);
            contract.identity = Some(OperationIdentity {
                source: ToolSource::Builtin,
                root: root.into(),
                operation: Some(operation.into()),
                root_presentation: crate::tool_contract::default_root_presentation(
                    root,
                    ToolSource::Builtin,
                ),
                availability: Default::default(),
            });
        }
        tools.push(OperationViewTool::new(inner.clone(), contract));
    }
}

fn add_system_scope_operation_views(
    tools: &mut Vec<ToolHandle>,
    inner: ToolHandle,
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

fn tool_run_start_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "tool_name": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Enabled tool to invoke in the background" },
            "input": { "type": "object", "description": "Arguments passed unchanged to tool_name" }
        },
        "required": ["tool_name"]
    })
}

fn system_display_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false
    })
}

fn add_admin_operation_views(
    tools: &mut Vec<ToolHandle>,
    inner: ToolHandle,
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
    tools: &mut Vec<ToolHandle>,
    inner: ToolHandle,
    _settings: &HashMap<String, haven_common::config::ToolConfig>,
) {
    for (name, schema) in [
        ("toolrun.start", tool_run_start_schema()),
        ("toolrun.list", tool_run_list_schema(false)),
        ("toolrun.inspect", tool_run_list_schema(true)),
    ] {
        let text = tool_prompts::operation_text(name);
        let contract = operation_spec(
            &inner,
            name,
            text.description,
            Vec::new(),
            schema,
            "toolrun",
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
            "media.run_ocr",
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
            "media.get_volume",
            "media.set_volume",
            "media.get_mute",
            "media.set_mute",
        ] {
            assert_eq!(
                policy(name),
                ToolConcurrency::Resource("media:audio-device".into()),
                "{name}"
            );
        }
    }

    #[test]
    fn memory_operation_views_are_grouped_under_haven_without_changing_model_or_permission_names() {
        let memory = Arc::new(memory::MemoryTool::new(
            None,
            crate::tool_runtime::new_memory_recall_slot(),
        ));
        let mut tools = Vec::new();
        add_operation_views(&mut tools, memory, &HashMap::new(), MEMORY_OPERATION_VIEWS);

        assert_eq!(tools.len(), MEMORY_OPERATION_VIEWS.len());
        for tool in tools {
            let name = tool.name();
            let manifest = tool.tool_manifest();
            assert_eq!(manifest.model.name, name);
            assert_eq!(manifest.identity.stable_name, name);
            assert_eq!(manifest.identity.root, "haven", "{name}");
            assert_eq!(manifest.identity.operation.as_deref(), Some(name.as_str()));
            assert_eq!(manifest.policy.permission_key, name, "{name}");
            assert_eq!(manifest.presentation.renderer, "memory", "{name}");
        }
    }

    #[test]
    fn tool_output_cap_falls_back_to_global_default() {
        assert_eq!(tool_output_cap(&HashMap::new(), "shell", 8_000), 8_000);
        assert_eq!(tool_output_cap(&HashMap::new(), "files", 5_000), 5_000);
    }

    #[test]
    fn tool_output_cap_allows_per_tool_lowering_but_not_raising() {
        let settings = settings_with("shell", Some(1_000));
        assert_eq!(tool_output_cap(&settings, "shell", 8_000), 1_000);
        // Tools without a settings entry still get the global default.
        assert_eq!(tool_output_cap(&settings, "files", 8_000), 8_000);

        let settings = settings_with("shell", Some(10_000));
        assert_eq!(tool_output_cap(&settings, "shell", 8_000), 8_000);
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
        let http = http::HttpTool::default();
        let input = input::InputTool;
        let system = system::SystemTool::default();
        let window = window::WindowTool::new(crate::ManagedAssetRegistry::default());
        let schedule = schedule::ScheduleTool {
            service: Arc::new(crate::ScheduleService::new()),
            registry: None,
        };
        let cases: Vec<(&dyn Tool, serde_json::Value, serde_json::Value)> = vec![
            (
                &media,
                json!({"operation": "volume_set", "volume": 0.5}),
                json!({"operation": "volume_set"}),
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
                json!({"scope": "env", "operation": "get", "name": "PATH"}),
                json!({"scope": "env", "operation": "set", "name": "HAVEN_TEST", "value": "x"}),
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
                "files.get_outline",
                "files.summarize",
                "files.search",
                "system.get_info",
                "lsp.find_definition",
                "lsp.find_implementations",
                "lsp.find_type_definition",
                "lsp.find_references",
                "lsp.get_hover",
                "lsp.list_document_symbols",
                "lsp.search_workspace_symbols",
                "lsp.get_diagnostics"
            ]
        );
        for contract in contracts {
            assert!(contract.schema.is_object(), "{} schema", contract.name);
            assert_eq!(contract.schema["additionalProperties"], json!(false));
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
        let status = operation_contract("haven.diagnostics.get_status");
        let status_attributes = operation_policy_attributes(
            &status,
            ToolConcurrency::SharedResource("haven:diagnostics".into()),
        );
        assert_eq!(status_attributes.effect, crate::OperationEffect::ReadOnly);
        assert_eq!(
            status_attributes.network_access,
            crate::NetworkAccess::Public
        );

        let errors = operation_contract("haven.diagnostics.list_errors");
        let errors_attributes = operation_policy_attributes(
            &errors,
            ToolConcurrency::SharedResource("haven:sessions".into()),
        );
        assert_eq!(errors_attributes.effect, crate::OperationEffect::ReadOnly);
        assert_eq!(errors_attributes.network_access, crate::NetworkAccess::None);
    }

    #[tokio::test]
    async fn diagnostics_status_confirms_network_access_but_errors_does_not() {
        use haven_common::types::ExecutionMode;

        let authorization = crate::security::AuthorizationEngine::new();
        authorization
            .set_execution_mode(ExecutionMode::Collaborative)
            .await;

        let request_for = |name: &'static str| {
            let contract = operation_contract(name);
            let concurrency = ToolConcurrency::SharedResource(
                if name == "haven.diagnostics.get_status" {
                    "haven:diagnostics"
                } else {
                    "haven:sessions"
                }
                .into(),
            );
            let attributes = operation_policy_attributes(&contract, concurrency.clone());
            let policy = OperationPolicy {
                risk_level: RiskLevel::Low,
                capability: haven_common::types::CapabilityScope::try_new(name).unwrap(),
                confirmation: crate::tool_contract::confirmation_for(
                    RiskLevel::Low,
                    matches!(attributes.effect, crate::OperationEffect::ReadOnly),
                ),
                idempotency: contract.idempotency,
                scope: crate::ToolOperationScope::Global,
                concurrency,
                effect: attributes.effect,
                data_sensitivity: attributes.data_sensitivity,
                network_access: attributes.network_access,
                execution_boundary: attributes.execution_boundary,
            };
            crate::AuthorizationRequest::new(None, name, json!({}), policy)
        };

        assert!(matches!(
            authorization
                .authorize(&request_for("haven.diagnostics.get_status"))
                .await,
            crate::AuthorizationDecision::RequiresConfirmation {
                reason_code: crate::AuthorizationReasonCode::SensitiveData,
                ..
            }
        ));
        assert!(matches!(
            authorization
                .authorize(&request_for("haven.diagnostics.list_errors"))
                .await,
            crate::AuthorizationDecision::AutoApproved
        ));
    }

    #[tokio::test]
    async fn model_mcp_connect_is_blocked_when_network_is_denied() {
        let name = "haven.mcp.connect";
        let contract = operation_contract(name);
        let concurrency = ToolConcurrency::Exclusive;
        let attributes = operation_policy_attributes(&contract, concurrency.clone());
        assert_eq!(attributes.network_access, crate::NetworkAccess::Opaque);

        let request = crate::AuthorizationRequest::new(
            None,
            name,
            json!({ "operation": "connect", "name": "server-a" }),
            OperationPolicy {
                risk_level: contract.risk_override.unwrap_or(RiskLevel::Medium),
                capability: haven_common::types::CapabilityScope::try_new(name).unwrap(),
                confirmation: crate::tool_contract::confirmation_for(
                    contract.risk_override.unwrap_or(RiskLevel::Medium),
                    matches!(attributes.effect, crate::OperationEffect::ReadOnly),
                ),
                idempotency: contract.idempotency,
                scope: ToolOperationScope::Global,
                concurrency,
                effect: attributes.effect,
                data_sensitivity: attributes.data_sensitivity,
                network_access: attributes.network_access,
                execution_boundary: attributes.execution_boundary,
            },
        );
        let engine = crate::security::AuthorizationEngine::new();
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
        assert_eq!(native.network_access(), attributes.network_access);
    }

    #[tokio::test]
    async fn model_mcp_operations_that_can_connect_are_blocked_by_restricted_policy() {
        use admin::{AdminRequest, McpOperationArgs};
        use haven_common::{
            mcp::McpServerTransport,
            types::{NetworkPolicy, SandboxMode},
        };

        let cases = [
            (
                "haven.mcp.add",
                json!({
                    "operation": "add",
                    "name": "server-a",
                    "transport": "http",
                    "url": "https://example.invalid/mcp",
                    "enabled": true,
                    "auto_connect": true,
                }),
                AdminRequest::Mcp(McpOperationArgs::McpAdd {
                    name: "server-a".into(),
                    transport: McpServerTransport::Http,
                    exposure: Default::default(),
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
                "haven.mcp.update",
                json!({
                    "operation": "update",
                    "name": "server-a",
                    "transport": "http",
                    "url": "https://example.invalid/mcp",
                    "enabled": true,
                }),
                AdminRequest::Mcp(McpOperationArgs::McpUpdate {
                    name: "server-a".into(),
                    exposure: None,
                    transport: Some(McpServerTransport::Http),
                    command: None,
                    url: Some("https://example.invalid/mcp".into()),
                    args: None,
                    env: None,
                    cwd: None,
                    enabled: Some(true),
                }),
            ),
            (
                "haven.mcp.toggle",
                json!({
                    "operation": "toggle",
                    "name": "server-a",
                    "enabled": true,
                }),
                AdminRequest::Mcp(McpOperationArgs::McpToggle {
                    name: "server-a".into(),
                    enabled: true,
                }),
            ),
            (
                "haven.mcp.reload",
                json!({"operation": "reload"}),
                AdminRequest::Mcp(McpOperationArgs::McpReload),
            ),
        ];

        let engine = crate::security::AuthorizationEngine::new();
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
            let attributes = operation_policy_attributes(&contract, concurrency.clone());
            assert_eq!(
                attributes.network_access,
                crate::NetworkAccess::Opaque,
                "{name}"
            );
            assert_eq!(native.network_access(), attributes.network_access, "{name}");

            let risk_level = contract.risk_override.unwrap_or(RiskLevel::High);
            let request = crate::AuthorizationRequest::new(
                None,
                name,
                input,
                OperationPolicy {
                    risk_level,
                    capability: haven_common::types::CapabilityScope::try_new(name).unwrap(),
                    confirmation: crate::tool_contract::confirmation_for(
                        risk_level,
                        matches!(attributes.effect, crate::OperationEffect::ReadOnly),
                    ),
                    idempotency: contract.idempotency,
                    scope: ToolOperationScope::Global,
                    concurrency,
                    effect: attributes.effect,
                    data_sensitivity: attributes.data_sensitivity,
                    network_access: attributes.network_access,
                    execution_boundary: attributes.execution_boundary,
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
                "haven.mcp.list",
                AdminRequest::Mcp(McpOperationArgs::McpList),
            ),
            (
                "haven.mcp.disconnect",
                AdminRequest::Mcp(McpOperationArgs::McpDisconnect {
                    name: "server-a".into(),
                }),
            ),
            (
                "haven.mcp.remove",
                AdminRequest::Mcp(McpOperationArgs::McpRemove {
                    name: "server-a".into(),
                }),
            ),
        ] {
            let attributes =
                operation_policy_attributes(&operation_contract(name), ToolConcurrency::Exclusive);
            assert_eq!(
                attributes.network_access,
                crate::NetworkAccess::None,
                "{name}"
            );
            assert_eq!(native.network_access(), attributes.network_access, "{name}");
        }
    }

    #[test]
    fn file_read_views_share_the_files_resource_with_writers() {
        let specs = operation_specs(64);
        for name in [
            "files.read",
            "files.get_outline",
            "files.summarize",
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
