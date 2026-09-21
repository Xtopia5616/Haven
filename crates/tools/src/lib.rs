mod action_lifecycle;
mod action_service;
pub mod adapters;
mod asset_registry;
pub mod builtin;
pub mod circuit;
mod document;
pub mod inbox;
pub mod live_output;
pub mod messaging_service;
mod operation_view;
mod output;
mod process;
mod prompts;
pub(crate) mod registry;
pub(crate) mod security;
mod shell_runtime;
pub mod simulate;
pub mod skill_runner;
mod tool_builtins;
pub(crate) mod tool_contract;
mod tool_core;
mod tool_runtime;
pub mod util;

use chrono::{DateTime, Utc};
use haven_common::config::{
    ContextLimitsConfig, McpServerConfig, RequestKind, SecurityConfig, SkillsExecConfig, ToolConfig,
};
use haven_common::types::{MessageAttachment, RiskLevel, ShellChoice};
use haven_llm::LlmRouter;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Whether a tool is enabled per `tool_settings`. Tools without a settings
/// entry are enabled by default. Single source of truth shared by the
/// registry filter, the execution gate, and the UI listing, so the three
/// cannot drift apart.
fn tool_config_enabled(settings: &HashMap<String, ToolConfig>, name: &str) -> bool {
    settings
        .get(name)
        .or_else(|| name.split('.').next().and_then(|root| settings.get(root)))
        .map(|c| c.enabled)
        .unwrap_or(true)
}

/// The always-visible provider surface is deliberately small. These tools
/// support clarification, narrow source inspection, and activation of deeper
/// capability layers; all other enabled builtins and Skills are loaded into a
/// session only when requested by the model.
fn is_core_model_tool(name: &str) -> bool {
    matches!(
        name,
        "ask"
            | "notify"
            | "load_builtin"
            | "tool_catalog"
            | "load_skill"
            | "load_mcp"
            | "files.read"
            | "files.outline"
            | "files.search"
            | "system.info"
    )
}

#[derive(Debug, Clone, Default)]
enum CatalogRebuildScope {
    #[default]
    All,
    Roots(HashSet<String>),
}

impl CatalogRebuildScope {
    fn roots(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Roots(names.into_iter().map(Into::into).collect())
    }

    fn affects(&self, name: &str) -> bool {
        match self {
            Self::All => true,
            Self::Roots(roots) => {
                let root = name.split('.').next().unwrap_or(name);
                roots.contains(root)
                    || (name.starts_with("skill__") && roots.contains("skills"))
                    || (name.starts_with("mcp__") && roots.contains("mcp"))
            }
        }
    }
}

