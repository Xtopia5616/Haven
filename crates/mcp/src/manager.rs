use crate::client::McpClient;
use crate::protocol::{
    McpCallOutput, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
use haven_llm::{McpToolCaller, McpToolOutcome};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// McpManager — manages multiple MCP clients
// ---------------------------------------------------------------------------

/// Diff of [`McpManager::reconcile_servers`]: what a config list implies for
/// the live clients. The caller owns the connect (with or without a health
/// monitor) and the shutdown, so startup reload and the UI refresh command
/// share one reconciliation rule.
#[derive(Debug, Default)]
pub struct McpReconcile {
    /// Enabled servers with no live client yet.
    pub to_connect_new: Vec<haven_common::McpServerConfig>,
    /// Enabled servers whose live client was spawned from a different
    /// config (command / args / env / url changed) and must be recreated.
    pub to_connect_changed: Vec<haven_common::McpServerConfig>,
    /// Live clients whose server was removed from config or disabled.
    pub to_remove: Vec<String>,
    /// Enabled servers already loaded from the identical config (kept).
    pub kept: Vec<String>,
}

pub struct McpManager {
    clients: Arc<Mutex<HashMap<String, Arc<McpClient>>>>,
    monitors: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    status_tx: tokio::sync::broadcast::Sender<McpStatusChangeEvent>,
    discovery_config: Arc<tokio::sync::RwLock<haven_common::config::McpDiscoveryConfig>>,
    limits: Arc<tokio::sync::RwLock<haven_common::config::ContextLimitsConfig>>,
    network_policy: Arc<tokio::sync::RwLock<haven_common::types::NetworkPolicy>>,
    /// Changes whenever the configured server set or a server's tools/list
    /// cache changes. Consumers use this as a cheap catalog invalidation
    /// clock; it is not a durable protocol version.
    catalog_version: Arc<AtomicU64>,
}

impl Clone for McpManager {
    fn clone(&self) -> Self {
        Self {
            clients: self.clients.clone(),
            monitors: self.monitors.clone(),
            status_tx: self.status_tx.clone(),
            discovery_config: self.discovery_config.clone(),
            limits: self.limits.clone(),
            network_policy: self.network_policy.clone(),
            catalog_version: self.catalog_version.clone(),
        }
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self::new_with_network_policy(haven_common::types::NetworkPolicy::Ask)
    }

    /// Construct a manager with an explicit transport policy. The regular
    /// constructor is fail-closed; this is useful for controlled hosts and
    /// tests that intentionally provide their own network boundary.
    pub fn new_with_network_policy(network_policy: haven_common::types::NetworkPolicy) -> Self {
        let (status_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            monitors: Arc::new(Mutex::new(HashMap::new())),
            status_tx,
            discovery_config: Arc::new(tokio::sync::RwLock::new(
                haven_common::config::McpDiscoveryConfig::default(),
            )),
            limits: Arc::new(tokio::sync::RwLock::new(
                haven_common::config::ContextLimitsConfig::default(),
            )),
            network_policy: Arc::new(tokio::sync::RwLock::new(network_policy)),
            catalog_version: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Monotonic in-process version for MCP catalog consumers.
    pub fn catalog_version(&self) -> u64 {
        self.catalog_version.load(Ordering::Relaxed)
    }

    /// Invalidate catalog consumers after a configured server changes without
    /// a corresponding live client mutation (for example, an admin operation
    /// that saves a disabled server or a failed connection).
    pub fn invalidate_catalog(&self) {
        self.bump_catalog_version();
    }

    fn bump_catalog_version(&self) {
        self.catalog_version.fetch_add(1, Ordering::Relaxed);
    }

    /// Install the tools/list_changed listener for every connection path.
    /// Startup and progressive `load_mcp` connections must invalidate the same
    /// catalog clock; otherwise a paged capability discovery can keep using a
    /// cursor created before the server changed its tools.
    fn start_catalog_listener(&self, client: Arc<McpClient>) -> tokio::task::JoinHandle<()> {
        let catalog_version = self.catalog_version.clone();
        client.start_notification_listener(move |server_name: &str| {
            tracing::info!("MCP server '{}' pushed tools/list_changed", server_name);
            catalog_version.fetch_add(1, Ordering::Relaxed);
        })
    }

    /// Replace the unified context limits (binary payload / SSE buffer caps)
    /// used when creating MCP clients from config.
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.limits.write().await = limits.clone();
    }

    /// Apply the process-wide MCP connection boundary. Any policy other than
    /// Open tears down existing clients so health monitors cannot reconnect
    /// behind a boundary whose destination validation is no longer known.
    /// Re-enabling network access does not auto-connect servers; the next
    /// discovery/refresh explicitly owns that side effect.
    pub async fn set_network_policy(&self, policy: haven_common::types::NetworkPolicy) {
        *self.network_policy.write().await = policy;
        let clients: Vec<_> = self
            .clients
            .lock()
            .await
            .iter()
            .map(|(name, client)| (name.clone(), client.clone()))
            .collect();
        for (_, client) in &clients {
            client.set_network_policy(policy).await;
        }
        if !matches!(policy, haven_common::types::NetworkPolicy::Open) {
            // Existing HTTP clients may have been created with a different
            // DNS resolution/pinning decision. Tear them down too; callers
            // must explicitly rediscover after a boundary change.
            for (name, _) in clients {
                self.remove_client(&name).await;
            }
        }
    }

    pub async fn network_policy(&self) -> haven_common::types::NetworkPolicy {
        *self.network_policy.read().await
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<McpStatusChangeEvent> {
        self.status_tx.subscribe()
    }

    pub(crate) async fn add_client(&self, client: Arc<McpClient>) {
        self.clients
            .lock()
            .await
            .insert(client.name().to_string(), client);
        self.bump_catalog_version();
    }

    pub async fn remove_client(&self, name: &str) {
        self.stop_monitor(name).await;
        let client = self.clients.lock().await.remove(name);
        if let Some(client) = client {
            if let Err(error) = client.shutdown().await {
                tracing::warn!(
                    "MCP server '{}' shutdown reported an error: {}",
                    name,
                    haven_common::error::sanitize_error_text(&error.to_string())
                );
            }
            self.bump_catalog_version();
            let _ = self.status_tx.send(McpStatusChangeEvent {
                name: name.to_string(),
                status: McpClientStatus::Disconnected,
            });
        }
    }

    async fn client(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.lock().await.get(name).cloned()
    }

    /// Whether this manager currently owns a client generation for `name`.
    /// This reports presence; use [`Self::client_status`] for connection state.
    pub async fn has_client(&self, name: &str) -> bool {
        self.clients.lock().await.contains_key(name)
    }

    /// Read a client status through the manager boundary. Missing clients are
    /// reported as disconnected, matching the public runtime state vocabulary.
    pub async fn client_status(&self, name: &str) -> McpClientStatus {
        match self.client(name).await {
            Some(client) => client.status().await,
            None => McpClientStatus::Disconnected,
        }
    }

    /// Return the public connection snapshot for one managed server.
    pub async fn client_snapshot(&self, name: &str) -> Option<McpServerSnapshot> {
        let client = self.client(name).await?;
        Some(client.snapshot().await)
    }

    /// Cached tool count. `None` means no tools/list result is available yet;
    /// `Some(0)` means discovery completed and returned an empty list.
    pub async fn cached_tool_count(&self, name: &str) -> Option<usize> {
        let client = self.client(name).await?;
        client.cached_tools_count().await
    }

    /// Read the completed tools/list cache. `None` means the client is absent
    /// or discovery has not completed; `Some(vec![])` is a known empty list.
    pub async fn cached_tools(&self, name: &str) -> Option<Vec<McpToolInfo>> {
        let client = self.client(name).await?;
        client.cached_tools().await
    }

    /// Wait for tools/list discovery without exposing the concrete client.
    pub async fn wait_for_tools(&self, name: &str, timeout: Duration) -> Option<Vec<McpToolInfo>> {
        let client = self.client(name).await?;
        Some(client.wait_for_tools(timeout).await)
    }

    /// Compare the managed client generation with the currently authorized
    /// configuration without returning the client implementation.
    pub async fn client_matches_config(
        &self,
        name: &str,
        config: &haven_common::McpServerConfig,
    ) -> bool {
        self.client(name)
            .await
            .is_some_and(|client| client.matches_config(config))
    }

    async fn install_monitor(
        &self,
        client: Arc<McpClient>,
        config: &haven_common::config::McpDiscoveryConfig,
    ) {
        let name = client.name().to_string();
        let mut monitors = self.monitors.lock().await;
        if let Some(previous) = monitors.remove(&name) {
            previous.abort();
            let _ = previous.await;
        }
        let handle = client.spawn_monitor_task(
            Duration::from_secs(config.health_interval_secs),
            Duration::from_millis(config.reconnect_initial_ms),
            Duration::from_millis(config.reconnect_max_ms),
            config.reconnect_max_retries,
            self.status_tx.clone(),
        );
        monitors.insert(name, handle);
    }

    async fn stop_monitor(&self, name: &str) {
        if let Some(monitor) = self.monitors.lock().await.remove(name) {
            monitor.abort();
            let _ = monitor.await;
        }
    }

    pub async fn list_clients(&self) -> Vec<String> {
        self.clients.lock().await.keys().cloned().collect()
    }

    /// Single reconciliation rule between a config list and the live clients.
    /// Diff-only: a server already loaded from the SAME config is kept as-is
    /// (a reload never respawns a heavy stdio server such as Ghidra), a live
    /// client whose persisted config changed is flagged for recreation, and
    /// clients whose server is gone or disabled are flagged for shutdown.
    pub async fn reconcile_servers(
        &self,
        servers: &[haven_common::McpServerConfig],
    ) -> McpReconcile {
        let mut out = McpReconcile::default();
        let enabled: HashSet<String> = servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.clone())
            .collect();
        let clients = self.clients.lock().await;
        for server in servers.iter().filter(|s| s.enabled) {
            match clients.get(&server.name) {
                Some(existing) if existing.matches_config(server) => {
                    out.kept.push(server.name.clone());
                }
                Some(_) => out.to_connect_changed.push(server.clone()),
                None => out.to_connect_new.push(server.clone()),
            }
        }
        for name in clients.keys() {
            if !enabled.contains(name) {
                out.to_remove.push(name.clone());
            }
        }
        out
    }

    /// Load MCP servers from config, create clients, and connect them.
    /// Diff-only (see [`Self::reconcile_servers`]): servers that already have
    /// a live client spawned from the SAME config are skipped, a live client
    /// whose persisted config changed is torn down and reconnected, and live
    /// clients whose server was removed or disabled are shut down.
    /// Does NOT start health monitors — call `start_monitors` separately
    /// or use `discover_all` for the combined operation.
    pub async fn load_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        self.bump_catalog_version();
        let t0 = std::time::Instant::now();
        let reconcile = self.reconcile_servers(servers).await;

        for name in reconcile.to_remove {
            tracing::info!("MCP server '{}' removed/disabled, shutting down", name);
            self.remove_client(&name).await;
        }

        if matches!(
            self.network_policy().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            tracing::info!("MCP discovery skipped because the network policy denies connections");
            return;
        }

        let mut pending = tokio::task::JoinSet::new();

        for server in reconcile
            .to_connect_new
            .into_iter()
            .chain(reconcile.to_connect_changed)
        {
            let name = server.name.clone();
            if self.has_client(&name).await {
                tracing::info!("MCP server '{}' config changed, reconnecting", name);
                self.remove_client(&name).await;
            }
            let connect_t0 = std::time::Instant::now();
            let limits = self.limits.read().await.clone();
            let client = Arc::new(McpClient::new(
                &server,
                limits.mcp_max_binary_payload_bytes,
                limits.mcp_max_sse_buffer_bytes,
            ));
            client.set_network_policy(self.network_policy().await).await;
            let name = client.name().to_string();
            self.clients
                .lock()
                .await
                .insert(name.clone(), client.clone());

            let _notification_listener = self.start_catalog_listener(client.clone());

            // Set authoritative client status AND broadcast before the connect
            // task so ToolsView's list_mcp_servers snapshot shows Connecting
            // (reconnect already does both; emit-only left status Disconnected).
            *client.status.lock().await = McpClientStatus::Connecting;
            let _ = self.status_tx.send(McpStatusChangeEvent {
                name: name.clone(),
                status: McpClientStatus::Connecting,
            });

            let status_tx = self.status_tx.clone();
            pending.spawn(async move {
                let result = client.connect().await;
                (name, client, status_tx, result, connect_t0)
            });
        }

        while let Some(result) = pending.join_next().await {
            match result {
                Ok((name, client, status_tx, Ok(()), t0)) => {
                    tracing::info!(
                        "MCP server '{}' connected successfully in {:?}",
                        name,
                        t0.elapsed()
                    );
                    let _ = status_tx.send(McpStatusChangeEvent {
                        name: name.clone(),
                        status: McpClientStatus::Connected,
                    });
                    drop(client);
                }
                Ok((name, client, status_tx, Err(e), t0)) => {
                    let safe_error = haven_common::error::sanitize_error_text(&e.to_string());
                    tracing::warn!(
                        "MCP server '{}' failed to connect after {:?}: {} (will retry later)",
                        name,
                        t0.elapsed(),
                        safe_error
                    );
                    *client.last_error.lock().await =
                        Some(format!("initial connect failed: {safe_error}"));
                    let _ = status_tx.send(McpStatusChangeEvent {
                        name: name.clone(),
                        status: McpClientStatus::Offline {
                            error: format!("initial connect failed: {safe_error}"),
                        },
                    });
                }
                Err(e) => {
                    tracing::warn!("MCP connection session failed: {}", e);
                }
            }
        }
        tracing::info!(
            "MCP load_from_config: {} servers, total {:?}",
            servers.len(),
            t0.elapsed()
        );
    }

    /// Dynamically connect a single MCP server from config.
    /// Used by the `load_mcp` builtin tool. Returns an error if already connected.
    /// Starts a health monitor + auto-reconnect loop using the stored discovery config.
    pub async fn connect_server(
        &self,
        config: &haven_common::McpServerConfig,
    ) -> anyhow::Result<()> {
        let name = &config.name;
        if matches!(
            self.network_policy().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            anyhow::bail!("MCP connection blocked by network policy")
        }
        {
            let clients = self.clients.lock().await;
            if clients.contains_key(name) {
                anyhow::bail!("MCP server '{}' is already loaded", name);
            }
        }

        let limits = self.limits.read().await.clone();
        let client = Arc::new(McpClient::new(
            config,
            limits.mcp_max_binary_payload_bytes,
            limits.mcp_max_sse_buffer_bytes,
        ));
        client.set_network_policy(self.network_policy().await).await;

        let _notification_listener = self.start_catalog_listener(client.clone());

        if let Err(error) = client.connect().await {
            let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
            tracing::warn!(
                server = %name,
                error = %safe_error,
                "MCP server connection failed"
            );
            return Err(anyhow::anyhow!(
                "failed to connect MCP server '{}': {}",
                name,
                safe_error
            ));
        }

        // Register before starting the manager-owned health monitor.
        self.add_client(client.clone()).await;

        // Start health monitor + auto-reconnect using the stored discovery config.
        let dc = self.discovery_config.read().await.clone();
        self.install_monitor(client, &dc).await;
        let _ = self.status_tx.send(McpStatusChangeEvent {
            name: name.clone(),
            status: McpClientStatus::Connected,
        });

        Ok(())
    }

    /// Start health-monitor + auto-reconnect loops for all connected clients.
    /// Also stores the discovery config for later use by `connect_server`.
    pub async fn start_monitors(&self, config: &haven_common::config::McpDiscoveryConfig) {
        *self.discovery_config.write().await = config.clone();
        if matches!(
            self.network_policy().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            return;
        }
        let clients = self
            .clients
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for client in clients {
            self.install_monitor(client, config).await;
        }
    }

    /// Combined: load config + connect + start health monitors.
    pub async fn discover_all(
        &self,
        servers: &[haven_common::McpServerConfig],
        config: &haven_common::McpDiscoveryConfig,
    ) {
        self.load_from_config(servers).await;
        self.start_monitors(config).await;
    }

    /// Shut down all clients and cancel their monitor sessions.
    pub async fn shutdown_all(&self) {
        let monitors = self
            .monitors
            .lock()
            .await
            .drain()
            .map(|(_, monitor)| monitor)
            .collect::<Vec<_>>();
        for monitor in &monitors {
            monitor.abort();
        }
        for monitor in monitors {
            let _ = monitor.await;
        }
        let clients = self
            .clients
            .lock()
            .await
            .iter()
            .map(|(name, client)| (name.clone(), client.clone()))
            .collect::<Vec<_>>();
        for (name, client) in clients {
            if let Err(e) = client.shutdown().await {
                tracing::warn!("Error shutting down MCP client '{}': {}", name, e);
            }
        }
    }

    /// Return a snapshot of every known client (including disconnected ones).
    pub async fn snapshot(&self) -> Vec<McpServerSnapshot> {
        let clients = self
            .clients
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        futures_util::future::join_all(clients.iter().map(|client| client.snapshot())).await
    }

    /// Manually trigger a reconnect for a client, bypassing the retry-limit.
    /// A successful reconnect also installs the manager-owned health monitor.
    pub async fn reconnect(&self, name: &str) -> anyhow::Result<()> {
        if matches!(
            self.network_policy().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            anyhow::bail!("MCP reconnect blocked by network policy")
        }
        let client = self
            .client(name)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' not found", name))?;
        self.stop_monitor(name).await;
        client.reconnect().await?;
        let config = self.discovery_config.read().await.clone();
        self.install_monitor(client, &config).await;
        self.bump_catalog_version();
        Ok(())
    }

    /// Refresh all tools caches in parallel for connected clients.
    pub async fn refresh_all_tools(&self) {
        let clients = self
            .clients
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut handles = Vec::new();
        for client in clients {
            let name = client.name().to_string();
            handles.push(tokio::spawn(async move {
                match client.list_tools().await {
                    Ok(tools) => {
                        *client.tools_cache.lock().await = Some(tools);
                        // The client may have been added directly in tests or
                        // by a native host path, bypassing load_from_config.
                        // Refreshing its discovery cache still invalidates
                        // the model-facing catalog.
                    }
                    Err(e) => {
                        tracing::warn!("Failed to refresh tools from '{}': {}", name, e);
                    }
                }
            }));
        }
        for h in handles {
            if let Err(error) = h.await {
                tracing::warn!("MCP tools refresh task failed: {}", error);
            }
        }
        self.bump_catalog_version();
    }

    pub async fn call_tool(
        &self,
        client_name: &str,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpCallOutput> {
        let client = self
            .client(client_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' not found", client_name))?;
        client.call_tool(tool_name, input, cancel).await
    }
}

/// Bridge from the llm crate's generic MCP tool surface into the live
/// `McpManager`, so the STT `mcp` provider can be built without the llm
/// crate depending on tools.
#[async_trait::async_trait]
impl McpToolCaller for McpManager {
    async fn invoke_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpToolOutcome> {
        let result = self
            .call_tool(server_name, tool_name, input, cancel)
            .await?;
        Ok(McpToolOutcome {
            success: result.success,
            error: result.error,
            output: result.output,
        })
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}
