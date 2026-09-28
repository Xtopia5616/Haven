//! One composition-root bundle for Agent-facing Tools capabilities.

use std::sync::Arc;

use haven_tools::ToolsManager;

use crate::prompt_context::PromptToolPort;
use crate::react::{ToolCatalogPort, ToolsManagerToolCatalogAdapter};
use crate::session::SessionToolPorts;

/// Explicit tool capabilities injected into Agent and SessionSupervisor.
///
/// `ToolsManager` is translated into narrow ports once at the composition
/// boundary. Agent runtime owners keep only the capability each path needs.
#[derive(Clone)]
pub struct AgentToolPorts {
    prompt: Arc<dyn PromptToolPort>,
    catalog: Arc<dyn ToolCatalogPort>,
    session: SessionToolPorts,
}

impl AgentToolPorts {
    pub fn from_tools_manager(tools: Arc<ToolsManager>) -> Self {
        let prompt: Arc<dyn PromptToolPort> = tools.clone();
        let catalog: Arc<dyn ToolCatalogPort> =
            Arc::new(ToolsManagerToolCatalogAdapter::new(Arc::clone(&tools)));
        let session = SessionToolPorts::from_tools_manager(tools);
        Self {
            prompt,
            catalog,
            session,
        }
    }

    pub fn session_ports(&self) -> SessionToolPorts {
        self.session.clone()
    }

    pub(crate) fn prompt_port(&self) -> Arc<dyn PromptToolPort> {
        self.prompt.clone()
    }

    pub(crate) fn catalog_port(&self) -> Arc<dyn ToolCatalogPort> {
        self.catalog.clone()
    }
}
