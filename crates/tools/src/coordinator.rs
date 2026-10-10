//! Composition and update ordering for the live tool runtime.
//!
//! `ToolsFacade` remains the public facade. This crate-private owner creates
//! its runtime pieces and serializes the ordering rules that publish
//! `PlatformRuntime`, update MCP discovery inputs, and rebuild provider
//! catalogs.

use super::*;

pub(crate) struct ToolRuntimeCoordinator {
    pub(crate) core: tool_core::ToolCore,
    pub(crate) runtime: tool_runtime::ToolRuntime,
    pub(crate) builtins: tool_builtins::ToolBuiltins,
}

impl ToolRuntimeCoordinator {
    pub(crate) fn new(exec_config: SkillsExecConfig) -> Self {
        Self {
            core: tool_core::ToolCore::new(),
            runtime: tool_runtime::ToolRuntime::new(),
            builtins: tool_builtins::ToolBuiltins::new(exec_config),
        }
    }

    pub(crate) async fn set_router_and_media_clients(
        &self,
        router: Arc<LlmRouter>,
        stt_client: Option<Arc<dyn haven_llm::SttClient>>,
        ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
        image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
        tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
        media_config: haven_common::config::MediaConfig,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.router = Some(router);
                next.stt_client = stt_client;
                next.ocr_client = ocr_client;
                next.image_gen_client = image_gen_client;
                next.tts_client = tts_client;
                next.media_config = media_config;
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["media", "files", "window"]))
            .await
    }

    pub(crate) async fn wire_startup(&self, wiring: StartupWiring) -> anyhow::Result<()> {
        let StartupWiring {
            tool_run_store,
            tool_settings,
            default_shell,
            context_limits,
            security,
            router,
            media_config,
            input_pipeline,
            stt_client,
            ocr_client,
            image_gen_client,
            tts_client,
            admin_context,
            messaging_runtime,
            memory_recall,
        } = wiring;
        self.runtime.bind_messaging_runtime(messaging_runtime)?;
        self.runtime.bind_memory_recall(memory_recall)?;
        self.builtins.mcp_manager.set_limits(&context_limits).await;
        self.builtins
            .skill_registry
            .set_limits(&context_limits)
            .await;
        self.runtime
            .tool_run_service
            .set_limits(&context_limits)
            .await;
        self.runtime
            .live_outputs
            .set_emit_interval(&context_limits)
            .await;
        self.core.authorization.apply_security(&security).await;
        self.builtins
            .mcp_manager
            .set_network_policy(security.network_policy)
            .await;
        self.core
            .authorization
            .set_tool_settings(tool_settings.clone())
            .await;
        self.runtime
            .tool_run_service
            .set_tool_run_store(tool_run_store)
            .await;
        self.runtime
            .replace_platform(crate::tool_runtime::PlatformRuntime {
                router: Some(router),
                admin_context: Some(admin_context),
                input_pipeline,
                tts_client,
                stt_client,
                ocr_client,
                image_gen_client,
                media_config,
                tool_settings,
                context_limits,
                default_shell,
                security,
            })
            .await;
        let applied = self.runtime.platform().await;
        tracing::debug!(
            network_policy = ?applied.security.network_policy,
            "startup platform snapshot published"
        );
        self.rebuild_catalog_scoped(CatalogRebuildScope::All)
            .await?;
        Ok(())
    }

    pub(crate) async fn apply_security(&self, security: &SecurityConfig) {
        self.core.authorization.apply_security(security).await;
        self.builtins
            .mcp_manager
            .set_network_policy(security.network_policy)
            .await;
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.security = security.clone();
                next
            })
            .await;
        let applied = self.runtime.platform().await;
        tracing::debug!(
            network_policy = ?applied.security.network_policy,
            "applied security configuration to the platform snapshot"
        );
    }

    pub(crate) async fn set_admin_context(
        &self,
        ctx: builtin::AdminContext,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.admin_context = Some(ctx);
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await
    }

    pub(crate) async fn set_tool_settings(
        &self,
        settings: HashMap<String, ToolConfig>,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        let affected = self
            .runtime
            .update_platform_with(|current| {
                let affected = current
                    .tool_settings
                    .keys()
                    .chain(settings.keys())
                    .filter(|name| current.tool_settings.get(*name) != settings.get(*name))
                    .map(|name| name.split('.').next().unwrap_or(name).to_string())
                    .collect::<HashSet<_>>();
                let mut next = current.clone();
                next.tool_settings = settings.clone();
                (next, affected)
            })
            .await;
        // Settings are not a security-policy change. Replaying `apply_security`
        // here would clear session grants.
        self.core.authorization.set_tool_settings(settings).await;
        if !affected.is_empty() {
            self.rebuild_catalog_scoped(CatalogRebuildScope::Roots(affected))
                .await
        } else {
            Ok(CatalogRebuildOutcome::Unchanged)
        }
    }

    pub(crate) async fn set_tool_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.tool_settings
                    .entry(name.to_string())
                    .or_insert_with(ToolConfig::default)
                    .enabled = enabled;
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots([name
            .split('.')
            .next()
            .unwrap_or(name)]))
            .await
    }

    pub(crate) async fn set_context_limits(
        &self,
        limits: ContextLimitsConfig,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.builtins.mcp_manager.set_limits(&limits).await;
        self.builtins.skill_registry.set_limits(&limits).await;
        self.runtime.tool_run_service.set_limits(&limits).await;
        self.runtime.live_outputs.set_emit_interval(&limits).await;
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.context_limits = limits;
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await
    }

    pub(crate) async fn set_default_shell(
        &self,
        shell: ShellChoice,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.default_shell = shell;
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["shell"]))
            .await
    }

    pub(crate) async fn load_mcp_from_config(&self, servers: &[McpServerConfig]) {
        let mut configs = self.builtins.mcp_server_configs.write().await;
        configs.clear();
        for server in servers {
            configs.insert(server.name.clone(), server.clone());
        }
        drop(configs);

        // Configuration changes alter discovery before clients connect.
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
        self.builtins.mcp_manager.load_from_config(servers).await;
    }

    pub(crate) async fn discover_all(
        &self,
        servers: &[McpServerConfig],
        config: &haven_common::McpDiscoveryConfig,
    ) {
        {
            let mut configs = self.builtins.mcp_server_configs.write().await;
            configs.clear();
            for server in servers {
                configs.insert(server.name.clone(), server.clone());
            }
        }
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
        self.builtins
            .mcp_manager
            .discover_all(servers, config)
            .await;
    }

    pub(crate) async fn upsert_mcp_server_config(&self, config: McpServerConfig) {
        self.builtins
            .mcp_server_configs
            .write()
            .await
            .insert(config.name.clone(), config);
        self.builtins.mcp_manager.invalidate_catalog();
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
    }

    pub(crate) async fn remove_mcp_server_config(&self, name: &str) {
        self.builtins.mcp_server_configs.write().await.remove(name);
        self.builtins.mcp_manager.invalidate_catalog();
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
    }

    pub(crate) async fn rebuild_catalog(
        &self,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await
    }

    pub(crate) async fn rebuild_catalog_scoped(
        &self,
        scope: CatalogRebuildScope,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        let mut all_tools: Vec<ToolHandle> = Vec::new();
        let previous_catalog = self.runtime.builtin_catalog().await;
        let previous_by_name: HashMap<String, ToolHandle> = previous_catalog
            .tools
            .iter()
            .cloned()
            .map(|tool| (tool.name(), tool))
            .collect();

        let platform = self.runtime.platform().await;
        let capabilities = self.tool_capability_snapshot(&platform).await;
        let context =
            self.builtins
                .build_context(&self.core, &self.runtime, platform, capabilities);
        let settings = context.settings.clone();
        let admin_surfaces = builtin::register_builtin_tools(&mut all_tools, context).await;

        let all_tools: Vec<ToolHandle> = all_tools
            .into_iter()
            .map(|tool| {
                if scope.affects(&tool.name()) {
                    tool
                } else {
                    previous_by_name.get(&tool.name()).cloned().unwrap_or(tool)
                }
            })
            .collect();
        let enabled_tools: Vec<ToolHandle> = all_tools
            .iter()
            .filter(|tool| tool_config_enabled(&settings, &tool.name()))
            .cloned()
            .collect();
        drop(settings);

        let (active_tools, deferred_tools): (Vec<_>, Vec<_>) = enabled_tools
            .iter()
            .cloned()
            .partition(|tool| is_core_model_tool(&tool.name()));
        if let Err(source) = self.core.operations.installed.rebuild(active_tools).await {
            // Keep the previous atomic snapshot on a construction conflict.
            // A partial catalog could make authorization and execution disagree.
            return Err(CatalogRebuildError::RegistryRejected { source });
        }
        self.core.operations.deferred.replace(deferred_tools).await;
        self.runtime
            .publish_builtin_catalog(crate::tool_runtime::BuiltinCatalog {
                tools: all_tools,
                admin_surfaces,
            })
            .await;
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
        Ok(CatalogRebuildOutcome::Published)
    }

    pub(crate) async fn build_mcp_index(&self) -> Vec<catalog::McpServerIndexEntry> {
        let configs = self.builtins.mcp_server_configs.read().await;
        let mut entries = Vec::new();
        for server in configs.values().filter(|server| server.enabled) {
            let tool_names: Vec<String> = self
                .builtins
                .mcp_manager
                .cached_tools(&server.name)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            entries.push(catalog::McpServerIndexEntry::from_raw(
                &server.name,
                tool_names,
            ));
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    pub(crate) async fn tool_capability_snapshot(
        &self,
        platform: &tool_runtime::PlatformRuntime,
    ) -> runtime_capabilities::ToolCapabilitySnapshot {
        let mcp_index = self.build_mcp_index().await;
        runtime_capabilities::resolve_snapshot(platform, &mcp_index).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{McpServerConfig, RouterConfig};
    use haven_memory::recall::{MemoryQuery, MemoryRecall};
    use haven_messaging::{
        AgentControlRequest, AgentControlResult, AgentSpawnRequest, AgentSpawnResult, Envelope,
        MessagingRuntime, SendOutcome, SessionMailbox,
    };

    struct UnusedMailbox;

    impl SessionMailbox for UnusedMailbox {
        fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
            tokio::sync::watch::channel(0).1
        }

        fn deliver(&self, _to: &str, _envelope: &Envelope) -> anyhow::Result<Option<SendOutcome>> {
            Ok(None)
        }

        fn claim(&self, _recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>> {
            Ok(None)
        }

        fn try_claim(&self, _recipient: &str) -> anyhow::Result<Option<Vec<Envelope>>> {
            Ok(None)
        }

        fn ack(&self, _recipient: &str, _ids: &[String]) -> anyhow::Result<Option<()>> {
            Ok(None)
        }

        fn last_received(&self, _name: &str) -> anyhow::Result<Option<Option<Envelope>>> {
            Ok(None)
        }

        fn find_message(&self, _name: &str, _id: &str) -> anyhow::Result<Option<Option<Envelope>>> {
            Ok(None)
        }

        fn take_matching_replies(
            &self,
            _name: &str,
            _in_reply_to: &str,
            _expected_from: &str,
        ) -> anyhow::Result<Option<Vec<Envelope>>> {
            Ok(None)
        }

        fn history(&self, _name: &str, _limit: usize) -> anyhow::Result<Option<Vec<Envelope>>> {
            Ok(None)
        }
    }

    struct UnusedMessagingRuntime;

    #[async_trait::async_trait]
    impl MessagingRuntime for UnusedMessagingRuntime {
        fn mailbox(&self) -> Arc<dyn SessionMailbox> {
            Arc::new(UnusedMailbox)
        }

        async fn spawn_peer_session(
            &self,
            _request: AgentSpawnRequest,
        ) -> anyhow::Result<AgentSpawnResult> {
            anyhow::bail!("unused test runtime")
        }

        async fn control_peer_session(
            &self,
            _request: AgentControlRequest,
        ) -> anyhow::Result<AgentControlResult> {
            anyhow::bail!("unused test runtime")
        }
    }

    struct UnusedMemoryRecall;

    #[async_trait::async_trait]
    impl MemoryRecallPort for UnusedMemoryRecall {
        async fn recall(&self, _query: MemoryQuery) -> anyhow::Result<MemoryRecall> {
            Ok(MemoryRecall::default())
        }
    }

    fn empty_admin_context() -> builtin::AdminContext {
        builtin::AdminContext {
            config_service: None,
            config_apply_gate: None,
            session_store: None,
            memory_facts: None,
            router: None,
            log_path: None,
            file_logging_enabled: false,
            log_level: None,
            tool_control: None,
        }
    }

    fn startup_wiring() -> StartupWiring {
        StartupWiring {
            tool_run_store: None,
            tool_settings: HashMap::new(),
            default_shell: ShellChoice::default(),
            context_limits: ContextLimitsConfig::default(),
            security: SecurityConfig::default(),
            router: Arc::new(LlmRouter::new(RouterConfig::default())),
            media_config: haven_common::config::MediaConfig::default(),
            input_pipeline: None,
            stt_client: None,
            ocr_client: None,
            image_gen_client: None,
            tts_client: None,
            admin_context: empty_admin_context(),
            messaging_runtime: Arc::new(UnusedMessagingRuntime),
            memory_recall: Arc::new(UnusedMemoryRecall),
        }
    }

    #[tokio::test]
    async fn runtime_publish_rebuilds_catalog_from_the_published_generation() {
        let coordinator = ToolRuntimeCoordinator::new(SkillsExecConfig::default());
        let before = coordinator.runtime.platform().await;
        let router = Arc::new(LlmRouter::new(RouterConfig::default()));

        coordinator
            .set_router_and_media_clients(
                Arc::clone(&router),
                None,
                None,
                None,
                None,
                haven_common::config::MediaConfig::default(),
            )
            .await
            .unwrap();

        let after = coordinator.runtime.platform().await;
        assert!(!Arc::ptr_eq(&before, &after));
        assert!(Arc::ptr_eq(after.router.as_ref().unwrap(), &router));
        let catalog = coordinator.runtime.builtin_catalog().await;
        assert!(catalog.tools.iter().any(|tool| tool.name() == "files.read"));
    }

    #[tokio::test]
    async fn failed_startup_binding_does_not_publish_platform_or_catalog() {
        let coordinator = ToolRuntimeCoordinator::new(SkillsExecConfig::default());
        coordinator
            .runtime
            .bind_memory_recall(Arc::new(UnusedMemoryRecall))
            .expect("test fixture binds memory recall once");
        let before_platform = coordinator.runtime.platform().await;
        let before_catalog = coordinator.runtime.builtin_catalog().await;

        let error = coordinator
            .wire_startup(startup_wiring())
            .await
            .expect_err("a second memory-recall binding must fail");

        assert_eq!(error.to_string(), "memory recall port is already bound");
        let after_platform = coordinator.runtime.platform().await;
        let after_catalog = coordinator.runtime.builtin_catalog().await;
        assert!(Arc::ptr_eq(&before_platform, &after_platform));
        assert!(Arc::ptr_eq(&before_catalog, &after_catalog));
        assert!(after_platform.router.is_none());
        assert!(after_catalog.tools.is_empty());
    }

    #[tokio::test]
    async fn mcp_config_refresh_precedes_catalog_rebuild_invalidation() {
        let coordinator = ToolRuntimeCoordinator::new(SkillsExecConfig::default());
        let disabled = McpServerConfig {
            name: "disabled-test-server".into(),
            enabled: false,
            ..McpServerConfig::default()
        };
        let initial_global = coordinator
            .core
            .operations
            .session_tool_overlay
            .global_version();
        let initial_mcp = coordinator.builtins.mcp_manager.catalog_version();

        coordinator
            .load_mcp_from_config(std::slice::from_ref(&disabled))
            .await;

        let stored = coordinator.builtins.mcp_server_configs.read().await;
        assert_eq!(stored.get(&disabled.name), Some(&disabled));
        drop(stored);
        assert!(
            coordinator
                .core
                .operations
                .session_tool_overlay
                .global_version()
                > initial_global
        );
        assert!(coordinator.builtins.mcp_manager.catalog_version() > initial_mcp);
        assert!(coordinator.build_mcp_index().await.is_empty());

        let after_mcp_refresh = coordinator
            .core
            .operations
            .session_tool_overlay
            .global_version();
        let mcp_version = coordinator.builtins.mcp_manager.catalog_version();
        coordinator
            .discover_all(
                std::slice::from_ref(&disabled),
                &haven_common::McpDiscoveryConfig::default(),
            )
            .await;
        assert!(
            coordinator
                .core
                .operations
                .session_tool_overlay
                .global_version()
                > after_mcp_refresh
        );
        assert!(coordinator.builtins.mcp_manager.catalog_version() > mcp_version);
        assert!(coordinator.build_mcp_index().await.is_empty());

        let before_rebuild = coordinator
            .core
            .operations
            .session_tool_overlay
            .global_version();
        let refreshed_mcp_version = coordinator.builtins.mcp_manager.catalog_version();
        coordinator.rebuild_catalog().await.unwrap();
        assert!(
            coordinator
                .core
                .operations
                .session_tool_overlay
                .global_version()
                > before_rebuild
        );
        assert_eq!(
            coordinator.builtins.mcp_manager.catalog_version(),
            refreshed_mcp_version
        );
    }
}
