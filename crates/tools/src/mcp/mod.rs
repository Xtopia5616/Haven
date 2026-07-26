use crate::ToolResult;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;


// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

static PROTOCOL_VERSION: &str = "2024-11-05";
static REQUEST_TIMEOUT_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 helpers
// ---------------------------------------------------------------------------

fn jsonrpc_request(id: u64, method: &str, params: Option<Value>) -> Value {
    let mut req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    });
    if let Some(p) = params {
        req["params"] = p;
    }
    req
}

fn jsonrpc_notification(method: &str, params: Option<Value>) -> Value {
    let mut req = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
    });
    if let Some(p) = params {
        req["params"] = p;
    }
    req
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum McpClientStatus {
    Disconnected,
    Connecting,
    Connected,
    Offline { error: String },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpServerSnapshot {
    pub name: String,
    pub transport: String,
    pub status: McpClientStatus,
    pub tools: Vec<McpToolInfo>,
    pub last_error: Option<String>,
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStatusChangeEvent {
    pub name: String,
    pub status: McpClientStatus,
}

// ---------------------------------------------------------------------------
// McpClient — single MCP server connection via stdio
// ---------------------------------------------------------------------------

struct McpClientInner {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
}

impl McpClientInner {
    async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        let req = jsonrpc_request(id, method, params);
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let timeout = tokio::time::Duration::from_secs(REQUEST_TIMEOUT_SECS);
        let result = tokio::time::timeout(timeout, async {
            loop {
                let mut buf = String::new();
                self.stdout.read_line(&mut buf).await?;
                let buf = buf.trim().to_string();
                if buf.is_empty() {
                    anyhow::bail!("MCP server stdout closed (process died)");
                }
                let parsed: Value = serde_json::from_str(&buf)?;
                if parsed.get("id").and_then(|v| v.as_u64()) == Some(id) {
                    if let Some(err) = parsed.get("error") {
                        let code = err["code"].as_i64().unwrap_or(-1);
                        let msg = err["message"].as_str().unwrap_or("unknown error");
                        anyhow::bail!("MCP error ({}): {}", code, msg);
                    }
                    return Ok(parsed["result"].clone());
                }
                // Route non-matching responses to notification handler (refine §4.6)
                if parsed.get("id").is_none() {
                    let _ = self.notification_tx.send(parsed);
                }
            }
        });

        match result.await {
            Ok(r) => r,
            Err(_) => anyhow::bail!("MCP request '{}' timed out", method),
        }
    }

    async fn notify(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        let notification = jsonrpc_notification(method, params);
        let mut line = serde_json::to_string(&notification)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

impl Drop for McpClientInner {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

pub struct McpClient {
    name: String,
    command: String,
    args: Vec<String>,
    env: Vec<String>,
    inner: Arc<Mutex<Option<McpClientInner>>>,
    status: Arc<Mutex<McpClientStatus>>,
    next_id: AtomicU64,
    tools_cache: Arc<Mutex<Option<Vec<McpToolInfo>>>>,
    last_error: Arc<Mutex<Option<String>>>,
    last_seen_at: Arc<Mutex<Option<i64>>>,
    reconnect_retries: Arc<Mutex<u32>>,
    cancel_token: Arc<Mutex<CancellationToken>>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    notification_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Value>>>>,
}

struct RateLimiter {
    tokens: f64,
    last_refill: Instant,
    capacity: f64,
    refill_rate: f64,
}

impl RateLimiter {
    fn new(calls_per_second: f64) -> Self {
        Self {
            tokens: calls_per_second,
            last_refill: Instant::now(),
            capacity: calls_per_second,
            refill_rate: calls_per_second,
        }
    }

    fn acquire(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

impl McpClient {
    pub fn new(name: &str, command: &str, args: &[String], env: &[String]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.to_vec(),
            env: env.to_vec(),
            inner: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(McpClientStatus::Disconnected)),
            next_id: AtomicU64::new(1),
            tools_cache: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(None)),
            last_seen_at: Arc::new(Mutex::new(None)),
            reconnect_retries: Arc::new(Mutex::new(0)),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new(10.0))),
            notification_rx: Arc::new(Mutex::new(None)),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn status(&self) -> McpClientStatus {
        self.status.lock().await.clone()
    }

    pub async fn tools_cache(&self) -> Vec<McpToolInfo> {
        self.tools_cache.lock().await.clone().unwrap_or_default()
    }

    pub async fn last_error(&self) -> Option<String> {
        self.last_error.lock().await.clone()
    }

    pub async fn last_seen_at(&self) -> Option<i64> {
        *self.last_seen_at.lock().await
    }

    pub async fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.lock().await.clone()
    }

    pub async fn snapshot(&self) -> McpServerSnapshot {
        McpServerSnapshot {
            name: self.name.clone(),
            transport: "stdio".into(),
            status: self.status.lock().await.clone(),
            tools: self.tools_cache.lock().await.clone().unwrap_or_default(),
            last_error: self.last_error.lock().await.clone(),
            last_seen_at: *self.last_seen_at.lock().await,
        }
    }

    pub async fn connect(&self) -> anyhow::Result<()> {
        *self.status.lock().await = McpClientStatus::Connecting;

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args);
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        for e in &self.env {
            if let Some((key, val)) = e.split_once('=') {
                cmd.env(key, val);
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn MCP server '{}': {}", self.name, e))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdin for '{}'", self.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout for '{}'", self.name))?;

        let (notification_tx, new_rx) = tokio::sync::mpsc::unbounded_channel();
        *self.notification_rx.lock().await = Some(new_rx);

        let mut inner = McpClientInner {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            notification_tx,
        };

        // Initialize handshake
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = inner
            .request(
                id,
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "clientInfo": {
                        "name": "haven",
                        "version": "0.1.0",
                    },
                })),
            )
            .await?;

        let server_name = result["serverInfo"]["name"].as_str().unwrap_or("?");
        let server_version = result["serverInfo"]["version"].as_str().unwrap_or("?");
        tracing::info!(
            "MCP server '{}' connected: {} v{}",
            self.name, server_name, server_version
        );

        // Send initialized notification
        inner.notify("notifications/initialized", None).await?;

        *self.inner.lock().await = Some(inner);
        *self.status.lock().await = McpClientStatus::Connected;

        // Cache tools after successful connection
        if let Ok(tools) = self.list_tools().await {
            *self.tools_cache.lock().await = Some(tools);
        }

        *self.last_error.lock().await = None;
        *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());

        Ok(())
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        let mut guard = self.inner.lock().await;
        if let Some(ref mut inner) = *guard {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let _ = inner.request(id, "shutdown", None).await;
            let _ = inner.notify("exit", None).await;
            let _ = inner.child.start_kill();
            let _ = inner.child.wait().await;
        }
        *guard = None;
        *self.status.lock().await = McpClientStatus::Disconnected;
        Ok(())
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<McpToolInfo>> {
        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' is not connected", self.name))?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = inner.request(id, "tools/list", None).await?;

        let tools: Vec<McpToolInfo> = serde_json::from_value(result["tools"].clone())
            .map_err(|e| anyhow::anyhow!("invalid tools/list from '{}': {}", self.name, e))?;
        Ok(tools)
    }

    /// Set calls-per-second rate limit for this client (refine §4.5).
    pub async fn set_rate_limit(&self, calls_per_second: f64) {
        let mut rl = self.rate_limiter.lock().await;
        rl.capacity = calls_per_second;
        rl.refill_rate = calls_per_second;
    }

    /// Register a callback for `notifications/tools/list_changed` (refine §4.6).
    pub fn start_notification_listener(
        self: Arc<McpClient>,
        on_tool_list_changed: impl Fn(&str) + Send + Sync + 'static,
    ) {
        tokio::spawn(async move {
            loop {
                let notification = {
                    let mut rx_guard = self.notification_rx.lock().await;
                    match rx_guard.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                };

                match notification {
                    Some(msg) => {
                        let method = msg["method"].as_str();
                        if method == Some("notifications/tools/list_changed") {
                            on_tool_list_changed(&self.name);
                            // Refresh tools cache
                            if let Ok(tools) = self.list_tools().await {
                                *self.tools_cache.lock().await = Some(tools);
                            }
                        }
                    }
                    None => {
                        // Channel closed (reconnect case)
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        });
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        input: Value,
        _cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        // Rate limiting (refine §4.5)
        {
            let mut rl = self.rate_limiter.lock().await;
            if !rl.acquire() {
                let wait = Duration::from_secs_f64(1.0 / rl.refill_rate);
                drop(rl);
                tokio::time::sleep(wait).await;
            }
        }

        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' is not connected", self.name))?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = inner
            .request(
                id,
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": input,
                })),
            )
            .await?;

        let content = result["content"].as_array().cloned().unwrap_or_default();
        let mut text_parts = Vec::new();
        for item in &content {
            if item["type"] == "text"
                && let Some(t) = item["text"].as_str()
            {
                text_parts.push(t.to_string());
            }
        }

        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let text = text_parts.join("\n");

        if is_error {
            Ok(ToolResult {
                success: false,
                output: serde_json::json!({"text": text}),
                error: Some(text),
                truncated: false,
            })
        } else {
            Ok(ToolResult {
                success: true,
                output: serde_json::json!({"text": text}),
                error: None,
                truncated: false,
            })
        }
    }

    pub async fn is_alive(&self) -> bool {
        let mut guard = self.inner.lock().await;
        match guard.as_mut() {
            Some(inner) => inner.child.try_wait().map(|s| s.is_none()).unwrap_or(false),
            None => false,
        }
    }

    pub async fn reconnect(&self) -> anyhow::Result<()> {
        // Cancel any ongoing monitor task
        self.cancel_token.lock().await.cancel();
        // Create a fresh token for the next monitor
        *self.cancel_token.lock().await = CancellationToken::new();

        *self.reconnect_retries.lock().await = 0;
        *self.status.lock().await = McpClientStatus::Connecting;

        self.shutdown().await.ok();
        self.connect().await?;

        // Refresh tools cache
        if let Ok(tools) = self.list_tools().await {
            *self.tools_cache.lock().await = Some(tools);
        }

        *self.last_error.lock().await = None;
        *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());

        Ok(())
    }

    /// Spawn a background health-monitor + auto-reconnect task.
    ///
    /// Every `health_interval` the task calls `is_alive()`. If the process is
    /// dead it enters the reconnect loop (exponential backoff). The loop stops
    /// when the client's cancel token is cancelled (e.g. via `shutdown_all`).
    /// After `max_retries` consecutive failures the client remains `Offline`
    /// and waits for a manual `reconnect()` call.
    ///
    /// The task terminates when the cancel token fires.
    pub fn spawn_monitor(
        self: Arc<McpClient>,
        health_interval: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
        max_retries: u32,
        status_tx: tokio::sync::broadcast::Sender<McpStatusChangeEvent>,
    ) {
        tokio::spawn(async move {
            loop {
                let cancel = self.cancel_token.lock().await.clone();
                if cancel.is_cancelled() {
                    break;
                }

                tokio::select! {
                    _ = tokio::time::sleep(health_interval) => {}
                    _ = cancel.cancelled() => break,
                }

                if self.cancel_token.lock().await.is_cancelled() {
                    break;
                }

                let alive = self.is_alive().await;
                if !alive {
                    // Transition to Offline
                    *self.status.lock().await = McpClientStatus::Offline {
                        error: "connection lost".into(),
                    };
                    *self.last_error.lock().await = Some("connection lost".into());
                    let _ = status_tx.send(McpStatusChangeEvent {
                        name: self.name.clone(),
                        status: McpClientStatus::Offline {
                            error: "connection lost".into(),
                        },
                    });

                    // Reconnect loop with exponential backoff
                    let mut backoff = initial_backoff;
                    let mut retries = 0u32;

                    while retries < max_retries {
                        if self.cancel_token.lock().await.is_cancelled() {
                            return;
                        }

                        tokio::time::sleep(backoff).await;

                        if self.cancel_token.lock().await.is_cancelled() {
                            return;
                        }

                        *self.status.lock().await = McpClientStatus::Connecting;
                        let _ = status_tx.send(McpStatusChangeEvent {
                            name: self.name.clone(),
                            status: McpClientStatus::Connecting,
                        });

                        let shutdown_ok = self.shutdown().await;
                        if let Err(e) = &shutdown_ok {
                            tracing::warn!("reconnect shutdown cleanup: {e}");
                        }

                        match self.connect().await {
                            Ok(()) => {
                                *self.reconnect_retries.lock().await = 0;
                                *self.last_error.lock().await = None;
                                *self.last_seen_at.lock().await =
                                    Some(chrono::Utc::now().timestamp());
                                let _ = status_tx.send(McpStatusChangeEvent {
                                    name: self.name.clone(),
                                    status: McpClientStatus::Connected,
                                });
                                break;
                            }
                            Err(e) => {
                                retries += 1;
                                *self.reconnect_retries.lock().await = retries;
                                *self.last_error.lock().await =
                                    Some(format!("reconnect failed: {e}"));
                                backoff = (backoff * 2).min(max_backoff);
                                let _ = status_tx.send(McpStatusChangeEvent {
                                    name: self.name.clone(),
                                    status: McpClientStatus::Offline {
                                        error: format!("reconnect failed: {e}"),
                                    },
                                });
                            }
                        }
                    }

                    // If we exhausted retries, mark as Disconnected with no more auto-reconnect
                    if retries >= max_retries {
                        *self.status.lock().await = McpClientStatus::Disconnected;
                        *self.last_error.lock().await = Some("max retries reached".into());
                        let _ = status_tx.send(McpStatusChangeEvent {
                            name: self.name.clone(),
                            status: McpClientStatus::Disconnected,
                        });
                    }
                } else {
                    *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());
                }
            }
        });
    }
}

