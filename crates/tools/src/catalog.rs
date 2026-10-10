use super::*;
use crate::adapters::McpToolAdapter;

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

impl ToolsFacade {
    pub async fn load_mcp_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        self.coordinator.load_mcp_from_config(servers).await;
    }

    pub async fn discover_all(
        &self,
        servers: &[haven_common::McpServerConfig],
        config: &haven_common::McpDiscoveryConfig,
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
        let catalog = builtin::tool_catalog::ToolCatalogTool {
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
                builtin::tool_catalog::ToolCatalogParams {
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
                Arc::new(McpToolAdapter::new(
                    self.coordinator.builtins.mcp_manager.clone(),
                    server_name,
                    info,
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
                server_name,
                net_new = budget.net_new,
                max = budget.max,
                global_count = budget.global_count,
                session_count = budget.session_count,
                "register_mcp_for_session: refusing server over max_tools_per_request"
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
        if let Some(tid) = session_id
            && let Some(tool) = self
                .coordinator
                .core
                .operations
                .session_tool_overlay
                .get(tid, name)
                .await
        {
            return Some(tool);
        }
        self.coordinator.core.operations.installed.get(name).await
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
        let mut last_fingerprints: Option<Vec<haven_skills::SkillFileFingerprint>> = None;
        loop {
            let fingerprints = tokio::select! {
                _ = cancellation.cancelled() => return,
                fingerprints = skill_registry.list_skill_file_fingerprints() => fingerprints,
            };
            let changed =
                last_fingerprints.is_none() || last_fingerprints.as_ref() != Some(&fingerprints);
            if changed {
                match tokio::select! {
                    _ = cancellation.cancelled() => return,
                    result = skill_registry.refresh_from_disk() => result,
                } {
                    Ok(()) => {
                        // Commit the fingerprints only after a successful
                        // refresh: on error the old fingerprints are kept so
                        // the next poll retries instead of treating the
                        // failed change as already seen.
                        last_fingerprints = Some(fingerprints);
                        if let Err(error) = self.rebuild_catalog().await {
                            tracing::warn!(error = %error, "skills changed but tool catalog rebuild failed");
                        }
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
    /// Capture the complete lookup surface used by one ReAct tool batch.
    pub async fn tool_catalog_snapshot(&self, session_id: &str) -> ToolCatalogSnapshot {
        let mut snapshot = None;
        for _ in 0..2 {
            let before = self.facade.catalog_version_for_session(session_id).await;
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
            let after = self.facade.catalog_version_for_session(session_id).await;

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
                .facade
                .coordinator
                .runtime
                .platform()
                .await
                .context_limits
                .max_tools_per_request
                .max(1);
            let provider_definitions =
                select_tool_definitions_for_budget(global_defs, session_defs, max).selected;
            snapshot = Some((after, tools, provider_definitions));
            if before == after {
                break;
            }
        }
        let (version, tools, provider_definitions) =
            snapshot.expect("tool catalog snapshot attempt must produce a view");
        ToolCatalogSnapshot::new_with_definitions(version, tools, provider_definitions)
    }

    pub async fn list_tool_definitions_for_session(&self, session_id: &str) -> Vec<ToolDef> {
        let max = self
            .facade
            .coordinator
            .runtime
            .platform()
            .await
            .context_limits
            .max_tools_per_request
            .max(1);
        let global_defs = self
            .facade
            .coordinator
            .core
            .operations
            .installed
            .list_tool_definitions()
            .await;
        let global_len = global_defs.len();
        let session_defs = self
            .facade
            .coordinator
            .core
            .operations
            .session_tool_overlay
            .list_tool_definitions(session_id)
            .await;
        let total = global_len + session_defs.len();
        let selection = select_tool_definitions_for_budget(global_defs, session_defs, max);
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
                "list_tool_definitions_for_session: omitted tools from max_tools_per_request budget; core builtins are selected before optional sources"
            );
        }
        selection.selected
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
