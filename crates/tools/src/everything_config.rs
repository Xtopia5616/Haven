//! Local-only configuration access for the optional Everything CLI backend.

use haven_common::config::{ConfigService, EverythingConfig};
use std::sync::{Arc, RwLock as SyncRwLock};

#[derive(Clone, Default)]
pub(crate) struct EverythingConfigSource {
    config_service: Arc<SyncRwLock<Option<Arc<ConfigService>>>>,
}

impl EverythingConfigSource {
    pub(crate) fn bind_config_service(
        &self,
        config_service: Option<Arc<ConfigService>>,
    ) -> anyhow::Result<()> {
        let mut current = self
            .config_service
            .write()
            .map_err(|_| anyhow::anyhow!("Everything configuration binding lock poisoned"))?;
        if let Some(existing) = current.as_ref()
            && config_service
                .as_ref()
                .is_some_and(|service| !Arc::ptr_eq(existing, service))
        {
            anyhow::bail!("Everything configuration source is already bound");
        }
        if config_service.is_some() {
            *current = config_service;
        }
        Ok(())
    }

    pub(crate) fn get(&self) -> anyhow::Result<EverythingConfig> {
        let config_service = self
            .config_service
            .read()
            .map_err(|_| anyhow::anyhow!("Everything configuration binding lock poisoned"))?
            .clone();
        match config_service {
            Some(service) => Ok(service.snapshot()?.config.everything),
            None => Ok(EverythingConfig::default()),
        }
    }
}
