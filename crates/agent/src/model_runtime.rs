//! One publication point for Agent model consumers. Snapshots outlive swaps.
use std::sync::{Arc, RwLock};

use haven_llm::LlmRouter;

#[derive(Clone)]
pub(crate) struct ModelRuntime {
    current: Arc<RwLock<(u64, Arc<LlmRouter>)>>,
}

impl ModelRuntime {
    pub(crate) fn new(router: Arc<LlmRouter>) -> Self {
        Self {
            current: Arc::new(RwLock::new((0, router))),
        }
    }

    pub(crate) fn snapshot(&self) -> Arc<LlmRouter> {
        self.versioned_snapshot().1
    }

    /// The generation and router are captured under the same read boundary.
    pub(crate) fn versioned_snapshot(&self) -> (u64, Arc<LlmRouter>) {
        self.current
            .read()
            .expect("model runtime lock poisoned")
            .clone()
    }

    pub(crate) fn replace(&self, router: Arc<LlmRouter>) -> anyhow::Result<()> {
        let mut current = self
            .current
            .write()
            .map_err(|_| anyhow::anyhow!("router runtime setting lock is poisoned"))?;
        let revision = current
            .0
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("model runtime revision exhausted"))?;
        *current = (revision, router);
        Ok(())
    }
}
