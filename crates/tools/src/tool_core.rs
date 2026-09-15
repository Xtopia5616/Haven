//! Core tool contracts owned by the catalog boundary.
//!
//! `ToolCore` contains only state that answers "what tools exist and may be
//! used?". It deliberately does not own MCP/Skills clients, media providers,
//! action workers, or application services.

use crate::circuit::ToolCircuitRegistry;
use crate::registry::{DeferredToolCatalog, SessionCatalog, ToolRegistry};
use crate::security::AuthorizationEngine;
use crate::tool_contract::ToolBox;
use haven_common::config::{ContextLimitsConfig, ToolConfig};
use std::collections::HashMap;
use tokio::sync::RwLock;

pub(crate) struct ToolCore {
    pub(crate) registry: ToolRegistry,
    pub(crate) authorization: AuthorizationEngine,
    pub(crate) tool_settings: RwLock<HashMap<String, ToolConfig>>,
    pub(crate) context_limits: RwLock<ContextLimitsConfig>,
    pub(crate) all_builtin_tools: RwLock<Vec<ToolBox>>,
    pub(crate) deferred_catalog: DeferredToolCatalog,
    pub(crate) session_catalog: SessionCatalog,
    pub(crate) tool_circuits: ToolCircuitRegistry,
}

impl ToolCore {
    pub(crate) fn new() -> Self {
        Self {
            registry: ToolRegistry::new(),
            authorization: AuthorizationEngine::new(),
            tool_settings: RwLock::new(HashMap::new()),
            context_limits: RwLock::new(ContextLimitsConfig::default()),
            all_builtin_tools: RwLock::new(Vec::new()),
            deferred_catalog: DeferredToolCatalog::new(),
            session_catalog: SessionCatalog::new(),
            tool_circuits: ToolCircuitRegistry::new(),
        }
    }
}
