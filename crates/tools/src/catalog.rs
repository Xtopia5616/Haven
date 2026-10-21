use super::*;
use crate::adapters::McpToolAdapter;
use haven_common::mcp::McpToolExposure;
use haven_mcp::McpToolInfo;

/// Stable summary of an enabled MCP server for the model-facing prompt.
/// Full tool schemas are loaded separately and remain dynamic JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerIndexEntry {
    pub name: String,
    pub tool_names: Vec<String>,
}

impl McpServerIndexEntry {
    pub(crate) fn from_raw(name: &str, tool_names: Vec<String>) -> Self {
        let mut tool_names: Vec<_> = tool_names
            .iter()
            .map(|tool_name| sanitize_index_field(tool_name))
            .collect();
        tool_names.sort();
        tool_names.dedup();
        Self {
            name: sanitize_index_field(name),
            tool_names,
        }
    }
}

fn attach_mcp_output_review_owner(
    adapter: McpToolAdapter,
    runtime: &crate::tool_runtime::ToolRuntime,
) -> McpToolAdapter {
    adapter.with_sandbox_output_reviews(runtime.sandbox_output_reviews.clone())
}

fn refreshed_mcp_adapters(
    server_name: &str,
    manager: &haven_mcp::McpManager,
    transport: &haven_common::mcp::McpServerTransport,
    runtime: &crate::tool_runtime::ToolRuntime,
    active_tools: Vec<(String, String, ToolHandle)>,
    available: &[McpToolInfo],
) -> (Vec<ToolHandle>, Vec<String>) {
    let available: HashMap<_, _> = available
        .iter()
        .map(|info| (info.name.as_str(), info))
        .collect();
    let mut replacements = Vec::new();
    let mut removals = Vec::new();
    for (qualified_name, operation, current) in active_tools {
        let Some(info) = available.get(operation.as_str()) else {
            removals.push(qualified_name);
            continue;
        };
        let refreshed = attach_mcp_output_review_owner(
            McpToolAdapter::new_with_transport(
                manager.clone(),
                server_name,
                (**info).clone(),
                transport.clone(),
            ),
            runtime,
        );
        let refreshed = Arc::new(refreshed) as ToolHandle;
        if current.tool_manifest() != refreshed.tool_manifest() {
            replacements.push(refreshed);
        }
    }
    (replacements, removals)
}

impl ToolsFacade {
    pub async fn load_mcp_from_config(&self, servers: &[haven_common::config::McpServerConfig]) {
        self.coordinator.load_mcp_from_config(servers).await;
    }

    pub async fn discover_all(
        &self,
        servers: &[haven_common::config::McpServerConfig],
        config: &haven_common::config::McpDiscoveryConfig,
    ) {
        self.coordinator.discover_all(servers, config).await;
    }

    /// Rebuild the tool catalog from the current builtin state.
    /// Called at startup and whenever MCP or Skills state changes.
    ///
    /// MCP and Skill providers are progressively loaded: their stable loader
    /// tools are always advertised, while adapters are registered per-session
    /// only after an explicit load succeeds. Enabled Skills are rebuilt into
    /// the deferred catalog from the live skills index.
    pub async fn rebuild_catalog(
        &self,
    ) -> Result<crate::CatalogRebuildOutcome, crate::CatalogRebuildError> {
        self.coordinator.rebuild_catalog().await
    }

    /// Register a tool for a specific session (per-session skill overlay).
    /// Does NOT modify the global registry.
    #[cfg(test)]
    pub(crate) async fn register_for_session(&self, session_id: &str, tool: ToolHandle) {
        self.coordinator
            .core
            .operations
            .session_tool_overlay
            .register(session_id, tool)
            .await;
    }

    /// Rehydrate a saved built-in selection during resume without exposing the
    /// loader's private session field to transcript or provider input.
    pub async fn load_builtin_operations_for_session(
        &self,
        session_id: &str,
        operations: Option<Vec<String>>,
        roots: Option<Vec<String>>,
    ) -> bool {
        let catalog = builtin::discover_tools::DiscoverToolsTool {
            deferred_catalog: self.coordinator.core.operations.deferred.clone(),
            registry: self.coordinator.core.operations.installed.clone(),
            session_tool_overlay: self
                .coordinator
                .core
                .operations
                .session_tool_overlay
                .clone(),
            max_tools_per_request: self
                .coordinator
                .runtime
                .platform()
                .await
                .context_limits
                .max_tools_per_request
                .max(1),
            mcp_manager: Arc::new(self.coordinator.builtins.mcp_manager.clone()),
            mcp_server_config_source: self.coordinator.builtins.mcp_server_config_source.clone(),
        };
        match catalog
            .run(
                builtin::discover_tools::DiscoverToolsParams {
                    action: "load".into(),
                    level: None,
                    source: Some("builtin".into()),
                    query: None,
                    name: None,
                    root: None,
                    cursor: None,
                    limit: None,
                    revision: None,
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
                tracing::warn!(
                    session_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to restore built-in tool selection"
                );
                false
            }
        }
    }

