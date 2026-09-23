use super::*;

impl ToolsManager {
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
        self.core.operations.sessions.bump_global_version();

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
        self.core.operations.sessions.bump_global_version();
        self.builtins
            .mcp_manager
            .discover_all(servers, config)
            .await;
    }

    /// Rebuild the tool catalog from the current builtin state.
    /// Called at startup and whenever MCP or Skills state changes.
    ///
    /// MCP and Skill providers are progressively loaded: their stable loader
    /// tools are always advertised, while adapters are registered per-session
    /// only after an explicit load succeeds. Enabled Skills are rebuilt into
    /// the deferred catalog from the live skills index.
    pub async fn rebuild_catalog(&self) {
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    /// Rebuild only the runtime instances affected by a live wiring change.
    /// The registration pass emits the current static operation set and live
    /// capability views. Unchanged runtime instances are retained by name so
    /// a scoped update does not tear down unrelated providers or action
    /// adapters.
    pub(super) async fn rebuild_catalog_scoped(&self, scope: CatalogRebuildScope) {
        let mut all_tools: Vec<ToolBox> = Vec::new();
        let previous_tools = self.core.all_builtin_tools.read().await.clone();
        let previous_by_name: HashMap<String, ToolBox> = previous_tools
            .into_iter()
            .map(|tool| (tool.name(), tool))
            .collect();

        // Register builtin tools, including stable control-plane loaders for
        // optional Skill/MCP sources even when those sources are unavailable.
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
        if let Err(error) = self.core.operations.installed.rebuild(active_tools).await {
            // Keep the previous atomic snapshot on a construction conflict.
            // A partial catalog is more dangerous than a stale one because it
            // can make authorization and execution disagree about a name.
            tracing::error!(error = %error, "builtin catalog rebuild rejected");
            return;
        }
        self.core.operations.deferred.replace(deferred_tools).await;
        *self.core.all_builtin_tools.write().await = all_tools;
        *self.runtime.admin_surfaces.write().await = admin_surfaces;
        self.core.operations.sessions.bump_global_version();
    }

    /// Register a tool for a specific session (per-session skill overlay).
    /// Does NOT modify the global registry.
    pub async fn register_for_session(&self, session_id: &str, tool: ToolBox) {
        self.core
            .operations
            .sessions
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
            deferred_catalog: self.core.operations.deferred.clone(),
            registry: self.core.operations.installed.clone(),
            session_catalog: self.core.operations.sessions.clone(),
            max_tools_per_request: self
                .core
                .context_limits
                .read()
                .await
                .max_tools_per_request
                .max(1),
            mcp_manager: Arc::new(self.builtins.mcp_manager.clone()),
            server_configs: self.builtins.mcp_server_configs.clone(),
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
            deferred_catalog: self.core.operations.deferred.clone(),
            registry: self.core.operations.installed.clone(),
            session_catalog: self.core.operations.sessions.clone(),
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
        self.core.operations.sessions.unregister(session_id).await;
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
        let global_count = self.core.operations.installed.list().await.len();
        let registrations = self.core.operations.sessions.registrations();
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
            .operations
            .sessions
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
            && let Some(tool) = self.core.operations.sessions.get(tid, name).await
        {
            return Some(tool);
        }
        self.core.operations.installed.get(name).await
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
        self.operation_catalog()
            .list_defs_for_session(session_id)
            .await
    }

    /// Return tool schemas for a session: global registry schemas derived
    /// from [`ToolDef`]s merged with per-session registered skill/MCP
    /// adapters. Convenience JSON view over [`Self::list_defs_for_session`].
    pub async fn list_schemas_for_session(&self, session_id: &str) -> Vec<Value> {
        self.operation_catalog()
            .list_schemas_for_session(session_id)
            .await
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
        self.builtins.mcp_manager.invalidate_catalog();
        self.core.operations.sessions.bump_global_version();
    }

    /// Remove a single MCP server config from the in-memory map.
    pub async fn remove_mcp_server_config(&self, name: &str) {
        self.builtins.mcp_server_configs.write().await.remove(name);
        self.builtins.mcp_manager.invalidate_catalog();
        self.core.operations.sessions.bump_global_version();
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
    /// core + session-loaded surface from `list_defs_for_session`.
    pub async fn list_enabled_builtin_defs(&self) -> Vec<ToolDef> {
        self.operation_catalog().list_enabled_builtin_defs().await
    }
}

/// Model-visible projection of [`OperationRegistry`].
///
/// Loading and session admission stay on `ToolsManager`. This type only
/// reads the registry and emits provider definitions or UI manifests.
pub struct OperationCatalog<'a> {
    manager: &'a ToolsManager,
}

impl ToolsManager {
    pub(crate) fn operation_catalog(&self) -> OperationCatalog<'_> {
        OperationCatalog { manager: self }
    }
}

impl OperationCatalog<'_> {
    /// Capture the complete lookup surface used by one ReAct tool batch.
    pub async fn tool_catalog_snapshot(&self, session_id: &str) -> ToolCatalogSnapshot {
        let mut snapshot = None;
        for _ in 0..2 {
            let before = self.manager.catalog_version_for_session(session_id).await;
            let global = self.manager.core.operations.installed.list().await;
            let session = self.manager.core.operations.sessions.list(session_id).await;
            let after = self.manager.catalog_version_for_session(session_id).await;

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
                .manager
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

    pub async fn list_defs_for_session(&self, session_id: &str) -> Vec<ToolDef> {
        let max = self
            .manager
            .core
            .context_limits
            .read()
            .await
            .max_tools_per_request
            .max(1);
        let global_defs = self.manager.core.operations.installed.list_defs().await;
        let global_len = global_defs.len();
        let session_defs = self
            .manager
            .core
            .operations
            .sessions
            .list_defs(session_id)
            .await;
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
    pub async fn list_schemas_for_session(&self, session_id: &str) -> Vec<Value> {
        self.list_defs_for_session(session_id)
            .await
            .into_iter()
            .map(|d| d.json())
            .collect()
    }
    pub async fn list_builtin_tools(&self) -> Vec<Value> {
        let tools = self.manager.core.all_builtin_tools.read().await;
        let settings = self.manager.core.tool_settings.read().await;
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
    pub async fn list_builtin_manifests(&self) -> Vec<ToolManifest> {
        let tools = self.manager.core.all_builtin_tools.read().await;
        let settings = self.manager.core.tool_settings.read().await;
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
    pub async fn list_enabled_builtin_defs(&self) -> Vec<ToolDef> {
        let tools = self.manager.core.all_builtin_tools.read().await;
        let settings = self.manager.core.tool_settings.read().await;
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
