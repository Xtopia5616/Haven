use crate::client::McpClient;
use crate::protocol::{
    McpCallOutput, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
};
use crate::{McpPrompt, McpPromptList, McpResourceList, McpResourceRead};
use futures_util::FutureExt;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;

const NOTIFICATION_LISTENER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECTION_TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_MANAGER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy)]
pub struct McpTargetChanged;

impl std::fmt::Display for McpTargetChanged {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MCP target changed after authorization")
    }
}

impl std::error::Error for McpTargetChanged {}

struct OwnedNotificationListener {
    client: Arc<McpClient>,
    task: tokio::task::JoinHandle<()>,
}

struct OwnedMonitor {
    client: Arc<McpClient>,
    task: tokio::task::JoinHandle<()>,
}

struct ConnectionReservation {
    generation: u64,
    name: String,
    client: Arc<McpClient>,
    cancel: CancellationToken,
}

#[derive(Clone)]
enum ConnectionPurpose {
    Initial {
        started_at: Instant,
    },
    Dynamic {
        discovery_config: haven_common::config::McpDiscoveryConfig,
    },
    Reconnect {
        discovery_config: haven_common::config::McpDiscoveryConfig,
    },
}

struct ConnectionWaiter {
    cancel: CancellationToken,
    result: Option<oneshot::Receiver<anyhow::Result<()>>>,
    completed: bool,
}

impl ConnectionWaiter {
    async fn wait(mut self) -> anyhow::Result<()> {
        let receiver = self
            .result
            .take()
            .expect("connection waiter result is consumed once");
        let result = receiver
            .await
            .unwrap_or_else(|_| Err(anyhow::anyhow!("MCP connection task stopped unexpectedly")));
        self.completed = true;
        result
    }
}

impl Drop for ConnectionWaiter {
    fn drop(&mut self) {
        if !self.completed {
            self.cancel.cancel();
        }
    }
}

struct ConnectionStartGuard {
    cancel: CancellationToken,
    armed: bool,
}

impl ConnectionStartGuard {
    fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ConnectionStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
        }
    }
}

async fn join_notification_listener(server_name: &str, mut task: tokio::task::JoinHandle<()>) {
    match tokio::time::timeout(NOTIFICATION_LISTENER_SHUTDOWN_TIMEOUT, &mut task).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(
            server = %server_name,
            task_panicked = error.is_panic(),
            task_cancelled = error.is_cancelled(),
            "MCP notification listener task failed"
        ),
        Err(_) => {
            tracing::warn!(
                server = %server_name,
                timeout = ?NOTIFICATION_LISTENER_SHUTDOWN_TIMEOUT,
                "MCP notification listener did not stop after cancellation; aborting"
            );
            task.abort();
            let _ = task.await;
        }
    }
}

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
    pub to_connect_new: Vec<haven_common::config::McpServerConfig>,
    /// Enabled servers whose live client was spawned from a different
    /// config (command / args / env / url changed) and must be recreated.
    pub to_connect_changed: Vec<haven_common::config::McpServerConfig>,
    /// Live clients whose server was removed from config or disabled.
    pub to_remove: Vec<String>,
    /// Enabled servers already loaded from the identical config (kept).
    pub kept_server_names: Vec<String>,
}

pub struct McpManager {
    manager_id: u64,
    clients: Arc<Mutex<HashMap<String, Arc<McpClient>>>>,
    monitors: Arc<Mutex<HashMap<String, OwnedMonitor>>>,
    notification_listeners: Arc<StdMutex<HashMap<usize, OwnedNotificationListener>>>,
    connection_lifecycle: Arc<Mutex<()>>,
    active_connections: Arc<StdMutex<HashMap<String, Arc<ConnectionReservation>>>>,
    connection_tasks: Arc<StdMutex<HashMap<u64, tokio::task::JoinHandle<()>>>>,
    next_connection_generation: Arc<AtomicU64>,
    shutting_down: Arc<AtomicBool>,
    #[cfg(test)]
    panic_next_connection: Arc<AtomicBool>,
    connection_slots: Arc<Semaphore>,
    status_tx: tokio::sync::broadcast::Sender<McpStatusChangeEvent>,
    discovery_config: Arc<tokio::sync::RwLock<haven_common::config::McpDiscoveryConfig>>,
    limits: Arc<tokio::sync::RwLock<haven_common::config::ContextLimitsConfig>>,
    network_policy: Arc<tokio::sync::RwLock<haven_common::types::NetworkPolicy>>,
    /// Authorization snapshot assigned to newly created client generations.
    /// Existing stdio clients retain the snapshot they were created with.
    stdio_process_authorization:
        Arc<RwLock<Option<Arc<dyn crate::McpStdioProcessStartAuthorization>>>>,
    /// Launcher assigned to newly created stdio client generations.
    stdio_process_launcher: Arc<RwLock<Arc<dyn crate::McpStdioProcessLauncher>>>,
    /// Owner for the policy-revocation subscription installed by Tools.
    stdio_policy_watcher: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// Changes whenever the configured server set or a server's tools/list
    /// cache changes. Consumers use this as a cheap catalog invalidation
    /// clock; it is not a durable protocol version.
    catalog_version: Arc<AtomicU64>,
}

impl Clone for McpManager {
    fn clone(&self) -> Self {
        Self {
            manager_id: self.manager_id,
            clients: self.clients.clone(),
            monitors: self.monitors.clone(),
            notification_listeners: self.notification_listeners.clone(),
            connection_lifecycle: self.connection_lifecycle.clone(),
            active_connections: self.active_connections.clone(),
            connection_tasks: self.connection_tasks.clone(),
            next_connection_generation: self.next_connection_generation.clone(),
            shutting_down: self.shutting_down.clone(),
            #[cfg(test)]
            panic_next_connection: self.panic_next_connection.clone(),
            connection_slots: self.connection_slots.clone(),
            status_tx: self.status_tx.clone(),
            discovery_config: self.discovery_config.clone(),
            limits: self.limits.clone(),
            network_policy: self.network_policy.clone(),
            stdio_process_authorization: self.stdio_process_authorization.clone(),
            stdio_process_launcher: self.stdio_process_launcher.clone(),
            stdio_policy_watcher: self.stdio_policy_watcher.clone(),
            catalog_version: self.catalog_version.clone(),
        }
    }
}

impl McpManager {
    pub fn new() -> Self {
        Self::new_with_network_policy(haven_common::types::NetworkPolicy::Ask)
    }

