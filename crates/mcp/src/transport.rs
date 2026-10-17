use crate::McpStdioProcess;
use crate::protocol::{
    MODERN_PROTOCOL_VERSION, McpHttpStatusError, McpRpcError, REQUEST_TIMEOUT_SECS,
    encode_mcp_header_value, jsonrpc_notification, jsonrpc_request, params_with_protocol_meta,
};
use crate::sse::SseParser;
use futures_util::StreamExt;
use haven_common::bounded_bytes::BoundedBytes;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const MAX_STDIO_LINE_BYTES: usize = 1024 * 1024;
const MAX_JSON_BODY_BYTES: usize = 4 * 1024 * 1024;
const STDIO_CLOSE_INPUT_TIMEOUT: Duration = Duration::from_millis(500);
const STDIO_EXIT_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const STDIO_KILL_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const NOTIFICATION_QUEUE_CAPACITY: usize = 256;

/// Coalesced signal for `tools/list_changed`. The notification itself carries
/// no useful payload, so retaining one pending bit avoids losing an invalidation
/// when the bounded advisory notification queue is full.
pub(crate) struct ToolsListChangedSignal {
    pending: AtomicBool,
    notify: Notify,
}

impl ToolsListChangedSignal {
    pub(crate) fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    pub(crate) fn request(&self) {
        self.pending.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    pub(crate) fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    pub(crate) async fn notified(&self) {
        self.notify.notified().await;
    }
}

/// stdio transport: a spawned child process speaking JSON-RPC over its stdin
/// and stdout pipes.
pub(crate) struct StdioInner {
    process: AsyncMutex<Box<dyn McpStdioProcess>>,
    pub(crate) io: Arc<StdioIo>,
    reader_task: JoinHandle<()>,
    stderr_task: Option<JoinHandle<()>>,
}

pub(crate) struct StdioIo {
    stdin: AsyncMutex<Option<Box<dyn tokio::io::AsyncWrite + Send + Unpin>>>,
    pending: AsyncMutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    active_subscriptions: std::sync::Mutex<HashSet<u64>>,
    reader_failed: AtomicBool,
    notification_tx: tokio::sync::mpsc::Sender<Value>,
    tools_list_changed: Arc<ToolsListChangedSignal>,
    cancel: CancellationToken,
}

impl StdioInner {
    pub(crate) fn new(
        mut process: Box<dyn McpStdioProcess>,
        notification_tx: tokio::sync::mpsc::Sender<Value>,
        tools_list_changed: Arc<ToolsListChangedSignal>,
        cancel: CancellationToken,
    ) -> std::io::Result<Self> {
        let streams = (|| {
            Ok::<_, std::io::Error>((
                process.take_stdin()?,
                process.take_stdout()?,
                process.take_stderr()?,
            ))
        })();
        let (stdin, stdout, stderr) = match streams {
            Ok(streams) => streams,
            Err(error) => {
                if let Err(kill_error) = process.start_kill() {
                    tracing::debug!(
                        %kill_error,
                        "failed to stop MCP stdio process after stream setup failed"
                    );
                }
                return Err(error);
            }
        };
        let stderr_task = stderr.map(|mut stderr| {
            tokio::spawn(async move {
                // stdio servers may log arbitrarily much text to stderr. It is
                // explicitly optional diagnostic output, so drain and discard
                // it with constant memory to keep the protocol pipe writable.
                let mut sink = tokio::io::sink();
                let _ = tokio::io::copy(&mut stderr, &mut sink).await;
            })
        });
        let io = Arc::new(StdioIo {
            stdin: AsyncMutex::new(Some(Box::new(stdin))),
            pending: AsyncMutex::new(HashMap::new()),
            active_subscriptions: std::sync::Mutex::new(HashSet::new()),
            reader_failed: AtomicBool::new(false),
            notification_tx,
            tools_list_changed,
            cancel,
        });
        let reader_task = tokio::spawn(read_stdio_messages(BufReader::new(stdout), io.clone()));
        Ok(Self {
            process: AsyncMutex::new(process),
            io,
            reader_task,
            stderr_task,
        })
    }

