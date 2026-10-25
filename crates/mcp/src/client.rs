mod feature_runtime;
mod subscriptions;

use crate::features::{McpPromptList, McpResourceList, McpResourceRead, McpServerCapabilities};
use crate::protocol::{
    MODERN_PROTOCOL_VERSION, McpCallOutput, McpClientStatus, McpRpcError, McpServerSnapshot,
    McpStatusChangeEvent, McpToolInfo, McpToolListPage, extract_mcp_content,
    http_parameter_headers,
};
use crate::transport::{
    HttpInner, HttpShared, McpClientInner, NOTIFICATION_QUEUE_CAPACITY, StdioInner,
    ToolsListChangedSignal, http_is_alive,
};
use crate::{McpStdioLaunchSpec, McpStdioProcess, McpStdioProcessLauncher};
use futures_util::future::BoxFuture;
use haven_common::{
    config::{NetworkAccessBoundary, NetworkResourcePolicy, NetworkResourcePolicySnapshot},
    mcp::McpServerTransport,
    types::NetworkPolicy,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;

#[cfg(all(test, windows))]
mod stdio_process_tree_tests;

const MAX_TOOL_LIST_PAGES: usize = 100;
const MAX_TOOLS_PER_SERVER: usize = 10_000;
const MAX_TOOL_LIST_CURSOR_BYTES: usize = 4096;
const MAX_TOOL_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Give stdio servers only the minimal launcher environment and this profile's
/// explicit runtime variables. Environment entries may opt into GUI launchers
/// by overriding the safe `MCP_AUTOLAUNCH=0` default.
fn configure_stdio_server_environment(
    command: &mut std::process::Command,
    inherited_environment: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    configured_environment: &[String],
) {
    command
        .env_clear()
        .envs(
            haven_platform::process_environment::filter_runtime_baseline_environment(
                inherited_environment,
            ),
        )
        .env("MCP_AUTOLAUNCH", "0");
    for entry in configured_environment {
        if let Some((key, value)) = entry.split_once('=') {
            command.env(key, value);
        }
    }
}

#[cfg(test)]
mod stdio_launcher_tests {
    use super::*;
    use crate::{McpStdioReader, McpStdioWriter};
    use futures_util::future::BoxFuture;
    use std::sync::Mutex as StdMutex;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct StreamProcess {
        stdin: Option<McpStdioWriter>,
        stdout: Option<McpStdioReader>,
        alive: Arc<std::sync::atomic::AtomicBool>,
    }

    impl McpStdioProcess for StreamProcess {
        fn take_stdin(&mut self) -> std::io::Result<McpStdioWriter> {
            self.stdin
                .take()
                .ok_or_else(|| std::io::Error::other("stdin already taken"))
        }

        fn take_stdout(&mut self) -> std::io::Result<McpStdioReader> {
            self.stdout
                .take()
                .ok_or_else(|| std::io::Error::other("stdout already taken"))
        }

        fn take_stderr(&mut self) -> std::io::Result<Option<McpStdioReader>> {
            Ok(None)
        }

        fn is_alive(&mut self) -> std::io::Result<bool> {
            Ok(self.alive.load(Ordering::Acquire))
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            self.alive.store(false, Ordering::Release);
            Ok(())
        }

        fn wait(&mut self) -> BoxFuture<'_, std::io::Result<()>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct StreamLauncher {
        debug_spec: StdMutex<Option<String>>,
        server_stream: StdMutex<Option<tokio::io::DuplexStream>>,
        alive: Arc<std::sync::atomic::AtomicBool>,
    }

    impl McpStdioProcessLauncher for StreamLauncher {
        fn launch<'a>(
            &'a self,
            spec: &'a McpStdioLaunchSpec,
            permit: Box<dyn crate::McpStdioProcessStartPermit>,
        ) -> BoxFuture<'a, anyhow::Result<Box<dyn McpStdioProcess>>> {
            Box::pin(async move {
                *self
                    .debug_spec
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!("{spec:?}"));
                let (client_stream, server_stream) = tokio::io::duplex(4096);
                let (stdout, stdin) = tokio::io::split(client_stream);
                *self
                    .server_stream
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(server_stream);
                self.alive.store(true, Ordering::Release);
                drop(permit);
                Ok(Box::new(StreamProcess {
                    stdin: Some(Box::new(stdin)),
                    stdout: Some(Box::new(stdout)),
                    alive: self.alive.clone(),
                }) as Box<dyn McpStdioProcess>)
            })
        }
    }

    #[tokio::test]
    async fn injected_process_stream_uses_the_same_mcp_protocol_and_redacts_launch_data() {
        let launcher = Arc::new(StreamLauncher {
            debug_spec: StdMutex::new(None),
            server_stream: StdMutex::new(None),
            alive: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let config = haven_common::config::McpServerConfig {
            name: "stream-test".into(),
            command: "sensitive-command".into(),
            args: vec!["sensitive-argument".into()],
            env: vec!["TOKEN=sensitive-secret".into()],
            cwd: Some("C:\\private\\working".into()),
            ..Default::default()
        };
        let client = McpClient::new_managed_with_launcher(
            &config,
            1024,
            1024,
            Some(Arc::new(TestMcpStdioProcessStartAuthorization)),
            launcher.clone(),
        );
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let stdio = client.spawn_stdio(notification_tx).await.unwrap();

        let debug_spec = launcher
            .debug_spec
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .unwrap();
        assert!(!debug_spec.contains("sensitive-command"));
        assert!(!debug_spec.contains("sensitive-argument"));
        assert!(!debug_spec.contains("sensitive-secret"));
        assert!(!debug_spec.contains("C:\\private\\working"));

        let server_stream = launcher
            .server_stream
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap();
        let (server_read, mut server_write) = tokio::io::split(server_stream);
        let server = tokio::spawn(async move {
            let mut reader = BufReader::new(server_read);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "tools/list");
            server_write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"tools\":[]}}\n")
                .await
                .unwrap();
        });

        let response = stdio.request(1, "tools/list", None).await.unwrap();
        assert_eq!(response["tools"], serde_json::json!([]));
        server.await.unwrap();
        drop(stdio);
        assert!(!launcher.alive.load(Ordering::Acquire));
    }

    struct FailingWaitProcess {
        stdin: Option<McpStdioWriter>,
        stdout: Option<McpStdioReader>,
        kill_requested: Arc<std::sync::atomic::AtomicBool>,
    }

    impl McpStdioProcess for FailingWaitProcess {
        fn take_stdin(&mut self) -> std::io::Result<McpStdioWriter> {
            self.stdin
                .take()
                .ok_or_else(|| std::io::Error::other("stdin already taken"))
        }

        fn take_stdout(&mut self) -> std::io::Result<McpStdioReader> {
            self.stdout
                .take()
                .ok_or_else(|| std::io::Error::other("stdout already taken"))
        }

        fn take_stderr(&mut self) -> std::io::Result<Option<McpStdioReader>> {
            Ok(None)
        }

        fn is_alive(&mut self) -> std::io::Result<bool> {
            Ok(true)
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            self.kill_requested.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn wait(&mut self) -> BoxFuture<'_, std::io::Result<()>> {
            Box::pin(async { Err(std::io::Error::other("simulated stop proof failure")) })
        }
    }

    #[tokio::test]
    async fn shutdown_reports_unconfirmed_stop_and_retains_the_process_owner() {
        let config = haven_common::config::McpServerConfig {
            name: "stop-proof-test".into(),
            ..Default::default()
        };
        let client = McpClient::new(&config, 1024, 1024);
        let (client_stream, _server_stream) = tokio::io::duplex(1024);
        let (stdout, stdin) = tokio::io::split(client_stream);
        let kill_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let process = Box::new(FailingWaitProcess {
            stdin: Some(Box::new(stdin)),
            stdout: Some(Box::new(stdout)),
            kill_requested: kill_requested.clone(),
        });
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let stdio = StdioInner::new(
            process,
            notification_tx,
            Arc::new(ToolsListChangedSignal::new()),
            CancellationToken::new(),
        )
        .unwrap();
        *client.inner.lock().await = Some(McpClientInner::Stdio(Box::new(stdio)));

        let error = client
            .shutdown()
            .await
            .expect_err("shutdown must report when the process stop cannot be proven");

        assert!(error.to_string().contains("stop could not be confirmed"));
        assert!(kill_requested.load(Ordering::SeqCst));
        assert!(client.inner.lock().await.is_some());
    }
}

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

