//! Read access to the process-owned MCP server configuration.
//!
//! Production reads always come from `ConfigService`; test fixtures are
//! available only in unit tests or the explicit `test-support` feature.

use haven_common::config::{ConfigService, McpServerConfig};
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
use std::sync::{Arc, RwLock as SyncRwLock};
#[cfg(any(test, feature = "test-support"))]
use tokio::sync::RwLock;

#[derive(Clone, Default)]
pub(crate) struct McpServerConfigSource {
    config_service: Arc<SyncRwLock<Option<Arc<ConfigService>>>>,
    #[cfg(any(test, feature = "test-support"))]
    test_fixtures: Arc<RwLock<HashMap<String, McpServerConfig>>>,
}

impl McpServerConfigSource {
    pub(crate) fn bind_config_service(
        &self,
        config_service: Option<Arc<ConfigService>>,
    ) -> anyhow::Result<()> {
        let mut current = self
            .config_service
            .write()
            .map_err(|_| anyhow::anyhow!("MCP configuration binding lock poisoned"))?;
        if let Some(existing) = current.as_ref()
            && config_service
                .as_ref()
                .is_some_and(|service| !Arc::ptr_eq(existing, service))
        {
            anyhow::bail!("MCP configuration source is already bound");
        }
        if config_service.is_some() {
            *current = config_service;
        }
        Ok(())
    }

    pub(crate) async fn list(&self) -> anyhow::Result<Vec<McpServerConfig>> {
        let config_service = self
            .config_service
            .read()
            .map_err(|_| anyhow::anyhow!("MCP configuration binding lock poisoned"))?
            .clone();
        if let Some(config_service) = config_service {
            return Ok(config_service.snapshot()?.config.mcp_servers);
        }

        #[cfg(any(test, feature = "test-support"))]
        {
            let mut configs = self
                .test_fixtures
                .read()
                .await
                .values()
                .cloned()
                .collect::<Vec<_>>();
            configs.sort_by(|left, right| left.name.cmp(&right.name));
            return Ok(configs);
        }

        #[cfg(not(any(test, feature = "test-support")))]
        Ok(Vec::new())
    }

    pub(crate) async fn get(&self, name: &str) -> anyhow::Result<Option<McpServerConfig>> {
        Ok(self
            .list()
            .await?
            .into_iter()
            .find(|config| config.name == name))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn upsert_fixture(&self, config: McpServerConfig) {
        self.test_fixtures
            .write()
            .await
            .insert(config.name.clone(), config);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn remove_fixture(&self, name: &str) {
        self.test_fixtures.write().await.remove(name);
    }
}
