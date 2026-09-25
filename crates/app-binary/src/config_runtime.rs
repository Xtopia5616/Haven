//! Runtime application planning for versioned configuration changes.
//!
//! `haven-common::config::ConfigService` owns durable snapshots. This module
//! stays in the application composition root because only the app knows which
//! live components can be rebuilt and which settings require a restart.

use crate::app_state::AppState;
use crate::logging::log_err;
use haven_common::config::{
    AppConfig, ConfigChanged, ConfigDomain, ConfigService, ConfigSnapshot, LogLevel,
};
use haven_llm::LlmRouter;
use haven_llm::stt::build_stt_client;
use std::future::Future;
use std::sync::Arc;
use tracing_subscriber::Registry;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload;

/// One serialization boundary for configuration writes that immediately
/// rebuild and publish live runtime state.
#[derive(Default)]
pub(crate) struct RuntimeConfigCoordinator {
    apply_gate: tokio::sync::Mutex<()>,
}

// Keep the composition-root field name stable while its owner grows to include
// the router prepare/publish boundary.
pub(crate) type ConfigApplyGate = RuntimeConfigCoordinator;

impl RuntimeConfigCoordinator {
    pub(crate) async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.apply_gate.lock().await
    }

    /// Commit a model configuration mutation and apply the committed router
    /// generation while holding the shared settings/model serialization gate.
    /// The mutation stays a closure so this app-level coordinator does not
    /// depend on command selectors or model-field details.
    pub(crate) async fn edit_model_and_apply(
        &self,
        state: &AppState,
        ctx: &str,
        edit: impl FnOnce(&mut AppConfig) -> anyhow::Result<()>,
    ) -> Result<(), String> {
        self.edit_model_and_apply_with(&state.config_service, ctx, edit, |snapshot| async move {
            self.apply_router_runtime(state, &snapshot, ctx).await
        })
        .await
    }

    /// Injectable apply edge used by the production helper and its focused
    /// concurrency/no-op tests. The apply callback is invoked only for a
    /// committed change whose plan includes the LLM router.
    async fn edit_model_and_apply_with<T, Edit, Apply, ApplyFuture>(
        &self,
        config_service: &ConfigService,
        ctx: &str,
        edit: Edit,
        apply_router: Apply,
    ) -> Result<T, String>
    where
        Edit: FnOnce(&mut AppConfig) -> anyhow::Result<T>,
        Apply: FnOnce(ConfigSnapshot) -> ApplyFuture,
        ApplyFuture: Future<Output = Result<(), String>>,
    {
        let _apply_guard = self.lock().await;
        let update = config_service
            .edit(edit)
            .map_err(|error| log_err(ctx, error))?;
        let should_apply_router = update.change.as_ref().is_some_and(|change| {
            RuntimeConfigApplyPlan::from_change(change).contains(RuntimeConfigTarget::LlmRouter)
        });
        if should_apply_router {
            apply_router(update.snapshot).await?;
        }
        Ok(update.value)
    }

    /// Prepare router and media clients from the exact committed snapshot.
    /// Callers hold this coordinator's gate across config edit and apply.
    pub(crate) fn prepare_router_runtime(
        &self,
        state: &AppState,
        snapshot: &ConfigSnapshot,
        ctx: &str,
    ) -> Result<PreparedRouterRuntime, String> {
        let mcp_caller: Arc<dyn haven_llm::McpToolCaller> = Arc::new(state.services.mcp.clone());
        Self::prepare_router_from_snapshot(snapshot, Some(mcp_caller), ctx)
    }

    /// Publish a fully prepared router generation and its dependent media
    /// clients. Publishing has the same ordering as the prior command helper.
    pub(crate) async fn publish_router_runtime(
        &self,
        state: &AppState,
        prepared: PreparedRouterRuntime,
    ) {
        tracing::debug!(
            config_version = prepared.config_version,
            "publishing prepared router runtime"
        );
        state.agent.replace_router(prepared.router.clone());
        state
            .tools
            .set_router_and_media_clients(
                prepared.router,
                prepared.stt_client,
                prepared.ocr_client,
                prepared.image_gen_client,
                prepared.tts_client,
                prepared.media,
            )
            .await;
    }

    /// Model updates have no intervening live side effects, so use the complete
    /// prepare/publish operation. Settings use the two stages separately to
    /// preserve their existing security, MCP, and context update order.
    pub(crate) async fn apply_router_runtime(
        &self,
        state: &AppState,
        snapshot: &ConfigSnapshot,
        ctx: &str,
    ) -> Result<(), String> {
        Self::prepare_then_publish(
            || self.prepare_router_runtime(state, snapshot, ctx),
            |prepared| async move {
                self.publish_router_runtime(state, prepared).await;
                Ok(())
            },
        )
        .await
    }

    /// Keep preparation and publication as one fallible boundary: `apply` is
    /// never invoked when constructing the replacement fails.
    async fn prepare_then_publish<T, E, Prepare, Apply, ApplyFuture>(
        prepare: Prepare,
        apply: Apply,
    ) -> Result<(), E>
    where
        Prepare: FnOnce() -> Result<T, E>,
        Apply: FnOnce(T) -> ApplyFuture,
        ApplyFuture: Future<Output = Result<(), E>>,
    {
        let prepared = prepare()?;
        apply(prepared).await
    }

    fn prepare_router_from_snapshot(
        snapshot: &ConfigSnapshot,
        mcp_caller: Option<Arc<dyn haven_llm::McpToolCaller>>,
        ctx: &str,
    ) -> Result<PreparedRouterRuntime, String> {
        let config = &snapshot.config;
        let router = Arc::new(LlmRouter::with_default_context_window(
            config.llm.materialize(
                Some(config.context_limits.max_response_tokens),
                Some(config.context_limits.reasoning_echo_max_chars),
            ),
            config.context_limits.default_context_window,
        ));
        let media = config.media.clone();
        let providers = &config.llm.providers;
        let stt_client: Option<Arc<dyn haven_llm::SttClient>> =
            build_stt_client(mcp_caller, &media.stt, providers)
                .map_err(|error| log_err(&format!("{ctx} STT"), error))?
                .map(Arc::from);
        let ocr_client: Option<Arc<dyn haven_llm::OcrClient>> =
            haven_llm::build_ocr_client(&media.ocr)
                .map_err(|error| log_err(&format!("{ctx} OCR"), error))?
                .map(Arc::from);
        let tts_client: Option<Arc<dyn haven_llm::TtsClient>> =
            haven_llm::build_tts_client(&media.tts, providers)
                .map_err(|error| log_err(&format!("{ctx} TTS"), error))?
                .map(Arc::from);
        let image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>> =
            haven_llm::build_image_gen_client(&media.image_gen, providers)
                .map_err(|error| log_err(&format!("{ctx} image generation"), error))?
                .map(Arc::from);

        Ok(PreparedRouterRuntime {
            config_version: snapshot.version,
            router,
            stt_client,
            ocr_client,
            image_gen_client,
            tts_client,
            media,
        })
    }
}