    pub(crate) fn poisoned(&self) -> bool {
        self.io.reader_failed.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn mark_reader_failed_for_test(&self) {
        self.io.reader_failed.store(true, Ordering::Release);
    }

    pub(crate) async fn is_alive(&self) -> bool {
        self.process.lock().await.is_alive().unwrap_or(false)
    }

    pub(crate) async fn start_kill(&self) -> std::io::Result<()> {
        self.process.lock().await.start_kill()
    }

    /// Close stdin to deliver EOF to the server. This is bounded and drops the
    /// writer even if its asynchronous shutdown cannot complete in time.
    pub(crate) async fn close_stdin(&self) -> std::io::Result<()> {
        self.io.close_stdin(STDIO_CLOSE_INPUT_TIMEOUT).await
    }

    /// Wait for process exit for a bounded interval. The process port's wait
    /// future must be safe to cancel and poll again after this returns false.
    pub(crate) async fn wait_for_exit(&self, timeout: Duration) -> std::io::Result<bool> {
        let mut process = self.process.lock().await;
        match tokio::time::timeout(timeout, process.wait()).await {
            Ok(result) => result.map(|()| true),
            Err(_) => Ok(false),
        }
    }

    pub(crate) async fn bounded_exit_wait(&self) -> std::io::Result<bool> {
        self.wait_for_exit(STDIO_EXIT_WAIT_TIMEOUT).await
    }

    pub(crate) async fn bounded_kill_wait(&self) -> std::io::Result<bool> {
        self.wait_for_exit(STDIO_KILL_WAIT_TIMEOUT).await
    }

    pub(crate) async fn take_output_review(&self) -> Option<haven_platform::SandboxOutputReview> {
        self.process.lock().await.take_output_review()
    }

    pub(crate) async fn request(
        &self,
        id: u64,
        method: &str,
        params: Option<Value>,
    ) -> anyhow::Result<Value> {
        self.io.request(id, method, params).await
    }

    pub(crate) fn spawn_tools_subscription(
        &self,
        name: String,
        next_id: Arc<std::sync::atomic::AtomicU64>,
    ) -> JoinHandle<()> {
        spawn_stdio_tools_subscription(name, self.io.clone(), next_id)
    }
}

impl Drop for StdioInner {
    fn drop(&mut self) {
        self.reader_task.abort();
        if let Some(stderr_task) = &self.stderr_task {
            stderr_task.abort();
        }
        if self.process.get_mut().start_kill().is_err() {
            tracing::debug!("MCP stdio child cleanup failed");
        }
    }
}

impl StdioIo {
    async fn request(&self, id: u64, method: &str, params: Option<Value>) -> anyhow::Result<Value> {
        if self.reader_failed.load(Ordering::Acquire) {
            anyhow::bail!("MCP stdio response reader has stopped");
        }
        let (response_tx, response_rx) = oneshot::channel();
        self.pending.lock().await.insert(id, response_tx);
        if method == "subscriptions/listen" {
            self.active_subscriptions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id);
        }
        let req = jsonrpc_request(id, method, params);
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        let write_result = {
            let mut stdin = self.stdin.lock().await;
            let Some(stdin) = stdin.as_mut() else {
                self.pending.lock().await.remove(&id);
                self.remove_subscription(id);
                anyhow::bail!("MCP stdio stdin has been closed");
            };
            write_stdio_line(&mut **stdin, &line, method, &self.cancel).await
        };
        if let Err(error) = write_result {
            self.pending.lock().await.remove(&id);
            self.remove_subscription(id);
            return Err(error);
        }

        let response = if method == "subscriptions/listen" {
            tokio::select! {
                _ = self.cancel.cancelled() => Err(anyhow::anyhow!("MCP stdio subscription cancelled")),
                result = response_rx => match result {
                    Err(_) => Err(anyhow::anyhow!("MCP stdio response reader stopped")),
                    Ok(Err(error)) => Err(anyhow::anyhow!("{error}")),
                    Ok(Ok(value)) => Ok(unpack_jsonrpc(value)?),
                }
            }
        } else {
            tokio::select! {
                _ = self.cancel.cancelled() => Err(anyhow::anyhow!("MCP stdio request '{}' cancelled", method)),
                result = tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), response_rx) => {
                    match result {
                        Err(_) => Err(anyhow::anyhow!("MCP request '{}' timed out", method)),
                        Ok(Err(_)) => Err(anyhow::anyhow!("MCP stdio response reader stopped")),
                        Ok(Ok(Err(error))) => Err(anyhow::anyhow!("{error}")),
                        Ok(Ok(Ok(value))) => Ok(unpack_jsonrpc(value)?),
                    }
                }
            }
        };
        self.pending.lock().await.remove(&id);
        self.remove_subscription(id);
        response
    }

    async fn close_stdin(&self, timeout: Duration) -> std::io::Result<()> {
        let mut stdin = tokio::time::timeout(timeout, self.stdin.lock())
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "MCP stdio writer lock did not close in time",
                )
            })?;
        let Some(mut writer) = stdin.take() else {
            return Ok(());
        };
        match tokio::time::timeout(timeout, writer.shutdown()).await {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "MCP stdio stdin did not close in time",
            )),
        }
    }

    async fn notify_cancelled_request(&self, request_id: u64) -> anyhow::Result<()> {
        let notification = jsonrpc_notification(
            "notifications/cancelled",
            Some(serde_json::json!({"requestId": request_id})),
        );
        let mut line = serde_json::to_string(&notification)?;
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        let Some(stdin) = stdin.as_mut() else {
            anyhow::bail!("MCP stdio stdin has been closed");
        };
        // This is the cancellation signal for the stdio request itself, so it
        // must still be written after the connection's normal work token has
        // been cancelled. Keep the write bounded while allowing shutdown to
        // proceed if the child has already stopped reading.
        write_stdio_line_with_timeout(
            &mut **stdin,
            &line,
            "notifications/cancelled",
            &CancellationToken::new(),
            Duration::from_secs(2),
        )
        .await
    }

    fn remove_subscription(&self, id: u64) {
        self.active_subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
    }

    fn accepts_notification(&self, value: &Value) -> bool {
        if value.get("method").and_then(Value::as_str) != Some("notifications/tools/list_changed") {
            return true;
        }
        let Some(subscription_id) =
            value["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].as_u64()
        else {
            return false;
        };
        self.active_subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&subscription_id)
    }
}

