use crate::protocol::{
    LEGACY_PROTOCOL_VERSION, MODERN_PROTOCOL_VERSION, McpCallOutput, McpClientStatus,
    McpProtocolMode, McpRpcError, McpServerSnapshot, McpStatusChangeEvent, McpToolInfo,
    McpToolListPage, extract_mcp_content, http_parameter_headers, should_fallback_to_legacy,
    supported_legacy_version,
};
use crate::transport::{
    HttpInner, HttpShared, McpClientInner, NOTIFICATION_QUEUE_CAPACITY, StdioInner,
    ToolsListChangedSignal, http_is_alive, spawn_sse_listener,
};
use haven_common::{mcp::McpServerTransport, types::NetworkPolicy};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

const MAX_TOOL_LIST_PAGES: usize = 100;
const MAX_TOOLS_PER_SERVER: usize = 10_000;
const MAX_TOOL_LIST_CURSOR_BYTES: usize = 4096;
const MAX_TOOL_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpToolCacheScope {
    Public,
    Private,
}

#[derive(Debug, Clone)]
pub(crate) struct McpToolListSnapshot {
    pub(crate) tools: Vec<McpToolInfo>,
    expires_at: Option<Instant>,
    _cache_scope: McpToolCacheScope,
}

impl McpToolListSnapshot {
    fn is_fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|expires_at| Instant::now() < expires_at)
    }
}

pub struct McpClient {
    pub(crate) name: String,
    pub(crate) transport: McpServerTransport,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<String>,
    /// Working directory the stdio server is spawned from (optional).
    pub(crate) cwd: Option<String>,
    pub(crate) url: String,
    pub(crate) enabled: Arc<AtomicBool>,
    pub(crate) inner: Arc<Mutex<Option<McpClientInner>>>,
    /// Protocol era and negotiated version for the active connection.
    pub(crate) protocol_mode: Arc<Mutex<McpProtocolMode>>,
    pub(crate) status: Arc<Mutex<McpClientStatus>>,
    pub(crate) next_id: Arc<AtomicU64>,
    pub(crate) tools_cache: Arc<Mutex<Option<McpToolListSnapshot>>>,
    tool_list_refresh: Mutex<()>,
    pub(crate) last_error: Arc<Mutex<Option<String>>>,
    /// Protocol selection and tool-discovery diagnostics; see `McpServerSnapshot`.
    pub(crate) last_diagnostic: Arc<Mutex<Option<String>>>,
    pub(crate) last_seen_at: Arc<Mutex<Option<i64>>>,
    pub(crate) reconnect_retries: Arc<Mutex<u32>>,
    pub(crate) cancel_token: Arc<Mutex<CancellationToken>>,
    /// Lifetime cancellation for the notification listener. Unlike the
    /// transport token, it survives reconnects so the listener can reuse the
    /// receiver after a new connection is installed.
    pub(crate) notification_cancel: CancellationToken,
    /// Join owner for the current generation's server notification stream:
    /// legacy HTTP GET SSE or modern `subscriptions/listen`.
    pub(crate) notification_stream_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) rate_limiter: Arc<Mutex<RateLimiter>>,
    pub(crate) notification_rx: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<Value>>>>,
    pub(crate) tools_list_changed: Arc<ToolsListChangedSignal>,
    /// Maximum bytes of an image/audio/resource Base64 representation kept in
    /// observations before being replaced by an `oversized` marker.
    pub(crate) max_binary_base64_bytes: usize,
    /// Single buffered (incomplete) SSE line/event cap for the HTTP transport.
    pub(crate) max_sse_buffer: usize,
    /// Runtime network boundary. Direct clients fail closed at construction;
    /// the manager overwrites it from SecurityConfig before a configured
    /// connection is started.
    pub(crate) network_policy: Arc<RwLock<NetworkPolicy>>,
}

pub(crate) struct RateLimiter {
    pub(crate) tokens: f64,
    pub(crate) last_refill: Instant,
    pub(crate) capacity: f64,
    pub(crate) refill_rate: f64,
}

