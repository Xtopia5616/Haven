use crate::client::McpClient;
use crate::protocol::{McpCallOutput, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent};
use haven_llm::{McpToolCaller, McpToolOutcome};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
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
    pub(crate) clients: Arc<Mutex<HashMap<String, Arc<McpClient>>>>,
    status_tx: tokio::sync::broadcast::Sender<McpStatusChangeEvent>,
    discovery_config: Arc<tokio::sync::RwLock<haven_common::config::McpDiscoveryConfig>>,
    limits: Arc<tokio::sync::RwLock<haven_common::config::ContextLimitsConfig>>,
}

impl Clone for McpManager {
    fn clone(&self) -> Self {
        Self {
            clients: self.clients.clone(),
            status_tx: self.status_tx.clone(),
            discovery_config: self.discovery_config.clone(),
            limits: self.limits.clone(),
        }
    }
}

impl McpManager {
    pub fn new() -> Self {
        let (status_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_tx,
            discovery_config: Arc::new(tokio::sync::RwLock::new(
                haven_common::config::McpDiscoveryConfig::default(),
            )),
            limits: Arc::new(tokio::sync::RwLock::new(
                haven_common::config::ContextLimitsConfig::default(),
            )),
        }
    }

    /// Replace the unified context limits (binary payload / SSE buffer caps)
    /// used when creating MCP clients from config.
    pub async fn set_limits(&self, limits: &haven_common::config::ContextLimitsConfig) {
        *self.limits.write().await = limits.clone();
    }

    pub fn status_tx(&self) -> tokio::sync::broadcast::Sender<McpStatusChangeEvent> {
        self.status_tx.clone()
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<McpStatusChangeEvent> {
        self.status_tx.subscribe()
    }

    pub async fn add_client(&self, client: Arc<McpClient>) {
        self.clients
            .lock()
            .await
            .insert(client.name().to_string(), client);
    }

    pub async fn remove_client(&self, name: &str) {
        let mut clients = self.clients.lock().await;
        if let Some(client) = clients.remove(name) {
            client.cancel_token.lock().await.cancel();
            let _ = client.shutdown().await;
        }
    }

    pub async fn get_client(&self, name: &str) -> Option<Arc<McpClient>> {
        self.clients.lock().await.get(name).cloned()
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
        let t0 = std::time::Instant::now();
        let reconcile = self.reconcile_servers(servers).await;

        for name in reconcile.to_remove {
            tracing::info!("MCP server '{}' removed/disabled, shutting down", name);
            self.remove_client(&name).await;
            let _ = self.status_tx.send(McpStatusChangeEvent {
                name,
                status: McpClientStatus::Disconnected,
            });
        }

        let mut pending = tokio::task::JoinSet::new();

        for server in reconcile
            .to_connect_new
            .into_iter()
            .chain(reconcile.to_connect_changed)
        {
            let name = server.name.clone();
            {
                let mut clients = self.clients.lock().await;
                if clients.contains_key(&name) {
                    tracing::info!("MCP server '{}' config changed, reconnecting", name);
                    clients.remove(&name);
                }
            }
            let connect_t0 = std::time::Instant::now();
            let limits = self.limits.read().await.clone();
            let client = Arc::new(McpClient::new(
                &server,
                limits.mcp_max_binary_payload_bytes,
                limits.mcp_max_sse_buffer_bytes,
            ));
            let name = client.name().to_string();
            self.clients
                .lock()
                .await
                .insert(name.clone(), client.clone());

            let listener_client = client.clone();
            listener_client.start_notification_listener(move |server_name: &str| {
                tracing::info!("MCP server '{}' pushed tools/list_changed", server_name);
            });

            // Set authoritative client status AND broadcast before the connect
            // task so ToolsView's list_mcp_tools snapshot shows Connecting
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
                    tracing::warn!(
                        "MCP server '{}' failed to connect after {:?}: {} (will retry later)",
                        name,
                        t0.elapsed(),
                        e
                    );
                    *client.last_error.lock().await = Some(format!("initial connect failed: {e}"));
                    let _ = status_tx.send(McpStatusChangeEvent {
                        name: name.clone(),
                        status: McpClientStatus::Offline {
                            error: format!("initial connect failed: {e}"),
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

        let listener_client = client.clone();
        listener_client.start_notification_listener(move |server_name: &str| {
            tracing::info!("MCP server '{}' pushed tools/list_changed", server_name);
        });

        client
            .connect()
            .await
            .map_err(|e| anyhow::anyhow!("failed to connect MCP server '{}': {}", name, e))?;

        // Start health monitor + auto-reconnect using the stored discovery config.
        let dc = self.discovery_config.read().await.clone();
        let health_interval = Duration::from_secs(dc.health_interval_secs);
        let initial_backoff = Duration::from_millis(dc.reconnect_initial_ms);
        let max_backoff = Duration::from_millis(dc.reconnect_max_ms);
        let max_retries = dc.reconnect_max_retries;
        let status_tx = self.status_tx.clone();
        client.clone().spawn_monitor(
            health_interval,
            initial_backoff,
            max_backoff,
            max_retries,
            status_tx,
        );

        self.clients.lock().await.insert(name.clone(), client);
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
        let health_interval = Duration::from_secs(config.health_interval_secs);
        let initial_backoff = Duration::from_millis(config.reconnect_initial_ms);
        let max_backoff = Duration::from_millis(config.reconnect_max_ms);
        let max_retries = config.reconnect_max_retries;

        let clients = self.clients.lock().await;
        for client in clients.values() {
            let client = client.clone();
            let status_tx = self.status_tx.clone();
            client.spawn_monitor(
                health_interval,
                initial_backoff,
                max_backoff,
                max_retries,
                status_tx,
            );
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
        let clients = self.clients.lock().await;
        for (name, client) in clients.iter() {
            client.cancel_token.lock().await.cancel();
            if let Err(e) = client.shutdown().await {
                tracing::warn!("Error shutting down MCP client '{}': {}", name, e);
            }
        }
    }

    /// Return a snapshot of every known client (including disconnected ones).
    pub async fn snapshot(&self) -> Vec<McpServerSnapshot> {
        let clients = self.clients.lock().await;
        let mut snapshots = Vec::new();
        for client in clients.values() {
            snapshots.push(client.snapshot().await);
        }
        snapshots
    }

    /// Manually trigger a reconnect for a client, bypassing the retry-limit.
    /// After a successful reconnect the caller should restart the health
    /// monitor (see `start_monitors`).
    pub async fn reconnect(&self, name: &str) -> anyhow::Result<()> {
        let clients = self.clients.lock().await;
        match clients.get(name) {
            Some(client) => client.reconnect().await,
            None => anyhow::bail!("MCP client '{}' not found", name),
        }
    }

    /// Refresh all tools caches in parallel for connected clients.
    pub async fn refresh_all_tools(&self) {
        let clients = self.clients.lock().await;
        let mut handles = Vec::new();
        for client in clients.values() {
            let client = client.clone();
            let name = client.name().to_string();
            handles.push(tokio::spawn(async move {
                match client.list_tools().await {
                    Ok(tools) => {
                        *client.tools_cache.lock().await = Some(tools);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to refresh tools from '{}': {}", name, e);
                    }
                }
            }));
        }
        for h in handles {
            let _ = h.await;
        }
    }

    pub async fn call_tool(
        &self,
        client_name: &str,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpCallOutput> {
        let clients = self.clients.lock().await;
        match clients.get(client_name) {
            Some(client) => client.call_tool(tool_name, input, cancel).await,
            None => anyhow::bail!("MCP client '{}' not found", client_name),
        }
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