async fn read_stdio_messages<R>(mut stdout: BufReader<R>, io: Arc<StdioIo>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let failure = loop {
        // Keep reading until EOF so a pending response can still be received
        // after the normal request token has been cancelled.
        let line = read_line_bounded(&mut stdout).await;
        let line = match line {
            Ok(line) if !line.is_empty() => line,
            Ok(_) => break "MCP server stdout closed (process died)".to_string(),
            Err(error) => break error.to_string(),
        };
        let parsed = match serde_json::from_slice::<Value>(&line) {
            Ok(value) => value,
            Err(error) => break format!("invalid MCP stdio JSON-RPC message: {error}"),
        };
        if let Some(id) = parsed.get("id").and_then(Value::as_u64) {
            if let Some(tx) = io.pending.lock().await.remove(&id) {
                let _ = tx.send(Ok(parsed));
            }
            continue;
        }
        if io.accepts_notification(&parsed)
            && let Err(error) =
                enqueue_notification(&io.notification_tx, &io.tools_list_changed, parsed)
        {
            tracing::debug!(
                error = %haven_common::error::sanitize_error_text(&error.to_string()),
                "MCP stdio notification could not be routed"
            );
        }
    };

    io.reader_failed.store(true, Ordering::Release);
    let pending = std::mem::take(&mut *io.pending.lock().await);
    for (_, tx) in pending {
        let _ = tx.send(Err(failure.clone()));
    }
}

fn spawn_stdio_tools_subscription(
    name: String,
    io: Arc<StdioIo>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(2);
        loop {
            if io.cancel.is_cancelled() {
                break;
            }
            let id = next_id.fetch_add(1, Ordering::SeqCst);
            let params = params_with_protocol_meta(
                Some(serde_json::json!({
                    "notifications": {"toolsListChanged": true}
                })),
                MODERN_PROTOCOL_VERSION,
            );
            let result = io.request(id, "subscriptions/listen", Some(params)).await;
            if io.cancel.is_cancelled() {
                if io.notify_cancelled_request(id).await.is_err() {
                    tracing::debug!(server = %name, "MCP stdio subscription cancellation could not be sent");
                }
                break;
            }
            if result.is_err() {
                tracing::debug!(server = %name, "MCP stdio tool-list subscription ended; retrying");
            }
            tokio::select! {
                _ = io.cancel.cancelled() => break,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
        }
    })
}

async fn write_stdio_line<W>(
    stdin: &mut W,
    line: &str,
    method: &str,
    cancel: &CancellationToken,
) -> anyhow::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + ?Sized,
{
    write_stdio_line_with_timeout(
        stdin,
        line,
        method,
        cancel,
        Duration::from_secs(REQUEST_TIMEOUT_SECS),
    )
    .await
}

async fn write_stdio_line_with_timeout<W: tokio::io::AsyncWrite + Unpin + ?Sized>(
    stdin: &mut W,
    line: &str,
    method: &str,
    cancel: &CancellationToken,
    timeout: Duration,
) -> anyhow::Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => {
            anyhow::bail!("MCP stdio write '{}' cancelled", method)
        }
        result = tokio::time::timeout(
            timeout,
            async {
                stdin.write_all(line.as_bytes()).await?;
                stdin.flush().await?;
                Ok::<(), std::io::Error>(())
            },
        ) => {
            result
                .map_err(|_| anyhow::anyhow!("MCP stdio write '{}' timed out", method))??;
            Ok(())
        }
    }
}

/// Shared state for the Streamable HTTP transport.
pub(crate) struct HttpShared {
    pub(crate) http: reqwest::Client,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) cancel: CancellationToken,
}

/// Streamable HTTP transport. Requests are POSTed to the endpoint; responses
/// may be JSON or request-scoped SSE.
pub(crate) struct HttpInner {
    pub(crate) shared: Arc<HttpShared>,
    pub(crate) notification_tx: tokio::sync::mpsc::Sender<Value>,
    /// Single buffered (incomplete) SSE line/event cap (from context limits).
    pub(crate) max_sse_buffer: usize,
    pub(crate) tools_list_changed: Arc<ToolsListChangedSignal>,
}

impl HttpInner {
    pub(crate) async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<Value>,
        parameter_headers: &[(String, String)],
    ) -> anyhow::Result<Value> {
        if method == "tools/call"
            && params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .is_none()
        {
            anyhow::bail!("modern MCP tools/call requires params.name");
        }
        let req = jsonrpc_request(id, method, params);
        let builder = self
            .shared
            .http
            .post(&self.shared.url)
            .json(&req)
            .header("Accept", "application/json, text/event-stream");
        let builder =
            apply_http_request_headers(builder, &self.shared, method, &req, parameter_headers);

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
            let status = status.as_u16();
            let body = read_bytes_bounded(resp, MAX_JSON_BODY_BYTES, &self.shared.cancel).await?;
            if let Ok(value) = serde_json::from_slice::<Value>(&body)
                && let Some(error) = McpRpcError::from_response(&value)
            {
                return Err(error.into());
            }
            return Err(McpHttpStatusError { status }.into());
        }
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.contains("text/event-stream"))
            .unwrap_or(false);

        if is_sse {
            read_sse_response(
                resp,
                id,
                &self.notification_tx,
                self.max_sse_buffer,
                &self.shared.cancel,
                &self.tools_list_changed,
            )
            .await
        } else {
            let value = read_json_bounded(resp, MAX_JSON_BODY_BYTES, &self.shared.cancel).await?;
            unpack_jsonrpc(value)
        }
    }
}

pub(crate) enum McpClientInner {
    Stdio(Box<StdioInner>),
    Http(HttpInner),
}

