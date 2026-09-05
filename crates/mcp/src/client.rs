use crate::protocol::{
    McpCallOutput, McpClientStatus, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
    PROTOCOL_VERSION, extract_mcp_content,
};
use crate::transport::{
    HttpInner, HttpShared, McpClientInner, StdioInner, http_is_alive, spawn_sse_listener,
};
use haven_common::McpTransportType;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub struct McpClient {
    pub(crate) name: String,
    pub(crate) transport: McpTransportType,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<String>,
    /// Working directory the stdio server is spawned from (optional).
    pub(crate) cwd: Option<String>,
    pub(crate) url: String,
    pub(crate) enabled: Arc<AtomicBool>,
    pub(crate) inner: Arc<Mutex<Option<McpClientInner>>>,
    pub(crate) status: Arc<Mutex<McpClientStatus>>,
    pub(crate) next_id: AtomicU64,
    pub(crate) tools_cache: Arc<Mutex<Option<Vec<McpToolInfo>>>>,
    pub(crate) last_error: Arc<Mutex<Option<String>>>,
    /// Handshake/tool-discovery diagnostics; see `McpServerSnapshot`.
    pub(crate) last_diagnostic: Arc<Mutex<Option<String>>>,
    pub(crate) last_seen_at: Arc<Mutex<Option<i64>>>,
    pub(crate) reconnect_retries: Arc<Mutex<u32>>,
    pub(crate) cancel_token: Arc<Mutex<CancellationToken>>,
    pub(crate) rate_limiter: Arc<Mutex<RateLimiter>>,
    pub(crate) notification_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Value>>>>,
    /// Binary content (image/audio/resource blob) kept in observations (base64
    /// chars) before being replaced by an `oversized` marker.
    pub(crate) max_binary_payload: usize,
    /// Single buffered (incomplete) SSE line/event cap for the HTTP transport.
    pub(crate) max_sse_buffer: usize,
}

pub(crate) struct RateLimiter {
    pub(crate) tokens: f64,
    pub(crate) last_refill: Instant,
    pub(crate) capacity: f64,
    pub(crate) refill_rate: f64,
}

impl RateLimiter {
    pub(crate) fn new(calls_per_second: f64) -> Self {
        Self {
            tokens: calls_per_second,
            last_refill: Instant::now(),
            capacity: calls_per_second,
            refill_rate: calls_per_second,
        }
    }