impl RateLimiter {
    pub(crate) fn new(calls_per_second: f64) -> Self {
        let refill_rate = calls_per_second.max(0.001);
        let capacity = calls_per_second.max(1.0);
        Self {
            tokens: capacity,
            last_refill: Instant::now(),
            capacity,
            refill_rate,
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

    pub(crate) fn wait_duration(&mut self) -> Duration {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else if self.refill_rate > 0.0 {
            Duration::from_secs_f64((1.0 - self.tokens) / self.refill_rate)
        } else {
            // A zero configured rate remains cancellable instead of causing a
            // division by zero or a busy retry loop.
            Duration::from_secs(60)
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
    containment: &haven_platform::process_containment::ProcessContainment,
) -> std::io::Result<Child> {
    let spawn = |program: &str, args: &[String]| {
        let mut cmd = build(program, args);
        containment.prepare_command(cmd.as_std_mut(), 0);
        cmd.spawn()
    };
    match spawn(command, args) {
        Ok(child) => contain_mcp_child(child, containment),
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
                    if let Ok(child) = spawn(&variant, args) {
                        tracing::warn!(
                            server = %name,
                            "MCP command resolved through .cmd wrapper"
                        );
                        return contain_mcp_child(child, containment);
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
                    if let Ok(child) = spawn("powershell", &ps_args) {
                        tracing::warn!(
                            server = %name,
                            "MCP command resolved through PowerShell script fallback"
                        );
                        return contain_mcp_child(child, containment);
                    }
                }
                Err(first)
            }
        }
    }
}

fn contain_mcp_child(
    child: Child,
    containment: &haven_platform::process_containment::ProcessContainment,
) -> std::io::Result<Child> {
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("MCP child did not expose a process id"))?;
    #[cfg(windows)]
    let process_handle = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("MCP child process handle is unavailable"))?;
    #[cfg(not(windows))]
    let process_handle = ();
    containment.attach_and_resume(pid, process_handle)?;
    Ok(child)
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
        config: &haven_common::config::McpServerConfig,
        max_binary_base64_bytes: usize,
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
            protocol_mode: Arc::new(Mutex::new(McpProtocolMode::Uninitialized)),
            status: Arc::new(Mutex::new(McpClientStatus::Disconnected)),
            next_id: Arc::new(AtomicU64::new(1)),
            tools_cache: Arc::new(Mutex::new(None)),
            tool_list_refresh: Mutex::new(()),
            last_error: Arc::new(Mutex::new(None)),
            last_diagnostic: Arc::new(Mutex::new(None)),
            last_seen_at: Arc::new(Mutex::new(None)),
            reconnect_retries: Arc::new(Mutex::new(0)),
            cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
            notification_cancel: CancellationToken::new(),
            notification_stream_task: Mutex::new(None),
            rate_limiter: Arc::new(Mutex::new(RateLimiter::new(10.0))),
            notification_rx: Arc::new(Mutex::new(None)),
            tools_list_changed: Arc::new(ToolsListChangedSignal::new()),
            max_binary_base64_bytes,
            max_sse_buffer,
            network_policy: Arc::new(RwLock::new(NetworkPolicy::Ask)),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub async fn set_network_policy(&self, policy: NetworkPolicy) {
        *self.network_policy.write().await = policy;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn network_policy(&self) -> NetworkPolicy {
        *self.network_policy.read().await
    }

    /// Whether the live client was spawned from the given persisted config.
    /// Refresh/reload paths use this to decide if an already-connected server
    /// can be kept as-is or must be torn down and reconnected (a changed
    /// command / args / env / url means the running child carries stale
    /// credentials or an outdated invocation).
    pub fn matches_config(&self, cfg: &haven_common::config::McpServerConfig) -> bool {
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

    #[cfg(any(test, feature = "test-support"))]
    #[allow(dead_code)]
    pub async fn tools_cache(&self) -> Vec<McpToolInfo> {
        self.all_cached_tools().await
    }

    async fn all_cached_tools(&self) -> Vec<McpToolInfo> {
        self.tools_cache
            .lock()
            .await
            .as_ref()
            .map(|snapshot| snapshot.tools.clone())
            .unwrap_or_default()
    }

    pub(crate) async fn cached_tools(&self) -> Option<Vec<McpToolInfo>> {
        self.tools_cache
            .lock()
            .await
            .as_ref()
            .filter(|snapshot| snapshot.is_fresh())
            .map(|snapshot| snapshot.tools.clone())
    }

    /// Return a fresh discovered list, re-fetching only after the server's
    /// cache hint expires. The boolean reports whether this call performed a
    /// refresh so the manager can advance its catalog generation afterward.
    pub(crate) async fn ensure_tool_cache_fresh(
        &self,
    ) -> anyhow::Result<(Option<Vec<McpToolInfo>>, bool)> {
        self.refresh_tool_cache(false).await
    }

    pub(crate) async fn refresh_tool_cache(
        &self,
        force: bool,
    ) -> anyhow::Result<(Option<Vec<McpToolInfo>>, bool)> {
        let _refresh_guard = self.tool_list_refresh.lock().await;
        let cache = self.tools_cache.lock().await.clone();
        if let Some(snapshot) = cache.as_ref().filter(|snapshot| snapshot.is_fresh()) {
            if !force {
                return Ok((Some(snapshot.tools.clone()), false));
            }
        } else if cache.is_none() && !force {
            // A connection may still be in progress. Do not issue tools/list
            // until its protocol mode and transport are installed.
            return Ok((None, false));
        }

        let snapshot = self.list_tool_snapshot().await?;
        let tools = snapshot.tools.clone();
        *self.tools_cache.lock().await = Some(snapshot);
        Ok((Some(tools), true))
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
            if let Some(tools) = self.cached_tools().await {
                return tools;
            }
            if matches!(&*self.status.lock().await, McpClientStatus::Offline { .. }) {
                return Vec::new();
            }
            if tokio::time::Instant::now() >= deadline {
                return self.all_cached_tools().await;
            }
            if matches!(&*self.status.lock().await, McpClientStatus::Connected)
                && let Ok((Some(tools), _)) = self.ensure_tool_cache_fresh().await
            {
                return tools;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Handshake/tool-discovery diagnostics (protocol mismatch, connected
    /// with zero tools, failed tools/list). `None` when everything is clean.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn cancel_token(&self) -> CancellationToken {
        self.cancel_token.lock().await.clone()
    }

    pub async fn snapshot(&self) -> McpServerSnapshot {
        McpServerSnapshot {
            name: self.name.clone(),
            transport: self.transport.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
            url: self.url.clone(),
            enabled: self.enabled(),
            status: self.status.lock().await.clone(),
            tools: self.all_cached_tools().await,
            last_error: self.last_error.lock().await.clone(),
            diagnostic: self.last_diagnostic.lock().await.clone(),
            last_seen_at: *self.last_seen_at.lock().await,
        }
    }

    async fn spawn_stdio(
        &self,
        notification_tx: tokio::sync::mpsc::Sender<Value>,
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
            // MCP launchers that can open companion GUI applications must
            // require an explicit opt-in. Per-server environment entries
            // below may override this default with `MCP_AUTOLAUNCH=1`.
            cmd.env("MCP_AUTOLAUNCH", "0");
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

        let containment =
            haven_platform::process_containment::ProcessContainment::new().map_err(|error| {
                anyhow::anyhow!(
                    "failed to create process containment for MCP server '{}': {error}",
                    self.name
                )
            })?;
        let mut child =
            spawn_mcp_child(&self.name, &self.command, &self.args, &build, &containment).map_err(
                |e| {
                    let hint = windows_spawn_hint(&self.command, &e);
                    anyhow::anyhow!(
                        "failed to spawn MCP server '{}': {}{}",
                        self.name,
                        haven_common::error::sanitize_error_text(&e.to_string()),
                        hint
                    )
                },
            )?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdin for '{}'", self.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to capture stdout for '{}'", self.name))?;

        Ok(StdioInner::new(
            child,
            containment,
            stdin,
            stdout,
            notification_tx,
            self.tools_list_changed.clone(),
            self.cancel_token.lock().await.clone(),
        ))
    }

    async fn spawn_http(
        &self,
        notification_tx: tokio::sync::mpsc::Sender<Value>,
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
                    .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
            })
            .collect::<Vec<_>>();
        if headers.iter().any(|(key, _)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "mcp-protocol-version" | "mcp-method" | "mcp-name" | "mcp-session-id"
            ) || key.to_ascii_lowercase().starts_with("mcp-param-")
        }) {
            anyhow::bail!(
                "MCP server '{}' configures an HTTP header managed by the MCP protocol",
                self.name
            );
        }
        // Disable redirects: user-configured credential headers (e.g.
        // X-API-Key) and the MCP session id are attached to every request,
        // and reqwest only strips the fixed auth-header list on cross-host
        // redirects. Following a redirect would leak those to an unverified
        // host, so refuse to follow any.
        let policy = *self.network_policy.read().await;
        let built_client = crate::network::build_http_client(&self.url, policy).await?;
        let shared = Arc::new(HttpShared {
            http: built_client.client,
            url: built_client.endpoint.to_string(),
            headers,
            mcp_session_id: Arc::new(tokio::sync::Mutex::new(String::new())),
            cancel: self.cancel_token.lock().await.clone(),
        });
        let inner = HttpInner {
            shared: shared.clone(),
            notification_tx,
            max_sse_buffer: self.max_sse_buffer,
            tools_list_changed: self.tools_list_changed.clone(),
        };
        Ok((shared, inner))
    }

    /// Select a protocol mode and discover tools. On failure, record whether
    /// the peer was unreachable or incompatible with the supported protocols.
    pub async fn connect(&self) -> anyhow::Result<()> {
        let network_policy = *self.network_policy.read().await;
        if matches!(self.transport, McpServerTransport::Stdio)
            && !matches!(network_policy, NetworkPolicy::Open)
        {
            let error = anyhow::anyhow!(
                "MCP stdio server '{}' is blocked because its network use is opaque; set network policy to Open explicitly",
                self.name
            );
            *self.status.lock().await = McpClientStatus::Offline {
                error: error.to_string(),
            };
            return Err(error);
        }
        *self.status.lock().await = McpClientStatus::Connecting;
        *self.last_diagnostic.lock().await = None;
        // A new connection must repopulate this cache from the new server.
        // Keeping entries from a previous transport would expose tools that
        // were never confirmed on the current session.
        *self.tools_cache.lock().await = None;
        let result = self.connect_inner().await;
        if let Err(e) = &result {
            if let Err(shutdown_error) = self.shutdown_inner(false).await {
                tracing::warn!(
                    server = %self.name,
                    error = %haven_common::error::sanitize_error_text(&shutdown_error.to_string()),
                    "MCP connection cleanup reported an error"
                );
            }
            let message = format!(
                "connect failed: {}",
                haven_common::error::sanitize_error_text(&e.to_string())
            );
            *self.last_diagnostic.lock().await = Some(message.clone());
            *self.status.lock().await = McpClientStatus::Offline { error: message };
        }
        result
    }

    async fn connect_inner(&self) -> anyhow::Result<()> {
        let (notification_tx, new_rx) = tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        *self.notification_rx.lock().await = Some(new_rx);

        // Refresh the cancel token: `shutdown()` (called before every
        // reconnect) cancels it, and reusing the cancelled token would make
        // the new transport's requests (and notification stream) abort
        // immediately after a reconnect.
        *self.cancel_token.lock().await = CancellationToken::new();
        *self.protocol_mode.lock().await = McpProtocolMode::Uninitialized;

        let mut http_shared: Option<Arc<HttpShared>> = None;
        let mut inner = match self.transport {
            McpServerTransport::Stdio => {
                McpClientInner::Stdio(Box::new(self.spawn_stdio(notification_tx.clone()).await?))
            }
            McpServerTransport::Http => {
                let (shared, inner) = self.spawn_http(notification_tx.clone()).await?;
                http_shared = Some(shared);
                McpClientInner::Http(inner)
            }
        };

        // Discover the modern stateless protocol first. stdio needs this probe
        // because a legacy server may otherwise process an era-ambiguous RPC
        // before initialization; HTTP identifies legacy peers through its
        // transport-level rejection.
        let modern_mode = McpProtocolMode::modern(MODERN_PROTOCOL_VERSION);
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let discovery = inner
            .request(id, "server/discover", None, &modern_mode, &[])
            .await;

        let mut modern_tools_supported = false;
        let mut modern_tools_list_changed = false;
        let (mode, server_name, server_version, server_protocol) = match discovery {
            Ok(result) => {
                let versions = result["supportedVersions"].as_array().ok_or_else(|| {
                    anyhow::anyhow!("MCP server discovery response omitted supportedVersions")
                })?;
                if !versions
                    .iter()
                    .any(|version| version.as_str() == Some(MODERN_PROTOCOL_VERSION))
                {
                    anyhow::bail!(
                        "MCP server discovery did not advertise the requested protocol version"
                    );
                }
                let tools_capabilities = result
                    .get("capabilities")
                    .and_then(|value| value.get("tools"));
                modern_tools_supported = tools_capabilities.is_some();
                modern_tools_list_changed = tools_capabilities
                    .and_then(|value| value.get("listChanged"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let server_info = &result["_meta"]["io.modelcontextprotocol/serverInfo"];
                (
                    modern_mode,
                    server_info["name"].as_str().unwrap_or("?").to_string(),
                    server_info["version"].as_str().unwrap_or("?").to_string(),
                    MODERN_PROTOCOL_VERSION.to_string(),
                )
            }
            Err(error)
                if should_fallback_to_legacy(
                    &error,
                    matches!(self.transport, McpServerTransport::Http),
                ) || supported_legacy_version(&error).is_some() =>
            {
                if self.cancel_token.lock().await.is_cancelled() {
                    return Err(error);
                }
                let legacy_request_version =
                    supported_legacy_version(&error).unwrap_or(LEGACY_PROTOCOL_VERSION);
                // If the stdio reader died during the probe, do not reuse the
                // broken stream for the legacy initialize handshake.
                if matches!(&inner, McpClientInner::Stdio(stdio) if stdio.poisoned()) {
                    inner = McpClientInner::Stdio(Box::new(
                        self.spawn_stdio(notification_tx.clone()).await?,
                    ));
                }
                let initialize = inner
                    .request(
                        self.next_id.fetch_add(1, Ordering::SeqCst),
                        "initialize",
                        Some(serde_json::json!({
                            "protocolVersion": legacy_request_version,
                            "capabilities": {},
                            "clientInfo": {
                                "name": "haven",
                                "version": env!("CARGO_PKG_VERSION"),
                            },
                        })),
                        &McpProtocolMode::Uninitialized,
                        &[],
                    )
                    .await?;
                let server_name = initialize["serverInfo"]["name"]
                    .as_str()
                    .unwrap_or("?")
                    .to_string();
                let server_version = initialize["serverInfo"]["version"]
                    .as_str()
                    .unwrap_or("?")
                    .to_string();
                let server_protocol = initialize["protocolVersion"]
                    .as_str()
                    .unwrap_or(LEGACY_PROTOCOL_VERSION)
                    .to_string();
                if let Some(warning) = initialize["warning"].as_str() {
                    tracing::warn!(
                        server = %self.name,
                        warning_bytes = warning.len(),
                        "MCP server returned a warning"
                    );
                }
                if server_protocol != legacy_request_version {
                    tracing::warn!(
                        server = %self.name,
                        negotiated_protocol_bytes = server_protocol.len(),
                        "MCP server selected a different legacy protocol version"
                    );
                    *self.last_diagnostic.lock().await = Some(format!(
                        "server negotiated legacy MCP protocol '{}'; Haven requested '{}'",
                        server_protocol, legacy_request_version
                    ));
                }
                (
                    McpProtocolMode::legacy(server_protocol.clone()),
                    server_name,
                    server_version,
                    server_protocol,
                )
            }
            Err(error) => {
                if let Some(rpc_error) = error.downcast_ref::<McpRpcError>()
                    && rpc_error.code == -32022
                {
                    let advertised = rpc_error
                        .data
                        .as_ref()
                        .and_then(|data| data["supported"].as_array())
                        .map_or(0, Vec::len);
                    anyhow::bail!(
                        "MCP server does not support protocol version '{}'; it advertised {} supported version(s)",
                        MODERN_PROTOCOL_VERSION,
                        advertised
                    );
                }
                return Err(error.context("MCP modern protocol discovery failed"));
            }
        };
        *self.protocol_mode.lock().await = mode.clone();
        inner.set_modern(mode.is_modern());

        tracing::info!(
            server = %self.name,
            transport = self.transport.as_str(),
            protocol_version = server_protocol,
            "MCP server protocol selected"
        );

        if mode.is_legacy() {
            inner
                .notify("notifications/initialized", None, &mode)
                .await?;
        }

        *self.inner.lock().await = Some(inner);

        // HTTP: keep an SSE stream open so server-to-client notifications
        // (e.g. tools/list_changed) are received.
        if let Some(shared) = http_shared.filter(|_| mode.is_legacy()) {
            let cancel = self.cancel_token.lock().await.clone();
            let listener = spawn_sse_listener(
                self.name.clone(),
                cancel,
                shared,
                notification_tx,
                self.max_sse_buffer,
                self.tools_list_changed.clone(),
                mode.clone(),
            );
            *self.notification_stream_task.lock().await = Some(listener);
        }

        // Cache tools after successful connection. A successful handshake
        // with zero tools is suspicious (often a client/server SDK protocol
        // mismatch rather than an empty server), so record a diagnostic that
        // distinguishes the two cases.
        let list_result = if mode.is_modern() && !modern_tools_supported {
            Ok(McpToolListSnapshot {
                tools: Vec::new(),
                expires_at: None,
                _cache_scope: McpToolCacheScope::Private,
            })
        } else {
            self.list_tool_snapshot().await
        };
        match list_result {
            Ok(snapshot) => {
                let tools = snapshot.tools.clone();
                *self.tools_cache.lock().await = Some(snapshot);
                if tools.is_empty() && (!mode.is_modern() || modern_tools_supported) {
                    *self.last_diagnostic.lock().await = Some(format!(
                        "Connected, but tools/list returned 0 tools (server {} v{}, protocol {}). Either the server exposes no tools, or the client/server protocols are incompatible — check the server's logs and its SDK protocol support.",
                        server_name, server_version, server_protocol
                    ));
                }
            }
            Err(e) => {
                let message = format!(
                    "tools/list failed after handshake: {}",
                    haven_common::error::sanitize_error_text(&e.to_string())
                );
                *self.tools_cache.lock().await = None;
                *self.last_diagnostic.lock().await = Some(message.clone());
                *self.status.lock().await = McpClientStatus::Offline {
                    error: message.clone(),
                };
                anyhow::bail!(message);
            }
        }

        *self.status.lock().await = McpClientStatus::Connected;
        *self.last_error.lock().await = None;
        *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());

        if mode.is_modern() && modern_tools_list_changed {
            let inner = self.inner.lock().await;
            if let Some(inner) = inner.as_ref() {
                let listener = inner.spawn_tools_subscription(
                    self.name.clone(),
                    self.next_id.clone(),
                    mode.clone(),
                );
                *self.notification_stream_task.lock().await = Some(listener);
            }
        }

        Ok(())
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown_inner(true).await
    }

    async fn shutdown_inner(&self, stop_notification_listener: bool) -> anyhow::Result<()> {
        let protocol_mode = self.protocol_mode.lock().await.clone();
        // Cancel the client's own token first so any in-flight `call_tool`
        // (which selects on this token) aborts and releases the `inner` lock.
        self.cancel_token.lock().await.cancel();
        if stop_notification_listener {
            self.notification_cancel.cancel();
        }

        let notification_stream = self.notification_stream_task.lock().await.take();
        if let Some(mut listener) = notification_stream {
            match tokio::time::timeout(Duration::from_secs(5), &mut listener).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => tracing::warn!(
                    server = %self.name,
                    task_panicked = error.is_panic(),
                    task_cancelled = error.is_cancelled(),
                    "MCP notification stream task failed"
                ),
                Err(_) => {
                    tracing::warn!(
                        server = %self.name,
                        "MCP notification stream did not stop after cancellation; aborting"
                    );
                    listener.abort();
                    let _ = listener.await;
                }
            }
        }

        let mut guard = self.inner.lock().await;
        if matches!(guard.as_ref(), Some(McpClientInner::Stdio(_))) {
            if protocol_mode.is_legacy() {
                let id = self.next_id.fetch_add(1, Ordering::SeqCst);
                if guard
                    .as_mut()
                    .expect("checked stdio client")
                    .request(id, "shutdown", None, &protocol_mode, &[])
                    .await
                    .is_err()
                {
                    tracing::debug!("MCP stdio shutdown request failed");
                }
                if guard
                    .as_mut()
                    .expect("checked stdio client")
                    .notify("exit", None, &protocol_mode)
                    .await
                    .is_err()
                {
                    tracing::debug!("MCP stdio exit notification failed");
                }
            }
            if let Some(McpClientInner::Stdio(s)) = guard.as_mut()
                && s.child.start_kill().is_err()
            {
                tracing::debug!("MCP stdio child kill failed");
            }
            if let Some(McpClientInner::Stdio(s)) = guard.as_mut()
                && s.child.wait().await.is_err()
            {
                tracing::debug!("MCP stdio child wait failed");
            }
        }
        *guard = None;
        *self.protocol_mode.lock().await = McpProtocolMode::Uninitialized;
        *self.status.lock().await = McpClientStatus::Disconnected;
        Ok(())
    }

    async fn list_tool_snapshot(&self) -> anyhow::Result<McpToolListSnapshot> {
        let protocol_mode = self.protocol_mode.lock().await.clone();
        if self.inner.lock().await.is_none() {
            anyhow::bail!("MCP client '{}' is not connected", self.name);
        }

        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let mut pages = 0usize;
        let mut ttl_ms = None::<u64>;
        let mut cache_scope = McpToolCacheScope::Public;
        let fetched_at = Instant::now();
        loop {
            pages += 1;
            if pages > MAX_TOOL_LIST_PAGES {
                anyhow::bail!(
                    "MCP tools/list from '{}' exceeded the page limit",
                    self.name
                );
            }
            let params = cursor
                .as_ref()
                .map(|cursor| serde_json::json!({"cursor": cursor}));
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let result = {
                let mut guard = self.inner.lock().await;
                let Some(inner) = guard.as_mut() else {
                    anyhow::bail!("MCP client '{}' disconnected during tools/list", self.name);
                };
                let result = inner
                    .request(id, "tools/list", params, &protocol_mode, &[])
                    .await;
                self.discard_interrupted_stdio(&mut guard).await;
                result?
            };
            let page: McpToolListPage = serde_json::from_value(result)
                .map_err(|e| anyhow::anyhow!("invalid tools/list from '{}': {}", self.name, e))?;
            if protocol_mode.is_modern() {
                let page_ttl = page.ttl_ms.unwrap_or(0);
                ttl_ms = Some(ttl_ms.map_or(page_ttl, |current| current.min(page_ttl)));
                if page.cache_scope.as_deref() != Some("public") {
                    cache_scope = McpToolCacheScope::Private;
                }
            }
            if tools.len().saturating_add(page.tools.len()) > MAX_TOOLS_PER_SERVER {
                anyhow::bail!(
                    "MCP tools/list from '{}' exceeded the tool limit",
                    self.name
                );
            }
            tools.extend(page.tools);
            cursor = page.next_cursor;
            let Some(next_cursor) = cursor.as_ref() else {
                break;
            };
            if next_cursor.is_empty() || next_cursor.len() > MAX_TOOL_LIST_CURSOR_BYTES {
                anyhow::bail!("MCP tools/list returned an invalid pagination cursor");
            }
            if !seen_cursors.insert(next_cursor.clone()) {
                anyhow::bail!("MCP tools/list repeated a pagination cursor");
            }
        }

        validate_unique_tool_names(&tools)?;
        if protocol_mode.is_modern() && matches!(self.transport, McpServerTransport::Http) {
            tools.retain(|tool| {
                if let Err(reason) = http_parameter_headers(&tool.input_schema, &Value::Null) {
                    tracing::warn!(
                        server = %self.name,
                        tool = %tool.name,
                        reason_bytes = reason.len(),
                        "MCP tool excluded because its HTTP parameter-header schema is invalid"
                    );
                    false
                } else {
                    true
                }
            });
        }
        let expires_at =
            ttl_ms.map(|ttl_ms| fetched_at + Duration::from_millis(ttl_ms).min(MAX_TOOL_CACHE_TTL));
        Ok(McpToolListSnapshot {
            tools,
            expires_at,
            _cache_scope: cache_scope,
        })
    }

    /// Drop a stdio client after an interrupted request. Its line reader may
    /// already have consumed a partial JSON-RPC frame, so later requests must
    /// not reuse the remaining bytes as a fresh response.
    async fn discard_interrupted_stdio(&self, guard: &mut Option<McpClientInner>) {
        let interrupted = matches!(
            guard.as_ref(),
            Some(McpClientInner::Stdio(stdio)) if stdio.poisoned()
        );
        if !interrupted {
            return;
        }

        *guard = None;
        let error = "MCP stdio connection discarded after an interrupted request".to_string();
        *self.status.lock().await = McpClientStatus::Offline {
            error: error.clone(),
        };
        *self.last_error.lock().await = Some(error);
    }

    async fn refresh_tool_list_after_notification<F>(&self, on_tool_list_changed: F)
    where
        F: Fn(&str),
    {
        // Serialize notification refreshes with TTL-driven reads and mark the
        // prior snapshot stale before the network round trip. A reader waiting
        // on `tool_list_refresh` will then observe only the refreshed result.
        let _refresh_guard = self.tool_list_refresh.lock().await;
        if let Some(snapshot) = self.tools_cache.lock().await.as_mut() {
            snapshot.expires_at = Some(Instant::now());
        }
        match self.list_tool_snapshot().await {
            Ok(snapshot) => *self.tools_cache.lock().await = Some(snapshot),
            Err(_) => {
                *self.tools_cache.lock().await = None;
                tracing::error!(
                    server = %self.name,
                    "MCP tools/list refresh failed; cache cleared"
                );
            }
        }
        // The catalog version must describe the already-published cache.
        // Bumping before the assignment can let a concurrent prompt cache old
        // schemas under the new generation.
        on_tool_list_changed(&self.name);
    }

    /// Set calls-per-second rate limit for this client (refine §4.5).
    #[cfg(any(test, feature = "test-support"))]
    pub async fn set_rate_limit(&self, calls_per_second: f64) {
        let mut rl = self.rate_limiter.lock().await;
        rl.capacity = calls_per_second.max(1.0);
        rl.refill_rate = calls_per_second.max(0.001);
    }

    /// Register a callback for `notifications/tools/list_changed` (refine §4.6).
    ///
    /// The returned task handle can be awaited after cancellation when the
    /// caller needs an explicit listener shutdown barrier.
    pub fn start_notification_listener(
        self: Arc<McpClient>,
        on_tool_list_changed: impl Fn(&str) + Send + Sync + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let notification_cancel = self.notification_cancel.clone();
        tokio::spawn(async move {
            enum Notification {
                ListChanged,
                Message(Option<Value>),
                ClientCancelled,
            }

            loop {
                let client_cancel = self.cancel_token.lock().await.clone();
                let notification = {
                    let mut rx_guard = self.notification_rx.lock().await;
                    match rx_guard.as_mut() {
                        Some(rx) => {
                            let signal = self.tools_list_changed.clone();
                            tokio::select! {
                                biased;
                                _ = notification_cancel.cancelled() => return,
                                _ = client_cancel.cancelled() => Notification::ClientCancelled,
                                _ = signal.notified() => Notification::ListChanged,
                                message = rx.recv() => Notification::Message(message),
                            }
                        }
                        None => {
                            let signal = self.tools_list_changed.clone();
                            tokio::select! {
                                biased;
                                _ = notification_cancel.cancelled() => return,
                                _ = client_cancel.cancelled() => Notification::ClientCancelled,
                                _ = signal.notified() => Notification::ListChanged,
                                _ = tokio::time::sleep(Duration::from_secs(1)) => continue,
                            }
                        }
                    }
                };

                match notification {
                    Notification::ClientCancelled => {
                        // A transport token is cancelled both during reconnect
                        // and after a failed connect attempt. Keep the
                        // manager-owned listener alive for a later generation;
                        // terminal shutdown cancels `notification_cancel`.
                        tokio::select! {
                            _ = notification_cancel.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                        }
                    }
                    Notification::ListChanged => {
                        if !self.tools_list_changed.take() {
                            continue;
                        }
                        self.refresh_tool_list_after_notification(&on_tool_list_changed)
                            .await;
                    }
                    Notification::Message(Some(msg)) => {
                        let method = msg["method"].as_str();
                        if method == Some("notifications/tools/list_changed") {
                            self.tools_list_changed.request();
                        }
                    }
                    Notification::Message(None) => {
                        // Channel closed (reconnect case)
                        tokio::select! {
                            _ = notification_cancel.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        }
                    }
                }
            }
        })
    }

    pub async fn call_tool(
        &self,
        tool_name: &str,
        input: Value,
        cancel: CancellationToken,
    ) -> anyhow::Result<McpCallOutput> {
        // Rate limiting (refine §4.5). The pacing wait is part of the same
        // cancellation chain as the network request.
        let client_cancel = self.cancel_token.lock().await.clone();
        loop {
            let wait = {
                let mut rl = self.rate_limiter.lock().await;
                if rl.acquire() {
                    Duration::ZERO
                } else {
                    rl.wait_duration()
                }
            };
            if wait.is_zero() {
                break;
            }
            tokio::select! {
                _ = client_cancel.cancelled() => {
                    anyhow::bail!("MCP call '{}' cancelled (client shutting down)", tool_name);
                }
                _ = cancel.cancelled() => {
                    anyhow::bail!("MCP call '{}' cancelled (session cancellation)", tool_name);
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }

        // Clone the client's own shutdown/reconnect token so that an in-flight
        // request can be interrupted when shutdown_all/reconnect cancels it.
        // Without this, call_tool would hold `inner` lock for up to 30s
        // (REQUEST_TIMEOUT_SECS), blocking shutdown/reconnect entirely.
        let mut guard = tokio::select! {
            biased;
            _ = client_cancel.cancelled() => {
                anyhow::bail!("MCP call '{}' cancelled (client shutting down)", tool_name);
            }
            _ = cancel.cancelled() => {
                anyhow::bail!("MCP call '{}' cancelled (session cancellation)", tool_name);
            }
            guard = self.inner.lock() => guard,
        };
        if guard.is_none() {
            anyhow::bail!("MCP client '{}' is not connected", self.name);
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let protocol_mode = self.protocol_mode.lock().await.clone();
        let parameter_headers = if protocol_mode.is_modern()
            && matches!(self.transport, McpServerTransport::Http)
        {
            let schema = self
                .tools_cache
                .lock()
                .await
                .as_ref()
                .and_then(|snapshot| snapshot.tools.iter().find(|tool| tool.name == tool_name))
                .map(|tool| tool.input_schema.clone())
                .ok_or_else(|| anyhow::anyhow!("MCP tool schema is unavailable for this call"))?;
            http_parameter_headers(&schema, &input).map_err(|_| {
                anyhow::anyhow!("MCP tool arguments do not match HTTP routing metadata")
            })?
        } else {
            Vec::new()
        };
        let (result, cancelled_by_client) = tokio::select! {
            r = guard.as_mut().expect("checked MCP client inner").request(
                id,
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": input,
                })),
                &protocol_mode,
                &parameter_headers,
            ) => (Some(r), None),
            _ = client_cancel.cancelled() => (None, Some(true)),
            _ = cancel.cancelled() => (None, Some(false)),
        };
        let result = match result {
            Some(Ok(result)) => result,
            Some(Err(error)) => {
                self.discard_interrupted_stdio(&mut guard).await;
                return Err(error);
            }
            None => {
                self.discard_interrupted_stdio(&mut guard).await;
                if cancelled_by_client == Some(true) {
                    anyhow::bail!("MCP call '{}' cancelled (client shutting down)", tool_name);
                }
                anyhow::bail!("MCP call '{}' cancelled (session cancellation)", tool_name);
            }
        };

        let content = result["content"].as_array().cloned().unwrap_or_default();
        let extracted = extract_mcp_content(&content, self.max_binary_base64_bytes);

        let is_error = result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(McpCallOutput {
            success: !is_error,
            output: extracted.output,
            error: is_error.then_some(extracted.text_summary),
        })
    }

    pub async fn is_alive(&self) -> bool {
        match self.transport {
            McpServerTransport::Stdio => {
                let mut guard = self.inner.lock().await;
                match guard.as_mut() {
                    Some(McpClientInner::Stdio(s)) if s.poisoned() => false,
                    Some(McpClientInner::Stdio(s)) => {
                        s.child.try_wait().map(|st| st.is_none()).unwrap_or(false)
                    }
                    _ => false,
                }
            }
            McpServerTransport::Http => {
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

        if self.shutdown_inner(false).await.is_err() {
            tracing::warn!(
                server = %self.name,
                "MCP reconnect: shutdown of previous session failed"
            );
        }
        self.connect().await?;

        *self.last_error.lock().await = None;
        *self.last_seen_at.lock().await = Some(chrono::Utc::now().timestamp());

        Ok(())
    }

    /// The manager owns monitor handles and the client implementation stays
    /// private to the MCP crate.
    pub(crate) fn spawn_monitor_task(
        self: Arc<McpClient>,
        health_interval: Duration,
        initial_backoff: Duration,
        max_backoff: Duration,
        max_retries: u32,
        status_tx: tokio::sync::broadcast::Sender<McpStatusChangeEvent>,
        connect_slots: Arc<Semaphore>,
    ) -> tokio::task::JoinHandle<()> {
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

                        let cancel = self.cancel_token.lock().await.clone();
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            _ = tokio::time::sleep(backoff) => {}
                        }

                        if self.cancel_token.lock().await.is_cancelled() {
                            return;
                        }

                        *self.status.lock().await = McpClientStatus::Connecting;
                        let _ = status_tx.send(McpStatusChangeEvent {
                            name: self.name.clone(),
                            status: McpClientStatus::Connecting,
                        });

                        let shutdown_ok = self.shutdown_inner(false).await;
                        if shutdown_ok.is_err() {
                            tracing::warn!(
                                server = %self.name,
                                "reconnect shutdown cleanup failed"
                            );
                        }

                        let permit = match connect_slots.clone().acquire_owned().await {
                            Ok(permit) => permit,
                            Err(error) => {
                                tracing::warn!(
                                    server = %self.name,
                                    error = %error,
                                    "MCP reconnect admission closed"
                                );
                                return;
                            }
                        };
                        let connect_result = self.connect().await;
                        drop(permit);
                        match connect_result {
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
        })
    }
}

