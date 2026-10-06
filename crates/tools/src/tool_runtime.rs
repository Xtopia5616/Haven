//! Runtime boundary for tool execution and application capabilities.
//!
//! This layer owns cancellation-aware execution dependencies and typed ports
//! supplied by the composition root. It is intentionally separate from the
//! catalog (`ToolCore`) and from concrete builtin construction.

use crate::asset_registry::ManagedAssetRegistry;
use crate::builtin::AdminContext;
use crate::live_output::LiveOutputHub;
use crate::tool_run_service::ToolRunService;
use haven_common::config::{ContextLimitsConfig, SecurityConfig, ToolConfig};
use haven_common::types::ShellChoice;
use haven_llm::LlmRouter;
use haven_memory::ToolRunStore;
use haven_memory::recall::{MemoryQuery, MemoryRecall};
use haven_messaging::{MessagingRuntime, MessagingService};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::sync::RwLock;

/// Typed memory capability consumed by `memory.recall`.
///
/// The port avoids a closure-shaped service locator. The implementation is
/// bound once by the composition root and remains stable for every catalog
/// generation.
#[async_trait::async_trait]
pub trait MemoryRecallPort: Send + Sync {
    async fn recall(&self, query: MemoryQuery) -> anyhow::Result<MemoryRecall>;
}

/// Typed port for applying a persisted tool toggle to the live catalog.
#[async_trait::async_trait]
pub trait ToolControlPort: Send + Sync {
    async fn set_tool_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()>;

    /// Rebuild the live builtin/deferred catalog after a Skill or other
    /// capability mutation performed through an admin surface.
    async fn rebuild_catalog(&self) -> anyhow::Result<()>;
}

/// Typed port for applying a persisted logging level to the host subscriber.
pub trait LogLevelPort: Send + Sync {
    fn set_level(&self, level: &haven_common::config::LogLevel) -> anyhow::Result<()>;
}

pub type MemoryRecallSlot = Arc<OnceLock<Arc<dyn MemoryRecallPort>>>;

pub fn new_memory_recall_slot() -> MemoryRecallSlot {
    Arc::new(OnceLock::new())
}

/// Immutable model, media and admin inputs for one catalog generation.
///
/// Hot updates replace this value as a whole. Readers keep the `Arc` they
/// already loaded, so a rebuild cannot observe a router from one generation
/// and a speech client from another.
#[derive(Clone, Default)]
pub(crate) struct PlatformRuntime {
    pub(crate) router: Option<Arc<LlmRouter>>,
    pub(crate) admin_context: Option<AdminContext>,
    pub(crate) audio_pipeline: Option<Arc<haven_input::InputPipeline>>,
    pub(crate) tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
    pub(crate) stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    pub(crate) ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    pub(crate) image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    pub(crate) media_config: haven_common::config::MediaConfig,
    /// Enabled flags, timeouts and per-tool caps for this generation.
    pub(crate) tool_settings: HashMap<String, ToolConfig>,
    pub(crate) context_limits: ContextLimitsConfig,
    pub(crate) default_shell: ShellChoice,
    /// Generation record applied to `AuthorizationEngine` and MCP network
    /// policy. Live decisions still go through the engine; storing the config
    /// here keeps it from tearing away from settings during a snapshot swap.
    pub(crate) security: SecurityConfig,
}

/// Builtin tools and the admin surfaces produced by one successful rebuild.
/// The pair is published together so a rejected rebuild cannot expose new
/// tools without the surfaces that own them, or the reverse.
pub(crate) struct BuiltinCatalog {
    pub(crate) tools: Vec<crate::tool_contract::ToolHandle>,
    pub(crate) admin_surfaces: Option<Arc<crate::builtin::AdminSurfaces>>,
}

impl BuiltinCatalog {
    pub(crate) fn empty() -> Self {
        Self {
            tools: Vec::new(),
            admin_surfaces: None,
        }
    }
}

/// Process services plus the swappable platform snapshot.
///
/// ToolRun, asset, messaging and memory ports are created with the process.
/// They are not optional bind slots. Platform clients change only by
/// replacing [`PlatformRuntime`].
pub(crate) struct ToolRuntime {
    pub(crate) managed_assets: ManagedAssetRegistry,
    platform: RwLock<Arc<PlatformRuntime>>,
    pub(crate) tool_run_service: Arc<ToolRunService>,
    pub(crate) live_outputs: Arc<LiveOutputHub>,
    /// Swapped only after a catalog rebuild is accepted.
    builtin_catalog: RwLock<Arc<BuiltinCatalog>>,
    pub(crate) clipboard_history: Arc<crate::builtin::clipboard::ClipboardHistory>,
    pub(crate) messaging_service: Arc<MessagingService>,
    pub(crate) memory_recall: MemoryRecallSlot,
}

impl ToolRuntime {
    pub(crate) fn new() -> Self {
        let tool_run_service = Arc::new(ToolRunService::new());
        let live_outputs = Arc::new(LiveOutputHub::with_tail_factory(
            tool_run_service.output_tail_factory(),
        ));
        Self {
            managed_assets: ManagedAssetRegistry::default(),
            platform: RwLock::new(Arc::new(PlatformRuntime::default())),
            tool_run_service,
            live_outputs,
            builtin_catalog: RwLock::new(Arc::new(BuiltinCatalog::empty())),
            clipboard_history: Arc::new(crate::builtin::clipboard::ClipboardHistory::new(50)),
            messaging_service: Arc::new(MessagingService::default_root()),
            memory_recall: new_memory_recall_slot(),
        }
    }