#[derive(Debug, Clone)]
struct McpResourceListSnapshot {
    result: McpResourceList,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct McpResourceTemplateListSnapshot {
    result: crate::features::McpResourceTemplateList,
    expires_at: Option<Instant>,
}

#[derive(Debug, Clone)]
struct McpResourceReadCacheEntry {
    result: McpResourceRead,
    expires_at: Option<Instant>,
}

struct ResourceSubscription {
    subscription_id: u64,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone)]
struct McpPromptListSnapshot {
    result: McpPromptList,
    expires_at: Option<Instant>,
}

impl McpToolListSnapshot {
    fn is_fresh(&self) -> bool {
        self.expires_at
            .is_none_or(|expires_at| Instant::now() < expires_at)
    }
}

pub struct McpClient {
    pub(crate) name: String,
    /// Manager-owned identity of the live connection generation. Authorization
    /// binds opaque MCP calls to this value so reconnects cannot redirect a
    /// pending confirmation to a different endpoint.
    manager_generation: AtomicU64,
    authorization_config_fingerprint: String,
    pub(crate) transport: McpServerTransport,
    pub(crate) command: String,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<String>,
    /// Working directory the stdio server is spawned from (optional).
    pub(crate) cwd: Option<String>,
    pub(crate) url: String,
    pub(crate) enabled: Arc<AtomicBool>,
    pub(crate) inner: Arc<Mutex<Option<McpClientInner>>>,
    server_capabilities: Arc<Mutex<McpServerCapabilities>>,
    pub(crate) status: Arc<Mutex<McpClientStatus>>,
    pub(crate) next_id: Arc<AtomicU64>,
    pub(crate) tools_cache: Arc<Mutex<Option<McpToolListSnapshot>>>,
    tool_list_refresh: Mutex<()>,
    resource_list_cache: Mutex<Option<McpResourceListSnapshot>>,
    resource_list_refresh: Mutex<()>,
    resource_template_list_cache: Mutex<Option<McpResourceTemplateListSnapshot>>,
    resource_template_list_refresh: Mutex<()>,
    resource_read_cache: Mutex<HashMap<String, McpResourceReadCacheEntry>>,
    resource_subscriptions: Mutex<HashMap<String, ResourceSubscription>>,
    resource_subscription_acks: Mutex<HashMap<u64, (String, tokio::sync::oneshot::Sender<bool>)>>,
    pending_feature_inputs: Mutex<HashMap<u64, feature_runtime::PendingFeatureRequest>>,
    prompt_list_cache: Mutex<Option<McpPromptListSnapshot>>,
    prompt_list_refresh: Mutex<()>,
    pub(crate) last_error: Arc<Mutex<Option<String>>>,
    /// Protocol discovery and tool-catalog diagnostics; see `McpServerSnapshot`.
    pub(crate) last_diagnostic: Arc<Mutex<Option<String>>>,
    pub(crate) last_seen_at: Arc<Mutex<Option<i64>>>,
    pub(crate) reconnect_retries: Arc<Mutex<u32>>,
    pub(crate) cancel_token: Arc<Mutex<CancellationToken>>,
    /// Lifetime cancellation for the notification listener. Unlike the
    /// transport token, it survives reconnects so the listener can reuse the
    /// receiver after a new connection is installed.
    pub(crate) notification_cancel: CancellationToken,
    /// Join owner for the current generation's server notification stream:
    /// modern `subscriptions/listen` response stream.
    pub(crate) notification_stream_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    pub(crate) rate_limiter: Arc<Mutex<RateLimiter>>,
    pub(crate) notification_rx: Arc<Mutex<Option<tokio::sync::mpsc::Receiver<Value>>>>,
    pub(crate) tools_list_changed: Arc<ToolsListChangedSignal>,
    /// Maximum bytes of an image/audio/resource Base64 representation kept in
    /// observations before being replaced by an `oversized` marker.
    pub(crate) max_binary_base64_bytes: usize,
    /// Single buffered (incomplete) SSE line/event cap for the HTTP transport.
    pub(crate) max_sse_buffer: usize,
    /// Runtime network snapshot. Direct clients fail closed at construction;
    /// the manager assigns the authorization owner's snapshot before a
    /// configured connection is started.
    pub(crate) network_policy: Arc<RwLock<NetworkResourcePolicySnapshot>>,
    /// Immutable policy snapshot used by every stdio spawn and reconnect for
    /// this manager-owned client generation.
    stdio_process_authorization: Option<Arc<dyn crate::McpStdioProcessStartAuthorization>>,
    stdio_launch_spec: McpStdioLaunchSpec,
    stdio_process_launcher: Arc<dyn McpStdioProcessLauncher>,
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
        let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
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

pub(crate) fn local_stdio_process_launcher() -> Arc<dyn McpStdioProcessLauncher> {
    Arc::new(LocalMcpStdioProcessLauncher)
}

struct LocalMcpStdioProcessLauncher;

struct LocalMcpStdioProcess {
    child: Child,
    _containment: haven_platform::process_containment::ProcessContainment,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl McpStdioProcess for LocalMcpStdioProcess {
    fn take_stdin(&mut self) -> std::io::Result<crate::McpStdioWriter> {
        self.stdin
            .take()
            .map(|stream| Box::new(stream) as crate::McpStdioWriter)
            .ok_or_else(|| std::io::Error::other("MCP stdio stdin was already taken"))
    }

    fn take_stdout(&mut self) -> std::io::Result<crate::McpStdioReader> {
        self.stdout
            .take()
            .map(|stream| Box::new(stream) as crate::McpStdioReader)
            .ok_or_else(|| std::io::Error::other("MCP stdio stdout was already taken"))
    }

    fn take_stderr(&mut self) -> std::io::Result<Option<crate::McpStdioReader>> {
        Ok(self
            .stderr
            .take()
            .map(|stream| Box::new(stream) as crate::McpStdioReader))
    }

    fn is_alive(&mut self) -> std::io::Result<bool> {
        self.child.try_wait().map(|status| status.is_none())
    }

    fn start_kill(&mut self) -> std::io::Result<()> {
        self.child.start_kill()
    }

    fn wait(&mut self) -> BoxFuture<'_, std::io::Result<()>> {
        Box::pin(async move { self.child.wait().await.map(|_| ()) })
    }
}

impl McpStdioProcessLauncher for LocalMcpStdioProcessLauncher {
    fn launch<'a>(
        &'a self,
        spec: &'a McpStdioLaunchSpec,
        permit: Box<dyn crate::McpStdioProcessStartPermit>,
    ) -> BoxFuture<'a, anyhow::Result<Box<dyn McpStdioProcess>>> {
        Box::pin(async move {
            // MCP servers may use relative command/cwd paths from trusted
            // configuration; this local launcher preserves that behavior.
            let build = |program: &str, args: &[String]| {
                let mut command = Command::new(program);
                command
                    .args(args)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .kill_on_drop(true);
                configure_stdio_server_environment(
                    command.as_std_mut(),
                    std::env::vars_os(),
                    spec.environment(),
                );
                if let Some(working_directory) = spec.working_directory() {
                    command.current_dir(working_directory);
                }
                command
            };

            let containment = haven_platform::process_containment::ProcessContainment::new()
                .map_err(|error| {
                    anyhow::anyhow!(
                        "failed to create process containment for MCP server '{}': {error}",
                        spec.server_name()
                    )
                })?;
            let child = spawn_mcp_child(
                spec.server_name(),
                spec.command(),
                spec.args(),
                &build,
                &containment,
            )
            .map_err(|error| {
                let hint = windows_spawn_hint(spec.command(), &error);
                anyhow::anyhow!(
                    "failed to spawn MCP server '{}': {}{}",
                    spec.server_name(),
                    haven_common::error::sanitize_error_text(&error.to_string()),
                    hint
                )
            })?;

            // `spawn_mcp_child` returns only after OS containment has attached
            // and the process has resumed.
            drop(permit);
            let mut process = LocalMcpStdioProcess {
                child,
                _containment: containment,
                stdin: None,
                stdout: None,
                stderr: None,
            };
            process.stdin = process.child.stdin.take();
            process.stdout = process.child.stdout.take();
            process.stderr = process.child.stderr.take();
            Ok(Box::new(process) as Box<dyn McpStdioProcess>)
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
struct TestMcpStdioProcessStartAuthorization;

#[cfg(any(test, feature = "test-support"))]
impl crate::McpStdioProcessStartAuthorization for TestMcpStdioProcessStartAuthorization {
    fn acquire_process_start_permit(&self) -> crate::McpStdioProcessStartPermitFuture<'_> {
        Box::pin(async { Ok(Box::new(()) as Box<dyn crate::McpStdioProcessStartPermit>) })
    }
}

impl McpClient {
    #[cfg(any(test, feature = "test-support"))]
    pub fn new(
        config: &haven_common::config::McpServerConfig,
        max_binary_base64_bytes: usize,
        max_sse_buffer: usize,
    ) -> Self {
        Self::new_inner(
            config,
            max_binary_base64_bytes,
            max_sse_buffer,
            Some(Arc::new(TestMcpStdioProcessStartAuthorization)),
            local_stdio_process_launcher(),
        )
    }

    pub(crate) fn new_managed_with_launcher(
        config: &haven_common::config::McpServerConfig,
        max_binary_base64_bytes: usize,
        max_sse_buffer: usize,
        stdio_process_authorization: Option<Arc<dyn crate::McpStdioProcessStartAuthorization>>,
        stdio_process_launcher: Arc<dyn McpStdioProcessLauncher>,
    ) -> Self {
        Self::new_inner(
            config,
            max_binary_base64_bytes,
            max_sse_buffer,
            stdio_process_authorization,
            stdio_process_launcher,
        )
    }

    fn new_inner(
        config: &haven_common::config::McpServerConfig,
        max_binary_base64_bytes: usize,
        max_sse_buffer: usize,
        stdio_process_authorization: Option<Arc<dyn crate::McpStdioProcessStartAuthorization>>,
        stdio_process_launcher: Arc<dyn McpStdioProcessLauncher>,
    ) -> Self {
        let authorization_config_fingerprint = mcp_target_config_fingerprint(config);
        Self {
            name: config.name.clone(),
            manager_generation: AtomicU64::new(0),
            authorization_config_fingerprint,
            transport: config.transport.clone(),
            command: config.command.clone(),
            args: config.args.clone(),
            env: config.env.clone(),
            cwd: config.cwd.clone(),
            url: config.url.clone(),
            enabled: Arc::new(AtomicBool::new(config.enabled)),
            inner: Arc::new(Mutex::new(None)),
            server_capabilities: Arc::new(Mutex::new(McpServerCapabilities::default())),
            status: Arc::new(Mutex::new(McpClientStatus::Disconnected)),
            next_id: Arc::new(AtomicU64::new(1)),
            tools_cache: Arc::new(Mutex::new(None)),
            tool_list_refresh: Mutex::new(()),
            resource_list_cache: Mutex::new(None),
            resource_list_refresh: Mutex::new(()),
            resource_template_list_cache: Mutex::new(None),
            resource_template_list_refresh: Mutex::new(()),
            resource_read_cache: Mutex::new(HashMap::new()),
            resource_subscriptions: Mutex::new(HashMap::new()),
            resource_subscription_acks: Mutex::new(HashMap::new()),
            pending_feature_inputs: Mutex::new(HashMap::new()),
            prompt_list_cache: Mutex::new(None),
            prompt_list_refresh: Mutex::new(()),
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
            network_policy: Arc::new(RwLock::new(NetworkResourcePolicySnapshot::new(
                0,
                NetworkResourcePolicy {
                    access: NetworkAccessBoundary::Restricted,
                    allowed_domains: Vec::new(),
                },
            ))),
            stdio_process_authorization,
            stdio_launch_spec: McpStdioLaunchSpec::new(
                config.name.clone(),
                config.command.clone(),
                config.args.clone(),
                config.env.clone(),
                config.cwd.clone(),
            ),
            stdio_process_launcher,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn set_manager_generation(&self, generation: u64) {
        self.manager_generation.store(generation, Ordering::SeqCst);
    }

    pub(crate) fn manager_generation(&self) -> u64 {
        self.manager_generation.load(Ordering::SeqCst)
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    pub(crate) async fn set_network_policy_snapshot(&self, policy: NetworkResourcePolicySnapshot) {
        *self.network_policy.write().await = policy;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn set_network_policy(&self, policy: NetworkPolicy) {
        let mut snapshot = self.network_policy.read().await.clone();
        snapshot.revision = snapshot.revision.saturating_add(1);
        snapshot.policy.access = policy.into();
        self.set_network_policy_snapshot(snapshot).await;
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn network_policy(&self) -> NetworkPolicy {
        self.network_policy.read().await.network_policy()
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

    /// Stable, opaque fingerprint of the connection configuration. Unlike the
    /// manager generation this remains useful for persisted resource grants;
    /// raw endpoints, commands, arguments, and environment values never leave
    /// this client.
    pub(crate) fn authorization_target_fingerprint(&self) -> String {
        self.authorization_config_fingerprint.clone()
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
        } else if cache.is_none()
            && !force
            && !matches!(&*self.status.lock().await, McpClientStatus::Connected)
        {
            // A connection may still be in progress. Do not issue tools/list
            // until its protocol mode and transport are installed. Once a
            // client is connected, however, a missing cache can mean that a
            // notification-driven refresh failed; retry on the next read.
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
        let capabilities = *self.server_capabilities.lock().await;
        McpServerSnapshot {
            name: self.name.clone(),
            transport: self.transport.clone(),
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
            cwd: self.cwd.clone(),
            url: self.url.clone(),
            enabled: self.enabled(),
            target_binding: None,
            supports_resources: capabilities.resources,
            supports_resource_subscriptions: capabilities.resource_subscribe,
            supports_prompts: capabilities.prompts,
            exposure: haven_common::config::McpExposureConfig::default(),
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
        let authorization = self.stdio_process_authorization.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "MCP stdio server '{}' has no process-start authorization",
                self.name
            )
        })?;
        let spawn_permit = authorization
            .acquire_process_start_permit()
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "MCP stdio server '{}' process start was denied: {}",
                    self.name,
                    haven_common::error::sanitize_error_text(&error.to_string())
                )
            })?;
        let process = self
            .stdio_process_launcher
            .launch(&self.stdio_launch_spec, spawn_permit)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to start MCP stdio server '{}': {}",
                    self.name,
                    haven_common::error::sanitize_error_text(&error.to_string())
                )
            })?;

        Ok(StdioInner::new(
            process,
            notification_tx,
            self.tools_list_changed.clone(),
            self.cancel_token.lock().await.clone(),
        )?)
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
        // X-API-Key) are attached to every request,
        // and reqwest only strips the fixed auth-header list on cross-host
        // redirects. Following a redirect would leak those to an unverified
        // host, so refuse to follow any.
        let policy = self.network_policy.read().await.clone();
        let built_client = crate::network::build_http_client(&self.url, &policy).await?;
        let shared = Arc::new(HttpShared {
            http: built_client.client,
            url: built_client.endpoint.to_string(),
            headers,
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
        self.connect_with_cancellation(None).await
    }

    pub(crate) async fn connect_with_cancellation(
        &self,
        external_cancel: Option<CancellationToken>,
    ) -> anyhow::Result<()> {
        *self.cancel_token.lock().await = CancellationToken::new();
        let connection_cancel = self.cancel_token.lock().await.clone();
        let cancellation_watcher = external_cancel.map(|external_cancel| {
            tokio::spawn(async move {
                tokio::select! {
                    _ = external_cancel.cancelled() => connection_cancel.cancel(),
                    _ = connection_cancel.cancelled() => {},
                }
            })
        });
        let network_policy = self.network_policy.read().await.clone();
        if matches!(self.transport, McpServerTransport::Stdio)
            && (!matches!(network_policy.network_policy(), NetworkPolicy::Open)
                || !network_policy.policy.allowed_domains.is_empty())
        {
            let error = anyhow::anyhow!(
                "MCP stdio server '{}' is blocked by network policy because its network use is opaque or its domain policy cannot be enforced",
                self.name
            );
            *self.status.lock().await = McpClientStatus::Offline {
                error: error.to_string(),
            };
            if let Some(watcher) = cancellation_watcher {
                watcher.abort();
                let _ = watcher.await;
            }
            return Err(error);
        }
        *self.status.lock().await = McpClientStatus::Connecting;
        *self.last_diagnostic.lock().await = None;
        // A new connection must repopulate this cache from the new server.
        // Keeping entries from a previous transport would expose tools that
        // were never confirmed on the current session.
        *self.tools_cache.lock().await = None;
        let result = self.connect_inner().await;
        if let Some(watcher) = cancellation_watcher {
            watcher.abort();
            let _ = watcher.await;
        }
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
        let mut inner = match self.transport {
            McpServerTransport::Stdio => {
                McpClientInner::Stdio(Box::new(self.spawn_stdio(notification_tx.clone()).await?))
            }
            McpServerTransport::Http => {
                let (_shared, inner) = self.spawn_http(notification_tx.clone()).await?;
                McpClientInner::Http(inner)
            }
        };

        // Every connection must implement the current stateless handshake.
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let discovery = inner.request(id, "server/discover", None, &[]).await;

        let result = match discovery {
            Ok(result) => result,
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
                        "MCP peer does not support the required modern protocol '{}'; it advertised {} version(s)",
                        MODERN_PROTOCOL_VERSION,
                        advertised
                    );
                }
                anyhow::bail!(
                    "MCP peer does not support the required modern server/discover handshake: {}",
                    haven_common::error::sanitize_error_text(&error.to_string())
                );
            }
        };
        let versions = result["supportedVersions"].as_array().ok_or_else(|| {
            anyhow::anyhow!("MCP server discovery response omitted supportedVersions")
        })?;
        if !versions
            .iter()
            .any(|version| version.as_str() == Some(MODERN_PROTOCOL_VERSION))
        {
            anyhow::bail!(
                "MCP server discovery did not advertise required protocol version '{}'",
                MODERN_PROTOCOL_VERSION
            );
        }
        let tools_capabilities = result
            .get("capabilities")
            .and_then(|value| value.get("tools"));
        let modern_tools_supported = tools_capabilities.is_some();
        let modern_tools_list_changed = tools_capabilities
            .and_then(|value| value.get("listChanged"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let capabilities =
            McpServerCapabilities::from_wire_capabilities(result.get("capabilities"));
        let server_info = &result["_meta"]["io.modelcontextprotocol/serverInfo"];
        let server_name = server_info["name"].as_str().unwrap_or("?").to_string();
        let server_version = server_info["version"].as_str().unwrap_or("?").to_string();
        let server_protocol = MODERN_PROTOCOL_VERSION.to_string();
        *self.server_capabilities.lock().await = capabilities;

        tracing::info!(
            server = %self.name,
            transport = self.transport.as_str(),
            protocol_version = server_protocol,
            "MCP server protocol selected"
        );

        *self.inner.lock().await = Some(inner);

        // Cache tools after successful connection. A successful handshake
        // with zero tools is suspicious (often a client/server SDK protocol
        // mismatch rather than an empty server), so record a diagnostic that
        // distinguishes the two cases.
        let list_result = if !modern_tools_supported {
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
                if tools.is_empty() && modern_tools_supported {
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

        if modern_tools_list_changed {
            let inner = self.inner.lock().await;
            if let Some(inner) = inner.as_ref() {
                let listener =
                    inner.spawn_tools_subscription(self.name.clone(), self.next_id.clone());
                *self.notification_stream_task.lock().await = Some(listener);
            }
        }

        Ok(())
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown_inner(true).await
    }

    async fn shutdown_inner(&self, stop_notification_listener: bool) -> anyhow::Result<()> {
        // Cancel the client's own token first so any in-flight `call_tool`
        // (which selects on this token) aborts and releases the `inner` lock.
        self.cancel_token.lock().await.cancel();
        let pending_inputs = std::mem::take(&mut *self.pending_feature_inputs.lock().await);
        for (_, pending) in pending_inputs {
            pending.cancel.cancel();
        }
        self.stop_resource_subscriptions().await;
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
        let mut stop_failure = None;
        if matches!(guard.as_ref(), Some(McpClientInner::Stdio(_)))
            && let Some(McpClientInner::Stdio(stdio)) = guard.as_mut()
        {
            if let Err(error) = stdio.close_stdin().await {
                tracing::debug!(
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "MCP stdio stdin did not close cleanly"
                );
            }

            let exited = match stdio.bounded_exit_wait().await {
                Ok(exited) => exited,
                Err(error) => {
                    tracing::debug!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "MCP stdio graceful process wait failed"
                    );
                    false
                }
            };
            if !exited {
                if let Err(error) = stdio.start_kill().await {
                    tracing::debug!(
                        error = %haven_common::error::sanitize_error_text(&error.to_string()),
                        "MCP stdio process termination request failed"
                    );
                }
                match stdio.bounded_kill_wait().await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!(
                            server = %self.name,
                            "MCP stdio process did not exit within the bounded termination wait"
                        );
                        stop_failure = Some(
                            "MCP stdio process did not stop within the bounded termination wait"
                                .to_owned(),
                        );
                    }
                    Err(error) => {
                        let safe_error =
                            haven_common::error::sanitize_error_text(&error.to_string());
                        tracing::warn!(
                            server = %self.name,
                            error = %safe_error,
                            "MCP stdio process wait failed after termination"
                        );
                        stop_failure = Some(format!(
                            "MCP stdio process stop could not be confirmed: {safe_error}"
                        ));
                    }
                }
            }
        }
        if let Some(error) = stop_failure {
            *self.status.lock().await = McpClientStatus::Offline {
                error: error.clone(),
            };
            return Err(anyhow::anyhow!(error));
        }
        *guard = None;
        *self.server_capabilities.lock().await = McpServerCapabilities::default();
        *self.resource_list_cache.lock().await = None;
        *self.resource_template_list_cache.lock().await = None;
        self.resource_read_cache.lock().await.clear();
        *self.prompt_list_cache.lock().await = None;
        *self.status.lock().await = McpClientStatus::Disconnected;
        Ok(())
    }