pub(crate) use action_lifecycle::{ActionLifecycle, EventSinkState};
pub use action_service::ActionService;
pub use action_service::{
    ActionCompletion, ActionCompletionReceiver, BackgroundActionCompletion, EventSink,
    ScheduledActionFired,
};
pub use adapters::{McpToolAdapter, SkillToolAdapter};
pub use asset_registry::{ManagedAsset, ManagedAssetRegistry};
pub use builtin::{
    AdminCapability, AdminContext, AdminOperationError, AdminRequest, AdminSurfaces, AgentTool,
    ConfigAdminContext, ConfigAdminOperation, ConfigAdminTool, ConfigOperationArgs,
    ConfigOperationError, ConfigOperationOutput, ConfigViewOutput, DiagnosticsOperationArgs,
    LogLevelOutput, McpOperationArgs, MediaTranscriptionResult, MediaTranscriptionStatus,
    ScheduleMode, SkillsOperationArgs, ToolsOperationArgs,
};
pub use circuit::ToolCircuitRegistry;
pub use haven_common::types::CapabilityScope;
pub use haven_mcp::{
    McpClient, McpClientStatus, McpManager, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
pub use haven_skills::{Language, Skill, SkillInfo, SkillManifest, SkillsEngine, VenvManager};
pub use live_output::LiveOutputHub;
pub use messaging_service::{
    AgentControlOperation, AgentControlRequest, AgentControlResult, AgentSpawnRequest,
    AgentSpawnResult, MessageClaim, MessageTransport, MessagingRuntime, MessagingService,
    SentMessage, SessionMailbox, is_expired,
};
pub use output::{
    OutputBudget, ToolOutput, append_windows_diagnostics, is_progress_clixml,
    sanitize_shell_output, summarize_error,
};
pub(crate) use process::{read_stream_capped, take_tail_if_changed};
pub use registry::{
    DeferredToolCatalog, RegistryProbe, SessionCatalog, ToolCatalogSnapshot, ToolRegistry,
};
pub use security::{
    AuthorizationDecision, AuthorizationEngine, AuthorizationReasonCode, AuthorizationRequest,
    ConfirmationReceipt, LOCAL_TOOL_SECURITY_MATRIX, LocalToolSecurityCase, is_safe_local_path,
    permission_prompt_summary,
};
#[cfg(windows)]
pub use shell_runtime::CREATE_NO_WINDOW;
pub use shell_runtime::{
    build_shell_command, build_shell_command_silent, collect_byte_cap, output_log_dir,
    proxy_env_vars, write_output_log,
};
pub use skill_runner::SkillRunner;
pub use tool_contract::{
    ConfirmationRequirement, DataSensitivity, NetworkAccess, OperationEffect, OperationIdempotency,
    OperationPolicy, StructuredToolError, Tool, ToolAvailability, ToolBox, ToolCancellationPolicy,
    ToolConcurrency, ToolDef, ToolErrorClass, ToolErrorMetadata, ToolExecutionOutcome,
    ToolIdentity, ToolLlmUsage, ToolManifest, ToolModel, ToolOperationMetadata, ToolOperationScope,
    ToolPolicy, ToolPresentation, ToolRegistration, ToolResult, ToolResultEnvelope,
    ToolRetryability, ToolRootPresentation, ToolSignals, ToolSource, TypedToolAdapter,
    TypedToolOperation, extract_ask_signal, extract_notify_signal, is_silent_action,
    parse_tool_input,
};
pub use tool_runtime::{
    LogLevelPort, MemoryRecallPort, MemoryRecallSlot, RuntimeCapabilities, StartupWiring,
    ToolControlPort,
};

/// Convert an internal qualified tool name (`mcp::server::tool`, `skill::name`)
/// into a provider-safe form (`mcp__server__tool`, `skill__name`) accepted by
/// tool-calling LLM APIs. OpenAI-compatible providers
/// restrict tool names to `^[a-zA-Z0-9_-]+$` (DeepSeek rejects the `::`
/// namespace separator with a 400, which permanently errors the session after a
/// successful `load_mcp`); Anthropic additionally caps the length at 64.
/// The transform is deterministic so the name advertised to the model in the
/// tool definitions always equals the per-session registration key used for
/// execution lookup — no reverse mapping is needed. A digest suffix is kept
/// when truncating, otherwise two long MCP names with the same prefix could
/// silently overwrite one another in the session catalog.
pub fn llm_tool_name(qualified: &str) -> String {
    let mut out: String = qualified
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.len() > 64 {
        let digest = Sha256::digest(qualified.as_bytes());
        let suffix: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let prefix_len = 64 - 1 - suffix.len();
        let idx = out.floor_char_boundary(prefix_len);
        out.truncate(idx);
        out.push('_');
        out.push_str(&suffix);
    }
    out
}

/// Lightweight sanitizer for strings interpolated into the system prompt:
/// replaces control characters (newlines, tabs) that could inject prompt
/// text, and caps the length. Shared implementation lives in
/// `haven_common::text` so the policy cannot drift from the agent prompt /
/// fact sanitizers.
fn sanitize_index_field(s: &str) -> String {
    haven_common::text::sanitize_prompt_field(s, 256)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolBudgetPriority {
    /// Builtin operation views are the stable core surface. They must be
    /// considered before any user-extensible source.
    Builtin = 0,
    /// A tool explicitly loaded into this session (normally an MCP tool) is
    /// preferred over globally enabled optional tools. This preserves the
    /// meaning of a successful explicit load without changing its admission
    /// check in `register_mcp_for_session`.
    Session = 1,
    /// Globally enabled skills are useful, but must not displace the core
    /// operation views when the provider budget is tight.
    Optional = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolBudgetOrigin {
    Global,
    Session,
}

#[derive(Debug)]
struct ToolBudgetCandidate {
    def: ToolDef,
    origin: ToolBudgetOrigin,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ToolBudgetSelection {
    selected: Vec<ToolDef>,
    omitted: Vec<String>,
    omitted_core: usize,
}

impl ToolBudgetCandidate {
    fn source(&self) -> Option<ToolSource> {
        self.def
            .manifest
            .as_ref()
            .map(|manifest| manifest.identity.source)
            .or_else(|| {
                // ToolDef is the shared contract, but keep the selector
                // defensive for legacy/custom definitions that predate the
                // manifest field. The name prefixes are the same canonical
                // boundaries used by the adapters.
                if self.def.name.starts_with("skill__") {
                    Some(ToolSource::Skill)
                } else if self.def.name.starts_with("mcp__") {
                    Some(ToolSource::Mcp)
                } else {
                    None
                }
            })
    }

    fn priority(&self) -> ToolBudgetPriority {
        if self.origin == ToolBudgetOrigin::Session {
            // Registration location is authoritative here. A custom tool
            // created through the default Tool implementation may carry a
            // Builtin source in its inferred manifest, but it is still an
            // explicit session overlay rather than part of the core catalog.
            return ToolBudgetPriority::Session;
        }
        match self.source() {
            Some(ToolSource::Builtin) => ToolBudgetPriority::Builtin,
            Some(ToolSource::Skill | ToolSource::Mcp) | None => ToolBudgetPriority::Optional,
        }
    }
}

/// Select a stable provider-facing tool surface under the per-request budget.
///
/// The registry order is an implementation detail and must not decide which
/// capabilities survive a tight provider limit. Builtins are always selected
/// before optional sources; explicitly registered session tools (including
/// MCP tools admitted by `register_mcp_for_session`) come next; globally
/// enabled skills are last. Every bucket is sorted by stable tool name so the
/// result does not depend on HashMap iteration or skill discovery order.
///
/// The returned omitted names are intentionally kept separate from the
/// provider definitions. The current public API returns only `Vec<ToolDef>`,
/// so `list_defs_for_session` logs this explanation without changing the
/// cross-crate contract.
fn select_tool_defs_for_budget(
    global: Vec<ToolDef>,
    session: Vec<ToolDef>,
    max: usize,
) -> ToolBudgetSelection {
    let mut candidates: Vec<_> = global
        .into_iter()
        .map(|def| ToolBudgetCandidate {
            def,
            origin: ToolBudgetOrigin::Global,
        })
        .chain(session.into_iter().map(|def| ToolBudgetCandidate {
            def,
            origin: ToolBudgetOrigin::Session,
        }))
        .collect();

    candidates.sort_by(|a, b| {
        a.priority()
            .cmp(&b.priority())
            .then_with(|| a.def.name.cmp(&b.def.name))
    });

    let selected_len = max.max(1).min(candidates.len());
    let omitted_core = candidates
        .iter()
        .skip(selected_len)
        .filter(|candidate| candidate.priority() == ToolBudgetPriority::Builtin)
        .count();
    let omitted = candidates
        .iter()
        .skip(selected_len)
        .map(|candidate| candidate.def.name.clone())
        .collect();
    let selected = candidates
        .into_iter()
        .take(selected_len)
        .map(|candidate| candidate.def)
        .collect();

    ToolBudgetSelection {
        selected,
        omitted,
        omitted_core,
    }
}

/// Composition object for the tool subsystem.
///
/// The manager intentionally contains three explicit boundaries rather than
/// exposing every provider and mutable dependency as a public field:
/// `ToolCore` owns contracts/catalog/authorization, `ToolRuntime` owns
/// execution capabilities, and `ToolBuiltins` owns concrete MCP/Skills
/// providers. Application code uses the narrow accessors below.
pub struct ToolsManager {
    core: tool_core::ToolCore,
    runtime: tool_runtime::ToolRuntime,
    builtins: tool_builtins::ToolBuiltins,
}

impl ToolsManager {
    pub fn new() -> Self {
        Self::new_with_exec_config(SkillsExecConfig::default())
    }

    pub fn new_with_exec_config(exec_config: SkillsExecConfig) -> Self {
        Self {
            core: tool_core::ToolCore::new(),
            runtime: tool_runtime::ToolRuntime::new(),
            builtins: tool_builtins::ToolBuiltins::new(exec_config),
        }
    }

    /// Bind the single session runtime used by peer spawn, lifecycle control,
    /// and in-process actor-mailbox delivery.
    pub fn bind_messaging_runtime(&self, runtime: Arc<dyn MessagingRuntime>) -> anyhow::Result<()> {
        self.runtime.bind_messaging_runtime(runtime)
    }

    /// Install History-aligned recall for `memory` operation=recall.
    pub fn bind_memory_recall(&self, recall: Arc<dyn MemoryRecallPort>) -> anyhow::Result<()> {
        self.runtime.bind_memory_recall(recall)
    }

    /// Create a non-owning, typed admin capability for live tool toggles.
    /// `Weak` prevents the native admin surface from forming a manager cycle.
    pub fn tool_control_port(self: &Arc<Self>) -> Arc<dyn ToolControlPort> {
        Arc::new(ToolControlHandle(Arc::downgrade(self)))
    }

    /// Core catalog view. These accessors expose domain boundaries without
    /// exposing `ToolsManager`'s composition fields.
    pub fn registry(&self) -> &ToolRegistry {
        &self.core.registry
    }

    pub fn authorization(&self) -> &AuthorizationEngine {
        &self.core.authorization
    }

    /// Builtin discovery services are domain views, not replaceable manager
    /// fields.
    pub fn mcp_manager(&self) -> &McpManager {
        &self.builtins.mcp_manager
    }

    pub fn mcp_server_configs(&self) -> &Arc<RwLock<HashMap<String, McpServerConfig>>> {
        &self.builtins.mcp_server_configs
    }

    pub fn skills_engine(&self) -> &SkillsEngine {
        &self.builtins.skills_engine
    }

    pub fn skill_runner(&self) -> &Arc<RwLock<SkillRunner>> {
        &self.builtins.skill_runner
    }

    pub fn managed_assets(&self) -> &ManagedAssetRegistry {
        &self.runtime.managed_assets
    }

    pub fn action_service(&self) -> &Arc<ActionService> {
        &self.runtime.action_service
    }

    pub fn live_outputs(&self) -> &Arc<live_output::LiveOutputHub> {
        &self.runtime.live_outputs
    }

    /// Register host-persisted attachments for the trusted files boundary and
    /// hold them in an ingress lease until a newly created session can claim
    /// them. Renderer-provided ids are not accepted because validation clears
    /// them before persistence mints a fresh host-owned id.
    pub fn register_managed_assets(&self, attachments: &[MessageAttachment]) {
        let uploads_root = haven_common::default_work_dir().join("uploads");
        for attachment in attachments {
            let (Some(asset_id), Some(path)) = (&attachment.asset_id, &attachment.path) else {
                continue;
            };
            if !self.runtime.managed_assets.register_under_root_pending(
                &uploads_root,
                asset_id.clone(),
                std::path::PathBuf::from(path),
                attachment.filename.clone(),
                attachment.media_type.clone(),
            ) {
                tracing::warn!(
                    asset_id = %asset_id,
                    "rejecting managed attachment outside the host uploads root"
                );
            }
        }
    }

    /// Register attachments and hold them for the lifetime of a live session.
    /// This protects event-backed assets before their `messages` projection is
    /// visible to retention cleanup.
    pub fn register_managed_assets_for_session(
        &self,
        session_id: &str,
        attachments: &[MessageAttachment],
    ) {
        let uploads_root = haven_common::default_work_dir().join("uploads");
        let generated_root = haven_common::config::default_generated_media_dir();
        for attachment in attachments {
            let (Some(asset_id), Some(path)) = (&attachment.asset_id, &attachment.path) else {
                continue;
            };
            let path = std::path::PathBuf::from(path);
            if self.runtime.managed_assets.register_under_root_for_session(
                session_id,
                &uploads_root,
                asset_id.clone(),
                path.clone(),
                attachment.filename.clone(),
                attachment.media_type.clone(),
            ) {
                continue;
            }
            let expires_at = match attachment.expires_at.as_deref() {
                Some(value) => match DateTime::parse_from_rfc3339(value) {
                    Ok(value) => Some(value.with_timezone(&Utc)),
                    Err(error) => {
                        tracing::warn!(
                            asset_id = %asset_id,
                            session_id = %session_id,
                            error = %error,
                            "rejecting generated attachment with invalid expiry metadata"
                        );
                        continue;
                    }
                },
                None => None,
            };
            if !self
                .runtime
                .managed_assets
                .register_under_root_for_session_with_metadata(
                    session_id,
                    &generated_root,
                    asset_id.clone(),
                    path,
                    attachment.filename.clone(),
                    attachment.media_type.clone(),
                    attachment.sha256.clone(),
                    attachment.size_bytes,
                    expires_at,
                )
            {
                tracing::warn!(
                    asset_id = %asset_id,
                    session_id = %session_id,
                    "rejecting managed attachment outside the host uploads root or session lease"
                );
            }
        }
    }

    /// Bind assets registered before new-session allocation to the resulting
    /// session lease.
    pub fn bind_pending_managed_assets_to_session(
        &self,
        session_id: &str,
        attachments: &[MessageAttachment],
    ) {
        for attachment in attachments {
            let Some(asset_id) = attachment.asset_id.as_deref() else {
                continue;
            };
            if !self
                .runtime
                .managed_assets
                .bind_pending_to_session(session_id, asset_id)
            {
                tracing::warn!(
                    asset_id = %asset_id,
                    session_id = %session_id,
                    "failed to bind pending managed attachment to session lease"
                );
            }
        }
    }

    /// Release assets registered for an ingress request whose new session was
    /// never created. Unreferenced entries are removed by the next GC pass.
    pub fn release_pending_managed_assets(&self, attachments: &[MessageAttachment]) {
        for attachment in attachments {
            if let Some(asset_id) = attachment.asset_id.as_deref() {
                self.runtime.managed_assets.release_pending(asset_id);
            }
        }
    }

    /// Release the process-local asset lease held by a terminal session.
    pub fn release_managed_assets_for_session(&self, session_id: &str) {
        self.runtime.managed_assets.release_session(session_id);
    }

    /// Monotonic catalog version (see `catalog_version`). Consumers cache
    /// derived views (e.g. per-step LLM tool definitions) keyed by this
    /// value and rebuild only when it changes.
    pub fn catalog_version(&self) -> u64 {
        self.core.session_catalog.global_version()
    }

    /// MCP has its own tools/list change clock and therefore must participate
    /// in prompt-index cache keys independently of the builtin registry.
    pub fn mcp_catalog_version(&self) -> u64 {
        self.builtins.mcp_manager.catalog_version()
    }

    /// Version pair for a session's complete tool-definition view. The first
    /// component covers global registry changes; the second covers only that
    /// session's progressive MCP overlay.
    pub async fn catalog_version_for_session(&self, session_id: &str) -> (u64, u64) {
        self.core
            .session_catalog
            .catalog_version_for_session(session_id)
            .await
    }

    /// Capture the complete lookup surface used by one ReAct tool batch.
    ///
    /// Global and session-overlay registries are copied into one name index;
    /// later admission metadata reads are synchronous map lookups rather than
    /// one async catalog walk per call. A bounded version check avoids
    /// publishing a mixed view when a loader updates the session while the
    /// snapshot is being assembled. The final attempt is deliberately used
    /// under sustained catalog churn: this is a performance snapshot, while
    /// the execution boundary remains responsible for a final runtime check.
    pub async fn tool_catalog_snapshot(&self, session_id: &str) -> ToolCatalogSnapshot {
        let mut snapshot = None;
        for _ in 0..2 {
            let before = self.catalog_version_for_session(session_id).await;
            let global = self.core.registry.list().await;
            let session = self.core.session_catalog.list(session_id).await;
            let after = self.catalog_version_for_session(session_id).await;

            let mut tools = HashMap::with_capacity(global.len() + session.len());
            let global_defs = global.iter().map(|tool| tool.tool_def()).collect();
            let session_defs = session.iter().map(|tool| tool.tool_def()).collect();
            for tool in global {
                tools.insert(tool.name(), tool);
            }
            for tool in session {
                tools.insert(tool.name(), tool);
            }
            let max = self
                .core
                .context_limits
                .read()
                .await
                .max_tools_per_request
                .max(1);
            let provider_definitions =
                select_tool_defs_for_budget(global_defs, session_defs, max).selected;
            snapshot = Some((after, tools, provider_definitions));
            if before == after {
                break;
            }
        }
        let (version, tools, provider_definitions) =
            snapshot.expect("tool catalog snapshot attempt must produce a view");
        ToolCatalogSnapshot::new_with_definitions(version, tools, provider_definitions)
    }

    /// Replace the shared LlmRouter and rebuild the catalog so tools (e.g.
    /// `file summary`) pick up the new endpoint config.
    pub async fn set_router(&self, router: Arc<LlmRouter>) {
        *self.runtime.router.write().await = Some(router);
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["media", "files"]))
            .await;
    }

    /// Replace the router and media clients together during a live settings
    /// update, then rebuild the builtin catalog once so media tools cannot
    /// observe a mixed-generation runtime.
    pub async fn set_router_and_media_clients(
        &self,
        router: Arc<LlmRouter>,
        stt_client: Option<Arc<dyn haven_llm::SttClient>>,
        ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
        image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
        tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
        media_config: haven_common::config::MediaConfig,
    ) {
        *self.runtime.router.write().await = Some(router);
        *self.runtime.stt_client.write().await = stt_client;
        *self.runtime.ocr_client.write().await = ocr_client;
        *self.runtime.image_gen_client.write().await = image_gen_client;
        *self.runtime.tts_client.write().await = tts_client;
        *self.runtime.media_config.write().await = media_config;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["media", "files", "window"]))
            .await;
    }

    /// Apply cold-start wiring in one pass and rebuild the catalog once.
    /// Avoids the N sequential rebuilds that used to block window creation
    /// (`set_tool_settings` + `set_default_shell` + `set_context_limits` +
    /// `set_router` + audio/TTS wiring + admin context).
    pub async fn wire_startup(&self, wiring: StartupWiring) {
        let StartupWiring {
            tool_settings,
            default_shell,
            context_limits,
            security,
            router,
            media_config,
            audio_pipeline,
            stt_client,
            ocr_client,
            image_gen_client,
            tts_client,
            admin_context,
        } = wiring;
        *self.core.tool_settings.write().await = tool_settings.clone();
        *self.builtins.default_shell.write().await = default_shell;
        self.builtins.mcp_manager.set_limits(&context_limits).await;
        self.builtins
            .skills_engine
            .set_limits(&context_limits)
            .await;
        self.runtime
            .action_service
            .set_limits(&context_limits)
            .await;
        self.runtime.live_outputs.set_limits(&context_limits).await;
        *self.core.context_limits.write().await = context_limits;
        self.apply_security(&security).await;
        self.core
            .authorization
            .set_tool_settings(tool_settings)
            .await;
        *self.runtime.router.write().await = Some(router);
        *self.runtime.media_config.write().await = media_config;
        *self.runtime.audio_pipeline.write().await = audio_pipeline;
        *self.runtime.stt_client.write().await = stt_client;
        *self.runtime.ocr_client.write().await = ocr_client;
        *self.runtime.image_gen_client.write().await = image_gen_client;
        *self.runtime.tts_client.write().await = tts_client;
        self.runtime
            .action_service
            .set_db(admin_context.db.clone())
            .await;
        *self.runtime.admin_context.write().await = Some(admin_context);
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    /// Apply the security configuration to every runtime boundary that needs
    /// the same snapshot. The authorization engine protects tool execution;
    /// the MCP manager additionally protects startup, refresh, reconnect, and
    /// health-monitor connection paths.
    pub async fn apply_security(&self, security: &SecurityConfig) {
        self.core.authorization.apply_security(security).await;
        self.builtins
            .mcp_manager
            .set_network_policy(security.network_policy)
            .await;
    }

    /// Wire the app-level context for the five native admin surfaces. Called by the
    /// desktop shell after the config loader exists; later catalog rebuilds
    /// keep the capability-scoped adapters registered. Also hands the DB to
    /// the unified action state machine so timer and process action results
    /// persist across restarts.
    pub async fn set_admin_context(&self, ctx: builtin::AdminContext) {
        self.runtime.action_service.set_db(ctx.db.clone()).await;
        *self.runtime.admin_context.write().await = Some(ctx);
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        let affected = {
            let current = self.core.tool_settings.read().await;
            current
                .keys()
                .chain(settings.keys())
                .filter(|name| current.get(*name) != settings.get(*name))
                .map(|name| name.split('.').next().unwrap_or(name).to_string())
                .collect::<HashSet<_>>()
        };
        *self.core.tool_settings.write().await = settings.clone();
        self.core.authorization.set_tool_settings(settings).await;
        if !affected.is_empty() {
            self.rebuild_catalog_scoped(CatalogRebuildScope::Roots(affected))
                .await;
        }
    }

    /// The five native admin surfaces, when the desktop shell wired the app
    /// context. The model sees the same operations through five typed adapters.
    pub async fn admin_surfaces(&self) -> Option<Arc<builtin::AdminSurfaces>> {
        self.runtime.admin_surfaces.read().await.clone()
    }

    /// Flip the `enabled` flag for one builtin tool in the in-memory
    /// `tool_settings` and rebuild the catalog so the toggle takes effect on
    /// the agent's next step. The config.toml persistence is done by the
    /// caller (the admin surface's `tool_enable`/`tool_disable` operations,
    /// which call this after persisting).
    pub async fn set_tool_enabled(&self, name: &str, enabled: bool) {
        let mut settings = self.core.tool_settings.write().await;
        settings
            .entry(name.to_string())
            .or_insert_with(ToolConfig::default)
            .enabled = enabled;
        drop(settings);
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots([name
            .split('.')
            .next()
            .unwrap_or(name)]))
            .await;
    }

    /// Replace the unified context limits (global tool output cap etc.) and
    /// rebuild the catalog so tools pick up the new values.
    pub async fn set_context_limits(&self, limits: ContextLimitsConfig) {
        self.builtins.mcp_manager.set_limits(&limits).await;
        self.builtins.skills_engine.set_limits(&limits).await;
        self.runtime.action_service.set_limits(&limits).await;
        self.runtime.live_outputs.set_limits(&limits).await;
        *self.core.context_limits.write().await = limits;
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    /// Replace the default shell for the `shell` tool and rebuild the catalog
    /// so the running agent picks up the new value on its next step.
    pub async fn set_default_shell(&self, shell: ShellChoice) {
        *self.builtins.default_shell.write().await = shell;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["shell"]))
            .await;
    }

    /// Snapshot the shell default used by the model-facing `shell` tool.
    pub async fn default_shell_name(&self) -> String {
        self.builtins
            .default_shell
            .read()
            .await
            .as_str()
            .to_string()
    }

    /// Snapshot the limits that shape model-visible tool and observation
    /// budgets. Prompt assembly uses this instead of duplicating defaults.
    pub async fn context_limits(&self) -> ContextLimitsConfig {
        self.core.context_limits.read().await.clone()
    }

    /// Whether the model-facing `media.speak` operation has a live TTS
    /// backend. This is intentionally separate from the media tool's schema
    /// so prompt assembly can report the same capability state.
    pub async fn tts_configured(&self) -> bool {
        self.runtime.tts_client.read().await.is_some()
    }

    /// Whether the shared media transcription boundary currently has a live
    /// route. This is the app-facing gate for voice ingress; capture itself is
    /// owned by `haven-input` and is intentionally not consulted here.
    pub async fn transcription_available(&self) -> bool {
        let router = self.runtime.router.read().await.clone();
        let stt_client = self.runtime.stt_client.read().await.clone();
        builtin::resolve_media_capabilities(router.as_ref(), stt_client.is_some())
            .await
            .transcribe
    }

    /// Transcribe app-captured WAV data through the same media provider
    /// policy used by `media.transcribe`: dedicated STT first, then the LLM
    /// route when the dedicated result is unusable. The input crate never
    /// sees provider clients or fallback decisions.
    pub async fn transcribe_recording(
        &self,
        wav_data: &[u8],
        cancel: CancellationToken,
    ) -> builtin::MediaTranscriptionResult {
        let router = self.runtime.router.read().await.clone();
        let stt_client = self.runtime.stt_client.read().await.clone();
        let capabilities =
            builtin::resolve_media_capabilities(router.as_ref(), stt_client.is_some()).await;
        if !capabilities.transcribe {
            return builtin::MediaTranscriptionResult::unavailable(
                "No speech-to-text provider is configured.",
            );
        }
        let media_config = self.runtime.media_config.read().await.clone();
        let limits = self.core.context_limits.read().await;
        let max_output_chars = limits.max_observation_chars;
        drop(limits);
        builtin::media::MediaTranscriber::new(
            router,
            stt_client,
            media_config.stt.timeout_secs,
            media_config.stt.min_confidence,
            max_output_chars,
        )
        .transcribe_wav(wav_data, &cancel)
        .await
    }

    /// Return the same live capability decisions used while rebuilding the
    /// builtin catalog. Keeping this at the manager boundary prevents the
    /// prompt snapshot from advertising a role that the tool schema removed.
    pub async fn runtime_capabilities(&self) -> RuntimeCapabilities {
        let router = self.runtime.router.read().await.clone();
        let stt_client = self.runtime.stt_client.read().await.clone();
        let media_capabilities =
            builtin::resolve_media_capabilities(router.as_ref(), stt_client.is_some()).await;
        let vision = media_capabilities.describe;
        let transcription = media_capabilities.transcribe;
        let audio_pipeline = self.runtime.audio_pipeline.read().await.clone();
        // Capturing and transcribing are separate capabilities: a recording
        // must remain available even when STT is temporarily unconfigured so
        // it can still produce an asset for a later `media.transcribe` call.
        // Recording is a capture capability. It remains available without an
        // STT provider so a managed audio asset can be retained for later
        // derivation.
        let recording = audio_pipeline.is_some();
        let image_generation = self.runtime.image_gen_client.read().await.is_some();
        let tts = self.runtime.tts_client.read().await.is_some();
        let mcp_search_available = self
            .build_mcp_index()
            .await
            .iter()
            .any(mcp_index_entry_has_search_tool);
        let web_search = match router.as_ref() {
            Some(router) => {
                let config = router.config().await;
                let endpoint = config
                    .route(RequestKind::Chat)
                    .map(|model| &model.endpoint)
                    .unwrap_or_else(|| {
                        // No configured route means provider search is not
                        // available; the default endpoint is never probed.
                        static EMPTY: std::sync::OnceLock<haven_common::config::ModelEndpoint> =
                            std::sync::OnceLock::new();
                        EMPTY.get_or_init(Default::default)
                    });
                let style = haven_llm::adapters::api_style_for(endpoint);
                let mode = haven_llm::adapters::resolve_web_search_mode(endpoint);
                if config.route(RequestKind::Chat).is_some()
                    && !matches!(mode, haven_llm::WebSearchMode::Off)
                    && haven_llm::supports_builtin_web_search(style)
                {
                    "provider".into()
                } else if mcp_search_available {
                    "mcp".into()
                } else {
                    "unavailable (no provider builtin search; no MCP search server)".into()
                }
            }
            None if mcp_search_available => "mcp".into(),
            None => "unavailable (no provider builtin search; no MCP search server)".into(),
        };
        RuntimeCapabilities {
            vision,
            image_generation,
            transcription,
            recording,
            tts,
            web_search,
        }
    }

    /// Replace the TTS client used by the `media` tool after a live settings
    /// update. A disabled or failed client is represented by `None`.
    pub async fn set_tts_client(&self, client: Option<Arc<dyn haven_llm::TtsClient>>) {
        *self.runtime.tts_client.write().await = client;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["media"]))
            .await;
    }

    pub async fn load_mcp_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        // Store configs for dynamic loading via load_mcp tool
        let mut configs = self.builtins.mcp_server_configs.write().await;
        configs.clear();
        for server in servers {
            configs.insert(server.name.clone(), server.clone());
        }
        drop(configs);

        // Configuration changes alter the discovery catalog even before a
        // client has connected. Keep catalog pagination/resume consumers on
        // the same invalidation clock as builtin rebuilds.
        self.core.session_catalog.bump_global_version();

        self.builtins.mcp_manager.load_from_config(servers).await;
    }

    pub async fn discover_all(
        &self,
        servers: &[haven_common::McpServerConfig],
        config: &haven_common::McpDiscoveryConfig,
    ) {
        // Populate the in-memory index so `self mcp_list` and
        // `build_mcp_index` see the configured servers right after startup,
        // before any config mutation (the index was previously only filled
        // by `update_settings` → `load_mcp_from_config`).
        {
            let mut configs = self.builtins.mcp_server_configs.write().await;
            configs.clear();
            for server in servers {
                configs.insert(server.name.clone(), server.clone());
            }
        }
        self.core.session_catalog.bump_global_version();
        self.builtins
            .mcp_manager
            .discover_all(servers, config)
            .await;
    }

    /// Rebuild the tool catalog from the current builtin state.
    /// Called at startup and whenever MCP or Skills state changes.
    ///
    /// MCP servers are progressively loaded: the `load_mcp` meta-tool is
    /// advertised only when an enabled server exists and its adapters are
    /// registered per-session. Enabled skills are ordinary global tools and
    /// are rebuilt into the catalog from the live skills index.
    pub async fn rebuild_catalog(&self) {
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    /// Rebuild only the runtime instances affected by a live wiring change.
    /// The registration pass emits the current static operation set and live
    /// capability views. Unchanged runtime instances are retained by name so
    /// a scoped update does not tear down unrelated providers or action
    /// adapters.
    async fn rebuild_catalog_scoped(&self, scope: CatalogRebuildScope) {
        let mut all_tools: Vec<ToolBox> = Vec::new();
        let previous_tools = self.core.all_builtin_tools.read().await.clone();
        let previous_by_name: HashMap<String, ToolBox> = previous_tools
            .into_iter()
            .map(|tool| (tool.name(), tool))
            .collect();

        // Register builtin tools, including capability-scoped progressive
        // loaders when an enabled skill/MCP source is actually available.
        let context = self.builtins.build_context(&self.core, &self.runtime).await;
        let settings = context.settings.clone();
        let admin_surfaces = builtin::register_builtin_tools(&mut all_tools, context).await;

        // Keep the full list (enabled + disabled) for the UI, and exclude
        // disabled tools from the registry the agent sees.
        let all_tools: Vec<ToolBox> = all_tools
            .into_iter()
            .map(|tool| {
                if scope.affects(&tool.name()) {
                    tool
                } else {
                    previous_by_name.get(&tool.name()).cloned().unwrap_or(tool)
                }
            })
            .collect();
        let enabled_tools: Vec<ToolBox> = all_tools
            .iter()
            .filter(|t| tool_config_enabled(&settings, &t.name()))
            .cloned()
            .collect();
        drop(settings);

        let (active_tools, deferred_tools): (Vec<_>, Vec<_>) = enabled_tools
            .iter()
            .cloned()
            .partition(|tool| is_core_model_tool(&tool.name()));
        if let Err(error) = self.core.registry.rebuild(active_tools).await {
            // Keep the previous atomic snapshot on a construction conflict.
            // A partial catalog is more dangerous than a stale one because it
            // can make authorization and execution disagree about a name.
            tracing::error!(error = %error, "builtin catalog rebuild rejected");
            return;
        }
        self.core.deferred_catalog.replace(deferred_tools).await;
        *self.core.all_builtin_tools.write().await = all_tools;
        *self.runtime.admin_surfaces.write().await = admin_surfaces;
        self.core.session_catalog.bump_global_version();
    }

    /// Register a tool for a specific session (per-session skill overlay).
    /// Does NOT modify the global registry.
    pub async fn register_for_session(&self, session_id: &str, tool: ToolBox) {
        self.core.session_catalog.register(session_id, tool).await;
    }

    /// Rehydrate a saved built-in selection during resume without exposing the
    /// loader's private session field to transcript or provider input.
    pub async fn load_builtin_for_session(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> bool {
        let loader = builtin::load_builtin::LoadBuiltinTool {
            deferred_catalog: self.core.deferred_catalog.clone(),
            registry: self.core.registry.clone(),
            session_catalog: self.core.session_catalog.clone(),
            max_tools_per_request: self
                .core
                .context_limits
                .read()
                .await
                .max_tools_per_request
                .max(1),
        };
        match loader
            .run(
                builtin::load_builtin::LoadBuiltinParams {
                    operations,
                    roots,
                    session_id: Some(session_id.into()),
                },
                CancellationToken::new(),
            )
            .await
        {
            Ok(result) => result.success,
            Err(error) => {
                tracing::warn!(session_id, error = %error, "failed to restore built-in tool selection");
                false
            }
        }
    }

    /// Rehydrate saved Skill selections during resume. A missing or disabled
    /// Skill is a soft restore failure; the session continues with the skills
    /// that are still available.
    pub async fn load_skill_for_session(&self, session_id: &str, names: Vec<String>) -> bool {
        let loader = builtin::load_skill::LoadSkillTool {
            deferred_catalog: self.core.deferred_catalog.clone(),
            registry: self.core.registry.clone(),
            session_catalog: self.core.session_catalog.clone(),
            max_tools_per_request: self
                .core
                .context_limits
                .read()
                .await
                .max_tools_per_request
                .max(1),
        };
        match loader
            .run(
                builtin::load_skill::LoadSkillParams {
                    skill_names: names,
                    session_id: Some(session_id.into()),
                },
                CancellationToken::new(),
            )
            .await
        {
            Ok(result) => result.success,
            Err(error) => {
                tracing::warn!(session_id, error = %error, "failed to restore Skill selection");
                false
            }
        }
    }

    /// Remove all per-session tool registrations for a given session.
    pub async fn unregister_session(&self, session_id: &str) {
        self.core.session_catalog.unregister(session_id).await;
    }

    /// Register tools from an MCP server as per-session adapters.
    /// Looks up the client by server name and registers `McpToolAdapter`
    /// for each selected cached tool. Returns `true` if the client was found.
    ///
    /// `tool_names`: when `Some` and non-empty, only those raw MCP tool names
    /// are registered (resume of a selective `load_mcp`). When `None`, every
    /// cached tool is registered — same all-or-nothing contract as an
    /// unfiltered live `load_mcp`.
    ///
    /// After a restart the server may still be connecting in the background
    /// (`discover_all`), so the tools cache can be empty even though the
    /// server is configured and enabled. Wait briefly (bounded) for the
    /// handshake + tools/list to complete so a fast resume does not register
    /// zero tools and silently lose the session's MCP access. A server that is
    /// definitively offline gives up early instead of stalling the resume.
    ///
    /// Defense in depth for resume: all-or-nothing for the selected set under
    /// the session write lock (same contract as live `load_mcp`). If the
    /// *net-new* tools would exceed the budget, none are registered.
    pub async fn register_mcp_for_session(
        &self,
        session_id: &str,
        server_name: &str,
        tool_names: Option<&[String]>,
    ) -> bool {
        let Some(client) = self.builtins.mcp_manager.get_client(server_name).await else {
            return false;
        };
        let all_tools = client.wait_for_tools(Duration::from_secs(3)).await;
        let tools = match tool_names {
            None => all_tools,
            Some(names) => {
                let want: std::collections::HashSet<&str> =
                    names.iter().map(|s| s.as_str()).collect();
                all_tools
                    .into_iter()
                    .filter(|info| want.contains(info.name.as_str()))
                    .collect::<Vec<_>>()
            }
        };
        let max = self
            .core
            .context_limits
            .read()
            .await
            .max_tools_per_request
            .max(1);
        let global_count = self.core.registry.list().await.len();
        let registrations = self.core.session_catalog.registrations();
        let mut reg = registrations.write().await;
        let entry = reg.entry(session_id.to_string()).or_default();
        let session_count = entry.len();
        let net_new = tools
            .iter()
            .filter(|info| {
                let name = McpToolAdapter::qualified_name_of(server_name, &info.name);
                !entry.contains_key(&name)
            })
            .count();
        if SessionCatalog::tool_budget_would_exceed(max, global_count, session_count, net_new) {
            tracing::warn!(
                session_id,
                server_name,
                net_new,
                max,
                global_count,
                session_count,
                "register_mcp_for_session: refusing server over max_tools_per_request"
            );
            return true;
        }
        for info in tools {
            let adapter = McpToolAdapter::new(client.clone(), server_name, info);
            entry.insert(adapter.name(), Arc::new(adapter));
        }
        drop(reg);
        self.core
            .session_catalog
            .bump_session_version(session_id)
            .await;
        true
    }

    /// Look up a tool: first check per-session registrations, then global registry.
    pub async fn get_tool_for_session(
        &self,
        session_id: Option<&str>,
        name: &str,
    ) -> Option<ToolBox> {
        if let Some(tid) = session_id
            && let Some(tool) = self.core.session_catalog.get(tid, name).await
        {
            return Some(tool);
        }
        self.core.registry.get(name).await
    }

    /// Build an MCP server index (name + available tool names) for injection
    /// into the system prompt. The LLM uses `load_mcp` to get full schemas.
    /// Only enabled servers are listed — disabled ones cannot be loaded.
    /// Tool names are included (when the server is connected and cached) so
    /// the LLM can judge whether a server's tools fit the session instead of
    /// defaulting to weaker built-ins.
    pub async fn build_mcp_index(&self) -> Vec<Value> {
        let configs = self.builtins.mcp_server_configs.read().await;
        let mut entries: Vec<Value> = Vec::new();
        for s in configs.values().filter(|s| s.enabled) {
            let mut tool_names: Vec<String> =
                match self.builtins.mcp_manager.get_client(&s.name).await {
                    Some(client) => client
                        .tools_cache()
                        .await
                        .into_iter()
                        .map(|t| sanitize_index_field(&t.name))
                        .collect(),
                    None => Vec::new(),
                };
            // MCP servers may return an unchanged tool set in a different
            // order after reconnect. Keep the cacheable prompt index stable.
            tool_names.sort();
            tool_names.dedup();
            // Never expose the configured process command or arguments to the
            // model. Besides being irrelevant to `load_mcp`, args commonly
            // contain credentials and are untrusted prompt text. The server
            // name/tool names are enough to choose a server; full schemas are
            // loaded only after the explicit tool call.
            let safe_name = sanitize_index_field(&s.name);
            let description = if tool_names.is_empty() {
                format!("MCP server '{safe_name}'")
            } else {
                format!("MCP server '{safe_name}'; tools: {}", tool_names.join(", "))
            };
            let tool_count = tool_names.len();
            entries.push(serde_json::json!({
                "name": safe_name,
                "description": description,
                "tool_names": tool_names,
                "tool_count": tool_count,
            }));
        }
        // Deterministic ordering for a stable prompt.
        entries.sort_by(|a, b| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .cmp(b["name"].as_str().unwrap_or(""))
        });
        entries
    }

    /// Structured tool definitions for a session: the eager core registry
    /// merged with per-session registered builtin/Skill/MCP adapters. Deferred
    /// builtin and Skill implementations are intentionally absent until a
    /// loader registers them. This is the canonical
    /// surface the ReAct loop turns into provider tool definitions and the
    /// schema listing is derived from — no loose JSON assembly in consumers.
    ///
    /// Capped at `context_limits.max_tools_per_request` with deterministic
    /// source-aware selection. Core builtin operation views are kept before
    /// explicitly loaded session tools. A
    /// successful MCP load still uses the all-or-nothing admission check in
    /// `register_mcp_for_session`; this method only handles defensive
    /// selection if the catalog later grows beyond the provider limit.
    pub async fn list_defs_for_session(&self, session_id: &str) -> Vec<ToolDef> {
        let max = self
            .core
            .context_limits
            .read()
            .await
            .max_tools_per_request
            .max(1);
        let global_defs = self.core.registry.list_defs().await;
        let global_len = global_defs.len();
        let session_defs = self.core.session_catalog.list_defs(session_id).await;
        let total = global_len + session_defs.len();
        let selection = select_tool_defs_for_budget(global_defs, session_defs, max);
        if !selection.omitted.is_empty() {
            let omitted_tools = selection.omitted.join(", ");
            tracing::warn!(
                session_id,
                total,
                max,
                global = global_len,
                selected = selection.selected.len(),
                omitted = selection.omitted.len(),
                omitted_core = selection.omitted_core,
                omitted_tools = %omitted_tools,
                "list_defs_for_session: omitted tools from max_tools_per_request budget; core builtins are selected before optional sources"
            );
        }
        selection.selected
    }

    /// Return tool schemas for a session: global registry schemas derived
    /// from [`ToolDef`]s merged with per-session registered skill/MCP
    /// adapters. Convenience JSON view over [`Self::list_defs_for_session`].
    pub async fn list_schemas_for_session(&self, session_id: &str) -> Vec<Value> {
        self.list_defs_for_session(session_id)
            .await
            .into_iter()
            .map(|d| d.json())
            .collect()
    }

    /// Insert or replace a single MCP server config in the in-memory map.
    /// Used by bridge commands (add/update/toggle) to keep `server_configs`
    /// in sync without reconnecting all servers.
    pub async fn upsert_mcp_server_config(&self, config: McpServerConfig) {
        self.builtins
            .mcp_server_configs
            .write()
            .await
            .insert(config.name.clone(), config);
        self.core.session_catalog.bump_global_version();
    }

    /// Remove a single MCP server config from the in-memory map.
    pub async fn remove_mcp_server_config(&self, name: &str) {
        self.builtins.mcp_server_configs.write().await.remove(name);
        self.core.session_catalog.bump_global_version();
    }

    /// List all known MCP server configs (enabled and disabled).
    pub async fn list_mcp_server_configs(&self) -> Vec<McpServerConfig> {
        self.builtins
            .mcp_server_configs
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Whether a tool is enabled per `tool_settings`. Tools without a
    /// settings entry are enabled by default.
    pub async fn tool_enabled(&self, name: &str) -> bool {
        tool_config_enabled(&*self.core.tool_settings.read().await, name)
    }

    /// Schemas for ALL model-facing builtin operation views (enabled and disabled) plus their
    /// `enabled` state, so the UI can list every tool and re-enable disabled
    /// ones. The registry itself only holds enabled tools (see
    /// `rebuild_catalog`).
    /// Poll the skills directory for changes and auto-refresh the engine
    /// whenever `SKILL.md` files are added / modified / removed. The first
    /// pass always refreshes too, so a UI that loaded before the initial
    /// scan finished (startup race) still catches up. `on_change` fires on
    /// the background action after a successful refresh so callers can
    /// re-sync views / emit events (e.g. `skills:status_change`).
    pub async fn run_skills_watcher(
        self: Arc<Self>,
        poll_interval: Duration,
        cancellation: CancellationToken,
        on_change: impl Fn() + Send + Sync + 'static,
    ) {
        let engine = self.builtins.skills_engine.clone();
        let mut last_sig: Option<Vec<(std::path::PathBuf, std::time::SystemTime, u64)>> = None;
        loop {
            let sig = tokio::select! {
                _ = cancellation.cancelled() => return,
                sig = engine.folder_signature() => sig,
            };
            let changed = last_sig.is_none() || last_sig.as_ref() != Some(&sig);
            if changed {
                match tokio::select! {
                    _ = cancellation.cancelled() => return,
                    result = engine.refresh_from_disk() => result,
                } {
                    Ok(()) => {
                        // Commit the signature only after a successful
                        // refresh: on error the old signature is kept so
                        // the next poll retries instead of treating the
                        // failed change as already seen.
                        last_sig = Some(sig);
                        self.rebuild_catalog().await;
                        on_change();
                    }
                    Err(e) => {
                        tracing::warn!("skills auto-refresh failed: {e}");
                    }
                }
            }
            tokio::select! {
                _ = cancellation.cancelled() => return,
                _ = tokio::time::sleep(poll_interval) => {}
            }
        }
    }

    pub async fn list_builtin_tools(&self) -> Vec<Value> {
        let tools = self.core.all_builtin_tools.read().await;
        let settings = self.core.tool_settings.read().await;
        tools
            .iter()
            .filter(|t| !t.name().starts_with("skill__"))
            .map(|t| {
                let def = t.tool_def();
                // ToolDef is the canonical catalog projection. Rebuilding a
                // second manifest directly from the runtime adapter here can
                // drift from custom/operation-view metadata (root, policy or
                // presentation) that the definition already carries.
                let mut manifest = def.manifest.clone().unwrap_or_else(|| t.tool_manifest());
                manifest.availability.enabled = tool_config_enabled(&settings, &t.name());
                let mut json = def.json();
                json.as_object_mut()
                    .expect("ToolDef::json returns an object")
                    .insert(
                        "catalog_group".into(),
                        Value::String(manifest.identity.catalog_group.as_str().into()),
                    );
                json.as_object_mut()
                    .expect("ToolDef::json returns an object")
                    .insert(
                        "enabled".into(),
                        serde_json::json!(manifest.availability.enabled),
                    );
                json.as_object_mut()
                    .expect("ToolDef::json returns an object")
                    .insert(
                        "manifest".into(),
                        serde_json::to_value(manifest).unwrap_or(Value::Null),
                    );
                json
            })
            .collect()
    }

    /// Canonical UI catalog projection. Unlike `list_builtin_tools`, this
    /// does not merge provider-facing fields into a second flat DTO: the
    /// manifest is the only source the frontend should hydrate.
    pub async fn list_builtin_manifests(&self) -> Vec<ToolManifest> {
        let tools = self.core.all_builtin_tools.read().await;
        let settings = self.core.tool_settings.read().await;
        tools
            .iter()
            .filter(|tool| !tool.name().starts_with("skill__"))
            .map(|tool| {
                let def = tool.tool_def();
                let mut manifest = def.manifest.clone().unwrap_or_else(|| tool.tool_manifest());
                manifest.availability.enabled = tool_config_enabled(&settings, &tool.name());
                manifest
            })
            .collect()
    }

    /// Prompt-facing catalog of every enabled builtin, including deferred
    /// operation views. This intentionally returns structured definitions only
    /// to the agent prompt builder; provider `tools[]` still uses the smaller
    /// core + session-loaded surface from `list_defs_for_session`.
    pub async fn list_enabled_builtin_defs(&self) -> Vec<ToolDef> {
        let tools = self.core.all_builtin_tools.read().await;
        let settings = self.core.tool_settings.read().await;
        let mut defs: Vec<_> = tools
            .iter()
            .filter(|tool| !tool.name().starts_with("skill__"))
            .filter(|tool| tool_config_enabled(&settings, &tool.name()))
            .map(|tool| tool.tool_def())
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }
}

struct ToolControlHandle(std::sync::Weak<ToolsManager>);

#[async_trait::async_trait]
impl ToolControlPort for ToolControlHandle {
    async fn set_tool_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        let tools = self
            .0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("tool catalog is no longer available"))?;
        tools.set_tool_enabled(name, enabled).await;
        Ok(())
    }
}

