//! Core tool contracts owned by the catalog boundary.
//!
//! `ToolCore` contains only state that answers "what tools exist and may be
//! used?". [`OperationRegistry`] owns the installed, deferred and session
//! catalogs. This object deliberately does not own MCP/Skills clients, media
//! providers, action workers, or application services.

use crate::circuit::ToolCircuitRegistry;
use crate::registry::OperationRegistry;
use crate::security::AuthorizationEngine;
use crate::tool_contract::ToolBox;
use haven_common::config::{ContextLimitsConfig, ToolConfig};
use std::collections::HashMap;
use tokio::sync::RwLock;

pub(crate) struct ToolCore {
    pub(crate) operations: OperationRegistry,
    pub(crate) authorization: AuthorizationEngine,
    pub(crate) tool_settings: RwLock<HashMap<String, ToolConfig>>,
    pub(crate) context_limits: RwLock<ContextLimitsConfig>,
    pub(crate) all_builtin_tools: RwLock<Vec<ToolBox>>,
    pub(crate) tool_circuits: ToolCircuitRegistry,
}

impl ToolCore {
    pub(crate) fn new() -> Self {
        Self {
            operations: OperationRegistry::new(),
            authorization: AuthorizationEngine::new(),
            tool_settings: RwLock::new(HashMap::new()),
            context_limits: RwLock::new(ContextLimitsConfig::default()),
            all_builtin_tools: RwLock::new(Vec::new()),
            tool_circuits: ToolCircuitRegistry::new(),
        }
    }
}