    async fn list_tool_snapshot(&self) -> anyhow::Result<McpToolListSnapshot> {
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
                let result = inner.request(id, "tools/list", params, &[]).await;
                self.discard_interrupted_stdio(&mut guard).await;
                result?
            };
            let page: McpToolListPage = serde_json::from_value(result)
                .map_err(|e| anyhow::anyhow!("invalid tools/list from '{}': {}", self.name, e))?;
            let page_ttl = page.ttl_ms.unwrap_or(0);
            ttl_ms = Some(ttl_ms.map_or(page_ttl, |current| current.min(page_ttl)));
            if page.cache_scope.as_deref() != Some("public") {
                cache_scope = McpToolCacheScope::Private;
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
        if matches!(self.transport, McpServerTransport::Http) {
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
        on_resource_changed: impl Fn(&str, Option<&str>) + Send + Sync + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let notification_cancel = self.notification_cancel.clone();
        tokio::spawn(async move {
            let on_resource_changed = Arc::new(on_resource_changed);
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
                        } else if method == Some("notifications/subscriptions/acknowledged") {
                            self.acknowledge_resource_subscription(&msg).await;
                        } else if method == Some("notifications/resources/updated")
                            && let Some(uri) = self.accept_resource_update(&msg).await
                        {
                            on_resource_changed(&self.name, Some(&uri));
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
        let parameter_headers = if matches!(self.transport, McpServerTransport::Http) {
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
                    Some(McpClientInner::Stdio(s)) => s.is_alive().await,
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

fn mcp_target_config_fingerprint(config: &haven_common::config::McpServerConfig) -> String {
    let environment_names = config
        .env
        .iter()
        .map(|entry| {
            entry
                .split_once('=')
                .map_or(entry.as_str(), |(name, _)| name)
        })
        .collect::<Vec<_>>();
    let identity = serde_json::json!({
        "name": config.name,
        "transport": config.transport.as_str(),
        "command": config.command,
        "args": config.args,
        "environment_names": environment_names,
        "environment_refs": config.env_refs,
        "cwd": config.cwd,
        "url": config.url,
        "enabled": config.enabled,
    });
    Sha256::digest(haven_common::json::canonical_json_bytes(&identity))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod authorization_target_tests {
    use super::*;

    #[test]
    fn target_fingerprint_is_stable_for_same_config_and_changes_with_account_material() {
        let config = haven_common::config::McpServerConfig {
            name: "calendar".into(),
            transport: McpServerTransport::Http,
            url: "https://mcp.example.test/tenant-a".into(),
            env: vec!["API_TOKEN=secret-a".into()],
            env_refs: vec![haven_common::config::McpEnvironmentCredentialRef {
                name: "API_TOKEN".into(),
                credential_ref: Some("cred-0123456789abcdef0123456789abcdef".into()),
                has_value: true,
            }],
            ..Default::default()
        };
        let first = McpClient::new(&config, 1024, 4096).authorization_target_fingerprint();
        let same = McpClient::new(&config, 1024, 4096).authorization_target_fingerprint();
        let changed = McpClient::new(
            &haven_common::config::McpServerConfig {
                url: "https://mcp.example.test/tenant-b".into(),
                ..config.clone()
            },
            1024,
            4096,
        )
        .authorization_target_fingerprint();
        let changed_account = McpClient::new(
            &haven_common::config::McpServerConfig {
                env: vec!["API_TOKEN=secret-b".into()],
                env_refs: vec![haven_common::config::McpEnvironmentCredentialRef {
                    name: "API_TOKEN".into(),
                    credential_ref: Some("cred-1123456789abcdef0123456789abcdef".into()),
                    has_value: true,
                }],
                ..config
            },
            1024,
            4096,
        )
        .authorization_target_fingerprint();

        assert_eq!(first, same);
        assert_ne!(first, changed);
        assert_ne!(first, changed_account);
        assert_eq!(first.len(), 64);
        assert!(!first.contains("secret-a"));
        assert!(!changed_account.contains("secret-b"));
    }
}

#[cfg(test)]
mod stdio_environment_tests {
    use super::*;

    const CHILD_PROBE_MARKER: &str = "HAVEN_MCP_ENVIRONMENT_PROBE";
    const AMBIENT_SECRET: &str = "HAVEN_MCP_TEST_AMBIENT_SECRET";

    #[test]
    fn stdio_server_receives_only_runtime_baseline_and_profile_values() {
        let mut command =
            std::process::Command::new(std::env::current_exe().expect("current test executable"));
        command.args([
            "--exact",
            "client::stdio_environment_tests::child_environment_probe",
            "--nocapture",
        ]);
        let inherited = std::env::vars_os().chain([
            (AMBIENT_SECRET.into(), "must-not-leak".into()),
            ("PYTHONPATH".into(), "untrusted-import-root".into()),
        ]);
        configure_stdio_server_environment(
            &mut command,
            inherited,
            &[
                "MCP_AUTOLAUNCH=1".into(),
                "MCP_PROFILE_VALUE=explicit".into(),
            ],
        );
        command.env(CHILD_PROBE_MARKER, "1");

        let child = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            command.spawn().expect("spawn isolated test child")
        };
        let output = child
            .wait_with_output()
            .expect("collect isolated test child");
        assert!(
            output.status.success(),
            "child environment probe failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn child_environment_probe() {
        if std::env::var_os(CHILD_PROBE_MARKER).is_none() {
            return;
        }
        assert!(std::env::var_os(AMBIENT_SECRET).is_none());
        assert!(std::env::var_os("PYTHONPATH").is_none());
        assert_eq!(std::env::var("MCP_AUTOLAUNCH").unwrap(), "1");
        assert_eq!(std::env::var("MCP_PROFILE_VALUE").unwrap(), "explicit");
        assert!(std::env::var_os("PATH").is_some());
    }
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
        let stdio = client
            .spawn_stdio(notification_tx)
            .await
            .expect("spawn live test server");
        assert!(stdio.is_alive().await, "the test child must still be alive");
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
    use crate::features::{McpElicitationAction, McpElicitationAnswer, McpFeatureCallStatus};
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

    async fn modern_http_client(
        address: std::net::SocketAddr,
        resources: bool,
        prompts: bool,
    ) -> McpClient {
        let config = haven_common::config::McpServerConfig {
            name: "feature-test".into(),
            transport: McpServerTransport::Http,
            url: format!("http://{address}/mcp"),
            ..Default::default()
        };
        let client = McpClient::new(&config, 1024, 1024);
        *client.server_capabilities.lock().await = McpServerCapabilities {
            tools: false,
            resources,
            resource_subscribe: false,
            prompts,
        };
        *client.status.lock().await = McpClientStatus::Connected;
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let cancel = client.cancel_token.lock().await.clone();
        let shared = Arc::new(HttpShared {
            http: reqwest::Client::new(),
            url: config.url,
            headers: Vec::new(),
            cancel,
        });
        *client.inner.lock().await = Some(McpClientInner::Http(HttpInner {
            shared,
            notification_tx,
            max_sse_buffer: 1024,
            tools_list_changed: client.tools_list_changed.clone(),
        }));
        client
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
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let cancel = client.cancel_token.lock().await.clone();
        let shared = Arc::new(HttpShared {
            http: reqwest::Client::new(),
            url: config.url,
            headers: Vec::new(),
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

    #[tokio::test]
    async fn connected_client_retries_tools_list_after_notification_refresh_failure() {
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
                    "tools": [{"name": "malformed"}],
                    "ttlMs": 60_000,
                    "cacheScope": "private"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "tools/list");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "tools": [{
                        "name": "recovered",
                        "description": "Recovered after a failed notification refresh",
                        "inputSchema": {"type": "object"}
                    }],
                    "ttlMs": 60_000,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let config = haven_common::config::McpServerConfig {
            name: "catalog-refresh-retry".into(),
            transport: McpServerTransport::Http,
            url: format!("http://{address}/mcp"),
            ..Default::default()
        };
        let client = McpClient::new(&config, 1024, 1024);
        *client.status.lock().await = McpClientStatus::Connected;
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
            cancel,
        });
        *client.inner.lock().await = Some(McpClientInner::Http(HttpInner {
            shared,
            notification_tx,
            max_sse_buffer: 1024,
            tools_list_changed: client.tools_list_changed.clone(),
        }));

        client.refresh_tool_list_after_notification(|_| {}).await;
        assert!(
            client.tools_cache.lock().await.is_none(),
            "a failed notification refresh clears the stale cache"
        );

        let (tools, refreshed) = client
            .ensure_tool_cache_fresh()
            .await
            .expect("a connected client should retry the missing tools/list cache");
        assert!(refreshed);
        assert_eq!(
            tools
                .expect("successful retry should publish a cache")
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["recovered"]
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resource_and_user_selected_prompt_calls_are_bounded_and_cached() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/list");
            assert_eq!(
                request["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                MODERN_PROTOCOL_VERSION
            );
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "resources": [{"uri": "mcp://demo/one", "name": "one"}],
                    "nextCursor": "second-page",
                    "ttlMs": 60_000,
                    "cacheScope": "public"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/list");
            assert_eq!(request["params"]["cursor"], "second-page");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "resources": [{
                        "uri": "mcp://demo/two",
                        "name": "two",
                        "mimeType": "text/plain"
                    }],
                    "ttlMs": 20_000,
                    "cacheScope": "private"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/read");
            assert_eq!(request["params"]["uri"], "file:///opaque/resource?key=1");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "contents": [
                        {"uri": "file:///opaque/resource?key=1", "mimeType": "text/plain", "text": "untrusted resource text"},
                        {"uri": "mcp://demo/image", "mimeType": "image/png", "blob": "aGVsbG8="}
                    ],
                    "ttlMs": 30_000,
                    "cacheScope": "private"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "prompts/list");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "prompts": [{
                        "name": "review",
                        "title": "Review source",
                        "arguments": [{"name": "source", "required": true}]
                    }],
                    "ttlMs": 45_000,
                    "cacheScope": "public"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "prompts/get");
            assert_eq!(request["params"]["name"], "review");
            assert_eq!(request["params"]["arguments"]["source"], "fn main() {}");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "description": "Rendered by an untrusted MCP server",
                    "messages": [{
                        "role": "user",
                        "content": {"type": "text", "text": "Review this source."}
                    }],
                    "ttlMs": 15_000,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let client = modern_http_client(address, true, true).await;
        let resources = client
            .list_resources(false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resources.resources.len(), 2);
        assert_eq!(resources.ttl_ms, Some(20_000));
        assert_eq!(
            resources.cache_scope,
            crate::features::McpCacheScope::Private
        );
        let cached_resources = client
            .list_resources(false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(cached_resources.resources.len(), 2);

        let uri = "file:///opaque/resource?key=1";
        let resource = client
            .read_resource_with_mrtr(uri, false, CancellationToken::new())
            .await
            .unwrap()
            .resource_read
            .unwrap();
        assert_eq!(resource.contents.len(), 2);
        assert_eq!(
            resource.contents[0].text.as_deref(),
            Some("untrusted resource text")
        );
        assert_eq!(resource.contents[1].blob.as_deref(), Some("aGVsbG8="));
        assert_eq!(
            client
                .read_resource_with_mrtr(uri, false, CancellationToken::new())
                .await
                .unwrap()
                .resource_read
                .unwrap()
                .contents,
            resource.contents
        );

        let prompts = client
            .list_prompts(false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(prompts.prompts[0].name, "review");
        let rendered = client
            .get_prompt_with_mrtr(
                "review",
                HashMap::from([("source".into(), "fn main() {}".into())]),
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .prompt
            .unwrap();
        assert_eq!(rendered.messages.len(), 1);
        assert_eq!(rendered.ttl_ms, Some(15_000));
        assert_eq!(
            rendered.cache_scope,
            crate::features::McpCacheScope::Private
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resource_template_lists_follow_pagination_hints_and_connection_cache() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/templates/list");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "resourceTemplates": [{
                        "uriTemplate": "mcp://demo/items/{item_id}",
                        "name": "Item"
                    }],
                    "nextCursor": "next-page",
                    "ttlMs": 30_000,
                    "cacheScope": "public"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/templates/list");
            assert_eq!(request["params"]["cursor"], "next-page");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "resourceTemplates": [{
                        "uriTemplate": "mcp://demo/groups{/group_id}/items{?limit}",
                        "name": "Group items",
                        "mimeType": "application/json"
                    }],
                    "ttlMs": 15_000,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let client = modern_http_client(address, true, false).await;
        let templates = client
            .list_resource_templates(false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(templates.resource_templates.len(), 2);
        assert_eq!(templates.ttl_ms, Some(15_000));
        assert_eq!(
            templates.cache_scope,
            crate::features::McpCacheScope::Private
        );
        assert_eq!(
            client
                .list_resource_templates(false, CancellationToken::new())
                .await
                .unwrap()
                .resource_templates
                .len(),
            2
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resource_read_exposes_bounded_form_input_to_its_ui_owner() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/read");
            assert_eq!(
                request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"]
                    ["form"],
                serde_json::json!({})
            );
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "input_required",
                    "requestState": "sensitive opaque state",
                    "inputRequests": {"user": {
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "message": "Provide a name",
                            "requestedSchema": {
                                "type": "object",
                                "properties": {"name": {"type": "string"}},
                                "required": ["name"]
                            }
                        }
                    }}
                }),
            )
            .await;
        });

        let client = modern_http_client(address, true, false).await;
        let outcome = client
            .read_resource_with_mrtr("mcp://demo/private", false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(outcome.status, McpFeatureCallStatus::InputRequired);
        assert_eq!(outcome.round, Some(1));
        assert_eq!(outcome.requests[0].message, "Provide a name");
        let request_id = outcome.request_id.unwrap();
        assert!(client.cancel_feature_input(request_id).await);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resource_read_resumes_state_only_round_without_answers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, first) = read_http_jsonrpc_request(stream).await;
            assert_eq!(first["method"], "resources/read");
            let first_id = first["id"].clone();
            respond_jsonrpc(
                writer,
                first_id.clone(),
                serde_json::json!({
                    "resultType": "input_required",
                    "requestState": "opaque processing checkpoint"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, resumed) = read_http_jsonrpc_request(stream).await;
            assert_eq!(resumed["method"], "resources/read");
            assert_ne!(resumed["id"], first_id);
            assert_eq!(resumed["params"]["uri"], "mcp://demo/private");
            assert_eq!(
                resumed["params"]["requestState"],
                "opaque processing checkpoint"
            );
            assert!(resumed["params"].get("inputResponses").is_none());
            respond_jsonrpc(
                writer,
                resumed["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "contents": [{"uri": "mcp://demo/private", "text": "finished"}]
                }),
            )
            .await;
        });

        let client = modern_http_client(address, true, false).await;
        let first = client
            .read_resource_with_mrtr("mcp://demo/private", false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(first.status, McpFeatureCallStatus::InputRequired);
        assert!(first.requests.is_empty());
        let request_id = first.request_id.unwrap();
        assert!(first.round.is_some());

        let complete = client
            .respond_to_feature_input(request_id, HashMap::new())
            .await
            .unwrap();
        assert_eq!(complete.status, McpFeatureCallStatus::Complete);
        assert_eq!(
            complete.resource_read.unwrap().contents[0].text.as_deref(),
            Some("finished")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn resource_mrtr_echoes_opaque_state_and_uses_fresh_ids_per_round() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, first) = read_http_jsonrpc_request(stream).await;
            assert_eq!(first["method"], "resources/read");
            let first_id = first["id"].clone();
            respond_jsonrpc(
                writer,
                first_id.clone(),
                serde_json::json!({
                    "resultType": "input_required",
                    "requestState": "opaque checkpoint one",
                    "inputRequests": {"topic-request": {
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "message": "Choose a topic",
                            "requestedSchema": {
                                "type": "object",
                                "properties": {"topic": {"type": "string", "enum": ["rust", "mcp"]}},
                                "required": ["topic"]
                            }
                        }
                    }}
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, second) = read_http_jsonrpc_request(stream).await;
            assert_ne!(second["id"], first_id);
            assert_eq!(second["params"]["requestState"], "opaque checkpoint one");
            assert_eq!(
                second["params"]["inputResponses"]["topic-request"]["action"],
                "accept"
            );
            assert_eq!(
                second["params"]["inputResponses"]["topic-request"]["content"]["topic"],
                "rust"
            );
            assert_eq!(
                second["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"]
                    ["form"],
                serde_json::json!({})
            );
            let second_id = second["id"].clone();
            respond_jsonrpc(
                writer,
                second_id.clone(),
                serde_json::json!({
                    "resultType": "input_required",
                    "requestState": "opaque checkpoint two",
                    "inputRequests": {"confirm-request": {
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "message": "Confirm",
                            "requestedSchema": {
                                "type": "object",
                                "properties": {"confirmed": {"type": "boolean"}},
                                "required": ["confirmed"]
                            }
                        }
                    }}
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, third) = read_http_jsonrpc_request(stream).await;
            assert_ne!(third["id"], second_id);
            assert_eq!(third["params"]["requestState"], "opaque checkpoint two");
            assert!(
                third["params"]["inputResponses"]
                    .get("topic-request")
                    .is_none()
            );
            assert_eq!(
                third["params"]["inputResponses"]["confirm-request"]["content"]["confirmed"],
                true
            );
            respond_jsonrpc(
                writer,
                third["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "contents": [{"uri": "mcp://demo/private", "text": "finished"}],
                    "ttlMs": 0,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let client = modern_http_client(address, true, false).await;
        let first = client
            .read_resource_with_mrtr("mcp://demo/private", false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(first.status, McpFeatureCallStatus::InputRequired);
        let request_id = first.request_id.unwrap();
        let second = client
            .respond_to_feature_input(
                request_id,
                HashMap::from([(
                    "topic-request".into(),
                    McpElicitationAnswer {
                        action: McpElicitationAction::Accept,
                        content: HashMap::from([("topic".into(), Value::String("rust".into()))]),
                    },
                )]),
            )
            .await
            .unwrap();
        assert_eq!(second.status, McpFeatureCallStatus::InputRequired);
        assert_eq!(second.round, Some(2));
        assert_eq!(second.request_id, Some(request_id));
        let complete = client
            .respond_to_feature_input(
                request_id,
                HashMap::from([(
                    "confirm-request".into(),
                    McpElicitationAnswer {
                        action: McpElicitationAction::Accept,
                        content: HashMap::from([("confirmed".into(), Value::Bool(true))]),
                    },
                )]),
            )
            .await
            .unwrap();
        assert_eq!(complete.status, McpFeatureCallStatus::Complete);
        assert_eq!(
            complete.resource_read.unwrap().contents[0].text.as_deref(),
            Some("finished")
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn selected_prompt_mrtr_returns_the_completed_prompt_after_user_input() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, prompt_list) = read_http_jsonrpc_request(stream).await;
            assert_eq!(prompt_list["method"], "prompts/list");
            respond_jsonrpc(
                writer,
                prompt_list["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "prompts": [{"name": "review", "arguments": [{"name": "topic", "required": true}]}]
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, first) = read_http_jsonrpc_request(stream).await;
            assert_eq!(first["method"], "prompts/get");
            assert_eq!(first["params"]["name"], "review");
            assert_eq!(first["params"]["arguments"]["topic"], "MCP");
            assert_eq!(
                first["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]["elicitation"]
                    ["form"],
                serde_json::json!({})
            );
            let first_id = first["id"].clone();
            respond_jsonrpc(
                writer,
                first_id.clone(),
                serde_json::json!({
                    "resultType": "input_required",
                    "requestState": "prompt state",
                    "inputRequests": {"audience": {
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "message": "Who is this for?",
                            "requestedSchema": {
                                "type": "object",
                                "properties": {"reader": {"type": "string"}},
                                "required": ["reader"]
                            }
                        }
                    }}
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, second) = read_http_jsonrpc_request(stream).await;
            assert_ne!(second["id"], first_id);
            assert_eq!(second["params"]["requestState"], "prompt state");
            assert_eq!(
                second["params"]["inputResponses"]["audience"]["content"]["reader"],
                "a developer"
            );
            respond_jsonrpc(
                writer,
                second["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "description": "A prompt",
                    "messages": [{"role": "user", "content": {"type": "text", "text": "Review this."}}]
                }),
            )
            .await;
        });

        let client = modern_http_client(address, false, true).await;
        let first = client
            .get_prompt_with_mrtr(
                "review",
                HashMap::from([("topic".into(), "MCP".into())]),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let complete = client
            .respond_to_feature_input(
                first.request_id.unwrap(),
                HashMap::from([(
                    "audience".into(),
                    McpElicitationAnswer {
                        action: McpElicitationAction::Accept,
                        content: HashMap::from([(
                            "reader".into(),
                            Value::String("a developer".into()),
                        )]),
                    },
                )]),
            )
            .await
            .unwrap();
        assert_eq!(complete.status, McpFeatureCallStatus::Complete);
        assert_eq!(complete.prompt.unwrap().messages.len(), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_resources_and_prompts_capabilities_make_no_rpc() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let client = modern_http_client(address, false, false).await;
        assert!(
            client
                .list_resources(false, CancellationToken::new())
                .await
                .unwrap_err()
                .to_string()
                .contains("does not advertise resources")
        );
        assert!(
            client
                .list_prompts(false, CancellationToken::new())
                .await
                .unwrap_err()
                .to_string()
                .contains("does not advertise prompts")
        );
        drop(listener);
    }

    #[tokio::test]
    async fn explicit_refresh_and_expired_feature_caches_use_the_live_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/list");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "resources": [{"uri": "mcp://demo/fresh", "name": "fresh"}],
                    "ttlMs": 60_000,
                    "cacheScope": "public"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "resources/read");
            assert_eq!(request["params"]["uri"], "mcp://demo/expired");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "contents": [{"uri": "mcp://demo/expired", "text": "fresh content"}],
                    "ttlMs": 10_000,
                    "cacheScope": "private"
                }),
            )
            .await;

            let (stream, _) = listener.accept().await.unwrap();
            let (writer, request) = read_http_jsonrpc_request(stream).await;
            assert_eq!(request["method"], "prompts/list");
            respond_jsonrpc(
                writer,
                request["id"].clone(),
                serde_json::json!({
                    "resultType": "complete",
                    "prompts": [{"name": "fresh-prompt", "arguments": []}],
                    "ttlMs": 10_000,
                    "cacheScope": "private"
                }),
            )
            .await;
        });

        let client = modern_http_client(address, true, true).await;
        *client.resource_list_cache.lock().await = Some(McpResourceListSnapshot {
            result: McpResourceList {
                resources: vec![crate::features::McpResourceInfo {
                    uri: "mcp://demo/stale".into(),
                    name: "stale".into(),
                    title: None,
                    description: None,
                    mime_type: None,
                    size: None,
                }],
                ttl_ms: Some(60_000),
                cache_scope: crate::features::McpCacheScope::Public,
            },
            expires_at: Some(Instant::now() + Duration::from_secs(60)),
        });
        let expired_at = Instant::now() - Duration::from_millis(1);
        client.resource_read_cache.lock().await.insert(
            "mcp://demo/expired".into(),
            McpResourceReadCacheEntry {
                result: McpResourceRead {
                    contents: vec![crate::features::McpResourceContent {
                        uri: "mcp://demo/expired".into(),
                        mime_type: Some("text/plain".into()),
                        text: Some("stale content".into()),
                        blob: None,
                    }],
                    ttl_ms: Some(60_000),
                    cache_scope: crate::features::McpCacheScope::Private,
                },
                expires_at: Some(expired_at),
            },
        );
        *client.prompt_list_cache.lock().await = Some(McpPromptListSnapshot {
            result: McpPromptList {
                prompts: vec![],
                ttl_ms: Some(60_000),
                cache_scope: crate::features::McpCacheScope::Private,
            },
            expires_at: Some(expired_at),
        });

        let resources = client
            .list_resources(true, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(resources.resources[0].name, "fresh");
        let resource = client
            .read_resource_with_mrtr("mcp://demo/expired", false, CancellationToken::new())
            .await
            .unwrap()
            .resource_read
            .unwrap();
        assert_eq!(resource.contents[0].text.as_deref(), Some("fresh content"));
        let prompts = client
            .list_prompts(false, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(prompts.prompts[0].name, "fresh-prompt");

        client.shutdown_inner(false).await.unwrap();
        assert!(client.resource_list_cache.lock().await.is_none());
        assert!(client.resource_read_cache.lock().await.is_empty());
        assert!(client.prompt_list_cache.lock().await.is_none());
        server.await.unwrap();
    }
}
