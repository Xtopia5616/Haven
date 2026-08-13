use crate::ToolResult;
use haven_common::McpTransportType;
use haven_llm::stt::{McpToolCaller, McpToolOutcome};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod sse;
use futures_util::StreamExt;
use sse::SseParser;

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
// MCP content block extraction
// ---------------------------------------------------------------------------

/// Approximate decoded length of a base64 payload, computed without allocating
/// the buffer (base64 adds ~1/3 overhead and up to 2 padding `=` bytes for a
/// block-aligned input). Only used for reporting sizes / caps.
fn base64_decoded_len(s: &str) -> usize {
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut out = len / 4 * 3;
    if len > 0 && bytes[len - 1] == b'=' {
        out -= 1;
    }
    if len > 1 && bytes[len - 2] == b'=' {
        out -= 1;
    }
    out
}

/// Normalize the response of a `tools/call` into a structured `output` object
/// and a plain-text `summary` for the agent loop.
///
/// MCP content blocks may be `text`, `image`, `audio`, or `resource`. Text and
/// text-resources fold into the plain-text summary. Binary media (`image`,
/// `audio`, embedded resource blobs) is surfaced once under `output.images` /
/// `output.audio` / `output.resources` (base64 + mimeType, capped at
/// `max_binary_payload` base64 chars) for downstream rendering, while
/// `output.content` keeps a metadata-only list of every block (no raw payload)
/// for UI fidelity. Unknown or malformed block types are preserved rather than
/// silently dropped.
fn extract_mcp_content(content: &[Value], max_binary_payload: usize) -> (Value, String) {
    let mut text_parts: Vec<String> = Vec::new();
    let mut images: Vec<Value> = Vec::new();
    let mut audio: Vec<Value> = Vec::new();
    let mut resources: Vec<Value> = Vec::new();
    let mut normalized: Vec<Value> = Vec::new();

    for item in content {
        let kind = match item.get("type").and_then(|t| t.as_str()) {
            Some(k) => k,
            // A type-less block is malformed: preserve it like unknown types.
            None => {
                text_parts.push(serde_json::to_string(item).unwrap_or_default());
                normalized.push(item.clone());
                continue;
            }
        };
        let mime_type = item["mimeType"]
            .as_str()
            .or_else(|| item["mime_type"].as_str())
            .unwrap_or("application/octet-stream");

        // Metadata-only alias of this block; the raw payload (if any) lives in
        // the typed collection below, not here, so it is never duplicated.
        let mut block = serde_json::Map::new();
        block.insert("type".into(), Value::String(kind.to_string()));
        block.insert("mimeType".into(), Value::String(mime_type.to_string()));

        match kind {
            "text" => {
                if let Some(t) = item["text"].as_str() {
                    text_parts.push(t.to_string());
                }
                block.insert(
                    "text".into(),
                    item.get("text")
                        .cloned()
                        .unwrap_or(Value::String(String::new())),
                );
            }
            "image" | "audio" => {
                let data = item["data"].as_str().unwrap_or("");
                let entry = if data.len() <= max_binary_payload {
                    serde_json::json!({
                        "type": kind,
                        "mimeType": mime_type,
                        "data": data,
                    })
                } else {
                    // Oversized payload: keep only metadata so the observation
                    // and DB record stay bounded.
                    serde_json::json!({
                        "type": kind,
                        "mimeType": mime_type,
                        "data": "",
                        "oversized": true,
                        "bytes": data.len(),
                    })
                };
                if kind == "image" {
                    images.push(entry);
                } else {
                    audio.push(entry);
                }
                block.insert("data_len".into(), Value::from(data.len()));
                // Marker so the text-only agent loop knows binary content exists.
                text_parts.push(format!(
                    "[{} block returned: {} ({} base64 chars{})]",
                    kind,
                    mime_type,
                    data.len(),
                    if data.len() > max_binary_payload {
                        ", oversized"
                    } else {
                        ""
                    }
                ));
            }
            "resource" => {
                let res = &item["resource"];
                if let Some(t) = res["text"].as_str() {
                    text_parts.push(t.to_string());
                    block.insert("text".into(), Value::String(t.to_string()));
                } else if let Some(blob) = res["blob"].as_str() {
                    let uri = res["uri"].as_str().unwrap_or("");
                    // Compute the decoded size without allocating the buffer.
                    let decoded_len = base64_decoded_len(blob);
                    let entry = if blob.len() <= max_binary_payload {
                        serde_json::json!({
                            "uri": uri,
                            "mimeType": res["mimeType"].as_str().unwrap_or(mime_type),
                            "blob": blob,
                            "bytes": decoded_len,
                        })
                    } else {
                        serde_json::json!({
                            "uri": uri,
                            "mimeType": res["mimeType"].as_str().unwrap_or(mime_type),
                            "blob": "",
                            "oversized": true,
                            "bytes": decoded_len,
                        })
                    };
                    resources.push(entry);
                    block.insert("bytes".into(), Value::from(decoded_len));
                    text_parts.push(format!(
                        "[resource block returned: {} ({} base64 chars, ~{} decoded bytes{})]",
                        if uri.is_empty() { mime_type } else { uri },
                        blob.len(),
                        decoded_len,
                        if blob.len() > max_binary_payload {
                            ", oversized"
                        } else {
                            ""
                        }
                    ));
                } else {
                    // Neither a readable text nor a blob: surface a marker so
                    // the block is not silently dropped (mirrors image/audio).
                    text_parts.push(format!(
                        "[resource block returned: {} (no readable payload)]",
                        res["uri"].as_str().unwrap_or(mime_type)
                    ));
                }
            }
            _ => {
                // Unknown block type — preserve it but don't fail.
                text_parts.push(serde_json::to_string(item).unwrap_or_default());
                normalized.push(item.clone());
                continue;
            }
        }

        normalized.push(Value::Object(block));
    }

    let text = text_parts.join("\n");
    let mut output = serde_json::Map::new();
    output.insert("text".into(), Value::String(text.clone()));
    output.insert("content".into(), Value::Array(normalized));
    if !images.is_empty() {
        output.insert("images".into(), Value::Array(images));
    }
    if !audio.is_empty() {
        output.insert("audio".into(), Value::Array(audio));
    }
    if !resources.is_empty() {
        output.insert("resources".into(), Value::Array(resources));
    }

    (Value::Object(output), text)
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
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<String>,
    pub cwd: Option<String>,
    pub url: String,
    pub enabled: bool,
    pub status: McpClientStatus,
    pub tools: Vec<McpToolInfo>,
    pub last_error: Option<String>,
    /// Handshake/tool-discovery diagnostics (protocol version mismatch,
    /// connected-but-zero-tools, failed list_tools). Lets the UI and the
    /// agent distinguish "the server has no tools" from "the client and the
    /// server are incompatible".
    pub diagnostic: Option<String>,
    pub last_seen_at: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct McpStatusChangeEvent {
    pub name: String,
    pub status: McpClientStatus,
}

// ---------------------------------------------------------------------------
// McpClient — single MCP server connection (stdio or Streamable HTTP)
// ---------------------------------------------------------------------------

/// stdio transport: a spawned child process speaking JSON-RPC over its stdin
/// and stdout pipes.
struct StdioInner {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
}

impl StdioInner {
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
                    // Shared error extraction with the HTTP transport so the
                    // two paths cannot drift.
                    return unpack_jsonrpc(parsed);
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

/// Shared state for the Streamable HTTP transport: the reqwest client, the
/// endpoint URL, request headers (from the config `env` list), and the session
/// id returned by the server (`Mcp-Session-Id`, per the MCP Streamable HTTP
/// spec).
struct HttpShared {
    http: reqwest::Client,
    url: String,
    headers: Vec<(String, String)>,
    session_id: Arc<tokio::sync::Mutex<String>>,
    cancel: CancellationToken,
}

/// Streamable HTTP transport (MCP spec, transport 2025-03-26). Requests are
/// POSTed to the endpoint; the server replies with JSON or an SSE stream.
/// Server-to-client notifications arrive over a long-lived SSE stream opened
/// with a GET request.
struct HttpInner {
    shared: Arc<HttpShared>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
    /// Single buffered (incomplete) SSE line/event cap (from context limits).
    max_sse_buffer: usize,
}

impl HttpInner {
    async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        let req = jsonrpc_request(id, method, params);
        let builder = self
            .shared
            .http
            .post(&self.shared.url)
            .json(&req)
            .header("Accept", "application/json, text/event-stream");
        let builder = apply_http_session_headers(builder, &self.shared).await;

        let resp = tokio::select! {
            _ = self.shared.cancel.cancelled() => {
                anyhow::bail!("MCP HTTP request '{}' cancelled", method)
            }
            r = tokio::time::timeout(
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
                builder.send(),
            ) => r.map_err(|_| anyhow::anyhow!("MCP HTTP request '{}' timed out", method))??,
        };
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("MCP HTTP error (status {}): {}", status.as_u16(), body);
        }
        // Capture/refresh the session id so subsequent requests (and the SSE
        // listener) can attach it.
        if let Some(sid) = resp
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.shared.session_id.lock().await = sid.to_string();
        }

        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        if is_sse {
            read_sse_response(resp, id, &self.notification_tx, self.max_sse_buffer).await
        } else {
            let value: Value = resp.json().await?;
            unpack_jsonrpc(value)
        }
    }

    async fn notify(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        let notification = jsonrpc_notification(method, params);
        let builder = self
            .shared
            .http
            .post(&self.shared.url)
            .json(&notification)
            .header("Accept", "application/json");
        let builder = apply_http_session_headers(builder, &self.shared).await;
        let resp = tokio::select! {
            _ = self.shared.cancel.cancelled() => {
                anyhow::bail!("MCP HTTP notify '{}' cancelled", method)
            }
            r = tokio::time::timeout(
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
                builder.send(),
            ) => r.map_err(|_| anyhow::anyhow!("MCP HTTP notify '{}' timed out", method))??,
        };
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!(
                "MCP HTTP notify error (status {}): {}",
                status.as_u16(),
                resp.text().await.unwrap_or_default()
            );
        }
        // Drain the body to release the connection for reuse.
        let _ = resp.bytes().await;
        Ok(())
    }
}

