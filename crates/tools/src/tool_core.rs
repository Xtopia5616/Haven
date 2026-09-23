//! Core tool contracts owned by the catalog boundary.
//!
//! `ToolCore` contains only state that answers "what tools exist and may be
//! used?". [`OperationRegistry`] owns the installed, deferred and session
//! catalogs. Settings, limits and security live on one [`crate::tool_runtime::PlatformRuntime`]
//! generation. This object deliberately does not own MCP/Skills clients, media
//! providers, action workers, or application services.

use crate::circuit::ToolCircuitRegistry;
use crate::registry::OperationRegistry;
use crate::security::AuthorizationEngine;
use std::sync::Arc;

pub(crate) struct ToolCore {
    pub(crate) operations: OperationRegistry,
    pub(crate) authorization: Arc<AuthorizationEngine>,
    pub(crate) tool_circuits: ToolCircuitRegistry,
}

impl ToolCore {
    pub(crate) fn new() -> Self {
        Self {
            operations: OperationRegistry::new(),
            authorization: Arc::new(AuthorizationEngine::new()),
            tool_circuits: ToolCircuitRegistry::new(),
        }
    }
}