impl McpClientInner {
    pub(crate) async fn request(
        &mut self,
        id: u64,
        method: &str,
        params: Option<Value>,
        parameter_headers: &[(String, String)],
    ) -> anyhow::Result<Value> {
        let params = Some(params_with_protocol_meta(params, MODERN_PROTOCOL_VERSION));
        match self {
            McpClientInner::Stdio(s) => s.request(id, method, params).await,
            McpClientInner::Http(h) => h.request(id, method, params, parameter_headers).await,
        }
    }

    pub(crate) fn spawn_tools_subscription(
        &self,
        name: String,
        next_id: Arc<std::sync::atomic::AtomicU64>,
    ) -> JoinHandle<()> {
        match self {
            McpClientInner::Stdio(stdio) => stdio.spawn_tools_subscription(name, next_id),
            McpClientInner::Http(http) => spawn_http_tools_subscription(
                name,
                http.shared.clone(),
                http.notification_tx.clone(),
                http.max_sse_buffer,
                http.tools_list_changed.clone(),
                next_id,
            ),
        }
    }
}

/// Read an SSE streamed response body until the JSON-RPC response matching
/// `id` arrives; id-less messages in the stream are routed as notifications.
fn unpack_jsonrpc(value: Value) -> anyhow::Result<Value> {
    if let Some(error) = McpRpcError::from_response(&value) {
        return Err(error.into());
    }
    Ok(value["result"].clone())
}

/// Attach every user-configured header to a request builder.
pub(crate) fn apply_http_headers(
    mut builder: reqwest::RequestBuilder,
    shared: &HttpShared,
) -> reqwest::RequestBuilder {
    for (k, v) in &shared.headers {
        builder = builder.header(k.as_str(), v);
    }
    builder
}

fn apply_http_request_headers(
    mut builder: reqwest::RequestBuilder,
    shared: &HttpShared,
    method: &str,
    request: &Value,
    parameter_headers: &[(String, String)],
) -> reqwest::RequestBuilder {
    for (key, value) in &shared.headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    for (key, value) in parameter_headers {
        builder = builder.header(key.as_str(), value.as_str());
    }
    builder = builder
        .header("MCP-Protocol-Version", MODERN_PROTOCOL_VERSION)
        .header("Mcp-Method", method);
    let name = request["params"]["name"]
        .as_str()
        .or_else(|| request["params"]["uri"].as_str());
    if let Some(name) = name {
        builder = builder.header("Mcp-Name", encode_mcp_header_value(name));
    }
    builder
}

async fn read_sse_response(
    resp: reqwest::Response,
    id: u64,
    tx: &tokio::sync::mpsc::Sender<Value>,
    max_sse_buffer: usize,
    cancel: &CancellationToken,
    tools_list_changed: &ToolsListChangedSignal,
) -> anyhow::Result<Value> {
    let mut stream = resp.bytes_stream();
    let mut parser = SseParser::new(max_sse_buffer);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(REQUEST_TIMEOUT_SECS);
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("MCP HTTP request cancelled"),
            result = tokio::time::timeout_at(deadline, stream.next()) => result
                .map_err(|_| anyhow::anyhow!("MCP HTTP: timed out waiting for SSE response"))?,
        };
        match chunk {
            Some(Ok(bytes)) => {
                for ev in parser.feed(&bytes) {
                    if ev.get("id").and_then(|v| v.as_u64()) == Some(id) {
                        return unpack_jsonrpc(ev);
                    }
                    if cancel.is_cancelled() {
                        anyhow::bail!("MCP HTTP request cancelled");
                    }
                    enqueue_notification(tx, tools_list_changed, ev)?;
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

fn spawn_http_tools_subscription(
    name: String,
    shared: Arc<HttpShared>,
    notification_tx: tokio::sync::mpsc::Sender<Value>,
    max_sse_buffer: usize,
    tools_list_changed: Arc<ToolsListChangedSignal>,
    next_id: Arc<std::sync::atomic::AtomicU64>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut backoff = Duration::from_secs(2);
        loop {
            if shared.cancel.is_cancelled() {
                break;
            }
            let id = next_id.fetch_add(1, Ordering::SeqCst);
            let params = params_with_protocol_meta(
                Some(serde_json::json!({
                    "notifications": {"toolsListChanged": true}
                })),
                MODERN_PROTOCOL_VERSION,
            );
            let request = jsonrpc_request(id, "subscriptions/listen", Some(params));
            let builder = shared
                .http
                .post(&shared.url)
                .json(&request)
                .header("Accept", "application/json, text/event-stream");
            let builder =
                apply_http_request_headers(builder, &shared, "subscriptions/listen", &request, &[]);
            let response = tokio::select! {
                _ = shared.cancel.cancelled() => break,
                result = tokio::time::timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS), builder.send()) => result
                    .map_err(|_| anyhow::anyhow!("MCP subscriptions/listen request timed out"))
                    .and_then(|result| result.map_err(anyhow::Error::from)),
            };
            let stream_result = match response {
                Ok(response) if response.status().is_success() => {
                    let is_sse = response
                        .headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|content_type| content_type.contains("text/event-stream"));
                    if is_sse {
                        read_tools_subscription_stream(
                            response,
                            id,
                            &notification_tx,
                            max_sse_buffer,
                            &shared.cancel,
                            &tools_list_changed,
                        )
                        .await
                    } else {
                        let body = read_text_bounded(response, MAX_JSON_BODY_BYTES, &shared.cancel)
                            .await
                            .unwrap_or_default();
                        Err(anyhow::anyhow!(
                            "MCP subscriptions/listen did not return an SSE stream (body bytes: {})",
                            body.len()
                        ))
                    }
                }
                Ok(response) => Err(anyhow::anyhow!(
                    "MCP subscriptions/listen returned HTTP status {}",
                    response.status().as_u16()
                )),
                Err(error) => Err(error),
            };
            if shared.cancel.is_cancelled() {
                break;
            }
            if let Err(error) = stream_result {
                tracing::debug!(
                    server = %name,
                    error = %haven_common::error::sanitize_error_text(&error.to_string()),
                    "MCP tool-list subscription ended; retrying"
                );
            }
            tokio::select! {
                _ = shared.cancel.cancelled() => break,
                _ = tokio::time::sleep(backoff) => {}
            }
            backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
        }
    })
}