    pub(crate) fn acquire(&mut self) -> bool {
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

/// Spawn an MCP stdio server, retrying Windows-friendly variants when the
/// configured command cannot be spawned directly. On Windows, npm/npx-style
/// commands are `.ps1` scripts whose bare name is not an executable
/// (`program not found`), and `.ps1` files are blocked by the execution
/// policy unless run via `powershell -ExecutionPolicy Bypass`. Variants
/// tried, in order: the configured command itself, its `.cmd` wrapper, and
/// (for .ps1) a powershell invocation. Non-Windows spawns never retry.
fn spawn_mcp_child(
    name: &str,
    command: &str,
    args: &[String],
    build: &dyn Fn(&str, &[String]) -> Command,
) -> std::io::Result<Child> {
    match build(command, args).spawn() {
        Ok(child) => Ok(child),
        Err(first) => {
            #[cfg(not(windows))]
            {
                let _ = (name, command, args);
                Err(first)
            }
            #[cfg(windows)]
            {
                use std::io::ErrorKind;
                let extensionless = !command.contains(['\\', '/', '.']);
                // 1) npm/npx-style extensionless commands: use the .cmd wrapper.
                if extensionless && first.kind() == ErrorKind::NotFound {
                    let variant = format!("{command}.cmd");
                    if let Ok(child) = build(&variant, args).spawn() {
                        tracing::warn!(
                            "MCP server '{}': '{}' not found on PATH, spawned via '{}'",
                            name,
                            command,
                            variant
                        );
                        return Ok(child);
                    }
                }
                // 2) .ps1 scripts: run through powershell with a bypass policy.
                let ps1 = command.to_lowercase().ends_with(".ps1");
                if ps1 || (extensionless && first.raw_os_error() == Some(193)) {
                    let mut ps_args: Vec<String> = vec![
                        "-NoProfile".into(),
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-File".into(),
                        command.to_string(),
                    ];
                    ps_args.extend(args.iter().cloned());
                    if let Ok(child) = build("powershell", &ps_args).spawn() {
                        tracing::warn!(
                            "MCP server '{}': spawned '{}' via powershell -ExecutionPolicy Bypass",
                            name,
                            command
                        );
                        return Ok(child);
                    }
                }
                Err(first)
            }
        }
    }
}

/// Human-readable fix hint appended to a failed MCP spawn, so "program not
/// found" / "not a valid Win32 application" carry the cure, not just the
/// symptom.
#[cfg(windows)]
fn windows_spawn_hint(command: &str, e: &std::io::Error) -> String {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::NotFound => format!(
            " — '{command}' was not found on PATH. On Windows, npm/npx-style commands must use their .cmd wrapper (e.g. npm.cmd) or an absolute path; .ps1 scripts need `powershell -ExecutionPolicy Bypass -File`."
        ),
        _ if e.raw_os_error() == Some(193) => format!(
            " — '%1 is not a valid Win32 application': '{command}' is a script, not an executable. Run it via its interpreter (node.exe/python/powershell) with the full script path, or use its .cmd wrapper."
        ),
        _ => String::new(),
    }
}

#[cfg(not(windows))]
fn windows_spawn_hint(_command: &str, _e: &std::io::Error) -> String {
    String::new()
}

impl McpClient {
    pub fn new(
        config: &haven_common::McpServerConfig,
        max_binary_payload: usize,
        max_sse_buffer: usize,
    ) -> Self {
        Self {
            name: config.name.clone(),
            transport: config.transport.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
            cwd: config.cwd.clone(),
            url: config.url.clone(),
            enabled: Arc::new(AtomicBool::new(config.enabled)),
            inner: Arc::new(Mutex::new(None)),
            status: Arc::new(Mutex::new(McpClientStatus::Disconnected)),
            next_id: AtomicU64::new(1),
            tools_cache: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(None)),
            last_diagnostic: Arc::new(Mutex::new(None)),
            last_seen_at: Arc::new(Mutex::new(None)),
            reconnect_retries: Arc::new(Mutex::new(0)),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new(10.0))),
            notification_rx: Arc::new(Mutex::new(None)),
            max_binary_payload,
            max_sse_buffer,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    /// Whether the live client was spawned from the given persisted config.
    /// Refresh/reload paths use this to decide if an already-connected server
    /// can be kept as-is or must be torn down and reconnected (a changed
    /// command / args / env / url means the running child carries stale
    /// credentials or an outdated invocation).
    pub fn matches_config(&self, cfg: &haven_common::McpServerConfig) -> bool {
        self.transport == cfg.transport
            && self.command == cfg.command
            && self.args == cfg.args
            && self.env == cfg.env
            && self.cwd == cfg.cwd
            && self.url == cfg.url
            && self.enabled.load(Ordering::SeqCst) == cfg.enabled
    }

    pub async fn status(&self) -> McpClientStatus {
        self.status.lock().await.clone()
    }

    pub async fn tools_cache(&self) -> Vec<McpToolInfo> {
        self.tools_cache.lock().await.clone().unwrap_or_default()
    }