enum McpClientInner {
    Stdio(Box<StdioInner>),
    Http(HttpInner),
}

impl McpClientInner {
    async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        match self {
            McpClientInner::Stdio(s) => s.request(id, method, params).await,
            McpClientInner::Http(h) => h.request(id, method, params).await,
        }
    }

    async fn notify(&mut self, method: &str, params: Option<Value>) -> anyhow::Result<()> {
        match self {
            McpClientInner::Stdio(s) => s.notify(method, params).await,
            McpClientInner::Http(h) => h.notify(method, params).await,
        }
    }
}

impl Drop for McpClientInner {
    fn drop(&mut self) {
        if let McpClientInner::Stdio(s) = self {
            let _ = s.child.start_kill();
        }
    }
}

/// Read an SSE streamed response body until the JSON-RPC response matching
/// `id` arrives; id-less messages in the stream are routed as notifications.
fn unpack_jsonrpc(value: Value) -> anyhow::Result<Value> {
    if let Some(err) = value.get("error") {
        let code = err["code"].as_i64().unwrap_or(-1);
        let msg = err["message"].as_str().unwrap_or("unknown error");
        anyhow::bail!("MCP error ({}): {}", code, msg);
    }
    Ok(value["result"].clone())
}

/// Attach the current MCP session id (if any) and every user-configured
/// header to a request builder. Shared by all HTTP transport call sites so
/// the session/auth header handling cannot drift.
async fn apply_http_session_headers(
    mut builder: reqwest::RequestBuilder,
    shared: &HttpShared,
) -> reqwest::RequestBuilder {
    let session = shared.session_id.lock().await;
    if !session.is_empty() {
        builder = builder.header("Mcp-Session-Id", session.as_str());
    }
    for (k, v) in &shared.headers {
        builder = builder.header(k.as_str(), v);
    }
    builder
}

