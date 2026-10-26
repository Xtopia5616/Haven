//! One composition-root bundle for Agent-facing Tools capabilities.

use std::sync::Arc;

use crate::prompt_context::PromptToolPort;
use crate::react::ToolCatalogPort;
use crate::session::SessionToolPorts;
use haven_common::hooks::{LifecycleHookDecision, LifecycleHookPayload};
use tokio_util::sync::CancellationToken;

/// Narrow Agent capability for invoking configured lifecycle hooks.
#[async_trait::async_trait]
pub trait LifecycleHooksPort: Send + Sync {
    async fn run(
        &self,
        payload: LifecycleHookPayload,
        cancel: CancellationToken,
    ) -> LifecycleHookDecision;
}

pub(crate) struct NoopLifecycleHooksPort;

#[async_trait::async_trait]
impl LifecycleHooksPort for NoopLifecycleHooksPort {
    async fn run(
        &self,
        _payload: LifecycleHookPayload,
        _cancel: CancellationToken,
    ) -> LifecycleHookDecision {
        LifecycleHookDecision::Allow
    }
}

/// Explicit tool capabilities injected into Agent and SessionSupervisor.
///
/// Agent runtime owners keep only the capability each path needs. Concrete
/// Tools facade adapters are assembled by the application composition root.
#[derive(Clone)]
pub struct AgentToolPorts {
    prompt: Arc<dyn PromptToolPort>,
    catalog: Arc<dyn ToolCatalogPort>,
    session: SessionToolPorts,
    lifecycle_hooks: Arc<dyn LifecycleHooksPort>,
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
            lifecycle_hooks: Arc::new(NoopLifecycleHooksPort),
        }
    }

    pub fn with_lifecycle_hooks(mut self, lifecycle_hooks: Arc<dyn LifecycleHooksPort>) -> Self {
        self.lifecycle_hooks = lifecycle_hooks;
        self
    }

    pub fn session_ports(&self) -> SessionToolPorts {
        self.session
            .clone()
            .with_lifecycle_hooks(Arc::clone(&self.lifecycle_hooks))
    }

    pub(crate) fn prompt_port(&self) -> Arc<dyn PromptToolPort> {
        self.prompt.clone()
    }

    pub(crate) fn catalog_port(&self) -> Arc<dyn ToolCatalogPort> {
        self.catalog.clone()
    }

    pub(crate) fn lifecycle_hooks_port(&self) -> Arc<dyn LifecycleHooksPort> {
        self.lifecycle_hooks.clone()
    }
}

#[cfg(test)]
impl AgentToolPorts {
    pub(crate) fn from_tools_facade(tools: Arc<haven_tools::ToolsFacade>) -> Self {
        let prompt: Arc<dyn PromptToolPort> = tools.clone();
        let catalog: Arc<dyn ToolCatalogPort> = Arc::new(
            crate::react::ToolsFacadeToolCatalogAdapter::new(Arc::clone(&tools)),
        );
        let session = SessionToolPorts::from_tools_facade(tools);
        Self::new(prompt, catalog, session)
    }
}