    /// Wait up to `timeout` for the server to finish connecting and populate
    /// the tools cache, then return whatever is cached (possibly empty for a
    /// connected server with zero tools). Returns immediately when the cache
    /// is already populated. Gives up early on a definitive `Offline` failure
    /// so a dead server cannot stall the caller for the whole timeout.
    ///
    /// Used by resume paths that re-register per-session MCP adapters after a
    /// restart, where the background connect may still be in flight.
    pub async fn wait_for_tools(&self, timeout: Duration) -> Vec<McpToolInfo> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(tools) = self.tools_cache.lock().await.clone() {
                return tools;
            }
            if matches!(&*self.status.lock().await, McpClientStatus::Offline { .. }) {
                return Vec::new();
            }
            if tokio::time::Instant::now() >= deadline {
                return self.tools_cache.lock().await.clone().unwrap_or_default();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn last_error(&self) -> Option<String> {
        self.last_error.lock().await.clone()
    }

    /// Handshake/tool-discovery diagnostics (protocol mismatch, connected
    /// with zero tools, failed tools/list). `None` when everything is clean.
    pub async fn diagnostic(&self) -> Option<String> {
        self.last_diagnostic.lock().await.clone()
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
            transport: self.transport.as_str().into(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
            url: self.url.clone(),
            enabled: self.enabled(),
            status: self.status.lock().await.clone(),
            tools: self.tools_cache.lock().await.clone().unwrap_or_default(),
            last_error: self.last_error.lock().await.clone(),
            diagnostic: self.last_diagnostic.lock().await.clone(),
            last_seen_at: *self.last_seen_at.lock().await,
        }
    }

    async fn spawn_stdio(
        &self,
        notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
    ) -> anyhow::Result<StdioInner> {
        // MCP servers are user-configured external programs: their command and
        // args may legitimately use relative paths, so they spawn from the
        // configured `cwd` when set, otherwise the app's working directory
        // (not the Temp default used for agent-executed commands).
        let build = |program: &str, args: &[String]| {
            let mut cmd = Command::new(program);
            cmd.args(args);
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            cmd.kill_on_drop(true);
            if let Some(cwd) = &self.cwd {
                cmd.current_dir(cwd);
            }
            for e in &self.env {
                if let Some((key, val)) = e.split_once('=') {
                    cmd.env(key, val);
                }
            }
            cmd
        };

        let mut child =
            spawn_mcp_child(&self.name, &self.command, &self.args, &build).map_err(|e| {
                let hint = windows_spawn_hint(&self.command, &e);
                anyhow::anyhow!("failed to spawn MCP server '{}': {}{}", self.name, e, hint)
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdin for '{}'", self.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout for '{}'", self.name))?;

        Ok(StdioInner {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            notification_tx,
        })
    }

    async fn spawn_http(
        &self,
        notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
    ) -> anyhow::Result<(Arc<HttpShared>, HttpInner)> {
        if self.url.trim().is_empty() {
            anyhow::bail!(
                "MCP server '{}': a URL is required for the HTTP transport",
                self.name
            );
        }
        let headers = self
            .env
            .iter()
            .filter_map(|e| {
                e.split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect::<Vec<_>>();
        // Disable redirects: user-configured credential headers (e.g.
        // X-API-Key) and the MCP session id are attached to every request,
        // and reqwest only strips the fixed auth-header list on cross-host
        // redirects. Following a redirect would leak those to an unverified
        // host, so refuse to follow any.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| anyhow::anyhow!("failed to build HTTP client: {e}"))?;
        let shared = Arc::new(HttpShared {
            http,
            url: self.url.clone(),
            headers,
            session_id: Arc::new(tokio::sync::Mutex::new(String::new())),
            cancel: self.cancel_token.lock().await.clone(),
        });
        let inner = HttpInner {
            shared: shared.clone(),
            notification_tx,
            max_sse_buffer: self.max_sse_buffer,
        };
        Ok((shared, inner))
    }

    /// Connect to the server (initialize handshake + tool discovery). On
    /// failure, records a diagnostic explaining what happened at the
    /// handshake level, so the caller can tell "server down" from "client
    /// and server are incompatible".
    pub async fn connect(&self) -> anyhow::Result<()> {
        *self.status.lock().await = McpClientStatus::Connecting;
        *self.last_diagnostic.lock().await = None;
        let result = self.connect_inner().await;
        if let Err(e) = &result {
            *self.last_diagnostic.lock().await = Some(format!("connect failed: {e}"));
        }
        result
    }

    async fn connect_inner(&self) -> anyhow::Result<()> {
        let (notification_tx, new_rx) = tokio::sync::mpsc::unbounded_channel();
        *self.notification_rx.lock().await = Some(new_rx);

        // Refresh the cancel token: `shutdown()` (called before every
        // reconnect) cancels it, and reusing the cancelled token would make
        // the new transport's requests (and the SSE listener) abort
        // immediately after a reconnect.
        *self.cancel_token.lock().await = CancellationToken::new();

        let mut http_shared: Option<Arc<HttpShared>> = None;
        let mut inner = match self.transport {
            McpTransportType::Stdio => {
                McpClientInner::Stdio(Box::new(self.spawn_stdio(notification_tx.clone()).await?))
            }
            McpTransportType::Http => {
                let (shared, inner) = self.spawn_http(notification_tx.clone()).await?;
                http_shared = Some(shared);
                McpClientInner::Http(inner)
            }
        };

        // Initialize handshake
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = inner
            .request(
                id,
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "haven",
                        "version": "0.1.0",
                    },
                })),
            )
            .await?;

