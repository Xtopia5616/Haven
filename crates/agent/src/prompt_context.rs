//! Turn-bound prompt context acquisition.
//!
//! A provider owns live capability snapshots and delegates memory retrieval to
//! [`MemoryService`].  It does not render model-facing text and it does not
//! expose SQLite or embedding details to the prompt builder.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use haven_common::tools::ToolDef;
#[cfg(test)]
use haven_tools::ToolsManager;
use haven_tools::{McpServerIndexEntry, RuntimeCapabilities, SkillInfo};

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
    pub builtin_tool_definitions: Vec<ToolDef>,
    pub mcp_index: Vec<McpServerIndexEntry>,
    pub skills: Vec<SkillInfo>,
}

pub struct PromptRuntimeContext {
    pub default_shell: String,
    pub capabilities: RuntimeCapabilities,
    pub permission_summary: String,
}

/// Read-only snapshot contract for the model-facing capability prompt.
#[async_trait]
pub trait PromptToolPort: Send + Sync {
    fn catalog_versions(&self) -> PromptCatalogVersions;

    async fn catalog_content(&self) -> PromptCatalogContent;

    async fn runtime_context(&self) -> PromptRuntimeContext;
}

#[async_trait]
#[cfg(test)]
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
        let mut builtin_tool_definitions = self.list_enabled_builtin_tool_definitions().await;
        // A small embedding may build a prompt before asynchronous builtin
        // catalog initialization has run. Preserve the eager-registry fallback.
        if builtin_tool_definitions.is_empty() {
            builtin_tool_definitions = self.registry().list_tool_definitions().await;
        }
        let mcp_index = self.build_mcp_index().await;
        let skills = self.share_services().skills.list().await;
        PromptCatalogContent {
            builtin_tool_definitions,
            mcp_index,
            skills,
        }
    }

    async fn runtime_context(&self) -> PromptRuntimeContext {
        let services = self.share_services();
        let default_shell = self.default_shell_name().await;
        let capabilities = self.runtime_capabilities().await;
        let permission_summary = services.authorization.prompt_summary().await;
        PromptRuntimeContext {
            default_shell,
            capabilities,
            permission_summary,
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
