//! Turn-bound prompt context acquisition.
//!
//! A provider owns live capability snapshots and delegates memory retrieval to
//! [`MemoryService`].  It does not render model-facing text and it does not
//! expose SQLite or embedding details to the prompt builder.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use haven_common::config::ContextLimitsConfig;
use haven_common::tools::ToolDef;
use haven_tools::{RuntimeCapabilities, SkillInfo, ToolsManager};
use serde_json::Value;

use crate::memory_service::MemoryService;

pub struct PromptContextProvider {
    tools: Arc<dyn PromptToolPort>,
    memory: Arc<MemoryService>,
    schema_cache: RwLock<Option<crate::prompt::SchemaCache>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptCatalogVersions {
    pub registry: u64,
    pub mcp: u64,
    pub skills: u64,
}

pub struct PromptCatalogContent {
    pub builtin_defs: Vec<ToolDef>,
    pub mcp_index: Vec<Value>,
    pub skills: Vec<SkillInfo>,
}

pub struct PromptRuntimeContext {
    pub context_limits: ContextLimitsConfig,
    pub default_shell: String,
    pub capabilities: RuntimeCapabilities,
    pub permission_summary: String,
    pub enabled_mcp_servers: usize,
    pub enabled_skills: usize,
}

/// Read-only snapshot contract for the model-facing capability prompt.
#[async_trait]
pub trait PromptToolPort: Send + Sync {
    fn catalog_versions(&self) -> PromptCatalogVersions;

    async fn catalog_content(&self) -> PromptCatalogContent;

    async fn runtime_context(&self) -> PromptRuntimeContext;
}

#[async_trait]
impl PromptToolPort for ToolsManager {
    fn catalog_versions(&self) -> PromptCatalogVersions {
        let services = self.share_services();
        PromptCatalogVersions {
            registry: self.registry().version(),
            mcp: self.mcp_catalog_version(),
            skills: services.skills.catalog_version(),
        }
    }

    async fn catalog_content(&self) -> PromptCatalogContent {
        let mut builtin_defs = self.list_enabled_builtin_defs().await;
        // A small embedding may build a prompt before asynchronous builtin
        // catalog initialization has run. Preserve the eager-registry fallback.
        if builtin_defs.is_empty() {
            builtin_defs = self.registry().list_defs().await;
        }
        let mcp_index = self.build_mcp_index().await;
        let skills = self.share_services().skills.list().await;
        PromptCatalogContent {
            builtin_defs,
            mcp_index,
            skills,
        }
    }

    async fn runtime_context(&self) -> PromptRuntimeContext {
        let services = self.share_services();
        let context_limits = self.context_limits().await;
        let default_shell = self.default_shell_name().await;
        let capabilities = self.runtime_capabilities().await;
        let permission_summary = services.authorization.prompt_summary().await;
        let enabled_mcp_servers = self
            .list_mcp_server_configs()
            .await
            .into_iter()
            .filter(|server| server.enabled)
            .count();
        let enabled_skills = services
            .skills
            .list()
            .await
            .into_iter()
            .filter(|skill| skill.enabled)
            .count();
        PromptRuntimeContext {
            context_limits,
            default_shell,
            capabilities,
            permission_summary,
            enabled_mcp_servers,
            enabled_skills,
        }
    }
}

impl PromptContextProvider {
    pub fn new(tools: Arc<dyn PromptToolPort>, memory: Arc<MemoryService>) -> Self {
        Self {
            tools,
            memory,
            schema_cache: RwLock::new(None),
        }
    }

    pub(crate) fn tools(&self) -> &Arc<dyn PromptToolPort> {
        &self.tools
    }

    pub(crate) fn memory(&self) -> &Arc<MemoryService> {
        &self.memory
    }

    pub(crate) fn cached_schema(
        &self,
        registry_version: u64,
        mcp_catalog_version: u64,
        skills_catalog_version: u64,
    ) -> Option<crate::prompt::SchemaCache> {
        let cache = self.schema_cache.read().ok()?;
        let cache = cache.as_ref()?;
        (cache.registry_version == registry_version
            && cache.mcp_catalog_version == mcp_catalog_version
            && cache.skills_catalog_version == skills_catalog_version)
            .then(|| cache.clone())
    }

    pub(crate) fn replace_schema(&self, cache: crate::prompt::SchemaCache) {
        if let Ok(mut current) = self.schema_cache.write() {
            *current = Some(cache);
        }
    }

    pub(crate) fn clear_schema(&self) {
        if let Ok(mut current) = self.schema_cache.write() {
            *current = None;
        }
    }
}