        let server_name = result["serverInfo"]["name"].as_str().unwrap_or("?");
        let server_version = result["serverInfo"]["version"].as_str().unwrap_or("?");
        let server_protocol = result["protocolVersion"].as_str().unwrap_or("?");
        if let Some(warning) = result["warning"].as_str() {
            tracing::warn!("MCP server '{}' warning: {}", self.name, warning);
        }
        if server_protocol != PROTOCOL_VERSION {
            tracing::warn!(
                "MCP server '{}' negotiated protocol version '{}' (client supports '{}')",
                self.name,
                server_protocol,
                PROTOCOL_VERSION
            );
            *self.last_diagnostic.lock().await = Some(format!(
                "protocol version mismatch: server negotiated '{}', client supports '{}' — tool discovery may be limited or fail",
                server_protocol, PROTOCOL_VERSION
            ));
        }
        tracing::info!(
            "MCP server '{}' connected ({}): {} v{} (protocol {})",
            self.name,
            self.transport.as_str(),
            server_name,
            server_version,
            server_protocol
        );

        // Send initialized notification
        inner.notify("notifications/initialized", None).await?;

        *self.inner.lock().await = Some(inner);

        // HTTP: keep an SSE stream open so server-to-client notifications
        // (e.g. tools/list_changed) are received.
        if let Some(shared) = http_shared {
            let cancel = self.cancel_token.lock().await.clone();
            spawn_sse_listener(
                self.name.clone(),
                cancel,
                shared,
                notification_tx,
                self.max_sse_buffer,
            );
        }

        *self.status.lock().await = McpClientStatus::Connected;

        // Cache tools after successful connection. A successful handshake
        // with zero tools is suspicious (often a client/server SDK protocol
        // mismatch rather than an empty server), so record a diagnostic that
        // distinguishes the two cases.
        match self.list_tools().await {
            Ok(tools) => {
                *self.tools_cache.lock().await = Some(tools.clone());
                if tools.is_empty() {
                    *self.last_diagnostic.lock().await = Some(format!(
                        "Connected, but tools/list returned 0 tools (server {} v{}, protocol {} vs client {}). Either the server exposes no tools, or the client/server protocols are incompatible — check the server's logs and its SDK protocol support.",
                        server_name, server_version, server_protocol, PROTOCOL_VERSION
                    ));
                }
            }
            Err(e) => {
                *self.last_diagnostic.lock().await =
                    Some(format!("connected, but tools/list failed: {e}"));
            }
        }

