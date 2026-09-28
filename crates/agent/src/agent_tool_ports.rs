//! One composition-root bundle for Agent-facing Tools capabilities.

use std::sync::Arc;

use crate::prompt_context::PromptToolPort;
use crate::react::ToolCatalogPort;
use crate::session::SessionToolPorts;

/// Explicit tool capabilities injected into Agent and SessionSupervisor.
///
/// Agent runtime owners keep only the capability each path needs. Concrete
/// manager adapters are assembled by the application composition root.
#[derive(Clone)]
pub struct AgentToolPorts {
    prompt: Arc<dyn PromptToolPort>,
    catalog: Arc<dyn ToolCatalogPort>,
    session: SessionToolPorts,
}

impl AgentToolPorts {
    pub fn new(
        prompt: Arc<dyn PromptToolPort>,
        catalog: Arc<dyn ToolCatalogPort>,
        session: SessionToolPorts,
    ) -> Self {
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

#[cfg(test)]
impl AgentToolPorts {
    pub(crate) fn from_tools_manager(tools: Arc<haven_tools::ToolsManager>) -> Self {
        let prompt: Arc<dyn PromptToolPort> = tools.clone();
        let catalog: Arc<dyn ToolCatalogPort> = Arc::new(
            crate::react::ToolsManagerToolCatalogAdapter::new(Arc::clone(&tools)),
        );
        let session = SessionToolPorts::from_tools_manager(tools);
        Self::new(prompt, catalog, session)
    }
}