    /// Rehydrate saved Skill selections during resume. A missing or disabled
    /// Skill is a soft restore failure; the session continues with the skills
    /// that are still available.
    pub async fn load_skill_for_session(&self, session_id: &str, names: Vec<String>) -> bool {
        let platform = self.coordinator.runtime.platform().await;
        let loader = builtin::load_skill::LoadSkillTool {
            deferred_catalog: self.coordinator.core.operations.deferred.clone(),
            registry: self.coordinator.core.operations.installed.clone(),
            skill_registry: self.coordinator.builtins.skill_registry.clone(),
            session_tool_overlay: self
                .coordinator
                .core
                .operations
                .session_tool_overlay
                .clone(),
            max_tools_per_request: platform.context_limits.max_tools_per_request.max(1),
            max_observation_chars: builtin::tool_output_cap(
                &platform.tool_settings,
                "load_skill",
                platform.context_limits.max_observation_chars,
            ),
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
                tracing::warn!(
                    session_id,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to restore Skill selection"
                );
                false
            }
        }
    }

    /// Remove all per-session tool registrations for a given session.
    pub async fn unregister_session(&self, session_id: &str) {
        self.coordinator
            .core
            .operations
            .session_tool_overlay
            .unregister(session_id)
            .await;
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
        if !self
            .coordinator
            .builtins
            .mcp_manager
            .has_client(server_name)
            .await
        {
            return false;
        }
        let all_tools = self
            .coordinator
            .builtins
            .mcp_manager
            .wait_for_tools(server_name, Duration::from_secs(3))
            .await
            .unwrap_or_default();
        let Some(config) = self
            .coordinator
            .builtins
            .mcp_server_config_source
            .get(server_name)
            .await
            .ok()
            .flatten()
            .filter(|config| config.enabled)
        else {
            return false;
        };
        let deferred_tools: Vec<_> = all_tools
            .into_iter()
            .filter(|info| config.exposure.for_tool(&info.name) == McpToolExposure::Deferred)
            .collect();
        let tools = match tool_names {
            None => deferred_tools,
            Some(names) => {
                let want: std::collections::HashSet<&str> =
                    names.iter().map(|s| s.as_str()).collect();
                deferred_tools
                    .into_iter()
                    .filter(|info| want.contains(info.name.as_str()))
                    .collect::<Vec<_>>()
            }
        };
        let max = self
            .coordinator
            .runtime
            .platform()
            .await
            .context_limits
            .max_tools_per_request
            .max(1);
        let global_count = self
            .coordinator
            .core
            .operations
            .installed
            .list()
            .await
            .len();
        let adapters = tools
            .into_iter()
            .map(|info| {
                Arc::new(attach_mcp_output_review_owner(
                    McpToolAdapter::new_with_transport(
                        self.coordinator.builtins.mcp_manager.clone(),
                        server_name,
                        info,
                        config.transport.clone(),
                    ),
                    &self.coordinator.runtime,
                )) as ToolHandle
            })
            .collect();
        if let Err(budget) = self
            .coordinator
            .core
            .operations
            .session_tool_overlay
            .register_many_if_within_budget(session_id, global_count, max, adapters)
            .await
        {
            tracing::warn!(
                session_id,
                net_new = budget.net_new,
                max = budget.max,
                global_count = budget.global_count,
                session_count = budget.session_count,
                "register_mcp_for_session: refusing server because max_tools_per_request was reached"
            );
        }
        true
    }

    /// Look up a tool: first check per-session registrations, then global registry.
    pub async fn get_tool_for_session(
        &self,
        session_id: Option<&str>,
        name: &str,
    ) -> Option<ToolHandle> {
        if let Some(tid) = session_id {
            if let Some(tool) = self
                .coordinator
                .core
                .operations
                .session_tool_overlay
                .get(tid, name)
                .await
            {
                if let Some((server_name, tool_name)) = tool.mcp_tool_identity() {
                    let config = self
                        .coordinator
                        .builtins
                        .mcp_server_config_source
                        .get(server_name)
                        .await
                        .ok()
                        .flatten()?;
                    if !config.enabled
                        || !matches!(
                            config.exposure.for_tool(tool_name),
                            McpToolExposure::Direct
                                | McpToolExposure::ModelOnly
                                | McpToolExposure::Deferred
                        )
                    {
                        return None;
                    }
                }
                return Some(tool);
            }

            let snapshot = self.tool_catalog_snapshot(tid).await;
            let advertised = snapshot
                .provider_definitions()
                .iter()
                .any(|definition| definition.name == name);
            if advertised {
                return snapshot.get(name).cloned();
            }
        }
        self.coordinator.core.operations.installed.get(name).await
    }

    /// Resolve an MCP adapter exclusively through a CodeMode allowlist. This
    /// path intentionally does not consult the provider-native surface, and
    /// refuses tools whose live config no longer selects CodeMode.
    pub(crate) async fn get_codemode_subcall_tool_for_session(
        &self,
        session_id: Option<&str>,
        name: &str,
    ) -> Option<ToolHandle> {
        if !session_id.is_some_and(|id| haven_common::types::is_canonical_id(id, "ses")) {
            return None;
        }
        let configs = self
            .coordinator
            .builtins
            .mcp_server_config_source
            .list()
            .await
            .ok()?;
        let manager = &self.coordinator.builtins.mcp_manager;
        let platform = self.coordinator.runtime.platform().await;
        let settings = &platform.tool_settings;
        for config in configs.into_iter().filter(|config| config.enabled) {
            let Some(available) = manager.cached_tools(&config.name).await else {
                continue;
            };
            for info in available {
                if config.exposure.for_tool(&info.name) != McpToolExposure::CodeMode {
                    continue;
                }
                let qualified_name = McpToolAdapter::qualified_name_of(&config.name, &info.name);
                if qualified_name == name && tool_config_enabled(settings, &qualified_name) {
                    return Some(Arc::new(attach_mcp_output_review_owner(
                        McpToolAdapter::new_with_transport(
                            manager.clone(),
                            &config.name,
                            info,
                            config.transport.clone(),
                        ),
                        &self.coordinator.runtime,
                    )) as ToolHandle);
                }
            }
        }
        None
    }

    /// Build an MCP server index (name + available tool names) for injection
    /// into the system prompt. The LLM uses `load_mcp` to get full schemas.
    /// Only enabled servers are listed — disabled ones cannot be loaded.
    /// Tool names are included (when the server is connected and cached) so
    /// the LLM can judge whether a server's tools fit the session instead of
    /// defaulting to weaker built-ins.
    pub async fn build_mcp_index(&self) -> Vec<McpServerIndexEntry> {
        self.coordinator.build_mcp_index().await
    }

    #[cfg(test)]
    pub(crate) async fn upsert_mcp_server_config(&self, config: McpServerConfig) {
        self.coordinator
            .builtins
            .mcp_server_config_source
            .upsert_fixture(config)
            .await;
        self.coordinator.builtins.mcp_manager.invalidate_catalog();
        self.coordinator
            .core
            .operations
            .session_tool_overlay
            .bump_global_version();
    }

    #[cfg(test)]
    pub(crate) async fn remove_mcp_server_config(&self, name: &str) {
        self.coordinator
            .builtins
            .mcp_server_config_source
            .remove_fixture(name)
            .await;
        self.coordinator.builtins.mcp_manager.invalidate_catalog();
        self.coordinator
            .core
            .operations
            .session_tool_overlay
            .bump_global_version();
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
    pub async fn list_tool_definitions_for_session(&self, session_id: &str) -> Vec<ToolDef> {
        self.operation_catalog()
            .list_tool_definitions_for_session(session_id)
            .await
    }

    /// Return tool schemas for a session: global registry schemas derived
    /// from [`ToolDef`]s merged with per-session registered skill/MCP
    /// adapters. Convenience JSON view over [`Self::list_tool_definitions_for_session`].
    pub async fn list_schemas_for_session(&self, session_id: &str) -> Vec<Value> {
        self.operation_catalog()
            .list_schemas_for_session(session_id)
            .await
    }

    /// Whether a tool is enabled per `tool_settings`. Tools without a
    /// settings entry are enabled by default.
    pub async fn tool_enabled(&self, name: &str) -> bool {
        tool_config_enabled(
            &self.coordinator.runtime.platform().await.tool_settings,
            name,
        )
    }

    /// Schemas for ALL model-facing builtin operation views (enabled and disabled) plus their
    /// `enabled` state, so the UI can list every tool and re-enable disabled
    /// ones. The registry itself only holds enabled tools (see
    /// `rebuild_catalog`).
    /// Poll the skills directory for changes and auto-refresh the registry
    /// whenever `SKILL.md` files are added / modified / removed. The first
    /// pass always refreshes too, so a UI that loaded before the initial
    /// scan finished (startup race) still catches up. `on_change` fires on
    /// the background ToolRun after a successful refresh so callers can
    /// re-sync views / emit events (e.g. `skills:status_change`).
    pub async fn run_skills_watcher(
        self: Arc<Self>,
        poll_interval: Duration,
        cancellation: CancellationToken,
        on_change: impl Fn() + Send + Sync + 'static,
    ) {
        let skill_registry = self.coordinator.builtins.skill_registry.clone();
        loop {
            let changed = tokio::select! {
                _ = cancellation.cancelled() => return,
                result = skill_registry.refresh_if_changed() => match result {
                    Ok(changed) => changed,
                    Err(error) => {
                        tracing::warn!(
                            error = %haven_common::error::sanitize_error_text(&error.to_string()),
                            "skills auto-refresh failed"
                        );
                        false
                    }
                },
            };
            if changed {
                if let Err(error) = self.rebuild_catalog().await {
                    tracing::warn!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "skills changed but tool catalog rebuild failed"
                    );
                }
                on_change();
            }
            tokio::select! {
                _ = cancellation.cancelled() => return,
                _ = tokio::time::sleep(poll_interval) => {}
            }
        }
    }

    pub async fn list_builtin_tools(&self) -> Vec<Value> {
        self.operation_catalog().list_builtin_tools().await
    }

    /// Canonical UI catalog projection. Unlike `list_builtin_tools`, this
    /// does not merge provider-facing fields into a second flat DTO: the
    /// manifest is the only source the frontend should hydrate.
    pub async fn list_builtin_manifests(&self) -> Vec<ToolManifest> {
        self.operation_catalog().list_builtin_manifests().await
    }

    /// Prompt-facing catalog of every enabled builtin, including deferred
    /// operation views. This intentionally returns structured definitions only
    /// to the agent prompt builder; provider `tools[]` still uses the smaller
    /// core + session-loaded surface from `list_tool_definitions_for_session`.
    pub async fn list_enabled_builtin_tool_definitions(&self) -> Vec<ToolDef> {
        self.operation_catalog()
            .list_enabled_builtin_tool_definitions()
            .await
    }
}