async fn read_sse_response(
    resp: reqwest::Response,
    id: u64,
    tx: &tokio::sync::mpsc::UnboundedSender<Value>,
    max_sse_buffer: usize,
) -> anyhow::Result<Value> {
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new(max_sse_buffer);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(REQUEST_TIMEOUT_SECS);
    loop {
        let chunk = tokio::time::timeout_at(deadline, stream.next())
            .await
            .map_err(|_| anyhow::anyhow!("MCP HTTP: timed out waiting for SSE response"))?;
        match chunk {
            Some(Ok(bytes)) => {
                for ev in parser.feed(&bytes) {
                    if ev.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        return unpack_jsonrpc(ev);
                    }
                    let _ = tx.send(ev);
                }
            }
            Some(Err(e)) => return Err(e.into()),
            None => break,
        }
    }
    anyhow::bail!(
        "MCP HTTP: SSE stream ended before the response for request {} arrived",
        id
    )
}

/// Open a long-lived SSE stream (GET) and route every server-to-client message
/// into the notification channel. Ends when the stream closes or the cancel
/// token fires; the caller retries with backoff.
async fn listen_sse(
    shared: &Arc<HttpShared>,
    tx: &tokio::sync::mpsc::UnboundedSender<Value>,
    cancel: &CancellationToken,
    max_sse_buffer: usize,
) -> anyhow::Result<()> {
    let builder = shared
        .http
        .get(&shared.url)
        .header("Accept", "text/event-stream");
    let req = apply_http_session_headers(builder, shared).await;
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("SSE stream open failed (status {})", status.as_u16());
    }
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new(max_sse_buffer);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(bytes)) => {
                        for ev in parser.feed(&bytes) {
                            let _ = tx.send(ev);
                        }
                    }
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()), // stream closed by the server
                }
            }
        }
    }
}

