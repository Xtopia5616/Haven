//! Read access to user-configured local language servers.
//!
//! Production configuration is read from the process-owned `ConfigService`.
//! Test fixtures are available only in unit tests or `test-support` builds.

use haven_common::config::{ConfigService, LspServerConfig};
use std::sync::{Arc, RwLock as SyncRwLock};

#[derive(Clone, Default)]
pub(crate) struct LspServerConfigSource {
    config_service: Arc<SyncRwLock<Option<Arc<ConfigService>>>>,
}

impl LspServerConfigSource {
    pub(crate) fn bind_config_service(
        &self,
        config_service: Option<Arc<ConfigService>>,
    ) -> anyhow::Result<()> {
        let mut current = self
            .config_service
            .write()
            .map_err(|_| anyhow::anyhow!("LSP configuration binding lock poisoned"))?;
        if let Some(existing) = current.as_ref()
            && config_service
                .as_ref()
                .is_some_and(|service| !Arc::ptr_eq(existing, service))
        {
            anyhow::bail!("LSP configuration source is already bound");
        }
        if config_service.is_some() {
            *current = config_service;
        }
        Ok(())
    }

    pub(crate) fn list(&self) -> anyhow::Result<Vec<LspServerConfig>> {
        let config_service = self
            .config_service
            .read()
            .map_err(|_| anyhow::anyhow!("LSP configuration binding lock poisoned"))?
            .clone();
        if let Some(config_service) = config_service {
            return Ok(config_service.snapshot()?.config.lsp_servers);
        }
        Ok(Vec::new())
    }
}
