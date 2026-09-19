//! Turn-bound prompt context acquisition.
//!
//! A provider owns live capability snapshots and delegates memory retrieval to
//! [`MemoryService`].  It does not render model-facing text and it does not
//! expose SQLite or embedding details to the prompt builder.

use std::sync::{Arc, RwLock};

use haven_tools::ToolsManager;

use crate::memory_service::MemoryService;

pub struct PromptContextProvider {
    tools: Arc<ToolsManager>,
    memory: Arc<MemoryService>,
    schema_cache: RwLock<Option<crate::prompt::SchemaCache>>,
}

impl PromptContextProvider {
    pub fn new(tools: Arc<ToolsManager>, memory: Arc<MemoryService>) -> Self {
        Self {
            tools,
            memory,
            schema_cache: RwLock::new(None),
        }
    }

    pub(crate) fn tools(&self) -> &Arc<ToolsManager> {
        &self.tools
    }

    pub(crate) fn memory(&self) -> &Arc<MemoryService> {
        &self.memory
    }

    pub(crate) fn cached_schema(
        &self,
        registry_version: u64,
        mcp_catalog_version: u64,
    ) -> Option<crate::prompt::SchemaCache> {
        let cache = self.schema_cache.read().ok()?;
        let cache = cache.as_ref()?;
        (cache.registry_version == registry_version
            && cache.mcp_catalog_version == mcp_catalog_version)
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