// ---------------------------------------------------------------------------
// McpManager — manages multiple MCP clients
// ---------------------------------------------------------------------------

pub struct McpManager {
    clients: Arc<Mutex<HashMap<String, Arc<McpClient>>>>,
    status_tx: tokio::sync::broadcast::Sender<McpStatusChangeEvent>,
}

impl Clone for McpManager {
    fn clone(&self) -> Self {
        Self {
            clients: self.clients.clone(),
            status_tx: self.status_tx.clone(),
        }
    }
}

impl McpManager {
    pub fn new() -> Self {
        let (status_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            clients: Arc::new(Mutex::new(HashMap::new())),
            status_tx,
        }
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

    /// Load MCP servers from config, create clients, and connect them.
    /// Does NOT start health monitors — call `start_monitors` separately
    /// or use `discover_all` for the combined operation.
    pub async fn load_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        let mut pending = tokio::task::JoinSet::new();

        for server in servers {
            if !server.enabled {
                continue;
            }
            let client = Arc::new(McpClient::new(
                &server.name,
                &server.command,
                &server.args,
                &server.env,
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

            let status_tx = self.status_tx.clone();
            pending.spawn(async move {
                let result = client.connect().await;
                (name, client, status_tx, result)
            });
        }

        while let Some(result) = pending.join_next().await {
            match result {
                Ok((name, client, status_tx, Ok(()))) => {
                    tracing::info!("MCP server '{}' connected successfully", name);
                    let _ = status_tx.send(McpStatusChangeEvent {
                        name: name.clone(),
                        status: McpClientStatus::Connected,
                    });
                    drop(client);
                }
                Ok((name, client, status_tx, Err(e))) => {
                    tracing::warn!(
                        "MCP server '{}' failed to connect: {} (will retry later)",
                        name, e
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
                    tracing::warn!("MCP connection task failed: {}", e);
                }
            }
        }
    }

    /// Dynamically connect a single MCP server from config.
    /// Used by the `load_mcp` builtin tool. Returns an error if already connected.
    pub async fn connect_server(&self, config: &haven_common::McpServerConfig) -> anyhow::Result<()> {
        let name = &config.name;
        {
            let clients = self.clients.lock().await;
            if clients.contains_key(name) {
                anyhow::bail!("MCP server '{}' is already loaded", name);
            }
        }

        let client = Arc::new(McpClient::new(
            name,
            &config.command,
            &config.args,
            &config.env,
        ));

        let listener_client = client.clone();
        listener_client.start_notification_listener(move |server_name: &str| {
            tracing::info!("MCP server '{}' pushed tools/list_changed", server_name);
        });

        client.connect().await.map_err(|e| {
            anyhow::anyhow!("failed to connect MCP server '{}': {}", name, e)
        })?;

        self.clients.lock().await.insert(name.clone(), client);
        let _ = self.status_tx.send(McpStatusChangeEvent {
            name: name.clone(),
            status: McpClientStatus::Connected,
        });

        Ok(())
    }

    /// Start health-monitor + auto-reconnect loops for all connected clients.
    pub async fn start_monitors(&self, config: &haven_common::McpDiscoveryConfig) {
        let clients = self.clients.lock().await;
        let health_interval = Duration::from_secs(config.health_interval_secs);
        let initial_backoff = Duration::from_millis(config.reconnect_initial_ms);
        let max_backoff = Duration::from_millis(config.reconnect_max_ms);
        let max_retries = config.reconnect_max_retries;

        for (_, client) in clients.iter() {
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

    /// Shut down all clients and cancel their monitor tasks.
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
        for (_, client) in clients.iter() {
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
        for (_, client) in clients.iter() {
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

    pub async fn list_all_tools(&self) -> Vec<(String, Vec<McpToolInfo>)> {
        let clients = self.clients.lock().await;
        let mut all_tools = Vec::new();
        for (name, client) in clients.iter() {
            let tools = client.tools_cache().await;
            all_tools.push((name.clone(), tools));
        }
        all_tools
    }

    pub async fn call_tool(
        &self,
        client_name: &str,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<ToolResult> {
        let clients = self.clients.lock().await;
        match clients.get(client_name) {
            Some(client) => client.call_tool(tool_name, input, cancel).await,
            None => anyhow::bail!("MCP client '{}' not found", client_name),
        }
    }
}

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn jsonrpc_request_basic() {
        let req = jsonrpc_request(1, "initialize", None);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "initialize");
        assert!(req.get("params").is_none());
    }

    #[test]
    fn jsonrpc_request_with_params() {
        let req = jsonrpc_request(2, "tools/call", Some(json!({"name": "test"})));
        assert_eq!(req["params"]["name"], "test");
    }

    #[test]
    fn jsonrpc_notification_no_id() {
        let req = jsonrpc_notification("notifications/initialized", None);
        assert_eq!(req["jsonrpc"], "2.0");
        assert!(req.get("id").is_none());
        assert_eq!(req["method"], "notifications/initialized");
    }

    #[test]
    fn mcp_tool_info_roundtrip() {
        let info = McpToolInfo {
            name: "echo".into(),
            description: "Echo input back".into(),
            input_schema: json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        };
        let json = serde_json::to_value(&info).unwrap();
        let deserialized: McpToolInfo = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized.name, "echo");
        assert_eq!(deserialized.description, "Echo input back");
    }

    #[test]
    fn mcp_client_status_serialize() {
        let status = McpClientStatus::Connected;
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json, "Connected");

        let offline = McpClientStatus::Offline {
            error: "test error".into(),
        };
        let json = serde_json::to_value(&offline).unwrap();
        assert_eq!(json["Offline"]["error"], "test error");
    }

    #[test]
    fn mcp_server_snapshot_roundtrip() {
        let snap = McpServerSnapshot {
            name: "test".into(),
            transport: "stdio".into(),
            status: McpClientStatus::Connected,
            tools: vec![],
            last_error: None,
            last_seen_at: Some(12345),
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["status"], "Connected");
        assert_eq!(json["last_seen_at"], 12345);
    }

    #[test]
    fn rate_limiter_acquire_rejects_when_empty() {
        let mut limiter = RateLimiter::new(1.0);
        assert!(limiter.acquire());
        assert!(!limiter.acquire());
    }

    #[test]
    fn rate_limiter_refills_over_time() {
        let mut limiter = RateLimiter::new(2.0);
        assert!(limiter.acquire());
        assert!(limiter.acquire());
        assert!(!limiter.acquire());
        limiter.last_refill = Instant::now() - Duration::from_secs(1);
        assert!(limiter.acquire());
    }

    #[tokio::test]
    async fn mcp_client_new_initial_state() {
        let client = McpClient::new("test", "echo", &[], &[]);
        assert_eq!(client.name(), "test");
        let status = client.status().await;
        assert!(matches!(status, McpClientStatus::Disconnected));
    }

    #[tokio::test]
    async fn mcp_client_snapshot_initial() {
        let client = McpClient::new("test", "echo", &[], &[]);
        let snap = client.snapshot().await;
        assert_eq!(snap.name, "test");
        assert!(matches!(snap.status, McpClientStatus::Disconnected));
        assert!(snap.tools.is_empty());
    }

    #[test]
    fn mcp_status_change_event_serde() {
        let event = McpStatusChangeEvent {
            name: "server-1".into(),
            status: McpClientStatus::Connected,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["name"], "server-1");
        assert_eq!(json["status"], "Connected");
    }

    #[tokio::test]
    async fn mcp_manager_new() {
        let mgr = McpManager::new();
        let clients = mgr.clients.lock().await;
        assert!(clients.is_empty());
    }

    #[tokio::test]
    async fn mcp_manager_default() {
        let mgr = McpManager::default();
        let clients = mgr.clients.lock().await;
        assert!(clients.is_empty());
    }
}