/// Model-visible projection of [`OperationRegistry`].
///
/// Loading and session admission stay on `ToolsFacade`. This type only
/// reads the registry and emits provider definitions or UI manifests.
pub struct OperationCatalog<'a> {
    facade: &'a ToolsFacade,
}

impl ToolsFacade {
    pub(crate) fn operation_catalog(&self) -> OperationCatalog<'_> {
        OperationCatalog { facade: self }
    }
}

impl OperationCatalog<'_> {
    /// Build ephemeral adapters for tools configured for native provider
    /// exposure. They are session scoped because MCP credentials and active
    /// server state remain owned by the manager, while the exposure policy is
    /// read from the authoritative config on every catalog snapshot.
    async fn direct_mcp_tools_for_session(&self) -> Vec<ToolHandle> {
        let configs = match self
            .facade
            .coordinator
            .builtins
            .mcp_server_config_source
            .list()
            .await
        {
            Ok(configs) => configs,
            Err(error) => {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to read MCP exposure config while building direct tool surface"
                );
                return Vec::new();
            }
        };
        let manager = &self.facade.coordinator.builtins.mcp_manager;
        let platform = self.facade.coordinator.runtime.platform().await;
        let settings = &platform.tool_settings;
        let mut tools = Vec::new();
        for config in configs.into_iter().filter(|config| config.enabled) {
            let Some(available) = manager.cached_tools(&config.name).await else {
                continue;
            };
            for info in available {
                if !config.exposure.for_tool(&info.name).is_provider_native() {
                    continue;
                }
                let name = McpToolAdapter::qualified_name_of(&config.name, &info.name);
                if !tool_config_enabled(settings, &name) {
                    continue;
                }
                tools.push(Arc::new(attach_mcp_output_review_owner(
                    McpToolAdapter::new_with_transport(
                        manager.clone(),
                        &config.name,
                        info,
                        config.transport.clone(),
                    ),
                    &self.facade.coordinator.runtime,
                )) as ToolHandle);
            }
        }
        tools.sort_by_key(|tool| tool.name());
        tools
    }

    /// Build the trusted MCP tool description set for the synthetic CodeMode
    /// invocation. These capabilities never become top-level model tools.
    async fn codemode_tool_for_session(&self) -> Option<ToolHandle> {
        let configs = match self
            .facade
            .coordinator
            .builtins
            .mcp_server_config_source
            .list()
            .await
        {
            Ok(configs) => configs,
            Err(error) => {
                tracing::warn!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to read MCP exposure config while building CodeMode tool surface"
                );
                return None;
            }
        };
        let manager = &self.facade.coordinator.builtins.mcp_manager;
        let platform = self.facade.coordinator.runtime.platform().await;
        let settings = &platform.tool_settings;
        let mut definitions = Vec::new();
        for config in configs.into_iter().filter(|config| config.enabled) {
            let Some(available) = manager.cached_tools(&config.name).await else {
                continue;
            };
            definitions.extend(available.iter().filter_map(|info| {
                let name = McpToolAdapter::qualified_name_of(&config.name, &info.name);
                (config.exposure.for_tool(&info.name) == McpToolExposure::CodeMode
                    && tool_config_enabled(settings, &name))
                .then(|| {
                    crate::codemode_tool::mcp_code_mode_definition(manager, &config.name, info)
                })
            }));
        }
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        let tool = crate::codemode_tool::CodeModeInvocationTool::from_definitions(definitions);
        if tool.is_none() {
            tracing::debug!("no eligible MCP CodeMode tools are available for this session");
        }
        tool
    }

    /// Refresh only the MCP adapters already active in this session. A live
    /// tools/list response can update or remove selected tools, but never
    /// activates newly discovered names without an explicit `load_mcp` call.
    async fn synchronize_loaded_mcp_tools(&self, session_id: &str) {
        let overlay = &self.facade.coordinator.core.operations.session_tool_overlay;
        let active = overlay.list(session_id).await;
        let mut by_server: HashMap<String, Vec<(String, String, ToolHandle)>> = HashMap::new();
        for tool in active {
            let Some((server_name, operation)) = tool.mcp_tool_identity() else {
                continue;
            };
            by_server.entry(server_name.to_string()).or_default().push((
                tool.name(),
                operation.to_string(),
                tool,
            ));
        }

        if by_server.is_empty() {
            return;
        }

        let manager = &self.facade.coordinator.builtins.mcp_manager;
        let mut replacements = Vec::new();
        let mut removals = Vec::new();
        let configs = self
            .facade
            .coordinator
            .builtins
            .mcp_server_config_source
            .list()
            .await
            .unwrap_or_default();
        let configs: HashMap<_, _> = configs
            .into_iter()
            .map(|config| (config.name.clone(), config))
            .collect();
        for (server_name, active_tools) in by_server {
            let Some(config) = configs.get(&server_name).filter(|config| config.enabled) else {
                removals.extend(active_tools.into_iter().map(|(name, _, _)| name));
                continue;
            };
            let (eligible, ineligible): (Vec<_>, Vec<_>) =
                active_tools.into_iter().partition(|(_, operation, _)| {
                    config.exposure.for_tool(operation) == McpToolExposure::Deferred
                });
            removals.extend(ineligible.into_iter().map(|(name, _, _)| name));
            if eligible.is_empty() {
                continue;
            }
            if !manager.has_client(&server_name).await {
                removals.extend(eligible.into_iter().map(|(name, _, _)| name));
                continue;
            }
            let Some(available) = manager.cached_tools(&server_name).await else {
                // Keep the last known tools if the live list cannot be read.
                // `cached_tools` logs the refresh failure and the next catalog
                // snapshot will retry from the manager's cache.
                continue;
            };
            let (server_replacements, server_removals) = refreshed_mcp_adapters(
                &server_name,
                manager,
                &config.transport,
                &self.facade.coordinator.runtime,
                eligible,
                &available,
            );
            replacements.extend(server_replacements);
            removals.extend(server_removals);
        }
        overlay
            .reconcile_mcp_tools(session_id, replacements, &removals)
            .await;
    }

    /// Capture the complete lookup surface used by one ReAct tool batch.
    pub async fn tool_catalog_snapshot(&self, session_id: &str) -> ToolCatalogSnapshot {
        let mut snapshot = None;
        for _ in 0..2 {
            let before = self.facade.catalog_version_for_session(session_id).await;
            self.synchronize_loaded_mcp_tools(session_id).await;
            let after_sync = self.facade.catalog_version_for_session(session_id).await;
            let global = self
                .facade
                .coordinator
                .core
                .operations
                .installed
                .list()
                .await;
            let session = self
                .facade
                .coordinator
                .core
                .operations
                .session_tool_overlay
                .list(session_id)
                .await;
            let direct = self.direct_mcp_tools_for_session().await;
            let codemode = self.codemode_tool_for_session().await;
            let platform = self.facade.coordinator.runtime.platform().await;
            let after = self.facade.catalog_version_for_session(session_id).await;

            let mut tools = HashMap::with_capacity(global.len() + session.len() + direct.len());
            let global_defs = global.iter().map(|tool| tool.tool_def()).collect();
            let mut session_defs: Vec<_> = session.iter().map(|tool| tool.tool_def()).collect();
            session_defs.extend(direct.iter().map(|tool| tool.tool_def()));
            if let Some(tool) = codemode.as_ref() {
                session_defs.push(tool.tool_def());
            }
            for tool in global {
                tools.insert(tool.name(), tool);
            }
            for tool in session {
                tools.insert(tool.name(), tool);
            }
            for tool in direct {
                tools.insert(tool.name(), tool);
            }
            if let Some(tool) = codemode {
                tools.insert(tool.name(), tool);
            }
            let max = platform.context_limits.max_tools_per_request.max(1);
            let provider_definitions =
                select_tool_definitions_for_budget(global_defs, session_defs, max).selected_tools;
            snapshot = Some((after, tools, provider_definitions));
            if before == after_sync && after_sync == after {
                break;
            }
        }
        let (version, tools, provider_definitions) =
            snapshot.expect("tool catalog snapshot attempt must produce a view");
        ToolCatalogSnapshot::new_with_definitions(version, tools, provider_definitions)
    }

    pub async fn list_tool_definitions_for_session(&self, session_id: &str) -> Vec<ToolDef> {
        let platform = self.facade.coordinator.runtime.platform().await;
        let max = platform.context_limits.max_tools_per_request.max(1);
        let global_defs = self
            .facade
            .coordinator
            .core
            .operations
            .installed
            .list_tool_definitions()
            .await;
        let global_len = global_defs.len();
        let mut session_defs = self
            .facade
            .coordinator
            .core
            .operations
            .session_tool_overlay
            .list_tool_definitions(session_id)
            .await;
        let direct = self.direct_mcp_tools_for_session().await;
        session_defs.extend(direct.iter().map(|tool| tool.tool_def()));
        if let Some(tool) = self.codemode_tool_for_session().await {
            session_defs.push(tool.tool_def());
        }
        let total = global_len + session_defs.len();
        let selection = select_tool_definitions_for_budget(global_defs, session_defs, max);
        if !selection.omitted.is_empty() {
            tracing::warn!(
                session_id,
                total,
                max,
                global = global_len,
                selected = selection.selected_tools.len(),
                omitted = selection.omitted.len(),
                omitted_core = selection.omitted_core,
                "list_tool_definitions_for_session: omitted tools from max_tools_per_request budget; core builtins are selected before optional sources"
            );
        }
        selection.selected_tools
    }
    pub async fn list_schemas_for_session(&self, session_id: &str) -> Vec<Value> {
        self.list_tool_definitions_for_session(session_id)
            .await
            .into_iter()
            .map(|d| d.json())
            .collect()
    }
    pub async fn list_builtin_tools(&self) -> Vec<Value> {
        let catalog = self.facade.coordinator.runtime.builtin_catalog().await;
        let platform = self.facade.coordinator.runtime.platform().await;
        let tools = &catalog.tools;
        let settings = &platform.tool_settings;
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
                manifest.availability.enabled = tool_config_enabled(settings, &t.name());
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
    pub async fn list_builtin_manifests(&self) -> Vec<ToolManifest> {
        let catalog = self.facade.coordinator.runtime.builtin_catalog().await;
        let platform = self.facade.coordinator.runtime.platform().await;
        let tools = &catalog.tools;
        let settings = &platform.tool_settings;
        tools
            .iter()
            .filter(|tool| !tool.name().starts_with("skill__"))
            .map(|tool| {
                let def = tool.tool_def();
                let mut manifest = def.manifest.clone().unwrap_or_else(|| tool.tool_manifest());
                manifest.availability.enabled = tool_config_enabled(settings, &tool.name());
                manifest
            })
            .collect()
    }
    pub async fn list_enabled_builtin_tool_definitions(&self) -> Vec<ToolDef> {
        let catalog = self.facade.coordinator.runtime.builtin_catalog().await;
        let platform = self.facade.coordinator.runtime.platform().await;
        let tools = &catalog.tools;
        let settings = &platform.tool_settings;
        let mut defs: Vec<_> = tools
            .iter()
            .filter(|tool| !tool.name().starts_with("skill__"))
            .filter(|tool| tool_config_enabled(settings, &tool.name()))
            .map(|tool| tool.tool_def())
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }
}