/// Background session that keeps an SSE notification stream open for an HTTP
/// client, reconnecting with a short backoff when the stream drops.
fn spawn_sse_listener(
    name: String,
    cancel: CancellationToken,
    shared: Arc<HttpShared>,
    notification_tx: tokio::sync::mpsc::UnboundedSender<Value>,
    max_sse_buffer: usize,
) {
    tokio::spawn(async move {
        let shared = Arc::new(shared);
        loop {
            if cancel.is_cancelled() {
                break;
            }
            match listen_sse(&shared, &notification_tx, &cancel, max_sse_buffer).await {
                Ok(()) => {}
                Err(e) => {
                    tracing::debug!(
                        "MCP HTTP SSE stream for '{}' closed: {} (will retry)",
                        name,
                        e
                    );
                }
            }
            if cancel.is_cancelled() {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(Duration::from_secs(2)) => {}
            }
        }
    });
}

/// Liveness probe for the HTTP transport: any HTTP response (even an error
/// status) means the endpoint is reachable; only a transport failure or
/// timeout reports it dead.
async fn http_is_alive(shared: &Arc<HttpShared>) -> bool {
    let builder = shared
        .http
        .get(&shared.url)
        .header("Accept", "text/event-stream");
    let req = apply_http_session_headers(builder, shared).await;
    matches!(
        tokio::time::timeout(Duration::from_secs(5), req.send()).await,
        Ok(Ok(_))
    )
}