async fn read_tools_subscription_stream(
    response: reqwest::Response,
    subscription_id: u64,
    notification_tx: &tokio::sync::mpsc::Sender<Value>,
    max_sse_buffer: usize,
    cancel: &CancellationToken,
    tools_list_changed: &ToolsListChangedSignal,
) -> anyhow::Result<()> {
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new(max_sse_buffer);
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            chunk = stream.next() => chunk,
        };
        match chunk {
            Some(Ok(bytes)) => {
                for event in parser.feed(&bytes) {
                    if cancel.is_cancelled() {
                        return Ok(());
                    }
                    if event.get("id").and_then(Value::as_u64) == Some(subscription_id) {
                        unpack_jsonrpc(event)?;
                        return Ok(());
                    }
                    let event_subscription_id =
                        event["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].as_u64();
                    if event_subscription_id == Some(subscription_id) {
                        enqueue_notification(notification_tx, tools_list_changed, event)?;
                    }
                }
            }
            Some(Err(error)) => return Err(error.into()),
            None => return Ok(()),
        }
    }
}

/// Read one JSON-RPC line without allowing a peer to force an unbounded
/// allocation. The reader consumes the input in chunks so an oversized line
/// is rejected before it can grow past the configured cap.
async fn read_line_bounded<R>(reader: &mut R) -> std::io::Result<Vec<u8>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if line.is_empty() {
                return Ok(line);
            }
            return Ok(line);
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        if line.len().saturating_add(take) > MAX_STDIO_LINE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "MCP stdio JSON-RPC line exceeds the maximum length",
            ));
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            return Ok(line);
        }
    }
}

/// Keep notification delivery finite without allowing a burst of unsolicited
/// events to stall the response that the caller is waiting for. Notifications
/// are advisory; dropping the newest event is preferable to deadlocking the
/// transport or growing memory without a bound. `tools/list_changed` is the
/// exception: it is coalesced into a separate signal before this queue.
fn enqueue_notification(
    tx: &tokio::sync::mpsc::Sender<Value>,
    tools_list_changed: &ToolsListChangedSignal,
    value: Value,
) -> anyhow::Result<()> {
    if value.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed") {
        tools_list_changed.request();
        return Ok(());
    }
    match tx.try_send(value) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("MCP notification queue is full; dropping notification");
            Ok(())
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            anyhow::bail!("MCP notification channel closed")
        }
    }
}