        *self.last_error.lock().await = None;
        *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());

        Ok(())
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        // Cancel the client's own token first so any in-flight `call_tool`
        // (which selects on this token) aborts and releases the `inner` lock.
        self.cancel_token.lock().await.cancel();

        let mut guard = self.inner.lock().await;
        if let Some(inner) = guard.as_mut() {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let _ = inner.request(id, "shutdown", None).await;
            if let McpClientInner::Stdio(s) = inner {
                let _ = s.notify("exit", None).await;
                let _ = s.child.start_kill();
                let _ = s.child.wait().await;
            }
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
        cancel: CancellationToken,
    ) -> anyhow::Result<McpCallOutput> {
        // Rate limiting (refine §4.5)
        {
            let mut rl = self.rate_limiter.lock().await;
            if !rl.acquire() {
                let wait = Duration::from_secs_f64(1.0 / rl.refill_rate);
                drop(rl);
                tokio::time::sleep(wait).await;
            }
        }

        // Clone the client's own shutdown/reconnect token so that an in-flight
        // request can be interrupted when shutdown_all/reconnect cancels it.
        // Without this, call_tool would hold `inner` lock for up to 30s
        // (REQUEST_TIMEOUT_SECS), blocking shutdown/reconnect entirely.
        let client_cancel = self.cancel_token.lock().await.clone();

        let mut guard = self.inner.lock().await;
        let inner = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("MCP client '{}' is not connected", self.name))?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = tokio::select! {
            r = inner.request(
                id,
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": input,
                })),
            ) => r?,
            _ = client_cancel.cancelled() => {
                anyhow::bail!("MCP call '{}' cancelled (client shutting down)", tool_name);
            }
            _ = cancel.cancelled() => {
                anyhow::bail!("MCP call '{}' cancelled (session cancellation)", tool_name);
            }
        };

        let content = result["content"].as_array().cloned().unwrap_or_default();
        let (output, text) = extract_mcp_content(&content, self.max_binary_payload);

        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(McpCallOutput {
            success: !is_error,
            output,
            error: is_error.then_some(text),
        })
    }

    pub async fn is_alive(&self) -> bool {
        match self.transport {
            McpTransportType::Stdio => {
                let mut guard = self.inner.lock().await;
                match guard.as_mut() {
                    Some(McpClientInner::Stdio(s)) => {
                        s.child.try_wait().map(|st| st.is_none()).unwrap_or(false)
                    }
                    _ => false,
                }
            }
            McpTransportType::Http => {
                let shared = {
                    let guard = self.inner.lock().await;
                    match guard.as_ref() {
                        Some(McpClientInner::Http(h)) => Some(h.shared.clone()),
                        _ => None,
                    }
                };
                match shared {
                    Some(shared) => http_is_alive(&shared).await,
                    None => false,
                }
            }
        }
    }

    pub async fn reconnect(&self) -> anyhow::Result<()> {
        // Cancel any ongoing monitor session and create a fresh token in a
        // single lock acquisition. Previously two separate lock() calls
        // left a window where a concurrent reader could observe the
        // already-cancelled old token but the new one wasn't set yet.
        {
            let mut token = self.cancel_token.lock().await;
            token.cancel();
            *token = CancellationToken::new();
        }

        *self.reconnect_retries.lock().await = 0;
        *self.status.lock().await = McpClientStatus::Connecting;

        if let Err(e) = self.shutdown().await {
            tracing::warn!("MCP reconnect: shutdown of previous session failed: {}", e);
        }
        self.connect().await?;

        // Refresh tools cache
        match self.list_tools().await {
            Ok(tools) => *self.tools_cache.lock().await = Some(tools),
            Err(e) => {
                tracing::warn!(
                    "MCP reconnect: list_tools failed, tool cache stays stale: {}",
                    e
                );
            }
        }

        *self.last_error.lock().await = None;
        *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());

        Ok(())
    }

    /// Spawn a background health-monitor + auto-reconnect session.
    ///
    /// Every `health_interval` the session calls `is_alive()`. If the process is
    /// dead it enters the reconnect loop (exponential backoff). The loop stops
    /// when the client's cancel token is cancelled (e.g. via `shutdown_all`).
    /// After `max_retries` consecutive failures the client remains `Offline`
    /// and waits for a manual `reconnect()` call.
    ///
    /// The session terminates when the cancel token fires.
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
                                let safe_error =
                                    haven_common::error::sanitize_error_text(&e.to_string());
                                retries += 1;
                                *self.reconnect_retries.lock().await = retries;
                                *self.last_error.lock().await =
                                    Some(format!("reconnect failed: {safe_error}"));
                                backoff = (backoff * 2).min(max_backoff);
                                let _ = status_tx.send(McpStatusChangeEvent {
                                    name: self.name.clone(),
                                    status: McpClientStatus::Offline {
                                        error: format!("reconnect failed: {safe_error}"),
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