#[cfg(test)]
mod mcp_refresh_tests {
    use super::*;
    use crate::{McpManager, McpToolInfo};
    use haven_common::config::McpServerConfig;
    use haven_common::mcp::McpServerTransport;
    use haven_common::types::NetworkPolicy;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::sync::CancellationToken;

    fn info(name: &str, description: &str, input_schema: Value) -> McpToolInfo {
        McpToolInfo {
            name: name.into(),
            description: description.into(),
            input_schema,
        }
    }

    fn active_tool(manager: &McpManager, info: McpToolInfo) -> ToolHandle {
        Arc::new(McpToolAdapter::new(manager.clone(), "calendar", info))
    }

    async fn serve_catalog_request(
        stream: TcpStream,
        catalog: Arc<tokio::sync::RwLock<Vec<Value>>>,
    ) {
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or_default() == 0 {
            return;
        }
        let mut content_length = 0usize;
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or_default() == 0 || line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or_default();
            }
        }
        let mut body = vec![0; content_length];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
        let Ok(request) = serde_json::from_slice::<Value>(&body) else {
            return;
        };
        let result = match request["method"].as_str() {
            Some("server/discover") => serde_json::json!({
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {"tools": {"listChanged": false}},
                "_meta": {
                    "io.modelcontextprotocol/serverInfo": {
                        "name": "catalog-test-server",
                        "version": "1"
                    }
                }
            }),
            Some("tools/list") => serde_json::json!({
                "tools": catalog.read().await.clone(),
                "ttlMs": 60_000,
                "cacheScope": "private"
            }),
            _ => serde_json::json!({}),
        };
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request["id"],
            "result": result
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response.len(),
            response
        );
        let _ = writer.write_all(response.as_bytes()).await;
    }

    async fn serve_catalog(
        listener: TcpListener,
        catalog: Arc<tokio::sync::RwLock<Vec<Value>>>,
        cancellation: CancellationToken,
    ) {
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { return; };
                    tokio::spawn(serve_catalog_request(stream, catalog.clone()));
                }
            }
        }
    }

    fn wire_tool(name: &str, description: &str, input_schema: Value) -> Value {
        serde_json::json!({
            "name": name,
            "description": description,
            "inputSchema": input_schema
        })
    }

    fn provider_definition<'a>(snapshot: &'a ToolCatalogSnapshot, name: &str) -> &'a ToolDef {
        snapshot
            .provider_definitions()
            .iter()
            .find(|definition| definition.name == name)
            .expect("active MCP tool should be advertised to the provider")
    }

    #[test]
    fn refresh_updates_only_active_tools_and_drops_deleted_tools() {
        let manager = McpManager::new();
        let runtime = crate::tool_runtime::ToolRuntime::new();
        let old_schema = serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"]
        });
        let old = active_tool(
            &manager,
            info("create_event", "Create an event", old_schema),
        );
        let selected_name = old.name();
        let active = vec![(selected_name.clone(), "create_event".to_string(), old)];
        let updated_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "starts_at": {"type": "string"}
            },
            "required": ["title", "starts_at"]
        });
        let available = vec![
            info(
                "create_event",
                "Create an event at a time",
                updated_schema.clone(),
            ),
            info(
                "delete_calendar",
                "Delete a calendar",
                serde_json::json!({"type": "object"}),
            ),
        ];

        let (replacements, removals) = refreshed_mcp_adapters(
            "calendar",
            &manager,
            &haven_common::mcp::McpServerTransport::Stdio,
            &runtime,
            active,
            &available,
        );

        assert!(removals.is_empty());
        assert_eq!(replacements.len(), 1);
        assert_eq!(replacements[0].name(), selected_name);
        let definition = replacements[0].tool_def();
        assert_eq!(definition.description, "Create an event at a time");
        assert_eq!(definition.input_schema, updated_schema);
        assert!(
            replacements.iter().all(|tool| tool.name()
                != McpToolAdapter::qualified_name_of("calendar", "delete_calendar")),
            "new server tools remain dormant until explicitly loaded"
        );

        let active = vec![(
            McpToolAdapter::qualified_name_of("calendar", "create_event"),
            "create_event".to_string(),
            active_tool(
                &manager,
                info(
                    "create_event",
                    "Create an event at a time",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "title": {"type": "string"},
                            "starts_at": {"type": "string"}
                        },
                        "required": ["title", "starts_at"]
                    }),
                ),
            ),
        )];
        let (replacements, removals) = refreshed_mcp_adapters(
            "calendar",
            &manager,
            &haven_common::mcp::McpServerTransport::Stdio,
            &runtime,
            active,
            &available[1..],
        );
        assert!(replacements.is_empty());
        assert_eq!(
            removals,
            vec![McpToolAdapter::qualified_name_of(
                "calendar",
                "create_event"
            )]
        );
    }

    #[tokio::test]
    async fn tool_catalog_snapshot_reconciles_refreshed_mcp_lists() {
        let facade = ToolsFacade::new();
        let manager = facade.coordinator.builtins.mcp_manager.clone();
        manager.set_network_policy(NetworkPolicy::Open).await;

        let initial_schema = serde_json::json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"]
        });
        let catalog = Arc::new(tokio::sync::RwLock::new(vec![wire_tool(
            "create_event",
            "Create an event",
            initial_schema,
        )]));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancellation = CancellationToken::new();
        let server = tokio::spawn(serve_catalog(
            listener,
            catalog.clone(),
            cancellation.clone(),
        ));
        facade
            .upsert_mcp_server_config(McpServerConfig {
                name: "calendar".into(),
                ..Default::default()
            })
            .await;
        manager
            .connect_server(&McpServerConfig {
                name: "calendar".into(),
                transport: McpServerTransport::Http,
                url: format!("http://{address}/mcp"),
                ..Default::default()
            })
            .await
            .expect("loopback MCP server should connect");

        let session_id = "ses-65dd7c6b35b99687a424c2fd8bdb08a9";
        let loaded = manager
            .cached_tools("calendar")
            .await
            .expect("initial discovery should publish tools");
        let active_name = McpToolAdapter::qualified_name_of("calendar", "create_event");
        facade
            .coordinator
            .core
            .operations
            .session_tool_overlay
            .register(session_id, active_tool(&manager, loaded[0].clone()))
            .await;
        let initial_snapshot = facade.tool_catalog_snapshot(session_id).await;
        assert_eq!(
            provider_definition(&initial_snapshot, &active_name).description,
            "Create an event"
        );
        let updated_schema = serde_json::json!({
            "type": "object",
            "properties": {
                "title": {"type": "string"},
                "starts_at": {"type": "string"}
            },
            "required": ["title", "starts_at"]
        });
        *catalog.write().await = vec![
            wire_tool(
                "create_event",
                "Create an event at a time",
                updated_schema.clone(),
            ),
            wire_tool(
                "delete_calendar",
                "Delete a calendar",
                serde_json::json!({"type": "object"}),
            ),
        ];
        manager.refresh_all_tools().await;
        let refreshed_snapshot = facade.tool_catalog_snapshot(session_id).await;
        let refreshed_definition = provider_definition(&refreshed_snapshot, &active_name);
        assert_eq!(
            refreshed_definition.description,
            "Create an event at a time"
        );
        assert_eq!(refreshed_definition.input_schema, updated_schema);
        assert!(
            refreshed_snapshot
                .provider_definitions()
                .iter()
                .all(|definition| definition.name
                    != McpToolAdapter::qualified_name_of("calendar", "delete_calendar")),
            "new MCP tools remain inactive until load_mcp selects them"
        );
        let repeated_snapshot = facade.tool_catalog_snapshot(session_id).await;
        assert_eq!(
            repeated_snapshot.catalog_version(),
            refreshed_snapshot.catalog_version(),
            "an unchanged manager list must not churn the session catalog version"
        );

        *catalog.write().await = vec![wire_tool(
            "delete_calendar",
            "Delete a calendar",
            serde_json::json!({"type": "object"}),
        )];
        manager.refresh_all_tools().await;
        let deleted_snapshot = facade.tool_catalog_snapshot(session_id).await;
        assert!(deleted_snapshot.get(&active_name).is_none());
        assert!(
            deleted_snapshot
                .provider_definitions()
                .iter()
                .all(|definition| definition.name != active_name),
            "a server-deleted MCP tool must be removed from the provider surface"
        );

        manager.shutdown_all().await;
        cancellation.cancel();
        server.await.unwrap();
    }
}