/// Complete router and media runtime derived from one immutable snapshot.
/// Construction may fail; publication only accepts this prepared value.
pub(crate) struct PreparedRouterRuntime {
    config_version: u64,
    router: Arc<LlmRouter>,
    stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
    media: haven_common::config::MediaConfig,
}

/// Apply a configured log level to every reloadable application filter.
///
/// The first reload failure is returned to the caller. Callers that own a
/// best-effort boundary can invoke this with one handle at a time and decide
/// how to report each failure.
pub(crate) fn apply_log_level_to_handles(
    handles: &[reload::Handle<EnvFilter, Registry>],
    level: &LogLevel,
) -> anyhow::Result<()> {
    for handle in handles {
        handle.modify(|current| {
            *current = EnvFilter::new(format!("haven={}", level.as_str()));
        })?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeConfigTarget {
    InputPipeline,
    Shell,
    ContextLimits,
    LlmRouter,
    SessionRuntime,
    Mcp,
    Security,
    Logging,
    Hotkey,
    Skills,
    ToolSettings,
    MemoryRuntime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeConfigApplyPlan {
    pub(crate) version: u64,
    pub(crate) live: Vec<RuntimeConfigTarget>,
    pub(crate) restart_required: Vec<RuntimeConfigTarget>,
}

impl RuntimeConfigApplyPlan {
    pub(crate) fn from_change(change: &ConfigChanged) -> Self {
        let mut plan = Self {
            version: change.version,
            live: Vec::new(),
            restart_required: Vec::new(),
        };
        for domain in &change.domains {
            match domain {
                ConfigDomain::DefaultShell => plan.push_live(RuntimeConfigTarget::Shell),
                ConfigDomain::Llm => plan.push_live(RuntimeConfigTarget::LlmRouter),
                ConfigDomain::Hotkey => plan.push_live(RuntimeConfigTarget::Hotkey),
                ConfigDomain::Session => plan.push_live(RuntimeConfigTarget::SessionRuntime),
                ConfigDomain::ContextLimits => {
                    plan.push_live(RuntimeConfigTarget::ContextLimits);
                    plan.push_live(RuntimeConfigTarget::LlmRouter);
                }
                ConfigDomain::Security => plan.push_live(RuntimeConfigTarget::Security),
                ConfigDomain::Media => {
                    plan.push_live(RuntimeConfigTarget::InputPipeline);
                    plan.push_live(RuntimeConfigTarget::LlmRouter);
                }
                ConfigDomain::McpDiscovery | ConfigDomain::McpServers => {
                    plan.push_live(RuntimeConfigTarget::Mcp)
                }
                ConfigDomain::Log => plan.push_live(RuntimeConfigTarget::Logging),
                ConfigDomain::Skills => plan.push_live(RuntimeConfigTarget::Skills),
                ConfigDomain::SkillsExec => plan.push_restart(RuntimeConfigTarget::Skills),
                ConfigDomain::Memory => plan.push_restart(RuntimeConfigTarget::MemoryRuntime),
                ConfigDomain::Notification => {
                    // Notification settings are read from the current
                    // snapshot by the notification sink; no rebuild is needed.
                }
                ConfigDomain::Tools => plan.push_live(RuntimeConfigTarget::ToolSettings),
            }
        }
        plan
    }

    pub(crate) fn contains(&self, target: RuntimeConfigTarget) -> bool {
        self.live.contains(&target) || self.restart_required.contains(&target)
    }

    fn push_live(&mut self, target: RuntimeConfigTarget) {
        if !self.live.contains(&target) && !self.restart_required.contains(&target) {
            self.live.push(target);
        }
    }

    fn push_restart(&mut self, target: RuntimeConfigTarget) {
        self.live.retain(|existing| *existing != target);
        if !self.restart_required.contains(&target) {
            self.restart_required.push(target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::{ConfigLoader, ModelConfig};
    use tracing_subscriber::reload;

    #[test]
    fn plan_deduplicates_targets_and_marks_restart_boundaries() {
        let plan = RuntimeConfigApplyPlan::from_change(&ConfigChanged {
            version: 7,
            domains: vec![
                ConfigDomain::Media,
                ConfigDomain::Llm,
                ConfigDomain::ContextLimits,
                ConfigDomain::SkillsExec,
                ConfigDomain::Skills,
                ConfigDomain::Notification,
            ],
        });

        assert_eq!(plan.version, 7);
        assert_eq!(
            plan.live,
            vec![
                RuntimeConfigTarget::InputPipeline,
                RuntimeConfigTarget::LlmRouter,
                RuntimeConfigTarget::ContextLimits
            ]
        );
        assert_eq!(plan.restart_required, vec![RuntimeConfigTarget::Skills]);
        assert!(plan.contains(RuntimeConfigTarget::LlmRouter));
        assert!(plan.contains(RuntimeConfigTarget::Skills));
    }

    #[test]
    fn context_limits_alone_refresh_context_consumers_and_router_live() {
        let plan = RuntimeConfigApplyPlan::from_change(&ConfigChanged {
            version: 8,
            domains: vec![ConfigDomain::ContextLimits],
        });

        assert_eq!(plan.version, 8);
        assert_eq!(
            plan.live,
            vec![
                RuntimeConfigTarget::ContextLimits,
                RuntimeConfigTarget::LlmRouter,
            ]
        );
        assert!(plan.restart_required.is_empty());
    }

    #[test]
    fn applies_log_level_to_every_reload_handle() {
        let (_layer_one, handle_one): (
            reload::Layer<EnvFilter, Registry>,
            reload::Handle<EnvFilter, Registry>,
        ) = reload::Layer::new(EnvFilter::new("haven=off"));
        let (_layer_two, handle_two): (
            reload::Layer<EnvFilter, Registry>,
            reload::Handle<EnvFilter, Registry>,
        ) = reload::Layer::new(EnvFilter::new("haven=error"));
        let handles = vec![handle_one, handle_two];
        let level = LogLevel::Debug;

        apply_log_level_to_handles(&handles, &level).unwrap();

        let expected = EnvFilter::new(format!("haven={}", level.as_str())).to_string();
        for handle in &handles {
            assert_eq!(
                handle.with_current(|filter| filter.to_string()).unwrap(),
                expected
            );
        }
    }

    #[tokio::test]
    async fn settings_and_model_apply_operations_do_not_overlap() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let order = Arc::new(Mutex::new(Vec::new()));

        let operation = |name: &'static str| {
            let coordinator = coordinator.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let order = order.clone();
            async move {
                let _guard = coordinator.lock().await;
                let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now_active, Ordering::SeqCst);
                order.lock().unwrap().push(format!("{name}:start"));
                tokio::task::yield_now().await;
                order.lock().unwrap().push(format!("{name}:finish"));
                active.fetch_sub(1, Ordering::SeqCst);
            }
        };

        tokio::join!(operation("settings"), operation("model"));

        let order = order.lock().unwrap();
        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(order.len(), 4);
        assert_eq!(
            &order[0][..order[0].find(':').unwrap()],
            &order[1][..order[1].find(':').unwrap()]
        );
        assert_eq!(
            &order[2][..order[2].find(':').unwrap()],
            &order[3][..order[3].find(':').unwrap()]
        );
        assert_ne!(
            &order[0][..order[0].find(':').unwrap()],
            &order[2][..order[2].find(':').unwrap()]
        );
        assert!(order[0].ends_with(":start"));
        assert!(order[1].ends_with(":finish"));
        assert!(order[2].ends_with(":start"));
        assert!(order[3].ends_with(":finish"));
    }

    #[tokio::test]
    async fn failed_prepare_does_not_call_publish() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let applied = std::sync::Arc::new(AtomicBool::new(false));
        let applied_in_closure = applied.clone();
        let result: Result<(), &str> = RuntimeConfigCoordinator::prepare_then_publish(
            || Err::<u8, _>("client preparation failed"),
            |_generation: u8| async move {
                applied_in_closure.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;

        assert_eq!(result, Err("client preparation failed"));
        assert!(!applied.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn model_edit_no_op_skips_router_rebuild() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(ConfigService::new(loader));
        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let rebuilds = Arc::new(AtomicUsize::new(0));
        let rebuilds_in_apply = rebuilds.clone();

        let value = coordinator
            .edit_model_and_apply_with(
                &service,
                "model_test",
                |_| Ok(7),
                move |_| async move {
                    rebuilds_in_apply.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .await
            .unwrap();

        assert_eq!(value, 7);
        assert_eq!(rebuilds.load(Ordering::SeqCst), 0);
        assert_eq!(service.snapshot().unwrap().version, 0);
    }

    struct ActiveModelOperation(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for ActiveModelOperation {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn two_model_operations_do_not_overlap_edit_and_apply() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let dir = tempfile::tempdir().unwrap();
        let loader = ConfigLoader::load_from(&dir.path().join("config.toml")).unwrap();
        let service = Arc::new(ConfigService::new(loader));
        let coordinator = Arc::new(RuntimeConfigCoordinator::default());
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let rebuilds = Arc::new(AtomicUsize::new(0));

        let run = |model_id: &'static str| {
            let coordinator = coordinator.clone();
            let service = service.clone();
            let active = active.clone();
            let max_active = max_active.clone();
            let rebuilds = rebuilds.clone();
            async move {
                coordinator
                    .edit_model_and_apply_with(
                        &service,
                        "model_test",
                        move |config| {
                            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                            max_active.fetch_max(current, Ordering::SeqCst);
                            config.llm.models.push(ModelConfig {
                                id: model_id.into(),
                                ..Default::default()
                            });
                            Ok(ActiveModelOperation(active))
                        },
                        move |_| async move {
                            rebuilds.fetch_add(1, Ordering::SeqCst);
                            tokio::task::yield_now().await;
                            Ok(())
                        },
                    )
                    .await
                    .unwrap();
            }
        };

        tokio::join!(run("model-one"), run("model-two"));

        assert_eq!(max_active.load(Ordering::SeqCst), 1);
        assert_eq!(rebuilds.load(Ordering::SeqCst), 2);
        assert_eq!(service.snapshot().unwrap().version, 2);
    }

    #[tokio::test]
    async fn coordinator_publishes_a_successfully_prepared_runtime() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let published = Arc::new(AtomicUsize::new(0));
        let published_in_apply = published.clone();
        let result = RuntimeConfigCoordinator::prepare_then_publish(
            || Ok::<_, &str>(23_u64),
            |version| async move {
                published_in_apply.store(version as usize, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;

        assert_eq!(result, Ok(()));
        assert_eq!(published.load(Ordering::SeqCst), 23);
    }

    #[test]
    fn router_and_media_clients_are_prepared_from_the_supplied_snapshot() {
        let mut config = haven_common::config::AppConfig::default();
        config.context_limits.default_context_window = 173_000;
        config.media.audio.max_duration_secs = 37;
        let snapshot = ConfigSnapshot {
            version: 23,
            config,
        };

        let prepared =
            RuntimeConfigCoordinator::prepare_router_from_snapshot(&snapshot, None, "test")
                .expect("default router and media clients should prepare");

        assert_eq!(prepared.config_version, snapshot.version);
        assert_eq!(prepared.media, snapshot.config.media);
        assert!(Arc::strong_count(&prepared.router) >= 1);
    }

    #[test]
    fn media_preparation_errors_and_logs_do_not_expose_config_secrets() {
        use std::io::{self, Write};
        use std::sync::Mutex;
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone)]
        struct BufferWriter(Arc<Mutex<Vec<u8>>>);

        impl Write for BufferWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        impl<'a> MakeWriter<'a> for BufferWriter {
            type Writer = Self;

            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let secret = "media-key-never-log-this";
        let mut config = haven_common::config::AppConfig::default();
        config.media.ocr.provider = "invalid-provider".into();
        config.media.ocr.api_key = secret.into();
        let snapshot = ConfigSnapshot {
            version: 24,
            config,
        };
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(BufferWriter(output.clone()))
            .finish();

        let result = tracing::subscriber::with_default(subscriber, || {
            RuntimeConfigCoordinator::prepare_router_from_snapshot(&snapshot, None, "test")
        });
        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();

        let error = result.err().expect("invalid OCR provider should fail");
        assert!(!logs.contains(secret));
        assert!(!error.contains(secret));
    }
}
