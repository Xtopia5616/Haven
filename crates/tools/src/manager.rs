use super::*;

/// Composition object for the tool subsystem.
///
/// The manager intentionally contains three explicit boundaries rather than
/// exposing every provider and mutable dependency as a public field:
/// `ToolCore` owns contracts/catalog/authorization, `ToolRuntime` owns
/// execution capabilities, and `ToolBuiltins` owns concrete MCP/Skills
/// providers. Application code uses the narrow accessors below.
pub struct ToolsManager {
    pub(crate) core: tool_core::ToolCore,
    pub(crate) runtime: tool_runtime::ToolRuntime,
    pub(crate) builtins: tool_builtins::ToolBuiltins,
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
