pub mod adapters;
pub mod bg;
pub mod builtin;
pub mod circuit;
pub mod inbox;
pub mod live_output;
pub mod simulate;
pub mod skill_runner;
pub mod tool;
pub mod util;

use haven_common::config::{ContextLimitsConfig, McpServerConfig, SkillsExecConfig, ToolConfig};
use haven_common::types::{RiskLevel, ShellChoice};
use haven_llm::LlmRouter;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Whether a tool is enabled per `tool_settings`. Tools without a settings
/// entry are enabled by default. Single source of truth shared by the
/// registry filter, the execution gate, and the UI listing, so the three
/// cannot drift apart.
fn tool_config_enabled(settings: &HashMap<String, ToolConfig>, name: &str) -> bool {
    settings.get(name).map(|c| c.enabled).unwrap_or(true)
}

pub use adapters::{McpToolAdapter, SkillToolAdapter};
pub use builtin::{
    AgentSpawnRequest, AgentSpawnResult, AgentSpawner, ScheduleMode, SelfOperation, SelfParams,
    SelfTool, SelfToolContext,
};
pub use circuit::ToolCircuitRegistry;
pub use haven_mcp::{
    McpClient, McpClientStatus, McpManager, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
pub use haven_skills::{Language, Skill, SkillInfo, SkillManifest, SkillsEngine, VenvManager};
pub use live_output::LiveOutputHub;
pub use skill_runner::SkillRunner;
pub use tool::{
    ConfirmationResult, LOCAL_TOOL_SECURITY_MATRIX, LocalToolSecurityCase, SafetyGateway, Tool,
    ToolBox, ToolDef, ToolRegistration, ToolRegistry, ToolResult, ToolSignals, extract_ask_signal,
    extract_notify_signal, is_safe_local_path, is_silent_action,
};

/// All dependencies needed to install the desktop tool catalog in one pass.
/// This keeps startup wiring extensible without adding another positional
/// argument to the app-to-tools boundary.
pub struct StartupWiring {
    pub tool_settings: HashMap<String, ToolConfig>,
    pub default_shell: ShellChoice,
    pub context_limits: ContextLimitsConfig,
    pub confirmation_mode: haven_common::types::ConfirmationMode,
    pub min_risk_level: RiskLevel,
    pub security_permissions: Vec<haven_common::config::StoredPermission>,
    pub router: Arc<LlmRouter>,
    pub audio_pipeline: Option<Arc<haven_input::InputPipeline>>,
    pub self_ctx: builtin::SelfToolContext,
}

/// Convert a qualified tool name (`mcp::server::tool`, `skill::name`) into a
/// form accepted by tool-calling LLM APIs. OpenAI-compatible providers
/// restrict tool names to `^[a-zA-Z0-9_-]+$` (DeepSeek rejects the `::`
/// namespace separator with a 400, which permanently errors the session after a
/// successful `load_mcp`); Anthropic additionally caps the length at 64.
/// The transform is deterministic so the name advertised to the model in the
/// tool definitions always equals the per-session registration key used for
/// execution lookup — no reverse mapping is needed.
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
        let idx = out.floor_char_boundary(64);
        out.truncate(idx);
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

pub struct ToolsManager {
    pub registry: ToolRegistry,
    pub mcp_manager: McpManager,
    pub mcp_server_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub skills_engine: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub safety_gateway: SafetyGateway,
    tool_settings: RwLock<HashMap<String, ToolConfig>>,
    /// Unified context limits. `max_observation_chars` is the observation
    /// budget for tool outputs fed back into the conversation; per-tool
    /// `tool_settings.*.max_output_chars` overrides it.
    context_limits: RwLock<ContextLimitsConfig>,
    /// Default shell for the `shell` tool, set from app settings
    /// (cmd / powershell / pwsh).
    default_shell: RwLock<ShellChoice>,
    /// Full builtin tool list (enabled and disabled) so the UI can list and
    /// re-enable disabled tools even though they are excluded from the
    /// registry snapshot used by the agent.
    all_builtin_tools: RwLock<Vec<ToolBox>>,
    /// Per-session skill/MCP overlays. Shared as `Arc` so `load_mcp` can
    /// budget-check against the live map before declaring success.
    session_registrations: Arc<RwLock<HashMap<String, HashMap<String, ToolBox>>>>,
    tool_circuits: ToolCircuitRegistry,
    /// Shared LlmRouter. Tools that need a model (currently the file `summary`
    /// and image-understanding operations) call `router.chat(...)` — text
    /// summarization uses the SmallModel role, image understanding uses the
    /// ImageModel role; the router handles retries and the balanced-model
    /// fallback.
    router: RwLock<Option<Arc<LlmRouter>>>,
    /// Registry of background actions (shell with background: true).
    pub background_actions: Arc<bg::BackgroundActions>,
    /// Live stdout/stderr previews for foreground tools (shell).
    pub live_outputs: Arc<live_output::LiveOutputHub>,
    /// Registry of in-process scheduled actions (the `schedule` tool). The fired
    /// channel is consumed by the agent layer, which notifies, runs the
    /// scheduled tool, or resumes the scheduling session (see `ScheduleMode`).
    pub scheduled_actions: Arc<builtin::scheduled_action::ScheduledActionCenter>,
    /// App-level context for the `self` management tool (config loader, DB,
    /// router, log file). Wired in by the desktop shell; `None` in headless
    /// tests so the tool is simply not registered.
    self_context: RwLock<Option<builtin::SelfToolContext>>,
    /// The registered `self` tool instance (the same Arc pushed into the
    /// catalog). Typed handle so app commands can call its native `run`
    /// entry for settings modifications instead of duplicating the mutation
    /// logic. `None` in headless builds.
    self_tool: RwLock<Option<Arc<builtin::SelfTool>>>,
    /// Shared clipboard history for the `clipboard` tool. Lives on the
    /// manager (not the tool) so it survives catalog rebuilds.
    pub clipboard_history: Arc<builtin::clipboard::ClipboardHistory>,
    /// Shared input pipeline for the `audio` tool's `record` operation.
    /// Wired in by the desktop shell; `None` in headless tests so the tool
    /// reports recording as unavailable.
    audio_pipeline: RwLock<Option<Arc<haven_input::InputPipeline>>>,
    /// Monotonic catalog version, bumped whenever the global registry or any
    /// per-session registration changes. The ReAct loop caches per-session tool
    /// definitions keyed by this version, so a bump forces a rebuild without
    /// the loop re-querying schemas on every step. Shared as `Arc` so
    /// `load_mcp` / `load_skill` can bump after in-tool atomic registration.
    catalog_version: Arc<AtomicU64>,
    /// Desktop-wired callback for `agent` spawn. Shared across catalog rebuilds.
    agent_spawner: builtin::AgentSpawnerSlot,
    /// Desktop-wired History/`InferenceEngine` recall for `memory` recall.
    memory_recall: builtin::MemoryRecallSlot,
}

impl ToolsManager {
    pub fn new() -> Self {
        Self::new_with_exec_config(SkillsExecConfig::default())
    }

    pub fn new_with_exec_config(exec_config: SkillsExecConfig) -> Self {
        let registry = ToolRegistry::new();
        let background_actions = Arc::new(bg::BackgroundActions::new());
        let live_outputs = Arc::new(live_output::LiveOutputHub::new());
        let scheduled_actions = Arc::new(builtin::scheduled_action::ScheduledActionCenter::new());
        // Wire the background-action registry into the scheduled_action center so
        // `watch_action_id` scheduled_actions can wait for a action to finish.
        scheduled_actions.set_actions(Some(background_actions.clone()));
        Self {
            registry,
            mcp_manager: McpManager::new(),
            mcp_server_configs: Arc::new(RwLock::new(HashMap::new())),
            skills_engine: haven_skills::SkillsEngine::new(),
            skill_runner: Arc::new(RwLock::new(SkillRunner::new(
                VenvManager::new(exec_config.venv_root.clone()),
                exec_config,
            ))),
            safety_gateway: SafetyGateway::new(RiskLevel::Medium),
            tool_settings: RwLock::new(HashMap::new()),
            context_limits: RwLock::new(ContextLimitsConfig::default()),
            default_shell: RwLock::new(ShellChoice::default()),
            all_builtin_tools: RwLock::new(Vec::new()),
            session_registrations: Arc::new(RwLock::new(HashMap::new())),
            tool_circuits: ToolCircuitRegistry::new(),
            router: RwLock::new(None),
            background_actions,
            live_outputs,
            scheduled_actions,
            self_context: RwLock::new(None),
            self_tool: RwLock::new(None),
            clipboard_history: Arc::new(builtin::clipboard::ClipboardHistory::new(50)),
            audio_pipeline: RwLock::new(None),
            catalog_version: Arc::new(AtomicU64::new(0)),
            agent_spawner: builtin::new_agent_spawner_slot(),
            memory_recall: builtin::new_memory_recall_slot(),
        }
    }

    /// Install the desktop agent-layer callback used by `agent` spawn.
    /// Does not rebuild the catalog (the tool already holds this slot).
    pub async fn set_agent_spawner(&self, spawner: builtin::AgentSpawner) {
        *self.agent_spawner.write().await = Some(spawner);
    }

    /// Install History-aligned recall for `memory` operation=recall.
    pub async fn set_memory_recall(&self, recall: builtin::MemoryRecallFn) {
        *self.memory_recall.write().await = Some(recall);
    }

    /// Monotonic catalog version (see `catalog_version`). Consumers cache
    /// derived views (e.g. per-step LLM tool definitions) keyed by this
    /// value and rebuild only when it changes.
    pub fn catalog_version(&self) -> u64 {
        self.catalog_version.load(Ordering::Relaxed)
    }

    /// Whether adding `net_new` unique session tools would exceed the
    /// per-request provider ceiling. Shared by `load_mcp` / `load_skill`
    /// refuse paths and resume registration.
    pub fn tool_budget_would_exceed(
        max: usize,
        global_count: usize,
        session_count: usize,
        net_new: usize,
    ) -> bool {
        if net_new == 0 {
            return false;
        }
        global_count
            .saturating_add(session_count)
            .saturating_add(net_new)
            > max.max(1)
    }

    /// Replace the shared LlmRouter and rebuild the catalog so tools (e.g.
    /// `file summary`) pick up the new endpoint config.
    pub async fn set_router(&self, router: Arc<LlmRouter>) {
        *self.router.write().await = Some(router);
        self.rebuild_catalog().await;
    }

    /// Apply cold-start wiring in one pass and rebuild the catalog once.
    /// Avoids the N sequential rebuilds that used to block window creation
    /// (`set_tool_settings` + `set_default_shell` + `set_context_limits` +
    /// `set_router` + audio_pipeline + `set_self_context`).
    pub async fn wire_startup(&self, wiring: StartupWiring) {
        let StartupWiring {
            tool_settings,
            default_shell,
            context_limits,
            confirmation_mode,
            min_risk_level,
            security_permissions,
            router,
            audio_pipeline,
            self_ctx,
        } = wiring;
        *self.tool_settings.write().await = tool_settings.clone();
        *self.default_shell.write().await = default_shell;
        self.mcp_manager.set_limits(&context_limits).await;
        self.skills_engine.set_limits(&context_limits).await;
        self.background_actions.set_limits(&context_limits).await;
        self.live_outputs.set_limits(&context_limits).await;
        self.scheduled_actions.set_limits(&context_limits).await;
        *self.context_limits.write().await = context_limits;
        self.safety_gateway
            .apply_security(confirmation_mode, min_risk_level, &security_permissions)
            .await;
        self.safety_gateway.set_tool_settings(tool_settings).await;
        *self.router.write().await = Some(router);
        *self.audio_pipeline.write().await = audio_pipeline;
        self.scheduled_actions.set_db(self_ctx.db.clone()).await;
        self.background_actions.set_db(self_ctx.db.clone()).await;
        *self.self_context.write().await = Some(self_ctx);
        self.rebuild_catalog().await;
    }

    /// Wire the app-level context for the `self` management tool and register
    /// the tool. Called by the desktop shell after the config loader exists;
    /// later catalog rebuilds keep the tool registered. Also hands the DB to
    /// the scheduled-action registry and the background-action registry so scheduled_actions and
    /// action results persist across restarts.
    pub async fn set_self_context(&self, ctx: builtin::SelfToolContext) {
        self.scheduled_actions.set_db(ctx.db.clone()).await;
        self.background_actions.set_db(ctx.db.clone()).await;
        *self.self_context.write().await = Some(ctx);
        self.rebuild_catalog().await;
    }

    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
        *self.tool_settings.write().await = settings.clone();
        self.safety_gateway.set_tool_settings(settings).await;
        self.rebuild_catalog().await;
    }

    /// The registered `self` tool instance, when the desktop shell wired the
    /// app context. App commands call its native `run(SelfParams)` entry for
    /// settings modifications so config mutation lives in one place (the
    /// `self` tool), with the JSON `execute` path serving the LLM.
    pub async fn self_tool(&self) -> Option<Arc<builtin::SelfTool>> {
        self.self_tool.read().await.clone()
    }

    /// Flip the `enabled` flag for one builtin tool in the in-memory
    /// `tool_settings` and rebuild the catalog so the toggle takes effect on
    /// the agent's next step. The config.toml persistence is done by the
    /// caller (the `self` tool's `tool_enable`/`tool_disable` ops, which call
    /// this after persisting).
    pub async fn set_tool_enabled(&self, name: &str, enabled: bool) {
        let mut settings = self.tool_settings.write().await;
        settings
            .entry(name.to_string())
            .or_insert_with(ToolConfig::default)
            .enabled = enabled;
        drop(settings);
        self.rebuild_catalog().await;
    }

    /// Replace the unified context limits (global tool output cap etc.) and
    /// rebuild the catalog so tools pick up the new values.
    pub async fn set_context_limits(&self, limits: ContextLimitsConfig) {
        self.mcp_manager.set_limits(&limits).await;
        self.skills_engine.set_limits(&limits).await;
        self.background_actions.set_limits(&limits).await;
        self.live_outputs.set_limits(&limits).await;
        self.scheduled_actions.set_limits(&limits).await;
        *self.context_limits.write().await = limits;
        self.rebuild_catalog().await;
    }

    /// Replace the default shell for the `shell` tool and rebuild the catalog
    /// so the running agent picks up the new value on its next step.
    pub async fn set_default_shell(&self, shell: ShellChoice) {
        *self.default_shell.write().await = shell;
        self.rebuild_catalog().await;
    }

    pub async fn load_mcp_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        // Store configs for dynamic loading via load_mcp tool
        let mut configs = self.mcp_server_configs.write().await;
        configs.clear();
        for server in servers {
            configs.insert(server.name.clone(), server.clone());
        }
        drop(configs);

        self.mcp_manager.load_from_config(servers).await;
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
            let mut configs = self.mcp_server_configs.write().await;
            configs.clear();
            for server in servers {
                configs.insert(server.name.clone(), server.clone());
            }
        }
        self.mcp_manager.discover_all(servers, config).await;
    }

    /// Rebuild the tool catalog from the current builtin state.
    /// Called at startup and whenever MCP or Skills state changes.
    ///
    /// Skills and MCP servers are progressively loaded (refine §4.7): only the
    /// `load_skill` / `load_mcp` meta-tools are registered globally. Full skill
    /// and MCP tool adapters are NOT injected into the global registry until the
    /// LLM explicitly calls `load_skill` / `load_mcp`, which registers them
    /// per-session (see `register_for_session`).
    pub async fn rebuild_catalog(&self) {
        let mut all_tools: Vec<ToolBox> = Vec::new();

        // Register builtin tools (including progressive load_skill and load_mcp)
        let router = self.router.read().await.clone();
        let self_context = self.self_context.read().await.clone();
        let settings = self.tool_settings.read().await;
        let limits = self.context_limits.read().await.clone();
        let audio_pipeline = self.audio_pipeline.read().await.clone();
        let self_tool_arc = builtin::register_builtin_tools(
            &mut all_tools,
            &self.skills_engine,
            &self.skill_runner,
            &Arc::new(self.mcp_manager.clone()),
            &self.mcp_server_configs,
            router,
            self.background_actions.clone(),
            self.live_outputs.clone(),
            self.scheduled_actions.clone(),
            self_context,
            self.registry.clone(),
            self.clipboard_history.clone(),
            &settings,
            &limits,
            *self.default_shell.read().await,
            audio_pipeline,
            self.session_registrations.clone(),
            self.catalog_version.clone(),
            self.agent_spawner.clone(),
            self.memory_recall.clone(),
        )
        .await;
        *self.self_tool.write().await = self_tool_arc;

        // Keep the full list (enabled + disabled) for the UI, and exclude
        // disabled tools from the registry the agent sees.
        let enabled_tools: Vec<ToolBox> = all_tools
            .iter()
            .filter(|t| tool_config_enabled(&settings, &t.name()))
            .cloned()
            .collect();
        drop(settings);

        *self.all_builtin_tools.write().await = all_tools;
        self.registry.rebuild(enabled_tools).await;
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Register a tool for a specific session (per-session skill overlay).
    /// Does NOT modify the global registry.
    pub async fn register_for_session(&self, session_id: &str, tool: ToolBox) {
        let name = tool.name();
        self.session_registrations
            .write()
            .await
            .entry(session_id.to_string())
            .or_default()
            .insert(name, tool);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Remove all per-session tool registrations for a given session.
    pub async fn unregister_session(&self, session_id: &str) {
        self.session_registrations.write().await.remove(session_id);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
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
        let Some(client) = self.mcp_manager.get_client(server_name).await else {
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
            .context_limits
            .read()
            .await
            .max_tools_per_request
            .max(1);
        let global_count = self.registry.list().await.len();
        let mut reg = self.session_registrations.write().await;
        let entry = reg.entry(session_id.to_string()).or_default();
        let session_count = entry.len();
        let net_new = tools
            .iter()
            .filter(|info| {
                let name = McpToolAdapter::qualified_name_of(server_name, &info.name);
                !entry.contains_key(&name)
            })
            .count();
        if Self::tool_budget_would_exceed(max, global_count, session_count, net_new) {
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
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Resume / executor path for skills: refuse when a *new* skill tool
    /// would exceed the per-request ceiling (reload of an already-registered
    /// skill name is always allowed).
    pub async fn register_skill_for_session(&self, session_id: &str, skill_name: &str) -> bool {
        let Some(skill) = self.skills_engine.get_skill(skill_name).await else {
            return false;
        };
        if !skill.enabled() {
            return false;
        }
        let runner = self.skill_runner.read().await.clone();
        let adapter = SkillToolAdapter::new(Arc::new(skill), runner);
        let name = adapter.name();
        let max = self
            .context_limits
            .read()
            .await
            .max_tools_per_request
            .max(1);
        let global_count = self.registry.list().await.len();
        let mut reg = self.session_registrations.write().await;
        let entry = reg.entry(session_id.to_string()).or_default();
        let already = entry.contains_key(&name);
        let net_new = if already { 0 } else { 1 };
        if Self::tool_budget_would_exceed(max, global_count, entry.len(), net_new) {
            tracing::warn!(
                session_id,
                skill_name,
                max,
                "register_skill_for_session: refusing skill over max_tools_per_request"
            );
            return false;
        }
        entry.insert(name, Arc::new(adapter));
        drop(reg);
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Look up a tool: first check per-session registrations, then global registry.
    pub async fn get_tool_for_session(
        &self,
        session_id: Option<&str>,
        name: &str,
    ) -> Option<ToolBox> {
        if let Some(tid) = session_id
            && let reg = self.session_registrations.read().await
            && let Some(tools) = reg.get(tid)
            && let Some(tool) = tools.get(name)
        {
            return Some(tool.clone());
        }
        self.registry.get(name).await
    }

    /// Build a skill index (raw name + description) for injection into the
    /// system prompt (refine §4.7). The LLM uses `load_skill` to get full
    /// schemas. The raw skill name is shown so the value passed to
    /// `load_skill(skill_name)` matches (the index previously advertised the
    /// transformed `skill__<name>` tool name, which `load_skill` rejected).
    pub async fn build_skill_index(&self) -> Vec<Value> {
        let skills = self.skills_engine.list().await;
        skills
            .into_iter()
            .filter(|s| s.enabled)
            .map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                })
            })
            .collect()
    }

    /// Build an MCP server index (name + available tool names) for injection
    /// into the system prompt. The LLM uses `load_mcp` to get full schemas.
    /// Only enabled servers are listed — disabled ones cannot be loaded.
    /// Tool names are included (when the server is connected and cached) so
    /// the LLM can judge whether a server's tools fit the session instead of
    /// defaulting to weaker built-ins.
    pub async fn build_mcp_index(&self) -> Vec<Value> {
        let configs = self.mcp_server_configs.read().await;
        let mut entries: Vec<Value> = Vec::new();
        for s in configs.values().filter(|s| s.enabled) {
            let mut tool_names: Vec<String> = match self.mcp_manager.get_client(&s.name).await {
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
            let description = if tool_names.is_empty() {
                format!(
                    "MCP server '{}' via {} ({})",
                    s.name,
                    s.command,
                    s.args.join(" ")
                )
            } else {
                format!(
                    "MCP server '{}' via {} ({}); tools: {}",
                    s.name,
                    s.command,
                    s.args.join(" "),
                    tool_names.join(", ")
                )
            };
            entries.push(serde_json::json!({
                "name": s.name.clone(),
                "description": description,
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

    /// Structured tool definitions for a session: the global registry merged
    /// with per-session registered skill/MCP adapters. This is the canonical
    /// surface the ReAct loop turns into provider tool definitions and the
    /// schema listing is derived from — no loose JSON assembly in consumers.
    ///
    /// Capped at `context_limits.max_tools_per_request`: builtins are kept
    /// first; session overlays are sorted by name and truncated to fit so a
    /// runaway `load_mcp` history cannot 400 the provider.
    pub async fn list_defs_for_session(&self, session_id: &str) -> Vec<ToolDef> {
        let max = self
            .context_limits
            .read()
            .await
            .max_tools_per_request
            .max(1);
        let mut defs = self.registry.list_defs().await;
        let global_len = defs.len();
        let reg = self.session_registrations.read().await;
        if let Some(tools) = reg.get(session_id) {
            let mut session_defs: Vec<ToolDef> = tools.values().map(|t| t.tool_def()).collect();
            session_defs.sort_by(|a, b| a.name.cmp(&b.name));
            defs.extend(session_defs);
        }
        if defs.len() > max {
            tracing::warn!(
                session_id,
                total = defs.len(),
                max,
                global = global_len,
                "list_defs_for_session: truncating tools to max_tools_per_request (builtins kept first)"
            );
            // Prefer builtins: if they alone exceed max, truncate them; else
            // drop the overflow from the (already sorted) session tail.
            defs.truncate(max);
        }
        defs
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
        self.mcp_server_configs
            .write()
            .await
            .insert(config.name.clone(), config);
    }

    /// Remove a single MCP server config from the in-memory map.
    pub async fn remove_mcp_server_config(&self, name: &str) {
        self.mcp_server_configs.write().await.remove(name);
    }

    /// List all known MCP server configs (enabled and disabled).
    pub async fn list_mcp_server_configs(&self) -> Vec<McpServerConfig> {
        self.mcp_server_configs
            .read()
            .await
            .values()
            .cloned()
            .collect()
    }

    /// Whether a tool is enabled per `tool_settings`. Tools without a
    /// settings entry are enabled by default.
    pub async fn tool_enabled(&self, name: &str) -> bool {
        tool_config_enabled(&*self.tool_settings.read().await, name)
    }

    /// Schemas for ALL builtin tools (enabled and disabled) plus their
    /// `enabled` state, so the UI can list every tool and re-enable disabled
    /// ones. The registry itself only holds enabled tools (see
    /// `rebuild_catalog`).
    /// Poll the skills directory for changes and auto-refresh the engine
    /// whenever `SKILL.md` files are added / modified / removed. The first
    /// pass always refreshes too, so a UI that loaded before the initial
    /// scan finished (startup race) still catches up. `on_change` fires on
    /// the background action after a successful refresh so callers can
    /// re-sync views / emit events (e.g. `skills:status_change`).
    pub fn spawn_skills_watcher(
        self: Arc<Self>,
        poll_interval: Duration,
        on_change: impl Fn() + Send + Sync + 'static,
    ) {
        let engine = self.skills_engine.clone();
        tokio::spawn(async move {
            let mut last_sig: Option<Vec<(std::path::PathBuf, std::time::SystemTime, u64)>> = None;
            loop {
                let sig = engine.folder_signature().await;
                let changed = last_sig.is_none() || last_sig.as_ref() != Some(&sig);
                if changed {
                    match engine.refresh_from_disk().await {
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
                tokio::time::sleep(poll_interval).await;
            }
        });
    }

    pub async fn list_builtin_tools(&self) -> Vec<Value> {
        let tools = self.all_builtin_tools.read().await;
        let settings = self.tool_settings.read().await;
        tools
            .iter()
            .map(|t| {
                let mut json = t.tool_def().json();
                json.as_object_mut()
                    .expect("ToolDef::json returns an object")
                    .insert(
                        "enabled".into(),
                        serde_json::json!(tool_config_enabled(&settings, &t.name())),
                    );
                json
            })
            .collect()
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
        if !self.tool_circuits.allow_request(tool_name) {
            tracing::warn!("tool '{}' circuit breaker open — fast-failing", tool_name);
            anyhow::bail!(
                "tool '{}' is temporarily unavailable (circuit breaker open)",
                tool_name
            );
        }

        if !self.tool_enabled(tool_name).await {
            tracing::warn!("tool '{}' is disabled", tool_name);
            anyhow::bail!("tool '{}' is disabled", tool_name);
        }

        let tool = self
            .get_tool_for_session(session_id, tool_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("tool '{}' not found in registry", tool_name))?;

        // Private fields (`_session_id` / `_step_id`) are never trusted from
        // the LLM or scheduled tool_args: always strip first, validate the
        // LLM-facing input, then re-inject only caller-supplied values.
        // Declared via `Tool::requires_session_id` / `supports_live_output`.
        let mut exec_input = input;
        if let Some(obj) = exec_input.as_object_mut() {
            obj.remove("_session_id");
            obj.remove("_step_id");
        }
        tool.validate_input(&exec_input)?;
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
        let settings = self.tool_settings.read().await;
        let cfg = settings.get(tool_name).cloned().unwrap_or_default();
        let timeout_secs = if settings.contains_key(tool_name) {
            cfg.timeout_secs
        } else {
            tool.timeout_secs_for(&exec_input)
        };
        let max_retries = cfg.max_retries;
        let backoff_secs = cfg.retry_backoff_secs;
        // A timeout can mean an external write actually completed but its
        // response was lost. Retry Safe operations by default; higher-risk
        // tools require an explicit per-tool opt-in.
        let retry_allowed = tool.risk_level(&exec_input) == RiskLevel::Safe || cfg.retry_unsafe;
        drop(settings);

        let max_attempts = 1 + max_retries;
        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = Duration::from_secs(
                    backoff_secs.saturating_mul(2u64.saturating_pow(attempt - 1)),
                );
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {},
                    _ = cancel.cancelled() => anyhow::bail!("cancelled"),
                }
            }
            if cancel.is_cancelled() {
                anyhow::bail!("cancelled");
            }

            match tool
                .execute_with_timeout(exec_input.clone(), cancel.clone(), timeout_secs)
                .await
            {
                Ok(result) => {
                    self.tool_circuits.record_success(tool_name);
                    // Attach the tool's declared side-channel signals (ask
                    // question / notify toast) BEFORE returning: consumers
                    // (the ReAct loop) read structured signals instead of
                    // name-matching and re-parsing the output JSON.
                    let mut result = result;
                    result.signals = tool.signals(&result.output);
                    return Ok(result);
                }
                Err(e)
                    if retry_allowed
                        && attempt + 1 < max_attempts
                        && is_retryable_tool_error(&e) =>
                {
                    tracing::debug!(
                        "tool '{}' attempt {} failed, retrying: {}",
                        tool_name,
                        attempt + 1,
                        e
                    );
                    continue;
                }
                Err(e) => {
                    self.tool_circuits.record_failure(tool_name);
                    // Long-running tools may hand the work to a background
                    // mechanism on timeout instead of failing the step, so
                    // the session can continue and pick the result up later
                    // (auto-pushed on completion). Declared per-tool via
                    // `Tool::timeout_fallback` (currently the shell tool).
                    if !cancel.is_cancelled()
                        && is_tool_timeout(&e)
                        && let Some(result) = tool.timeout_fallback(&exec_input).await
                    {
                        return Ok(result);
                    }
                    return Err(e);
                }
            }
        }
        self.tool_circuits.record_failure(tool_name);
        anyhow::bail!("tool '{}' retries exhausted", tool_name);
    }

    pub fn tool_circuits(&self) -> &ToolCircuitRegistry {
        &self.tool_circuits
    }

    pub async fn get_tool(&self, name: &str) -> Option<ToolBox> {
        self.registry.get(name).await
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
            .map(|t| t.risk_level(input))
            .unwrap_or(RiskLevel::Safe);
        self.safety_gateway
            .effective_risk(tool_name, reported)
            .await
    }
}

/// Returns `true` if a tool execution error is transient and worth retrying.
fn is_retryable_tool_error(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_lowercase();
    !msg.contains("cancelled")
        && (msg.contains("timed out")
            || msg.contains("timeout")
            || msg.contains("connection refused")
            || msg.contains("connection reset")
            || msg.contains("connection aborted")
            || msg.contains("connection closed")
            || msg.contains("network is unreachable")
            || msg.contains("temporary failure")
            || msg.contains("temporarily unavailable")
            || msg.contains("service unavailable")
            || msg.contains("too many requests")
            || msg.contains("rate limit")
            || msg.contains("status 429")
            || msg.contains("status 502")
            || msg.contains("status 503")
            || msg.contains("status 504")
            || msg.contains("eof"))
}

/// Whether a tool call failed because its time budget ran out.
fn is_tool_timeout(err: &anyhow::Error) -> bool {
    err.to_string().to_lowercase().contains("timed out")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn test_tools_manager_new() {
        let mgr = ToolsManager::new();
        let tools = mgr.registry.list().await;
        assert!(tools.is_empty());
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
        assert_eq!(mgr.context_limits.read().await.max_observation_chars, 8_000);
        let limits = ContextLimitsConfig {
            max_observation_chars: 5_000,
            ..Default::default()
        };
        mgr.set_context_limits(limits).await;
        assert_eq!(mgr.context_limits.read().await.max_observation_chars, 5_000);
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

        let file_tool = mgr.get_tool("files").await;
        assert!(file_tool.is_some());

        let process_tool = mgr.get_tool("process").await;
        assert!(process_tool.is_some());

        let clipboard_tool = mgr.get_tool("clipboard").await;
        assert!(clipboard_tool.is_some());

        let load_skill_tool = mgr.get_tool("load_skill").await;
        assert!(load_skill_tool.is_some());
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
        let schemas = mgr.registry.list_schemas().await;
        assert!(!schemas.iter().any(|s| s["name"].as_str() == Some("files")));

        // ...still listed for the UI with enabled = false...
        let all = mgr.list_builtin_tools().await;
        let file = all
            .iter()
            .find(|s| s["name"].as_str() == Some("files"))
            .unwrap();
        assert_eq!(file["enabled"].as_bool(), Some(false));

        // ...and execution is blocked.
        let result = mgr
            .execute_tool(None, "files", json!({}), CancellationToken::new())
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
                "files",
                json!({"operation": "read", "path": file.to_string_lossy()}),
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
        mgr.registry
            .register(Arc::new(FailingTool {
                name: "failing".into(),
                call_count: call_count.clone(),
            }))
            .await;

        for i in 0..5 {
            let r = mgr
                .execute_tool(None, "failing", json!({}), CancellationToken::new())
                .await;
            assert!(r.is_err(), "call {} should fail", i + 1);
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
                retry_backoff_secs: 0,
                ..Default::default()
            },
        )]))
        .await;
        mgr.registry
            .register(Arc::new(FlakyTool {
                attempts: attempts.clone(),
            }))
            .await;

        let result = mgr
            .execute_tool(None, "flaky", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn retryable_tool_errors_exclude_cancellation_and_logic_failures() {
        assert!(is_retryable_tool_error(&anyhow::anyhow!("status 503")));
        assert!(is_retryable_tool_error(&anyhow::anyhow!(
            "connection reset"
        )));
        assert!(!is_retryable_tool_error(&anyhow::anyhow!(
            "cancelled while waiting"
        )));
        assert!(!is_retryable_tool_error(&anyhow::anyhow!(
            "input validation failed"
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
        let runner = mgr.skill_runner.read().await.clone();
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
        let schemas = mgr.registry.list_schemas().await;
        assert!(
            !schemas
                .iter()
                .any(|s| { s["name"].as_str().unwrap_or("").starts_with("mcp__") }),
            "MCP tools must not be pre-registered globally"
        );
    }

    #[tokio::test]
    async fn test_list_defs_for_session_caps_at_max_tools() {
        use haven_common::config::ContextLimitsConfig;

        let mgr = ToolsManager::new();
        mgr.rebuild_catalog().await;
        let global = mgr.registry.list_defs().await.len();
        assert!(global > 0, "catalog should have builtins");

        // Leave room for only 2 session overlays. Write the limit directly so
        // we do not rebuild the catalog (and shift `global`) mid-test.
        let mut limits = ContextLimitsConfig::default();
        limits.max_tools_per_request = global + 2;
        *mgr.context_limits.write().await = limits;

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
        mgr.live_outputs
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