    /// Construct a manager with an explicit transport policy. The regular
    /// constructor is fail-closed; internal setup and tests may provide a
    /// different network boundary.
    fn new_with_network_policy(network_policy: haven_common::types::NetworkPolicy) -> Self {
        let (status_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            manager_id: NEXT_MANAGER_ID.fetch_add(1, Ordering::SeqCst),
            clients: Arc::new(Mutex::new(HashMap::new())),
            monitors: Arc::new(Mutex::new(HashMap::new())),
            notification_listeners: Arc::new(StdMutex::new(HashMap::new())),
            connection_lifecycle: Arc::new(Mutex::new(())),
            active_connections: Arc::new(StdMutex::new(HashMap::new())),
            connection_tasks: Arc::new(StdMutex::new(HashMap::new())),
            next_connection_generation: Arc::new(AtomicU64::new(1)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            panic_next_connection: Arc::new(AtomicBool::new(false)),
            connection_slots: Arc::new(Semaphore::new(crate::MCP_MAX_PARALLEL_CONNECTIONS)),
            status_tx,
            discovery_config: Arc::new(tokio::sync::RwLock::new(
                haven_common::config::McpDiscoveryConfig::default(),
            )),
            limits: Arc::new(tokio::sync::RwLock::new(
                haven_common::config::ContextLimitsConfig::default(),
            )),
            network_policy: Arc::new(tokio::sync::RwLock::new(network_policy)),
            stdio_process_authorization: Arc::new(RwLock::new(None)),
            stdio_process_launcher: Arc::new(RwLock::new(
                crate::client::default_stdio_process_launcher(),
            )),
            stdio_policy_watcher: Arc::new(Mutex::new(None)),
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
    async fn start_catalog_listener(&self, client: Arc<McpClient>) {
        let catalog_version = self.catalog_version.clone();
        let task = client
            .clone()
            .start_notification_listener(move |server_name: &str| {
                tracing::info!("MCP server '{}' pushed tools/list_changed", server_name);
                catalog_version.fetch_add(1, Ordering::Relaxed);
            });
        let key = Arc::as_ptr(&client) as usize;
        let previous = self
            .notification_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, OwnedNotificationListener { client, task });
        if let Some(previous) = previous {
            previous.client.notification_cancel.cancel();
            join_notification_listener(previous.client.name(), previous.task).await;
        }
    }

    async fn stop_catalog_listener(&self, client: &Arc<McpClient>) {
        let key = Arc::as_ptr(client) as usize;
        let listener = self
            .notification_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&key);
        if let Some(listener) = listener {
            join_notification_listener(listener.client.name(), listener.task).await;
        }
    }

    fn take_catalog_listeners(&self) -> Vec<OwnedNotificationListener> {
        self.notification_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, listener)| listener)
            .collect()
    }

    async fn begin_connection(
        &self,
        config: &haven_common::config::McpServerConfig,
        purpose: ConnectionPurpose,
        keep_matching_client: bool,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> anyhow::Result<Option<ConnectionWaiter>> {
        let limits = self.limits.read().await.clone();
        let _lifecycle = self.connection_lifecycle.lock().await;
        if self.shutting_down.load(Ordering::SeqCst) {
            anyhow::bail!("MCP manager is shutting down");
        }
        let network_policy = *self.network_policy.read().await;
        if matches!(network_policy, haven_common::types::NetworkPolicy::Deny) {
            anyhow::bail!("MCP connection blocked by network policy");
        }

        let stdio_process_authorization = self.stdio_process_authorization.read().await.clone();
        let stdio_process_launcher = self.stdio_process_launcher.read().await.clone();
        let client = Arc::new(McpClient::new_managed_with_launcher(
            config,
            limits.mcp_max_binary_base64_bytes,
            limits.mcp_max_sse_buffer_bytes,
            stdio_process_authorization,
            stdio_process_launcher,
        ));
        client.set_network_policy(network_policy).await;

        let name = client.name().to_string();
        {
            let mut clients = self.clients.lock().await;
            if let Some(existing) = clients.get(&name) {
                if keep_matching_client && existing.matches_config(config) {
                    return Ok(None);
                }
                anyhow::bail!("MCP server '{}' is already loaded", name);
            }
            // The client is visible before any network or child-process work
            // starts. This entry is also the atomic per-name reservation used
            // by concurrent dynamic connections and policy changes.
            clients.insert(name.clone(), client.clone());
        }

        let generation = self
            .next_connection_generation
            .fetch_add(1, Ordering::SeqCst);
        client.set_manager_generation(generation);
        let cancel = CancellationToken::new();
        let reservation = Arc::new(ConnectionReservation {
            generation,
            name: name.clone(),
            client: client.clone(),
            cancel: cancel.clone(),
        });
        self.active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.clone(), reservation.clone());

        let mut start_guard = ConnectionStartGuard::new(cancel.clone());
        let (waiter, start_tx) = self.spawn_connection_worker(reservation.clone(), purpose, permit);
        self.start_catalog_listener(client.clone()).await;
        *client.status.lock().await = McpClientStatus::Connecting;
        let _ = self.status_tx.send(McpStatusChangeEvent {
            name,
            status: McpClientStatus::Connecting,
        });
        let _ = start_tx.send(());
        start_guard.disarm();

        Ok(Some(waiter))
    }

    fn spawn_connection_worker(
        &self,
        reservation: Arc<ConnectionReservation>,
        purpose: ConnectionPurpose,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> (ConnectionWaiter, oneshot::Sender<()>) {
        let (start_tx, start_rx) = oneshot::channel();
        let (result_tx, result_rx) = oneshot::channel();
        let manager = self.clone();
        let worker_reservation = reservation.clone();
        let generation = reservation.generation;
        let task = tokio::spawn(async move {
            let _permit = permit;
            tokio::select! {
                biased;
                _ = worker_reservation.cancel.cancelled() => {
                    manager.cleanup_connection_generation(worker_reservation.clone()).await;
                    let _ = result_tx.send(Err(anyhow::anyhow!("MCP connection attempt was cancelled")));
                }
                started = start_rx => {
                    if started.is_ok() {
                        let run = AssertUnwindSafe(manager.run_connection_attempt(
                            worker_reservation.clone(),
                            purpose,
                        ));
                        match run.catch_unwind().await {
                            Ok(result) => {
                                let _ = result_tx.send(result);
                            }
                            Err(_) => {
                                tracing::error!(server = %worker_reservation.name, "MCP connection task panicked");
                                manager.cleanup_connection_generation(worker_reservation.clone()).await;
                                let _ = result_tx.send(Err(anyhow::anyhow!("MCP connection task panicked")));
                            }
                        }
                    } else {
                        manager.cleanup_connection_generation(worker_reservation.clone()).await;
                        let _ = result_tx.send(Err(anyhow::anyhow!("MCP connection task could not start")));
                    }
                }
            }
            manager.forget_connection_task(generation);
        });
        self.connection_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(generation, task);
        (
            ConnectionWaiter {
                cancel: reservation.cancel.clone(),
                result: Some(result_rx),
                completed: false,
            },
            start_tx,
        )
    }

    fn forget_connection_task(&self, generation: u64) {
        self.connection_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&generation);
    }

    async fn run_connection_attempt(
        &self,
        reservation: Arc<ConnectionReservation>,
        purpose: ConnectionPurpose,
    ) -> anyhow::Result<()> {
        #[cfg(test)]
        if self.panic_next_connection.swap(false, Ordering::SeqCst) {
            panic!("injected MCP connection task panic");
        }

        let connect = async {
            match &purpose {
                ConnectionPurpose::Reconnect { .. } => reservation.client.reconnect().await,
                ConnectionPurpose::Initial { .. } | ConnectionPurpose::Dynamic { .. } => {
                    reservation.client.connect().await
                }
            }
        };
        let connect_result = tokio::select! {
            biased;
            _ = reservation.cancel.cancelled() => Err(anyhow::anyhow!("MCP connection attempt was cancelled")),
            result = connect => result,
        };
        if reservation.cancel.is_cancelled() {
            self.cleanup_connection_generation(reservation).await;
            return Err(anyhow::anyhow!("MCP connection attempt was cancelled"));
        }

        match connect_result {
            Ok(()) => {
                if self.commit_connection(&reservation, &purpose).await {
                    Ok(())
                } else {
                    self.cleanup_connection_generation(reservation).await;
                    Err(anyhow::anyhow!(
                        "MCP connection was superseded or blocked by policy"
                    ))
                }
            }
            Err(error) => {
                match purpose {
                    ConnectionPurpose::Initial { .. } => {
                        self.finish_connection_failure(
                            &reservation,
                            &error,
                            "initial connect failed",
                        )
                        .await;
                    }
                    ConnectionPurpose::Reconnect { .. } => {
                        self.finish_connection_failure(&reservation, &error, "reconnect failed")
                            .await;
                    }
                    ConnectionPurpose::Dynamic { .. } => {
                        self.cleanup_connection_generation(reservation).await;
                    }
                }
                Err(error)
            }
        }
    }

    async fn commit_connection(
        &self,
        reservation: &Arc<ConnectionReservation>,
        purpose: &ConnectionPurpose,
    ) -> bool {
        let _lifecycle = self.connection_lifecycle.lock().await;
        if self.shutting_down.load(Ordering::SeqCst)
            || reservation.cancel.is_cancelled()
            || matches!(
                *self.network_policy.read().await,
                haven_common::types::NetworkPolicy::Deny
            )
        {
            return false;
        }
        let client_is_current = self
            .clients
            .lock()
            .await
            .get(&reservation.name)
            .is_some_and(|client| Arc::ptr_eq(client, &reservation.client));
        let attempt_is_current = self
            .active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&reservation.name)
            .is_some_and(|attempt| attempt.generation == reservation.generation);
        if !client_is_current || !attempt_is_current {
            return false;
        }

        match purpose {
            ConnectionPurpose::Dynamic { discovery_config }
            | ConnectionPurpose::Reconnect { discovery_config } => {
                self.install_monitor(reservation.client.clone(), discovery_config)
                    .await;
            }
            ConnectionPurpose::Initial { .. } => {}
        }
        self.active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation.name);
        if let ConnectionPurpose::Initial { started_at } = purpose {
            tracing::info!(
                "MCP server '{}' connected successfully in {:?}",
                reservation.name,
                started_at.elapsed()
            );
        }
        let _ = self.status_tx.send(McpStatusChangeEvent {
            name: reservation.name.clone(),
            status: McpClientStatus::Connected,
        });
        true
    }

    async fn finish_connection_failure(
        &self,
        reservation: &Arc<ConnectionReservation>,
        error: &anyhow::Error,
        error_context: &str,
    ) {
        let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
        let _lifecycle = self.connection_lifecycle.lock().await;
        let client_is_current = self
            .clients
            .lock()
            .await
            .get(&reservation.name)
            .is_some_and(|client| Arc::ptr_eq(client, &reservation.client));
        let attempt_is_current = self
            .active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&reservation.name)
            .is_some_and(|attempt| attempt.generation == reservation.generation);
        if !client_is_current || !attempt_is_current {
            drop(_lifecycle);
            self.cleanup_connection_generation(reservation.clone())
                .await;
            return;
        }

        tracing::warn!(
            server = %reservation.name,
            "MCP server connection failed; will retry later"
        );
        let message = format!("{error_context}: {safe_error}");
        *reservation.client.last_error.lock().await = Some(message.clone());
        *reservation.client.status.lock().await = McpClientStatus::Offline {
            error: message.clone(),
        };
        self.active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&reservation.name);
        let _ = self.status_tx.send(McpStatusChangeEvent {
            name: reservation.name.clone(),
            status: McpClientStatus::Offline { error: message },
        });
    }

    async fn cleanup_connection_generation(&self, reservation: Arc<ConnectionReservation>) {
        reservation.cancel.cancel();
        let removed = {
            let _lifecycle = self.connection_lifecycle.lock().await;
            let removed = {
                let mut clients = self.clients.lock().await;
                if clients
                    .get(&reservation.name)
                    .is_some_and(|client| Arc::ptr_eq(client, &reservation.client))
                {
                    clients.remove(&reservation.name)
                } else {
                    None
                }
            };
            let mut active = self
                .active_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active
                .get(&reservation.name)
                .is_some_and(|attempt| attempt.generation == reservation.generation)
            {
                active.remove(&reservation.name);
            }
            removed
        };
        self.cleanup_client_runtime(
            &reservation.name,
            reservation.client.clone(),
            removed.is_some(),
        )
        .await;
    }

    async fn cleanup_client_runtime(&self, name: &str, client: Arc<McpClient>, was_current: bool) {
        self.stop_monitor_for_generation(name, &client).await;
        if let Err(error) = client.shutdown().await {
            tracing::warn!(
                server = %name,
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "MCP connection cleanup reported an error"
            );
        }
        self.stop_catalog_listener(&client).await;
        if was_current {
            let _lifecycle = self.connection_lifecycle.lock().await;
            if !self.clients.lock().await.contains_key(name) {
                self.bump_catalog_version();
                let _ = self.status_tx.send(McpStatusChangeEvent {
                    name: name.to_string(),
                    status: McpClientStatus::Disconnected,
                });
            }
        }
    }

    async fn stop_monitor_for_generation(&self, name: &str, client: &Arc<McpClient>) {
        let monitor = {
            let mut monitors = self.monitors.lock().await;
            if monitors
                .get(name)
                .is_some_and(|monitor| Arc::ptr_eq(&monitor.client, client))
            {
                monitors.remove(name)
            } else {
                None
            }
        };
        if let Some(monitor) = monitor {
            monitor.task.abort();
            let _ = monitor.task.await;
        }
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
        let _lifecycle = self.connection_lifecycle.lock().await;
        *self.network_policy.write().await = policy;
        let clients: Vec<_> = self
            .clients
            .lock()
            .await
            .iter()
            .map(|(name, client)| (name.clone(), client.clone()))
            .collect();
        if !matches!(policy, haven_common::types::NetworkPolicy::Open) {
            for attempt in self
                .active_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
            {
                attempt.cancel.cancel();
            }
        }
        for (_, client) in &clients {
            client.set_network_policy(policy).await;
        }
        drop(_lifecycle);
        if !matches!(policy, haven_common::types::NetworkPolicy::Open) {
            // Existing HTTP clients may have been created with a different
            // DNS resolution/pinning decision. Tear them down too; callers
            // must explicitly rediscover after a boundary change.
            for (name, client) in clients {
                self.remove_client_generation(&name, Some(&client)).await;
            }
        }
    }

    /// Set the authorization snapshot used for future stdio client
    /// generations. Existing generations keep their original snapshot and are
    /// disconnected when the bound policy signal changes.
    pub async fn set_stdio_process_start_authorization(
        &self,
        authorization: Option<Arc<dyn crate::McpStdioProcessStartAuthorization>>,
    ) {
        *self.stdio_process_authorization.write().await = authorization;
    }

    /// Set the process owner for future stdio client generations. Changing it
    /// disconnects live stdio clients so a reconnect cannot retain an obsolete
    /// host/HCS execution boundary.
    pub async fn set_stdio_process_launcher(
        &self,
        launcher: Arc<dyn crate::McpStdioProcessLauncher>,
    ) {
        *self.stdio_process_launcher.write().await = launcher;
        self.disconnect_stdio_clients().await;
    }

    /// Subscribe to the authorization owner's policy signal. Each change
    /// removes all live stdio generations; their health monitors cannot
    /// reconnect because their captured spawn authorization is stale.
    pub async fn watch_stdio_policy_changes(
        &self,
        mut changes: tokio::sync::watch::Receiver<Arc<()>>,
    ) {
        let previous = self.stdio_policy_watcher.lock().await.take();
        if let Some(previous) = previous {
            previous.abort();
            let _ = previous.await;
        }
        let manager = self.clone();
        let task = tokio::spawn(async move {
            while changes.changed().await.is_ok() {
                manager.disconnect_stdio_clients().await;
            }
        });
        *self.stdio_policy_watcher.lock().await = Some(task);
    }

    async fn disconnect_stdio_clients(&self) {
        let clients = self
            .clients
            .lock()
            .await
            .iter()
            .filter(|(_, client)| {
                matches!(
                    client.transport,
                    haven_common::mcp::McpServerTransport::Stdio
                )
            })
            .map(|(name, client)| (name.clone(), client.clone()))
            .collect::<Vec<_>>();
        for (name, client) in clients {
            self.remove_client_generation(&name, Some(&client)).await;
        }
    }

    pub(crate) async fn network_policy(&self) -> haven_common::types::NetworkPolicy {
        *self.network_policy.read().await
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<McpStatusChangeEvent> {
        self.status_tx.subscribe()
    }

    #[cfg(test)]
    pub(crate) async fn add_client(&self, client: Arc<McpClient>) {
        let inserted = {
            let _lifecycle = self.connection_lifecycle.lock().await;
            if self.shutting_down.load(Ordering::SeqCst) {
                false
            } else {
                let generation = self
                    .next_connection_generation
                    .fetch_add(1, Ordering::SeqCst);
                client.set_manager_generation(generation);
                self.clients
                    .lock()
                    .await
                    .insert(client.name().to_string(), client.clone());
                true
            }
        };
        if inserted {
            self.bump_catalog_version();
        } else if let Err(error) = client.shutdown().await {
            tracing::warn!(
                server = %client.name(),
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "MCP client rejected after manager shutdown"
            );
        }
    }

    pub async fn remove_client(&self, name: &str) {
        self.remove_client_generation(name, None).await;
    }

    async fn remove_client_generation(&self, name: &str, expected: Option<&Arc<McpClient>>) {
        let client = {
            let _lifecycle = self.connection_lifecycle.lock().await;
            let client = {
                let mut clients = self.clients.lock().await;
                let current = clients.get(name).cloned();
                if current.as_ref().is_some_and(|current| {
                    expected.is_none_or(|expected| Arc::ptr_eq(current, expected))
                }) {
                    clients.remove(name)
                } else {
                    None
                }
            };
            if let Some(client) = &client {
                let mut active = self
                    .active_connections
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if active
                    .get(name)
                    .is_some_and(|attempt| Arc::ptr_eq(&attempt.client, client))
                    && let Some(attempt) = active.remove(name)
                {
                    attempt.cancel.cancel();
                }
            }
            client
        };
        if let Some(client) = client {
            self.cleanup_client_runtime(name, client, true).await;
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

    /// Opaque identity for the manager and live connection that route this
    /// server name. It contains no endpoint or credential data.
    pub async fn client_target_binding(&self, name: &str) -> Option<String> {
        let client = self.client(name).await?;
        self.target_binding_for(&client)
    }

    /// Return a target binding only when the live generation uses the
    /// transport expected by the adapter. The check and binding refer to the
    /// same client snapshot, so a replacement cannot change transport between
    /// the two observations.
    pub async fn client_target_binding_for_transport(
        &self,
        name: &str,
        expected_transport: &haven_common::mcp::McpServerTransport,
    ) -> Option<String> {
        let client = self.client(name).await?;
        if client.transport != *expected_transport {
            return None;
        }
        self.target_binding_for(&client)
    }

    fn target_binding_for(&self, client: &McpClient) -> Option<String> {
        let generation = client.manager_generation();
        (generation != 0).then(|| format!("mcp-target:{:016x}:{generation:016x}", self.manager_id))
    }

    async fn client_at_target_binding(
        &self,
        client_name: &str,
        expected_binding: &str,
    ) -> anyhow::Result<Arc<McpClient>> {
        let client = self
            .client(client_name)
            .await
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' not found", client_name))?;
        let generation = client.manager_generation();
        let actual_binding = (generation != 0)
            .then(|| format!("mcp-target:{:016x}:{generation:016x}", self.manager_id));
        if actual_binding.as_deref() != Some(expected_binding) {
            return Err(anyhow::Error::new(McpTargetChanged));
        }
        if !client.enabled() {
            anyhow::bail!("MCP client '{}' is disabled", client_name);
        }
        Ok(client)
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
        let mut snapshot = client.snapshot().await;
        snapshot.target_binding = self.target_binding_for(&client);
        Some(snapshot)
    }

    /// Cached tool count. `None` means no tools/list result is available yet;
    /// `Some(0)` means discovery completed and returned an empty list.
    pub async fn cached_tool_count(&self, name: &str) -> Option<usize> {
        self.cached_tools(name).await.map(|tools| tools.len())
    }

    /// Read the completed tools/list cache. `None` means the client is absent
    /// or discovery has not completed; `Some(vec![])` is a known empty list.
    pub async fn cached_tools(&self, name: &str) -> Option<Vec<McpToolInfo>> {
        let client = self.client(name).await?;
        let (tools, refreshed) = match client.ensure_tool_cache_fresh().await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(
                    server = %name,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "MCP tools/list refresh after cache expiry failed"
                );
                return None;
            }
        };
        if refreshed {
            // Cache publication precedes the generation bump so a concurrent
            // prompt cannot memoize stale tool schemas under the new version.
            self.bump_catalog_version();
        }
        tools
    }

    /// Wait for tools/list discovery without exposing the concrete client.
    pub async fn wait_for_tools(&self, name: &str, timeout: Duration) -> Option<Vec<McpToolInfo>> {
        let client = self.client(name).await?;
        if let Some(tools) = self.cached_tools(name).await {
            return Some(tools);
        }
        Some(client.wait_for_tools(timeout).await)
    }

    /// Compare the managed client generation with the currently authorized
    /// configuration without returning the client implementation.
    pub async fn client_matches_config(
        &self,
        name: &str,
        config: &haven_common::config::McpServerConfig,
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
            previous.task.abort();
            let _ = previous.task.await;
        }
        let handle = client.clone().spawn_monitor_task(
            Duration::from_secs(config.health_interval_secs),
            Duration::from_millis(config.reconnect_initial_ms),
            Duration::from_millis(config.reconnect_max_ms),
            config.reconnect_max_retries,
            self.status_tx.clone(),
            self.connection_slots.clone(),
        );
        monitors.insert(
            name,
            OwnedMonitor {
                client,
                task: handle,
            },
        );
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
        servers: &[haven_common::config::McpServerConfig],
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
                    out.kept_server_names.push(server.name.clone());
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
    pub async fn load_from_config(&self, servers: &[haven_common::config::McpServerConfig]) {
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

        let mut pending = Vec::new();
        let candidates = reconcile
            .to_connect_new
            .into_iter()
            .map(|server| (server, false))
            .chain(
                reconcile
                    .to_connect_changed
                    .into_iter()
                    .map(|server| (server, true)),
            );
        for (server, changed_config) in candidates {
            // Acquire before creating the client or starting its notification
            // consumer, so both connection attempts and queued setup stay
            // bounded at every manager entry point.
            let permit = match self.connection_slots.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    tracing::error!(error = %error, "MCP connection admission closed");
                    break;
                }
            };

            let name = server.name.clone();
            if changed_config
                && let Some(existing) = self.client(&name).await
                && !existing.matches_config(&server)
            {
                self.remove_client_generation(&name, Some(&existing)).await;
            }
            let started_at = Instant::now();
            match self
                .begin_connection(
                    &server,
                    ConnectionPurpose::Initial { started_at },
                    true,
                    permit,
                )
                .await
            {
                Ok(Some(waiter)) => pending.push((name, waiter)),
                Ok(None) => {}
                Err(error) => tracing::warn!(
                    server = %name,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "MCP config connection was not started"
                ),
            }
        }

        for (name, waiter) in pending {
            if waiter.wait().await.is_err() {
                tracing::warn!(
                    server = %name,
                    failure_kind = "connection_attempt",
                    "MCP server connection attempt did not complete successfully"
                );
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
        config: &haven_common::config::McpServerConfig,
    ) -> anyhow::Result<()> {
        if matches!(
            self.network_policy().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            anyhow::bail!("MCP connection blocked by network policy");
        }
        let permit = self
            .connection_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| anyhow::anyhow!("MCP connection admission closed: {error}"))?;
        let dc = self.discovery_config.read().await.clone();
        let waiter = self
            .begin_connection(
                config,
                ConnectionPurpose::Dynamic {
                    discovery_config: dc,
                },
                false,
                permit,
            )
            .await?
            .expect("dynamic connections always create a reservation");
        waiter.wait().await.map_err(|error| {
            let safe_error = haven_common::error::sanitize_error_text(&error.to_string());
            anyhow::anyhow!(
                "failed to connect MCP server '{}': {}",
                config.name,
                safe_error
            )
        })
    }

    async fn begin_reconnect(
        &self,
        name: &str,
        permit: tokio::sync::OwnedSemaphorePermit,
    ) -> anyhow::Result<ConnectionWaiter> {
        let discovery_config = self.discovery_config.read().await.clone();
        let _lifecycle = self.connection_lifecycle.lock().await;
        if self.shutting_down.load(Ordering::SeqCst) {
            anyhow::bail!("MCP manager is shutting down");
        }
        if matches!(
            *self.network_policy.read().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            anyhow::bail!("MCP reconnect blocked by network policy");
        }
        let client = self
            .clients
            .lock()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' not found", name))?;
        let reservation = {
            let mut active = self
                .active_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if active.contains_key(name) {
                anyhow::bail!("MCP client '{}' already has a connection in progress", name);
            }
            let generation = self
                .next_connection_generation
                .fetch_add(1, Ordering::SeqCst);
            let cancel = CancellationToken::new();
            let reservation = Arc::new(ConnectionReservation {
                generation,
                name: name.to_string(),
                client: client.clone(),
                cancel,
            });
            active.insert(name.to_string(), reservation.clone());
            reservation
        };

        let mut start_guard = ConnectionStartGuard::new(reservation.cancel.clone());
        let (waiter, start_tx) = self.spawn_connection_worker(
            reservation,
            ConnectionPurpose::Reconnect { discovery_config },
            permit,
        );
        self.stop_monitor_for_generation(name, &client).await;
        *client.status.lock().await = McpClientStatus::Connecting;
        let _ = self.status_tx.send(McpStatusChangeEvent {
            name: name.to_string(),
            status: McpClientStatus::Connecting,
        });
        let _ = start_tx.send(());
        start_guard.disarm();
        Ok(waiter)
    }

    /// Start health-monitor + auto-reconnect loops for all connected clients.
    /// Also stores the discovery config for later use by `connect_server`.
    pub async fn start_monitors(&self, config: &haven_common::config::McpDiscoveryConfig) {
        *self.discovery_config.write().await = config.clone();
        let _lifecycle = self.connection_lifecycle.lock().await;
        if self.shutting_down.load(Ordering::SeqCst)
            || matches!(
                *self.network_policy.read().await,
                haven_common::types::NetworkPolicy::Deny
            )
        {
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
            let name = client.name().to_string();
            if self
                .active_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&name)
            {
                continue;
            }
            if !self
                .clients
                .lock()
                .await
                .get(&name)
                .is_some_and(|current| Arc::ptr_eq(current, &client))
            {
                continue;
            }
            self.install_monitor(client, config).await;
        }
    }

    /// Combined: load config + connect + start health monitors.
    pub async fn discover_all(
        &self,
        servers: &[haven_common::config::McpServerConfig],
        config: &haven_common::config::McpDiscoveryConfig,
    ) {
        self.load_from_config(servers).await;
        self.start_monitors(config).await;
    }

    /// Shut down all clients and cancel their monitor sessions.
    pub async fn shutdown_all(&self) {
        let policy_watcher = self.stdio_policy_watcher.lock().await.take();
        if let Some(policy_watcher) = policy_watcher {
            policy_watcher.abort();
            let _ = policy_watcher.await;
        }
        {
            let _lifecycle = self.connection_lifecycle.lock().await;
            self.shutting_down.store(true, Ordering::SeqCst);
            for attempt in self
                .active_connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
            {
                attempt.cancel.cancel();
            }
        }

        let mut connection_tasks = {
            let mut tasks = self
                .connection_tasks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *tasks)
                .into_values()
                .collect::<Vec<_>>()
        };
        if tokio::time::timeout(CONNECTION_TASK_SHUTDOWN_TIMEOUT, async {
            for task in &mut connection_tasks {
                let _ = task.await;
            }
        })
        .await
        .is_err()
        {
            tracing::warn!(
                timeout = ?CONNECTION_TASK_SHUTDOWN_TIMEOUT,
                "MCP connection tasks did not drain; aborting before final cleanup"
            );
            for task in &connection_tasks {
                task.abort();
            }
            for task in connection_tasks {
                let _ = task.await;
            }
        }

        let clients = self
            .clients
            .lock()
            .await
            .iter()
            .map(|(name, client)| (name.clone(), client.clone()))
            .collect::<Vec<_>>();
        for (name, client) in clients {
            self.remove_client_generation(&name, Some(&client)).await;
        }

        let remaining_reservations = self
            .active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
            .map(|(_, reservation)| reservation)
            .collect::<Vec<_>>();
        for reservation in remaining_reservations {
            self.cleanup_connection_generation(reservation).await;
        }

        let monitors = self
            .monitors
            .lock()
            .await
            .drain()
            .map(|(_, monitor)| monitor)
            .collect::<Vec<_>>();
        for monitor in &monitors {
            monitor.task.abort();
        }
        for monitor in monitors {
            let _ = monitor.task.await;
        }

        let listeners = self.take_catalog_listeners();
        for listener in listeners {
            listener.client.notification_cancel.cancel();
            if let Err(error) = listener.client.shutdown().await {
                tracing::warn!(
                    server = %listener.client.name(),
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "MCP client shutdown failed while joining notification listener"
                );
            }
            join_notification_listener(listener.client.name(), listener.task).await;
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
        futures_util::future::join_all(clients.iter().map(|client| async move {
            let mut snapshot = client.snapshot().await;
            snapshot.target_binding = self.target_binding_for(client);
            snapshot
        }))
        .await
    }

    /// Manually trigger a reconnect for a client, bypassing the retry-limit.
    /// A successful reconnect also installs the manager-owned health monitor.
    pub async fn reconnect(&self, name: &str) -> anyhow::Result<()> {
        if matches!(
            self.network_policy().await,
            haven_common::types::NetworkPolicy::Deny
        ) {
            anyhow::bail!("MCP reconnect blocked by network policy");
        }
        let permit = self
            .connection_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|error| anyhow::anyhow!("MCP connection admission closed: {error}"))?;
        self.begin_reconnect(name, permit).await?.wait().await
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
                match client.refresh_tool_cache(true).await {
                    Ok((Some(_), _)) => {
                        // The client may have been added directly in tests or
                        // by a native host path, bypassing load_from_config.
                        // Refreshing its discovery cache still invalidates
                        // the model-facing catalog.
                    }
                    Ok((None, _)) => {
                        tracing::debug!(server = %name, "MCP tools refresh skipped before discovery completed");
                    }
                    Err(_) => {
                        tracing::warn!(server = %name, "MCP tools refresh failed");
                    }
                }
            }));
        }
        for h in handles {
            if h.await.is_err() {
                tracing::warn!("MCP tools refresh task failed");
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
        self.call_tool_on_client(client, tool_name, input, cancel)
            .await
    }

    /// Call only the connection generation that was included in the
    /// authorization request. Holding the selected client Arc prevents a
    /// concurrent replacement from redirecting this call to a new target.
    pub async fn call_tool_at_target_binding(
        &self,
        client_name: &str,
        expected_binding: &str,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpCallOutput> {
        let client = self
            .client_at_target_binding(client_name, expected_binding)
            .await?;
        self.call_tool_on_client(client, tool_name, input, cancel)
            .await
    }

    /// List the current resource catalog through the selected live connection.
    /// UI callers must pass the binding from the snapshot they acted on so a
    /// stale selection cannot be redirected to a replacement server.
    pub async fn list_resources_at_target_binding(
        &self,
        client_name: &str,
        expected_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceList> {
        self.client_at_target_binding(client_name, expected_binding)
            .await?
            .list_resources(force_refresh, cancel)
            .await
    }

    /// Read only the opaque URI selected by the UI from the selected live
    /// server. The MCP client never maps it to a local path or HTTP fetch.
    pub async fn read_resource_at_target_binding(
        &self,
        client_name: &str,
        expected_binding: &str,
        uri: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpResourceRead> {
        self.client_at_target_binding(client_name, expected_binding)
            .await?
            .read_resource(uri, force_refresh, cancel)
            .await
    }

    /// List prompts for explicit user selection; prompts are never model
    /// callable by this API.
    pub async fn list_prompts_at_target_binding(
        &self,
        client_name: &str,
        expected_binding: &str,
        force_refresh: bool,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPromptList> {
        self.client_at_target_binding(client_name, expected_binding)
            .await?
            .list_prompts(force_refresh, cancel)
            .await
    }

    /// Render a prompt only after the caller has selected its name and passed
    /// the prompt's declared string arguments.
    pub async fn get_prompt_at_target_binding(
        &self,
        client_name: &str,
        expected_binding: &str,
        name: &str,
        arguments: HashMap<String, String>,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpPrompt> {
        self.client_at_target_binding(client_name, expected_binding)
            .await?
            .get_prompt(name, arguments, cancel)
            .await
    }

    async fn call_tool_on_client(
        &self,
        client: Arc<McpClient>,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpCallOutput> {
        let (tools, refreshed) = client
            .ensure_tool_cache_fresh()
            .await
            .map_err(|error| anyhow::anyhow!("MCP tool catalog refresh failed: {error}"))?;
        if tools.is_none() {
            anyhow::bail!("MCP tool catalog is not available for '{}'", client.name());
        }
        if refreshed {
            self.bump_catalog_version();
        }
        client.call_tool(tool_name, input, cancel).await
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod manager_lifecycle_tests {
    use super::*;
    use haven_common::config::McpServerConfig;
    use haven_common::mcp::McpServerTransport;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::task::JoinSet;

    type MockRequestGate = Arc<Mutex<Option<(String, usize, oneshot::Receiver<()>)>>>;

    fn unreachable_http_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: McpServerTransport::Http,
            url: "http://127.0.0.1:1/mcp".to_string(),
            ..McpServerConfig::default()
        }
    }

    #[tokio::test]
    async fn mcp_target_binding_rejects_replacement_and_other_managers() {
        let config = unreachable_http_server("target-binding");
        let first_manager = McpManager::new();
        first_manager
            .add_client(Arc::new(McpClient::new(&config, 1024, 1024)))
            .await;
        let first_binding = first_manager
            .client_target_binding("target-binding")
            .await
            .unwrap();

        let second_manager = McpManager::new();
        second_manager
            .add_client(Arc::new(McpClient::new(&config, 1024, 1024)))
            .await;
        let second_binding = second_manager
            .client_target_binding("target-binding")
            .await
            .unwrap();
        assert_ne!(first_binding, second_binding);
        let error = second_manager
            .call_tool_at_target_binding(
                "target-binding",
                &first_binding,
                "probe",
                Value::Null,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<McpTargetChanged>().is_some());
        let error = second_manager
            .list_resources_at_target_binding(
                "target-binding",
                &first_binding,
                false,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<McpTargetChanged>().is_some());

        second_manager.remove_client("target-binding").await;
        second_manager
            .add_client(Arc::new(McpClient::new(&config, 1024, 1024)))
            .await;
        let replacement_binding = second_manager
            .client_target_binding("target-binding")
            .await
            .unwrap();
        assert_ne!(second_binding, replacement_binding);
        let error = second_manager
            .call_tool_at_target_binding(
                "target-binding",
                &second_binding,
                "probe",
                Value::Null,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<McpTargetChanged>().is_some());
    }

    #[tokio::test]
    async fn target_binding_is_unavailable_for_a_different_live_transport() {
        let config = McpServerConfig {
            name: "transport-bound".into(),
            transport: McpServerTransport::Stdio,
            command: "not-spawned-in-this-test".into(),
            ..Default::default()
        };
        let manager = McpManager::new();
        manager
            .add_client(Arc::new(McpClient::new(&config, 1024, 1024)))
            .await;

        assert!(
            manager
                .client_target_binding_for_transport("transport-bound", &McpServerTransport::Stdio)
                .await
                .is_some()
        );
        assert!(
            manager
                .client_target_binding_for_transport("transport-bound", &McpServerTransport::Http)
                .await
                .is_none()
        );
    }

    struct MockHttpServer {
        address: std::net::SocketAddr,
        methods: tokio::sync::mpsc::UnboundedReceiver<String>,
        session_ids: tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
        cancel: CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    impl MockHttpServer {
        async fn start(gated_method: Option<&str>) -> (Self, Option<oneshot::Sender<()>>) {
            Self::start_on_occurrence(gated_method, 1).await
        }

        async fn start_legacy() -> Self {
            Self::start_on_occurrence_mode(None, 1, true).await.0
        }

        async fn start_on_occurrence(
            gated_method: Option<&str>,
            occurrence: usize,
        ) -> (Self, Option<oneshot::Sender<()>>) {
            Self::start_on_occurrence_mode(gated_method, occurrence, false).await
        }

        async fn start_on_occurrence_mode(
            gated_method: Option<&str>,
            occurrence: usize,
            legacy_probe: bool,
        ) -> (Self, Option<oneshot::Sender<()>>) {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind local MCP test endpoint");
            let address = listener.local_addr().expect("read test endpoint address");
            let cancel = CancellationToken::new();
            let server_cancel = cancel.clone();
            let (method_tx, methods) = tokio::sync::mpsc::unbounded_channel();
            let (session_id_tx, session_ids) = tokio::sync::mpsc::unbounded_channel();
            let (release_tx, release_rx) = oneshot::channel();
            let gate = gated_method.map(|method| {
                Arc::new(Mutex::new(Some((
                    method.to_string(),
                    occurrence.max(1),
                    release_rx,
                ))))
            });
            let server_gate = gate.clone();
            let task = tokio::spawn(async move {
                let mut handlers = JoinSet::new();
                loop {
                    tokio::select! {
                        _ = server_cancel.cancelled() => break,
                        accepted = listener.accept() => match accepted {
                            Ok((stream, _)) => {
                                let method_tx = method_tx.clone();
                                let gate = server_gate.clone();
                                let cancel = server_cancel.clone();
                                let legacy_probe = legacy_probe;
                                let session_id_tx = session_id_tx.clone();
                                handlers.spawn(async move {
                                    serve_mcp_request(
                                        stream,
                                        method_tx,
                                        session_id_tx,
                                        gate,
                                        cancel,
                                        legacy_probe,
                                    )
                                    .await;
                                });
                            }
                            Err(_) => break,
                        },
                        Some(_) = handlers.join_next(), if !handlers.is_empty() => {}
                    }
                }
                handlers.abort_all();
                while handlers.join_next().await.is_some() {}
            });
            (
                Self {
                    address,
                    methods,
                    session_ids,
                    cancel,
                    task,
                },
                gated_method.map(|_| release_tx),
            )
        }

        fn config(&self, name: &str) -> McpServerConfig {
            McpServerConfig {
                name: name.to_string(),
                transport: McpServerTransport::Http,
                url: format!("http://{}/mcp", self.address),
                ..McpServerConfig::default()
            }
        }

        async fn wait_for_method(&mut self, expected: &str) {
            tokio::time::timeout(Duration::from_secs(3), async {
                while let Some(method) = self.methods.recv().await {
                    if method == expected {
                        return;
                    }
                }
                panic!("mock MCP endpoint stopped before receiving {expected}");
            })
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for MCP method {expected}"));
        }

        async fn wait_for_session_id(&mut self, expected: &str) {
            tokio::time::timeout(Duration::from_secs(3), async {
                while let Some(session_id) = self.session_ids.recv().await {
                    if session_id.as_deref() == Some(expected) {
                        return;
                    }
                }
                panic!("mock MCP endpoint stopped before receiving the session id");
            })
            .await
            .expect("timed out waiting for the MCP session header");
        }

        async fn shutdown(self) {
            self.cancel.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
        }
    }

    async fn serve_mcp_request(
        stream: TcpStream,
        method_tx: tokio::sync::mpsc::UnboundedSender<String>,
        session_id_tx: tokio::sync::mpsc::UnboundedSender<Option<String>>,
        gate: Option<MockRequestGate>,
        cancel: CancellationToken,
        legacy_probe: bool,
    ) {
        let (read_half, mut writer) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let Ok(Ok(length)) =
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await
        else {
            return;
        };
        if length == 0 {
            return;
        }
        let is_get = line.starts_with("GET ");
        let mut content_length = 0usize;
        let mut session_id = None;
        loop {
            line.clear();
            let Ok(Ok(length)) =
                tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await
            else {
                return;
            };
            if length == 0 || line == "\r\n" {
                break;
            }
            if let Some((header, value)) = line.split_once(':')
                && header.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap_or_default();
            }
            if let Some((header, value)) = line.split_once(':')
                && header.eq_ignore_ascii_case("mcp-session-id")
            {
                session_id = Some(value.trim().to_string());
            }
        }
        if is_get {
            let _ = writer
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return;
        }

        let mut body = vec![0; content_length];
        if reader.read_exact(&mut body).await.is_err() {
            return;
        }
        let Ok(request) = serde_json::from_slice::<Value>(&body) else {
            return;
        };
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return;
        };
        let _ = session_id_tx.send(session_id);
        let _ = method_tx.send(method.to_string());
        if let Some(gate) = gate {
            let receiver = {
                let mut gate = gate.lock().await;
                let should_take = if let Some((gated_method, remaining, _)) = gate.as_mut() {
                    if gated_method == method {
                        if *remaining > 1 {
                            *remaining -= 1;
                            false
                        } else {
                            true
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if should_take {
                    gate.take().map(|(_, _, receiver)| receiver)
                } else {
                    None
                }
            };
            if let Some(receiver) = receiver {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = receiver => {}
                }
            }
        }

        if method == "server/discover" && legacy_probe {
            let _ = writer
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return;
        }

        let (status, response) = match method {
            "server/discover" => (
                "200 OK",
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "result": {
                        "resultType": "complete",
                        "supportedVersions": [crate::protocol::MODERN_PROTOCOL_VERSION],
                        "capabilities": {"tools": {}},
                        "_meta": {
                            "io.modelcontextprotocol/serverInfo": {
                                "name": "manager-test-server",
                                "version": "1"
                            }
                        }
                    }
                })
                .to_string(),
            ),
            "initialize" => (
                "200 OK",
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "result": {
                        "protocolVersion": crate::protocol::LEGACY_PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "manager-test-server", "version": "1"}
                    }
                })
                .to_string(),
            ),
            "tools/list" => (
                "200 OK",
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": request.get("id"),
                    "result": {
                        "tools": [],
                        "ttlMs": 60_000,
                        "cacheScope": "private"
                    }
                })
                .to_string(),
            ),
            "notifications/initialized" => ("202 Accepted", String::new()),
            _ => ("202 Accepted", String::new()),
        };
        let content_type = if response.is_empty() {
            String::new()
        } else {
            "Content-Type: application/json\r\n".to_string()
        };
        let session_header = if legacy_probe && method == "initialize" {
            "Mcp-Session-Id: manager-test-session\r\n"
        } else {
            ""
        };
        let response_bytes = format!(
            "HTTP/1.1 {status}\r\n{content_type}{session_header}Content-Length: {}\r\nConnection: close\r\n\r\n{response}",
            response.len()
        );
        let _ = writer.write_all(response_bytes.as_bytes()).await;
    }

    async fn wait_for_manager_cleanup(manager: &McpManager, name: &str) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let listeners_empty = manager
                    .notification_listeners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty();
                let connections_empty = manager
                    .active_connections
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty();
                let tasks_empty = manager
                    .connection_tasks
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty();
                if !manager.has_client(name).await
                    && listeners_empty
                    && connections_empty
                    && tasks_empty
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("manager-owned connection cleanup should finish");
    }

    #[tokio::test]
    async fn legacy_http_peer_uses_initialize_fallback_and_session_header() {
        let manager = McpManager::new_with_network_policy(haven_common::types::NetworkPolicy::Open);
        let mut server = MockHttpServer::start_legacy().await;

        manager
            .connect_server(&server.config("legacy-http"))
            .await
            .expect("legacy MCP HTTP peers remain supported");

        server.wait_for_method("server/discover").await;
        server.wait_for_method("initialize").await;
        server.wait_for_method("notifications/initialized").await;
        server.wait_for_method("tools/list").await;
        server.wait_for_session_id("manager-test-session").await;
        assert!(matches!(
            manager.client_status("legacy-http").await,
            McpClientStatus::Connected
        ));

        manager.shutdown_all().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn initial_connection_panic_cleans_the_provisional_generation() {
        let manager = McpManager::new_with_network_policy(haven_common::types::NetworkPolicy::Open);
        manager.panic_next_connection.store(true, Ordering::SeqCst);

        manager
            .load_from_config(&[unreachable_http_server("initial-panic")])
            .await;

        wait_for_manager_cleanup(&manager, "initial-panic").await;
        assert!(matches!(
            manager.client_status("initial-panic").await,
            McpClientStatus::Disconnected
        ));
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn aborting_config_caller_cancels_and_cleans_initial_connection() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, _release) = MockHttpServer::start(Some("server/discover")).await;
        let config = server.config("initial-caller-abort");
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.load_from_config(&[config]).await })
        };

        server.wait_for_method("server/discover").await;
        task.abort();
        assert!(
            task.await
                .expect_err("caller should be aborted")
                .is_cancelled()
        );
        wait_for_manager_cleanup(&manager, "initial-caller-abort").await;
        server.shutdown().await;
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn denied_policy_cancels_an_inflight_dynamic_connection() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, _release) = MockHttpServer::start(Some("server/discover")).await;
        let config = server.config("dynamic-denied");
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.connect_server(&config).await })
        };

        server.wait_for_method("server/discover").await;
        manager
            .set_network_policy(haven_common::types::NetworkPolicy::Deny)
            .await;
        assert!(
            task.await
                .expect("dynamic caller task should finish")
                .is_err()
        );
        wait_for_manager_cleanup(&manager, "dynamic-denied").await;
        assert!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        server.shutdown().await;
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn denied_policy_cancels_a_manual_reconnect_generation() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, _release) =
            MockHttpServer::start_on_occurrence(Some("server/discover"), 2).await;
        let config = server.config("reconnect-denied");
        manager
            .connect_server(&config)
            .await
            .expect("initial connection should succeed");
        while server.methods.try_recv().is_ok() {}

        let reconnect = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.reconnect("reconnect-denied").await })
        };
        server.wait_for_method("server/discover").await;
        manager
            .set_network_policy(haven_common::types::NetworkPolicy::Deny)
            .await;
        assert!(
            reconnect
                .await
                .expect("manual reconnect caller should finish")
                .is_err()
        );
        wait_for_manager_cleanup(&manager, "reconnect-denied").await;
        server.shutdown().await;
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn duplicate_dynamic_name_is_reserved_before_connecting() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, release) = MockHttpServer::start(Some("server/discover")).await;
        let config = server.config("same-name");
        let first = {
            let manager = manager.clone();
            let config = config.clone();
            tokio::spawn(async move { manager.connect_server(&config).await })
        };

        server.wait_for_method("server/discover").await;
        let duplicate_error = manager
            .connect_server(&config)
            .await
            .expect_err("a concurrent same-name connection must be rejected");
        assert!(duplicate_error.to_string().contains("already loaded"));
        assert_eq!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );

        release.expect("gate configured").send(()).unwrap();
        first
            .await
            .expect("first caller task should finish")
            .expect("first connection should succeed");
        assert!(manager.has_client("same-name").await);
        manager.shutdown_all().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn late_cleanup_of_old_generation_does_not_remove_same_name_replacement() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, release) = MockHttpServer::start(Some("server/discover")).await;
        let config = server.config("generation-identity");
        let first = {
            let manager = manager.clone();
            let config = config.clone();
            tokio::spawn(async move { manager.connect_server(&config).await })
        };

        server.wait_for_method("server/discover").await;
        let old_generation = manager
            .active_connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get("generation-identity")
            .cloned()
            .expect("in-flight generation should be reserved");
        manager.remove_client("generation-identity").await;
        assert!(
            first
                .await
                .expect("first connection caller should finish")
                .is_err()
        );
        release.expect("gate configured").send(()).unwrap();
        wait_for_manager_cleanup(&manager, "generation-identity").await;

        manager
            .connect_server(&config)
            .await
            .expect("replacement generation should connect");
        let replacement = manager
            .client("generation-identity")
            .await
            .expect("replacement client should remain managed");
        assert!(!Arc::ptr_eq(&replacement, &old_generation.client));

        manager.cleanup_connection_generation(old_generation).await;
        assert!(
            manager
                .client("generation-identity")
                .await
                .is_some_and(|current| Arc::ptr_eq(&current, &replacement))
        );
        assert_eq!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        manager.shutdown_all().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn aborting_dynamic_caller_cleans_listener_and_transport() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, _release) = MockHttpServer::start(Some("tools/list")).await;
        let config = server.config("dynamic-caller-abort");
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.connect_server(&config).await })
        };

        server.wait_for_method("tools/list").await;
        task.abort();
        assert!(
            task.await
                .expect_err("caller should be aborted")
                .is_cancelled()
        );
        wait_for_manager_cleanup(&manager, "dynamic-caller-abort").await;
        manager.shutdown_all().await;
        server.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_fences_admission_and_joins_inflight_connection_worker() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let (mut server, _release) = MockHttpServer::start(Some("server/discover")).await;
        let config = server.config("shutdown-inflight");
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move { manager.connect_server(&config).await })
        };

        server.wait_for_method("server/discover").await;
        tokio::time::timeout(Duration::from_secs(3), manager.shutdown_all())
            .await
            .expect("shutdown should cancel and join active connection workers");
        assert!(
            task.await
                .expect("dynamic caller task should finish")
                .is_err()
        );
        wait_for_manager_cleanup(&manager, "shutdown-inflight").await;
        assert!(
            manager
                .connect_server(&server.config("after-shutdown"))
                .await
                .is_err()
        );
        server.shutdown().await;
    }

    async fn acquire_all_connection_slots(
        manager: &McpManager,
    ) -> Vec<tokio::sync::OwnedSemaphorePermit> {
        let mut permits = Vec::with_capacity(crate::MCP_MAX_PARALLEL_CONNECTIONS);
        for _ in 0..crate::MCP_MAX_PARALLEL_CONNECTIONS {
            permits.push(
                manager
                    .connection_slots
                    .clone()
                    .acquire_owned()
                    .await
                    .expect("manager connection admission remains open"),
            );
        }
        permits
    }

    #[tokio::test]
    async fn failed_dynamic_connect_joins_its_notification_listener() {
        let manager = McpManager::new_with_network_policy(haven_common::types::NetworkPolicy::Open);

        assert!(
            manager
                .connect_server(&unreachable_http_server("failed-connect"))
                .await
                .is_err()
        );

        assert!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
        assert!(!manager.has_client("failed-connect").await);
    }

    #[tokio::test]
    async fn shutdown_all_joins_listeners_for_failed_config_connections() {
        let manager = McpManager::new_with_network_policy(haven_common::types::NetworkPolicy::Open);
        manager
            .load_from_config(&[unreachable_http_server("offline-config")])
            .await;

        assert_eq!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        manager.shutdown_all().await;
        assert!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn dynamic_connect_waits_for_manager_wide_admission() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let mut permits = acquire_all_connection_slots(&manager).await;
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .connect_server(&unreachable_http_server("queued-dynamic"))
                    .await
            })
        };

        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        assert!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        drop(permits.pop());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if task.is_finished()
                    || !manager
                        .notification_listeners
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a released slot lets the dynamic connection path proceed");
        task.abort();
        let _ = task.await;
        manager.shutdown_all().await;
        assert!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }

    #[tokio::test]
    async fn denied_connection_and_reconnect_fail_before_waiting_for_admission() {
        let manager = McpManager::new_with_network_policy(haven_common::types::NetworkPolicy::Deny);
        let permits = acquire_all_connection_slots(&manager).await;

        let connect = tokio::time::timeout(
            Duration::from_millis(100),
            manager.connect_server(&unreachable_http_server("denied-before-admission")),
        )
        .await
        .expect("policy denial must not wait for a connection slot");
        assert!(connect.is_err());

        let reconnect = tokio::time::timeout(
            Duration::from_millis(100),
            manager.reconnect("missing-under-deny"),
        )
        .await
        .expect("policy denial must not wait for a connection slot");
        assert!(reconnect.is_err());

        drop(permits);
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn config_loading_waits_before_creating_clients_when_admission_is_full() {
        let manager = Arc::new(McpManager::new_with_network_policy(
            haven_common::types::NetworkPolicy::Open,
        ));
        let mut permits = acquire_all_connection_slots(&manager).await;
        let task = {
            let manager = manager.clone();
            tokio::spawn(async move {
                manager
                    .load_from_config(&[unreachable_http_server("queued-config")])
                    .await;
            })
        };

        tokio::task::yield_now().await;
        assert!(manager.list_clients().await.is_empty());
        assert!(
            manager
                .notification_listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        drop(permits.pop());
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if manager.has_client("queued-config").await {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a released slot lets config loading proceed");
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .expect("the admitted config attempt should finish")
            .expect("config loading task should not panic");
        assert!(manager.has_client("queued-config").await);
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn policy_change_removes_stdio_clients_but_keeps_http_clients() {
        let manager = McpManager::new();
        let stdio_config = McpServerConfig {
            name: "revoked-stdio".into(),
            transport: McpServerTransport::Stdio,
            command: "unused-test-command".into(),
            ..McpServerConfig::default()
        };
        let http_config = unreachable_http_server("retained-http");
        manager
            .add_client(Arc::new(McpClient::new(&stdio_config, 1024, 1024)))
            .await;
        manager
            .add_client(Arc::new(McpClient::new(&http_config, 1024, 1024)))
            .await;

        let (signal, receiver) = tokio::sync::watch::channel(Arc::new(()));
        manager.watch_stdio_policy_changes(receiver).await;
        signal.send_replace(Arc::new(()));

        tokio::time::timeout(Duration::from_secs(2), async {
            while manager.has_client("revoked-stdio").await {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("policy change must drain stale stdio clients");
        assert!(manager.has_client("retained-http").await);
        manager.shutdown_all().await;
    }
}