pub struct McpClient {
    name: String,
    transport: McpTransportType,
    command: String,
    args: Vec<String>,
    env: Vec<String>,
    /// Working directory the stdio server is spawned from (optional).
    cwd: Option<String>,
    url: String,
    enabled: Arc<AtomicBool>,
    inner: Arc<Mutex<Option<McpClientInner>>>,
    status: Arc<Mutex<McpClientStatus>>,
    next_id: AtomicU64,
    tools_cache: Arc<Mutex<Option<Vec<McpToolInfo>>>>,
    last_error: Arc<Mutex<Option<String>>>,
    /// Handshake/tool-discovery diagnostics; see `McpServerSnapshot`.
    last_diagnostic: Arc<Mutex<Option<String>>>,
    last_seen_at: Arc<Mutex<Option<i64>>>,
    reconnect_retries: Arc<Mutex<u32>>,
    cancel_token: Arc<Mutex<CancellationToken>>,
    rate_limiter: Arc<Mutex<RateLimiter>>,
    notification_rx: Arc<Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<Value>>>>,
    /// Binary content (image/audio/resource blob) kept in observations (base64
    /// chars) before being replaced by an `oversized` marker.
    max_binary_payload: usize,
    /// Single buffered (incomplete) SSE line/event cap for the HTTP transport.
    max_sse_buffer: usize,
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

        if is_error {
            Ok(ToolResult {
                success: false,
                output,
                error: Some(text),
                truncated: false,
                signals: crate::tool::ToolSignals::default(),
            })
        } else {
            Ok(ToolResult {
                success: true,
                output,
                error: None,
                truncated: false,
                signals: crate::tool::ToolSignals::default(),
            })
        }
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

    /// Load MCP servers from config, create clients, and connect them.
    /// Does NOT start health monitors — call `start_monitors` separately
    /// or use `discover_all` for the combined operation.
    pub async fn load_from_config(&self, servers: &[haven_common::McpServerConfig]) {
        let mut pending = tokio::task::JoinSet::new();

        for server in servers {
            if !server.enabled {
                continue;
            }
            let limits = self.limits.read().await.clone();
            let client = Arc::new(McpClient::new(
                server,
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
                        name,
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use haven_common::McpServerConfig;
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
            transport: "http".into(),
            command: "".into(),
            args: vec![],
            env: vec!["AUTHORIZATION=Bearer x".into()],
            cwd: None,
            url: "http://localhost:3001/mcp".into(),
            enabled: true,
            status: McpClientStatus::Connected,
            tools: vec![],
            last_error: None,
            diagnostic: None,
            last_seen_at: Some(12345),
        };
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["name"], "test");
        assert_eq!(json["transport"], "http");
        assert_eq!(json["status"], "Connected");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["url"], "http://localhost:3001/mcp");
        assert_eq!(json["env"][0], "AUTHORIZATION=Bearer x");
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
        let client = McpClient::new(
            &McpServerConfig {
                name: "test".into(),
                command: "echo".into(),
                ..Default::default()
            },
            2 * 1024 * 1024,
            2 * 1024 * 1024,
        );
        assert_eq!(client.name(), "test");
        assert!(client.enabled());
        let status = client.status().await;
        assert!(matches!(status, McpClientStatus::Disconnected));
    }

