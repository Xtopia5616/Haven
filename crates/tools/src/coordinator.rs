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
    pub(crate) lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookService,
    #[cfg(test)]
    fail_next_catalog_rebuild: std::sync::atomic::AtomicBool,
}

impl ToolRuntimeCoordinator {
    pub(crate) fn new(exec_config: SkillsExecConfig) -> Self {
        let core = tool_core::ToolCore::new();
        Self {
            core,
            runtime: tool_runtime::ToolRuntime::new(),
            builtins: tool_builtins::ToolBuiltins::new(exec_config),
            lifecycle_hooks: crate::lifecycle_hooks::LifecycleHookService::default(),
            #[cfg(test)]
            fail_next_catalog_rebuild: std::sync::atomic::AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) async fn set_router_and_media_clients_for_test(
        &self,
        router: Arc<LlmRouter>,
        stt_client: Option<Arc<dyn haven_llm::SttClient>>,
        ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
        image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
        tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
        media_config: haven_common::config::MediaConfig,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        let snapshot = haven_llm::ModelMediaSnapshot::new(0, router, media_config).with_clients(
            stt_client,
            ocr_client,
            image_gen_client,
            tts_client,
        );
        let (model_media_runtime, _publisher) = haven_llm::ModelMediaRuntime::new(snapshot);
        let current = self.runtime.platform().await;
        self.runtime
            .replace_platform(crate::tool_runtime::PlatformRuntime {
                model_media_runtime: Some(model_media_runtime),
                ..current.as_ref().clone()
            })
            .await;
        self.rebuild_model_media_catalog().await
    }

    pub(crate) async fn rebuild_model_media_catalog(
        &self,
    ) -> Result<CatalogRebuildOutcome, CatalogRebuildError> {
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots([
            "media",
            "files",
            "window",
            "web_search_api",
        ]))
        .await
    }

    pub(crate) async fn wire_startup(&self, wiring: StartupWiring) -> anyhow::Result<()> {
        self.lifecycle_hooks
            .bind_config_service(wiring.admin_context.config_service.clone())?;
        let StartupWiring {
            tool_run_store,
            schedule_store,
            tool_settings,
            default_shell,
            context_limits,
            security,
            model_media_runtime,
            input_pipeline,
            admin_context,
            messaging_runtime,
            memory_recall,
        } = wiring;
        self.runtime.bind_messaging_runtime(messaging_runtime)?;
        self.runtime.bind_memory_recall(memory_recall)?;
        self.builtins
            .mcp_manager
            .watch_stdio_policy_changes(self.core.authorization.subscribe_stdio_process_changes())
            .await;
        self.builtins.mcp_manager.set_limits(&context_limits).await;
        self.builtins
            .skill_registry
            .set_limits(&context_limits)
            .await;
        self.runtime
            .tool_run_service
            .set_limits(&context_limits)
            .await;
        self.runtime.live_outputs.set_limits(&context_limits).await;
        if let Err(error) = self.core.authorization.apply_security(&security).await {
            tracing::error!(%error, "configured execution workspace unavailable at startup; relative file access is disabled");
        }
        self.builtins
            .mcp_server_config_source
            .bind_config_service(admin_context.config_service.clone())?;
        self.builtins
            .lsp_server_config_source
            .bind_config_service(admin_context.config_service.clone())?;
        self.builtins
            .everything_config_source
            .bind_config_service(admin_context.config_service.clone())?;
        self.builtins
            .mcp_manager
            .set_network_policy(security.resource_policy.network_policy())
            .await;
        self.builtins
            .mcp_manager
            .set_stdio_process_start_authorization(
                self.core
                    .authorization
                    .capture_mcp_stdio_process_start_authorization()
                    .await
                    .map(|authorization| {
                        authorization as Arc<dyn haven_mcp::McpStdioProcessStartAuthorization>
                    }),
            )
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
            .schedule_service
            .set_store(schedule_store)
            .await;
        self.runtime
            .clipboard_history
            .set_capacity(context_limits.clipboard_history_entries);
        self.runtime
            .replace_platform(crate::tool_runtime::PlatformRuntime {
                model_media_runtime: Some(model_media_runtime),
                admin_context: Some(admin_context),
                input_pipeline,
                tool_settings,
                context_limits,
                default_shell,
                security,
            })
            .await;
        let applied = self.runtime.platform().await;
        tracing::debug!(
            network_policy = ?applied.security.resource_policy.network_policy(),
            "startup platform snapshot published"
        );
        self.rebuild_catalog_scoped(CatalogRebuildScope::All)
            .await?;
        Ok(())
    }

