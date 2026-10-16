//! Read access to the process-owned MCP server configuration.
//!
//! Production reads always come from `ConfigService`; test fixtures are
//! available only in unit tests or the explicit `test-support` feature.

use haven_common::config::{ConfigService, McpServerConfig};
#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock as SyncRwLock};
#[cfg(any(test, feature = "test-support"))]
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub(crate) struct VersionedMcpServerConfig {
    pub server: McpServerConfig,
    revision: u64,
}

impl VersionedMcpServerConfig {
    pub(crate) fn authorization_target_binding(&self) -> String {
        format!(
            "mcp-config-v1:{}:{}:{}",
            self.revision,
            self.server.name,
            self.server.transport.as_str()
        )
    }
}

#[derive(Clone, Default)]
pub(crate) struct McpServerConfigSource {
    config_service: Arc<SyncRwLock<Option<Arc<ConfigService>>>>,
    #[cfg(any(test, feature = "test-support"))]
    test_fixtures: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    #[cfg(any(test, feature = "test-support"))]
    test_revision: Arc<AtomicU64>,
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
            .get_versioned(name)
            .await?
            .map(|snapshot| snapshot.server))
    }

    /// Read one server and the authoritative config revision from a single
    /// immutable ConfigService snapshot. The revision becomes part of the
    /// authorization target binding and is rechecked before connection.
    pub(crate) async fn get_versioned(
        &self,
        name: &str,
    ) -> anyhow::Result<Option<VersionedMcpServerConfig>> {
        let config_service = self
            .config_service
            .read()
            .map_err(|_| anyhow::anyhow!("MCP configuration binding lock poisoned"))?
            .clone();
        if let Some(config_service) = config_service {
            let snapshot = config_service.snapshot()?;
            return Ok(snapshot
                .config
                .mcp_servers
                .into_iter()
                .find(|server| server.name == name)
                .map(|server| VersionedMcpServerConfig {
                    server,
                    revision: snapshot.version,
                }));
        }

        #[cfg(any(test, feature = "test-support"))]
        {
            let fixtures = self.test_fixtures.read().await;
            let revision = self.test_revision.load(Ordering::Acquire);
            return Ok(fixtures
                .get(name)
                .cloned()
                .map(|server| VersionedMcpServerConfig { server, revision }));
        }

        #[cfg(not(any(test, feature = "test-support")))]
        Ok(None)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn upsert_fixture(&self, config: McpServerConfig) {
        let mut fixtures = self.test_fixtures.write().await;
        fixtures.insert(config.name.clone(), config);
        self.test_revision.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) async fn remove_fixture(&self, name: &str) {
        let mut fixtures = self.test_fixtures.write().await;
        if fixtures.remove(name).is_some() {
            self.test_revision.fetch_add(1, Ordering::AcqRel);
        }
    }
}
