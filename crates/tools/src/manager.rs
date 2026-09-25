use super::*;

/// Process services shared outside the execution facade.
///
/// MCP, skills and the asset registry clone as handles. Authorization,
/// actions and live output are `Arc`s. Callers keep this bundle instead of
/// asking `ToolsManager` for each service.
#[derive(Clone)]
pub struct ToolServices {
    pub mcp: McpManager,
    pub mcp_configs: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    pub skills: SkillsEngine,
    pub skill_runner: Arc<RwLock<SkillRunner>>,
    pub authorization: Arc<AuthorizationEngine>,
    pub assets: ManagedAssetRegistry,
    pub actions: Arc<ActionService>,
    pub live_outputs: Arc<LiveOutputHub>,
}

impl ToolServices {
    fn from_parts(
        core: &tool_core::ToolCore,
        runtime: &tool_runtime::ToolRuntime,
        builtins: &tool_builtins::ToolBuiltins,
    ) -> Self {
        Self {
            mcp: builtins.mcp_manager.clone(),
            mcp_configs: builtins.mcp_server_configs.clone(),
            skills: builtins.skills_engine.clone(),
            skill_runner: builtins.skill_runner.clone(),
            authorization: Arc::clone(&core.authorization),
            assets: runtime.managed_assets.clone(),
            actions: Arc::clone(&runtime.action_service),
            live_outputs: Arc::clone(&runtime.live_outputs),
        }
    }
}

/// Facade for tool execution.
///
/// Callers enter through execution and catalog projection methods. Process
/// services are handed out once via [`ToolServices`]; the manager does not
/// bind individual model or media clients. Those inputs live on one
/// `PlatformRuntime` snapshot owned by `ToolRuntime`. Installed, deferred and
/// session operations live on [`OperationRegistry`].
pub struct ToolsManager {
    pub(crate) core: tool_core::ToolCore,
    pub(crate) runtime: tool_runtime::ToolRuntime,
    pub(crate) builtins: tool_builtins::ToolBuiltins,
    services: ToolServices,
}

impl ToolsManager {
    pub fn new() -> Self {
        Self::new_with_exec_config(SkillsExecConfig::default())
    }

    pub fn new_with_exec_config(exec_config: SkillsExecConfig) -> Self {
        let core = tool_core::ToolCore::new();
        let runtime = tool_runtime::ToolRuntime::new();
        let builtins = tool_builtins::ToolBuiltins::new(exec_config);
        let services = ToolServices::from_parts(&core, &runtime, &builtins);
        Self {
            core,
            runtime,
            builtins,
            services,
        }
    }

    /// Clone of the process-service bundle captured at construction.
    pub fn share_services(&self) -> ToolServices {
        self.services.clone()
    }

    /// Create a non-owning, typed admin capability for live tool toggles.
    /// `Weak` prevents the native admin surface from forming a manager cycle.
    pub fn tool_control_port(self: &Arc<Self>) -> Arc<dyn ToolControlPort> {
        Arc::new(ToolControlHandle(Arc::downgrade(self)))
    }

    /// Core catalog view. These accessors expose domain boundaries without
    /// exposing `ToolsManager`'s composition fields.
    pub fn operations(&self) -> &OperationRegistry {
        &self.core.operations
    }

    pub fn registry(&self) -> &ToolRegistry {
        self.operations().installed()
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
        self.core.operations.sessions.global_version()
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
            .operations
            .sessions
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
        self.operation_catalog()
            .tool_catalog_snapshot(session_id)
            .await
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
            .await;
    }