    pub(crate) async fn apply_security(&self, security: &SecurityConfig) -> Result<(), String> {
        let authorization_result = self.core.authorization.apply_security(security).await;
        self.builtins
            .mcp_manager
            .set_network_policy(security.resource_policy.network_policy())
            .await;
        self.builtins
            .mcp_manager
            .set_stdio_process_start_authorization(
                self.core
                    .authorization
                    .capture_mcp_stdio_process_start_authorization()
                    .await
                    .map(|authorization| {
                        authorization as Arc<dyn haven_mcp::McpStdioProcessStartAuthorization>
                    }),
            )
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
            network_policy = ?applied.security.resource_policy.network_policy(),
            "applied security configuration to the platform snapshot"
        );
        authorization_result
    }

    #[cfg(test)]
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
        self.runtime
            .clipboard_history
            .set_capacity(limits.clipboard_history_entries);
        self.builtins.mcp_manager.set_limits(&limits).await;
        self.builtins.skill_registry.set_limits(&limits).await;
        self.runtime.tool_run_service.set_limits(&limits).await;
        self.runtime.live_outputs.set_limits(&limits).await;
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
        config: &haven_common::config::McpDiscoveryConfig,
    ) {
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
        self.builtins
            .mcp_manager
            .discover_all(servers, config)
            .await;
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
        #[cfg(test)]
        if self
            .fail_next_catalog_rebuild
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(CatalogRebuildError::RegistryRejected {
                source: anyhow::anyhow!("injected catalog rebuild failure"),
            });
        }
        let mut all_tools: Vec<ToolHandle> = Vec::new();
        let previous_catalog = self.runtime.builtin_catalog().await;
        let previous_by_name: HashMap<String, ToolHandle> = previous_catalog
            .tools
            .iter()
            .cloned()
            .map(|tool| (tool.name(), tool))
            .collect();

        let platform = self.runtime.platform().await;
        let model_media_snapshot = platform
            .model_media_runtime
            .as_ref()
            .map(haven_llm::ModelMediaRuntime::snapshot);
        let capabilities = self
            .tool_capability_snapshot(&platform, model_media_snapshot.as_deref())
            .await;
        let context = self.builtins.build_context(
            &self.core,
            &self.runtime,
            platform,
            capabilities,
            model_media_snapshot,
        );
        let settings = context.settings.clone();
        let admin_operations = builtin::register_builtin_tools(&mut all_tools, context).await;

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
                admin_operations,
            })
            .await;
        self.core
            .operations
            .session_tool_overlay
            .bump_global_version();
        Ok(CatalogRebuildOutcome::Published)
    }

    pub(crate) async fn build_mcp_index(&self) -> Vec<catalog::McpServerIndexEntry> {
        let configs = match self.builtins.mcp_server_config_source.list().await {
            Ok(configs) => configs,
            Err(error) => {
                tracing::error!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "failed to read MCP configuration while building capability index"
                );
                return Vec::new();
            }
        };
        let mut entries = Vec::new();
        for server in configs.iter().filter(|server| server.enabled) {
            let tool_names: Vec<String> = self
                .builtins
                .mcp_manager
                .cached_tools(&server.name)
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|tool| server.exposure.for_tool(&tool.name).requires_native_load())
                .map(|tool| tool.name)
                .collect();
            if tool_names.is_empty() && !server.exposure.default.requires_native_load() {
                continue;
            }
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
        model_media: Option<&haven_llm::ModelMediaSnapshot>,
    ) -> runtime_capabilities::ToolCapabilitySnapshot {
        let mcp_index = self.build_mcp_index().await;
        runtime_capabilities::resolve_snapshot(platform, model_media, &mcp_index).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{McpServerConfig, RouterConfig};
    use haven_memory::{MemoryQuery, MemoryRecall};
    use haven_messaging::{
        AgentControlRequest, AgentControlResult, AgentSpawnRequest, AgentSpawnResult,
        MessagingRuntime,
    };

    struct UnusedMessagingRuntime;

    #[async_trait::async_trait]
    impl MessagingRuntime for UnusedMessagingRuntime {
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
            session_working_document: None,
            memory_facts: None,
            model_media_runtime: None,
            log_path: None,
            file_logging_enabled: false,
            log_level: None,
            tool_control: None,
        }
    }

    fn startup_wiring() -> StartupWiring {
        let (model_media_runtime, _publisher) =
            haven_llm::ModelMediaRuntime::new(haven_llm::ModelMediaSnapshot::new(
                0,
                Arc::new(LlmRouter::new(RouterConfig::default())),
                haven_common::config::MediaConfig::default(),
            ));
        StartupWiring {
            tool_run_store: None,
            schedule_store: None,
            tool_settings: HashMap::new(),
            default_shell: ShellChoice::default(),
            context_limits: ContextLimitsConfig::default(),
            security: SecurityConfig::default(),
            model_media_runtime,
            input_pipeline: None,
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
            .set_router_and_media_clients_for_test(
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
        assert!(Arc::ptr_eq(
            after
                .model_media_runtime
                .as_ref()
                .unwrap()
                .snapshot()
                .router(),
            &router
        ));
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
        assert!(after_platform.model_media_runtime.is_none());
        assert!(after_catalog.tools.is_empty());
    }

    #[tokio::test]
    async fn failed_catalog_rebuild_keeps_the_shared_generation_and_old_catalog_atomic() {
        let coordinator = ToolRuntimeCoordinator::new(SkillsExecConfig::default());
        let (runtime, publisher) =
            haven_llm::ModelMediaRuntime::new(haven_llm::ModelMediaSnapshot::new(
                1,
                Arc::new(LlmRouter::new(RouterConfig::default())),
                haven_common::config::MediaConfig::default(),
            ));
        let mut wiring = startup_wiring();
        wiring.model_media_runtime = runtime.clone();
        coordinator.wire_startup(wiring).await.unwrap();
        let old_catalog = coordinator.runtime.builtin_catalog().await;
        let old_snapshot = runtime.snapshot();

        publisher
            .publish(haven_llm::ModelMediaSnapshot::new(
                2,
                Arc::new(LlmRouter::new(RouterConfig::default())),
                haven_common::config::MediaConfig::default(),
            ))
            .unwrap();
        coordinator
            .fail_next_catalog_rebuild
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let error = coordinator.rebuild_model_media_catalog().await.unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected catalog rebuild failure")
        );
        assert_eq!(old_snapshot.config_version(), 1);
        assert_eq!(runtime.snapshot().config_version(), 2);
        assert!(Arc::ptr_eq(
            &old_catalog,
            &coordinator.runtime.builtin_catalog().await
        ));
    }

    #[tokio::test]
    async fn clipboard_history_capacity_tracks_startup_and_live_limit_updates() {
        let coordinator = ToolRuntimeCoordinator::new(SkillsExecConfig::default());
        let mut wiring = startup_wiring();
        wiring.context_limits.clipboard_history_entries = 2;
        coordinator.wire_startup(wiring).await.unwrap();

        let history = &coordinator.runtime.clipboard_history;
        history.record("oldest".into());
        history.record("middle".into());
        history.record("newest".into());
        assert_eq!(history.len(), 2);
        assert_eq!(history.recent(10)[0].content, "newest");

        let mut limits = coordinator.runtime.platform().await.context_limits.clone();
        limits.clipboard_history_entries = 1;
        coordinator
            .set_context_limits(limits.clone())
            .await
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history.recent(10)[0].content, "newest");

        limits.clipboard_history_entries = 0;
        coordinator.set_context_limits(limits).await.unwrap();
        history.record("disabled".into());
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn mcp_refresh_invalidates_catalog_without_owning_config_state() {
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
            .builtins
            .mcp_server_config_source
            .upsert_fixture(disabled.clone())
            .await;

        coordinator
            .load_mcp_from_config(std::slice::from_ref(&disabled))
            .await;

        assert_eq!(
            coordinator
                .builtins
                .mcp_server_config_source
                .get(&disabled.name)
                .await
                .unwrap(),
            Some(disabled.clone())
        );
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
                &haven_common::config::McpDiscoveryConfig::default(),
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