fn validate_unique_tool_names(tools: &[McpToolInfo]) -> anyhow::Result<()> {
    let mut names = HashSet::with_capacity(tools.len());
    if tools
        .iter()
        .any(|tool| tool.name.trim().is_empty() || !names.insert(tool.name.as_str()))
    {
        anyhow::bail!("invalid tools/list: tool names must be non-empty and unique");
    }
    Ok(())
}

#[cfg(test)]
mod stdio_poison_tests {
    use super::*;
    use std::{fs, path::PathBuf, thread, time::Duration};

    struct TestFiles {
        root: PathBuf,
    }

    impl Drop for TestFiles {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn poisoned_stdio_is_reported_dead_and_discarded() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "haven-mcp-poisoned-stdio-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create isolated fake server working directory");
        fs::write(root.join("stdio_liveness_child"), b"test")
            .expect("mark fake server working directory");
        let files = TestFiles { root };

        let config = haven_common::config::McpServerConfig {
            name: "poisoned-stdio-test".into(),
            command: std::env::current_exe()
                .expect("current test executable")
                .to_string_lossy()
                .into_owned(),
            args: vec![
                "--exact".into(),
                "client::stdio_poison_tests::stdio_liveness_child_helper".into(),
                "--nocapture".into(),
            ],
            cwd: Some(files.root.to_string_lossy().into_owned()),
            ..Default::default()
        };