    /// Apply cold-start wiring in one pass and rebuild the catalog once.
    ///
    /// Messaging and memory ports are bound before the rebuild. Limits,
    /// security and tool settings are applied to their engines, then one
    /// `PlatformRuntime` snapshot is published with every field.
    pub async fn wire_startup(&self, wiring: StartupWiring) -> anyhow::Result<()> {
        let StartupWiring {
            action_store,
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
            messaging_runtime,
            memory_recall,
        } = wiring;
        self.runtime.bind_messaging_runtime(messaging_runtime)?;
        self.runtime.bind_memory_recall(memory_recall)?;
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
            .action_service
            .set_action_store(action_store)
            .await;
        self.runtime
            .replace_platform(crate::tool_runtime::PlatformRuntime {
                router: Some(router),
                admin_context: Some(admin_context),
                audio_pipeline,
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
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
        Ok(())
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

    /// Wire the app-level context for the five native admin surfaces. Called by the
    /// desktop shell after the config loader exists; later catalog rebuilds
    /// keep the capability-scoped adapters registered. Durable action storage
    /// is injected separately through startup wiring by the composition root.
    pub async fn set_admin_context(&self, ctx: builtin::AdminContext) {
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.admin_context = Some(ctx);
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    pub async fn set_tool_settings(&self, settings: HashMap<String, ToolConfig>) {
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
                .await;
        }
    }

    /// The five native admin surfaces, when the desktop shell wired the app
    /// context. The model sees the same operations through five typed adapters.
    pub async fn admin_surfaces(&self) -> Option<Arc<builtin::AdminSurfaces>> {
        self.runtime.builtin_catalog().await.admin_surfaces.clone()
    }

    /// Flip the `enabled` flag for one builtin tool in the in-memory
    /// `tool_settings` and rebuild the catalog so the toggle takes effect on
    /// the agent's next step. The config.toml persistence is done by the
    /// caller (the admin surface's `tool_enable`/`tool_disable` operations,
    /// which call this after persisting).
    pub async fn set_tool_enabled(&self, name: &str, enabled: bool) {
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
            .await;
    }

    /// Replace the unified context limits (global tool output cap etc.) and
    /// rebuild the catalog so tools pick up the new values.
    pub async fn set_context_limits(&self, limits: ContextLimitsConfig) {
        self.builtins.mcp_manager.set_limits(&limits).await;
        self.builtins.skills_engine.set_limits(&limits).await;
        self.runtime.action_service.set_limits(&limits).await;
        self.runtime.live_outputs.set_limits(&limits).await;
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.context_limits = limits;
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::All).await;
    }

    /// Replace the default shell for the `shell` tool and rebuild the catalog
    /// so the running agent picks up the new value on its next step.
    pub async fn set_default_shell(&self, shell: ShellChoice) {
        self.runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.default_shell = shell;
                next
            })
            .await;
        self.rebuild_catalog_scoped(CatalogRebuildScope::roots(["shell"]))
            .await;
    }

    /// Snapshot the shell default used by the model-facing `shell` tool.
    pub async fn default_shell_name(&self) -> String {
        self.runtime
            .platform()
            .await
            .default_shell
            .as_str()
            .to_string()
    }

    /// Snapshot the limits that shape model-visible tool and observation
    /// budgets. Prompt assembly uses this instead of duplicating defaults.
    pub async fn context_limits(&self) -> ContextLimitsConfig {
        self.runtime.platform().await.context_limits.clone()
    }

    /// Whether the model-facing `media.speak` operation has a live TTS
    /// backend. This is intentionally separate from the media tool's schema
    /// so prompt assembly can report the same capability state.
    pub async fn tts_configured(&self) -> bool {
        let platform = self.runtime.platform().await;
        self.tool_capability_snapshot(&platform).await.media.speak
    }

    /// Whether the shared media transcription boundary currently has a live
    /// route. This is the app-facing gate for voice ingress; capture itself is
    /// owned by `haven-input` and is intentionally not consulted here.
    pub async fn transcription_available(&self) -> bool {
        let platform = self.runtime.platform().await;
        self.tool_capability_snapshot(&platform)
            .await
            .media
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
        let platform = self.runtime.platform().await;
        let router = platform.router.clone();
        let stt_client = platform.stt_client.clone();
        let capabilities = self.tool_capability_snapshot(&platform).await;
        if !capabilities.media.transcribe {
            return builtin::MediaTranscriptionResult::unavailable(
                "No speech-to-text provider is configured.",
            );
        }
        let media_config = platform.media_config.clone();
        let max_output_chars = platform.context_limits.max_observation_chars;
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
        let platform = self.runtime.platform().await;
        self.tool_capability_snapshot(&platform)
            .await
            .runtime_capabilities()
    }

    /// Construct the single capability view used by manager reads and builtin
    /// catalog assembly. Each call resolves current platform, router and MCP
    /// inputs instead of returning a cached value whose invalidation would
    /// have to coordinate their independent update clocks.
    pub(super) async fn tool_capability_snapshot(
        &self,
        platform: &crate::tool_runtime::PlatformRuntime,
    ) -> runtime_capabilities::ToolCapabilitySnapshot {
        let mcp_index = self.build_mcp_index().await;
        runtime_capabilities::resolve_snapshot(platform, &mcp_index).await
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

    async fn rebuild_catalog(&self) -> anyhow::Result<()> {
        let tools = self
            .0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("tool catalog is no longer available"))?;
        tools.rebuild_catalog().await;
        Ok(())
    }
}