async fn read_bytes_bounded(
    resp: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<u8>> {
    read_bytes_bounded_with_timeout(
        resp,
        limit,
        cancel,
        Duration::from_secs(REQUEST_TIMEOUT_SECS),
    )
    .await
}

/// Read a finite HTTP response body with both a byte cap and an independent
/// body deadline. `RequestBuilder::send()` only covers the header phase here;
/// a peer can otherwise keep the connection open forever after returning 200.
async fn read_bytes_bounded_with_timeout(
    resp: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    tokio::time::timeout(timeout, read_bytes_bounded_inner(resp, limit, cancel))
        .await
        .map_err(|_| anyhow::anyhow!("MCP response body read timed out"))?
}

async fn read_bytes_bounded_inner(
    resp: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<u8>> {
    if cancel.is_cancelled() {
        anyhow::bail!("MCP response body read cancelled");
    }
    let mut body =
        BoundedBytes::new(limit, resp.content_length()).map_err(|_| mcp_body_limit_error(limit))?;
    let mut stream = resp.bytes_stream();
    loop {
        if cancel.is_cancelled() {
            anyhow::bail!("MCP response body read cancelled");
        }
        let chunk = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("MCP response body read cancelled"),
            chunk = stream.next() => chunk,
        };
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk?;
        body.push(&chunk).map_err(|_| mcp_body_limit_error(limit))?;
    }
    Ok(body.into_vec())
}

fn mcp_body_limit_error(limit: usize) -> anyhow::Error {
    anyhow::anyhow!("MCP response body exceeds the {} byte limit", limit)
}

async fn read_text_bounded(
    resp: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<String> {
    Ok(String::from_utf8_lossy(&read_bytes_bounded(resp, limit, cancel).await?).into_owned())
}

async fn read_json_bounded(
    resp: reqwest::Response,
    limit: usize,
    cancel: &CancellationToken,
) -> anyhow::Result<Value> {
    Ok(serde_json::from_slice(
        &read_bytes_bounded(resp, limit, cancel).await?,
    )?)
}

/// Liveness probe for the HTTP transport: any HTTP response (even an error
/// status) means the endpoint is reachable; only a transport failure or
/// timeout reports it dead.
pub(crate) async fn http_is_alive(shared: &Arc<HttpShared>) -> bool {
    let builder = shared.http.head(&shared.url);
    let req = apply_http_headers(builder, shared);
    tokio::select! {
        biased;
        _ = shared.cancel.cancelled() => false,
        result = tokio::time::timeout(Duration::from_secs(5), req.send()) => {
            matches!(result, Ok(Ok(_)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::process::Command;

    struct PipeProcess {
        stdin: Option<crate::McpStdioWriter>,
        stdout: Option<crate::McpStdioReader>,
        stderr: Option<crate::McpStdioReader>,
        alive: Arc<AtomicBool>,
        kill_count: Arc<AtomicUsize>,
    }

    impl McpStdioProcess for PipeProcess {
        fn take_stdin(&mut self) -> std::io::Result<crate::McpStdioWriter> {
            self.stdin
                .take()
                .ok_or_else(|| std::io::Error::other("stdin already taken"))
        }

        fn take_stdout(&mut self) -> std::io::Result<crate::McpStdioReader> {
            self.stdout
                .take()
                .ok_or_else(|| std::io::Error::other("stdout already taken"))
        }

        fn take_stderr(&mut self) -> std::io::Result<Option<crate::McpStdioReader>> {
            Ok(self.stderr.take())
        }

        fn is_alive(&mut self) -> std::io::Result<bool> {
            Ok(self.alive.load(Ordering::Acquire))
        }

        fn start_kill(&mut self) -> std::io::Result<()> {
            self.kill_count.fetch_add(1, Ordering::AcqRel);
            self.alive.store(false, Ordering::Release);
            Ok(())
        }

        fn wait(&mut self) -> futures_util::future::BoxFuture<'_, std::io::Result<()>> {
            let alive = self.alive.clone();
            Box::pin(async move {
                while alive.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn stdio_inner_drains_diagnostic_stderr_with_bounded_memory() {
        let (client_stream, _peer_stream) = tokio::io::duplex(64);
        let (stdout, stdin) = tokio::io::split(client_stream);
        let (stderr_reader, mut stderr_writer) = tokio::io::duplex(16);
        let alive = Arc::new(AtomicBool::new(true));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let process = Box::new(PipeProcess {
            stdin: Some(Box::new(stdin)),
            stdout: Some(Box::new(stdout)),
            stderr: Some(Box::new(stderr_reader)),
            alive: alive.clone(),
            kill_count,
        });
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let inner = StdioInner::new(
            process,
            notification_tx,
            Arc::new(ToolsListChangedSignal::new()),
            CancellationToken::new(),
        )
        .unwrap();

        let write = tokio::spawn(async move {
            let chunk = [b'x'; 1024];
            for _ in 0..1024 {
                stderr_writer.write_all(&chunk).await.unwrap();
            }
        });
        tokio::time::timeout(Duration::from_secs(2), write)
            .await
            .expect("stderr must keep draining after the pipe buffer fills")
            .unwrap();
        drop(inner);
        assert!(!alive.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn stdio_inner_stops_process_when_stream_setup_fails() {
        let (client_stream, _peer_stream) = tokio::io::duplex(64);
        let (_stdout, stdin) = tokio::io::split(client_stream);
        let alive = Arc::new(AtomicBool::new(true));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let process = Box::new(PipeProcess {
            stdin: Some(Box::new(stdin)),
            stdout: None,
            stderr: None,
            alive: alive.clone(),
            kill_count,
        });
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);

        let result = StdioInner::new(
            process,
            notification_tx,
            Arc::new(ToolsListChangedSignal::new()),
            CancellationToken::new(),
        );

        assert!(result.is_err());
        assert!(!alive.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn process_wait_can_be_bounded_then_retried_after_kill() {
        let (client_stream, _peer_stream) = tokio::io::duplex(64);
        let (stdout, stdin) = tokio::io::split(client_stream);
        let alive = Arc::new(AtomicBool::new(true));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let process = Box::new(PipeProcess {
            stdin: Some(Box::new(stdin)),
            stdout: Some(Box::new(stdout)),
            stderr: None,
            alive: alive.clone(),
            kill_count: kill_count.clone(),
        });
        let (notification_tx, _notification_rx) =
            tokio::sync::mpsc::channel(NOTIFICATION_QUEUE_CAPACITY);
        let inner = StdioInner::new(
            process,
            notification_tx,
            Arc::new(ToolsListChangedSignal::new()),
            CancellationToken::new(),
        )
        .unwrap();

        assert!(
            !inner
                .wait_for_exit(Duration::from_millis(10))
                .await
                .unwrap()
        );
        inner.start_kill().await.unwrap();
        assert!(
            inner
                .wait_for_exit(Duration::from_millis(100))
                .await
                .unwrap()
        );
        assert_eq!(kill_count.load(Ordering::Acquire), 1);
    }

    async fn response_from_server(response: &'static str) -> reqwest::Response {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        let _ = task.await;
        result
    }

    #[tokio::test]
    async fn modern_http_headers_route_each_request_without_a_session() {
        use base64::Engine;

        let shared = HttpShared {
            http: reqwest::Client::new(),
            url: "http://127.0.0.1:1/mcp".into(),
            headers: vec![("Authorization".into(), "Bearer token".into())],
            cancel: CancellationToken::new(),
        };
        let request = jsonrpc_request(
            1,
            "tools/call",
            Some(serde_json::json!({"name": "東京", "arguments": {}})),
        );
        let builder = shared.http.post(&shared.url).json(&request);
        let builder = apply_http_request_headers(builder, &shared, "tools/call", &request, &[]);
        let headers = builder.build().unwrap().headers().clone();

        assert_eq!(headers["mcp-protocol-version"], "2026-07-28");
        assert_eq!(headers["mcp-method"], "tools/call");
        assert_eq!(
            headers["mcp-name"],
            format!(
                "=?base64?{}?=",
                base64::engine::general_purpose::STANDARD.encode("東京".as_bytes())
            )
        );
        assert!(headers.get("mcp-session-id").is_none());
        assert_eq!(headers["authorization"], "Bearer token");
    }

    #[tokio::test]
    async fn body_limit_rejects_content_length_before_reading() {
        let response = response_from_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n123456789",
        )
        .await;
        let error = read_bytes_bounded(response, 8, &CancellationToken::new())
            .await
            .expect_err("declared body over the limit must fail before allocation");
        assert!(error.to_string().contains("8 byte limit"));
    }

    #[tokio::test]
    async fn body_limit_rejects_chunked_body_while_consuming() {
        let response = response_from_server(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n5\r\nworld\r\n0\r\n\r\n",
        )
        .await;
        let error = read_bytes_bounded(response, 8, &CancellationToken::new())
            .await
            .expect_err("chunked body over the limit must fail while reading");
        assert!(error.to_string().contains("8 byte limit"));
    }

    #[tokio::test]
    async fn body_read_honors_cancellation() {
        let response = response_from_server(
            "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\nx",
        )
        .await;
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = read_bytes_bounded(response, 8, &cancel)
            .await
            .expect_err("cancelled body reads must stop");
        assert!(error.to_string().contains("cancelled"));
    }

    #[tokio::test]
    async fn body_read_has_an_independent_deadline_after_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();

        let error = read_bytes_bounded_with_timeout(
            response,
            8,
            &CancellationToken::new(),
            Duration::from_millis(25),
        )
        .await
        .expect_err("body reads must not wait forever after headers");
        assert!(error.to_string().contains("body read timed out"));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn http_liveness_probe_honors_transport_cancellation() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
        });

        let cancel = CancellationToken::new();
        let shared = Arc::new(HttpShared {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            url: format!("http://{address}/"),
            headers: Vec::new(),
            cancel: cancel.clone(),
        });
        let probe = tokio::spawn({
            let shared = shared.clone();
            async move { http_is_alive(&shared).await }
        });

        tokio::time::timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("liveness probe must reach the test server")
            .expect("test server must accept the probe");
        cancel.cancel();

        let alive = tokio::time::timeout(Duration::from_millis(200), probe)
            .await
            .expect("cancelling a liveness probe must not wait for its 5-second timeout")
            .expect("liveness probe task must finish cleanly");
        assert!(!alive);

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn stdio_write_honors_cancellation_before_writing() {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/c", "more"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "cat >/dev/null"]);
            command
        };
        let mut child = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut stdin = child.stdin.take().unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = write_stdio_line(
            &mut stdin,
            "{\"jsonrpc\":\"2.0\"}\n",
            "notifications/test",
            &cancel,
        )
        .await
        .expect_err("cancelled stdio writes must stop before touching the pipe");
        assert!(error.to_string().contains("cancelled"));
        drop(stdin);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn stdio_write_honors_its_deadline() {
        let mut command = if cfg!(windows) {
            let mut command = Command::new("cmd");
            command.args(["/c", "ping -n 6 127.0.0.1 > nul"]);
            command
        } else {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 5"]);
            command
        };
        let mut child = {
            let _spawn_guard = haven_platform::process_containment::process_spawn_lock();
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        };
        let mut writer = child.stdin.take().unwrap();
        // A real child that never reads stdin lets the OS pipe fill, so this
        // exercises the actual ChildStdin write path rather than a synthetic
        // pending future.
        let line = "x".repeat(8 * 1024 * 1024);
        let error = write_stdio_line_with_timeout(
            &mut writer,
            &line,
            "notifications/test",
            &CancellationToken::new(),
            Duration::from_millis(100),
        )
        .await
        .expect_err("stdio writes must have an independent deadline");
        assert!(error.to_string().contains("timed out"));
        drop(writer);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }

    #[tokio::test]
    async fn stdio_subscription_does_not_block_concurrent_requests() {
        let (client_stream, server_stream) = tokio::io::duplex(4096);
        let (client_read, client_write) = tokio::io::split(client_stream);
        let (server_read, mut server_write) = tokio::io::split(server_stream);
        let (notification_tx, _notification_rx) = tokio::sync::mpsc::channel(4);
        let tools_list_changed = Arc::new(ToolsListChangedSignal::new());
        let cancel = CancellationToken::new();
        let io = Arc::new(StdioIo {
            stdin: AsyncMutex::new(Some(Box::new(client_write))),
            pending: AsyncMutex::new(HashMap::new()),
            active_subscriptions: std::sync::Mutex::new(HashSet::new()),
            reader_failed: AtomicBool::new(false),
            notification_tx,
            tools_list_changed: tools_list_changed.clone(),
            cancel: cancel.clone(),
        });
        let reader_task =
            tokio::spawn(read_stdio_messages(BufReader::new(client_read), io.clone()));

        let subscription = tokio::spawn({
            let io = io.clone();
            async move {
                io.request(
                    1,
                    "subscriptions/listen",
                    Some(serde_json::json!({"notifications": {"toolsListChanged": true}})),
                )
                .await
            }
        });
        let request = tokio::spawn({
            let io = io.clone();
            async move { io.request(2, "tools/list", None).await }
        });

        let mut server_reader = BufReader::new(server_read);
        let mut first = String::new();
        let mut second = String::new();
        server_reader.read_line(&mut first).await.unwrap();
        server_reader.read_line(&mut second).await.unwrap();
        let first: Value = serde_json::from_str(&first).unwrap();
        let second: Value = serde_json::from_str(&second).unwrap();
        assert_eq!(first["method"], "subscriptions/listen");
        assert_eq!(second["method"], "tools/list");

        server_write
            .write_all(br#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#)
            .await
            .unwrap();
        server_write.write_all(b"\n").await.unwrap();
        server_write
            .write_all(
                br#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{"_meta":{"io.modelcontextprotocol/subscriptionId":1}}}"#,
            )
            .await
            .unwrap();
        server_write.write_all(b"\n").await.unwrap();
        server_write
            .write_all(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
            .await
            .unwrap();
        server_write.write_all(b"\n").await.unwrap();

        let result = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .expect("normal request must complete while the subscription is open")
            .unwrap()
            .unwrap();
        assert_eq!(result["tools"], serde_json::json!([]));
        tokio::time::timeout(Duration::from_secs(1), subscription)
            .await
            .expect("subscription stream response must be correlated independently")
            .unwrap()
            .unwrap();
        assert!(tools_list_changed.take());

        cancel.cancel();
        reader_task.abort();
    }

    #[tokio::test]
    async fn stdio_subscription_cancellation_is_sent_after_transport_cancel() {
        let (client_stream, server_stream) = tokio::io::duplex(1024);
        let (server_read, _server_write) = tokio::io::split(server_stream);
        let (_notification_tx, notification_rx) = tokio::sync::mpsc::channel(1);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let io = StdioIo {
            stdin: AsyncMutex::new(Some(Box::new(client_stream))),
            pending: AsyncMutex::new(HashMap::new()),
            active_subscriptions: std::sync::Mutex::new(HashSet::new()),
            reader_failed: AtomicBool::new(false),
            notification_tx: _notification_tx,
            tools_list_changed: Arc::new(ToolsListChangedSignal::new()),
            cancel,
        };

        io.notify_cancelled_request(42)
            .await
            .expect("subscription cancellation must still be writable after shutdown starts");
        let mut reader = BufReader::new(server_read);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let notification: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(notification["method"], "notifications/cancelled");
        assert_eq!(notification["params"]["requestId"], 42);
        drop(notification_rx);
    }

    #[tokio::test]
    async fn modern_http_subscription_routes_only_correlated_tool_changes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server_cancel = cancel.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let mut content_length = 0usize;
            let mut headers = HashMap::new();
            loop {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    let name = name.trim().to_ascii_lowercase();
                    let value = value.trim().to_string();
                    if name == "content-length" {
                        content_length = value.parse().unwrap();
                    }
                    headers.insert(name, value);
                }
            }
            let mut body = vec![0; content_length];
            reader.read_exact(&mut body).await.unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["method"], "subscriptions/listen");
            assert_eq!(
                headers.get("mcp-method").map(String::as_str),
                Some("subscriptions/listen")
            );
            assert_eq!(
                headers.get("mcp-protocol-version").map(String::as_str),
                Some("2026-07-28")
            );
            assert!(!headers.contains_key("mcp-session-id"));

            write_half
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            for event in [
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/subscriptions/acknowledged",
                    "params": {
                        "_meta": {"io.modelcontextprotocol/subscriptionId": 1},
                        "notifications": {"toolsListChanged": true}
                    }
                }),
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/tools/list_changed",
                    "params": {"_meta": {"io.modelcontextprotocol/subscriptionId": 1}}
                }),
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/tools/list_changed",
                    "params": {"_meta": {"io.modelcontextprotocol/subscriptionId": 999}}
                }),
            ] {
                let data = format!("data: {}\n\n", event);
                let chunk = format!("{:X}\r\n{}\r\n", data.len(), data);
                write_half.write_all(chunk.as_bytes()).await.unwrap();
                write_half.flush().await.unwrap();
            }
            server_cancel.cancelled().await;
        });

        let (notification_tx, _notification_rx) = tokio::sync::mpsc::channel(4);
        let signal = Arc::new(ToolsListChangedSignal::new());
        let shared = Arc::new(HttpShared {
            http: reqwest::Client::new(),
            url: format!("http://{address}/mcp"),
            headers: Vec::new(),
            cancel: cancel.clone(),
        });
        let task = spawn_http_tools_subscription(
            "subscription-test".into(),
            shared,
            notification_tx,
            1024,
            signal.clone(),
            Arc::new(std::sync::atomic::AtomicU64::new(1)),
        );
        tokio::time::timeout(Duration::from_secs(2), signal.notified())
            .await
            .expect("the correlated list-changed event should invalidate the cache");
        assert!(signal.take());
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("subscription task should stop on cancellation")
            .expect("subscription task should not panic");
        server.await.unwrap();
    }

    #[test]
    fn list_changed_is_coalesced_when_notification_queue_is_full() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(serde_json::json!({"method": "notifications/progress"}))
            .unwrap();
        let signal = ToolsListChangedSignal::new();

        enqueue_notification(
            &tx,
            &signal,
            serde_json::json!({"method": "notifications/tools/list_changed"}),
        )
        .unwrap();

        assert!(
            signal.take(),
            "list invalidation must survive queue pressure"
        );
        assert!(
            rx.try_recv().is_ok(),
            "ordinary notification remains queued"
        );
    }
}