    pub(crate) async fn platform(&self) -> Arc<PlatformRuntime> {
        self.platform.read().await.clone()
    }

    /// Replace every platform client together.
    pub(crate) async fn replace_platform(&self, platform: PlatformRuntime) {
        *self.platform.write().await = Arc::new(platform);
    }

    /// Build the next snapshot from the current one and publish it atomically.
    pub(crate) async fn update_platform(
        &self,
        update: impl FnOnce(&PlatformRuntime) -> PlatformRuntime,
    ) {
        self.update_platform_with(|current| (update(current), ()))
            .await;
    }

    /// Apply a platform update and return a value computed from the same
    /// pre-update snapshot. Settings mutations use this so a concurrent
    /// replacement cannot drop keys that were read outside the write lock.
    pub(crate) async fn update_platform_with<T>(
        &self,
        update: impl FnOnce(&PlatformRuntime) -> (PlatformRuntime, T),
    ) -> T {
        let mut slot = self.platform.write().await;
        let (next, value) = update(slot.as_ref());
        *slot = Arc::new(next);
        value
    }

    pub(crate) async fn builtin_catalog(&self) -> Arc<BuiltinCatalog> {
        self.builtin_catalog.read().await.clone()
    }

    pub(crate) async fn publish_builtin_catalog(&self, catalog: BuiltinCatalog) {
        *self.builtin_catalog.write().await = Arc::new(catalog);
    }

    pub(crate) fn bind_memory_recall(
        &self,
        recall: Arc<dyn MemoryRecallPort>,
    ) -> anyhow::Result<()> {
        self.memory_recall
            .set(recall)
            .map_err(|_| anyhow::anyhow!("memory recall port is already bound"))
    }

    pub(crate) fn bind_messaging_runtime(
        &self,
        runtime: Arc<dyn MessagingRuntime>,
    ) -> anyhow::Result<()> {
        self.messaging_service.bind_runtime(runtime)
    }
}

/// All application-provided values needed for the first builtin catalog.
/// This is a composition value, not a mutable service registry.
pub struct StartupWiring {
    /// Durable ToolRun persistence constructed by the application composition root.
    pub tool_run_store: Option<ToolRunStore>,
    pub tool_settings: std::collections::HashMap<String, ToolConfig>,
    pub default_shell: ShellChoice,
    pub context_limits: ContextLimitsConfig,
    pub security: SecurityConfig,
    pub router: Arc<LlmRouter>,
    pub media_config: haven_common::config::MediaConfig,
    pub audio_pipeline: Option<Arc<haven_input::InputPipeline>>,
    pub stt_client: Option<Arc<dyn haven_llm::SttClient>>,
    pub ocr_client: Option<Arc<dyn haven_llm::OcrClient>>,
    pub image_gen_client: Option<Arc<dyn haven_llm::ImageGenClient>>,
    pub tts_client: Option<Arc<dyn haven_llm::TtsClient>>,
    pub admin_context: AdminContext,
    /// Installed before the first catalog rebuild. `AgentLayer` is the only
    /// production implementation; there is no later public bind slot.
    pub messaging_runtime: Arc<dyn MessagingRuntime>,
    pub memory_recall: Arc<dyn MemoryRecallPort>,
}

/// The source currently providing model-visible web search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchAvailability {
    /// The configured Chat route supports provider built-in search.
    Provider,
    /// At least one discovered MCP server exposes a web-search tool.
    Mcp,
    /// Neither provider built-in search nor MCP search is available.
    Unavailable,
}

/// Live capabilities shared by prompt assembly and builtin registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub vision: bool,
    pub image_generation: bool,
    pub transcription: bool,
    pub recording: bool,
    pub tts: bool,
    pub web_search: WebSearchAvailability,
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestRecallPort;

    #[async_trait::async_trait]
    impl MemoryRecallPort for TestRecallPort {
        async fn recall(&self, _query: MemoryQuery) -> anyhow::Result<MemoryRecall> {
            Ok(MemoryRecall::default())
        }
    }

    #[test]
    fn memory_recall_port_is_bound_once() {
        let slot = new_memory_recall_slot();
        assert!(slot.set(Arc::new(TestRecallPort)).is_ok());
        assert!(slot.set(Arc::new(TestRecallPort)).is_err());
    }

    #[tokio::test]
    async fn platform_snapshot_replacement_keeps_the_previous_view() {
        let runtime = ToolRuntime::new();
        let before = runtime.platform().await;
        assert_eq!(before.media_config.stt.timeout_secs, 30);
        assert!(before.router.is_none());
        assert!(before.tts_client.is_none());
        runtime
            .update_platform(|current| {
                let mut next = current.clone();
                next.media_config.stt.timeout_secs = 42;
                next.media_config.ocr.timeout_secs = 7;
                next
            })
            .await;
        let after = runtime.platform().await;
        assert_eq!(before.media_config.stt.timeout_secs, 30);
        assert_eq!(before.media_config.ocr.timeout_secs, 20);
        assert_eq!(after.media_config.stt.timeout_secs, 42);
        assert_eq!(after.media_config.ocr.timeout_secs, 7);
        assert!(!Arc::ptr_eq(&before, &after));
    }
}