impl Default for ToolsManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;
    use haven_common::config::{
        Capability, ModelEndpoint, RequestKind, RequestPolicy, RoutedModel, RouterConfig,
    };

    struct AvailableStt;

    #[async_trait::async_trait]
    impl haven_llm::SttClient for AvailableStt {
        async fn transcribe(&self, _wav_data: &[u8]) -> anyhow::Result<haven_llm::SttResult> {
            anyhow::bail!("unused test STT client")
        }
    }

    struct AvailableTts;

    #[async_trait::async_trait]
    impl haven_llm::TtsClient for AvailableTts {
        async fn synthesize(&self, _text: &str) -> anyhow::Result<Vec<u8>> {
            anyhow::bail!("unused test TTS client")
        }
    }

    fn provider_search_router() -> Arc<LlmRouter> {
        Arc::new(LlmRouter::new(RouterConfig {
            models: vec![RoutedModel {
                id: "chat".into(),
                endpoint: ModelEndpoint {
                    provider: "deepseek".into(),
                    api_style: Some("openai-responses".into()),
                    api_key: "test-key".into(),
                    web_search: Some("auto".into()),
                    ..Default::default()
                },
                capabilities: vec![Capability::Chat],
            }],
            request_policies: vec![RequestPolicy {
                request: RequestKind::Chat,
                primary: "chat".into(),
            }],
            ..Default::default()
        }))
    }

    #[tokio::test]
    async fn capability_snapshot_tracks_runtime_replacement_across_read_paths() {
        let tools = ToolsManager::new();
        let initial = tools.runtime_capabilities().await;
        assert_eq!(initial.web_search, WebSearchAvailability::Unavailable);
        assert!(!initial.transcription);
        assert!(!initial.recording);
        assert!(!tools.transcription_available().await);
        assert!(!tools.tts_configured().await);

        tools
            .set_router_and_media_clients(
                provider_search_router(),
                Some(Arc::new(AvailableStt)),
                None,
                None,
                Some(Arc::new(AvailableTts)),
                haven_common::config::MediaConfig::default(),
            )
            .await;

        let after_config_publish = tools.runtime_capabilities().await;
        assert_eq!(
            after_config_publish.web_search,
            WebSearchAvailability::Provider
        );
        assert!(after_config_publish.transcription);
        assert!(!after_config_publish.recording);
        assert!(tools.transcription_available().await);
        assert!(after_config_publish.tts);
        assert!(tools.tts_configured().await);

        let pipeline = Arc::new(haven_input::InputPipeline::new());
        tools
            .runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.audio_pipeline = Some(pipeline);
                next
            })
            .await;
        tools.rebuild_catalog().await;

        let platform = tools.runtime.platform().await;
        let snapshot = tools.tool_capability_snapshot(&platform).await;
        assert!(snapshot.media.record);
        assert!(snapshot.media.transcribe);
        assert_eq!(snapshot.web_search, WebSearchAvailability::Provider);
        let media_catalog = tools.runtime.builtin_catalog().await;
        assert!(
            media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.record")
        );
        assert!(
            media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.transcribe")
        );

        tools
            .set_router_and_media_clients(
                Arc::new(LlmRouter::new(RouterConfig::default())),
                None,
                None,
                None,
                None,
                haven_common::config::MediaConfig::default(),
            )
            .await;

        let after_runtime_replacement = tools.runtime_capabilities().await;
        assert_eq!(
            after_runtime_replacement.web_search,
            WebSearchAvailability::Unavailable
        );
        assert!(!after_runtime_replacement.transcription);
        assert!(after_runtime_replacement.recording);
        assert!(!tools.transcription_available().await);
        let transcription = tools
            .transcribe_recording(&[], CancellationToken::new())
            .await;
        assert_eq!(
            transcription.status,
            builtin::MediaTranscriptionStatus::Unavailable
        );
        assert!(!tools.tts_configured().await);
        let media_catalog = tools.runtime.builtin_catalog().await;
        assert!(
            media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.record")
        );
        assert!(
            !media_catalog
                .tools
                .iter()
                .any(|tool| tool.name() == "media.transcribe")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_capability_reads_and_platform_replacements_do_not_panic() {
        let tools = Arc::new(ToolsManager::new());
        let pipeline = Arc::new(haven_input::InputPipeline::new());
        let stt: Arc<dyn haven_llm::SttClient> = Arc::new(AvailableStt);

        let writer_tools = Arc::clone(&tools);
        let writer_pipeline = Arc::clone(&pipeline);
        let writer_stt = Arc::clone(&stt);
        let writer = tokio::spawn(async move {
            for generation in 0..32 {
                let pipeline = (generation % 2 == 0).then(|| Arc::clone(&writer_pipeline));
                let stt = (generation % 2 == 0).then(|| Arc::clone(&writer_stt));
                writer_tools
                    .runtime
                    .update_platform(|current| {
                        let mut next = current.clone();
                        next.audio_pipeline = pipeline;
                        next.stt_client = stt;
                        next
                    })
                    .await;
            }
        });

        let mut readers = Vec::new();
        for _ in 0..4 {
            let tools = Arc::clone(&tools);
            readers.push(tokio::spawn(async move {
                for _ in 0..16 {
                    let capabilities = tools.runtime_capabilities().await;
                    assert!(matches!(
                        capabilities.web_search,
                        WebSearchAvailability::Provider
                            | WebSearchAvailability::Mcp
                            | WebSearchAvailability::Unavailable
                    ));
                    let _ = tools.transcription_available().await;
                    let _ = tools.tts_configured().await;
                }
            }));
        }

        writer.await.expect("platform writer must not panic");
        for reader in readers {
            reader.await.expect("capability reader must not panic");
        }
    }
}