    #[tokio::test]
    async fn mcp_client_snapshot_initial() {
        let client = McpClient::new(
            &McpServerConfig {
                name: "test".into(),
                command: "echo".into(),
                ..Default::default()
            },
            2 * 1024 * 1024,
            2 * 1024 * 1024,
        );
        let snap = client.snapshot().await;
        assert_eq!(snap.name, "test");
        assert_eq!(snap.transport, "stdio");
        assert!(snap.enabled);
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

    #[test]
    fn extract_mcp_content_plain_text() {
        let content = json!([
            {"type": "text", "text": "hello"},
            {"type": "text", "text": "world"},
        ])
        .as_array()
        .unwrap()
        .clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert_eq!(text, "hello\nworld");
        assert_eq!(output["text"], "hello\nworld");
        assert!(output.get("images").is_none());
        assert!(output.get("audio").is_none());
        assert!(output.get("resources").is_none());
        assert_eq!(output["content"].as_array().unwrap().len(), 2);
        assert_eq!(output["content"][0]["type"], "text");
        assert_eq!(output["content"][0]["text"], "hello");
    }

    #[test]
    fn extract_mcp_content_image_and_audio() {
        let content = json!([
            {"type": "text", "text": "caption"},
            {"type": "image", "mimeType": "image/png", "data": "aGVsbG8="},
            {"type": "audio", "mimeType": "audio/wav", "data": "d29ybGQ="},
        ])
        .as_array()
        .unwrap()
        .clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert!(text.contains("caption"));
        assert!(text.contains("[image block returned: image/png"));
        assert!(text.contains("[audio block returned: audio/wav"));

        let images = output["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["type"], "image");
        assert_eq!(images[0]["mimeType"], "image/png");
        assert_eq!(images[0]["data"], "aGVsbG8=");

        let audio_blocks = output["audio"].as_array().unwrap();
        assert_eq!(audio_blocks.len(), 1);
        assert_eq!(audio_blocks[0]["mimeType"], "audio/wav");
        assert_eq!(audio_blocks[0]["data"], "d29ybGQ=");

        assert_eq!(output["content"].as_array().unwrap().len(), 3);
        assert_eq!(output["content"][1]["type"], "image");
        assert_eq!(output["content"][1]["mimeType"], "image/png");
    }

    #[test]
    fn extract_mcp_content_text_resource() {
        let content = json!([
            {
                "type": "resource",
                "resource": {
                    "uri": "file:///x.txt",
                    "mimeType": "text/plain",
                    "text": "file contents here",
                },
            },
        ])
        .as_array()
        .unwrap()
        .clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert_eq!(text, "file contents here");
        assert_eq!(output["text"], "file contents here");
        assert!(output.get("resources").is_none());
    }

    #[test]
    fn extract_mcp_content_blob_resource() {
        let blob = base64::engine::general_purpose::STANDARD.encode("abcd");
        let content = json!([
            {
                "type": "resource",
                "resource": {
                    "uri": "result.bin",
                    "mimeType": "application/octet-stream",
                    "blob": blob,
                },
            },
        ])
        .as_array()
        .unwrap()
        .clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert!(text.contains("[resource block returned: result.bin"));
        assert!(text.contains("~4 decoded bytes"));
        let resources = output["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0]["uri"], "result.bin");
        assert_eq!(resources[0]["bytes"], 4);
    }

    #[test]
    fn extract_mcp_content_resource_no_readable_payload() {
        let content = json!([
            {
                "type": "resource",
                "resource": {
                    "uri": "memory://note",
                    "mimeType": "application/octet-stream",
                },
            },
        ])
        .as_array()
        .unwrap()
        .clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert!(text.contains("[resource block returned: memory://note"));
        assert!(text.contains("no readable payload"));
        assert!(output.get("resources").is_none());
    }

    #[test]
    fn extract_mcp_content_type_less_block_preserved() {
        let content = json!([
            {"data": "somedata"},
            {"type": "weird", "foo": "bar"},
        ])
        .as_array()
        .unwrap()
        .clone();
        let (_, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        // Both malformed/unknown blocks must be preserved, not swallowed.
        assert!(text.contains("somedata"));
        assert!(text.contains("weird"));
    }

    #[test]
    fn extract_mcp_content_oversized_image_capped() {
        let big = "A".repeat((2 * 1024 * 1024) + 1);
        let content = json!([
            {"type": "image", "mimeType": "image/png", "data": big},
        ])
        .as_array()
        .unwrap()
        .clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert!(text.contains("oversized"));
        let images = output["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0]["oversized"], true);
        assert_eq!(images[0]["data"], "");
        assert_eq!(images[0]["bytes"], (2 * 1024 * 1024) + 1);
    }

    #[test]
    fn extract_mcp_content_empty() {
        let content = json!([]).as_array().unwrap().clone();
        let (output, text) = extract_mcp_content(&content, 2 * 1024 * 1024);
        assert_eq!(text, "");
        assert_eq!(output["text"], "");
    }
}
