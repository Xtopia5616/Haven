//! Runtime boundary for tool execution and application capabilities.
//!
//! This layer owns cancellation-aware execution dependencies and typed ports
//! supplied by the composition root. It is intentionally separate from the
//! catalog (`ToolCore`) and from concrete builtin construction.

use crate::action_service::ActionService;
use crate::asset_registry::ManagedAssetRegistry;
use crate::builtin::AdminContext;
use crate::live_output::LiveOutputHub;
use crate::messaging_service::{MessagingRuntime, MessagingService};
use haven_common::config::{ContextLimitsConfig, SecurityConfig, ToolConfig};
use haven_common::types::ShellChoice;
use haven_llm::LlmRouter;
use haven_memory::recall::{MemoryQuery, MemoryRecall};
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
}

/// Typed port for applying a persisted logging level to the host subscriber.
pub trait LogLevelPort: Send + Sync {
    fn set_level(&self, level: &haven_common::config::LogLevel) -> anyhow::Result<()>;
}

pub type MemoryRecallSlot = Arc<OnceLock<Arc<dyn MemoryRecallPort>>>;

pub fn new_memory_recall_slot() -> MemoryRecallSlot {
    Arc::new(OnceLock::new())
}

/// Dependencies that can change while the application is running.
pub(crate) struct ToolRuntime {
    pub(crate) managed_assets: ManagedAssetRegistry,
    pub(crate) router: RwLock<Option<Arc<LlmRouter>>>,
    pub(crate) action_service: Arc<ActionService>,
    pub(crate) live_outputs: Arc<LiveOutputHub>,
    pub(crate) admin_context: RwLock<Option<AdminContext>>,
    pub(crate) admin_surfaces: RwLock<Option<Arc<crate::builtin::AdminSurfaces>>>,
    pub(crate) clipboard_history: Arc<crate::builtin::clipboard::ClipboardHistory>,
    pub(crate) audio_pipeline: RwLock<Option<Arc<haven_input::InputPipeline>>>,
    pub(crate) tts_client: RwLock<Option<Arc<dyn haven_llm::TtsClient>>>,
    pub(crate) stt_client: RwLock<Option<Arc<dyn haven_llm::SttClient>>>,
    pub(crate) ocr_client: RwLock<Option<Arc<dyn haven_llm::OcrClient>>>,
    pub(crate) image_gen_client: RwLock<Option<Arc<dyn haven_llm::ImageGenClient>>>,
    pub(crate) media_config: RwLock<haven_common::config::MediaConfig>,
    pub(crate) messaging_service: Arc<MessagingService>,
    pub(crate) memory_recall: MemoryRecallSlot,
}

impl ToolRuntime {
    pub(crate) fn new() -> Self {
        Self {
            managed_assets: ManagedAssetRegistry::default(),
            router: RwLock::new(None),
            action_service: Arc::new(ActionService::new()),
            live_outputs: Arc::new(LiveOutputHub::new()),
            admin_context: RwLock::new(None),
            admin_surfaces: RwLock::new(None),
            clipboard_history: Arc::new(crate::builtin::clipboard::ClipboardHistory::new(50)),
            audio_pipeline: RwLock::new(None),
            tts_client: RwLock::new(None),
            stt_client: RwLock::new(None),
            ocr_client: RwLock::new(None),
            image_gen_client: RwLock::new(None),
            media_config: RwLock::new(haven_common::config::MediaConfig::default()),
            messaging_service: Arc::new(MessagingService::default_root()),
            memory_recall: new_memory_recall_slot(),
        }
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
}

/// Live capabilities shared by prompt assembly and builtin registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCapabilities {
    pub vision: bool,
    pub transcription: bool,
    pub recording: bool,
    pub tts: bool,
    pub web_search: String,
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
}