        let client = McpClient::new(&config, 1024, 1024);
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let mut stdio = client
            .spawn_stdio(notification_tx)
            .await
            .expect("spawn live test server");
        assert!(
            stdio
                .child
                .try_wait()
                .expect("inspect child process")
                .is_none(),
            "the test child must still be alive"
        );
        stdio.mark_reader_failed_for_test();
        *client.inner.lock().await = Some(McpClientInner::Stdio(Box::new(stdio)));

        assert!(
            !client.is_alive().await,
            "a poisoned stdio connection must trigger the existing health monitor"
        );
        let mut guard = client.inner.lock().await;
        client.discard_interrupted_stdio(&mut guard).await;
        assert!(
            guard.is_none(),
            "poisoned child transport must be discarded"
        );
        assert!(matches!(
            &*client.status.lock().await,
            McpClientStatus::Offline { .. }
        ));
    }

    #[test]
    fn stdio_liveness_child_helper() {
        let Ok(root) = std::env::current_dir() else {
            return;
        };
        if !root.join("stdio_liveness_child").exists() {
            return;
        }
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tool_list_identity_tests {
    use super::*;

    fn tool(name: &str) -> McpToolInfo {
        McpToolInfo {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn tools_list_rejects_duplicate_and_blank_names() {
        assert!(validate_unique_tool_names(&[tool("same"), tool("same")]).is_err());
        assert!(validate_unique_tool_names(&[tool(" ")]).is_err());
        assert!(validate_unique_tool_names(&[tool("first"), tool("second")]).is_ok());
    }
}

#[cfg(test)]
mod modern_tool_list_tests {
    use super::*;
    use crate::transport::HttpInner;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_http_jsonrpc_request(
        stream: TcpStream,
    ) -> (tokio::net::tcp::OwnedWriteHalf, Value) {
        let (read_half, write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let mut content_length = 0usize;
        loop {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                content_length = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; content_length];
        reader.read_exact(&mut body).await.unwrap();
        (write_half, serde_json::from_slice(&body).unwrap())
    }

    async fn respond_jsonrpc(
        mut stream: tokio::net::tcp::OwnedWriteHalf,
        id: Value,
        result: Value,
    ) {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[tokio::test]
    async fn tools_list_aggregates_pages_and_uses_the_most_restrictive_cache_hint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "tools/list");
            assert!(request["params"].get("cursor").is_none());
            assert_eq!(
                request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                MODERN_PROTOCOL_VERSION
            );
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "tools": [{"name": "first", "inputSchema": {"type": "object"}}],
                    "nextCursor": "page-two",
                    "ttlMs": 50_000,
                    "cacheScope": "public"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "tools/list");
            assert_eq!(request["params"]["cursor"], "page-two");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "tools": [{"name": "second", "inputSchema": {"type": "object"}}],
                    "ttlMs": 5_000,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let config = haven_common::config::McpServerConfig {
            name: "paged-modern-list".into(),
            transport: McpServerTransport::Http,
            url: format!("http://{address}/mcp"),
            ..Default::default()
        };
        let client = McpClient::new(&config, 1024, 1024);
        *client.protocol_mode.lock().await = McpProtocolMode::modern(MODERN_PROTOCOL_VERSION);
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let cancel = client.cancel_token.lock().await.clone();
        let shared = Arc::new(HttpShared {
            http: reqwest::Client::new(),
            url: config.url,
            headers: Vec::new(),
            mcp_session_id: Arc::new(Mutex::new(String::new())),
            cancel,
        });
        *client.inner.lock().await = Some(McpClientInner::Http(HttpInner {
            shared,
            notification_tx,
            max_sse_buffer: 1024,
            tools_list_changed: client.tools_list_changed.clone(),
        }));

        let started = Instant::now();
        let snapshot = client.list_tool_snapshot().await.unwrap();
        server.await.unwrap();
        assert_eq!(
            snapshot
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(snapshot._cache_scope, McpToolCacheScope::Private);
        assert!(snapshot.is_fresh());
        assert!(snapshot.expires_at.unwrap() <= started + Duration::from_secs(6));
    }

    #[tokio::test]
    async fn notification_publishes_refreshed_cache_before_catalog_invalidation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "tools/list");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "tools": [{"name": "published", "inputSchema": {"type": "object"}}],
                    "ttlMs": 60_000,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let config = haven_common::config::McpServerConfig {
            name: "catalog-publication".into(),
            transport: McpServerTransport::Http,
            url: format!("http://{address}/mcp"),
            ..Default::default()
        };
        let client = McpClient::new(&config, 1024, 1024);
        *client.protocol_mode.lock().await = McpProtocolMode::modern(MODERN_PROTOCOL_VERSION);
        *client.tools_cache.lock().await = Some(McpToolListSnapshot {
            tools: vec![McpToolInfo {
                name: "stale".into(),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
            }],
            expires_at: Some(Instant::now() + Duration::from_secs(60)),
            _cache_scope: McpToolCacheScope::Private,
        });
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let cancel = client.cancel_token.lock().await.clone();
        let shared = Arc::new(HttpShared {
            http: reqwest::Client::new(),
            url: config.url,
            headers: Vec::new(),
            mcp_session_id: Arc::new(Mutex::new(String::new())),
            cancel,
        });
        *client.inner.lock().await = Some(McpClientInner::Http(HttpInner {
            shared,
            notification_tx,
            max_sse_buffer: 1024,
            tools_list_changed: client.tools_list_changed.clone(),
        }));

        let observed = client.tools_cache.clone();
        client
            .refresh_tool_list_after_notification(move |_| {
                assert_eq!(
                    observed
                        .try_lock()
                        .unwrap()
                        .as_ref()
                        .map(|snapshot| snapshot
                            .tools
                            .iter()
                            .map(|tool| tool.name.as_str())
                            .collect::<Vec<_>>()),
                    Some(vec!["published"]),
                    "the cache must be visible before the new catalog generation is published"
                );
            })
            .await;
        server.await.unwrap();
    }
}