impl Default for ToolsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolsManager {
    pub async fn execute_tool(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        self.execute_tool_with_step(session_id, tool_name, input, cancel, None)
            .await
    }

    /// Like [`Self::execute_tool`], but also injects the pre-minted `step-*`
    /// id so tools that stream live output (shell) can key `agent:tool_output`
    /// events to the matching chat card.
    pub async fn execute_tool_with_step(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
        step_id: Option<&str>,
    ) -> anyhow::Result<ToolResult> {
        if !self.core.tool_circuits.allow_request(tool_name) {
            tracing::warn!("tool '{}' circuit breaker open — fast-failing", tool_name);
            return Err(anyhow::Error::new(StructuredToolError::new(
                format!(
                    "tool '{}' is temporarily unavailable (circuit breaker open)",
                    tool_name
                ),
                ToolErrorMetadata::transient(),
            )));
        }

        if !self.tool_enabled(tool_name).await {
            tracing::warn!("tool '{}' is disabled", tool_name);
            return Err(anyhow::Error::new(StructuredToolError::new(
                format!("tool '{}' is disabled", tool_name),
                ToolErrorMetadata {
                    class: ToolErrorClass::Permission,
                    outcome: ToolExecutionOutcome::Failed,
                    retryability: ToolRetryability::NotRetryable,
                },
            )));
        }

        let tool = self
            .get_tool_for_session(session_id, tool_name)
            .await
            .ok_or_else(|| {
                anyhow::Error::new(StructuredToolError::new(
                    format!("tool '{}' not found in registry", tool_name),
                    ToolErrorMetadata::other(),
                ))
            })?;

        // Private fields (`_session_id` / `_step_id` / `_idempotency_key`) are never trusted from
        // the LLM or scheduled tool_args: always strip first, validate the
        // LLM-facing input, then re-inject only caller-supplied values.
        // Declared via `Tool::requires_session_id` / `supports_live_output`.
        let mut exec_input = input;
        tool_contract::strip_private_tool_fields(&mut exec_input);
        if let Err(error) = tool.validate_input(&exec_input) {
            return Ok(ToolResult::failed_with_class(
                Value::Null,
                error.to_string(),
                ToolErrorClass::Validation,
            ));
        }
        if let Some(obj) = exec_input.as_object_mut() {
            let want_session =
                tool.requires_session_id() || (tool.supports_live_output() && step_id.is_some());
            if want_session && let Some(tid) = session_id {
                obj.insert("_session_id".into(), serde_json::json!(tid));
            }
            if tool.supports_live_output()
                && let Some(sid) = step_id.filter(|s| !s.is_empty())
            {
                obj.insert("_step_id".into(), serde_json::json!(sid));
            }
        }
        let settings = self.core.tool_settings.read().await;
        let configured = settings
            .get(tool_name)
            .or_else(|| {
                tool_name
                    .split('.')
                    .next()
                    .and_then(|root| settings.get(root))
            })
            .cloned();
        let cfg = configured.clone().unwrap_or_default();
        // A settings entry refines only fields explicitly configured. In
        // particular, `None` must preserve operation-specific intrinsic
        // timeouts instead of silently replacing them with 30 seconds.
        let timeout_secs = cfg
            .timeout_secs
            .unwrap_or_else(|| tool.timeout_secs_for(&exec_input));
        let max_retries = configured
            .as_ref()
            .and_then(|c| c.max_retries)
            .unwrap_or_else(|| tool.default_max_retries());
        let backoff_secs = configured
            .as_ref()
            .and_then(|c| c.retry_backoff_secs)
            .unwrap_or_else(|| tool.default_retry_backoff_secs());
        let idempotency = tool.idempotency(&exec_input);
        drop(settings);

        // Keep tool-local retries bounded even when a persisted settings file
        // contains an accidentally large value. Agent-level retries have a
        // separate budget in haven-agent.
        let max_attempts = 1 + max_retries.min(8);
        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = tool_retry_delay(tool_name, backoff_secs, attempt);
                tracing::debug!(
                    tool = %tool_name,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    "waiting before idempotent tool retry"
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = cancel.cancelled() => {
                        return Ok(ToolResult::cancelled("tool execution cancelled during retry backoff"));
                    },
                }
            }
            if cancel.is_cancelled() {
                return Ok(ToolResult::cancelled("tool execution cancelled"));
            }

            let mut result = match tool
                .execute_with_timeout(exec_input.clone(), cancel.clone(), timeout_secs)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    let message = e.to_string();
                    // Cancellation is a control-plane fact owned by the
                    // token, not a substring in an arbitrary tool error. A
                    // normal tool failure such as "cancelled request was
                    // rejected" must remain Failed so retry/telemetry do not
                    // treat it as an externally cancelled run.
                    if cancel.is_cancelled() {
                        ToolResult::cancelled(message)
                    } else {
                        let metadata = tool.error_metadata(&e);
                        ToolResult::failed_with_metadata(Value::Null, message.clone(), metadata)
                    }
                }
            };
            result.attempts = attempt + 1;
            if result.success {
                self.core.tool_circuits.record_success(tool_name);
                // Attach the tool's declared side-channel signals (ask
                // question / notify toast) BEFORE returning.
                result.signals = tool.signals(&result.output);
                return Ok(result);
            }

            let can_retry = matches!(idempotency, OperationIdempotency::Idempotent)
                && attempt + 1 < max_attempts
                && retryable_result(&result);
            if can_retry {
                tracing::warn!(
                    tool = %tool_name,
                    attempt = result.attempts,
                    max_attempts,
                    outcome = ?result.outcome,
                    "idempotent tool attempt failed; retrying"
                );
                continue;
            }
            self.core.tool_circuits.record_failure(tool_name);
            annotate_retry_safety(&mut result, idempotency);
            return Ok(result);
        }
        self.core.tool_circuits.record_failure(tool_name);
        Ok(ToolResult::failed(
            Value::Null,
            format!("tool '{}' retries exhausted", tool_name),
        ))
    }

    pub fn tool_circuits(&self) -> &ToolCircuitRegistry {
        &self.core.tool_circuits
    }

    pub async fn get_tool(&self, name: &str) -> Option<ToolBox> {
        if let Some(tool) = self.core.registry.get(name).await {
            return Some(tool);
        }
        self.core.deferred_catalog.get(name).await
    }

    pub async fn get_risk_level(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> RiskLevel {
        let reported = self
            .get_tool_for_session(session_id, tool_name)
            .await
            .map(|t| t.operation_policy(input).risk_level)
            .unwrap_or(RiskLevel::Safe);
        self.core
            .authorization
            .effective_risk(tool_name, reported)
            .await
    }

    /// Return the canonical operation policy used by runtime and catalog
    /// consumers. The authorization override is intentionally applied by the
    /// security gateway, so this method exposes intrinsic policy only.
    pub async fn get_operation_policy(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> OperationPolicy {
        self.get_tool_for_session(session_id, tool_name)
            .await
            .map(|tool| tool.operation_policy(input))
            .unwrap_or_else(|| OperationPolicy {
                risk_level: RiskLevel::Safe,
                capability: haven_common::types::permission_key(tool_name, input).into(),
                confirmation: crate::ConfirmationRequirement::None,
                idempotency: OperationIdempotency::Unknown,
                scope: ToolOperationScope::Session,
                concurrency: ToolConcurrency::Exclusive,
                effect: OperationEffect::ExternalEffect,
                data_sensitivity: DataSensitivity::None,
                network_access: NetworkAccess::None,
            })
    }

    /// Build the single authorization request used by agent, scheduled,
    /// native and renderer-triggered execution paths.
    pub async fn get_authorization_request(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        let policy = self
            .get_operation_policy(session_id, tool_name, input)
            .await;
        let authorization_input = self
            .get_authorization_input(session_id, tool_name, input)
            .await;
        AuthorizationRequest::new(session_id, tool_name, authorization_input, policy)
    }

    /// Build an authorization request from the immutable turn catalog. The
    /// authorization engine itself remains live and authoritative; only tool
    /// lookup and invocation policy derivation are served by the snapshot.
    pub fn get_authorization_request_from_snapshot(
        &self,
        catalog: &ToolCatalogSnapshot,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> AuthorizationRequest {
        let authorization_input = catalog
            .get(tool_name)
            .map(|tool| tool.authorization_input(input))
            .unwrap_or_else(|| input.clone());
        AuthorizationRequest::new(
            session_id,
            tool_name,
            authorization_input,
            catalog.operation_policy(tool_name, input),
        )
    }

    /// Return the canonical policy input used by a tool, including fixed
    /// operation-view discriminators. This keeps authorization, validation,
    /// execution and history attached to one operation identity.
    pub async fn get_authorization_input(
        &self,
        session_id: Option<&str>,
        tool_name: &str,
        input: &Value,
    ) -> Value {
        self.get_tool_for_session(session_id, tool_name)
            .await
            .map(|tool| tool.authorization_input(input))
            .unwrap_or_else(|| input.clone())
    }

    /// Apply the configured per-tool/global observation cap to the stable
    /// ToolResult summary. Agent, step persistence and resume all consume this
    /// exact helper so an adapter cannot create a longer recovery observation.
    pub async fn observation_text(&self, tool_name: &str, result: &ToolResult) -> String {
        let limits = self.core.context_limits.read().await;
        let settings = self.core.tool_settings.read().await;
        let cap = settings
            .get(tool_name)
            .or_else(|| {
                tool_name
                    .split('.')
                    .next()
                    .and_then(|root| settings.get(root))
            })
            .and_then(|config| config.max_output_chars)
            .unwrap_or(limits.max_observation_chars);
        OutputBudget::new(cap).observe(result)
    }
}

fn mcp_index_entry_has_search_tool(entry: &Value) -> bool {
    let Some(description) = entry["description"].as_str() else {
        return false;
    };
    description
        .split_once("; tools:")
        .is_some_and(|(_, tools)| {
            tools
                .split(',')
                .any(|tool| tool.trim().to_ascii_lowercase().contains("search"))
        })
}

/// Put the concrete retry policy beside a terminal failure so the model can
/// choose an equivalent read-only path without escalating to a user-facing
/// confirmation. This is result metadata, not authorization: unsafe and
/// unknown operations remain non-replayable by the executor.
fn annotate_retry_safety(result: &mut ToolResult, idempotency: OperationIdempotency) {
    if result.success {
        return;
    }
    let retry_safety = Value::String(idempotency.as_str().into());
    match &mut result.output {
        Value::Object(object) => {
            object.insert("retry_safety".into(), retry_safety);
            if let Some(error_class) = result.error_class {
                object.insert(
                    "error_class".into(),
                    Value::String(error_class.as_str().into()),
                );
            }
            object.insert(
                "retryability".into(),
                Value::String(result.retryability.as_str().into()),
            );
        }
        output => {
            let previous = std::mem::replace(output, Value::Null);
            *output = serde_json::json!({
                "retry_safety": retry_safety,
                "retryability": result.retryability.as_str(),
                "output": previous,
            });
        }
    }
}

const MAX_TOOL_RETRY_DELAY: Duration = Duration::from_secs(30);

/// Exponential delay with a small deterministic jitter and a hard ceiling.
/// Deterministic jitter avoids synchronizing repeated calls from the same
/// process without introducing a new random source into the tool crate.
fn tool_retry_delay(tool_name: &str, base_secs: u64, attempt: u32) -> Duration {
    if base_secs == 0 {
        return Duration::ZERO;
    }
    let exponential_secs = base_secs.saturating_mul(2u64.saturating_pow(attempt - 1));
    let base = Duration::from_secs(exponential_secs).min(MAX_TOOL_RETRY_DELAY);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    tool_name.hash(&mut hasher);
    attempt.hash(&mut hasher);
    let jitter_ms = hasher.finish() % 250;
    (base + Duration::from_millis(jitter_ms)).min(MAX_TOOL_RETRY_DELAY)
}

/// Retry only failures that are known to be transient. Unknown/cancelled
/// outcomes are never replayed, because the operation may still be running.
fn retryable_result(result: &ToolResult) -> bool {
    if matches!(
        result.outcome,
        ToolExecutionOutcome::Cancelled | ToolExecutionOutcome::TimedOutUnknown
    ) {
        return false;
    }
    result.retryability == crate::ToolRetryability::Retryable
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn llm_tool_name_preserves_short_names_and_hashes_truncated_names() {
        assert_eq!(llm_tool_name("mcp::calendar::list"), "mcp__calendar__list");

        let first = llm_tool_name(&format!("mcp::{}::read", "a".repeat(100)));
        let second = llm_tool_name(&format!("mcp::{}::read", "a".repeat(99) + "b"));
        assert_eq!(first.len(), 64);
        assert_eq!(second.len(), 64);
        assert!(first.starts_with("mcp__"));
        assert_ne!(first, second);
    }

    fn budget_test_def(name: &str, source: ToolSource) -> ToolDef {
        ToolDef::new(
            name,
            format!("test tool {name}"),
            json!({"type": "object"}),
            RiskLevel::Safe,
        )
        .with_manifest(ToolManifest {
            identity: ToolIdentity {
                source,
                catalog_group: haven_common::tools::ToolCatalogGroup::Other,
                root: name.split('.').next().unwrap_or(name).into(),
                operation: None,
                stable_name: name.into(),
            },
            model: ToolModel {
                name: name.into(),
                description: format!("test tool {name}"),
                input_schema: json!({"type": "object"}),
            },
            policy: ToolPolicy {
                risk_level: RiskLevel::Safe,
                permission_key: name.into(),
                confirmation: "none".into(),
                idempotency: "safe".into(),
                scope: "session".into(),
                concurrency: "exclusive".into(),
                effect: "read_only".into(),
                data_sensitivity: "none".into(),
                network_access: "none".into(),
            },
            presentation: ToolPresentation {
                label: name.into(),
                renderer: "tools".into(),
                icon: "tools".into(),
                represented_source: ToolSource::Builtin,
            },
            root_presentation: haven_common::tools::ToolRootPresentation {
                label: name.split('.').next().unwrap_or(name).into(),
                description: format!("{} test capabilities", name),
                icon: "tools".into(),
            },
            prompt: haven_common::tools::ToolPrompt {
                when_to_use: "test".into(),
                when_not_to_use: "never".into(),
                key_operations: vec![name.into()],
            },
            availability: ToolAvailability::default(),
        })
    }

    #[test]
    fn tool_budget_selection_is_source_aware_and_deterministic() {
        let selection = select_tool_defs_for_budget(
            vec![
                budget_test_def("skill__z", ToolSource::Skill),
                budget_test_def("core.z", ToolSource::Builtin),
                budget_test_def("skill__a", ToolSource::Skill),
                budget_test_def("core.a", ToolSource::Builtin),
            ],
            vec![
                budget_test_def("mcp__z", ToolSource::Mcp),
                budget_test_def("mcp__a", ToolSource::Mcp),
            ],
            4,
        );

        let selected: Vec<_> = selection
            .selected
            .iter()
            .map(|def| def.name.as_str())
            .collect();
        assert_eq!(selected, ["core.a", "core.z", "mcp__a", "mcp__z"]);
        assert_eq!(selection.omitted, ["skill__a", "skill__z"]);
        assert_eq!(selection.omitted_core, 0);
    }

    #[test]
    fn tool_budget_selection_reports_core_omissions_when_core_exceeds_limit() {
        let selection = select_tool_defs_for_budget(
            vec![
                budget_test_def("core.c", ToolSource::Builtin),
                budget_test_def("skill__a", ToolSource::Skill),
                budget_test_def("core.a", ToolSource::Builtin),
                budget_test_def("core.b", ToolSource::Builtin),
            ],
            Vec::new(),
            2,
        );

        let selected: Vec<_> = selection
            .selected
            .iter()
            .map(|def| def.name.as_str())
            .collect();
        assert_eq!(selected, ["core.a", "core.b"]);
        assert_eq!(selection.omitted, ["core.c", "skill__a"]);
        assert_eq!(selection.omitted_core, 1);
    }

    #[tokio::test]
    async fn test_tools_manager_new() {
        let mgr = ToolsManager::new();
        let tools = mgr.registry().list().await;
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn scoped_catalog_rebuild_reuses_unaffected_runtime_instances() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let ask_before = mgr.registry().get("ask").await.unwrap();
        let shell_before = mgr.get_tool("shell").await.unwrap();

        mgr.set_default_shell(ShellChoice::default()).await;

        let ask_after = mgr.registry().get("ask").await.unwrap();
        let shell_after = mgr.get_tool("shell").await.unwrap();
        assert!(Arc::ptr_eq(&ask_before, &ask_after));
        assert!(!Arc::ptr_eq(&shell_before, &shell_after));
    }

    #[tokio::test]
    async fn manager_control_plane_errors_preserve_structured_metadata() {
        let mgr = ToolsManager::new();
        let missing = mgr
            .execute_tool(None, "missing", json!({}), CancellationToken::new())
            .await
            .expect_err("missing tool must fail");
        let missing_metadata = missing
            .downcast_ref::<StructuredToolError>()
            .expect("missing tool error must carry metadata")
            .metadata();
        assert_eq!(missing_metadata, ToolErrorMetadata::other());

        mgr.set_tool_settings(HashMap::from([(
            "ask".into(),
            ToolConfig {
                enabled: false,
                ..Default::default()
            },
        )]))
        .await;
        let disabled = mgr
            .execute_tool(None, "ask", json!({}), CancellationToken::new())
            .await
            .expect_err("disabled tool must fail");
        let disabled_metadata = disabled
            .downcast_ref::<StructuredToolError>()
            .expect("disabled tool error must carry metadata")
            .metadata();
        assert_eq!(disabled_metadata.class, ToolErrorClass::Permission);
        assert_eq!(disabled_metadata.outcome, ToolExecutionOutcome::Failed);
        assert_eq!(
            disabled_metadata.retryability,
            ToolRetryability::NotRetryable
        );
    }

    #[tokio::test]
    async fn runtime_capabilities_report_unavailable_backends_explicitly() {
        let mgr = ToolsManager::new();
        let capabilities = mgr.runtime_capabilities().await;
        assert!(!capabilities.vision);
        assert!(!capabilities.image_generation);
        assert!(!capabilities.transcription);
        assert!(!capabilities.recording);
        assert!(!capabilities.tts);
        assert_eq!(
            capabilities.web_search,
            "unavailable (no provider builtin search; no MCP search server)"
        );
    }

    #[tokio::test]
    async fn tool_catalog_snapshot_captures_lookup_policy_and_manifest() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let snapshot = mgr.tool_catalog_snapshot("ses-snapshot").await;
        let tool = snapshot
            .get("ask")
            .expect("core tool must be present in the session snapshot");
        assert_eq!(tool.name(), "ask");
        assert!(!snapshot.is_empty());

        let input = json!({"question": "continue?"});
        let policy = snapshot.operation_policy("ask", &input);
        assert_eq!(policy.scope, ToolOperationScope::Session);
        assert_eq!(
            snapshot
                .manifest("ask")
                .expect("snapshot must retain renderer metadata")
                .identity
                .stable_name,
            "ask"
        );
    }

    #[tokio::test]
    async fn tool_catalog_snapshot_keeps_provider_surface_stable_after_drift() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let before = mgr.tool_catalog_snapshot("ses-drift").await;
        assert!(
            before
                .provider_definitions()
                .iter()
                .all(|definition| definition.name != "drift_only")
        );

        struct DriftTool;
        #[async_trait::async_trait]
        impl Tool for DriftTool {
            fn name(&self) -> String {
                "drift_only".into()
            }
            fn description(&self) -> String {
                "catalog drift probe".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                Ok(ToolResult::ok(json!({})))
            }
        }

        mgr.register_for_session("ses-drift", Arc::new(DriftTool))
            .await;
        let after = mgr.tool_catalog_snapshot("ses-drift").await;
        assert!(
            after
                .provider_definitions()
                .iter()
                .any(|definition| definition.name == "drift_only")
        );
        assert!(
            before
                .provider_definitions()
                .iter()
                .all(|definition| definition.name != "drift_only"),
            "the prepared Turn snapshot must not change when the registry mutates"
        );
        assert_ne!(before.version(), after.version());
    }

    #[tokio::test]
    async fn catalog_drift_reaches_the_live_execution_boundary() {
        struct DriftTool(&'static str);

        #[async_trait::async_trait]
        impl Tool for DriftTool {
            fn name(&self) -> String {
                "drift_execution".into()
            }
            fn description(&self) -> String {
                format!("catalog version {}", self.0)
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                Ok(ToolResult::ok(json!({"implementation": self.0})))
            }
        }

        let mgr = ToolsManager::new();
        mgr.registry()
            .register(Arc::new(DriftTool("prepared")))
            .await
            .unwrap();
        let catalog = mgr.tool_catalog_snapshot("ses-drift-execution").await;
        assert_eq!(
            catalog.get("drift_execution").unwrap().description(),
            "catalog version prepared"
        );

        // The prepared provider surface remains immutable, but execution must
        // consult the current session overlay at the safety boundary.
        mgr.register_for_session("ses-drift-execution", Arc::new(DriftTool("live")))
            .await;
        let result = mgr
            .execute_tool(
                Some("ses-drift-execution"),
                "drift_execution",
                json!({}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["implementation"], "live");
        assert_eq!(
            catalog.get("drift_execution").unwrap().description(),
            "catalog version prepared",
            "catalog drift must not mutate the already prepared turn view"
        );
    }

    #[tokio::test]
    async fn execute_tool_does_not_inject_idempotency_key_into_strict_tool_args() {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StrictArgs {}

        struct StrictTool;

        #[async_trait::async_trait]
        impl Tool for StrictTool {
            fn name(&self) -> String {
                "strict_args".into()
            }
            fn description(&self) -> String {
                "test tool with a strict serde contract".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({
                    "type": "object",
                    "additionalProperties": false
                })
            }
            async fn execute(
                &self,
                input: Value,
                _: CancellationToken,
            ) -> anyhow::Result<ToolResult> {
                let _: StrictArgs = serde_json::from_value(input)?;
                Ok(ToolResult::ok(json!({ "ok": true })))
            }
        }

        let mgr = ToolsManager::new();
        mgr.registry().register(Arc::new(StrictTool)).await.unwrap();
        let result = mgr
            .execute_tool_with_step(
                None,
                "strict_args",
                json!({}),
                CancellationToken::new(),
                Some("step-0123456789abcdef0123456789abcdef"),
            )
            .await
            .expect("strict tool must execute");
        assert!(result.success, "strict tool failed: {:?}", result.error);
    }

    #[tokio::test]
    async fn test_tools_manager_set_tool_settings() {
        let mgr = ToolsManager::new();
        let mut settings = HashMap::new();
        settings.insert("test_tool".into(), ToolConfig::default());
        mgr.set_tool_settings(settings).await;
    }

    #[tokio::test]
    async fn test_tools_manager_set_context_limits_stores_global_cap() {
        let mgr = ToolsManager::new();
        assert_eq!(
            mgr.core.context_limits.read().await.max_observation_chars,
            16_000
        );
        let limits = ContextLimitsConfig {
            max_observation_chars: 5_000,
            ..Default::default()
        };
        mgr.set_context_limits(limits).await;
        assert_eq!(
            mgr.core.context_limits.read().await.max_observation_chars,
            5_000
        );
    }

    #[tokio::test]
    async fn observation_text_uses_same_global_cap_for_adapters() {
        let mgr = ToolsManager::new();
        let mut limits = ContextLimitsConfig::default();
        limits.max_observation_chars = 4;
        mgr.set_context_limits(limits).await;
        let result = ToolResult::ok(json!("123456"));
        assert_eq!(mgr.observation_text("adapter", &result).await, "1234");
    }

    #[tokio::test]
    async fn test_tools_manager_get_tool_not_found() {
        let mgr = ToolsManager::new();
        let tool = mgr.get_tool("nonexistent").await;
        assert!(tool.is_none());
    }

    /// Tools that emit a side-channel signal (`ask` / `notify`) must populate
    /// `ToolResult::signals` through their `signals()` hook — the ReAct loop
    /// reads structured signals instead of name-matching the output. This
    /// exercises the full wiring (`execute_tool` → `tool.signals`), so a tool
    /// that stops declaring its signal fails here instead of silently losing
    /// the ask/notify behavior.
    #[tokio::test]
    async fn test_signal_declaring_tools_populate_result_signals() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let ask = mgr
            .execute_tool(
                None,
                "ask",
                json!({"question": "Which file?"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(ask.signals.ask_question.as_deref(), Some("Which file?"));

        let notify = mgr
            .execute_tool(
                None,
                "notify",
                json!({"title": "Build", "body": "Compilation finished"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(notify.signals.notify_title.as_deref(), Some("Build"));
        assert_eq!(
            notify.signals.notify_body.as_deref(),
            Some("Compilation finished")
        );
    }

    #[tokio::test]
    async fn test_tools_manager_execute_tool_not_found() {
        let mgr = ToolsManager::new();
        let result = mgr
            .execute_tool(None, "nonexistent", json!({}), CancellationToken::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tools_manager_rebuild_catalog_registers_builtins() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let builtin_tools = mgr.list_builtin_tools().await;
        let names: Vec<_> = builtin_tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect();
        let unique_names: std::collections::HashSet<_> = names.iter().copied().collect();
        assert_eq!(
            names.len(),
            unique_names.len(),
            "builtin tool names must be unique"
        );

        for (name, group) in [
            ("ask", "haven"),
            ("notify", "system"),
            ("shell", "system"),
            ("http", "system"),
            ("files.read", "system"),
            ("agent.list", "agent"),
        ] {
            let listed = builtin_tools
                .iter()
                .find(|tool| tool["name"].as_str() == Some(name))
                .unwrap_or_else(|| panic!("builtin tool {name} should be listed"));
            assert_eq!(listed["catalog_group"].as_str(), Some(group), "{name}");
            assert_eq!(
                listed["manifest"]["identity"]["stable_name"].as_str(),
                Some(name),
                "manifest identity drift for {name}"
            );
            assert_eq!(
                listed["manifest"]["identity"]["root"].as_str(),
                Some(name.split('.').next().unwrap_or(name)),
                "manifest root drift for {name}"
            );
            assert!(listed["manifest"]["presentation"]["renderer"].is_string());
        }

        assert!(mgr.get_tool("files").await.is_none());
        assert!(mgr.get_tool("media").await.is_none());
        assert!(mgr.get_tool("audio").await.is_none());
        for name in [
            "files.read",
            "files.outline",
            "files.summary",
            "files.search",
            "system.info",
            "files.write",
            "files.list",
            "process.list",
            "clipboard.read",
            "input.click",
            "window.list",
            "media.inspect",
            "actions.list",
            "schedule.list",
            "preferences.get",
            "checklist.list",
            "agent.list",
        ] {
            let view = mgr.get_tool(name).await;
            assert!(view.is_some(), "operation view {name} should be registered");
            assert!(
                view.unwrap().input_schema()["properties"]
                    .get("operation")
                    .is_none(),
                "operation discriminator stays fixed in {name}"
            );
        }
        let read_view = mgr.get_tool("files.read").await.unwrap();
        assert!(
            read_view
                .validate_input(&json!({"path": "notes.md"}))
                .is_ok()
        );
        assert!(
            read_view
                .validate_input(&json!({"path": "notes.md", "operation": "write"}))
                .is_err()
        );
        assert_eq!(
            read_view.authorization_input(&json!({"operation": "delete", "path": "notes.md"}))["operation"],
            "read"
        );
        let search_view = mgr.get_tool("files.search").await.unwrap();
        assert_eq!(
            search_view.risk_level(&json!({"mode": "filename"})),
            haven_common::types::RiskLevel::Low
        );
        assert_eq!(
            search_view.risk_level(&json!({"mode": "content"})),
            haven_common::types::RiskLevel::Medium
        );

        assert!(mgr.get_tool("system").await.is_none());
        assert!(mgr.get_tool("process").await.is_none());
        assert!(mgr.get_tool("clipboard").await.is_none());
        assert!(mgr.get_tool("system.env.get").await.is_some());
        assert!(mgr.get_tool("system.power.hibernate").await.is_some());
        assert_eq!(
            mgr.get_tool("process.kill")
                .await
                .expect("process.kill view")
                .risk_level(&json!({})),
            haven_common::types::RiskLevel::High
        );
        assert!(mgr.get_tool("haven").await.is_none());
        assert!(mgr.get_tool("load_skill").await.is_none());
    }

    #[tokio::test]
    async fn test_tool_catalog_exposes_three_layers_without_loading_deferred_tools() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let session_id = "ses-0123456789abcdef0123456789abcdef";

        let families = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({"action": "list"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let family_items = families.output["items"].as_array().unwrap();
        for family in ["agent", "haven", "system"] {
            assert!(
                family_items.iter().any(|item| item["name"] == family),
                "layer 1 should expose the {family} family"
            );
        }
        assert!(
            family_items
                .iter()
                .all(|item| item.get("input_schema").is_none())
        );

        let roots = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({"action": "list", "level": "tools"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let window = roots.output["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["name"] == "window")
            .expect("layer 2 should expose the window root");
        assert!(window["operation_count"].as_u64().unwrap() >= 10);
        assert!(window.get("input_schema").is_none());
        assert!(
            mgr.list_defs_for_session(session_id)
                .await
                .iter()
                .all(|def| def.name != "window.screenshot")
        );

        let window_detail = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({"action": "describe", "name": "window"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let operations = window_detail.output["operations"].as_array().unwrap();
        assert!(
            operations
                .iter()
                .any(|item| item["name"] == "window.screenshot")
        );
        assert!(
            operations
                .iter()
                .all(|item| item.get("input_schema").is_none())
        );

        let operation_page = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({
                    "action": "list",
                    "level": "operations",
                    "root": "window",
                    "limit": 1
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(operation_page.output["items"].as_array().unwrap().len(), 1);
        assert_eq!(operation_page.output["next_cursor"], 1);
        let revision = operation_page.output["catalog_revision"]
            .as_str()
            .expect("paged catalog responses carry a revision")
            .to_string();
        let next_page = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({
                    "action": "list",
                    "level": "operations",
                    "root": "window",
                    "cursor": 1,
                    "revision": revision,
                    "limit": 1
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(next_page.output["status"], "ok");

        // A config mutation invalidates an outstanding cursor instead of
        // returning a page from a different catalog snapshot.
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "catalog-revision-test".into(),
            enabled: true,
            ..Default::default()
        })
        .await;
        let stale_page = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({
                    "action": "list",
                    "level": "operations",
                    "root": "window",
                    "cursor": 1,
                    "revision": operation_page.output["catalog_revision"],
                    "limit": 1
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(stale_page.output["status"], "stale_cursor");

        let screenshot_detail = mgr
            .execute_tool(
                Some(session_id),
                "tool_catalog",
                json!({"action": "describe", "name": "window.screenshot"}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(screenshot_detail.output["loaded"], false);
        assert!(screenshot_detail.output["input_schema"].is_object());
        assert_eq!(screenshot_detail.output["load"]["tool"], "load_builtin");

        let loaded = mgr
            .execute_tool(
                Some(session_id),
                "load_builtin",
                json!({"operations": ["window.screenshot"]}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(loaded.success);
        assert!(
            mgr.get_tool_for_session(Some(session_id), "window.screenshot")
                .await
                .is_some()
        );
    }

    #[tokio::test]
    async fn tool_catalog_describe_does_not_connect_or_execute_mcp() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "unconnected-catalog-server".into(),
            command: "this-command-must-not-run".into(),
            enabled: true,
            ..Default::default()
        })
        .await;

        let result = mgr
            .execute_tool(
                Some("ses-0123456789abcdef0123456789abcdef"),
                "tool_catalog",
                json!({
                    "action": "describe",
                    "name": "unconnected-catalog-server"
                }),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.output["status"], "ok");
        assert_eq!(result.output["source"], "mcp");
        assert!(mgr.mcp_manager().list_clients().await.is_empty());
    }

    #[tokio::test]
    async fn test_tools_manager_disabled_tool_excluded_and_blocked() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        // Disable the `files` tool.
        let mut settings = HashMap::new();
        settings.insert(
            "files".into(),
            ToolConfig {
                enabled: false,
                ..Default::default()
            },
        );
        mgr.set_tool_settings(settings).await;

        // Excluded from the agent-facing registry...
        assert!(mgr.get_tool("files").await.is_none());
        let schemas = mgr.registry().list_schemas().await;
        assert!(!schemas.iter().any(|s| s["name"].as_str() == Some("files")));
        assert!(mgr.get_tool("files.read").await.is_none());
        assert!(mgr.get_tool("files.search").await.is_none());

        // ...still listed for the UI with enabled = false...
        let all = mgr.list_builtin_tools().await;
        let file = all
            .iter()
            .find(|s| s["name"].as_str() == Some("files.read"))
            .unwrap();
        assert_eq!(file["enabled"].as_bool(), Some(false));

        // ...and execution is blocked.
        let result = mgr
            .execute_tool(
                None,
                "files.read",
                json!({"path": "notes.md"}),
                CancellationToken::new(),
            )
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn test_tools_manager_execute_builtin_tool() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("hello.txt");
        tokio::fs::write(&file, "hello from manager").await.unwrap();

        let result = mgr
            .execute_tool(
                None,
                "files.read",
                json!({"path": file.to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(
            result.output["content"].as_str().unwrap(),
            "hello from manager"
        );
    }

    #[tokio::test]
    async fn operation_view_accepts_trusted_private_session_metadata() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        let tmp = TempDir::new().unwrap();
        let file = tmp.path().join("listed.txt");
        tokio::fs::write(&file, "listed by manager").await.unwrap();
        assert!(
            mgr.load_builtin_for_session(
                "ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                Some(vec!["files.list".into()]),
                None,
            )
            .await
        );

        let result = mgr
            .execute_tool(
                Some("ses-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
                "files.list",
                json!({"path": tmp.path().to_string_lossy()}),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(result.success, "files.list failed: {:?}", result.error);
        assert_eq!(result.output["count"], 1);
    }

    #[tokio::test]
    async fn test_tools_manager_get_risk_level_unknown() {
        let mgr = ToolsManager::new();
        let risk = mgr.get_risk_level(None, "nonexistent", &json!({})).await;
        assert_eq!(risk, RiskLevel::Safe);
    }

    /// End-to-end: execute_tool fast-fails once the per-tool circuit opens
    /// (refine §5).
    #[tokio::test]
    async fn test_execute_tool_circuit_breaker_opens() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct FailingTool {
            name: String,
            call_count: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl Tool for FailingTool {
            fn name(&self) -> String {
                self.name.clone()
            }
            fn description(&self) -> String {
                "always fails".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(
                &self,
                _: Value,
                _: tokio_util::sync::CancellationToken,
            ) -> anyhow::Result<ToolResult> {
                self.call_count.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("deliberate failure")
            }
        }

        let mgr = ToolsManager::new();
        let call_count = Arc::new(AtomicU32::new(0));
        mgr.registry()
            .register(Arc::new(FailingTool {
                name: "failing".into(),
                call_count: call_count.clone(),
            }))
            .await
            .unwrap();

        for i in 0..5 {
            let r = mgr
                .execute_tool(None, "failing", json!({}), CancellationToken::new())
                .await;
            assert!(!r.unwrap().success, "call {} should fail", i + 1);
        }
        assert!(mgr.tool_circuits().is_open("failing"));

        let before = call_count.load(Ordering::SeqCst);
        let r = mgr
            .execute_tool(None, "failing", json!({}), CancellationToken::new())
            .await;
        assert!(r.is_err());
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            before,
            "tool should not be called when breaker is open"
        );
        assert!(
            r.unwrap_err().to_string().contains("circuit breaker"),
            "error should mention circuit breaker"
        );
    }

    #[tokio::test]
    async fn execute_tool_retries_transient_failure_by_default() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct FlakyTool {
            attempts: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl Tool for FlakyTool {
            fn name(&self) -> String {
                "flaky".into()
            }
            fn description(&self) -> String {
                "fails once with a transient error".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn idempotency(&self, _: &Value) -> OperationIdempotency {
                OperationIdempotency::Idempotent
            }
            fn error_metadata(&self, _: &anyhow::Error) -> ToolErrorMetadata {
                ToolErrorMetadata::transient()
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(
                &self,
                _: Value,
                _: tokio_util::sync::CancellationToken,
            ) -> anyhow::Result<ToolResult> {
                if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("service unavailable")
                }
                Ok(ToolResult::ok(json!({"recovered": true})))
            }
        }

        let mgr = ToolsManager::new();
        let attempts = Arc::new(AtomicU32::new(0));
        mgr.set_tool_settings(HashMap::from([(
            "flaky".into(),
            ToolConfig {
                max_retries: Some(1),
                retry_backoff_secs: Some(0),
                ..Default::default()
            },
        )]))
        .await;
        mgr.registry()
            .register(Arc::new(FlakyTool {
                attempts: attempts.clone(),
            }))
            .await
            .unwrap();

        let result = mgr
            .execute_tool(None, "flaky", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn execute_tool_never_retries_when_side_effect_outcome_is_unknown() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct UnknownSideEffectTool {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl Tool for UnknownSideEffectTool {
            fn name(&self) -> String {
                "unknown_side_effect".into()
            }
            fn description(&self) -> String {
                "test tool with an unknown side-effect outcome".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::High
            }
            fn idempotency(&self, _: &Value) -> OperationIdempotency {
                OperationIdempotency::Idempotent
            }
            fn default_max_retries(&self) -> u32 {
                3
            }
            fn default_retry_backoff_secs(&self) -> u64 {
                0
            }
            fn error_metadata(&self, _: &anyhow::Error) -> ToolErrorMetadata {
                ToolErrorMetadata {
                    class: ToolErrorClass::SideEffectMayHaveHappened,
                    outcome: ToolExecutionOutcome::Failed,
                    retryability: ToolRetryability::Unknown,
                }
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object", "additionalProperties": false})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(anyhow::Error::new(StructuredToolError::new(
                    "the external side effect may have happened",
                    ToolErrorMetadata {
                        class: ToolErrorClass::SideEffectMayHaveHappened,
                        outcome: ToolExecutionOutcome::Failed,
                        retryability: ToolRetryability::Unknown,
                    },
                )))
            }
        }

        let calls = Arc::new(AtomicU32::new(0));
        let manager = ToolsManager::new();
        manager
            .set_tool_settings(HashMap::from([(
                "unknown_side_effect".into(),
                ToolConfig {
                    max_retries: Some(3),
                    retry_backoff_secs: Some(0),
                    ..Default::default()
                },
            )]))
            .await;
        manager
            .registry()
            .register(Arc::new(UnknownSideEffectTool {
                calls: calls.clone(),
            }))
            .await
            .unwrap();

        let result = manager
            .execute_tool(
                None,
                "unknown_side_effect",
                json!({}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(result.attempts, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            result.error_class,
            Some(ToolErrorClass::SideEffectMayHaveHappened)
        );
        assert_eq!(result.retryability, ToolRetryability::Unknown);
    }

    #[tokio::test]
    async fn settings_without_timeout_preserve_intrinsic_timeout() {
        struct SlowIntrinsicTool;

        #[async_trait::async_trait]
        impl Tool for SlowIntrinsicTool {
            fn name(&self) -> String {
                "slow_intrinsic".into()
            }
            fn description(&self) -> String {
                "test tool".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::High
            }
            fn default_timeout_secs(&self) -> u64 {
                1
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                tokio::time::sleep(Duration::from_secs(2)).await;
                Ok(ToolResult::ok(json!({"done": true})))
            }
        }

        let mgr = ToolsManager::new();
        mgr.set_tool_settings(HashMap::from([(
            "slow_intrinsic".into(),
            ToolConfig {
                max_output_chars: Some(100),
                ..Default::default()
            },
        )]))
        .await;
        mgr.registry()
            .register(Arc::new(SlowIntrinsicTool))
            .await
            .unwrap();

        let result = mgr
            .execute_tool(None, "slow_intrinsic", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.outcome, ToolExecutionOutcome::TimedOutUnknown);
        assert_eq!(result.attempts, 1);
    }

    #[tokio::test]
    async fn settings_without_retry_fields_preserve_intrinsic_retry_policy() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct IntrinsicRetryTool {
            attempts: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl Tool for IntrinsicRetryTool {
            fn name(&self) -> String {
                "intrinsic_retry".into()
            }
            fn description(&self) -> String {
                "test tool".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn idempotency(&self, _: &Value) -> OperationIdempotency {
                OperationIdempotency::Idempotent
            }
            fn default_max_retries(&self) -> u32 {
                1
            }
            fn default_retry_backoff_secs(&self) -> u64 {
                0
            }
            fn error_metadata(&self, _: &anyhow::Error) -> ToolErrorMetadata {
                ToolErrorMetadata::transient()
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    anyhow::bail!("service unavailable")
                }
                Ok(ToolResult::ok(json!({"recovered": true})))
            }
        }

        let mgr = ToolsManager::new();
        let attempts = Arc::new(AtomicU32::new(0));
        mgr.set_tool_settings(HashMap::from([(
            "intrinsic_retry".into(),
            ToolConfig {
                max_output_chars: Some(100),
                ..Default::default()
            },
        )]))
        .await;
        mgr.registry()
            .register(Arc::new(IntrinsicRetryTool {
                attempts: attempts.clone(),
            }))
            .await
            .unwrap();

        let result = mgr
            .execute_tool(None, "intrinsic_retry", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.attempts, 2);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retryable_tool_results_require_known_transient_failure() {
        assert!(retryable_result(&ToolResult::failed_with_class(
            Value::Null,
            "status 503",
            ToolErrorClass::Transient,
        )));
        assert!(!retryable_result(&ToolResult::failed(
            Value::Null,
            "connection refused",
        )));
        assert!(!retryable_result(&ToolResult::cancelled(
            "cancelled while waiting"
        )));
        assert!(!retryable_result(&ToolResult::timed_out(
            ToolExecutionOutcome::TimedOutUnknown,
            "timeout"
        )));
    }

    // ── Progressive loading: per-session schemas & MCP index ──────────────

    #[tokio::test]
    async fn test_list_schemas_for_session_includes_per_session_tools() {
        use haven_skills::SkillManifest;

        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        // Before registering a per-session tool, schemas come only from the
        // global registry.
        let base_schemas = mgr.list_schemas_for_session("ses-a").await;
        let base_count = base_schemas.len();

        // Register a fake per-session tool.
        let manifest = SkillManifest {
            name: "demo".into(),
            description: "demo skill".into(),
            version: None,
            language: haven_skills::Language::Python,
            instructions: "do stuff".into(),
        };
        let skill = Skill::from_manifest_unchecked(manifest, std::path::PathBuf::from("."), true);
        let runner = mgr.skill_runner().read().await.clone();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
        mgr.register_for_session("ses-a", Arc::new(adapter)).await;

        let schemas = mgr.list_schemas_for_session("ses-a").await;
        assert_eq!(
            schemas.len(),
            base_count + 1,
            "per-session skill tool should appear in schemas"
        );
        assert!(schemas.iter().any(|s| s["name"] == "skill__demo"));

        // Other sessions should NOT see this tool.
        let other = mgr.list_schemas_for_session("ses-b").await;
        assert_eq!(other.len(), base_count);
        assert!(!other.iter().any(|s| s["name"] == "skill__demo"));
    }

    #[tokio::test]
    async fn test_session_catalog_version_does_not_invalidate_other_sessions() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let before_a = mgr.catalog_version_for_session("ses-a").await;
        let before_b = mgr.catalog_version_for_session("ses-b").await;

        struct NamedStub(&'static str);
        #[async_trait::async_trait]
        impl Tool for NamedStub {
            fn name(&self) -> String {
                self.0.into()
            }
            fn description(&self) -> String {
                "stub".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                Ok(ToolResult::ok(json!({})))
            }
        }

        mgr.register_for_session("ses-a", Arc::new(NamedStub("session_only")))
            .await;
        let after_a = mgr.catalog_version_for_session("ses-a").await;
        let after_b = mgr.catalog_version_for_session("ses-b").await;
        assert_eq!(after_a.0, before_a.0);
        assert_eq!(after_a.1, before_a.1 + 1);
        assert_eq!(after_b, before_b);

        mgr.rebuild_catalog().await;
        let after_global_a = mgr.catalog_version_for_session("ses-a").await;
        let after_global_b = mgr.catalog_version_for_session("ses-b").await;
        assert!(after_global_a.0 > after_a.0);
        assert_eq!(after_global_a.1, after_a.1);
        assert_eq!(after_global_b.1, after_b.1);
    }

    #[tokio::test]
    async fn test_build_mcp_index_filters_disabled() {
        use haven_common::config::McpServerConfig;

        let mgr = ToolsManager::new();
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "on".into(),
            enabled: true,
            ..Default::default()
        })
        .await;
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "off".into(),
            enabled: false,
            ..Default::default()
        })
        .await;

        let index = mgr.build_mcp_index().await;
        let names: Vec<&str> = index.iter().filter_map(|e| e["name"].as_str()).collect();
        assert!(names.contains(&"on"));
        assert!(!names.contains(&"off"), "disabled server should not appear");
    }

    #[test]
    fn mcp_search_detection_only_uses_cached_tool_names() {
        assert!(mcp_index_entry_has_search_tool(&serde_json::json!({
            "name": "research",
            "description": "MCP server 'research'; tools: fetch, web_search",
        })));
        assert!(!mcp_index_entry_has_search_tool(&serde_json::json!({
            "name": "search-like-server",
            "description": "MCP server 'search-like-server'",
        })));
    }

    #[tokio::test]
    async fn test_build_mcp_index_does_not_expose_process_args() {
        use haven_common::config::McpServerConfig;

        let mgr = ToolsManager::new();
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "safe-server".into(),
            command: "server.exe".into(),
            args: vec!["--token".into(), "SECRET_SHOULD_NOT_REACH_PROMPT".into()],
            enabled: true,
            ..Default::default()
        })
        .await;

        let index = mgr.build_mcp_index().await;
        let description = index[0]["description"].as_str().unwrap_or("");
        assert!(!description.contains("server.exe"));
        assert!(!description.contains("SECRET_SHOULD_NOT_REACH_PROMPT"));
        assert!(description.contains("safe-server"));
    }

    #[tokio::test]
    async fn test_upsert_and_remove_mcp_server_config() {
        use haven_common::config::McpServerConfig;

        let mgr = ToolsManager::new();
        mgr.upsert_mcp_server_config(McpServerConfig {
            name: "srv".into(),
            enabled: true,
            ..Default::default()
        })
        .await;
        assert_eq!(mgr.build_mcp_index().await.len(), 1);

        mgr.remove_mcp_server_config("srv").await;
        assert!(mgr.build_mcp_index().await.is_empty());
    }

    #[tokio::test]
    async fn test_rebuild_catalog_does_not_register_mcp_tools() {
        // Progressive loading: MCP tools must NOT be in the global registry.
        // They should only appear per-session after `load_mcp`.
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let schemas = mgr.registry().list_schemas().await;
        assert!(
            !schemas
                .iter()
                .any(|s| { s["name"].as_str().unwrap_or("").starts_with("mcp__") }),
            "MCP tools must not be pre-registered globally"
        );
    }

    #[tokio::test]
    async fn test_builtin_loader_keeps_deferred_tools_out_of_provider_surface() {
        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;

        assert!(mgr.registry().get("shell").await.is_none());
        assert!(mgr.get_tool("shell").await.is_some());
        assert!(
            mgr.list_defs_for_session("ses-lazy-builtin")
                .await
                .iter()
                .all(|def| def.name != "shell")
        );

        let result = mgr
            .execute_tool(
                Some("ses-lazy-builtin"),
                "load_builtin",
                serde_json::json!({"operations": ["shell"]}),
                CancellationToken::new(),
            )
            .await
            .expect("load_builtin should be executable from the core surface");
        assert!(result.success, "loader failed: {:?}", result.error);
        assert_eq!(result.output["status"], "loaded");
        assert!(result.output.get("input_schema").is_none());

        let loaded = mgr.list_defs_for_session("ses-lazy-builtin").await;
        assert!(loaded.iter().any(|def| def.name == "shell"));
    }

    #[tokio::test]
    async fn test_list_defs_for_session_caps_at_max_tools() {
        use haven_common::config::ContextLimitsConfig;

        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let global = mgr.registry().list_defs().await.len();
        assert!(global > 0, "catalog should have builtins");

        // Leave room for only 2 session overlays. Write the limit directly so
        // we do not rebuild the catalog (and shift `global`) mid-test.
        let mut limits = ContextLimitsConfig::default();
        limits.max_tools_per_request = global + 2;
        *mgr.core.context_limits.write().await = limits;

        struct NamedStub(&'static str);
        #[async_trait::async_trait]
        impl Tool for NamedStub {
            fn name(&self) -> String {
                self.0.into()
            }
            fn description(&self) -> String {
                "stub".into()
            }
            fn risk_level(&self, _: &Value) -> RiskLevel {
                RiskLevel::Safe
            }
            fn input_schema(&self) -> Value {
                json!({"type": "object"})
            }
            async fn execute(&self, _: Value, _: CancellationToken) -> anyhow::Result<ToolResult> {
                Ok(ToolResult::ok(json!({})))
            }
        }
        for name in ["s_a", "s_b", "s_c", "s_d", "s_e"] {
            mgr.register_for_session("ses-cap", Arc::new(NamedStub(name)))
                .await;
        }

        let defs = mgr.list_defs_for_session("ses-cap").await;
        assert_eq!(defs.len(), global + 2, "must truncate session overlays");
        let session_kept: Vec<_> = defs
            .iter()
            .filter(|d| d.name.starts_with("s_"))
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(session_kept, vec!["s_a", "s_b"]);
    }

    /// LLM-/schedule-supplied `_step_id` / `_session_id` must never reach the
    /// live-output hub. Without a trusted step id the shell path stays silent;
    /// with one, only the trusted id is emitted.
    #[cfg(windows)]
    #[tokio::test]
    async fn private_live_output_ids_are_stripped_and_reinjected() {
        use std::sync::Mutex;

        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hits2 = hits.clone();
        mgr.live_outputs()
            .set_event_sink(Arc::new(move |_event, payload| {
                if let Some(sid) = payload["step_id"].as_str() {
                    hits2.lock().unwrap().push(sid.to_string());
                }
            }));

        // Forged private fields, no trusted step → no live emit.
        let _ = mgr
            .execute_tool(
                Some("ses-1"),
                "shell",
                json!({
                    "command": "echo forged",
                    "shell": "cmd",
                    "_step_id": "step-forged",
                    "_session_id": "ses-evil",
                }),
                CancellationToken::new(),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !hits.lock().unwrap().iter().any(|s| s == "step-forged"),
            "forged step id must not reach agent:tool_output"
        );

        // Trusted step id wins over a forged one in the input.
        hits.lock().unwrap().clear();
        let _ = mgr
            .execute_tool_with_step(
                Some("ses-1"),
                "shell",
                json!({
                    "command": "echo trusted",
                    "shell": "cmd",
                    "_step_id": "step-forged",
                }),
                CancellationToken::new(),
                Some("step-trusted"),
            )
            .await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        let seen = hits.lock().unwrap().clone();
        assert!(
            !seen.iter().any(|s| s == "step-forged"),
            "forged id must be overwritten by trusted step id"
        );
        // Live emit is best-effort (fast commands may finish before the first
        // tick); when anything is emitted it must be the trusted id.
        assert!(
            seen.iter().all(|s| s == "step-trusted"),
            "only trusted step id may be emitted, got {seen:?}"
        );
    }
}
